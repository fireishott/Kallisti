import CryptoKit
import Foundation
import os

/// Repository for note metadata and drawing blobs.
/// This is the ONLY writer for note data — all persistence flows through here.
/// Drawing blobs are atomic files in Application Support; metadata is JSON on disk.
actor NotesRepository: NotesRepositoryProtocol {
    private let fileManager = FileManager.default
    private let notesDirectoryName = "Notes"
    private let metadataFileName = "notes-index.json"
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "notes-repository")
    private let customBaseDirectory: URL?

    private var baseDirectory: URL {
        customBaseDirectory ?? fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Herald", isDirectory: true)
    }

    init(baseDirectory: URL? = nil) {
        self.customBaseDirectory = baseDirectory
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
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode([HeraldNote].self, from: data)
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

    /// Save a PKDrawing blob for a note. Returns the revision UUID, file path, and content hash.
    /// Atomic write — crash yields prior or next complete revision, never a partial blob.
    /// Also records a `NoteDrawingRevision` metadata entry.
    @discardableResult
    func saveDrawingBlob(noteId: UUID, data: Data, revision: Int, pageStyle: NotePageStyle = .linesMedium) async throws -> (revisionId: UUID, blobPath: String, contentHash: String) {
        let noteDir = noteDirectory(for: noteId)
        try fileManager.createDirectory(at: noteDir, withIntermediateDirectories: true, attributes: nil)

        let contentHash = SHA256.hash(data: data)
        let hashHex = contentHash.map { String(format: "%02x", $0) }.joined()

        let blobURL = noteDir.appendingPathComponent("rev-\(revision).pkdrawing")
        try data.write(to: blobURL, options: .atomic)

        // Record the revision metadata
        let drawingRevision = NoteDrawingRevision(
            noteId: noteId,
            revision: revision,
            drawingData: data,
            contentHash: hashHex,
            pageStyle: pageStyle,
            deviceId: deviceIdentifier
        )
        try saveDrawingRevision(drawingRevision, noteId: noteId)

        return (drawingRevision.id, blobURL.path, hashHex)
    }

    /// Content-hash deduplication: skip write if the latest revision has the same hash.
    func saveDrawingBlobIfChanged(noteId: UUID, data: Data, revision: Int, pageStyle: NotePageStyle = .linesMedium) async throws -> (revisionId: UUID, blobPath: String, contentHash: String, changed: Bool) {
        let contentHash = SHA256.hash(data: data)
        let hashHex = contentHash.map { String(format: "%02x", $0) }.joined()

        // Check if the previous revision has the same hash
        if revision > 0 {
            let revisions = try loadDrawingRevisions(noteId: noteId)
            if let lastRev = revisions.last, lastRev.contentHash == hashHex {
                return (lastRev.id, "", hashHex, false)
            }
        }

        let result = try await saveDrawingBlob(noteId: noteId, data: data, revision: revision, pageStyle: pageStyle)
        return (result.revisionId, result.blobPath, result.contentHash, true)
    }

    /// Load a PKDrawing blob from disk.
    func loadDrawingBlob(noteId: UUID, revision: Int) async throws -> Data {
        let blobURL = noteDirectory(for: noteId).appendingPathComponent("rev-\(revision).pkdrawing")
        guard fileManager.fileExists(atPath: blobURL.path) else {
            throw NotesRepositoryError.blobNotFound(noteId, revision)
        }
        return try Data(contentsOf: blobURL)
    }

    /// Verify the content hash of a blob on disk.
    func verifyBlobHash(noteId: UUID, revision: Int, expectedHash: String) async throws -> Bool {
        let data = try await loadDrawingBlob(noteId: noteId, revision: revision)
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

    // MARK: - Drawing Revision Metadata

    private func revisionsMetadataURL(for noteId: UUID) -> URL {
        noteDirectory(for: noteId).appendingPathComponent("revisions.json")
    }

    /// Load all drawing revision records for a note.
    func loadDrawingRevisions(noteId: UUID) throws -> [NoteDrawingRevision] {
        let url = revisionsMetadataURL(for: noteId)
        guard fileManager.fileExists(atPath: url.path) else { return [] }
        let data = try Data(contentsOf: url)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode([NoteDrawingRevision].self, from: data)
    }

    /// Save a drawing revision metadata record.
    private func saveDrawingRevision(_ revision: NoteDrawingRevision, noteId: UUID) throws {
        var revisions = try loadDrawingRevisions(noteId: noteId)
        revisions.append(revision)
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let data = try encoder.encode(revisions)
        try data.write(to: revisionsMetadataURL(for: noteId), options: .atomic)
    }

    /// Get the current device identifier (stable per install).
    private var deviceIdentifier: String {
        if let id = UserDefaults.standard.string(forKey: "herald.deviceId") {
            return id
        }
        let id = UUID().uuidString
        UserDefaults.standard.set(id, forKey: "herald.deviceId")
        return id
    }

    // MARK: - Note Attachments

    private func attachmentsMetadataURL(for noteId: UUID) -> URL {
        noteDirectory(for: noteId).appendingPathComponent("attachments.json")
    }

    /// Load all attachments for a note.
    func loadAttachments(noteId: UUID) async throws -> [NoteAttachment] {
        let url = attachmentsMetadataURL(for: noteId)
        guard fileManager.fileExists(atPath: url.path) else { return [] }
        let data = try Data(contentsOf: url)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode([NoteAttachment].self, from: data)
    }

    /// Save attachment metadata index.
    func saveAttachments(_ attachments: [NoteAttachment], noteId: UUID) throws {
        let noteDir = noteDirectory(for: noteId)
        try fileManager.createDirectory(at: noteDir, withIntermediateDirectories: true, attributes: nil)
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let data = try encoder.encode(attachments)
        try data.write(to: attachmentsMetadataURL(for: noteId), options: .atomic)
    }

    /// Save an attachment blob and register it in the metadata index.
    func saveAttachmentBlob(
        noteId: UUID,
        data: Data,
        type: NoteAttachmentType,
        fileName: String,
        mimeType: String
    ) async throws -> NoteAttachment {
        let noteDir = noteDirectory(for: noteId)
        try fileManager.createDirectory(at: noteDir, withIntermediateDirectories: true, attributes: nil)

        let contentHash = SHA256.hash(data: data)
        let hashHex = contentHash.map { String(format: "%02x", $0) }.joined()

        let blobURL = noteDir.appendingPathComponent("att-\(UUID().uuidString.prefix(8))-\(fileName)")
        try data.write(to: blobURL, options: .atomic)

        let attachment = NoteAttachment(
            noteId: noteId,
            type: type,
            fileName: fileName,
            mimeType: mimeType,
            blobPath: blobURL.path,
            contentHash: hashHex
        )

        var attachments = try await loadAttachments(noteId: noteId)
        attachments.append(attachment)
        try saveAttachments(attachments, noteId: noteId)

        return attachment
    }

    /// Delete an attachment blob and remove it from the metadata index.
    func deleteAttachment(_ attachment: NoteAttachment) async throws {
        // Remove blob file
        let blobURL = URL(fileURLWithPath: attachment.blobPath)
        if fileManager.fileExists(atPath: blobURL.path) {
            try fileManager.removeItem(at: blobURL)
        }

        // Remove from metadata index
        var attachments = try await loadAttachments(noteId: attachment.noteId)
        attachments.removeAll { $0.id == attachment.id }
        try saveAttachments(attachments, noteId: attachment.noteId)
    }

    // MARK: - Recognitions

    private func recognitionsMetadataURL(for noteId: UUID) -> URL {
        noteDirectory(for: noteId).appendingPathComponent("recognitions.json")
    }

    /// Load all recognitions for a note.
    func loadRecognitions(noteId: UUID) throws -> [NoteRecognition] {
        let url = recognitionsMetadataURL(for: noteId)
        guard fileManager.fileExists(atPath: url.path) else { return [] }
        let data = try Data(contentsOf: url)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode([NoteRecognition].self, from: data)
    }

    /// Save a recognition record.
    func saveRecognition(_ recognition: NoteRecognition, noteId: UUID) throws {
        var recognitions = try loadRecognitions(noteId: noteId)
        recognitions.append(recognition)
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let data = try encoder.encode(recognitions)
        try data.write(to: recognitionsMetadataURL(for: noteId), options: .atomic)
    }

    // MARK: - Enrichment Results

    private func enrichmentResultURL(for noteId: UUID) -> URL {
        noteDirectory(for: noteId).appendingPathComponent("enrichment-result.json")
    }

    /// Load the latest enrichment result for a note.
    func loadEnrichmentResult(noteId: UUID) throws -> EnrichmentResult? {
        let url = enrichmentResultURL(for: noteId)
        guard fileManager.fileExists(atPath: url.path) else { return nil }
        let data = try Data(contentsOf: url)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(EnrichmentResult.self, from: data)
    }

    /// Save an enrichment result.
    func saveEnrichmentResult(_ result: EnrichmentResult, noteId: UUID) throws {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let data = try encoder.encode(result)
        try data.write(to: enrichmentResultURL(for: noteId), options: .atomic)
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
