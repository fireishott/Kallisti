import Foundation
import OSLog

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
        // Build 131.2: tear down any previous socket BEFORE installing the
        // new one. URLSessionWebSocketTask has no automatic replacement
        // semantics - overwriting self.task without cancel() left the old
        // socket alive server-side, so a reconnect opened a SECOND parallel
        // connection. The two sockets fought over the session (gateway saw
        // peer pairs accepted seconds apart, one dying with 1006, the other
        // with send_failed_after_response) - the iPhone flapping-connection
        // storm. The iPad reconnects rarely, so it never hit the race.
        if let previous = task {
            previous.cancel(with: .goingAway, reason: nil)
            task = nil
        }
        let task = session.webSocketTask(with: url)
        // Build 57 (root cause): URLSessionWebSocketTask.maximumMessageSize
        // defaults to 1MB. The gateway's tool.complete frame carries the
        // FULL tool result, and image generation returns 2.5-4MB base64
        // payloads (agent saw LEN 4030283 / LEN 2665324). A frame over the
        // 1MB cap makes receive() throw -> socket dies mid-turn -> the
        // terminal message.complete never arrives -> app sits on "Thinking"
        // until the stall watchdog fires and the user re-asks (double
        // billing). Electron on the same 9119 endpoint never hit this
        // because Node's WS has no message-size cap. Raise it well above
        // the largest plausible tool result (gateway emits one-shot base64
        // file.attach frames up to ~16MiB per the uvicorn comment, so 64MB
        // is a safe ceiling).
        task.maximumMessageSize = 64 * 1024 * 1024
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
                    // One failed ping is NOT proof the socket is dead - on
                    // cellular a slow pong (radio stall, proxy hiccup) can
                    // exceed the ping timeout while the socket recovers.
                    // Only after three CONSECUTIVE failed pings do we tear
                    // the socket down; anything less would kill a healthy
                    // socket every ~60s (3 keepalive ticks).
                    self?.consecutivePingFailures += 1
                    if (self?.consecutivePingFailures ?? 0) < Self.maxPingFailures {
                        Self.logger.info("keepalive ping failed (\(self?.consecutivePingFailures ?? 0)), keeping socket")
                        continue
                    }
                    // Three failures in a row: socket is genuinely gone.
                    // Tear it down so the client's receive loop errors and
                    // the reconnect loop kicks in instead of leaving a
                    // phantom connection.
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
                self?.consecutivePingFailures = 0
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
    ///
    /// Build 53: the callback is not guaranteed to fire for a half-dead
    /// socket (TCP alive, no response). Without a timeout the keepalive
    /// Task hangs forever on this continuation, the phantom socket is never
    /// torn down, and the app sits "connected" while every request fails
    /// with transportClosed - the constant-disconnect experience. The gate
    /// makes a late callback a no-op, so racing the timeout is safe.
    ///
    /// Build 55: 8s was too aggressive — the gateway's event loop can stall
    /// for 10-20s+ during heavy turns (long tool calls, image generation),
    /// and the client was killing a HEALTHY socket whose pong merely arrived
    /// late, then reconnecting into churn (which also flashed the loading
    /// surface and ate typed text). The server itself reaps dead peers at
    /// 20s (ws_ping_interval/ws_ping_timeout on 0.0.0.0 binds), so the
    /// client must be strictly MORE tolerant: 45s gives stalled loops room
    /// to breathe while still tearing down genuinely dead sockets.
    private static let pingTimeout: Duration = .seconds(45)
    private static let logger = Logger(subsystem: "net.fihonline.kallisti", category: "WSKeepalive")
    /// Build 131.16: consecutive failed pings before the keepalive tears down
    /// the socket. A SINGLE slow pong on cellular (radio stall, proxy hiccup)
    /// must not kill a healthy socket - that was the 60s death pattern (socket
    /// cancelled on the 3rd keepalive tick). Three consecutive failures means
    /// the socket is genuinely gone; the receive loop will surface the error
    /// and the reconnect path takes over.
    private var consecutivePingFailures = 0
    private static let maxPingFailures = 3

    private static func oneShotSendPing(_ task: URLSessionWebSocketTask) async -> Bool {
        await withCheckedContinuation { continuation in
            let gate = PingContinuationGate(continuation)
            task.sendPing { error in
                gate.resume(error == nil)
            }
            Task {
                try? await Task.sleep(for: Self.pingTimeout)
                gate.resume(false)
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
