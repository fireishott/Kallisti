import Foundation
import os
import UIKit
import UserNotifications

/// Indicates the current phase of the streaming pipeline for UI feedback.
enum StreamingPhase: Sendable {
    case idle
    case sending            // POST /messages — waiting for relay to accept
    case waitingForJob      // Job accepted, waiting for first event (connector warming up)
    case streaming          // Receiving text/reasoning/tool deltas
    case reconnecting       // Transport dropped — cursor-based resume in progress
    case stalled            // Watchdog is about to fire — showing "Waiting…"
}

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
                autoCompressAttempted = false
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
    /// Streaming phase for UI indicators (e.g. "Sending…", "Waiting…", "Streaming…").
    /// Updated by `runStreamingAttempt` as the job progresses through the SSE pipeline.
    var streamingPhase: StreamingPhase = .idle
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
    ///
    /// Set to 60s — large models can take 30-45s to load/prefill before the
    /// first token, and the connector has its own 120s watchdog. 30s was too
    /// tight for real-world usage with local models on constrained hardware.
    /// If no text/reasoning/tool/finished event arrives within this window,
    /// the job is treated as stalled and parallel polling begins.
    static var watchdogTimeout: Duration = .seconds(90)

    /// Absolute deadline for a single streaming job from message acceptance to
    /// terminal resolution. After this duration, the polling loop forcibly
    /// resolves the placeholder to a timeout failure, even if heartbeats are
    /// still arriving. This prevents the "infinite thinking" bug where a hung
    /// upstream model keeps the job alive via heartbeats forever.
    /// Mirrors the relay's max_job_duration_seconds.
    static var absoluteJobDeadline: Duration = .seconds(180)

    /// Timestamp of the last streaming progress signal. Updated on every
    /// textDelta, reasoningDelta, toolActivity, keepalive, and messageSent.
    /// The continuous watchdog checks this to detect mid-stream stalls.
    private var streamingProgressAt: Date = .now

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
    private var reasoningBuffers: [UUID: DeltaBuffer] = [:]
    private static let deltaFlushInterval: Duration = .milliseconds(16)  // 60 fps cap
    private static let deltaFlushByteThreshold = 4_096

    /// Whether `autoTitleIfNeeded` has already been attempted for the current
    /// conversation. Prevents re-attempting on every stream completion when
    /// the title RPC fails and the title remains a default placeholder.
    private var autoTitleAttempted = false

    /// Set by `clearConversation()` to force the next `loadConversationIfNeeded()`
    /// to bypass the local cache and fetch fresh data from the relay. Prevents
    /// the /new bug where a stale cached conversation survives the clear.
    private var needsServerRefresh = false

    var isStreaming: Bool { streamingMessageID != nil }
    var connectionStatus: ConnectionStatus { heraldClient.connectionStatus }

    func updateConnectionStatus(_ status: ConnectionStatus) {
        heraldClient.connectionStatus = status
    }

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

    var heraldClient: any HeraldClientProtocol
    private let chatLiveActivity = LiveActivityService()
    let persistence: any AppPersistenceStoreProtocol

    /// TTS service for speaking responses during/after streaming.
    @ObservationIgnored var ttsService: (any TTSServiceProtocol)?
    /// Provides current TTS settings (enabled, voice, autoSpeak, autoSpeakDuringStreaming, appleVoiceIdentifier).
    @ObservationIgnored var ttsSettingsProvider: (@MainActor () -> (enabled: Bool, voice: String, autoSpeak: Bool, autoSpeakDuringStreaming: Bool, appleVoiceIdentifier: String))?

    /// Called when conversation content changes (new message, streaming complete).
    /// Used by AppContainer to push widget data updates.
    var onConversationChanged: (@MainActor () -> Void)?

    /// Called when the conversation title changes (server-derived or renamed).
    /// Used by SessionListStore to update sidebar immediately.
    var onTitleChanged: (@MainActor (_ conversationID: UUID, _ newTitle: String) -> Void)?
    var useStreaming: Bool = false

    /// Maximum number of log entries to keep in memory and on disk.
    private static let maxLogEntries = 500

    init(heraldClient: any HeraldClientProtocol, persistence: any AppPersistenceStoreProtocol) {
        self.heraldClient = heraldClient
        self.persistence = persistence
        // Restore persisted logs so the Logs tab isn't empty on launch.
        if let persisted = persistence.loadLogEntries(), !persisted.isEmpty {
            logEntries = persisted
        } else {
            logEntries = [LogEntry(level: .info, message: "Herald started — waiting for activity")]
        }
    }

    func loadConversationIfNeeded() async {
        if conversation == nil {
            conversation = persistence.loadConversationCache()
            // IMPORTANT: Do NOT trust cached contextPercent or latestUsage —
            // they're stale by definition (the model's context window may have
            // changed, a different model may be active, or the relay may report
            // completely different usage after the next response). Restoring
            // stale usage causes fabricated "Session nearly full" banners on
            // first launch and after session switches.
            conversation?.contextPercent = nil
            conversation?.latestUsage = nil
        }
        // After clearConversation(), bypass the local cache and force a
        // server fetch so the UI never shows the stale archived conversation.
        guard conversation == nil || needsServerRefresh else { return }
        needsServerRefresh = false
        await loadConversation()
        clearNotificationsForCurrentConversation()
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
            // Strip transient relay-reported fields before caching so stale
            // context percent / token usage never survive a relaunch.
            var cacheCopy = conversation
            cacheCopy.contextPercent = nil
            cacheCopy.latestUsage = nil
            persistence.saveConversationCache(cacheCopy)
            onConversationChanged?()
        }
        restartPendingPollingIfNeeded()
        clearNotificationsForCurrentConversation()
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
            // Strip transient relay-reported fields before caching so stale
            // context percent / token usage never survive a relaunch.
            var cacheCopy = conversation
            cacheCopy.contextPercent = nil
            cacheCopy.latestUsage = nil
            persistence.saveConversationCache(cacheCopy)
            onConversationChanged?()
        }
    }

    /// Drives a single streaming attempt for an outgoing message.
    ///
    /// If the SSE stream stalls (no progress events within the watchdog window)
    /// we start parallel HTTP polling rather than failing the message.  The
    /// relay owns retries via leases, so the client never resubmits the same
    /// message — it just keeps waiting until the relay resolves the job.
    ///
    /// Only an explicit ``.failed`` event from the relay or exceeding the
    /// absolute job deadline causes a "tap to retry" error to appear.
    /// A stalled stream is a transport concern, not a failure.
    private func runAttemptLoop(
        content: String,
        attachments: [PendingAttachment],
        clientMessageID: UUID,
        placeholderID: UUID
    ) async {
        let jobAcceptedAt = Date.now
        let stalled = await runStreamingAttempt(
            content: content,
            attachments: attachments,
            clientMessageID: clientMessageID,
            placeholderID: placeholderID
        )

        // If the stream completed normally (including explicit .failed), done.
        guard stalled else { return }

        // — Stream stalled — start parallel polling —
        streamingPhase = .stalled
        if let idx = conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
            conversation?.messages[idx].toolActivity = "Waiting for host..."
        }

        // Poll until the job resolves or the absolute deadline is exceeded.
        // Heartbeats can keep a hung upstream job alive forever; this deadline
        // guarantees a terminal user-visible state regardless of connector/relay
        // behavior.
        var pollCount = 0
        while !Task.isCancelled {
            // Check absolute deadline first
            let elapsed = Date.now.timeIntervalSince(jobAcceptedAt)
            let deadlineSeconds = Self.absoluteJobDeadline / .seconds(1)
            if elapsed >= deadlineSeconds {
                appendLog(level: .warn, "Job exceeded absolute deadline (\(Int(elapsed))s) — timing out")
                flushPendingReasoning(placeholderID: placeholderID)
                flushPendingDeltas(placeholderID: placeholderID)
                if let idx = conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
                    conversation?.messages[idx] = Message(
                        sender: .system,
                        content: failureMessage(for: "timeout"),
                        status: .failed,
                        errorCategory: "timeout"
                    )
                }
                if let idx = conversation?.messages.firstIndex(where: { $0.id == clientMessageID }) {
                    conversation?.messages[idx].status = .sending  // user message is retryable
                }
                activeStreams.removeAll()
                streamingPhase = .idle
                chatLiveActivity.endActivity()
                pendingMessageSentAt = nil
                return
            }

            try? await Task.sleep(for: .seconds(10))
            pollCount += 1

            let refreshed = await refreshActiveConversation()
            conversation = mergeConversationMetadata(from: conversation, into: refreshed)

            // Check if the placeholder was resolved by a late SSE event or polling
            if let msg = conversation?.messages.first(where: { $0.id == placeholderID }) {
                if msg.status == .delivered || msg.status == .failed {
                    streamingPhase = .idle
                    chatLiveActivity.endActivity()
                    return
                }
                if !msg.content.isEmpty && msg.status != .sending {
                    streamingPhase = .idle
                    chatLiveActivity.endActivity()
                    return
                }
            }

            // Check whether the original user message was marked failed
            if let userMsg = conversation?.messages.first(where: { $0.id == clientMessageID }),
               userMsg.status == .failed {
                streamingPhase = .idle
                chatLiveActivity.endActivity()
                return
            }

            // Update waiting indicator with elapsed time
            if let idx = conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
                let elapsedSecs = Int(Date.now.timeIntervalSince(jobAcceptedAt))
                if elapsedSecs > 0 {
                    conversation?.messages[idx].toolActivity = "Waiting... (\(elapsedSecs)s)"
                }
            }
        }
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

        streamingPhase = .sending

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
                    self.streamingPhase = .waitingForJob
                    self.activeStreams[jobID] = placeholderID
                    // Arm the polling safety net. If the SSE stream fails silently
                    // (transport drop, proxy timeout, connector stall), polling
                    // recovers the response so the user isn't stuck staring at a
                    // blank screen. When streaming delivers normally the pending
                    // message is already resolved, so polling becomes a no-op.
                    needsPollingFallback = true
                    // Start Live Activity with "Thinking" phase — the agent is
                    // processing but hasn't begun streaming content yet.
                    self.chatLiveActivity.startThinking()
                    // Show a phase indicator on the streaming placeholder so the
                    // user knows the app isn't frozen during long model loads.
                    if var conv = self.conversation,
                       let idx = conv.messages.firstIndex(where: { $0.id == placeholderID }) {
                        conv.messages[idx].toolActivity = "Model loading…"
                        self.conversation = conv
                    }
                    // Yield progress — the relay accepting the job IS proof the
                    // connection is alive. This keeps the continuous watchdog
                    // satisfied during long model loads (30-45s prefill on
                    // constrained hardware).
                    self.streamingProgressAt = .now
                    progressContinuation?.yield(())

                case .textDelta(let delta):
                    self.streamingProgressAt = .now
                    progressContinuation?.yield(())
                    self.streamingPhase = .streaming
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
                    self.streamingProgressAt = .now
                    progressContinuation?.yield(())
                    self.chatLiveActivity.updatePhase("Thinking")
                    if reasoningStartedAt == nil { reasoningStartedAt = .now }
                    self.enqueueReasoningDelta(delta, placeholderID: placeholderID)

                case .toolActivity(let label):
                    self.streamingProgressAt = .now
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
                    // Transport keepalives prove the connection is alive but do
                    // NOT prove the model is making progress. Do not reset the
                    // watchdog — only text/reasoning/tool events count.
                    break

                case .finished(let finalMessage, let usage, let diff, let context):
                    progressContinuation?.yield(())
                    Self.logger.info("stream finished content=\(finalMessage.content.count) chars")
                    self.flushPendingReasoning(placeholderID: placeholderID)
                    self.flushPendingDeltas(placeholderID: placeholderID)
                    if let idx = self.conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
                        let placeholder = self.conversation?.messages[idx]
                        let activities = placeholder?.toolActivities ?? []
                        let streamedReasoning = placeholder?.reasoning ?? ""
                        // A terminal event carrying empty content must never erase
                        // text that already streamed into the placeholder. Build 41
                        // dropped this guard (it arrived in B35 as `c5069af`), which
                        // re-opened the blank-bubble regression.
                        let streamedContent = placeholder?.content ?? ""
                        var resolved = Self.mergeResolvedMessage(
                            resolved: finalMessage,
                            streamedContent: streamedContent
                        )
                        resolved.toolActivities = activities
                        resolved.codeDiff = diff
                        // Priority for reasoning:
                        // 1) finalMessage.reasoning — set by LiveHeraldClient (SSE terminal
                        //    reasoning from the done payload, or splitThinkingBlocks extraction).
                        // 2) placeholder's streamed reasoning — from reasoningDelta SSE events,
                        //    only used when finalMessage has no reasoning of its own.
                        // 3) regex extraction from content — last resort for models that embed
                        //    <think> tags inline without a separate reasoning field.
                        if resolved.reasoning.isEmpty && !streamedReasoning.isEmpty {
                            resolved.reasoning = streamedReasoning
                            if let startedAt = reasoningStartedAt {
                                resolved.reasoningDuration = Date().timeIntervalSince(startedAt)
                            }
                        } else if !streamedReasoning.isEmpty {
                            // finalMessage already has reasoning — keep it but carry over
                            // the duration from the streamed placeholder
                            if resolved.reasoningDuration == nil, let startedAt = reasoningStartedAt {
                                resolved.reasoningDuration = Date().timeIntervalSince(startedAt)
                            }
                        }
                        // Last resort: regex extraction for models that embed reasoning
                        // as XML tags inline in the content (DeepSeek <think>, Qwen <thinking>).
                        // splitThinkingBlocks in LiveHeraldClient handles this on the sync
                        // path; this is the SSE-path safety net.
                        if resolved.reasoning.isEmpty {
                            if let thinkRegex = try? NSRegularExpression(
                                pattern: "<think(?:ing)?>(.*?)</think(?:ing)?>",
                                options: [.dotMatchesLineSeparators, .caseInsensitive]
                            ) {
                                let nsContent = resolved.content as NSString
                                let matches = thinkRegex.matches(
                                    in: resolved.content,
                                    range: NSRange(location: 0, length: nsContent.length)
                                )
                                let extracted = matches.compactMap { match -> String? in
                                    guard match.numberOfRanges > 1 else { return nil }
                                    return nsContent.substring(with: match.range(at: 1))
                                }.joined(separator: "\n")
                                if !extracted.isEmpty {
                                    resolved.reasoning = extracted.trimmingCharacters(in: .whitespacesAndNewlines)
                                    if let startedAt = reasoningStartedAt {
                                        resolved.reasoningDuration = Date().timeIntervalSince(startedAt)
                                    }
                                }
                            }
                            // Also try unclosed <think> tags (model interrupted mid-reasoning)
                            if resolved.reasoning.isEmpty {
                                if let unclosedRegex = try? NSRegularExpression(
                                    pattern: "<think(?:ing)?>([\\s\\S]*?)$",
                                    options: [.caseInsensitive]
                                ) {
                                    let nsContent = resolved.content as NSString
                                    if let match = unclosedRegex.firstMatch(
                                        in: resolved.content,
                                        range: NSRange(location: 0, length: nsContent.length)
                                    ), match.numberOfRanges > 1 {
                                        let extracted = nsContent.substring(with: match.range(at: 1))
                                            .trimmingCharacters(in: .whitespacesAndNewlines)
                                        if !extracted.isEmpty {
                                            resolved.reasoning = extracted
                                        }
                                    }
                                }
                            }
                        }
                        // Always strip <think>…</think> (and <thinking>…</thinking>)
                        // tags from the visible content.
                        if let regex = try? NSRegularExpression(pattern: "<think(?:ing)?>.*?</think(?:ing)?>", options: [.dotMatchesLineSeparators, .caseInsensitive]) {
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
                    await self.detectProfileSwitch(in: finalMessage.content)
                    if let jobID = acceptedJobID { self.activeStreams.removeValue(forKey: jobID) }
                    self.pendingMessageSentAt = nil
                    self.chatLiveActivity.endActivity()
                    self.streamingPhase = .idle

                    // Haptic feedback on response completion — fired immediately
                    // in the stream handler so it's synchronous with the content
                    // appearing, not delayed by the ChatScreen's onChange observer.
                    HapticEngine.responseReceived()

                    // Finish TTS streaming — flush any remaining buffered text
                    self.ttsService?.finishStream()
                    // Notify if merge changed the title (server-derived title)
                    if let conv = self.conversation, conv.title != oldTitle {
                        self.onTitleChanged?(conv.id, conv.title)
                    }
                    await self.autoTitleIfNeeded()

                    // Auto-compress when context exceeds 85% — sends /compress
                    // as a system directive so the user doesn't have to hit the
                    // banner button manually.
                    if let pct = self.conversation?.contextPercent, pct > 85.0 {
                        await self.autoCompress()
                    }

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
                            identifier: "herald-response-\(finalMessage.id.uuidString)",
                            content: content,
                            trigger: nil
                        )
                        try? await UNUserNotificationCenter.current().add(request)
                    }

                case .started(let phase):
                    self.appendLog(level: .info, "Job started — phase: \(phase)")
                    progressContinuation?.yield(())
                    self.chatLiveActivity.updateToolProgress(phase)
                    // Update the placeholder to show what the model is doing
                    if var conv = self.conversation,
                       let idx = conv.messages.firstIndex(where: { $0.id == placeholderID }) {
                        conv.messages[idx].toolActivity = phase
                        self.conversation = conv
                    }

                case .heartbeat(let phase):
                    // Heartbeat proves the connector process is alive, but does
                    // NOT prove the model is making progress. Do NOT reset the
                    // watchdog — keepalive/transport liveness alone must not
                    // prevent the stall detector from firing.
                    self.appendLog(level: .debug, "Job heartbeat — phase: \(phase)")

                case .reconnecting:
                    self.appendLog(level: .warn, "Stream reconnecting...")
                    self.streamingPhase = .reconnecting
                    // Reconnection attempts are transport recovery, not model
                    // progress. Do not reset the watchdog.
                    if var conv = self.conversation,
                       let idx = conv.messages.firstIndex(where: { $0.id == placeholderID }) {
                        conv.messages[idx].toolActivity = "Reconnecting..."
                        self.conversation = conv
                    }
                    // Start polling immediately as a parallel recovery path.
                    // If the SSE stream is struggling to stay connected, the
                    // polling loop can pick up the response independently.
                    // When polling resolves the pending message the stream
                    // coordinator will naturally exit on its next reconnect.
                    self.restartPendingPollingIfNeeded()

                case .cancelled:
                    self.appendLog(level: .info, "Job cancelled")
                    progressContinuation?.yield(())
                    self.flushPendingReasoning(placeholderID: placeholderID)
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
                    self.streamingPhase = .idle
                    if let idx = self.conversation?.messages.firstIndex(where: { $0.id == clientMessageID }) {
                        self.conversation?.messages[idx].status = .delivered
                    }
                    await self.autoTitleIfNeeded()

                case .failed(let errorMessage, let category, let action):
                    // An explicit failure is a real signal, not silence — let it
                    // resolve the watchdog race immediately rather than waiting
                    // out the timeout, and handle it exactly as before.
                    progressContinuation?.yield(())
                    self.flushPendingReasoning(placeholderID: placeholderID)
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
                    self.streamingPhase = .idle
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

        // Continuous watchdog — monitors progress at 5s intervals instead of
        // a one-shot race. This catches mid-stream stalls (e.g. SSE transport
        // drop after the first token), not just complete silence before the
        // first event. The old one-shot race would satisfy on the first
        // progress signal then never check again.
        //
        // Also enforces the absolute job deadline — even if heartbeats or
        // occasional text deltas keep arriving, the stream is terminated
        // after absoluteJobDeadline to prevent infinite hangs.
        self.streamingProgressAt = .now
        let jobAcceptedAt = Date.now
        var stallDetected = false
        while !consumerTask.isCancelled {
            // Check if the consumer finished while we were sleeping
            if streamingTask == nil { break }

            try? await Task.sleep(for: .seconds(5))

            // Absolute deadline check — terminate even if progress events
            // are still arriving, to prevent an infinite "Thinking..." hang
            // when heartbeats keep the job alive but the model never finishes.
            let wallElapsed = Date.now.timeIntervalSince(jobAcceptedAt)
            let wallDeadline = Self.absoluteJobDeadline / .seconds(1)
            if wallElapsed >= wallDeadline {
                appendLog(level: .warn, "SSE stream exceeded absolute deadline (\(Int(wallElapsed))s)")
                streamingPhase = .stalled
                stallDetected = true
                break
            }

            let elapsed = Duration.seconds(Date.now.timeIntervalSince(self.streamingProgressAt))

            if elapsed > Self.watchdogTimeout {
                self.streamingPhase = .stalled
                stallDetected = true
                break
            }
        }

        if stallDetected {
            // Progress stalled — do NOT cancel the consumer task; it may still
            // receive events. Signal the caller to start parallel polling.
            return true
        }

        await consumerTask.value
        streamingTask = nil

        // If streaming failed after the job was accepted, give the relay a grace
        // period to finish delivering events before falling back to polling.
        // SSE streams can end prematurely due to proxy timeouts or mobile network
        // transitions, but the relay may still be writing events. A 10-second
        // pause prevents the polling safety net from racing a healthy SSE stream.
        if needsPollingFallback {
            try? await Task.sleep(for: .seconds(10))
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
    func failureMessage(for category: String? = nil) -> String {
        let name = profileStore?.activeProfile?.name ?? "Herald"
        switch category {
        case "context_exceeded":
            return "Session too long. Start a new chat."
        case "rate_limited":
            return "\(name) is rate-limited. Wait and retry."
        case "timeout":
            return "\(name) took too long. Tap to retry."
        case "empty_response":
            return "\(name) returned an empty response. Tap to retry."
        default:
            return "\(name) didn't respond. Tap to retry."
        }
    }

    func clearConversation() async throws {
        streamingTask?.cancel()
        streamingTask = nil
        activeStreams.removeAll()
        chatLiveActivity.endActivity()
        // Zero out context immediately so the UI resets to 0% before the
        // server round-trip — prevents the ring lingering at 100% if the
        // server returns a fresh conversation that still carries stale usage.
        lastTokenUsage = nil
        lastContextInfo = nil

        let fresh: Conversation
        do {
            fresh = try await heraldClient.clearConversation()
        } catch {
            // If the relay is unreachable (502, network error, etc.), fall back
            // to a local-only clear. The user gets a blank conversation immediately
            // rather than an "internal error" dialog. The old conversation will
            // be archived on the relay next time it's reachable.
            Self.logger.warning("Relay clear failed, using local fallback: \(error.localizedDescription)")
            fresh = Conversation(title: "New Chat")
        }

        conversation = fresh
        lastTokenUsage = fresh.latestUsage
        lastContextInfo = nil
        pendingMessageSentAt = nil
        persistence.saveConversationCache(fresh)
        needsServerRefresh = true  // Force next loadConversationIfNeeded() to bypass cache
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
            flushPendingReasoning(placeholderID: sid)
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
            // Strip transient relay-reported fields before caching so stale
            // context percent / token usage never survive a relaunch.
            var cacheCopy = conversation
            cacheCopy.contextPercent = nil
            cacheCopy.latestUsage = nil
            persistence.saveConversationCache(cacheCopy)
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

    /// Automatically sends `/compress` when context exceeds 85%.
    /// Only triggers once per conversation to avoid compression loops.
    /// The compress directive is sent as a system message so the agent
    /// summarizes existing context and the user perceives a seamless
    /// continuation rather than a disruptive reload.
    private var autoCompressAttempted = false

    private func autoCompress() async {
        guard !autoCompressAttempted else { return }
        autoCompressAttempted = true
        Self.logger.info("Auto-compressing at \(self.conversation?.contextPercent ?? 0) context")

        // Reuse the same message-send pipeline; the agent handles /compress
        // natively, producing a summary then resuming normal conversation.
        let clientMessageID = UUID()
        let compressMsg = Message(
            id: clientMessageID,
            clientMessageID: clientMessageID,
            sender: .user,
            content: "/compress",
            status: .sending
        )
        if conversation == nil {
            conversation = Conversation(title: "New Chat")
        }
        conversation?.messages.append(compressMsg)
        conversation?.lastActivity = compressMsg.timestamp

        // Don't use streaming for compress — it's a fast system directive.
        let response = await heraldClient.send(
            message: "/compress",
            attachments: [],
            clientMessageID: clientMessageID
        )
        if let idx = conversation?.messages.firstIndex(where: { $0.id == clientMessageID }) {
            conversation?.messages[idx].status = .delivered
        }
        conversation?.messages.append(response)
        conversation?.lastActivity = response.timestamp
        // Usage comes from the current conversation metadata, not the Message object
        if let latestUsage = heraldClient.currentConversation?.latestUsage {
            lastTokenUsage = latestUsage
        }
        if let conversation {
            // Strip transient relay-reported fields before caching so stale
            // context percent / token usage never survive a relaunch.
            var cacheCopy = conversation
            cacheCopy.contextPercent = nil
            cacheCopy.latestUsage = nil
            persistence.saveConversationCache(cacheCopy)
            // Also reset in-memory state so the UI reflects post-compress.
            self.conversation?.contextPercent = nil
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

        // Deterministic local fallback: smart truncation of first message.
        // Strip leading slash commands and common prefixes, then use the
        // first meaningful line as the title.
        let cleaned = raw
            .replacingOccurrences(of: #"^/\w+\s*"#, with: "", options: .regularExpression)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let firstLine = cleaned.split(separator: "\n").first.map(String.init) ?? cleaned
        let title = firstLine.count > 50
            ? String(firstLine.prefix(47)).trimmingCharacters(in: .whitespaces) + "..."
            : firstLine
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
        let timeoutSeconds: TimeInterval = 12  // Relay has a 15s timeout; stay under it

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
        // Determine the user content to retry.
        let sourceMessage: Message?
        if message.sender == .user {
            sourceMessage = message
        } else {
            sourceMessage = conversation?.messages.last(where: { $0.sender == .user })
        }

        guard let sourceMessage else { return }
        let attachments = sourceMessage.attachments.compactMap(PendingAttachment.restore)
        var content = normalizedRetryContent(for: sourceMessage)
        guard !content.isEmpty || !attachments.isEmpty else { return }

        // If the failed message was an assistant response that had partial
        // content (truncated/incomplete), prepend a continuation hint so the
        // agent knows to pick up where it left off rather than restarting.
        if message.sender != .user && !message.content.isEmpty {
            // Only remove the failed assistant message — keep the user message.
            conversation?.messages.removeAll { $0.id == message.id }
            let tail = String(message.content.suffix(120))
            content = "[Your previous response was cut off. It ended with: \"\(tail)\". Continue from where you stopped.]\n\n\(content)"
        } else {
            // Failed user message or empty assistant response — remove and resend fresh.
            conversation?.messages.removeAll { $0.id == message.id }
        }

        // Always use a fresh clientMessageID so the relay processes this as a
        // new message rather than deduplicating against the failed attempt.
        await sendMessage(content, attachments: attachments, clientMessageID: UUID())
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
    /// inspector panel's Logs tab. Capped at 500 entries, persisted to disk.
    func appendLog(level: LogLevel, _ message: String) {
        logEntries.append(LogEntry(level: level, message: message))
        if logEntries.count > Self.maxLogEntries { logEntries.removeFirst(100) }
        // Persist on the next main-actor cycle so logging doesn't stall
        let snapshot = logEntries
        Task { @MainActor [persistence] in
            persistence.saveLogEntries(snapshot)
        }
    }

    func reset() {
        pollingTask?.cancel()
        pollingTask = nil
        streamingTask?.cancel()
        streamingTask = nil
        activeStreams.removeAll()
        // Preserve log entries across resets — they're diagnostic history,
        // not session state. Append a marker so the user can see where
        // one session ended and another began.
        appendLog(level: .info, "——— session reset ———")
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
        // Prefer relay-provided context window from SSE metadata
        if let ctx = contextWindow, ctx > 0 { return ctx }

        // If the ModelStore has context window info from the model catalog
        // (config.yaml providers), use that. Otherwise return nil rather than
        // fabricating a guess — never show made-up numbers.
        return nil
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

    /// Enqueue a reasoning delta into the reasoning buffer.
    /// Uses the same coalescing strategy as text deltas (33ms / 4KB) to avoid
    /// per-token `@Observable` mutations during deep reasoning phases.
    private func enqueueReasoningDelta(_ delta: String, placeholderID: UUID) {
        guard !delta.isEmpty else { return }
        var buf = reasoningBuffers[placeholderID] ?? DeltaBuffer()
        buf.chunks.append(delta)
        buf.bytes += delta.utf8.count

        if buf.bytes >= Self.deltaFlushByteThreshold {
            reasoningBuffers[placeholderID] = buf
            flushPendingReasoning(placeholderID: placeholderID)
            return
        }

        guard buf.flushTask == nil else {
            reasoningBuffers[placeholderID] = buf
            return
        }
        buf.flushTask = Task { [weak self, placeholderID] in
            try? await Task.sleep(for: Self.deltaFlushInterval)
            guard !Task.isCancelled else { return }
            await MainActor.run {
                self?.flushPendingReasoning(placeholderID: placeholderID)
            }
        }
        reasoningBuffers[placeholderID] = buf
    }

    /// Flush all buffered reasoning deltas onto the placeholder message.
    private func flushPendingReasoning(placeholderID: UUID) {
        guard var buf = reasoningBuffers[placeholderID] else { return }
        buf.flushTask?.cancel()
        buf.flushTask = nil

        guard !buf.chunks.isEmpty else {
            reasoningBuffers.removeValue(forKey: placeholderID)
            return
        }
        let totalBytes = buf.bytes
        reasoningBuffers.removeValue(forKey: placeholderID)

        guard var conv = conversation,
              let idx = conv.messages.firstIndex(where: { $0.id == placeholderID })
        else { return }

        // Single concat for all buffered chunks
        var buffer = conv.messages[idx].reasoning
        buffer.reserveCapacity(buffer.count + totalBytes)
        for chunk in buf.chunks { buffer.append(chunk) }
        conv.messages[idx].reasoning = buffer
        conversation = conv
    }

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

    /// Merge a server-resolved message with whatever already streamed into the
    /// placeholder. A resolved message with empty content must never erase
    /// streamed text — that regression rendered every reply as a blank bubble.
    ///
    /// Originally added in B35 (`c5069af`) and removed by the Build 41 refactor
    /// (`71884b9`); restored for 2.4.0. `StreamedContentPreservationTests` covers it.
    static func mergeResolvedMessage(resolved: Message, streamedContent: String) -> Message {
        guard resolved.content.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
              !streamedContent.isEmpty else {
            return resolved
        }
        var merged = resolved
        merged.content = streamedContent
        return merged
    }

    /// Internal rather than private so the merge invariants can be tested
    /// directly — this is the function that dropped completed replies in B39.
    func mergeConversationMetadata(
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
                // differs from the server-assigned message ID.  Match on jobID + sender.
                //
                // B40: this used to additionally require the local message to
                // carry toolActivities/codeDiff/reasoning, so a plain-text reply
                // never matched — which also meant B39 T5's empty-content and
                // truncation guards below never ran for the most common shape of
                // answer. The artifact copies are each guarded on their own.
                local = localConversation.messages.first(where: {
                    $0.jobID == remoteJobID
                        && $0.sender == remote.sender
                        && $0.sender == .herald
                })
            } else {
                local = nil
            }

            guard let local else { continue }

            // B39 T5: defence in depth — if the local (streamed) message has
            // non-empty content and the server's version is empty or a strict
            // prefix, keep the locally-rendered text. This protects against a
            // server refetch clobbering a good streamed answer with a truncated
            // or empty version.
            //
            // Added in B39 (`6c90009`), reverted by the Build 41 refactor
            // (`71884b9`), restored for 2.4.0. Covered by
            // `B40ConversationMergeTests.emptyServerCopyDoesNotBlankAPlainReply`.
            let localContent = local.content.trimmingCharacters(in: .whitespacesAndNewlines)
            let remoteContent = refreshedConversation.messages[index].content
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if !localContent.isEmpty {
                if remoteContent.isEmpty {
                    Self.logger.error(
                        "B39 T5: server returned empty content for message \(remote.id) (jobId=\(String(describing: remote.jobID))), keeping local text (len=\(localContent.count))"
                    )
                    refreshedConversation.messages[index].content = local.content
                } else if localContent.count > remoteContent.count,
                          localContent.hasPrefix(remoteContent) {
                    Self.logger.error(
                        "B39 T5: server content (len=\(remoteContent.count)) is a strict prefix of local (len=\(localContent.count)) for message \(remote.id) — keeping streamed text"
                    )
                    refreshedConversation.messages[index].content = local.content
                }
            }

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

        // B40 P0-1: preserve EVERY local message the refreshed payload is
        // missing — not just streaming placeholders and artifact-carrying
        // replies.
        //
        // This merge is fed `heraldClient.currentConversation`, which after a
        // send is the POST /v1/messages payload: a conversation containing
        // *only the user message just sent* (http_facade.py:795). The old
        // predicate kept a resolved reply only when it carried reasoning,
        // toolActivities or a codeDiff — so a plain-text answer (the normal
        // shape for models that emit no reasoning_content) matched nothing and
        // was dropped by `.finished` immediately after the delivered check and
        // the completion haptic. That is the "every indicator says done, no
        // reply on screen" P0.
        //
        // Dropping a message the server merely hasn't caught up on is never
        // correct here; keep it and let a real conversation fetch reconcile.
        //
        // Added in B40 (`dc151db`), reverted by the Build 41 refactor
        // (`71884b9`), restored for 2.4.0.
        let refreshedIDs = Set(refreshedConversation.messages.map(\.id))
        // Keyed by sender as well as jobID: a user message and the reply it
        // produced share a jobID, and dropping the prompt because the server
        // returned the answer would be its own bug.
        let refreshedJobKeys = Set(
            refreshedConversation.messages.compactMap { message in
                message.jobID.map { "\($0.uuidString)|\(message.sender)" }
            }
        )
        let refreshedFingerprints = Set(
            refreshedConversation.messages.map { Self.messageFingerprint($0) }
        )

        let localOnly = localConversation.messages.filter { message in
            if refreshedIDs.contains(message.id) { return false }
            // The server assigns its own message ids; jobID and content are the
            // only cross-identity handles we have, and matching on them keeps
            // the same answer from appearing twice.
            if let jobID = message.jobID,
               refreshedJobKeys.contains("\(jobID.uuidString)|\(message.sender)") {
                return false
            }
            if !message.content.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
               refreshedFingerprints.contains(Self.messageFingerprint(message)) {
                return false
            }
            return true
        }

        if !localOnly.isEmpty {
            Self.logger.info(
                "Merge preserved \(localOnly.count) local message(s) absent from the refreshed conversation"
            )
            refreshedConversation.messages.append(contentsOf: localOnly)
            // Stable sort by timestamp: appending puts a preserved reply after
            // messages that are chronologically later than it.
            refreshedConversation.messages = refreshedConversation.messages
                .enumerated()
                .sorted { lhs, rhs in
                    lhs.element.timestamp == rhs.element.timestamp
                        ? lhs.offset < rhs.offset
                        : lhs.element.timestamp < rhs.element.timestamp
                }
                .map(\.element)
        }

        return refreshedConversation
    }

    /// Sender + normalized content, used to recognize the same message across
    /// the local/server id boundary.
    private static func messageFingerprint(_ message: Message) -> String {
        "\(message.sender)|\(message.content.trimmingCharacters(in: .whitespacesAndNewlines))"
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

    /// Remove delivered notifications for the currently active conversation.
    /// Prevents Notification Center clutter when the user is already viewing
    /// a conversation — stale notifications for it are cleared.
    private func clearNotificationsForCurrentConversation() {
        guard let convId = conversation?.id else { return }
        let center = UNUserNotificationCenter.current()
        Task {
            let delivered = await center.deliveredNotifications()
            let staleIDs = delivered.compactMap { notification -> String? in
                let info = notification.request.content.userInfo
                guard let notificationConvId = info["conversationId"] as? String else {
                    return nil
                }
                return notificationConvId == convId.uuidString.lowercased()
                    ? notification.request.identifier
                    : nil
            }
            if !staleIDs.isEmpty {
                center.removeDeliveredNotifications(withIdentifiers: staleIDs)
            }
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
    private func detectProfileSwitch(in text: String) async {
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
                // Refresh the catalog so activeProfile computed property
                // resolves immediately instead of waiting for the next
                // automatic load (up to 60s away).
                await profileStore?.loadProfiles(force: true)
                return
            }
        }
    }

    /// Fallback-only lookup for cases where the connector has not yet provided
    /// an explicit context window. This should never overwrite a known value.
    // REMOVED: inferredContextWindow — all context info now comes from the
    // relay model catalog. Never fabricate context limits client-side.
}

private extension Array {
    subscript(safe index: Int) -> Element? {
        guard indices.contains(index) else { return nil }
        return self[index]
    }
}
