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

    init(session: URLSession = .init(configuration: .default)) {
        self.session = session
    }

    func connect(url: URL) async throws {
        let task = session.webSocketTask(with: url)
        task.resume()
        self.task = task
    }

    func send(_ data: Data) async throws {
        guard let task else { throw NativeGatewayTransportError.notConnected }
        try await task.send(.data(data))
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
        task?.cancel(with: .goingAway, reason: nil)
        task = nil
    }
}

enum NativeGatewayTransportError: Error {
    case notConnected
}
