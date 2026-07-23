@preconcurrency import AVFoundation
import Foundation
import MediaPlayer
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

    /// Whether the synthesizer is actively speaking (for Now Playing).
    var isSpeaking: Bool { synthesizer.isSpeaking }

    /// Current rate in user-facing units (0.4-2.0 range).
    var currentRate: Float { rate / AVSpeechUtteranceDefaultSpeechRate }

    /// Current speech buffer content (for testing).
    var currentBuffer: String { speechBuffer }

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
        configureAudioSession()
        setupRemoteCommands()
        setupInterruptionHandler()
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
                updateNowPlaying(title: sentence)

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

        updateNowPlaying(title: renderedText)

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
        MPNowPlayingInfoCenter.default().nowPlayingInfo = nil
    }

    var volume: Float = 1.0

    /// Configure the speech rate (0.4x - 2.0x).
    func setRate(_ newRate: Float) {
        rate = min(max(newRate, 0.4), 2.0) * AVSpeechUtteranceDefaultSpeechRate
    }

    /// Configure the voice by identifier.
    func setVoice(identifier: String) {
        voiceIdentifier = identifier
    }

    // MARK: - Audio Session + Now Playing

    /// Configure audio session for TTS playback with ducking.
    private func configureAudioSession() {
        let session = AVAudioSession.sharedInstance()
        do {
            // .playback with .duckOthers: music gets quieter, not stopped
            try session.setCategory(
                .playback,
                mode: .spokenAudio,
                options: [.duckOthers, .allowBluetooth, .allowAirPlay]
            )
            try session.setActive(true)
        } catch {
            Self.logger.error("Audio session config failed: \(error)")
        }
    }

    /// Update Now Playing info center with current title.
    private func updateNowPlaying(title: String) {
        let info: [String: Any] = [
            MPMediaItemPropertyTitle: title,
            MPMediaItemPropertyArtist: "Herald",
            MPNowPlayingInfoPropertyMediaType: MPNowPlayingInfoMediaType.audio.rawValue,
            MPNowPlayingInfoPropertyPlaybackRate: isSpeaking ? 1.0 : 0.0,
        ]
        MPNowPlayingInfoCenter.default().nowPlayingInfo = info
    }

    /// Register remote command handlers for lock screen / Control Center.
    private func setupRemoteCommands() {
        let center = MPRemoteCommandCenter.shared()

        center.playCommand.addTarget { [weak self] _ in
            // Resume speaking — no-op since we can't resume stopped utterances
            return .success
        }

        center.pauseCommand.addTarget { [weak self] _ in
            self?.stop()
            return .success
        }

        center.skipForwardCommand.addTarget { [weak self] _ in
            self?.skipToNextSentence()
            return .success
        }

        center.skipBackwardCommand.addTarget { [weak self] _ in
            self?.restartCurrentSentence()
            return .success
        }
    }

    /// Observe audio interruptions (phone calls, alarms, etc.).
    private func setupInterruptionHandler() {
        NotificationCenter.default.addObserver(
            forName: AVAudioSession.interruptionNotification,
            object: nil,
            queue: .main
        ) { [weak self] notification in
            guard let self,
                  let type = notification.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt,
                  let interruptionType = AVAudioSession.InterruptionType(rawValue: type)
            else { return }

            switch interruptionType {
            case .began:
                // Phone call started — pause TTS
                self.stop()
            case .ended:
                // Phone call ended — resume if was speaking
                if let options = notification.userInfo?[AVAudioSessionInterruptionOptionKey] as? UInt,
                   AVAudioSession.InterruptionOptions(rawValue: options).contains(.shouldResume) {
                    // Resume speaking — no-op since we can't resume stopped utterances
                }
            @unknown default:
                break
            }
        }
    }

    /// Skip to the next sentence in the queue.
    private func skipToNextSentence() {
        synthesizer.stopSpeaking(at: .word)
    }

    /// Restart the current sentence from the beginning.
    private func restartCurrentSentence() {
        // AVSpeechSynthesizer doesn't support seeking, so we just stop
        synthesizer.stopSpeaking(at: .immediate)
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
