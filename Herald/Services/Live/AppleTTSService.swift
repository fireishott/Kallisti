@preconcurrency import AVFoundation
import Foundation
import os

/// Local TTS using AVSpeechSynthesizer.
/// Uses sentence-chunking to create the perception of streaming:
/// accumulates tokens, detects sentence boundaries, flushes as utterances.
/// Serves as a fallback when the MiMo TTS API is unreachable.
@MainActor
final class AppleTTSService: NSObject, TTSServiceProtocol, AVSpeechSynthesizerDelegate, @unchecked Sendable {
    private static let logger = Logger(subsystem: "net.fihonline.herald", category: "AppleTTS")

    private let synthesizer = AVSpeechSynthesizer()
    private var rate: Float = AVSpeechUtteranceDefaultSpeechRate
    private var speechBuffer = ""
    private var wordCount = 0
    private let maxWordsBeforeFlush = 12

    /// Sentence-terminating characters (Western + CJK)
    private static let terminators = CharacterSet(charactersIn: ".!?。！？")

    /// Continuation for the speak() async method.
    private var speakContinuation: CheckedContinuation<Void, Error>?

    var isPlaying: Bool { synthesizer.isSpeaking || !speechBuffer.isEmpty }

    // Voice selection
    var availableVoices: [AVSpeechSynthesisVoice] {
        AVSpeechSynthesisVoice.speechVoices()
            .filter { $0.language.hasPrefix("en") }
            .sorted { ($0.quality.rawValue, $0.name) > ($1.quality.rawValue, $1.name) }
    }

    var currentVoice: AVSpeechSynthesisVoice {
        AVSpeechSynthesisVoice(identifier: voiceIdentifier)
            ?? AVSpeechSynthesisVoice(language: "en-US")
            ?? AVSpeechSynthesisVoice()
    }

    var voiceIdentifier: String = "com.apple.ttsbundle.siri_female_en-US_compact"

    override init() {
        super.init()
        synthesizer.delegate = self
    }

    /// Called with each new token/chunk from the LLM stream.
    /// Buffers and flushes complete sentences as utterances.
    func speakStreaming(_ chunk: String, voice: String? = nil) {
        speechBuffer.append(chunk)
        wordCount += chunk.split(separator: " ").count

        // Flush when we have enough words and a sentence-ending punctuation
        let hasTerminator = speechBuffer.unicodeScalars.last.map {
            Self.terminators.contains($0)
        } ?? false

        if (wordCount >= 3 && hasTerminator) || wordCount >= maxWordsBeforeFlush {
            // Extract the last complete sentence
            if let boundary = SpeechTextRenderer.findSentenceBoundary(in: speechBuffer) {
                let sentence = boundary.text
                let utterance = AVSpeechUtterance(string: sentence)
                utterance.voice = currentVoice
                utterance.rate = rate
                utterance.postUtteranceDelay = 0.3  // natural pause between sentences
                utterance.volume = volume
                synthesizer.speak(utterance)

                // Remove the spoken sentence from the buffer
                speechBuffer = String(speechBuffer[boundary.endIndex...])
                wordCount = speechBuffer.split(separator: " ").count
            }
        }
    }

    /// Called when the stream ends — flush any remaining text.
    func finishStream() {
        let remaining = speechBuffer.trimmingCharacters(in: .whitespacesAndNewlines)
        if !remaining.isEmpty {
            let utterance = AVSpeechUtterance(string: remaining)
            utterance.voice = currentVoice
            utterance.rate = rate
            utterance.volume = volume
            synthesizer.speak(utterance)
        }
        speechBuffer = ""
        wordCount = 0
    }

    /// Synthesize and return audio data (not supported for AVSpeechSynthesizer).
    func synthesize(text: String, voice: String, context: String?) async throws -> Data {
        throw AppleTTSError.synthesizeNotSupported
    }

    /// Speak complete text (non-streaming, for full responses).
    func speak(_ text: String, voice: String, context: String?) async throws {
        let renderedText = SpeechTextRenderer.render(text)
        guard !renderedText.isEmpty else { return }

        let utterance = AVSpeechUtterance(string: renderedText)
        utterance.voice = currentVoice
        utterance.rate = rate
        utterance.volume = volume

        return try await withCheckedThrowingContinuation { continuation in
            self.speakContinuation = continuation
            synthesizer.speak(utterance)
        }
    }

    func stop() {
        synthesizer.stopSpeaking(at: .immediate)
        speechBuffer = ""
        wordCount = 0
        if let continuation = speakContinuation {
            speakContinuation = nil
            continuation.resume()
        }
    }

    var volume: Float = 1.0

    /// Configure the speech rate (0.4x - 2.0x).
    func setRate(_ newRate: Float) {
        rate = min(max(newRate, 0.4), 2.0) * AVSpeechUtteranceDefaultSpeechRate
    }

    // MARK: - AVSpeechSynthesizerDelegate

    nonisolated func speechSynthesizer(_ synthesizer: AVSpeechSynthesizer, didFinish utterance: AVSpeechUtterance) {
        Task { @MainActor [weak self] in
            guard let self else { return }
            // If there's a pending speak() continuation and no more utterances queued
            if !self.synthesizer.isSpeaking, let continuation = self.speakContinuation {
                self.speakContinuation = nil
                continuation.resume()
            }
        }
    }

    nonisolated func speechSynthesizer(_ synthesizer: AVSpeechSynthesizer, willSpeakRangeOfSpeechString characterRange: NSRange, utterance: AVSpeechUtterance) {
        // Could be used for progress tracking in the future
    }

    enum AppleTTSError: LocalizedError {
        case synthesizeNotSupported

        var errorDescription: String? {
            switch self {
            case .synthesizeNotSupported:
                "AVSpeechSynthesizer does not support returning raw audio data."
            }
        }
    }
}
