import Foundation

// MARK: - Eris wire types
//
// Shapes here mirror `crates/eris-host/src/device/{handlers,turns}.rs`.
// Field names are the snake_case names the daemon emits; the Swift side keeps
// camelCase properties and maps them in `CodingKeys`, so a rename on one side
// is a compile-or-test failure rather than a silent nil.

// MARK: - Token pair

/// One access/refresh pair from `POST /v1/pair`.
///
/// `refresh()` answers with an access token only (the daemon deliberately does
/// not rotate the refresh token), so `refreshToken` is optional and a refreshed
/// pair is merged with what was already stored.
struct ErisTokenPair: Equatable, Sendable {
    /// Bearer access token. The only credential the turn routes accept.
    let accessToken: String
    /// Refresh token, when the response carried one.
    let refreshToken: String?
    /// Device id the daemon bound this pair to.
    let deviceID: String?
    /// Scopes the pair carries, as the daemon spelled them.
    let scopes: [String]
    /// Access token expiry, parsed from the daemon's RFC 3339 `expires_at`.
    let expiresAt: Date?

    init(
        accessToken: String,
        refreshToken: String? = nil,
        deviceID: String? = nil,
        scopes: [String] = [],
        expiresAt: Date? = nil
    ) {
        self.accessToken = accessToken
        self.refreshToken = refreshToken
        self.deviceID = deviceID
        self.scopes = scopes
        self.expiresAt = expiresAt
    }

    /// Fold a `POST /v1/token/refresh` response into the pair it refreshes.
    ///
    /// The refresh response carries no refresh token or device id, so those are
    /// carried forward from the stored pair rather than blanked out.
    func mergingRefreshResponse(_ response: ErisTokenPair) -> ErisTokenPair {
        ErisTokenPair(
            accessToken: response.accessToken,
            refreshToken: response.refreshToken ?? refreshToken,
            deviceID: response.deviceID ?? deviceID,
            scopes: response.scopes.isEmpty ? scopes : response.scopes,
            expiresAt: response.expiresAt ?? expiresAt
        )
    }
}

extension ErisTokenPair: Decodable {
    private enum CodingKeys: String, CodingKey {
        case accessToken = "access_token"
        case refreshToken = "refresh_token"
        case deviceID = "device_id"
        case scopes
        case expiresAt = "expires_at"
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        guard
            let accessToken = try container.decodeIfPresent(String.self, forKey: .accessToken),
            !accessToken.isEmpty
        else {
            throw ErisError.decoding("the response carried no access_token")
        }
        self.init(
            accessToken: accessToken,
            refreshToken: try container.decodeIfPresent(String.self, forKey: .refreshToken),
            deviceID: try container.decodeIfPresent(String.self, forKey: .deviceID),
            scopes: try container.decodeIfPresent([String].self, forKey: .scopes) ?? [],
            expiresAt: ErisTimestamp.date(from: try container.decodeIfPresent(String.self, forKey: .expiresAt))
        )
    }
}

// MARK: - Turn

/// The `202` body from `POST /v1/turns`.
struct ErisTurn: Equatable, Sendable, Decodable {
    /// Job id. This is the `{id}` every follow-up route takes.
    let jobID: String
    /// Session the job was queued into.
    let sessionID: String
    /// Job state at the moment the turn was accepted (`queued`).
    let state: String
    /// The conversation this turn was filed under, when the request named one.
    ///
    /// The handler echoes the same value it validated, so a client that sent a
    /// conversation id can confirm the turn landed in the transcript it meant.
    let conversationID: String?

    private enum CodingKeys: String, CodingKey {
        case jobID = "job_id"
        case sessionID = "session_id"
        case state
        case conversationID = "conversation_id"
    }

