import Foundation

@MainActor
final class PushRegistrationCoordinator {
    private struct RelayIdentityEnvelope: Decodable {
        let identity: PushBrokerRelayIdentity
    }

    private struct DirectPushRegisterBody: Encodable {
        let deviceId: String
        let transport: String = "direct"
        let apnsToken: String
        let pushEnvironment: String
        let bundleId: String
    }

    private struct RelayPushRegisterBody: Encodable {
        let deviceId: String
        let transport: String = "relay"
        let pushEnvironment: String
        let bundleId: String
        let relayHandle: String
        let sendGrant: String
        let relayId: String
        let relayPublicKey: String
        let tokenDebugSuffix: String?
    }

    private let relayAPIClient: RelayAPIClient
    private let brokerClient: PushBrokerClient?
    private let registrationStore: PushBrokerRegistrationStore
    private let appAttestService: any AppAttestServiceProtocol
    private let buildConfiguration: AppBuildConfiguration

    init(
        relayAPIClient: RelayAPIClient,
        brokerClient: PushBrokerClient?,
        registrationStore: PushBrokerRegistrationStore,
        appAttestService: any AppAttestServiceProtocol,
        buildConfiguration: AppBuildConfiguration
    ) {
        self.relayAPIClient = relayAPIClient
        self.brokerClient = brokerClient
        self.registrationStore = registrationStore
        self.appAttestService = appAttestService
        self.buildConfiguration = buildConfiguration
    }

    func registerPushToken(
        _ token: String,
        relayConfiguration: RelayConfiguration,
        accessToken: String,
        deviceID: UUID,
        installationID: UUID,
        bundleID: String,
        appVersion: String,
        pushEnvironment: String
    ) async throws -> Bool {
        if shouldUseBroker(relayConfiguration: relayConfiguration) {
            let relayResponse: RelayIdentityEnvelope = try await relayAPIClient.get(path: "relay/identity")
            let relayIdentity = relayResponse.identity
            let existingBrokerState = await brokerRegistrationState(
                relayIdentity: relayIdentity,
                token: token,
                installationID: installationID.uuidString.lowercased()
            )
            let brokerState: PushBrokerRegistrationState
            if let existingBrokerState {
                brokerState = existingBrokerState
            } else {
                brokerState = try await createBrokerRegistrationState(
                    relayIdentity: relayIdentity,
                    token: token,
                    installationID: installationID.uuidString.lowercased(),
                    bundleID: bundleID,
                    appVersion: appVersion,
                    pushEnvironment: pushEnvironment
                )
            }

            let body = RelayPushRegisterBody(
                deviceId: deviceID.uuidString.lowercased(),
                pushEnvironment: pushEnvironment,
                bundleId: bundleID,
                relayHandle: brokerState.relayHandle,
                sendGrant: brokerState.sendGrant,
                relayId: brokerState.relayID,
                relayPublicKey: brokerState.relayPublicKey,
                tokenDebugSuffix: brokerState.tokenDebugSuffix
            )
            struct Response: Decodable { let registered: Bool? }
            let _: Response = try await relayAPIClient.post(path: "push/register", body: body, accessToken: accessToken)
            return true
        }

        let body = DirectPushRegisterBody(
            deviceId: deviceID.uuidString.lowercased(),
            apnsToken: token,
            pushEnvironment: pushEnvironment,
            bundleId: bundleID
        )
        struct Response: Decodable { let registered: Bool? }
        let _: Response = try await relayAPIClient.post(path: "push/register", body: body, accessToken: accessToken)
        return true
    }

    private func shouldUseBroker(relayConfiguration: RelayConfiguration) -> Bool {
        relayConfiguration.connectionMode.reliesOnOfficialPushRelay && buildConfiguration.usesManagedPushBroker
    }

    private func brokerRegistrationState(
        relayIdentity: PushBrokerRelayIdentity,
        token: String,
        installationID: String
    ) async -> PushBrokerRegistrationState? {
        guard let state = await registrationStore.loadRegistrationState() else { return nil }
        guard state.installationID == installationID else { return nil }
        guard state.relayID == relayIdentity.id else { return nil }
        guard state.relayPublicKey == relayIdentity.publicKey else { return nil }
        guard state.tokenHash == PushBrokerRegistrationState.tokenHash(for: token) else { return nil }
        guard state.brokerBaseURL == brokerClient?.normalizedBaseURLString else { return nil }
        guard state.expiresAt > .now else { return nil }
        return state
    }

