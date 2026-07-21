import Foundation
import os

/// Manages the notes list state and coordinates with the repository.
/// Injected via `AppContainer` like `ChatStore`.
@MainActor
@Observable
final class NotesStore {
    var notes: [HeraldNote] = []
    var selectedNoteId: UUID?
    var isLoading = false
    var errorMessage: String?

    private let repository: NotesRepository
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "notes-store")

    init(repository: NotesRepository = NotesRepository()) {
        self.repository = repository
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
            let note = try await repository.createNote(title: title)
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

    func saveDrawing(noteId: UUID, data: Data, revision: Int) async -> (String, String)? {
        do {
            let result = try await repository.saveDrawingBlob(noteId: noteId, data: data, revision: revision)
            // Update note's drawing revision
            if let index = notes.firstIndex(where: { $0.id == noteId }) {
                notes[index].currentDrawingRevision = revision
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
}
