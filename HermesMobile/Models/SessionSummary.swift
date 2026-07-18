import Foundation

/// Source channel where a session originated.
enum SessionSource: String, Codable, Sendable, CaseIterable {
    case agent
    case telegram
    case discord
    case slack
    case whatsapp
    case signal
    case unknown

    var displayName: String {
        switch self {
        case .agent:    return "Hermes"
        case .telegram: return "Telegram"
        case .discord:  return "Discord"
        case .slack:    return "Slack"
        case .whatsapp: return "WhatsApp"
        case .signal:   return "Signal"
        case .unknown:  return "Unknown"
        }
    }

    var iconName: String {
        switch self {
        case .agent:    return "bubble.left.and.bubble.right"
        case .telegram: return "paperplane"
        case .discord:  return "gamecontroller"
        case .slack:    return "number.square"
        case .whatsapp: return "phone"
        case .signal:   return "lock.shield"
        case .unknown:  return "questionmark.circle"
        }
    }
}

/// Lightweight session representation for the sidebar.
struct SessionSummary: Codable, Identifiable, Hashable, Sendable {
    let id: UUID
    var title: String
    var previewText: String
    var lastActivity: Date
    var createdAt: Date
    var isPinned: Bool
    var messageCount: Int
    var source: SessionSource
    var isActive: Bool

    init(
        id: UUID = UUID(),
        title: String,
        previewText: String = "",
        lastActivity: Date = .now,
        createdAt: Date = .now,
        isPinned: Bool = false,
        messageCount: Int = 0,
        source: SessionSource = .agent,
        isActive: Bool = false
    ) {
        self.id = id
        self.title = title
        self.previewText = previewText
        self.lastActivity = lastActivity
        self.createdAt = createdAt
        self.isPinned = isPinned
        self.messageCount = messageCount
        self.source = source
        self.isActive = isActive
    }

    /// Whether this session had activity within the last 5 minutes.
    var isRecentlyActive: Bool {
        Date.now.timeIntervalSince(lastActivity) < 300
    }
}
