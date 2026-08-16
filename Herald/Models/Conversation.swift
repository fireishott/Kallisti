import Foundation

struct Conversation: Codable, Identifiable, Hashable, Sendable {
    let id: UUID
    var title: String
    var messages: [Message]
    var lastActivity: Date
    var latestUsage: TokenUsage?
    var contextPercent: Double?
    /// Build 128.45: the native gateway session_id this conversation's
    /// messages actually live under. Persisted with the cache so a merge can
    /// detect when a local conversation has been re-pointed at a DIFFERENT
    /// server session (resume/switch after a gateway restart). Without this,
    /// the old session's cached rows get spliced into the new session's
    /// thread - the out-of-order cross-session mix. Optional so caches
    /// written before this build decode cleanly.
    var sessionKey: String?

    init(
        id: UUID = UUID(),
        title: String,
        messages: [Message] = [],
        lastActivity: Date = .now,
        latestUsage: TokenUsage? = nil,
        contextPercent: Double? = nil,
        sessionKey: String? = nil
    ) {
        self.id = id
        self.title = title
        self.messages = messages
        self.lastActivity = lastActivity
        self.latestUsage = latestUsage
        self.contextPercent = contextPercent
        self.sessionKey = sessionKey
    }

    var lastMessage: Message? {
        messages.last
    }

    var previewText: String {
        lastMessage?.content ?? "No messages yet"
    }
}
