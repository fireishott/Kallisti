import Foundation
import os

@MainActor
@Observable
final class AppSessionStore {
    private enum SecureKeys {
        static let accessToken = "session.accessToken"
        static let refreshToken = "session.refreshToken"
    }

    private static let logger = Logger(subsystem: "net.fihonline.herald", category: "session")

    var state: AppSessionState {
        didSet { persistence.saveSessionState(state) }
    }
    var isBootstrapping = false
    var lastErrorMessage: String?
    /// Explicit launch outcome for UI to observe.
    var launchState: LaunchState = .initializing

    private let bootstrapService: any SessionBootstrapServiceProtocol
    private let syncCoordinator: any SyncCoordinatorProtocol
    private let secureStore: any SecureStoreProtocol
    private let persistence: any AppPersistenceStoreProtocol
    private let notificationService: any NotificationServiceProtocol
    private let environmentProvider: @MainActor () -> AppEnvironment

    init(
        bootstrapService: any SessionBootstrapServiceProtocol,
        syncCoordinator: any SyncCoordinatorProtocol,
        secureStore: any SecureStoreProtocol,
        persistence: any AppPersistenceStoreProtocol,
        notificationService: any NotificationServiceProtocol,
        environmentProvider: @escaping @MainActor () -> AppEnvironment
    ) {
        self.bootstrapService = bootstrapService
        self.syncCoordinator = syncCoordinator
        self.secureStore = secureStore
        self.persistence = persistence
        self.notificationService = notificationService
        self.environmentProvider = environmentProvider
        self.state = persistence.loadSessionState() ?? AppSessionState()
    }

    func bootstrap(forceRegistration: Bool = false) async {
        guard !isBootstrapping else { return }

        isBootstrapping = true
        lastErrorMessage = nil
        launchState = .initializing
        state.connectionStatus = .connecting
        state.syncStatus = .syncing

        defer { isBootstrapping = false }

        let request = makeRegistrationRequest()
        let accessTokenBeforeBootstrap = await currentAccessToken()
        let needsRegistration =
            forceRegistration
            || !state.deviceRegistered
            || state.deviceID == nil
            || accessTokenBeforeBootstrap == nil

        do {
            if needsRegistration {
                let response = try await bootstrapService.registerDevice(request)
                await applySessionState(response.state, tokens: response.tokens)
            }

            try await loadAndApplySessionState(installationID: request.installationID)
            launchState = .ready
        } catch {
            // Attempt refresh and reload
            if await attemptRefreshAndReload(installationID: request.installationID) {
                launchState = .ready
                return
            }

            // If the error is unauthorized and we have a refresh token, try forced registration
            if isUnauthorizedError(error) {
                Self.logger.warning("Bootstrap received 401, attempting forced registration")
                do {
                    let response = try await bootstrapService.registerDevice(request)
                    await applySessionState(response.state, tokens: response.tokens)
                    try await loadAndApplySessionState(installationID: request.installationID)
                    launchState = .ready
                    return
                } catch {
                    Self.logger.error("Forced registration also failed: \(error.localizedDescription)")
                    // Fall through to error handling
                }
            }

            lastErrorMessage = error.localizedDescription
            state.connectionStatus = .error
            state.syncStatus = .error

            // Classify error for UI
            if isUnauthorizedError(error) {
                launchState = .authFailure
            } else if isNetworkError(error) {
                launchState = .networkFailure(error.localizedDescription)
            } else {
                launchState = .networkFailure(error.localizedDescription)
            }
        }
    }

    func refreshSession() async {
        await syncCoordinator.sync()
        state.syncStatus = .syncing
        await bootstrap(forceRegistration: false)
    }

    func currentAccessToken() async -> String? {
        await secureStore.retrieve(key: SecureKeys.accessToken)
    }

    func currentRefreshToken() async -> String? {
        await secureStore.retrieve(key: SecureKeys.refreshToken)
    }

    func refreshAccessTokenIfNeeded() async {
        guard let refreshToken = await currentRefreshToken() else { return }

        do {
            let tokens = try await bootstrapService.refreshAuth(refreshToken: refreshToken)
            try await persist(tokens: tokens)
        } catch {
            lastErrorMessage = error.localizedDescription
        }
    }

    func applyPairedSession(state: AppSessionState, tokens: AuthTokens) async {
        lastErrorMessage = nil
        await applySessionState(state, tokens: tokens)
    }

    func revokeCurrentSession() async {
        do {
            try await bootstrapService.revokeCurrentSession(accessToken: await currentAccessToken())
        } catch {
            lastErrorMessage = error.localizedDescription
        }
    }

    func clearSession() async {
        await secureStore.delete(key: SecureKeys.accessToken)
        await secureStore.delete(key: SecureKeys.refreshToken)

        let retainedInstallationID = state.installationID
        let retainedEndpoint = state.backendEndpoint
        lastErrorMessage = nil
        isBootstrapping = false
        state = AppSessionState(
            installationID: retainedInstallationID,
            backendEndpoint: retainedEndpoint
        )
        persistence.clearSessionState()
    }

    private func persist(tokens: AuthTokens) async throws {
        await secureStore.store(key: SecureKeys.accessToken, value: tokens.accessToken)
        await secureStore.store(key: SecureKeys.refreshToken, value: tokens.refreshToken)
    }

    private func makeRegistrationRequest() -> DeviceRegistrationRequest {
        DeviceRegistrationRequest.current(
            installationID: state.installationID,
            environment: environmentProvider()
        )
    }

    private func loadAndApplySessionState(installationID: UUID) async throws {
        let accessToken = await currentAccessToken()
        var loadedState = try await bootstrapService.loadSession(accessToken: accessToken)
        loadedState = mergeInstallationID(into: loadedState, from: installationID)
        loadedState.syncStatus = .synced
        loadedState.lastSyncAt = .now
        loadedState.pushTokenRegistered = notificationService.isPushTokenRegistered
        state = loadedState
    }

    private func applySessionState(_ remoteState: AppSessionState, tokens: AuthTokens) async {
        try? await persist(tokens: tokens)
        state = mergeInstallationID(into: remoteState, from: state.installationID)
    }

    private func attemptRefreshAndReload(installationID: UUID) async -> Bool {
        guard let refreshToken = await currentRefreshToken() else { return false }

        do {
            let tokens = try await bootstrapService.refreshAuth(refreshToken: refreshToken)
            try await persist(tokens: tokens)
            try await loadAndApplySessionState(installationID: installationID)
            return true
        } catch {
            lastErrorMessage = error.localizedDescription
            return false
        }
    }

    private func mergeInstallationID(into state: AppSessionState, from installationID: UUID) -> AppSessionState {
        var mergedState = state
        mergedState.installationID = installationID
        return mergedState
    }

    private func isUnauthorizedError(_ error: Error) -> Bool {
        if let clientError = error as? RelayAPIClient.ClientError,
           case .unauthorized = clientError {
            return true
        }
        let nsError = error as NSError
        return nsError.domain == NSURLErrorDomain && nsError.code == -1013 // 401
    }

    private func isNetworkError(_ error: Error) -> Bool {
        let nsError = error as NSError
        // Timeouts, DNS failures, cannot connect, etc.
        let networkCodes: [Int] = [
            -1001, // timedOut
            -1003, // cannotFindHost
            -1004, // cannotConnectToHost
            -1005, // networkConnectionLost
            -1009, // notConnectedToInternet
            -1014, // badServerResponse
        ]
        return nsError.domain == NSURLErrorDomain && networkCodes.contains(nsError.code)
    }
}
