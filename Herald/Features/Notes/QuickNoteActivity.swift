import Foundation

/// Constants for Quick Note NSUserActivity integration.
enum QuickNoteConstants {
    /// Activity type for viewing a note (registered in Info.plist NSUserActivityTypes).
    static let activityType = "net.fihonline.herald.viewNote"

    /// Prefix for content identifiers — a note with UUID `X` maps to `"note-X"`.
    static let contentIdentifierPrefix = "note-"

    /// Build the content identifier string for a note UUID.
    static func contentIdentifier(for noteId: UUID) -> String {
        "\(contentIdentifierPrefix)\(noteId.uuidString)"
    }

    /// Extract a note UUID from a content identifier string.
    static func noteId(from contentIdentifier: String) -> UUID? {
        guard contentIdentifier.hasPrefix(contentIdentifierPrefix) else { return nil }
        let uuidString = String(contentIdentifier.dropFirst(contentIdentifierPrefix.count))
        return UUID(uuidString: uuidString)
    }
}

/// Parsed parameters from a `herald://share` URL.
struct SharedContentParams: Equatable {
    let text: String
    let title: String?
}

/// Parses `herald://share?text=...&title=...` URLs for receiving shared content.
enum ShareURLParser {
    static func parse(_ url: URL) -> SharedContentParams? {
        guard url.scheme == "herald", url.host == "share" else { return nil }
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let queryItems = components.queryItems else { return nil }

        guard let textItem = queryItems.first(where: { $0.name == "text" }),
              let text = textItem.value, !text.isEmpty else { return nil }

        let title = queryItems.first(where: { $0.name == "title" })?.value
        return SharedContentParams(text: text, title: title)
    }
}
