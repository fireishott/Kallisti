import Foundation

struct SSEEvent: Sendable {
    let event: String
    let data: String
    let id: String?
}
