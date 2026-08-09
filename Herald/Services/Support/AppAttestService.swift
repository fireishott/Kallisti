import CryptoKit
import DeviceCheck
import Foundation

struct AppAttestProof: Sendable {
    let keyId: String
    let attestationObject: String
    let assertion: String
}

@MainActor
protocol AppAttestServiceProtocol {
    func createProof(challenge: String, signedPayload: Data) async throws -> AppAttestProof
}

enum AppAttestServiceError: LocalizedError {
    case unsupported
    case invalidPayload

    var errorDescription: String? {
        switch self {
        case .unsupported:
            return "App Attest is unavailable on this device."
        case .invalidPayload:
            return "The App Attest payload was invalid."
        }
    }
}

@MainActor
final class LiveAppAttestService: AppAttestServiceProtocol {
    init(secureStore: any SecureStoreProtocol) {}

    func createProof(challenge: String, signedPayload: Data) async throws -> AppAttestProof {
        let service = DCAppAttestService.shared
        guard service.isSupported else { throw AppAttestServiceError.unsupported }

        let keyID = try await service.generateKey()
        let challengeHash = Data(SHA256.hash(data: Data(challenge.utf8)))
        let attestationObject = try await service.attestKey(keyID, clientDataHash: challengeHash)
        let signedPayloadHash = Data(SHA256.hash(data: signedPayload))
        let assertion = try await service.generateAssertion(keyID, clientDataHash: signedPayloadHash)

        return AppAttestProof(
            keyId: keyID,
            attestationObject: b64url(attestationObject),
            assertion: b64url(assertion)
        )
    }

    private func b64url(_ data: Data) -> String {
        data.base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
