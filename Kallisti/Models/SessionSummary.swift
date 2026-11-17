import Foundation

/// Lightweight representation of a conversation session for sidebar listing.
/// Maps to the relay API's session list response without loading full message history.
struct SessionSummary: Codable, Identifiable, Hashable, Sendable {
    let id: UUID
    var title: String
    var previewText: String
    var lastActivity: Date
    var source: String?
    var isPinned: Bool
    var isArchived: Bool
    /// The real Hermes session id (e.g. 20260815_194256_f67d9a) as stored
    /// in state.db. The app-facing id is a deterministic UUID alias;
    /// this is what you'd paste into hermes -r or an @session link.
    var sessionKey: String?
    /// True when the gateway reports this session has an active turn in
    /// flight (status working). Rendered as a blue dot next to the row.
    var hasActivity: Bool = false

    init(
        id: UUID = UUID(),
        title: String,
        previewText: String = "",
        lastActivity: Date = .now,
        source: String? = nil,
        isPinned: Bool = false,
        isArchived: Bool = false,
        sessionKey: String? = nil,
        hasActivity: Bool = false
    ) {
        self.id = id
        self.title = title
        self.previewText = previewText
        self.lastActivity = lastActivity
        self.source = source
        self.isPinned = isPinned
        self.isArchived = isArchived
        self.sessionKey = sessionKey
        self.hasActivity = hasActivity
    }

    /// Icon name for the session source (iMessage, Telegram, CLI, etc.)
    var sourceIcon: String {
        switch source?.lowercased() {
        case "imessage", "messages":  "message.fill"
        case "telegram":              "paperplane.fill"
        case "slack":                 "number.square.fill"
        case "discord":               "gamecontroller.fill"
        case "whatsapp":              "phone.fill"
        case "web":                   "globe"
        case "ios", "herald-ios":     "iphone"
        case "cli", "terminal":       "terminal.fill"
        case "voice", "talk":         "waveform"
        default:                      "bubble.left.and.bubble.right"
        }
    }

    /// Relative time string (e.g. "2m ago", "3h ago", "Yesterday")
    var relativeTimeString: String {
        let interval = Date().timeIntervalSince(lastActivity)
        if interval < 60 { return "now" }
        if interval < 3600 { return "\(Int(interval / 60))m ago" }
        if interval < 86400 { return "\(Int(interval / 3600))h ago" }
        if interval < 172800 { return "Yesterday" }
        let days = Int(interval / 86400)
        if days < 7 { return "\(days)d ago" }
        let formatter = DateFormatter()
        formatter.dateStyle = .short
        return formatter.string(from: lastActivity)
    }
}
