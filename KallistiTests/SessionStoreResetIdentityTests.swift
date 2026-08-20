import Foundation
import Testing

// Added in build 131.13: regression test for the reset/pairing loop.
// The installationID must survive clearSession (Reset Connection). If it
// rotates on every wipe, the connector rejects the pairing code (bound to
// the original installation) and the app loops on "Pairing failed".

@Suite("SessionStore Reset Identity")
struct SessionStoreResetIdentityTests {
    @Test @MainActor
    func clearSessionPreservesInstallationID() async throws {
        let suiteName = "session-clear-preserves-id-\\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suiteName)!
        defaults.removePersistentDomain(forName: suiteName)

        let persistence = UserDefaultsAppPersistenceStore(defaults: defaults)
        let secureStore = MockSecureStore()
        let sessionStore = AppSessionStore(
            bootstrapService: MockSessionBootstrapService(),
            syncCoordinator: MockSyncCoordinator(),
            secureStore: secureStore,
            persistence: persistence,
            notificationService: MockNotificationService(),
            environmentProvider: { .development }
        )

        let originalID = sessionStore.state.installationID

        // Simulate a real reset: clear credentials + pairing state.
        await sessionStore.clearSession()

        #expect(sessionStore.state.installationID == originalID)

        // The identity must ALSO survive a cold relaunch - clearSession
        // persists the cleared state, so loadSessionState returns it.
        let reloaded = persistence.loadSessionState()
        #expect(reloaded?.installationID == originalID)
    }
}
