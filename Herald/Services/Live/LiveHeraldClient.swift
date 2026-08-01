import Foundation
import os

@MainActor
final class LiveHeraldClient: HeraldClientProtocol {
    private static let logger = Logger(subsystem: "net.fihonline.herald", category: "LiveHeraldClient")
    private static let maxRequestBodyBytes = 1_000_000
    private struct ConversationResponse: Decodable {
        let conversation: RelayConversation
    }

    /// Build 34 Workstream K: the server-side reply-state machine replaces the
    /// old `replyState != "pending"` heuristic.  The connector returns one of:
    ///
    ///   * `.pending`     — job accepted, stream/poll for completion
    ///   * `.complete`    — synchronous terminal response, render `message`
    ///   * `.duplicate`   — a prior request with this clientMessageId was
    ///                       already accepted; `existingState` + `jobId` carry
    ///                       the durable identity.  Reattach to the same job;
    ///                       NEVER render a synthetic "Herald did not return
    ///                       a message" row.
    ///   * `.conflict`    — same clientMessageId with different content;
    ///                       permanent typed error.
    ///   * `.error`       — terminal failure with a typed category.
    enum ReplyState: String, Decodable {
        case pending
        case complete
        case duplicate
        case conflict
        case error
    }

    /// Build 34 Workstream K: typed existing job state returned alongside a
    /// `.duplicate` acknowledgement.
    enum ExistingJobState: String, Decodable {
        case accepted
        case running
        case terminal
    }

    private struct MessageResponse: Decodable {
        let replyState: ReplyState
        let existingState: ExistingJobState?
        let errorCategory: String?
        let conversation: RelayConversation
        let userMessage: RelayMessage?
        let message: RelayMessage?
        let jobId: UUID?
        let usage: TokenUsage?
        let context: ContextInfo?
        let diff: CodeDiff?

        enum CodingKeys: String, CodingKey {
            case replyState, existingState, errorCategory
            case conversation, userMessage, message
            case jobId, usage, context, diff
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            // replyState is required and typed.  Treat a missing or unknown
            // value as `.complete` for backward compatibility with very old
            // connectors, but `.duplicate`/`.conflict` MUST be parseable
            // here or the bug recurs.
            replyState = (try? container.decode(ReplyState.self, forKey: .replyState)) ?? .complete
            existingState = try? container.decodeIfPresent(ExistingJobState.self, forKey: .existingState) ?? nil
            errorCategory = try? container.decodeIfPresent(String.self, forKey: .errorCategory) ?? nil
            // conversation is required — without it we can't maintain state
            conversation = try container.decode(RelayConversation.self, forKey: .conversation)
            // All other fields are optional and resilient to decode failures
            userMessage = try? container.decodeIfPresent(RelayMessage.self, forKey: .userMessage) ?? nil
            message = try? container.decodeIfPresent(RelayMessage.self, forKey: .message) ?? nil
            jobId = try? container.decodeIfPresent(UUID.self, forKey: .jobId) ?? nil
            usage = try? container.decodeIfPresent(TokenUsage.self, forKey: .usage) ?? nil
            context = try? container.decodeIfPresent(ContextInfo.self, forKey: .context) ?? nil
            diff = try? container.decodeIfPresent(CodeDiff.self, forKey: .diff) ?? nil
        }
    }

    private struct RelayConversation: Decodable {
        let id: UUID
        let title: String
        let updatedAt: Date
        let messages: [RelayMessage]
        let latestUsage: TokenUsage?
        let latestContext: ContextInfo?

        enum CodingKeys: String, CodingKey {
            case id, title, updatedAt, messages
            case latestUsage, latestContext
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            id = try container.decode(UUID.self, forKey: .id)
            title = (try? container.decode(String.self, forKey: .title)) ?? "New Chat"
            updatedAt = (try? container.decode(Date.self, forKey: .updatedAt)) ?? Date()
            messages = (try? container.decode([RelayMessage].self, forKey: .messages)) ?? []
            latestUsage = try container.decodeIfPresent(TokenUsage.self, forKey: .latestUsage)
            latestContext = try container.decodeIfPresent(ContextInfo.self, forKey: .latestContext)
        }
    }

    private struct RelayAttachment: Decodable {
        let type: String
        let filename: String
        let mimeType: String
        let thumbnailData: String?
    }

    private struct RelayMessage: Decodable {
        let id: UUID
        let clientMessageId: UUID?
        let role: MessageSender
        let text: String
        let timestamp: Date
        let deliveryStatus: String?
        let jobId: UUID?
        let attachments: [RelayAttachment]?
        let reasoning: String?

        enum CodingKeys: String, CodingKey {
            case id, role, text, timestamp
            case clientMessageId, deliveryStatus, jobId, attachments, reasoning
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            // id is required — a message without an id is meaningless
            id = try container.decode(UUID.self, forKey: .id)
            // role defaults to .herald if missing or unrecognized
            role = (try? container.decode(MessageSender.self, forKey: .role)) ?? .herald
            // text defaults to empty string if missing
            text = (try? container.decode(String.self, forKey: .text)) ?? ""
            // timestamp defaults to now if missing (shouldn't happen but prevents a crash)
            timestamp = (try? container.decode(Date.self, forKey: .timestamp)) ?? Date()
            clientMessageId = try container.decodeIfPresent(UUID.self, forKey: .clientMessageId)
            deliveryStatus = try container.decodeIfPresent(String.self, forKey: .deliveryStatus)
            jobId = try container.decodeIfPresent(UUID.self, forKey: .jobId)
            attachments = try container.decodeIfPresent([RelayAttachment].self, forKey: .attachments)
            reasoning = try container.decodeIfPresent(String.self, forKey: .reasoning)
        }
    }

    private struct StreamProgressPayload: Decodable {
        let jobId: UUID?
        let kind: String?
        let delta: String?
        let label: String?
        let phase: String?
    }

    private struct StreamDonePayload: Decodable {
        let jobId: UUID?
        let status: String
        let usage: TokenUsage?
        let context: ContextInfo?
        let diff: CodeDiff?
        let error: String?
        let message: RelayMessage?
    }

    struct JobStatusResponse: Sendable {
        let status: String
        let conversationId: UUID?
        let message: Message?
        let error: String?
        let usage: TokenUsage?
        let context: ContextInfo?
        let diff: CodeDiff?
        let attempt: Int?
        let lastSeq: Int?
        let errorCategory: String?
        let errorAction: String?
    }