    init(jobID: String, sessionID: String, state: String, conversationID: String? = nil) {
        self.jobID = jobID
        self.sessionID = sessionID
        self.state = state
        self.conversationID = conversationID
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        guard
            let jobID = try container.decodeIfPresent(String.self, forKey: .jobID),
            let sessionID = try container.decodeIfPresent(String.self, forKey: .sessionID)
        else {
            throw ErisError.decoding("the turn response carried no job_id or session_id")
        }
        self.init(
            jobID: jobID,
            sessionID: sessionID,
            state: try container.decodeIfPresent(String.self, forKey: .state) ?? "queued",
            conversationID: try container.decodeIfPresent(String.self, forKey: .conversationID)
        )
    }
}

// MARK: - Session snapshot

/// The `GET /v1/sessions/{id}` snapshot a `buffer_miss` points at.
struct ErisSession: Equatable, Sendable, Decodable {
    /// One turn inside the session.
    struct Turn: Equatable, Sendable, Decodable {
        /// Job id of this turn.
        let jobID: String
        /// Job state (`queued`, `running`, `succeeded`, `failed`, `cancelled`).
        let state: String
        /// Assembled reply text, or the partial text of a turn that did not finish.
        let text: String
        /// RFC 3339 creation stamp, kept as the daemon's own string.
        let createdAt: String?
        /// RFC 3339 finish stamp, absent while the turn is unfinished.
        let finishedAt: String?

        private enum CodingKeys: String, CodingKey {
            case jobID = "job_id"
            case state
            case text
            case createdAt = "created_at"
            case finishedAt = "finished_at"
        }

        init(jobID: String, state: String, text: String, createdAt: String?, finishedAt: String?) {
            self.jobID = jobID
            self.state = state
            self.text = text
            self.createdAt = createdAt
            self.finishedAt = finishedAt
        }

        init(from decoder: any Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            self.init(
                jobID: try container.decodeIfPresent(String.self, forKey: .jobID) ?? "",
                state: try container.decodeIfPresent(String.self, forKey: .state) ?? "",
                text: try container.decodeIfPresent(String.self, forKey: .text) ?? "",
                createdAt: try container.decodeIfPresent(String.self, forKey: .createdAt),
                finishedAt: try container.decodeIfPresent(String.self, forKey: .finishedAt)
            )
        }
    }

    /// Session id.
    let sessionID: String
    /// `active`, `expired` or `revoked`.
    let state: String
    /// Every turn the session holds.
    let turns: [Turn]

    private enum CodingKeys: String, CodingKey {
        case sessionID = "session_id"
        case state
        case turns
    }

    init(sessionID: String, state: String, turns: [Turn]) {
        self.sessionID = sessionID
        self.state = state
        self.turns = turns
    }

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        self.init(
            sessionID: try container.decodeIfPresent(String.self, forKey: .sessionID) ?? "",
            state: try container.decodeIfPresent(String.self, forKey: .state) ?? "",
            turns: try container.decodeIfPresent([Turn].self, forKey: .turns) ?? []
        )
    }
}

// MARK: - Usage

/// Token accounting reported on a `done` frame.
///
/// The daemon serialises each count as a tagged object
/// (`{"state":"reported","tokens":12}` or
/// `{"state":"provider_unavailable","reason":"fields_absent"}`). A count the
/// provider did not report stays `nil` and never becomes a zero, because zero
/// and "unknown" are different facts on this wire.
struct ErisUsage: Equatable, Sendable {
    /// Prompt tokens, when the provider reported them.
    let inputTokens: Int?
    /// Completion tokens, when the provider reported them.
    let outputTokens: Int?
    /// Prompt tokens the provider served from cache, when reported.
    let cacheReadTokens: Int?

    init(inputTokens: Int?, outputTokens: Int?, cacheReadTokens: Int?) {
        self.inputTokens = inputTokens
        self.outputTokens = outputTokens
        self.cacheReadTokens = cacheReadTokens
    }

