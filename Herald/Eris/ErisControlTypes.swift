import Foundation

// MARK: - Eris control-plane wire types
//
// Shapes mirror `crates/eris-host/src/device/control.rs`. That file serves
// snake_case and the rest of `/v1` does the same, so every type here names its
// keys rather than relying on a decoding strategy the app does not install.
//
// Two absences are deliberate on the daemon side and mirrored here: a provider
// row carries an id, a name and a primary flag and never an endpoint or a key
// reference, and a model row carries the probed catalog's id rather than a
// credential.

/// `{}` - the body and the response of a route nothing is carried on.
struct ErisControlEmpty: Codable, Sendable {}

/// One provider as `/v1/models` and `/v1/aux` report it.
struct ErisControlProvider: Decodable, Sendable, Identifiable {
    let id: String
    let name: String
    let primary: Bool
}

/// One model row. `provider` is the primary provider's id, which is the only
/// provider this daemon probes; it is nil on a daemon with no provider at all.
struct ErisControlModelRow: Decodable, Sendable, Identifiable {
    let id: String
    let provider: String?
    let contextWindow: Int?
    let isDefault: Bool

    private enum CodingKeys: String, CodingKey {
        case id
        case provider
        case contextWindow = "context_window"
        case isDefault = "is_default"
    }
}

/// `GET /v1/models`.
struct ErisControlModels: Decodable, Sendable {
    let ok: Bool
    let model: String
    /// False means no probe has published a list, which is a different state
    /// from a probe that published an empty one.
    let probed: Bool
    let providers: [ErisControlProvider]
    let models: [ErisControlModelRow]
}

/// `POST /v1/models` body.
struct ErisControlModelBody: Encodable, Sendable {
    let model: String
}

/// `POST /v1/models` response.
struct ErisControlModelSet: Decodable, Sendable {
    let ok: Bool
    let model: String
}

/// One schedule as `/v1/cron` carries it. The stamps are the RFC 3339 UTC
/// strings the database holds, passed through rather than reformatted.
struct ErisControlCronJob: Decodable, Sendable, Identifiable {
    let id: String
    let name: String
    let schedule: String
    let prompt: String
    let enabled: Bool
    let nextRun: String?
    let lastRun: String?
    let lastStatus: String?
    let lastError: String?
    let firedCount: Int

    private enum CodingKeys: String, CodingKey {
        case id, name, schedule, prompt, enabled
        case nextRun = "next_run"
        case lastRun = "last_run"
        case lastStatus = "last_status"
        case lastError = "last_error"
        case firedCount = "fired_count"
    }
}

/// `GET /v1/cron`. `enabled` is the global ticker switch, on the same read as
/// the rows so a client can explain "nothing fired" without a second request.
struct ErisControlCronList: Decodable, Sendable {
    let ok: Bool
    let enabled: Bool
    let timezone: String
    let count: Int
    let jobs: [ErisControlCronJob]
}

/// `POST /v1/cron` and the `PATCH` content body.
struct ErisControlCronContent: Encodable, Sendable {
    let name: String
    let schedule: String
    let prompt: String
}

/// The `PATCH` switch body.
struct ErisControlCronFlag: Encodable, Sendable {
    let enabled: Bool
}

/// `{ok, job}` - what every schedule write answers with, read back from the
/// store rather than echoed from the request.
struct ErisControlCronJobEnvelope: Decodable, Sendable {
    let ok: Bool
    let job: ErisControlCronJob
}

/// One skill in the library.
struct ErisControlSkill: Decodable, Sendable, Identifiable {
    let key: String
    let name: String
    let description: String?
    let path: String?
    let enabled: Bool
    let bytes: Int

    var id: String { key }
}

/// `GET /v1/skills`. A library that cannot be read answers `ok: true` with
/// `library_error` set and an empty list, so the view can say which of the two
/// empty states it is in.
struct ErisControlSkillList: Decodable, Sendable {
    let ok: Bool
    let enabled: Bool
    let dir: String?
    let libraryError: String?
    let skills: [ErisControlSkill]

    private enum CodingKeys: String, CodingKey {
        case ok, enabled, dir, skills
        case libraryError = "library_error"
    }
}

/// One skill's body.
struct ErisControlSkillDetail: Decodable, Sendable {
    let key: String
    let name: String
    let description: String?
    let path: String?
    let content: String
}

/// `GET /v1/skills/{key}`.
struct ErisControlSkillEnvelope: Decodable, Sendable {
    let ok: Bool
    let skill: ErisControlSkillDetail
}

/// One auxiliary role. `inherits` is the honest form of "follows the primary";
/// `pinned` is null in exactly that case, so a client never has to infer it
/// from a match that might be a coincidence.
struct ErisControlAuxRole: Decodable, Sendable, Identifiable {
    let key: String
    let label: String
    let purpose: String
    let consumedBy: String
    let model: String
    let pinned: String?
    let inherits: Bool
    let provider: String
    let pinnedProvider: String?
    let probeValidated: Bool

    var id: String { key }

    private enum CodingKeys: String, CodingKey {
        case key, label, purpose, model, pinned, inherits, provider
        case consumedBy = "consumed_by"
        case pinnedProvider = "pinned_provider"
        case probeValidated = "probe_validated"
    }
}

/// `GET /v1/aux`. The ids a picker needs ride along so a row can be drawn from
/// one read rather than three.
struct ErisControlAuxList: Decodable, Sendable {
    let ok: Bool
    let primaryModel: String
    let roles: [ErisControlAuxRole]
    let models: [String]
    let providers: [ErisControlProvider]

    private enum CodingKeys: String, CodingKey {
        case ok, roles, models, providers
        case primaryModel = "primary_model"
    }
}

/// `POST /v1/aux` body. One role per request on purpose: a body carrying seven
/// would let one mistyped id refuse the six that were fine.
struct ErisControlAuxBody: Encodable, Sendable {
    let role: String
    let provider: String
    let model: String
}

/// `POST /v1/aux` response.
struct ErisControlAuxSet: Decodable, Sendable {
    let ok: Bool
}

// MARK: - Subagent wire types (GET /v1/subagents)
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