    private struct AttachmentPayload: Encodable {
        let type: String    // "image" or "file"
        let filename: String
        let mimeType: String
        let data: String    // base64 encoded
        let thumbnailData: String?
    }

    private struct MessageCreateBody: Encodable {
        let heraldProtocol: Int = 5           // Build 34: schema validation, health probe, protocol 5
        let conversationId: UUID?
        let text: String
        let clientMessageId: UUID
        let attachments: [AttachmentPayload]?
        let reasoningEffort: String?
        /// Build 31 (fix): retry continuation context.  When present, the connector
        /// prepends this to the Hermes input so the model resumes from the cut-off
        /// point, but stores only `cleanText` (the original user prompt) in the
        /// canonical user message.  Never displayed in transcripts or notifications.
        let continuationContext: String?
    }

    var connectionStatus: ConnectionStatus = .disconnected
    var currentConversation: Conversation?

    private let apiClient: RelayAPIClient
    private let accessTokenProvider: @MainActor () async -> String?
    private let accessTokenRefresher: @MainActor () async -> String?
    private let allowDemoFallback: Bool
    var reasoningEffortProvider: (@MainActor () -> ReasoningEffort)?

    init(
        apiClient: RelayAPIClient,
        accessTokenProvider: @escaping @MainActor () async -> String?,
        accessTokenRefresher: @escaping @MainActor () async -> String? = { nil },
        allowDemoFallback: Bool = true
    ) {
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
        self.accessTokenRefresher = accessTokenRefresher
        self.allowDemoFallback = allowDemoFallback
    }

    func connect() async {
        connectionStatus = .connecting
        do {
            let response: ConversationResponse = try await performAuthorizedRequest { [self] token in
                try await self.apiClient.get(
                    path: "conversations/current",
                    accessToken: token
                )
            }
            currentConversation = mapConversation(response.conversation)
            connectionStatus = .connected
        } catch {
            connectionStatus = .error
        }
    }

    func disconnect() async {
        connectionStatus = .disconnected
    }

    func send(message: String, attachments: [PendingAttachment] = [], clientMessageID: UUID, continuationContext: String? = nil) async -> Message {
        do {
            let body = try self.makeCreateBody(
                text: message,
                attachments: attachments,
                clientMessageID: clientMessageID,
                continuationContext: continuationContext
            )
            let response: MessageResponse = try await performAuthorizedRequest { [self] token in
                try await self.apiClient.post(
                    path: "messages",
                    body: body,
                    accessToken: token
                )
            }
            currentConversation = mapConversation(response.conversation)
            connectionStatus = .connected

            // Complete response returned synchronously
            if let message = response.message {
                return mapMessage(message)
            }
            if let userMessage = response.userMessage {
                return mapMessage(userMessage)
            }

            // Build 28: If job is pending, poll until complete
            if response.replyState == .pending, let jobId = response.jobId {
                return await pollForJobCompletion(jobId: jobId)
            }

            return Message(sender: .system, content: "Herald did not return a message.", status: .failed)
        } catch {
            connectionStatus = .error
            return Message(sender: .system, content: failureMessage(for: error), status: .failed)
        }
    }

    /// Poll job status until terminal, returning the completed or failed Message.
    private func pollForJobCompletion(jobId: UUID) async -> Message {
        let pollInterval: Duration = .seconds(1)
        let maxWait: Duration = .seconds(180)  // matches relay max_job_duration_seconds

        let deadline = ContinuousClock.now + maxWait
        var consecutiveNils = 0
        while ContinuousClock.now < deadline {
            try? await Task.sleep(for: pollInterval)
            try? Task.checkCancellation()

            guard let status = await getJobStatus(jobId) else {
                consecutiveNils += 1
                if consecutiveNils >= 5 {
                    return Message(sender: .system,
                                   content: "Lost contact with the relay while waiting for a reply.",
                                   status: .failed)
                }
                continue
            }
            consecutiveNils = 0

            switch status.status {
            case "completed":
                if let message = status.message {
                    return message
                }
                return Message(sender: .system, content: "Herald completed but returned no message.", status: .failed)
            case "failed":
                let errorText = status.error ?? "An error occurred."
                return Message(sender: .system, content: errorText, status: .failed)
            case "cancelled":
                return Message(sender: .system, content: "Request cancelled.", status: .failed)
            case "queued", "running":
                continue  // still in progress, poll again
            default:
                continue
            }
        }
        return Message(sender: .system, content: "Request timed out.", status: .failed)
    }

    /// Build 34 Workstream K: handle the `.complete` reply state — a true
    /// synchronous terminal response.  The connector returned a final
    /// assistant message in the same POST; we render it once with the
    /// standard defense-in-depth reasoning strip.
    private func handleCompleteReply(
        _ response: MessageResponse,
        continuation: AsyncStream<StreamingUpdate>.Continuation,
        content: String
    ) {
        let mappedMsg: Message
        if let msg = response.message {
            mappedMsg = self.mapMessage(msg)
        } else if let userMsg = response.userMessage {
            // Defensive: a `.complete` response that lacks a `message` is
            // unusual but should not produce the old "Herald did not return
            // a message" synthetic row.  Render the user message as a
            // sent receipt instead — the connector will surface the real
            // terminal assistant row through the conversation fetch.
            mappedMsg = self.mapMessage(userMsg)
        } else {
            mappedMsg = Message(sender: .user, content: content, status: .sent)
        }

        // Emit a synthetic messageSent so the Live Activity starts.
        // (The assistant row is already terminal so no streaming state
        // needs to track it — but Live Activity / haptic come from
        // messageSent.)
        let syntheticJobID = response.jobId ?? UUID()
        continuation.yield(.messageSent(jobID: syntheticJobID))

        // Sanitize any residual reasoning blocks (defense in depth).
        let fullText = mappedMsg.content
        let (reasoning, visibleText) = Self.splitThinkingBlocks(fullText)

        var finalMsg = mappedMsg
        if !reasoning.isEmpty {
            finalMsg.reasoning = reasoning
            finalMsg.reasoningDuration = 0
        }
        finalMsg.content = visibleText
        continuation.yield(.finished(finalMsg, response.usage, response.diff, response.context))
        continuation.finish()
    }

