import Foundation
import os
import UIKit

/// Maps between the app's UUID-based session model and the native gateway's
/// short-hex session_id model. Stores the mapping in UserDefaults.
actor NativeSessionIdMap {
    private static let defaultsKey = "NativeGwSessionIdMap"
    private var uuidToNative: [String: String] = [:]
    private var nativeToUuid: [String: String] = [:]

    init() {
        if let data = UserDefaults.standard.data(forKey: Self.defaultsKey),
           let map = try? JSONDecoder().decode([String: String].self, from: data) {
            uuidToNative = map
            for (k, v) in map { nativeToUuid[v] = k }
        }
    }

    func register(uuid: UUID, nativeId: String) {
        uuidToNative[uuid.uuidString] = nativeId
        nativeToUuid[nativeId] = uuid.uuidString
        save()
    }

    func nativeId(for uuid: UUID) -> String? {
        uuidToNative[uuid.uuidString]
    }

    func uuid(for nativeId: String) -> UUID? {
        nativeToUuid[nativeId].flatMap(UUID.init)
    }

    func remove(uuid: UUID) {
        if let native = uuidToNative.removeValue(forKey: uuid.uuidString) {
            nativeToUuid.removeValue(forKey: native)
            save()
        }
    }

    private func save() {
        if let data = try? JSONEncoder().encode(uuidToNative) {
            UserDefaults.standard.set(data, forKey: Self.defaultsKey)
        }
    }
}

/// NativeKallistiClient: implements HeraldClientProtocol using the native
/// JSON-RPC/WebSocket gateway (ws://host:9119/api/ws) instead of the
/// REST+SSE connector facade.
///
/// This is the 0.2.0 path. Session IDs are mapped between UUID (app side)
/// and short-hex (gateway side) via NativeSessionIdMap.
@MainActor
@Observable
final class NativeKallistiClient: HeraldClientProtocol {
    private static let logger = Logger(subsystem: "net.fihonline.kallisti", category: "NativeKallistiClient")

    // MARK: - Dependencies

    /// Resolves the gateway base URL at call time from the app's current relay
    /// configuration (AppContainer passes a closure reading settingsStore). A
    /// fresh install has no relay -> localhost fallback, but once the user
    /// enters their server in onboarding, the provider reads the updated relay
    /// so every connect/login hits the real host, not a launch-time stale value.
    private let gatewayBaseURLProvider: @MainActor () -> String
    private let authCoordinator: NativeAuthCoordinator

    /// Resolve the current gateway base URL (live, not cached).
    private var gatewayBaseURL: String { gatewayBaseURLProvider() }
    private let idMap = NativeSessionIdMap()
    private let transportFactory: @Sendable () -> any NativeGatewayTransport
    private(set) var client: NativeGatewayClient?
    private var transport: (any NativeGatewayTransport)?
    private var reconnectTask: Task<Void, Never>?
    private var reconnectAttempt = 0
    /// When the current socket was opened, used to tell a working connection
    /// that later dropped from one that never came up at all.
    private var connectedAt: Date?
    /// Set by disconnect() so an intentional teardown isn't fought by the
    /// reconnect loop.
    private var isDeliberatelyDisconnected = false

    // MARK: - HeraldClientProtocol

    var connectionStatus: ConnectionStatus = .disconnected
    var currentConversation: Conversation?
    private(set) var hasStoredLogin = false

