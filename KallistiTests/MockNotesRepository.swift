import Foundation
@testable import Kallisti

/// Mock notes repository for testing.
actor MockNotesRepository: NotesRepositoryProtocol {
    var mockTypedText: String? = nil
    var notes: [KallistiNote] = []
    var recognitions: [UUID: [NoteRecognition]] = [:]
    var error: Error?
    
    func setNotes(_ notes: [KallistiNote]) {
        self.notes = notes
    }
    
    func ensureDirectories() async throws {
        if let error { throw error }
    }
    
    func loadNotes() async throws -> [KallistiNote] {
        if let error { throw error }
        return notes
    }
    
    func createNote(title: String, folderId: UUID? = nil) async throws -> KallistiNote {
        if let error { throw error }
        let note = KallistiNote(title: title, folderId: folderId)
        notes.append(note)
        return note
    }
    
    func updateNote(_ note: KallistiNote) async throws {
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
    
    func saveTypedTextBlob(noteId: UUID, text: String) async throws {
        if let error { throw error }
        mockTypedText = text
    }
    
    func loadTypedTextBlob(noteId: UUID) async throws -> String? {
        if let error { throw error }
        return mockTypedText
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

    func snapshotCurrentStateAsCheckpoint(noteId: UUID, reason: String) async throws -> NoteCheckpoint {
        if let error { throw error }
        return NoteCheckpoint(
            id: UUID(), noteId: noteId, reason: reason, createdAt: .now,
            drawingRevision: 0, drawingHash: "", textHash: "", title: "",
            attachmentsCount: 0, hadEnrichment: false
        )
    }

    func loadCheckpoints(noteId: UUID, limit: Int) async throws -> [NoteCheckpoint] {
        if let error { throw error }
        return []
    }

    func restoreCheckpoint(noteId: UUID, checkpointId: UUID) async throws -> NoteRestoreResult {
        if let error { throw error }
        return NoteRestoreResult(
            checkpointId: checkpointId, noteId: noteId, title: "", drawingData: nil,
            drawingRevision: 0, typedText: mockTypedText ?? "", attachments: [],
            enrichment: nil, pageStyle: .linesMedium
        )
    }

    func loadEnrichmentResultFromCheckpoint(checkpointId: UUID, noteId: UUID) async throws -> EnrichmentResult? {
        if let error { throw error }
        return nil
    }
}
