import Foundation
import CryptoKit
import os
import UIKit

/// Maps between the app's UUID-based session model and the native gateway's
/// short-hex session_id model. Stores the mapping in UserDefaults.
actor NativeSessionIdMap {
    private static let defaultsKey = "NativeGwSessionIdMap"
    private static let defaultsKeyFull = "NativeGwSessionKeyMap"
    private var uuidToNative: [String: String] = [:]
    private var nativeToUuid: [String: String] = [:]
    /// FULL gateway session key (e.g. 20260816_144338_2bbd59) per UUID.
    /// Kept separate from the SHORT live id: session.resume requires the
    /// FULL key while session.history/status require the SHORT id.
    private var uuidToKey: [String: String] = [:]
    private var keyToUuid: [String: String] = [:]

    init() {
        if let data = UserDefaults.standard.data(forKey: Self.defaultsKey),
           let map = try? JSONDecoder().decode([String: String].self, from: data) {
            uuidToNative = map
            for (k, v) in map { nativeToUuid[v] = k }
        }
        if let data = UserDefaults.standard.data(forKey: Self.defaultsKeyFull),
           let map = try? JSONDecoder().decode([String: String].self, from: data) {
            uuidToKey = map
            for (k, v) in map { keyToUuid[v] = k }
        }
    }

    func register(uuid: UUID, nativeId: String) {
        let key = uuid.uuidString
        // Build 128.45 (cross-session merge fix): keep the bidirectional map
        // internally consistent. Re-pointing a UUID to a NEW native session
        // (resume/switch/createSession with an existing conversationID) must
        // drop the OLD nativeId's reverse entry, or the old server session
        // still resolves to this same local UUID - two server sessions share
        // one thread, and ChatStore's merge splices both histories together
        // out of order. Same for the forward direction: if this nativeId was
        // already owned by another UUID, that UUID must lose it so one server
        // session never maps to two local conversations.
        if let oldNative = uuidToNative[key], oldNative != nativeId {
            nativeToUuid.removeValue(forKey: oldNative)
        }
        if let oldUuid = nativeToUuid[nativeId], oldUuid != key {
            uuidToNative.removeValue(forKey: oldUuid)
        }
        uuidToNative[key] = nativeId
        nativeToUuid[nativeId] = key
        save()
    }

    func nativeId(for uuid: UUID) -> String? {
        uuidToNative[uuid.uuidString]
    }

    func uuid(for nativeId: String) -> UUID? {
        nativeToUuid[nativeId].flatMap(UUID.init)
    }

    /// Register the FULL gateway session key for a UUID (dual-id fix).
    /// Keeps the key namespace independent of the SHORT live id so a
    /// session that was resumed (and re-pointed to a new live id) still
    /// resolves by its stable FULL key.
    func registerKey(uuid: UUID, sessionKey: String) {
        let key = uuid.uuidString
        if let oldKey = uuidToKey[key], oldKey != sessionKey {
            keyToUuid.removeValue(forKey: oldKey)
        }
        if let oldUuid = keyToUuid[sessionKey], oldUuid != key {
            uuidToKey.removeValue(forKey: oldUuid)
        }
        uuidToKey[key] = sessionKey
        keyToUuid[sessionKey] = key
        saveKeys()
    }

    /// The FULL gateway session key for a UUID, if known.
    func sessionKey(for uuid: UUID) -> String? {
        uuidToKey[uuid.uuidString]
    }

    /// Resolve a UUID by FULL gateway session key. Used by listSessions so
    /// rows carrying the FULL key map to the SAME app UUID the conversation
    /// already holds (prevents sidebar row UUID drift / lost highlight).
    func uuid(forKey key: String) -> UUID? {
        keyToUuid[key].flatMap(UUID.init)
    }

    /// Remove a UUID's FULL-key mapping too.
    func removeKey(uuid: UUID) {
        if let key = uuidToKey.removeValue(forKey: uuid.uuidString) {
            keyToUuid.removeValue(forKey: key)
            saveKeys()
        }
    }

    func remove(uuid: UUID) {
        if let native = uuidToNative.removeValue(forKey: uuid.uuidString) {
            nativeToUuid.removeValue(forKey: native)
            save()
        }
        removeKey(uuid: uuid)
    }

    private func save() {
        if let data = try? JSONEncoder().encode(uuidToNative) {
            UserDefaults.standard.set(data, forKey: Self.defaultsKey)
        }
        saveKeys()
    }

    private func saveKeys() {
        if let data = try? JSONEncoder().encode(uuidToKey) {
            UserDefaults.standard.set(data, forKey: Self.defaultsKeyFull)
        }
    }
}

/// Observable connection stages shown by the full-screen overlay while the
/// native gateway is connecting, reconnecting, or recovering.
enum ConnectionStage: String, CaseIterable, Sendable {
    case preparing       // minting ticket, setting up transport
    case contacting      // WebSocket opening
    case authenticating  // ticket accepted, server verifying identity
    case openingChannel  // WebSocket connected, running verification round-trip
    case verifying       // session.list probe in flight
    case restoring       // loading conversations, refreshing state
    case connected       // all done, overlay fades out

    var displayLabel: String {
        switch self {
        case .preparing:      "Connecting..."
        case .contacting:     "Contacting gateway"
        case .authenticating: "Authenticating"
        case .openingChannel: "Connecting..."
        case .verifying:      "Verifying session"
        case .restoring:      "Restoring conversations"
        case .connected:      "Connected"
        }
    }

