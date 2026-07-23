import Foundation

// MARK: - Per-event payloads

struct RunStartedPayload: Codable, Sendable, Hashable {
    let phase: String
    let attempt: Int
}

struct TextDeltaPayload: Codable, Sendable, Hashable {
    let delta: String
    let segmentId: String
}

struct ReasoningDeltaPayload: Codable, Sendable, Hashable {
    let delta: String
    let segmentId: String
}

struct ToolStartedPayload: Codable, Sendable, Hashable {
    let toolCallId: String
    let name: String
    let args: String
}

struct ToolProgressPayload: Codable, Sendable, Hashable {
    let toolCallId: String
    let label: String
}

struct ToolCompletedPayload: Codable, Sendable, Hashable {
    let toolCallId: String
    let output: String
}

struct CommentaryPayload: Codable, Sendable, Hashable {
    let text: String
}

struct ApprovalRequiredPayload: Codable, Sendable, Hashable {
    let toolCallId: String
    let prompt: String
}

struct Usage: Codable, Sendable, Hashable {
    let promptTokens: Int?
    let completionTokens: Int?
    let totalTokens: Int?

    enum CodingKeys: String, CodingKey {
        case promptTokens = "prompt_tokens"
        case completionTokens = "completion_tokens"
        case totalTokens = "total_tokens"
    }
}

struct DiffFile: Codable, Sendable, Hashable {
    let path: String
    let status: String
}

struct Diff: Codable, Sendable, Hashable {
    let files: [DiffFile]
    let summary: String
}

struct RunCompletedPayload: Codable, Sendable, Hashable {
    let messageId: String
    let text: String
    let usage: Usage?
    let diff: Diff?
}

struct RunFailedPayload: Codable, Sendable, Hashable {
    let error: String
    let retryable: Bool
    let errorCategory: String?
    let errorAction: String?
}

struct RunCancelledPayload: Codable, Sendable, Hashable {
    let reason: String
}

struct RunRequeuedPayload: Codable, Sendable, Hashable {
    let fromAttempt: Int
    let toAttempt: Int
}

// MARK: - Event type enum

enum JobEventType: String, Codable, Sendable {
    case runStarted = "run.started"
    case textDelta = "text.delta"
    case reasoningDelta = "reasoning.delta"
    case toolStarted = "tool.started"
    case toolProgress = "tool.progress"
    case toolCompleted = "tool.completed"
    case commentary
    case approvalRequired = "approval.required"
    case runCompleted = "run.completed"
    case runFailed = "run.failed"
    case runCancelled = "run.cancelled"
    case runRequeued = "run.requeued"

    var isTerminal: Bool {
        switch self {
        case .runCompleted, .runFailed, .runCancelled:
            return true
        default:
            return false
        }
    }
}

// MARK: - Typed payload wrapper

enum JobEventPayload: Sendable, Hashable {
    case runStarted(RunStartedPayload)
    case textDelta(TextDeltaPayload)
    case reasoningDelta(ReasoningDeltaPayload)
    case toolStarted(ToolStartedPayload)
    case toolProgress(ToolProgressPayload)
    case toolCompleted(ToolCompletedPayload)
    case commentary(CommentaryPayload)
    case approvalRequired(ApprovalRequiredPayload)
    case runCompleted(RunCompletedPayload)
    case runFailed(RunFailedPayload)
    case runCancelled(RunCancelledPayload)
    case runRequeued(RunRequeuedPayload)
}

// MARK: - Envelope

struct JobEventEnvelope: Codable, Sendable, Hashable {
    let contractVersion: Int
    let jobId: String
    let conversationId: String
    let attempt: Int
    let seq: Int
    let type: JobEventType
    let timestamp: Date
    let payload: JobEventPayload

    enum CodingKeys: String, CodingKey {
        case contractVersion
        case jobId
        case conversationId
        case attempt
        case seq
        case type
        case timestamp
        case payload
    }

    // MARK: Codable

    init(
        contractVersion: Int,
        jobId: String,
        conversationId: String,
        attempt: Int,
        seq: Int,
        type: JobEventType,
        timestamp: Date,
        payload: JobEventPayload
    ) {
        self.contractVersion = contractVersion
        self.jobId = jobId
        self.conversationId = conversationId
        self.attempt = attempt
        self.seq = seq
        self.type = type
        self.timestamp = timestamp
        self.payload = payload
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        contractVersion = try container.decode(Int.self, forKey: .contractVersion)
        jobId = try container.decode(String.self, forKey: .jobId)
        conversationId = try container.decode(String.self, forKey: .conversationId)
        attempt = try container.decode(Int.self, forKey: .attempt)
        seq = try container.decode(Int.self, forKey: .seq)
        type = try container.decode(JobEventType.self, forKey: .type)
        timestamp = try container.decode(Date.self, forKey: .timestamp)

        switch type {
        case .runStarted:
            payload = .runStarted(try container.decode(RunStartedPayload.self, forKey: .payload))
        case .textDelta:
            payload = .textDelta(try container.decode(TextDeltaPayload.self, forKey: .payload))
        case .reasoningDelta:
            payload = .reasoningDelta(try container.decode(ReasoningDeltaPayload.self, forKey: .payload))
        case .toolStarted:
            payload = .toolStarted(try container.decode(ToolStartedPayload.self, forKey: .payload))
        case .toolProgress:
            payload = .toolProgress(try container.decode(ToolProgressPayload.self, forKey: .payload))
        case .toolCompleted:
            payload = .toolCompleted(try container.decode(ToolCompletedPayload.self, forKey: .payload))
        case .commentary:
            payload = .commentary(try container.decode(CommentaryPayload.self, forKey: .payload))
        case .approvalRequired:
            payload = .approvalRequired(try container.decode(ApprovalRequiredPayload.self, forKey: .payload))
        case .runCompleted:
            payload = .runCompleted(try container.decode(RunCompletedPayload.self, forKey: .payload))
        case .runFailed:
            payload = .runFailed(try container.decode(RunFailedPayload.self, forKey: .payload))
        case .runCancelled:
            payload = .runCancelled(try container.decode(RunCancelledPayload.self, forKey: .payload))
        case .runRequeued:
            payload = .runRequeued(try container.decode(RunRequeuedPayload.self, forKey: .payload))
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(contractVersion, forKey: .contractVersion)
        try container.encode(jobId, forKey: .jobId)
        try container.encode(conversationId, forKey: .conversationId)
        try container.encode(attempt, forKey: .attempt)
        try container.encode(seq, forKey: .seq)
        try container.encode(type, forKey: .type)
        try container.encode(timestamp, forKey: .timestamp)

        switch payload {
        case .runStarted(let p):
            try container.encode(p, forKey: .payload)
        case .textDelta(let p):
            try container.encode(p, forKey: .payload)
        case .reasoningDelta(let p):
            try container.encode(p, forKey: .payload)
        case .toolStarted(let p):
            try container.encode(p, forKey: .payload)
        case .toolProgress(let p):
            try container.encode(p, forKey: .payload)
        case .toolCompleted(let p):
            try container.encode(p, forKey: .payload)
        case .commentary(let p):
            try container.encode(p, forKey: .payload)
        case .approvalRequired(let p):
            try container.encode(p, forKey: .payload)
        case .runCompleted(let p):
            try container.encode(p, forKey: .payload)
        case .runFailed(let p):
            try container.encode(p, forKey: .payload)
        case .runCancelled(let p):
            try container.encode(p, forKey: .payload)
        case .runRequeued(let p):
            try container.encode(p, forKey: .payload)
        }
    }
}
