import Foundation

@MainActor
@Observable
final class ChatStore {
    var conversation: Conversation?
    var isLoading = false
    var pendingMessageSentAt: Date?
    var lastTokenUsage: TokenUsage?
    /// Live log entries for the iPad inspector panel's Logs tab.
    var logEntries: [LogEntry] = []
    private var isPollingEnabled = false
    private var pollingTask: Task<Void, Never>?
    private var streamingTask: Task<Void, Never>?
    private(set) var streamingMessageID: UUID?

    /// If no progress event (text/reasoning delta, tool activity, or finish)
    /// arrives within this window of job submission, the job is treated as
    /// silently stalled/dropped — see `runStreamingAttempt`.
    private static let watchdogTimeout: Duration = .seconds(30)
    private static let maxAutoRetries = 1
    /// Tracks how many times a stalled job has been auto-retried, keyed by
    /// the user message's clientMessageID. Cleared once the send completes
    /// (success or final failure).
    private var stallRetryCounts: [UUID: Int] = [:]

    // Delta coalescing — tokens arrive faster than SwiftUI can usefully redraw.
    // Buffer deltas in an Array<String> (avoids O(n²) inline concat) and flush
    // onto the placeholder at ~30fps so every append triggers at most one
    // @Observable notification per frame.
    private var pendingDeltaChunks: [String] = []
    private var pendingDeltaBytes: Int = 0
    private var deltaFlushTask: Task<Void, Never>?
    private static let deltaFlushInterval: Duration = .milliseconds(33)
    private static let deltaFlushByteThreshold = 4_096

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

    private let heraldClient: any HeraldClientProtocol
    private let chatLiveActivity = LiveActivityService()
    let persistence: any AppPersistenceStoreProtocol

