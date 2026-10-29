import Foundation

// MARK: - Routes

/// One route on the Kallisti-Eris device listener.
///
/// The paths are literals from `crates/eris-host/src/device/mod.rs`
/// (`ROUTE_PAIR`, `ROUTE_REFRESH`, `ROUTE_TURNS`, `ROUTE_EVENTS`,
/// `ROUTE_CANCEL`, `ROUTE_SESSION`, `ROUTE_PROJECTS`, `ROUTE_CONVERSATIONS`,
/// `ROUTE_CONVERSATION`, `ROUTE_CONVERSATION_CLEAR`, `ROUTE_SETTINGS`). Every one
/// of them lives under `/v1`; the listener answers a typed 404 for anything else,
/// so a path typo is a hard failure rather than a fallback surprise.
enum ErisRoute: Equatable, Sendable {
    /// `POST /v1/pair` - redeem a pairing code.
    case pair
    /// `POST /v1/token/refresh` - trade a refresh token for an access token.
    case tokenRefresh
    /// `POST /v1/turns` - queue one turn.
    case turns
    /// `GET /v1/turns/{id}/events` - the SSE stream for one job.
    case events(turnID: String)
    /// `POST /v1/turns/{id}/cancel` - stop a running turn.
    case cancel(turnID: String)
    /// `GET /v1/turns/{id}/asked` - the question a turn is parked on, if any.
    ///
    /// The read-back half of `asked`: a phone whose stream dropped, or that opened
    /// the job after the frame went out, asks this instead of waiting for a frame
    /// that has already been and gone.
    case asked(turnID: String)
    /// `POST /v1/turns/{id}/answer` - settle the question and resume the turn.
    case answer(turnID: String)
    /// `GET /v1/sessions/{id}` - the snapshot a `buffer_miss` points at.
    case session(sessionID: String)
    /// `GET /v1/conversations` - the newest conversations, newest first.
    case conversations
    /// `POST /v1/conversations` - start an empty conversation.
    ///
    /// A case of its own because the router binds two verbs to one path and
    /// [`method`] is total: one case, one verb.
    case createConversation
    /// `GET /v1/conversations/{id}` - one conversation and its transcript.
    case conversation(conversationID: String)
    /// `POST /v1/conversations/{id}/clear` - drop that conversation's messages.
    case clearConversation(conversationID: String)
    /// `GET /v1/settings` - the operator-facing settings snapshot.
    case settings
    /// `POST /v1/settings` - change a subset of those settings.
    case updateSettings
    /// `GET /v1/backups` - the archives the daemon holds.
    case backups
    /// `POST /v1/backups` - write one verified archive.
    case createBackup
    /// `POST /v1/backups/{name}/restore` - replace the live state from one.
    case restoreBackup(name: String)
    /// `POST /v1/settings/full-access` - the switch that decides whether `full`
    /// is a sandbox a save may pick. Enabling needs the word typed; disabling
    /// needs nothing and demotes a `full` sandbox with it.
    case fullAccess
    /// `GET /v1/projects` - the names a conversation's `project_id` points at.
    case projects
    /// `GET /v1/approvals` - the calls parked for a person, newest first.
    case approvals
    /// `POST /v1/approvals/{id}/decide` - settle one, and run it when approved.
    case decideApproval(approvalID: String)
    /// `POST /v1/device/sensor/location` - deliver one location sample.
    case sensorLocation
    /// `POST /v1/device/sensor/health` - deliver a batch of health samples.
    case sensorHealth
    /// `GET /v1/device/health/latest` - the newest metric per tracked kind.
    case healthLatest
    /// `GET /v1/device/location` - the most recent location the daemon holds.
    case location
    /// `GET /v1/inbox` - list the device's inbox items.
    case inbox
    /// `POST /v1/inbox/{item_id}/action` - act on one inbox item.
    case inboxAction(itemID: String)
    /// `GET /v1/notes` - list the device's notes.
    case notes
    /// `POST /v1/notes` - create a note.
    case createNote
    /// `GET /v1/notes/{note_id}` - one note.
    case noteDetail(noteID: String)
    /// `PATCH /v1/notes/{note_id}` - update a note's metadata (If-Match gated).
    case updateNote(noteID: String)
    /// `DELETE /v1/notes/{note_id}` - delete a note.
    case deleteNote(noteID: String)
    /// `POST /v1/notes/{note_id}/recognitions` - stage the note's text content.
    case noteRecognition(noteID: String)
    /// `POST /v1/notes/{note_id}/runs` - open an enrichment run.
    case noteRuns(noteID: String)
    /// `GET /v1/note-runs/{run_id}` - one run's state.
    case noteRun(runID: String)
    /// `GET /v1/note-runs/{run_id}/events` - the run's event log.
    case noteRunEvents(runID: String)
    /// `POST /v1/note-runs/{run_id}/cancel` - stop a run.
    case cancelNoteRun(runID: String)
    /// `GET /v1/talk/readiness` - the voice lane's placeholder readiness probe.
    case talkReadiness
    /// `GET /v1/models` - the pin, the probed catalog and the providers.
    case models
    /// `POST /v1/models` - pin the default model.
    case setModel
    /// `GET /v1/cron` - the schedules and the ticker switch.
    case cron
    /// `POST /v1/cron` - create one schedule.
    case createCron
    /// `PATCH`/`DELETE /v1/cron/{id}` - edit, switch or remove one schedule.
    ///
    /// One case with the verb carried as data because the router binds three
    /// verbs to one path, and a case per verb would triple the path literal.
    case cronJob(id: String, verb: String)
    /// `GET /v1/skills` - the skill library.
    case skills
    /// `GET /v1/skills/{key}` - one skill's body.
    case skill(key: String)
    /// `GET /v1/aux` - the auxiliary roles and what each may run on.
    case aux
    /// `POST /v1/aux` - pin one role's model and provider.
    case setAux
    /// `GET /v1/subagents` - the subagents this daemon has run, newest first.
    case subagents
    /// `GET /v1/subagents/{id}` - one row, whether or not it is still listed.
    case subagent(id: String)
    /// `GET /v1/gw/health` - the daemon's own state.
    case gwHealth
    /// `GET /v1/gw/restart/preflight` - restart confirmation snapshot.
    case gwPreflight
    /// `POST /v1/gw/restart` - schedule a self-restart.
    case gwRestart

