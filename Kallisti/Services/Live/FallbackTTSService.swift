import Foundation

/// Composite TTS service that falls back to Apple's on-device AVSpeechSynthesizer
/// when the host TTS is unavailable (speech worker down, network error, etc.).
///
/// Kallisti 0.1.0: `hostAvailableProvider` replaces the old `mimoKeyProvider`.
/// The primary is TalkSpeechService (hermes-native via connector); the
/// fallback is Apple on-device TTS.
@MainActor
final class FallbackTTSService: TTSServiceProtocol {
    private let primary: TTSServiceProtocol   // TalkSpeechService (host)
    private let fallback: TTSServiceProtocol  // Apple on-device
    private let hostAvailableProvider: @MainActor () -> Bool

    private var hasUsableHost: Bool {
        hostAvailableProvider()
    }

    init(primary: TTSServiceProtocol, fallback: TTSServiceProtocol, hostAvailableProvider: @escaping @MainActor () -> Bool) {
        self.primary = primary
        self.fallback = fallback
        self.hostAvailableProvider = hostAvailableProvider
    }

    var isPlaying: Bool {
        primary.isPlaying || fallback.isPlaying
    }

    /// Return synthesized audio data. Host only — Apple throws synthesizeNotSupported.
    func synthesize(text: String, voice: String, context: String?) async throws -> Data {
        return try await primary.synthesize(text: text, voice: voice, context: context)
    }

    /// Speak complete text. Host first; falls through to Apple on recoverable errors.
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

    /// Streaming TTS — host first, Apple only when host is unavailable.
    func speakStreaming(_ chunk: String, voice: String?) {
        guard hasUsableHost else {
            fallback.speakStreaming(chunk, voice: voice)
            return
        }
        primary.speakStreaming(chunk, voice: voice)
    }

    /// Flush any remaining buffered text on whichever engine is streaming.
    func finishStream() {
        if hasUsableHost {
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

    /// Recoverable errors are ones that indicate host TTS is unavailable at
    /// runtime (speech worker down, network error) rather than permanently
    /// misconfigured.
    private func isRecoverable(_ error: Error) -> Bool {
        let errorString = String(describing: error)

        // TalkSpeechError — all cases are recoverable (host unavailable)
        if errorString.contains("TalkSpeechError") || errorString.contains("noRelay")
            || errorString.contains("transport") || errorString.contains("server") {
            return true
        }

        // Generic network/timeout errors → fallback
        let nsError = error as NSError
        if nsError.domain == NSURLErrorDomain {
            return true
        }

        // Unknown errors → fallback (safer to try Apple than to fail silently)
        return true
    }
}
