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
    var toolCallID: String?
    var name: String?
    var emoji: String?
    var argsPreview: String?
    var resultPreview: String?
    var finishedAt: Date?
    var isError: Bool
    var durationMs: Int?

    private enum CodingKeys: String, CodingKey {
        case id, label, startedAt, isActive, toolCallID, name, emoji, argsPreview, resultPreview, finishedAt, isError, durationMs
    }

    init(
        id: UUID = UUID(),
        label: String,
        startedAt: Date = .now,
        isActive: Bool = true,
        toolCallID: String? = nil,
        name: String? = nil,
        emoji: String? = nil,
        argsPreview: String? = nil,
        resultPreview: String? = nil,
        finishedAt: Date? = nil,
        isError: Bool = false,
        durationMs: Int? = nil
    ) {
        self.id = id
        self.label = label
        self.startedAt = startedAt
        self.isActive = isActive
        self.toolCallID = toolCallID
        self.name = name
        self.emoji = emoji
        self.argsPreview = argsPreview
        self.resultPreview = resultPreview
        self.finishedAt = finishedAt
        self.isError = isError
        self.durationMs = durationMs
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(UUID.self, forKey: .id)
        label = try c.decode(String.self, forKey: .label)
        startedAt = try c.decode(Date.self, forKey: .startedAt)
        isActive = try c.decodeIfPresent(Bool.self, forKey: .isActive) ?? false
        toolCallID = try c.decodeIfPresent(String.self, forKey: .toolCallID)
        name = try c.decodeIfPresent(String.self, forKey: .name)
        emoji = try c.decodeIfPresent(String.self, forKey: .emoji)
        argsPreview = try c.decodeIfPresent(String.self, forKey: .argsPreview)
        resultPreview = try c.decodeIfPresent(String.self, forKey: .resultPreview)
        finishedAt = try c.decodeIfPresent(Date.self, forKey: .finishedAt)
        isError = try c.decodeIfPresent(Bool.self, forKey: .isError) ?? false
        durationMs = try c.decodeIfPresent(Int.self, forKey: .durationMs)
    }
}