    /// The request path, with ids percent-encoded so one id cannot escape its
    /// path segment.
    var path: String {
        switch self {
        case .pair:
            return "/v1/pair"
        case .tokenRefresh:
            return "/v1/token/refresh"
        case .turns:
            return "/v1/turns"
        case .events(let turnID):
            return "/v1/turns/\(Self.segment(turnID))/events"
        case .cancel(let turnID):
            return "/v1/turns/\(Self.segment(turnID))/cancel"
        case .asked(let turnID):
            return "/v1/turns/\(Self.segment(turnID))/asked"
        case .answer(let turnID):
            return "/v1/turns/\(Self.segment(turnID))/answer"
        case .session(let sessionID):
            return "/v1/sessions/\(Self.segment(sessionID))"
        case .conversations, .createConversation:
            return "/v1/conversations"
        case .conversation(let conversationID):
            return "/v1/conversations/\(Self.segment(conversationID))"
        case .clearConversation(let conversationID):
            return "/v1/conversations/\(Self.segment(conversationID))/clear"
        case .settings, .updateSettings:
            return "/v1/settings"
        case .fullAccess:
            return "/v1/settings/full-access"
        case .backups, .createBackup:
            return "/v1/backups"
        case .restoreBackup(let name):
            return "/v1/backups/\(Self.segment(name))/restore"
        case .projects:
            return "/v1/projects"
        case .approvals:
            return "/v1/approvals"
        case .decideApproval(let approvalID):
            return "/v1/approvals/\(Self.segment(approvalID))/decide"
        case .sensorLocation:
            return "/v1/device/sensor/location"
        case .sensorHealth:
            return "/v1/device/sensor/health"
        case .healthLatest:
            return "/v1/device/health/latest"
        case .location:
            return "/v1/device/location"
        case .inbox:
            return "/v1/inbox"
        case .inboxAction(let itemID):
            return "/v1/inbox/\(Self.segment(itemID))/action"
        case .notes, .createNote:
            return "/v1/notes"
        case .noteDetail(let noteID):
            return "/v1/notes/\(Self.segment(noteID))"
        case .updateNote(let noteID):
            return "/v1/notes/\(Self.segment(noteID))"
        case .deleteNote(let noteID):
            return "/v1/notes/\(Self.segment(noteID))"
        case .noteRecognition(let noteID):
            return "/v1/notes/\(Self.segment(noteID))/recognitions"
        case .noteRuns(let noteID):
            return "/v1/notes/\(Self.segment(noteID))/runs"
        case .noteRun(let runID):
            return "/v1/note-runs/\(Self.segment(runID))"
        case .noteRunEvents(let runID):
            return "/v1/note-runs/\(Self.segment(runID))/events"
        case .cancelNoteRun(let runID):
            return "/v1/note-runs/\(Self.segment(runID))/cancel"
        case .models, .setModel:
            return "/v1/models"
        case .cron, .createCron:
            return "/v1/cron"
        case .cronJob(let id, _):
            return "/v1/cron/\(Self.segment(id))"
        case .skills:
            return "/v1/skills"
        case .skill(let key):
            return "/v1/skills/\(Self.segment(key))"
        case .aux, .setAux:
            return "/v1/aux"
        case .subagents:
            return "/v1/subagents"
        case .subagent(let id):
            return "/v1/subagents/\(Self.segment(id))"
        case .talkReadiness:
            return "/v1/talk/readiness"
        case .gwHealth:
            return "/v1/gw/health"
        case .gwPreflight:
            return "/v1/gw/restart/preflight"
        case .gwRestart:
            return "/v1/gw/restart"
        }
    }

    /// The HTTP method the daemon's router binds this path to.
    var method: String {
        switch self {
        case .events, .session, .conversations, .conversation, .settings, .projects, .approvals,
             .backups,
             .healthLatest, .location, .inbox, .notes, .noteDetail, .talkReadiness,
             .models, .cron, .skills, .aux, .noteRun, .noteRunEvents, .gwHealth, .gwPreflight,
             .subagents, .subagent, .asked:
            return "GET"
        case .skill:
            return "GET"
        case .pair, .tokenRefresh, .turns, .cancel, .createConversation, .clearConversation, .updateSettings,
             .fullAccess, .createBackup, .restoreBackup,
             .decideApproval, .sensorLocation, .sensorHealth, .inboxAction, .createNote,
             .setModel, .createCron, .setAux,
             .noteRecognition, .noteRuns, .cancelNoteRun, .gwRestart, .answer:
            return "POST"
        case .cronJob(_, let verb):
            return verb
        case .updateNote:
            return "PATCH"
        case .deleteNote:
            return "DELETE"
        }
    }

    /// Percent-encode one path segment, leaving unreserved characters alone.
    static func segment(_ value: String) -> String {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        return value.addingPercentEncoding(withAllowedCharacters: allowed) ?? value
    }
}

// MARK: - Client