    /// Typed feature calls (gateway status, model options, aux models) riding
    /// the SAME socket as chat. Stateless: each call fetches the current
    /// client so reconnects are handled transparently. The class is
    /// @MainActor, so the closure captures self.client directly.
    var featureClient: NativeGatewayFeatureClient {
        NativeGatewayFeatureClient { [weak self] in
            self?.client
        } currentSessionIdProvider: { [weak self] in
            guard let self else { return nil }
            // A fresh "New Chat" conversation has no gateway session yet.
            // Without one, slash.exec /model dies with 4001 "session not
            // found" and the caller falls back to the legacy relay path,
            // which rejects native bearer tokens.
            //
            // Build 31 created a session when the mapping was MISSING, but
            // never validated STALE mappings: after a gateway restart the
            // server reaps sessions from its in-memory registry, the
            // persisted idMap entry points at a dead session, and the switch
            // still 4001s. Use the same stale-check as the chat path
            // (ensureSessionForSwitch) so dead mappings are unregistered and
            // recreated before the switch runs.
            if let conv = self.currentConversation {
                if await self.ensureSessionForSwitch(id: conv.id) {
                    return await self.idMap.nativeId(for: conv.id)
                }
                return nil
            }
            // Hub opened from a fresh chat where the native client has not
            // loaded a conversation yet. Fall back to the persisted active
            // session id (set by ChatStore on send/selection) so the switch
            // still has a session to scope to.
            if let persistedID = UserDefaults.standard.string(forKey: "herald.activeSessionId"),
               let uuid = UUID(uuidString: persistedID),
               await self.ensureSessionForSwitch(id: uuid) {
                return await self.idMap.nativeId(for: uuid)
            }
            return nil
        } gatewayBaseURLProvider: { [weak self] in
            self?.gatewayBaseURL ?? "http://localhost:9119"
        }
    }

    private let secureStore: (any SecureStoreProtocol)?

    init(
        gatewayBaseURLProvider: @escaping @MainActor () -> String,
        authCoordinator: NativeAuthCoordinator,
        secureStore: (any SecureStoreProtocol)? = nil,
        transportFactory: @escaping @Sendable () -> any NativeGatewayTransport = { URLSessionWebSocketTransport() }
    ) {
        self.gatewayBaseURLProvider = gatewayBaseURLProvider
        self.authCoordinator = authCoordinator
        self.secureStore = secureStore
        self.transportFactory = transportFactory
    }

    /// Convenience for callers with a fixed base URL (e.g. tests).
    convenience init(
        gatewayBaseURL: String,
        authCoordinator: NativeAuthCoordinator,
        secureStore: (any SecureStoreProtocol)? = nil,
        transportFactory: @escaping @Sendable () -> any NativeGatewayTransport = { URLSessionWebSocketTransport() }
    ) {
        self.init(
            gatewayBaseURLProvider: { gatewayBaseURL },
            authCoordinator: authCoordinator,
            secureStore: secureStore,
            transportFactory: transportFactory
        )
    }

    /// Registers this session + APNs token with the connector so it can
    /// deliver a push when the turn finishes with the app backgrounded.
    /// Authenticates with the gateway bearer token, since native-gateway
    /// clients never establish the connector's paired credential.
    private func registerNativeWatch(sessionId: String) async {
        guard let secureStore else { return }
        guard let deviceToken = await secureStore.retrieve(key: AppContainer.apnsTokenKeychainKey),
              !deviceToken.isEmpty,
              let accessToken = await authCoordinator.currentAccessToken()
        else { return }

        // APNs watch registration lives on the connector's HTTP facade
        // (port 8010), NOT the gateway (9119). The gateway has no
        // /v1/native/watch route and its auth gate redirects the POST (302),
        // so posting to gatewayBaseURL silently failed and the connector
        // never watched the session -> no backgrounded-turn push. Derive the
        // facade URL the same way NativeGatewayFeatureClient.connectorVersion
        // does: same scheme+host, port 8010.
        guard let base = URL(string: gatewayBaseURL),
              let host = base.host,
              let url = URL(string: "\(base.scheme ?? "http")://\(host):8010/v1/native/watch")
        else { return }

        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
        request.httpBody = try? JSONEncoder().encode([
            "session_id": sessionId,
            "device_token": deviceToken,
            "token_kind": "alert",
        ])

        do {
            let (_, response) = try await URLSession.shared.data(for: request)
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            if status != 200 {
                Self.logger.warning("native watch registration returned HTTP \(status)")
            }
        } catch {
            Self.logger.warning("native watch registration failed: \(error.localizedDescription)")
        }
    }

