import Foundation

/// Native DeepSeek Harness client.
///
/// Talks to the DSH phone API (`dsh-phone-api`) directly over HTTP + SSE:
///
///     POST /phone/v1/session   create a Session (agent preset)
///     POST /phone/v1/prompt    admit a prompt
///     GET  /phone/v1/follow    SSE stream of SessionFollowFrame
///     GET  /phone/v1/sessions  list
///     GET  /phone/v1/page      history page
///     POST /phone/v1/cancel    cancel the active turn
///
/// This replaces the Herald relay + connector for the DSH path: the app speaks
/// to DSH itself, and nothing in the request path touches Hermes.
///
/// Streaming order matters. The follow stream is opened and its opening snapshot
/// consumed BEFORE the prompt is admitted, so the snapshot is the pre-turn state
/// and every later frame belongs to our turn. Prompting first would race the
/// snapshot against the reply and could replay old history as live deltas.
@MainActor
final class DSHClient: HeraldClientProtocol {

    // MARK: - Transport

    /// Base URL of the DSH phone API, e.g.
    /// `https://host.ts.net/dsh` — the `/phone/v1/...` routes are appended.
    private let baseURLProvider: @MainActor () -> String
    private let token: String

    private var session: URLSession
    private let decoder = JSONDecoder()

    // MARK: - Protocol state

    var connectionStatus: ConnectionStatus = .disconnected
    private(set) var currentConversation: Conversation?

    /// Local conversation UUID -> DSH sessionId. DSH mints its own ids
    /// (`session-<uuid>`), so the app's stable local UUID is the map key.
    private var nativeIdByConversation: [UUID: String] = [:]
    private var conversationByNativeId: [String: UUID] = [:]

    /// In-flight turns, keyed by the local job UUID the UI sees.
    private var activeStreams: [UUID: Task<Void, Never>] = [:]
    private var currentJobID: UUID?
    private var pendingStreamTask: Task<Void, Never>?
    /// Model picked before a session exists; applied when the session is created.
    private var pendingModelSelection: (provider: String, model: String)?
    private var pendingClarify: PendingClarify?

    /// Token usage reported by the most recent turn.
    private var lastUsage: TokenUsage?

    var supportsServerTurnInterrupt: Bool { true }
    var deliversAttachmentsInline: Bool { false }

    // MARK: - Init

    init(
        baseURLProvider: @escaping @MainActor () -> String,
        token: String,
        secureStore: Any? = nil
    ) {
        self.baseURLProvider = baseURLProvider
        self.token = token
        let cfg = URLSessionConfiguration.default
        cfg.timeoutIntervalForRequest = 60
        cfg.timeoutIntervalForResource = 3600
        cfg.waitsForConnectivity = true
        self.session = URLSession(configuration: cfg)
    }

    // MARK: - Session identity (fix B)

    /// DSH session id for a local conversation. Kallisti mints DSH sessions
    /// with an explicit id derived from the conversation UUID
    /// (`session-<uuid>`), so the mapping is a pure function and survives an
    /// app relaunch, a session-list reopen, or a DSH restart. The dictionary is
    /// only a cache; sessions minted elsewhere (web GUI, older builds) map back
    /// through `stableUUID(from:)`, which inverts this for every DSH id.
    static func sessionId(for conversationID: UUID) -> String {
        "session-\(conversationID.uuidString.lowercased())"
    }

    /// Resolvable DSH id for a conversation, without a network call. The
    /// deterministic id is the fallback, so a relaunch or a list reopen never
    /// loses the session - the old nil here is what forked a new session per
    /// resume and dropped the chat's history.
    private func nativeId(for conversationID: UUID) -> String? {
        nativeIdByConversation[conversationID] ?? Self.sessionId(for: conversationID)
    }

    /// Resolve, and if needed create/resume, the DSH session for a
    /// conversation. Explicit-id `session.create` adopts a live session,
    /// resumes a persisted one, or creates it - so this is idempotent.
    private var ensuredSessions: Set<String> = []

    private func ensureSession(for conversationID: UUID) async throws -> String {
        let sid = nativeIdByConversation[conversationID] ?? Self.sessionId(for: conversationID)
        if ensuredSessions.contains(sid) { return sid }
        let body: [String: Any] = ["agentPreset": "ignyte", "sessionId": sid]
        let created = try decoder.decode(CreateBody.self, from: try await sendWithBody("session", body))
        remember(conversationID, created.sessionId)
        ensuredSessions.insert(created.sessionId)
        if let pick = pendingModelSelection {
            // Best effort: a failed apply leaves the host default, which the
            // picker then reports truthfully on its next load.
            _ = try? await sendWithBody("model", ["sessionId": created.sessionId, "provider": pick.provider, "model": pick.model])
            pendingModelSelection = nil
        }
        return created.sessionId
    }

    private func remember(_ conversationID: UUID, _ sessionId: String) {
        nativeIdByConversation[conversationID] = sessionId
        conversationByNativeId[sessionId] = conversationID
    }

    // MARK: - URL helpers

    private var base: String {
        baseURLProvider().trimmingCharacters(in: CharacterSet(charactersIn: "/"))
    }

    private func url(_ path: String) -> URL? {
        URL(string: "\(base)/phone/v1/\(path)")
    }

    private func request(_ path: String, method: String = "GET") -> URLRequest? {
        guard let u = url(path) else { return nil }
        var r = URLRequest(url: u)
        r.httpMethod = method
        r.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        if method == "POST" { r.setValue("application/json", forHTTPHeaderField: "Content-Type") }
        return r
    }

    private struct ServerError: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    @discardableResult
    private func send(_ req: URLRequest?) async throws -> Data {
        guard let req else { throw ServerError(message: "Invalid DSH base URL: \(base)") }
        let (data, resp) = try await session.data(for: req)
        guard let http = resp as? HTTPURLResponse else {
            throw ServerError(message: "No HTTP response from DSH")
        }
        guard (200..<300).contains(http.statusCode) else {
            struct ErrBody: Decodable { let error: String }
            if let e = try? decoder.decode(ErrBody.self, from: data) {
                throw ServerError(message: http.statusCode == 422 ? e.error : "DSH \(http.statusCode): \(e.error)")
            }
            let body = String(data: data, encoding: .utf8) ?? ""
            throw ServerError(message: "DSH \(http.statusCode): \(body.prefix(200))")
        }
        return data
    }

    // MARK: - Wire types

    private struct HealthBody: Decodable { let ok: Bool; let backend: String? }
    private struct CreateBody: Decodable { let sessionId: String; let agentPreset: String? }
    private struct PromptBody: Decodable { let accepted: Bool; let requestId: String? }
    private struct SessionRow: Decodable {
        let sessionId: String
        let updatedAt: Double?
        let running: Bool?
        let blank: Bool?
        let cwd: String?
        let projections: Projections?
    }
    /// DSH publishes per-session projections; `title` is the model-generated
    /// session name and `contextPressure` carries the window size. Reading the
    /// title here is what stops the list showing bare workspace folder names.
    private struct Projections: Decodable {
        let values: Values?
        struct Values: Decodable {
            let title: String?
            let contextPressure: ContextPressure?
        }
        struct ContextPressure: Decodable {
            let projectedTokens: Double?
            let contextWindow: Double?
        }
    }
    private struct SessionListBody: Decodable { let items: [SessionRow] }
    private struct PageBody: Decodable { let hasMore: Bool; let records: [JSONValue] }
    private struct CancelBody: Decodable { let accepted: Bool }

