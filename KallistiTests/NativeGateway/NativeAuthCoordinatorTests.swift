import Testing
import Foundation
@testable import Kallisti

@Suite("NativeAuthCoordinator")
struct NativeAuthCoordinatorTests {

    @Test("Invalid base URL throws invalidBaseURL")
    func invalidBaseURL() async {
        await #expect(throws: NativeAuthCoordinator.AuthError.self) {
            _ = try await NativeAuthCoordinator.authenticatedWSURL(
                baseURL: "://bad url",
                credentials: .init(username: "u", password: "p", provider: "basic"),
                session: .shared
            )
        }
    }

    @Test("Credentials struct holds values")
    func credentialsStruct() {
        let creds = NativeAuthCoordinator.Credentials(
            username: "testuser",
            password: "testpass",
            provider: "basic"
        )
        #expect(creds.username == "testuser")
        #expect(creds.password == "testpass")
        #expect(creds.provider == "basic")
    }

    @Test("Authenticated URL includes ticket as query parameter")
    func authenticatedURLIncludesTicket() async throws {
        let handler = MockAuthHandler()
        handler.responses = [
            .login: (HTTPURLResponse(url: URL(string: "http://localhost/auth/password-login")!, statusCode: 200, httpVersion: nil, headerFields: ["Set-Cookie": "session=abc123; Path=/"])!, Data()),
            .ticket: (HTTPURLResponse(url: URL(string: "http://localhost/api/auth/ws-ticket")!, statusCode: 200, httpVersion: nil, headerFields: nil)!, try! JSONSerialization.data(withJSONObject: ["ticket": "test-ticket-xyz"])),
        ]

        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [MockAuthURLProtocol.self]
        let session = URLSession(configuration: config)
        MockAuthURLProtocol.handler = handler

        let url = try await NativeAuthCoordinator.authenticatedWSURL(
            baseURL: "http://localhost:9119",
            credentials: .init(username: "user", password: "pass", provider: "basic"),
            session: session
        )

        #expect(url.scheme == "ws")
        #expect(url.host() == "localhost")
        #expect(url.path.hasSuffix("/api/ws"))
        #expect(url.query?.contains("ticket=test-ticket-xyz") == true)
    }

    @Test("HTTPS base URL produces wss:// scheme")
    func httpsProducesWSS() async throws {
        let handler = MockAuthHandler()
        handler.responses = [
            .login: (HTTPURLResponse(url: URL(string: "https://relay.example.com/auth/password-login")!, statusCode: 200, httpVersion: nil, headerFields: ["Set-Cookie": "s=v"])!, Data()),
            .ticket: (HTTPURLResponse(url: URL(string: "https://relay.example.com/api/auth/ws-ticket")!, statusCode: 200, httpVersion: nil, headerFields: nil)!, try! JSONSerialization.data(withJSONObject: ["ticket": "t"])),
        ]

        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [MockAuthURLProtocol.self]
        let session = URLSession(configuration: config)
        MockAuthURLProtocol.handler = handler

        let url = try await NativeAuthCoordinator.authenticatedWSURL(
            baseURL: "https://relay.example.com",
            credentials: .init(username: "u", password: "p", provider: "basic"),
            session: session
        )

        #expect(url.scheme == "wss")
    }

    @Test("Login failure throws loginFailed error")
    func loginFailure() async throws {
        let handler = MockAuthHandler()
        handler.responses = [
            .login: (HTTPURLResponse(url: URL(string: "http://localhost/auth/password-login")!, statusCode: 401, httpVersion: nil, headerFields: nil)!, Data("Unauthorized".utf8)),
        ]

        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [MockAuthURLProtocol.self]
        let session = URLSession(configuration: config)
        MockAuthURLProtocol.handler = handler

        await #expect(throws: NativeAuthCoordinator.AuthError.self) {
            _ = try await NativeAuthCoordinator.authenticatedWSURL(
                baseURL: "http://localhost:9119",
                credentials: .init(username: "u", password: "p", provider: "basic"),
                session: session
            )
        }
    }
}

// MARK: - Mock URLProtocol

private class MockAuthHandler: @unchecked Sendable {
    enum Endpoint { case login, ticket }
    var responses: [Endpoint: (HTTPURLResponse, Data)] = [:]
}

private class MockAuthURLProtocol: URLProtocol {
    nonisolated(unsafe) static var handler: MockAuthHandler?

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let handler = MockAuthURLProtocol.handler else { return }
        let endpoint: MockAuthHandler.Endpoint = request.url!.path.hasSuffix("/auth/password-login") ? .login : .ticket
        guard let (response, data) = handler.responses[endpoint] else { return }
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: data)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}