    private func createBrokerRegistrationState(
        relayIdentity: PushBrokerRelayIdentity,
        token: String,
        installationID: String,
        bundleID: String,
        appVersion: String,
        pushEnvironment: String
    ) async throws -> PushBrokerRegistrationState {
        guard let brokerClient else {
            throw RelayAPIClient.ClientError.requestFailed("Managed push broker is not configured in this build.")
        }
        let challenge = try await brokerClient.fetchChallenge()
        // Build the canonical signed-payload bytes with sorted keys + compact
        // separators so the relay can independently reconstruct the same byte
        // sequence from the register request fields. `relayBaseURL` falls back
        // to an empty string (never omitted) so Swift's nil-omission behaviour
        // doesn't diverge from Python's dict serialization server-side.
        let signedPayloadData = try canonicalSignedPayloadData(
            challengeId: challenge.challengeId,
            installationId: installationID,
            bundleId: bundleID,
            appVersion: appVersion,
            apnsEnvironment: pushEnvironment,
            apnsToken: token,
            relayIdentity: relayIdentity
        )
        let proof = try await appAttestService.createProof(challenge: challenge.challenge, signedPayload: signedPayloadData)
        let response = try await brokerClient.register(
            challenge: challenge,
            relayIdentity: relayIdentity,
            installationId: installationID,
            bundleId: bundleID,
            appVersion: appVersion,
            apnsEnvironment: pushEnvironment,
            apnsToken: token,
            proof: proof
        )
        let state = PushBrokerRegistrationState(
            relayHandle: response.relayHandle,
            sendGrant: response.sendGrant,
            relayID: response.relayId,
            relayPublicKey: response.relayPublicKey,
            brokerBaseURL: brokerClient.normalizedBaseURLString,
            installationID: installationID,
            tokenHash: PushBrokerRegistrationState.tokenHash(for: token),
            tokenDebugSuffix: response.tokenDebugSuffix,
            expiresAt: response.expiresAt
        )
        await registrationStore.saveRegistrationState(state)
        return state
    }

    func deactivatePushRegistration(accessToken: String) async throws {
        struct Response: Decodable { let deactivated: Bool? }
        let _: Response = try await relayAPIClient.post(
            path: "push/deactivate",
            accessToken: accessToken
        )
        await registrationStore.clearRegistrationState()
    }

    /// Drops the cached broker registration locally without contacting the
    /// relay. Used on unpair — the session is gone, so there's no access token
    /// to call `/push/deactivate`, but we still need to forget the cached
    /// `sendGrant`/`relayHandle` so a future re-pair mints a fresh one rather
    /// than reusing state tied to the now-revoked device session.
    func clearLocalBrokerRegistration() async {
        await registrationStore.clearRegistrationState()
    }
}

// Encodes the push-broker register inputs into a byte sequence that matches
// `canonical_push_broker_signed_payload` in relay/app/push_broker.py. The
// shared format is:
//
//   - JSON with alphabetical top-level and nested keys
//   - Compact separators (`,`/`:`, no whitespace)
//   - UTF-8 bytes
//   - Nullable fields (`appVersion`, `relayBaseURL`) emitted as ``""``
//
// Any drift between the two sides will cause App Attest verification to fail
// because the relay hashes a different byte sequence than the device signed.
private func canonicalSignedPayloadData(
    challengeId: String,
    installationId: String,
    bundleId: String,
    appVersion: String,
    apnsEnvironment: String,
    apnsToken: String,
    relayIdentity: PushBrokerRelayIdentity
) throws -> Data {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
    return try encoder.encode(
        CanonicalPushBrokerSignedPayload(
            apnsEnvironment: apnsEnvironment,
            apnsToken: apnsToken,
            appVersion: appVersion,
            bundleId: bundleId,
            challengeId: challengeId,
            installationId: installationId,
            relayIdentity: CanonicalRelayIdentity(
                id: relayIdentity.id,
                publicKey: relayIdentity.publicKey,
                relayBaseURL: relayIdentity.relayBaseURL ?? ""
            )
        )
    )
}

private struct CanonicalPushBrokerSignedPayload: Encodable {
    let apnsEnvironment: String
    let apnsToken: String
    let appVersion: String
    let bundleId: String
    let challengeId: String
    let installationId: String
    let relayIdentity: CanonicalRelayIdentity
}

private struct CanonicalRelayIdentity: Encodable {
    let id: String
    let publicKey: String
    let relayBaseURL: String
}
