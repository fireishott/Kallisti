import Foundation

struct TranscriptFailure: Sendable, Equatable, Codable {
    let category: String?
    let message: String?
    let retryable: Bool
}

enum TranscriptRowKind: Sendable, Equatable, Codable {
    case user
    case assistant
    case reasoning
    case tool
    case systemStatus
}

enum TranscriptRowLifecycle: Sendable, Equatable, Codable {
    case optimistic
    case submitting
    case accepted
    case streaming
    case complete
    case failed(TranscriptFailure)
    case rejected(TranscriptFailure)
    case cancelled
    case deleted
}

/// Reducer-owned transcript row. `displayContent` is restricted to text intended
/// for display and must never contain prompt envelopes or transport metadata.
struct TranscriptRow: Sendable, Identifiable {
    let renderID: TranscriptRenderID
    let canonicalMessageID: CanonicalMessageID?
    let clientMessageID: ClientMessageID?
    let jobID: JobID?
    var canonicalSequence: CanonicalSequence?
    var messageRevision: MessageRevision
    let conversationRevisionSeen: ConversationRevision
    var retryGeneration: Int
    let localOrdinal: LocalOrdinal
    let kind: TranscriptRowKind
    var lifecycle: TranscriptRowLifecycle
    var displayContent: String
    var reasoning: String?
    var toolActivity: ToolActivity?
    var attachments: [MessageAttachment]
    let createdAt: Date
    var lastUpdatedAt: Date

    var id: TranscriptRenderID { renderID }
}
