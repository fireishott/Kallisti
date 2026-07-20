import Foundation

/// Protocol for handwriting recognition engines.
/// Sendable — implementations must be thread-safe.
protocol HandwritingRecognizing: Sendable {
    /// Recognize text from a rendered image of a PKDrawing.
    /// - Parameters:
    ///   - imageData: PNG/JPEG data of the rendered drawing
    ///   - languages: ISO language codes to prefer
    /// - Returns: Array of recognition candidates, ordered by confidence
    func recognizeText(from imageData: Data, languages: [String]) async throws -> [RecognizedTextCandidate]

    /// The engine identifier (e.g., "vn_accurate", "vn_fast")
    var engineId: String { get }

    /// Engine version string (where available)
    var engineVersion: String? { get }
}

/// A single recognition candidate with confidence.
struct RecognizedTextCandidate: Sendable {
    let text: String
    let confidence: Float  // 0.0–1.0
}
