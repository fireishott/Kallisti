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
}

/// A single recognition candidate with confidence.
struct RecognizedTextCandidate: Sendable {
    let text: String
    let confidence: Float  // 0.0–1.0
}
