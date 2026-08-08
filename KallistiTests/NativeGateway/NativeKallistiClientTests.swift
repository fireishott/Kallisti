import Testing
import Foundation
@testable import Kallisti

/// Covers the 0.2.0 login-loop bug: `URLSessionWebSocketTask.resume()`
/// never throws, so `connect()` was declaring `.connected` before proving
/// the socket actually opened, and a transient failure afterward reset
/// `AppRootView` all the way back to onboarding (and a redundant Nous
/// OAuth login) even though the device was still validly logged in.
@Suite("NativeKallistiClient connect() status")
struct NativeKallistiClientTests {

    /// mintTicket() does a real HTTP POST -- stub it out so tests that have
    /// a token stored don't reach the network, mirroring the pattern in
    /// NativeAuthCoordinatorTests.
    @MainActor
    private func stubbedAuthCoordinator(secureStore: MockSecureStore) -> NativeAuthCoordinator {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [StubTicketURLProtocol.self]
        return NativeAuthCoordinator(
            host: "10.0.0.1",
            port: 9119,
            session: URLSession(configuration: config),
            secureStore: secureStore
        )
    }

    // MARK: - Verification-before-connected (the false-positive itself)

    @Test("connect() does not report .connected when the post-handshake verification fails")
    @MainActor
    func connectDoesNotReportConnectedOnDeadSocket() async throws {
        let store = MockSecureStore()
        await store.store(key: "nativeGatewayAccessToken", value: "existing-token")
        let auth = stubbedAuthCoordinator(secureStore: store)

        let transport = MockNativeGatewayTransport()
        // connect() itself never throws (mirrors task.resume()), but the
        // socket is actually dead -- every send fails, exactly like a
        // ws-ticket the server silently rejected.
        transport.sendError = URLError(.networkConnectionLost)

        let sut = NativeKallistiClient(
            gatewayBaseURL: "http://10.0.0.1:9119",
            authCoordinator: auth,
            transportFactory: { transport }
        )

        await sut.connect()

        #expect(sut.connectionStatus != .connected)

        await sut.disconnect()
    }

    @Test("connect() reports .connected once the verification round-trip succeeds")
    @MainActor
    func connectReportsConnectedAfterVerification() async throws {
        let store = MockSecureStore()
        await store.store(key: "nativeGatewayAccessToken", value: "existing-token")
        let auth = stubbedAuthCoordinator(secureStore: store)

        let transport = MockNativeGatewayTransport()
        // First (and only) request the fresh client sends gets id 1 --
        // answer it so the verification round-trip succeeds.
        transport.queueIncoming(Data(#"{"jsonrpc":"2.0","id":1,"result":{}}"#.utf8))

        let sut = NativeKallistiClient(
            gatewayBaseURL: "http://10.0.0.1:9119",
            authCoordinator: auth,
            transportFactory: { transport }
        )

        await sut.connect()

        #expect(sut.connectionStatus == .connected)

        await sut.disconnect()
    }

    // MARK: - hasStoredLogin: the signal AppRootView should route on

    @Test("hasStoredLogin becomes true once connect() succeeds")
    @MainActor
    func hasStoredLoginTrueAfterSuccessfulConnect() async throws {
        let store = MockSecureStore()
        await store.store(key: "nativeGatewayAccessToken", value: "existing-token")
        let auth = stubbedAuthCoordinator(secureStore: store)

        let transport = MockNativeGatewayTransport()
        transport.queueIncoming(Data(#"{"jsonrpc":"2.0","id":1,"result":{}}"#.utf8))

        let sut = NativeKallistiClient(
            gatewayBaseURL: "http://10.0.0.1:9119",
            authCoordinator: auth,
            transportFactory: { transport }
        )

        await sut.connect()

        #expect(sut.hasStoredLogin == true)

        await sut.disconnect()
    }

    @Test("an unproven stored token that fails verification does NOT skip onboarding")
    @MainActor
    func unprovenTokenDoesNotSkipOnboarding() async throws {
        let store = MockSecureStore()
        // Simulates a stale/orphaned token left in Keychain from a previous
        // install or a previous broken build -- present, but this device
        // has never actually PROVEN it can reach the gateway with it.
        // Regression: this used to set hasStoredLogin = true just because
        // mintTicket() got past the "is there a token" check, which sent a
        // fresh install straight past onboarding into a permanently broken
        // main app with no way back -- reported live 2026-08-08.
        await store.store(key: "nativeGatewayAccessToken", value: "stale-token")
        let auth = stubbedAuthCoordinator(secureStore: store)

        let transport = MockNativeGatewayTransport()
        transport.sendError = URLError(.networkConnectionLost)

        let sut = NativeKallistiClient(
            gatewayBaseURL: "http://10.0.0.1:9119",
            authCoordinator: auth,
            transportFactory: { transport }
        )

        await sut.connect()

        #expect(sut.hasStoredLogin == false)
        #expect(sut.connectionStatus == .disconnected)

        await sut.disconnect()
    }

    @Test("a failure AFTER a previously-proven connection keeps hasStoredLogin true and stays .reconnecting")
    @MainActor
    func provenConnectionSurvivesALaterFailure() async throws {
        let store = MockSecureStore()
        await store.store(key: "nativeGatewayAccessToken", value: "existing-token")
        let auth = stubbedAuthCoordinator(secureStore: store)

        let transport = MockNativeGatewayTransport()
        transport.queueIncoming(Data(#"{"jsonrpc":"2.0","id":1,"result":{}}"#.utf8))

        let sut = NativeKallistiClient(
            gatewayBaseURL: "http://10.0.0.1:9119",
            authCoordinator: auth,
            transportFactory: { transport }
        )

        // First connect proves the token actually works.
        await sut.connect()
        #expect(sut.hasStoredLogin == true)

        // A later reconnect attempt (e.g. an idle-reaped socket) fails --
        // this must NOT be treated the same as an unproven token.
        transport.sendError = URLError(.networkConnectionLost)
        await sut.connect()

        #expect(sut.connectionStatus == .reconnecting)
        #expect(sut.hasStoredLogin == true)

        await sut.disconnect()
    }

    @Test("connect() with no stored token at all leaves hasStoredLogin false and status .disconnected")
    @MainActor
    func noStoredTokenStaysDisconnected() async throws {
        let store = MockSecureStore()
        // No token stored -- never logged in.
        let auth = stubbedAuthCoordinator(secureStore: store)

        let transport = MockNativeGatewayTransport()

        let sut = NativeKallistiClient(
            gatewayBaseURL: "http://10.0.0.1:9119",
            authCoordinator: auth,
            transportFactory: { transport }
        )

        await sut.connect()

        #expect(sut.hasStoredLogin == false)
        #expect(sut.connectionStatus == .disconnected)

        await sut.disconnect()
    }
}

/// Answers every `/api/auth/ws-ticket` POST with a fixed ticket so
/// `mintTicket()` succeeds without touching the network. Mirrors
/// `MockTicketURLProtocol` in NativeAuthCoordinatorTests.swift.
private final class StubTicketURLProtocol: URLProtocol {
    override class func canInit(with request: URLRequest) -> Bool {
        request.url?.path.hasSuffix("/api/auth/ws-ticket") == true
    }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let response = HTTPURLResponse(
            url: request.url!,
            statusCode: 200,
            httpVersion: nil,
            headerFields: nil
        )!
        let body = Data(#"{"ticket":"test-ticket"}"#.utf8)
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}
