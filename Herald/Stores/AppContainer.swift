import Foundation
import os

import BackgroundTasks
import Speech
import UserNotifications

extension Logger {
    static let app = Logger(subsystem: "net.fihonline.kallisti", category: "app")
}

/// Thread-safe cached holder for the MiMo API key.
/// Reads from Keychain once on first access and caches the value.
/// Call `refresh()` after Settings writes/deletes the key.
@MainActor
final class APIKeyHolder {
    private let secureStore: (any SecureStoreProtocol)?
    private var cachedKey: String?
    private var hasLoaded = false

    init(secureStore: (any SecureStoreProtocol)?) {
        self.secureStore = secureStore
    }

    func get() -> String? {
        if !hasLoaded {
            // Synchronous return of cached value; first load happens in refresh()
            return nil
        }
        return cachedKey
    }

    func refresh() async {
        guard let secureStore else { return }
        let key = await secureStore.retrieve(key: "mimo.apiKey")
        cachedKey = key?.trimmingCharacters(in: .whitespacesAndNewlines)
        hasLoaded = true
    }
}

@MainActor
@Observable
final class AppContainer {
    // Pre-1.1.1 releases stored the raw APNs token in UserDefaults.standard under
    // this key. We now keep the token in Keychain (ThisDeviceOnly). The legacy
    // key is read once on first launch after upgrade to migrate + delete.
    private static let legacyAPNsTokenDefaultsKey = "kallisti.apns.deviceToken"
    static let apnsTokenKeychainKey = "kallisti.apns.deviceToken"

    /// APNs environment for the CURRENT build. TestFlight / App Store
    /// builds carry a receipt and use the production APNs environment;
    /// devicectl/sideloaded builds have no receipt and use development.
    /// The relay delivers to the environment stored on the registration,
    /// so registering a devicectl build as production gets APNs
    /// `BadEnvironmentKeyInToken` (and vice versa).
    static var apnsEnvironment: String {
        if let receiptURL = Bundle.main.appStoreReceiptURL,
           !receiptURL.pathComponents.isEmpty {
            return "production"
        }
        return "development"
    }
    private static let sharedDefaultContainer = AppContainer.makeDefault()

    let router = TabRouter()
    let sessionStore: AppSessionStore
    let pairingStore: PairingStore
    /// Non-nil only when native-gateway mode built a client (see makeDefault).
    /// Onboarding uses this to trigger the Nous OAuth login it can't reach
    /// any other way — `NativeKallistiClient.connect()` alone has no UI to
    /// present a login screen from.
    let nativeGatewayClient: NativeKallistiClient?
    let hostStore: KallistiHostStore
    let chatStore: ChatStore
    let inboxStore: InboxStore
    let permissionsStore: PermissionsStore
    let settingsStore: SettingsStore
    let talkStore: TalkStore
    let sessionListStore: SessionListStore
    let modelStore: ModelStore
    let profileStore: ProfileStore
    let skillsStore: SkillsStore
    let cronStore: CronStore
    let canvasStore: HeraldCanvasStore
    let notesStore: NotesStore
    let attachmentService: AttachmentService
    let sensorUploadService: SensorUploadService?
    let dashboardLogService: DashboardLogService
    let themeManager: ThemeManager
    let hostStatusStream: HostStatusStreamService
    /// Build 33: restart-safe Hermes gateway control (preflight → confirm →
    /// idempotent submit → poll). App-scoped so an in-flight restart survives
    /// the Settings screen being dismissed mid-poll.
    let gatewayControl: GatewayControlService
    private let apiClient: RelayAPIClient?
    private let notificationService: (any NotificationServiceProtocol)?
    private let pushRegistrationCoordinator: PushRegistrationCoordinator?
    private let secureStore: (any SecureStoreProtocol)?
    private var didMigrateLegacyAPNsToken = false
    private var isInitialized = false
    private var isInitializing = false
    /// Set to true on launch if a persisted in-flight checkpoint was found
    /// and is less than 10 minutes old. The root view reads this to show
    /// a "resuming" indicator instead of a blank/failed screen.
    private(set) var isResumingFromBackground = false
    private var lastCommandCatalogRefreshAt: Date?
    private var lastKnownHostOnline = false

    // Build 70: connector latency, polled while the native gateway is
    // connected (not just while Settings is visible) so the Settings row is
    // instantly ready and stays live.
    private(set) var connectorLatencyMs: Int?
    private var latencyMonitorTask: Task<Void, Never>?

    // Build 70: aux model service hoisted from Settings so the Infrastructure
    // section loads at connection time, not when Settings first appears.
    var auxService: AuxModelService?

    // Notification routing: stores a pending route while initialization is incomplete
    struct PendingNotificationRoute: Sendable {
        let conversationID: UUID?
        let messageID: String?
        let jobID: String?
        let action: String?
        let replyText: String?
    }
    private var pendingNotificationRoute: PendingNotificationRoute?

    private static let commandCatalogRefreshInterval: TimeInterval = 60

    init(
        sessionStore: AppSessionStore,
        pairingStore: PairingStore,
        nativeGatewayClient: NativeKallistiClient? = nil,
        hostStore: KallistiHostStore,
        chatStore: ChatStore,
        inboxStore: InboxStore,
        permissionsStore: PermissionsStore,
        settingsStore: SettingsStore,
        talkStore: TalkStore,
        sessionListStore: SessionListStore,
        modelStore: ModelStore? = nil,
        profileStore: ProfileStore? = nil,
        skillsStore: SkillsStore? = nil,
        cronStore: CronStore? = nil,
        canvasStore: HeraldCanvasStore? = nil,
        notesStore: NotesStore? = nil,
        attachmentService: AttachmentService? = nil,
        sensorUploadService: SensorUploadService? = nil,
        dashboardLogService: DashboardLogService? = nil,
        apiClient: RelayAPIClient? = nil,
        notificationService: (any NotificationServiceProtocol)? = nil,
        pushRegistrationCoordinator: PushRegistrationCoordinator? = nil,
        secureStore: (any SecureStoreProtocol)? = nil,
        hostStatusStream: HostStatusStreamService? = nil,
        gatewayControl: GatewayControlService? = nil
    ) {
        self.sessionStore = sessionStore
        self.pairingStore = pairingStore
        self.nativeGatewayClient = nativeGatewayClient
        self.hostStore = hostStore
        self.chatStore = chatStore
        self.inboxStore = inboxStore
        self.permissionsStore = permissionsStore
        self.settingsStore = settingsStore
        self.talkStore = talkStore
        self.sessionListStore = sessionListStore

        // Use the shared ThemeManager instance so static Design.Colors lookups
        // (which read ThemeManager.shared) reflect the same state the
        // environment-injected instance exposes to views.
        let loadedThemeManager = ThemeManager.shared
        loadedThemeManager.load(from: settingsStore.settings)
        self.themeManager = loadedThemeManager
        self.modelStore = modelStore ?? ModelStore(
            apiClient: apiClient,
            accessTokenProvider: { await sessionStore.currentAccessToken() },
            nativeFeatureClientProvider: { [nativeGatewayClient] in
                nativeGatewayClient?.featureClient
            }
        )
        self.profileStore = profileStore ?? ProfileStore(
            apiClient: apiClient,
            accessTokenProvider: { await sessionStore.currentAccessToken() },
            nativeFeatureClientProvider: { [nativeGatewayClient] in
                nativeGatewayClient?.featureClient
            }
        )
        self.skillsStore = skillsStore ?? SkillsStore(
            apiClient: apiClient,
            accessTokenProvider: { await sessionStore.currentAccessToken() }
        )
        self.cronStore = cronStore ?? CronStore(
            apiClient: apiClient,
            accessTokenProvider: { await sessionStore.currentAccessToken() }
        )
        self.canvasStore = canvasStore ?? HeraldCanvasStore()
        self.notesStore = notesStore ?? NotesStore()
        self.attachmentService = attachmentService ?? AttachmentService(
            apiClient: apiClient,
            accessTokenProvider: { [nativeGatewayClient] in
                // Native mode: /v1/native/media wants the NATIVE gateway
                // bearer (verified against the gateway). BUT basic/pairing
                // auth DELETES nativeGatewayAccessToken at login, so the
                // native token is nil in that mode - and the connector also
                // accepts the relay session token as a paired credential.
                // Fall back to the relay token when the native token is nil.
                if let nativeGatewayClient,
                   let nativeToken = await nativeGatewayClient.nativeAccessToken(),
                   !nativeToken.isEmpty {
                    return nativeToken
                }
                return await sessionStore.currentAccessToken()
            },
            accessTokenRefresher: {
                await sessionStore.refreshAccessTokenIfNeeded()
                return await sessionStore.currentAccessToken()
            }
        )
        self.sensorUploadService = sensorUploadService
        self.dashboardLogService = dashboardLogService ?? DashboardLogService(
            baseURLProvider: { "http://localhost:9119" },
            credentialsProvider: { nil }
        )
        self.apiClient = apiClient
        self.notificationService = notificationService
        self.pushRegistrationCoordinator = pushRegistrationCoordinator
        self.secureStore = secureStore
        self.hostStatusStream = hostStatusStream ?? HostStatusStreamService(
            apiClient: apiClient ?? RelayAPIClient { "" },
            accessTokenProvider: { [nativeGatewayClient] in
                // Build 55: native mode never sets the legacy pairing token
                // (SecureKeys.accessToken), so the old provider returned nil
                // and every gateway restart / status call failed with "Not
                // authenticated — please pair your device first." Present the
                // native gateway bearer when it exists; fall back to the
                // relay session token for basic/pairing auth modes.
                if let nativeGatewayClient,
                   let nativeToken = await nativeGatewayClient.nativeAccessToken(),
                   !nativeToken.isEmpty {
                    return nativeToken
                }
                return await sessionStore.currentAccessToken()
            }
        )
        self.gatewayControl = gatewayControl ?? GatewayControlService(
            apiClient: apiClient ?? RelayAPIClient { "" },
            accessTokenProvider: { [nativeGatewayClient] in
                if let nativeGatewayClient,
                   let nativeToken = await nativeGatewayClient.nativeAccessToken(),
                   !nativeToken.isEmpty {
                    return nativeToken
                }
                return await sessionStore.currentAccessToken()
            }
        )

        // Observe Live Activity push token updates and register with relay
        NotificationCenter.default.addObserver(
            forName: LiveActivityService.pushTokenDidUpdateNotification,
            object: nil,
            queue: .main
        ) { [weak self] notification in
            guard let self, let token = notification.object as? String else { return }
            Task { await self.registerLiveActivityPushToken(token) }
        }

        // When notification permission is granted mid-session, immediately
        // register the stored APNs token so push works without an app restart.
        NotificationCenter.default.addObserver(
            forName: .heraldPushPermissionGranted,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            Task { await self?.registerStoredPushTokenIfNeeded() }
        }
    }