    /// Minimal dynamic JSON so we can walk DSH's frame union without modelling
    /// every variant up front.
    enum JSONValue: Decodable {
        case string(String), number(Double), bool(Bool), null
        case array([JSONValue])
        case object([String: JSONValue])

        init(from decoder: Decoder) throws {
            let c = try decoder.singleValueContainer()
            if c.decodeNil() { self = .null; return }
            if let v = try? c.decode(Bool.self) { self = .bool(v); return }
            if let v = try? c.decode(Double.self) { self = .number(v); return }
            if let v = try? c.decode(String.self) { self = .string(v); return }
            if let v = try? c.decode([JSONValue].self) { self = .array(v); return }
            self = .object((try? c.decode([String: JSONValue].self)) ?? [:])
        }

        subscript(key: String) -> JSONValue? {
            if case .object(let o) = self { return o[key] }
            return nil
        }
        var stringValue: String? { if case .string(let s) = self { return s }; return nil }
        var doubleValue: Double? { if case .number(let n) = self { return n }; return nil }
        var boolValue: Bool? { if case .bool(let b) = self { return b }; return nil }
        var arrayValue: [JSONValue]? { if case .array(let a) = self { return a }; return nil }
        var objectValue: [String: JSONValue]? { if case .object(let o) = self { return o }; return nil }
    }

    // MARK: - Connection

    func connect() async {
        connectionStatus = .connecting
        // Retry the health probe for a bounded window instead of giving up
        // after a single attempt. A one-shot ping raced the phone's network
        // bring-up: the probe failed, the status latched at .disconnected, and
        // nothing ever re-probed - so the composer stayed read-only (no
        // keyboard) on a harness that was actually healthy and reachable the
        // whole time. ChatScreen's poll re-reads this value, so a later
        // success still opens the composer without needing a relaunch.
        var lastError: Error?
        for attempt in 0..<5 {
            if Task.isCancelled { return }
            do {
                let req = request("health")
                let data = try await send(req)
                let body = try decoder.decode(HealthBody.self, from: data)
                if body.ok {
                    connectionStatus = .connected
                    return
                }
                lastError = ServerError(message: "DSH health reported not ok")
            } catch {
                lastError = error
            }
            // 0.4s, 0.8s, 1.6s, 3.2s - stays well inside a normal launch.
            let delay = UInt64(400_000_000 * (1 << attempt))
            try? await Task.sleep(nanoseconds: delay)
        }
        _ = lastError
        connectionStatus = .disconnected
    }

    func disconnect() async {
        for (_, task) in activeStreams { task.cancel() }
        activeStreams.removeAll()
        connectionStatus = .disconnected
    }

    /// Host row for Settings → Infrastructure in DSH mode.
    ///
    /// Without this the store falls through to the relay path and reports the
    /// Hermes gateway's version and model even though the app is talking to
    /// DSH — the row would name a backend that is not serving the request.
    func hostStatus() async -> HeraldHostStatus? {
        do {
            _ = try await send(request("health"))
            let models = try? await fetchModelCatalog()
            return HeraldHostStatus(
                id: UUID(uuidString: "00000000-0000-0000-0000-000000000002") ?? UUID(),
                displayName: "DeepSeek Harness",
                hostname: nil,
                platform: "dsh",
                connectorVersion: "dsh-phone-api",
                heraldCommand: nil,
                heraldVersion: "DeepSeek Harness",
                heraldModel: models?.defaultModel ?? models?.defaultProvider,
                lastSeenAt: .now,
                lastConnectedAt: .now,
                isOnline: true
            )
        } catch {
            return nil
        }
    }

    // MARK: - Host restart (Settings > Infrastructure)

    struct RestartResult: Sendable {
        let interruptedSessions: Int
        let seconds: Int
    }

    private struct HealthDetail: Decodable { let ok: Bool; let startedAt: Double?; let pid: Int? }
    private struct RestartAck: Decodable { let restarting: Bool; let interruptedSessions: Int?; let startedAt: Double? }

    /// Restart DSH through its LaunchAgent (`launchctl kickstart -k`) and
    /// wait until a NEW process answers. Proof is a changed `startedAt`, not
    /// a 200 - the old process can answer health until it is killed.
    func restartHost(timeout: TimeInterval = 90) async throws -> RestartResult {
        let t0 = Date()
        var before: Double?
        if let d = try? await send(request("health")),
           let h = try? decoder.decode(HealthDetail.self, from: d) { before = h.startedAt }
        let ack = try decoder.decode(RestartAck.self, from: try await sendWithBody("restart", [:]))
        guard ack.restarting else { throw ServerError(message: "DSH declined the restart") }
        before = before ?? ack.startedAt
        connectionStatus = .reconnecting
        try await Task.sleep(nanoseconds: 2_000_000_000)
        while Date().timeIntervalSince(t0) < timeout {
            if let d = try? await send(request("health")),
               let h = try? decoder.decode(HealthDetail.self, from: d), h.ok,
               h.startedAt != nil, h.startedAt != before {
                connectionStatus = .connected
                return RestartResult(interruptedSessions: ack.interruptedSessions ?? 0,
                                     seconds: Int(Date().timeIntervalSince(t0)))
            }
            try await Task.sleep(nanoseconds: 1_500_000_000)
        }
        await reconnectIfNeeded()
        throw ServerError(message: "DSH did not come back within \(Int(timeout))s. On the Mac run: dsh-restart")
    }

    /// Sessions with a turn running right now, for the restart confirmation.
    func runningSessionCount() async -> Int {
        guard let data = try? await send(request("sessions")),
              let body = try? decoder.decode(SessionListBody.self, from: data) else { return 0 }
        return body.items.filter { $0.running == true }.count
    }

    private struct ModelCatalogBody: Decodable {
        let defaultProvider: String?
        let defaultModel: String?
        let groups: [Group]?
        struct Group: Decodable {
            let id: String
            let name: String?
            let models: [Entry]?
        }
        struct Entry: Decodable {
            let id: String
            let name: String?
        }
    }

    /// One pickable model row: `provider` is the DSH provider route id.
    struct ModelRow: Sendable {
        let provider: String
        let providerName: String
        let model: String
    }

    /// DSH model catalog plus the selection that the NEXT turn will use:
    /// the current session's pending/last selection, else the host default.
    func modelCatalog() async throws -> (models: [ModelRow], activeProvider: String?, activeModel: String?) {
        let cat = try await fetchModelCatalog()
        let rows = (cat.groups ?? []).flatMap { g in
            (g.models ?? []).map { ModelRow(provider: g.id, providerName: g.name ?? g.id, model: $0.id) }
        }
        if let pending = pendingModelSelection {
            return (rows, pending.provider, pending.model)
        }
        if let conv = currentConversation, let native = nativeId(for: conv.id),
           let sel = try? await sessionModelSelection(sessionId: native) {
            return (rows, sel.provider, sel.model)
        }
        return (rows, cat.defaultProvider, cat.defaultModel)
    }

    /// Selects the model for the current session. Before the first send there
    /// is no DSH session, so the choice is held and applied at creation.
    func selectModel(provider: String, model: String) async throws {
        if let conv = currentConversation {
            // Deterministic ids mean the session can be created right here,
            // so the pick applies to THIS chat instead of waiting on a send.
            pendingModelSelection = nil
            let native = try await ensureSession(for: conv.id)
            _ = try await sendWithBody("model", ["sessionId": native, "provider": provider, "model": model])
        } else {
            pendingModelSelection = (provider, model)
        }
    }

