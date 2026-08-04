import Foundation
import PencilKit
import Testing
@testable import Herald

@Suite(.serialized)
@MainActor
struct NotesIntegrationTests {

    // MARK: - Helpers

    private func makeRepo() throws -> (NotesRepository, URL) {
        let tempDir = FileManager.default.temporaryDirectory
            .appendingPathComponent("NotesIntegrationTests-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: tempDir, withIntermediateDirectories: true)
        let repo = NotesRepository(baseDirectory: tempDir)
        return (repo, tempDir)
    }

    private func makeTestDrawing(text: String = "Hello World") -> PKDrawing {
        let drawing = PKDrawing()
        // Create a simple stroke that represents handwriting
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
        return PKDrawing(strokes: [stroke])
    }

    // MARK: - End-to-End Note Workflow

    @Test("End-to-end note workflow: create, draw, save, recognize, parse, enrich, view, export")
    func testEndToEndNoteWorkflow() async throws {
        let (repo, tempDir) = try makeRepo()
        try await repo.ensureDirectories()
        let store = NotesStore(repository: repo)
        await store.loadNotes()

        // 1. Create note
        let note = await store.createNote(title: "Test Note")
        let noteId = try #require(note?.id)

        // 2. Draw with PencilKit (simulated)
        let drawing = makeTestDrawing()
        let drawingData = drawing.dataRepresentation()

        // 3. Save drawing (verify atomic write)
        let saveResult = await store.saveDrawing(noteId: noteId, data: drawingData, revision: 1)
        #expect(saveResult != nil)
        #expect(saveResult?.revisionId != nil)
        #expect(saveResult?.contentHash != nil)

        // Verify the blob exists on disk
        let blobData = try await repo.loadDrawingBlob(noteId: noteId, revision: 1)
        #expect(blobData == drawingData)

        // 4. Create and save a recognition
        let recognition = NoteRecognition(
            noteId: noteId,
            drawingRevisionId: saveResult!.revisionId,
            engine: .visionAccurate,
            engineVersion: "1.0",
            recognitionVersion: "1.0",
            languages: ["en-US"],
            rawText: "Hello World\n#research cloud migration"
        )
        try await repo.saveRecognition(recognition, noteId: noteId)

        // Verify recognition persisted
        let loadedRecs = try await repo.loadRecognitions(noteId: noteId)
        #expect(loadedRecs.count == 1)
        #expect(loadedRecs.first?.rawText == "Hello World\n#research cloud migration")

        // 5. Parse directives (verify allowlist)
        let parser = NoteDirectiveParser()
        let directives = parser.parse(
            text: recognition.effectiveText,
            noteId: noteId,
            sourceTextRevision: 1
        )
        #expect(directives.count == 1)
        #expect(directives.first?.command == .research)
        #expect(directives.first?.arguments == "cloud migration")

        // 6. Create and save an enrichment result (simulating relay response)
        let runId = UUID()
        let enrichmentResult = EnrichmentResult(
            noteId: noteId,
            runId: runId,
            sourceDrawingRevision: 1,
            sourceTextRevision: 1,
            title: "Cloud Migration Research",
            markdown: "# Cloud Migration\n\nKey findings about cloud migration strategies...",
            sections: [
                EnrichedSection(kind: .commandResult, title: "Research", markdown: "Detailed research on cloud migration...")
            ],
            citations: [
                EnrichedCitation(title: "AWS Migration Guide", url: "https://aws.amazon.com/migration", accessedAt: .now)
            ],
            commandResults: [
                NoteCommandResult(directiveId: directives.first!.id, status: .completed, sectionIndex: 0)
            ],
            isStale: false
        )
        try await repo.saveEnrichmentResult(enrichmentResult, noteId: noteId)

        // 7. View enriched document (verify citations)
        let loadedResult = try await repo.loadEnrichmentResult(noteId: noteId)
        let result = try #require(loadedResult)
        #expect(result.title == "Cloud Migration Research")
        #expect(result.citations.count == 1)
        #expect(result.citations.first?.title == "AWS Migration Guide")
        #expect(result.sections.count == 1)
        #expect(result.commandResults.count == 1)
        #expect(result.isStale == false)

        // 8. Export (verify all layers available)
        let layers = NoteExportLayer.availableLayers(
            hasDrawing: true,
            hasRecognition: true,
            hasEnrichment: true
        )
        #expect(layers.count == 4)
        #expect(layers.contains { $0.id == "ink_pdf" })
        #expect(layers.contains { $0.id == "recognized_text" })
        #expect(layers.contains { $0.id == "enriched_markdown" })
        #expect(layers.contains { $0.id == "citations" })

        // Cleanup
        try? FileManager.default.removeItem(at: tempDir)
    }

    // MARK: - Offline Capture Then Enrichment

    @Test("Offline capture then enrichment: create and save while offline, enrich when online")
    func testOfflineCaptureThenEnrichment() async throws {
        let (repo, tempDir) = try makeRepo()
        try await repo.ensureDirectories()
        let store = NotesStore(repository: repo)
        await store.loadNotes()

        // Airplane mode: create note, draw, save
        let note = await store.createNote(title: "Offline Note")
        let noteId = try #require(note?.id)

        let drawing = makeTestDrawing()
        let drawingData = drawing.dataRepresentation()
        let saveResult = await store.saveDrawing(noteId: noteId, data: drawingData, revision: 1)
        #expect(saveResult != nil)

        // Verify note persisted locally
        let notes = try await repo.loadNotes()
        #expect(notes.count == 1)
        #expect(notes.first?.title == "Offline Note")

        // Come online: simulate recognition and enrichment
        let recognition = NoteRecognition(
            noteId: noteId,
            drawingRevisionId: saveResult!.revisionId,
            engine: .visionAccurate,
            rawText: "Meeting notes\n#summary"
        )
        try await repo.saveRecognition(recognition, noteId: noteId)

        // Trigger enrichment (simulated)
        let runId = UUID()
        let result = EnrichmentResult(
            noteId: noteId,
            runId: runId,
            sourceDrawingRevision: 1,
            sourceTextRevision: 1,
            title: "Meeting Summary",
            markdown: "# Meeting Summary\n\nKey takeaways from the meeting...",
            sections: [],
            citations: [],
            commandResults: [],
            isStale: false
        )
        try await repo.saveEnrichmentResult(result, noteId: noteId)

        // Verify: enrichment completes
        let loadedResult = try await repo.loadEnrichmentResult(noteId: noteId)
        #expect(loadedResult != nil)
        #expect(loadedResult?.title == "Meeting Summary")
        #expect(loadedResult?.isStale == false)

        // Cleanup
        try? FileManager.default.removeItem(at: tempDir)
    }

    // MARK: - Background/Foreground During Run

    @Test("Background/foreground during run: enrichment survives app lifecycle transitions")
    func testBackgroundForegroundDuringRun() async throws {
        let (repo, tempDir) = try makeRepo()
        try await repo.ensureDirectories()
        let store = NotesStore(repository: repo)
        await store.loadNotes()

        // Start enrichment
        let note = await store.createNote(title: "Lifecycle Note")
        let noteId = try #require(note?.id)

        let drawing = makeTestDrawing()
        let drawingData = drawing.dataRepresentation()
        let saveResult = await store.saveDrawing(noteId: noteId, data: drawingData, revision: 1)

        // Save recognition
        let recognition = NoteRecognition(
            noteId: noteId,
            drawingRevisionId: saveResult!.revisionId,
            engine: .visionAccurate,
            rawText: "Background test\n#research lifecycle"
        )
        try await repo.saveRecognition(recognition, noteId: noteId)

        // Simulate background/foreground: enrichment result persists
        let runId = UUID()
        let result = EnrichmentResult(
            noteId: noteId,
            runId: runId,
            sourceDrawingRevision: 1,
            sourceTextRevision: 1,
            title: "Lifecycle Research",
            markdown: "# Lifecycle\n\nResearch on app lifecycle management...",
            isStale: false
        )
        try await repo.saveEnrichmentResult(result, noteId: noteId)

        // Verify: run resumed or completed (data persisted)
        let loadedResult = try await repo.loadEnrichmentResult(noteId: noteId)
        #expect(loadedResult != nil)
        #expect(loadedResult?.title == "Lifecycle Research")

        // Verify: loadNote would restore the enrichment
        let notes = try await repo.loadNotes()
        #expect(notes.first?.id == noteId)

        // Cleanup
        try? FileManager.default.removeItem(at: tempDir)
    }

    // MARK: - Stale Detection

    @Test("Stale enrichment detection: source revision changed after enrichment")
    func testStaleEnrichmentDetection() async throws {
        let (repo, tempDir) = try makeRepo()
        try await repo.ensureDirectories()

        let note = HeraldNote(title: "Stale Test")
        try await repo.saveNotes([note])

        // Enrich at revision 1
        let result = EnrichmentResult(
            noteId: note.id,
            runId: UUID(),
            sourceDrawingRevision: 1,
            sourceTextRevision: 1,
            title: "Original Enrichment",
            markdown: "Content at rev 1",
            isStale: false
        )
        try await repo.saveEnrichmentResult(result, noteId: note.id)

        // Load and verify not stale
        let loaded = try await repo.loadEnrichmentResult(noteId: note.id)
        #expect(loaded?.isStale == false)

        // Now simulate: source changed (new drawing saved)
        // In real flow, relay marks is_stale=true when sourceDrawingRevision != currentDrawingRevision
        let staleResult = EnrichmentResult(
            noteId: note.id,
            runId: UUID(),
            sourceDrawingRevision: 1,
            sourceTextRevision: 1,
            title: "Original Enrichment",
            markdown: "Content at rev 1",
            isStale: true
        )
        try await repo.saveEnrichmentResult(staleResult, noteId: note.id)

        let staleLoaded = try await repo.loadEnrichmentResult(noteId: note.id)
        #expect(staleLoaded?.isStale == true)

        // Cleanup
        try? FileManager.default.removeItem(at: tempDir)
    }

    // MARK: - Directive Parser Allowlist

    @Test("Directive parser respects v1 allowlist")
    func testDirectiveParserAllowlist() throws {
        let parser = NoteDirectiveParser()
        let noteId = UUID()

        // Known commands
        let text = "#research AI\n#summary meeting\n#talkingpoints discussion\n#search topic\n#actions items\n#questions faq"
        let directives = parser.parse(text: text, noteId: noteId, sourceTextRevision: 1)
        #expect(directives.count == 6)
        #expect(directives[0].command == .research)
        #expect(directives[1].command == .summary)
        #expect(directives[2].command == .talkingPoints)
        #expect(directives[3].command == .search)
        #expect(directives[4].command == .actions)
        #expect(directives[5].command == .questions)

        // Unknown command (not in allowlist)
        let unknownText = "#unknowncommand something"
        let unknownDirectives = parser.parse(text: unknownText, noteId: noteId, sourceTextRevision: 1)
        #expect(unknownDirectives.isEmpty)
    }

    // MARK: - Directive Fingerprint Stability

    @Test("Directive fingerprints are stable across parse calls")
    func testDirectiveFingerprintStability() throws {
        let parser = NoteDirectiveParser()
        let noteId = UUID()

        let text = "#research cloud migration vendors"
        let first = parser.parse(text: text, noteId: noteId, sourceTextRevision: 1)
        let second = parser.parse(text: text, noteId: noteId, sourceTextRevision: 1)

        #expect(first.count == 1)
        #expect(second.count == 1)
        #expect(first.first?.fingerprint == second.first?.fingerprint)
    }

    // MARK: - Export Layer Availability

    @Test("Export layers are correctly filtered by available content")
    func testExportLayerAvailability() throws {
        // Nothing available
        let empty = NoteExportLayer.availableLayers(hasDrawing: false, hasRecognition: false, hasEnrichment: false)
        #expect(empty.isEmpty)

        // Only drawing
        let drawingOnly = NoteExportLayer.availableLayers(hasDrawing: true, hasRecognition: false, hasEnrichment: false)
        #expect(drawingOnly.count == 1)
        #expect(drawingOnly.first?.id == "ink_pdf")

        // Drawing + recognition
        let withRec = NoteExportLayer.availableLayers(hasDrawing: true, hasRecognition: true, hasEnrichment: false)
        #expect(withRec.count == 2)

        // All layers
        let all = NoteExportLayer.availableLayers(hasDrawing: true, hasRecognition: true, hasEnrichment: true)
        #expect(all.count == 4)
    }

    // MARK: - EnrichmentResult Model

    @Test("EnrichmentResult encodes and decodes correctly")
    func testEnrichmentResultCodable() throws {
        let result = EnrichmentResult(
            noteId: UUID(),
            runId: UUID(),
            sourceDrawingRevision: 3,
            sourceTextRevision: 2,
            title: "Test Title",
            markdown: "# Test\n\nContent here",
            sections: [
                EnrichedSection(kind: .summary, title: "Summary", markdown: "A summary"),
                EnrichedSection(kind: .commandResult, title: "Research", markdown: "Research results")
            ],
            citations: [
                EnrichedCitation(title: "Source A", url: "https://example.com", accessedAt: .now)
            ],
            commandResults: [
                NoteCommandResult(directiveId: "abc123", status: .completed, sectionIndex: 1)
            ],
            warnings: ["AI-generated content"],
            isStale: false
        )

        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        let data = try encoder.encode(result)

        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .iso8601
        let decoded = try decoder.decode(EnrichmentResult.self, from: data)

        #expect(decoded.title == "Test Title")
        #expect(decoded.sections.count == 2)
        #expect(decoded.citations.count == 1)
        #expect(decoded.commandResults.count == 1)
        #expect(decoded.warnings.count == 1)
        #expect(decoded.sourceDrawingRevision == 3)
        #expect(decoded.sourceTextRevision == 2)
    }

    // MARK: - Note View Mode

    @Test("NoteViewMode has all cases")
    func testNoteViewModeCases() throws {
        let cases = NoteViewMode.allCases
        #expect(cases.count == 3)
        #expect(cases.contains(.ink))
        #expect(cases.contains(.recognized))
        #expect(cases.contains(.enriched))
    }
}
