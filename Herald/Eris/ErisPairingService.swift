import Foundation

// MARK: - Eris pairing service
//
// The `PairingServiceProtocol` implementation the 0.4.0 onboarding calls. It
// redeems an Eris pairing code against the daemon the user typed in the "relay
// URL" field, stores the pair in the keychain (exactly where `ErisDaemonStore` /
// `ErisHeraldClient` read it), and reports the daemon as the app's host.

@MainActor
final class ErisPairingService: PairingServiceProtocol {
    func normalizePairingCode(_ rawCode: String) throws -> String {
        let normalized = rawCode
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .uppercased()
            .replacingOccurrences(of: "-", with: "")
            .replacingOccurrences(of: " ", with: "")
        guard !normalized.isEmpty else {
            throw PhonePairingCodeError.invalidFormat
        }
        return normalized
    }

    func redeemPairingCode(
        _ normalizedCode: String,
        request: DeviceRegistrationRequest
    ) async throws -> PairingRedeemResult {
        // The 0.4.0 "relay URL" field carries the Eris daemon address; accept a
        // scheme-less host:port the way the daemon's own screen shows it.
        let address = ErisDaemonStore.normalizedAddress(request.relayBaseURLString)
        guard let baseURL = URL(string: address) else {
            throw ErisError.invalidBaseURL(request.relayBaseURLString)
        }

        let client = ErisClient(baseURL: baseURL, tokenStore: KeychainErisTokenStore())
        let pair = try await client.pair(code: normalizedCode, deviceName: request.deviceName)

        // `ErisClient.pair` stored the pair (base URL + tokens) in the keychain
        // before returning, so `ErisDaemonStore.start()` / `ErisHeraldClient
        // .connect()` restore it on the next connect without re-pairing.
        let host = baseURL.host ?? baseURL.absoluteString
        let deviceID = pair.deviceID.flatMap { UUID(uuidString: $0) }

        return PairingRedeemResult(
            configuration: PairedRelayConfiguration(
                baseURLString: address,
                hostDisplayName: host,
                pairedAt: .now
            ),
            state: AppSessionState(
                deviceID: deviceID,
                installationID: request.installationID,
                deviceRegistered: true,
                connectionStatus: .connected,
                syncStatus: .synced,
                isMockMode: false,
                backendEndpoint: address,
                lastSyncAt: .now,
                pushTokenRegistered: false
            ),
            tokens: AuthTokens(
                accessToken: pair.accessToken,
                refreshToken: pair.refreshToken ?? "",
                expiresAt: pair.expiresAt ?? Date().addingTimeInterval(3600)
            )
        )
    }
}
