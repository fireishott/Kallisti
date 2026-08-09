import Foundation

/// Client-side model for an enrichment result from a completed run.
/// Mirrors the relay's EnrichedNoteRevision structure.
struct EnrichmentResult: Codable, Identifiable, Hashable, Sendable {
    let id: UUID
    let noteId: UUID
    let runId: UUID
    let sourceDrawingRevision: Int
    let sourceTextRevision: Int
    let schemaVersion: Int
    let title: String
    let markdown: String
    let sections: [EnrichedSection]
    let citations: [EnrichedCitation]
    let commandResults: [NoteCommandResult]
    let warnings: [String]
    let isStale: Bool
    let createdAt: Date

    init(
        id: UUID = UUID(),
        noteId: UUID,
        runId: UUID,
        sourceDrawingRevision: Int,
        sourceTextRevision: Int,
        schemaVersion: Int = 1,
        title: String,
        markdown: String,
        sections: [EnrichedSection] = [],
        citations: [EnrichedCitation] = [],
        commandResults: [NoteCommandResult] = [],
        warnings: [String] = [],
        isStale: Bool = false,
        createdAt: Date = .now
    ) {
        self.id = id
        self.noteId = noteId
        self.runId = runId
        self.sourceDrawingRevision = sourceDrawingRevision
        self.sourceTextRevision = sourceTextRevision
        self.schemaVersion = schemaVersion
        self.title = title
        self.markdown = markdown
        self.sections = sections
        self.citations = citations
        self.commandResults = commandResults
        self.warnings = warnings
        self.isStale = isStale
        self.createdAt = createdAt
    }
}

// MARK: - Enriched Section

/// A structured section within an enriched document.
struct EnrichedSection: Codable, Identifiable, Hashable, Sendable {
    let id: UUID
    let kind: SectionKind
    let title: String
    let markdown: String

    init(
        id: UUID = UUID(),
        kind: SectionKind,
        title: String,
        markdown: String
    ) {
        self.id = id
        self.kind = kind
        self.title = title
        self.markdown = markdown
    }
}

enum SectionKind: String, Codable, Sendable {
    case summary
    case commandResult = "command_result"
}

// MARK: - Enriched Citation

/// A citation with source information and access date.
struct EnrichedCitation: Codable, Identifiable, Hashable, Sendable {
    let id: UUID
    let title: String
    let url: String?
    let accessedAt: Date

    init(
        id: UUID = UUID(),
        title: String,
        url: String? = nil,
        accessedAt: Date = .now
    ) {
        self.id = id
        self.title = title
        self.url = url
        self.accessedAt = accessedAt
    }
}
