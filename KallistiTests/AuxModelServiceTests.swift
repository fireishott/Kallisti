import Foundation
import Testing
@testable import Kallisti

@Suite("AuxModelService set()", .serialized)
@MainActor
struct AuxModelServiceTests {

    // Helper: build a feature client whose cli.exec calls go through a
    // mock transport, then auto-respond to every request the service makes
    // (cli.exec -> success, config.get -> empty auxiliary block).
    private struct Harness {
        let transport: MockNativeGatewayTransport
        let client: NativeGatewayClient
        let featureClient: NativeGatewayFeatureClient

        @MainActor
        static func make() async throws -> Harness {
            let transport = MockNativeGatewayTransport()
            let client = NativeGatewayClient(transport: transport, requestTimeout: .seconds(5))
            try await client.connect(url: URL(string: "ws://test")!)
            let featureClient = NativeGatewayFeatureClient(clientProvider: { client })

            // Auto-respond to anything the service sends so cli.exec and the
            // follow-up load()'s config.get both resolve quickly. cli.exec
            // returns a {"blocked":false,"output":""} shape; config.get
            // returns a payload that decodes to an empty auxiliary block.
            transport.onSend = { data in
                guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                      let id = obj["id"] as? Int,
                      let method = obj["method"] as? String
                else { return }
                let resultJSON: String
                switch method {
                case "cli.exec":
                    resultJSON = #"{"jsonrpc":"2.0","id":\#(id),"result":{"blocked":false,"output":""}}"#
                case "config.get":
                    resultJSON = #"{"jsonrpc":"2.0","id":\#(id),"result":{"config":{"auxiliary":{}}}}"#
                default:
                    resultJSON = #"{"jsonrpc":"2.0","id":\#(id),"result":{}}"#
                }
                transport.queueIncoming(Data(resultJSON.utf8))
            }

            return Harness(transport: transport, client: client, featureClient: featureClient)
        }
    }

    @Test("set() routes through cli.exec with normalized argv (Vision)")
    func setRoutesThroughCliExec() async throws {
        let harness = try await Harness.make()
        let aux = AuxModelService(
            apiClient: RelayAPIClient(baseURLProvider: { "http://test.invalid" }),
            accessTokenProvider: { nil },
            nativeFeatureClientProvider: { harness.featureClient }
        )

        await aux.set(
            task: "Vision",
            provider: "openrouter",
            model: "anthropic/claude-3.5-sonnet"
        )

        let frames = harness.transport.sentFrames
        // Two cli.exec calls (provider + model). We don't strictly require
        // a third config.get from load() because the harness auto-responds.
        #expect(frames.count >= 2)

        // Inspect the first cli.exec payload: should set the provider.
        let providerFrame = try #require(frames.first.flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String: Any])
        #expect(providerFrame["method"] as? String == "cli.exec")
        let providerParams = providerFrame["params"] as? [String: Any] ?? [:]
        let providerArgv = providerParams["argv"] as? [String] ?? []
        #expect(providerArgv == ["config", "set", "auxiliary.vision.provider", "openrouter"])

        // Inspect the second cli.exec payload: should set the model.
        let modelFrame = try #require(frames.dropFirst().first.flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String: Any])
        #expect(modelFrame["method"] as? String == "cli.exec")
        let modelParams = modelFrame["params"] as? [String: Any] ?? [:]
        let modelArgv = modelParams["argv"] as? [String] ?? []
        #expect(modelArgv == ["config", "set", "auxiliary.vision.model", "anthropic/claude-3.5-sonnet"])

        // The legacy relay path must NOT be hit when the feature client is
        // available - lastError stays nil since cli.exec and config.get
        // both resolved cleanly.
        #expect(aux.lastError == nil)
    }

    @Test("set() normalizes 'Web extract' to web_extract argv")
    func setNormalizesWebExtract() async throws {
        let harness = try await Harness.make()
        let aux = AuxModelService(
            apiClient: RelayAPIClient(baseURLProvider: { "http://test.invalid" }),
            accessTokenProvider: { nil },
            nativeFeatureClientProvider: { harness.featureClient }
        )

        await aux.set(
            task: "Web extract",
            provider: "openrouter",
            model: "openai/gpt-4o-mini"
        )

        let frames = harness.transport.sentFrames
        let providerFrame = try #require(frames.first.flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String: Any])
        let providerArgv = (providerFrame["params"] as? [String: Any])?["argv"] as? [String] ?? []
        #expect(providerArgv == ["config", "set", "auxiliary.web_extract.provider", "openrouter"])
    }

    @Test("set() normalizes 'Compression' to compression argv")
    func setNormalizesCompression() async throws {
        let harness = try await Harness.make()
        let aux = AuxModelService(
            apiClient: RelayAPIClient(baseURLProvider: { "http://test.invalid" }),
            accessTokenProvider: { nil },
            nativeFeatureClientProvider: { harness.featureClient }
        )

        await aux.set(
            task: "Compression",
            provider: "openrouter",
            model: "openai/gpt-4o-mini"
        )

        let frames = harness.transport.sentFrames
        let providerFrame = try #require(frames.first.flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String: Any])
        let providerArgv = (providerFrame["params"] as? [String: Any])?["argv"] as? [String] ?? []
        #expect(providerArgv == ["config", "set", "auxiliary.compression.provider", "openrouter"])
    }

    @Test("set() maps provider 'auto' to empty argv value")
    func setAutoProviderBecomesEmpty() async throws {
        let harness = try await Harness.make()
        let aux = AuxModelService(
            apiClient: RelayAPIClient(baseURLProvider: { "http://test.invalid" }),
            accessTokenProvider: { nil },
            nativeFeatureClientProvider: { harness.featureClient }
        )

        await aux.set(task: "Vision", provider: "auto", model: "")

        let frames = harness.transport.sentFrames
        let providerFrame = try #require(frames.first.flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String: Any])
        let providerArgv = (providerFrame["params"] as? [String: Any])?["argv"] as? [String] ?? []
        #expect(providerArgv == ["config", "set", "auxiliary.vision.provider", ""])
    }

    @Test("set() with no feature client falls back to legacy relay path")
    func setFallsBackToLegacyWhenNoFeatureClient() async throws {
        // No mock transport, no feature client - service must hit apiClient.
        // We point RelayAPIClient at an unroutable URL and verify the call
        // attempt records an error in lastError. The point is that we did
        // NOT raise an exception and we did NOT touch any feature client.
        let aux = AuxModelService(
            apiClient: RelayAPIClient(baseURLProvider: { "http://127.0.0.1:1" }),
            accessTokenProvider: { "fake-token" },
            nativeFeatureClientProvider: { nil }
        )

        await aux.set(
            task: "Vision",
            provider: "openrouter",
            model: "anthropic/claude-3.5-sonnet"
        )

        // Legacy path was attempted against a dead URL; the network failure
        // is captured into lastError. What we really want to assert is that
        // the native cli.exec path was NOT used - the only way for that to
        // be true is that lastError got populated by the legacy POST. The
        // service never throws out of set().
        #expect(aux.lastError != nil)
    }
}
