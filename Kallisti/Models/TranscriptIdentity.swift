import Foundation

/// Stable identity used by the transcript renderer. It is never replaced when a
/// provisional row is matched to a canonical server row.
struct TranscriptRenderID: Hashable, Codable, Sendable, Identifiable {
    let rawValue: UUID

    init(rawValue: UUID = UUID()) { self.rawValue = rawValue }
    var id: UUID { rawValue }
}

struct CanonicalConversationID: Hashable, Codable, Sendable, Comparable {
    let rawValue: String
    init?(_ rawValue: String) {
        let value = rawValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return nil }
        self.rawValue = value
    }
    init?(jsonString: String) { self.init(jsonString) }
    static func < (lhs: Self, rhs: Self) -> Bool { lhs.rawValue < rhs.rawValue }
}

struct CanonicalMessageID: Hashable, Codable, Sendable, Comparable {
    let rawValue: String
    init?(_ rawValue: String) {
        let value = rawValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return nil }
        self.rawValue = value
    }
    init?(jsonString: String) { self.init(jsonString) }
    static func < (lhs: Self, rhs: Self) -> Bool { lhs.rawValue < rhs.rawValue }
}

struct ClientMessageID: Hashable, Codable, Sendable, Comparable {
    let rawValue: String
    init?(_ rawValue: String) {
        let value = rawValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return nil }
        self.rawValue = value
    }
    init?(jsonString: String) { self.init(jsonString) }
    static func < (lhs: Self, rhs: Self) -> Bool { lhs.rawValue < rhs.rawValue }
}

struct JobID: Hashable, Codable, Sendable, Comparable {
    let rawValue: String
    init?(_ rawValue: String) {
        let value = rawValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return nil }
        self.rawValue = value
    }
    init?(jsonString: String) { self.init(jsonString) }
    static func < (lhs: Self, rhs: Self) -> Bool { lhs.rawValue < rhs.rawValue }
}

struct ConversationRevision: Hashable, Codable, Sendable, Comparable {
    let rawValue: Int
    static let zero = Self(rawValue: 0)
    init(rawValue: Int) { self.rawValue = rawValue }
    init?(_ rawValue: Int) { guard rawValue >= 0 else { return nil }; self.rawValue = rawValue }
    static func < (lhs: Self, rhs: Self) -> Bool { lhs.rawValue < rhs.rawValue }
    func incremented() -> Self { Self(rawValue: rawValue + 1) }
}

struct MessageRevision: Hashable, Codable, Sendable, Comparable {
    let rawValue: Int
    static let zero = Self(rawValue: 0)
    init(rawValue: Int) { self.rawValue = rawValue }
    init?(_ rawValue: Int) { guard rawValue >= 0 else { return nil }; self.rawValue = rawValue }
    static func < (lhs: Self, rhs: Self) -> Bool { lhs.rawValue < rhs.rawValue }
}

struct CanonicalSequence: Hashable, Codable, Sendable, Comparable {
    let rawValue: Int
    init(rawValue: Int) { self.rawValue = rawValue }
    init?(_ rawValue: Int) { guard rawValue > 0 else { return nil }; self.rawValue = rawValue }
    static func < (lhs: Self, rhs: Self) -> Bool { lhs.rawValue < rhs.rawValue }
}

struct NavigationEpoch: Hashable, Codable, Sendable, Comparable {
    let rawValue: UInt64
    init(rawValue: UInt64 = 0) { self.rawValue = rawValue }
    static let zero = Self(rawValue: 0)
    static func < (lhs: Self, rhs: Self) -> Bool { lhs.rawValue < rhs.rawValue }
    func incremented() -> Self { Self(rawValue: rawValue &+ 1) }
}

struct LocalOrdinal: Hashable, Codable, Sendable, Comparable {
    let rawValue: UInt64
    init(rawValue: UInt64) { self.rawValue = rawValue }
    static func < (lhs: Self, rhs: Self) -> Bool { lhs.rawValue < rhs.rawValue }
}

struct TranscriptDiagnostic: Hashable, Codable, Sendable {
    let category: String
    let message: String
    let conversationID: CanonicalConversationID?
    let renderID: TranscriptRenderID?
    let canonicalMessageID: CanonicalMessageID?
    let jobID: JobID?

    init(category: String, message: String, conversationID: CanonicalConversationID? = nil,
         renderID: TranscriptRenderID? = nil, canonicalMessageID: CanonicalMessageID? = nil,
         jobID: JobID? = nil) {
        self.category = category
        self.message = message
        self.conversationID = conversationID
        self.renderID = renderID
        self.canonicalMessageID = canonicalMessageID
        self.jobID = jobID
    }
}