    var icon: String {
        switch self {
        case .preparing:      "key.fill"
        case .contacting:     "arrow.triangle.2.circlepath"
        case .authenticating: "lock.shield.fill"
        case .openingChannel: "bolt.fill"
        case .verifying:      "checkmark.circle.fill"
        case .restoring:      "arrow.clockwise"
        case .connected:      "checkmark.circle.fill"
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

    /// Connector HTTP facade base URL (same scheme+host, port 8010 on LAN).
    /// Used for /gw/logs and /gw/logs/stream - the facade reads journald
    /// directly (no cli.exec 48K truncation) and answers native-gateway
    /// bearer tokens (dual-auth, same as /v1/native/watch).
    ///
    /// Public HTTPS hosts route /v1/push/register through Caddy on 443 -
    /// adding :8010 makes the URL unreachable from WAN (connection refused).
    /// Port 8010 is only correct for LAN hosts.
    func facadeBaseURLString() async -> String? {
        let base = gatewayBaseURL
        guard let url = URL(string: base), let host = url.host else { return nil }
        return Self.facadeBaseURL(for: base)
    }

    /// Current native-gateway access token for facade requests.
    func nativeAccessToken() async -> String? {
        await authCoordinator.currentAccessToken()
    }

    /// Build 127: whether this session authenticates via the gateway session
    /// cookie (basic / kallisti-pairing login) instead of a stored bearer.
    /// Gateway-control requests (restart / health) must ride the cookie in
    /// this mode — login deletes the keychain access token, so forcing a
    /// Bearer either fails outright or presents a stale token the connector
    /// rejects with 401.
    func usesCookieAuth() async -> Bool {
        await authCoordinator.usesCookieAuth()
    }

    /// Refreshed native-gateway access token, for connector facade calls
    /// (push registration, /gw/logs) that must present a LIVE bearer. The WS
    /// path refreshes via mintTicket, but facade HTTP calls were reading the
    /// raw stored token — an expired bearer 401s the connector even while the
    /// socket is healthy. Refresh is a no-op when the token is still valid;
    /// returns nil only when there is no stored session at all.
    func refreshAccessToken() async -> String? {
        try? await authCoordinator.refreshAccessTokenIfNeeded()
    }
    private let idMap = NativeSessionIdMap()
    private let transportFactory: @Sendable () -> any NativeGatewayTransport
    private(set) var client: NativeGatewayClient?
    private var transport: (any NativeGatewayTransport)?
    /// Stream handlers keyed by native session id. connect() replaces
    /// `client` with a fresh NativeGatewayClient, which would orphan any
    /// handler registered on the old client (its eventHandlers die with it).
    /// Re-registering from this registry on every connect keeps an in-flight
    /// stream alive across a suspension - the gateway parks the session,
    /// session.resume reattaches it, and the terminal event reaches a handler
    /// listening on the CURRENT client.
    private var activeStreamHandlers: [String: StreamEventHandler] = [:]
    private var reconnectTask: Task<Void, Never>?
    private var reconnectAttempt = 0
    /// Build 97: delays the yellow-flash to .reconnecting after an unexpected
    /// transport close so a single transient 1006 (proxy reap, brief cell
    /// handoff) does not visibly toggle the status dot. Cancelled by a
    /// successful connect() (or by another handleUnexpectedDisconnect) before
    /// the timer fires; if the timer DOES fire, the dot transitions to
    /// .reconnecting and the existing reconnect/backoff path takes over.
    private var reconnectGraceTask: Task<Void, Never>?
    /// Build 97: grace window before flipping to .reconnecting. Short enough
    /// that a real outage is visible within a second or two; long enough that
    /// a flaky-but-recovering WS never paints the dot yellow at all.
    private let reconnectGraceSeconds: TimeInterval = 3.0
    /// Build 97: jitter factor applied to the exponential backoff so multiple
    /// clients / reconnect storms don't synchronize. ±25% of the computed
    /// delay keeps the average the same while avoiding thundering-herd.
    private let reconnectJitterFactor: Double = 0.25
    /// Build 64: true while a connect() is in flight. connect() mints a
    /// fresh ticket and opens a NEW socket each call, so two racing connects
    /// (AppContainer launch trigger + scheduleReconnect backoff + a manual
    /// reset) open parallel sockets that kill each other mid-verification -
    /// the multi-socket churn seen server-side (8 sockets in 39s, each dying
    /// 1-64 messages later with 1006/1001/send_failed). The guard makes
    /// duplicate callers return immediately; the in-flight attempt's result
    /// is the one that stands.
    private var isConnecting = false
    /// prompt.submit is only an acknowledgement that the gateway accepted the
    /// turn. Model output arrives via stream events, so bound the ack rather
    /// than inheriting NativeGatewayClient's 60-second request default.
    private static let submitAckTimeoutNanos: UInt64 = 8_000_000_000
    /// Build 117: attachment upload RPC timeout. These RPCs carry base64-encoded
    /// image/file payloads over the WebSocket JSON-RPC channel, which can take
    /// well beyond 10s to encode, frame, transmit, and persist on the host for
    /// any non-trivial file. The previous 10s cap caused every sizable upload to
    /// surface as REQUESTTIMEOUT, so we now give uploads a generous 60s budget.
    private static let attachmentUploadTimeoutNanos: UInt64 = 60_000_000_000
    // Media directives cross the desktop -> gateway -> iOS boundary. Keep this
    // parser non-fatal: malformed text must never make launch depend on regex
    // compilation. The test suite exercises every supported extension.
    nonisolated(unsafe) private static let nativeMediaPattern = try? NSRegularExpression(
        pattern: #"MEDIA:\s*(?:`([^`\n]+)`|"([^"\n]+)"|'([^'\n]+)'|((?:~/|/)\S+(?:[^\S\n]+\S+)*?\.(?:png|jpe?g|gif|webp|pdf|mp4|mov|m4v|webm|mp3|m4a|wav|aac|zip|txt|md|csv|json|xml|ya?ml|docx?|xlsx?|pptx?|rtf)))(?=[\s`"',;:)\]}]|$)"#,
        options: [.caseInsensitive]
    )
    // Build 112: user messages with attachments come back from session.history
    // with `@image:`/`@file:` directive lines appended to the content (the
    // connector's directive convention, mirrored by _extract_directive_attachments
    // in session_store.py). Mirror the connector's regex so the app resolves the
    // same attachments the HTTP facade would have.
    // Same rule for connector-originated user attachment directives: parsing
    // failure degrades that one message, never the entire application.
    nonisolated(unsafe) private static let nativeDirectivePattern = try? NSRegularExpression(
        pattern: #"@(?:image|file):\s*(?:`([^`\n]+)`|"([^"\n]+)"|'([^'\n]+)'|(\S+))"#,
        options: [.caseInsensitive]
    )
    /// When the current socket was opened, used to tell a working connection
    /// that later dropped from one that never came up at all.
    private var connectedAt: Date?
    /// Build 55: set true the first time a connect() succeeds and NEVER reset
    /// on subsequent drops. The loading surface in AppRootView uses this to
    /// distinguish "cold launch, first connection attempt still resolving"
    /// (show the surface) from "mid-session reconnect churn" (stay in the
    /// chat UI). The old check (connectionStatus != .connecting) treated every
    /// reconnect as a cold launch, which tore down AdaptiveRootView, wiped the
    /// composer's @State draft text, and re-synced the stale Settings tab.
    private(set) var hasConnectedOnce = false
    /// Set by disconnect() so an intentional teardown isn't fought by the
    /// reconnect loop.
    private var isDeliberatelyDisconnected = false
    /// Tracks the granular stage of the connection lifecycle for the overlay.
    var connectionStage: ConnectionStage = .preparing
    /// Number of reconnect attempts in the current backoff cycle, exposed
    /// so the overlay can show "Reconnect attempt 3" when appropriate.
    var currentReconnectAttempt: Int { reconnectAttempt }

    /// Build 69: last measured round-trip to the connector (ms), or nil when
    /// not connected / measurement failed. Updated by measureLatency(); the
    /// Settings Connection section polls it while visible.
    var latencyMs: Int? = nil
    /// Timestamp of the last successful latency measurement.
    var latencyMeasuredAt: Date? = nil

    // MARK: - HeraldClientProtocol

    var connectionStatus: ConnectionStatus = .disconnected {
        didSet {
            if oldValue != connectionStatus {
                onConnectionStatusChanged?(connectionStatus)
            }
        }
    }
    /// Build 72: fired on every connectionStatus transition so the container
    /// can react to the REAL native signal (start/stop latency monitoring,
    /// reload models) instead of the legacy relay SSE which never fires for
    /// native-gateway users.
    var onConnectionStatusChanged: (@MainActor (ConnectionStatus) -> Void)?
    /// Build 104: fired on out-of-band `review.summary` events (self-improvement
    /// review summaries, memory updates) so ChatStore can append a faint
    /// in-transcript system line even when no stream is active. Stream
    /// handlers finish on message.complete, so post-turn reviews need this
    /// persistent connect-time listener to survive.
    var onSystemNotice: (@MainActor (String) -> Void)?
    var currentConversation: Conversation?
    private(set) var hasStoredLogin = false
    /// Build 76: gate that flips true only AFTER the init-resolution Task has
    /// finished probing `usesCookieAuth()` / `currentAccessToken()`. Until
    /// this flips, AppRootView must keep the loading surface mounted so the
    /// first SwiftUI render never sees hasStoredLogin=false on a returning
    /// user and flashes OnboardingFlowView for one frame before the init
    /// Task catches up. The init resolution is a cheap keychain read (no
    /// network) so this gates almost immediately on first launch, but the
    /// race window between view creation and Task hop was the splash
    /// glimpse the user saw. Never reset.
    private(set) var hasResolvedStoredLogin = false

    /// Typed feature calls (gateway status, model options, aux models) riding
    /// the SAME socket as chat. Stateless: each call fetches the current
    /// client so reconnects are handled transparently. The class is
    /// @MainActor, so the closure captures self.client directly.
    /// Build 128.45: fallback credential for the Config Editor when cookie
    /// auth is active and the session cookie has expired. AppContainer wires
    /// this to sessionStore.currentAccessToken() (the relay session token),
    /// which the connector accepts as a paired credential.
    var configFallbackTokenProvider: (@MainActor () async -> String?)? = nil

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
        } accessTokenProvider: { [weak self] in
            // Build 97: /v1/profiles (skill counts for the Hub) requires
            // native-or-paired auth on the connector facade.  Reuse the
            // same token refresh the chat client uses so cookie-auth and
            // bearer-auth modes both work.
            guard let self else { return nil }
            return try? await self.authCoordinator.refreshAccessTokenIfNeeded()
        } cookieAuthProvider: { [weak self] in
            // Build 128: when cookie auth is active the keychain bearer
            // is DELETED and the session cookie authenticates. The
            // feature client suppresses the Authorization header in
            // cookie-auth mode and lets URLSession.shared ride the cookie.
            await self?.usesCookieAuth() ?? false
        } fallbackTokenProvider: { [weak self] in
            // Build 128.45: relay session token fallback for the Config
            // Editor (cookie TTL is 12h; once expired, cookie-auth mode has
            // no other credential unless we hand it the relay token).
            await self?.configFallbackTokenProvider?()
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
        Task { @MainActor in
            // Returning-user detection at init. Cookie-auth modes (basic /
            // kallisti-pairing) DELETE the keychain access token on login and
            // ride the session cookie instead, so currentAccessToken() alone
            // misses them. Either a stored cookie auth mode OR a stored bearer
            // token means this device has logged in before - skip onboarding.
            //
            // Build 76: must set hasResolvedStoredLogin = true in BOTH
            // branches (and on any thrown error) before returning so
            // AppRootView's first render always sees the resolved state.
            // The cheap keychain reads below run synchronously in the same
            // actor hop the init runs on, so there is no observable delay -
            // but if either ever becomes async, do NOT forget to flip the
            // gate on every exit path or the loading surface becomes a
            // permanent splash on a fresh install.
            defer { hasResolvedStoredLogin = true }
            // Build 83: iOS preserves Keychain across uninstall, so a fresh
            // install can inherit a stale cookie-auth marker or bearer token
            // from a deleted build. That marker alone is NOT proof of a
            // returning user: with no relay URL configured there is no host
            // to authenticate against, and granting hasStoredLogin on the
            // stale marker alone bypasses onboarding into a dead-end chat.
            // Require BOTH a credential marker AND a user-configured relay.
            let hasCookieAuth = await authCoordinator.usesCookieAuth()
            let hasBearerToken = await authCoordinator.currentAccessToken() != nil
            let configuredBase = gatewayBaseURLProvider()
            let hasConfiguredRelay = !configuredBase.isEmpty
                && !configuredBase.contains("localhost")
            self.hasStoredLogin = (hasCookieAuth || hasBearerToken) && hasConfiguredRelay
            // Build 104: do NOT purge credentials at init when the relay reads
            // unresolved. The Build 83 purge wiped a legit paired user's
            // keychain whenever hasConfiguredRelay was false (missing relay at
            // init, or the localhost fallback sentinel), dumping them back into
            // onboarding. hasStoredLogin already gates on hasConfiguredRelay,
            // so stale creds with no relay can never bypass onboarding; the
            // keychain is harmlessly overwritten when the user re-pairs.
        }
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
              !deviceToken.isEmpty
        else { return }

        let body: [String: Any] = [
            "session_id": sessionId,
            "device_token": deviceToken,
            "token_kind": "alert",
        ]
        let status = await postFacadeJSON(
            path: "/v1/native/watch",
            body: body,
            logTag: "native watch registration"
        )
        if status != 200 {
            Self.logger.warning("native watch registration returned HTTP \(status)")
        }
    }

    /// Unregister a session watch once this client's own WS delivered the
    /// terminal event (message.complete). The connector watch exists only as
    /// a fallback push for turns the app missed while backgrounded/killed -
    /// if the app has the reply, pushing a "Turn complete" banner is noise.
    /// Mirror of registerNativeWatch with action=unwatch; fire-and-forget.
    private func unregisterNativeWatch(sessionId: String) async {
        guard let secureStore else { return }
        guard let deviceToken = await secureStore.retrieve(key: AppContainer.apnsTokenKeychainKey),
              !deviceToken.isEmpty
        else { return }

        let body: [String: Any] = [
            "session_id": sessionId,
            "device_token": deviceToken,
            "token_kind": "alert",
            "action": "unwatch",
        ]
        let status = await postFacadeJSON(
            path: "/v1/native/watch",
            body: body,
            logTag: "native watch unregistration"
        )
        if status != 200 {
            Self.logger.warning("native watch unregistration returned HTTP \(status)")
        }
    }

    /// POST a JSON body to the connector's HTTP facade with the live native
    /// bearer. On 401 we force a bearer rotation (the cached `expiresAt` may
    /// be stale even though the local check considers the token valid) and
    /// retry exactly once before giving up. Returns the HTTP status of the
    /// final attempt: a 200 means accepted, 401 means session is gone, any
    /// other status is logged but not fatal so callers can decide.
    ///
    /// Centralizing this avoids the original bug pattern where
    /// `registerPushToken` and `registerNativeWatch` each read the raw stored
    /// bearer (or only refreshed it once) and gave up on the first 401 --
    /// leaving Settings stuck on "Push: Not Registered" until the next app
    /// launch forced a fresh `mintTicket` -> `refreshAccessTokenIfNeeded`.
    private func postFacadeJSON(
        path: String,
        body: [String: Any],
        logTag: String
    ) async -> Int {
        guard let facadeBase = Self.facadeBaseURL(for: gatewayBaseURL),
              let url = URL(string: "\(facadeBase)\(path)")
        else {
            Self.logger.warning("\(logTag): invalid facade URL")
            return -1
        }
        // Build 66: cookie-auth sessions (basic / kallisti-pairing) authenticate
        // facade calls with the gateway session cookie URLSession carries
        // automatically - the connector accepts gateway cookies and delegates
        // verification to /api/auth/me. Pairing login deletes the keychain
        // access token, so the old code path could never mint a Bearer here
        // and push registration died with -1/401.
        let cookieAuth = await authCoordinator.usesCookieAuth()
        if cookieAuth {
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.timeoutInterval = 8
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try? JSONSerialization.data(withJSONObject: body)
            do {
                let (_, response) = try await URLSession.shared.data(for: request)
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                Self.logger.info("\(logTag): cookie-auth status \(status)")
                return status
            } catch {
                Self.logger.warning("\(logTag) failed: \(error.localizedDescription)")
                return -1
            }
        }
        var attempt = 0
        var lastStatus: Int = -1
        while attempt < 2 {
            attempt += 1
            // First attempt uses the cached token (which refreshAccessTokenIfNeeded
            // will rotate if expired); the retry path uses forceRefreshAccessToken
            // to bypass the cached expiresAt after a server-side 401.
            let tokenResult: String?
            if attempt == 1 {
                tokenResult = try? await authCoordinator.refreshAccessTokenIfNeeded()
            } else {
                tokenResult = try? await authCoordinator.forceRefreshAccessToken()
            }
            guard let accessToken = tokenResult, !accessToken.isEmpty else {
                Self.logger.warning("\(logTag): no native access token")
                return -1
            }
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.timeoutInterval = 8
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
            request.httpBody = try? JSONSerialization.data(withJSONObject: body)

            do {
                let (_, response) = try await URLSession.shared.data(for: request)
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                lastStatus = status
                if status == 401 && attempt == 1 {
                    // Cached expiresAt is stale; force a real rotation and retry.
                    Self.logger.info("\(logTag): 401 on first attempt, forcing bearer rotation")
                    continue
                }
                return status
            } catch {
                Self.logger.warning("\(logTag) failed: \(error.localizedDescription)")
                return lastStatus >= 0 ? lastStatus : -1
            }
        }
        return lastStatus
    }

    /// POST to the connector facade and return the decoded response body
    /// (build 69 / r7). Same auth flow as postFacadeJSON (cookie-auth
    /// sessions ride the gateway cookie; bearer sessions rotate once on
    /// 401) but returns Data instead of discarding it. Used by
    /// updateCheck() to read /v1/gw/update/check's structured payload.
    private func postFacadeJSONData(
        path: String,
        body: [String: Any],
        logTag: String
    ) async -> Data? {
        guard let facadeBase = Self.facadeBaseURL(for: gatewayBaseURL),
              let url = URL(string: "\(facadeBase)\(path)")
        else {
            Self.logger.warning("\(logTag): invalid facade URL")
            return nil
        }
        let cookieAuth = await authCoordinator.usesCookieAuth()
        if cookieAuth {
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.timeoutInterval = 15
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try? JSONSerialization.data(withJSONObject: body)
            do {
                let (data, response) = try await URLSession.shared.data(for: request)
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                Self.logger.info("\(logTag): cookie-auth status \(status)")
                guard status == 200 else { return nil }
                return data
            } catch {
                Self.logger.warning("\(logTag) failed: \(error.localizedDescription)")
                return nil
            }
        }
        var attempt = 0
        while attempt < 2 {
            attempt += 1
            let tokenResult: String?
            if attempt == 1 {
                tokenResult = try? await authCoordinator.refreshAccessTokenIfNeeded()
            } else {
                tokenResult = try? await authCoordinator.forceRefreshAccessToken()
            }
            guard let accessToken = tokenResult, !accessToken.isEmpty else {
                Self.logger.warning("\(logTag): no native access token")
                return nil
            }
            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.timeoutInterval = 15
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
            request.httpBody = try? JSONSerialization.data(withJSONObject: body)
            do {
                let (data, response) = try await URLSession.shared.data(for: request)
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                if status == 401 && attempt == 1 {
                    Self.logger.info("\(logTag): 401 on first attempt, forcing bearer rotation")
                    continue
                }
                guard status == 200 else {
                    Self.logger.warning("\(logTag): HTTP \(status)")
                    return nil
                }
                return data
            } catch {
                Self.logger.warning("\(logTag) failed: \(error.localizedDescription)")
                return nil
            }
        }
        return nil
    }

    /// GET from the connector facade and return the decoded response body.
    /// Mirrors postFacadeJSONData's auth flow (cookie-auth rides the gateway
    /// cookie; bearer sessions rotate once on 401) but for read-only GET.
    private func getFacadeJSON(path: String, logTag: String) async -> Data? {
        guard let facadeBase = Self.facadeBaseURL(for: gatewayBaseURL),
              let url = URL(string: "\(facadeBase)\(path)")
        else {
            Self.logger.warning("\(logTag): invalid facade URL")
            return nil
        }
        let cookieAuth = await authCoordinator.usesCookieAuth()
        if cookieAuth {
            var request = URLRequest(url: url)
            request.httpMethod = "GET"
            request.timeoutInterval = 15
            do {
                let (data, response) = try await URLSession.shared.data(for: request)
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                guard status == 200 else {
                    Self.logger.warning("\(logTag): cookie-auth HTTP \(status)")
                    return nil
                }
                return data
            } catch {
                Self.logger.warning("\(logTag) failed: \(error.localizedDescription)")
                return nil
            }
        }
        var attempt = 0
        while attempt < 2 {
            attempt += 1
            let tokenResult: String?
            if attempt == 1 {
                tokenResult = try? await authCoordinator.refreshAccessTokenIfNeeded()
            } else {
                tokenResult = try? await authCoordinator.forceRefreshAccessToken()
            }
            guard let accessToken = tokenResult, !accessToken.isEmpty else {
                Self.logger.warning("\(logTag): no native access token")
                return nil
            }
            var request = URLRequest(url: url)
            request.httpMethod = "GET"
            request.timeoutInterval = 15
            request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
            do {
                let (data, response) = try await URLSession.shared.data(for: request)
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                if status == 401 && attempt == 1 {
                    Self.logger.info("\(logTag): 401 on first attempt, forcing bearer rotation")
                    continue
                }
                guard status == 200 else {
                    Self.logger.warning("\(logTag): HTTP \(status)")
                    return nil
                }
                return data
            } catch {
                Self.logger.warning("\(logTag) failed: \(error.localizedDescription)")
                return nil
            }
        }
        return nil
    }

    /// Fetch real per-row timestamps from the connector facade and return a
    /// rowId -> Date map. Empty map on any failure; the caller falls back to
    /// `.now` for rows it cannot stamp.
    private func loadHistoryTimestamps(nativeId: String) async -> [Int: Date] {
        struct Stamp: Decodable {
            let rowId: Int?
            let timestamp: String?
        }
        // The connector wraps every JSON body as {"data":...,"meta":...} via
        // envelope_middleware, so the messages array lives one level deep.
        struct Envelope: Decodable {
            struct DataBox: Decodable {
                let messages: [Stamp]?
            }
            let data: DataBox?
        }
        guard let data = await getFacadeJSON(
            path: "/v1/sessions/\(nativeId)/messages",
            logTag: "loadHistoryTimestamps"
        ) else { return [:] }
        guard let envelope = try? JSONDecoder().decode(Envelope.self, from: data),
              let stamps = envelope.data?.messages else { return [:] }
        var map: [Int: Date] = [:]
        for stamp in stamps {
            guard let rid = stamp.rowId,
                  let ts = stamp.timestamp,
                  let date = Self.parseServerTimestamp(ts) else { continue }
            map[rid] = date
        }
        return map
    }

    /// Resolve a history message's timestamp: server value first, then the
    /// connector's real state.db timestamp keyed by row id, then `.now`.
    nonisolated static func historyTimestamp(
        for msg: NativeHistoryMessage,
        rowTimestamps: [Int: Date]
    ) -> Date {
        if let ts = msg.timestamp.flatMap({ parseServerTimestamp($0) }) {
            return ts
        }
        if let rid = msg.rowId, let ts = rowTimestamps[rid] {
            return ts
        }
        return .now
    }

    /// Parse an ISO 8601 timestamp, tolerating both fractional and whole
    /// seconds (the connector emits `datetime.isoformat()`, which includes
    /// microseconds whenever the stored epoch has a fractional part).
    nonisolated static func parseServerTimestamp(_ s: String) -> Date? {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = fractional.date(from: s) { return date }
        return ISO8601DateFormatter().date(from: s)
    }

    /// Stable identity for a Hermes history row. The native gateway does not
    /// expose a UUID, but it does expose the immutable state.db row id. Using a
    /// deterministic UUID prevents each refresh from turning every historical
    /// message into a brand-new row in ChatStore's reconciliation pass.
    nonisolated static func historyMessageID(
        nativeSessionID: String,
        rowID: Int?,
        ordinal: Int,
        role: String,
        content: String
    ) -> UUID {
        let identity: String
        if let rowID {
            identity = "kallisti-history|\(nativeSessionID)|row|\(rowID)"
        } else {
            // Legacy gateways may omit row_id. Ordinal is stable because
            // session.history is authoritative transcript order; role/content
            // keep different projections from colliding across malformed rows.
            identity = "kallisti-history|\(nativeSessionID)|ordinal|\(ordinal)|\(role)|\(content)"
        }
        var bytes = Array(SHA256.hash(data: Data(identity.utf8)).prefix(16))
        bytes[6] = (bytes[6] & 0x0f) | 0x50
        bytes[8] = (bytes[8] & 0x3f) | 0x80
        return UUID(uuid: (
            bytes[0], bytes[1], bytes[2], bytes[3],
            bytes[4], bytes[5], bytes[6], bytes[7],
            bytes[8], bytes[9], bytes[10], bytes[11],
            bytes[12], bytes[13], bytes[14], bytes[15]
        ))
    }

    /// Build 69 (r7): pull the structured update check from the connector.
    /// Returns nil on auth/network failure or when the endpoint has no data.
    /// The payload shape (envelope-wrapped) matches the connector's
    /// gateway_update_check response:
    ///   { "data": { "components": { "hermes-agent": { currentVersion,
    ///     latestVersion, updateAvailable, behindCount, releaseURL,
    ///     changelog, lastCheckedAt, error } } } }
    struct HermesUpdateInfo: Decodable {
        var currentVersion: String?
        var latestVersion: String?
        var updateAvailable: Bool?
        var behindCount: Int?
        var releaseURL: String?
        var changelog: String?
        // Build 128.49: structured commits (sha/summary/author/at) from the
        // connector so the changelog sheet can group "Added" / "Fixed" by
        // conventional-commit type instead of dumping raw oneline text.
        var commits: [UpdateCommit]?
        var lastCheckedAt: String?
        var error: String?
    }

    /// One commit the install is behind origin/main by.
    struct UpdateCommit: Decodable {
        var sha: String?
        var summary: String?
        var author: String?
        var at: Int?
    }

    /// Live state of an in-flight Hermes update (Build 128.49).
    struct UpdateProgress: Decodable {
        var state: String?        // idle | running | done | failed
        var exitCode: Int?
        var pid: Int?
        var startedAt: String?
        var finishedAt: String?
        var output: [String]?
        var error: String?

        var isRunning: Bool { state == "running" }
        var isDone: Bool { state == "done" }
        var isFailed: Bool { state == "failed" }
    }

    func updateCheck() async -> HermesUpdateInfo? {
        struct Envelope: Decodable {
            struct DataBox: Decodable {
                struct ComponentsBox: Decodable {
                    let hermesAgent: HermesUpdateInfo?
                    enum CodingKeys: String, CodingKey { case hermesAgent = "hermes-agent" }
                }
                let components: ComponentsBox?
            }
            let data: DataBox?
        }
        guard let payload = await postFacadeJSONData(
            path: "/v1/gw/update/check",
            body: [:],
            logTag: "updateCheck"
        ) else { return nil }
        do {
            let envelope = try JSONDecoder().decode(Envelope.self, from: payload)
            return envelope.data?.components?.hermesAgent
        } catch {
            Self.logger.warning("updateCheck: decode failed: \(error.localizedDescription)")
            return nil
        }
    }

    /// Build 128.49: start `hermes update --yes` on the host in the
    /// background. The connector runs in its own venv, so it survives the
    /// update and can stream progress via updateProgress(). Returns the
    /// initial progress snapshot, or nil on auth/network failure.
    func startUpdate() async -> UpdateProgress? {
        struct Envelope: Decodable {
            struct DataBox: Decodable {
                let state: String?
                let alreadyRunning: Bool?
                let pid: Int?
                let output: [String]?
            }
            let data: DataBox?
        }
        guard let payload = await postFacadeJSONData(
            path: "/v1/gw/update/apply",
            body: ["target": "hermes-agent"],
            logTag: "updateApply"
        ) else { return nil }
        do {
            let envelope = try JSONDecoder().decode(Envelope.self, from: payload)
            guard let data = envelope.data else { return nil }
            return UpdateProgress(
                state: data.state,
                exitCode: nil,
                pid: data.pid,
                startedAt: nil,
                finishedAt: nil,
                output: data.output,
                error: nil
            )
        } catch {
            Self.logger.warning("startUpdate: decode failed: \(error.localizedDescription)")
            return nil
        }
    }

    /// Build 128.49: poll the in-flight Hermes update for live output lines
    /// and terminal state (done/failed + exit code).
    func updateProgress() async -> UpdateProgress? {
        struct Envelope: Decodable {
            struct DataBox: Decodable {
                let state: String?
                let exitCode: Int?
                let pid: Int?
                let startedAt: String?
                let finishedAt: String?
                let output: [String]?
                let error: String?
            }
            let data: DataBox?
        }
        guard let payload = await getFacadeJSON(
            path: "/v1/gw/update/progress",
            logTag: "updateProgress"
        ) else { return nil }
        do {
            let envelope = try JSONDecoder().decode(Envelope.self, from: payload)
            guard let data = envelope.data else { return nil }
            return UpdateProgress(
                state: data.state,
                exitCode: data.exitCode,
                pid: data.pid,
                startedAt: data.startedAt,
                finishedAt: data.finishedAt,
                output: data.output,
                error: data.error
            )
        } catch {
            Self.logger.warning("updateProgress: decode failed: \(error.localizedDescription)")
            return nil
        }
    }

    /// Register the APNs device token with the connector's push registration
    /// endpoint using native-gateway authentication. Public traffic routes
    /// through the gateway host on port 443 (routed by Caddy); port 8010 is
    /// only correct for LAN hosts.
    ///
    /// Returns true if the connector accepted the registration.
    func registerPushToken(_ token: String, pushEnvironment: String) async -> Bool {
        guard let facadeBase = await facadeBaseURLString() else {
            Self.logger.warning("registerPushToken: invalid facade URL")
            return false
        }
        let body: [String: Any] = [
            "apnsToken": token,
            "pushEnvironment": pushEnvironment,
            "bundleId": Bundle.main.bundleIdentifier ?? "net.fihonline.kallisti",
            "tokenKind": "device",
            // Build 67: send the install UUID so the connector keeps this
            // device's token separate from other devices (iPad vs iPhone).
            // Matches the installationID the pairing flow uses.
            "installationId": AppContainer.sharedDefault().sessionStore.state.installationID.uuidString.lowercased(),
        ]
        // Build 65: route through `postFacadeJSON` so a stale cached bearer
        // (server-side expired while the local `expiresAt` is still ahead)
        // gets one forced-rotation retry instead of logging "HTTP 401" and
        // leaving Settings on "Not Registered" until next app launch. The
        // connector delegates bearer verification to the gateway's
        // `/api/auth/me`, so a dead bearer is indistinguishable from no
        // token from the app's side.
        let status = await postFacadeJSON(
            path: "/v1/push/register",
            body: body,
            logTag: "registerPushToken"
        )
        if status == 200 {
            Self.logger.info("registerPushToken: accepted (environment=\(pushEnvironment))")
            return true
        }
        Self.logger.warning("registerPushToken: HTTP \(status)")
        return false
    }

    /// Register the ActivityKit push-to-update token for the CURRENT Live
    /// Activity (tokenKind=liveActivity). Separate from the device's alert
    /// token: ActivityKit mints a fresh token per activity instance, and the
    /// connector stores it in live_activity_push_token so it can remotely
    /// end the lock-screen activity when the turn completes.
    ///
    /// Build 96: MUST use the cookie-aware postFacadeJSON path. The previous
    /// implementation built a raw Bearer URLRequest from AppContainer; in
    /// cookie-auth mode (basic / kallisti-pairing) there is no bearer token,
    /// so registration silently no-op'd, the connector kept a stale token
    /// from an ended activity, and every end-push 410'd - lock screen stuck
    /// on "Thinking...". Same bug family as b93 sensor uploads.
    func registerLiveActivityToken(_ token: String, pushEnvironment: String) async -> Bool {
        guard let facadeBase = await facadeBaseURLString() else {
            Self.logger.warning("registerLiveActivityToken: invalid facade URL")
            return false
        }
        let body: [String: Any] = [
            "apnsToken": token,
            "pushEnvironment": pushEnvironment,
            "bundleId": Bundle.main.bundleIdentifier ?? "net.fihonline.kallisti",
            "tokenKind": "liveActivity",
            "installationId": AppContainer.sharedDefault().sessionStore.state.installationID.uuidString.lowercased(),
        ]
        let status = await postFacadeJSON(
            path: "/v1/push/register",
            body: body,
            logTag: "registerLiveActivityToken"
        )
        if status == 200 {
            Self.logger.info("registerLiveActivityToken: accepted (environment=\(pushEnvironment))")
            return true
        }
        Self.logger.warning("registerLiveActivityToken: HTTP \(status)")
        return false
    }

    func connect() async {
        // Build 64: collapse concurrent connect() calls. Each connect() mints
        // a fresh ticket + socket, so racing callers (launch trigger,
        // scheduleReconnect backoff, manual reset) opened parallel sockets
        // that killed each other mid-verification. A duplicate caller
        // returns immediately - the in-flight attempt sets connectionStatus
        // when it settles, so the UI converges on that single result.
        guard !isConnecting else {
            Self.logger.info("connect() skipped - another connect is already in flight")
            return
        }
        isConnecting = true
        defer { isConnecting = false }
        connectionStatus = .connecting
        connectionStage = .preparing
        // Torn down in the catch block if the verification round-trip
        // below fails, so a socket that "opened" but never proved itself
        // doesn't linger open in the background on every backoff retry.
        var provisionalClient: NativeGatewayClient?
        do {
            // Tickets are single-use with a 30s TTL, so every connect --
            // including each reconnect attempt -- mints a fresh one.
            connectionStage = .authenticating
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

            connectionStage = .contacting
            try await client.connect(url: wsURL)
            // task.resume() never throws, so reaching this line does not
            // prove the socket actually opened -- a rejected ws-ticket or a
            // routing hiccup looks identical to success until something is
            // actually sent. Verify with a cheap round-trip before this
            // client is published and connectionStatus says .connected;
            // a socket that's really dead fails here and falls through to
            // the same catch as any other connect failure below instead of
            // a false "connected" that dies on the very first real request.
            //
            // LATENCY (build 34): this verification probe used to inherit
            // the client's default 60s requestTimeoutNanos. connect() is
            // ITSELF the fallback path reconnectIfNeeded() takes after its
            // fast 5s phantom-socket probe fails -- so a routing hiccup here
            // (e.g. the fresh socket opens but the reverse proxy silently
            // black-holes it) could hang another full 60s on top of the 8s
            // ticket-mint timeout, stacking toward the ~85-120s dead window
            // users saw on follow-up sends after backgrounding. Use the same
            // short probe timeout as every other liveness check in this file.
            connectionStage = .openingChannel
            connectionStage = .verifying
            _ = try await client.send(method: "session.most_recent", params: [String: String](), timeoutNanos: Self.probeTimeoutNanos)

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
            // Build 97: a successful connect cancels any pending
            // .reconnecting grace transition so the dot never flashes yellow.
            cancelReconnectGrace()
            // Build 52 (sleep recovery): a reconnect swaps in a fresh
            // NativeGatewayClient whose eventHandlers are empty. Re-register
            // every still-live stream handler so terminal events for a
            // resumed (parked) session reach the consumer instead of hanging
            // until the absolute job deadline.
            for (sid, handler) in activeStreamHandlers {
                await client.onEvent { event in
                    handler.handle(event)
                }
            }
            // Prune handlers that completed while we were disconnected (their
            // continuation already got .finished; keeping them would re-run a
            // stale message.complete if the gateway replayed it).
            activeStreamHandlers = activeStreamHandlers.filter { !$0.value.isCompleted }
            // Build 104: persistent out-of-band listener for review.summary
            // (self-improvement review / memory updates). Stream handlers die
            // on message.complete, so a review fired post-turn would otherwise
            // be dropped. Registered on the CURRENT client so a reconnect
            // re-arms it; hops to MainActor before firing the callback.
            await client.onEvent { [weak self] event in
                guard event.params.type == "review.summary" else { return }
                let text = event.params.decodePayload(NativeReviewSummaryPayload.self)?.text ?? ""
                guard !text.isEmpty else { return }
                Task { @MainActor in
                    self?.onSystemNotice?(text)
                }
            }
            // NOT resetting reconnectAttempt here: a socket can pass the
            // verification above and still be new/flaky. The counter is
            // reset only once a connection has proven itself durable (see
            // handleUnexpectedDisconnect below).
            connectedAt = Date()
            hasStoredLogin = true
            hasConnectedOnce = true
            connectionStatus = .connected
            connectionStage = .connected
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
            // Build 97: defer the .reconnecting flip via the same grace task
            // handleUnexpectedDisconnect uses, so a verification probe that
            // fails on a flaky-but-recovering socket does not toggle the dot.
            // The scheduleReconnect() below still fires immediately so the
            // next attempt is not gated on the grace window.
            if hasStoredLogin {
                cancelReconnectGrace()
                scheduleReconnectGrace()
            } else {
                connectionStatus = .disconnected
            }
            if !hasStoredLogin {
                hasConnectedOnce = false
            }
            // Build 60: only reset stage on cold launch. After first connect,
            // the stage stays .connected and the connection banner handles state.
            if !hasConnectedOnce {
                connectionStage = .preparing
            }
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
        Self.logger.warning("Native gateway transport closed unexpectedly - reconnecting")
        // Build 97: defer the .reconnecting (yellow dot) flip by a short
        // grace window. A rapid reconnect within `reconnectGraceSeconds`
        // cancels this task and the dot stays green -- eliminates the
        // green<->yellow flapping from WS reconnect churn (e.g. 32 abnormal
        // 1006 closures in 5 min from proxy idle reaps). If the reconnect
        // does NOT succeed in time, the task fires and we paint yellow as
        // before, so a real outage is still visible.
        cancelReconnectGrace()
        scheduleReconnectGrace()
        // Build 60: only reset stage on cold launch.
        if !hasConnectedOnce {
            connectionStage = .preparing
        }
        // Build 128.45 (desync fix): terminate every in-flight stream
        // handler before the transport is replaced. The socket is dead; the
        // gateway will not replay the turn's terminal events on the
        // reconnected socket, so leaving handlers registered left the chat
        // consumer hanging forever (iPad: stale stop button + thinking
        // bubble while the server kept streaming - Electron showed the real
        // state). Terminating with a bare .failed lets ChatStore keep the
        // placeholder and hand off to job-status polling, which recovers
        // the response when the server finishes.
        for (_, handler) in activeStreamHandlers {
            handler.terminate(reason: "Connection lost")
        }
        activeStreamHandlers.removeAll()
        client = nil
        transport = nil
        scheduleReconnect()
    }

    /// Build 97: schedule a deferred .reconnecting transition that fires
    /// only if the reconnect hasn't succeeded within the grace window. The
    /// task is cancelled by a successful connect() or another unexpected
    /// disconnect before it runs.
    private func scheduleReconnectGrace() {
        reconnectGraceTask = Task { @MainActor [weak self] in
            try? await Task.sleep(for: .seconds(self?.reconnectGraceSeconds ?? 3.0))
            guard let self, !Task.isCancelled else { return }
            // Only flip to .reconnecting if we're not already back up.
            // connect()'s success path will have cancelled this task and
            // set .connected; if we get here while disconnected we still
            // honor the user's intent (a stale grace firing during a
            // deliberate disconnect is a no-op because disconnect() sets
            // .disconnected and ignores later flips).
            guard self.connectionStatus != .connected else { return }
            self.connectionStatus = .reconnecting
        }
    }

    /// Build 97: cancel any pending deferred .reconnecting transition. Called
    /// on successful connect() (dot stays green) and on a fresh unexpected
    /// disconnect (the newest close owns the grace window).
    private func cancelReconnectGrace() {
        reconnectGraceTask?.cancel()
        reconnectGraceTask = nil
    }

    /// Build 69: measure round-trip latency to the connector via a cheap
    /// session.list probe (limit 1, short timeout). Returns ms or nil if the
    /// socket isn't live. Safe to call on a timer - it never mints a ticket
    /// or opens a socket, it reuses the live client.
    func measureLatency() async -> Int? {
        guard let client, connectionStatus == .connected else {
            latencyMs = nil
            latencyMeasuredAt = nil
            return nil
        }
        let clock = ContinuousClock()
        do {
            let start = clock.now
            _ = try await client.send(
                method: "session.most_recent",
                params: [String: String](),
                timeoutNanos: Self.probeTimeoutNanos
            )
            let elapsed = clock.now - start
            let ms = Int(elapsed.components.attoseconds / 1_000_000_000_000_000)
            latencyMs = ms
            latencyMeasuredAt = Date()
            return ms
        } catch {
            // Keep the last successful measurement visible while the background
            // monitor retries. A transient socket miss is not a zero-latency nor
            // an unknown-host state.
            return nil
        }
    }

    private func scheduleReconnect() {
        guard !isDeliberatelyDisconnected else { return }
        guard reconnectTask == nil else { return }
        let attempt = reconnectAttempt
        reconnectAttempt += 1
        // 1s, 2s, 4s ... capped at 30s.
        let baseDelay = min(pow(2.0, Double(attempt)), 30.0)
        // Build 97: apply ±25% jitter so multiple clients / reconnect storms
        // don't synchronize their backoff curves. Thundering-herd is what
        // makes the WS server flap visible at the dot level.
        let jitter = (Double.random(in: -reconnectJitterFactor...reconnectJitterFactor)) * baseDelay
        let delaySeconds = max(0.25, baseDelay + jitter)
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

    /// Cancel in-flight reconnect work, tear down stale transport/client
    /// state, and immediately begin a fresh authenticated connection.
    /// Idempotent: concurrent calls are no-ops. Does not delete stored
    /// credentials or conversations -- only transient connection state.
    func resetConnection() async {
        guard !isResetting else { return }
        isResetting = true
        defer { isResetting = false }

        // Cancel any queued reconnect.
        reconnectTask?.cancel()
        reconnectTask = nil
        isDeliberatelyDisconnected = false
        reconnectAttempt = 0

        // Tear down stale client/transport.
        await client?.close()
        client = nil
        transport = nil
        connectedAt = nil

        // Build 83: Reset always purges local credentials and auth state,
        // regardless of the stored-login flag. The old gate (only clear when
        // !hasStoredLogin) was backwards: a device that LOOKS logged in via a
        // stale keychain marker never cleared anything, so Reset appeared to
        // do nothing. Every Reset is a deliberate wipe back to onboarding.
        await authCoordinator.clearLocalCredentials()
        hasConnectedOnce = false
        hasStoredLogin = false

        // Start a fresh authenticated connection.
        connectionStatus = .connecting
        if !hasConnectedOnce {
            connectionStage = .preparing
        }
        await connect()
    }

    /// Guard against overlapping resetConnection() calls.
    private var isResetting = false

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
                    method: "session.most_recent",
                    params: [String: String](),
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

    /// Liveness probe timeout: 8 seconds. Short enough that a phantom dead
    /// socket is detected before the user notices, long enough that a
    /// genuinely slow gateway doesn't cause spurious reconnects.
    private static let probeTimeoutNanos: UInt64 = 8_000_000_000

    /// Drives the interactive Nous OAuth/PKCE login (browser handoff) and
    /// then retries `connect()`. Called from onboarding's "Open app" step -
    /// `connect()` alone can never trigger this itself, since a failed
    /// mintTicket() there has nowhere to present a login screen from.
    /// Redeems the connector-issued one-time pairing code, then connects through
    /// the normal cookie-backed WebSocket ticket flow.
    func startPairingLogin(code: String, installationID: UUID) async throws {
        try await authCoordinator.loginWithPairingCode(code, installationID: installationID)
        await connect()
        guard connectionStatus == .connected else {
            throw NativeAuthError.connectFailedAfterLogin
        }
    }

    func startBasicLogin(username: String, password: String) async throws {
        try await authCoordinator.loginWithBasic(username: username, password: password)
        await connect()
        guard connectionStatus == .connected else {
            throw NativeAuthError.connectFailedAfterLogin
        }
    }

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
            // Build 53: honor the All Devices toggle client-side. The native
            // gateway has no server-side device scoping (one profile = one
            // shared session DB), so session.list returns everything and the
            // toggle was a no-op on iPad / hidden on iPhone. The app tags its
            // own sessions with source "ios" at create time (legacy app
            // sessions are "tui"), so "this device" = sessions the app owns.
            if !allDevices {
                let src = (native.source ?? "").lowercased()
                if src != "ios" && src != "tui" {
                    continue
                }
            }
            // CRITICAL: resolve the app-side UUID AND register the reverse
            // mapping when it's missing. Previously listSessions minted a
            // fresh UUID() that was NEVER registered in idMap, so any
            // interaction with a resumed session (delete, rename, follow-up
            // message, model switch) failed with "session not found" / 4001
            // or created a brand-new server session (context loss + cold
            // start latency on every follow-up).
            // Dual-id fix: session.list rows carry the FULL key as sessionId.
            // Resolve the UUID by FULL key FIRST so the row maps to the SAME
            // app UUID the conversation already holds (launch highlight),
            // then fall back to the SHORT-id map, then mint + register both.
            let uuid: UUID
            if let existing = await idMap.uuid(forKey: native.sessionId) {
                uuid = existing
            } else if let existing = await idMap.uuid(for: native.sessionId) {
                uuid = existing
                await idMap.registerKey(uuid: uuid, sessionKey: native.sessionId)
            } else {
                uuid = UUID()
                await idMap.register(uuid: uuid, nativeId: native.sessionId)
                await idMap.registerKey(uuid: uuid, sessionKey: native.sessionId)
            }
            sessions.append(SessionSummary(
                id: uuid,
                title: native.title ?? "Untitled",
                previewText: native.previewText ?? "",
                lastActivity: native.lastActivity.flatMap { ISO8601DateFormatter().date(from: $0) } ?? .now,
                isPinned: native.isPinned ?? false,
                isArchived: native.isArchived ?? false,
                sessionKey: native.sessionId
            ))
        }
        return SessionListResponse(sessions: sessions, total: decoded.total ?? sessions.count)
    }

    /// Blue-dot source: session keys with an active turn in flight.
    ///
    /// Probes `session.active_list` (live in-memory sessions only) and
    /// returns the FULL gateway session keys whose status is `working`
    /// (or waiting/starting - anything not idle). The sidebar matches
    /// these against each row's `sessionKey` to render the activity dot.
    /// Returns empty on any failure - the dot is best-effort decoration,
    /// never worth an error state.
    func activeSessionKeys() async -> Set<String> {
        guard let client, connectionStatus == .connected else { return [] }
        do {
            let response = try await client.send(
                method: "session.active_list",
                params: [String: String](),
                timeoutNanos: Self.probeTimeoutNanos
            )
            if let error = response.error {
                Logger.app.debug("activeSessionKeys: active_list error \(error)")
                return []
            }
            guard let result = response.result else { return [] }
            let data = try JSONEncoder().encode(result)
            let decoded = try JSONDecoder().decode(NativeActiveSessionListResult.self, from: data)
            let active = decoded.sessions.filter { item in
                let status = (item.status ?? "").lowercased()
                return status != "idle" && !status.isEmpty
            }
            let keys = Set(active.compactMap { $0.sessionKey })
            if !keys.isEmpty {
                Logger.app.debug("activeSessionKeys: \(keys.count) working session(s)")
            }
            return keys
        } catch {
            Logger.app.debug("activeSessionKeys: probe failed \(error.localizedDescription)")
            return []
        }
    }

    func searchSessions(query: String, allDevices: Bool) async throws -> [SessionSummary] {
        guard let client else { throw NativeGatewayClientError.notConnected }
        let response = try await client.send(method: "session.search", params: ["query": query])
        if let error = response.error { throw error }
        guard let result = response.result else { return [] }
        let data = try JSONEncoder().encode(result)
        let decoded = try JSONDecoder().decode(NativeSessionListResult.self, from: data)
        // Build 53: mirror the listSessions filter so search honors the
        // All Devices toggle too. Also resolve/register the UUID the
        // same way listSessions does (CRITICAL: never mint unregistered
        // UUIDs - they 4001 on follow-up). Note: async idMap access means
        // this must be a for-loop, not compactMap (Swift 6 concurrency).
        var sessions: [SessionSummary] = []
        for native in decoded.sessions {
            if let source = native.source?.lowercased(), source == "cron" { continue }
            if !allDevices {
                let src = (native.source ?? "").lowercased()
                if src != "ios" && src != "tui" { continue }
            }
            let uuid: UUID
            if let existing = await idMap.uuid(forKey: native.sessionId) {
                uuid = existing
            } else if let existing = await idMap.uuid(for: native.sessionId) {
                uuid = existing
                await idMap.registerKey(uuid: uuid, sessionKey: native.sessionId)
            } else {
                uuid = UUID()
                await idMap.register(uuid: uuid, nativeId: native.sessionId)
                await idMap.registerKey(uuid: uuid, sessionKey: native.sessionId)
            }
            sessions.append(SessionSummary(
                id: uuid,
                title: native.title ?? "Untitled",
                previewText: native.previewText ?? "",
                lastActivity: native.lastActivity.flatMap { ISO8601DateFormatter().date(from: $0) } ?? .now,
                isPinned: native.isPinned ?? false,
                isArchived: native.isArchived ?? false,
                sessionKey: native.sessionId
            ))
        }
        return sessions
    }

    /// Protocol-conforming overload (HeraldClientProtocol requires the exact
    /// single-arg signature; a defaulted parameter does NOT satisfy it).
    func createSession(title: String) async throws -> SessionSummary {
        try await createSession(title: title, conversationID: nil)
    }

    func createSession(title: String, conversationID: UUID? = nil) async throws -> SessionSummary {
        guard let client else { throw NativeGatewayClientError.notConnected }
        // LATENCY (build 36): session.create is part of the reconnect-path
        // chain (ensureConversation/ensureSessionForSwitch/_sendStreaming all
        // fall into this when idMap is stale -- e.g. the gateway reaped the
        // session while backgrounded). It had no explicit timeout, so it
        // inherited the client's 60s default, stacking on top of every other
        // bounded probe in this reconnect chain. Unlike the pure liveness
        // probes (5s), this does real server-side work (creating a session
        // row) so it gets the same 8s bound as the other real-work calls
        // (ticket mint, token refresh) rather than the 5s probe timeout.
        // Build 53: tag app-created sessions with source "ios" so the
        // client-side All Devices filter can distinguish this device's
        // sessions from CLI/desktop/cron sessions on the shared gateway DB.
        let response = try await client.send(method: "session.create", params: ["title": title, "source": "ios"], timeoutNanos: 8_000_000_000)
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
        // Dual-id fix: session.create returns BOTH the SHORT live id and the
        // FULL stored_session_id. Register the FULL key so later session.resume
        // probes (refresh / thinking bubble) and session.list row matching
        // resolve this conversation by its stable key.
        if let storedKey = decoded.storedSessionId, !storedKey.isEmpty {
            await idMap.registerKey(uuid: uuid, sessionKey: storedKey)
        }
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
        // Build 128.45: this is the "most recent" catch-all load. The native
        // session is resolved by the caller's ensureConversation flow and the
        // idMap; this placeholder conversation carries no sessionKey yet (the
        // real id-bound loadConversation(id:) stamps it). A nil sessionKey on
        // a brand-new local chat is correct - there is no server session to
        // mismatch against.
        let conv = Conversation(title: "New Chat")
        currentConversation = conv
        return conv
    }

    func loadConversation(id: UUID) async throws -> Conversation {
        guard let client else { throw NativeGatewayClientError.notConnected }
        // Build 53: a persisted idMap entry can point at a session the
        // gateway reaped (restart, test-build purge). ensureSessionForSwitch
        // probes with session.status, unregisters the stale mapping, and
        // recreates a fresh session - so tapping a purgatory row from an
        // old build lands in a working chat instead of 4001 "session not
        // found" + an error alert.
        guard await ensureSessionForSwitch(id: id),
              let nativeId = await idMap.nativeId(for: id) else {
            throw NativeGatewayClientError.unexpectedFrame
        }
        connectionStage = .restoring
        defer {
            if connectionStatus == .connected {
                connectionStage = .connected
            }
        }
        let response = try await client.send(method: "session.history", params: ["session_id": nativeId])
        if let error = response.error { throw error }
        guard let result = response.result else { return Conversation(id: id, title: "Untitled") }
        let data = try JSONEncoder().encode(result)
        let decoded = try JSONDecoder().decode(NativeSessionHistoryResult.self, from: data)
        // Build 50: resolve MEDIA references in historical messages so
        // images remain accessible after gateway restarts or new sessions.
        // The raw MEDIA: directives are converted to authenticated inline
        // image Markdown using the same normalized path logic as streaming.
        let mediaBaseURL = gatewayBaseURL
        // Fetch real per-row timestamps from the connector facade. Hermes'
        // session.history omits `timestamp`, so without this every history row
        // is re-stamped with the phone clock on each reload (drifting
        // timestamps + apparent reordering). The facade serves state.db
        // timestamps keyed by raw row id.
        let rowTimestamps = await loadHistoryTimestamps(nativeId: nativeId)
        let messages = decoded.messages.enumerated().compactMap { ordinal, msg -> Message? in
            // Build 69: tool rows come through as {"role":"tool","name",
            // "context"} with no display text; system markers are
            // scaffolding. Skip them instead of rendering blank bubbles.
            if msg.role == "tool" { return nil }
            if msg.role == "system" && msg.content.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                return nil
            }
            let resolvedContent: String
            var resolvedAttachments: [MessageAttachment] = []
            if msg.role == "assistant" {
                let resolved = Self.resolveHistoryMedia(
                    in: msg.content,
                    mediaURLProvider: { path in
                        Self.nativeMediaURL(for: path, gatewayBaseURL: mediaBaseURL)
                    }
                )
                resolvedContent = resolved.text
                resolvedAttachments = resolved.attachments
            } else {
                // Build 112: user messages with attachments come back from
                // session.history with `@image:`/`@file:` directive lines appended
                // to the content. Resolve them into real attachments and strip the
                // directives, so (a) the bubble renders the screenshots instead of
                // dead directive text and (b) the cleaned content matches the local
                // optimistic row, letting the ChatStore merge fingerprint-collapse
                // the server twin instead of rendering a duplicate.
                let resolved = Self.resolveHistoryDirectives(
                    in: msg.content,
                    mediaURLProvider: { path in
                        Self.nativeMediaURL(for: path, gatewayBaseURL: mediaBaseURL)
                    }
                )
                resolvedContent = resolved.text
                resolvedAttachments = resolved.attachments
            }
            return Message(
                id: Self.historyMessageID(
                    nativeSessionID: nativeId,
                    rowID: msg.rowId,
                    ordinal: ordinal,
                    role: msg.role,
                    content: msg.content
                ),
                sender: Self.decodeHistorySender(from: msg.role),
                content: resolvedContent,
                timestamp: Self.historyTimestamp(for: msg, rowTimestamps: rowTimestamps),
                attachments: resolvedAttachments
            )
        }
        let conv = Conversation(
            id: id,
            title: decoded.title ?? "Untitled",
            messages: Self.mapRestoredHistoryStatuses(messages),
            sessionKey: nativeId
        )
        currentConversation = conv
        return conv
    }