    func sendStreaming(message content: String, attachments: [PendingAttachment] = [], clientMessageID: UUID, continuationContext: String? = nil) -> AsyncStream<StreamingUpdate> {
        AsyncStream { continuation in
            Task { @MainActor [weak self] in
                guard let self else {
                    continuation.yield(.failed("Client deallocated"))
                    continuation.finish()
                    return
                }

                do {
                    let body = try self.makeCreateBody(
                        text: content,
                        attachments: attachments,
                        clientMessageID: clientMessageID,
                        continuationContext: continuationContext
                    )
                    let response: MessageResponse = try await self.performAuthorizedRequest { [self] token in
                        try await self.apiClient.post(
                            path: "messages",
                            body: body,
                            accessToken: token
                        )
                    }

                    // A pending POST is an acknowledgement, not a refreshed
                    // conversation.  The connector intentionally returns just
                    // the accepted user message here; replacing the open
                    // conversation with that one-row payload discarded all
                    // earlier turns and the live assistant placeholder.  The
                    // subsequent UI merge then had to guess identity from text,
                    // which is unsafe for repeated content and tool/reasoning
                    // turns.  Keep the selected conversation authoritative until
                    // an explicit GET /sessions/{id}/conversation reconciles it.
                    let acceptedConversation = self.mapConversation(response.conversation)
                    if self.currentConversation == nil {
                        self.currentConversation = acceptedConversation
                    } else if self.currentConversation?.id != acceptedConversation.id {
                        Self.logger.error(
                            "Ignoring POST acknowledgement for a different conversation (selected=\(self.currentConversation?.id.uuidString ?? "nil"), ack=\(acceptedConversation.id.uuidString))"
                        )
                    }
                    self.connectionStatus = .connected

                    Self.logger.info("POST /messages replyState: \(response.replyState.rawValue) existingState: \(response.existingState?.rawValue ?? "nil") jobId: \(response.jobId?.uuidString ?? "nil")")

                    // Build 34 Workstream K: tagged state machine, NOT a
                    // `replyState != "pending"` heuristic.  The old check
                    // emitted a synthetic "Herald did not return a message"
                    // row for the .duplicate case (10:22→10:31 incident)
                    // because the duplicate-running response has no `message`.
                    switch response.replyState {
                    case .pending:
                        break // fall through to jobId / poll path below
                    case .complete:
                        self.handleCompleteReply(response, continuation: continuation, content: content)
                        return
                    case .duplicate:
                        // The connector reuses the durable request by
                        // clientMessageId.  Reattach to the existing job;
                        // never allocate a synthetic user row, thinking row,
                        // haptic, notification, or "Herald did not return
                        // a message" control row.
                        if let jobId = response.jobId {
                            Self.logger.info("Reattaching to duplicate job \(jobId.uuidString.prefix(8)) (existingState=\(response.existingState?.rawValue ?? "nil"))")
                            continuation.yield(.messageSent(jobID: jobId))
                            // Continue to the poll/stream path below so the
                            // assistant placeholder ties to the original job.
                            // We deliberately do NOT yield .finished here —
                            // the original job will deliver its own terminal
                            // event through the SSE/poll/foreground channel.
                        } else {
                            // Duplicate without a jobId: query the durable
                            // request status to recover the server lease,
                            // then resume.  Until we can recover the job,
                            // hold the optimistic user row open — do not
                            // emit a terminal synthetic message.
                            Self.logger.warning("Duplicate ack with no jobId — waiting for durable request status to surface a jobId")
                        }
                        // Fall through to the post-pending path that begins
                        // a stream/poll on whatever jobId we have.
                        // (No return.)
                    case .conflict:
                        let category = response.errorCategory ?? "request_conflict"
                        continuation.yield(.failed("Same message id was reused with different content (\(category)). Please send a new message."))
                        continuation.finish()
                        return
                    case .error:
                        let category = response.errorCategory ?? "server_error"
                        continuation.yield(.failed("Server error: \(category). Please try again."))
                        continuation.finish()
                        return
                    }

                    // Build 28: Reply is pending — use simplified polling via
                    // JobStreamCoordinator for the SSE path (still available
                    // but rarely reached since useStreaming defaults to false).
                    guard let jobId = response.jobId else {
                        // No jobId — relay returned a message directly
                        let mappedMsg: Message
                        if let msg = response.message ?? response.userMessage {
                            mappedMsg = self.mapMessage(msg)
                        } else {
                            mappedMsg = Message(sender: .user, content: content, status: .sent)
                        }

                        let syntheticJobID = UUID()
                        continuation.yield(.messageSent(jobID: syntheticJobID))

                        // Defense-in-depth reasoning strip (connector already sanitized)
                        let fullText = mappedMsg.content
                        let (reasoning, visibleText) = Self.splitThinkingBlocks(fullText)

                        var finalMsg = mappedMsg
                        if !reasoning.isEmpty {
                            finalMsg.reasoning = reasoning
                            finalMsg.reasoningDuration = 0
                        }
                        finalMsg.content = visibleText
                        continuation.yield(.finished(finalMsg, response.usage, response.diff, response.context))
                        continuation.finish()
                        return
                    }

                    continuation.yield(.messageSent(jobID: jobId))

                    // Bind this stream to the conversation captured by the
                    // request body, never whatever another device/load changed
                    // `currentConversation` to while the POST was in flight.
                    let conversationId = body.conversationId ?? acceptedConversation.id
                    let coordinator = JobStreamCoordinator(
                        jobId: jobId,
                        conversationId: conversationId,
                        clientMessageId: clientMessageID,
                        apiClient: self.apiClient,
                        accessTokenProvider: { [weak self] in await self?.accessTokenProvider() },
                        accessTokenRefresher: { [weak self] in await self?.accessTokenRefresher() },
                        jobStatusProvider: { [weak self] jobId in await self?.getJobStatusSnapshot(jobId) }
                    )

                    let result = await coordinator.run(continuation: continuation)

                    switch result {
                    case .completed(let terminalResult):
                        // Build a StreamDonePayload from the terminal result
                        let donePayload: StreamDonePayload?
                        if let terminalResult {
                            let usage: TokenUsage? = {
                                guard let prompt = terminalResult.promptTokens,
                                      let completion = terminalResult.completionTokens,
                                      let total = terminalResult.totalTokens else { return nil }
                                return TokenUsage(promptTokens: prompt, completionTokens: completion, totalTokens: total)
                            }()
                            let context: ContextInfo? = {
                                guard let window = terminalResult.contextWindow,
                                      let used = terminalResult.contextUsed else { return nil }
                                return ContextInfo(window: window, used: used)
                            }()
                            donePayload = StreamDonePayload(
                                jobId: jobId,
                                status: "completed",
                                usage: usage,
                                context: context,
                                diff: nil,
                                error: nil,
                                message: nil
                            )
                        } else {
                            donePayload = nil
                        }

                        // Prefer the text the `done` event already delivered.
                        //
                        // This ordering is load-bearing. `donePayload` is built
                        // above with `message: nil` — its only construction site —
                        // so `donePayload?.message` can never be non-nil, and the
                        // server round-trip below reloads from a stub that returns
                        // an empty conversation. With no terminal-text branch,
                        // `resolveFinalMessage` therefore falls all the way through
                        // to `Message(content: "", status: .delivered)`: a blank
                        // bubble that still fires the delivered check and haptic.
                        //
                        // B35 (`6172ec0`) fixed this; the Build 41 refactor
                        // (`71884b9`) reverted it. Restored for 2.4.0 and covered by
                        // `TerminalMessageMappingTests`.
                        let finalMessage: Message
                        let usage: TokenUsage?
                        if let streamed = Self.finalMessage(
                            fromTerminalText: terminalResult?.text,
                            jobId: jobId,
                            messageJSON: terminalResult?.messageJSON
                        ) {
                            finalMessage = streamed
                            usage = donePayload?.usage
                        } else if let doneMessage = donePayload?.message {
                            finalMessage = self.mapMessage(doneMessage)
                            usage = donePayload?.usage
                        } else {
                            let refreshedConversation = await self.reloadConversationForStreaming()
                            finalMessage = self.resolveFinalMessage(
                                jobId: jobId,
                                donePayload: donePayload,
                                conversation: refreshedConversation ?? self.currentConversation
                            )
                            usage = donePayload?.usage ?? refreshedConversation?.latestUsage
                        }
                        // Carry terminal reasoning from the SSE done payload through
                        // to the finished message so ChatStore can display it.
                        var resolvedFinal = finalMessage
                        if let terminalReasoning = terminalResult?.reasoning, !terminalReasoning.isEmpty {
                            resolvedFinal.reasoning = terminalReasoning
                        }
                        // Apply splitThinkingBlocks as a safety net for any residual
                        // <think> tags in the content that weren't stripped server-side.
                        let fullText = resolvedFinal.content
                        let (extractedReasoning, visibleText) = Self.splitThinkingBlocks(fullText)
                        if !extractedReasoning.isEmpty && resolvedFinal.reasoning.isEmpty {
                            resolvedFinal.reasoning = extractedReasoning
                        }
                        resolvedFinal.content = visibleText
                        let context: ContextInfo? = donePayload?.context
                        continuation.yield(.finished(resolvedFinal, usage, nil, context))
                    case .failed:
                        // Coordinator already yielded .failed StreamingUpdate
                        // with error category/action. Reload conversation so
                        // ChatStore picks up the persisted failed message.
                        _ = await self.reloadConversationForStreaming()
                    case .cancelled:
                        break // Coordinator already yielded .cancelled
                    case .error:
                        // Coordinator returned an unexpected error — attempt
                        // HTTP polling as a last-resort fallback in case the
                        // job completed server-side while SSE was down.
                        Self.logger.warning("SSE coordinator returned .error for job \(jobId), falling back to polling")
                        await self.pollJobUntilTerminal(jobId: jobId, continuation: continuation)
                    }
                    continuation.finish()

                } catch {
                    self.connectionStatus = .error
                    continuation.yield(.failed(self.failureMessage(for: error)))
                    continuation.finish()
                }
            }
        }
    }

