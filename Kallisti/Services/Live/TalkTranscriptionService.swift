import Foundation
import os

/// Kallisti 0.1.0: hermes-native STT via connector `/v1/talk/transcribe`.
/// Drop-in replacement for MimoASRService — same `SpeechRecognizing` protocol,
/// no device key, plain JSON response.
@MainActor
final class TalkTranscriptionService: SpeechRecognizing {
    private let logger = Logger(subsystem: "net.fihonline.kallisti", category: "TalkSTT")
    private let relayBaseURLProvider: @MainActor () -> URL?
    private let accessTokenProvider: @MainActor () async -> String?
    private let session: URLSession

    init(
        relayBaseURLProvider: @escaping @MainActor () -> URL?,
        accessTokenProvider: @escaping @MainActor () async -> String?
    ) {
        self.relayBaseURLProvider = relayBaseURLProvider
        self.accessTokenProvider = accessTokenProvider
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 60   // local whisper can take a few seconds
        self.session = URLSession(configuration: config)
    }

    func transcribe(_ utterance: RecordedUtterance, language: SpeechLanguage) -> AsyncThrowingStream<TranscriptUpdate, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    guard let relayBase = self.relayBaseURLProvider() else {
                        throw TalkSpeechError.noRelay
                    }
                    var request = URLRequest(url: relayBase.appendingPathComponent("talk/transcribe"))
                    request.httpMethod = "POST"
                    if let token = await self.accessTokenProvider() {
                        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
                    }
                    let boundary = UUID().uuidString
                    request.setValue("multipart/form-data; boundary=\(boundary)", forHTTPHeaderField: "Content-Type")
                    var body = Data()
                    body.append("--\(boundary)\r\n".data(using: .utf8)!)
                    body.append("Content-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n".data(using: .utf8)!)
                    body.append("Content-Type: audio/wav\r\n\r\n".data(using: .utf8)!)
                    body.append(utterance.audioData)
                    body.append("\r\n--\(boundary)\r\n".data(using: .utf8)!)
                    body.append("Content-Disposition: form-data; name=\"language\"\r\n\r\n".data(using: .utf8)!)
                    body.append(language.rawValue.data(using: .utf8)!)
                    body.append("\r\n--\(boundary)--\r\n".data(using: .utf8)!)
                    request.httpBody = body

                    let (data, response) = try await self.session.data(for: request)
                    guard let http = response as? HTTPURLResponse else {
                        throw TalkSpeechError.transport("No HTTP response")
                    }
                    let json = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
                    guard (200..<300).contains(http.statusCode) else {
                        let message = (json?["message"] as? String) ?? "Transcription failed (HTTP \(http.statusCode))."
                        throw TalkSpeechError.server(message)
                    }
                    let text = (json?["text"] as? String) ?? ""
                    continuation.yield(TranscriptUpdate(text: text, isFinal: true, confidence: nil))
                    continuation.finish()
                } catch is CancellationError {
                    continuation.finish(throwing: CancellationError())
                } catch {
                    self.logger.error("STT failed: \(error.localizedDescription, privacy: .public)")
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { @Sendable _ in task.cancel() }
        }
    }

    func cancel() {}
}

enum TalkSpeechError: Error, LocalizedError {
    case noRelay
    case transport(String)
    case server(String)

    var errorDescription: String? {
        switch self {
        case .noRelay: "No relay is configured; pair with a host before using voice."
        case .transport(let m): m
        case .server(let m): m
        }
    }
}
