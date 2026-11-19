//
//  DaemonASRService.swift
//  Kallisti
//
//  Transcription from the paired daemon. `POST /v1/transcribe` takes the clip and
//  forwards it to whatever `{base}/audio/transcriptions` endpoint the daemon is
//  configured with, so the model is the operator's choice, not the app's.
//

import Foundation
import os

@MainActor
final class DaemonASRService: SpeechRecognizing {
    private let logger = Logger(subsystem: "net.fihonline.kallisti", category: "DaemonASR")
    private let baseURLProvider: @MainActor () -> URL?
    private let accessTokenProvider: @MainActor () async -> String?
    private let session: URLSession
    private var task: Task<Void, Never>?

    init(
        baseURLProvider: @escaping @MainActor () -> URL?,
        accessTokenProvider: @escaping @MainActor () async -> String?,
        session: URLSession = .shared
    ) {
        self.baseURLProvider = baseURLProvider
        self.accessTokenProvider = accessTokenProvider
        self.session = session
    }

    func transcribe(_ utterance: RecordedUtterance, language: SpeechLanguage) -> AsyncThrowingStream<TranscriptUpdate, Error> {
        AsyncThrowingStream { continuation in
            let task = Task { [weak self] in
                guard let self else {
                    continuation.finish()
                    return
                }
                do {
                    let text = try await self.send(utterance)
                    if !text.isEmpty {
                        continuation.yield(TranscriptUpdate(text: text, isFinal: true, confidence: nil))
                    }
                    continuation.finish()
                } catch {
                    self.logger.error("daemon transcription failed: \(error.localizedDescription, privacy: .public)")
                    continuation.finish(throwing: error)
                }
            }
            self.task = task
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    func cancel() {
        task?.cancel()
        task = nil
    }

    private func send(_ utterance: RecordedUtterance) async throws -> String {
        guard !utterance.audioData.isEmpty else { throw DaemonASRError.emptyClip }
        guard let base = baseURLProvider() else { throw DaemonASRError.notPaired }
        var request = URLRequest(url: base.appending(path: "/v1/transcribe"))
        request.httpMethod = "POST"
        request.timeoutInterval = 180
        request.setValue("audio/wav", forHTTPHeaderField: "Content-Type")
        if let token = await accessTokenProvider(), !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        request.httpBody = utterance.audioData
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw DaemonASRError.unreachable }
        guard (200..<300).contains(http.statusCode) else {
            let body = String(data: data, encoding: .utf8) ?? ""
            throw DaemonASRError.refused(status: http.statusCode, body: body)
        }
        // The provider's own shape, passed through: OpenAI-compatible endpoints
        // answer `{"text": "..."}`. Anything else is reported, not guessed.
        guard
            let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let text = object["text"] as? String
        else {
            let body = String(data: data, encoding: .utf8) ?? ""
            throw DaemonASRError.unexpectedShape(body)
        }
        return text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    enum DaemonASRError: LocalizedError {
        case emptyClip
        case notPaired
        case unreachable
        case refused(status: Int, body: String)
        case unexpectedShape(String)

        var errorDescription: String? {
            switch self {
            case .emptyClip: "The recording was empty."
            case .notPaired: "No daemon is paired, so nothing can transcribe this."
            case .unreachable: "The daemon did not answer the transcription request."
            case let .refused(status, body):
                body.isEmpty
                    ? "The daemon refused the transcription (\(status))."
                    : "The daemon refused the transcription (\(status)): \(body)"
            case let .unexpectedShape(body):
                "The transcription answer had no text: \(body.prefix(160))"
            }
        }
    }
}
