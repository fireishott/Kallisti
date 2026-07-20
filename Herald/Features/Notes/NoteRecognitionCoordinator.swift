import Foundation
import os

/// Actor that manages recognition for a single open note.
/// Cancels stale recognition tasks and drops results whose sourceDrawingRevision no longer matches.
actor NoteRecognitionCoordinator {
    private let recognizer: HandwritingRecognizing
    private let repository: NotesRepository
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "note-recognition")

    /// The current recognition task (cancelled when a new one starts).
    private var currentTask: Task<NoteRecognition?, Never>?

    /// The drawing revision currently being recognized.
    private var recognizingRevision: Int?

    init(recognizer: HandwritingRecognizing = VisionHandwritingRecognizer(), repository: NotesRepository = NotesRepository()) {
        self.recognizer = recognizer
        self.repository = repository
    }

    /// Start recognition for a drawing revision. Cancels any in-progress recognition.
    /// Returns nil if the revision is stale or recognition fails.
    func recognize(
        noteId: UUID,
        drawingRevision: Int,
        languages: [String] = []
    ) async -> NoteRecognition? {
        // Cancel any existing recognition
        currentTask?.cancel()
        recognizingRevision = drawingRevision

        let task = Task<NoteRecognition?, Never> { [recognizer, repository, logger] in
            // Render the drawing to an image
            guard let drawingData = try? await repository.loadDrawingBlob(noteId: noteId, revision: drawingRevision) else {
                logger.error("Failed to load drawing blob for recognition")
                return nil
            }

            // Create PKDrawing and render to image
            guard let drawing = try? PKDrawing(data: drawingData) else {
                logger.error("Failed to deserialize PKDrawing")
                return nil
            }

            let image = drawing.image(from: drawing.bounds, scale: 2.0)
            guard let imageData = image.pngData() else {
                logger.error("Failed to render drawing to PNG")
                return nil
            }

            // Run recognition
            do {
                let candidates = try await recognizer.recognizeText(from: imageData, languages: languages)
                let rawText = candidates.map(\.text).joined(separator: "\n")

                // Check if revision is still current
                guard !Task.isCancelled else { return nil }

                return NoteRecognition(
                    noteId: noteId,
                    drawingRevision: drawingRevision,
                    engine: recognizer.engineId == "vn_fast" ? .visionFast : .visionAccurate,
                    engineVersion: recognizer.engineVersion,
                    languages: languages.isEmpty ? ["en-US"] : languages,
                    rawText: rawText
                )
            } catch {
                logger.error("Recognition failed: \(error.localizedDescription)")
                return nil
            }
        }

        currentTask = task
        return await task.value
    }

    /// Cancel any in-progress recognition.
    func cancel() {
        currentTask?.cancel()
        currentTask = nil
        recognizingRevision = nil
    }

    /// The revision currently being recognized (nil if idle).
    var activeRevision: Int? {
        recognizingRevision
    }
}

// MARK: - PKDrawing import

import PencilKit
