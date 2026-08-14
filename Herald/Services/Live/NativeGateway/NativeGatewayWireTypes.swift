import Foundation

// MARK: - JSON-RPC 2.0 Envelope

/// A JSON-RPC 2.0 request frame sent to the native gateway.
struct JSONRPCRequest<Params: Encodable>: Encodable {
    let jsonrpc = "2.0"
    let id: Int
    let method: String
    let params: Params
}

/// A response frame correlated to a request by `id`.
/// Exactly one of `result`/`error` is non-nil per JSON-RPC 2.0.
struct NativeGatewayResponse: Decodable {
    let jsonrpc: String?
    let id: Int
    let result: NativeJSONValue?
    let error: NativeGatewayError?
}

struct NativeGatewayError: Decodable, Error, Equatable, LocalizedError {
    let code: Int
    let message: String

    var errorDescription: String? {
        message.isEmpty ? "The gateway rejected the request (\(code))." : message
    }
}

// MARK: - Server-initiated Events

/// A server-initiated event — dispatched by `params.type`.
/// Shape confirmed from live capture: `{"jsonrpc":"2.0","method":"event","params":{"type":"...","session_id":"...","payload":{...}}}`
struct NativeGatewayEvent: Decodable {
    let jsonrpc: String?
    let method: String?
    let params: NativeGatewayEventParams
}

struct NativeGatewayEventParams: Decodable {
    let type: String
    let sessionId: String?
    let payload: NativeJSONValue?

    enum CodingKeys: String, CodingKey {
        case type
        case sessionId = "session_id"
        case payload
    }

    /// Decode the payload as a specific Decodable type.
    func decodePayload<T: Decodable>(_ type: T.Type) -> T? {
        guard let payload else { return nil }
        guard let data = try? JSONEncoder().encode(payload) else { return nil }
        return try? JSONDecoder().decode(T.self, from: data)
    }
}

// MARK: - Typed Event Payloads

/// `message.delta` — streaming text chunk.
/// Shape: `{"type":"message.delta","session_id":"...","payload":{"text":"..."}}`
struct NativeMessageDeltaPayload: Decodable {
    let text: String
}

/// `thinking.delta` — reasoning/thinking text chunk.
/// Shape: `{"type":"thinking.delta","session_id":"...","payload":{"text":"..."}}`
struct NativeThinkingDeltaPayload: Decodable {
    let text: String
}

/// `message.complete` — terminal event for a turn.
/// Shape: `{"type":"message.complete","session_id":"...","payload":{"text":"...","usage":{...},"status":"complete"}}`
struct NativeMessageCompletePayload: Decodable {
    let text: String
    let usage: NativeUsagePayload?
    let status: String?
}

struct NativeUsagePayload: Decodable {
    let model: String?
    let input: Int?
    let output: Int?
    let reasoning: Int?
    let total: Int?
    let calls: Int?
}

/// `tool.start` — a tool invocation has begun.
/// The gateway (tui_gateway/server.py `_on_tool_start`) ships
/// `{"tool_id","name","context","args"}` — note `tool_id`, not `tool_call_id`,
/// and `context` as the display preview. Decode tolerantly against both
/// spellings so the tool bubble renders and tool.complete can correlate.
struct NativeToolStartPayload: Decodable {
    let toolCallID: String?
    let name: String?
    let preview: String?
    let emoji: String?

    enum CodingKeys: String, CodingKey {
        case toolCallID = "tool_call_id"
        case toolID = "tool_id"
        case name
        case preview
        case context
        case emoji
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        toolCallID = try c.decodeIfPresent(String.self, forKey: .toolCallID)
            ?? c.decodeIfPresent(String.self, forKey: .toolID)
        name = try c.decodeIfPresent(String.self, forKey: .name)
        preview = try c.decodeIfPresent(String.self, forKey: .preview)
            ?? c.decodeIfPresent(String.self, forKey: .context)
        emoji = try c.decodeIfPresent(String.self, forKey: .emoji)
    }
}

/// `tool.complete` — a tool invocation has finished.
/// Gateway ships `{"tool_id","name","args","result","duration_s"}`. Decode
/// tolerantly (`tool_id` alias, `result` for output, `duration_s` seconds for
/// the app's `duration_ms` millis) so completion can mark the activity done.
struct NativeToolCompletePayload: Decodable {
    let toolCallID: String?
    let output: String?
    let isError: Bool?
    let durationMs: Int?
    /// ANSI-colored inline unified diff rendered by the gateway for file-edit
    /// tools (patch/write_file/skill_manage). Populated from server.py
    /// `payload["inline_diff"]` (display.py `render_edit_diff_with_delta`).
    /// Parsed by `CodeDiffParser` into the app's CodeDiff model.
    let inlineDiff: String?

