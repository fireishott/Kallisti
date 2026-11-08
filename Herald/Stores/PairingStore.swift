import Foundation
import os

@MainActor
@Observable
final class PairingStore {
    private static let onboardingKey = "kallisti.needsPermissionsOnboarding"
    private static let logger = Logger(subsystem: "net.fihonline.herald", category: "pairing")

    var pairedRelayConfiguration: PairedRelayConfiguration?
    var isWorking = false
    var lastErrorMessage: String?
    var needsPermissionsOnboarding = false
    var onPairingChanged: (@MainActor (Bool) async -> Void)?

    private let pairingService: any PairingServiceProtocol
    private let sessionStore: AppSessionStore
    private let persistence: any AppPersistenceStoreProtocol
    private let environmentProvider: @MainActor () -> AppEnvironment
    private let relayBaseURLProvider: @MainActor () -> String?

    init(
        pairingService: any PairingServiceProtocol,
        sessionStore: AppSessionStore,
        persistence: any AppPersistenceStoreProtocol,
        environmentProvider: @escaping @MainActor () -> AppEnvironment,
        relayBaseURLProvider: @escaping @MainActor () -> String?
    ) {
        self.pairingService = pairingService
        self.sessionStore = sessionStore
        self.persistence = persistence
        self.environmentProvider = environmentProvider
        self.relayBaseURLProvider = relayBaseURLProvider
        self.pairedRelayConfiguration = persistence.loadPairedRelayConfiguration()
        self.needsPermissionsOnboarding = UserDefaults.standard.bool(forKey: Self.onboardingKey)
    }

    var isPaired: Bool {
        pairedRelayConfiguration != nil
    }

    func normalizePairingCode(_ rawCode: String) throws -> String {
        try pairingService.normalizePairingCode(rawCode)
    }

    @discardableResult
    func pair(using rawSetupCode: String) async -> Bool {
        isWorking = true
        lastErrorMessage = nil
        defer { isWorking = false }

        do {
            let normalizedCode = try pairingService.normalizePairingCode(rawSetupCode)
            guard let relayBaseURLString = RelayConfiguration.normalizeBaseURL(relayBaseURLProvider()) else {
                lastErrorMessage = "Enter a valid relay URL ending with /v1 before pairing."
                return false
            }
            let request = DeviceRegistrationRequest.current(
                installationID: sessionStore.state.installationID,
                environment: environmentProvider(),
                relayBaseURLString: relayBaseURLString
            )
            let result = try await pairingService.redeemPairingCode(
                normalizedCode,
                request: request
            )

            persistence.savePairedRelayConfiguration(result.configuration)
            pairedRelayConfiguration = result.configuration
            lastErrorMessage = nil
            setNeedsPermissionsOnboarding(true)
            await sessionStore.applyPairedSession(state: result.state, tokens: result.tokens)
            await onPairingChanged?(true)
            return true
        } catch {
            lastErrorMessage = error.localizedDescription
            return false
        }
    }

    /// Unpair this device: clear everything here first, then tell the daemon.
    ///
    /// The order is the fix for a button that looked dead. It used to POST the
    /// revoke first and wait for the answer, so a daemon that was slow, asleep or
    /// holding a token this device could no longer use left the screen unchanged
    /// with nothing said. Clearing local state first makes the tap land at once;
    /// the revoke is cleanup on the host, and a refusal is reported instead of
    /// swallowed by a catch nobody rendered.
    func disconnect() async {
        isWorking = true
        lastErrorMessage = nil
        defer { isWorking = false }

        // Read the bearer before the clear, because the clear deletes it.
        let accessToken = await sessionStore.currentAccessToken()
        await clearLocalPairing(notify: true)

        do {
            try await sessionStore.revokeSession(accessToken: accessToken)
        } catch {
            // The device is already unpaired on this one. Say what did not happen
            // on the host rather than implying the tap failed.
            lastErrorMessage = "Disconnected on this device. The host could not be told, so its device list may still carry this one: \(error.localizedDescription)"
            Self.logger.error("unpair revoke failed: \(error.localizedDescription)")
        }
    }

    func completePermissionsOnboarding() {
        setNeedsPermissionsOnboarding(false)
    }

    func clearLocalPairing(notify: Bool = true) async {
        persistence.clearPairedRelayConfiguration()
        pairedRelayConfiguration = nil
        lastErrorMessage = nil
        setNeedsPermissionsOnboarding(false)
        await sessionStore.clearSession()
        // The Eris transport keeps the other half of the pair: the daemon address
        // in UserDefaults and the metadata entry beside the two Keychain accounts.
        // `clearSession` deletes those two accounts and nothing else, which left an
        // address and a metadata blob behind - a pairing the app could not sign in
        // to and would not forget. Every unpair path ends here, so both are cleared
        // here rather than in one caller.
        await KeychainErisTokenStore().clear()
        ErisDaemonStore.clearRememberedAddress()
        if notify {
            await onPairingChanged?(false)
        }
    }

    private func setNeedsPermissionsOnboarding(_ value: Bool) {
        needsPermissionsOnboarding = value
        UserDefaults.standard.set(value, forKey: Self.onboardingKey)
    }
}
