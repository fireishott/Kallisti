import Foundation

@MainActor
final class LiveKallistiHostService: KallistiHostServiceProtocol {
    private struct EmptyBody: Encodable {}

    private struct EnrollmentResponse: Decodable {
        let setupCode: String
        let expiresAt: Date?
        let relayHost: String
    }

    private struct CurrentHostResponse: Decodable {
        let host: RelayHost?
    }

    private struct RelayHost: Decodable {
        let id: UUID
        let displayName: String?
        let hostname: String?
        let platform: String?
        let connectorVersion: String?
        let heraldCommand: String?
        let heraldVersion: String?
        let heraldModel: String?
        let lastSeenAt: Date?
        let lastConnectedAt: Date?
        let isOnline: Bool

        enum CodingKeys: String, CodingKey {
            case id, displayName, hostname, platform
            case connectorVersion, heraldCommand, heraldVersion, heraldModel
            case lastSeenAt, lastConnectedAt, isOnline
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            // id is required — a host without an id is meaningless
            id = try container.decode(UUID.self, forKey: .id)
            // All other fields are resilient to being absent or malformed
            displayName = try container.decodeIfPresent(String.self, forKey: .displayName)
            hostname = try container.decodeIfPresent(String.self, forKey: .hostname)
            platform = try container.decodeIfPresent(String.self, forKey: .platform)
            connectorVersion = try container.decodeIfPresent(String.self, forKey: .connectorVersion)
            heraldCommand = try container.decodeIfPresent(String.self, forKey: .heraldCommand)
            heraldVersion = try container.decodeIfPresent(String.self, forKey: .heraldVersion)
            heraldModel = try container.decodeIfPresent(String.self, forKey: .heraldModel)
            lastSeenAt = try container.decodeIfPresent(Date.self, forKey: .lastSeenAt)
            lastConnectedAt = try container.decodeIfPresent(Date.self, forKey: .lastConnectedAt)
            // Default isOnline to false if missing — a missing key shouldn't
            // break the entire host decode (the field may have been added in a
            // newer relay version that the client predates).
            isOnline = try container.decodeIfPresent(Bool.self, forKey: .isOnline) ?? false
        }
    }

    private let apiClient: RelayAPIClient
    private let accessTokenRefresher: @MainActor () async -> String?

    init(
        apiClient: RelayAPIClient,
        accessTokenRefresher: @escaping @MainActor () async -> String? = { nil }
    ) {
        self.apiClient = apiClient
        self.accessTokenRefresher = accessTokenRefresher
    }

    func fetchCurrentHost(accessToken: String?) async throws -> HeraldHostStatus? {
        let response: CurrentHostResponse = try await performAuthorizedRequest(initialAccessToken: accessToken) { token in
            try await self.apiClient.get(
                path: "hosts/current",
                accessToken: token
            )
        }
        guard let host = response.host else { return nil }
        return mapHost(host)
    }

    func createEnrollmentCode(accessToken: String?) async throws -> HostEnrollmentCode {
        let response: EnrollmentResponse = try await performAuthorizedRequest(initialAccessToken: accessToken) { token in
            try await self.apiClient.post(
                path: "hosts/enrollment-codes",
                body: EmptyBody(),
                accessToken: token
            )
        }
        return HostEnrollmentCode(
            setupCode: response.setupCode,
            expiresAt: response.expiresAt,
            relayHost: response.relayHost
        )
    }

    func revokeCurrentHost(accessToken: String?) async throws {
        let _: EmptyResponse = try await performAuthorizedRequest(initialAccessToken: accessToken) { token in
            try await self.apiClient.post(
                path: "hosts/current/revoke",
                body: EmptyBody(),
                accessToken: token
            )
        }
    }

    private func mapHost(_ host: RelayHost) -> HeraldHostStatus {
        HeraldHostStatus(
            id: host.id,
            displayName: host.displayName,
            hostname: host.hostname,
            platform: host.platform,
            connectorVersion: host.connectorVersion,
            heraldCommand: host.heraldCommand,
            heraldVersion: host.heraldVersion,
            heraldModel: host.heraldModel,
            lastSeenAt: host.lastSeenAt,
            lastConnectedAt: host.lastConnectedAt,
            isOnline: host.isOnline
        )
    }

    private func performAuthorizedRequest<T>(
        initialAccessToken: String?,
        _ operation: @escaping @MainActor (_ accessToken: String?) async throws -> T
    ) async throws -> T {
        do {
            return try await operation(initialAccessToken)
        } catch RelayAPIClient.ClientError.unauthorized {
            guard let refreshedToken = await accessTokenRefresher(), !refreshedToken.isEmpty else {
                throw RelayAPIClient.ClientError.unauthorized("Expired or invalid access token.")
            }
            return try await operation(refreshedToken)
        }
    }
}

private struct EmptyResponse: Decodable {}
