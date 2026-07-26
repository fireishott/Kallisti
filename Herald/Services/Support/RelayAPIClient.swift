import Foundation
import os

private let sseLogger = Logger(subsystem: "net.fihonline.herald", category: "SSE")

// MARK: - SSE Line Iterator (chunked, delegate-driven)

/// `URLSession.AsyncBytes` iterates one byte at a time and is known to stall
/// on iOS when the server doesn't flush after every byte (which no real server
/// does). Instead we use a `URLSessionDataDelegate` that receives data in
/// chunks, drain lines from a buffer, and preserve empty lines (critical SSE
/// event delimiters that `AsyncLineSequence` silently drops).
///
/// This delegate-based approach has been reliable since iOS 7 and avoids the
/// hang/stall issue that made streaming broken across 60+ releases.
final class StreamingDataDelegate: NSObject, URLSessionDataDelegate, @unchecked Sendable {
    private let chunkContinuation: AsyncStream<Data>.Continuation
    private let completionContinuation: AsyncStream<Result<Void, Error>>.Continuation

    init(
        chunkContinuation: AsyncStream<Data>.Continuation,
        completionContinuation: AsyncStream<Result<Void, Error>>.Continuation
    ) {
        self.chunkContinuation = chunkContinuation
        self.completionContinuation = completionContinuation
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        chunkContinuation.yield(data)
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        if let error {
            completionContinuation.yield(.failure(error))
        } else {
            completionContinuation.yield(.success(()))
        }
        chunkContinuation.finish()
        completionContinuation.finish()
    }
}

/// Drains complete lines from the buffer into an async stream. Empty strings
/// (from `\n\n`) are preserved as SSE event delimiters.
func sseLines(from dataStream: AsyncStream<Data>) -> AsyncThrowingStream<String, Error> {
    AsyncThrowingStream { continuation in
        let task = Task {
            var buffer = Data()
            for await chunk in dataStream {
                if Task.isCancelled { break }
                buffer.append(chunk)
                for line in drainSSELines(from: &buffer) {
                    continuation.yield(line)
                }
            }
            // Flush whatever remains
            if !buffer.isEmpty {
                continuation.yield(String(data: buffer, encoding: .utf8) ?? "")
            }
            continuation.finish()
        }
        continuation.onTermination = { _ in task.cancel() }
    }
}

/// Splits a `Data` buffer on `\n` boundaries, yielding each line as a
/// `String`. Consecutive `\n` characters produce empty strings — these are
/// the critical SSE event delimiters. Partial (non-newline-terminated) data
/// remains in `buffer` for the next call.
///
/// - Parameter buffer: accumulated bytes; consumed content is removed
/// - Returns: array of decoded line strings
func drainSSELines(from buffer: inout Data) -> [String] {
    guard !buffer.isEmpty else { return [] }
    var lines: [String] = []
    var searchStart = buffer.startIndex
    while let newlineIndex = buffer[searchStart...].firstIndex(of: 0x0A) {
        let lineEnd = newlineIndex
        // Strip \r if present (handle \r\n line endings)
        var lineData: Data
        if lineEnd > buffer.startIndex, buffer[lineEnd - 1] == 0x0D {
            lineData = buffer[buffer.startIndex ..< (lineEnd - 1)]
        } else {
            lineData = buffer[buffer.startIndex ..< lineEnd]
        }
        lines.append(String(data: lineData, encoding: .utf8) ?? "")
        searchStart = lineEnd + 1
    }
    // Keep unconsumed bytes in buffer for next call
    if searchStart < buffer.endIndex {
        buffer = Data(buffer[searchStart...])
    } else {
        buffer.removeAll(keepingCapacity: true)
    }
    return lines
}

enum RelayCoders {
    private static func internetDateTimeStyle() -> Date.ISO8601FormatStyle {
        Date.ISO8601FormatStyle(timeZone: .gmt)
    }

    private static func internetDateTimeFractionalStyle() -> Date.ISO8601FormatStyle {
        Date.ISO8601FormatStyle(includingFractionalSeconds: true, timeZone: .gmt)
    }

