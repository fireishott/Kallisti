import Foundation

// MARK: - Eris note-run wire types
//
// The enrichment path on Eris: a note's content is staged as a recognition, a run
// is opened against the daemon-side note, and the run's events are polled. The
// daemon writes `started`, then `delta` events as the model produces text, then
// one terminal event (`completed`, `failed` or `cancelled`). Every shape here is
// snake_case-free but the JSON keys are camelCase, which is what the daemon
// serves for this router.

/// `POST /v1/notes/{id}/recognitions` request body.
struct ErisNoteRecognitionPayload: Encodable, Sendable {
    let drawingRevision: Int64
    let engine: String
    let engineVersion: String?
    let rawText: String
    let userCorrectedText: String?

    init(
        drawingRevision: Int64,
        engine: String,
        engineVersion: String? = nil,
        rawText: String,
        userCorrectedText: String? = nil
    ) {
        self.drawingRevision = drawingRevision
        self.engine = engine
        self.engineVersion = engineVersion
        self.rawText = rawText
        self.userCorrectedText = userCorrectedText
    }
}

/// `POST /v1/notes/{id}/recognitions` reply.
struct ErisNoteRecognitionEnvelope: Decodable, Sendable {
    struct Recognition: Decodable, Sendable {
        let id: String
        let noteId: String
        let drawingRevision: Int64
        let engine: String
        let rawText: String
    }

    let data: Recognition
}

/// `POST /v1/notes/{id}/runs` request body.
struct ErisNoteRunCreatePayload: Encodable, Sendable {
    let clientRunId: String
    let sourceDrawingRevision: Int64?
    let sourceTextRevision: Int64?
    let directives: [String]

    init(
        clientRunId: String,
        sourceDrawingRevision: Int64? = nil,
        sourceTextRevision: Int64? = nil,
        directives: [String] = []
    ) {
        self.clientRunId = clientRunId
        self.sourceDrawingRevision = sourceDrawingRevision
        self.sourceTextRevision = sourceTextRevision
        self.directives = directives
    }
}

/// One run, as `data` returns it.
struct ErisNoteRun: Decodable, Sendable {
    struct Result: Decodable, Sendable {
        let revisionId: String?
        let title: String?
        let markdown: String?
    }

    let id: String
    let noteId: String
    let clientRunId: String
    let status: String
    let attempt: Int
    let errorText: String?
    let result: Result?
    let createdAt: String
    let completedAt: String?

    /// Whether the run will not change again.
    var isTerminal: Bool {
        status == "completed" || status == "failed" || status == "cancelled"
    }
}

/// `POST /v1/notes/{id}/runs`, `GET /v1/note-runs/{id}` and the cancel reply.
struct ErisNoteRunEnvelope: Decodable, Sendable {
    let data: ErisNoteRun
}

/// One run event.
struct ErisNoteRunEvent: Decodable, Sendable {
    struct DeltaPayload: Decodable, Sendable {
        let text: String
        let seq: Int64?
    }

    let id: String
    let runId: String
    let seq: Int64
    let type: String
    let payload: Payload

    /// The event's payload, in the shape that event's type carries.
    enum Payload: Decodable, Sendable {
        case delta(DeltaPayload)
        case started(attempt: Int64?)
        case completed(revisionId: String?, title: String?)
        case failed(code: String?, message: String?)
        case cancelled
        case other

        init(from decoder: Decoder) throws {
            let container = try decoder.singleValueContainer()
            let raw = try container.decode(JSONValue.self)
            switch raw {
            case .object(let object):
                if let text = object["text"]?.stringValue {
                    self = .delta(DeltaPayload(text: text, seq: object["seq"]?.intValue))
                } else if let message = object["message"]?.stringValue {
                    self = .failed(code: object["code"]?.stringValue, message: message)
                } else if object["revisionId"] != nil {
                    self = .completed(
                        revisionId: object["revisionId"]?.stringValue,
                        title: object["title"]?.stringValue
                    )
                } else if let attempt = object["attempt"]?.intValue {
                    self = .started(attempt: attempt)
                } else {
                    self = .other
                }
            default:
                self = .other
            }
        }
    }
}

/// `GET /v1/note-runs/{id}/events` reply.
struct ErisNoteRunEventsEnvelope: Decodable, Sendable {
    struct Meta: Decodable, Sendable {
        let count: Int
        let runId: String
    }

    let data: [ErisNoteRunEvent]
    let meta: Meta
}

/// A minimal JSON value, so an event payload of any shape can be read without a
/// second decoder per event type.
enum JSONValue: Decodable, Sendable {
    case string(String)
    case number(Double)
    case bool(Bool)
    case object([String: JSONValue])
    case array([JSONValue])
    case null

    init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let value = try? container.decode(String.self) {
            self = .string(value)
        } else if let value = try? container.decode(Bool.self) {
            self = .bool(value)
        } else if let value = try? container.decode(Double.self) {
            self = .number(value)
        } else if let value = try? container.decode([String: JSONValue].self) {
            self = .object(value)
        } else if let value = try? container.decode([JSONValue].self) {
            self = .array(value)
        } else {
            self = .null
        }
    }

    var stringValue: String? {
        switch self {
        case .string(let value): return value
        case .number(let value): return String(value)
        default: return nil
        }
    }

    var intValue: Int64? {
        switch self {
        case .number(let value): return Int64(value)
        case .string(let value): return Int64(value)
        default: return nil
        }
    }
}

/// An empty JSON request body, for the routes that take no fields but still bind
/// `POST`.
struct EmptyBody: Encodable, Sendable {}
