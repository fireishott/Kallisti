import Testing
import Foundation
@testable import Kallisti

/// The credential gate is the half of inline images that no parser test can see:
/// a correctly rewritten `![alt](https://…/phone/v1/media?p=…)` still renders as a
/// failure placeholder unless the fetch carries the DSH bearer, and the gate has
/// to keep refusing credentials to external image hosts.
@Suite("Media auth gate")
@MainActor
struct AttachmentServiceAuthTests {
    private static let dshMediaURL =
        "https://fih-ai-host.tail4d497d.ts.net/dsh/phone/v1/media?p=%2Fhome%2Ffihadmin%2Fjj_headshot_square.jpg"

    @Test("a DSH media URL carries the DSH bearer")
    func dshMediaCarriesDSHBearer() async throws {
        let service = AttachmentService(apiClient: nil, accessTokenProvider: { "relay-token" })
        service.dshMediaToken = "dsh-token"
        let url = try #require(URL(string: Self.dshMediaURL))

        let request = await service.authorizedRequest(for: url)

        #expect(request.value(forHTTPHeaderField: "Authorization") == "Bearer dsh-token")
    }

    @Test("a DSH media URL never falls back to the relay token")
    func dshMediaDoesNotUseRelayToken() async throws {
        let service = AttachmentService(apiClient: nil, accessTokenProvider: { "relay-token" })
        let url = try #require(URL(string: Self.dshMediaURL))

        let request = await service.authorizedRequest(for: url)

        #expect(request.value(forHTTPHeaderField: "Authorization") == nil)
    }

    @Test("the native media route still uses the gateway or relay token")
    func nativeMediaKeepsGatewayCredential() async throws {
        let service = AttachmentService(apiClient: nil, accessTokenProvider: { "relay-token" })
        let url = try #require(URL(string: "https://relay.example.com/v1/native/media?path=images/a.png"))

        let request = await service.authorizedRequest(for: url)

        #expect(request.value(forHTTPHeaderField: "Authorization") == "Bearer relay-token")
    }

    @Test("an external image host receives no credential")
    func externalHostGetsNoCredential() async throws {
        let service = AttachmentService(apiClient: nil, accessTokenProvider: { "relay-token" })
        service.dshMediaToken = "dsh-token"
        let url = try #require(URL(string: "https://example.com/pic.jpg"))

        let request = await service.authorizedRequest(for: url)

        #expect(request.value(forHTTPHeaderField: "Authorization") == nil)
    }
}