    func connect() async {
        connectionStatus = .connecting
        // Torn down in the catch block if the verification round-trip
        // below fails, so a socket that "opened" but never proved itself
        // doesn't linger open in the background on every backoff retry.
        var provisionalClient: NativeGatewayClient?
        do {
            // Tickets are single-use with a 30s TTL, so every connect --
            // including each reconnect attempt -- mints a fresh one.
            let ticket = try await authCoordinator.mintTicket()
            guard let base = URL(string: gatewayBaseURL) else {
                throw NativeAuthError.invalidBaseURL(gatewayBaseURL)
            }
            var components = URLComponents(url: base.appendingPathComponent("api/ws"), resolvingAgainstBaseURL: false)!
            components.scheme = base.scheme == "https" ? "wss" : "ws"
            components.queryItems = [URLQueryItem(name: "ticket", value: ticket)]
            guard let wsURL = components.url else {
                throw NativeAuthError.invalidBaseURL(components.description)
            }
            let transport = transportFactory()
            let client = NativeGatewayClient(transport: transport)
            provisionalClient = client

            try await client.connect(url: wsURL)
            // task.resume() never throws, so reaching this line does not
            // prove the socket actually opened -- a rejected ws-ticket or a
            // routing hiccup looks identical to success until something is
            // actually sent. Verify with a cheap round-trip before this
            // client is published and connectionStatus says .connected;
            // a socket that's really dead fails here and falls through to
            // the same catch as any other connect failure below instead of
            // a false "connected" that dies on the very first real request.
            _ = try await client.send(method: "session.list", params: SessionListParams(limit: 1, offset: 0, allDevices: nil))

            await client.onDisconnect { [weak self] in
                Task { @MainActor in await self?.handleUnexpectedDisconnect() }
            }
            // Close any previous transport so a reconnect does not leave an
            // orphaned WS open server-side (each connect() mints a fresh
            // ticket + socket; replacing self.client without closing the old
            // transport leaks the old connection and its sessions).
            if let old = self.transport {
                await old.close()
            }
            self.transport = transport
            self.client = client
            // NOT resetting reconnectAttempt here: a socket can pass the
            // verification above and still be new/flaky. The counter is
            // reset only once a connection has proven itself durable (see
            // handleUnexpectedDisconnect below).
            connectedAt = Date()
            hasStoredLogin = true
            connectionStatus = .connected
            Self.logger.info("Connected to native gateway")
        } catch {
            // The client only reaches self.client/self.transport after
            // verification succeeds (above) -- if we got here with a
            // provisional client, its socket "opened" but never proved
            // itself, so close it rather than leaving it dangling.
            if let provisionalClient {
                await provisionalClient.close()
            }
            // notLoggedIn means there truly is no token -- clear the flag.
            // Any OTHER failure (network blip, rejected ticket, dead
            // verification round-trip) must NOT set hasStoredLogin true:
            // a token existing is not proof this device can actually reach
            // its gateway (a stale/orphaned token from a previous broken
            // build mints fine and then fails verification every time,
            // which used to skip onboarding straight into a permanently
            // broken main app with no way back). Only a verified successful
            // connect (above) may set it true. A later failure on an
            // ALREADY-proven connection correctly leaves it true, which is
            // what keeps a transient drop from bouncing back to onboarding
            // and a redundant Nous OAuth login -- that's still intact here,
            // it just isn't granted on faith from an untested token anymore.
            if case NativeAuthError.notLoggedIn = error {
                hasStoredLogin = false
            }
            connectionStatus = hasStoredLogin ? .reconnecting : .disconnected
            Self.logger.error("Failed to connect: \(error)")
            scheduleReconnect()
        }
    }

    /// The socket died on its own (proxy idle reap, network change, cell
    /// handoff). Without this the app stays up but every request fails with
    /// transportClosed until it's force-quit.
    private func handleUnexpectedDisconnect() async {
        guard !isDeliberatelyDisconnected else { return }
        // A socket that survived a while was genuinely working, so start its
        // backoff fresh. One that died on arrival was never up -- keep
        // escalating so a persistent failure backs off instead of spinning.
        if let connectedAt, Date().timeIntervalSince(connectedAt) >= 30 {
            reconnectAttempt = 0
        }
        connectedAt = nil
        Self.logger.warning("Native gateway transport closed unexpectedly — reconnecting")
        connectionStatus = .reconnecting
        client = nil
        transport = nil
        scheduleReconnect()
    }