    /// Build usage from the raw `usage` value of a `done` payload.
    ///
    /// `nil` when the frame carried no usable usage object at all.
    init?(json: Any?) {
        guard let object = json as? [String: Any] else { return nil }
        self.init(
            inputTokens: Self.count(in: object["input"]),
            outputTokens: Self.count(in: object["output"]),
            cacheReadTokens: Self.count(in: object["cache_read"])
        )
    }

    /// One count: a tagged object, or a bare number for a flatter provider.
    private static func count(in value: Any?) -> Int? {
        guard let value else { return nil }
        if let number = value as? Int { return number }
        guard let object = value as? [String: Any] else { return nil }
        guard (object["state"] as? String) == "reported" else { return nil }
        return object["tokens"] as? Int
    }
}

// MARK: - Frames

/// One server-sent event from `GET /v1/turns/{id}/events`.
///
/// The daemon's `FrameKind` has exactly these four names, and its SSE frames
/// carry them in the `event:` field with a JSON `data:` payload.
enum ErisFrame: Equatable, Sendable {
    /// One assistant text fragment: `{"text":"..."}`.
    case delta(text: String)
    /// The turn finished: `{"text":"<assembled>","usage":{...}}`.
    case done(text: String, usage: ErisUsage?)
    /// The turn failed: `{"code":"...","message":"..."}`.
    case error(ErisError)
    /// The turn was cancelled: `{"reason":"cancel"}`.
    case cancelled(reason: String?)

    /// Whether this frame ends the stream. Matches `FrameKind::is_terminal`.
    var isTerminal: Bool {
        switch self {
        case .delta: return false
        case .done, .error, .cancelled: return true
        }
    }

    /// The text this frame contributes, when it carries text.
    var text: String? {
        switch self {
        case .delta(let text): return text
        case .done(let text, _): return text
        case .error, .cancelled: return nil
        }
    }

    /// Decode one dispatched SSE event.
    ///
    /// Returns `nil` for an event name this client does not know, so a future
    /// frame kind from a newer daemon is skipped rather than failing a stream
    /// that is otherwise fine.
    static func decode(event: String?, data: String) throws -> ErisFrame? {
        guard let event else { return nil }
        switch event {
        case "delta":
            let object = try ErisJSON.object(data)
            return .delta(text: object["text"] as? String ?? "")
        case "done":
            let object = try ErisJSON.object(data)
            return .done(
                text: object["text"] as? String ?? "",
                usage: ErisUsage(json: object["usage"])
            )
        case "error":
            let object = (try? ErisJSON.object(data)) ?? [:]
            return .error(
                .turn(
                    code: object["code"] as? String,
                    message: object["message"] as? String
                )
            )
        case "cancelled":
            let object = (try? ErisJSON.object(data)) ?? [:]
            return .cancelled(reason: object["reason"] as? String)
        default:
            return nil
        }
    }
}

// MARK: - Errors

/// A typed failure from the device surface.
///
/// The daemon's error body is always `{"ok":false,"code":"...","message":"..."}`
/// (`DeviceErrorBody`), so `code` is the thing to branch on and `message` is for
/// a human reading a log line. A terminal `error` frame carries the same two
/// fields without an HTTP status.
enum ErisError: Error, Equatable, Sendable, LocalizedError {
    /// Non-2xx HTTP answer from the daemon.
    case server(status: Int, code: String?, message: String?)
    /// An `error` frame inside a stream that had already started.
    case turn(code: String?, message: String?)
    /// The request never produced an HTTP answer.
    case transport(String)
    /// A body did not decode into the shape the contract promises.
    case decoding(String)
    /// The base URL could not be joined with a route path.
    case invalidBaseURL(String)
    /// The route needs a bearer and the token store has none.
    case missingToken
    /// The SSE stream was malformed (a data payload that is not JSON).
    case stream(String)
    /// The keychain refused the write, or could not be read.
    case storage(String)

    /// The server's stable machine-readable code, when there was one.
    var code: String? {
        switch self {
        case .server(_, let code, _): return code
        case .turn(let code, _): return code
        default: return nil
        }
    }