    /// Build 31: ensure a server conversation exists for the given local UUID.
    /// Called before the first message in a new conversation so the connector
    /// can create a Hermes session and bind the mapping before the job runs.
    /// Without this, the first message's conversationId is a random UUID with
    /// no server session behind it, and the connector falls through to its
    /// process-wide singleton.
    ///
    /// - Returns: `true` if the server session was created or already existed.
    ///   `false` means the session could not be established — the caller must
    ///   not proceed with message submission.
    func ensureConversation(id: UUID) async -> Bool {
        struct EnsureResponse: Decodable {
            let conversationId: String?
            let sessionId: String?
            let created: Bool?
        }
        do {
            struct EnsureBody: Encodable {
                let conversationId: String
            }
            let body = EnsureBody(conversationId: id.uuidString.lowercased())
            let response: EnsureResponse = try await performAuthorizedRequest { [self] token in
                try await self.apiClient.post(
                    path: "conversations/ensure",
                    body: body,
                    accessToken: token
                )
            }
            let hasSession = response.sessionId != nil
            Self.logger.info("ensureConversation: sessionId=\(response.sessionId ?? "none"), created=\(response.created ?? false), success=\(hasSession)")
            return hasSession
        } catch {
            Self.logger.error("ensureConversation failed: \(error.localizedDescription) — first message will be blocked")
            return false
        }
    }

    func loadConversation() async -> Conversation {
        do {
            let response: ConversationResponse = try await performAuthorizedRequest { [self] token in
                try await self.apiClient.get(
                    path: "conversations/current",
                    accessToken: token
                )
            }
            let conversation = mapConversation(response.conversation)
            // Trust the relay as the source of truth for the current conversation.
            // Previously we guarded against the relay returning a different ID,
            // but that caused /new to silently return the stale conversation
            // after clearConversation() created a fresh one.
            if let existing = currentConversation, existing.id != conversation.id {
                Self.logger.info("Relay reports new conversation \(conversation.id), replacing current \(existing.id)")
            }
            currentConversation = conversation
            connectionStatus = .connected
            return conversation
        } catch {
            Self.logger.warning("Failed to load conversation from relay: \(error.localizedDescription)")
            connectionStatus = .error
            return currentConversation ?? fallbackConversation()
        }
    }

    func clearConversation() async throws -> Conversation {
        // Invalidate currentConversation before the network call so that any
        // concurrent loadConversation() won't reject the fresh conversation
        // the relay is about to create (see loadConversation() guard removal).
        currentConversation = nil
        let response: ConversationResponse = try await performAuthorizedRequest { [self] token in
            try await self.apiClient.post(
                path: "conversations/current/clear",
                accessToken: token
            )
        }
        let conversation = mapConversation(response.conversation)
        currentConversation = conversation
        connectionStatus = .connected
        return conversation
    }

    func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation {
        let response: ConversationResponse = try await performAuthorizedRequest { [self] token in
            try await self.apiClient.post(
                path: "talk/session/\(voiceSessionId.uuidString.lowercased())/inject",
                accessToken: token
            )
        }
        let conversation = mapConversation(response.conversation)
        currentConversation = conversation
        return conversation
    }

