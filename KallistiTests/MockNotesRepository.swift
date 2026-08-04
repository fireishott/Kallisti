import Foundation
@testable import Herald

/// Mock notes repository for testing.
actor MockNotesRepository: NotesRepositoryProtocol {
    var notes: [HeraldNote] = []
    var recognitions: [UUID: [NoteRecognition]] = [:]
    var error: Error?
    
    func setNotes(_ notes: [HeraldNote]) {
        self.notes = notes
    }
    
    func ensureDirectories() async throws {
        if let error { throw error }
    }
    
    func loadNotes() async throws -> [HeraldNote] {
        if let error { throw error }
        return notes
    }
    
    func createNote(title: String, folderId: UUID? = nil) async throws -> HeraldNote {
        if let error { throw error }
        let note = HeraldNote(title: title, folderId: folderId)
        notes.append(note)
        return note
    }
    
    func updateNote(_ note: HeraldNote) async throws {
        if let error { throw error }
        if let index = notes.firstIndex(where: { $0.id == note.id }) {
            notes[index] = note
        }
    }
    
    func softDeleteNote(id: UUID) async throws {
        if let error { throw error }
        if let index = notes.firstIndex(where: { $0.id == id }) {
            notes[index].deletedAt = .now
        }
    }
    
    func restoreNote(id: UUID) async throws {
        if let error { throw error }
        if let index = notes.firstIndex(where: { $0.id == id }) {
            notes[index].deletedAt = nil
        }
    }
    
    func saveDrawingBlob(noteId: UUID, data: Data, revision: Int, pageStyle: NotePageStyle = .linesMedium) async throws -> (revisionId: UUID, blobPath: String, contentHash: String) {
        if let error { throw error }
        return (UUID(), "/mock/path", "mockhash")
    }
    
    func loadDrawingBlob(noteId: UUID, revision: Int) async throws -> Data {
        if let error { throw error }
        return Data()
    }
    
    func loadAttachments(noteId: UUID) async throws -> [NoteAttachment] {
        if let error { throw error }
        return []
    }
    
    func saveAttachmentBlob(noteId: UUID, data: Data, type: NoteAttachmentType, fileName: String, mimeType: String) async throws -> NoteAttachment {
        if let error { throw error }
        return NoteAttachment(noteId: noteId, type: type, fileName: fileName, mimeType: mimeType, blobPath: "/mock/path", contentHash: "mockhash")
    }
    
    func deleteAttachment(_ attachment: NoteAttachment) async throws {
        if let error { throw error }
    }

    func saveRecognition(_ recognition: NoteRecognition, noteId: UUID) async throws {
        if let error { throw error }
        if recognitions[noteId] == nil {
            recognitions[noteId] = []
        }
        recognitions[noteId]?.append(recognition)
    }
}
