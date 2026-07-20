import AVFoundation
import Foundation
import OSLog

/// Mimo v2.5 TTS service using the OpenAI-compatible chat completions API.
///
/// API: POST https://api.xiaomimimo.com/v1/chat/completions
/// Model: mimo-v2.5-tts
/// Auth: `api-key` header (MIMO_API_KEY)
/// Response: choices[0].message.audio.data = base64-encoded WAV
@MainActor
final class MimoTTSService: NSObject, TTSServiceProtocol, SpeechSynthesizing {
    private static let logger = Logger(
        subsystem: Bundle.main.bundleIdentifier ?? "io.hermesmobile.HermesMobile",
        category: "MimoTTS"
    )

    private static let baseURL = "https://api.xiaomimimo.com/v1"
    private static let model = "mimo-v2.5-tts"
    private static let sessionTimeout: TimeInterval = 30

    private(set) var isPlaying = false

    private let apiKeyProvider: @MainActor () -> String?
    private let session: URLSession
    private let streamingSession: URLSession
    private let decoder = JSONDecoder()
    private var audioPlayer: AVAudioPlayer?
    private var currentStreamingTask: Task<Void, Never>?

    init(apiKeyProvider: @escaping @MainActor () -> String?) {
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = Self.sessionTimeout
        config.timeoutIntervalForResource = Self.sessionTimeout
        self.session = URLSession(configuration: config)

        let streamingConfig = URLSessionConfiguration.default
        streamingConfig.timeoutIntervalForRequest = 120
        streamingConfig.timeoutIntervalForResource = 120
        self.streamingSession = URLSession(configuration: streamingConfig)

        self.apiKeyProvider = apiKeyProvider
        super.init()
    }

    func synthesize(text: String, voice: String = "Mia", context: String? = nil) async throws -> Data {
        guard let apiKey = apiKeyProvider(), !apiKey.isEmpty else {
            throw TTSError.noAPIKey
        }

        guard let url = URL(string: "\(Self.baseURL)/chat/completions") else {
            throw TTSError.invalidURL
        }

        // Build messages array:
        // - Optional user message for style/context control
        // - Assistant message contains the text to synthesize
        var messages: [[String: String]] = []
        if let context, !context.isEmpty {
            messages.append(["role": "user", "content": context])
        }
        messages.append(["role": "assistant", "content": text])

        let body: [String: Any] = [
            "model": Self.model,
            "messages": messages,
            "audio": [
                "format": "wav",
                "voice": voice,
            ],
        ]

        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue(apiKey, forHTTPHeaderField: "api-key")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: body)

        Self.logger.info("Synthesizing \(text.count, privacy: .public) chars with voice \(voice, privacy: .public)")

        let (data, response) = try await session.data(for: request)

        guard let httpResponse = response as? HTTPURLResponse else {
            throw TTSError.invalidResponse
        }

        guard (200 ..< 300).contains(httpResponse.statusCode) else {
            let errorBody = String(data: data, encoding: .utf8) ?? "Unknown error"
            Self.logger.error("Mimo TTS HTTP \(httpResponse.statusCode, privacy: .public): \(errorBody, privacy: .public)")
            throw TTSError.httpError(statusCode: httpResponse.statusCode, message: errorBody)
        }

        // Parse OpenAI-compatible response: choices[0].message.audio.data
        let apiResponse = try decoder.decode(MimoChatResponse.self, from: data)

        guard let audioData = apiResponse.choices.first?.message.audio?.data else {
            Self.logger.error("No audio data in Mimo TTS response")
            throw TTSError.noAudioData
        }

        guard let wavData = Data(base64Encoded: audioData) else {
            Self.logger.error("Failed to decode base64 audio data")
            throw TTSError.decodeFailed
        }