    private func makeCreateBody(
        text: String,
        attachments: [PendingAttachment],
        clientMessageID: UUID,
        continuationContext: String? = nil
    ) throws -> MessageCreateBody {
        let payloads: [AttachmentPayload]? = attachments.isEmpty ? nil : attachments.map { att in
            AttachmentPayload(
                type: att.kind.rawValue,
                filename: att.fileName,
                mimeType: att.mimeType,
                data: att.base64Data,
                thumbnailData: att.thumbnailBase64
            )
        }
        let effort = reasoningEffortProvider?()
        // Build 31: never send with a nil conversationId — that path
        // forced the connector to fall through to the process-wide
        // singleton, collapsing all device conversations onto one
        // Hermes session.  If currentConversation isn't set yet, use
        // a fresh UUID that the connector will bind on its side.
        let resolvedConversationId: UUID
        if let id = currentConversation?.id {
            resolvedConversationId = id
        } else {
            resolvedConversationId = UUID()
            Logger.app.info("makeCreateBody: currentConversation nil, using fresh UUID \(resolvedConversationId.uuidString.prefix(8))")
        }
        let body = MessageCreateBody(
            conversationId: resolvedConversationId,
            text: text,
            clientMessageId: clientMessageID,
            attachments: payloads,
            reasoningEffort: effort?.rawValue,
            continuationContext: continuationContext
        )
        try validateRequestBodySize(for: body)
        return body
    }

    private func fallbackConversation() -> Conversation {
        if allowDemoFallback {
            return DemoData.sampleConversation
        }

        return Conversation(title: "New Chat")
    }

    private func mapConversation(_ relayConversation: RelayConversation) -> Conversation {
        Conversation(
            id: relayConversation.id,
            title: relayConversation.title,
            messages: relayConversation.messages.map(mapMessage),
            lastActivity: relayConversation.updatedAt,
            latestUsage: relayConversation.latestUsage,
            contextPercent: relayConversation.latestContext?.percentUsed
        )
    }

    private func mapMessage(_ relayMessage: RelayMessage) -> Message {
        let attachments: [MessageAttachment] = (relayMessage.attachments ?? []).enumerated().map { index, att in
            MessageAttachment(
                kind: att.type,
                fileName: att.filename,
                mimeType: att.mimeType,
                thumbnailBase64: att.thumbnailData,
                messageID: relayMessage.id,
                remoteIndex: index
            )
        }
        return Message(
            id: relayMessage.id,
            clientMessageID: relayMessage.clientMessageId,
            sender: relayMessage.role,
            content: relayMessage.text,
            timestamp: relayMessage.timestamp,
            jobID: relayMessage.jobId,
            status: mapDeliveryStatus(relayMessage.deliveryStatus, sender: relayMessage.role),
            attachments: attachments,
            reasoning: relayMessage.reasoning ?? ""
        )
    }

    private func mapDeliveryStatus(_ deliveryStatus: String?, sender: MessageSender) -> MessageStatus {
        switch deliveryStatus {
        case "pending":
            return .sending
        case "sent":
            return .sent
        case "delivered":
            return .delivered
        case "failed":
            return .failed
        default:
            return sender == .user ? .sent : .delivered
        }
    }

    private func performAuthorizedRequest<T>(
        _ operation: @escaping @MainActor (_ accessToken: String?) async throws -> T
    ) async throws -> T {
        do {
            return try await operation(await accessTokenProvider())
        } catch RelayAPIClient.ClientError.unauthorized {
            guard let refreshedToken = await accessTokenRefresher(), !refreshedToken.isEmpty else {
                throw RelayAPIClient.ClientError.unauthorized("Expired or invalid access token.")
            }
            return try await operation(refreshedToken)
        }
    }

    private func reloadConversationForStreaming() async -> Conversation? {
        // Reload the specific conversation the message was just sent to — never
        // the device's arbitrary "current" conversation, which (now that a
        // device can have many sessions) may resolve to an unrelated session
        // and silently swap out the one actually on screen.
        guard let activeID = currentConversation?.id else {
            return await loadConversation()
        }
        do {
            return try await loadConversation(id: activeID)
        } catch {
            Self.logger.warning("Failed to refresh conversation after streaming: \(error.localizedDescription)")
            return currentConversation
        }
    }