    /// The server's human-readable detail, when there was one.
    var message: String? {
        switch self {
        case .server(_, _, let message): return message
        case .turn(_, let message): return message
        case .transport(let message): return message
        case .decoding(let message): return message
        case .stream(let message): return message
        case .storage(let message): return message
        case .invalidBaseURL(let value): return "base URL \(value) cannot take a /v1 route"
        case .missingToken: return "no Eris access token is stored"
        }
    }

    /// The HTTP status, when the failure came from a response.
    var status: Int? {
        switch self {
        case .server(let status, _, _): return status
        default: return nil
        }
    }

    /// Whether this failure is the daemon saying "refresh and try again".
    var isUnauthorized: Bool {
        status == 401
    }

    /// Whether the token is real but not allowed to do this.
    ///
    /// `403` is the daemon's `scope_denied`: the pair is valid, so retrying with
    /// the same token can only fail the same way. A screen that gets this must
    /// explain the scope rather than offer a retry that cannot work.
    var isForbidden: Bool {
        status == 403
    }

    /// Whether the missing scope is `admin`, which is the settings and approvals
    /// case and the only one with a fix the operator can act on.
    var isAdminScopeDenied: Bool {
        status == 403 && code == "scope_denied"
    }

    /// Whether the request never reached an HTTP answer.
    ///
    /// Distinct from every server answer on purpose: the daemon being unreachable
    /// says nothing about the credential, so the pair is kept and retrying is a
    /// reasonable thing to offer.
    var isTransport: Bool {
        if case .transport = self { return true }
        return false
    }

    var errorDescription: String? {
        switch self {
        case .server(let status, let code, let message):
            return "Eris answered \(status)\(code.map { " \($0)" } ?? ""): \(message ?? "no message")"
        case .turn(let code, let message):
            return "Eris turn failed\(code.map { " \($0)" } ?? ""): \(message ?? "no message")"
        case .transport(let message):
            return "Eris transport failed: \(message)"
        case .decoding(let message):
            return "Eris response did not decode: \(message)"
        case .invalidBaseURL(let value):
            return "Eris base URL \(value) cannot take a /v1 route"
        case .missingToken:
            return "no Eris access token is stored: pair the device first"
        case .stream(let message):
            return "Eris stream was malformed: \(message)"
        case .storage(let message):
            return "Eris token storage failed: \(message)"
        }
    }
}

// MARK: - Timestamps

/// RFC 3339 helpers for the daemon's `expires_at`/`created_at` stamps.
///
/// The daemon formats with the `time` crate's `Rfc3339`, which emits a
/// fractional part only when the nanosecond is non-zero, so both spellings have
/// to parse. Formatters are built per call: they are not `Sendable` and a shared
/// static instance would be a concurrency hole for no gain at this volume.
enum ErisTimestamp {
    /// Parse an RFC 3339 stamp, with or without fractional seconds.
    static func date(from text: String?) -> Date? {
        guard let text, !text.isEmpty else { return nil }
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = formatter.date(from: text) { return date }
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.date(from: text)
    }

    /// Format a stamp the way the daemon does, without fractional seconds.
    static func string(from date: Date) -> String {
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.string(from: date)
    }
}

// MARK: - JSON

/// The one place a raw JSON object is read, so frames stay lenient about extra
/// keys the daemon may add.
enum ErisJSON {
    /// Decode one JSON object payload.
    static func object(_ text: String) throws -> [String: Any] {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { throw ErisError.stream("an event carried no data") }
        guard let data = trimmed.data(using: .utf8) else {
            throw ErisError.stream("an event payload is not UTF-8")
        }
        let value: Any
        do {
            value = try JSONSerialization.jsonObject(with: data)
        } catch {
            throw ErisError.stream("an event payload is not JSON: \(trimmed.prefix(120))")
        }
        guard let object = value as? [String: Any] else {
            throw ErisError.stream("an event payload is not a JSON object: \(trimmed.prefix(120))")
        }
        return object
    }
}