    static func sharedDefault() -> AppContainer {
        sharedDefaultContainer
    }

    /// Returns true once we know which top-level screen to render — either we're
    /// unpaired (OnboardingFlowView shows immediately) or we've finished the
    /// paired-session bootstrap. The old launch splash has been removed; during
    /// the brief window before this flips true the app shows only the deep-ink
    /// background, continuous with the iOS launch image.
    var isLaunchReady: Bool {
        if let nativeGatewayClient {
            // Native gateway doesn't use the pairing/session-bootstrap path
            // at all -- wait for the silent connect() attempt (using any
            // stored Nous token) to resolve one way or the other before
            // deciding whether to show onboarding or the main app.
            //
            // Build 60: the old OR (hasConnectedOnce || connectionStatus != .connecting)
            // caused a flicker storm on cold launch. On app start, connectionStatus is
            // .disconnected (default), so .disconnected != .connecting = true, which
            // made isLaunchReady true BEFORE any connect attempt. Then connect() sets
            // .connecting (isLaunchReady=false, loading surface shows), which fails and
            // sets .reconnecting (isLaunchReady=true, loading surface hides), then the
            // next attempt sets .connecting again -- the surface flickers on every
            // reconnect cycle. Fix: single condition. Before first connect = always
            // show surface. After first connect = never show surface (connection
            // banner handles mid-session reconnect communication).
            return nativeGatewayClient.hasConnectedOnce
        }
        if !pairingStore.isPaired { return true }
        if isInitialized && !sessionStore.isBootstrapping { return true }
        if sessionStore.launchState == .authFailure { return true }
        if case .networkFailure = sessionStore.launchState { return true }
        return false
    }

    /// Derives the native gateway's connection details from the relay base
    /// URL string (e.g. "https://hermes-relay.fihonline.net/v1") -- same
    /// setting the legacy connector path uses, so there's exactly one
    /// user-facing "where's my server" field, not a second hidden one.
    static func resolveNativeGatewayHost(
        from relayBaseURLString: String?
    ) -> (host: String, port: Int, baseURL: String) {
        guard let relayBaseURLString,
              let url = URL(string: relayBaseURLString),
              let host = url.host else {
            // No relay configured at all (fresh install, hostedRelayEnabled
            // false, user hasn't set one) -- last-resort fallback so native
            // gateway mode doesn't crash with a force-unwrap; startLogin will
            // simply fail with a clear network error the user can act on by
            // configuring a relay URL in Settings.
            return (host: "localhost", port: 9119, baseURL: "http://localhost:9119")
        }
        let isHTTPS = (url.scheme?.lowercased() == "https")
        let port = url.port ?? (isHTTPS ? 443 : 80)
        let scheme = isHTTPS ? "https" : "http"
        // Include the port explicitly in baseURL: the gateway WS endpoint
        // (api/ws) lives on the dashboard port (9119) for LAN hosts, and
        // the connector facade on 8010. Dropping the port made every
        // native connect resolve to :80 and fail, so the app looped
        // authenticating/verifying with zero traffic reaching the host.
        let baseURL = port == (isHTTPS ? 443 : 80) ? "\(scheme)://\(host)" : "\(scheme)://\(host):\(port)"
        return (host: host, port: port, baseURL: baseURL)
    }

