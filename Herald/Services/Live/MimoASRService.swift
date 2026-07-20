import Foundation
import os

@MainActor
final class MimoASRService: SpeechRecognizing {
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "MimoASR")
    private let apiKeyProvider: @MainActor () -> String?
    private let session: URLSession
    private var currentTask: Task<Void, Never>?

    init(apiKeyProvider: @escaping @MainActor () -> String?) {
        self.apiKeyProvider = apiKeyProvider
        let config = URLSessionConfiguration.default
        config.timeoutIntervalForRequest = 30
        self.session = URLSession(configuration: config)
    }

    func transcribe(_ utterance: RecordedUtterance, language: SpeechLanguage) -> AsyncThrowingStream<TranscriptUpdate, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    guard let apiKey = self.apiKeyProvider(), !apiKey.isEmpty else {
                        throw ASRError.noAPIKey
                    }

                    let url = URL(string: "https://api.xiaomimimo.com/v1/audio/transcriptions")!
                    var request = URLRequest(url: url)
                    request.httpMethod = "POST"
                    request.setValue(apiKey, forHTTPHeaderField: "api-key")

                    let boundary = UUID().uuidString
                    request.setValue(
                        "multipart/form-data; boundary=\(boundary)",
                        forHTTPHeaderField: "Content-Type"
                    )

                    var body = Data()
                    // Audio file part
                    body.append("--\(boundary)\r\n".data(using: .utf8)!)
                    body.append("Content-Disposition: form-data; name=\"file\"; filename=\"audio.wav\"\r\n".data(using: .utf8)!)
                    body.append("Content-Type: audio/wav\r\n\r\n".data(using: .utf8)!)
                    body.append(utterance.audioData)
                    body.append("\r\n".data(using: .utf8)!)
                    // Model part
                    body.append("--\(boundary)\r\n".data(using: .utf8)!)
                    body.append("Content-Disposition: form-data; name=\"model\"\r\n\r\n".data(using: .utf8)!)
                    body.append("mimo-v2.5-asr".data(using: .utf8)!)
                    body.append("\r\n".data(using: .utf8)!)
                    // Language part
                    body.append("--\(boundary)\r\n".data(using: .utf8)!)
                    body.append("Content-Disposition: form-data; name=\"language\"\r\n\r\n".data(using: .utf8)!)
                    body.append(language.rawValue.data(using: .utf8)!)
                    body.append("\r\n".data(using: .utf8)!)
                    // Stream part
                    body.append("--\(boundary)\r\n".data(using: .utf8)!)
                    body.append("Content-Disposition: form-data; name=\"stream\"\r\n\r\n".data(using: .utf8)!)
                    body.append("true".data(using: .utf8)!)
                    body.append("\r\n".data(using: .utf8)!)
                    body.append("--\(boundary)--\r\n".data(using: .utf8)!)

                    request.httpBody = body

                    self.logger.info("Transcribing \(utterance.audioData.count, privacy: .public) bytes, language=\(language.rawValue, privacy: .public)")

                    let (bytes, response) = try await self.session.bytes(for: request)
                    guard let httpResponse = response as? HTTPURLResponse,
                          (200..<300).contains(httpResponse.statusCode) else {
                        let statusCode = (response as? HTTPURLResponse)?.statusCode ?? -1
                        self.logger.error("ASR HTTP error: \(statusCode, privacy: .public)")
                        throw ASRError.httpError(statusCode)
                    }

                    for try await line in bytes.lines {
                        guard !Task.isCancelled else { break }
                        guard let data = line.data(using: .utf8),
                              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                              let type = json["type"] as? String else { continue }

                        if type == "delta" {
                            let text = json["text"] as? String ?? ""
                            continuation.yield(TranscriptUpdate(text: text, isFinal: false, confidence: nil))
                        } else if type == "final" {
                            let text = json["text"] as? String ?? ""
                            self.logger.info("ASR final: \(text.count, privacy: .public) chars")
                            continuation.yield(TranscriptUpdate(text: text, isFinal: true, confidence: nil))
                            continuation.finish()
                            return
                        }
                    }
                    continuation.finish()
                } catch is CancellationError {
                    continuation.finish(throwing: CancellationError())
                } catch {
                    self.logger.error("ASR failed: \(error.localizedDescription, privacy: .public)")
                    continuation.finish(throwing: error)
                }
            }
            self.currentTask = task
            continuation.onTermination = { @Sendable _ in task.cancel() }
        }
    }

    func cancel() {
        currentTask?.cancel()
        currentTask = nil
    }
}

enum ASRError: Error, LocalizedError {
    case noAPIKey
    case httpError(Int)

    var errorDescription: String? {
        switch self {
        case .noAPIKey:
            "No MiMo API key configured."
        case .httpError(let code):
            "ASR request failed with HTTP \(code)."
        }
    }
}
