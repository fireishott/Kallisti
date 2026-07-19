import CryptoKit
import Foundation

struct PushBrokerRegistrationState: Codable, Equatable, Sendable {
    let relayHandle: String
    let sendGrant: String
    let relayID: String
    let relayPublicKey: String
    let brokerBaseURL: String
    let installationID: String
    let tokenHash: String
    let tokenDebugSuffix: String?
    let expiresAt: Date

    static func tokenHash(for token: String) -> String {
        SHA256.hash(data: Data(token.utf8)).map { String(format: "%02x", $0) }.joined()
    }
}

@MainActor
final class PushBrokerRegistrationStore {
    private enum Keys {
        static let registrationState = "push.broker.registrationState"
    }

    private let secureStore: any SecureStoreProtocol
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    init(secureStore: any SecureStoreProtocol) {
        self.secureStore = secureStore
        encoder.dateEncodingStrategy = .iso8601
        decoder.dateDecodingStrategy = .iso8601
    }

    func loadRegistrationState() async -> PushBrokerRegistrationState? {
        guard let raw = await secureStore.retrieve(key: Keys.registrationState),
              let data = raw.data(using: .utf8) else { return nil }
        return try? decoder.decode(PushBrokerRegistrationState.self, from: data)
    }

    func saveRegistrationState(_ state: PushBrokerRegistrationState) async {
        guard let data = try? encoder.encode(state),
              let raw = String(data: data, encoding: .utf8) else { return }
        await secureStore.store(key: Keys.registrationState, value: raw)
    }

    func clearRegistrationState() async {
        await secureStore.delete(key: Keys.registrationState)
    }
}