    private func scheduleReconnect() {
        guard !isDeliberatelyDisconnected else { return }
        guard reconnectTask == nil else { return }
        let attempt = reconnectAttempt
        reconnectAttempt += 1
        // 1s, 2s, 4s ... capped at 30s.
        let delaySeconds = min(pow(2.0, Double(attempt)), 30.0)
        reconnectTask = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .seconds(delaySeconds))
            guard let self, !Task.isCancelled else { return }
            self.reconnectTask = nil
            guard !self.isDeliberatelyDisconnected else { return }
            await self.connect()
        }
    }

    func disconnect() async {
        isDeliberatelyDisconnected = true
        reconnectTask?.cancel()
        reconnectTask = nil
        await client?.close()
        client = nil
        transport = nil
        connectionStatus = .disconnected
    }

    /// Called on app foreground. iOS suspends URLSessionWebSocketTask sockets
    /// silently -- receive() never surfaces an error, so handleUnexpectedDisconnect
    /// may never fire and the app keeps a phantom "connected" socket that fails
    /// every request. Force a fresh connect whenever we're not verifiably
    /// connected with a live client.
    func reconnectIfNeeded() async {
        guard !isDeliberatelyDisconnected else { return }
        if let client, connectionStatus == .connected {
            // Probe cheaply: a dead socket errors on the send, and the
            // receive loop tears down + reconnects. If the probe succeeds we
            // are genuinely alive.
            //
            // CRITICAL (build 26 latency fix): the probe must use a SHORT
            // timeout. A phantom socket (iOS suspended the WS in background,
            // receive() never surfaced the error) would otherwise make this
            // session.list probe hang the full 60s request timeout before we
            // fall through to a fresh connect - and the user's next message
            // sat in the outbox behind that 60s hang (~85s total round trip
            // for a 4s LLM call).
            do {
                _ = try await client.send(
                    method: "session.list",
                    params: SessionListParams(limit: 1, offset: 0, allDevices: nil),
                    timeoutNanos: Self.probeTimeoutNanos
                )
                return
            } catch {
                // Fall through to a fresh connect below.
            }
        }
        Self.logger.info("reconnectIfNeeded: forcing fresh connect")
        connectionStatus = .connecting
        reconnectTask?.cancel()
        reconnectTask = nil
        await connect()
    }

    /// Liveness probe timeout: 5 seconds. Short enough that a phantom dead
    /// socket is detected before the user notices, long enough that a
    /// genuinely slow gateway doesn't cause spurious reconnects.
    private static let probeTimeoutNanos: UInt64 = 5_000_000_000

    /// Drives the interactive Nous OAuth/PKCE login (browser handoff) and
    /// then retries `connect()`. Called from onboarding's "Open app" step —
    /// `connect()` alone can never trigger this itself, since a failed
    /// mintTicket() there has nowhere to present a login screen from.
    func startInteractiveLogin(presentingFrom viewController: UIViewController) async throws {
        try await authCoordinator.startLogin(presentingFrom: viewController)
        await connect()
        // connect() never throws -- by design, its other callers (the
        // silent launch-time connect, the background reconnect loop) want
        // to observe connectionStatus reactively, not catch an exception.
        // But onboarding's "Open app" button needs to know: without this,
        // a login that succeeds followed by a connect that silently fails
        // (unreachable gateway, rejected ticket, etc.) looked identical to
        // success -- Nous OAuth would fire again with zero error shown,
        // every single tap, forever. Surface it here instead.
        guard connectionStatus == .connected else {
            throw NativeAuthError.connectFailedAfterLogin
        }
    }

    // MARK: - Session Lifecycle

    func listSessions(limit: Int, offset: Int, allDevices: Bool) async throws -> SessionListResponse {
        guard let client else { throw NativeGatewayClientError.notConnected }
        let params = SessionListParams(limit: limit, offset: offset, allDevices: allDevices)
        let response = try await client.send(method: "session.list", params: params)
        if let error = response.error { throw error }
        guard let result = response.result else {
            return SessionListResponse(sessions: [], total: 0)
        }
        let data = try JSONEncoder().encode(result)
        let decoded = try JSONDecoder().decode(NativeSessionListResult.self, from: data)

        var sessions: [SessionSummary] = []
        for native in decoded.sessions {
            // Filter out internal/cron sessions (wedding-board enrichment
            // agent, etc.) so the history list only shows real chats. The
            // gateway's session.list RPC returns ALL sources (the deny-list
            // only filters kanban/tool for the `recent` command).
            if let source = native.source?.lowercased(), source == "cron" {
                continue
            }
            // CRITICAL: resolve the app-side UUID AND register the reverse
            // mapping when it's missing. Previously listSessions minted a
            // fresh UUID() that was NEVER registered in idMap, so any
            // interaction with a resumed session (delete, rename, follow-up
            // message, model switch) failed with "session not found" / 4001
            // or created a brand-new server session (context loss + cold
            // start latency on every follow-up).
            let uuid: UUID
            if let existing = await idMap.uuid(for: native.sessionId) {
                uuid = existing
            } else {
                uuid = UUID()
                await idMap.register(uuid: uuid, nativeId: native.sessionId)
            }
            sessions.append(SessionSummary(
                id: uuid,
                title: native.title ?? "Untitled",
                previewText: native.previewText ?? "",
                lastActivity: native.lastActivity.flatMap { ISO8601DateFormatter().date(from: $0) } ?? .now,
                isPinned: native.isPinned ?? false,
                isArchived: native.isArchived ?? false
            ))
        }
        return SessionListResponse(sessions: sessions, total: decoded.total ?? sessions.count)
    }

    func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] {
        guard let client else { throw NativeGatewayClientError.notConnected }
        let response = try await client.send(method: "session.search", params: ["query": query])
        if let error = response.error { throw error }
        guard let result = response.result else { return [] }
        let data = try JSONEncoder().encode(result)
        let decoded = try JSONDecoder().decode(NativeSessionListResult.self, from: data)
        return decoded.sessions.map { native in
            SessionSummary(
                title: native.title ?? "Untitled",
                previewText: native.previewText ?? ""
            )
        }
    }

    /// Protocol-conforming overload (HeraldClientProtocol requires the exact
    /// single-arg signature; a defaulted parameter does NOT satisfy it).
    func createSession(title: String) async throws -> SessionSummary {
        try await createSession(title: title, conversationID: nil)
    }

    func createSession(title: String, conversationID: UUID? = nil) async throws -> SessionSummary {
        guard let client else { throw NativeGatewayClientError.notConnected }
        let response = try await client.send(method: "session.create", params: ["title": title])
        if let error = response.error { throw error }
        guard let result = response.result else { throw NativeGatewayClientError.unexpectedFrame }
        let data = try JSONEncoder().encode(result)
        let decoded = try JSONDecoder().decode(NativeSessionCreateResult.self, from: data)
        // CRITICAL: register the CALLER's conversation UUID (the one
        // ChatStore / currentConversation uses), NOT a fresh random UUID.
        // Registering a random UUID here means idMap.nativeId(for:
        // conversation.id) returns nil on the next message, so every message
        // creates a brand-new server session and follow-ups die with 4001
        // "session not found" (or orphan the previous session).
        let uuid = conversationID ?? UUID()
        await idMap.register(uuid: uuid, nativeId: decoded.sessionId)
        return SessionSummary(id: uuid, title: title, lastActivity: .now)
    }

    func deleteSession(id: UUID) async throws {
        guard let client else { throw NativeGatewayClientError.notConnected }
        guard let nativeId = await idMap.nativeId(for: id) else { throw NativeGatewayClientError.unexpectedFrame }
        let response = try await client.send(method: "session.delete", params: ["session_id": nativeId])
        if let error = response.error { throw error }
        await idMap.remove(uuid: id)
    }

    func archiveSession(id: UUID) async throws {
        guard let client else { throw NativeGatewayClientError.notConnected }
        guard let nativeId = await idMap.nativeId(for: id) else { throw NativeGatewayClientError.unexpectedFrame }
        let response = try await client.send(method: "session.archive", params: ["session_id": nativeId])
        if let error = response.error { throw error }
    }

    func togglePinSession(id: UUID) async throws -> SessionSummary {
        guard let client else { throw NativeGatewayClientError.notConnected }
        guard let nativeId = await idMap.nativeId(for: id) else { throw NativeGatewayClientError.unexpectedFrame }
        let response = try await client.send(method: "session.toggle_pin", params: ["session_id": nativeId])
        if let error = response.error { throw error }
        return SessionSummary(id: id, title: "")
    }

    func renameSession(id: UUID, title: String) async throws -> SessionSummary {
        guard let client else { throw NativeGatewayClientError.notConnected }
        guard let nativeId = await idMap.nativeId(for: id) else { throw NativeGatewayClientError.unexpectedFrame }
        let response = try await client.send(method: "session.rename", params: ["session_id": nativeId, "title": title])
        if let error = response.error { throw error }
        return SessionSummary(id: id, title: title)
    }

    func generateSessionTitle(sessionId: UUID, userMessage: String, assistantMessage: String) async throws -> String {
        return ""
    }

    // MARK: - Conversation

    func loadConversation() async -> Conversation {
        let conv = Conversation(title: "New Chat")
        currentConversation = conv
        return conv
    }

    func loadConversation(id: UUID) async throws -> Conversation {
        guard let client else { throw NativeGatewayClientError.notConnected }
        guard let nativeId = await idMap.nativeId(for: id) else { throw NativeGatewayClientError.unexpectedFrame }
        let response = try await client.send(method: "session.history", params: ["session_id": nativeId])
        if let error = response.error { throw error }
        guard let result = response.result else { return Conversation(id: id, title: "Untitled") }
        let data = try JSONEncoder().encode(result)
        let decoded = try JSONDecoder().decode(NativeSessionHistoryResult.self, from: data)
        let messages = decoded.messages.map { msg -> Message in
            Message(
                id: UUID(),
                sender: msg.role == "assistant" ? .herald : .user,
                content: msg.content,
                timestamp: msg.timestamp.flatMap { ISO8601DateFormatter().date(from: $0) } ?? .now
            )
        }
        let conv = Conversation(id: id, title: decoded.title ?? "Untitled", messages: messages)
        currentConversation = conv
        return conv
    }

    func clearConversation() async throws -> Conversation {
        let conv = Conversation(title: "New Chat")
        currentConversation = conv
        return conv
    }

    func injectVoiceTranscript(voiceSessionId: UUID) async throws -> Conversation {
        return currentConversation ?? Conversation(title: "New Chat")
    }

    /// Resolve a native session id for `id`, validating staleness like
    /// ensureConversation (cheap session.status probe; unregister + recreate
    /// when the mapping points at a reaped session). Returns the mapped
    /// native id, or nil if no session could be established.
    func ensureSessionForSwitch(id: UUID) async -> Bool {
        if let nativeId = await idMap.nativeId(for: id) {
            do {
                guard let client else { throw NativeGatewayClientError.notConnected }
                let response = try await client.send(
                    method: "session.status",
                    params: ["session_id": nativeId]
                )
                if response.error == nil {
                    return true
                }
            } catch {
                // Treat any probe failure as stale; fall through to recreate.
            }
            await idMap.remove(uuid: id)
        }
        do {
            _ = try await createSession(title: "New Chat", conversationID: id)
            return true
        } catch {
            return false
        }
    }

    func ensureConversation(id: UUID) async -> Bool {
        // A persisted mapping can point at a session the gateway no longer
        // has (reaped after a disconnect / gateway restart). Validate with a
        // cheap metadata probe before trusting it, and unregister + recreate
        // when stale -- otherwise the next prompt.submit dies with 4001
        // "session not found".
        //
        // LATENCY (build 32): the probe used to call session.history, which
        // returns the ENTIRE transcript (with ancestors). On a long session
        // that download took 30-60s+ and happened on EVERY follow-up send
        // before the thinking placeholder rendered -- the 60-120s "dead"
        // window with zero UI feedback. session.status returns small
        // metadata text through the same _sess_nowait 4001 path, so stale
        // detection is identical and the payload is trivial.
        if let nativeId = await idMap.nativeId(for: id) {
            do {
                guard let client else { throw NativeGatewayClientError.notConnected }
                let response = try await client.send(
                    method: "session.status",
                    params: ["session_id": nativeId]
                )
                if response.error == nil {
                    // Keep the native client's currentConversation pointing at
                    // this conversation so _sendStreaming resolves the right
                    // session UUID without a redundant transcript download.
                    if currentConversation?.id != id {
                        currentConversation = Conversation(id: id, title: currentConversation?.title ?? "New Chat")
                    }
                    return true
                }
            } catch {
                // Treat any probe failure as stale; fall through to recreate.
            }
            await idMap.remove(uuid: id)
        }
        do {
            _ = try await createSession(title: "New Chat", conversationID: id)
            if currentConversation?.id != id {
                currentConversation = Conversation(id: id, title: "New Chat")
            }
            return true
        } catch {
            return false
        }
    }

    // MARK: - Send / Streaming

    func send(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String?) async -> Message {
        var finalContent = ""
        var finalUsage: TokenUsage?
        for await update in sendStreaming(message: message, attachments: attachments, clientMessageID: clientMessageID, continuationContext: continuationContext) {
            switch update {
            case .textDelta(let text):
                finalContent += text
            case .finished(let msg, let usage, _, _):
                return msg
            case .failed(let error, _, _):
                return Message(id: clientMessageID, sender: .herald, content: "Error: \(error)", timestamp: .now)
            default:
                break
            }
        }
        return Message(id: clientMessageID, sender: .herald, content: finalContent, timestamp: .now)
    }

    func sendStreaming(message: String, attachments: [PendingAttachment], clientMessageID: UUID, continuationContext: String?) -> AsyncStream<StreamingUpdate> {
        AsyncStream { continuation in
            Task { [weak self] in
                guard let self else { continuation.finish(); return }
                await self._sendStreaming(
                    message: message,
                    clientMessageID: clientMessageID,
                    continuation: continuation
                )
            }
        }
    }

    private func _sendStreaming(
        message: String,
        clientMessageID: UUID,
        continuation: AsyncStream<StreamingUpdate>.Continuation
    ) async {
        // Build 26 (latency fix): verify the socket is genuinely alive BEFORE
        // submitting. A phantom connection (iOS suspended the WS in
        // background, receive() never surfaced the error) would otherwise let
        // prompt.submit hang the full 60s request timeout, and the user's
        // message waits behind that hang (~85s total for a 4s LLM call).
        // reconnectIfNeeded() probes with a 5s timeout and forces a fresh
        // connect when the probe fails, so a dead socket heals fast and the
        // submit lands on a live connection.
        await reconnectIfNeeded()

        guard let client else {
            continuation.yield(.failed("Not connected"))
            continuation.finish()
            return
        }

        // Get or create session
        let sessionUUID = currentConversation?.id ?? UUID()
        var nativeSessionId = await idMap.nativeId(for: sessionUUID)

        if nativeSessionId == nil {
            do {
                let summary = try await createSession(title: "New Chat", conversationID: sessionUUID)
                nativeSessionId = await idMap.nativeId(for: summary.id)
            } catch {
                continuation.yield(.failed("Failed to create session: \(error)"))
                continuation.finish()
                return
            }
        }

        guard let sid = nativeSessionId else {
            continuation.yield(.failed("No session ID"))
            continuation.finish()
            return
        }

        // Register a one-shot event handler for this stream
        let handler = StreamEventHandler(sessionId: sid, continuation: continuation)
        await client.onEvent { event in
            handler.handle(event)
        }

        // Tell the connector to watch this session so it can fire an APNs
        // push if the turn finishes while the app is backgrounded or killed
        // (the app's own WebSocket is suspended then, so nothing else can
        // notice). Best-effort: a failure here costs a notification, never
        // the turn itself.
        await registerNativeWatch(sessionId: sid)

        // Submit the prompt
        do {
            let response = try await client.send(
                method: "prompt.submit",
                params: PromptSubmitParams(sessionId: sid, text: message)
            )
            if let error = response.error {
                // A stale idMap entry (persisted from a previous run, reaped
                // server-side) surfaces as 4001 "session not found". Unregister
                // the dead mapping, create a fresh session, register a NEW
                // handler for it, and retry ONCE so the user's message still
                // lands instead of failing.
                if error.message.localizedCaseInsensitiveContains("session not found") {
                    await idMap.remove(uuid: sessionUUID)
                    do {
                        let summary = try await createSession(title: "New Chat", conversationID: sessionUUID)
                        guard let freshSid = await idMap.nativeId(for: summary.id) else {
                            continuation.yield(.failed("No session ID"))
                            continuation.finish()
                            return
                        }
                        let freshHandler = StreamEventHandler(sessionId: freshSid, continuation: continuation)
                        await client.onEvent { event in
                            freshHandler.handle(event)
                        }
                        let retry = try await client.send(
                            method: "prompt.submit",
                            params: PromptSubmitParams(sessionId: freshSid, text: message)
                        )
                        if let retryError = retry.error {
                            continuation.yield(.failed(retryError.message))
                            continuation.finish()
                            return
                        }
                        // No finish() here - the fresh handler's stream events
                        // drive the continuation to completion.
                        return
                    } catch {
                        continuation.yield(.failed("Submit failed: \(error)"))
                        continuation.finish()
                        return
                    }
                }
                continuation.yield(.failed(error.message))
                continuation.finish()
                return
            }
        } catch {
            continuation.yield(.failed("Submit failed: \(error)"))
            continuation.finish()
            return
        }
    }

    func cancelJob(jobID: UUID) async throws {
        guard let client else { throw NativeGatewayClientError.notConnected }
        let response = try await client.send(method: "prompt.cancel", params: ["job_id": jobID.uuidString])
        if let error = response.error { throw error }
    }

    func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }

    func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message {
        return await send(message: text, attachments: [], clientMessageID: clientMessageID, continuationContext: nil)
    }
}

