import Foundation

enum MessageSender: String, Codable, Hashable, Sendable {
    case user
    case herald
    case system
    case voiceUser = "voice_user"
    case voiceHerald = "voice_herald"
}
