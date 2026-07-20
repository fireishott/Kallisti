import Foundation
import os

/// Coordinates a single job's SSE event stream with cursor-based resume.
/// Replaces the one-shot `streamJobEvents` that could not reconnect.
actor JobStreamCoordinator {
    enum RunResult: Sendable {
        case completed
        case failed
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

    private var lastAppliedSeq: Int = 0
    private var lastAttempt: Int = 0
    private var isCancelled = false
    private var backoffSeconds: TimeInterval = 1.0
    private static let maxBackoff: TimeInterval = 60.0

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

                for try await sseEvent in stream {
                    if self.isCancelled || Task.isCancelled { return .cancelled }
                    gotAnyEvent = true

                    guard let envelope = self.parseEnvelope(from: sseEvent) else {
                        self.logger.warning("Failed to parse event envelope for seq \(sseEvent.id ?? "nil")")
                        continue
                    }

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
                        continuation.yield(update)
                    }

                    // Terminal events end the stream
                    if envelope.type.isTerminal {
                        switch envelope.type {
                        case .runCompleted: return .completed
                        case .runFailed: return .failed
                        case .runCancelled: return .cancelled
                        default: return .completed
                        }
                    }
                }

                // EOF without terminal event — check job status
                if gotAnyEvent {
                    continuation.yield(.reconnecting)
                }

                // Check authoritative job status
                if let status = await jobStatusProvider(jobId) {
                    switch status.status {
                    case "completed":
                        return .completed
                    case "failed":
                        return .failed
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

    private func parseEnvelope(from sseEvent: SSEEvent) -> JobEventEnvelope? {
        guard let data = sseEvent.data.data(using: .utf8) else { return nil }
        do {
            let decoder = JSONDecoder()
            decoder.dateDecodingStrategy = .iso8601
            var envelope = try decoder.decode(JobEventEnvelope.self, from: data)
            // The server may send type in the SSE event field; use that if payload type is missing
            if envelope.type == .runStarted && sseEvent.event != "run.started" {
                if JobEventType(rawValue: sseEvent.event) != nil {
                    var dict = try JSONSerialization.jsonObject(with: data) as? [String: Any] ?? [:]
                    dict["type"] = sseEvent.event
                    let fixedData = try JSONSerialization.data(withJSONObject: dict)
                    envelope = try decoder.decode(JobEventEnvelope.self, from: fixedData)
                }
            }
            return envelope
        } catch {
            // Fall back to v1 format if v2 parse fails
            // DEPRECATED: v1 fallback will be removed in a future release once
            // metrics confirm all clients are on v2. See Phase A-4 in the streaming plan.
            return parseV1Fallback(from: sseEvent)
        }
    }

    /// DEPRECATED: Parses legacy v1 SSE event format. Kept temporarily for backward
    /// compatibility during the v1→v2 transition. Will be removed once dogfood metrics
    /// confirm v1 usage is negligible.
    private func parseV1Fallback(from sseEvent: SSEEvent) -> JobEventEnvelope? {
        guard let data = sseEvent.data.data(using: .utf8) else { return nil }
        guard let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }

        // Use the coordinator's known jobId when the legacy payload omits it
        let envelopeJobId = (json["jobId"] as? String).flatMap { $0.isEmpty ? nil : $0 }
            ?? self.jobId.uuidString.lowercased()

        // For legacy frames without id:, assign a local next sequence
        // rather than passing seq=0 into duplicate/gap logic
        let seq: Int
        if let idString = sseEvent.id, let parsedSeq = Int(idString), parsedSeq > 0 {
            seq = parsedSeq
        } else {
            // Assign a compatibility-local sequence that advances past lastAppliedSeq
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
                let text = json["message"] as? String ?? json["text"] as? String ?? ""
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
}