    enum CodingKeys: String, CodingKey {
        case toolCallID = "tool_call_id"
        case toolID = "tool_id"
        case output
        case result
        case isError = "is_error"
        case error
        case durationMs = "duration_ms"
        case durationS = "duration_s"
        case inlineDiff = "inline_diff"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        toolCallID = try c.decodeIfPresent(String.self, forKey: .toolCallID)
            ?? c.decodeIfPresent(String.self, forKey: .toolID)
        // The gateway ships `result` as a JSON value that may be a string,
        // object or list (server.py `json.loads(result)`). decodeIfPresent
        // THROWS on a type mismatch, so use try? and stringify objects.
        if let v = try? c.decode(String.self, forKey: .output) {
            output = v
        } else if let v = try? c.decode(String.self, forKey: .result) {
            output = v
        } else if let v = try? c.decode([String: NativeJSONValue].self, forKey: .result) {
            output = v.isEmpty ? nil : String(describing: v)
        } else if let v = try? c.decode([NativeJSONValue].self, forKey: .result) {
            output = v.isEmpty ? nil : String(describing: v)
        } else {
            output = nil
        }
        isError = try c.decodeIfPresent(Bool.self, forKey: .isError)
            ?? (c.contains(.error) ? true : nil)
        durationMs = try c.decodeIfPresent(Int.self, forKey: .durationMs)
            ?? c.decodeIfPresent(Int.self, forKey: .durationS).map { Int($0 * 1000) }
        inlineDiff = try c.decodeIfPresent(String.self, forKey: .inlineDiff)
    }
}

/// `tool.output` — a live stdout chunk streamed during a tool invocation.
/// Gateway ships `{"tool_id","name","chunk"}` (see `_on_tool_output` in
/// tui_gateway/server.py). `tool_id` correlates with the `tool.start` /
/// `tool.complete` toolCallID so the app can append chunks in place.
struct NativeToolOutputPayload: Decodable {
    let toolCallID: String?
    let name: String?
    let chunk: String?

    enum CodingKeys: String, CodingKey {
        case toolCallID = "tool_call_id"
        case toolID = "tool_id"
        case name
        case chunk
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        toolCallID = try c.decodeIfPresent(String.self, forKey: .toolCallID)
            ?? c.decodeIfPresent(String.self, forKey: .toolID)
        name = try c.decodeIfPresent(String.self, forKey: .name)
        chunk = try c.decodeIfPresent(String.self, forKey: .chunk)
    }
}


/// `review.summary` — self-improvement / memory review summary fired by the
/// gateway's background_review callback ("💾 Self-improvement review: …").
/// The desktop renders this as a faint persistent system line in the
/// transcript; Kallisti surfaces it the same way (Build 104).
struct NativeReviewSummaryPayload: Decodable {
    let text: String
}

/// `message.start` — signals a turn has begun.
struct NativeMessageStartPayload: Decodable {
    // Empty payload confirmed from capture
}

/// `session.info` — session metadata update.
struct NativeSessionInfoPayload: Decodable {
    let model: String?
    let provider: String?
    let title: String?
    let running: Bool?
}

// MARK: - Session Create/Response Types

/// Response from `session.create`
struct NativeSessionCreateResult: Decodable {
    let sessionId: String
    let storedSessionId: String?
    let messageCount: Int?
    let title: String?

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case storedSessionId = "stored_session_id"
        case messageCount = "message_count"
        case title
    }
}

/// Response from `session.list`
/// The gateway sends `id`/`preview`/`started_at`; older wire revisions sent
/// `session_id`/`preview_text`/`last_activity`. Decode tolerantly so a single
/// missing alias cannot throw the whole list (which surfaced as
/// "The data couldn't be read because it is missing" in the resume picker).
struct NativeSessionListItem: Decodable {
    let sessionId: String
    let title: String?
    let lastActivity: String?
    let previewText: String?
    let isPinned: Bool?
    let isArchived: Bool?
    let messageCount: Int?
    let source: String?

