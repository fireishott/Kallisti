import Foundation
import os

/// Protocol for notes repository operations.
/// Enables testing by allowing mock implementations.
protocol NotesRepositoryProtocol: Sendable {
    func ensureDirectories() async throws
    func loadNotes() async throws -> [HeraldNote]
    func createNote(title: String, folderId: UUID?) async throws -> HeraldNote
    func updateNote(_ note: HeraldNote) async throws
    func softDeleteNote(id: UUID) async throws
    func restoreNote(id: UUID) async throws
    func saveDrawingBlob(noteId: UUID, data: Data, revision: Int, pageStyle: NotePageStyle) async throws -> (revisionId: UUID, blobPath: String, contentHash: String)
    func loadDrawingBlob(noteId: UUID, revision: Int) async throws -> Data
    func loadAttachments(noteId: UUID) async throws -> [NoteAttachment]
    func saveAttachmentBlob(noteId: UUID, data: Data, type: NoteAttachmentType, fileName: String, mimeType: String) async throws -> NoteAttachment
    func deleteAttachment(_ attachment: NoteAttachment) async throws
    func saveRecognition(_ recognition: NoteRecognition, noteId: UUID) async throws
}

/// Manages the notes list state and coordinates with the repository.
/// Injected via `AppContainer` like `ChatStore`.
@MainActor
@Observable
final class NotesStore {
    var notes: [HeraldNote] = []
    var folders: [NoteFolder] = []
    var selectedNoteId: UUID?
    var isLoading = false
    var errorMessage: String?