    private static func normalizedRelayDateStrings(for value: String) -> [String] {
        if value.hasSuffix("Z") || value.range(of: #"[+-]\d{2}:\d{2}$"#, options: .regularExpression) != nil {
            return [value]
        }

        return ["\(value)Z"]
    }

    static func makeEncoder() -> JSONEncoder {
        let encoder = JSONEncoder()
        encoder.dateEncodingStrategy = .iso8601
        return encoder
    }

    static func makeDecoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.dateDecodingStrategy = .custom { decoder in
            let container = try decoder.singleValueContainer()
            let value = try container.decode(String.self)

            if let date = parseRelayDate(value) {
                return date
            }

            throw DecodingError.dataCorruptedError(
                in: container,
                debugDescription: "Unsupported relay date: \(value)"
            )
        }
        return decoder
    }

    static func parseRelayDate(_ value: String) -> Date? {
        for candidate in normalizedRelayDateStrings(for: value) {
            if let date = try? internetDateTimeFractionalStyle().parse(candidate) {
                return date
            }

            if let date = try? internetDateTimeStyle().parse(candidate) {
                return date
            }
        }

        return nil
    }
}

@MainActor
final class RelayAPIClient {
    private struct Envelope<T: Decodable>: Decodable {
        let data: T
    }

    private struct ErrorEnvelope: Decodable {
        struct ErrorPayload: Decodable {
            let code: String
            let message: String
            let retryable: Bool?
            let requestId: String?
            let timestamp: String?
        }

        let error: ErrorPayload
    }

    private struct FastAPIErrorEnvelope: Decodable {
        let detail: String

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            // FastAPI's default 422 handler sends `detail` as a list of
            // validation-error objects rather than a plain string.
            if let text = try? container.decode(String.self, forKey: .detail) {
                detail = text
            } else {
                let items = try container.decode([FastAPIValidationItem].self, forKey: .detail)
                detail = items.map(\.msg).joined(separator: "; ")
            }
        }

