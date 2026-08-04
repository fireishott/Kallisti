import Foundation

struct PushBrokerRelayIdentity: Codable, Sendable, Equatable {
    let id: String
    let publicKey: String
    let relayBaseURL: String?
}

struct PushBrokerChallengeResponse: Decodable, Sendable {
    let challengeId: String
    let challenge: String
    let expiresAt: Date
}

struct PushBrokerRegisterResponse: Decodable, Sendable {
    let transport: String
    let relayHandle: String
    let sendGrant: String
    let relayId: String
    let relayPublicKey: String
    let installationId: String
    let topic: String
    let environment: String
    let tokenDebugSuffix: String?
    let expiresAt: Date
}

@MainActor
final class PushBrokerClient {
    private struct Envelope<T: Decodable>: Decodable {
        let data: T
    }

    private struct ChallengeBody: Encodable {}

    private struct AppAttestBody: Encodable {
        let keyId: String
        let attestationObject: String
        let assertion: String
    }

    private struct RegisterBody: Encodable {
        let challengeId: String
        let challenge: String
        let relayIdentity: PushBrokerRelayIdentity
        let installationId: String
        let bundleId: String
        let appVersion: String
        let apnsEnvironment: String
        let apnsToken: String
        let appAttest: AppAttestBody
    }

    let baseURL: URL
    private let session: URLSession
    private let encoder = RelayCoders.makeEncoder()
    private let decoder = RelayCoders.makeDecoder()

    init(baseURL: URL, session: URLSession = .shared) {
        self.baseURL = baseURL
        self.session = session
    }

    var normalizedBaseURLString: String {
        var value = baseURL.absoluteString
        while value.hasSuffix("/") {
            value.removeLast()
        }
        return value
    }

    func fetchChallenge() async throws -> PushBrokerChallengeResponse {
        let request = try makeRequest(path: "push-broker/challenge", method: "POST", body: try encoder.encode(ChallengeBody()))
        return try await send(request)
    }

    func register(
        challenge: PushBrokerChallengeResponse,
        relayIdentity: PushBrokerRelayIdentity,
        installationId: String,
        bundleId: String,
        appVersion: String,
        apnsEnvironment: String,
        apnsToken: String,
        proof: AppAttestProof
    ) async throws -> PushBrokerRegisterResponse {
        let body = RegisterBody(
            challengeId: challenge.challengeId,
            challenge: challenge.challenge,
            relayIdentity: relayIdentity,
            installationId: installationId,
            bundleId: bundleId,
            appVersion: appVersion,
            apnsEnvironment: apnsEnvironment,
            apnsToken: apnsToken,
            appAttest: AppAttestBody(
                keyId: proof.keyId,
                attestationObject: proof.attestationObject,
                assertion: proof.assertion
            )
        )
        let request = try makeRequest(path: "push-broker/register", method: "POST", body: try encoder.encode(body))
        return try await send(request)
    }

    private func makeRequest(path: String, method: String, body: Data?) throws -> URLRequest {
        let trimmedPath = path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard let url = URL(string: "\(normalizedBaseURLString)/\(trimmedPath)") else {
            throw RelayAPIClient.ClientError.invalidURL(normalizedBaseURLString)
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.httpBody = body
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        return request
    }

    private func send<T: Decodable>(_ request: URLRequest) async throws -> T {
        let (data, response) = try await session.data(for: request)
        guard let httpResponse = response as? HTTPURLResponse else {
            throw RelayAPIClient.ClientError.requestFailed("Push broker returned an invalid response.")
        }
        guard (200..<300).contains(httpResponse.statusCode) else {
            throw RelayAPIClient.ClientError.requestFailed("Push broker request failed with status \(httpResponse.statusCode).")
        }
        return try decoder.decode(Envelope<T>.self, from: data).data
    }
}
