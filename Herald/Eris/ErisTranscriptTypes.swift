import Foundation

// MARK: - What the 1.6.0 routes answer
//
// Every one of these decodes the daemon's flat `{"ok": true, ...}` body. They are
// kept in one file because they arrive together: the transcript index, the
// background process table, the cost numbers and the two memory files are four
// reads of state this daemon already held and never published until 1.6.0.

// MARK: - Transcript search

/// One conversation that matched, as `/v1/search` groups it.
///
/// The daemon answers one row per conversation rather than one per message,
/// because that is the question a person types: "where did we talk about this".
/// `matches` is how many messages inside it hit, and `snippet` is the best one.
struct ErisSearchHit: Decodable, Identifiable, Sendable, Hashable {
    let conversation: String
    let title: String
    let snippet: String
    let createdAt: String
    let matches: Int

    var id: String { conversation }

    enum CodingKeys: String, CodingKey {
        case conversation, title, snippet, matches
        case createdAt = "created_at"
    }
}

struct ErisSearchPage: Decodable, Sendable {
    let query: String
    let hits: [ErisSearchHit]
    let returned: Int
}

// MARK: - Background processes

/// One process the daemon is running, or ran.
struct ErisProcessRow: Decodable, Identifiable, Sendable, Hashable {
    let processId: String
    let pid: Int?
    let command: String
    let startedAt: String
    let status: String
    let bytes: Int?
    /// Present when a signal ended it: the daemon names the signal and the likely
    /// cause, so a phone user does not have to know what `exit -9` means.
    let signalNote: String?

    var id: String { processId }

    /// Whether this is still work in progress.
    var isRunning: Bool { status == "running" }

    enum CodingKeys: String, CodingKey {
        case pid, command, status, bytes
        case processId = "process_id"
        case startedAt = "started_at"
        case signalNote = "signal_note"
    }
}

struct ErisProcessPage: Decodable, Sendable {
    let processes: [ErisProcessRow]
    let running: Int
    let total: Int
}

/// The answer to a kill. The daemon includes the handle it stopped; the app only
/// needs to know the call succeeded and refreshes the list afterwards.
struct ErisProcessKilled: Decodable, Sendable {
    let ok: Bool
}

// MARK: - Cost and latency

/// One model's numbers inside a window.
struct ErisMetricModel: Decodable, Identifiable, Sendable, Hashable {
    let model: String
    let calls: Int
    let failed: Int
    let medianMs: Double?
    let p95Ms: Double?
    let promptTokens: Int
    let completionTokens: Int
    let cacheRate: Double

    var id: String { model }

    enum CodingKeys: String, CodingKey {
        case model, calls, failed
        case medianMs = "median_ms"
        case p95Ms = "p95_ms"
        case promptTokens = "prompt_tokens"
        case completionTokens = "completion_tokens"
        case cacheRate = "cache_rate"
    }
}

struct ErisMetrics: Decodable, Sendable {
    let window: String
    let calls: Int
    let failed: Int
    let promptTokens: Int
    let completionTokens: Int
    let cacheRate: Double
    /// Rows that recorded no duration, so the percentiles say what they skip.
    let unmeasured: Int
    let models: [ErisMetricModel]

    enum CodingKeys: String, CodingKey {
        case window, calls, failed, unmeasured, models
        case promptTokens = "prompt_tokens"
        case completionTokens = "completion_tokens"
        case cacheRate = "cache_rate"
    }
}

// MARK: - Memory

/// One of the assistant's two bounded note files.
struct ErisMemoryFile: Decodable, Identifiable, Sendable, Hashable {
    let target: String
    let file: String
    let used: Int
    let limit: Int
    let remaining: Int
    let entries: [String]

    var id: String { file }

    /// A person-readable heading for the file.
    var title: String {
        target == "user" ? "What it knows about you" : "Notes to itself"
    }
}

struct ErisMemorySnapshot: Decodable, Sendable {
    let files: [ErisMemoryFile]
}

struct ErisMemoryEnvelope: Decodable, Sendable {
    let memory: ErisMemorySnapshot
}
