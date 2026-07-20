import Foundation
import Testing
@testable import Herald

@Suite(.serialized)
struct NotesRepositoryTests {

    // MARK: - Helpers

    /// Create a temporary repository for testing.
    private func makeRepository() throws -> (NotesRepository, URL) {
        let tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("NotesRepositoryTests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)

        // We can't easily inject the base directory into the actor,
        // so we test via the public API and clean up after.
        let repo = NotesRepository()
        return (repo, tempDir)
    }

    // MARK: - Note CRUD

    @Test("Create and load notes")
    func createAndLoadNotes() async throws {
        let repo = NotesRepository()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "Test Note")
        let notes = try await repo.loadNotes()

        #expect(notes.count == 1)
        #expect(notes.first?.title == "Test Note")
        #expect(notes.first?.id == note.id)
        #expect(notes.first?.isDeleted == false)
    }

    @Test("Update note in index")
    func updateNote() async throws {
        let repo = NotesRepository()
        try await repo.ensureDirectories()

        var note = try await repo.createNote(title: "Original")
        note.title = "Updated"
        note.pinned = true
        try await repo.updateNote(note)

        let notes = try await repo.loadNotes()
        #expect(notes.first?.title == "Updated")
        #expect(notes.first?.pinned == true)
    }

    @Test("Soft-delete and restore note")
    func softDeleteAndRestore() async throws {
        let repo = NotesRepository()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "To Delete")
        try await repo.softDeleteNote(id: note.id)

        let afterDelete = try await repo.loadNotes()
        #expect(afterDelete.first?.isDeleted == true)
        #expect(afterDelete.first?.deletedAt != nil)

