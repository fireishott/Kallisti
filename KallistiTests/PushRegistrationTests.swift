import Foundation
import Testing
@testable import Herald

@Suite(.serialized)
struct PushRegistrationTests {

    /// Shared mutable state for stub URL protocol captures.
    private final class CaptureState: @unchecked Sendable {
        var requestCount: Int = 0
        var lastRequestBody: String?
        var capturedBody: String?
    }

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

    @MainActor
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
        let capture = CaptureState()

        StubURLProtocol.requestHandler = { request in
            capture.requestCount += 1
            if let body = request.httpBody {
                capture.lastRequestBody = String(data: body, encoding: .utf8)
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
        #expect(capture.requestCount == 1)

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
        #expect(capture.requestCount == 2, "Coordinator must always send registration, never short-circuit")
    }

    @MainActor
    @Test("Per-environment routing: development token uses development environment")
    func testPerEnvironmentRoutingDevelopment() async throws {
        let capture = CaptureState()

        StubURLProtocol.requestHandler = { request in
            capture.capturedBody = String(data: request.httpBody ?? Data(), encoding: .utf8)
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

        let body = try #require(capture.capturedBody)
        #expect(body.contains("\"pushEnvironment\":\"development\""))
    }

    @MainActor
    @Test("Per-environment routing: production token uses production environment")
    func testPerEnvironmentRoutingProduction() async throws {
        let capture = CaptureState()

        StubURLProtocol.requestHandler = { request in
            capture.capturedBody = String(data: request.httpBody ?? Data(), encoding: .utf8)
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

        let body = try #require(capture.capturedBody)
        #expect(body.contains("\"pushEnvironment\":\"production\""))
    }

    @MainActor
    @Test("Registration succeeds and sends correct deviceId and bundleId")
    func testRegistrationPayload() async throws {
        let capture = CaptureState()
        let deviceID = UUID(uuidString: "11111111-1111-1111-1111-111111111111")!

        StubURLProtocol.requestHandler = { request in
            capture.capturedBody = String(data: request.httpBody ?? Data(), encoding: .utf8)
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

        let body = try #require(capture.capturedBody)
        #expect(body.contains("11111111-1111-1111-1111-111111111111"))
        #expect(body.contains("net.fihonline.herald"))
        #expect(body.contains("deadbeef"))
    }

    @MainActor
    @Test("Coordinator propagates relay error after APNs 410 Gone deactivation")
    func testCoordinatorPropagates410GoneError() async throws {
        // Simulate a relay that rejects registration with 410 Gone
        // (e.g., APNs invalidated the token and relay deactivated the registration)
        StubURLProtocol.requestHandler = { request in
            let response = HTTPURLResponse(
                url: request.url!,
                statusCode: 410,
                httpVersion: nil,
                headerFields: nil
            )!
            let errorJSON = """
            {"error":{"code":"TOKEN_INVALID","message":"APNs token is permanently invalid (410 Gone)"}}
            """.data(using: .utf8)!
            return (response, errorJSON)
        }

        let (coordinator, _) = makeCoordinator()

        do {
            _ = try await coordinator.registerPushToken(
                "expired-token",
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
            Issue.record("Expected registration to throw on 410 Gone response")
        } catch {
            // The coordinator should propagate the error — not silently succeed.
            // This ensures the app knows the registration failed and can retry
            // on next launch (the short-circuit removal guarantees it always tries).
        }
    }
}
