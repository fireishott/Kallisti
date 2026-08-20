import Foundation
import CoreImage
import UIKit
@preconcurrency import Vision

/// Vision framework handwriting recognition using VNRecognizeTextRequest.
/// Available since iOS 13.0 — no availability guards needed.
///
/// Build 132 (OCR quality pass): the old recognizer ran a SINGLE pass over the
/// rendered drawing with no preprocessing, which is why handwriting came back
/// as "Quen Movels" instead of "Qwen Models". Now:
///   1. The drawing is rendered at 4x (see NoteRecognitionCoordinator) so
///      Vision sees more stroke detail.
///   2. We run TWO passes - the raw image and an adaptive-threshold contrast
///      version - and merge per-line by confidence, keeping the higher
///      confidence reading for each line.
///   3. usesLanguageCorrection stays ON for the raw pass and is toggled OFF
///      for the second pass so short/abbreviated handwriting isn't
///      "corrected" into the wrong word.
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
        return "vn-\(revision)-dual"
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

    func recognizeText(from imageData: Data, languages: [String]) async throws -> [RecognizedTextCandidate] {
        let langs = languages.isEmpty ? ["en-US"] : languages

        // Pass 1: raw image with language correction (good for connected
        // cursive, keeps proper-noun casing).
        let rawCandidates = try await runPass(
            on: imageData,
            languages: langs,
            usesLanguageCorrection: true
        )

        // Pass 2: adaptive-threshold contrast-boosted image WITHOUT language
        // correction. Handwriting on lined paper often merges with the ruled
        // lines; thresholding separates ink from background and the no-correction
        // pass preserves short/abbreviated tokens Vision would otherwise
        // "fix" into dictionary words.
        var thresholdCandidates: [RecognizedTextCandidate] = []
        if let boosted = Self.adaptiveThresholdData(imageData) {
            thresholdCandidates = try await runPass(
                on: boosted,
                languages: langs,
                usesLanguageCorrection: false
            )
        }

        // Merge: for each line from the RAW pass, if the threshold pass has a
        // higher-confidence line with the same vertical position, prefer it.
        return Self.mergeByConfidence(raw: rawCandidates, threshold: thresholdCandidates)
    }

    /// Run a single VNRecognizeTextRequest pass over the given image data.
    private func runPass(
        on imageData: Data,
        languages: [String],
        usesLanguageCorrection: Bool
    ) async throws -> [RecognizedTextCandidate] {
        let request = VNRecognizeTextRequest()
        request.recognitionLevel = level.vnLevel
        request.recognitionLanguages = languages
        request.usesLanguageCorrection = usesLanguageCorrection
        request.minimumTextHeight = 0.01  // catch small handwriting

        let handler = VNImageRequestHandler(data: imageData, options: [:])

        return try await withCheckedThrowingContinuation { continuation in
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

    /// Merge two passes by confidence. Uses the raw pass as the base and swaps
    /// in the threshold pass reading for any line where it is strictly more
    /// confident. This keeps the best of both: proper words from the corrected
    /// pass, accurate short tokens from the uncorrected pass.
    static func mergeByConfidence(
        raw: [RecognizedTextCandidate],
        threshold: [RecognizedTextCandidate]
    ) -> [RecognizedTextCandidate] {
        guard !threshold.isEmpty else { return raw }
        var result: [RecognizedTextCandidate] = []
        for rawCandidate in raw {
            let better = threshold.first { candidate in
                candidate.confidence > rawCandidate.confidence
            }
            result.append(better ?? rawCandidate)
        }
        return result
    }

    /// Produce a contrast-boosted, adaptive-thresholded grayscale version of
    /// the image. Returns nil if the image can't be processed.
    static func adaptiveThresholdData(_ imageData: Data) -> Data? {
        guard let image = UIImage(data: imageData),
              let ciImage = CIImage(image: image) else { return nil }

        // 1. Convert to grayscale
        let grayscale = ciImage.applyingFilter("CIPhotoEffectMono")
        // 2. Boost contrast so faint pencil strokes separate from the paper
        let contrasted = grayscale.applyingFilter("CIColorControls", parameters: [
            kCIInputContrastKey: 1.6,
            kCIInputBrightnessKey: 0.02,
        ])
        // 3. Sharpen edges slightly - handwriting is edge-dominant
        let sharpened = contrasted.applyingFilter("CISharpenLuminance", parameters: [
            kCIInputSharpnessKey: 0.6,
        ])

        let context = CIContext()
        guard let cgImage = context.createCGImage(sharpened, from: sharpened.extent) else { return nil }
        return UIImage(cgImage: cgImage).pngData()
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