// MARK: - SSE decoder

/// Turns a byte stream into `ErisFrameOutcome`s.
///
/// Feed it whatever chunk sizes arrive: it holds the partial line internally, so
/// a frame split across two chunks is decoded once, when its terminating blank
/// line lands. Blank lines are the frame delimiter, which is why the client must
/// not use Foundation's line sequence here (it drops empty lines).
struct ErisSSEDecoder {
    private var buffer = Data()
    private var event: String?
    private var dataLines: [String] = []

    init() {}

    /// Feed one chunk of bytes and take whatever outcomes are now complete.
    mutating func consume(_ chunk: Data) throws -> [ErisFrameOutcome] {
        buffer.append(chunk)
        var frames: [ErisFrameOutcome] = []
        while let newlineIndex = buffer.firstIndex(of: 0x0A) {
            var lineBytes = buffer[buffer.startIndex..<newlineIndex]
            if lineBytes.last == 0x0D {
                lineBytes = lineBytes.dropLast()
            }
            let line = String(decoding: lineBytes, as: UTF8.self)
            buffer.removeSubrange(buffer.startIndex...newlineIndex)
            if let frame = try consume(line: line) {
                frames.append(frame)
            }
        }
        return frames
    }

    /// Flush a trailing frame that arrived without its blank-line terminator.
    ///
    /// A well-behaved stream always terminates a frame, but a daemon that is
    /// killed mid-frame should not cost the client a frame it fully received.
    mutating func finish() throws -> [ErisFrameOutcome] {
        var frames: [ErisFrameOutcome] = []
        if !buffer.isEmpty {
            let line = String(decoding: buffer, as: UTF8.self)
            buffer.removeAll(keepingCapacity: true)
            if let frame = try consume(line: line) {
                frames.append(frame)
            }
        }
        if let frame = try dispatch() {
            frames.append(frame)
        }
        return frames
    }

    /// One line, per the SSE grammar: `field: value`, `: comment`, or blank.
    private mutating func consume(line: String) throws -> ErisFrameOutcome? {
        if line.isEmpty {
            return try dispatch()
        }
        if line.hasPrefix(":") {
            // Comment / keep-alive (`: keep-alive`). Nothing to dispatch.
            return nil
        }
        let (field, value) = Self.split(line)
        switch field {
        case "event":
            event = value
        case "data":
            dataLines.append(value)
        default:
            // `id:` (sequence), `retry:` and anything new are not needed. The
            // frame kinds are carried in `event:`, not here, and all six of
            // them are now rendered.
            break
        }
        return nil
    }

    /// Emit the outcome the accumulated fields describe, then reset.
    ///
    /// Routed through `ErisFrameBridge` rather than `ErisFrame.decode` so the
    /// `reasoning` and `tool` frames the daemon sends are carried. They used to
    /// hit `ErisFrame.decode`'s `default` and return `nil`, which is why a
    /// thought card only ever appeared after the answer, from the transcript
    /// refetch, and why the run log was empty during a turn.
    private mutating func dispatch() throws -> ErisFrameOutcome? {
        defer {
            event = nil
            dataLines.removeAll(keepingCapacity: true)
        }
        guard !dataLines.isEmpty else { return nil }
        return try ErisFrameBridge.decode(event: event, data: dataLines.joined(separator: "\n"))
    }

    /// Split `field: value`, dropping exactly one space after the colon.
    private static func split(_ line: String) -> (field: String, value: String) {
        guard let colon = line.firstIndex(of: ":") else {
            return (line.trimmingCharacters(in: .whitespaces), "")
        }
        let field = String(line[line.startIndex..<colon]).trimmingCharacters(in: .whitespaces)
        var value = String(line[line.index(after: colon)...])
        if value.hasPrefix(" ") {
            value.removeFirst()
        }
        return (field, value)
    }
}
