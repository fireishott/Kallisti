import Foundation

/// A single tool invocation event captured during streaming.
///
/// Tool activities are accumulated on the ``Message`` during streaming so the UI
/// can show a compact, expandable timeline of what Herald did.
struct ToolActivity: Codable, Identifiable, Hashable, Sendable {
    let id: UUID
    let label: String
    let startedAt: Date
    var isActive: Bool

    init(
        id: UUID = UUID(),
        label: String,
        startedAt: Date = .now,
        isActive: Bool = true
    ) {
        self.id = id
        self.label = label
        self.startedAt = startedAt
        self.isActive = isActive
    }
}
