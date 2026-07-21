import Foundation
import os

/// Coordinates a single job's SSE event stream with cursor-based resume.
/// Replaces the one-shot `streamJobEvents` that could not reconnect.
actor JobStreamCoordinator {
    /// Terminal payload extracted from the done event.
    struct TerminalResult: Sendable {
        let text: String?
        let promptTokens: Int?
        let completionTokens: Int?
        let totalTokens: Int?
        let error: String?
    }

    enum RunResult: Sendable {
        case completed(TerminalResult?)
        case failed(TerminalResult?)
        case cancelled
        case error(String)
    }
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "JobStreamCoordinator")

    let jobId: UUID
    let conversationId: UUID
    let clientMessageId: UUID
    let placeholderId: UUID?

    private let apiClient: RelayAPIClient
    private let accessTokenProvider: @Sendable () async -> String?
    private let accessTokenRefresher: @Sendable () async -> String?
    private let jobStatusProvider: @Sendable (UUID) async -> JobStatusSnapshot?

    private(set) var lastAppliedSeq: Int = 0
    private var lastAttempt: Int = 0
    private var isCancelled = false
    private var backoffSeconds: TimeInterval = 1.0
    private static let maxBackoff: TimeInterval = 60.0

    /// Watchdog timeout in seconds. If no SSE data (including heartbeats)
    /// arrives within this window, the stream is considered dead.
    private static let watchdogTimeoutSeconds: TimeInterval = 120.0
    private var lastSSEDataTime: Date = .distantPast

    struct JobStatusSnapshot: Sendable {
        let status: String
        let attempt: Int
        let lastSeq: Int
    }

    init(
        jobId: UUID,
        conversationId: UUID,
        clientMessageId: UUID,
        placeholderId: UUID? = nil,
        apiClient: RelayAPIClient,
        accessTokenProvider: @escaping @Sendable () async -> String?,
        accessTokenRefresher: @escaping @Sendable () async -> String?,
        jobStatusProvider: @escaping @Sendable (UUID) async -> JobStatusSnapshot?
    ) {
        self.jobId = jobId
        self.conversationId = conversationId
        self.clientMessageId = clientMessageId
        self.placeholderId = placeholderId
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
        self.accessTokenRefresher = accessTokenRefresher
        self.jobStatusProvider = jobStatusProvider
    }

    func cancel() {
        isCancelled = true
    }

    /// Run the SSE stream with automatic reconnect on transport failure.
    /// Yields `StreamingUpdate` values to the caller's continuation.
    /// Returns a `RunResult` indicating how the stream ended.
    func run(
        continuation: AsyncStream<StreamingUpdate>.Continuation
    ) async -> RunResult {
        var didRetryUnauthorized = false

        while !isCancelled {
            let accessToken = await accessTokenProvider()
            let cursorString = lastAppliedSeq > 0 ? String(lastAppliedSeq) : nil

            do {
                let stream = apiClient.streamEvents(
                    path: "jobs/\(jobId.uuidString.lowercased())/events",
                    accessToken: accessToken,
                    lastEventID: cursorString
                )

                var gotAnyEvent = false
                var streamTimedOut = false
                lastSSEDataTime = Date()

                for try await sseEvent in stream {
                    if self.isCancelled || Task.isCancelled { return .cancelled }
                    gotAnyEvent = true

                    // Reset watchdog on ANY SSE data, including comments/heartbeats
                    lastSSEDataTime = Date()

                    guard let envelope = self.parseEnvelope(from: sseEvent) else {
                        self.logger.debug("job=\(self.jobId.uuidString.prefix(8)) skipped event=\(sseEvent.event) id=\(sseEvent.id ?? "nil")")
                        continue
                    }

                    self.logger.debug("job=\(self.jobId.uuidString.prefix(8)) decoded seq=\(envelope.seq) type=\(envelope.type.rawValue) bytes=\(sseEvent.data.utf8.count)")

                    // Reject wrong-job events
                    guard envelope.jobId == self.jobId.uuidString.lowercased() else {
                        self.logger.warning("Rejecting event for wrong job: \(envelope.jobId)")
                        continue
                    }

                    // Detect seq gaps
                    if envelope.seq > self.lastAppliedSeq + 1 {
                        self.logger.warning("Seq gap detected: expected \(self.lastAppliedSeq + 1), got \(envelope.seq). Reconnecting.")
                        break  // Reconnect from lastAppliedSeq
                    }

                    // Skip duplicates
                    if envelope.seq <= self.lastAppliedSeq {
                        continue
                    }

                    self.lastAppliedSeq = envelope.seq
                    self.lastAttempt = envelope.attempt
                    self.backoffSeconds = 1.0  // Reset backoff on successful event

                    // Map envelope type to StreamingUpdate
                    if let update = self.mapToStreamingUpdate(envelope) {
                        self.logger.info("job=\(self.jobId.uuidString.prefix(8)) yield seq=\(envelope.seq) update=\(String(describing: update).prefix(40))")
                        continuation.yield(update)
                    }

                    // Terminal events end the stream
                    if envelope.type.isTerminal {
                        let terminalResult = self.extractTerminalResult(from: envelope)
                        switch envelope.type {
                        case .runCompleted: return .completed(terminalResult)
                        case .runFailed: return .failed(terminalResult)
                        case .runCancelled: return .cancelled
                        default: return .completed(terminalResult)
                        }
                    }
                }

                // Check if the stream was idle too long (heartbeat-aware watchdog).
                // If no SSE data arrived within the timeout, log and reconnect.
                let elapsed = Date().timeIntervalSince(lastSSEDataTime)
                if elapsed >= Self.watchdogTimeoutSeconds {
                    logger.warning("job=\(self.jobId.uuidString.prefix(8)) watchdog: no SSE data for \(elapsed)s, reconnecting")
                    streamTimedOut = true
                }

                // EOF without terminal event — check job status
                if gotAnyEvent {
                    continuation.yield(.reconnecting)
                }

                // Check authoritative job status
                if let status = await jobStatusProvider(jobId) {
                    switch status.status {
                    case "completed":
                        return .completed(nil)
                    case "failed":
                        return .failed(nil)
                    case "cancelled":
                        return .cancelled
                    case "queued", "running":
                        // Job is still live — reconnect with backoff
                        logger.info("Job \(self.jobId) still \(status.status), reconnecting after \(self.backoffSeconds)s")
                        try await Task.sleep(for: .seconds(backoffSeconds))
                        backoffSeconds = min(backoffSeconds * 2, Self.maxBackoff)
                        continue
                    default:
                        continuation.yield(.failed("Unexpected job status: \(status.status)"))
                        return .error("Unexpected job status: \(status.status)")
                    }
                } else if streamTimedOut {
                    // Watchdog fired and no job status available — reconnect
                    try await Task.sleep(for: .seconds(backoffSeconds))
                    backoffSeconds = min(backoffSeconds * 2, Self.maxBackoff)
                    continue
                } else {
                    continuation.yield(.failed("Could not fetch job status"))
                    return .error("Could not fetch job status")
                }

            } catch is CancellationError {
                return .cancelled
            } catch {
                // Transport or auth error
                if let clientError = error as? RelayAPIClient.ClientError {
                    if case .unauthorized = clientError {
                        if !didRetryUnauthorized {
                            didRetryUnauthorized = true
                            _ = await accessTokenRefresher()
                            continue
                        }
                    }
                }

                logger.error("SSE error for job \(self.jobId): \(error.localizedDescription)")
                continuation.yield(.reconnecting)
                try? await Task.sleep(for: .seconds(backoffSeconds))
                backoffSeconds = min(backoffSeconds * 2, Self.maxBackoff)
                continue
            }
        }
        return .cancelled
    }

    // MARK: - Parsing

    /// Extract terminal text from the `message` field, which may be either
    /// a plain string or a serialized message object with `content`/`role`.
    static func parseTerminalText(from json: [String: Any]) -> String? {
        guard let messageField = json["message"] else {
            return json["text"] as? String
        }

        // Handle serialized message object: {"content": "...", "role": "assistant"}
        if let messageDict = messageField as? [String: Any],
           let content = messageDict["content"] as? String {
            return content
        }

        // Handle legacy string form
        if let messageString = messageField as? String {
            return messageString
        }

        return json["text"] as? String
    }

    /// Extract terminal reasoning from the envelope, which may be a string
    /// or nested inside a reasoning object.
    static func parseTerminalReasoning(from json: [String: Any]) -> String? {
        guard let reasoningField = json["reasoning"] else { return nil }
        if let reasoningString = reasoningField as? String {
            return reasoningString
        }
        if let reasoningDict = reasoningField as? [String: Any],
           let content = reasoningDict["content"] as? String {
            return content
        }
        return nil
    }

    /// Parse an SSE event into a JobEventEnvelope.
    ///
    /// The relay's SSE wire format is:
    /// - `id:` = seq (integer)
    /// - `event:` = event type (text_delta, reasoning_delta, tool_activity, started, heartbeat, done, commentary)
    /// - `data:` = JSON payload (delta, label, status, message, usage, etc.)
    ///
    /// This is the canonical decoder for the relay→iOS contract. The relay strips
    /// the v2 envelope fields (contractVersion, jobId, conversationId, etc.) and
    /// sends only the payload in `data:`, with the type in `event:`.
    ///
    /// Returns `nil` for SSE comment keepalives (event == "comment" or empty data)
    /// without advancing the sequence counter.
    func parseEnvelope(from sseEvent: SSEEvent) -> JobEventEnvelope? {
        guard let data = sseEvent.data.data(using: .utf8) else { return nil }
        guard let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }

        let envelopeJobId = (json["jobId"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            ?? self.jobId.uuidString.lowercased()

        let seq: Int
        if let idString = sseEvent.id, let parsedSeq = Int(idString), parsedSeq > 0 {
            seq = parsedSeq
        } else {
            self.lastAppliedSeq += 1
            seq = self.lastAppliedSeq
        }

        let eventType: JobEventType
        let payload: JobEventPayload

        switch sseEvent.event {
        case "text_delta":
            eventType = .textDelta
            let delta = json["delta"] as? String ?? ""
            payload = .textDelta(TextDeltaPayload(delta: delta, segmentId: ""))

        case "reasoning_delta":
            eventType = .reasoningDelta
            let delta = json["delta"] as? String ?? ""
            payload = .reasoningDelta(ReasoningDeltaPayload(delta: delta, segmentId: ""))

        case "tool_activity":
            eventType = .toolProgress
            let label = json["label"] as? String ?? "Working..."
            payload = .toolProgress(ToolProgressPayload(toolCallId: "", label: label))

        case "started":
            eventType = .runStarted
            let phase = json["phase"] as? String ?? "starting"
            payload = .runStarted(RunStartedPayload(phase: phase, attempt: 0))

        case "heartbeat":
            eventType = .commentary
            let phase = json["phase"] as? String ?? "unknown"
            payload = .commentary(CommentaryPayload(text: phase))

        case "done":
            let status = json["status"] as? String ?? "completed"
            switch status {
            case "completed":
                eventType = .runCompleted
                let text = Self.parseTerminalText(from: json) ?? ""
                let usageDict = json["usage"] as? [String: Any]
                let usage = usageDict.map { Usage(
                    promptTokens: $0["prompt_tokens"] as? Int,
                    completionTokens: $0["completion_tokens"] as? Int,
                    totalTokens: $0["total_tokens"] as? Int
                )}
                payload = .runCompleted(RunCompletedPayload(messageId: "", text: text, usage: usage, diff: nil))
            case "failed":
                eventType = .runFailed
                let error = json["error"] as? String ?? "Unknown error"
                payload = .runFailed(RunFailedPayload(error: error, retryable: false))
            case "cancelled":
                eventType = .runCancelled
                let reason = json["error"] as? String ?? "Cancelled"
                payload = .runCancelled(RunCancelledPayload(reason: reason))
            default:
                return nil
            }

        case "comment":
            // SSE comment keepalive — do not advance sequence counter
            return nil

        default:
            return nil
        }

        return JobEventEnvelope(
            contractVersion: 1,
            jobId: envelopeJobId,
            conversationId: "",
            attempt: 0,
            seq: seq,
            type: eventType,
            timestamp: Date(),
            payload: payload
        )
    }

    private func mapToStreamingUpdate(_ envelope: JobEventEnvelope) -> StreamingUpdate? {
        switch envelope.type {
        case .runStarted:
            if case .runStarted(let payload) = envelope.payload {
                return .started(phase: payload.phase)
            }
            return .started(phase: "starting")
        case .textDelta:
            if case .textDelta(let payload) = envelope.payload {
                return .textDelta(payload.delta)
            }
            return nil
        case .reasoningDelta:
            if case .reasoningDelta(let payload) = envelope.payload {
                return .reasoningDelta(payload.delta)
            }
            return nil
        case .toolStarted:
            if case .toolStarted(let payload) = envelope.payload {
                return .toolActivity(payload.name)
            }
            return .toolActivity("Working...")
        case .toolProgress:
            if case .toolProgress(let payload) = envelope.payload {
                return .toolActivity(payload.label)
            }
            return .toolActivity("Working...")
        case .toolCompleted:
            return .toolActivity("Done")
        case .commentary:
            return .heartbeat(phase: "commentary")
        case .approvalRequired:
            if case .approvalRequired(let payload) = envelope.payload {
                return .toolActivity(payload.prompt)
            }
            return .toolActivity("Approval needed")
        case .runCompleted:
            return nil
        case .runFailed:
            return nil
        case .runCancelled:
            return .cancelled
        case .runRequeued:
            return .reconnecting
        }
    }

    private func extractTerminalResult(from envelope: JobEventEnvelope) -> TerminalResult {
        switch envelope.payload {
        case .runCompleted(let p):
            return TerminalResult(
                text: p.text,
                promptTokens: p.usage?.promptTokens,
                completionTokens: p.usage?.completionTokens,
                totalTokens: p.usage?.totalTokens,
                error: nil
            )
        case .runFailed(let p):
            return TerminalResult(text: nil, promptTokens: nil, completionTokens: nil, totalTokens: nil, error: p.error)
        case .runCancelled(let p):
            return TerminalResult(text: nil, promptTokens: nil, completionTokens: nil, totalTokens: nil, error: p.reason)
        default:
            return TerminalResult(text: nil, promptTokens: nil, completionTokens: nil, totalTokens: nil, error: nil)
        }
    }
}
