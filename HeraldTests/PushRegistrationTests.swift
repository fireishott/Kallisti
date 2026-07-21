import Foundation
import Testing
@testable import Herald

@Suite(.serialized)
struct PushRegistrationTests {

    private final class StubURLProtocol: URLProtocol, @unchecked Sendable {
        nonisolated(unsafe) static var requestHandler: (@Sendable (URLRequest) throws -> (HTTPURLResponse, Data))?

        override class func canInit(with request: URLRequest) -> Bool { true }
        override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

        override func startLoading() {
            guard let handler = Self.requestHandler else {
                client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
                return
            }
            do {
                let (response, data) = try handler(request)
                client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
                client?.urlProtocol(self, didLoad: data)
                client?.urlProtocolDidFinishLoading(self)
            } catch {
                client?.urlProtocol(self, didFailWithError: error)
            }
        }

        override func stopLoading() {}
    }

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

    private final class MockAppAttestService: AppAttestServiceProtocol {
        func createProof(challenge: String, signedPayload: Data) async throws -> AppAttestProof {
            AppAttestProof(keyId: "test-key", attestationObject: "test", assertion: "test")
        }
    }

    private func makeURLSession() -> URLSession {
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [StubURLProtocol.self]
        return URLSession(configuration: config)
    }

    private func makeCoordinator(
        session: URLSession? = nil,
        usesManagedPushBroker: Bool = false
    ) -> (PushRegistrationCoordinator, RelayAPIClient) {
        let urlSession = session ?? makeURLSession()
        let relayClient = RelayAPIClient(
            baseURLProvider: { "https://relay.example.com/v1" },
            session: urlSession
        )
        let store = PushBrokerRegistrationStore(secureStore: MockSecureStore())
        let buildConfig = AppBuildConfiguration(
            infoDictionary: [
                "APP_PUSH_TRANSPORT": "direct",
                "APP_HOSTED_RELAY_ENABLED": false,
            ]
        )
        let coordinator = PushRegistrationCoordinator(
            relayAPIClient: relayClient,
            brokerClient: nil,
            registrationStore: store,
            appAttestService: MockAppAttestService(),
            buildConfiguration: buildConfig
        )
        return (coordinator, relayClient)
    }

    @MainActor
    @Test("Registration always sends request to relay, even if token hasn't changed")
    func testRegistrationAlwaysSendsToRelay() async throws {
        var requestCount = 0
        var lastRequestBody: String?

        StubURLProtocol.requestHandler = { request in
            requestCount += 1
            if let body = request.httpBody {
                lastRequestBody = String(data: body, encoding: .utf8)
            }
            let response = HTTPURLResponse(
                url: request.url!,
                statusCode: 200,
                httpVersion: nil,
                headerFields: nil
            )!
            let data = try! JSONEncoder().encode(["registered": true])
            return (response, data)
        }

        let (coordinator, _) = makeCoordinator()

        // First registration
        let result1 = try await coordinator.registerPushToken(
            "abc123",
            relayConfiguration: RelayConfiguration(
                connectionMode: .selfHostedRelay,
                customRelayBaseURL: "https://relay.example.com/v1"
            ),
            accessToken: "test-token",
            deviceID: UUID(),
            installationID: UUID(),
            bundleID: "net.fihonline.herald",
            appVersion: "1.0.0",
            pushEnvironment: "development"
        )
        #expect(result1 == true)
        #expect(requestCount == 1)

        // Second registration with same token — must still send request
        let result2 = try await coordinator.registerPushToken(
            "abc123",
            relayConfiguration: RelayConfiguration(
                connectionMode: .selfHostedRelay,
                customRelayBaseURL: "https://relay.example.com/v1"
            ),
            accessToken: "test-token",
            deviceID: UUID(),
            installationID: UUID(),
            bundleID: "net.fihonline.herald",
            appVersion: "1.0.0",
            pushEnvironment: "development"
        )
        #expect(result2 == true)
        #expect(requestCount == 2, "Coordinator must always send registration, never short-circuit")
    }

    @MainActor
    @Test("Per-environment routing: development token uses development environment")
    func testPerEnvironmentRoutingDevelopment() async throws {
        var capturedBody: String?

        StubURLProtocol.requestHandler = { request in
            capturedBody = String(data: request.httpBody ?? Data(), encoding: .utf8)
            let response = HTTPURLResponse(
                url: request.url!,
                statusCode: 200,
                httpVersion: nil,
                headerFields: nil
            )!
            let data = try! JSONEncoder().encode(["registered": true])
            return (response, data)
        }

        let (coordinator, _) = makeCoordinator()

        _ = try await coordinator.registerPushToken(
            "abc123",
            relayConfiguration: RelayConfiguration(
                connectionMode: .selfHostedRelay,
                customRelayBaseURL: "https://relay.example.com/v1"
            ),
            accessToken: "test-token",
            deviceID: UUID(),
            installationID: UUID(),
            bundleID: "net.fihonline.herald",
            appVersion: "1.0.0",
            pushEnvironment: "development"
        )

        let body = try #require(capturedBody)
        #expect(body.contains("\"pushEnvironment\":\"development\""))
    }

    @MainActor
    @Test("Per-environment routing: production token uses production environment")
    func testPerEnvironmentRoutingProduction() async throws {
        var capturedBody: String?

        StubURLProtocol.requestHandler = { request in
            capturedBody = String(data: request.httpBody ?? Data(), encoding: .utf8)
            let response = HTTPURLResponse(
                url: request.url!,
                statusCode: 200,
                httpVersion: nil,
                headerFields: nil
            )!
            let data = try! JSONEncoder().encode(["registered": true])
            return (response, data)
        }

        let (coordinator, _) = makeCoordinator()

        _ = try await coordinator.registerPushToken(
            "abc123",
            relayConfiguration: RelayConfiguration(
                connectionMode: .selfHostedRelay,
                customRelayBaseURL: "https://relay.example.com/v1"
            ),
            accessToken: "test-token",
            deviceID: UUID(),
            installationID: UUID(),
            bundleID: "net.fihonline.herald",
            appVersion: "1.0.0",
            pushEnvironment: "production"
        )

        let body = try #require(capturedBody)
        #expect(body.contains("\"pushEnvironment\":\"production\""))
    }

    @MainActor
    @Test("Registration succeeds and sends correct deviceId and bundleId")
    func testRegistrationPayload() async throws {
        var capturedBody: String?
        let deviceID = UUID(uuidString: "11111111-1111-1111-1111-111111111111")!

        StubURLProtocol.requestHandler = { request in
            capturedBody = String(data: request.httpBody ?? Data(), encoding: .utf8)
            let response = HTTPURLResponse(
                url: request.url!,
                statusCode: 200,
                httpVersion: nil,
                headerFields: nil
            )!
            let data = try! JSONEncoder().encode(["registered": true])
            return (response, data)
        }

        let (coordinator, _) = makeCoordinator()

        _ = try await coordinator.registerPushToken(
            "deadbeef",
            relayConfiguration: RelayConfiguration(
                connectionMode: .selfHostedRelay,
                customRelayBaseURL: "https://relay.example.com/v1"
            ),
            accessToken: "test-token",
            deviceID: deviceID,
            installationID: UUID(),
            bundleID: "net.fihonline.herald",
            appVersion: "2.0.0",
            pushEnvironment: "development"
        )

        let body = try #require(capturedBody)
        #expect(body.contains("11111111-1111-1111-1111-111111111111"))
        #expect(body.contains("net.fihonline.herald"))
        #expect(body.contains("deadbeef"))
    }
}
