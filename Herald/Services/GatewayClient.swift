import Foundation
import os

// MARK: - JSON-RPC 2.0 Gateway Client

/// Native Swift gateway client for the Hermes JSON-RPC 2.0 WebSocket protocol.
///
/// This client connects to the relay's WebSocket endpoint and implements the
/// same protocol as the Desktop's `JsonRpcGatewayClient`. It is transport-only:
/// it does not make model, tool, reasoning, or queue decisions.
///
/// Protocol reference: `apps/shared/src/json-rpc-gateway.ts` in hermes-agent.
actor GatewayClient {
    private static let logger = Logger(subsystem: "net.fihonline.herald", category: "GatewayClient")

    // MARK: - Connection state

    enum ConnectionState: Sendable, Equatable {
        case disconnected
        case connecting
        case connected
        case reconnecting(attempt: Int)

        static func == (lhs: ConnectionState, rhs: ConnectionState) -> Bool {
            switch (lhs, rhs) {
            case (.disconnected, .disconnected): return true
            case (.connecting, .connecting): return true
            case (.connected, .connected): return true
            case (.reconnecting(let a), .reconnecting(let b)): return a == b
            default: return false
            }
        }
    }

    // MARK: - Configuration

    struct Configuration: Sendable {
        let url: URL
        let authToken: String?
        let connectTimeoutSeconds: Double
        let requestTimeoutSeconds: Double
        let maxReconnectAttempts: Int
        let baseReconnectDelaySeconds: Double

        /// Create configuration from the relay's base URL.
        ///
        /// The relay's REST API is at `<base>/v1/...` but the WebSocket
        /// endpoint is at `<host>/api/ws` (not under /v1).
        static func from(relayBaseURL: String, authToken: String? = nil) -> Configuration? {
            guard let baseURL = URL(string: relayBaseURL) else { return nil }
            // Strip /v1 suffix if present, then append /api/ws
            var host = baseURL.absoluteString
            if host.hasSuffix("/v1") {
                host = String(host.dropLast(3))
            } else if host.hasSuffix("/v1/") {
                host = String(host.dropLast(4))
            }
            // Convert https:// to wss:// and http:// to ws://
            if host.hasPrefix("https://") {
                host = "wss://" + host.dropFirst(8)
            } else if host.hasPrefix("http://") {
                host = "ws://" + host.dropFirst(7)
            }
            guard let wsURL = URL(string: "\(host)/api/ws") else { return nil }
            return Configuration(
                url: wsURL,
                authToken: authToken,
                connectTimeoutSeconds: 15,
                requestTimeoutSeconds: 60,
                maxReconnectAttempts: 10,
                baseReconnectDelaySeconds: 1
            )
        }

        static let `default` = Configuration(
            url: URL(string: "wss://localhost:8010/api/ws")!,
            authToken: nil,
            connectTimeoutSeconds: 15,
            requestTimeoutSeconds: 60,
            maxReconnectAttempts: 10,
            baseReconnectDelaySeconds: 1
        )
    }

    // MARK: - State

    private let config: Configuration
    private var webSocketTask: URLSessionWebSocketTask?
    private var connectionState: ConnectionState = .disconnected
    private var nextRequestID: Int = 1
    private var pendingRequests: [Int: CheckedContinuation<GatewayResponse, any Error>] = [:]
    private var eventContinuation: AsyncStream<GatewayEvent>.Continuation?
    private var reconnectAttempt: Int = 0
    private var isCancelled: Bool = false

    // MARK: - Event stream

    /// Async stream of gateway events. Events are session-keyed and must be
    /// routed by the consumer to the correct session projection store.
    nonisolated let events: AsyncStream<GatewayEvent>

    private let eventsContinuation: AsyncStream<GatewayEvent>.Continuation

    // MARK: - Init

    init(config: Configuration = .default) {
        self.config = config
        let (stream, continuation) = AsyncStream<GatewayEvent>.makeStream()
        self.events = stream
        self.eventsContinuation = continuation
    }

    // MARK: - Connection

    /// Connect to the gateway WebSocket. Reconnects automatically on failure.
    func connect() async throws {
        guard connectionState == .disconnected else { return }

        connectionState = .connecting
        isCancelled = false

        try await establishConnection()
    }

    /// Disconnect from the gateway. Cancels all pending requests.
    func disconnect() {
        isCancelled = true
        connectionState = .disconnected
        webSocketTask?.cancel(with: .goingAway, reason: nil)
        webSocketTask = nil
        rejectAllPending(GatewayError.connectionClosed)
        eventsContinuation.finish()
    }

    // MARK: - Request/response

    /// Send a JSON-RPC request and await the correlated response.
    func request(method: String, params: [String: Any]) async throws -> GatewayResponse {
        guard connectionState == .connected else {
            throw GatewayError.notConnected
        }

        let id = allocateRequestID()
        let frame: [String: Any] = [
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        ]

        let data = try JSONSerialization.data(withJSONObject: frame)
        let text = String(data: data, encoding: .utf8)!
        let message = URLSessionWebSocketTask.Message.string(text)

        // Send and await response with timeout
        try await sendWebSocketMessage(message)

        return try await withThrowingTaskGroup(of: GatewayResponse.self) { group in
            group.addTask { [weak self] in
                guard let self else { throw GatewayError.connectionClosed }
                return try await self.awaitResponse(id: id)
            }

            group.addTask { [weak self] in
                guard let self else { throw GatewayError.connectionClosed }
                let timeout = self.config.requestTimeoutSeconds
                try await Task.sleep(nanoseconds: UInt64(timeout * 1_000_000_000))
                throw GatewayError.requestTimeout(id: id)
            }

            let result = try await group.next()!
            group.cancelAll()
            return result
        }
    }

    // MARK: - WebSocket receive loop

    private func startReceiveLoop() async {
        while !isCancelled, let task = webSocketTask {
            do {
                let message = try await task.receive()
                switch message {
                case .string(let text):
                    await handleIncomingText(text)
                case .data(let data):
                    if let text = String(data: data, encoding: .utf8) {
                        await handleIncomingText(text)
                    }
                @unknown default:
                    Self.logger.warning("Unknown WebSocket message type")
                }
            } catch {
                if !isCancelled {
                    Self.logger.error("WebSocket receive error: \(error.localizedDescription)")
                    await handleDisconnect()
                }
                break
            }
        }
    }

    private func handleIncomingText(_ text: String) async {
        guard let data = text.data(using: .utf8) else { return }

        do {
            let frame = try JSONDecoder().decode(GatewayFrame.self, from: data)
            switch frame {
            case .response(let response):
                await handleResponse(response)
            case .event(let event):
                await handleEvent(event)
            }
        } catch {
            Self.logger.error("Failed to decode gateway frame: \(error.localizedDescription)")
        }
    }

    private func handleResponse(_ response: GatewayResponse) async {
        guard let continuation = pendingRequests.removeValue(forKey: response.id) else {
            Self.logger.warning("Response for unknown request ID: \(response.id)")
            return
        }
        continuation.resume(returning: response)
    }

    private func handleEvent(_ event: GatewayEvent) async {
        eventsContinuation.yield(event)
    }

    // MARK: - Connection management

    private func establishConnection() async throws {
        // Construct URL with token query parameter if available
        var url = config.url
        if let token = config.authToken {
            var components = URLComponents(url: url, resolvingAgainstBaseURL: false)
            var queryItems = components?.queryItems ?? []
            queryItems.append(URLQueryItem(name: "token", value: token))
            components?.queryItems = queryItems
            if let tokenURL = components?.url {
                url = tokenURL
            }
        }

        var request = URLRequest(url: url)
        request.timeoutInterval = config.connectTimeoutSeconds

        let session = URLSession(configuration: .default)
        let task = session.webSocketTask(with: request)
        self.webSocketTask = task
        task.resume()

        // Verify connection with a ping
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
            task.sendPing { error in
                if let error {
                    continuation.resume(throwing: error)
                } else {
                    continuation.resume()
                }
            }
        }

        connectionState = .connected
        reconnectAttempt = 0
        Self.logger.info("Connected to gateway at \(self.config.url)")

        // Start receive loop in background
        Task { await startReceiveLoop() }
    }

    private func handleDisconnect() async {
        guard !isCancelled else { return }

        connectionState = .disconnected
        rejectAllPending(GatewayError.connectionClosed)

        guard reconnectAttempt < config.maxReconnectAttempts else {
            Self.logger.error("Max reconnect attempts reached")
            return
        }

        reconnectAttempt += 1
        connectionState = .reconnecting(attempt: reconnectAttempt)

        let delay = config.baseReconnectDelaySeconds * pow(2.0, Double(reconnectAttempt - 1))
        Self.logger.info("Reconnecting in \(delay)s (attempt \(self.reconnectAttempt))")
        try? await Task.sleep(nanoseconds: UInt64(delay * 1_000_000_000))

        guard !isCancelled else { return }

        do {
            try await establishConnection()
        } catch {
            Self.logger.error("Reconnect failed: \(error.localizedDescription)")
            await handleDisconnect()
        }
    }

    // MARK: - Helpers

    private func allocateRequestID() -> Int {
        let id = nextRequestID
        nextRequestID += 1
        return id
    }

    private func awaitResponse(id: Int) async throws -> GatewayResponse {
        try await withCheckedThrowingContinuation { continuation in
            pendingRequests[id] = continuation
        }
    }

    private func sendWebSocketMessage(_ message: URLSessionWebSocketTask.Message) async throws {
        guard let task = webSocketTask else {
            throw GatewayError.notConnected
        }
        try await task.send(message)
    }

    private func rejectAllPending(_ error: any Error) {
        for (_, continuation) in pendingRequests {
            continuation.resume(throwing: error)
        }
        pendingRequests.removeAll()
    }
}

