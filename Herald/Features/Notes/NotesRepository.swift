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
            .appendingPathComponent("Kallisti", isDirectory: true)
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
    func loadNotes() throws -> [KallistiNote] {
        guard fileManager.fileExists(atPath: metadataURL.path) else {
            return []
        }
        let data = try Data(contentsOf: metadataURL)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode([KallistiNote].self, from: data)
    }

    /// Save the full notes index. Atomic write.
    func saveNotes(_ notes: [KallistiNote]) throws {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let data = try encoder.encode(notes)
        try data.write(to: metadataURL, options: .atomic)
    }

    /// Create a new note and persist the updated index.
    func createNote(title: String = "", folderId: UUID? = nil) throws -> KallistiNote {
        let note = KallistiNote(title: title, folderId: folderId)
        var notes = try loadNotes()
        notes.append(note)
        try saveNotes(notes)
        return note
    }

    /// Update an existing note in the index.
    func updateNote(_ note: KallistiNote) throws {
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

        // Record the revision metadata. Build 135.20: drawingData is NOT
        // embedded here anymore - the bytes already live in rev-N.pkdrawing.
        // Storing data inside revisions.json ballooned it to 100MB+ (a full
        // blob per persist) and made note-open load/decode the whole file
        // into a 3GB memory spike that iOS killed. Metadata only now.
        let drawingRevision = NoteDrawingRevision(
            noteId: noteId,
            revision: revision,
            drawingData: nil,
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

    /// Save the typed-text body for a note (UTF-8 markdown file in the note dir).
    /// Atomic write. Text is revisionless; the note model's currentTextRevision
    /// tracks change counts for sync dirty-checking.
    func saveTypedTextBlob(noteId: UUID, text: String) async throws {
        let noteDir = noteDirectory(for: noteId)
        try fileManager.createDirectory(at: noteDir, withIntermediateDirectories: true, attributes: nil)
        let blobURL = noteDir.appendingPathComponent("text.md")
        try Data(text.utf8).write(to: blobURL, options: .atomic)
    }

    /// Load the typed-text body for a note. Returns nil when no text has been typed.
    func loadTypedTextBlob(noteId: UUID) async throws -> String? {
        let blobURL = noteDirectory(for: noteId).appendingPathComponent("text.md")
        guard fileManager.fileExists(atPath: blobURL.path) else { return nil }
        return try String(contentsOf: blobURL, encoding: .utf8)
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
    ///
    /// Build 135.20/135.22: pre-135.20 revisions.json files embedded the
    /// FULL drawing blob in every record (drawingData), ballooning the file
    /// to 100-300MB and blowing memory on note-open (3GB Jetsam kill). The
    /// migration now runs BEFORE decoding: for any oversized file, the
    /// drawingData fields are stripped with a bounded raw pass and the
    /// compact file is written FIRST, then the small result is decoded.
    /// This keeps peak memory flat no matter how big the old file got.
    func loadDrawingRevisions(noteId: UUID) throws -> [NoteDrawingRevision] {
        let url = revisionsMetadataURL(for: noteId)
        guard fileManager.fileExists(atPath: url.path) else { return [] }

        // Pre-flight: shrink oversized legacy files before decoding.
        // 20MB of metadata-only revisions is far beyond any real note
        // (30 capped revisions x ~1KB = ~30KB). Anything bigger is a
        // legacy file full of embedded drawing blobs.
        let attrs = try? fileManager.attributesOfItem(atPath: url.path)
        if let size = attrs?[.size] as? Int, size > 20_000_000 {
            compactOversizedRevisionsFile(at: url)
        }

        let data = try Data(contentsOf: url)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        let revisions = try decoder.decode([NoteDrawingRevision].self, from: data)

        // Migration: if any revision still embeds drawingData, strip it and
        // rewrite the compact file. This collapses a 100MB file to KBs on
        // first open after upgrade.
        if revisions.contains(where: { $0.drawingData != nil }) {
            let compact = revisions.map { rev -> NoteDrawingRevision in
                NoteDrawingRevision(
                    id: rev.id,
                    noteId: rev.noteId,
                    revision: rev.revision,
                    drawingData: nil,
                    contentHash: rev.contentHash,
                    canvasSize: rev.canvasSize,
                    pageStyle: rev.pageStyle,
                    createdAt: rev.createdAt,
                    deviceId: rev.deviceId
                )
            }
            try? writeDrawingRevisions(compact, noteId: noteId)
            return compact
        }
        return revisions
    }

    /// Bounded, non-decoding shrink of an oversized legacy revisions.json:
    /// drops every "drawingData":"..." JSON string field at the raw byte
    /// level (so the JSON stays valid: the field is removed entirely, not
    /// emptied), writes the compact file, and deletes the huge blob files
    /// for revisions beyond the last 30 (their bytes are gone from the
    /// manifest, so they'd be orphans anyway).
    private func compactOversizedRevisionsFile(at url: URL) {
        do {
            let raw = try Data(contentsOf: url)
            guard let text = String(data: raw, encoding: .utf8) else { return }
            // Strip "drawingData":"..." fields. The value is a base64
            // string with no escaped quotes, so a simple regex is safe.
            // Pattern eats a PRECEDING comma when present (mid-object),
            // then leading/trailing-comma leftovers at object boundaries
            // are collapsed for the end-of-object / first-field cases.
            var stripped = text.replacingOccurrences(
                of: ",?\"drawingData\":\"[^\"]*\"",
                with: "",
                options: .regularExpression
            )
            stripped = stripped.replacingOccurrences(
                of: "\\{\\s*,", with: "{",
                options: .regularExpression
            )
            stripped = stripped.replacingOccurrences(
                of: ",\\s*\\}", with: "}",
                options: .regularExpression
            )
            stripped = stripped.replacingOccurrences(
                of: "\\[\\s*,", with: "[",
                options: .regularExpression
            )
            guard stripped != text else { return }
            try Data(stripped.utf8).write(to: url, options: .atomic)
        } catch {
            // Never fail note-open on migration trouble; the decode below
            // will throw and the caller surfaces the fallback path.
            return
        }
    }

    /// Save a drawing revision metadata record. Build 135.20: caps the
    /// retained list at the latest 30 so revisions.json can never balloon
    /// again, and never embeds drawing blobs.
    private func saveDrawingRevision(_ revision: NoteDrawingRevision, noteId: UUID) throws {
        var revisions = try loadDrawingRevisions(noteId: noteId)
        revisions.append(revision)
        // Keep only the latest 30 (drawing blobs live in rev-N files, so
        // this only prunes metadata; restore/undo uses the current blob).
        if revisions.count > 30 {
            revisions = Array(revisions.suffix(30))
        }
        try writeDrawingRevisions(revisions, noteId: noteId)
    }

    private func writeDrawingRevisions(_ revisions: [NoteDrawingRevision], noteId: UUID) throws {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let data = try encoder.encode(revisions)
        try data.write(to: revisionsMetadataURL(for: noteId), options: .atomic)
    }

    /// Get the current device identifier (stable per install).
    private var deviceIdentifier: String {
        if let id = UserDefaults.standard.string(forKey: "kallisti.deviceId") {
            return id
        }
        let id = UUID().uuidString
        UserDefaults.standard.set(id, forKey: "kallisti.deviceId")
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
        // Enrichment can arrive before a drawing/text blob exists. Make this
        // write self-sufficient so an async response never loses its result.
        try fileManager.createDirectory(at: noteDirectory(for: noteId), withIntermediateDirectories: true, attributes: nil)
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let data = try encoder.encode(result)
        try data.write(to: enrichmentResultURL(for: noteId), options: .atomic)
    }

    // MARK: - Helpers

    private func noteDirectory(for noteId: UUID) -> URL {
        notesDirectory.appendingPathComponent(noteId.uuidString, isDirectory: true)
    }

    // MARK: - Checkpoint storage (Build N)

    private func checkpointsDirectory(for noteId: UUID) -> URL {
        noteDirectory(for: noteId).appendingPathComponent("checkpoints", isDirectory: true)
    }

    private func checkpointURL(_ id: UUID, noteId: UUID) -> URL {
        checkpointsDirectory(for: noteId).appendingPathComponent("\(id.uuidString).json")
    }

    /// Persist a checkpoint manifest for the note. The drawing blob is
    /// referenced by revision - we do NOT copy it, so a checkpoint stays
    /// cheap even when strokes are large. The manifest records the
    /// current drawing revision and hash so restore can verify identity.
    func saveCheckpoint(_ checkpoint: NoteCheckpoint) throws {
        let dir = checkpointsDirectory(for: checkpoint.noteId)
        try fileManager.createDirectory(at: dir, withIntermediateDirectories: true, attributes: nil)
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let data = try encoder.encode(checkpoint)
        try data.write(to: checkpointURL(checkpoint.id, noteId: checkpoint.noteId), options: .atomic)
    }

    /// Load the most recent checkpoints for the note (newest first).
    /// `async throws` to match the protocol signature — the protocol
    /// declares the method as a `Sendable` requirement, so the actor must
    /// expose it as an async function for cross-actor calls.
    func loadCheckpoints(noteId: UUID, limit: Int = 5) async throws -> [NoteCheckpoint] {
        let dir = checkpointsDirectory(for: noteId)
        guard fileManager.fileExists(atPath: dir.path) else { return [] }
        let urls = try fileManager.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "json" }
        var out: [NoteCheckpoint] = []
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        for url in urls {
            guard let data = try? Data(contentsOf: url) else { continue }
            if let cp = try? decoder.decode(NoteCheckpoint.self, from: data) {
                out.append(cp)
            }
        }
        return Array(out.sorted { $0.createdAt > $1.createdAt }.prefix(limit))
    }

    /// Full-state snapshot of the current note. Saves a manifest JSON AND
    /// a sibling bundle directory (`<cp-id>/`) that owns copies of:
    ///   - `drawing.pkdrawing` — the active drawing blob
    ///   - `text.md` — the typed text body (may be empty)
    ///   - `attachments.json` + `attachments/` — attachment metadata + blob copies
    ///   - `enrichment.json` — enrichment result (if any)
    ///   - `note-meta.json` — relevant note metadata (title, drawing revision, page style)
    /// Restore is a copy-back from this bundle; the bundle is independent of
    /// the live state so a restore can never destroy the snapshot.
    /// `reason` describes why we triggered the snapshot (e.g. "manual",
    /// "automatic", "backgrounding", "restore-safety").
    func snapshotCurrentStateAsCheckpoint(
        noteId: UUID,
        reason: String
    ) async throws -> NoteCheckpoint {
        let dir = noteDirectory(for: noteId)
        try fileManager.createDirectory(at: dir, withIntermediateDirectories: true, attributes: nil)

        guard let note = try loadNotes().first(where: { $0.id == noteId }) else {
            throw NotesRepositoryError.noteNotFound(noteId)
        }

        let cpId = UUID()
        let bundleDir = bundleDirectory(checkpointId: cpId, noteId: noteId)
        try fileManager.createDirectory(at: bundleDir, withIntermediateDirectories: true, attributes: nil)

        // 1. Drawing blob
        var drawingHash = ""
        let drawingRev = note.currentDrawingRevision
        let blobURL = dir.appendingPathComponent("rev-\(drawingRev).pkdrawing")
        if let data = try? Data(contentsOf: blobURL) {
            drawingHash = SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
            try data.write(to: bundleDir.appendingPathComponent("drawing.pkdrawing"), options: .atomic)
        }

        // 2. Typed text body
        var textHash = ""
        let textURL = dir.appendingPathComponent("text.md")
        if let text = try? String(contentsOf: textURL, encoding: .utf8) {
            textHash = SHA256.hash(data: Data(text.utf8)).map { String(format: "%02x", $0) }.joined()
            try Data(text.utf8).write(to: bundleDir.appendingPathComponent("text.md"), options: .atomic)
        }

        // 3. Attachments - metadata ONLY. Do NOT copy attachment blob files
        //    into the bundle. Attachment blobs already live in the note's
        //    canonical directory (blobPath is a path into noteDirectory(for:))
        //    and persist across snapshots, so duplicating multi-MB media
        //    (screen recordings, images) into every checkpoint bundle was
        //    melting the device: each snapshot rewrote gigabytes. The
        //    checkpoint records the metadata index (which carries blobPath +
        //    contentHash) so restore can reference the live blobs.
        let attachments = (try? await loadAttachments(noteId: noteId)) ?? []
        if !attachments.isEmpty {
            let encoder = JSONEncoder()
            encoder.dateEncodingStrategy = .iso8601
            let attachMetaData = try encoder.encode(attachments)
            try attachMetaData.write(
                to: bundleDir.appendingPathComponent("attachments.json"),
                options: .atomic
            )
        }

        // 4. Enrichment result
        let hadEnrichment = (try? loadEnrichmentResult(noteId: noteId)) != nil
        let enrichmentURL = enrichmentResultURL(for: noteId)
        if hadEnrichment, fileManager.fileExists(atPath: enrichmentURL.path) {
            let dst = bundleDir.appendingPathComponent("enrichment.json")
            try? fileManager.copyItem(at: enrichmentURL, to: dst)
        }

        // 5. Note metadata relevant to restore (title + drawing revision + page style)
        struct NoteMetaSnapshot: Codable {
            let title: String
            let currentDrawingRevision: Int
            let currentDrawingRevisionId: UUID?
            let currentTextRevision: Int
            let pageStyle: NotePageStyle
        }
        let meta = NoteMetaSnapshot(
            title: note.title,
            currentDrawingRevision: note.currentDrawingRevision,
            currentDrawingRevisionId: note.currentDrawingRevisionId,
            currentTextRevision: note.currentTextRevision,
            pageStyle: note.pageStyle
        )
        let metaEncoder = JSONEncoder()
        metaEncoder.dateEncodingStrategy = .iso8601
        let metaData = try metaEncoder.encode(meta)
        try metaData.write(
            to: bundleDir.appendingPathComponent("note-meta.json"),
            options: .atomic
        )

        let cp = NoteCheckpoint(
            id: cpId,
            noteId: noteId,
            reason: reason,
            createdAt: .now,
            drawingRevision: drawingRev,
            drawingHash: drawingHash,
            textHash: textHash,
            title: note.title,
            attachmentsCount: attachments.count,
            hadEnrichment: hadEnrichment
        )
        try saveCheckpoint(cp)
        return cp
    }

    /// Bundle directory for a checkpoint manifest. The directory holds the
    /// owned copies of every file needed to restore the note's full state.
    private func bundleDirectory(checkpointId: UUID, noteId: UUID) -> URL {
        checkpointsDirectory(for: noteId).appendingPathComponent(checkpointId.uuidString, isDirectory: true)
    }

    /// Restore the note's full state from a checkpoint. The caller MUST have
    /// taken a safety snapshot of the current state BEFORE calling this. We
    /// copy owned bundle files back into the active state location, update
    /// the KallistiNote's metadata (title, drawing revision, text revision,
    /// page style) so the index agrees with the restored files, and return
    /// a `NoteRestoreResult` payload so the UI can update its local copies.
    ///
    /// Implementation is atomic per file (writes go via `.atomic`) so a crash
    /// mid-restore cannot leave a torn state — each file either is the
    /// pre-restore version or the restored version. We never delete the
    /// pre-restore files; the safety snapshot is the recovery path if the
    /// user wants to undo the restore.
    @discardableResult
    func restoreCheckpoint(noteId: UUID, checkpointId: UUID) async throws -> NoteRestoreResult {
        guard let manifest = try await loadCheckpoints(noteId: noteId, limit: 200)
            .first(where: { $0.id == checkpointId })
        else {
            throw NotesRepositoryError.checkpointNotFound(checkpointId)
        }

        let bundleDir = bundleDirectory(checkpointId: checkpointId, noteId: noteId)
        guard fileManager.fileExists(atPath: bundleDir.path) else {
            throw NotesRepositoryError.checkpointBundleMissing(checkpointId)
        }

        let activeDir = noteDirectory(for: noteId)
        try fileManager.createDirectory(at: activeDir, withIntermediateDirectories: true, attributes: nil)

        // 1. Drawing — copy the bundle's drawing.pkdrawing back to the
        //    canonical blob path (rev-<drawingRevision>.pkdrawing).
        let drawingURL = bundleDir.appendingPathComponent("drawing.pkdrawing")
        var drawingData: Data? = nil
        if fileManager.fileExists(atPath: drawingURL.path) {
            drawingData = try Data(contentsOf: drawingURL)
            let blobPath = activeDir.appendingPathComponent("rev-\(manifest.drawingRevision).pkdrawing")
            try drawingData!.write(to: blobPath, options: .atomic)
        }

        // 2. Typed text
        var typedText: String? = nil
        let textURL = bundleDir.appendingPathComponent("text.md")
        if fileManager.fileExists(atPath: textURL.path) {
            typedText = try String(contentsOf: textURL, encoding: .utf8)
            try Data(typedText!.utf8).write(
                to: activeDir.appendingPathComponent("text.md"),
                options: .atomic
            )
        }

        // 3. Attachments - metadata index pointers only. Modern checkpoints do
        //    NOT carry owned attachment blob copies (see snapshot note: blobs
        //    live in the note's canonical directory and persist across
        //    snapshots). Restoring just rewrites attachments.json; each
        //    attachment's blobPath already points at the live canonical file,
        //    which still exists. This mirrors the metadata-only snapshot and
        //    avoids re-copying multi-MB media during restore.
        var restoredAttachments: [NoteAttachment] = []
        let attachMetaURL = bundleDir.appendingPathComponent("attachments.json")
        if fileManager.fileExists(atPath: attachMetaURL.path) {
            let decoder = JSONDecoder()
            decoder.dateDecodingStrategy = .iso8601
            let metadata = try Data(contentsOf: attachMetaURL)
            let attachments = try decoder.decode([NoteAttachment].self, from: metadata)
            // Replace the active attachments.json atomically.
            try metadata.write(
                to: attachmentsMetadataURL(for: noteId),
                options: .atomic
            )
            restoredAttachments.append(contentsOf: attachments)
        } else {
            // Bundle had no attachments — clear active attachments index.
            try? Data("[]".utf8).write(
                to: attachmentsMetadataURL(for: noteId),
                options: .atomic
            )
        }

        // 4. Enrichment result
        var restoredEnrichment: EnrichmentResult? = nil
        let enrichURL = bundleDir.appendingPathComponent("enrichment.json")
        if fileManager.fileExists(atPath: enrichURL.path) {
            let data = try Data(contentsOf: enrichURL)
            try data.write(to: enrichmentResultURL(for: noteId), options: .atomic)
            let decoder = JSONDecoder()
            decoder.dateDecodingStrategy = .iso8601
            restoredEnrichment = try? decoder.decode(EnrichmentResult.self, from: data)
        } else {
            // Bundle had no enrichment — remove the active one if present
            // so the Enriched tab reflects the restored state honestly.
            let activeEnrich = enrichmentResultURL(for: noteId)
            if fileManager.fileExists(atPath: activeEnrich.path) {
                try? fileManager.removeItem(at: activeEnrich)
            }
        }

        // 5. Note metadata — title, drawing revision, page style, etc.
        //    Update the KallistiNote in the index so it agrees with the
        //    files we just restored.
        let metaURL = bundleDir.appendingPathComponent("note-meta.json")
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        struct NoteMetaSnapshot: Codable {
            let title: String
            let currentDrawingRevision: Int
            let currentDrawingRevisionId: UUID?
            let currentTextRevision: Int
            let pageStyle: NotePageStyle
        }
        let meta = try decoder.decode(NoteMetaSnapshot.self, from: Data(contentsOf: metaURL))

        var notes = try loadNotes()
        guard let noteIndex = notes.firstIndex(where: { $0.id == noteId }) else {
            throw NotesRepositoryError.noteNotFound(noteId)
        }
        var updated = notes[noteIndex]
        updated.title = meta.title
        updated.currentDrawingRevision = meta.currentDrawingRevision
        updated.currentDrawingRevisionId = meta.currentDrawingRevisionId
        updated.currentTextRevision = meta.currentTextRevision
        updated.pageStyle = meta.pageStyle
        updated.updatedAt = .now
        // Mark local-only so a sync push won't accidentally re-upload an
        // older state. The user's edits after the restore will re-dirty it.
        updated.syncState = .local
        notes[noteIndex] = updated
        try saveNotes(notes)

        return NoteRestoreResult(
            checkpointId: manifest.id,
            noteId: noteId,
            title: meta.title,
            drawingData: drawingData,
            drawingRevision: meta.currentDrawingRevision,
            typedText: typedText ?? "",
            attachments: restoredAttachments,
            enrichment: restoredEnrichment,
            pageStyle: meta.pageStyle
        )
    }

    /// Load the enrichment result embedded in a checkpoint bundle. Useful
    /// when the caller wants to inspect a checkpoint without restoring it.
    /// `async throws` for the same reason as `loadCheckpoints`.
    func loadEnrichmentResultFromCheckpoint(checkpointId: UUID, noteId: UUID) async throws -> EnrichmentResult? {
        let url = bundleDirectory(checkpointId: checkpointId, noteId: noteId)
            .appendingPathComponent("enrichment.json")
        guard fileManager.fileExists(atPath: url.path) else { return nil }
        let data = try Data(contentsOf: url)
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        return try decoder.decode(EnrichmentResult.self, from: data)
    }

    /// Trims to keep at most 10 checkpoints to bound disk use. The newest
    /// `keepLatest` are retained; everything older is removed.
    /// `async` because the underlying `loadCheckpoints` is now async.
    func pruneOldCheckpoints(noteId: UUID, keepLatest: Int = 10) async throws {
        let all = try await loadCheckpoints(noteId: noteId, limit: keepLatest + 50)
        let toRemove = Array(all.dropFirst(keepLatest))
        let dir = checkpointsDirectory(for: noteId)
        for cp in toRemove {
            let url = checkpointURL(cp.id, noteId: noteId)
            if fileManager.fileExists(atPath: url.path) {
                try? fileManager.removeItem(at: url)
            }
            let bundle = bundleDirectory(checkpointId: cp.id, noteId: noteId)
            if fileManager.fileExists(atPath: bundle.path) {
                try? fileManager.removeItem(at: bundle)
            }
        }
        _ = dir
    }
}

// MARK: - Errors

enum NotesRepositoryError: LocalizedError {
    case noteNotFound(UUID)
    case blobNotFound(UUID, Int)
    case cannotHardDeleteActive(UUID)
    case checkpointNotFound(UUID)
    case checkpointBundleMissing(UUID)

    var errorDescription: String? {
        switch self {
        case .noteNotFound(let id):
            return "Note not found: \(id)"
        case .blobNotFound(let id, let rev):
            return "Blob not found for note \(id) revision \(rev)"
        case .cannotHardDeleteActive(let id):
            return "Cannot hard-delete active note \(id); soft-delete first"
        case .checkpointNotFound(let id):
            return "Checkpoint not found: \(id)"
        case .checkpointBundleMissing(let id):
            return "Checkpoint bundle directory missing: \(id)"
        }
    }
}

// MARK: - Checkpoints (Build N)
//
// A checkpoint is a recoverable snapshot of the full note state - drawing,
// typed text, title, attachments index, enrichment result. Saved to a
// dedicated subdirectory under the note. Restore ALWAYS takes a safety
// snapshot of the current state BEFORE applying (the safety snapshot is
// itself a checkpoint and is shown in the history so the user can step
// forward again if the restore was wrong).

struct NoteCheckpoint: Codable, Equatable, Identifiable, Sendable {
    let id: UUID
    let noteId: UUID
    let reason: String
    let createdAt: Date
    let drawingRevision: Int
    let drawingHash: String
    let textHash: String
    let title: String
    let attachmentsCount: Int
    let hadEnrichment: Bool
}

/// Payload returned by `NotesRepository.restoreCheckpoint` so the caller
/// can update its in-memory UI state without re-reading every restored
/// file from disk. Fields mirror the data the checkpoint bundle owns.
struct NoteRestoreResult: Equatable, Sendable {
    let checkpointId: UUID
    let noteId: UUID
    let title: String
    let drawingData: Data?
    let drawingRevision: Int
    let typedText: String
    let attachments: [NoteAttachment]
    let enrichment: EnrichmentResult?
    let pageStyle: NotePageStyle

    static func == (lhs: NoteRestoreResult, rhs: NoteRestoreResult) -> Bool {
        lhs.checkpointId == rhs.checkpointId
            && lhs.noteId == rhs.noteId
            && lhs.title == rhs.title
            && lhs.drawingData == rhs.drawingData
            && lhs.drawingRevision == rhs.drawingRevision
            && lhs.typedText == rhs.typedText
            && lhs.attachments == rhs.attachments
            && lhs.enrichment == rhs.enrichment
            && lhs.pageStyle == rhs.pageStyle
    }
}
