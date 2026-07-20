import Foundation

enum MessageSender: String, Codable, Hashable, Sendable {
    case user
    case herald
    case system
    case voiceUser = "voice_user"
    case voiceHerald = "voice_herald"

    init(from decoder: Decoder) throws {
        let raw = try decoder.singleValueContainer().decode(String.self)
        switch raw {
        case "herald", "hermes", "assistant": self = .herald
        case "user": self = .user
        case "system": self = .system
        case "voice_user": self = .voiceUser
        case "voice_herald": self = .voiceHerald
        default: self = .herald
        }
    }
}