    static func makeDefault(
        defaults: UserDefaults? = nil,
        processEnvironment: [String: String] = ProcessInfo.processInfo.environment
    ) -> AppContainer {
        let resolvedDefaults: UserDefaults
        if let defaults {
            resolvedDefaults = defaults
        } else if let suiteName = processEnvironment["UITEST_DEFAULTS_SUITE"] {
            resolvedDefaults = UserDefaults(suiteName: suiteName) ?? .standard
        } else {
            resolvedDefaults = .standard
        }

        let persistence = UserDefaultsAppPersistenceStore(defaults: resolvedDefaults)
        let buildConfiguration = AppBuildConfiguration.current()
        let secureStore = KeychainSecureStore(
            serviceName: processEnvironment["UITEST_KEYCHAIN_SERVICE"] ?? "net.fihonline.kallisti.session"
        )
        let settingsStore = SettingsStore(
            persistence: persistence,
            buildConfiguration: buildConfiguration
        )
        let syncCoordinator = MockSyncCoordinator()
        let notificationService = LiveNotificationService()
        let usesMockPairingService = processEnvironment["UITEST_PAIRING_MODE"] == "mock"
        // Mock responses must never mask a failed or missing real pairing on a
        // developer build. They are reserved for the explicitly mocked UI-test
        // harness, which opts in through its launch environment.
        let allowMockFallbacks = usesMockPairingService
        let pairingService: any PairingServiceProtocol
        var activePairingStore: PairingStore?
        var nativeGatewayClient: NativeKallistiClient?

        if processEnvironment["UITEST_PAIRING_MODE"] == "mock" {
            pairingService = MockPairingService()
        } else {
            pairingService = LivePairingService()
        }

        let apiClient = RelayAPIClient {
            activePairingStore?.pairedRelayConfiguration?.baseURLString
                ?? settingsStore.settings.relayConfiguration.activeBaseURLString
                ?? ""
        }
        let pushBrokerClient = buildConfiguration.pushBrokerBaseURL.map { PushBrokerClient(baseURL: $0) }

        // Derive the connector HTTP facade URL from the configured host.
        // Native Kallisti push registration uses the authenticated facade
        // on port 8010, not the MCP transport on port 8767. The old code
        // added 2 to whatever public relay port was configured, producing
        // invalid URLs such as https://relay.example:445 and silently
        // forcing push registration into the wrong fallback path.
        let connectorMCPURL: String? = {
            let relayBase = activePairingStore?.pairedRelayConfiguration?.baseURLString
                ?? settingsStore.settings.relayConfiguration.activeBaseURLString
                ?? ""
            return NativeKallistiClient.facadeBaseURL(for: relayBase)
        }()

        let pushRegistrationCoordinator = PushRegistrationCoordinator(
            relayAPIClient: apiClient,
            brokerClient: pushBrokerClient,
            registrationStore: PushBrokerRegistrationStore(secureStore: secureStore),
            appAttestService: LiveAppAttestService(secureStore: secureStore),
            buildConfiguration: buildConfiguration,
            connectorMCPBaseURL: connectorMCPURL
        )

        let sessionBootstrapService = ResilientSessionBootstrapService(
            primary: LiveSessionBootstrapService(apiClient: apiClient),
            fallback: MockSessionBootstrapService(),
            allowsFallback: { allowMockFallbacks && (activePairingStore?.isPaired != true || usesMockPairingService) }
        )

        let inboxService = ResilientInboxService(
            primary: LiveInboxService(apiClient: apiClient),
            fallback: MockInboxService(),
            allowsFallback: { allowMockFallbacks && (activePairingStore?.isPaired != true || usesMockPairingService) }
        )

        let sessionStore = AppSessionStore(
            bootstrapService: sessionBootstrapService,
            syncCoordinator: syncCoordinator,
            secureStore: secureStore,
            persistence: persistence,
            notificationService: notificationService,
            environmentProvider: { settingsStore.settings.environment }
        )

        let runtimePairingStore = PairingStore(
            pairingService: pairingService,
            sessionStore: sessionStore,
            persistence: persistence,
            environmentProvider: { settingsStore.settings.environment },
            relayBaseURLProvider: { settingsStore.settings.relayConfiguration.activeBaseURLString }
        )
        activePairingStore = runtimePairingStore

        let hostService: any KallistiHostServiceProtocol
        if usesMockPairingService {
            hostService = MockKallistiHostService()
        } else {
            hostService = LiveKallistiHostService(
                apiClient: apiClient,
                accessTokenRefresher: {
                    await sessionStore.refreshAccessTokenIfNeeded()
                    return await sessionStore.currentAccessToken()
                }
            )
        }

        let hostStore = KallistiHostStore(
            hostService: hostService,
            accessTokenProvider: { await sessionStore.currentAccessToken() },
            nativeFeatureClientProvider: { nativeGatewayClient?.featureClient }
        )

        let heraldClient: any HeraldClientProtocol
        if usesMockPairingService {
            heraldClient = MockHeraldClient()
        } else if UserDefaults.standard.object(forKey: "useNativeGateway") == nil
                   || UserDefaults.standard.bool(forKey: "useNativeGateway") {
            // Not hardcoded: derived from the same relay-URL setting the
            // legacy connector path already uses (settingsStore.settings
            // .relayConfiguration -- user-editable in Settings, same field
            // RelayStepView writes during onboarding), falling back to the
            // build-configured default (APP_HOSTED_RELAY_URL in Info.plist)
            // only when the user hasn't set anything -- never a literal
            // string baked into this branch.
            // Resolve the gateway host LIVE from settings on every call, never
            // once at construction: a fresh install has no relay (localhost
            // fallback) but the user types their real server during onboarding,
            // and only lazy resolution honors that updated relay. Baking it at
            // construction made every login/connect keep hitting the stale
            // launch-time localhost regardless of the address the user entered.
            let auth = NativeAuthCoordinator(
                baseURLProvider: { @MainActor in
                    Self.resolveNativeGatewayHost(
                        from: settingsStore.settings.relayConfiguration.activeBaseURLString
                    ).baseURL
                },
                secureStore: secureStore
            )
            let nativeClient = NativeKallistiClient(
                gatewayBaseURLProvider: { @MainActor in
                    Self.resolveNativeGatewayHost(
                        from: settingsStore.settings.relayConfiguration.activeBaseURLString
                    ).baseURL
                },
                authCoordinator: auth,
                secureStore: secureStore
            )
            // Build 68: only auto-connect when a relay URL is actually
            // configured. On a fresh install (UserDefaults wiped, Keychain
            // may still hold an old authMode from a previous install),
            // resolveNativeGatewayHost falls back to http://localhost:9119
            // and connect() loops "Connection struggling" forever against
            // the device itself - the loading surface never releases to
            // onboarding because hasConnectedOnce stays false. Skip the
            // silent connect; onboarding's startPairingLogin / startBasicLogin
            // / startInteractiveLogin all call connect() themselves after the
            // user enters a real relay URL.
            if settingsStore.settings.relayConfiguration.activeBaseURLString != nil {
                Task { @MainActor in
                    await nativeClient.connect()
                }
            }
            heraldClient = nativeClient
            nativeGatewayClient = nativeClient
        } else {
            let liveClient = LiveHeraldClient(
                apiClient: apiClient,
                accessTokenProvider: { await sessionStore.currentAccessToken() },
                accessTokenRefresher: {
                    await sessionStore.refreshAccessTokenIfNeeded()
                    return await sessionStore.currentAccessToken()
                },
                allowDemoFallback: false
            )
            liveClient.reasoningEffortProvider = { settingsStore.settings.reasoningEffort }
            heraldClient = liveClient
        }

        let liveLocationService = LiveLocationService()
        liveLocationService.updateSyncPreference(settingsStore.settings.locationSyncPreference)
        let liveHealthService = LiveHealthService(persistence: persistence)
        let liveMotionService = LiveMotionService()
        let sensorUploadService: SensorUploadService? = usesMockPairingService ? nil : SensorUploadService(
            apiClient: apiClient,
            accessTokenProvider: { await sessionStore.currentAccessToken() },
            accessTokenRefresher: {
                await sessionStore.refreshAccessTokenIfNeeded()
                return await sessionStore.currentAccessToken()
            },
            persistence: persistence,
            isPairedProvider: { activePairingStore?.isPaired == true },
            locationService: liveLocationService,
            healthService: liveHealthService,
            motionService: liveMotionService
        )
        let dashboardLogService = DashboardLogService(
            baseURLProvider: {
                // Derive dashboard URL from the relay host when paired,
                // falling back to the user-configured URL or localhost.
                if let relayURL = activePairingStore?.pairedRelayConfiguration?.baseURLString,
                   let host = URL(string: relayURL)?.host {
                    return "http://\(host):9119"
                }
                return settingsStore.settings.dashboardURL ?? "http://localhost:9119"
            },
            credentialsProvider: {
                guard let user = settingsStore.settings.dashboardUsername,
                      let pass = settingsStore.settings.dashboardPassword else { return nil }
                return (user, pass)
            },
            nativeLogStreamProvider: {
                // Native mode: stream from the connector facade
                // /gw/logs/stream (port 8010) with the native bearer token.
                // The gateway :9119 has no /logs/stream route, so the old
                // dashboard path silently connected to nothing.
                guard let nativeClient = nativeGatewayClient else { return nil }
                guard let facadeBase = await nativeClient.facadeBaseURLString(),
                      let token = await nativeClient.nativeAccessToken(),
                      let url = URL(string: "\(facadeBase)/gw/logs/stream?level=all&source=hermes-gateway")
                else { return nil }
                var request = URLRequest(url: url)
                request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
                return request
            }
        )
        let chatStore = ChatStore(heraldClient: heraldClient, persistence: persistence)
        chatStore.hapticFeedbackEnabled = { [settingsStore] in settingsStore.settings.hapticFeedbackEnabled }

        let permissionsStore = PermissionsStore(
            locationService: liveLocationService,
            healthService: liveHealthService,
            notificationService: notificationService,
            mediaService: processEnvironment["UITEST_PAIRING_MODE"] != nil ? MockMediaService() : LiveMediaService(),
            motionService: liveMotionService
        )
        // Speech authorization trigger: uses the version-appropriate API.
        // On iOS 26+, prepareAuthorization() reserves the speech locale and
        // checks asset status, which triggers the TCC dialog via the modern
        // SpeechAnalyzer/DictationTranscriber APIs (SFSpeechRecognizer
        // .requestAuthorization crashes on iOS 26 betas — FB radar).
        // On iOS 18–25, it falls back to the legacy API.
        permissionsStore.speechAuthorizationTrigger = { @MainActor in
            if #available(iOS 26.0, *) {
                let speechService = LiveSpeechService()
                return await speechService.prepareAuthorization()
            } else {
                // iOS 18–25: use the legacy SFSpeechRecognizer API
                return await SpeechAuthorizationBridge.requestAuthorization()
            }
        }

