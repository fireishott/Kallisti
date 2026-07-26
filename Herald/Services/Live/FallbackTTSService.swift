import Foundation

/// Composite TTS service that falls back to Apple's on-device AVSpeechSynthesizer
/// when the Mimo TTS API is unavailable (no API key, network error, etc.).
///
/// Streaming TTS (`speakStreaming` / `finishStream`) always delegates to Apple
/// because MimoTTSService's streaming methods are no-ops.
@MainActor
final class FallbackTTSService: TTSServiceProtocol {
    private let primary: TTSServiceProtocol   // Mimo
    private let fallback: TTSServiceProtocol  // Apple

    init(primary: TTSServiceProtocol, fallback: TTSServiceProtocol) {
        self.primary = primary
        self.fallback = fallback
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

    /// Streaming TTS — Mimo doesn't support it, so delegate directly to Apple.
    func speakStreaming(_ chunk: String, voice: String?) {
        fallback.speakStreaming(chunk, voice: voice)
    }

    /// Flush any remaining buffered text — delegated to Apple.
    func finishStream() {
        fallback.finishStream()
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
            // invalidURL, decodeFailed → don't fallback (config issue)
            if errorString.contains("invalidURL") || errorString.contains("decodeFailed") {
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
