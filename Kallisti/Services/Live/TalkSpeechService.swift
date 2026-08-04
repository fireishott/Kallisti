import AVFoundation
import Foundation
import OSLog

/// Kallisti 0.1.0: hermes-native TTS via connector `/v1/talk/speak`.
/// Drop-in replacement for MimoTTSService — same `TTSServiceProtocol` +
/// `SpeechSynthesizing` conformances, no device key.
///
/// Voice selection is host config now; the `voice`/`style` params are
/// accepted and ignored (protocol compatibility).
@MainActor
final class TalkSpeechService: NSObject, TTSServiceProtocol, SpeechSynthesizing {
    private static let logger = Logger(
        subsystem: "net.fihonline.kallisti",
        category: "TalkTTS"
    )

    private let relayBaseURLProvider: @MainActor () -> URL?
    private let accessTokenProvider: @MainActor () async -> String?
    private let session: URLSession
    private(set) var isPlaying = false
    private var audioPlayer: AVAudioPlayer?
    private var currentTask: Task<Void, Never>?

    init(
        relayBaseURLProvider: @escaping @MainActor () -> URL?,
        accessTokenProvider: @escaping @MainActor () async -> String?
    ) {
        self.relayBaseURLProvider = relayBaseURLProvider
        self.accessTokenProvider = accessTokenProvider
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 60
        config.timeoutIntervalForResource = 60
        self.session = URLSession(configuration: config)
        super.init()
    }

    // MARK: - TTSServiceProtocol

    func synthesize(text: String, voice: String = "", context: String? = nil) async throws -> Data {
        return try await fetchWAV(text)
    }

    func speak(_ text: String, voice: String = "", context: String? = nil) async throws {
        let wavData = try await fetchWAV(text)
        try await playWAV(wavData)
    }

    func stop() {
        audioPlayer?.stop()
        audioPlayer = nil
        isPlaying = false
        currentTask?.cancel()
        currentTask = nil
    }

    func speakStreaming(_ chunk: String, voice: String? = nil) {
        // Per-sentence early synthesis: buffer and flush on sentence boundaries.
        // For now, accumulate — finishStream() flushes. This matches the
        // MimoTTSService behavior that HermesTalkCoordinator expects.
        streamingBuffer += chunk
        flushIfSentenceBoundary()
    }

    func finishStream() {
        let remaining = streamingBuffer.trimmingCharacters(in: .whitespacesAndNewlines)
        streamingBuffer = ""
        guard !remaining.isEmpty else { return }
        currentTask = Task {
            do {
                try await speak(remaining)
            } catch {
                Self.logger.error("finishStream speak failed: \(error.localizedDescription, privacy: .public)")
            }
        }
    }

    // MARK: - SpeechSynthesizing

    func audio(for text: String, voice: SpeechVoice, style: String?) -> AsyncThrowingStream<PCMChunk, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    let wav = try await self.fetchWAV(text)
                    guard let chunk = Self.pcmChunk(fromWAV: wav, sequence: 0) else {
                        throw TalkSpeechError.transport("Host returned malformed audio.")
                    }
                    continuation.yield(chunk)
                    continuation.yield(PCMChunk(data: Data(), sampleRate: 24000, channels: 1,
                                                format: .pcm16, sequence: 1, isTerminal: true))
                    continuation.finish()
                } catch is CancellationError {
                    continuation.finish(throwing: CancellationError())
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            self.currentTask = task
            continuation.onTermination = { @Sendable _ in task.cancel() }
        }
    }

    func cancel() {
        stop()
    }

    // MARK: - Private

    private var streamingBuffer = ""

    private func flushIfSentenceBoundary() {
        // Flush on sentence-ending punctuation.
        let sentences = streamingBuffer.components(separatedBy: CharacterSet(charactersIn: ".!?"))
        if sentences.count > 1 {
            let toSpeak = sentences.dropLast().joined(separator: ".") + "."
            streamingBuffer = sentences.last ?? ""
            currentTask = Task {
                do {
                    try await speak(toSpeak)
                } catch {
                    Self.logger.error("speakStreaming flush failed: \(error.localizedDescription, privacy: .public)")
                }
            }
        }
    }

    private func fetchWAV(_ text: String) async throws -> Data {
        guard let relayBase = relayBaseURLProvider() else { throw TalkSpeechError.noRelay }
        var request = URLRequest(url: relayBase.appendingPathComponent("talk/speak"))
        request.httpMethod = "POST"
        if let token = await accessTokenProvider() {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: ["text": text])
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw TalkSpeechError.transport("No HTTP response") }
        guard (200..<300).contains(http.statusCode) else {
            let json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
            throw TalkSpeechError.server((json?["message"] as? String) ?? "Speech failed (HTTP \(http.statusCode)).")
        }
        return data
    }

    /// 24 kHz mono s16 WAV (the connector normalizes via ffmpeg) → one PCMChunk.
    /// Minimal RIFF walk: find the "data" subchunk; the connector controls the
    /// writer, so exotic WAV layouts are out of scope by contract.
    static func pcmChunk(fromWAV wav: Data, sequence: Int) -> PCMChunk? {
        guard wav.count > 44 else { return nil }
        var offset = 12
        while offset + 8 <= wav.count {
            let chunkID = String(data: wav.subdata(in: offset..<offset + 4), encoding: .ascii) ?? ""
            let size = wav.subdata(in: offset + 4..<offset + 8).withUnsafeBytes { $0.loadUnaligned(as: UInt32.self) }
            if chunkID == "data" {
                let start = offset + 8
                let end = min(start + Int(size), wav.count)
                return PCMChunk(data: wav.subdata(in: start..<end), sampleRate: 24000,
                                channels: 1, format: .pcm16, sequence: sequence, isTerminal: false)
            }
            offset += 8 + Int(size) + (Int(size) % 2)
        }
        return nil
    }

    private func playWAV(_ data: Data) async throws {
        audioPlayer = try AVAudioPlayer(data: data)
        audioPlayer?.delegate = self
        audioPlayer?.prepareToPlay()
        configureAudioSession()
        isPlaying = true
        audioPlayer?.play()
        // Wait for playback to finish.
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            self.playbackContinuation = continuation
        }
    }

    private var playbackContinuation: CheckedContinuation<Void, Error>?

    private func configureAudioSession() {
        do {
            let session = AVAudioSession.sharedInstance()
            try session.setCategory(.playback, mode: .default, options: [.duckOthers])
            try session.setActive(true)
        } catch {
            Self.logger.error("Audio session config failed: \(error.localizedDescription, privacy: .public)")
        }
    }
}

extension TalkSpeechService: AVAudioPlayerDelegate {
    nonisolated func audioPlayerDidFinishPlaying(_ player: AVAudioPlayer, successfully flag: Bool) {
        Task { @MainActor in
            self.isPlaying = false
            if flag {
                self.playbackContinuation?.resume()
            } else {
                self.playbackContinuation?.resume(throwing: TalkSpeechError.transport("Audio playback failed."))
            }
            self.playbackContinuation = nil
        }
    }
}
