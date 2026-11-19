//
//  DaemonTTSService.swift
//  Kallisti
//
//  Speech from the paired daemon. `POST /v1/speak` resolves its engine from the
//  provider the daemon is already configured with, so a local engine is used
//  when one is configured and a cloud provider is only touched when it is the
//  only one there is.
//

import Foundation

@MainActor
final class DaemonTTSService: SpeechSynthesizing {
    private let baseURLProvider: @MainActor () -> URL?
    private let accessTokenProvider: @MainActor () async -> String?
    private let session: URLSession
    private var task: Task<Void, Never>?
    private var sequence = 0

    init(
        baseURLProvider: @escaping @MainActor () -> URL?,
        accessTokenProvider: @escaping @MainActor () async -> String?,
        session: URLSession = .shared
    ) {
        self.baseURLProvider = baseURLProvider
        self.accessTokenProvider = accessTokenProvider
        self.session = session
    }

    func audio(for text: String, voice: SpeechVoice, style: String?) -> AsyncThrowingStream<PCMChunk, Error> {
        AsyncThrowingStream { continuation in
            let task = Task { [weak self] in
                guard let self else {
                    continuation.finish()
                    return
                }
                do {
                    let wav = try await self.synthesize(text)
                    let (pcm, rate) = Self.pcm(fromWAV: wav)
                    self.sequence += 1
                    continuation.yield(
                        PCMChunk(
                            data: pcm,
                            sampleRate: rate,
                            channels: 1,
                            format: .pcm16,
                            sequence: self.sequence,
                            isTerminal: true
                        )
                    )
                    continuation.finish()
                } catch {
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

    private func synthesize(_ text: String) async throws -> Data {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { throw DaemonTTSError.emptyText }
        guard let base = baseURLProvider() else { throw DaemonTTSError.notPaired }
        var request = URLRequest(url: base.appending(path: "/v1/speak"))
        request.httpMethod = "POST"
        request.timeoutInterval = 60
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        if let token = await accessTokenProvider(), !token.isEmpty {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        request.httpBody = try JSONSerialization.data(withJSONObject: ["text": trimmed])
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw DaemonTTSError.unreachable }
        guard (200..<300).contains(http.statusCode) else {
            // The daemon names its own refusal, such as no engine configured.
            let body = String(data: data, encoding: .utf8) ?? ""
            throw DaemonTTSError.refused(status: http.statusCode, body: body)
        }
        return data
    }

    /// Strip the WAV header the daemon answers with, and read the rate it declares.
    private static func pcm(fromWAV data: Data) -> (Data, Int) {
        guard data.count > 44 else { return (data, 24_000) }
        let bytes = [UInt8](data)
        // Find the `data` chunk rather than assuming 44 bytes: a `LIST` chunk
        // before it is legal and common.
        var index = 12
        while index + 8 <= bytes.count {
            let id = String(bytes: bytes[index..<(index + 4)], encoding: .ascii) ?? ""
            let size = Int(bytes[index + 4]) | Int(bytes[index + 5]) << 8
                | Int(bytes[index + 6]) << 16 | Int(bytes[index + 7]) << 24
            if id == "data" {
                let start = index + 8
                let end = min(bytes.count, start + max(0, size))
                return (Data(bytes[start..<end]), sampleRate(from: bytes) ?? 24_000)
            }
            index += 8 + max(0, size) + (size % 2)
        }
        return (data, 24_000)
    }

    private static func sampleRate(from bytes: [UInt8]) -> Int? {
        guard bytes.count >= 28 else { return nil }
        return Int(bytes[24]) | Int(bytes[25]) << 8 | Int(bytes[26]) << 16 | Int(bytes[27]) << 24
    }

    enum DaemonTTSError: LocalizedError {
        case emptyText
        case notPaired
        case unreachable
        case refused(status: Int, body: String)

        var errorDescription: String? {
            switch self {
            case .emptyText: "There is nothing to say."
            case .notPaired: "No daemon is paired, so there is no speech engine."
            case .unreachable: "The daemon did not answer the speech request."
            case let .refused(status, body):
                body.isEmpty
                    ? "The daemon refused the speech request (\(status))."
                    : "The daemon refused the speech request (\(status)): \(body)"
            }
        }
    }
}