/// The Kallisti-Eris device transport.
///
/// Foundation only: no UIKit, no app types, so this layer can be exercised with
/// an injected `URLSession` and an in-memory token store. An actor because every
/// call mutates or reads the stored pair (a 401 refresh rewrites it while other
/// calls may be in flight).
actor ErisClient {
    /// How much of a non-2xx body is read to recover `{code,message}`.
    private static let maximumErrorBodyBytes = 64 * 1024

    /// The device listener base URL, e.g. `http://192.168.1.20:7341`.
    private let baseURL: URL
    /// The session requests go through. Injected so tests never touch a socket.
    private let session: URLSession
    /// Where the bearer and refresh token live.
    private let tokenStore: any ErisTokenStoring

    /// Build a client for one listener.
    init(baseURL: URL, tokenStore: any ErisTokenStoring, session: URLSession = .shared) {
        self.baseURL = baseURL
        self.tokenStore = tokenStore
        self.session = session
    }

    // MARK: Routes

    /// Join a route path onto a base URL.
    ///
    /// String concatenation rather than `URLComponents`: the route paths already
    /// carry percent-escaped ids, and re-encoding them through a path component
    /// would double-escape `%`.
    static func url(baseURL: URL, route: ErisRoute) throws -> URL {
        let base = baseURL.absoluteString
        var trimmed = base.hasSuffix("/") ? String(base.dropLast()) : base
        // Every route path already carries the listener's `/v1` prefix, and
        // `RelayConfiguration.normalizeBaseURL` normalizes a typed host to end
        // in `/v1`. Joining both produced `.../v1/v1/pair`, which the listener
        // answers with a typed 404. Exactly one `/v1` at the join, never two.
        if route.path.hasPrefix("/v1/"), trimmed.hasSuffix("/v1") {
            trimmed.removeLast(3)
        }
        guard let url = URL(string: trimmed + route.path) else {
            throw ErisError.invalidBaseURL(base)
        }
        return url
    }

    /// The URL this client would call for a route.
    func url(for route: ErisRoute) throws -> URL {
        try Self.url(baseURL: baseURL, route: route)
    }

    // MARK: Pairing

    /// `POST /v1/pair` - redeem a single-use pairing code.
    ///
    /// The daemon answers `201` with `{ok, access_token, refresh_token,
    /// device_id, scopes, expires_at}` and binds the pair to this device. The
    /// pair is stored before it is returned, so a caller that crashes between the
    /// response and the next screen has not lost the only refresh token.
    func pair(code: String, deviceName: String) async throws -> ErisTokenPair {
        let trimmedCode = code.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedCode.isEmpty else {
            throw ErisError.server(
                status: 401,
                code: "pairing_code_invalid",
                message: "a pairing code is required"
            )
        }
        var form = ["code": trimmedCode]
        let name = deviceName.trimmingCharacters(in: .whitespacesAndNewlines)
        if !name.isEmpty {
            form["device_name"] = name
        }
        let data = try await sendAnonymous(route: .pair, form: form)
        let tokens = try decode(ErisTokenPair.self, from: data)
        try await tokenStore.save(ErisStoredTokens(baseURL: baseURL, tokens: tokens))
        return tokens
    }

    /// `POST /v1/token/refresh` - one fresh access token.
    ///
    /// The daemon does not rotate the refresh token on this route, so the stored
    /// pair keeps the refresh token it already had and only the access half is
    /// replaced.
    func refresh() async throws -> ErisTokenPair {
        guard let stored = try await tokenStore.load() else {
            throw ErisError.missingToken
        }
        guard let refreshToken = stored.refreshToken, !refreshToken.isEmpty else {
            throw ErisError.missingToken
        }
        let data = try await sendAnonymous(route: .tokenRefresh, form: ["refresh_token": refreshToken])
        let fresh = try decode(ErisTokenPair.self, from: data)
        let merged = stored.tokens.mergingRefreshResponse(fresh)
        try await tokenStore.save(ErisStoredTokens(baseURL: baseURL, tokens: merged))
        return merged
    }

    // MARK: Turns

    /// `POST /v1/turns` - queue one turn and take the `202`.
    ///
    /// The response arrives before the provider is called, so this returns as
    /// soon as the job exists; the text comes from `events(turnID:)`.
    ///
    /// `conversation` is the optional field the handler reads to make this turn a
    /// transcript row pair instead of a session of its own. A conversation id the
    /// daemon does not hold is refused here, before anything is written, so a
    /// refused turn never leaves half a conversation behind.
    /// `clientMessageID` is the id the app minted for this message. It travels
    /// twice: in the body as `client_message_id`, so the daemon stamps it on the
    /// row it writes, and as the `idempotency-key` header derived from it, so a
    /// retry of the same message resolves to the same job instead of queueing a
    /// second turn. Passing `nil` is the old behaviour exactly: no key, no
    /// echoed id, and a retry that duplicates.
    func createTurn(
        text: String,
        conversation: String? = nil,
        clientMessageID: UUID? = nil
    ) async throws -> ErisTurn {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            throw ErisError.server(status: 400, code: "text_required", message: "the turn text must not be empty")
        }
        var form = ["text": trimmed]
        if let conversation {
            let trimmedID = conversation.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmedID.isEmpty {
                form["conversation"] = trimmedID
            }
        }
        var headers: [String: String] = [:]
        if let clientMessageID {
            form[ErisTurnWire.clientMessageIDField] = clientMessageID.uuidString
            headers[ErisTurnWire.idempotencyHeader] = ErisTurnWire.idempotencyKey(
                for: clientMessageID
            )
        }
        let data = try await send(route: .turns, form: form, extraHeaders: headers)
        return try decode(ErisTurn.self, from: data)
    }

    /// `GET /v1/turns/{id}/events` - the turn as server-sent events.
    ///
    /// Frames are `delta`, `done`, `error` and `cancelled`. The stream finishes
    /// after the first terminal frame; a transport failure or a malformed frame
    /// finishes it with a thrown error, so a caller can distinguish "the turn
    /// ended" from "the stream broke".
    func events(turnID: String) -> AsyncThrowingStream<ErisFrameOutcome, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    try await self.streamEvents(turnID: turnID, into: continuation)
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    /// `POST /v1/turns/{id}/cancel` - stop a running turn.
    ///
    /// A second cancel is the daemon's `409 turn_not_cancellable`, surfaced as
    /// `ErisError.server(status: 409, code: "turn_not_cancellable", ...)`.
    func cancel(turnID: String) async throws {
        _ = try await send(route: .cancel(turnID: turnID), form: nil)
    }

    /// `GET /v1/turns/{id}/asked` - the question this turn is waiting on.
    ///
    /// `nil` means nothing is waiting, which is a normal answer rather than an
    /// error: the turn either never asked, was already answered, or has ended.
    /// The route distinguishes all three from "waiting" by not pretending a
    /// question exists when none does.
    func asked(turnID: String) async throws -> ErisAsked? {
        let data = try await send(route: .asked(turnID: turnID), form: nil)
        // The device surface wraps successes in `{data, meta}` and this route's
        // body is `{ok, waiting, asked}` inside it, so both are unwrapped here.
        let outer = try ErisJSON.object(String(decoding: data, as: UTF8.self))
        let body = (outer["data"] as? [String: Any]) ?? outer
        guard body["waiting"] as? Bool == true else { return nil }
        guard let asked = body["asked"] as? [String: Any],
              let id = asked["id"] as? String
        else { return nil }
        return try ErisAsked.decode(id: id, request: asked["request"])
    }

    /// `POST /v1/turns/{id}/answer` - settle the question, resuming the turn.
    ///
    /// At least one label, or some words. Both is allowed and means "these
    /// options, and this note about them". An empty answer is refused by the
    /// daemon as `answer_empty` rather than silently leaving the turn parked.
    func answer(turnID: String, labels: [String], note: String?, text: String?, question: String?) async throws {
        var form: [String: String] = [:]
        // The unit separator is the daemon's wire shape for a label list, the same
        // one the browser's card posts, so one script-shaped answer serves both.
        if !labels.isEmpty {
            form["labels"] = labels.joined(separator: "\u{1f}")
        }
        if let note, !note.isEmpty { form["note"] = note }
        if let text, !text.isEmpty { form["text"] = text }
        if let question, !question.isEmpty { form["question"] = question }
        _ = try await send(route: .answer(turnID: turnID), form: form)
    }

    /// `GET /v1/sessions/{id}` - the session snapshot.
    func session(sessionID: String) async throws -> ErisSession {
        let data = try await send(route: .session(sessionID: sessionID), form: nil)
        return try decode(ErisSession.self, from: data)
    }

    // MARK: Conversations

    /// `GET /v1/conversations` - the newest conversations, newest first.
    ///
    /// `truncated` in the answer is the daemon saying it cut the page, which is a
    /// fact the client has to be able to state rather than a silent cap.
    func conversations() async throws -> ErisConversationPage {
        let data = try await send(route: .conversations, form: nil)
        return try decode(ErisConversationPage.self, from: data)
    }

    /// `POST /v1/conversations` - start an empty conversation, `201` with its id.
    ///
    /// The route takes no body: the daemon names it with the same placeholder the
    /// browser uses, and the first question renames it through the same code.
    func createConversation() async throws -> ErisCreatedConversation {
        let data = try await send(route: .createConversation, form: nil)
        return try decode(ErisCreatedConversation.self, from: data)
    }

    /// `GET /v1/conversations/{id}` - one conversation and its stored transcript.
    ///
    /// An id the daemon does not hold is a typed 404 (`conversation_not_found`),
    /// not an empty transcript, so "gone" and "nothing said yet" stay distinct.
    func conversation(id: String) async throws -> ErisConversationDetail {
        let data = try await send(route: .conversation(conversationID: id), form: nil)
        return try decode(ErisConversationDetail.self, from: data)
    }

    /// `POST /v1/conversations/{id}/clear` - drop that conversation's messages.
    ///
    /// Returns how many messages the daemon dropped. The conversation row itself
    /// stays: clearing a conversation is not deleting it.
    func clearConversation(id: String) async throws -> Int {
        let data = try await send(route: .clearConversation(conversationID: id), form: nil)
        return try decode(ErisClearResult.self, from: data).cleared
    }

    /// `GET /v1/projects` - the project list, so `project_id` can be shown as a
    /// name. The conversation list carries ids only.
    func projects() async throws -> [ErisProjectSummary] {
        let data = try await send(route: .projects, form: nil)
        return try decode(ErisProjectList.self, from: data).projects
    }

    // MARK: Settings

    /// `GET /v1/settings` - the operator-facing snapshot.
    ///
    /// Needs the `admin` scope. A device paired from a code minted without the
    /// administration box gets a typed `403 scope_denied` whose message is the
    /// daemon's own sentence about the pairing code; that message is what the
    /// settings screen shows, because it is more accurate than anything a client
    /// could invent here.
    // MARK: Control plane (models, schedules, skills, auxiliary roles)
    //
    // Every one of these needs the `admin` scope, so a device paired without the
    // administration box gets the daemon's own 403 scope_denied sentence and no
    // second implementation of the gate lives over here.

    /// `GET /v1/models` - the pin, the probed catalog and the providers.
    func controlModels() async throws -> ErisControlModels {
        let data = try await send(route: .models, form: nil)
        return try decode(ErisControlModels.self, from: data)
    }

    /// `POST /v1/models` - pin the default model. The id is the catalog id.
    func setControlModel(_ model: String) async throws {
        let _: ErisControlModelSet = try await sendJSON(
            route: .setModel,
            body: ErisControlModelBody(model: model)
        )
    }

    /// `GET /v1/cron` - the schedules, plus whether the ticker runs at all.
    func controlCron() async throws -> ErisControlCronList {
        let data = try await send(route: .cron, form: nil)
        return try decode(ErisControlCronList.self, from: data)
    }

    /// `POST /v1/cron` - create one schedule.
    func createControlCron(name: String, schedule: String, prompt: String) async throws -> ErisControlCronJobEnvelope {
        try await sendJSON(
            route: .createCron,
            body: ErisControlCronContent(name: name, schedule: schedule, prompt: prompt)
        ) as ErisControlCronJobEnvelope
    }

    /// `PATCH /v1/cron/{id}` with `{"enabled": ...}` - the switch the row draws.
    func setControlCronEnabled(id: String, enabled: Bool) async throws -> ErisControlCronJobEnvelope {
        try await sendJSON(
            route: .cronJob(id: id, verb: "PATCH"),
            body: ErisControlCronFlag(enabled: enabled)
        ) as ErisControlCronJobEnvelope
    }

    /// `PATCH /v1/cron/{id}` with all three content fields - the editor.
    func updateControlCron(id: String, name: String, schedule: String, prompt: String) async throws -> ErisControlCronJobEnvelope {
        try await sendJSON(
            route: .cronJob(id: id, verb: "PATCH"),
            body: ErisControlCronContent(name: name, schedule: schedule, prompt: prompt)
        ) as ErisControlCronJobEnvelope
    }

    /// `DELETE /v1/cron/{id}`.
    func deleteControlCron(id: String) async throws {
        let _: ErisControlEmpty = try await sendJSON(
            route: .cronJob(id: id, verb: "DELETE"),
            body: ErisControlEmpty()
        )
    }

    /// `GET /v1/skills` - the library.
    func controlSkills() async throws -> ErisControlSkillList {
        let data = try await send(route: .skills, form: nil)
        return try decode(ErisControlSkillList.self, from: data)
    }

    /// `GET /v1/skills/{key}` - one skill's body.
    func controlSkill(key: String) async throws -> ErisControlSkillEnvelope {
        let data = try await send(route: .skill(key: key), form: nil)
        return try decode(ErisControlSkillEnvelope.self, from: data)
    }

    /// `GET /v1/aux` - the roles, and the ids a picker may choose from.
    func controlAux() async throws -> ErisControlAuxList {
        let data = try await send(route: .aux, form: nil)
        return try decode(ErisControlAuxList.self, from: data)
    }

    /// `POST /v1/aux` - pin one role. `"primary"` means "follow the primary".
    func setControlAux(role: String, provider: String, model: String) async throws {
        let _: ErisControlAuxSet = try await sendJSON(
            route: .setAux,
            body: ErisControlAuxBody(role: role, provider: provider, model: model)
        )
    }

    /// `GET /v1/subagents` - what this daemon has handed off, newest first.
    ///
    /// The list is the dashboard bar's own view: anything still working plus
    /// anything that finished recently. A row that aged out is still readable by
    /// its id, which is the handle the dispatch tool returned.
    func subagents() async throws -> ErisSubagentList {
        let data = try await send(route: .subagents, form: nil)
        return try decode(ErisSubagentList.self, from: data)
    }

    /// `GET /v1/subagents/{id}` - one row by the id the tool returned.
    func subagent(id: String) async throws -> ErisSubagentDetail {
        let data = try await send(route: .subagent(id: id), form: nil)
        return try decode(ErisSubagentDetail.self, from: data)
    }

    // MARK: Gateway control (`/v1/gw/*`)

    /// `GET /v1/gw/health` - the daemon's own state, in the legacy field names.
    func gwHealth() async throws -> GatewayHealth {
        let data = try await send(route: .gwHealth, form: nil)
        return try decode(GatewayHealth.self, from: data)
    }

    /// `GET /v1/gw/restart/preflight` - the snapshot a restart confirmation shows.
    func gwPreflight() async throws -> RestartPreflight {
        let data = try await send(route: .gwPreflight, form: nil)
        return try decode(RestartPreflight.self, from: data)
    }

    /// `POST /v1/gw/restart` - schedule a self-restart, answering the legacy
    /// `{restarting: ...}` shape (no operation tracking on Eris).
    func gwRestart(target: String, preflightVersion: String) async throws -> RestartResponse {
        try await sendJSON(
            route: .gwRestart,
            body: RestartSubmission(target: target, preflightVersion: preflightVersion)
        )
    }

    func settings() async throws -> ErisSettingsSnapshot {
        let data = try await send(route: .settings, form: nil)
        return try decode(ErisSettingsSnapshot.self, from: data)
    }

    /// `POST /v1/settings` - write the subset of settings that actually changed.
    ///
    /// An empty patch is refused here rather than sent: the daemon answers an
    /// empty body with `400 settings_empty`, and a client that knows it changed
    /// nothing has no reason to make the daemon say so.
    func updateSettings(_ patch: ErisSettingsPatch) async throws -> [String] {
        guard !patch.isEmpty else {
            throw ErisError.server(
                status: 400,
                code: "settings_empty",
                message: "nothing changed, so nothing was sent"
            )
        }
        let data = try await send(route: .updateSettings, form: patch.fields)
        return try decode(ErisSettingsUpdate.self, from: data).changed
    }

    /// `GET /v1/backups` - the archives on the daemon, newest first.
    func backups() async throws -> ErisBackupPage {
        let data = try await send(route: .backups, form: nil)
        return try decode(ErisBackupPage.self, from: data)
    }

    /// `POST /v1/backups` - write one verified archive.
    ///
    /// The body is empty on purpose: the daemon then writes a generated
    /// `kallisti-<stamp>.bk` in its own archive directory, which is where a
    /// person would look for it. A phone has nothing better to say about where
    /// the daemon keeps its own backups.
    func createBackup() async throws -> ErisBackupCreated {
        let data = try await send(route: .createBackup, form: nil)
        return try decode(ErisBackupCreated.self, from: data)
    }

    /// `POST /v1/backups/{name}/restore` - replace the live state from one.
    ///
    /// The daemon verifies every member before it writes, and keeps a safety
    /// copy. It cannot take effect while it holds the database open, so the
    /// restored state is live on the next service start and the answer says so.
    func restoreBackup(name: String) async throws -> ErisBackupRestored {
        let data = try await send(route: .restoreBackup(name: name), form: nil)
        return try decode(ErisBackupRestored.self, from: data)
    }

    /// `POST /v1/settings/full-access` - turn full host access on or off.
    ///
    /// Enable is the guarded direction: the daemon requires the word `full`, so
    /// a dialog here is not ceremony, it is the same guard the browser has.
    /// Disable needs nothing, and the daemon demotes a `full` sandbox with it,
    /// so the answer carries the sandbox it now holds rather than echoing the
    /// request.
    func setFullAccess(enable: Bool, confirm: String? = nil) async throws -> ErisFullAccessResult {
        var form: [String: String] = ["enable": enable ? "on" : "off"]
        if let confirm, !confirm.isEmpty { form["confirm"] = confirm }
        let data = try await send(route: .fullAccess, form: form)
        return try decode(ErisFullAccessResult.self, from: data)
    }

    /// `POST /v1/settings` - write one field from the daemon's own record.
    ///
    /// The record names the key and the value's spelling, so a write is one
    /// `key=value` pair and the daemon's parser does the judging. A refusal comes
    /// back naming the field and the values it would have accepted.
    func updateSetting(_ write: ErisSettingWrite) async throws -> [String] {
        let data = try await send(route: .updateSettings, form: write.fields)
        return try decode(ErisSettingsUpdate.self, from: data).changed
    }

    /// `GET /v1/approvals` - the queue an admin device can answer.
    ///
    /// Needs the `admin` scope: a device paired from a code minted without the
    /// administration box is refused with the daemon's own sentence.
    func approvals() async throws -> ErisApprovalPage {
        let data = try await send(route: .approvals, form: nil)
        return try ErisApprovalPage.decode(data)
    }

    /// `POST /v1/approvals/{id}/decide` - answer one.
    ///
    /// An approval runs the call, so the answer carries what the tool said. The
    /// client returns that rather than discarding it: a person tapping Allow
    /// wants to see the consequence.
    func decideApproval(id: String, decision: ErisApprovalDecision) async throws -> ErisApprovalOutcome {
        let data = try await send(
            route: .decideApproval(approvalID: id),
            form: ["decision": decision.rawValue]
        )
        return try ErisApprovalOutcome.decode(data)
    }

    // MARK: Sensors & health
    //
    // These routes mirror the legacy connector payloads exactly, so the daemon
    // being built to accept them sees the same shapes the old relay forwarded.

    /// `POST /v1/device/sensor/location` - deliver one location sample.
    func uploadSensorLocation(_ body: ErisSensorLocationPayload) async throws -> ErisDeliveryResult {
        try await sendJSON(route: .sensorLocation, body: body)
    }

    /// `POST /v1/device/sensor/health` - deliver a batch of health samples.
    func uploadSensorHealth(_ body: ErisSensorHealthPayload) async throws -> ErisDeliveryResult {
        try await sendJSON(route: .sensorHealth, body: body)
    }

    /// `GET /v1/device/health/latest` - the newest metric per tracked kind.
    func latestHealthMetrics() async throws -> ErisHealthLatest {
        let data = try await send(route: .healthLatest, form: nil)
        return try decode(ErisHealthLatest.self, from: data)
    }

    /// `GET /v1/device/location` - the most recent location the daemon holds.
    func latestLocation() async throws -> ErisLocationSnapshot {
        let data = try await send(route: .location, form: nil)
        return try decode(ErisLocationSnapshot.self, from: data)
    }

    // MARK: Inbox

    /// `GET /v1/inbox` - the device's inbox items.
    func inbox() async throws -> ErisInboxEnvelope {
        let data = try await send(route: .inbox, form: nil)
        return try decode(ErisInboxEnvelope.self, from: data)
    }

    /// `POST /v1/inbox/{item_id}/action` - act on one inbox item.
    func inboxAction(itemID: String, action: String) async throws -> ErisInboxActionEnvelope {
        try await sendJSON(route: .inboxAction(itemID: itemID), body: ErisInboxActionPayload(actionId: action))
    }

    // MARK: Notes

    /// `GET /v1/notes` - the device's notes.
    func notes() async throws -> ErisNotesListEnvelope {
        let data = try await send(route: .notes, form: nil)
        return try decode(ErisNotesListEnvelope.self, from: data)
    }

    /// `POST /v1/notes` - create a note.
    func createNote(_ payload: ErisNoteCreatePayload) async throws -> ErisNoteEnvelope {
        try await sendJSON(route: .createNote, body: payload)
    }

    /// `GET /v1/notes/{note_id}` - one note.
    func note(id: String) async throws -> ErisNoteEnvelope {
        let data = try await send(route: .noteDetail(noteID: id), form: nil)
        return try decode(ErisNoteEnvelope.self, from: data)
    }

    /// `PATCH /v1/notes/{note_id}` - update a note's metadata, gated by the
    /// quoted `If-Match` revision so a stale edit cannot clobber a newer one.
    func updateNote(id: String, revision: Int64, payload: ErisNotePatchPayload) async throws -> ErisNoteEnvelope {
        try await sendJSON(
            route: .updateNote(noteID: id),
            body: payload,
            extraHeaders: ["If-Match": "\"\(revision)\""]
        )
    }

    /// `DELETE /v1/notes/{note_id}` - delete a note.
    func deleteNote(id: String) async throws {
        _ = try await send(route: .deleteNote(noteID: id), form: nil)
    }

    // MARK: Note runs (enrichment)

    /// `POST /v1/notes/{note_id}/recognitions` - stage the note's text so the
    /// daemon's enrichment worker has something to read.
    func createNoteRecognition(
        noteID: String,
        payload: ErisNoteRecognitionPayload
    ) async throws -> ErisNoteRecognitionEnvelope {
        try await sendJSON(route: .noteRecognition(noteID: noteID), body: payload)
    }

    /// `POST /v1/notes/{note_id}/runs` - open an enrichment run.
    func createNoteRun(
        noteID: String,
        payload: ErisNoteRunCreatePayload
    ) async throws -> ErisNoteRunEnvelope {
        try await sendJSON(route: .noteRuns(noteID: noteID), body: payload)
    }

    /// `GET /v1/note-runs/{run_id}` - one run's state.
    func noteRun(runID: String) async throws -> ErisNoteRunEnvelope {
        let data = try await send(route: .noteRun(runID: runID), form: nil)
        return try decode(ErisNoteRunEnvelope.self, from: data)
    }

    /// `GET /v1/note-runs/{run_id}/events` - the event log, oldest first.
    ///
    /// No cursor is sent: the log of one enrichment is tens of rows, so the whole
    /// thing is fetched and the caller deduplicates on `seq`. A cursor would be
    /// the right shape for a long log and this is not one.
    func noteRunEvents(runID: String) async throws -> [ErisNoteRunEvent] {
        let data = try await send(route: .noteRunEvents(runID: runID), form: nil)
        let envelope = try decode(ErisNoteRunEventsEnvelope.self, from: data)
        return envelope.data
    }

    /// `POST /v1/note-runs/{run_id}/cancel` - stop a run. Idempotent.
    func cancelNoteRun(runID: String) async throws -> ErisNoteRunEnvelope {
        try await sendJSON(route: .cancelNoteRun(runID: runID), body: EmptyBody())
    }

    // MARK: Talk

    /// `GET /v1/talk/readiness` - the voice lane's placeholder readiness probe.
    func talkReadiness() async throws -> ErisTalkReadiness {
        let data = try await send(route: .talkReadiness, form: nil)
        return try decode(ErisTalkReadiness.self, from: data)
    }

    // MARK: Streaming

    /// Open the stream, refresh once on a 401, and feed bytes to the decoder.
    ///
    /// Emits `ErisFrameOutcome`, not `ErisFrame`: the daemon's vocabulary has six
    /// frames and `ErisFrame` models four, so the stream has to be able to carry
    /// a reasoning fragment and a finished tool call without either being dropped
    /// (which is what `default: return nil` used to do) or pretending to be a
    /// text delta.
    private func streamEvents(
        turnID: String,
        into continuation: AsyncThrowingStream<ErisFrameOutcome, Error>.Continuation
    ) async throws {
        let route = ErisRoute.events(turnID: turnID)
        var (bytes, status) = try await openStream(
            try await authorizedRequest(route: route, form: nil, accept: "text/event-stream")
        )
        if status == 401 {
            _ = try await refresh()
            (bytes, status) = try await openStream(
                try await authorizedRequest(route: route, form: nil, accept: "text/event-stream")
            )
        }
        guard (200..<300).contains(status) else {
            let body = await collect(bytes: bytes, limit: Self.maximumErrorBodyBytes)
            throw Self.serverError(status: status, data: body)
        }

        var decoder = ErisSSEDecoder()
        var pending = Data()
        for try await byte in bytes {
            if Task.isCancelled {
                continuation.finish()
                return
            }
            pending.append(byte)
            // Complete the line on the newline, so a frame that straddles two
            // chunks is decoded once, when its blank-line terminator lands.
            guard byte == 0x0A else { continue }
            let chunk = pending
            pending.removeAll(keepingCapacity: true)
            if emit(try decoder.consume(chunk), into: continuation) {
                return
            }
        }
        if !pending.isEmpty, emit(try decoder.consume(pending), into: continuation) {
            return
        }
        if emit(try decoder.finish(), into: continuation) {
            return
        }
        continuation.finish()
    }

    /// Yield outcomes, and report whether a terminal one ended the stream.
    private func emit(
        _ outcomes: [ErisFrameOutcome],
        into continuation: AsyncThrowingStream<ErisFrameOutcome, Error>.Continuation
    ) -> Bool {
        for outcome in outcomes {
            continuation.yield(outcome)
            if outcome.isTerminal {
                continuation.finish()
                return true
            }
        }
        return false
    }

    // MARK: Requests

    /// A request that carries the bearer, retried once after a refresh on a 401.
    private func send(
        route: ErisRoute,
        form: [String: String]?,
        extraHeaders: [String: String] = [:]
    ) async throws -> Data {
        var refreshed = false
        while true {
            let request = try await authorizedRequest(
                route: route,
                form: form,
                accept: "application/json",
                extraHeaders: extraHeaders
            )
            let (data, response) = try await perform(request)
            let status = Self.status(of: response)
            if status == 401 && !refreshed {
                _ = try await refresh()
                refreshed = true
                continue
            }
            guard (200..<300).contains(status) else {
                throw Self.serverError(status: status, data: data)
            }
            return data
        }
    }

    /// A request with no bearer: pairing and refresh are the two routes that
    /// mint credentials rather than use them.
    private func sendAnonymous(route: ErisRoute, form: [String: String]?) async throws -> Data {
        let request = try makeRequest(route: route, form: form)
        let (data, response) = try await perform(request)
        let status = Self.status(of: response)
        guard (200..<300).contains(status) else {
            throw Self.serverError(status: status, data: data)
        }
        return data
    }

    private func authorizedRequest(
        route: ErisRoute,
        form: [String: String]?,
        accept: String,
        extraHeaders: [String: String] = [:]
    ) async throws -> URLRequest {
        guard let stored = try await tokenStore.load(), !stored.accessToken.isEmpty else {
            throw ErisError.missingToken
        }
        var request = try makeRequest(route: route, form: form)
        request.setValue("Bearer \(stored.accessToken)", forHTTPHeaderField: "Authorization")
        request.setValue(accept, forHTTPHeaderField: "Accept")
        // Ridden on every attempt, refresh included: the retry after a 401 is the
        // very retry the idempotency key exists to collapse.
        for (field, value) in extraHeaders {
            request.setValue(value, forHTTPHeaderField: field)
        }
        return request
    }

    private func makeRequest(route: ErisRoute, form: [String: String]?) throws -> URLRequest {
        var request = URLRequest(url: try Self.url(baseURL: baseURL, route: route))
        request.httpMethod = route.method
        request.timeoutInterval = 30
        if route.method == "POST" {
            request.httpBody = Data(Self.formEncode(form ?? [:]).utf8)
            request.setValue(
                "application/x-www-form-urlencoded; charset=utf-8",
                forHTTPHeaderField: "Content-Type"
            )
        }
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        return request
    }

    /// A JSON-bodied request carrying the bearer, retried once after a refresh on
    /// a 401. Same contract as `send`, but the body is `application/json` rather
    /// than form-encoded - the sensor, inbox and notes routes take structured
    /// payloads. `extraHeaders` (e.g. the notes `If-Match`) ride on every attempt.
    private func sendJSON<Body: Encodable, T: Decodable>(
        route: ErisRoute,
        body: Body,
        extraHeaders: [String: String] = [:]
    ) async throws -> T {
        var refreshed = false
        while true {
            let request = try await authorizedJSONRequest(route: route, body: body, extraHeaders: extraHeaders)
            let (data, response) = try await perform(request)
            let status = Self.status(of: response)
            if status == 401 && !refreshed {
                _ = try await refresh()
                refreshed = true
                continue
            }
            guard (200..<300).contains(status) else {
                throw Self.serverError(status: status, data: data)
            }
            return try decode(T.self, from: data)
        }
    }

    private func authorizedJSONRequest<Body: Encodable>(
        route: ErisRoute,
        body: Body,
        extraHeaders: [String: String] = [:]
    ) async throws -> URLRequest {
        guard let stored = try await tokenStore.load(), !stored.accessToken.isEmpty else {
            throw ErisError.missingToken
        }
        var request = try makeJSONRequest(route: route, body: body)
        request.setValue("Bearer \(stored.accessToken)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        for (field, value) in extraHeaders {
            request.setValue(value, forHTTPHeaderField: field)
        }
        return request
    }

    private func makeJSONRequest<Body: Encodable>(route: ErisRoute, body: Body) throws -> URLRequest {
        var request = URLRequest(url: try Self.url(baseURL: baseURL, route: route))
        request.httpMethod = route.method
        request.timeoutInterval = 30
        if route.method == "POST" || route.method == "PATCH" {
            let encoder = JSONEncoder()
            request.httpBody = try encoder.encode(body)
            request.setValue("application/json; charset=utf-8", forHTTPHeaderField: "Content-Type")
        }
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        return request
    }

    private func perform(_ request: URLRequest) async throws -> (Data, URLResponse) {
        do {
            return try await session.data(for: request)
        } catch {
            throw ErisError.transport(error.localizedDescription)
        }
    }

    private func openStream(_ request: URLRequest) async throws -> (URLSession.AsyncBytes, Int) {
        do {
            let (bytes, response) = try await session.bytes(for: request)
            return (bytes, Self.status(of: response))
        } catch {
            throw ErisError.transport(error.localizedDescription)
        }
    }

    /// Read a bounded body, so an error response can be decoded instead of
    /// leaving the caller with a bare status code.
    private func collect(bytes: URLSession.AsyncBytes, limit: Int) async -> Data {
        var data = Data()
        do {
            for try await byte in bytes {
                data.append(byte)
                if data.count >= limit { break }
            }
        } catch {
            return data
        }
        return data
    }

    // MARK: Wire helpers

    private func decode<T: Decodable>(_ type: T.Type, from data: Data) throws -> T {
        do {
            return try JSONDecoder().decode(type, from: data)
        } catch let error as ErisError {
            throw error
        } catch {
            throw ErisError.decoding("\(T.self) did not decode: \(error.localizedDescription)")
        }
    }

    private static func status(of response: URLResponse) -> Int {
        (response as? HTTPURLResponse)?.statusCode ?? 0
    }

    /// Turn a non-2xx body into the daemon's typed refusal.
    ///
    /// The device surface always answers `{"ok":false,"code":"...","message":
    /// "..."}`; a proxy or a wrong host may answer HTML instead, so the raw text
    /// is kept (truncated) rather than dropped.
    static func serverError(status: Int, data: Data) -> ErisError {
        if let value = try? JSONSerialization.jsonObject(with: data),
           let object = value as? [String: Any] {
            return .server(
                status: status,
                code: object["code"] as? String,
                message: object["message"] as? String
            )
        }
        let text = String(decoding: data, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return .server(
            status: status,
            code: nil,
            message: text.isEmpty ? nil : String(text.prefix(200))
        )
    }

    /// Form-encode fields, sorted so the body is deterministic.
    static func formEncode(_ fields: [String: String]) -> String {
        fields.keys.sorted().map { key in
            "\(escapeForm(key))=\(escapeForm(fields[key] ?? ""))"
        }.joined(separator: "&")
    }

    /// Percent-encode one form component (`x-www-form-urlencoded`).
    static func escapeForm(_ value: String) -> String {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        return value.addingPercentEncoding(withAllowedCharacters: allowed) ?? value
    }
}
