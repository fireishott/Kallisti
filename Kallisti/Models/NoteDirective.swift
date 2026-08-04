import Foundation

/// A parsed directive from recognized text (e.g., `#research battery supply chain`).
struct NoteDirective: Codable, Identifiable, Hashable, Sendable {
    let id: String  // stable ID derived from fingerprint
    let command: NoteCommand
    let arguments: String
    let sourceRange: NSRange
    let fingerprint: String  // normalized hash over (noteId, sourceTextRevision, command, arguments, sourceRange)

    init(
        id: String,
        command: NoteCommand,
        arguments: String,
        sourceRange: NSRange,
        fingerprint: String
    ) {
        self.id = id
        self.command = command
        self.arguments = arguments
        self.sourceRange = sourceRange
        self.fingerprint = fingerprint
    }
}

// MARK: - Note Command (v1 allowlist)

/// v1 command allowlist — read-only commands only. Unknown tags are data.
enum NoteCommand: String, Codable, Sendable, CaseIterable {
    case research
    case search
    case talkingPoints = "talkingpoints"
    case summary
    case actions
    case questions

    /// Parse a command string (case-insensitive) to a known command.
    /// Returns nil for unknown tags.
    static func parse(_ raw: String) -> NoteCommand? {
        NoteCommand(rawValue: raw.lowercased().trimmingCharacters(in: .whitespacesAndNewlines))
    }

    var displayName: String {
        switch self {
        case .research:      "Research"
        case .search:        "Search"
        case .talkingPoints: "Talking Points"
        case .summary:       "Summary"
        case .actions:       "Action Items"
        case .questions:     "Questions"
        }
    }
}

// MARK: - Directive Status

/// Execution status for a single directive within a run.
enum DirectiveStatus: String, Codable, Sendable {
    case pending
    case completed
    case failed
    case skipped  // e.g., unknown command, relay-side allowlist rejection
}

// MARK: - Command Result

/// The result of executing a single directive.
struct NoteCommandResult: Codable, Hashable, Sendable, Identifiable {
    var id: String { directiveId }
    let directiveId: String
    let status: DirectiveStatus
    let sectionIndex: Int?
    let errorText: String?

    init(
        directiveId: String,
        status: DirectiveStatus,
        sectionIndex: Int? = nil,
        errorText: String? = nil
    ) {
        self.directiveId = directiveId
        self.status = status
        self.sectionIndex = sectionIndex
        self.errorText = errorText
    }
}
