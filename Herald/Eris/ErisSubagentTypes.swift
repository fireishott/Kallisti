import Foundation

// MARK: - Subagent wire types
//
// Shapes mirror `crates/eris-host/src/device/handlers.rs` (`GET /v1/subagents`
// and `GET /v1/subagents/{id}`). The daemon serves snake_case and names its own
// status words, so every key is named here rather than decoded through a
// strategy, and `status` stays a string: a client should render a word it was
// given, not refuse a row that used one it did not know.

/// One subagent as the daemon reports it.
struct ErisSubagent: Decodable, Sendable, Identifiable, Equatable {
    let id: String
    let label: String
    let prompt: String
    let model: String
    /// `queued`, `running`, `succeeded`, `failed` or `cancelled`.
    let status: String
    /// What it produced, once it produced something.
    let result: String?
    /// The short code a failure carries.
    let errorCode: String?
    let parentConversation: String
    let parentTurn: String?
    let createdAt: String
    let startedAt: String?
    let finishedAt: String?
    let elapsedSeconds: Int
    /// The daemon's own rendering of the elapsed time ("12s", "3m 4s").
    let elapsed: String
    let promptTokens: Int?
    let completionTokens: Int?

    /// Still working, which is what the badge counts.
    var isActive: Bool { status == "queued" || status == "running" }

    /// Whether this row is in a terminal state.
    var isFinished: Bool { !isActive }

    private enum CodingKeys: String, CodingKey {
        case id
        case label
        case prompt
        case model
        case status
        case result
        case elapsed
        case errorCode = "error_code"
        case parentConversation = "parent_conversation"
        case parentTurn = "parent_turn"
        case createdAt = "created_at"
        case startedAt = "started_at"
        case finishedAt = "finished_at"
        case elapsedSeconds = "elapsed_secs"
        case promptTokens = "prompt_tokens"
        case completionTokens = "completion_tokens"
    }
}

/// `GET /v1/subagents`.
struct ErisSubagentList: Decodable, Sendable {
    let ok: Bool
    let count: Int
    /// True when the daemon bound the list and there are older rows behind it.
    let truncated: Bool
    let subagents: [ErisSubagent]
}

/// `GET /v1/subagents/{id}`.
struct ErisSubagentDetail: Decodable, Sendable {
    let ok: Bool
    let subagent: ErisSubagent
}
