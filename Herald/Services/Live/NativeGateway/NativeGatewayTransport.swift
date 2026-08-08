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

    init(session: URLSession = .init(configuration: .default)) {
        self.session = session
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
                guard !Task.isCancelled, let task = self?.task else { return }
                await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                    task.sendPing { _ in continuation.resume() }
                }
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