// MARK: - Stream Event Handler

/// Processes gateway events for a single streaming turn.
private final class StreamEventHandler: @unchecked Sendable {
    let sessionId: String
    let continuation: AsyncStream<StreamingUpdate>.Continuation
    private var completed = false

    init(sessionId: String, continuation: AsyncStream<StreamingUpdate>.Continuation) {
        self.sessionId = sessionId
        self.continuation = continuation
    }

    func handle(_ event: NativeGatewayEvent) {
        guard event.params.sessionId == sessionId, !completed else { return }
        switch event.params.type {
        case "message.delta":
            if let delta = event.params.decodePayload(NativeMessageDeltaPayload.self) {
                continuation.yield(.textDelta(delta.text))
            }
        case "thinking.delta":
            // Build 26 (thought-stacking fix): the gateway streams
            // KawaiiSpinner frames ("(°□°) analyzing...", "(・_・)>musing...")
            // on this event via thinking_callback - one frame per tool-call
            // API round. They are transient UI noise, NOT reasoning: feeding
            // them into .reasoningDelta stacked faces up in the thought card
            // and pushed out the real chain-of-thought. Real CoT arrives on
            // "reasoning.delta" (handled below). Drop the spinner frames.
            break
        case "reasoning.delta":
            // Real chain-of-thought from the gateway's reasoning_callback.
            if let delta = event.params.decodePayload(NativeThinkingDeltaPayload.self) {
                continuation.yield(.reasoningDelta(delta.text))
            }
        case "message.complete":
            if let complete = event.params.decodePayload(NativeMessageCompletePayload.self) {
                let usage = complete.usage.map { u in
                    TokenUsage(
                        promptTokens: u.input ?? 0,
                        completionTokens: u.output ?? 0,
                        totalTokens: u.total ?? 0
                    )
                }
                let msg = Message(
                    id: UUID(),
                    sender: .herald,
                    content: complete.text ?? "",
                    timestamp: .now
                )
                continuation.yield(.finished(msg, usage, nil, nil))
                completed = true
                continuation.finish()
            }
        default:
            break
        }
    }
}

// MARK: - Encodable param structs

private struct SessionListParams: Encodable {
    let limit: Int
    let offset: Int
    // The native gateway currently returns all sessions regardless of device
    // scope; the flag is forwarded so a future server-side filter works and
    // the toggle at least round-trips instead of being silently dropped.
    let allDevices: Bool?
}

private struct PromptSubmitParams: Encodable {
    let sessionId: String
    let text: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case text
    }
}