        let container = AppContainer(
            sessionStore: sessionStore,
            pairingStore: runtimePairingStore,
            nativeGatewayClient: nativeGatewayClient,
            hostStore: hostStore,
            chatStore: chatStore,
            inboxStore: InboxStore(
                inboxService: inboxService,
                persistence: persistence,
                sessionStore: sessionStore,
                allowDemoFallback: allowMockFallbacks,
                isPairedProvider: { activePairingStore?.isPaired == true }
            ),
            permissionsStore: permissionsStore,
            settingsStore: settingsStore,
            talkStore: {
                let ts = TalkStore()
                let apiKeyHolder = APIKeyHolder(secureStore: secureStore)
                // Load the key from Keychain
                Task { await apiKeyHolder.refresh() }
                let mimoTTSService = MimoTTSService(
                    apiKeyProvider: { apiKeyHolder.get() },
                    modelProvider: { settingsStore.settings.mimoTTSModel }
                )
                let appleTTSService = AppleTTSService()
                let tts = FallbackTTSService(
                    primary: mimoTTSService,
                    fallback: appleTTSService,
                    mimoKeyProvider: { apiKeyHolder.get() }
                )
                ts.ttsService = tts
                ts.ttsSettingsProvider = { let s = settingsStore.settings; return (enabled: s.ttsEnabled, voice: s.ttsVoice, autoSpeak: s.ttsAutoSpeak, autoSpeakDuringStreaming: s.ttsAutoSpeakDuringStreaming, appleVoiceIdentifier: s.ttsAppleVoiceIdentifier) }
                ts.apiKeyHolder = apiKeyHolder
                // Gate Talk on connector-side readiness. The MiMo key remains
                // in this device's Keychain and is supplied only in the
                // authenticated readiness/ASR request.
                ts.talkReadinessProvider = { [sessionStore] in
                    let base = settingsStore.settings.relayConfiguration.activeBaseURLString
                        ?? activePairingStore?.pairedRelayConfiguration?.baseURLString
                    guard let base, URL(string: base) != nil else {
                        return (ready: false, blockedReason: "No relay configured.")
                    }
                    let token = await sessionStore.currentAccessToken()
                    guard let apiKey = apiKeyHolder.get(), !apiKey.isEmpty else {
                        return (ready: false, blockedReason: "Mimo API key required — add it in Settings → Voice")
                    }
                    guard let url = URL(string: base)?.appendingPathComponent("talk/readiness") else {
                        return (ready: false, blockedReason: "Invalid relay URL.")
                    }
                    struct Readiness: Decodable {
                        let ready: Bool
                        let hostOnline: Bool?
                        let configured: Bool?
                        let blockedReason: String?
                        let selectedModel: String?
                    }
                    do {
                        var request = URLRequest(url: url)
                        request.httpMethod = "GET"
                        request.setValue("Bearer \(token ?? "")", forHTTPHeaderField: "Authorization")
                        request.setValue(apiKey, forHTTPHeaderField: "X-Herald-MiMo-API-Key")
                        let (data, response) = try await URLSession.shared.data(for: request)
                        guard let http = response as? HTTPURLResponse,
                              (200..<300).contains(http.statusCode) else {
                            let statusCode = (response as? HTTPURLResponse)?.statusCode ?? -1
                            // Build 107: map 401/403 to invalid credential state.
                            // The MiMo key is present but rejected by the server.
                            // Surface a clear message so the user knows to update it.
                            if statusCode == 401 || statusCode == 403 {
                                return (ready: false, blockedReason: "Invalid MiMo API key — update it in Settings → Voice")
                            }
                            return (ready: false, blockedReason: "Talk readiness request failed.")
                        }
                        struct Response: Decodable { let data: Readiness }
                        let resp = try JSONDecoder().decode(Response.self, from: data)
                        return (
                            ready: resp.data.ready,
                            blockedReason: resp.data.blockedReason
                                ?? (resp.data.ready ? nil : "Realtime Talk is not ready on the host.")
                        )
                    } catch {
                        return (
                            ready: false,
                            blockedReason: "Talk readiness check failed: \(error.localizedDescription)"
                        )
                    }
                }

                // Wire the full Talk pipeline when not in UI-test mock mode
                if !usesMockPairingService {
                    let capture = TalkAudioCapture()
                    let asr = MimoASRService(
                        apiKeyProvider: { apiKeyHolder.get() },
                        relayBaseURLProvider: {
                            guard let baseURLString =
                                settingsStore.settings.relayConfiguration.activeBaseURLString
                                ?? activePairingStore?.pairedRelayConfiguration?.baseURLString
                            else {
                                return nil
                            }
                            return URL(string: baseURLString)
                        },
                        accessTokenProvider: { [sessionStore] in
                            await sessionStore.currentAccessToken()
                        },
                    )
                    let playback = PCMPlaybackQueue()
                    let turnClient = TalkTurnClient(heraldClient: heraldClient)
                    let conversationId = UUID()
                    let coordinator = HermesTalkCoordinator(
                        capture: capture,
                        asr: asr,
                        tts: mimoTTSService,
                        turnClient: turnClient,
                        playback: playback,
                        conversationId: conversationId
                    )
                    ts.attachHermesCoordinator(coordinator)
                }

                return ts
            }(),
            sessionListStore: SessionListStore(heraldClient: heraldClient, chatStore: chatStore, settingsStore: settingsStore, persistence: persistence),
            sensorUploadService: sensorUploadService,
            dashboardLogService: dashboardLogService,
            apiClient: apiClient,
            notificationService: notificationService,
            pushRegistrationCoordinator: pushRegistrationCoordinator,
            secureStore: secureStore
        )

        // Build 70: hoist the aux model service so Infrastructure loads at
        // connection time (Settings previously created it lazily on appear).
        if let relayBase = settingsStore.settings.relayConfiguration.activeBaseURLString {
            container.auxService = AuxModelService(
                apiClient: RelayAPIClient { relayBase },
                accessTokenProvider: { await sessionStore.currentAccessToken() },
                nativeFeatureClientProvider: { nativeGatewayClient?.featureClient }
            )
        }

        chatStore.profileStore = container.profileStore
        // Streaming forced on — the sync (non-streaming) path has known bugs
        // with progress tracking, watchdog management, and LiveActivity lifecycle.
        // When the sync path is intentionally surfaced as a user toggle, remove this.
        chatStore.useStreaming = true
        chatStore.ttsService = container.talkStore.ttsService
        chatStore.ttsSettingsProvider = {
            let s = settingsStore.settings
            return (enabled: s.ttsEnabled, voice: s.ttsVoice, autoSpeak: s.ttsAutoSpeak, autoSpeakDuringStreaming: s.ttsAutoSpeakDuringStreaming, appleVoiceIdentifier: s.ttsAppleVoiceIdentifier)
        }

        let refreshUnpairedRelayContext: @MainActor () async -> Void = { [weak sessionStore, weak container] in
            guard container?.pairingStore.isPaired == false else { return }
            await sessionStore?.clearSession()
            guard let relayBaseURL = container?.settingsStore.settings.relayConfiguration.activeBaseURLString,
                  !relayBaseURL.isEmpty else { return }
            _ = relayBaseURL
            await sessionStore?.bootstrap(forceRegistration: true)
            await container?.inboxStore.loadInbox(force: true)
        }

        settingsStore.onEnvironmentChanged = { _ in
            await refreshUnpairedRelayContext()
        }
        settingsStore.onRelayConfigurationChanged = { _ in
            await refreshUnpairedRelayContext()
        }
        settingsStore.onThemeChanged = { [weak container] _ in
            guard let container else { return }
            container.themeManager.load(from: container.settingsStore.settings)
        }

        runtimePairingStore.onPairingChanged = { [weak container] isPaired in
            if isPaired {
                await container?.handlePairingActivated()
            } else {
                await container?.handlePairingRemoved()
            }
        }

        // Keep widget data fresh while app is foregrounded
        container.chatStore.onConversationChanged = { [weak container] in
            container?.updateWidgetData()
        }
        // Keep session list in sync when title is derived or renamed
        container.chatStore.onTitleChanged = { [weak container] (conversationID: UUID, newTitle: String) in
            container?.sessionListStore.updateSessionTitle(id: conversationID, newTitle: newTitle)
            // Persist the title to the server too. updateSessionTitle only
            // mutates the local cached list, so without this the server
            // session stayed "New Chat" and the next session-list refresh
            // (frequent) overwrote the local title back to "New Chat" — the
            // naming regression (2026-08-04). renameSession PATCHes
            // /v1/sessions/{id}; best-effort, local title already applied.
            Task { [weak container] in
                guard let container else { return }
                do {
                    _ = try await container.chatStore.heraldClient.renameSession(
                        id: conversationID, title: newTitle
                    )
                } catch {
                    Logger.app.warning("renameSession failed for \(conversationID.uuidString.prefix(8)): \(error.localizedDescription)")
                }
            }
        }
        container.inboxStore.onOpenConversation = { [weak container] (convId: UUID) in
            guard let container else { return }
            Task { @MainActor in
                // Build 31: merge instead of raw assignment so local-only rows
                // (optimistic sends, in-flight placeholders) survive the server
                // snapshot.  Raw assignment would delete any message that hasn't
                // yet been acknowledged.
                if let conv = try? await container.chatStore.heraldClient.loadConversation(id: convId) {
                    container.chatStore.conversation = container.chatStore.mergeConversationMetadata(
                        from: container.chatStore.conversation,
                        into: conv
                    )
                    container.chatStore.lastTokenUsage = conv.latestUsage
                }
                container.router.selectedTab = .chat
            }
        }
        container.talkStore.onSessionStateChanged = { [weak container] in
            container?.updateWidgetData()
        }
        container.hostStore.onHostChanged = { [weak container] in
            guard let container else { return }
            let isOnline = container.hostStore.isHostOnline
            let becameOnline = isOnline && container.lastKnownHostOnline == false
            container.lastKnownHostOnline = isOnline
            container.updateWidgetData()
            Task { [weak container] in
                await container?.refreshCommandCatalog(force: becameOnline)
            }
        }

