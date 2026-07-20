import CryptoKit
import Foundation
import os

/// Repository for note metadata and drawing blobs.
/// This is the ONLY writer for note data — all persistence flows through here.
/// Drawing blobs are atomic files in Application Support; metadata is JSON on disk.
actor NotesRepository {
    private let fileManager = FileManager.default
    private let notesDirectoryName = "Notes"
    private let metadataFileName = "notes-index.json"
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "notes-repository")

    private var baseDirectory: URL {
        fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Herald", isDirectory: true)
    }

    private var notesDirectory: URL {
        baseDirectory.appendingPathComponent(notesDirectoryName, isDirectory: true)
    }

    private var metadataURL: URL {
        notesDirectory.appendingPathComponent(metadataFileName)
    }

    // MARK: - Initialization

    func ensureDirectories() throws {
        try fileManager.createDirectory(at: notesDirectory, withIntermediateDirectories: true, attributes: nil)
    }

    // MARK: - Note CRUD

    /// Load all notes from the metadata index.
    func loadNotes() throws -> [HeraldNote] {
        guard fileManager.fileExists(atPath: metadataURL.path) else {
            return []
        }
        let data = try Data(contentsOf: metadataURL)
        return try JSONDecoder().decode([HeraldNote].self, from: data)
    }

    /// Save the full notes index. Atomic write.
    func saveNotes(_ notes: [HeraldNote]) throws {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let data = try encoder.encode(notes)
        try data.write(to: metadataURL, options: .atomic)
    }

    /// Create a new note and persist the updated index.
    func createNote(title: String = "", folderId: UUID? = nil) throws -> HeraldNote {
        let note = HeraldNote(title: title, folderId: folderId)
        var notes = try loadNotes()
        notes.append(note)
        try saveNotes(notes)
        return note
    }

    /// Update an existing note in the index.
    func updateNote(_ note: HeraldNote) throws {
        var notes = try loadNotes()
        guard let index = notes.firstIndex(where: { $0.id == note.id }) else {
            throw NotesRepositoryError.noteNotFound(note.id)
        }
        notes[index] = note
        try saveNotes(notes)
    }

    /// Soft-delete a note (sets deletedAt).
    func softDeleteNote(id: UUID) throws {
        var notes = try loadNotes()
        guard let index = notes.firstIndex(where: { $0.id == id }) else {
            throw NotesRepositoryError.noteNotFound(id)
        }
        notes[index].deletedAt = .now
        notes[index].syncState = .local
        try saveNotes(notes)
    }

    /// Restore a soft-deleted note.
    func restoreNote(id: UUID) throws {
        var notes = try loadNotes()
        guard let index = notes.firstIndex(where: { $0.id == id }) else {
            throw NotesRepositoryError.noteNotFound(id)
        }
        notes[index].deletedAt = nil
        try saveNotes(notes)
    }

    /// Hard-delete a note and its blob directory. Only after 30-day window.
    func hardDeleteNote(id: UUID) throws {
        var notes = try loadNotes()
        guard let index = notes.firstIndex(where: { $0.id == id }) else {
            throw NotesRepositoryError.noteNotFound(id)
        }
        let note = notes[index]
        guard note.isDeleted else {
            throw NotesRepositoryError.cannotHardDeleteActive(note.id)
        }
        // Remove blob directory
        let noteDir = noteDirectory(for: id)
        if fileManager.fileExists(atPath: noteDir.path) {
            try fileManager.removeItem(at: noteDir)
        }
        notes.remove(at: index)
        try saveNotes(notes)
    }

    /// Purge notes past the 30-day soft-delete window.
    func purgeExpiredNotes() throws {
        var notes = try loadNotes()
        let expired = notes.filter { note in
            guard let deletedAt = note.deletedAt else { return false }
            let elapsed = Calendar.current.dateComponents([.day], from: deletedAt, to: .now).day ?? 0
            return elapsed > 30
        }
        for note in expired {
            let noteDir = noteDirectory(for: note.id)
            if fileManager.fileExists(atPath: noteDir.path) {
                try fileManager.removeItem(at: noteDir)
            }
        }
        notes.removeAll { note in
            expired.contains(where: { $0.id == note.id })
        }
        try saveNotes(notes)
    }

    // MARK: - Drawing Blobs

    /// Save a PKDrawing blob for a note. Returns the revision number and content hash.
    /// Atomic write — crash yields prior or next complete revision, never a partial blob.
    func saveDrawingBlob(noteId: UUID, data: Data, revision: Int) throws -> (blobPath: String, contentHash: String) {
        let noteDir = noteDirectory(for: noteId)
        try fileManager.createDirectory(at: noteDir, withIntermediateDirectories: true, attributes: nil)

        let contentHash = SHA256.hash(data: data)
        let hashHex = contentHash.map { String(format: "%02x", $0) }.joined()

        let blobURL = noteDir.appendingPathComponent("rev-\(revision).pkdrawing")
        try data.write(to: blobURL, options: .atomic)

        return (blobURL.path, hashHex)
    }

    /// Load a PKDrawing blob from disk.
    func loadDrawingBlob(noteId: UUID, revision: Int) throws -> Data {
        let blobURL = noteDirectory(for: noteId).appendingPathComponent("rev-\(revision).pkdrawing")
        guard fileManager.fileExists(atPath: blobURL.path) else {
            throw NotesRepositoryError.blobNotFound(noteId, revision)
        }
        return try Data(contentsOf: blobURL)
    }

    /// Verify the content hash of a blob on disk.
    func verifyBlobHash(noteId: UUID, revision: Int, expectedHash: String) throws -> Bool {
        let data = try loadDrawingBlob(noteId: noteId, revision: revision)
        let actualHash = SHA256.hash(data: data)
        let hashHex = actualHash.map { String(format: "%02x", $0) }.joined()
        return hashHex == expectedHash
    }

    /// Delete a specific blob revision.
    func deleteBlob(noteId: UUID, revision: Int) throws {
        let blobURL = noteDirectory(for: noteId).appendingPathComponent("rev-\(revision).pkdrawing")
        if fileManager.fileExists(atPath: blobURL.path) {
            try fileManager.removeItem(at: blobURL)
        }
    }

    // MARK: - Helpers

    private func noteDirectory(for noteId: UUID) -> URL {
        notesDirectory.appendingPathComponent(noteId.uuidString, isDirectory: true)
    }
}

// MARK: - Errors

enum NotesRepositoryError: LocalizedError {
    case noteNotFound(UUID)
    case blobNotFound(UUID, Int)
    case cannotHardDeleteActive(UUID)

    var errorDescription: String? {
        switch self {
        case .noteNotFound(let id):
            return "Note not found: \(id)"
        case .blobNotFound(let id, let rev):
            return "Blob not found for note \(id) revision \(rev)"
        case .cannotHardDeleteActive(let id):
            return "Cannot hard-delete active note \(id); soft-delete first"
        }
    }
}
