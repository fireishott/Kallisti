import Foundation
import os
import UIKit
import UserNotifications

@MainActor
@Observable
final class ChatStore {
    private static let logger = Logger(subsystem: "net.fihonline.herald", category: "ChatStore")
    var conversation: Conversation? {
        didSet {
            // Reset auto-title guard only when switching to a different conversation,
            // not on in-place updates (merge, message appends) to the same conversation.
            if oldValue?.id != conversation?.id {
                autoTitleAttempted = false
            }
        }
    }
    var isLoading = false
    var pendingMessageSentAt: Date?
    var lastTokenUsage: TokenUsage?
    var lastContextInfo: ContextInfo?
    /// Error context from the most recent `.failed` streaming update.
    var lastErrorCategory: String?
    var lastErrorAction: String?
    /// Live log entries for the iPad inspector panel's Logs tab.
    var logEntries: [LogEntry] = []
    private var isPollingEnabled = false
    private var pollingTask: Task<Void, Never>?
    private var streamingTask: Task<Void, Never>?
    private var activeStreams: [UUID: UUID] = [:]  // jobId → placeholderId
    var streamingMessageID: UUID? {
        activeStreams.values.first
    }

    /// After `messageSent`, if no real progress (text/reasoning delta, tool
    /// activity, or finish) arrives within this window, the job is treated as
    /// silently stalled/dropped — see `runStreamingAttempt`.
    /// Mutable so tests can set it to milliseconds.
    static var watchdogTimeout: Duration = .seconds(30)

    // Delta coalescing — tokens arrive faster than SwiftUI can usefully redraw.
    // Buffer deltas per-placeholder in an Array<String> (avoids O(n²) inline
    // concat) and flush onto the placeholder at ~30fps so every append triggers
    // at most one @Observable notification per frame.
    private struct DeltaBuffer {
        var chunks: [String] = []
        var bytes: Int = 0
        var flushTask: Task<Void, Never>?
    }
    private var deltaBuffers: [UUID: DeltaBuffer] = [:]
    private static let deltaFlushInterval: Duration = .milliseconds(33)
    private static let deltaFlushByteThreshold = 4_096

    /// Whether `autoTitleIfNeeded` has already been attempted for the current
    /// conversation. Prevents re-attempting on every stream completion when
    /// the title RPC fails and the title remains a default placeholder.
    private var autoTitleAttempted = false

    var isStreaming: Bool { streamingMessageID != nil }
    var connectionStatus: ConnectionStatus { heraldClient.connectionStatus }

    /// Dynamic slash command catalog fetched from the connected Hermes host.
    /// Includes gateway commands, installed skills, custom personalities,
    /// and hidden quick-command metadata for manual slash dispatch.
    private(set) var commandCatalog: [SlashCommand] = SlashCommand.allBuiltIn

    /// Active model name from the Herald agent config (e.g., "gpt-5.4-mini").
    private(set) var activeModelName: String?
    /// Context window size for the active model (e.g., 400000).
    private(set) var contextWindow: Int?

    var currentContextTokens: Int? {
        lastTokenUsage?.promptTokens
    }

    /// Injected by AppContainer so profile-switch detection can update the
    /// active profile name on the owning ProfileStore.
    var profileStore: ProfileStore?

    let heraldClient: any HeraldClientProtocol
    private let chatLiveActivity = LiveActivityService()
    let persistence: any AppPersistenceStoreProtocol

    /// TTS service for speaking responses during/after streaming.
    @ObservationIgnored var ttsService: (any TTSServiceProtocol)?
    /// Provides current TTS settings (enabled, voice, autoSpeak, autoSpeakDuringStreaming).
    @ObservationIgnored var ttsSettingsProvider: (@MainActor () -> (enabled: Bool, voice: String, autoSpeak: Bool, autoSpeakDuringStreaming: Bool))?

    /// Called when conversation content changes (new message, streaming complete).
    /// Used by AppContainer to push widget data updates.
    var onConversationChanged: (@MainActor () -> Void)?

    /// Called when the conversation title changes (server-derived or renamed).
    /// Used by SessionListStore to update sidebar immediately.
    var onTitleChanged: (@MainActor (_ conversationID: UUID, _ newTitle: String) -> Void)?
    var useStreaming: Bool = true

    init(heraldClient: any HeraldClientProtocol, persistence: any AppPersistenceStoreProtocol) {
        self.heraldClient = heraldClient
        self.persistence = persistence
    }

    func loadConversationIfNeeded() async {
        if conversation == nil {
            conversation = persistence.loadConversationCache()
            if let cachedUsage = conversation?.latestUsage {
                lastTokenUsage = cachedUsage
            }
        }
        guard conversation == nil else { return }
        await loadConversation()
    }

    func loadConversation() async {
        isLoading = true
        defer { isLoading = false }
        let cachedConversation = conversation ?? persistence.loadConversationCache()
        conversation = mergeConversationMetadata(
            from: cachedConversation,
            into: await heraldClient.loadConversation()
        )
        autoTitleAttempted = false
        if let latestUsage = conversation?.latestUsage {
            lastTokenUsage = latestUsage
        }
        if let conversation {
            persistence.saveConversationCache(conversation)
            onConversationChanged?()
        }
        restartPendingPollingIfNeeded()
    }

