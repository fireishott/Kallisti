import Foundation

/// Projection of a job's state, built by reducing events from the durable log.
/// Pure function: same events → same projection, no side effects.
struct JobProjection: Sendable {
    var jobId: String
    var conversationId: String
    var attempt: Int = 0
    var lastAppliedSeq: Int = 0
    var phase: JobPhase = .queued
    var textSegments: [TextSegment] = []
    var reasoningSegments: [TextSegment] = []
    var toolActivities: [ToolProjection] = []
    var isTerminal: Bool = false
    var terminalEvent: TerminalEvent?
    var canonicalText: String?
    var errorMessage: String?

    struct TextSegment: Sendable, Identifiable {
        let id: String
        var text: String
    }

    struct ToolProjection: Sendable, Identifiable {
        let id: String
        var name: String
        var label: String
        var isActive: Bool
        var output: String?
    }

    enum JobPhase: Sendable {
        case queued
        case starting
        case thinking
        case writing
        case tool
        case completed
        case failed
        case cancelled
    }

    enum TerminalEvent: Sendable {
        case completed(messageId: String?, text: String?)
        case failed(error: String?, retryable: Bool)
        case cancelled(reason: String?)
    }
}

/// Pure reducer: applies a single event to a projection, returning the updated projection.
/// Duplicate events (seq <= lastAppliedSeq) and stale attempts are no-ops.
enum JobEventReducer {
    static func reduce(
        _ projection: inout JobProjection,
        event: JobEventEnvelope
    ) {
        // Reject stale attempts
        guard event.attempt >= projection.attempt else { return }

        // If attempt advanced, reset mutable state
        if event.attempt > projection.attempt {
            projection.attempt = event.attempt
            projection.lastAppliedSeq = 0
            projection.textSegments = []
            projection.reasoningSegments = []
            projection.toolActivities = []
            projection.isTerminal = false
            projection.terminalEvent = nil
            projection.canonicalText = nil
            projection.phase = .starting
        }

        // Reject duplicates
        guard event.seq > projection.lastAppliedSeq else { return }

        // Detect seq gap — caller must reconnect and replay from lastAppliedSeq
        guard event.seq == projection.lastAppliedSeq + 1 else { return }

        projection.lastAppliedSeq = event.seq

        switch event.type {
        case .runStarted:
            projection.phase = .starting

        case .textDelta:
            if case .textDelta(let payload) = event.payload {
                let segmentId = payload.segmentId
                if let idx = projection.textSegments.firstIndex(where: { $0.id == segmentId }) {
                    projection.textSegments[idx].text += payload.delta
                } else {
                    projection.textSegments.append(.init(id: segmentId, text: payload.delta))
                }
                projection.phase = .writing
            }

        case .reasoningDelta:
            if case .reasoningDelta(let payload) = event.payload {
                let segmentId = payload.segmentId
                if let idx = projection.reasoningSegments.firstIndex(where: { $0.id == segmentId }) {
                    projection.reasoningSegments[idx].text += payload.delta
                } else {
                    projection.reasoningSegments.append(.init(id: segmentId, text: payload.delta))
                }
                projection.phase = .thinking
            }

        case .toolStarted:
            if case .toolStarted(let payload) = event.payload {
                projection.toolActivities.append(.init(
                    id: payload.toolCallId,
                    name: payload.name,
                    label: payload.name,
                    isActive: true
                ))
                projection.phase = .tool
            }

        case .toolProgress:
            if case .toolProgress(let payload) = event.payload {
                if let idx = projection.toolActivities.firstIndex(where: { $0.id == payload.toolCallId }) {
                    projection.toolActivities[idx].label = payload.label
                }
                projection.phase = .tool
            }

        case .toolCompleted:
            if case .toolCompleted(let payload) = event.payload {
                if let idx = projection.toolActivities.firstIndex(where: { $0.id == payload.toolCallId }) {
                    projection.toolActivities[idx].isActive = false
                    projection.toolActivities[idx].output = payload.output
                }
            }

        case .commentary:
            break

        case .approvalRequired:
            projection.phase = .tool

        case .runCompleted:
            if case .runCompleted(let payload) = event.payload {
                projection.isTerminal = true
                projection.phase = .completed
                projection.terminalEvent = .completed(messageId: payload.messageId, text: payload.text)
                projection.canonicalText = payload.text
            }

        case .runFailed:
            if case .runFailed(let payload) = event.payload {
                projection.isTerminal = true
                projection.phase = .failed
                projection.terminalEvent = .failed(error: payload.error, retryable: payload.retryable)
                projection.errorMessage = payload.error
            }

        case .runCancelled:
            if case .runCancelled(let payload) = event.payload {
                projection.isTerminal = true
                projection.phase = .cancelled
                projection.terminalEvent = .cancelled(reason: payload.reason)
            }

        case .runRequeued:
            projection.phase = .queued
        }
    }

    /// Reduce a batch of events.
    static func reduceAll(
        _ projection: inout JobProjection,
        events: [JobEventEnvelope]
    ) {
        for event in events {
            reduce(&projection, event: event)
        }
    }
}
