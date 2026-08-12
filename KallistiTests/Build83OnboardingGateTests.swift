import Testing
import Foundation
import UIKit
@testable import Kallisti

/// Build 83 regression tests: fresh-install onboarding gate and Reset purge.
///
/// Covers the exact reported failure: a fresh install inherits a stale
/// Keychain cookie-auth marker from an uninstalled build. The old init gate
/// flipped `hasStoredLogin = true` on that marker alone, so AppRootView
/// mounted chat instead of onboarding and nothing ever connected (no relay
/// URL configured). Reset appeared dead because clearLocalCredentials()
/// never deleted the auth-mode marker that usesCookieAuth() keys off.
@Suite("Build 83 onboarding gate", .serialized)
struct Build83OnboardingGateTests {

    private final class MockSecureStore: SecureStoreProtocol {
        var storage: [String: String] = [:]

        func store(key: String, value: String) async -> Bool {
            storage[key] = value
            return true
        }

        func retrieve(key: String) async -> String? {
            storage[key]
        }

        func delete(key: String) async {
            storage[key] = nil
        }
    }

    @MainActor
    private func makeClient(
        store: MockSecureStore,
        gatewayBaseURL: String
    ) async -> NativeKallistiClient {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [StubTicketURLProtocol.self]
        let auth = NativeAuthCoordinator(
            host: "10.0.0.1",
            port: 9119,
            session: URLSession(configuration: config),
            secureStore: store
        )
        let transport = MockNativeGatewayTransport()
        return NativeKallistiClient(
            gatewayBaseURL: gatewayBaseURL,
            authCoordinator: auth,
            transportFactory: { transport }
        )
    }

    @Test("stale cookie marker with no relay URL does NOT set hasStoredLogin")
    @MainActor
    func staleMarkerWithoutRelayStaysOnboarding() async throws {
        let store = MockSecureStore()
        // Stale marker from an uninstalled build survives in Keychain.
        await store.store(key: "nativeGatewayAuthMode", value: "kallisti-pairing")

        // The init gate runs on a Task; give it main-actor hops.
        let client = await makeClient(store: store, gatewayBaseURL: "http://localhost:9119")
        await Task.yield()
        await Task.yield()

        #expect(!client.hasStoredLogin, "stale marker with no relay must NOT count as a returning user")
        #expect(client.hasResolvedStoredLogin, "init gate must resolve before first render")
    }

    @Test("stale marker without relay gets purged from the store")
    @MainActor
    func staleMarkerIsPurged() async throws {
        let store = MockSecureStore()
        await store.store(key: "nativeGatewayAuthMode", value: "kallisti-pairing")
        await store.store(key: "nativeGatewayAccessToken", value: "stale-token")

        let client = await makeClient(store: store, gatewayBaseURL: "http://localhost:9119")
        await Task.yield()
        await Task.yield()

        let mode = await store.retrieve(key: "nativeGatewayAuthMode")
        #expect(mode == nil, "stale auth mode must be purged so onboarding is clean")
        let token = await store.retrieve(key: "nativeGatewayAccessToken")
        #expect(token == nil, "stale bearer token must be purged")
    }

    @Test("credential marker WITH configured relay keeps hasStoredLogin true")
    @MainActor
    func markerWithRelayIsReturningUser() async throws {
        let store = MockSecureStore()
        await store.store(key: "nativeGatewayAuthMode", value: "kallisti-pairing")

        let client = await makeClient(store: store, gatewayBaseURL: "https://hermes-relay.fihonline.net")
        await Task.yield()
        await Task.yield()

        #expect(client.hasStoredLogin, "credential marker + configured relay = returning user")
    }

    @Test("clearLocalCredentials purges auth mode marker")
    @MainActor
    func clearLocalCredentialsPurgesAuthMode() async throws {
        let store = MockSecureStore()
        await store.store(key: "nativeGatewayAuthMode", value: "basic")
        await store.store(key: "nativeGatewayAccessToken", value: "tok")
        await store.store(key: "nativeGatewayRefreshToken", value: "rt")
        await store.store(key: "nativeGatewayAccessTokenExpiresAt", value: "9999999999")
        await store.store(key: "nativeGatewayBasicUsername", value: "u")
        await store.store(key: "nativeGatewayBasicPassword", value: "p")

        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [StubTicketURLProtocol.self]
        let auth = NativeAuthCoordinator(
            host: "10.0.0.1",
            port: 9119,
            session: URLSession(configuration: config),
            secureStore: store
        )

        await auth.clearLocalCredentials()

        #expect(await store.retrieve(key: "nativeGatewayAuthMode") == nil, "auth mode marker must be purged")
        #expect(await store.retrieve(key: "nativeGatewayAccessToken") == nil)
        #expect(await store.retrieve(key: "nativeGatewayRefreshToken") == nil)
        #expect(await store.retrieve(key: "nativeGatewayBasicUsername") == nil)
        #expect(await store.retrieve(key: "nativeGatewayBasicPassword") == nil)
    }
}
