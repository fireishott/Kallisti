import Foundation

/// An immutable OCR recognition snapshot tied to a specific drawing revision.
struct NoteRecognition: Codable, Identifiable, Hashable, Sendable {
    let id: UUID
    let noteId: UUID
    let drawingRevision: Int
    let engine: RecognitionEngine
    let engineVersion: String?
    let languages: [String]  // ISO language codes
    let rawText: String
    var userCorrectedText: String?
    let createdAt: Date

    init(
        id: UUID = UUID(),
        noteId: UUID,
        drawingRevision: Int,
        engine: RecognitionEngine,
        engineVersion: String? = nil,
        languages: [String] = [],
        rawText: String,
        userCorrectedText: String? = nil,
        createdAt: Date = .now
    ) {
        self.id = id
        self.noteId = noteId
        self.drawingRevision = drawingRevision
        self.engine = engine
        self.engineVersion = engineVersion
        self.languages = languages
        self.rawText = rawText
        self.userCorrectedText = userCorrectedText
        self.createdAt = createdAt
    }

    /// The text to use for directive parsing — corrected if available, else raw.
    var effectiveText: String {
        userCorrectedText ?? rawText
    }
}

// MARK: - Recognition Engine

enum RecognitionEngine: String, Codable, Sendable {
    case visionAccurate  // VNRecognizeTextRequest with .accurate level
    case visionFast      // VNRecognizeTextRequest with .fast level
}
