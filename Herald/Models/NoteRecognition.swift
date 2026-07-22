import Foundation

/// An immutable OCR recognition snapshot tied to a specific drawing revision.
struct NoteRecognition: Codable, Identifiable, Hashable, Sendable {
    let id: UUID
    let noteId: UUID
    let drawingRevisionId: UUID
    let engine: RecognitionEngine
    let engineVersion: String?
    let recognitionVersion: String
    let languages: [String]  // ISO language codes
    let rawText: String
    var userCorrectedText: String?
    let createdAt: Date

    // Contract aliases — `rawResult`/`correctedResult` match the wire spec.
    var rawResult: String { rawText }
    var correctedResult: String? { userCorrectedText }

    init(
        id: UUID = UUID(),
        noteId: UUID,
        drawingRevisionId: UUID,
        engine: RecognitionEngine,
        engineVersion: String? = nil,
        recognitionVersion: String = "1.0",
        languages: [String] = [],
        rawText: String,
        userCorrectedText: String? = nil,
        createdAt: Date = .now
    ) {
        self.id = id
        self.noteId = noteId
        self.drawingRevisionId = drawingRevisionId
        self.engine = engine
        self.engineVersion = engineVersion
        self.recognitionVersion = recognitionVersion
        self.languages = languages
        self.rawText = rawText
        self.userCorrectedText = userCorrectedText
        self.createdAt = createdAt
    }

    /// Custom decoding to handle pre-existing JSON without `recognitionVersion`.
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(UUID.self, forKey: .id)
        noteId = try container.decode(UUID.self, forKey: .noteId)
        drawingRevisionId = try container.decode(UUID.self, forKey: .drawingRevisionId)
        engine = try container.decode(RecognitionEngine.self, forKey: .engine)
        engineVersion = try container.decodeIfPresent(String.self, forKey: .engineVersion)
        recognitionVersion = try container.decodeIfPresent(String.self, forKey: .recognitionVersion) ?? "1.0"
        languages = try container.decode([String].self, forKey: .languages)
        rawText = try container.decode(String.self, forKey: .rawText)
        userCorrectedText = try container.decodeIfPresent(String.self, forKey: .userCorrectedText)
        createdAt = try container.decode(Date.self, forKey: .createdAt)
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