    enum CodingKeys: String, CodingKey {
        case sessionId
        case sessionIdAlias = "session_id"
        case sessionIdGateway = "id"
        case title
        case lastActivity = "last_activity"
        case lastActivityAlias = "started_at"
        case previewText = "preview_text"
        case previewTextAlias = "preview"
        case isPinned = "is_pinned"
        case isArchived = "is_archived"
        case messageCount = "message_count"
        case source
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        if let v = try c.decodeIfPresent(String.self, forKey: .sessionIdGateway) {
            sessionId = v
        } else if let v = try c.decodeIfPresent(String.self, forKey: .sessionId) {
            sessionId = v
        } else if let v = try c.decodeIfPresent(String.self, forKey: .sessionIdAlias) {
            sessionId = v
        } else {
            throw DecodingError.dataCorruptedError(
                forKey: .sessionId,
                in: c,
                debugDescription: "session.list item missing id/session_id"
            )
        }
        title = try c.decodeIfPresent(String.self, forKey: .title)
        // The gateway sends `started_at` as a NUMBER (unix seconds), not an
        // ISO8601 string. decodeIfPresent(String.self) THROWS a typeMismatch
        // when the key is present but numeric, which surfaced as "The data
        // couldn't be read because it isn't in the correct format" on every
        // app open. Normalize: String stays, Number converts to ISO8601.
        if let v = try c.decodeIfPresent(String.self, forKey: .lastActivity) {
            lastActivity = v
        } else if c.contains(.lastActivityAlias) {
            if let number = try? c.decode(Double.self, forKey: .lastActivityAlias) {
                lastActivity = ISO8601DateFormatter().string(from: Date(timeIntervalSince1970: number))
            } else if let s = try? c.decode(String.self, forKey: .lastActivityAlias) {
                lastActivity = s
            } else {
                lastActivity = nil
            }
        } else {
            lastActivity = nil
        }
        if let v = try c.decodeIfPresent(String.self, forKey: .previewText) {
            previewText = v
        } else if let v = try c.decodeIfPresent(String.self, forKey: .previewTextAlias) {
            previewText = v
        } else {
            previewText = nil
        }
        isPinned = try c.decodeIfPresent(Bool.self, forKey: .isPinned)
        isArchived = try c.decodeIfPresent(Bool.self, forKey: .isArchived)
        messageCount = try c.decodeIfPresent(Int.self, forKey: .messageCount)
        source = try c.decodeIfPresent(String.self, forKey: .source)
    }
}

struct NativeSessionListResult: Decodable {
    let sessions: [NativeSessionListItem]
    let total: Int?
}

/// Response from `session.history`
struct NativeHistoryMessage: Decodable {
    let role: String
    let content: String
    let timestamp: String?
    let rowId: Int?

    enum CodingKeys: String, CodingKey {
        case role
        case text
        case content
        case timestamp
        case rowId = "row_id"
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        role = try container.decodeIfPresent(String.self, forKey: .role) ?? ""
        content = try container.decodeIfPresent(String.self, forKey: .text)
            ?? container.decodeIfPresent(String.self, forKey: .content)
            ?? ""
        timestamp = try container.decodeIfPresent(String.self, forKey: .timestamp)
        // Hermes emits row_id as the raw state.db integer row id; it survives
        // the NativeJSONValue round-trip as a JSON number. Decode leniently
        // (Int, then Double, then String) so either serialization works.
        if let n = try? container.decodeIfPresent(Int.self, forKey: .rowId) {
            rowId = n
        } else if let d = try? container.decodeIfPresent(Double.self, forKey: .rowId) {
            rowId = Int(d)
        } else if let s = try? container.decodeIfPresent(String.self, forKey: .rowId) {
            rowId = Int(s)
        } else {
            rowId = nil
        }
    }
}

struct NativeSessionHistoryResult: Decodable {
    let messages: [NativeHistoryMessage]
    let title: String?
}

// MARK: - Untyped JSON Value

/// Minimal untyped JSON value for `NativeGatewayResponse.result` and event payloads.
enum NativeJSONValue: Codable {
    case object([String: NativeJSONValue])
    case array([NativeJSONValue])
    case string(String)
    case number(Double)
    case bool(Bool)
    case null

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let v = try? container.decode([String: NativeJSONValue].self) { self = .object(v); return }
        if let v = try? container.decode([NativeJSONValue].self) { self = .array(v); return }
        if let v = try? container.decode(String.self) { self = .string(v); return }
        if let v = try? container.decode(Double.self) { self = .number(v); return }
        if let v = try? container.decode(Bool.self) { self = .bool(v); return }
        self = .null
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .object(let v): try container.encode(v)
        case .array(let v): try container.encode(v)
        case .string(let v): try container.encode(v)
        case .number(let v): try container.encode(v)
        case .bool(let v): try container.encode(v)
        case .null: try container.encodeNil()
        }
    }
}

// MARK: - Streaming Event Types

/// High-frequency streaming event types confirmed from live capture.
enum NativeGatewayStreamingEventType: String {
    case messageStart = "message.start"
    case messageDelta = "message.delta"
    case messageComplete = "message.complete"
    case thinkingDelta = "thinking.delta"
    case sessionInfo = "session.info"
    case gatewayReady = "gateway.ready"
    case toolStart = "tool.start"
    case toolComplete = "tool.complete"
    case toolOutput = "tool.output"
}