// MARK: - Gateway Frame Types

/// A decoded gateway frame — either a response or an event.
enum GatewayFrame: Decodable, Sendable {
    case response(GatewayResponse)
    case event(GatewayEvent)

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let method = try container.decodeIfPresent(String.self, forKey: .method)
        if method == "event" {
            self = .event(try GatewayEvent(from: decoder))
        } else {
            self = .response(try GatewayResponse(from: decoder))
        }
    }

    private enum CodingKeys: String, CodingKey {
        case method
    }
}

/// A JSON-RPC 2.0 response with correlated ID.
struct GatewayResponse: Decodable, Sendable {
    let id: Int
    let result: GatewayValue?
    let error: GatewayErrorResponse?

    var isSuccess: Bool { error == nil }

    struct GatewayErrorResponse: Decodable, Sendable {
        let code: Int
        let message: String
    }
}

/// A JSON-RPC 2.0 push event from the gateway.
struct GatewayEvent: Decodable, Sendable {
    let type: String
    let sessionID: String?
    let payload: [String: GatewayValue]?

    init(from decoder: any Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let params = try container.decode(Params.self, forKey: .params)
        self.type = params.type
        self.sessionID = params.sessionID
        self.payload = params.payload
    }

    private enum CodingKeys: String, CodingKey {
        case params
    }

