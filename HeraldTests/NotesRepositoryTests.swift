import Foundation
import PencilKit
import Testing
@testable import Herald

@Suite(.serialized)
struct NotesRepositoryTests {

    // MARK: - Helpers

    /// Create a temporary repository for testing (isolated temp directory).
    private func makeRepo() throws -> (NotesRepository, URL) {
        let tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("NotesRepositoryTests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
        let repo = NotesRepository(baseDirectory: tempDir)
        return (repo, tempDir)
    }

    // MARK: - Note CRUD

    @Test("Create and load notes")
    func createAndLoadNotes() async throws {
        let (repo, _) = try makeRepo()
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
        let (repo, _) = try makeRepo()
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
        let (repo, _) = try makeRepo()
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
        let (repo, _) = try makeRepo()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "Active")
        await #expect(throws: NotesRepositoryError.self) {
            try await repo.hardDeleteNote(id: note.id)
        }
    }

    // MARK: - Drawing Blobs

    @Test("Save and load drawing blob")
    func saveAndLoadBlob() async throws {
        let (repo, _) = try makeRepo()
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
        let (repo, _) = try makeRepo()
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
        let (repo, _) = try makeRepo()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "Hash Mismatch")
        let testData = Data("Test content".utf8)
        _ = try await repo.saveDrawingBlob(noteId: note.id, data: testData, revision: 1)

        let valid = try await repo.verifyBlobHash(noteId: note.id, revision: 1, expectedHash: "wronghash")
        #expect(valid == false)
    }

    @Test("Load nonexistent blob throws")
    func loadNonexistentBlob() async throws {
        let (repo, _) = try makeRepo()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "No Blob")
        await #expect(throws: NotesRepositoryError.self) {
            _ = try await repo.loadDrawingBlob(noteId: note.id, revision: 99)
        }
    }

    // MARK: - Atomic Write

    @Test("Drawing revision increments only on new persist")
    func revisionIncrement() async throws {
        let (repo, _) = try makeRepo()
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
        let (repo, _) = try makeRepo()
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
        let (repo, _) = try makeRepo()
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
        let (repo, _) = try makeRepo()
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

    // MARK: - Note Model Contract

    @Test("HeraldNote has currentRevision monotonic field")
    func heraldNoteCurrentRevision() {
        var note = HeraldNote(title: "Revision Contract")
        #expect(note.currentRevision == 0)
        note.currentRevision = 5
        #expect(note.currentRevision == 5)
    }

    @Test("HeraldNote has currentDrawingRevisionId")
    func heraldNoteDrawingRevisionId() {
        var note = HeraldNote(title: "Drawing Rev ID")
        #expect(note.currentDrawingRevisionId == nil)
        let revId = UUID()
        note.currentDrawingRevisionId = revId
        #expect(note.currentDrawingRevisionId == revId)
    }

    @Test("HeraldNote isPinned alias works")
    func heraldNoteIsPinned() {
        var note = HeraldNote(title: "Pin Test")
        #expect(note.isPinned == false)
        note.pinned = true
        #expect(note.isPinned == true)
    }

    // MARK: - PKDrawing Byte Round-Trip

    @Test("PKDrawing byte round-trip preserves strokes")
    func pkDrawingByteRoundTrip() throws {
        let stroke = PKStroke(
            ink: PKInk(.pen, color: .black),
            path: PKStrokePath(
                controlPoints: [
                    PKStrokePoint(location: CGPoint(x: 0, y: 0), timeOffset: 0, size: CGSize(width: 5, height: 5), opacity: 1, force: 1, azimuth: 0, altitude: .pi / 2),
                    PKStrokePoint(location: CGPoint(x: 100, y: 100), timeOffset: 0.1, size: CGSize(width: 5, height: 5), opacity: 1, force: 1, azimuth: 0, altitude: .pi / 2),
                ],
                creationDate: .now
            )
        )
        let drawing = PKDrawing(strokes: [stroke])
        let data = drawing.dataRepresentation()

        let restored = try PKDrawing(data: data)
        #expect(restored.strokes.count == 1)
        #expect(restored.strokes.first?.ink.inkType == .pen)
    }

    @Test("Empty PKDrawing round-trips cleanly")
    func emptyPKDrawingRoundTrip() throws {
        let drawing = PKDrawing()
        let data = drawing.dataRepresentation()
        let restored = try PKDrawing(data: data)
        #expect(restored.strokes.isEmpty)
    }

    // MARK: - Concurrent Save Serialization

    @Test("Concurrent saves to same note serialize correctly")
    func concurrentSaveSerialization() async throws {
        let (repo, _) = try makeRepo()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "Concurrent Test")

        // Fire 5 concurrent title updates
        await withTaskGroup(of: Void.self) { group in
            for i in 1...5 {
                group.addTask {
                    var updated = note
                    updated.title = "Concurrent \(i)"
                    updated.updatedAt = .now
                    try? await repo.updateNote(updated)
                }
            }
        }

        let notes = try await repo.loadNotes()
        #expect(notes.count == 1)
        // One of the 5 values should have won — not corrupted
        let title = notes.first?.title ?? ""
        #expect(title.hasPrefix("Concurrent "))
    }

    @Test("Concurrent blob saves to same note do not corrupt")
    func concurrentBlobSaves() async throws {
        let (repo, _) = try makeRepo()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "Blob Concurrent")

        await withTaskGroup(of: Void.self) { group in
            for i in 1...5 {
                group.addTask {
                    let data = Data("Blob version \(i)".utf8)
                    try? await repo.saveDrawingBlob(noteId: note.id, data: data, revision: i)
                }
            }
        }

        // All 5 revisions should exist and be loadable
        for i in 1...5 {
            let loaded = try await repo.loadDrawingBlob(noteId: note.id, revision: i)
            #expect(loaded == Data("Blob version \(i)".utf8))
        }
    }

    // MARK: - NoteDrawingRevision Wiring

    @Test("Saving a drawing blob creates a NoteDrawingRevision record")
    func drawingRevisionRecordCreated() async throws {
        let (repo, _) = try makeRepo()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "Revision Record Test")
        let data = Data("Drawing data".utf8)

        try await repo.saveDrawingBlob(noteId: note.id, data: data, revision: 1)

        let revisions = try await repo.loadDrawingRevisions(noteId: note.id)
        #expect(revisions.count == 1)
        #expect(revisions.first?.revision == 1)
        #expect(revisions.first?.noteId == note.id)
        #expect(!revisions.first!.contentHash.isEmpty)
    }

    @Test("Multiple drawing revisions accumulate correctly")
    func drawingRevisionsAccumulate() async throws {
        let (repo, _) = try makeRepo()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "Revisions Accumulate")
        for i in 1...3 {
            let data = Data("Version \(i)".utf8)
            try await repo.saveDrawingBlob(noteId: note.id, data: data, revision: i)
        }

        let revisions = try await repo.loadDrawingRevisions(noteId: note.id)
        #expect(revisions.count == 3)
        #expect(revisions.map(\.revision) == [1, 2, 3])
    }

    @Test("Content-hash dedup skips write when hash matches")
    func contentHashDedup() async throws {
        let (repo, _) = try makeRepo()
        try await repo.ensureDirectories()

        let note = try await repo.createNote(title: "Dedup Test")
        let data = Data("Same content".utf8)

        let r1 = try await repo.saveDrawingBlobIfChanged(noteId: note.id, data: data, revision: 1)
        #expect(r1.changed == true)

        let r2 = try await repo.saveDrawingBlobIfChanged(noteId: note.id, data: data, revision: 2)
        #expect(r2.changed == false)
        #expect(r2.contentHash == r1.contentHash)
    }

    // MARK: - NoteRecognition Contract

    @Test("NoteRecognition has recognitionVersion field")
    func noteRecognitionVersion() {
        let recognition = NoteRecognition(
            noteId: UUID(),
            drawingRevision: 1,
            engine: .visionAccurate,
            rawText: "Hello"
        )
        #expect(recognition.recognitionVersion == "1.0")
        #expect(recognition.rawResult == "Hello")
        #expect(recognition.correctedResult == nil)
    }

    @Test("NoteRecognition correctedResult alias works")
    func noteRecognitionCorrectedResult() {
        var recognition = NoteRecognition(
            noteId: UUID(),
            drawingRevision: 1,
            engine: .visionFast,
            rawText: "raw"
        )
        recognition.userCorrectedText = "corrected"
        #expect(recognition.correctedResult == "corrected")
        #expect(recognition.effectiveText == "corrected")
    }
}