    /// Called when conversation content changes (new message, streaming complete).
    /// Used by AppContainer to push widget data updates.
    var onConversationChanged: (@MainActor () -> Void)?

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
        if let latestUsage = conversation?.latestUsage {
            lastTokenUsage = latestUsage
        }
        if let conversation {
            persistence.saveConversationCache(conversation)
            onConversationChanged?()
        }
        restartPendingPollingIfNeeded()
    }

    func sendMessage(_ content: String, attachments: [PendingAttachment] = []) async {
        let trimmedContent = content.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedContent.isEmpty || !attachments.isEmpty else { return }
        guard hasPendingDuplicateMessage(trimmedContent, attachments: attachments) == false else { return }

        let clientMessageID = UUID()
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
            conversation = Conversation(title: "Herald")
        }
        conversation?.messages.append(optimistic)
        conversation?.lastActivity = optimistic.timestamp
        pendingMessageSentAt = optimistic.timestamp

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
        streamingMessageID = placeholderID
        restartPendingPollingIfNeeded()

        stallRetryCounts[clientMessageID] = 0
        await runAttemptLoop(
            content: trimmedContent,
            attachments: attachments,
            clientMessageID: clientMessageID,
            placeholderID: placeholderID
        )
        stallRetryCounts.removeValue(forKey: clientMessageID)

        if !hasPendingMessages {
            pendingMessageSentAt = nil
        }

        if let conversation {
            persistence.saveConversationCache(conversation)
            onConversationChanged?()
        }
    }

    /// Drives one or more streaming attempts for a single outgoing message.
    ///
    /// A ~30s watchdog guards each attempt (see `runStreamingAttempt`): if no
    /// progress event arrives in that window, the job is treated as silently
    /// dropped — the exact failure mode confirmed via device testing, where a
    /// job was claimed by the relay but never dispatched by the connector, with
    /// no server error at all. The first stall triggers one automatic retry that
    /// reuses the same message bubble (same clientMessageID/placeholderID, so no
    /// duplicate appears); a second stall gives up with real error text and
    /// leaves the existing manual "tap to retry" affordance for the user.
    private func runAttemptLoop(
        content: String,
        attachments: [PendingAttachment],
        clientMessageID: UUID,
        placeholderID: UUID
    ) async {
        while true {
            let stalled = await runStreamingAttempt(
                content: content,
                attachments: attachments,
                clientMessageID: clientMessageID,
                placeholderID: placeholderID
            )
            guard stalled else { return }

            let attempt = stallRetryCounts[clientMessageID] ?? 0
            if attempt < Self.maxAutoRetries {
                stallRetryCounts[clientMessageID] = attempt + 1
                continue // re-send with the same bubble/IDs, restart the watchdog
            }

            failStalledMessage(clientMessageID: clientMessageID, placeholderID: placeholderID)
            return
        }
    }

    /// Runs a single streaming attempt, racing the update stream against a
    /// ~30s watchdog. Returns `true` if the watchdog fired before any progress
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

                case .textDelta(let delta):
                    progressContinuation?.yield(())
                    self.enqueueDelta(delta, placeholderID: placeholderID)

                case .reasoningDelta(let delta):
                    progressContinuation?.yield(())
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

                case .finished(let finalMessage, let usage, let diff):
                    progressContinuation?.yield(())
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
                        self.conversation?.messages[idx] = resolved
                    }
                    // Mark user message as delivered if it's still in sending state
                    if let idx = self.conversation?.messages.firstIndex(where: { $0.id == clientMessageID }) {
                        if self.conversation?.messages[idx].status == .sending {
                            self.conversation?.messages[idx].status = .delivered
                        }
                    }
                    self.conversation = self.mergeConversationMetadata(
                        from: self.conversation,
                        into: self.heraldClient.currentConversation
                    )
                    if let latestUsage = self.conversation?.latestUsage {
                        self.lastTokenUsage = latestUsage
                    } else if let usage {
                        self.lastTokenUsage = usage
                    }
                    self.detectProfileSwitch(in: finalMessage.content)
                    self.streamingMessageID = nil
                    self.pendingMessageSentAt = nil
                    self.chatLiveActivity.endActivity()

                case .failed(let errorMessage):
                    // An explicit failure is a real signal, not silence — let it
                    // resolve the watchdog race immediately rather than waiting
                    // out the timeout, and handle it exactly as before.
                    progressContinuation?.yield(())
                    self.flushPendingDeltas(placeholderID: placeholderID)
                    if let idx = self.conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
                        if acceptedJobID == nil {
                            self.conversation?.messages[idx] = Message(
                                sender: .system,
                                content: errorMessage,
                                status: .failed
                            )
                        } else {
                            self.conversation?.messages.remove(at: idx)
                        }
                    }
                    self.streamingMessageID = nil
                    self.chatLiveActivity.endActivity()
                    if let idx = self.conversation?.messages.firstIndex(where: { $0.id == clientMessageID }) {
                        self.conversation?.messages[idx].status = acceptedJobID == nil ? .failed : .sending
                    }
                    if acceptedJobID != nil {
                        needsPollingFallback = true
                    } else {
                        self.pendingMessageSentAt = nil
                    }
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
            streamingMessageID = nil
            restartPendingPollingIfNeeded()
        }

        return false
    }

    /// Marks a message as failed with real, actionable error text after both
    /// the initial attempt and the automatic retry have stalled with zero
    /// progress. Mirrors the shape of the existing `.failed` stream-event
    /// handling above (Herald placeholder becomes a system error message, user
    /// message flips to `.failed`) so the existing manual "tap to retry" flow
    /// (`retryMessage(_:)`, wired up in `MessageBubble`) keeps working unchanged.
    private func failStalledMessage(clientMessageID: UUID, placeholderID: UUID) {
        let errorText = "Herald didn't respond — tap to retry"
        if let idx = conversation?.messages.firstIndex(where: { $0.id == placeholderID }) {
            conversation?.messages[idx] = Message(
                sender: .system,
                content: errorText,
                status: .failed
            )
        }
        if let idx = conversation?.messages.firstIndex(where: { $0.id == clientMessageID }) {
            conversation?.messages[idx].status = .failed
        }
        streamingMessageID = nil
        pendingMessageSentAt = nil
        chatLiveActivity.endActivity()
    }

    func clearConversation() async throws {
        streamingTask?.cancel()
        streamingTask = nil
        streamingMessageID = nil
        stallRetryCounts.removeAll()
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

    func cancelStreaming() {
        streamingTask?.cancel()
        streamingTask = nil
        chatLiveActivity.endActivity()

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
        streamingMessageID = nil
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
            onConversationChanged?()
        }
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

        await sendMessage(content, attachments: attachments)
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
        streamingMessageID = nil
        stallRetryCounts.removeAll()
        logEntries = []
        isPollingEnabled = false
        resetCommandCatalog()
        conversation = nil
        isLoading = false
        pendingMessageSentAt = nil
        lastTokenUsage = nil
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
        pendingDeltaChunks.append(delta)
        pendingDeltaBytes += delta.utf8.count

        // If we've buffered a lot, flush immediately so the UI doesn't fall
        // multiple frames behind during a burst.
        if pendingDeltaBytes >= Self.deltaFlushByteThreshold {
            flushPendingDeltas(placeholderID: placeholderID)
            return
        }

        guard deltaFlushTask == nil else { return }
        deltaFlushTask = Task { [weak self, placeholderID] in
            try? await Task.sleep(for: Self.deltaFlushInterval)
            guard !Task.isCancelled else { return }
            await MainActor.run {
                self?.flushPendingDeltas(placeholderID: placeholderID)
            }
        }
    }

    private func flushPendingDeltas(placeholderID: UUID) {
        deltaFlushTask?.cancel()
        deltaFlushTask = nil

        guard !pendingDeltaChunks.isEmpty else { return }
        let chunks = pendingDeltaChunks
        pendingDeltaChunks = []
        pendingDeltaBytes = 0

        guard var conv = conversation,
              let idx = conv.messages.firstIndex(where: { $0.id == placeholderID })
        else { return }

        // Single concat: O(sum(chunk sizes)) instead of O(n·chunks) across ticks.
        var buffer = conv.messages[idx].content
        buffer.reserveCapacity(buffer.count + pendingDeltaBytes)
        for chunk in chunks { buffer.append(chunk) }
        conv.messages[idx].content = buffer

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
    // spread out so we don't hammer a struggling relay. ~130s total budget.
    private static let pollingBackoffSeconds: [Double] = [
        1.5, 2, 3, 5, 8, 12, 18, 25, 30, 30,
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

            for (attempt, delay) in Self.pollingBackoffSeconds.enumerated() {
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

                // On the last attempt, mark anything still pending as failed
                // so the user sees an actionable state instead of forever-sending.
                let isLastAttempt = attempt == Self.pollingBackoffSeconds.count - 1
                if isLastAttempt, self.hasPendingMessages {
                    if var conv = self.conversation {
                        for i in conv.messages.indices
                            where conv.messages[i].sender == .user && conv.messages[i].status == .sending
                        {
                            conv.messages[i].status = .failed
                        }
                        self.conversation = conv
                        self.persistence.saveConversationCache(conv)
                    }
                    self.pendingMessageSentAt = nil
                }
            }

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
                        && (!$0.toolActivities.isEmpty || $0.codeDiff != nil)
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
