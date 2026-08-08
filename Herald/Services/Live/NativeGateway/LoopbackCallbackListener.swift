import Foundation
import Network

/// One-shot local HTTP listener for RFC 8252 loopback redirect capture.
///
/// Binds to `127.0.0.1:<os-assigned-port>`, accepts one connection, parses
/// the request line for `?code=...&state=...`, responds with minimal HTML,
/// and shuts down.
final class LoopbackCallbackListener: @unchecked Sendable {
    struct Callback: Sendable {
        let code: String
        let state: String
    }

    private(set) var port: UInt16 = 0
    private var listener: NWListener?

    func start() async throws {
        let params = NWParameters.tcp
        params.requiredLocalEndpoint = NWEndpoint.hostPort(host: "127.0.0.1", port: .any)
        let listener = try NWListener(using: params)
        self.listener = listener

        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            listener.stateUpdateHandler = { [weak self] state in
                switch state {
                case .ready:
                    self?.port = listener.port?.rawValue ?? 0
                    continuation.resume()
                case .failed(let error):
                    continuation.resume(throwing: error)
                default:
                    break
                }
            }
            listener.start(queue: .main)
        }
    }

    func waitForCallback() async throws -> Callback {
        guard let listener else { throw NativeAuthError.listenerSetupFailed }
        return try await withCheckedThrowingContinuation { continuation in
            listener.newConnectionHandler = { connection in
                Self.handle(connection, listener: listener, continuation: continuation)
            }
        }
    }

    private static func handle(
        _ connection: NWConnection,
        listener: NWListener,
        continuation: CheckedContinuation<Callback, Error>
    ) {
        connection.start(queue: .main)
        connection.receive(minimumIncompleteLength: 1, maximumLength: 8192) { data, _, _, _ in
            guard let data, let request = String(data: data, encoding: .utf8),
                  let requestLine = request.split(separator: "\r\n").first,
                  let path = requestLine.split(separator: " ").dropFirst().first,
                  let components = URLComponents(string: "http://127.0.0.1" + path),
                  let code = components.queryItems?.first(where: { $0.name == "code" })?.value,
                  let state = components.queryItems?.first(where: { $0.name == "state" })?.value
            else {
                continuation.resume(throwing: NativeAuthError.callbackParseFailed)
                connection.cancel()
                return
            }
            let body = "<html><body>Signed in \u{2014} you can close this tab.</body></html>"
            let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: \(body.utf8.count)\r\nConnection: close\r\n\r\n\(body)"
            connection.send(content: Data(response.utf8), completion: .contentProcessed { _ in
                connection.cancel()
                listener.cancel()
            })
            continuation.resume(returning: Callback(code: code, state: state))
        }
    }

    func stop() {
        listener?.cancel()
    }
}

extension Data {
    func base64URLEncodedString() -> String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