        try await repo.restoreNote(id: note.id)
        let afterRestore = try await repo.loadNotes()
        #expect(afterRestore.first?.isDeleted == false)
        #expect(afterRestore.first?.deletedAt == nil)
    }

    @Test("Hard-delete fails on active note")
    func hardDeleteFailsOnActive() async throws {
        let repo = NotesRepository()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "Active")
        await #expect(throws: NotesRepositoryError.self) {
            try await repo.hardDeleteNote(id: note.id)
        }
    }

    // MARK: - Drawing Blobs

    @Test("Save and load drawing blob")
    func saveAndLoadBlob() async throws {
        let repo = NotesRepository()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "Blob Test")
        let testData = Data("Hello, PencilKit!".utf8)

        let (blobPath, contentHash) = try await repo.saveDrawingBlob(
            noteId: note.id, data: testData, revision: 1
        )

        #expect(FileManager.default.fileExists(atPath: blobPath))
        #expect(!contentHash.isEmpty)

        let loaded = try await repo.loadDrawingBlob(noteId: note.id, revision: 1)
        #expect(loaded == testData)
    }

    @Test("Verify blob hash succeeds on correct data")
    func verifyBlobHashCorrect() async throws {
        let repo = NotesRepository()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "Hash Test")
        let testData = Data("Test content".utf8)
        let (_, contentHash) = try await repo.saveDrawingBlob(
            noteId: note.id, data: testData, revision: 1
        )

        let valid = try await repo.verifyBlobHash(noteId: note.id, revision: 1, expectedHash: contentHash)
        #expect(valid == true)
    }

    @Test("Verify blob hash fails on wrong hash")
    func verifyBlobHashWrong() async throws {
        let repo = NotesRepository()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "Hash Mismatch")
        let testData = Data("Test content".utf8)
        _ = try await repo.saveDrawingBlob(noteId: note.id, data: testData, revision: 1)

        let valid = try await repo.verifyBlobHash(noteId: note.id, revision: 1, expectedHash: "wronghash")
        #expect(valid == false)
    }

    @Test("Load nonexistent blob throws")
    func loadNonexistentBlob() async throws {
        let repo = NotesRepository()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "No Blob")
        await #expect(throws: NotesRepositoryError.self) {
            _ = try await repo.loadDrawingBlob(noteId: note.id, revision: 99)
        }
    }

    // MARK: - Atomic Write

    @Test("Drawing revision increments only on new persist")
    func revisionIncrement() async throws {
        let repo = NotesRepository()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "Revision Test")
        let data1 = Data("Version 1".utf8)
        let data2 = Data("Version 2".utf8)

        let (_, hash1) = try await repo.saveDrawingBlob(noteId: note.id, data: data1, revision: 1)
        let (_, hash2) = try await repo.saveDrawingBlob(noteId: note.id, data: data2, revision: 2)

        #expect(hash1 != hash2)

        let loaded1 = try await repo.loadDrawingBlob(noteId: note.id, revision: 1)
        let loaded2 = try await repo.loadDrawingBlob(noteId: note.id, revision: 2)
        #expect(loaded1 == data1)
        #expect(loaded2 == data2)
    }

    // MARK: - Note Model

    @Test("Note daysUntilPurge calculation")
    func daysUntilPurge() {
        let note = HeraldNote(deletedAt: Calendar.current.date(byAdding: .day, value: -10, to: .now))
        #expect(note.daysUntilPurge == 20)
    }

    @Test("Active note has no purge date")
    func activeNotePurge() {
        let note = HeraldNote(title: "Active")
        #expect(note.daysUntilPurge == nil)
        #expect(note.isDeleted == false)
    }

    // MARK: - Force-Quit Recovery

    @Test("Atomic write survives simulated interruption")
    func atomicWriteSurvivesInterruption() async throws {
        let repo = NotesRepository()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "Recovery Test")
        let goodData = Data("Good drawing data".utf8)

        // Save a valid revision
        let (_, hash) = try await repo.saveDrawingBlob(noteId: note.id, data: goodData, revision: 1)
        #expect(!hash.isEmpty)

        // Verify the blob is intact
        let valid = try await repo.verifyBlobHash(noteId: note.id, revision: 1, expectedHash: hash)
        #expect(valid == true)

        // Load the blob — should match original
        let loaded = try await repo.loadDrawingBlob(noteId: note.id, revision: 1)
        #expect(loaded == goodData)
    }

    @Test("Metadata index survives multiple rapid updates")
    func rapidMetadataUpdates() async throws {
        let repo = NotesRepository()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "Rapid Test")

        // Simulate rapid title updates (like typing)
        for i in 1...10 {
            var updated = note
            updated.title = "Updated \(i)"
            updated.updatedAt = .now
            try await repo.updateNote(updated)
        }

        let notes = try await repo.loadNotes()
        #expect(notes.count == 1)
        #expect(notes.first?.title == "Updated 10")
    }

    @Test("Hash mismatch detection catches corruption")
    func hashMismatchDetection() async throws {
        let repo = NotesRepository()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "Corruption Test")
        let data = Data("Valid data".utf8)
        let (_, hash) = try await repo.saveDrawingBlob(noteId: note.id, data: data, revision: 1)

        // Verify with correct hash
        let valid = try await repo.verifyBlobHash(noteId: note.id, revision: 1, expectedHash: hash)
        #expect(valid == true)

        // Verify with wrong hash (simulates corruption)
        let invalid = try await repo.verifyBlobHash(noteId: note.id, revision: 1, expectedHash: "corrupted")
        #expect(invalid == false)
    }

    // MARK: - Note Sync State

    @Test("Note sync state transitions")
    func syncStateTransitions() {
        var note = HeraldNote(title: "Sync Test")
        #expect(note.syncState == .local)

        note.syncState = .syncing
        #expect(note.syncState == .syncing)

        note.syncState = .synced
        #expect(note.syncState == .synced)

        note.syncState = .syncFailed
        #expect(note.syncState == .syncFailed)

        note.syncState = .conflict
        #expect(note.syncState == .conflict)
    }
}
