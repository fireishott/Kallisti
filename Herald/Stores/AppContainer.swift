import Foundation
import os

extension Logger {
    static let app = Logger(subsystem: "net.fihonline.herald", category: "app")
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
    private static let legacyAPNsTokenDefaultsKey = "herald.apns.deviceToken"
    static let apnsTokenKeychainKey = "herald.apns.deviceToken"
    private static let sharedDefaultContainer = AppContainer.makeDefault()

    let router = TabRouter()
    let sessionStore: AppSessionStore
    let pairingStore: PairingStore
    let hostStore: HeraldHostStore
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
    let attachmentService: AttachmentService
    let sensorUploadService: SensorUploadService?
    let themeManager: ThemeManager
    private let apiClient: RelayAPIClient?
    private let notificationService: (any NotificationServiceProtocol)?
    private let pushRegistrationCoordinator: PushRegistrationCoordinator?
    private let secureStore: (any SecureStoreProtocol)?
    private var didMigrateLegacyAPNsToken = false
    private var isInitialized = false
    private var isInitializing = false
    private var lastCommandCatalogRefreshAt: Date?
    private var lastKnownHostOnline = false

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
        hostStore: HeraldHostStore,
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
        attachmentService: AttachmentService? = nil,
        sensorUploadService: SensorUploadService? = nil,
        apiClient: RelayAPIClient? = nil,
        notificationService: (any NotificationServiceProtocol)? = nil,
        pushRegistrationCoordinator: PushRegistrationCoordinator? = nil,
        secureStore: (any SecureStoreProtocol)? = nil
    ) {
        self.sessionStore = sessionStore
        self.pairingStore = pairingStore
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
            accessTokenProvider: { await sessionStore.currentAccessToken() }
        )
        self.profileStore = profileStore ?? ProfileStore(
            apiClient: apiClient,
            accessTokenProvider: { await sessionStore.currentAccessToken() }
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
        self.attachmentService = attachmentService ?? AttachmentService(
            apiClient: apiClient,
            accessTokenProvider: { await sessionStore.currentAccessToken() },
            accessTokenRefresher: {
                await sessionStore.refreshAccessTokenIfNeeded()
                return await sessionStore.currentAccessToken()
            }
        )
        self.sensorUploadService = sensorUploadService
        self.apiClient = apiClient
        self.notificationService = notificationService
        self.pushRegistrationCoordinator = pushRegistrationCoordinator
        self.secureStore = secureStore
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
        if !pairingStore.isPaired { return true }
        // Launch is ready when initialization succeeded OR when we have a recoverable error
        return (isInitialized && !sessionStore.isBootstrapping)
            || sessionStore.launchState == .authFailure
            || sessionStore.launchState == .networkFailure("")
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
            serviceName: processEnvironment["UITEST_KEYCHAIN_SERVICE"] ?? "net.fihonline.herald.session"
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
        let pushRegistrationCoordinator = PushRegistrationCoordinator(
            relayAPIClient: apiClient,
            brokerClient: pushBrokerClient,
            registrationStore: PushBrokerRegistrationStore(secureStore: secureStore),
            appAttestService: LiveAppAttestService(secureStore: secureStore),
            buildConfiguration: buildConfiguration
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

        let hostService: any HeraldHostServiceProtocol
        if usesMockPairingService {
            hostService = MockHeraldHostService()
        } else {
            hostService = LiveHeraldHostService(
                apiClient: apiClient,
                accessTokenRefresher: {
                    await sessionStore.refreshAccessTokenIfNeeded()
                    return await sessionStore.currentAccessToken()
                }
            )
        }

        let hostStore = HeraldHostStore(
            hostService: hostService,
            accessTokenProvider: { await sessionStore.currentAccessToken() }
        )

        let heraldClient: any HeraldClientProtocol
        if usesMockPairingService {
            heraldClient = MockHeraldClient()
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
        let chatStore = ChatStore(heraldClient: heraldClient, persistence: persistence)

        let container = AppContainer(
            sessionStore: sessionStore,
            pairingStore: runtimePairingStore,
            hostStore: hostStore,
            chatStore: chatStore,
            inboxStore: InboxStore(
                inboxService: inboxService,
                persistence: persistence,
                sessionStore: sessionStore,
                allowDemoFallback: allowMockFallbacks
            ),
            permissionsStore: PermissionsStore(
                locationService: liveLocationService,
                healthService: liveHealthService,
                notificationService: notificationService,
                mediaService: processEnvironment["UITEST_PAIRING_MODE"] != nil ? MockMediaService() : LiveMediaService(),
                motionService: liveMotionService
            ),
            settingsStore: settingsStore,
            talkStore: {
                let ts = TalkStore()
                let apiKeyHolder = APIKeyHolder(secureStore: secureStore)
                // Load the key from Keychain
                Task { await apiKeyHolder.refresh() }
                let tts = MimoTTSService(apiKeyProvider: { apiKeyHolder.get() })
                ts.ttsService = tts
                ts.ttsSettingsProvider = { let s = settingsStore.settings; return (enabled: s.ttsEnabled, voice: s.ttsVoice, autoSpeak: s.ttsAutoSpeak) }
                ts.apiKeyHolder = apiKeyHolder

                // Wire the full Talk pipeline when not in UI-test mock mode
                if !usesMockPairingService {
                    let capture = TalkAudioCapture()
                    let asr = MimoASRService(apiKeyProvider: { apiKeyHolder.get() })
                    let playback = PCMPlaybackQueue()
                    let turnClient = TalkTurnClient(heraldClient: heraldClient)
                    let conversationId = UUID()
                    let coordinator = HermesTalkCoordinator(
                        capture: capture,
                        asr: asr,
                        tts: tts,
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
            apiClient: apiClient,
            notificationService: notificationService,
            pushRegistrationCoordinator: pushRegistrationCoordinator,
            secureStore: secureStore
        )

        chatStore.profileStore = container.profileStore

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

        return container
    }

    func initialize() async {
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

        await hostStore.refresh()
        lastKnownHostOnline = hostStore.isHostOnline
        await chatStore.loadConversationIfNeeded()
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

    private func processPendingNotificationRoute() async {
        guard let route = pendingNotificationRoute else { return }
        pendingNotificationRoute = nil
        await processRoute(route, replyText: route.replyText)
    }

    private func processRoute(_ route: PendingNotificationRoute, replyText: String? = nil) async {
        // Handle actions that don't require navigation
        switch route.action {
        case NotificationActionID.reply:
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

        case NotificationActionID.stop:
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

        case NotificationActionID.nudge:
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

        default:
            break
        }

        // Read action or default tap — navigate to chat tab
        router.activeSheet = nil
        router.popToRoot()
        router.switchToTab(.chat)

        guard let conversationID = route.conversationID else {
            Logger.app.info("Notification route: no conversation ID, staying on current chat")
            return
        }

        // Load the specific conversation by ID — never fall back to "current" conversation
        do {
            let conversation = try await chatStore.heraldClient.loadConversation(id: conversationID)
            chatStore.conversation = conversation
            Logger.app.info("Notification route: loaded conversation \(conversationID.uuidString.prefix(8))")
        } catch {
            Logger.app.warning("Notification route: failed to load conversation \(conversationID.uuidString.prefix(8)): \(error.localizedDescription)")
            // Show a recoverable error state rather than crashing
            // The user will see the chat tab with whatever conversation was last loaded
        }
    }

    func handleAppDidBecomeActive() async {
        guard pairingStore.isPaired else { return }
        guard await sessionStore.currentAccessToken() != nil else { return }

        await permissionsStore.reloadCapabilities()
        await hostStore.refresh()
        lastKnownHostOnline = hostStore.isHostOnline
        await refreshCommandCatalog(force: true)
        await registerStoredPushTokenIfNeeded()
        await sensorUploadService?.handleAppDidBecomeActive()
        talkStore.handleAppDidBecomeActive()
        await talkStore.refreshReadiness()
        reconcileLiveActivities()
        await reportAppStateIfNeeded("foreground")
        updateWidgetData()
    }

    func handleRemoteNotificationWake() async {
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
        await chatStore.loadConversation()
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
        cronStore.reset()
        await initialize()

        // Start sensor data pipeline
        sensorUploadService?.start()
        await talkStore.refreshReadiness()
    }

    /// Registers the APNs device token with the relay so it can send silent push notifications.
    func registerPushTokenIfNeeded(_ token: String) async {
        guard pairingStore.isPaired,
              let apiClient,
              let notificationService
        else { return }

        // Respect the user's in-app notifications toggle.
        // If disabled, deactivate any existing registration on the relay
        // so the user actually stops receiving pushes.
        guard settingsStore.settings.notificationsEnabled else {
            // Always attempt deactivation — the relay may have an active
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

        if notificationService.isPushTokenRegistered,
           notificationService.currentPushToken == normalizedToken {
            sessionStore.state.pushTokenRegistered = true
            return
        }

        guard let deviceID = sessionStore.state.deviceID else {
            await notificationService.markPushTokenRegistered(false)
            sessionStore.state.pushTokenRegistered = false
            return
        }

        #if DEBUG
        let pushEnvironment = "development"
        #else
        let pushEnvironment = "production"
        #endif

        do {
            let didRegister = try await pushRegistrationCoordinator?.registerPushToken(
                normalizedToken,
                relayConfiguration: settingsStore.settings.relayConfiguration,
                accessToken: accessToken,
                deviceID: deviceID,
                installationID: sessionStore.state.installationID,
                bundleID: Bundle.main.bundleIdentifier ?? "net.fihonline.herald",
                appVersion: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "1.0.0",
                pushEnvironment: pushEnvironment
            )
            await notificationService.markPushTokenRegistered(didRegister ?? false)
            sessionStore.state.pushTokenRegistered = didRegister ?? false
        } catch {
            // Non-critical — token will be retried on next app launch
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

        _ = try? await apiClient.post(
            path: "device/app-state",
            body: AppStateBody(state: state),
            accessToken: accessToken
        ) as AppStateResponse
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
}