    private struct SelectionRow: Decodable {
        let sessionId: String
        let projections: SelectionProjections?
        struct SelectionProjections: Decodable {
            let values: Values?
            struct Values: Decodable { let modelSelection: ModelSelectionValue? }
        }
        struct ModelSelectionValue: Decodable {
            let next: Pick?
            let lastUsed: Pick?
        }
        struct Pick: Decodable { let provider: String; let model: String }
    }

    private func sessionModelSelection(sessionId: String) async throws -> (provider: String, model: String)? {
        struct Body: Decodable { let items: [SelectionRow] }
        let body = try decoder.decode(Body.self, from: try await send(request("sessions")))
        guard let row = body.items.first(where: { $0.sessionId == sessionId }),
              let sel = row.projections?.values?.modelSelection,
              let pick = sel.next ?? sel.lastUsed else { return nil }
        return (pick.provider, pick.model)
    }

    /// One skill row as the Skills browser needs it.
    struct SkillRow: Sendable {
        let name: String
        let description: String
        let path: String
    }

    private struct SkillListBody: Decodable {
        let skills: [Entry]
        struct Entry: Decodable {
            let name: String
            let description: String?
            let path: String?
        }
    }

    private struct SkillDetailBody: Decodable {
        let name: String
        let description: String?
        let path: String?
        let content: String?
    }

    /// Skill catalog for this harness. The host lists skills per Session scope,
    /// so the endpoint resolves the preset's layer rather than the global one.
    func listSkills() async throws -> [SkillRow] {
        let data = try await send(request("skills"))
        let body = try decoder.decode(SkillListBody.self, from: data)
        return body.skills.map { SkillRow(name: $0.name, description: $0.description ?? "", path: $0.path ?? "") }
    }

    func skillDetail(name: String) async throws -> (name: String, description: String, path: String, content: String) {
        let encoded = name.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? name
        let data = try await send(request("skill?name=\(encoded)"))
        let body = try decoder.decode(SkillDetailBody.self, from: data)
        return (body.name, body.description ?? "", body.path ?? "", body.content ?? "")
    }

    // MARK: - Config (DSH profile patch layer)

    struct ConfigDocument: Decodable, Sendable {
        let path: String
        let size: Int?
        let content: String
    }

    struct ConfigSaveResult: Decodable, Sendable {
        let ok: Bool?
        let path: String?
        let backup: String?
    }

    /// DSH's editable config: `~/.dsh/profiles/web/cordis.patch.yml`, the
    /// layer DSH's own Settings writes. DSH reloads it live on save.
    func configDocument() async throws -> ConfigDocument {
        try decoder.decode(ConfigDocument.self, from: try await send(request("config")))
    }

    func validateConfigDocument(_ content: String) async throws {
        _ = try await sendWithBody("config/validate", ["content": content])
    }

    func saveConfigDocument(_ content: String) async throws -> ConfigSaveResult {
        guard var req = request("config", method: "PUT") else {
            throw ServerError(message: "Invalid DSH base URL")
        }
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = try JSONSerialization.data(withJSONObject: ["content": content])
        return try decoder.decode(ConfigSaveResult.self, from: try await send(req))
    }

    private func fetchModelCatalog() async throws -> ModelCatalogBody {
        let data = try await send(request("models"))
        return try decoder.decode(ModelCatalogBody.self, from: data)
    }

    /// Round-trip time of an authenticated health probe, for Settings >
    /// Connection. Nil when the harness is unreachable.
    func measureLatency() async -> Int? {
        let started = Date()
        do {
            _ = try await send(request("health"))
            return max(Int(Date().timeIntervalSince(started) * 1000), 0)
        } catch {
            return nil
        }
    }

    func reconnectIfNeeded() async {
        do {
            _ = try await send(request("health"))
            connectionStatus = .connected
        } catch {
            connectionStatus = .reconnecting
            await connect()
        }
    }

    func resumeActiveSessionIfNeeded() async -> Bool {
        guard let job = currentJobID else { return false }
        if activeStreams[job] != nil { return true }
        // The local stream is gone (suspension) but the server may still be
        // on it: ask instead of assuming, so the watchdog does not fail a live
        // turn and trigger a resend.
        guard let status = await fetchJob(clientMessageID: job) else { return false }
        return status.status == "running" || status.status == "queued"
    }

    func activeSessionKeys() async -> Set<String> {
        guard let conv = currentConversation,
              let native = nativeId(for: conv.id),
              currentJobID != nil else { return [] }
        return [native]
    }

    // MARK: - Job status (fix A)

    /// Wire shape of `GET /phone/v1/job`.
    struct JobWire: Decodable {
        let status: String
        let sessionId: String?
        let turn: Int?
        let text: String?
        let messageId: String?
        let error: String?
        let errorCode: String?
        let usage: UsageWire?
        struct UsageWire: Decodable {
            let inputTokens: Double?
            let outputTokens: Double?
            let totalTokens: Double?
        }
    }