    private struct Params: Decodable {
        let type: String
        let sessionID: String?
        let payload: [String: GatewayValue]?

        enum CodingKeys: String, CodingKey {
            case type
            case sessionID = "session_id"
            case payload
        }
    }
}

/// A type-erased JSON value for gateway payloads.
enum GatewayValue: Codable, Sendable {
    case string(String)
    case int(Int)
    case double(Double)
    case bool(Bool)
    case dict([String: GatewayValue])
    case array([GatewayValue])
    case null

    init(from decoder: any Decoder) throws {
        let container = try decoder.singleValueContainer()
        if let v = try? container.decode(String.self) { self = .string(v) }
        else if let v = try? container.decode(Int.self) { self = .int(v) }
        else if let v = try? container.decode(Double.self) { self = .double(v) }
        else if let v = try? container.decode(Bool.self) { self = .bool(v) }
        else if let v = try? container.decode([String: GatewayValue].self) { self = .dict(v) }
        else if let v = try? container.decode([GatewayValue].self) { self = .array(v) }
        else { self = .null }
    }

    func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let v): try container.encode(v)
        case .int(let v): try container.encode(v)
        case .double(let v): try container.encode(v)
        case .bool(let v): try container.encode(v)
        case .dict(let v): try container.encode(v)
        case .array(let v): try container.encode(v)
        case .null: try container.encodeNil()
        }
    }
}

/// Known gateway event types.
enum GatewayEventType: String, Sendable {
    case gatewayReady = "gateway.ready"
    case sessionInfo = "session.info"
    case messageStart = "message.start"
    case messageDelta = "message.delta"
    case messageInterim = "message.interim"
    case messageComplete = "message.complete"
    case thinkingDelta = "thinking.delta"
    case reasoningDelta = "reasoning.delta"
    case reasoningAvailable = "reasoning.available"
    case statusUpdate = "status.update"
    case toolStart = "tool.start"
    case toolProgress = "tool.progress"
    case toolComplete = "tool.complete"
    case toolGenerating = "tool.generating"
    case error = "error"
    case sessionsChanged = "sessions.changed"
}

// MARK: - Gateway Errors

enum GatewayError: Error, Sendable {
    case notConnected
    case connectTimeout
    case connectionClosed
    case requestTimeout(id: Int)
    case decodingError(String)
}
