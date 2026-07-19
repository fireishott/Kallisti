import Foundation

@MainActor
final class MockHeraldHostService: HeraldHostServiceProtocol {
    var currentHost: HeraldHostStatus? = HeraldHostStatus(
        id: UUID(),
        displayName: "Mock Herald Host",
        hostname: "mock-hermes.local",
        platform: "macos",
        connectorVersion: "0.1.0",
        heraldCommand: "herald",
        heraldVersion: "herald mock",
        heraldModel: "gpt-5.4-mini",
        lastSeenAt: .now,
        lastConnectedAt: .now,
        isOnline: true
    )

    func fetchCurrentHost(accessToken: String?) async throws -> HeraldHostStatus? {
        currentHost
    }

    func createEnrollmentCode(accessToken: String?) async throws -> HostEnrollmentCode {
        HostEnrollmentCode(
            setupCode: "HC1:mock-host-setup-code",
            expiresAt: .now.addingTimeInterval(900),
            relayHost: "relay.example.test"
        )
    }

    func revokeCurrentHost(accessToken: String?) async throws {
        currentHost = nil
    }
}