    func sendMessage(_ content: String, attachments: [PendingAttachment] = [], clientMessageID: UUID? = nil) async {
        let trimmedContent = content.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedContent.isEmpty || !attachments.isEmpty else { return }
        guard hasPendingDuplicateMessage(trimmedContent, attachments: attachments) == false else { return }

        let clientMessageID = clientMessageID ?? UUID()
        let displayContent = trimmedContent.isEmpty && !attachments.isEmpty
            ? "[\(attachments.count) attachment\(attachments.count == 1 ? "" : "s")]"
            : trimmedContent
        let optimistic = Message(
            id: clientMessageID,
            clientMessageID: clientMessageID,
            sender: .user,
            content: displayContent,
            status: .sending,
            attachments: attachments.map { MessageAttachment(from: $0) }
        )
        if conversation == nil {
            conversation = Conversation(title: "New Chat")
        }
        conversation?.messages.append(optimistic)
        conversation?.lastActivity = optimistic.timestamp
        pendingMessageSentAt = optimistic.timestamp

        if useStreaming {
            // Append a placeholder Herald message for streaming content
            let placeholderID = UUID()
            let placeholder = Message(
                id: placeholderID,
                sender: .herald,
                content: "",
                status: .sending,
                isStreaming: true
            )
            conversation?.messages.append(placeholder)
            // activeStreams entry is added in the .messageSent handler once jobId is known.
            // streamingMessageID (computed) remains nil until then — that's correct.
            restartPendingPollingIfNeeded()

            await runAttemptLoop(
                content: trimmedContent,
                attachments: attachments,
                clientMessageID: clientMessageID,
                placeholderID: placeholderID
            )
        } else {
            let response = await heraldClient.send(
                message: trimmedContent,
                attachments: attachments,
                clientMessageID: clientMessageID
            )
            if let idx = conversation?.messages.firstIndex(where: { $0.id == clientMessageID }) {
                conversation?.messages[idx].status = .delivered
            }
            conversation?.messages.append(response)
            conversation?.lastActivity = response.timestamp
            conversation = mergeConversationMetadata(
                from: conversation,
                into: heraldClient.currentConversation
            )
            if let latestUsage = conversation?.latestUsage {
                lastTokenUsage = latestUsage
            }
            await autoTitleIfNeeded()
        }

        if !hasPendingMessages {
            pendingMessageSentAt = nil
        }

        if let conversation {
            persistence.saveConversationCache(conversation)
            onConversationChanged?()
        }
    }

    /// Drives a single streaming attempt for an outgoing message.
    ///
    /// A ~120s watchdog guards the attempt (see `runStreamingAttempt`): if no
    /// progress event arrives in that window, the job is treated as silently
    /// dropped. The relay now owns retries via leases, so the client never
    /// resubmits the same message — it just shows a waiting state.
    private func runAttemptLoop(
        content: String,
        attachments: [PendingAttachment],
        clientMessageID: UUID,
        placeholderID: UUID
    ) async {
        let stalled = await runStreamingAttempt(
            content: content,
            attachments: attachments,
            clientMessageID: clientMessageID,
            placeholderID: placeholderID
        )
        guard stalled else { return }

        // Watchdog fired. Show "Waiting for host..." state
        if let idx = conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
            conversation?.messages[idx].toolActivity = "Waiting for host..."
        }

