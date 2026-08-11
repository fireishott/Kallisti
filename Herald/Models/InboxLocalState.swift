import Foundation

struct InboxLocalState: Codable, Hashable, Sendable {
    var readItemIDs: Set<String> = []
    var dismissedItemIDs: Set<String> = []
    /// Build 68: snoozed items - stable identifier -> date to re-surface.
    /// The item disappears from the list until that time, then comes back
    /// as actionable so the user can act on it later.
    var snoozedItemIDs: [String: Date] = [:]
}
