import Foundation

struct Conversation: Codable, Identifiable, Hashable, Sendable {
    let id: UUID
    var title: String
    var messages: [Message]
    var lastActivity: Date
    var latestUsage: TokenUsage?
    var contextPercent: Double?

    init(
        id: UUID = UUID(),
        title: String,
        messages: [Message] = [],
        lastActivity: Date = .now,
        latestUsage: TokenUsage? = nil,
        contextPercent: Double? = nil
    ) {
        self.id = id
        self.title = title
        self.messages = messages
        self.lastActivity = lastActivity
        self.latestUsage = latestUsage
        self.contextPercent = contextPercent
    }

    var lastMessage: Message? {
        messages.last
    }

    var previewText: String {
        lastMessage?.content ?? "No messages yet"
    }
}