        Self.logger.info("Received \(wavData.count, privacy: .public) bytes of WAV audio")
        return wavData
    }

    func speak(_ text: String, voice: String = "Mia", context: String? = nil) async throws {
        stop()

        let audioData = try await synthesize(text: text, voice: voice, context: context)

        try configureAudioSession()

        let player = try AVAudioPlayer(data: audioData)
        player.delegate = self
        audioPlayer = player
        isPlaying = true
        player.play()

        Self.logger.info("Playing TTS audio (\(audioData.count, privacy: .public) bytes)")
    }

    func stop() {
        audioPlayer?.stop()
        audioPlayer = nil
        isPlaying = false
    }

    // MARK: - Streaming TTS

    private var streamingCancelled = false

    /// Stream PCM16 audio chunks for the given text.
    /// Sends `stream: true` in the request and parses SSE audio deltas.
    func audioStream(for text: String, voice: SpeechVoice, style: String?) -> AsyncThrowingStream<PCMChunk, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    guard let apiKey = self.apiKeyProvider(), !apiKey.isEmpty else {
                        throw TTSError.noAPIKey
                    }

                    guard let url = URL(string: "\(Self.baseURL)/chat/completions") else {
                        throw TTSError.invalidURL
                    }

                    var messages: [[String: String]] = []
                    if let style, !style.isEmpty {
                        messages.append(["role": "user", "content": style])
                    }
                    messages.append(["role": "assistant", "content": text])

                    let body: [String: Any] = [
                        "model": Self.model,
                        "messages": messages,
                        "stream": true,
                        "audio": [
                            "format": "pcm16",
                            "voice": voice.rawValue,
                        ],
                    ]

                    var request = URLRequest(url: url)
                    request.httpMethod = "POST"
                    request.setValue(apiKey, forHTTPHeaderField: "api-key")
                    request.setValue("application/json", forHTTPHeaderField: "Content-Type")
                    request.httpBody = try JSONSerialization.data(withJSONObject: body)

                    Self.logger.info("Streaming TTS \(text.count, privacy: .public) chars, voice=\(voice.rawValue, privacy: .public)")

                    let (bytes, response) = try await self.streamingSession.bytes(for: request)
                    guard let httpResponse = response as? HTTPURLResponse,
                          (200..<300).contains(httpResponse.statusCode) else {
                        throw TTSError.httpError(
                            statusCode: (response as? HTTPURLResponse)?.statusCode ?? -1,
                            message: "TTS streaming failed"
                        )
                    }

                    var sequence = 0
                    for try await line in bytes.lines {
                        guard !Task.isCancelled else { break }
                        guard line.hasPrefix("data: ") else { continue }
                        let jsonString = String(line.dropFirst(6))
                        guard jsonString != "[DONE]",
                              let data = jsonString.data(using: .utf8),
                              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                            break
                        }

                        // Extract audio delta from choices[0].delta.audio.data
                        guard let choices = json["choices"] as? [[String: Any]],
                              let first = choices.first,
                              let delta = first["delta"] as? [String: Any],
                              let audio = delta["audio"] as? [String: Any],
                              let b64Data = audio["data"] as? String,
                              let pcmData = Data(base64Encoded: b64Data) else {
                            continue
                        }

                        let chunk = PCMChunk(
                            data: pcmData,
                            sampleRate: 24000,
                            channels: 1,
                            format: .pcm16,
                            sequence: sequence,
                            isTerminal: false
                        )
                        continuation.yield(chunk)
                        sequence += 1
                    }

                    // Yield terminal chunk
                    if sequence > 0 {
                        continuation.yield(PCMChunk(
                            data: Data(),
                            sampleRate: 24000,
                            channels: 1,
                            format: .pcm16,
                            sequence: sequence,
                            isTerminal: true
                        ))
                    }
                    continuation.finish()
                } catch is CancellationError {
                    continuation.finish(throwing: CancellationError())
                } catch {
                    Self.logger.error("TTS streaming failed: \(error.localizedDescription, privacy: .public)")
                    continuation.finish(throwing: error)
                }
            }
            self.currentStreamingTask = task
            continuation.onTermination = { @Sendable _ in task.cancel() }
        }
    }

    func cancelStreaming() {
        currentStreamingTask?.cancel()
        currentStreamingTask = nil
    }

    // MARK: - SpeechSynthesizing

    func audio(for text: String, voice: SpeechVoice, style: String?) -> AsyncThrowingStream<PCMChunk, Error> {
        audioStream(for: text, voice: voice, style: style)
    }

    func cancel() {
        stop()
        cancelStreaming()
    }

    private func configureAudioSession() throws {
        let session = AVAudioSession.sharedInstance()
        try session.setCategory(.playAndRecord, mode: .default, options: [.defaultToSpeaker, .duckOthers])
        try session.setActive(true)
    }

    // MARK: - Response Models

    private struct MimoChatResponse: Decodable {
        let choices: [Choice]

        struct Choice: Decodable {
            let message: Message
        }

        struct Message: Decodable {
            let audio: AudioData?
        }

        struct AudioData: Decodable {
            let data: String // base64-encoded WAV
        }
    }

    enum TTSError: LocalizedError {
        case noAPIKey
        case invalidURL
        case invalidResponse
        case httpError(statusCode: Int, message: String)
        case noAudioData
        case decodeFailed

        var errorDescription: String? {
            switch self {
            case .noAPIKey:
                "No Mimo API key configured. Add one in Settings → Voice."
            case .invalidURL:
                "Invalid Mimo API URL."
            case .invalidResponse:
                "Mimo returned an invalid response."
            case .httpError(let code, let message):
                "Mimo API error (\(code)): \(message)"
            case .noAudioData:
                "Mimo returned no audio data."
            case .decodeFailed:
                "Failed to decode Mimo audio response."
            }
        }
    }
}

// MARK: - AVAudioPlayerDelegate

extension MimoTTSService: AVAudioPlayerDelegate {
    nonisolated func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
        Task { @MainActor [weak self] in
            self?.isPlaying = false
            Self.logger.info("TTS playback finished (success=\(flag, privacy: .public))")
        }
    }

    nonisolated func audioPlayerDecodeErrorDidOccur(_ player: AVAudioPlayer, error: Error?) {
        Task { @MainActor [weak self] in
            self?.isPlaying = false
            self?.audioPlayer = nil
            Self.logger.error("TTS playback decode error: \(error?.localizedDescription ?? "unknown", privacy: .public)")
        }
    }
}