    /// Authoritative state of one prompt. On DSH the job id IS the
    /// clientMessageID (see `runTurn`), so a job can be resolved after a
    /// relaunch or DSH restart with nothing but the outbox record.
    /// Nil only when the host cannot be reached or has never seen the prompt.
    func fetchJob(clientMessageID: UUID) async -> JobWire? {
        var path = "job?clientMessageId=\(clientMessageID.uuidString.lowercased())"
        if let conv = currentConversation, let sid = nativeId(for: conv.id),
           let enc = sid.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) {
            path += "&sessionId=\(enc)"
        }
        guard let data = try? await send(request(path)) else { return nil }
        return try? decoder.decode(JobWire.self, from: data)
    }

    /// Maps DSH job states onto the relay vocabulary ChatStore settles on.
    /// `interrupted` (host restart / crash mid-turn) maps to `cancelled`, NOT
    /// `failed`: ChatStore auto-resends `failed` on a backoff, and resending a
    /// turn the host killed is exactly the duplicate-reply storm this fixes.
    /// The user gets a manual Retry instead.
    func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? {
        guard let job = await fetchJob(clientMessageID: jobId) else { return nil }
        let mapped: String
        var error = job.error
        switch job.status {
        case "completed": mapped = "completed"
        case "failed": mapped = "failed"
        case "cancelled": mapped = "cancelled"
        case "interrupted":
            mapped = "cancelled"
            error = "Interrupted: the host restarted mid-turn. Tap retry to run it again."
        default: mapped = "running"
        }
        var message: Message?
        if mapped == "completed" {
            message = Message(
                id: job.messageId.flatMap(UUID.init(uuidString:)) ?? UUID(),
                clientMessageID: jobId,
                sender: .herald,
                content: job.text ?? "",
                status: .sent
            )
        }
        let usage = job.usage.flatMap { u -> TokenUsage? in
            guard let i = u.inputTokens, let o = u.outputTokens else { return nil }
            return TokenUsage(promptTokens: Int(i), completionTokens: Int(o), totalTokens: Int(u.totalTokens ?? i + o))
        }
        return LiveHeraldClient.JobStatusResponse(
            status: mapped,
            conversationId: job.sessionId.map(Self.stableUUID(from:)),
            message: message,
            error: error,
            usage: usage,
            context: nil,
            diff: nil,
            attempt: nil,
            lastSeq: nil,
            errorCategory: mapped == "failed" ? Self.errorCategory(code: job.errorCode, message: job.error ?? "") : nil,
            errorAction: mapped == "failed" ? ChatStore.serverTerminalFailureAction : nil
        )
    }

    func isServerTurnAwaitingUserInput() async -> Bool { pendingClarify != nil }

    func fetchPendingClarify() async -> PendingClarify? {
        // Live event path already surfaced it.
        if let pending = pendingClarify { return pending }
        // The follow stream can miss a tool/call while reconnecting; the phone
        // API persists pending questions server-side, so poll it on watchdog
        // probes and app foregrounding to re-surface the card.
        guard let conv = currentConversation,
              let native = nativeId(for: conv.id) else { return nil }
        guard let req = request("questions?sessionId=\(native.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? native)") else { return nil }
        do {
            let data = try await send(req)
            guard let dict = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let questions = dict["questions"] as? [[String: Any]] else { return nil }
            // Keep it so the answer is matched against the real options.
            let recovered = Self.pendingClarify(fromQuestions: questions)
            if let recovered { pendingClarify = recovered }
            return recovered
        } catch {
            return nil
        }
    }

    func respondToClarify(requestID: String, answer: String) async throws {
        // DSH ask_user_question answers are submitted through the phone API's
        // /answer endpoint so the result feeds back as the tool result, not as
        // a steer prompt.
        guard let conv = currentConversation, let native = nativeId(for: conv.id) else {
            throw ServerError(message: "No active session to answer on")
        }
        try await submitAnswer(sessionId: native, questionID: requestID, answer: answer)
        pendingClarify = nil
    }

    // MARK: - Conversation

    func loadConversation() async -> Conversation {
        if let c = currentConversation { return c }
        let c = Conversation(title: "New chat")
        currentConversation = c
        return c
    }

    func loadConversation(id: UUID) async throws -> Conversation {
        guard let native = nativeId(for: id) else { return Conversation(id: id, title: "Chat") }
        let page: PageBody
        do {
            page = try await fetchPage(sessionId: native, maxMessages: 200)
        } catch let e as ServerError where e.message.contains("not found") {
            // A brand-new chat whose session is created on first send.
            return Conversation(id: id, title: currentConversation?.title ?? "Chat", sessionKey: native)
        }
        var messages: [Message] = []
        var model: String?
        for record in page.records {
            if let m = Self.modelName(fromEvent: record["event"]) { model = m }
            if let m = Self.message(fromWireEvent: record, model: model) { messages.append(m) }
        }
        remember(id, native)
        let conv = Conversation(
            id: id,
            title: currentConversation?.id == id ? (currentConversation?.title ?? "Chat") : "Chat",
            messages: messages,
            lastActivity: .now,
            latestUsage: lastUsage,
            contextPercent: nil,
            sessionKey: native
        )
        currentConversation = conv
        return conv
    }

    func clearConversation() async throws -> Conversation {
        currentJobID = nil
        let c = Conversation(title: "New chat")
        currentConversation = c
        return c
    }

    func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation {
        await loadConversation()
    }

    func adoptConversation(id: UUID, title: String) {
        currentConversation = Conversation(id: id, title: title)
    }

    func ensureConversation(id: UUID) async -> Bool {
        // Create-or-resume the deterministic session. The old "mapping known
        // -> true" short cut returned false after every relaunch, and the next
        // send then minted a random new session: history gone, context gone.
        (try? await ensureSession(for: id)) != nil
    }

    // MARK: - Sessions

    func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse {
        let data = try await send(request("sessions"))
        let body = try decoder.decode(SessionListBody.self, from: data)
        let summaries = body.items.map { row -> SessionSummary in
            let local = conversationByNativeId[row.sessionId] ?? Self.stableUUID(from: row.sessionId)
            // Record both directions. Only the reverse map was filled, so a
            // chat opened from this list had no DSH id and forked a new one.
            remember(local, row.sessionId)
            // Prefer DSH's model-generated title; the workspace folder name is
            // only a last resort for a session that has not been named yet.
            let title = row.projections?.values?.title?.trimmingCharacters(in: .whitespacesAndNewlines)
            let resolvedTitle: String = {
                if let t = title, !t.isEmpty { return t }
                if let cwd = row.cwd, !cwd.isEmpty { return (cwd as NSString).lastPathComponent }
                return "Chat"
            }()
            return SessionSummary(
                id: local,
                title: resolvedTitle,
                previewText: "",
                lastActivity: row.updatedAt.map { Date(timeIntervalSince1970: $0 / 1000) } ?? .now,
                source: "dsh",
                isPinned: false,
                isArchived: false,
                sessionKey: row.sessionId,
                hasActivity: row.running ?? false
            )
        }
        return SessionListResponse(sessions: Array(summaries.dropFirst(offset).prefix(limit)), total: summaries.count)
    }

    func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] {
        let all = try await listSessions(limit: 500, offset: 0, allDevices: allDevices)
        guard !query.isEmpty else { return all.sessions }
        return all.sessions.filter { $0.title.localizedCaseInsensitiveContains(query) }
    }

    func createSession(title: String) async throws -> SessionSummary {
        try await createSession(title: title, conversationID: nil)
    }

    func createSession(title: String, conversationID: UUID?) async throws -> SessionSummary {
        let local = conversationID ?? UUID()
        let sid = try await ensureSession(for: local)
        currentConversation = Conversation(id: local, title: title, sessionKey: sid)
        return SessionSummary(
            id: local,
            title: title,
            previewText: "",
            lastActivity: .now,
            source: "dsh",
            isPinned: false,
            isArchived: false,
            sessionKey: sid,
            hasActivity: false
        )
    }

    func deleteSession(id: UUID) async throws {
        nativeIdByConversation.removeValue(forKey: id)
        if currentConversation?.id == id { currentConversation = nil }
    }

    func deleteNoteSession(conversationID: UUID, gatewaySessionKey: String?) async throws {
        nativeIdByConversation.removeValue(forKey: conversationID)
    }

    func archiveSession(id: UUID) async throws {}
    func togglePinSession(id: UUID) async throws -> SessionSummary {
        throw ServerError(message: "Pinning is not supported on the DSH transport")
    }

    func renameSession(id: UUID, title: String) async throws -> SessionSummary {
        if let conv = currentConversation, conv.id == id {
            currentConversation?.title = title
        }
        return SessionSummary(
            id: id, title: title, previewText: "", lastActivity: .now,
            source: "dsh", isPinned: false, isArchived: false,
            sessionKey: nativeId(for: id), hasActivity: false
        )
    }

    func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String {
        // DSH titles sessions itself; derive a short label from the prompt.
        let trimmed = userMessage.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "Chat" : String(trimmed.prefix(40))
    }

    func generateCreativeTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String {
        try await generateSessionTitle(sessionId: sessionId, userMessage: userMessage, assistantMessage: assistantMessage)
    }

    func resumeNoteSession(conversationID: UUID, sessionKey: String) async -> Bool {
        guard !sessionKey.isEmpty else { return false }
        remember(conversationID, sessionKey)
        return true
    }

    func nativeSessionKey(for conversationID: UUID) async -> String? {
        nativeId(for: conversationID)
    }

    // MARK: - Sending

    func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String?) async -> Message {
        var final: Message?
        for await update in sendStreaming(message: message, attachments: attachments, clientMessageID: clientMessageID, continuationContext: continuationContext) {
            if case .finished(let m, _, _, _) = update { final = m }
            if case .failed(let reason, _, _) = update {
                return Message(id: clientMessageID, clientMessageID: clientMessageID,
                               sender: .herald, content: reason, status: .failed)
            }
        }
        return final ?? Message(id: clientMessageID, clientMessageID: clientMessageID,
                                sender: .herald, content: "", status: .sent)
    }

    func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message {
        _ = try await ensureSession(for: conversationID)
        return await send(message: text, attachments: [], clientMessageID: clientMessageID, continuationContext: nil)
    }

    func sendNoteMessage(text: String, attachments: [PendingAttachment], clientMessageID: UUID, conversationID: UUID, title: String) async -> Message {
        await send(message: text, attachments: attachments, clientMessageID: clientMessageID, continuationContext: nil)
    }

    func sendNoteMessageStreaming(text: String, attachments: [PendingAttachment], clientMessageID: UUID, conversationID: UUID, title: String, enrichmentModelName: String?, enrichmentProvider: String?, thinkingAsReasoning: Bool) -> AsyncStream<StreamingUpdate> {
        sendStreaming(message: text, attachments: attachments, clientMessageID: clientMessageID, continuationContext: nil)
    }

    // MARK: - Streaming

    func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String?) -> AsyncStream<StreamingUpdate> {
        AsyncStream { continuation in
            let task = Task { @MainActor in
                await self.runTurn(
                    message: message,
                    attachments: attachments,
                    clientMessageID: clientMessageID,
                    continuation: continuation
                )
            }
            pendingStreamTask = task
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    private func runTurn(
        message: String,
        attachments: [PendingAttachment],
        clientMessageID: UUID,
        continuation: AsyncStream<StreamingUpdate>.Continuation
    ) async {
        // The job id IS the clientMessageID. A resend of the same message
        // (outbox retry, relaunch recovery) therefore carries the same id,
        // the phone API maps it to the same DSH prompt requestId, and DSH
        // refuses to run it twice. It also lets getJobStatus(jobID) resolve
        // the job from nothing but the outbox record.
        let jobID = clientMessageID
        currentJobID = jobID
        if let t = pendingStreamTask { activeStreams[jobID] = t; pendingStreamTask = nil }
        continuation.yield(.messageSent(jobID: jobID))
        continuation.yield(.started(phase: "thinking"))

        do {
            // 1. Resolve the session. Deterministic id: create-or-resume.
            let conversationID = currentConversation?.id ?? UUID()
            if currentConversation == nil {
                currentConversation = Conversation(id: conversationID, title: "Chat")
            }
            let sessionId = try await ensureSession(for: conversationID)

            // 2. Open follow FIRST so its opening snapshot is the pre-turn
            //    state. The phone API withholds headers until the snapshot is
            //    in hand, so returning here means the snapshot exists.
            var stream = try await openFollow(sessionId: sessionId)

            // 3. Admit the prompt. A duplicate (this message already queued,
            //    running, or answered) is attached to, never re-run.
            let admitted = try await prompt(
                sessionId: sessionId, text: message, mode: "queue",
                attachments: attachments, clientMessageID: clientMessageID
            )
            var targetSession = sessionId
            if admitted.duplicate, let other = admitted.sessionId, other != sessionId {
                // Pre-135.87 builds put this message in another session.
                // Follow it there instead of running it again here.
                targetSession = other
                stream = try await openFollow(sessionId: other)
            }
            let requestId = admitted.requestId ?? "kallisti-\(clientMessageID.uuidString.lowercased())"

            // 4. Consume frames until OUR turn ends. A dropped stream (DSH
            //    restart, network handoff) reconnects and re-derives state
            //    from the snapshot instead of failing the turn.
            var attempt = 0
            var knownTurn: Int?
            while true {
                let outcome = try await consume(
                    stream: stream,
                    sessionId: targetSession,
                    requestId: requestId,
                    knownTurn: knownTurn,
                    clientMessageID: clientMessageID,
                    continuation: continuation
                )
                if outcome == .done { break }
                attempt += 1
                if Task.isCancelled { throw CancellationError() }
                // Ask the host what happened before reconnecting blind.
                if let job = await fetchJob(clientMessageID: clientMessageID) {
                    if finishFromJob(job, clientMessageID: clientMessageID, continuation: continuation) { break }
                    knownTurn = job.turn ?? knownTurn
                }
                if attempt > 6 {
                    continuation.yield(.failed("Lost the DSH stream. The turn may still finish; pull to refresh."))
                    continuation.finish()
                    break
                }
                continuation.yield(.reconnecting)
                try await Task.sleep(nanoseconds: UInt64(min(8, 1 << attempt)) * 500_000_000)
                await reconnectIfNeeded()
                guard let reopened = try? await openFollow(sessionId: targetSession) else { continue }
                stream = reopened
            }
        } catch is CancellationError {
            continuation.yield(.cancelled)
            continuation.finish()
        } catch {
            continuation.yield(.failed(error.localizedDescription))
            continuation.finish()
        }
        activeStreams[jobID] = nil
        if currentJobID == jobID { currentJobID = nil }
    }

    /// Settle a turn from `GET /job` when the stream cannot. Returns true
    /// when the job is terminal and the continuation has been finished.
    private func finishFromJob(
        _ job: JobWire,
        clientMessageID: UUID,
        continuation: AsyncStream<StreamingUpdate>.Continuation
    ) -> Bool {
        switch job.status {
        case "completed":
            let final = Message(id: UUID(), clientMessageID: clientMessageID, sender: .herald,
                                content: job.text ?? "(no content)", status: .sent)
            if var conv = currentConversation {
                conv.messages.append(final)
                conv.lastActivity = .now
                currentConversation = conv
            }
            continuation.yield(.finished(final, nil, nil, nil))
            continuation.finish()
            return true
        case "failed":
            continuation.yield(.failed(
                Self.readableTurnError(job.error ?? "DSH turn failed", model: nil),
                category: Self.errorCategory(code: job.errorCode, message: job.error ?? ""),
                action: ChatStore.serverTerminalFailureAction
            ))
            continuation.finish()
            return true
        case "cancelled":
            continuation.yield(.cancelled)
            continuation.finish()
            return true
        case "interrupted":
            // Terminal, and deliberately NOT auto-retried.
            continuation.yield(.failed(
                "Interrupted: the host restarted mid-turn. Tap retry to run it again.",
                category: "upstream_interrupted",
                action: ChatStore.serverTerminalFailureAction
            ))
            continuation.finish()
            return true
        default:
            return false
        }
    }

    private struct PromptAdmission: Decodable {
        let accepted: Bool
        let duplicate: Bool
        let requestId: String?
        let sessionId: String?
        init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            accepted = (try? c.decode(Bool.self, forKey: .accepted)) ?? true
            duplicate = (try? c.decode(Bool.self, forKey: .duplicate)) ?? false
            requestId = try? c.decode(String.self, forKey: .requestId)
            sessionId = try? c.decode(String.self, forKey: .sessionId)
        }
        enum CodingKeys: String, CodingKey { case accepted, duplicate, requestId, sessionId }
    }

    @discardableResult
    private func prompt(sessionId: String, text: String, mode: String, attachments: [PendingAttachment] = [], clientMessageID: UUID? = nil) async throws -> PromptAdmission {
        guard var req = request("prompt", method: "POST") else {
            throw ServerError(message: "Invalid DSH base URL")
        }
        let images: [[String: Any]] = attachments.compactMap { attachment in
            guard attachment.kind == .image else { return nil }
            return [
                "data": attachment.base64Data,
                "mimeType": attachment.mimeType,
            ]
        }
        guard images.count == attachments.count else {
            throw ServerError(message: "The DSH transport currently supports image attachments only")
        }
        var body: [String: Any] = [
            "sessionId": sessionId,
            "text": text,
            "mode": mode,
            "images": images,
        ]
        if let clientMessageID { body["clientMessageId"] = clientMessageID.uuidString.lowercased() }
        req.httpBody = try JSONSerialization.data(withJSONObject: body)
        return try decoder.decode(PromptAdmission.self, from: try await send(req))
    }

    /// Opens the SSE follow stream and returns the byte stream, after the HTTP
    /// response headers are known good.
    private func openFollow(sessionId: String) async throws -> URLSession.AsyncBytes {
        guard let u = url("follow?sessionId=\(sessionId.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? sessionId)") else {
            throw ServerError(message: "Invalid DSH base URL")
        }
        var req = URLRequest(url: u)
        req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        req.setValue("text/event-stream", forHTTPHeaderField: "Accept")
        req.timeoutInterval = 3600
        let (bytes, resp) = try await session.bytes(for: req)
        guard let http = resp as? HTTPURLResponse, (200..<300).contains(http.statusCode) else {
            let code = (resp as? HTTPURLResponse)?.statusCode ?? -1
            throw ServerError(message: "DSH follow failed (HTTP \(code))")
        }
        return bytes
    }

    enum ConsumeOutcome { case done, streamLost }

    /// Consume one follow connection. Returns `.done` once the continuation
    /// is finished, `.streamLost` when the socket ended first (the caller
    /// asks `/job`, then reconnects - never fabricates a reply).
    ///
    /// Our turn is identified by `requestId`: DSH stamps it on the durable
    /// user message as `source.rpcId`, and the turn that message was spliced
    /// into is ours. The old rule - "the first turn/start after the snapshot"
    /// - claimed whatever turn happened to run next (a queued earlier prompt,
    /// a resend), which is how replies landed under the wrong message and a
    /// finished turn left "Thinking..." up.
    private func consume(
        stream: URLSession.AsyncBytes,
        sessionId: String,
        requestId: String,
        knownTurn: Int? = nil,
        clientMessageID: UUID,
        continuation: AsyncStream<StreamingUpdate>.Continuation
    ) async throws -> ConsumeOutcome {
        var textBuffer = ""
        var reasoningBuffer = ""
        var tools: [String: ToolActivity] = [:]
        var usage: TokenUsage?
        var sawSnapshot = false
        // A turn is several model steps: every tool-calling step records its own
        // `assistant/message`. Finishing on the first one ended the turn at the
        // tool call ("2 tools used", no answer) and let the next queued prompt
        // go out while DSH was still working. Keep the latest message with text
        // and finish only on this turn's `turn/end`.
        var ourTurn: Int? = knownTurn
        // Latest turn/start seen, snapshot included. Our user/message lands
        // inside its own turn, so this names our turn when we see it.
        var currentTurn: Int?
        var lastAssistantText: String?
        // Model the turn ran on, for a readable failure message.
        var turnModel: String?

        // Track the highest seq we have seen so the opening snapshot's history
        // is never re-emitted as live deltas.
        var snapshotMaxSeq = -1
        var clarifyCallIDs: Set<String> = []

        do {
        for try await line in stream.lines {
            if Task.isCancelled { throw CancellationError() }
            guard line.hasPrefix("data: ") else { continue }
            let payload = String(line.dropFirst(6))
            guard let data = payload.data(using: .utf8),
                  let frame = try? decoder.decode(JSONValue.self, from: data) else { continue }

            let type = frame["type"]?.stringValue ?? ""

            if type == "snapshot" {
                sawSnapshot = true
                if let records = frame["records"]?.arrayValue {
                    for r in records {
                        guard let ev = r["event"] else { continue }
                        if let seq = ev["seq"]?.doubleValue { snapshotMaxSeq = max(snapshotMaxSeq, Int(seq)) }
                        if let m = Self.modelName(fromEvent: ev) { turnModel = m }
                        // A reconnect lands mid-turn or after it: recover our
                        // turn and anything it already said from history.
                        let t = ev["type"]?.stringValue ?? ""
                        let d = ev["data"]
                        if t == "turn/start", let n = d?["turn"]?.doubleValue { currentTurn = Int(n) }
                        if t == "user/message", d?["source"]?["rpcId"]?.stringValue == requestId { ourTurn = currentTurn }
                        if ourTurn != nil, t == "assistant/message",
                           d?["message"]?["source"]?["kind"]?.stringValue == "model",
                           let txt = Self.text(fromContentBlocks: d?["message"]?["content"]), !txt.isEmpty {
                            lastAssistantText = txt
                        }
                        if let ours = ourTurn, t == "turn/end", let n = d?["turn"]?.doubleValue, Int(n) == ours {
                            // Finished while we were away. Settle from the
                            // snapshot instead of waiting on a turn that ended.
                            let kind = d?["reason"]?["kind"]?.stringValue ?? "completed"
                            if kind == "completed" {
                                let final = Message(id: UUID(), clientMessageID: clientMessageID, sender: .herald,
                                                    content: lastAssistantText ?? "(no content)", status: .sent)
                                if var conv = currentConversation {
                                    conv.messages.append(final); conv.lastActivity = .now; currentConversation = conv
                                }
                                continuation.yield(.finished(final, usage, nil, nil))
                                continuation.finish()
                                return .done
                            }
                            // Error / abort / interrupt: let /job phrase it.
                            return .streamLost
                        }
                    }
                }
                continuation.yield(.heartbeat(phase: "streaming"))
                continue
            }

            if type == "assistant-stream" {
                guard sawSnapshot else { continue }
                // Live chunks carry no turn number. Only render them while
                // the running turn is ours, so a queued earlier prompt's
                // answer is never painted under this message.
                guard let ours = ourTurn, currentTurn == ours else { continue }
                guard let f = frame["frame"] else { continue }
                let kind = f["type"]?.stringValue ?? ""
                if kind == "chunk", let chunk = f["chunk"] {
                    let ck = chunk["type"]?.stringValue ?? ""
                    switch ck {
                    case "text-delta":
                        if let t = chunk["text"]?.stringValue, !t.isEmpty {
                            textBuffer += t
                            continuation.yield(.textDelta(t))
                        }
                    case "reasoning-delta":
                        if let t = chunk["text"]?.stringValue, !t.isEmpty {
                            reasoningBuffer += t
                            continuation.yield(.reasoningDelta(t))
                        }
                    case "tool-call-delta":
                        let id = chunk["id"]?.stringValue ?? UUID().uuidString
                        let name = chunk["name"]?.stringValue
                        // ask_user_question renders as the clarify card from its
                        // tool/call event. Streaming it as a tool row dumped the
                        // raw questions JSON into a stdout block under the card.
                        if name == "ask_user_question" { clarifyCallIDs.insert(id) }
                        if clarifyCallIDs.contains(id) { break }
                        if tools[id] == nil {
                            let a = ToolActivity(label: name ?? "tool", toolCallID: id, name: name)
                            tools[id] = a
                            continuation.yield(.toolStarted(a))
                        }
                        if let args = chunk["argumentsDelta"]?.stringValue, !args.isEmpty {
                            continuation.yield(.toolOutput(toolCallID: id, chunk: args))
                        }
                    case "usage":
                        if let u = chunk["usage"] { usage = Self.usage(from: u) }
                    default:
                        break
                    }
                }
                continue
            }

            if type == "event" {
                guard sawSnapshot, let ev = frame["event"] else { continue }
                let seq = ev["seq"]?.doubleValue.map { Int($0) } ?? -1
                if seq >= 0 && seq <= snapshotMaxSeq { continue }
                let evType = ev["type"]?.stringValue ?? ""
                let evData = ev["data"]
                if let m = Self.modelName(fromEvent: ev) { turnModel = m }

                if evType == "turn/start", let t = evData?["turn"]?.doubleValue { currentTurn = Int(t) }
                if evType == "user/message", evData?["source"]?["rpcId"]?.stringValue == requestId {
                    ourTurn = currentTurn
                    continuation.yield(.started(phase: "thinking"))
                }
                // Everything below belongs to a specific turn; ignore other
                // turns' tools, messages, and endings entirely.
                let evTurn = evData?["turn"]?.doubleValue.map { Int($0) } ?? currentTurn
                guard let ours = ourTurn, evTurn == ours else { continue }

                switch evType {

                case "tool/call":
                    let id = evData?["callId"]?.stringValue ?? UUID().uuidString
                    let name = evData?["name"]?.stringValue ?? "tool"
                    let args = evData?["arguments"]?.stringValue
                    if name == "ask_user_question" { clarifyCallIDs.insert(id) }
                    if name == "ask_user_question",
                       let clarify = Self.pendingClarify(from: args, callId: id) {
                        pendingClarify = clarify
                        continuation.yield(.clarifyRequest(
                            question: clarify.question,
                            choices: clarify.choices,
                            requestID: clarify.requestID,
                            multiSelect: clarify.multiSelect
                        ))
                    } else {
                        let a = ToolActivity(label: name, toolCallID: id, name: name,
                                             argsPreview: args.map { String($0.prefix(400)) })
                        tools[id] = a
                        continuation.yield(.toolStarted(a))
                    }

                case "tool/result":
                    let id = evData?["message"]?["content"]?.arrayValue?.first?["toolCallId"]?.stringValue
                        ?? evData?["callId"]?.stringValue
                        ?? ""
                    if clarifyCallIDs.contains(id) {
                        // Answered (or timed out): the card is done either way.
                        pendingClarify = nil
                        continue
                    }
                    let isError = (evData?["error"] != nil)
                    var preview: String?
                    if let blocks = evData?["message"]?["content"]?.arrayValue {
                        var acc = ""
                        for b in blocks {
                            if let inner = b["content"]?.arrayValue {
                                for x in inner { if let t = x["text"]?.stringValue { acc += t } }
                            }
                        }
                        if !acc.isEmpty { preview = String(acc.prefix(600)) }
                    }
                    if var existing = tools[id] {
                        existing.isActive = false
                        existing.isError = isError
                        existing.resultPreview = preview
                        existing.finishedAt = .now
                        tools[id] = existing
                    }
                    continuation.yield(.toolCompleted(
                        toolCallID: id,
                        resultPreview: preview,
                        isError: isError,
                        durationMs: nil
                    ))

                case "assistant/message":
                    let msg = evData?["message"]
                    if let t = Self.text(fromContentBlocks: msg?["content"]), !t.isEmpty {
                        lastAssistantText = t
                    }
                    if let u = evData?["usage"] { usage = Self.usage(from: u) }

                case "turn/end":
                    // The parked clarify, if any, is no longer pending once the
                    // turn ends (answered, timed out, or skipped).
                    pendingClarify = nil
                    let kind = evData?["reason"]?["kind"]?.stringValue ?? "completed"
                    lastUsage = usage
                    if kind == "aborted" {
                        continuation.yield(.cancelled)
                        continuation.finish()
                        return .done
                    }
                    if kind != "completed" && kind != "error" {
                        // interrupted (host restart) and friends: terminal,
                        // manual retry only - never an automatic resend.
                        continuation.yield(.failed(
                            "Interrupted: the host restarted mid-turn. Tap retry to run it again.",
                            category: "upstream_interrupted",
                            action: ChatStore.serverTerminalFailureAction
                        ))
                        continuation.finish()
                        return .done
                    }
                    if kind == "error" {
                        let raw = evData?["reason"]?["error"]?["message"]?.stringValue ?? "DSH turn failed"
                        let code = evData?["reason"]?["error"]?["code"]?.stringValue
                        continuation.yield(.failed(
                            Self.readableTurnError(raw, model: turnModel),
                            category: Self.errorCategory(code: code, message: raw),
                            action: ChatStore.serverTerminalFailureAction
                        ))
                        continuation.finish()
                        return .done
                    }
                    let text = lastAssistantText ?? (textBuffer.isEmpty ? "(no content)" : textBuffer)
                    let finalMessage = Message(
                        id: UUID(), clientMessageID: clientMessageID,
                        sender: .herald, content: text, status: .sent
                    )
                    if var conv = currentConversation {
                        conv.messages.append(finalMessage)
                        conv.lastActivity = .now
                        conv.latestUsage = usage
                        currentConversation = conv
                    }
                    continuation.yield(.finished(finalMessage, usage, nil, nil))
                    // The follow stream never closes by itself; leaving it open
                    // leaked one socket and one Task per turn.
                    continuation.finish()
                    return .done

                default:
                    break
                }
                continue
            }

            if type == "error" {
                // A follow-level error is a transport fault, not a verdict
                // on the turn. Let the caller ask /job and reconnect.
                return .streamLost
            }
        }
        } catch is CancellationError {
            throw CancellationError()
        } catch {
            if Task.isCancelled { throw CancellationError() }
            // Socket dropped (DSH restart, network handoff). The turn may
            // still be running server-side.
            return .streamLost
        }
        // The stream ended before our turn did. Previously this fabricated a
        // "finished" reply from partial text - a wrong answer marked done.
        return .streamLost
    }

    // MARK: - User questions

    private func submitAnswer(sessionId: String, questionID: String, answer: String) async throws {
        // Determine whether the answer matches one of the question's options.
        let saved = pendingClarify
        var selected: [String] = []
        var custom: String? = nil
        let trimmed = answer.trimmingCharacters(in: .whitespacesAndNewlines)
        if let pending = saved {
            if let choices = pending.choices, choices.contains(trimmed) {
                selected = [trimmed]
            } else if let choices = pending.choices, let n = Int(trimmed), n >= 1, n <= choices.count {
                // "2" typed against a numbered card means option 2.
                selected = [choices[n - 1]]
            } else {
                custom = answer
            }
        } else {
            custom = answer
        }
        var answerDict: [String: Any] = [
            "id": questionID,
            "selected": selected
        ]
        if let custom = custom {
            answerDict["custom"] = custom
        }
        let body: [String: Any] = [
            "sessionId": sessionId,
            "answers": [answerDict]
        ]
        guard var req = request("answer", method: "POST") else {
            throw ServerError(message: "Invalid DSH base URL")
        }
        req.httpBody = try JSONSerialization.data(withJSONObject: body)
        _ = try await send(req)
    }

    // MARK: - Cancel

    func cancelJob(jobID: UUID) async throws {
        activeStreams[jobID]?.cancel()
        activeStreams[jobID] = nil
        if let conv = currentConversation, let native = nativeId(for: conv.id) {
            _ = try? await sendWithBody("cancel", ["sessionId": native])
        }
    }

    func interruptSession() async -> Bool {
        guard let conv = currentConversation, let native = nativeId(for: conv.id) else { return false }
        if let job = currentJobID, let task = activeStreams[job] { task.cancel() }
        do {
            _ = try await sendWithBody("cancel", ["sessionId": native])
            return true
        } catch {
            return false
        }
    }

    @discardableResult
    private func sendWithBody(_ path: String, _ body: [String: Any]) async throws -> Data {
        guard var req = request(path, method: "POST") else {
            throw ServerError(message: "Invalid DSH base URL")
        }
        req.httpBody = try JSONSerialization.data(withJSONObject: body)
        return try await send(req)
    }

    // MARK: - History

    private func fetchPage(sessionId: String, maxMessages: Int) async throws -> PageBody {
        let encoded = sessionId.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? sessionId
        let data = try await send(request("page?sessionId=\(encoded)&maxMessages=\(maxMessages)"))
        return try decoder.decode(PageBody.self, from: data)
    }

    // MARK: - Decoding helpers

    private static func text(fromContentBlocks blocks: JSONValue?) -> String? {
        guard let arr = blocks?.arrayValue else { return nil }
        var out = ""
        for b in arr {
            if b["type"]?.stringValue == "text", let t = b["text"]?.stringValue { out += t }
        }
        return out.isEmpty ? nil : out
    }

    /// Model id carried by a `model/selection` or `request/context` event.
    static func modelName(fromEvent ev: JSONValue?) -> String? {
        guard let ev, let type = ev["type"]?.stringValue,
              type == "model/selection" || type == "request/context" else { return nil }
        return ev["data"]?["model"]?.stringValue
    }

    /// Map a DSH failure code to the app's error categories. Only codes with
    /// a better generic message are mapped; everything else keeps the text.
    static func errorCategory(code: String?, message: String) -> String {
        switch code {
        // TIMEOUT is not mapped: the app's "timeout" copy blames the phone's
        // connection, and a DSH TIMEOUT is the model provider timing out.
        case "RATE_LIMIT": return "rate_limited"
        case "CONTEXT_OVERFLOW", "CONTEXT_LENGTH": return "context_exceeded"
        default: return "server_error"
        }
    }

    /// Turn a raw provider error into one readable line. DSH reports e.g.
    /// `503: {"message":"[anthropic-compatible-<id>/minimax-m3] [404]: 404 page
    /// not found (reset after 2m)"}` - the user needs the model and the cause.
    static func readableTurnError(_ raw: String, model: String?) -> String {
        var s = raw
        // Unwrap `NNN: {"message": "..."}`.
        if let brace = s.firstIndex(of: "{"),
           let data = String(s[brace...]).data(using: .utf8),
           let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let inner = (obj["message"] as? String) ?? ((obj["error"] as? [String: Any])?["message"] as? String) {
            s = inner
        }
        // Drop the `[provider-route/model]` prefix and `(reset after ...)` tail.
        s = s.replacingOccurrences(of: #"^\[[^\]]*/[^\]]*\]\s*"#, with: "", options: .regularExpression)
        s = s.replacingOccurrences(of: #"\s*\(reset after [^)]*\)"#, with: "", options: .regularExpression)
        // `[404]: 404 page not found` -> `404 page not found`.
        s = s.replacingOccurrences(of: #"^\[\d{3}\]:\s*"#, with: "", options: .regularExpression)
        s = s.trimmingCharacters(in: .whitespacesAndNewlines)
        if s.isEmpty { s = "the provider returned an error" }
        let who = model.map { "\($0) failed" } ?? "The model failed"
        return "\(who): \(s). Switch models and retry."
    }

    private static func usage(from v: JSONValue) -> TokenUsage? {
        guard let input = v["inputTokens"]?.doubleValue,
              let output = v["outputTokens"]?.doubleValue else { return nil }
        let total = v["totalTokens"]?.doubleValue ?? (input + output)
        return TokenUsage(promptTokens: Int(input), completionTokens: Int(output), totalTokens: Int(total))
    }

    /// A DSH sessionId is `session-<uuid>`; recover the UUID when possible so a
    /// session list row keeps a stable local identity across launches.
    static func stableUUID(from sessionId: String) -> UUID {
        let stripped = sessionId.hasPrefix("session-") ? String(sessionId.dropFirst("session-".count)) : sessionId
        return UUID(uuidString: stripped) ?? UUID()
    }

    /// Build a PendingClarify from the first question in an ask_user_question
    /// questions array. The common case is a single question; multiples are
    /// reduced to the first one so the UI stays simple.
    private static func pendingClarify(fromQuestions questions: [[String: Any]]) -> PendingClarify? {
        guard let first = questions.first,
              let id = first["id"] as? String,
              let question = first["question"] as? String else { return nil }
        let choices = (first["options"] as? [[String: Any]])?.compactMap { $0["label"] as? String }
        // Tool args say multi_select; the parked host request says multiSelect.
        let multiSelect = (first["multi_select"] as? Bool) ?? (first["multiSelect"] as? Bool) ?? false
        return PendingClarify(
            question: question,
            choices: choices?.isEmpty == false ? choices : nil,
            requestID: id,
            multiSelect: multiSelect
        )
    }

    /// Parse the ask_user_question tool arguments string into a PendingClarify.
    private static func pendingClarify(from arguments: String?, callId: String) -> PendingClarify? {
        guard let arguments = arguments,
              let data = arguments.data(using: .utf8),
              let decoded = try? JSONSerialization.jsonObject(with: data, options: []) as? [String: Any],
              let questions = decoded["questions"] as? [[String: Any]] else { return nil }
        return pendingClarify(fromQuestions: questions)
    }

    /// Rebuild a displayable message from a durable session event.
    ///
    /// DSH injects prompt context as `user/message` events carrying a NON-user
    /// `source.kind` — `agent-instructions` (the AGENTS.md `<system-reminder>`),
    /// `plugin` with `form: snapshot` (runtime context), and `skill-catalog`
    /// with `form: catalog` (the `<available_skills>` list). Rendering those
    /// puts the prompt itself in the transcript. Only `kind: user` is a real
    /// turn; assistant rows must come from the model, not from a tool result.
    static func message(fromWireEvent record: JSONValue, model: String? = nil) -> Message? {
        guard let ev = record["event"], let type = ev["type"]?.stringValue else { return nil }
        let data = ev["data"]
        let time = ev["time"]?.doubleValue.map { Date(timeIntervalSince1970: $0 / 1000) } ?? .now
        switch type {
        case "user/message":
            guard data?["source"]?["kind"]?.stringValue == "user" else { return nil }
            guard let text = text(fromContentBlocks: data?["content"]),
                  !text.hasPrefix("<system-reminder>") else { return nil }
            return Message(id: UUID(), clientMessageID: nil, sender: .user,
                           content: text, timestamp: time, status: .sent)

        case "assistant/message":
            guard data?["message"]?["source"]?["kind"]?.stringValue == "model" else { return nil }
            guard let text = text(fromContentBlocks: data?["message"]?["content"]),
                  !text.hasPrefix("<system-reminder>") else { return nil }
            return Message(id: UUID(), clientMessageID: nil, sender: .herald,
                           content: text, timestamp: time, status: .sent)

        case "turn/end":
            // A turn that ended in error has no assistant row; without this the
            // user's message sits unanswered in history with no explanation.
            guard data?["reason"]?["kind"]?.stringValue == "error" else { return nil }
            let raw = data?["reason"]?["error"]?["message"]?.stringValue ?? "DSH turn failed"
            let code = data?["reason"]?["error"]?["code"]?.stringValue
            return Message(id: UUID(), clientMessageID: nil, sender: .system,
                           content: readableTurnError(raw, model: model), timestamp: time,
                           status: .failed, errorCategory: errorCategory(code: code, message: raw))

        default:
            return nil
        }
    }
}