        // Grace period with progress checking
        for i in 0..<3 {
            try? await Task.sleep(for: .seconds(10))

            // Check if answered during grace period
            let refreshed = await refreshActiveConversation()
            conversation = mergeConversationMetadata(from: conversation, into: refreshed)
            if let msg = conversation?.messages.first(where: { $0.id == placeholderID }),
               msg.status == .delivered || !msg.content.isEmpty {
                return
            }

            // Update status message
            if let idx = conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
                conversation?.messages[idx].toolActivity = "Still waiting... (\((i + 1) * 10)s)"
            }
        }

        // No response after 30s — fail with tap-to-retry
        failStalledMessage(clientMessageID: clientMessageID, placeholderID: placeholderID)
    }

    /// Runs a single streaming attempt, racing the update stream against a
    /// ~120s watchdog. Returns `true` if the watchdog fired before any progress
    /// event (`.textDelta`, `.reasoningDelta`, `.toolActivity`, `.finished`)
    /// arrived — i.e. the job appears to have stalled/been silently dropped.
    /// `.messageSent` (the relay merely accepting the job) does NOT count as
    /// progress, since that's precisely the point where the observed bug drops
    /// the job with zero further activity.
    private func runStreamingAttempt(
        content: String,
        attachments: [PendingAttachment],
        clientMessageID: UUID,
        placeholderID: UUID
    ) async -> Bool {
        let stream = heraldClient.sendStreaming(message: content, attachments: attachments, clientMessageID: clientMessageID)
        var acceptedJobID: UUID?
        var needsPollingFallback = false
        var reasoningStartedAt: Date?

        var progressContinuation: AsyncStream<Void>.Continuation?
        let progressSignal = AsyncStream<Void> { continuation in
            progressContinuation = continuation
        }

        let consumerTask = Task { [weak self] in
            guard let self else { return }
            self.appendLog(level: .info, "Streaming started")
            for await update in stream {
                if Task.isCancelled { break }
                switch update {
                case .messageSent(let jobID):
                    self.appendLog(level: .info, "Message accepted — job \(jobID.uuidString.prefix(8))")
                    acceptedJobID = jobID
                    self.activeStreams[jobID] = placeholderID
                    // Start Live Activity with "Thinking" phase — the agent is
                    // processing but hasn't begun streaming content yet.
                    self.chatLiveActivity.startThinking()
                    // Do NOT yield progress — .messageSent is the relay accepting the job,
                    // not the connector producing real progress. The watchdog must keep
                    // waiting for actual content (text/tool/reasoning/terminal).

                case .textDelta(let delta):
                    progressContinuation?.yield(())
                    Self.logger.info("stream textDelta bytes=\(delta.utf8.count) placeholder=\(placeholderID.uuidString.prefix(8))")
                    self.chatLiveActivity.updatePhase("Responding")
                    self.enqueueDelta(delta, placeholderID: placeholderID)

                    // Stream to TTS if enabled during streaming
                    if let settings = self.ttsSettingsProvider?(),
                       settings.enabled,
                       settings.autoSpeakDuringStreaming {
                        self.ttsService?.speakStreaming(delta, voice: settings.voice)
                    }

                case .reasoningDelta(let delta):
                    progressContinuation?.yield(())
                    self.chatLiveActivity.updatePhase("Thinking")
                    if reasoningStartedAt == nil { reasoningStartedAt = .now }
                    if var conv = self.conversation,
                       let idx = conv.messages.firstIndex(where: { $0.id == placeholderID }) {
                        conv.messages[idx].reasoning += delta
                        self.conversation = conv
                    }

                case .toolActivity(let label):
                    progressContinuation?.yield(())
                    self.flushPendingDeltas(placeholderID: placeholderID)
                    if var conv = self.conversation,
                       let idx = conv.messages.firstIndex(where: { $0.id == placeholderID }) {
                        for i in conv.messages[idx].toolActivities.indices {
                            conv.messages[idx].toolActivities[i].isActive = false
                        }
                        let activity = ToolActivity(label: label)
                        conv.messages[idx].toolActivities.append(activity)
                        conv.messages[idx].toolActivity = label
                        self.conversation = conv
                    }
                    // Show tool progress on Lock Screen / Dynamic Island
                    self.chatLiveActivity.startToolCall(toolName: label)
                    self.chatLiveActivity.updateToolProgress(label)

                case .keepalive:
                    progressContinuation?.yield(())

                case .finished(let finalMessage, let usage, let diff, let context):
                    progressContinuation?.yield(())
                    Self.logger.info("stream finished content=\(finalMessage.content.count) chars")
                    self.flushPendingDeltas(placeholderID: placeholderID)
                    if let idx = self.conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
                        let placeholder = self.conversation?.messages[idx]
                        let activities = placeholder?.toolActivities ?? []
                        let streamedReasoning = placeholder?.reasoning ?? ""
                        var resolved = finalMessage
                        resolved.toolActivities = activities
                        resolved.codeDiff = diff
                        // The reloaded server message has no reasoning — carry over
                        // what streamed and freeze its duration so the collapsed
                        // "Thought for Xs" summary survives.
                        if !streamedReasoning.isEmpty {
                            resolved.reasoning = streamedReasoning
                            if let startedAt = reasoningStartedAt {
                                resolved.reasoningDuration = Date().timeIntervalSince(startedAt)
                            }
                        }
                        // Always strip <think>…</think> from content — whether or not we
                        // streamed reasoning. NSRegularExpression with
                        // .dotMatchesLineSeparators handles multiline blocks reliably.
                        if let regex = try? NSRegularExpression(pattern: "<think>.*?</think>", options: [.dotMatchesLineSeparators]) {
                            let range = NSRange(resolved.content.startIndex..., in: resolved.content)
                            resolved.content = regex.stringByReplacingMatches(in: resolved.content, range: range, withTemplate: "")
                                .trimmingCharacters(in: .whitespacesAndNewlines)
                        }
                        self.conversation?.messages[idx] = resolved
                    }
                    // Mark user message as delivered if it's still in sending state
                    if let idx = self.conversation?.messages.firstIndex(where: { $0.id == clientMessageID }) {
                        if self.conversation?.messages[idx].status == .sending {
                            self.conversation?.messages[idx].status = .delivered
                        }
                    }
                    let oldTitle = self.conversation?.title
                    self.conversation = self.mergeConversationMetadata(
                        from: self.conversation,
                        into: self.heraldClient.currentConversation
                    )
                    if let latestUsage = self.conversation?.latestUsage {
                        self.lastTokenUsage = latestUsage
                    } else if let usage {
                        self.lastTokenUsage = usage
                    }
                    if let context {
                        self.lastContextInfo = context
                        self.conversation?.contextPercent = context.percentUsed
                    }
                    self.detectProfileSwitch(in: finalMessage.content)
                    if let jobID = acceptedJobID { self.activeStreams.removeValue(forKey: jobID) }
                    self.pendingMessageSentAt = nil
                    self.chatLiveActivity.endActivity()

                    // Finish TTS streaming — flush any remaining buffered text
                    self.ttsService?.finishStream()
                    // Notify if merge changed the title (server-derived title)
                    if let conv = self.conversation, conv.title != oldTitle {
                        self.onTitleChanged?(conv.id, conv.title)
                    }
                    await self.autoTitleIfNeeded()

                    // Post local notification if app is in background
                    if UIApplication.shared.applicationState == .background {
                        let content = UNMutableNotificationContent()
                        content.title = "Herald"
                        content.body = String(finalMessage.content.prefix(100))
                        content.sound = .default
                        content.categoryIdentifier = NotificationCategoryID.messageReady
                        if let convId = self.conversation?.id.uuidString {
                            content.userInfo = [
                                "conversationId": convId,
                                "messageId": finalMessage.id.uuidString,
                            ]
                        }

                        let request = UNNotificationRequest(
                            identifier: "herald-response-\(UUID().uuidString)",
                            content: content,
                            trigger: nil
                        )
                        try? await UNUserNotificationCenter.current().add(request)
                    }

                case .started(let phase):
                    self.appendLog(level: .info, "Job started — phase: \(phase)")
                    progressContinuation?.yield(())
                    self.chatLiveActivity.updateToolProgress(phase)

                case .heartbeat(let phase):
                    // Heartbeat resets the watchdog — job is alive
                    progressContinuation?.yield(())
                    self.appendLog(level: .debug, "Job heartbeat — phase: \(phase)")

                case .reconnecting:
                    self.appendLog(level: .warn, "Stream reconnecting...")
                    progressContinuation?.yield(())
                    if var conv = self.conversation,
                       let idx = conv.messages.firstIndex(where: { $0.id == placeholderID }) {
                        conv.messages[idx].toolActivity = "Reconnecting..."
                        self.conversation = conv
                    }

                case .cancelled:
                    self.appendLog(level: .info, "Job cancelled")
                    progressContinuation?.yield(())
                    self.flushPendingDeltas(placeholderID: placeholderID)
                    if let idx = self.conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
                        self.conversation?.messages[idx] = Message(
                            sender: .system,
                            content: "Cancelled",
                            status: .failed
                        )
                    }
                    if let jobID = acceptedJobID { self.activeStreams.removeValue(forKey: jobID) }
                    self.pendingMessageSentAt = nil
                    self.chatLiveActivity.endActivity()
                    if let idx = self.conversation?.messages.firstIndex(where: { $0.id == clientMessageID }) {
                        self.conversation?.messages[idx].status = .delivered
                    }
                    await self.autoTitleIfNeeded()

                case .failed(let errorMessage, let category, let action):
                    // An explicit failure is a real signal, not silence — let it
                    // resolve the watchdog race immediately rather than waiting
                    // out the timeout, and handle it exactly as before.
                    progressContinuation?.yield(())
                    self.flushPendingDeltas(placeholderID: placeholderID)

                    // Store error context for the UI
                    self.lastErrorCategory = category
                    self.lastErrorAction = action

                    // Show actionable guidance based on error category
                    let guidance: String
                    switch category {
                    case "context_exceeded":
                        guidance = "This session is too long for the current model. Start a new session or switch models."
                    case "rate_limited":
                        guidance = "Herald is rate-limited. Please wait and try again."
                    case "timeout":
                        guidance = "The request timed out. Check your connection and retry."
                    case "empty_response":
                        guidance = "Herald returned an empty response. Try again or start a new session."
                    default:
                        guidance = errorMessage
                    }

                    if let idx = self.conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
                        if acceptedJobID == nil {
                            self.conversation?.messages[idx] = Message(
                                sender: .system,
                                content: guidance,
                                status: .failed,
                                errorCategory: category
                            )
                        } else {
                            self.conversation?.messages.remove(at: idx)
                        }
                    }
                    if let jobID = acceptedJobID { self.activeStreams.removeValue(forKey: jobID) }
                    self.chatLiveActivity.endActivity()
                    if let idx = self.conversation?.messages.firstIndex(where: { $0.id == clientMessageID }) {
                        self.conversation?.messages[idx].status = acceptedJobID == nil ? .failed : .sending
                    }
                    if acceptedJobID != nil {
                        needsPollingFallback = true
                    } else {
                        self.pendingMessageSentAt = nil
                    }
                    await self.autoTitleIfNeeded()
                }
            }
            progressContinuation?.finish()
        }
        streamingTask = consumerTask

        let watchdogFired = await withTaskGroup(of: Bool.self) { group -> Bool in
            group.addTask {
                for await _ in progressSignal { return false }
                return false
            }
            group.addTask {
                try? await Task.sleep(for: Self.watchdogTimeout)
                return true
            }
            let outcome = await group.next() ?? false
            group.cancelAll()
            return outcome
        }

        if watchdogFired {
            // No progress at all — cancel this attempt's stream so any events
            // that trickle in late can't clobber the retry's state (the loop
            // above checks Task.isCancelled before every write).
            consumerTask.cancel()
            streamingTask = nil
            chatLiveActivity.endActivity()
            return true
        }

        await consumerTask.value
        streamingTask = nil

        // If streaming failed after the job was accepted, immediately refresh once
        // and then fall back to polling only if the server still hasn't delivered.
        if needsPollingFallback {
            let refreshed = await refreshActiveConversation()
            conversation = mergeConversationMetadata(from: conversation, into: refreshed)
            if let latestUsage = conversation?.latestUsage {
                lastTokenUsage = latestUsage
            }
            activeStreams.removeAll()
            restartPendingPollingIfNeeded()
        }

        return false
    }

    /// The user-facing failure copy, using the active profile name when
    /// available and falling back to "Herald".
    func failureMessage() -> String {
        let name = profileStore?.activeProfile?.name ?? "Herald"
        return "\(name) didn't respond. Tap to retry."
    }

    /// Marks a message as failed with real, actionable error text after both
    /// the initial attempt and the automatic retry have stalled with zero
    /// progress. Mirrors the shape of the existing `.failed` stream-event
    /// handling above (Herald placeholder becomes a system error message, user
    /// message flips to `.failed`) so the existing manual "tap to retry" flow
    /// (`retryMessage(_:)`, wired up in `MessageBubble`) keeps working unchanged.
    private func failStalledMessage(clientMessageID: UUID, placeholderID: UUID) {
        let errorText = failureMessage()
        if let idx = conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
            // Replace with error message but keep the same ID so a late
            // .finished can still find and replace it with the actual response.
            conversation?.messages[idx] = Message(
                id: placeholderID,
                sender: .system,
                content: errorText,
                status: .failed
            )
        }
        if let idx = conversation?.messages.firstIndex(where: { $0.id == clientMessageID }) {
            conversation?.messages[idx].status = .failed
        }
        activeStreams.removeAll()
        pendingMessageSentAt = nil
        chatLiveActivity.endActivity()
    }

    func clearConversation() async throws {
        streamingTask?.cancel()
        streamingTask = nil
        activeStreams.removeAll()
        chatLiveActivity.endActivity()
        let fresh = try await heraldClient.clearConversation()
        conversation = fresh
        lastTokenUsage = fresh.latestUsage
        pendingMessageSentAt = nil
        persistence.saveConversationCache(fresh)
        onConversationChanged?()
        pollingTask?.cancel()
        pollingTask = nil
    }

    /// Recover from a stalled stream after app foregrounding.
    /// If the server completed a response while the app was backgrounded,
    /// this will pick it up and clear the stale streaming state.
    func recoverStalledStream() async {
        guard isStreaming else { return }

        // Refresh conversation from server
        let refreshed = await refreshActiveConversation()
        guard let refreshed else { return }

        // Check if the server has a completed response that we missed
        let serverMessages = refreshed.messages
        let localMessages = conversation?.messages ?? []

        // If server has more delivered messages than we do, the stream
        // completed while we were suspended
        let serverDelivered = serverMessages.filter { $0.status == .delivered && $0.sender == .herald }
        let localDelivered = localMessages.filter { $0.status == .delivered && $0.sender == .herald }

        if serverDelivered.count > localDelivered.count {
            // Server has the response — merge and clear streaming state
            conversation = mergeConversationMetadata(from: conversation, into: refreshed)

            // Clear all active streams
            for (jobID, _) in activeStreams {
                activeStreams.removeValue(forKey: jobID)
            }
            streamingTask?.cancel()
            streamingTask = nil
            chatLiveActivity.endActivity()
            pendingMessageSentAt = nil

            if let latestUsage = conversation?.latestUsage {
                lastTokenUsage = latestUsage
            }
            if let conversation {
                persistence.saveConversationCache(conversation)
                onConversationChanged?()
            }
        }
    }

    func cancelStreaming() {
        streamingTask?.cancel()
        streamingTask = nil
        chatLiveActivity.endActivity()
        ttsService?.stop()

        // Flush any buffered deltas onto the placeholder before finalizing.
        if let sid = streamingMessageID {
            flushPendingDeltas(placeholderID: sid)
        }

        // Finalize current streaming message with content received so far
        if let sid = streamingMessageID,
           var conv = conversation,
           let idx = conv.messages.firstIndex(where: { $0.id == sid }) {
            conv.messages[idx].isStreaming = false
            conv.messages[idx].status = .delivered
            for i in conv.messages[idx].toolActivities.indices {
                conv.messages[idx].toolActivities[i].isActive = false
            }
            conversation = conv
        }
        activeStreams.removeAll()
        pendingMessageSentAt = nil

        if let conversation {
            persistence.saveConversationCache(conversation)
            onConversationChanged?()
        }
    }

    func injectVoiceTranscript(voiceSessionId: UUID, duration: TimeInterval) async {
        do {
            let updated = try await heraldClient.injectVoiceTranscript(voiceSessionId: voiceSessionId)
            conversation = updated
            lastTokenUsage = updated.latestUsage

            // Set voiceSessionDuration on the system banner message
            if let idx = conversation?.messages.lastIndex(where: {
                $0.sender == .system && $0.content.contains("[Voice session ended]")
            }) {
                conversation?.messages[idx].voiceSessionDuration = duration
            }

            if let conversation {
                persistence.saveConversationCache(conversation)
                onConversationChanged?()
            }
        } catch {
            // Injection failed — voice transcript not added to chat. Non-fatal.
        }
    }

    func exportConversationToFile() {
        guard let conversation else { return }

        let formatter = DateFormatter()
        formatter.dateFormat = "yyyyMMdd_HHmmss"
        let timestamp = formatter.string(from: Date())
        let filename = "herald_conversation_\(timestamp).json"

        let exportData: [String: Any] = [
            "title": conversation.title,
            "sessionId": conversation.id.uuidString,
            "exportedAt": ISO8601DateFormatter().string(from: Date()),
            "messageCount": conversation.messages.count,
            "messages": conversation.messages.map { msg in
                [
                    "role": msg.sender.rawValue,
                    "content": msg.content,
                    "timestamp": ISO8601DateFormatter().string(from: msg.timestamp),
                ] as [String: String]
            },
        ]

        guard let dir = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first else { return }
        let fileURL = dir.appendingPathComponent(filename)

        do {
            let data = try JSONSerialization.data(withJSONObject: exportData, options: [.prettyPrinted, .sortedKeys])
            try data.write(to: fileURL)
            // Append a system message confirming the save (caller handles this)
        } catch {
            // Export failed silently — caller can check
        }
    }

    func setConversationTitle(_ title: String) {
        conversation?.title = title
        if let conversation {
            persistence.saveConversationCache(conversation)
            onTitleChanged?(conversation.id, title)
            onConversationChanged?()
        }
    }

    private func autoTitleIfNeeded() async {
        let defaultTitles: Set<String> = ["New Chat", "Herald"]
        guard let conv = conversation,
              defaultTitles.contains(conv.title),
              !autoTitleAttempted,
              let firstUserMessage = conv.messages.first(where: { $0.sender == .user })
        else { return }
        let raw = firstUserMessage.content.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !raw.isEmpty else { return }

        autoTitleAttempted = true

        // Try LLM-generated title with timeout and retry
        let assistantContent = conv.messages.first(where: { $0.sender == .herald })?.content ?? ""
        let generated = await generateTitleWithRetry(
            sessionId: conv.id,
            userMessage: String(raw.prefix(500)),
            assistantMessage: String(assistantContent.prefix(500))
        )
        if let generated {
            // Re-verify title is still a default (user may have renamed during RPC)
            if let current = conversation, defaultTitles.contains(current.title) {
                conversation?.title = generated
                onTitleChanged?(current.id, generated)
            }
            return
        }

        // Deterministic local fallback: truncated first message
        let title = raw.count > 60 ? String(raw.prefix(57)) + "..." : raw
        do {
            _ = try await heraldClient.renameSession(id: conv.id, title: title)
            if let current = conversation, defaultTitles.contains(current.title) {
                conversation?.title = title
                onTitleChanged?(current.id, title)
            }
        } catch {
            Self.logger.warning("Auto-title rename failed for session \(conv.id): \(error.localizedDescription)")
            appendLog(level: .warn, "Auto-title rename failed: \(error.localizedDescription)")
        }
    }

    /// Attempt to generate a title via RPC with a 5-second timeout and up to 2 attempts.
    /// Returns nil on failure (all attempts exhausted or timeout).
    private func generateTitleWithRetry(sessionId: UUID, userMessage: String, assistantMessage: String) async -> String? {
        let maxAttempts = 2
        let timeoutSeconds: TimeInterval = 5

        for attempt in 1...maxAttempts {
            let title: String? = await withCheckedContinuation { continuation in
                let task = Task { @MainActor in
                    do {
                        let result = try await self.heraldClient.generateSessionTitle(
                            sessionId: sessionId,
                            userMessage: userMessage,
                            assistantMessage: assistantMessage
                        )
                        guard !Task.isCancelled else {
                            continuation.resume(returning: nil)
                            return
                        }
                        continuation.resume(returning: result)
                    } catch {
                        continuation.resume(returning: nil)
                    }
                }
                // Timeout: cancel the RPC task if it hasn't completed
                Task { @MainActor in
                    try? await Task.sleep(for: .seconds(timeoutSeconds))
                    task.cancel()
                }
            }
            if let title { return title }
            Self.logger.warning("Title RPC attempt \(attempt)/\(maxAttempts) failed for session \(sessionId)")
        }
        Self.logger.error("Title RPC failed after \(maxAttempts) attempts for session \(sessionId)")
        return nil
    }

    func deleteMessage(_ message: Message) {
        conversation?.messages.removeAll { $0.id == message.id }
    }

    func retryMessage(_ message: Message) async {
        // Remove the failed message
        conversation?.messages.removeAll { $0.id == message.id }

        // Determine the user content to retry (attachments can't be recovered from metadata)
        let sourceMessage: Message?
        if message.sender == .user {
            sourceMessage = message
        } else {
            sourceMessage = conversation?.messages.last(where: { $0.sender == .user })
        }

        guard let sourceMessage else { return }
        let attachments = sourceMessage.attachments.compactMap(PendingAttachment.restore)
        let content = normalizedRetryContent(for: sourceMessage)
        guard !content.isEmpty || !attachments.isEmpty else { return }

        // Reuse the original clientMessageId so the server can deduplicate
        await sendMessage(content, attachments: attachments, clientMessageID: sourceMessage.clientMessageID)
    }

    func setPollingEnabled(_ isEnabled: Bool) {
        isPollingEnabled = isEnabled
        if isEnabled {
            restartPendingPollingIfNeeded()
        } else {
            pollingTask?.cancel()
            pollingTask = nil
        }
    }

    func replaceCommandCatalog(_ catalog: [SlashCommand], activeModel: String? = nil, contextWindow: Int? = nil) {
        commandCatalog = catalog.isEmpty ? SlashCommand.allBuiltIn : catalog
        if let activeModel { activeModelName = activeModel }
        if let contextWindow { self.contextWindow = contextWindow }
    }

    func resetCommandCatalog() {
        commandCatalog = SlashCommand.allBuiltIn
        activeModelName = nil
        contextWindow = nil
    }

    /// Append a log entry to the live log buffer shown in the iPad
    /// inspector panel's Logs tab. Capped at 500 entries.
    func appendLog(level: LogLevel, _ message: String) {
        logEntries.append(LogEntry(level: level, message: message))
        if logEntries.count > 500 { logEntries.removeFirst(100) }
    }

    func reset() {
        pollingTask?.cancel()
        pollingTask = nil
        streamingTask?.cancel()
        streamingTask = nil
        activeStreams.removeAll()
        logEntries = []
        isPollingEnabled = false
        resetCommandCatalog()
        conversation = nil
        isLoading = false
        pendingMessageSentAt = nil
        lastTokenUsage = nil
        lastContextInfo = nil
        persistence.clearConversationCache()
    }

    func resolvedContextWindow(fallbackModelName: String?) -> Int? {
        contextWindow ?? Self.inferredContextWindow(for: fallbackModelName)
    }

    private var hasPendingMessages: Bool {
        conversation?.messages.contains(where: { $0.sender == .user && $0.status == .sending }) == true
    }

    private func hasPendingDuplicateMessage(_ content: String, attachments: [PendingAttachment]) -> Bool {
        conversation?.messages.contains(where: {
            $0.sender == .user
                && $0.status == .sending
                && normalizedRetryContent(for: $0) == content
                && attachmentSignature(for: $0.attachments) == attachmentSignature(for: attachments.map { MessageAttachment(from: $0) })
        }) == true
    }

    // MARK: - Delta coalescing

    private func enqueueDelta(_ delta: String, placeholderID: UUID) {
        guard !delta.isEmpty else { return }
        var buf = deltaBuffers[placeholderID] ?? DeltaBuffer()
        buf.chunks.append(delta)
        buf.bytes += delta.utf8.count

        // If we've buffered a lot, flush immediately so the UI doesn't fall
        // multiple frames behind during a burst.
        if buf.bytes >= Self.deltaFlushByteThreshold {
            deltaBuffers[placeholderID] = buf
            flushPendingDeltas(placeholderID: placeholderID)
            return
        }

        guard buf.flushTask == nil else {
            deltaBuffers[placeholderID] = buf
            return
        }
        buf.flushTask = Task { [weak self, placeholderID] in
            try? await Task.sleep(for: Self.deltaFlushInterval)
            guard !Task.isCancelled else { return }
            await MainActor.run {
                self?.flushPendingDeltas(placeholderID: placeholderID)
            }
        }
        deltaBuffers[placeholderID] = buf
    }

    private func flushPendingDeltas(placeholderID: UUID) {
        guard var buf = deltaBuffers[placeholderID] else { return }
        buf.flushTask?.cancel()
        buf.flushTask = nil

        guard !buf.chunks.isEmpty else {
            deltaBuffers.removeValue(forKey: placeholderID)
            return
        }
        let chunks = buf.chunks
        let totalBytes = buf.bytes
        deltaBuffers.removeValue(forKey: placeholderID)

        guard var conv = conversation,
              let idx = conv.messages.firstIndex(where: { $0.id == placeholderID })
        else { return }

        // Single concat: O(sum(chunk sizes)) instead of O(n·chunks) across ticks.
        var buffer = conv.messages[idx].content
        let beforeCount = buffer.count
        buffer.reserveCapacity(buffer.count + totalBytes)
        for chunk in chunks { buffer.append(chunk) }
        conv.messages[idx].content = buffer
        Self.logger.debug("flush deltas chunks=\(chunks.count) bytes=\(totalBytes) content \(beforeCount)→\(buffer.count) chars")

        // Only touch tool-activity state when it actually needs clearing —
        // avoids spurious writes on every delta for messages that never ran tools.
        if conv.messages[idx].toolActivity != nil {
            conv.messages[idx].toolActivity = nil
        }
        var toolActivities = conv.messages[idx].toolActivities
        var didClearActive = false
        for i in toolActivities.indices where toolActivities[i].isActive {
            toolActivities[i].isActive = false
            didClearActive = true
        }
        if didClearActive {
            conv.messages[idx].toolActivities = toolActivities
        }

        conversation = conv
    }

    // Exponential backoff delays (seconds). The first polls are fast because
    // the relay usually delivers within a handful of seconds; later polls
    // spread out so we don't hammer a struggling relay. Polling is a low-frequency
    // safety net — it must never override a nonterminal server job.
    private static let pollingBackoffSeconds: [Double] = [
        2, 3, 5, 8, 12, 18, 25, 30, 30, 30, 30, 30,
    ]

    private func restartPendingPollingIfNeeded() {
        guard isPollingEnabled, hasPendingMessages else {
            pollingTask?.cancel()
            pollingTask = nil
            return
        }

        guard pollingTask == nil else { return }

        pollingTask = Task { [weak self] in
            guard let self else { return }

            for delay in Self.pollingBackoffSeconds {
                try? await Task.sleep(for: .seconds(delay))
                guard !Task.isCancelled else { break }
                let fresh = await self.refreshActiveConversation()
                self.conversation = self.mergeConversationMetadata(from: self.conversation, into: fresh)
                if let latestUsage = self.conversation?.latestUsage {
                    self.lastTokenUsage = latestUsage
                }
                if let conversation = self.conversation {
                    self.persistence.saveConversationCache(conversation)
                    self.onConversationChanged?()
                }
                if self.hasPendingMessages == false {
                    self.pendingMessageSentAt = nil
                    break
                }
            }
            // Polling exhausted — do NOT mark messages as failed.
            // The job may still be running on the server. The user can
            // see the sending state and choose to retry manually.

            if self.pollingTask?.isCancelled == false {
                self.pollingTask = nil
            }
        }
    }

    /// Re-attaches transient streaming artifacts (tool timeline, code diff) onto the
    /// canonical conversation that the relay returned, since the relay knows nothing
    /// about those client-only fields.
    /// Refreshes `conversation` from the relay. When a specific conversation/session
    /// is already active, refreshes THAT conversation by id — never the device's
    /// arbitrary "current" conversation, which (now that a device can have many
    /// sessions) may silently resolve to an unrelated session and clobber the one
    /// actually on screen.
    private func refreshActiveConversation() async -> Conversation? {
        if let activeID = conversation?.id {
            return try? await heraldClient.loadConversation(id: activeID)
        }
        return await heraldClient.loadConversation()
    }

    private func mergeConversationMetadata(
        from localConversation: Conversation?,
        into refreshedConversation: Conversation?
    ) -> Conversation? {
        guard var refreshedConversation else { return localConversation }
        guard let localConversation else { return refreshedConversation }

        // Preserve user-set titles — only accept the server's title if the local
        // title is still a default placeholder. This prevents a late server-derived
        // title from overwriting a user rename.
        let defaultTitles: Set<String> = ["New Chat", "Herald"]
        if !defaultTitles.contains(localConversation.title) {
            refreshedConversation.title = localConversation.title
        }

        if refreshedConversation.latestUsage == nil {
            refreshedConversation.latestUsage = localConversation.latestUsage
        }

        for index in refreshedConversation.messages.indices {
            let remote = refreshedConversation.messages[index]

            // Prefer exact UUID match (works when the relay echoes back the same ID).
            let local: Message?
            if let byID = localConversation.messages.first(where: { $0.id == remote.id }) {
                local = byID
            } else if let remoteClientMessageID = remote.clientMessageID {
                local = localConversation.messages.first(where: {
                    $0.id == remoteClientMessageID || $0.clientMessageID == remoteClientMessageID
                })
            } else if let remoteJobID = remote.jobID {
                // Fallback: the streaming placeholder had a client-generated UUID that
                // differs from the server-assigned message ID.  Match on jobID + sender
                // instead, but only for Herald messages that actually carry artifacts.
                local = localConversation.messages.first(where: {
                    $0.jobID == remoteJobID
                        && $0.sender == remote.sender
                        && $0.sender == .herald
                        && (!$0.toolActivities.isEmpty || $0.codeDiff != nil || !$0.reasoning.isEmpty)
                })
            } else {
                local = nil
            }

            guard let local else { continue }

            if !local.toolActivities.isEmpty {
                refreshedConversation.messages[index].toolActivities = local.toolActivities
                refreshedConversation.messages[index].toolActivity = local.toolActivity
            }

            if let diff = local.codeDiff, refreshedConversation.messages[index].codeDiff == nil {
                refreshedConversation.messages[index].codeDiff = diff
            }

            if !local.reasoning.isEmpty {
                refreshedConversation.messages[index].reasoning = local.reasoning
                if local.reasoningDuration != nil {
                    refreshedConversation.messages[index].reasoningDuration = local.reasoningDuration
                }
            }

            if !local.attachments.isEmpty {
                refreshedConversation.messages[index].attachments = mergeAttachments(
                    local.attachments,
                    onto: refreshedConversation.messages[index].attachments
                )
            }
        }

        // Preserve local-only streaming placeholders while the server is still
        // catching up, so polling doesn't make the active reply disappear.
        let refreshedIDs = Set(refreshedConversation.messages.map(\.id))
        let localStreamingPlaceholders = localConversation.messages.filter {
            $0.isStreaming && !refreshedIDs.contains($0.id)
        }
        refreshedConversation.messages.append(contentsOf: localStreamingPlaceholders)

        return refreshedConversation
    }

    private func mergeAttachments(_ localAttachments: [MessageAttachment], onto remoteAttachments: [MessageAttachment]) -> [MessageAttachment] {
        guard !remoteAttachments.isEmpty else { return localAttachments }

        return remoteAttachments.enumerated().map { index, remote in
            let match = localAttachments.first(where: {
                $0.fileName == remote.fileName && $0.mimeType == remote.mimeType
            }) ?? localAttachments[safe: index]
            guard let match else { return remote }
            return MessageAttachment(
                id: remote.id,
                kind: remote.kind,
                fileName: remote.fileName,
                mimeType: remote.mimeType,
                thumbnailBase64: remote.thumbnailBase64 ?? match.thumbnailBase64,
                localStoragePath: match.localStoragePath
            )
        }
    }

    private func normalizedRetryContent(for message: Message) -> String {
        if !message.attachments.isEmpty,
           message.content.range(of: #"^\[\d+ attachment"#, options: .regularExpression) != nil {
            return ""
        }
        return message.content.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func attachmentSignature(for attachments: [MessageAttachment]) -> String {
        attachments
            .map { "\($0.kind)|\($0.fileName)|\($0.mimeType)" }
            .sorted()
            .joined(separator: "||")
    }

    // MARK: - Profile Switch Detection

    /// Detect a profile switch from the agent's response text.
    /// Updates the active profile name on ProfileStore immediately so the
    /// toolbar chip reflects the change in the same render frame.
    private func detectProfileSwitch(in text: String) {
        let patterns: [Regex<(Substring, Substring)>] = [
            /[Ss]witched\s+(?:to\s+)?profile\s+["'`]?(\w+)["'`]?/,
            /[Cc]hanged\s+(?:to\s+)?profile\s+["'`]?(\w+)["'`]?/,
            /[Aa]ctivated\s+(?:profile\s+)?["'`]?(\w+)["'`]?\s+profile/,
            /[Pp]rofile\s+switched\s+(?:to\s+)?["'`]?(\w+)["'`]?/,
            /[Pp]rofile\s+["'`]?(\w+)["'`]?\s+activated/,
        ]
        for pattern in patterns {
            if let match = text.firstMatch(of: pattern) {
                let profileName = String(match.1)
                profileStore?.markActive(profileName)
                return
            }
        }
    }

    /// Fallback-only lookup for cases where the connector has not yet provided
    /// an explicit context window. This should never overwrite a known value.
    static func inferredContextWindow(for modelName: String?) -> Int? {
        guard let modelName, !modelName.isEmpty else { return nil }
        let n = modelName.lowercased()

        if n.contains("claude-opus-4-6") || n.contains("claude-opus-4.6")
            || n.contains("claude-sonnet-4-6") || n.contains("claude-sonnet-4.6") {
            return 1_000_000
        }
        if n.contains("claude") { return 200_000 }
        if n.contains("gpt-4.1") { return 1_047_576 }
        if n.contains("gpt-5") { return 128_000 }
        if n.contains("gpt-4") { return 128_000 }
        if n.contains("gemini") { return 1_048_576 }
        if n.contains("gemma-4-31b") || n.contains("gemma-4-26b") { return 256_000 }
        if n.contains("gemma-3") { return 131_072 }
        if n.contains("gemma") { return 8_192 }
        if n.contains("deepseek") { return 128_000 }
        if n.contains("llama") { return 131_072 }
        if n.contains("qwen") { return 131_072 }
        if n.contains("minimax") { return 204_800 }
        if n.contains("glm") { return 202_752 }
        if n.contains("kimi") { return 262_144 }
        if n.contains("mimo-v2-pro") || n.contains("mimo-v2-omni") { return 1_048_576 }
        return 128_000
    }
}

private extension Array {
    subscript(safe index: Int) -> Element? {
        guard indices.contains(index) else { return nil }
        return self[index]
    }
}
