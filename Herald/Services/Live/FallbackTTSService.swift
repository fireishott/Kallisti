import Foundation

/// Composite TTS service that falls back to Apple's on-device AVSpeechSynthesizer
/// when the Mimo TTS API is unavailable (no API key, network error, etc.).
///
/// Streaming TTS prefers Mimo (pcm16) and falls back to Apple only when Mimo
/// is unavailable — no key, network failure. Mimo restored low-latency
/// streaming in the v2.5 series.
@MainActor
final class FallbackTTSService: TTSServiceProtocol {
    private let primary: TTSServiceProtocol   // Mimo
    private let fallback: TTSServiceProtocol  // Apple
    private let mimoKeyProvider: @MainActor () -> String?

    private var hasUsableMimoKey: Bool {
        guard let key = mimoKeyProvider() else { return false }
        return !key.isEmpty
    }

    init(primary: TTSServiceProtocol, fallback: TTSServiceProtocol, mimoKeyProvider: @escaping @MainActor () -> String?) {
        self.primary = primary
        self.fallback = fallback
        self.mimoKeyProvider = mimoKeyProvider
    }

    var isPlaying: Bool {
        primary.isPlaying || fallback.isPlaying
    }

    /// Return synthesized audio data. Mimo only — Apple throws synthesizeNotSupported.
    func synthesize(text: String, voice: String, context: String?) async throws -> Data {
        return try await primary.synthesize(text: text, voice: voice, context: context)
    }

    /// Speak complete text. Mimo first; falls through to Apple on recoverable errors
    /// (no API key, network error, invalid response, no audio data).
    func speak(_ text: String, voice: String, context: String?) async throws {
        do {
            try await primary.speak(text, voice: voice, context: context)
        } catch {
            if isRecoverable(error) {
                try await fallback.speak(text, voice: voice, context: context)
            } else {
                throw error
            }
        }
    }

    /// Streaming TTS — Mimo first, Apple only when Mimo has no usable key.
    func speakStreaming(_ chunk: String, voice: String?) {
        guard hasUsableMimoKey else {
            fallback.speakStreaming(chunk, voice: voice)
            return
        }
        primary.speakStreaming(chunk, voice: voice)
    }

    /// Flush any remaining buffered text on whichever engine is streaming.
    func finishStream() {
        if hasUsableMimoKey {
            primary.finishStream()
        } else {
            fallback.finishStream()
        }
    }

    func stop() {
        primary.stop()
        fallback.stop()
    }

    // MARK: - Private

    /// Recoverable errors are ones that indicate Mimo is unavailable at runtime
    /// (missing key, network down) rather than permanently misconfigured.
    private func isRecoverable(_ error: Error) -> Bool {
        // Check for Mimo-specific error types
        let nsError = error as NSError
        let domain = nsError.domain
        let errorString = String(describing: error)

        // MimoTTSService.TTSError cases that are recoverable
        if domain.contains("TTSError") || errorString.contains("TTSError") {
            // noAPIKey, httpError, invalidResponse, noAudioData → fallback
            // invalidURL, decodeFailed, voiceDesignRequiresStyle → don't fallback
            if errorString.contains("invalidURL") || errorString.contains("decodeFailed")
                || errorString.contains("voiceDesignRequiresStyle") {
                return false
            }
            return true
        }

        // Generic network/timeout errors → fallback
        if domain == NSURLErrorDomain {
            return true
        }

        // Unknown errors → fallback (safer to try Apple than to fail silently)
        return true
    }
}
