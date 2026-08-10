import Foundation

/// Raw byte-frame transport, abstracted so NativeGatewayClient is testable
/// without a real socket. One implementation talks to a live WebSocket;
/// tests use MockNativeGatewayTransport.
protocol NativeGatewayTransport: Sendable {
    func connect(url: URL) async throws
    func send(_ data: Data) async throws
    /// Each call returns a fresh stream of inbound frames from this point on.
    func receive() -> AsyncThrowingStream<Data, Error>
    func close() async
}

final class URLSessionWebSocketTransport: NativeGatewayTransport, @unchecked Sendable {
    private var task: URLSessionWebSocketTask?
    private let session: URLSession
    private var keepaliveTask: Task<Void, Never>?

    /// Idle WebSockets get reaped by the reverse proxy and by NAT on
    /// cellular. Nothing in the JSON-RPC protocol is periodic, so without
    /// this a quiet connection silently dies and every later request fails
    /// with transportClosed.
    private static let keepaliveInterval: Duration = .seconds(20)

    init(session: URLSession? = nil) {
        if let session {
            self.session = session
        } else {
            // Default URLSessionConfiguration.timeoutIntervalForRequest is 60s.
            // For WebSocket tasks that acts as an idle deadline: if the server
            // sends nothing within 60s (normal between requests), the task
            // fails and the socket dies even though the keepalive pings are
            // flowing. Disable it -- liveness is handled by the keepalive.
            let config = URLSessionConfiguration.default
            config.timeoutIntervalForRequest = 0
            config.timeoutIntervalForResource = 0
            config.waitsForConnectivity = true
            config.connectionProxyDictionary = [:]
            self.session = URLSession(configuration: config)
        }
    }

    func connect(url: URL) async throws {
        let task = session.webSocketTask(with: url)
        task.resume()
        self.task = task
        startKeepalive()
    }

    private func startKeepalive() {
        keepaliveTask?.cancel()
        keepaliveTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: Self.keepaliveInterval)
                guard !Task.isCancelled else { return }
                // Capture the live task BEFORE the ping so a reconnect that
                // replaces self.task during the await doesn't get nulled.
                guard let task = self?.task else { return }
                let pingOK = await Self.oneShotSendPing(task)
                if !pingOK {
                    // Socket is gone but receive() may never surface the error
                    // (iOS suspends WS tasks silently). Tear it down so the
                    // client's receive loop errors and the reconnect loop
                    // kicks in instead of leaving a phantom connection.
                    //
                    // Only clear self.task if it's STILL the same task that
                    // failed the ping - a concurrent reconnect may have already
                    // installed a fresh socket, and nulling it would kill the
                    // new connection.
                    task.cancel(with: .goingAway, reason: nil)
                    if self?.task === task {
                        self?.task = nil
                    }
                    return
                }
            }
        }
    }

    private final class PingContinuationGate: @unchecked Sendable {
        private let lock = NSLock()
        private var continuation: CheckedContinuation<Bool, Never>?

        init(_ continuation: CheckedContinuation<Bool, Never>) {
            self.continuation = continuation
        }

        func resume(_ value: Bool) {
            lock.lock()
            let pending = continuation
            continuation = nil
            lock.unlock()
            pending?.resume(returning: value)
        }
    }

    /// Bridges sendPing's callback while guaranteeing one continuation resume
    /// even if URLSession invokes the callback more than once.
    private static func oneShotSendPing(_ task: URLSessionWebSocketTask) async -> Bool {
        await withCheckedContinuation { continuation in
            let gate = PingContinuationGate(continuation)
            task.sendPing { error in
                gate.resume(error == nil)
            }
        }
    }

    func send(_ data: Data) async throws {
        guard let task else { throw NativeGatewayTransportError.notConnected }
        // The gateway's tui_gateway reads inbound frames with receive_text(),
        // so we must send TEXT frames. Sending binary (URLSessionWebSocketTask
        // message .data) made the server raise "KeyError: 'text'" and drop the
        // socket the instant our first request arrived (session.list) -- the
        // connection opened and authenticated, then died with messages=0 on
        // the server, which the app surfaced as "couldn't reach the gateway".
        guard let text = String(data: data, encoding: .utf8) else {
            throw NativeGatewayTransportError.notTextEncodable
        }
        try await task.send(.string(text))
    }

    func receive() -> AsyncThrowingStream<Data, Error> {
        AsyncThrowingStream { continuation in
            let task = self.task
            Task {
                guard let task else {
                    continuation.finish(throwing: NativeGatewayTransportError.notConnected)
                    return
                }
                while true {
                    do {
                        let message = try await task.receive()
                        switch message {
                        case .data(let d): continuation.yield(d)
                        case .string(let s): continuation.yield(Data(s.utf8))
                        @unknown default: break
                        }
                    } catch {
                        continuation.finish(throwing: error)
                        return
                    }
                }
            }
        }
    }

    func close() async {
        keepaliveTask?.cancel()
        keepaliveTask = nil
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
    }
}

enum NativeGatewayTransportError: Error {
    case notConnected
    case notTextEncodable
}
