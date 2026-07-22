import Foundation
@preconcurrency import Vision

/// Vision framework handwriting recognition using VNRecognizeTextRequest.
/// Available since iOS 13.0 — no availability guards needed.
struct VisionHandwritingRecognizer: HandwritingRecognizing {
    let level: RecognitionLevel
    let engineVersion: String? = nil

    var isAvailable: Bool { true }  // VNRecognizeTextRequest available since iOS 13

    var supportedLanguages: [String] {
        (try? Self.supportedLanguages(level: level)) ?? ["en-US"]
    }

    var engineId: String {
        switch level {
        case .accurate: "vn_accurate"
        case .fast:     "vn_fast"
        }
    }

    var recognitionVersion: String { "1.0" }

    enum RecognitionLevel {
        case accurate
        case fast

        var vnLevel: VNRequestTextRecognitionLevel {
            switch self {
            case .accurate: .accurate
            case .fast:     .fast
            }
        }
    }

    init(level: RecognitionLevel = .accurate) {
        self.level = level
    }

    func recognizeText(from imageData: Data, languages: [String]) async throws -> [RecognizedTextCandidate] {
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = level.vnLevel
        request.recognitionLanguages = languages.isEmpty ? ["en-US"] : languages
        request.usesLanguageCorrection = true

        // Create a handler from the image data
        let handler = VNImageRequestHandler(data: imageData, options: [:])

        return try await withCheckedThrowingContinuation { continuation in
            // Vision requests must run on a background thread
            DispatchQueue.global(qos: .userInitiated).async {
                do {
                    try handler.perform([request])

                    guard let observations = request.results else {
                        continuation.resume(returning: [])
                        return
                    }

                    let candidates: [RecognizedTextCandidate] = observations.compactMap { observation in
                        guard let topCandidate = observation.topCandidates(1).first else {
                            return nil
                        }
                        return RecognizedTextCandidate(
                            text: topCandidate.string,
                            confidence: topCandidate.confidence
                        )
                    }

                    continuation.resume(returning: candidates)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }
}

/// List of supported recognition languages.
extension VisionHandwritingRecognizer {
    static func supportedLanguages(level: RecognitionLevel = .accurate) throws -> [String] {
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = level.vnLevel
        return try request.supportedRecognitionLanguages()
    }
}
