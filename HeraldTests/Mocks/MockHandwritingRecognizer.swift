import Foundation
@testable import Herald

/// Mock handwriting recognizer for testing.
struct MockHandwritingRecognizer: HandwritingRecognizing {
    var engineId: String = "mock"
    var engineVersion: String? = "1.0.0"

    /// Pre-configured results to return.
    var results: [RecognizedTextCandidate] = []

    /// Error to throw (if set).
    var error: Error?

    func recognizeText(from imageData: Data, languages: [String]) async throws -> [RecognizedTextCandidate] {
        if let error { throw error }
        return results
    }
}