    /// Map terminal SSE text to the final chat message.
    ///
    /// The `done` event already carries the canonical answer. Before B35 that
    /// text was parsed and then dropped, forcing a fallback to
    /// `/v1/conversations/current` — a stub that returns an empty message list —
    /// so every reply rendered as an empty bubble. Returns `nil` for absent or
    /// blank text so the caller can fall through to its other sources.
    ///
    /// Build 23: when *messageJSON* is provided, reconstruct a complete Message
    /// with the server-assigned ID and attachment metadata instead of a
    /// text-only placeholder with a fresh UUID.  This preserves inline images
    /// through the terminal projection path.
    static func finalMessage(
        fromTerminalText text: String?,
        jobId: UUID,
        messageJSON: [String: Any]? = nil
    ) -> Message? {
        // When a canonical server message is present, always prefer it —
        // attachments (e.g., inline images) are valid even with empty
        // terminal text (image-only completion, Build 23).
        if let json = messageJSON {
            return mapTerminalMessage(json: json, fallbackText: text ?? "", jobId: jobId)
        }
        guard let text, !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return nil
        }
        return Message(sender: .herald, content: text, jobID: jobId, status: .delivered)
    }

    /// Reconstruct a Message from the terminal `done` event's serialized
    /// message object (a RelayMessage dict).  Preserves the server-assigned
    /// message ID and attachment metadata so inline images survive the
    /// SSE→client bridge without a second network round-trip.
    private static func mapTerminalMessage(
        json: [String: Any],
        fallbackText: String,
        jobId: UUID
    ) -> Message {
        let messageId: UUID
        if let idStr = json["id"] as? String, let parsed = UUID(uuidString: idStr) {
            messageId = parsed
        } else {
            messageId = UUID()
        }

        let text = (json["text"] as? String) ?? (json["content"] as? String) ?? fallbackText

        let roleStr = json["role"] as? String
        let sender: MessageSender = roleStr == "user" ? .user : .herald

        // Map attachments preserving thumbnailData for immediate rendering
        // and remoteIndex for full-resolution fetch.
        let attachments: [MessageAttachment]
        if let rawAtts = json["attachments"] as? [[String: Any]] {
            attachments = rawAtts.enumerated().map { index, att in
                MessageAttachment(
                    kind: att["type"] as? String ?? "file",
                    fileName: att["filename"] as? String ?? "attachment",
                    mimeType: att["mimeType"] as? String ?? "application/octet-stream",
                    thumbnailBase64: att["thumbnailData"] as? String,
                    messageID: messageId,
                    remoteIndex: index
                )
            }
        } else {
            attachments = []
        }

        return Message(
            id: messageId,
            sender: sender,
            content: text,
            jobID: jobId,
            status: .delivered,
            attachments: attachments
        )
    }

    private func resolveFinalMessage(
        jobId: UUID,
        donePayload: StreamDonePayload?,
        conversation: Conversation?
    ) -> Message {
        if let relayMessage = donePayload?.message {
            return mapMessage(relayMessage)
        }

        if let conversation,
           let message = conversation.messages.last(where: { $0.jobID == jobId && $0.sender != .user }) {
            return message
        }

        if donePayload?.status == "failed" {
            let rawError = donePayload?.error ?? ""
            let text: String
            if rawError.contains("413") || rawError.lowercased().contains("too large") {
                text = "The attachment was too large for Herald to process. Try a smaller image."
            } else if rawError.isEmpty {
                text = "Herald could not process this message."
            } else {
                // Strip URLs and technical details for a cleaner message
                let cleaned = rawError
                    .replacingOccurrences(of: #"For more information check: \S+"#, with: "", options: .regularExpression)
                    .replacingOccurrences(of: #"for url '\S+'"#, with: "", options: .regularExpression)
                    .trimmingCharacters(in: .whitespacesAndNewlines)
                text = "Herald could not process this message: \(cleaned)"
            }
            return Message(sender: .system, content: text, jobID: jobId, status: .failed)
        }

        // Build 16: never produce a blank delivered bubble. When all sources
        // (SSE terminal text, done payload message, conversation reload) fail
        // to produce a non-empty assistant response, surface an explicit
        // failure so the user sees a retryable error instead of a blank reply
        // with a delivered checkmark and haptic.
        return Message(sender: .system, content: "Herald completed but returned no message.", jobID: jobId, status: .failed)
    }

    private func validateRequestBodySize(for body: MessageCreateBody) throws {
        let encoded = try RelayCoders.makeEncoder().encode(body)
        guard encoded.count <= Self.maxRequestBodyBytes else {
            throw RelayAPIClient.ClientError.requestFailed(
                "The attachment was too large for Herald to process. Try a smaller image."
            )
        }
    }

    private func failureMessage(for error: Error) -> String {
        // Build 32: protocol mismatch gets a clean compatibility message.
        // The raw JSON dict must never appear in the transcript.
        if let clientError = error as? RelayAPIClient.ClientError,
           let mismatch = clientError.protocolMismatchInfo {
            return "Connector update required — this build needs protocol \(mismatch.required) but the host is running protocol \(mismatch.client). Update the Herald connector on your host to continue."
        }

        let rawError: String
        if let clientError = error as? RelayAPIClient.ClientError {
            rawError = clientError.errorDescription ?? error.localizedDescription
        } else {
            rawError = error.localizedDescription
        }

        if rawError.contains("413") || rawError.lowercased().contains("too large") {
            return "The attachment was too large for Herald to process. Try a smaller image."
        }
        if rawError.isEmpty {
            return "Herald relay is unavailable right now."
        }
        return rawError
    }

    private func decode<T: Decodable>(_ type: T.Type, from raw: String) -> T? {
        guard let data = raw.data(using: .utf8) else {
            Self.logger.warning("SSE decode: failed to convert raw string to UTF-8 data")
            return nil
        }
        do {
            return try RelayCoders.makeDecoder().decode(type, from: data)
        } catch {
            let snippet = String(raw.prefix(200))
            Self.logger.warning("SSE decode failed for \(String(describing: T.self)): \(error.localizedDescription) — raw: \(snippet)")
            return nil
        }
    }
}

// MARK: - Session Management

extension LiveHeraldClient {
    private struct SessionListAPIResponse: Decodable {
        let sessions: [SessionAPIEntry]
        let total: Int
    }

    private struct SessionAPIEntry: Decodable {
        let id: UUID
        let title: String
        let previewText: String?
        let updatedAt: Date?
        let source: String?
        let isPinned: Bool?
        let isArchived: Bool?
    }

    private struct SessionAPIResponse: Decodable {
        let session: SessionAPIEntry
    }

    func listSessions(limit: Int, offset: Int, allDevices: Bool = false) async throws -> SessionListResponse {
        let response: SessionListAPIResponse = try await performAuthorizedRequest { [self] token in
            try await self.apiClient.get(
                path: "sessions?limit=\(limit)&offset=\(offset)&allDevices=\(allDevices)",
                accessToken: token
            )
        }
        let sessions = response.sessions.map { entry in
            SessionSummary(
                id: entry.id,
                title: entry.title,
                previewText: entry.previewText ?? "",
                lastActivity: entry.updatedAt ?? .now,
                source: entry.source,
                isPinned: entry.isPinned ?? false,
                isArchived: entry.isArchived ?? false
            )
        }
        return SessionListResponse(sessions: sessions, total: response.total)
    }

    func searchSessions(query: String, allDevices: Bool = false) async throws -> [SessionSummary] {
        let encoded = query.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? query
        let response: SessionListAPIResponse = try await performAuthorizedRequest { [self] token in
            try await self.apiClient.get(
                path: "sessions/search?q=\(encoded)&allDevices=\(allDevices)",
                accessToken: token
            )
        }
        return response.sessions.map { entry in
            SessionSummary(
                id: entry.id,
                title: entry.title,
                previewText: entry.previewText ?? "",
                lastActivity: entry.updatedAt ?? .now,
                source: entry.source,
                isPinned: entry.isPinned ?? false,
                isArchived: entry.isArchived ?? false
            )
        }
    }

    func createSession(title: String) async throws -> SessionSummary {
        struct CreateSessionBody: Encodable { let title: String }
        let response: SessionAPIResponse = try await performAuthorizedRequest { [self] token in
            try await self.apiClient.post(
                path: "sessions",
                body: CreateSessionBody(title: title),
                accessToken: token
            )
        }
        let entry = response.session
        return SessionSummary(
            id: entry.id,
            title: entry.title,
            previewText: entry.previewText ?? "",
            lastActivity: entry.updatedAt ?? .now,
            source: entry.source,
            isPinned: entry.isPinned ?? false,
            isArchived: entry.isArchived ?? false
        )
    }

    func deleteSession(id: UUID) async throws {
        struct EmptyResponse: Decodable {}
        let _: EmptyResponse = try await performAuthorizedRequest { [self] token in
            try await self.apiClient.delete(
                path: "sessions/\(id.uuidString.lowercased())",
                accessToken: token
            )
        }
    }

    func archiveSession(id: UUID) async throws {
        struct EmptyResponse: Decodable {}
        let _: EmptyResponse = try await performAuthorizedRequest { [self] token in
            try await self.apiClient.post(
                path: "sessions/\(id.uuidString.lowercased())/archive",
                accessToken: token
            )
        }
    }

    func togglePinSession(id: UUID) async throws -> SessionSummary {
        let response: SessionAPIResponse = try await performAuthorizedRequest { [self] token in
            try await self.apiClient.post(
                path: "sessions/\(id.uuidString.lowercased())/pin",
                accessToken: token
            )
        }
        let entry = response.session
        return SessionSummary(
            id: entry.id,
            title: entry.title,
            previewText: entry.previewText ?? "",
            lastActivity: entry.updatedAt ?? .now,
            source: entry.source,
            isPinned: entry.isPinned ?? false,
            isArchived: entry.isArchived ?? false
        )
    }

    func renameSession(id: UUID, title: String) async throws -> SessionSummary {
        struct RenameBody: Encodable { let title: String }
        let response: SessionAPIResponse = try await performAuthorizedRequest { [self] token in
            try await self.apiClient.patch(
                path: "sessions/\(id.uuidString.lowercased())",
                body: RenameBody(title: title),
                accessToken: token
            )
        }
        let entry = response.session
        return SessionSummary(
            id: entry.id,
            title: entry.title,
            previewText: entry.previewText ?? "",
            lastActivity: entry.updatedAt ?? .now,
            source: entry.source,
            isPinned: entry.isPinned ?? false,
            isArchived: entry.isArchived ?? false
        )
    }

    func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String {
        struct GenerateTitleBody: Encodable {
            let userMessage: String
            let assistantMessage: String
        }
        struct GenerateTitleResponse: Decodable {
            let title: String?
        }
        let response: GenerateTitleResponse = try await performAuthorizedRequest { [self] token in
            try await self.apiClient.post(
                path: "sessions/\(sessionId.uuidString.lowercased())/generate-title",
                body: GenerateTitleBody(userMessage: userMessage, assistantMessage: assistantMessage),
                accessToken: token
            )
        }
        guard let title = response.title, !title.isEmpty else {
            throw URLError(.badServerResponse)
        }
        return title
    }

    func loadConversation(id: UUID) async throws -> Conversation {
        let response: ConversationResponse = try await performAuthorizedRequest { [self] token in
            try await self.apiClient.get(
                path: "sessions/\(id.uuidString.lowercased())/conversation",
                accessToken: token
            )
        }
        let conversation = mapConversation(response.conversation)
        currentConversation = conversation
        connectionStatus = .connected
        return conversation
    }

    func getJobStatus(_ jobId: UUID) async -> JobStatusResponse? {
        struct JobStatusData: Decodable {
            let jobId: String
            let status: String
            let conversationId: UUID?
            let error: String?
            let usage: TokenUsage?
            let context: ContextInfo?
            let diff: CodeDiff?
            let message: RelayMessage?
            let attempt: Int?
            let lastSeq: Int?
            let errorCategory: String?
            let errorAction: String?
        }
        struct JobStatusAPIResponse: Decodable {
            let data: JobStatusData
        }
        do {
            let response: JobStatusAPIResponse = try await performAuthorizedRequest { [self] token in
                try await self.apiClient.get(
                    path: "jobs/\(jobId.uuidString.lowercased())",
                    accessToken: token
                )
            }
            let data = response.data
            return JobStatusResponse(
                status: data.status,
                conversationId: data.conversationId,
                message: data.message.map { mapMessage($0) },
                error: data.error,
                usage: data.usage,
                context: data.context,
                diff: data.diff,
                attempt: data.attempt,
                lastSeq: data.lastSeq,
                errorCategory: data.errorCategory,
                errorAction: data.errorAction
            )
        } catch let decodingError as DecodingError {
            Self.logger.error("Job status decode failure: \(String(describing: decodingError))")
            if case .keyNotFound(let key, let context) = decodingError {
                Self.logger.error("  → missing key '\(key.stringValue)' at \(context.codingPath.map(\.stringValue))")
            } else if case .typeMismatch(let type, let context) = decodingError {
                Self.logger.error("  → type mismatch: expected \(type) at \(context.codingPath.map(\.stringValue))")
            }
            return nil
        } catch {
            Self.logger.warning("Failed to get job status: \(error.localizedDescription)")
            return nil
        }
    }

    func getJobStatusSnapshot(_ jobId: UUID) async -> JobStreamCoordinator.JobStatusSnapshot? {
        guard let status = await getJobStatus(jobId) else { return nil }
        return JobStreamCoordinator.JobStatusSnapshot(
            status: status.status,
            attempt: status.attempt ?? 0,
            lastSeq: status.lastSeq ?? 0
        )
    }

    func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message {
        let effort = reasoningEffortProvider?()
        let body = MessageCreateBody(
            conversationId: conversationID,
            text: text,
            clientMessageId: clientMessageID,
            attachments: nil,
            reasoningEffort: effort?.rawValue,
            continuationContext: nil
        )
        struct MessageResponse: Decodable {
            let message: RelayMessage?
        }
        let response: MessageResponse = try await performAuthorizedRequest { [self] token in
            try await self.apiClient.post(
                path: "messages",
                body: body,
                accessToken: token
            )
        }
        if let msg = response.message {
            return mapMessage(msg)
        }
        return Message(sender: .user, content: text, status: .sent)
    }

    func cancelJob(jobID: UUID) async throws {
        struct CancelResponse: Decodable {
            let data: CancelData?
            struct CancelData: Decodable {
                let jobId: String?
                let status: String?
            }
        }
        _ = try await performAuthorizedRequest { [self] token in
            try await self.apiClient.post(
                path: "jobs/\(jobID.uuidString.lowercased())/cancel",
                accessToken: token
            ) as CancelResponse
        }
    }

    /// Polls job status with bounded exponential backoff until it reaches a terminal state.
    /// Used when SSE stream ends but job is still running/queued.
    private func pollJobUntilTerminal(
        jobId: UUID,
        continuation: AsyncStream<StreamingUpdate>.Continuation
    ) async {
        let maxPolls = 10
        var delayMs: Double = 500 // Start with500ms
        let maxDelayMs: Double = 10_000 // Cap at10 seconds

        for attempt in 0..<maxPolls {
            if Task.isCancelled { break }

            Self.logger.info("Polling job \(jobId.uuidString.prefix(8)) status (attempt \(attempt + 1))")
            try? await Task.sleep(for: .milliseconds(delayMs))

            if let statusResponse = await self.getJobStatus(jobId) {
                switch statusResponse.status {
                case "completed":
                    Self.logger.info("Job \(jobId.uuidString.prefix(8)) completed during polling")
                    if let msg = statusResponse.message {
                        continuation.yield(.finished(msg, statusResponse.usage, statusResponse.diff, statusResponse.context))
                    } else {
                        let refreshed = await self.reloadConversationForStreaming()
                        let finalMsg = self.resolveFinalMessage(
                            jobId: jobId,
                            donePayload: nil,
                            conversation: refreshed ?? self.currentConversation
                        )
                        continuation.yield(.finished(finalMsg, nil, nil, nil))
                    }
                    return

                case "failed":
                    Self.logger.info("Job \(jobId.uuidString.prefix(8)) failed during polling")
                    continuation.yield(.failed(
                        statusResponse.error ?? "Job failed",
                        category: statusResponse.errorCategory,
                        action: statusResponse.errorAction
                    ))
                    return

                case "cancelled":
                    Self.logger.info("Job \(jobId.uuidString.prefix(8)) cancelled during polling")
                    continuation.yield(.cancelled)
                    return

                default:
                    // Still running/queued — continue polling with exponential backoff
                    Self.logger.info("Job \(jobId.uuidString.prefix(8)) still \(statusResponse.status), continuing polling")
                    delayMs = min(delayMs * 2, maxDelayMs)
                }
            } else {
                // Could not get status — continue polling
                Self.logger.warning("Could not get status for job \(jobId.uuidString.prefix(8)), retrying")
                delayMs = min(delayMs * 2, maxDelayMs)
            }
        }

        // Exhausted all poll attempts
        Self.logger.error("Exhausted polling attempts for job \(jobId.uuidString.prefix(8))")
        continuation.yield(.failed("Stream interrupted — job did not complete in time"))
    }

    // MARK: - Client-Side Streaming Simulation

    /// Splits text into word-level chunks for client-side streaming simulation.
    /// Preserves whitespace and punctuation attachment so the rendered output
    /// reads naturally as words appear.
    private static func chunkTextForStreaming(_ text: String) -> [String] {
        guard !text.isEmpty else { return [] }
        // Split on word boundaries but keep whitespace attached to the trailing word
        var chunks: [String] = []
        var current = ""
        for char in text {
            current.append(char)
            if char == " " || char == "\n" {
                chunks.append(current)
                current = ""
            }
        }
        if !current.isEmpty {
            chunks.append(current)
        }
        // If chunking produced nothing useful (e.g., no spaces in short text),
        // fall back to character-level chunks
        if chunks.isEmpty {
            chunks = text.map { String($0) }
        }
        return chunks
    }

    /// Extracts reasoning blocks from response text.
    /// Recognizes the same tag variants as the connector's reasoning_sanitizer:
    /// think, thinking, reasoning, thought, REASONING_SCRATCHPAD
    /// Returns (reasoning, visibleText) — reasoning is stripped content,
    /// visibleText is everything else.
    private static func splitThinkingBlocks(_ text: String) -> (reasoning: String, visible: String) {
        let tags = ["think", "thinking", "reasoning", "thought", "REASONING_SCRATCHPAD"]
        let pattern = "<(" + tags.joined(separator: "|") + ")>(.*?)</\\1>"
        guard let regex = try? NSRegularExpression(
            pattern: pattern,
            options: [.dotMatchesLineSeparators, .caseInsensitive]
        ) else {
            return ("", text)
        }

        let nsText = text as NSString
        let matches = regex.matches(in: text, range: NSRange(location: 0, length: nsText.length))

        let reasoning: String
        if !matches.isEmpty {
            reasoning = matches.compactMap { match -> String? in
                guard match.numberOfRanges > 2 else { return nil }
                return nsText.substring(with: match.range(at: 2))
            }.joined(separator: "\n")
                .trimmingCharacters(in: .whitespacesAndNewlines)
        } else {
            // No closed tags found — check for unclosed opening tag
            // (model was interrupted mid-reasoning, e.g. context full).
            let unclosedPattern = "<(" + tags.joined(separator: "|") + ")>([\\s\\S]*?)$"
            if let unclosedRegex = try? NSRegularExpression(
                pattern: unclosedPattern,
                options: [.caseInsensitive]
            ),
               let match = unclosedRegex.firstMatch(
                in: text,
                range: NSRange(location: 0, length: nsText.length)
               ),
               match.numberOfRanges > 2 {
                reasoning = nsText.substring(with: match.range(at: 2))
                    .trimmingCharacters(in: .whitespacesAndNewlines)
            } else {
                reasoning = ""
            }
        }

        guard !reasoning.isEmpty else { return ("", text) }

        // Strip ALL tag variants (closed and unclosed) from visible content
        let closedStripPattern = "<(" + tags.joined(separator: "|") + ")>.*?</\\1>"
        let unclosedStripPattern = "<(" + tags.joined(separator: "|") + ")>[\\s\\S]*$"
        var visible = text
        if let closedRegex = try? NSRegularExpression(
            pattern: closedStripPattern,
            options: [.dotMatchesLineSeparators, .caseInsensitive]
        ) {
            visible = closedRegex.stringByReplacingMatches(
                in: visible,
                range: NSRange(location: 0, length: (visible as NSString).length),
                withTemplate: ""
            )
        }
        if let unclosedRegex = try? NSRegularExpression(
            pattern: unclosedStripPattern,
            options: [.caseInsensitive]
        ) {
            visible = unclosedRegex.stringByReplacingMatches(
                in: visible,
                range: NSRange(location: 0, length: (visible as NSString).length),
                withTemplate: ""
            )
        }
        visible = visible.trimmingCharacters(in: .whitespacesAndNewlines)

        return (reasoning, visible)
    }
}
