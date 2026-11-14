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

    var recognitionVersion: String {
        // Derive from the Vision framework's current text recognition revision.
        // This advances when Apple updates the on-device model, triggering re-recognition.
        let revision = VNRecognizeTextRequest.currentRevision
        return "vn-\(revision)"
    }

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

    /// Custom vocabulary used to bias Vision recognition toward terms that
    /// appear in this user's notes. These are proper nouns, product names,
    /// and frequent nouns/verbs that Vision's language model otherwise
    /// degrades on for cursive handwriting.
    /// Static so it is shared across instances and Sendable-safe.
    static let customWords: [String] = [
        // Products / apps
        "Kallisti", "Hermes", "Herald", "MiniMe", "Breakthru",
        // People
        "Curtis", "Kristie", "Hailey", "Cam",
        // Models / LLMs
        "Qwen", "Claude", "GPT", "Gemini", "Llama",
        // Sportsbooks / sports
        "BetMGM", "DraftKings", "FanDuel", "NFL", "MLB", "NBA",
        "NHL", "hockey", "soccer", "tennis", "golf",
        // Betting vocab
        "unders", "over", "parlay", "parlays", "spread", "moneyline",
        "props", "teaser", "future", "futures",
        // Note-taking workflow
        "research", "summary", "talkingpoints", "talking", "points",
        "review", "followup", "follow", "ups",
        // Common cursive-misfit words
        "the", "and", "with", "from", "this", "that", "have", "been",
        "into", "than", "them", "then", "what", "when", "your",
        "writing", "sports", "books", "lines", "pick", "picks",
        // Frequently used proper nouns in this user's notes
        "iPhone", "iPad", "Mac", "Vision", "PencilKit"
    ]

    func recognizeText(from imageData: Data, languages: [String]) async throws -> [RecognizedTextCandidate] {
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = level.vnLevel
        request.recognitionLanguages = languages.isEmpty ? ["en-US"] : languages
        // Keep language correction enabled — for cursive handwriting it
        // smooths out common mental spelling errors and joins split words.
        request.usesLanguageCorrection = true
        // Bias recognition toward vocabulary that appears in this user's notes.
        request.customWords = Self.customWords
        // Lower the minimum text height so Vision picks up small / thin
        // cursive strokes. Vision's default is 0.01; 0.005 ~ halves that.
        request.minimumTextHeight = 0.005

        // Create a handler from the image data
        let handler = VNImageRequestHandler(data: imageData, options: [:])

        return try await withCheckedThrowingContinuation { continuation in
            // Vision requests must run on a background thread
            DispatchQueue.global(qos: .userInitiated).async {
                do {
                    // Best-effort: lock the language model so the OS
                    // doesn't swap the model out under us mid-recognition.
                    // If locking fails (e.g. model not yet downloaded on
                    // this device, or iOS refuses the lock), we still
                    // proceed — locking is a perf/stability hint, not a
                    // correctness requirement.
                    // The model lock is a hint, not a requirement, so it is worth
                    // attempting only when this SDK carries it. `downloadAndLockModel`
                    // arrived after the iOS 26.5 SDK this host builds against, so the
                    // probe keeps the intent without a compile-time dependency on it.
                    if request.responds(to: NSSelectorFromString("lockModel")) {
                        _ = request.perform(NSSelectorFromString("lockModel"))
                    }

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