    /// Build 97: decode a raw role string from the native gateway's session.history
    /// through the canonical MessageSender decoder so "assistant", "herald", and
    /// "hermes" all map to .herald. Two role vocabularies exist in the wild: the
    /// connector HTTP history emits "herald" (session_store.py _message_to_dict),
    /// while the native gateway's session.history emits raw "assistant"
    /// (tui_gateway/server.py _history_to_messages). The strict == "assistant"
    /// check this replaced silently downgraded any "herald" row to .user, hiding
    /// the assistant Retry button in MessageBubble. Round-tripping the string
    /// through JSONDecoder keeps the role vocabulary in MessageSender.swift as
    /// the single source of truth.
    nonisolated static func decodeHistorySender(from role: String) -> MessageSender {
        guard let data = ("\"" + role + "\"").data(using: .utf8),
              let sender = try? JSONDecoder().decode(MessageSender.self, from: data) else {
            return .user
        }
        return sender
    }

    /// A restored user row is delivered when a later assistant reply appears
    /// in the persisted history. A final user-only row was sent but has not
    /// been acknowledged by a reply yet.
    nonisolated static func mapRestoredHistoryStatuses(_ messages: [Message]) -> [Message] {
        messages.enumerated().map { index, message in
            guard message.sender == .user,
                  messages.dropFirst(index + 1).contains(where: { $0.sender == .herald }) else {
                return message
            }
            var mapped = message
            mapped.status = .delivered
            return mapped
        }
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
        // LATENCY (build 33): same phantom-socket guard as ensureConversation.
        // A stale probe here hung the default 60s before model switch
        // errored; probe with the short timeout after healing the socket.
        await reconnectIfNeeded()
        if let nativeId = await idMap.nativeId(for: id) {
            do {
                guard let client else { throw NativeGatewayClientError.notConnected }
                let response = try await client.send(
                    method: "session.status",
                    params: ["session_id": nativeId],
                    timeoutNanos: Self.probeTimeoutNanos
                )
                if response.error == nil {
                    return true
                }
                // Build 54: the status probe 4001s for sessions the gateway
                // reaped from memory (restart, idle, background suspension).
                // Before giving up and minting a NEW empty session (which is
                // what made old chats open blank), try session.resume - the
                // gateway rebuilds the session from its persisted state.db
                // row, so the history and title come back intact. This is
                // exactly what the desktop/electron client does on reconnect.
                if let resumeError = response.error {
                    Self.logger.info("ensureSessionForSwitch: status failed (\(resumeError)), trying session.resume for \(nativeId)")
                }
                let resume = try await client.send(
                    method: "session.resume",
                    params: ["session_id": nativeId],
                    timeoutNanos: Self.probeTimeoutNanos
                )
                if resume.error == nil {
                    // Build 64: the gateway registers the resumed session
                    // under a NEW live session_id (the resume payload's
                    // `session_id` field), NOT the original id the client
                    // asked with. session.history/status look up the live
                    // registry by the passed id, so the next call with the
                    // stale original id 4001s "session not found" - the exact
                    // error when opening a previous session. Re-point the
                    // idMap at the resumed live id so the subsequent
                    // session.history in loadConversation hits the live
                    // session instead of the dead original.
                    if let result = resume.result,
                       let data = try? JSONEncoder().encode(result),
                       let decoded = try? JSONDecoder().decode(NativeResumeResult.self, from: data),
                       let resumedLiveID = decoded.sessionId, !resumedLiveID.isEmpty {
                        await idMap.register(uuid: id, nativeId: resumedLiveID)
                        // Dual-id fix: also keep the FULL session key so
                        // session.resume probes (thinking bubble / refresh)
                        // and session.list row matching resolve the same UUID.
                        if let resumedKey = decoded.sessionKey, !resumedKey.isEmpty {
                            await idMap.registerKey(uuid: id, sessionKey: resumedKey)
                        }
                        Self.logger.info("ensureSessionForSwitch: re-pointed idMap \(id) -> \(resumedLiveID) after resume")
                    }
                    return true
                }
            } catch {
                // Build 128: same transport guard as ensureConversation.
                // A THROWN transport error (notConnected / transportClosed /
                // requestTimeout) means we never reached the server, so we
                // can't know whether the session is stale. Preserve the
                // idMap and return false so the caller waits / retries
                // instead of minting a fresh empty session that orphans the
                // user's history (the b127/b128 bug class).
                if SessionListStore.isTransientConnectivityError(error) {
                    Self.logger.warning("ensureSessionForSwitch: transport failure for \(id.uuidString.prefix(8)) (\(error.localizedDescription)) — keeping idMap")
                    return false
                }
                // Definitive probe failure (non-transport): treat as stale;
                // fall through to recreate, as before.
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
        //
        // LATENCY (build 33): ensureConversation runs BEFORE the streaming
        // placeholder is appended, so any hang here is invisible to the
        // user. The session.status probe used the DEFAULT 60s request
        // timeout and never checked the socket first. After backgrounding,
        // iOS silently suspends the WebSocket (receive() never surfaces the
        // error), connectionStatus stays .connected, and the probe hung the
        // full 60s on the phantom socket -- then fell through to
        // createSession, another 60s-class round trip -- before the bubble
        // ever rendered and before the message hit the gateway (which is
        // why follow-ups never showed up in hermes logs). Fix: probe the
        // socket with a 5s timeout FIRST (reconnectIfNeeded), then use the
        // same short timeout for the session.status probe so a dead socket
        // heals in ~5s instead of minutes.
        await reconnectIfNeeded()

        if let nativeId = await idMap.nativeId(for: id) {
            do {
                guard let client else { throw NativeGatewayClientError.notConnected }
                let response = try await client.send(
                    method: "session.status",
                    params: ["session_id": nativeId],
                    timeoutNanos: Self.probeTimeoutNanos
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
                // Build 128: a thrown transport error (notConnected,
                // transportClosed, requestTimeout) means the probe didn't
                // actually reach the server -- it does NOT mean the session
                // is stale. Preserve the idMap and report failure so the
                // caller can decide whether to recreate or wait. This is the
                // mirror of the b127 restart-button bug: a connectivity blip
                // wiped a perfectly good idMap and forced a fresh empty
                // session, so the user's old chat opened blank.
                if SessionListStore.isTransientConnectivityError(error) {
                    Self.logger.warning("ensureConversation: transport failure for \(id.uuidString.prefix(8)) (\(error.localizedDescription)) — keeping idMap")
                    return false
                }
                // Definitive probe failure (non-transport): treat as stale
                // and fall through to recreate, as before.
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

    /// Adopt a locally-minted conversation id as the active native
    /// conversation without going through the network. Mirrors what the
    /// success branch of `ensureConversation` does (re-point
    /// `currentConversation`) but skips the probe, so ChatStore can
    /// install an empty local chat and the next send resolves the right
    /// session UUID without waiting for `loadConversationInBackground`.
    func adoptConversation(id: UUID, title: String) {
        if currentConversation?.id != id {
            currentConversation = Conversation(id: id, title: title)
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
        return AsyncStream { continuation in
            Task { [weak self] in
                guard let self else { continuation.finish(); return }
                await self._sendStreaming(
                    message: message,
                    attachments: attachments,
                    clientMessageID: clientMessageID,
                    continuation: continuation
                )
            }
        }
    }

    private func _sendStreaming(
        message: String,
        attachments: [PendingAttachment],
        clientMessageID: UUID,
        conversationID: UUID? = nil,
        sessionTitle: String? = nil,
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
        let sessionUUID = conversationID ?? currentConversation?.id ?? UUID()
        var nativeSessionId = await idMap.nativeId(for: sessionUUID)

        if nativeSessionId == nil {
            do {
                let summary = try await createSession(title: sessionTitle ?? "New Chat", conversationID: sessionUUID)
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
        let mediaBaseURL = gatewayBaseURL
        let handler = StreamEventHandler(
            sessionId: sid,
            continuation: continuation,
            mediaURLProvider: { path in
                NativeKallistiClient.nativeMediaURL(for: path, gatewayBaseURL: mediaBaseURL)
            }
        )
        handler.onComplete = { [weak self] in
            self?.activeStreamHandlers.removeValue(forKey: sid)
            // Build 95: the app has the reply (message.complete arrived over
            // the app's own WS), so the connector must not fire its fallback
            // "Turn complete" push for this turn. Fire-and-forget unwatch.
            Task.detached { [weak self] in
                await self?.unregisterNativeWatch(sessionId: sid)
            }
        }
        activeStreamHandlers[sid] = handler
        // Build 97: unwatch on ANY stream termination, not just
        // message.complete.  The old code only fired unwatch from
        // handler.onComplete, which requires message.complete to arrive
        // over the app's own WebSocket.  When that WS dropped mid-turn
        // (code-1006 closures observed every few minutes on this host),
        // the connector watch stayed registered forever and kept polling +
        // pushing "Turn complete" for a turn the app had already seen or
        // abandoned.  onTermination runs on every finish()/cancel() path
        // (message.complete, failed send, consumer cancellation), so the
        // watch can never outlive the stream that created it.  A parked
        // stream (WS suspended, turn still in flight) does NOT terminate,
        // which is exactly right: the connector watch is the fallback push
        // for that case.
        continuation.onTermination = { @Sendable [weak self] _ in
            Task.detached { [weak self] in
                await self?.unregisterNativeWatch(sessionId: sid)
            }
        }
        await client.onEvent { event in
            handler.handle(event)
        }

        // Tell the connector to watch this session so it can fire an APNs
        // push if the turn finishes while the app is backgrounded or killed
        // (the app's own WebSocket is suspended then, so nothing else can
        // notice). Best-effort: a failure here costs a notification, never
        // the turn itself.
        // LATENCY (build 35): fire-and-forget. This was `await`ed directly
        // in the hot send path - every prompt.submit waited on this HTTP
        // call finishing first, even though it's a best-effort push
        // registration whose own doc comment says a failure "costs a
        // notification, never the turn itself." The code didn't match the
        // comment. Detached so a slow/unreachable connector facade (port
        // 8010) can no longer add latency to the actual message send.
        Task.detached { [weak self] in
            await self?.registerNativeWatch(sessionId: sid)
        }

        // Stage images through the native gateway's attachment RPC. The
        // gateway consumes session-attached image paths on prompt.submit;
        // sending an unrecognized attachments field drops the image before
        // vision routing.
        do {
            for attachment in attachments where attachment.kind == .image {
                let upload = try await client.send(
                    method: "image.attach_bytes",
                    params: NativeImageAttachParams(
                        sessionId: sid,
                        contentBase64: attachment.base64Data,
                        filename: attachment.fileName
                    ),
                    timeoutNanos: Self.attachmentUploadTimeoutNanos
                )
                if let error = upload.error {
                    throw error
                }
            }

            // Build 78.7: stage file attachments via file.attach RPC.
            var fileRefTexts: [String] = []
            for attachment in attachments where attachment.kind == .file {
                let upload = try await client.send(
                    method: "file.attach",
                    params: NativeFileAttachParams(
                        sessionId: sid,
                        dataUrl: "data:\(attachment.mimeType);base64,\(attachment.base64Data)",
                        name: attachment.fileName
                    ),
                    timeoutNanos: Self.attachmentUploadTimeoutNanos
                )
                if let error = upload.error {
                    throw error
                }
                fileRefTexts.append("@file:\(attachment.fileName)")
            }

            // Append file references to the message text so the agent
            // knows about attached files.
            var finalMessage = message
            if !fileRefTexts.isEmpty {
                finalMessage += "\n\n" + fileRefTexts.joined(separator: "\n")
            }

            // prompt.submit returns a quick "streaming" acknowledgement;
            // the actual model turn arrives through gateway events. Never give
            // this acknowledgement the client's 60-second default timeout,
            // because a dropped frame otherwise holds the composer even though
            // no work has reached the host.
            let response = try await client.send(
                method: "prompt.submit",
                params: PromptSubmitParams(sessionId: sid, text: finalMessage),
                timeoutNanos: Self.submitAckTimeoutNanos
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
                        let freshMediaBaseURL = gatewayBaseURL
                        let freshHandler = StreamEventHandler(
                            sessionId: freshSid,
                            continuation: continuation,
                            mediaURLProvider: { path in
                                NativeKallistiClient.nativeMediaURL(for: path, gatewayBaseURL: freshMediaBaseURL)
                            }
                        )
                        await client.onEvent { event in
                            freshHandler.handle(event)
                        }
                        var retryFileRefs: [String] = []
                        for attachment in attachments where attachment.kind == .image {
                            let upload = try await client.send(
                                method: "image.attach_bytes",
                                params: NativeImageAttachParams(
                                    sessionId: freshSid,
                                    contentBase64: attachment.base64Data,
                                    filename: attachment.fileName
                                ),
                                timeoutNanos: Self.attachmentUploadTimeoutNanos
                            )
                            if let uploadError = upload.error {
                                throw uploadError
                            }
                        }
                        for attachment in attachments where attachment.kind == .file {
                            let upload = try await client.send(
                                method: "file.attach",
                                params: NativeFileAttachParams(
                                    sessionId: freshSid,
                                    dataUrl: "data:\(attachment.mimeType);base64,\(attachment.base64Data)",
                                    name: attachment.fileName
                                ),
                                timeoutNanos: Self.attachmentUploadTimeoutNanos
                            )
                            if let uploadError = upload.error {
                                throw uploadError
                            }
                            retryFileRefs.append("@file:\(attachment.fileName)")
                        }
                        var retryMessage = message
                        if !retryFileRefs.isEmpty {
                            retryMessage += "\n\n" + retryFileRefs.joined(separator: "\n")
                        }
                        let retry = try await client.send(
                            method: "prompt.submit",
                            params: PromptSubmitParams(sessionId: freshSid, text: retryMessage),
                            timeoutNanos: Self.submitAckTimeoutNanos
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

    nonisolated static func nativeMediaURL(for path: String, gatewayBaseURL: String) -> URL? {
        // Media must ride the SAME base URL as the gateway WS connection, not
        // a hardcoded :8010. The connector facade only listens on the LAN
        // (192.168.1.10:8010); the public relay exposes 443 via Caddy, which
        // routes /v1/native/media to the connector. Hardcoding :8010 made the
        // URL unreachable from WAN (connection refused) and the image render
        // as a failure placeholder. Port 8010 is only correct when the gateway
        // base itself is a LAN host.
        guard let base = URL(string: gatewayBaseURL), let host = base.host else { return nil }
        var components = URLComponents()
        components.scheme = base.scheme == "https" ? "https" : "http"
        components.host = host
        if Self.isLANHost(host) {
            components.port = 8010
        }
        components.path = "/v1/native/media"
        // Build 50: normalize the path to a relative form so historical
        // images survive HERMES_HOME changes or profile consolidation.
        let normalizedPath = Self.normalizeMediaPath(path)
        components.queryItems = [URLQueryItem(name: "path", value: normalizedPath)]
        return components.url
    }

    /// Pure helper: connector facade base URL from a gateway base URL.
    /// LAN hosts get scheme://host:8010; public HTTPS gets scheme://host
    /// (Caddy on 443 routes /v1/push/register, /v1/native/media, etc.).
    nonisolated static func facadeBaseURL(for gatewayBaseURL: String) -> String? {
        guard let url = URL(string: gatewayBaseURL), let host = url.host else { return nil }
        let scheme = url.scheme == "https" ? "https" : "http"
        if isLANHost(host) {
            return "\(scheme)://\(host):8010"
        }
        return "\(scheme)://\(host)"
    }

    /// Returns true when the host is a LAN/rFC1918 address or mDNS name.
    nonisolated static func isLANHost(_ host: String) -> Bool {
        host.hasPrefix("192.168.") || host.hasPrefix("10.") || host.hasPrefix("172.16.") || host.hasSuffix(".local")
    }

    /// Normalize a raw MEDIA path to a stable relative key the connector
    /// can resolve across HERMES_HOME changes or profile consolidation.
    ///
    /// The contract: the returned string is one of
    ///   - `<approved-root-segment>/<basename>`   e.g. `images/file.jpg`
    ///   - `media/<basename>`                      e.g. `media/file`
    ///   - `cache/images/<basename>`               e.g. `cache/images/file.jpg`
    ///
    /// Legacy paths containing `profiles/<profile>/` are collapsed so the
    /// connector finds them under the consolidated roots.  Traversal
    /// components (`..`) are rejected.  Already-relative paths in the
    /// approved form are returned unchanged.
    nonisolated static func normalizeMediaPath(_ raw: String) -> String {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.split(separator: "/", omittingEmptySubsequences: false).contains("..") else {
            return ""
        }

        var relative: String
        if trimmed.hasPrefix("~/") {
            let rest = String(trimmed.dropFirst(2))
            relative = Self._stripHermesPrefix(rest) ?? rest
        } else if trimmed.hasPrefix("/") {
            relative = Self._stripHermesPrefix(trimmed) ?? trimmed
        } else {
            relative = trimmed
        }
        relative = relative.trimmingCharacters(in: CharacterSet(charactersIn: "/"))

        if relative.hasPrefix("profiles/") {
            let components = relative.split(separator: "/", omittingEmptySubsequences: true)
            guard components.count >= 4 else { return "" }
            relative = components.dropFirst(2).joined(separator: "/")
        }

        let components = relative.split(separator: "/", omittingEmptySubsequences: true)
        guard !components.isEmpty else { return "" }
        if components.count >= 3, components[0] == "cache", components[1] == "images" {
            return components.joined(separator: "/")
        }
        if components.count >= 2, components[0] == "images" || components[0] == "media" {
            return components.joined(separator: "/")
        }
        return ""
    }

    /// Strip the hermes home directory prefix from an absolute path.
    private nonisolated static func _stripHermesPrefix(_ path: String) -> String? {
        let patterns = ["/.hermes/", "/hermes/", "/.hermes-mobile/"]
        for prefix in patterns {
            if let range = path.range(of: prefix) {
                return String(path[range.upperBound...])
            }
        }
        return nil
    }

    nonisolated static func resolveNativeMedia(in text: String, mediaURLProvider: (String) -> URL?) -> String {
        let fullRange = NSRange(text.startIndex..., in: text)
        let matches = nativeMediaPattern?.matches(in: text, range: fullRange) ?? []
        guard !matches.isEmpty else { return text }
        let mutable = NSMutableString(string: text)
        for match in matches.reversed() {
            let path = (1...4).compactMap { index -> String? in
                let range = match.range(at: index)
                guard range.location != NSNotFound else { return nil }
                return (text as NSString).substring(with: range)
            }.first
            // The mediaURLProvider already normalizes the path (via
            // nativeMediaURL -> normalizeMediaPath), so pass the raw path
            // directly to avoid double-normalization.
            guard let rawPath = path else { continue }
            guard let url = mediaURLProvider(rawPath) else { continue }
            mutable.replaceCharacters(in: match.range, with: "![image](\(url.absoluteString))")
        }
        return mutable as String
    }

    /// Build 101: resolve MEDIA references in history into typed
    /// MessageAttachment records (reusing the live render UI) instead of
    /// inline Markdown. Returns the cleaned text (MEDIA directives stripped)
    /// and the attachments to attach to the Message. Non-image types (PDF,
    /// video, files) now survive history reload; previously only image Markdown
    /// was emitted and everything else was dead text.
    nonisolated static func resolveHistoryMedia(
        in text: String,
        mediaURLProvider: (String) -> URL?
    ) -> (text: String, attachments: [MessageAttachment]) {
        let fullRange = NSRange(text.startIndex..., in: text)
        let matches = nativeMediaPattern?.matches(in: text, range: fullRange) ?? []
        guard !matches.isEmpty else { return (text, []) }
        let mutable = NSMutableString(string: text)
        var attachments: [MessageAttachment] = []
        for match in matches.reversed() {
            let rawPath = (1...4).compactMap { index -> String? in
                let range = match.range(at: index)
                guard range.location != NSNotFound else { return nil }
                return (text as NSString).substring(with: range)
            }.first
            guard let rawPath else { continue }
            guard let url = mediaURLProvider(rawPath) else { continue }
            let classification = Self.classifyMediaPath(rawPath)
            attachments.append(
                MessageAttachment(
                    kind: classification.kind,
                    fileName: (rawPath as NSString).lastPathComponent,
                    mimeType: classification.mimeType,
                    mediaURL: url
                )
            )
            mutable.replaceCharacters(in: match.range, with: "")
        }
        return (mutable as String, attachments)
    }

    /// Build 112: resolve `@image:`/`@file:` directives (the connector's user-
    /// attachment convention) out of a history message, mirroring the connector's
    /// _extract_directive_attachments: return cleaned text plus attachment objects
    /// keyed by media URL. Without this, user rows with attachments reload as raw
    /// directive text with zero attachments, and the merge cannot fingerprint-
    /// collapse the server twin against the local optimistic row, so the message
    /// renders twice.
    nonisolated static func resolveHistoryDirectives(
        in text: String,
        mediaURLProvider: (String) -> URL?
    ) -> (text: String, attachments: [MessageAttachment]) {
        let fullRange = NSRange(text.startIndex..., in: text)
        let matches = nativeDirectivePattern?.matches(in: text, range: fullRange) ?? []
        guard !matches.isEmpty else { return (text, []) }
        let mutable = NSMutableString(string: text)
        var attachments: [MessageAttachment] = []
        for match in matches.reversed() {
            let rawPath = (1...4).compactMap { index -> String? in
                let range = match.range(at: index)
                guard range.location != NSNotFound else { return nil }
                return (text as NSString).substring(with: range)
            }.first
            guard let rawPath else { continue }
            guard let url = mediaURLProvider(rawPath) else { continue }
            let classification = Self.classifyMediaPath(rawPath)
            attachments.append(
                MessageAttachment(
                    kind: classification.kind,
                    fileName: (rawPath as NSString).lastPathComponent,
                    mimeType: classification.mimeType,
                    mediaURL: url
                )
            )
            mutable.replaceCharacters(in: match.range, with: "")
        }
        return (mutable as String, attachments)
    }

    /// Classify a media path by extension into a kind + MIME type so the
    /// history renderer can route image/video/PDF to inline previews and
    /// everything else to the file card.
    nonisolated static func classifyMediaPath(_ path: String) -> (kind: String, mimeType: String) {
        let ext = (path as NSString).pathExtension.lowercased()
        switch ext {
        case "png": return ("image", "image/png")
        case "jpg", "jpeg": return ("image", "image/jpeg")
        case "gif": return ("image", "image/gif")
        case "webp": return ("image", "image/webp")
        case "mp4": return ("video", "video/mp4")
        case "mov": return ("video", "video/quicktime")
        case "m4v": return ("video", "video/x-m4v")
        case "webm": return ("video", "video/webm")
        case "pdf": return ("pdf", "application/pdf")
        case "mp3": return ("audio", "audio/mpeg")
        case "m4a": return ("audio", "audio/mp4")
        case "wav": return ("audio", "audio/wav")
        case "aac": return ("audio", "audio/aac")
        case "zip": return ("file", "application/zip")
        case "txt", "md": return ("file", "text/plain")
        case "csv": return ("file", "text/csv")
        case "json": return ("file", "application/json")
        case "xml": return ("file", "application/xml")
        case "yaml", "yml": return ("file", "application/yaml")
        case "doc", "docx": return ("file", "application/msword")
        case "xls", "xlsx": return ("file", "application/vnd.ms-excel")
        case "ppt", "pptx": return ("file", "application/vnd.ms-powerpoint")
        case "rtf": return ("file", "application/rtf")
        default: return ("file", "application/octet-stream")
        }
    }

    func cancelJob(jobID: UUID) async throws {
        guard let client else { throw NativeGatewayClientError.notConnected }
        let response = try await client.send(method: "prompt.cancel", params: ["job_id": jobID.uuidString])
        if let error = response.error { throw error }
    }

    /// Build 128.50: interrupt the CURRENT session's running turn server-side
    /// via session.interrupt. Unlike cancelJob (which needs the local jobID
    /// from activeStreams), this works when dropping into a session that is
    /// mid-turn on the server from another device - the app has no jobID for
    /// that turn, only the session. Returns true when the gateway accepted
    /// the interrupt.
    func interruptSession() async -> Bool {
        guard let client else { return false }
        guard let currentConversation else { return false }
        let interruptID: String
        if let key = await idMap.sessionKey(for: currentConversation.id), !key.isEmpty {
            interruptID = key
        } else if let native = await idMap.nativeId(for: currentConversation.id), !native.isEmpty {
            interruptID = native
        } else {
            return false
        }
        do {
            let response = try await client.send(
                method: "session.interrupt",
                params: ["session_id": interruptID],
                timeoutNanos: Self.probeTimeoutNanos
            )
            return response.error == nil
        } catch {
            Self.logger.warning("interruptSession: \(error.localizedDescription)")
            return false
        }
    }

    var supportsServerTurnInterrupt: Bool { true }

    func getJobStatus(_ jobId: UUID) async -> LiveHeraldClient.JobStatusResponse? { nil }

    /// Reattach to a parked gateway session (desktop parity: Electron calls
    /// session.resume on reconnect). The gateway parks a live session when the
    /// WS detaches; resume rebinds the transport so a still-running job's
    /// terminal events flow again. Returns true when the session is live and
    /// still running - the caller then keeps the stream alive and resets the
    /// watchdog instead of declaring "took too long".
    func resumeActiveSessionIfNeeded() async -> Bool {
        guard let client else { return false }
        guard let currentConversation else { return false }
        // Dual-id fix: session.resume requires the FULL gateway session key
        // (db.get_session lookup), NOT the SHORT live id. The old code sent
        // idMap.nativeId (SHORT after createSession/ensureSessionForSwitch),
        // which the gateway rejected with 4007 -> watch never started -> no
        // thinking bubble on refresh. Prefer the FULL key when registered.
        // NOTE: 'await' cannot appear on the right of ?? - resolve each
        // actor call sequentially.
        let resumeID: String
        if let key = await idMap.sessionKey(for: currentConversation.id), !key.isEmpty {
            resumeID = key
        } else if let native = await idMap.nativeId(for: currentConversation.id), !native.isEmpty {
            resumeID = native
        } else {
            return false
        }
        do {
            let response = try await client.send(
                method: "session.resume",
                params: ["session_id": resumeID],
                timeoutNanos: Self.probeTimeoutNanos
            )
            guard response.error == nil, let result = response.result else { return false }
            // The resume payload carries  and ; a live
            // in-flight job reports running=true. If the job already
            // finished, running=false and the transcript refresh in
            // recoverStalledStream picks up the completed response.
            let data = try JSONEncoder().encode(result)
            let decoded = try JSONDecoder().decode(NativeResumeResult.self, from: data)
            return decoded.running == true
        } catch {
            return false
        }
    }

    func isServerTurnAwaitingUserInput() async -> Bool {
        guard let client, let currentConversation else { return false }
        let resumeID: String
        if let key = await idMap.sessionKey(for: currentConversation.id), !key.isEmpty {
            resumeID = key
        } else if let native = await idMap.nativeId(for: currentConversation.id), !native.isEmpty {
            resumeID = native
        } else {
            return false
        }
        do {
            let response = try await client.send(
                method: "session.resume",
                params: ["session_id": resumeID],
                timeoutNanos: Self.probeTimeoutNanos
            )
            guard response.error == nil, let result = response.result else { return false }
            let data = try JSONEncoder().encode(result)
            let decoded = try JSONDecoder().decode(NativeResumeResult.self, from: data)
            // Parked on user input: a clarify question or approval prompt is
            // blocking the turn. The model is NOT working - the user is.
            return decoded.pendingClarify != nil || decoded.pendingApproval != nil
        } catch {
            return false
        }
    }

    func sendMessage(_ text: String, conversationID: UUID, clientMessageID: UUID) async throws -> Message {
        return await send(message: text, attachments: [], clientMessageID: clientMessageID, continuationContext: nil)
    }

    /// Send a note's content as a message to a SPECIFIC conversation/session,
    /// used by note-as-session sync. Unlike `send`, this targets the note's
    /// own gateway session (created with the note title if needed) and never
    /// touches the currently-open chat conversation.
    func sendNoteMessage(text: String, attachments: [PendingAttachment], clientMessageID: UUID, conversationID: UUID, title: String) async -> Message {
        var finalContent = ""
        for await update in sendStreamingToConversation(
            message: text,
            attachments: attachments,
            clientMessageID: clientMessageID,
            conversationID: conversationID,
            title: title
        ) {
            switch update {
            case .textDelta(let delta):
                finalContent += delta
            case .finished(let msg, _, _, _):
                return msg
            case .failed(let error, _, _):
                return Message(id: clientMessageID, sender: .herald, content: "Note sync error: \(error)", timestamp: .now)
            default:
                break
            }
        }
        return Message(id: clientMessageID, sender: .herald, content: finalContent, timestamp: .now)
    }

    /// Streaming variant of sendNoteMessage. Resolves the note's session by the
    /// conversation UUID (creating it with `title` when absent) and submits the
    /// message against THAT session, leaving `currentConversation` untouched.
    func sendStreamingToConversation(
        message: String,
        attachments: [PendingAttachment],
        clientMessageID: UUID,
        conversationID: UUID,
        title: String
    ) -> AsyncStream<StreamingUpdate> {
        return AsyncStream { continuation in
            Task { [weak self] in
                guard let self else { continuation.finish(); return }
                await self._sendStreaming(
                    message: message,
                    attachments: attachments,
                    clientMessageID: clientMessageID,
                    conversationID: conversationID,
                    sessionTitle: title,
                    continuation: continuation
                )
            }
        }
    }
}


/// Result envelope for session.resume. `running` mirrors the gateway's live
/// session payload so the app can distinguish a still-working job from a
/// finished one after a suspension.
private struct NativeResumeResult: Decodable {
    /// Build 64: the gateway registers the resumed session under this NEW
    /// live id. loadConversation must use it (not the original id) for the
    /// follow-up session.history call, or the lookup 4001s "session not
    /// found" against the live registry.
    let sessionId: String?
    /// The FULL gateway session key (e.g. 20260816_144338_2bbd59). The
    /// resume payload carries it alongside the short live id; the app keeps
    /// it in the idMap's key map so session.resume probes (thinking bubble,
    /// refresh) send the FULL key the gateway's db lookup requires.
    let sessionKey: String?
    let running: Bool?
    let status: String?
    /// When the gateway session is parked on a clarify question (the turn
    /// emitted clarify.request and is waiting for the user to answer).
    let pendingClarify: [String: NativeJSONValue]?
    /// When the gateway session is parked on an approval prompt.
    let pendingApproval: [String: NativeJSONValue]?

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case sessionKey = "session_key"
        case running
        case status
        case pendingClarify = "pending_clarify"
        case pendingApproval = "pending_approval"
    }
}

// MARK: - Stream Event Handler

/// Processes gateway events for a single streaming turn.
private final class StreamEventHandler: @unchecked Sendable {
    let sessionId: String
    var isCompleted: Bool { completed }
    /// Invoked once when the stream terminally completes (message.complete).
    /// Lets the owning client drop the handler from its reconnect registry.
    var onComplete: (@MainActor () -> Void)?
    let continuation: AsyncStream<StreamingUpdate>.Continuation
    private var completed = false
    private let mediaURLProvider: @Sendable (String) -> URL?
    /// ANSI-colored inline diffs accumulated from `tool.complete` events this
    /// turn. Parsed into a CodeDiff at `message.complete` so a multi-file edit
    /// turn renders one collapsible diff card on the final message.
    private var pendingDiffTexts: [String] = []

    init(
        sessionId: String,
        continuation: AsyncStream<StreamingUpdate>.Continuation,
        mediaURLProvider: @escaping @Sendable (String) -> URL? = { _ in nil }
    ) {
        self.sessionId = sessionId
        self.continuation = continuation
        self.mediaURLProvider = mediaURLProvider
    }

    /// Build 128.45 (desync fix): terminate the stream because the
    /// transport died. The gateway does NOT replay the in-flight turn's
    /// terminal events over a fresh socket, so a handler left registered
    /// hangs the ChatStore consumer forever (iPad stuck showing the stop
    /// button / thinking bubble while the server keeps working - Electron
    /// was fine). Yields a bare .failed with no category so ChatStore keeps
    /// the placeholder (acceptedJobID != nil) and hands off to the
    /// job-status polling fallback, which recovers the response when the
    /// server finishes.
    func terminate(reason: String) {
        guard !completed else { return }
        completed = true
        continuation.yield(.failed(reason))
        continuation.finish()
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
                    content: NativeKallistiClient.resolveNativeMedia(
                        in: complete.text ?? "",
                        mediaURLProvider: mediaURLProvider
                    ),
                    timestamp: .now
                )
                let codeDiff = CodeDiffParser.parse(accumulatedTexts: pendingDiffTexts)
                continuation.yield(.finished(msg, usage, codeDiff, nil))
                completed = true
                continuation.finish()
                // Release the registry entry: the stream is done and the
                // next reconnect should not re-register a finished handler.
                // The handler is nonisolated; hop to MainActor for the
                // registry mutation.
                if let onComplete {
                    Task { @MainActor in onComplete() }
                }
            }
        // ── Tool lifecycle ──────────────────────────────────────────────
        case "tool.start":
            if let tool = event.params.decodePayload(NativeToolStartPayload.self) {
                let label = tool.name ?? "tool"
                let activity = ToolActivity(
                    label: label,
                    toolCallID: tool.toolCallID,
                    name: tool.name,
                    emoji: tool.emoji,
                    argsPreview: tool.argsText ?? tool.preview
                )
                continuation.yield(.toolStarted(activity))
            }
        case "tool.complete":
            if let tool = event.params.decodePayload(NativeToolCompletePayload.self) {
                // Desktop parity: prefer the gateway-rendered inline_diff, but
                // fall back to the diff embedded in the tool result object
                // (write_file/patch return `diff` in their result) when the
                // rendered path is absent — e.g. snapshot capture failed.
                if let inlineDiff = tool.inlineDiff, !inlineDiff.isEmpty {
                    pendingDiffTexts.append(inlineDiff)
                } else if let resultDiff = tool.resultDiff, !resultDiff.isEmpty {
                    pendingDiffTexts.append(resultDiff)
                }
                continuation.yield(.toolCompleted(
                    toolCallID: tool.toolCallID ?? "",
                    resultPreview: tool.output,
                    isError: tool.isError ?? false,
                    durationMs: tool.durationMs
                ))
            }
        case "tool.output":
            if let tool = event.params.decodePayload(NativeToolOutputPayload.self),
               let chunk = tool.chunk, !chunk.isEmpty {
                continuation.yield(.toolOutput(toolCallID: tool.toolCallID ?? "", chunk: chunk))
            }
        case "tool.generating":
            // Transient — analogous to thinking.delta spinner frames. Drop.
            break
        case "review.summary":
            // Build 104: self-improvement / memory review summary arriving
            // mid-stream. Yield as a faint system line.
            if let notice = event.params.decodePayload(NativeReviewSummaryPayload.self),
               !notice.text.isEmpty {
                continuation.yield(.systemNotice(notice.text))
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

private struct NativeImageAttachParams: Encodable {
    let sessionId: String
    let contentBase64: String
    let filename: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case contentBase64 = "content_base64"
        case filename
    }
}

// Build 78.7: file attachment RPC for videos and non-image files.
// file.attach expects a data_url (data:<mime>;base64,<b64>) + name, not
// content_base64 (that contract belongs to image.attach_bytes).
private struct NativeFileAttachParams: Encodable {
    let sessionId: String
    let dataUrl: String
    let name: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case dataUrl = "data_url"
        case name
    }
}

private struct PromptSubmitParams: Encodable {
    let sessionId: String
    let text: String

    enum CodingKeys: String, CodingKey {
        case sessionId = "session_id"
        case text
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(sessionId, forKey: .sessionId)
        try container.encode(text, forKey: .text)
    }
}
