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
    /// One fragment of the model's own reasoning: `{"text":"..."}`.
    ///
    /// A separate frame from `delta` on purpose: the daemon streams the thinking
    /// it got from the provider, and a surface that folds it into the answer
    /// renders the model's doubts as the reply. Dropping it (the previous
    /// behaviour) is why the Thought process row sat empty for the whole turn.
    case reasoning(text: String)
    /// The turn finished: `{"text":"<assembled>","usage":{...},"accounting":{...}}`.
    ///
    /// `accounting` is the receipt the footer draws: model, rounds, wall time,
    /// the token split, the context reading, and a dollar cost when the operator
    /// has declared a price. It is separate from `usage` because `usage` carries
    /// the provider's own tags and says nothing about the turn. A daemon that
    /// does not send it produces `nil`, which the footer handles by saying
    /// nothing rather than drawing zeros.
    case done(text: String, usage: ErisUsage?, accounting: TurnAccounting?)
    /// The turn failed: `{"code":"...","message":"..."}`.
    case error(ErisError)
    /// The turn was cancelled: `{"reason":"cancel"}`.
    case cancelled(reason: String?)
    /// A tool call is about to run: `{"name":"run_command","arguments":"..."}`.
    case toolStart(name: String)
    /// A tool call finished: `{"name":..,"output":..,"is_error":..}`.
    case toolFinished(name: String, output: String, isError: Bool)
    /// Output as a command runs: `{"name":..,"chunk":..}`. Live only; the
    /// finished frame's preview is what a reconnect sees.
    case toolOutput(name: String, chunk: String)
    /// One step of the turn: `{"turn_id":..,"step_id":..,"step_type":..,"label":..}`.
    ///
    /// WP-03 writes these on every tool call and thinking step, and this client
    /// used to skip them as an unknown event, which is why the Thought process
    /// row stayed empty on a turn whose model emitted no reasoning.
    case step(label: String, stepType: String?)
    /// Files a step changed, summarised: `path +added -removed`, comma separated.
    case artifactDiff(ErisDiff)
    /// A page a step wrote, with the height cap the daemon set.
    case artifactPage(html: String, heightCap: Int?)
    /// A file the turn attached to its reply:
    /// `{"id":"...","name":"...","size":123,"media_type":"..."}`.
    ///
    /// The daemon journals this frame and replays it on reconnect. The
    /// transcript rows carry the same four fields, but the bubble a phone is
    /// watching arrived over the stream and had no rows to read.
    case attachment(ErisAttachmentRef)
    /// The turn is waiting on the operator: turn.ask.
    case ask(question: String, choices: [String]?, requestID: String, multi: Bool)
    /// A command is parked until a person allows or denies it: turn.approval.
    case approval(ErisApproval)
    /// The turn opened: `{"model":"...","route":{...}}`.
    ///
    /// The first frame of a turn, and the only one that says which way the
    /// daemon routed it. Both payload fields are optional on purpose: a daemon
    /// that does not route sends the frame with no `route`, and one that routes
    /// may still leave the model unnamed.
    case turnStarted(model: String?, route: TurnRoute?)

    /// A round is starting, before the model has said anything:
    /// `{"round":1,"max_rounds":100,"tools_allowed":true}`.
    ///
    /// The turn loop writes one of these before every tool round. This client
    /// skipped the frame as unknown, so a long turn showed a spinner and no
    /// progress at all. `count` is one-based and `total` is the turn's tool
    /// budget; `toolsAllowed` false means the loop has taken the tools away and
    /// the reply is the next thing to arrive.
    case round(count: Int, total: Int, toolsAllowed: Bool)

    /// Whether this frame ends the stream. Matches `FrameKind::is_terminal`.
    var isTerminal: Bool {
        switch self {
        case .delta, .reasoning, .toolStart, .toolFinished, .toolOutput, .step,
             .artifactDiff, .artifactPage, .attachment, .ask, .approval,
             .turnStarted, .round: return false
        case .done, .error, .cancelled: return true
        }
    }

    /// The text this frame contributes, when it carries text.
    var text: String? {
        switch self {
        case .delta(let text): return text
        case .done(let text, _, _): return text
        case .reasoning, .toolStart, .toolFinished, .toolOutput, .step,
             .artifactDiff, .artifactPage, .attachment, .ask, .approval,
             .turnStarted, .round, .error, .cancelled: return nil
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
        case "tool_start":
            let object = (try? ErisJSON.object(data)) ?? [:]
            return .toolStart(name: object["name"] as? String ?? "")
        case "tool":
            let object = (try? ErisJSON.object(data)) ?? [:]
            return .toolFinished(
                name: object["name"] as? String ?? "",
                output: object["output"] as? String ?? "",
                isError: object["is_error"] as? Bool ?? false
            )
        case "tool_output":
            let object = (try? ErisJSON.object(data)) ?? [:]
            return .toolOutput(
                name: object["name"] as? String ?? "",
                chunk: object["chunk"] as? String ?? ""
            )
        case "delta":
            let object = try ErisJSON.object(data)
            return .delta(text: object["text"] as? String ?? "")
        case "reasoning":
            let object = try ErisJSON.object(data)
            return .reasoning(text: object["text"] as? String ?? "")
        case "done":
            let object = try ErisJSON.object(data)
            // The receipt is decoded from its own sub-object. A daemon that does
            // not send one, or sends a shape this build does not know, costs the
            // footer and not the answer: the text above is what the operator is
            // waiting for, and it must land either way.
            let accounting = (object["accounting"] as? [String: Any])
                .flatMap { TurnAccounting(json: $0) }
            return .done(
                text: object["text"] as? String ?? "",
                usage: ErisUsage(json: object["usage"]),
                accounting: accounting
            )
        case "round":
            // Read separately from every other optional key: a daemon that
            // omits one of these must still produce a usable frame, so each
            // falls back rather than throwing the turn away.
            let object = (try? ErisJSON.object(data)) ?? [:]
            return .round(
                count: object["round"] as? Int ?? 0,
                total: object["max_rounds"] as? Int ?? 0,
                toolsAllowed: object["tools_allowed"] as? Bool ?? true
            )
        case "attachment":
            let object = (try? ErisJSON.object(data)) ?? [:]
            return .attachment(ErisAttachmentRef(
                id: object["id"] as? String ?? "",
                name: object["name"] as? String ?? "",
                size: object["size"] as? Int ?? 0,
                mediaType: object["media_type"] as? String
            ))
        case "error":
            let object = (try? ErisJSON.object(data)) ?? [:]
            return .error(
                .turn(
                    code: object["code"] as? String,
                    message: object["message"] as? String
                )
            )
        case "turn.step":
            let object = try ErisJSON.object(data)
            let label = (object["label"] as? String) ?? (object["step_type"] as? String) ?? "step"
            return .step(label: label, stepType: object["step_type"] as? String)
        case "turn.start":
            // The opening frame: the model about to answer, and - on a daemon
            // that routes turns - which way this one went. Read separately so a
            // route this build cannot parse never costs the frame its model.
            let object = (try? ErisJSON.object(data)) ?? [:]
            let model = (object["model"] as? String) ?? ""
            return .turnStarted(
                model: model.isEmpty ? nil : model,
                route: TurnRoute(json: object["route"])
            )
        case "turn.artifact.diff":
            let object = try ErisJSON.object(data)
            return .artifactDiff(ErisDiff(json: object))
        case "turn.artifact.page":
            let object = try ErisJSON.object(data)
            return .artifactPage(
                html: object["html"] as? String ?? "",
                heightCap: object["height_cap"] as? Int
            )
        case "turn.ask":
            let object = try ErisJSON.object(data)
            let choices = object["choices"] as? [String]
            return .ask(
                question: object["question"] as? String ?? "",
                choices: (choices?.isEmpty ?? true) ? nil : choices,
                requestID: object["ask_id"] as? String ?? "",
                multi: object["multi"] as? Bool ?? false
            )
        case "turn.approval":
            let object = try ErisJSON.object(data)
            return .approval(ErisApproval(
                id: object["approval_id"] as? String ?? "",
                tool: object["scope"] as? String ?? "tool",
                arguments: object["command"] as? String ?? "",
                reason: object["reason"] as? String,
                createdAt: object["created_at"] as? String ?? "",
                expiresAt: object["deadline"] as? String
            ))
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

/// Turns a byte stream into `ErisFrame`s.
///
/// Feed it whatever chunk sizes arrive: it holds the partial line internally, so
/// a frame split across two chunks is decoded once, when its terminating blank
/// line lands. Blank lines are the frame delimiter, which is why the client must
/// not use Foundation's line sequence here (it drops empty lines).
struct ErisSSEDecoder {
    private var buffer = Data()
    private var event: String?
    private var dataLines: [String] = []

    /// The `id:` of the last complete frame. The daemon's events route replays
    /// strictly after this value, which is the whole of a mid-turn reattach.
    private(set) var lastEventID: String?

    init() {}

    /// Feed one chunk of bytes and take whatever frames are now complete.
    mutating func consume(_ chunk: Data) throws -> [ErisFrame] {
        buffer.append(chunk)
        var frames: [ErisFrame] = []
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
    mutating func finish() throws -> [ErisFrame] {
        var frames: [ErisFrame] = []
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
    private mutating func consume(line: String) throws -> ErisFrame? {
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
        case "id":
            if !value.isEmpty { lastEventID = value }
        default:
            // `retry:` and anything new are not needed: the frame kinds carry
            // everything this client renders.
            break
        }
        return nil
    }

    /// Emit the frame the accumulated fields describe, then reset.
    private mutating func dispatch() throws -> ErisFrame? {
        defer {
            event = nil
            dataLines.removeAll(keepingCapacity: true)
        }
        guard !dataLines.isEmpty else { return nil }
        return try ErisFrame.decode(event: event, data: dataLines.joined(separator: "\n"))
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

// MARK: - Control plane (models and auxiliary roles)
//
// Shapes mirror `crates/eris-host/src/device/control.rs`. That file serves
// snake_case and so does the rest of `/v1`, so every key is named here rather
// than decoded through a strategy the app does not install.
//
// Two absences are deliberate on the daemon side and mirrored here: a provider
// row carries an id, a name and a primary flag and never an endpoint or a key
// reference, and a model row carries the probed catalogue's id rather than a
// credential.

/// One provider as `/v1/models` and `/v1/aux` report it.
struct ErisControlProvider: Decodable, Sendable, Identifiable {
    let id: String
    let name: String
    let primary: Bool
}

/// One model row. `provider` is the provider that published the id; it is nil on
/// a daemon with no provider at all.
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

/// One auxiliary role. `inherits` is the honest form of "follows the primary";
/// `pinned` is null in exactly that case, so a client never has to infer it from
/// a match that might be a coincidence.
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

/// `GET /v1/aux`. The ids a picker needs ride along, so a row can be drawn from
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

/// The one place the daemon's control-plane shapes become the shapes the 0.4.0
/// stores already draw.
///
/// The mapping lives here rather than in the stores so a store keeps its own
/// model and the wire stays snake_case on one side and store-shaped on the
/// other, with exactly one seam between them.
@MainActor
enum ErisControlMapping {
    /// The catalogue, the pin, and the providers that would answer a turn.
    ///
    /// A pinned id that is not in the catalogue still becomes the active model:
    /// the daemon holds that pin, so hiding it would leave the picker claiming
    /// nothing is selected on a daemon that has one selected.
    static func catalog(_ wire: ErisControlModels) -> (models: [ModelStore.HeraldModel], active: ModelStore.ActiveModel?) {
        let models = wire.models.map { row in
            ModelStore.HeraldModel(
                name: row.id,
                provider: row.provider ?? "",
                providerName: row.provider,
                contextWindow: row.contextWindow,
                isProviderDefault: row.isDefault
            )
        }
        let pinned = wire.model.trimmingCharacters(in: .whitespacesAndNewlines)
        // A pin is stored `provider/model` and a row's `id` is the bare id its
        // own provider spells. Match either spelling, or a pinned row is never
        // recognised: no tick is drawn and the active model is named after the
        // primary provider instead of the one that serves it.
        func isPinned(_ row: ErisControlModelRow) -> Bool {
            row.id == pinned || "\(row.provider ?? "")/\(row.id)" == pinned
        }
        let active: ModelStore.ActiveModel?
        if pinned.isEmpty {
            active = nil
        } else if let row = wire.models.first(where: isPinned) {
            active = ModelStore.ActiveModel(
                name: row.id,
                provider: row.provider,
                contextWindow: row.contextWindow
            )
        } else {
            active = ModelStore.ActiveModel(
                name: pinned,
                provider: wire.providers.first(where: \.primary)?.id,
                contextWindow: nil
            )
        }
        return (models, active)
    }

    /// The aux rows. `isAuto` is the daemon's `inherits`, which is the state a
    /// row draws as "follows the primary" rather than a match inferred here.
    static func auxTasks(_ wire: ErisControlAuxList) -> [AuxTask] {
        wire.roles.map { role in
            AuxTask(
                key: role.key,
                task: role.label,
                provider: role.provider,
                model: role.model,
                isAuto: role.inherits
            )
        }
    }
}


// MARK: - Turn artifacts

/// One changed file, as turn.artifact.diff states it.
struct ErisDiffFile: Sendable, Hashable {
    let path: String
    let added: Int
    let removed: Int
    let hunks: [ErisHunk]

    /// The one-line form a card header shows.
    var summary: String { path + " +" + String(added) + " -" + String(removed) }
}

/// One hunk: where it starts on each side, and the lines with their kind.
struct ErisHunk: Sendable, Hashable {
    let oldStart: Int
    let oldLines: Int
    let newStart: Int
    let newLines: Int
    let lines: [ErisDiffLine]
}

/// One line of a hunk, tagged the way the daemon tags it.
struct ErisDiffLine: Sendable, Hashable {
    enum Kind: String, Sendable { case context, add, remove }
    let kind: Kind
    let text: String
}

/// A whole turn.artifact.diff payload.
struct ErisDiff: Sendable, Hashable {
    let files: [ErisDiffFile]

    /// Read the JSON the frame carries. An unknown shape becomes an empty diff
    /// rather than a thrown error: a frame this client half understands must not
    /// fail the stream that carries the answer.
    init(json: [String: Any]) {
        let raw = json["files"] as? [[String: Any]] ?? []
        files = raw.map { file in
            let hunks = (file["hunks"] as? [[String: Any]] ?? []).map { hunk in
                let lines = (hunk["lines"] as? [[String: Any]] ?? []).map { line in
                    ErisDiffLine(
                        kind: ErisDiffLine.Kind(rawValue: line["kind"] as? String ?? "context") ?? .context,
                        text: line["text"] as? String ?? ""
                    )
                }
                return ErisHunk(
                    oldStart: hunk["old_start"] as? Int ?? 0,
                    oldLines: hunk["old_lines"] as? Int ?? 0,
                    newStart: hunk["new_start"] as? Int ?? 0,
                    newLines: hunk["new_lines"] as? Int ?? 0,
                    lines: lines
                )
            }
            return ErisDiffFile(
                path: file["path"] as? String ?? "file",
                added: file["added"] as? Int ?? 0,
                removed: file["removed"] as? Int ?? 0,
                hunks: hunks
            )
        }
    }

    /// This diff in the shape the chat already renders.
    ///
    /// The app draws one diff card per message (CodeDiff), so the daemon frame is
    /// rendered into that shape - one patch string per file, hunks with their
    /// headers - rather than growing a second diff view beside it.
    var codeDiff: CodeDiff {
        CodeDiff(
            files: files.map { file in
                FileDiff(
                    path: file.path,
                    status: file.added > 0 && file.removed == 0 ? "added" : "modified",
                    additions: file.added,
                    deletions: file.removed,
                    patch: file.hunks.map { hunk in
                        var out = "@@ -\(hunk.oldStart),\(hunk.oldLines) +\(hunk.newStart),\(hunk.newLines) @@"
                        for line in hunk.lines {
                            out += "\n" + (line.kind == .add ? "+" : line.kind == .remove ? "-" : " ") + line.text
                        }
                        return out
                    }.joined(separator: "\n")
                )
            },
            summary: files.map(\.summary).joined(separator: ", ")
        )
    }

    /// Added and removed totals across every file.
    var totals: (added: Int, removed: Int) {
        files.reduce(into: (0, 0)) { totals, file in
            totals.0 += file.added
            totals.1 += file.removed
        }
    }
}
