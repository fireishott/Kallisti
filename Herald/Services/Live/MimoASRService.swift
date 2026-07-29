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

                    // B38 P1-2: handle three Mimo response shapes:
                    // 1. Newline-delimited JSON: {"type":"delta","text":"..."}
                    // 2. SSE: data: {"type":"delta","text":"..."}
                    // 3. Non-streaming JSON: {"text":"..."}
                    var sawAnyLine = false
                    for try await line in bytes.lines {
                        guard !Task.isCancelled else { break }
                        sawAnyLine = true

                        // Strip SSE "data: " prefix if present
                        var payload = line
                        if payload.hasPrefix("data: ") {
                            payload = String(payload.dropFirst(6))
                        }
                        // Strip SSE "data:" (no space) if present
                        if payload.hasPrefix("data:") {
                            payload = String(payload.dropFirst(5))
                        }
                        payload = payload.trimmingCharacters(in: .whitespaces)

                        guard !payload.isEmpty,
                              let data = payload.data(using: .utf8),
                              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
                            // Non-streaming fallback: the whole body might be a
                            // single JSON object with a "text" key.
                            if let data = line.data(using: .utf8),
                               let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                               let text = json["text"] as? String, !text.isEmpty {
                                self.logger.info("ASR non-streaming result: \(text.count, privacy: .public) chars")
                                continuation.yield(TranscriptUpdate(text: text, isFinal: true, confidence: nil))
                                continuation.finish()
                                return
                            }
                            continue
                        }

                        // Streaming path: type + text
                        if let type = json["type"] as? String {
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
                        } else if let text = json["text"] as? String {
                            // Non-streaming shape inside a line-delimited stream
                            self.logger.info("ASR non-streaming in stream: \(text.count, privacy: .public) chars")
                            continuation.yield(TranscriptUpdate(text: text, isFinal: true, confidence: nil))
                            continuation.finish()
                            return
                        }
                    }
                    // B38 P1-2: stream ended without a final — distinct error.
                    if sawAnyLine {
                        throw ASRError.noFinalTranscript
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
    case noFinalTranscript

    var errorDescription: String? {
        switch self {
        case .noAPIKey:
            "No MiMo API key configured."
        case .httpError(let code):
            "ASR request failed with HTTP \(code)."
        case .noFinalTranscript:
            "ASR stream ended without a final transcript — the audio may be too short or silent."
        }
    }
}
