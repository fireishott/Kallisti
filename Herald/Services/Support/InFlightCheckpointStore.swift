import Foundation

/// Client-persisted checkpoint capturing the last known in-flight turn state.
/// Saved on background-task expiry so the next launch can immediately show
/// a "resuming" indicator instead of a blank or failed screen.
struct InFlightCheckpoint: Codable, Equatable {
    let conversationID: UUID
    let nativeSessionID: String
    let jobID: UUID?
    let backgroundedAt: Date
}

@MainActor
final class InFlightCheckpointStore {
    private static let key = "kallisti.inFlightCheckpoint"

    static func save(_ checkpoint: InFlightCheckpoint) {
        guard let data = try? JSONEncoder().encode(checkpoint) else { return }
        UserDefaults.standard.set(data, forKey: key)
    }

    static func load() -> InFlightCheckpoint? {
        guard let data = UserDefaults.standard.data(forKey: key) else { return nil }
        return try? JSONDecoder().decode(InFlightCheckpoint.self, from: data)
    }

    static func clear() {
        UserDefaults.standard.removeObject(forKey: key)
    }
}
