import Foundation

// MARK: - Note

/// A Herald note — the top-level container for ink, recognition, and enrichment.
/// Metadata-only; drawing blobs live on disk as atomic `.pkdrawing` files.
struct HeraldNote: Codable, Identifiable, Hashable, Sendable {
    let id: UUID
    var title: String
    var folderId: UUID?
    var pinned: Bool
    var currentDrawingRevisionId: UUID?
    var currentRevision: Int
    var currentDrawingRevision: Int
    var currentTextRevision: Int
    var pageStyle: NotePageStyle
    var syncState: NoteSyncState
    let createdAt: Date
    var updatedAt: Date
    var deletedAt: Date?

    var isPinned: Bool { pinned }
    var folder: NoteFolder? { nil } // folders not yet wired; folderId is the raw key

    init(
        id: UUID = UUID(),
        title: String = "",
        folderId: UUID? = nil,
        pinned: Bool = false,
        currentDrawingRevisionId: UUID? = nil,
        currentRevision: Int = 0,
        currentDrawingRevision: Int = 0,
        currentTextRevision: Int = 0,
        pageStyle: NotePageStyle = .linesMedium,
        syncState: NoteSyncState = .local,
        createdAt: Date = .now,
        updatedAt: Date = .now,
        deletedAt: Date? = nil
    ) {
        self.id = id
        self.title = title
        self.folderId = folderId
        self.pinned = pinned
        self.currentDrawingRevisionId = currentDrawingRevisionId
        self.currentRevision = currentRevision
        self.currentDrawingRevision = currentDrawingRevision
        self.currentTextRevision = currentTextRevision
        self.pageStyle = pageStyle
        self.syncState = syncState
        self.createdAt = createdAt
        self.updatedAt = updatedAt
        self.deletedAt = deletedAt
    }

    var isDeleted: Bool { deletedAt != nil }

    /// Custom decoding to handle pre-existing JSON without newer fields.
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        title = try container.decode(String.self, forKey: .title)
        folderId = try container.decodeIfPresent(UUID.self, forKey: .folderId)
        pinned = try container.decode(Bool.self, forKey: .pinned)
        currentDrawingRevisionId = try container.decodeIfPresent(UUID.self, forKey: .currentDrawingRevisionId)
        currentRevision = try container.decodeIfPresent(Int.self, forKey: .currentRevision) ?? 0
        currentDrawingRevision = try container.decode(Int.self, forKey: .currentDrawingRevision)
        currentTextRevision = try container.decode(Int.self, forKey: .currentTextRevision)
        pageStyle = try container.decodeIfPresent(NotePageStyle.self, forKey: .pageStyle) ?? .linesMedium
        syncState = try container.decode(NoteSyncState.self, forKey: .syncState)
        createdAt = try container.decode(Date.self, forKey: .createdAt)
        updatedAt = try container.decode(Date.self, forKey: .updatedAt)
        deletedAt = try container.decodeIfPresent(Date.self, forKey: .deletedAt)
    }

    /// Days remaining before hard-delete (30-day soft-delete window).
    var daysUntilPurge: Int? {
        guard let deletedAt else { return nil }
        let elapsed = Calendar.current.dateComponents([.day], from: deletedAt, to: .now).day ?? 0
        return max(0, 30 - elapsed)
    }
}

// MARK: - Note Drawing Revision

/// An immutable snapshot of a PKDrawing at a point in time.
struct NoteDrawingRevision: Codable, Identifiable, Hashable, Sendable {
    let id: UUID
    let noteId: UUID
    let revision: Int
    let drawingData: Data
    let contentHash: String  // SHA-256 hex
    let canvasSize: CGSize
    let pageStyle: NotePageStyle
    let createdAt: Date
    let deviceId: String

