import Foundation
import os

private let sseLogger = Logger(subsystem: "net.fihonline.herald", category: "SSE")

// MARK: - Backend error fallback

/// Backends that aren't the relay — the connector's HTTP facade, or Caddy itself — answer with a
/// bare `text/plain` reason instead of the relay's error envelope. Surfacing that text beats showing
/// a context-free "Unauthorized". Bounded and non-markup so an HTML error page can't reach the UI.
private func plainTextReason(from data: Data) -> String? {
    guard let text = String(data: data, encoding: .utf8)?
        .trimmingCharacters(in: .whitespacesAndNewlines),
        !text.isEmpty,
        text.count <= 200,
        !text.hasPrefix("<")
    else { return nil }
    return text
}

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
    var lineStart = buffer.startIndex
    while let newlineIndex = buffer[lineStart...].firstIndex(of: 0x0A) {
        // Slice from the CURRENT line start, not the buffer start. Data slices
        // inherit the parent's index space, so using buffer.startIndex here
        // made every line after the first include all preceding lines — which
        // meant no line ever matched "data:" and no line was ever empty, so
        // the SSE dispatch branch never fired and zero events were emitted.
        var lineEnd = newlineIndex
        if lineEnd > lineStart, buffer[lineEnd - 1] == 0x0D {
            lineEnd -= 1                     // strip CR from CRLF endings
        }
        lines.append(String(data: buffer[lineStart ..< lineEnd], encoding: .utf8) ?? "")
        lineStart = newlineIndex + 1
    }
    // Keep unconsumed bytes for the next call
    if lineStart < buffer.endIndex {
        buffer = Data(buffer[lineStart...])
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

    /// Build 32: protocol mismatch detail returned by the connector (HTTP 426).
    /// FastAPI wraps the dict-style detail in {"detail": {...}} — the inner
    /// dict carries the structured fields so the UI can render a typed
    /// compatibility card instead of raw JSON.
    private struct ProtocolMismatchBody: Decodable {
        struct Detail: Decodable {
            let requiredProtocol: Int
            let clientProtocol: Int?
            let message: String?
        }
        let detail: Detail
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
        /// Build 32: protocol version mismatch — the app requires a connector
        /// update.  Carries structured fields so the UI can render a typed
        /// compatibility card instead of a raw JSON dictionary.
        case protocolMismatch(requiredProtocol: Int, clientProtocol: Int, message: String)

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
            case .protocolMismatch(let requiredProtocol, let clientProtocol, let message):
                return message
            }
        }

        /// Structured protocol mismatch info, or nil for other error kinds.
        var protocolMismatchInfo: (required: Int, client: Int, message: String)? {
            if case let .protocolMismatch(required, client, message) = self {
                return (required, client, message)
            }
            return nil
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

    // MARK: - Gateway (non-/v1) requests

    /// POST to a gateway-control endpoint mounted at the host root (`/gw/…`),
    /// not under the `/v1` API prefix.
    func postGateway<Body: Encodable, T: Decodable>(
        path: String,
        body: Body,
        accessToken: String? = nil
    ) async throws -> T {
        let requestBody = try encoder.encode(body)
        let request = try makeGatewayRequest(
            path: path,
            method: "POST",
            accessToken: accessToken,
            body: requestBody
        )
        return try await sendRequest(request)
    }

    /// POST to a gateway-control endpoint with no request body.
    func postGateway<T: Decodable>(
        path: String,
        accessToken: String? = nil
    ) async throws -> T {
        let request = try makeGatewayRequest(path: path, method: "POST", accessToken: accessToken, body: nil)
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

        return try buildRequest(url: url, method: method, accessToken: accessToken, body: body)
    }

    /// Constructs a request targeting the gateway control plane.
    ///
    /// Gateway routes are mounted at `/gw/…` on the host root, NOT under `/v1`.
    /// This method strips the API prefix (e.g. `/v1`) from the base URL before
    /// appending the gateway path, so `path: "gw/restart"` resolves to
    /// `https://host:8010/gw/restart` instead of `…/v1/gw/restart`.
    func makeGatewayRequest(
        path: String,
        method: String,
        accessToken: String?,
        body: Data?
    ) throws -> URLRequest {
        let path = path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        var baseURLString = baseURLProvider().trimmingCharacters(in: CharacterSet(charactersIn: "/"))

        // Strip the API version prefix (e.g. /v1, /v2) so gateway routes
        // resolve against the host root.
        if let lastSlash = baseURLString.lastIndex(of: "/") {
            let lastComponent = baseURLString[baseURLString.index(after: lastSlash)...]
            if lastComponent.hasPrefix("v") && lastComponent.allSatisfy({ $0.isNumber || $0 == "v" }) {
                baseURLString = String(baseURLString[..<lastSlash])
            }
        }

        guard let url = URL(string: "\(baseURLString)/\(path)") else {
            throw ClientError.invalidURL(baseURLString)
        }

        return try buildRequest(url: url, method: method, accessToken: accessToken, body: body)
    }

    private func buildRequest(
        url: URL,
        method: String,
        accessToken: String?,
        body: Data?
    ) throws -> URLRequest {
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.httpBody = body

        if let accessToken, !accessToken.isEmpty {
            request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
        }

        request.setValue(UUID().uuidString, forHTTPHeaderField: "X-Request-ID")
        request.timeoutInterval = 60  // POST /messages can take 30-45s for connector to respond

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
            throw Self.decodeClientError(data: data, status: httpResponse.statusCode, decoder: decoder)
        }

        return try decoder.decode(Envelope<T>.self, from: data).data
    }

    /// Builds the typed `ClientError` for a non-2xx response, shared by the
    /// envelope-decoding request path and the gateway-control path.
    private static func decodeClientError(data: Data, status: Int, decoder: JSONDecoder) -> ClientError {
        if status == 401 {
            if let envelope = try? decoder.decode(ErrorEnvelope.self, from: data) {
                return .unauthorized(envelope.error.message)
            }
            if let envelope = try? decoder.decode(FastAPIErrorEnvelope.self, from: data) {
                return .unauthorized(envelope.detail)
            }
            return .unauthorized(plainTextReason(from: data) ?? "Unauthorized")
        }

        // Build 32: detect protocol version mismatch (HTTP 426).
        // The connector returns structured fields so the UI can render a
        // typed "Connector update required" card instead of raw JSON.
        if status == 426 {
            if let mismatch = try? decoder.decode(ProtocolMismatchBody.self, from: data) {
                return .protocolMismatch(
                    requiredProtocol: mismatch.detail.requiredProtocol,
                    clientProtocol: mismatch.detail.clientProtocol ?? 0,
                    message: mismatch.detail.message ?? "This Herald build requires a connector update."
                )
            }
            // Fallback: still show a clean message, not raw JSON
            return .protocolMismatch(
                requiredProtocol: 0, clientProtocol: 0,
                message: "This Herald build requires a connector update. Please update the Herald connector to continue."
            )
        }

        if let envelope = try? decoder.decode(ErrorEnvelope.self, from: data) {
            return .serverError(
                code: envelope.error.code,
                message: envelope.error.message,
                requestId: envelope.error.requestId,
                status: status
            )
        }

        if let envelope = try? decoder.decode(FastAPIErrorEnvelope.self, from: data) {
            return .requestFailed(envelope.detail)
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
        return .requestFailed(plainTextReason(from: data) ?? hint)
    }

    // MARK: - Gateway control-plane requests (Build 33 restart flow)

    /// GET a gateway-control endpoint, decoding the payload raw-first with the
    /// relay envelope (`{"data": …}`) as fallback. The connector's envelope
    /// middleware passes JSON bodies containing an `error` key through
    /// un-enveloped — restart-operation bodies always carry one (null while
    /// healthy) so they arrive raw; preflight and health bodies are enveloped.
    func getGatewayControl<T: Decodable>(
        path: String,
        accessToken: String? = nil
    ) async throws -> T {
        let request = try makeGatewayRequest(path: path, method: "GET", accessToken: accessToken, body: nil)
        return try await sendGatewayControl(request)
    }

    /// Outcome of a `POST /gw/restart` call.
    enum GatewayRestartResult: Sendable {
        /// Build 33 connector: an operation with phase/checks that can be
        /// polled via `GET /gw/restart/{operationId}`.
        case operation(RestartOperation)
        /// Pre-Build-33 connector: legacy `{restarting: …}` shape, no
        /// operation tracking. Verification falls back to `GET /gw/health`.
        case legacy(RestartResponse)
    }

    /// POST `/gw/restart` with an optional `Idempotency-Key` header.
    ///
    /// Decodes, in order: the new raw operation shape (fixture
    /// `operation_accepted.json`), the enveloped operation shape, the legacy
    /// `RestartResponse` shape (raw or enveloped) — the pre-Build-33 connector
    /// still accepts old-style calls without an Idempotency-Key, and its
    /// `{restarting: …}` answer must not crash the new flow.
    ///
    /// A 409 conflict whose body is an existing operation (fixture
    /// `conflict_in_progress.json`) is returned as `.operation` — a restart is
    /// already in progress and the caller should join and poll it. A 409 with
    /// the standard error envelope means the confirmed preflight is stale and
    /// surfaces as `ClientError.serverError(…, status: 409)`.
    func postGatewayRestart(
        body: RestartSubmission,
        accessToken: String?,
        idempotencyKey: String?
    ) async throws -> GatewayRestartResult {
        let requestBody = try encoder.encode(body)
        var request = try makeGatewayRequest(
            path: "gw/restart",
            method: "POST",
            accessToken: accessToken,
            body: requestBody
        )
        if let idempotencyKey, !idempotencyKey.isEmpty {
            request.setValue(idempotencyKey, forHTTPHeaderField: "Idempotency-Key")
        }

        let (data, response) = try await session.data(for: request)
        guard let httpResponse = response as? HTTPURLResponse else {
            throw ClientError.requestFailed("Relay returned an invalid response.")
        }
        let status = httpResponse.statusCode

        if (200 ..< 300).contains(status) {
            if let operation = try? decoder.decode(RestartOperation.self, from: data) {
                return .operation(operation)
            }
            if let enveloped = try? decoder.decode(Envelope<RestartOperation>.self, from: data) {
                return .operation(enveloped.data)
            }
            // Legacy shape: the old connector returns `{restarting: Bool, …}`.
            if let legacy = try? decoder.decode(RestartResponse.self, from: data) {
                return .legacy(legacy)
            }
            if let enveloped = try? decoder.decode(Envelope<RestartResponse>.self, from: data) {
                return .legacy(enveloped.data)
            }
            throw ClientError.requestFailed("Unrecognized restart response from the gateway.")
        }

        if status == 409 {
            // Restart already in progress — the connector returns the existing
            // operation body (it carries an `error` key, so the middleware
            // passes it through un-enveloped).
            if let operation = try? decoder.decode(RestartOperation.self, from: data) {
                return .operation(operation)
            }
        }

        throw Self.decodeClientError(data: data, status: status, decoder: decoder)
    }

    /// Sends a gateway-control request and decodes the body with
    /// `decodeGatewayPayload` (envelope-aware; see `getGatewayControl`).
    private func sendGatewayControl<T: Decodable>(_ request: URLRequest) async throws -> T {
        let (data, response) = try await session.data(for: request)
        guard let httpResponse = response as? HTTPURLResponse else {
            throw ClientError.requestFailed("Relay returned an invalid response.")
        }
        let status = httpResponse.statusCode

        if (200 ..< 300).contains(status) {
            return try Self.decodeGatewayPayload(data, as: T.self, decoder: decoder)
        }

        if status == 409 {
            // A 409 restart conflict carries the existing operation body raw
            // (operation dicts pass the middleware un-enveloped).
            if let existing = try? Self.decodeGatewayPayload(data, as: T.self, decoder: decoder) {
                return existing
            }
        }

        throw Self.decodeClientError(data: data, status: status, decoder: decoder)
    }

    /// Decodes a gateway-control payload: enveloped (`{"data": …}`) when the
    /// body carries a top-level `data` key, raw otherwise. The connector's
    /// middleware wraps every JSON body except payloads containing an `error`
    /// key (restart-operation dicts) — so top-level `data` presence is the
    /// reliable discriminator. A raw-first `try?` heuristic is NOT safe here:
    /// all-optional DTOs (GatewayHealth) would decode the envelope vacuously,
    /// returning nil-everywhere objects and silently dropping the real data.
    private static func decodeGatewayPayload<T: Decodable>(
        _ data: Data,
        as type: T.Type,
        decoder: JSONDecoder
    ) throws -> T {
        if let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           dict["data"] != nil {
            return try decoder.decode(Envelope<T>.self, from: data).data
        }
        return try decoder.decode(T.self, from: data)
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
                if let text = plainTextReason(from: data) {
                    throw ClientError.unauthorized(text)
                }
                throw ClientError.unauthorized("Unauthorized")
            }
            throw ClientError.requestFailed(plainTextReason(from: data) ?? "Attachment request failed with status \(httpResponse.statusCode).")
        }
        return (data, httpResponse.mimeType)
    }
}