        // Wire up WS-based connection status to update ChatStore
        // Build 72: the legacy hostStatusStream SSE (connector/events) is
        // dead for native-gateway users - it never emits .connected, so
        // latency monitoring, model reload, and push re-registration never
        // ran on cold start. Funnel the native client's REAL connection
        // status into the same handler so that logic fires on the native
        // signal.
        nativeGatewayClient?.onConnectionStatusChanged = { [weak container] status in
            Task { @MainActor [weak container] in
                guard let container else { return }
                await container.hostStatusStream.updateConnectionStatus(status)
            }
        }
        container.hostStatusStream.onConnectionStatusChanged = { [weak container] (status: ConnectionStatus) in
            Task { @MainActor [weak container] in
                guard let container else { return }
                container.chatStore.updateConnectionStatus(status)
                // Build 53: re-register push the moment the native gateway
                // comes back online. The connector's stored device token can
                // go stale (test runs, connector restarts, token rotation) and
                // the app previously only re-registered at launch or on APNs
                // permission grant - so Settings could sit at "Not Registered"
                // indefinitely even though the host is healthy. Registration
                // is idempotent and cheap (single POST, no-op on empty token).
                if status == .connected {
                    await container.registerStoredPushTokenIfNeeded()
                }
                // Build 54: reload data that only exists after the socket is
                // live. loadModels() runs once at ChatScreen .task (usually
                // before the first connect finishes) and errors out silently -
                // the model pill then sits as a bare green dot until the user
                // opens the picker, which forces a reload. Same for the host
                // row and session list: refresh them on every reconnect so
                // the UI reflects the live gateway without user action.
                if status == .connected {
                    await container.modelStore.loadModels(force: true)
                    await container.hostStore.refresh()
                    await container.sessionListStore.loadSessions(forceRefresh: true)
                    // Preload infra + latency at connection time so Settings
                    // shows live values the moment it opens. A fresh onboarding writes
                    // the relay after AppContainer initialization, so create the aux
                    // service here as well as at construction time.
                    container.startLatencyMonitoring()
                    container.ensureAuxService()
                    if let aux = container.auxService, aux.tasks.isEmpty {
                        await aux.load()
                    }
                } else {
                    container.stopLatencyMonitoring()
                }
            }
        }

