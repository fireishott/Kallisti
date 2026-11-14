import Foundation
import PencilKit
import Testing
@testable import Kallisti

@Suite(.serialized)
struct NoteRecognitionCoordinatorTests {

    // MARK: - Helpers

    private func makeRepo() throws -> (NotesRepository, URL) {
        let tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("NoteRecognitionCoordinatorTests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
        let repo = NotesRepository(baseDirectory: tempDir)
        return (repo, tempDir)
    }

    /// Recognition renders the stored blob through PencilKit, so the blob must
    /// be a real serialized PKDrawing with at least one stroke.
    private static func makeDrawingData() -> Data {
        let stroke = PKStroke(
            ink: PKInk(.pen, color: .black),
            path: PKStrokePath(
                controlPoints: [
                    PKStrokePoint(location: CGPoint(x: 10, y: 10), timeOffset: 0, size: CGSize(width: 5, height: 5), opacity: 1, force: 1, azimuth: 0, altitude: 0),
                    PKStrokePoint(location: CGPoint(x: 100, y: 10), timeOffset: 0.1, size: CGSize(width: 5, height: 5), opacity: 1, force: 1, azimuth: 0, altitude: 0),
                ],
                creationDate: .now
            )
        )
        return PKDrawing(strokes: [stroke]).dataRepresentation()
    }

    // MARK: - Debounce

    @Test("Recognition fires after settled debounce, not for every call")
    func testRecognitionRunsAfterSettledDebounce() async throws {
        let recognizer = MockHandwritingRecognizer(
            results: [RecognizedTextCandidate(text: "hello", confidence: 0.9)]
        )
        let (repo, _) = try makeRepo()
        try await repo.ensureDirectories()
        let note = try await repo.createNote(title: "Debounce Test")
        let data = Self.makeDrawingData()
        try await repo.saveDrawingBlob(noteId: note.id, data: data, revision: 1, pageStyle: .linesMedium)

        let coordinator = NoteRecognitionCoordinator(
            recognizer: recognizer,
            repository: repo,
            debounceDuration: 0.5
        )

        // Fire 5 overlapping calls, 40ms apart (well inside the 0.5s debounce).
        // Each call supersedes the previous one; only the last should recognize.
        var results: [NoteRecognition?] = []
        await withTaskGroup(of: NoteRecognition?.self) { group in
            for index in 0..<5 {
                group.addTask {
                    try? await Task.sleep(nanoseconds: UInt64(index) * 40_000_000)
                    return await coordinator.scheduleRecognition(
                        noteId: note.id,
                        drawingRevision: 1,
                        languages: ["en-US"]
                    )
                }
            }
            for await result in group { results.append(result) }
        }

        // The first 4 should return nil (cancelled by debounce); the last one succeeds
        let nonNilResults = results.compactMap { $0 }
        #expect(nonNilResults.count == 1, "Only one recognition should fire after debounce settles")
        #expect(nonNilResults.first?.rawText == "hello")
    }

    // MARK: - Recognition Version Refresh

    @Test("Recognition is regenerated when recognitionVersion advances")
    func testRecognitionVersionRefresh() async throws {
        var recognizer = MockHandwritingRecognizer(
            recognitionVersion: "1.0",
            results: [RecognizedTextCandidate(text: "version one", confidence: 0.9)]
        )
        let (repo, _) = try makeRepo()
        try await repo.ensureDirectories()
        let note = try await repo.createNote(title: "Version Refresh Test")
        let data = Self.makeDrawingData()
        try await repo.saveDrawingBlob(noteId: note.id, data: data, revision: 1, pageStyle: .linesMedium)

        // First recognition with version 1.0
        let coordinator = NoteRecognitionCoordinator(
            recognizer: recognizer,
            repository: repo,
            debounceDuration: 0.0
        )
        let result1 = await coordinator.recognize(noteId: note.id, drawingRevision: 1, languages: ["en-US"])
        #expect(result1 != nil)
        #expect(result1?.recognitionVersion == "1.0")
        #expect(result1?.rawText == "version one")

        // Advance the version — simulates an engine update
        recognizer.recognitionVersion = "2.0"
        recognizer.results = [RecognizedTextCandidate(text: "version two", confidence: 0.95)]

        let coordinator2 = NoteRecognitionCoordinator(
            recognizer: recognizer,
            repository: repo,
            debounceDuration: 0.0
        )
        let result2 = await coordinator2.recognize(noteId: note.id, drawingRevision: 1, languages: ["en-US"])
        #expect(result2 != nil)
        #expect(result2?.recognitionVersion == "2.0", "Recognition should use the new version")
        #expect(result2?.rawText == "version two", "Recognition should re-run with updated engine")
    }
}
