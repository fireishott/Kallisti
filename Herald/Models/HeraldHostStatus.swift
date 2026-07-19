import Foundation

struct HeraldHostStatus: Codable, Hashable, Sendable {
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

    var resolvedDisplayName: String {
        displayName ?? hostname ?? "Herald Host"
    }
}