    private let repository: NotesRepositoryProtocol
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "notes-store")
    private let foldersKey = "com.herald.notes.folders"

    init(repository: NotesRepositoryProtocol = NotesRepository()) {
        self.repository = repository
        loadFolders()
    }
    
    // MARK: - Folder Persistence
    
    private func loadFolders() {
        guard let data = UserDefaults.standard.data(forKey: foldersKey) else { return }
        do {
            let decoder = JSONDecoder()
            decoder.dateDecodingStrategy = .iso8601
            folders = try decoder.decode([NoteFolder].self, from: data)
        } catch {
            logger.error("Failed to load folders: \(error.localizedDescription)")
        }
    }
    
    private func saveFolders() {
        do {
            let encoder = JSONEncoder()
            encoder.dateEncodingStrategy = .iso8601
            let data = try encoder.encode(folders)
            UserDefaults.standard.set(data, forKey: foldersKey)
        } catch {
            logger.error("Failed to save folders: \(error.localizedDescription)")
        }
    }
    
    func createFolder(name: String) -> NoteFolder {
        let folder = NoteFolder(name: name)
        folders.append(folder)
        saveFolders()
        return folder
    }
    
    func deleteFolder(id: UUID) {
        folders.removeAll { $0.id == id }
        saveFolders()
    }
    
    func noteCount(for folder: NoteFolder) -> Int {
        notes.filter { $0.folderId == folder.id && !$0.isDeleted }.count
    }

    // MARK: - Computed

    var activeNotes: [HeraldNote] {
        notes.filter { !$0.isDeleted }
            .sorted { ($0.pinned && !$1.pinned) || ($0.pinned == $1.pinned && $0.updatedAt > $1.updatedAt) }
    }

    var deletedNotes: [HeraldNote] {
        notes.filter { $0.isDeleted }
            .sorted { ($0.deletedAt ?? .distantPast) > ($1.deletedAt ?? .distantPast) }
    }

    var selectedNote: HeraldNote? {
        notes.first { $0.id == selectedNoteId }
    }
    
    var recentNotes: [HeraldNote] {
        notes.filter { !$0.isDeleted }
            .sorted { $0.updatedAt > $1.updatedAt }
            .prefix(10)
            .map { $0 }
    }

    // MARK: - Loading

    func loadNotes() async {
        isLoading = true
        errorMessage = nil
        do {
            try await repository.ensureDirectories()
            notes = try await repository.loadNotes()
        } catch {
            logger.error("Failed to load notes: \(error.localizedDescription)")
            errorMessage = error.localizedDescription
        }
        isLoading = false
    }

    // MARK: - CRUD

    func createNote(title: String = "") async -> HeraldNote? {
        do {
            let note = try await repository.createNote(title: title, folderId: nil)
            notes.append(note)
            selectedNoteId = note.id
            return note
        } catch {
            logger.error("Failed to create note: \(error.localizedDescription)")
            errorMessage = error.localizedDescription
            return nil
        }
    }

    func updateNote(_ note: HeraldNote) async {
        do {
            try await repository.updateNote(note)
            if let index = notes.firstIndex(where: { $0.id == note.id }) {
                notes[index] = note
            }
        } catch {
            logger.error("Failed to update note: \(error.localizedDescription)")
            errorMessage = error.localizedDescription
        }
    }

    func deleteNote(id: UUID) async {
        do {
            try await repository.softDeleteNote(id: id)
            if let index = notes.firstIndex(where: { $0.id == id }) {
                notes[index].deletedAt = .now
            }
            if selectedNoteId == id {
                selectedNoteId = nil
            }
        } catch {
            logger.error("Failed to delete note: \(error.localizedDescription)")
            errorMessage = error.localizedDescription
        }
    }

    func restoreNote(id: UUID) async {
        do {
            try await repository.restoreNote(id: id)
            if let index = notes.firstIndex(where: { $0.id == id }) {
                notes[index].deletedAt = nil
            }
        } catch {
            logger.error("Failed to restore note: \(error.localizedDescription)")
            errorMessage = error.localizedDescription
        }
    }

    func togglePin(id: UUID) async {
        guard let index = notes.firstIndex(where: { $0.id == id }) else { return }
        notes[index].pinned.toggle()
        notes[index].updatedAt = .now
        await updateNote(notes[index])
    }

    // MARK: - Drawing

    func saveDrawing(noteId: UUID, data: Data, revision: Int) async -> (revisionId: UUID, blobPath: String, contentHash: String)? {
        do {
            let result = try await repository.saveDrawingBlob(noteId: noteId, data: data, revision: revision, pageStyle: .linesMedium)
            // Update note's drawing revision and monotonic counter
            if let index = notes.firstIndex(where: { $0.id == noteId }) {
                notes[index].currentDrawingRevisionId = result.revisionId
                notes[index].currentDrawingRevision = revision
                notes[index].currentRevision = revision
                notes[index].updatedAt = .now
                try await repository.updateNote(notes[index])
            }
            return result
        } catch {
            logger.error("Failed to save drawing: \(error.localizedDescription)")
            errorMessage = error.localizedDescription
            return nil
        }
    }

    func loadDrawing(noteId: UUID, revision: Int) async -> Data? {
        do {
            return try await repository.loadDrawingBlob(noteId: noteId, revision: revision)
        } catch {
            logger.error("Failed to load drawing: \(error.localizedDescription)")
            return nil
        }
    }

    // MARK: - Attachments

    func loadAttachments(noteId: UUID) async -> [NoteAttachment] {
        do {
            return try await repository.loadAttachments(noteId: noteId)
        } catch {
            logger.error("Failed to load attachments: \(error.localizedDescription)")
            return []
        }
    }

    func saveAttachment(
        noteId: UUID,
        data: Data,
        type: NoteAttachmentType,
        fileName: String,
        mimeType: String
    ) async -> NoteAttachment? {
        do {
            let attachment = try await repository.saveAttachmentBlob(
                noteId: noteId,
                data: data,
                type: type,
                fileName: fileName,
                mimeType: mimeType
            )
            if let index = notes.firstIndex(where: { $0.id == noteId }) {
                notes[index].updatedAt = .now
                try await repository.updateNote(notes[index])
            }
            return attachment
        } catch {
            logger.error("Failed to save attachment: \(error.localizedDescription)")
            errorMessage = error.localizedDescription
            return nil
        }
    }

    func deleteAttachment(_ attachment: NoteAttachment) async {
        do {
            try await repository.deleteAttachment(attachment)
            if let index = notes.firstIndex(where: { $0.id == attachment.noteId }) {
                notes[index].updatedAt = .now
                try await repository.updateNote(notes[index])
            }
        } catch {
            logger.error("Failed to delete attachment: \(error.localizedDescription)")
            errorMessage = error.localizedDescription
        }
    }

    // MARK: - Selection

    func selectNote(_ id: UUID?) {
        selectedNoteId = id
    }

    // MARK: - Quick Notes (shared content)

    /// Create a note from text shared from another app via the Share Sheet or URL scheme.
    /// Returns `nil` if the text is empty.
    func createNoteFromSharedText(_ text: String, title: String?) async -> HeraldNote? {
        guard !text.isEmpty else { return nil }
        guard let note = await createNote(title: title ?? "Shared Note") else { return nil }

        let recognition = NoteRecognition(
            noteId: note.id,
            drawingRevisionId: note.id,
            engine: .userImported,
            rawText: text
        )
        do {
            try await repository.saveRecognition(recognition, noteId: note.id)
        } catch {
            logger.error("Failed to save shared text recognition: \(error.localizedDescription)")
        }
        return note
    }
}
