import Foundation

/// Protocol for handwriting recognition engines.
/// Sendable — implementations must be thread-safe.
protocol HandwritingRecognizing: Sendable {
    /// Whether this recognizer is available on the current device/OS.
    var isAvailable: Bool { get }

    /// ISO language codes supported by this recognizer.
    var supportedLanguages: [String] { get }

    /// The engine identifier (e.g., "vn_accurate", "vn_fast")
    var engineId: String { get }

    /// Engine version string (where available)
    var engineVersion: String? { get }

    /// Version of the recognition model/configuration.
    /// Advance to force re-recognition of existing drawings.
    var recognitionVersion: String { get }

    /// Recognize text from a rendered image of a PKDrawing.
    /// - Parameters:
    ///   - imageData: PNG/JPEG data of the rendered drawing
    ///   - languages: ISO language codes to prefer
    /// - Returns: Array of recognition candidates, ordered by confidence
    func recognizeText(from imageData: Data, languages: [String]) async throws -> [RecognizedTextCandidate]

    /// Recognize text per-stroke. The caller (e.g. NoteRecognitionCoordinator)
    /// is expected to render each PencilKit stroke individually using the
    /// provided `strokeBounds` (normalized to the full drawing's coordinate
    /// space) and call back into the recognizer. Returning `nil` signals the
    /// recognizer cannot do per-stroke work and the caller should fall back
    /// to whole-image recognition via `recognizeText`.
    ///
    /// Default implementation returns `nil` so existing conformers / mocks
    /// keep compiling.
    func recognizePerStroke(
        from imageData: Data,
        languages: [String],
        strokeBounds: [CGRect]
    ) async throws -> [RecognizedTextCandidate]?
}

extension HandwritingRecognizing {
    func recognizePerStroke(
        from imageData: Data,
        languages: [String],
        strokeBounds: [CGRect]
    ) async throws -> [RecognizedTextCandidate]? {
        return nil
    }
}

/// A single recognition candidate with confidence.
struct RecognizedTextCandidate: Sendable {
    let text: String
    let confidence: Float  // 0.0–1.0

    /// Normalized bounding box of the recognized text in the source image's
    /// coordinate space. Optional so legacy call sites that don't yet care
    /// about layout keep compiling. When populated by Vision, the rect is in
    /// Vision's normalized space (origin at bottom-left, y increases upward).
    let boundingBox: CGRect?

    /// Index of the line this candidate belongs to, when known. Optional so
    /// legacy call sites keep compiling.
    let lineIndex: Int?

    init(
        text: String,
        confidence: Float,
        boundingBox: CGRect? = nil,
        lineIndex: Int? = nil
    ) {
        self.text = text
        self.confidence = confidence
        self.boundingBox = boundingBox
        self.lineIndex = lineIndex
    }
}