        private enum CodingKeys: String, CodingKey { case detail }
    }

    private struct FastAPIValidationItem: Decodable {
        let msg: String
    }

    enum ClientError: LocalizedError {
        case unauthorized(String)
        case invalidURL(String)
        case requestFailed(String)
        case serverError(code: String, message: String, requestId: String?, status: Int)

        var errorDescription: String? {
            switch self {
            case .unauthorized(let message):
                return message
            case .invalidURL(let url):
                return "Invalid relay URL: \(url)"
            case .requestFailed(let message):
                return message
            case .serverError(let code, let message, let requestId, _):
                var desc = "[\(code)] \(message)"
                if let requestId { desc += " (request: \(requestId))" }
                return desc
            }
        }
    }

    private let baseURLProvider: @MainActor () -> String
    private let session: URLSession
    private let encoder: JSONEncoder
    private let decoder: JSONDecoder

    init(
        baseURLProvider: @escaping @MainActor () -> String,
        session: URLSession = .shared
    ) {
        self.baseURLProvider = baseURLProvider
        self.session = session
        self.encoder = RelayCoders.makeEncoder()
        self.decoder = RelayCoders.makeDecoder()
    }

    func get<T: Decodable>(
        path: String,
        accessToken: String? = nil
    ) async throws -> T {
        let request = try makeRequest(path: path, method: "GET", accessToken: accessToken, body: nil)
        return try await sendRequest(request)
    }

    func post<T: Decodable>(
        path: String,
        accessToken: String? = nil
    ) async throws -> T {
        let request = try makeRequest(path: path, method: "POST", accessToken: accessToken, body: nil)
        return try await sendRequest(request)
    }

    func post<Body: Encodable, T: Decodable>(
        path: String,
        body: Body,
        accessToken: String? = nil
    ) async throws -> T {
        let requestBody = try encoder.encode(body)
        let request = try makeRequest(
            path: path,
            method: "POST",
            accessToken: accessToken,
            body: requestBody
        )
        return try await sendRequest(request)
    }

    func makeRequest(
        path: String,
        method: String,
        accessToken: String?,
        body: Data?
    ) throws -> URLRequest {
        let path = path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let baseURLString = baseURLProvider().trimmingCharacters(in: CharacterSet(charactersIn: "/"))

        guard let url = URL(string: "\(baseURLString)/\(path)") else {
            throw ClientError.invalidURL(baseURLString)
        }

        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.httpBody = body

        if let accessToken, !accessToken.isEmpty {
            request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
        }

        request.setValue(UUID().uuidString, forHTTPHeaderField: "X-Request-ID")
        request.timeoutInterval = 15  // Fail fast on dropped connections (was default 60s)

        return request
    }

    /// Opens an SSE stream to the given path and yields parsed events.
    ///
    /// Uses a `URLSessionDataDelegate` to receive data in chunks (not the
    /// broken byte-by-byte `AsyncBytes` iterator), drains lines preserving
    /// empty SSE delimiters, and parses `event:` / `data:` / `id:` fields
    /// per the SSE spec.
    nonisolated func streamEvents(
        path: String,
        accessToken: String?,
        lastEventID: String? = nil
    ) -> AsyncThrowingStream<SSEEvent, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    var request = try await MainActor.run {
                        try makeRequest(
                            path: path,
                            method: "GET",
                            accessToken: accessToken,
                            body: nil
                        )
                    }
                    request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
                    if let lastEventID, !lastEventID.isEmpty {
                        request.setValue(lastEventID, forHTTPHeaderField: "Last-Event-ID")
                    }
                    request.timeoutInterval = TimeInterval(Int.max)

                    // Build the delegate-driven pipeline: chunks → lines → SSE events
                    var chunkContinuation: AsyncStream<Data>.Continuation!
                    let dataStream = AsyncStream<Data> { chunkContinuation = $0 }

                    var completionContinuation: AsyncStream<Result<Void, Error>>.Continuation!
                    let completionStream = AsyncStream<Result<Void, Error>> { completionContinuation = $0 }

                    let delegate = StreamingDataDelegate(
                        chunkContinuation: chunkContinuation,
                        completionContinuation: completionContinuation
                    )
                    let session = URLSession(configuration: .ephemeral, delegate: delegate, delegateQueue: nil)
                    let dataTask = session.dataTask(with: request)
                    dataTask.resume()

                    // Ensure cleanup when the stream is cancelled
                    continuation.onTermination = { _ in
                        dataTask.cancel()
                        session.invalidateAndCancel()
                    }

                    var currentEvent = "message"
                    var currentData = ""
                    var currentID: String?
                    var lastKeepaliveLogTime = Date.distantPast

                    let lineStream = sseLines(from: dataStream)
                    for try await line in lineStream {
                        if Task.isCancelled { break }

                        // Keepalive comment — log periodically for liveness
                        if line.hasPrefix(":") {
                            let now = Date()
                            if now.timeIntervalSince(lastKeepaliveLogTime) >= 60 {
                                lastKeepaliveLogTime = now
                                sseLogger.debug("SSE keepalive received path=\(path)")
                            }
                            continue
                        }

                        // Empty line = dispatch accumulated event
                        if line.isEmpty {
                            if !currentData.isEmpty {
                                sseLogger.debug("SSE dispatch event=\(currentEvent) id=\(currentID ?? "nil") bytes=\(currentData.utf8.count)")
                                continuation.yield(SSEEvent(
                                    event: currentEvent,
                                    data: currentData,
                                    id: currentID
                                ))
                                currentEvent = "message"
                                currentData = ""
                                currentID = nil
                            }
                            continue
                        }

                        if line.hasPrefix("event:") {
                            currentEvent = String(line.dropFirst(6)).trimmingCharacters(in: .whitespaces)
                        } else if line.hasPrefix("data:") {
                            let value = String(line.dropFirst(5)).trimmingCharacters(in: .whitespaces)
                            if currentData.isEmpty {
                                currentData = value
                            } else {
                                currentData += "\n" + value
                            }
                        } else if line.hasPrefix("id:") {
                            currentID = String(line.dropFirst(3)).trimmingCharacters(in: .whitespaces)
                        }
                    }

                    // Check the completion result
                    var errored = false
                    for await result in completionStream {
                        if case .failure(let error) = result {
                            errored = true
                            continuation.finish(throwing: error)
                        }
                    }
                    if !errored {
                        sseLogger.info("SSE stream ended path=\(path)")
                        continuation.finish()
                    }
                } catch {
                    sseLogger.error("SSE connection failed path=\(path) error=\(error.localizedDescription)")
                    continuation.finish(throwing: error)
                }
            }
        }
    }

    func sendRequest<T: Decodable>(_ request: URLRequest) async throws -> T {
        let (data, response) = try await session.data(for: request)
        let httpResponse = response as? HTTPURLResponse

        guard let httpResponse else {
            throw ClientError.requestFailed("Relay returned an invalid response.")
        }

        guard (200 ..< 300).contains(httpResponse.statusCode) else {
            let status = httpResponse.statusCode

            if status == 401 {
                if let envelope = try? decoder.decode(ErrorEnvelope.self, from: data) {
                    throw ClientError.unauthorized(envelope.error.message)
                }
                throw ClientError.unauthorized("Unauthorized")
            }

            if let envelope = try? decoder.decode(ErrorEnvelope.self, from: data) {
                throw ClientError.serverError(
                    code: envelope.error.code,
                    message: envelope.error.message,
                    requestId: envelope.error.requestId,
                    status: status
                )
            }

            if let envelope = try? decoder.decode(FastAPIErrorEnvelope.self, from: data) {
                throw ClientError.requestFailed(envelope.detail)
            }

            let hint: String
            switch status {
            case 502:
                hint = "Relay gateway error (502) — the relay cannot reach the connector backend. Check that the connector is running on the host."
            case 503:
                hint = "Relay temporarily unavailable (503) — the service may be restarting. Retry in a moment."
            case 504:
                hint = "Relay gateway timeout (504) — the connector did not respond in time. The host may be overloaded."
            default:
                hint = "Relay request failed with status \(status)."
            }
            throw ClientError.requestFailed(hint)
        }

        return try decoder.decode(Envelope<T>.self, from: data).data
    }
}