    init(
        id: UUID = UUID(),
        noteId: UUID,
        revision: Int,
        drawingData: Data,
        contentHash: String,
        canvasSize: CGSize = NotePageStyle.letter.size,
        pageStyle: NotePageStyle = .letter,
        createdAt: Date = .now,
        deviceId: String
    ) {
        self.id = id
        self.noteId = noteId
        self.revision = revision
        self.drawingData = drawingData
        self.contentHash = contentHash
        self.canvasSize = canvasSize
        self.pageStyle = pageStyle
        self.createdAt = createdAt
        self.deviceId = deviceId
    }
}

// MARK: - Note Sync State

enum NoteSyncState: String, Codable, Sendable {
    case local           // exists only on device
    case syncing         // upload in progress
    case synced          // relay has the latest revision
    case syncFailed      // last upload failed; will retry
    case conflict        // relay has a different revision; needs resolution
}

// MARK: - Note Page Style

enum NotePageStyle: String, Codable, Sendable, CaseIterable {
    // Legacy cases — mapped to new equivalents on decode
    case letter    // → .linesMedium
    case a4        // → .linesMedium
    case blank

    // Ruled lines
    case linesSmall
    case linesMedium
    case linesLarge

    // Grid
    case gridSmall
    case gridMedium
    case gridLarge

    static var pickerCases: [NotePageStyle] {
        [.blank, .linesSmall, .linesMedium, .linesLarge, .gridSmall, .gridMedium, .gridLarge]
    }

    var displayName: String {
        switch self {
        case .letter:      return "Letter"
        case .a4:          return "A4"
        case .blank:       return "Blank"
        case .linesSmall:  return "Lines (Fine)"
        case .linesMedium: return "Lines (Medium)"
        case .linesLarge:  return "Lines (Wide)"
        case .gridSmall:   return "Grid (Fine)"
        case .gridMedium:  return "Grid (Medium)"
        case .gridLarge:   return "Grid (Wide)"
        }
    }

    var size: CGSize {
        switch self {
        case .a4: CGSize(width: 595, height: 842)
        default:  CGSize(width: 612, height: 792)
        }
    }

    var lineSpacing: CGFloat {
        switch self {
        case .linesSmall, .gridSmall:   return 18
        case .linesMedium, .gridMedium: return 24
        case .linesLarge, .gridLarge:   return 32
        case .a4:                       return 24
        default:                        return 0
        }
    }

    var showsRuledLines: Bool {
        switch self {
        case .linesSmall, .linesMedium, .linesLarge, .letter, .a4: return true
        default: return false
        }
    }

    var showsGrid: Bool {
        switch self {
        case .gridSmall, .gridMedium, .gridLarge: return true
        default: return false
        }
    }

    var showsMarginLine: Bool { showsRuledLines }
}

// MARK: - Note Folder

/// A lightweight folder for organizing notes. No sync in v1.
struct NoteFolder: Codable, Identifiable, Hashable, Sendable {
    let id: UUID
    var name: String
    var color: String?  // hex color for the folder icon
    let createdAt: Date

    init(
        id: UUID = UUID(),
        name: String,
        color: String? = nil,
        createdAt: Date = .now
    ) {
        self.id = id
        self.name = name
        self.color = color
        self.createdAt = createdAt
    }
}

// MARK: - Note Attachment

enum NoteAttachmentType: String, Codable, Sendable {
    case photo
    case scan
}

struct NoteAttachment: Codable, Identifiable, Hashable, Sendable {
    let id: UUID
    let noteId: UUID
    let type: NoteAttachmentType
    let fileName: String
    let mimeType: String
    let blobPath: String
    let contentHash: String
    let createdAt: Date

    init(
        id: UUID = UUID(),
        noteId: UUID,
        type: NoteAttachmentType,
        fileName: String,
        mimeType: String,
        blobPath: String,
        contentHash: String,
        createdAt: Date = .now
    ) {
        self.id = id
        self.noteId = noteId
        self.type = type
        self.fileName = fileName
        self.mimeType = mimeType
        self.blobPath = blobPath
        self.contentHash = contentHash
        self.createdAt = createdAt
    }
}
