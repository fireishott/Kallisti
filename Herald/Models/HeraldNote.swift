import Foundation

// MARK: - Note

/// A Herald note — the top-level container for ink, recognition, and enrichment.
/// Metadata-only; drawing blobs live on disk as atomic `.pkdrawing` files.
struct HeraldNote: Codable, Identifiable, Hashable, Sendable {
    let id: UUID
    var title: String
    var folderId: UUID?
    var pinned: Bool
    var currentDrawingRevision: Int
    var currentTextRevision: Int
    var syncState: NoteSyncState
    let createdAt: Date
    var updatedAt: Date
    var deletedAt: Date?

    init(
        id: UUID = UUID(),
        title: String = "",
        folderId: UUID? = nil,
        pinned: Bool = false,
        currentDrawingRevision: Int = 0,
        currentTextRevision: Int = 0,
        syncState: NoteSyncState = .local,
        createdAt: Date = .now,
        updatedAt: Date = .now,
        deletedAt: Date? = nil
    ) {
        self.id = id
        self.title = title
        self.folderId = folderId
        self.pinned = pinned
        self.currentDrawingRevision = currentDrawingRevision
        self.currentTextRevision = currentTextRevision
        self.syncState = syncState
        self.createdAt = createdAt
        self.updatedAt = updatedAt
        self.deletedAt = deletedAt
    }

    var isDeleted: Bool { deletedAt != nil }

    /// Days remaining before hard-delete (30-day soft-delete window).
    var daysUntilPurge: Int? {
        guard let deletedAt else { return nil }
        let elapsed = Calendar.current.dateComponents([.day], from: deletedAt, to: .now).day ?? 0
        return max(0, 30 - elapsed)
    }
}

// MARK: - Note Drawing Revision

/// An immutable snapshot of a PKDrawing at a point in time.
/// The actual bytes live on disk at `blobPath`; this is the metadata record.
struct NoteDrawingRevision: Codable, Identifiable, Hashable, Sendable {
    let id: UUID
    let noteId: UUID
    let revision: Int
    let blobPath: String
    let contentHash: String  // SHA-256 hex
    let canvasSize: CGSize
    let pageStyle: NotePageStyle
    let createdAt: Date
    let deviceId: String

    init(
        id: UUID = UUID(),
        noteId: UUID,
        revision: Int,
        blobPath: String,
        contentHash: String,
        canvasSize: CGSize = NotePageStyle.letter.size,
        pageStyle: NotePageStyle = .letter,
        createdAt: Date = .now,
        deviceId: String
    ) {
        self.id = id
        self.noteId = noteId
        self.revision = revision
        self.blobPath = blobPath
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

enum NotePageStyle: String, Codable, Sendable {
    case letter
    case a4
    case blank

    var size: CGSize {
        switch self {
        case .letter: CGSize(width: 612, height: 792)  // US Letter at 72 PPI
        case .a4:     CGSize(width: 595, height: 842)  // A4 at 72 PPI
        case .blank:  CGSize(width: 612, height: 792)  // default to Letter
        }
    }
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
