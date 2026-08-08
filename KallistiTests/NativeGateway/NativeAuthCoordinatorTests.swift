import Testing
import Foundation
import CryptoKit
@testable import Kallisti

@Suite("NativeAuthCoordinator")
struct NativeAuthCoordinatorTests {

    // MARK: - Test 1: PKCE challenge is base64url(SHA256(verifier)) with no padding

    @Test("PKCE challenge is base64url SHA256 of verifier, no padding")
    func pkceChallenge() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"
        let challenge = NativeAuthCoordinator.s256Challenge(for: verifier)

        let expectedDigest = SHA256.hash(data: Data(verifier.utf8))
        let expected = Data(expectedDigest).base64URLEncodedString()

        #expect(challenge == expected)
        #expect(!challenge.contains("+"), "must not contain +")
        #expect(!challenge.contains("/"), "must not contain /")
        #expect(!challenge.contains("="), "must not contain = padding")
    }

    @Test("PKCE challenge is deterministic for same input")
    func pkceChallengeDeterministic() {
        let v = "test-verifier-string-for-determinism"
        let c1 = NativeAuthCoordinator.s256Challenge(for: v)
        let c2 = NativeAuthCoordinator.s256Challenge(for: v)
        #expect(c1 == c2)
    }

    // MARK: - Test 2: exchangeCode posts code+verifier and stores returned access token

    @Test("exchangeCode posts code+verifier JSON and stores tokens")
    @MainActor
    func exchangeCodeStoresTokens() async throws {
        let store = MockSecureStoreForAuth()
        let mockURLProtocol = MockTokenExchangeURLProtocol.self
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [mockURLProtocol]
        let session = URLSession(configuration: config)

        mockURLProtocol.stub(
            statusCode: 200,
            body: """
            {"access_token":"at_abc123","refresh_token":"rt_xyz789","expires_at":9999999999}
            """.data(using: .utf8)!
        )

        let coordinator = NativeAuthCoordinator(
            host: "192.168.10.118",
            port: 9119,
            session: session,
            secureStore: store
        )

        try await coordinator.exchangeCode("auth_code_42", verifier: "my_verifier")

        #expect(store.stored["nativeGatewayAccessToken"] == "at_abc123")
        #expect(store.stored["nativeGatewayRefreshToken"] == "rt_xyz789")

        // Verify the sent body contains code + code_verifier
        if let bodyData = mockURLProtocol.lastRequestBody {
            let sentBody = try JSONDecoder().decode([String: String].self, from: bodyData)
            #expect(sentBody["code"] == "auth_code_42")
            #expect(sentBody["code_verifier"] == "my_verifier")
        }
    }

    @Test("exchangeCode throws on non-200 response")
    @MainActor
    func exchangeCodeThrowsOnFailure() async throws {
        let store = MockSecureStoreForAuth()
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [MockTokenExchangeURLProtocol.self]
        let session = URLSession(configuration: config)

        MockTokenExchangeURLProtocol.stub(statusCode: 400, body: Data())

        let coordinator = NativeAuthCoordinator(
            host: "10.0.0.1",
            port: 8080,
            session: session,
            secureStore: store
        )

        await #expect(throws: NativeAuthError.self) {
            _ = try await coordinator.exchangeCode("bad", verifier: "bad")
        }
    }

    // MARK: - Test 3: mintTicket sends Authorization: Bearer, not a cookie

    @Test("mintTicket sends Bearer authorization header, not cookie")
    @MainActor
    func mintTicketUsesBearerAuth() async throws {
        let store = MockSecureStoreForAuth()
        store.stored["nativeGatewayAccessToken"] = "my_access_token"

        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [MockTicketURLProtocol.self]
        let session = URLSession(configuration: config)

        MockTicketURLProtocol.stub(
            statusCode: 200,
            body: """
            {"ticket":"fresh-ticket-abc"}
            """.data(using: .utf8)!
        )

        let coordinator = NativeAuthCoordinator(
            host: "10.0.0.1",
            port: 8080,
            session: session,
            secureStore: store
        )

        let ticket = try await coordinator.mintTicket()
        #expect(ticket == "fresh-ticket-abc")

        let authHeader = MockTicketURLProtocol.lastRequest?.value(forHTTPHeaderField: "Authorization")
        #expect(authHeader == "Bearer my_access_token")

        let cookieHeader = MockTicketURLProtocol.lastRequest?.value(forHTTPHeaderField: "Cookie")
        #expect(cookieHeader == nil)
    }

    @Test("mintTicket throws notLoggedIn when no access token stored")
    @MainActor
    func mintTicketThrowsWhenNotLoggedIn() async throws {
        let store = MockSecureStoreForAuth()
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [MockTicketURLProtocol.self]
        let session = URLSession(configuration: config)

        let coordinator = NativeAuthCoordinator(
            host: "10.0.0.1",
            port: 8080,
            session: session,
            secureStore: store
        )

        await #expect(throws: NativeAuthError.self) {
            _ = try await coordinator.mintTicket()
        }
    }
}

// MARK: - Mock URLProtocols

/// Intercepts /auth/native/token requests for exchangeCode tests.
private class MockTokenExchangeURLProtocol: URLProtocol {
    nonisolated(unsafe) static var statusCode: Int = 200
    nonisolated(unsafe) static var stubBody: Data = Data()
    nonisolated(unsafe) static var lastRequestBody: Data?

    static func stub(statusCode: Int, body: Data) {
        self.statusCode = statusCode
        self.stubBody = body
        self.lastRequestBody = nil
    }

    override class func canInit(with request: URLRequest) -> Bool {
        request.url?.path.hasSuffix("/auth/native/token") == true
    }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        Self.lastRequestBody = self.request.httpBody
        let response = HTTPURLResponse(
            url: request.url!,
            statusCode: Self.statusCode,
            httpVersion: nil,
            headerFields: nil
        )!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Self.stubBody)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

/// Intercepts /api/auth/ws-ticket requests for mintTicket tests.
private class MockTicketURLProtocol: URLProtocol {
    nonisolated(unsafe) static var statusCode: Int = 200
    nonisolated(unsafe) static var stubBody: Data = Data()
    nonisolated(unsafe) static var lastRequest: URLRequest?

    static func stub(statusCode: Int, body: Data) {
        self.statusCode = statusCode
        self.stubBody = body
        self.lastRequest = nil
    }

    override class func canInit(with request: URLRequest) -> Bool {
        request.url?.path.hasSuffix("/api/auth/ws-ticket") == true
    }

    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        Self.lastRequest = request
        let response = HTTPURLResponse(
            url: request.url!,
            statusCode: Self.statusCode,
            httpVersion: nil,
            headerFields: nil
        )!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Self.stubBody)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

// MARK: - Mock Secure Store (test-only, no @Observable)

@MainActor
private final class MockSecureStoreForAuth: SecureStoreProtocol {
    var stored: [String: String] = [:]

    @discardableResult
    func store(key: String, value: String) async -> Bool {
        stored[key] = value
        return true
    }

    func retrieve(key: String) async -> String? {
        stored[key]
    }

    func delete(key: String) async {
        stored.removeValue(forKey: key)
    }
}
