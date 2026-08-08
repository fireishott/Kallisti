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

struct NativeGatewayError: Decodable, Error, Equatable {
    let code: Int
    let message: String
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
        if let v = try c.decodeIfPresent(String.self, forKey: .sessionId) {
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
        if let v = try c.decodeIfPresent(String.self, forKey: .lastActivity) {
            lastActivity = v
        } else if let v = try c.decodeIfPresent(String.self, forKey: .lastActivityAlias) {
            lastActivity = v
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
}