        return container
    }

    /// Ensures native Settings has an auxiliary-model service after a relay
    /// is configured during onboarding. This is idempotent and intentionally does
    /// not replace an existing service, preserving any visible load/error state.
    func ensureAuxService() {
        guard auxService == nil,
              let relayBase = settingsStore.settings.relayConfiguration.activeBaseURLString
        else { return }
        auxService = AuxModelService(
            apiClient: RelayAPIClient { relayBase },
            accessTokenProvider: { [sessionStore] in await sessionStore.currentAccessToken() },
            nativeFeatureClientProvider: { [nativeGatewayClient] in nativeGatewayClient?.featureClient }
        )
    }

    /// Build 70: begin polling connector latency on a 3s cadence. The value
    /// is stored on the container so any view (Settings > Connection) reads
    /// the latest without triggering its own measurement.
    func startLatencyMonitoring() {
        guard latencyMonitorTask == nil else { return }
        latencyMonitorTask = Task { @MainActor in
            while !Task.isCancelled {
                if let nativeClient = nativeGatewayClient {
                    connectorLatencyMs = await nativeClient.measureLatency()
                } else {
                    connectorLatencyMs = nil
                }
                try? await Task.sleep(for: .seconds(3))
            }
        }
    }

    func stopLatencyMonitoring() {
        latencyMonitorTask?.cancel()
        latencyMonitorTask = nil
        connectorLatencyMs = nil
    }

    func initialize() async {
        // Build 108 §15A.4: run the legacy → shared-Keychain token migration
        // once per launch.  Idempotent and best-effort; if the shared
        // Keychain entry is unreachable (no entitlement yet) we log and
        // continue so the rest of the app can boot.
        SharedTokenBridge.migrateOnce()

        guard pairingStore.isPaired else {
            sessionStore.launchState = .unpaired
            return
        }
        guard !isInitialized else {
            // Already initialized — process any pending notification route immediately
            await processPendingNotificationRoute()
            return
        }
        // Single-flight protection: prevent concurrent initialization
        guard !isInitializing else { return }
        isInitializing = true
        defer { isInitializing = false }

        guard await sessionStore.currentAccessToken() != nil else {
            await pairingStore.clearLocalPairing()
            sessionStore.launchState = .unpaired
            return
        }

        await permissionsStore.reloadCapabilities()
        await sessionStore.bootstrap()

        // Check if bootstrap succeeded
        guard sessionStore.state.connectionStatus == .connected else {
            // Launch state is already set by bootstrap()
            return
        }

        // Register notification categories before remote notifications can be acted on
        notificationService?.registerCategories()

        // Build 108 Phase 3W: bridge WatchConnectivity actions into the same
        // identity-anchored pipeline as lock-screen notification actions.
        // (Temporarily disabled until Task 10 wires the Watch target's
        // WatchActionBridge into the iOS host — the type currently lives
        // in the watchOS target.)
        // _ = WatchActionBridge(container: self)

        await hostStore.refresh()
        lastKnownHostOnline = hostStore.isHostOnline
        await chatStore.loadConversationIfNeeded()

        // Update shared state for Control Center widgets
        updateSharedAppState()
        updateSharedGatewayState()
        await inboxStore.loadInbox()
        await sessionListStore.loadSessions()
        await refreshCommandCatalog(force: true)
        await registerStoredPushTokenIfNeeded()
        sensorUploadService?.start()
        await sensorUploadService?.handleAppDidBecomeActive()
        reconcileLiveActivities()
        updateWidgetData()
        isInitialized = true
        sessionStore.launchState = .ready

        // Check for a persisted in-flight checkpoint from a prior background-task expiry.
        // If present and less than 10 minutes old, set the resuming flag so the root
        // view can show a "resuming" indicator. Clear the checkpoint after reading (one-shot).
        if let checkpoint = InFlightCheckpointStore.load(),
           Date().timeIntervalSince(checkpoint.backgroundedAt) < 600 {
            isResumingFromBackground = true
            Logger.app.info("Launch: resuming from background checkpoint (conversation \(checkpoint.conversationID.uuidString.prefix(8)))")
        }
        InFlightCheckpointStore.clear()

        // Process any notification route that arrived during initialization
        await processPendingNotificationRoute()
    }

    func handleNotificationRoute(
        conversationID: UUID?,
        messageID: String?,
        jobID: String?,
        action: String?,
        replyText: String? = nil
    ) {
        let route = PendingNotificationRoute(
            conversationID: conversationID,
            messageID: messageID,
            jobID: jobID,
            action: action,
            replyText: replyText
        )

        if isInitialized {
            // Already initialized — process immediately
            Task { await processRoute(route, replyText: replyText) }
        } else {
            // Store for processing after initialization completes
            pendingNotificationRoute = route
        }
    }

    /// Bridge a Watch-originated action into the same identifier-anchored
    /// pipeline as a lock-screen notification action. The Watch coordinator
    /// calls into this so the iOS side performs exactly one canonical user
    /// submission per Watch-originated reply.
    func handleWatchAction(
        action: String,
        conversationID: UUID?,
        messageID: String?,
        jobID: String?,
        replyText: String? = nil
    ) {
        handleNotificationRoute(
            conversationID: conversationID,
            messageID: messageID,
            jobID: jobID,
            action: action,
            replyText: replyText
        )
    }

    private func processPendingNotificationRoute() async {
        guard let route = pendingNotificationRoute else { return }
        pendingNotificationRoute = nil
        await processRoute(route, replyText: route.replyText)
    }

    private func processRoute(_ route: PendingNotificationRoute, replyText: String? = nil) async {
        // Handle actions that don't require navigation
        switch route.action {
        case NotificationActionID.reply.rawValue:
            guard let conversationID = route.conversationID,
                  let text = replyText, !text.isEmpty else {
                Logger.app.warning("Notification reply: missing conversation ID or empty text")
                return
            }
            let clientMessageID = UUID()
            do {
                _ = try await chatStore.heraldClient.sendMessage(text, conversationID: conversationID, clientMessageID: clientMessageID)
                Logger.app.info("Notification reply: sent to conversation \(conversationID.uuidString.prefix(8))")
            } catch {
                Logger.app.warning("Notification reply failed: \(error.localizedDescription)")
            }
            return

        case NotificationActionID.stop.rawValue:
            guard let jobIDString = route.jobID, let jobID = UUID(uuidString: jobIDString) else {
                Logger.app.warning("Notification stop: missing or invalid job ID")
                return
            }
            do {
                try await chatStore.heraldClient.cancelJob(jobID: jobID)
                Logger.app.info("Notification stop: cancelled job \(jobID.uuidString.prefix(8))")
            } catch {
                Logger.app.warning("Notification stop failed: \(error.localizedDescription)")
            }
            return

        case NotificationActionID.nudge.rawValue:
            guard let conversationID = route.conversationID else {
                Logger.app.warning("Notification nudge: missing conversation ID")
                return
            }
            let nudgeText = "Continue, and give me a concise status update."
            let clientMessageID = UUID()
            do {
                _ = try await chatStore.heraldClient.sendMessage(nudgeText, conversationID: conversationID, clientMessageID: clientMessageID)
                Logger.app.info("Notification nudge: sent to conversation \(conversationID.uuidString.prefix(8))")
            } catch {
                Logger.app.warning("Notification nudge failed: \(error.localizedDescription)")
            }
            return

        case NotificationActionID.remindLater.rawValue:
            // Schedule a reminder notification for 1 hour from now
            let content = UNMutableNotificationContent()
            content.title = "Kallisti"
            content.body = "You asked to be reminded about this conversation."
            content.sound = .default
            content.categoryIdentifier = NotificationCategoryID.sessionReminder.rawValue
            if let conversationID = route.conversationID {
                content.userInfo["conversationId"] = conversationID.uuidString
            }
            let trigger = UNTimeIntervalNotificationTrigger(timeInterval: 3600, repeats: false)
            let request = UNNotificationRequest(
                identifier: "herald-remind-\(UUID().uuidString)",
                content: content,
                trigger: trigger
            )
            do {
                try await UNUserNotificationCenter.current().add(request)
                Logger.app.info("Notification remind-later: scheduled for 1 hour")
            } catch {
                Logger.app.warning("Notification remind-later failed: \(error.localizedDescription)")
            }
            return

        case NotificationActionID.dismiss.rawValue:
            // Dismiss the notification without navigating
            return

        default:
            break
        }

        // Read action or default tap. A notification that references a
        // conversation opens that chat. A generic notification (test push,
        // system alert) has no conversation ID - take the user to the Inbox
        // tab where the full item with its action buttons lives, instead of
        // dumping them on the current chat with nothing to act on.
        router.activeSheet = nil
        router.popToRoot()

        guard let conversationID = route.conversationID else {
            router.switchToTab(.inbox)
            Logger.app.info("Notification route: no conversation ID, opening Inbox tab")
            return
        }

        router.switchToTab(.chat)

        // Load the specific conversation by ID — never fall back to "current" conversation.
        // Clear any stale streaming state first: if the app was suspended mid-stream,
        // the SSE connection is dead, but activeStreams/streamingPhase/pendingMessageSentAt
        // may still be set, causing the UI to show a perpetual "thinking" placeholder
        // even though the relay has already completed the response.
        chatStore.cancelStreaming()

        do {
            let conversation = try await chatStore.heraldClient.loadConversation(id: conversationID)
            // Build 31: merge instead of raw assignment so local-only rows survive.
            chatStore.conversation = chatStore.mergeConversationMetadata(
                from: chatStore.conversation,
                into: conversation
            )
            Logger.app.info("Notification route: loaded conversation \(conversationID.uuidString.prefix(8))")
        } catch {
            Logger.app.warning("Notification route: failed to load conversation \(conversationID.uuidString.prefix(8)): \(error.localizedDescription)")
            // Show a recoverable error state rather than crashing
            // The user will see the chat tab with whatever conversation was last loaded
        }
    }

    func handleAppDidBecomeActive() async {
        // Native gateway first: it bypasses pairing entirely (AppRootView
        // checks nativeGatewayClient before pairingStore), so the pairing
        // guards below would return early and the socket would never heal.
        // iOS kills WS sockets on suspension and receive() may never surface
        // the error - force a healthy socket on foreground instead of letting
        // a phantom connection fail every request.
        if let nativeGatewayClient {
            // iOS preserves Keychain across an uninstall, while a fresh app
            // install has no relay configuration in UserDefaults. Do not let
            // a stale auth marker force that fresh install into a reconnect
            // loop before it can reach onboarding. A configured relay is the
            // local opt-in for cold-start auto-connect; after a live connection
            // the client also owns normal foreground recovery.
            let hasConfiguredRelay = settingsStore.settings.relayConfiguration.activeBaseURLString != nil
            if hasConfiguredRelay || nativeGatewayClient.hasConnectedOnce {
                await nativeGatewayClient.reconnectIfNeeded()
            }
        }

        guard pairingStore.isPaired else { return }
        guard await sessionStore.currentAccessToken() != nil else { return }

        await permissionsStore.reloadCapabilities()
        await hostStore.refresh()
        lastKnownHostOnline = hostStore.isHostOnline
        updateSharedGatewayState()
        await refreshCommandCatalog(force: true)
        await registerStoredPushTokenIfNeeded()
        await sensorUploadService?.handleAppDidBecomeActive()
        talkStore.handleAppDidBecomeActive()
        await talkStore.refreshReadiness()
        reconcileLiveActivities()
        await reportAppStateIfNeeded("foreground")
        updateWidgetData()

        // If a streaming response was in-flight when we backgrounded, the SSE
        // connection may have died silently. Force a conversation reload — if
        // the server has the completed response, we'll pick it up and clear
        // the stale streaming state.
        // D3: Reconcile streamingPhase unconditionally — backgrounding kills
        // the SSE task without a terminal event, so a "Reconnecting…" banner
        // can latch even after the stream resolves.
        chatStore.reconcileStreamingPhase()
        if chatStore.isStreaming {
            await chatStore.recoverStalledStream()
        }

        // Build 33 WSB: reconcile the durable outbox on every foreground —
        // accepted jobs are settled against the relay, expired retry backoffs
        // and queued items for the current conversation are resubmitted.
        await chatStore.recoverOutbox()

        // Native gateway users get authoritative status directly from the
        // WebSocket callback wired above. The legacy connector/events SSE
        // loop emits connecting/disconnected when it cannot reach the old
        // endpoint and overwrites the real native connected state.
        if nativeGatewayClient == nil {
            await hostStatusStream.start()
        } else {
            await hostStatusStream.stop()
        }
    }

    func handleRemoteNotificationWake(pushCategory: String? = nil) async {
        guard pairingStore.isPaired else { return }
        guard await sessionStore.currentAccessToken() != nil else { return }

        await permissionsStore.reloadCapabilities()
        await hostStore.refresh()
        lastKnownHostOnline = hostStore.isHostOnline
        await registerStoredPushTokenIfNeeded()
        await sensorUploadService?.handleAppDidBecomeActive()
        talkStore.handleAppDidBecomeActive()
        await talkStore.refreshReadiness()
        reconcileLiveActivities()
        updateWidgetData()
        // Build 68: a HERALD_MESSAGE_READY push means the turn is done - settle
        // any latched streaming state before reloading, so the chat never sits
        // on a perpetual "thinking" placeholder after a push.
        if pushCategory == NotificationCategoryID.messageReady.rawValue {
            chatStore.settleStreamFromCompletionPush()
        }
        chatStore.reconcileStreamingPhase()  // D3: Same gap as handleAppDidBecomeActive
        await chatStore.loadConversation()
        await inboxStore.loadInbox(force: true)
    }

    func handleBackgroundRefresh(_ task: BGAppRefreshTask) async {
        // Schedule the next background refresh before we start
        let request = BGAppRefreshTaskRequest(identifier: "net.fihonline.kallisti.refresh")
        request.earliestBeginDate = Date(timeIntervalSinceNow: 15 * 60)
        try? BGTaskScheduler.shared.submit(request)

        guard pairingStore.isPaired else {
            task.setTaskCompleted(success: true)
            return
        }
        guard await sessionStore.currentAccessToken() != nil else {
            task.setTaskCompleted(success: true)
            return
        }

        let expiration = Task {
            try? await Task.sleep(for: .seconds(25))
            task.setTaskCompleted(success: false)
        }

        await chatStore.loadConversation()
        await inboxStore.loadInbox(force: true)
        await hostStore.refresh()

        expiration.cancel()
        task.setTaskCompleted(success: true)
    }

    func handleSystemLaunch() async {
        guard pairingStore.isPaired else { return }
        guard await sessionStore.currentAccessToken() != nil else { return }

        sensorUploadService?.start()
        await sensorUploadService?.handleSystemLaunch()
        await registerStoredPushTokenIfNeeded()
        await talkStore.refreshReadiness()
        reconcileLiveActivities()
        await reportAppStateIfNeeded("foreground")
    }

    private func handlePairingActivated() async {
        isInitialized = false
        chatStore.reset()
        inboxStore.reset()
        sessionListStore.reset()
        modelStore.reset()
        profileStore.reset()
        skillsStore.reset()
        gatewayControl.reset()
        cronStore.reset()
        await initialize()

        // Start sensor data pipeline
        sensorUploadService?.start()
        await talkStore.refreshReadiness()
    }

    /// Registers the APNs device token with the relay so it can send silent push notifications.
    func registerPushTokenIfNeeded(_ token: String) async {
        // Native gateway path: uses native-gateway bearer auth and the
        // connector facade URL, bypassing legacy pairing/apiClient entirely.
        if let nativeGatewayClient {
            guard settingsStore.settings.notificationsEnabled else {
                sessionStore.state.pushTokenRegistered = false
                return
            }
            let normalizedToken = token.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !normalizedToken.isEmpty else { return }
            let pushEnvironment = Self.apnsEnvironment
            let accepted = await nativeGatewayClient.registerPushToken(
                normalizedToken,
                pushEnvironment: pushEnvironment
            )
            sessionStore.state.pushTokenRegistered = accepted
            await notificationService?.markPushTokenRegistered(accepted)
            return
        }

        guard pairingStore.isPaired,
              let apiClient,
              let notificationService
        else { return }

        // Respect the user's in-app notifications toggle.
        // If disabled, deactivate any existing registration on the relay
        // so the user actually stops receiving pushes.
        guard settingsStore.settings.notificationsEnabled else {
            // Always attempt deactivation -- the relay may have an active
            // registration from a previous session even if the local flag is false.
            await deactivatePushRegistration()
            await notificationService.markPushTokenRegistered(false)
            sessionStore.state.pushTokenRegistered = false
            return
        }

        let normalizedToken = token.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !normalizedToken.isEmpty else { return }

        await notificationService.updatePushToken(normalizedToken)

        guard let accessToken = await sessionStore.currentAccessToken() else {
            await notificationService.markPushTokenRegistered(false)
            sessionStore.state.pushTokenRegistered = false
            return
        }

        guard let deviceID = sessionStore.state.deviceID else {
            await notificationService.markPushTokenRegistered(false)
            sessionStore.state.pushTokenRegistered = false
            return
        }

        // APNs environment follows the build: TestFlight/App Store are
        // production, devicectl/sideloaded builds are development. The
        // relay sends to the environment stored on the registration, so
        // this MUST match the token type iOS actually issued.
        let pushEnvironment = Self.apnsEnvironment

        do {
            let didRegister = try await pushRegistrationCoordinator?.registerPushToken(
                normalizedToken,
                relayConfiguration: settingsStore.settings.relayConfiguration,
                accessToken: accessToken,
                deviceID: deviceID,
                installationID: sessionStore.state.installationID,
                bundleID: Bundle.main.bundleIdentifier ?? "net.fihonline.kallisti",
                appVersion: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "1.0.0",
                pushEnvironment: pushEnvironment
            )
            await notificationService.markPushTokenRegistered(didRegister ?? false)
            sessionStore.state.pushTokenRegistered = didRegister ?? false
        } catch {
            // Non-critical -- token will be retried on next app launch
            await notificationService.markPushTokenRegistered(false)
            sessionStore.state.pushTokenRegistered = false
        }
    }

    /// Tells the relay to deactivate push registrations for this device.
    private func deactivatePushRegistration() async {
        guard let accessToken = await sessionStore.currentAccessToken() else { return }
        if let pushRegistrationCoordinator {
            try? await pushRegistrationCoordinator.deactivatePushRegistration(accessToken: accessToken)
            return
        }
        guard let apiClient else { return }

        struct DeactivateResponse: Decodable {
            let deactivated: Bool?
        }

        _ = try? await apiClient.post(path: "push/deactivate", accessToken: accessToken) as DeactivateResponse
    }

    /// Persists a freshly delivered APNs device token into Keychain (ThisDeviceOnly,
    /// AfterFirstUnlock) and attempts registration with the relay. Called from
    /// `UIApplicationDelegate.didRegisterForRemoteNotificationsWithDeviceToken`.
    func persistAndRegisterAPNsToken(_ token: String) async {
        let normalized = token.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !normalized.isEmpty else { return }
        if let secureStore {
            await secureStore.store(key: Self.apnsTokenKeychainKey, value: normalized)
        }
        // Clear the legacy UserDefaults copy if still present — the token now
        // lives in Keychain, which is excluded from iCloud backups.
        UserDefaults.standard.removeObject(forKey: Self.legacyAPNsTokenDefaultsKey)
        didMigrateLegacyAPNsToken = true
        await registerPushTokenIfNeeded(normalized)
    }

    /// Re-registers the currently stored APNs token with the relay. Used when
    /// settings that affect push registration change (e.g. the notifications
    /// toggle) so the user immediately sees the effect.
    func reregisterStoredPushToken() async {
        guard let token = await currentStoredAPNsToken() else { return }
        await registerPushTokenIfNeeded(token)
    }

    private func registerStoredPushTokenIfNeeded() async {
        guard let storedToken = await currentStoredAPNsToken() else { return }
        await registerPushTokenIfNeeded(storedToken)
    }

    /// Register a Live Activity push token with the connector so it can push
    /// remote updates to the Lock Screen / Dynamic Island activity.
    ///
    /// Uses the standard push/register endpoint with tokenKind=liveActivity.
    /// The connector returns a flat {registered: true, environment: ...} --
    /// NOT the wrapped {data: {registered: ...}} envelope that the relay
    /// uses. Native-gateway mode authenticates with the native bearer token
    /// against the connector facade URL; legacy mode uses the relay client.
    private func registerLiveActivityPushToken(_ token: String) async {
        let pushEnvironment = Self.apnsEnvironment
        let bundleId = Bundle.main.bundleIdentifier ?? "net.fihonline.kallisti"
        let normalizedToken = token.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !normalizedToken.isEmpty else { return }

        // Native path: use native bearer + connector facade URL.
        if let nativeGatewayClient {
            // Build 55: refresh first — same expired-bearer bug as
            // registerPushToken. A stale token 401s the connector even
            // though the socket is fine, leaving the Live Activity without
            // push-update capability silently.
            guard let accessToken = try? await nativeGatewayClient.refreshAccessToken(),
                  !accessToken.isEmpty else {
                Logger.app.warning("registerLiveActivityPushToken: no native access token")
                return
            }
            guard let facadeBase = await nativeGatewayClient.facadeBaseURLString(),
                  let url = URL(string: "\(facadeBase)/v1/push/register") else {
                Logger.app.warning("registerLiveActivityPushToken: invalid facade URL")
                return
            }

            var request = URLRequest(url: url)
            request.httpMethod = "POST"
            request.timeoutInterval = 8
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
            let body: [String: Any] = [
                "apnsToken": normalizedToken,
                "pushEnvironment": pushEnvironment,
                "bundleId": bundleId,
                "tokenKind": "liveActivity",
            ]
            request.httpBody = try? JSONSerialization.data(withJSONObject: body)

            do {
                let (data, response) = try await URLSession.shared.data(for: request)
                let status = (response as? HTTPURLResponse)?.statusCode ?? 0
                // Connector returns flat {registered: true, environment: "..."}
                // NOT the wrapped {data: {registered: ...}} envelope.
                if status == 200,
                   let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                   json["registered"] as? Bool == true {
                    Logger.app.info("registerLiveActivityPushToken: accepted (native, environment=\(pushEnvironment))")
                    return
                }
                Logger.app.warning("registerLiveActivityPushToken: native HTTP \(status)")
            } catch {
                Logger.app.warning("registerLiveActivityPushToken failed (native): \(error.localizedDescription)")
            }
            return
        }

        // Legacy path: use relay client.
        guard let accessToken = await sessionStore.currentAccessToken(),
              let relayURL = settingsStore.settings.relayConfiguration.activeBaseURLString
                ?? pairingStore.pairedRelayConfiguration?.baseURLString,
              !relayURL.isEmpty,
              let deviceID = sessionStore.state.deviceID
        else { return }

        let client = RelayAPIClient { relayURL }
        struct Body: Encodable {
            let deviceId: String
            let transport: String
            let apnsToken: String
            let pushEnvironment: String
            let bundleId: String
            // Distinguishes this from the device's own alert-push token --
            // ActivityKit gives each Live Activity its own push-to-update
            // token, which the connector must store and address separately
            // (see ConnectorState.live_activity_push_token). Without this,
            // every Live Activity token rotation -- i.e. every chat turn --
            // silently overwrote the device's real alert-push token.
            let tokenKind: String = "liveActivity"
        }
        struct RegisterResponse: Decodable {
            private struct WrappedData: Decodable {
                let registered: Bool
            }

            let registered: Bool
            let environment: String?

            private enum CodingKeys: String, CodingKey {
                case registered
                case environment
                case data
            }

            init(from decoder: Decoder) throws {
                let container = try decoder.container(keyedBy: CodingKeys.self)
                if let flat = try container.decodeIfPresent(Bool.self, forKey: .registered) {
                    registered = flat
                } else {
                    registered = try container.decode(WrappedData.self, forKey: .data).registered
                }
                environment = try container.decodeIfPresent(String.self, forKey: .environment)
            }
        }
        let body = Body(
            deviceId: deviceID.uuidString.lowercased(),
            transport: "direct",
            apnsToken: normalizedToken,
            pushEnvironment: pushEnvironment,
            bundleId: bundleId
        )
        do {
            let response: RegisterResponse = try await client.post(
                path: "push/register",
                body: body,
                accessToken: accessToken
            )
            if response.registered {
                Logger.app.info("registerLiveActivityPushToken: accepted (legacy, environment=\(pushEnvironment))")
            }
        } catch {
            // Non-critical -- will be retried on next token rotation
            Logger.app.warning("registerLiveActivityPushToken failed (legacy): \(error.localizedDescription)")
        }
    }

    /// Reads the APNs token from Keychain. On first launch after upgrading from
    /// a pre-1.1.1 build, migrates any legacy UserDefaults-stored token into
    /// Keychain and removes the UserDefaults entry.
    private func currentStoredAPNsToken() async -> String? {
        if let secureStore,
           let token = await secureStore.retrieve(key: Self.apnsTokenKeychainKey) {
            if !didMigrateLegacyAPNsToken {
                UserDefaults.standard.removeObject(forKey: Self.legacyAPNsTokenDefaultsKey)
                didMigrateLegacyAPNsToken = true
            }
            return token
        }

        // Legacy migration path: copy any token that a prior build wrote to
        // UserDefaults into Keychain, then remove it from UserDefaults.
        guard !didMigrateLegacyAPNsToken else { return nil }
        didMigrateLegacyAPNsToken = true
        guard let legacyToken = UserDefaults.standard.string(forKey: Self.legacyAPNsTokenDefaultsKey) else {
            return nil
        }
        if let secureStore {
            await secureStore.store(key: Self.apnsTokenKeychainKey, value: legacyToken)
        }
        UserDefaults.standard.removeObject(forKey: Self.legacyAPNsTokenDefaultsKey)
        return legacyToken
    }

    /// Fetches the dynamic slash command catalog from the connected Hermes host.
    /// Merges built-in commands, gateway commands, skills, and personality options.
    func refreshCommandCatalog(force: Bool = false) async {
        if !force,
           let lastCommandCatalogRefreshAt,
           Date().timeIntervalSince(lastCommandCatalogRefreshAt) < Self.commandCatalogRefreshInterval {
            return
        }

        guard let token = await sessionStore.currentAccessToken(),
              let client = apiClient else { return }

        struct CatalogResponse: Decodable {
            let commands: [RemoteCommand]?
            let skills: [RemoteSkill]?
            let personalities: [RemotePersonality]?
            let quickCommands: [RemoteQuickCommand]?
            let activeModel: ActiveModel?

            struct RemoteCommand: Decodable {
                let name: String
                let description: String
                let category: String?
                let args: String?
            }
            struct RemoteSkill: Decodable {
                let name: String
                let description: String
            }
            struct RemotePersonality: Decodable {
                let name: String
                let description: String
            }
            struct RemoteQuickCommand: Decodable {
                let name: String
                let description: String
            }
            struct ActiveModel: Decodable {
                let name: String
                let provider: String?
                let contextWindow: Int?
            }
        }

        do {
            let response: CatalogResponse = try await client.get(
                path: "commands",
                accessToken: token
            )

            var catalog = SlashCommand.localCommands
            var catalogIDs = Set(catalog.map(\.id))
            let remoteCommands = response.commands ?? []
            let skills = response.skills ?? []
            let personalities = response.personalities ?? []
            let quickCommands = response.quickCommands ?? []

            // Add remote built-in commands (skip any that overlap with local)
            for cmd in remoteCommands {
                let command = SlashCommand.fromRemote(
                    name: cmd.name,
                    description: cmd.description,
                    category: cmd.category ?? "Agent",
                    args: cmd.args
                )
                if catalogIDs.insert(command.id).inserted {
                    catalog.append(command)
                }
            }

            // Add skill commands
            for skill in skills {
                let command = SlashCommand.fromSkill(name: skill.name, description: skill.description)
                if catalogIDs.insert(command.id).inserted {
                    catalog.append(command)
                }
            }

            // `/personality <name>` suggestions only appear once the user starts
            // typing `/personality`, keeping the top-level dropdown manageable.
            for personality in personalities {
                let command = SlashCommand.fromPersonality(
                    name: personality.name,
                    description: personality.description
                )
                if catalogIDs.insert(command.id).inserted {
                    catalog.append(command)
                }
            }

            // Herald docs say quick commands resolve at dispatch time and are not
            // included in built-in autocomplete tables, but we still track them so
            // typed commands can be considered part of the known catalog.
            for quickCommand in quickCommands {
                let command = SlashCommand.fromQuickCommand(
                    name: quickCommand.name,
                    description: quickCommand.description
                )
                if catalogIDs.insert(command.id).inserted {
                    catalog.append(command)
                }
            }

            if remoteCommands.isEmpty && skills.isEmpty && personalities.isEmpty && quickCommands.isEmpty {
                chatStore.resetCommandCatalog()
            } else {
                chatStore.replaceCommandCatalog(
                    catalog,
                    activeModel: response.activeModel?.name,
                    contextWindow: response.activeModel?.contextWindow
                )
                lastCommandCatalogRefreshAt = .now
            }
        } catch {
            // Fallback to built-in list — catalog is a nice-to-have
            chatStore.resetCommandCatalog()
        }
    }

    func reportAppStateIfNeeded(_ state: String) async {
        guard pairingStore.isPaired, let apiClient, let accessToken = await sessionStore.currentAccessToken() else {
            return
        }

        struct AppStateBody: Encodable {
            let state: String
        }

        struct AppStateResponse: Decodable {}

        do {
            _ = try await apiClient.post(
                path: "device/app-state",
                body: AppStateBody(state: state),
                accessToken: accessToken
            ) as AppStateResponse
        } catch {
            // Non-fatal — app state reporting is best-effort
        }
    }

    /// Snapshots current app state into the App Group shared container
    /// so Home Screen widgets and CarPlay widgets can display it.
    func updateWidgetData() {
        let lastMessage = chatStore.conversation?.messages.last
        var data = SharedWidgetDataStore.read()
        data.hostName = hostStore.currentHost?.resolvedDisplayName
        data.hostOnline = hostStore.isHostOnline
        data.voiceSessionActive = talkStore.isSessionActive
        data.updatedAt = .now
        data.relayBaseURL = pairingStore.pairedRelayConfiguration?.baseURLString
            ?? settingsStore.settings.relayConfiguration.activeBaseURLString
        if let msg = lastMessage {
            data.lastMessagePreview = String(msg.content.prefix(120))
            data.lastMessageSender = msg.sender.rawValue
            data.lastMessageAt = msg.timestamp
        }
        SharedWidgetDataStore.write(data)
    }

    /// Retry initialization after a network/server failure.
    func retryInitialization() async {
        isInitialized = false
        await initialize()
    }

    /// Clear pairing and return to onboarding after an auth failure.
    func repairFromAuthFailure() async {
        await pairingStore.clearLocalPairing()
        await sessionStore.clearSession()
        isInitialized = false
        sessionStore.launchState = .unpaired
    }

    private func handlePairingRemoved() async {
        isInitialized = false
        await talkStore.endSessionIfNeeded()
        talkStore.reset()
        sensorUploadService?.stop()
        sensorUploadService?.resetOutbox()
        router.switchToTab(.chat)
        router.activeSheet = nil
        router.resetAll()
        chatStore.reset()
        inboxStore.reset()
        sessionListStore.reset()
        modelStore.reset()
        profileStore.reset()
        skillsStore.reset()
        gatewayControl.reset()
        cronStore.reset()
        hostStore.reset()
        lastKnownHostOnline = false
        lastCommandCatalogRefreshAt = nil
        LiveActivityService.endAllActivities()
        SharedWidgetDataStore.write(.empty)
        // Forget the cached push-broker grant so a future re-pair mints a fresh
        // one instead of replaying a send-grant tied to the revoked session.
        await pushRegistrationCoordinator?.clearLocalBrokerRegistration()
    }

    private func reconcileLiveActivities() {
        if talkStore.isSessionActive || chatStore.isStreaming {
            return
        }
        LiveActivityService.endAllActivities()
    }

    // MARK: - Shared State (Control Center)

    /// Persist relay URL and access token to the cross-process support
    /// module (`HeraldSupport`) so the HeraldControls extension can make
    /// authenticated gateway calls without launching the main app.  The
    /// legacy `KallistiAppState` UserDefaults entry is mirrored for backward
    /// compatibility with widget binaries that still read it.
    private func updateSharedAppState() {
        guard let relayURL = settingsStore.settings.relayConfiguration.activeBaseURLString
            ?? pairingStore.pairedRelayConfiguration?.baseURLString
        else { return }
        Task {
            let token = await sessionStore.currentAccessToken()
            // Build 108 §15A: publish to the shared Keychain (extension-
            // safe) and the App Group URL store.
            SharedTokenBridge.publish(relayBaseURL: relayURL, accessToken: token)
            // Legacy widget/control binaries still read the App-Group
            // UserDefaults entry for the URL; mirror it so they don't
            // see an empty value after upgrade.
            KallistiAppState.shared.update(relayBaseURL: relayURL, accessToken: nil)
        }
    }

    /// Persist gateway health to App Group UserDefaults for Control Center
    /// widget display. Called after host refresh and on app foreground.
    private func updateSharedGatewayState() {
        GatewayState.shared.update(
            connected: hostStore.isHostOnline,
            activeJobs: chatStore.isStreaming ? 1 : 0,
            version: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        )
    }
}