// MARK: - DELETE and PATCH support

extension RelayAPIClient {
    func delete<T: Decodable>(
        path: String,
        accessToken: String? = nil
    ) async throws -> T {
        let request = try makeRequest(path: path, method: "DELETE", accessToken: accessToken, body: nil)
        return try await sendRequest(request)
    }

    func patch<Body: Encodable, T: Decodable>(
        path: String,
        body: Body,
        accessToken: String? = nil
    ) async throws -> T {
        let requestBody = try encoder.encode(body)
        let request = try makeRequest(
            path: path,
            method: "PATCH",
            accessToken: accessToken,
            body: requestBody
        )
        return try await sendRequest(request)
    }

    func patchWithHeaders<T: Decodable>(
        path: String,
        body: Data,
        accessToken: String? = nil,
        additionalHeaders: [String: String] = [:]
    ) async throws -> T {
        var request = try makeRequest(
            path: path,
            method: "PATCH",
            accessToken: accessToken,
            body: body
        )
        for (key, value) in additionalHeaders {
            request.setValue(value, forHTTPHeaderField: key)
        }
        return try await sendRequest(request)
    }

    /// Fetches a raw (non-JSON) response body — used for attachment bytes.
    /// Returns the data along with the response's MIME type.
    func getRawData(
        path: String,
        accessToken: String? = nil
    ) async throws -> (data: Data, mimeType: String?) {
        var request = try makeRequest(path: path, method: "GET", accessToken: accessToken, body: nil)
        request.setValue("*/*", forHTTPHeaderField: "Accept")
        let (data, response) = try await session.data(for: request)
        guard let httpResponse = response as? HTTPURLResponse else {
            throw ClientError.requestFailed("Relay returned an invalid response.")
        }
        guard (200 ..< 300).contains(httpResponse.statusCode) else {
            if httpResponse.statusCode == 401 {
                throw ClientError.unauthorized("Unauthorized")
            }
            throw ClientError.requestFailed("Attachment request failed with status \(httpResponse.statusCode).")
        }
        return (data, httpResponse.mimeType)
    }
}
