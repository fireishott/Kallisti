import Foundation
import Darwin

/// One-shot local HTTP listener for RFC 8252 loopback redirect capture.
///
/// Binds `127.0.0.1:<os-assigned-port>`, accepts one connection, parses the
/// request line for `?code=...&state=...`, responds with minimal HTML, and
/// shuts down.
///
/// Implemented on plain BSD sockets rather than Network.framework: every
/// `NWListener` variant tried (requiredLocalEndpoint with `.any` or a
/// concrete port, and the plain `NWListener(using:on:)` initializer) fails
/// with NWError 22 / EINVAL, which is what produced the "Hermes sign-in
/// failed ... Invalid argument" the user hit. Verified empirically against
/// a POSIX-bind control that succeeded in the same process, so this is an
/// NWListener problem, not a sandbox or permissions one. Sockets are also
/// the more honest fit here — this needs exactly one blocking accept on
/// loopback, none of what NWListener adds.
final class LoopbackCallbackListener: @unchecked Sendable {
    struct Callback: Sendable {
        let code: String
        let state: String
    }

    private(set) var port: UInt16 = 0

    private var listenFD: Int32 = -1
    private let queue = DispatchQueue(label: "net.fihonline.kallisti.loopback-oauth")
    private let lock = NSLock()
    private var didResume = false
    /// Internal access for testing the stop-continuation interaction.
    internal var pendingContinuation: CheckedContinuation<Callback, Error>?

    func start() async throws {
        let fd = socket(AF_INET, SOCK_STREAM, 0)
        guard fd >= 0 else { throw NativeAuthError.listenerSetupFailed }

        var reuse: Int32 = 1
        setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &reuse, socklen_t(MemoryLayout<Int32>.size))

        var addr = sockaddr_in()
        addr.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_port = 0  // 0 = let the kernel assign a free port
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")  // loopback only, never all interfaces

        let didBind = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard didBind == 0, listen(fd, 1) == 0 else {
            close(fd)
            throw NativeAuthError.listenerSetupFailed
        }

        // Read back whichever port the kernel picked — it goes in the
        // redirect_uri, so we need the real value, not the 0 we asked for.
        var bound = sockaddr_in()
        var length = socklen_t(MemoryLayout<sockaddr_in>.size)
        let didReadName = withUnsafeMutablePointer(to: &bound) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                getsockname(fd, $0, &length)
            }
        }
        guard didReadName == 0 else {
            close(fd)
            throw NativeAuthError.listenerSetupFailed
        }

        listenFD = fd
        port = UInt16(bigEndian: bound.sin_port)
    }

    func waitForCallback() async throws -> Callback {
        guard listenFD >= 0 else { throw NativeAuthError.listenerSetupFailed }
        return try await withCheckedThrowingContinuation { continuation in
            lock.lock()
            pendingContinuation = continuation
            lock.unlock()
            queue.async { [weak self] in
                self?.acceptOne()
            }
        }
    }

    /// Blocking accept + read + reply, on the private queue. `stop()` closes
    /// the socket out from under this, which makes `accept` return -1 and
    /// unwinds cleanly.
    private func acceptOne() {
        let client = accept(listenFD, nil, nil)
        guard client >= 0 else {
            finish(.failure(NativeAuthError.loginCancelled))
            return
        }
        defer { close(client) }

        var buffer = [UInt8](repeating: 0, count: 8192)
        let count = recv(client, &buffer, buffer.count, 0)
        guard count > 0,
              let request = String(bytes: buffer[0..<count], encoding: .utf8),
              let requestLine = request.split(separator: "\r\n").first,
              let path = requestLine.split(separator: " ").dropFirst().first,
              let components = URLComponents(string: "http://127.0.0.1" + path),
              let code = components.queryItems?.first(where: { $0.name == "code" })?.value,
              let state = components.queryItems?.first(where: { $0.name == "state" })?.value
        else {
            finish(.failure(NativeAuthError.callbackParseFailed))
            return
        }

        let body = "<html><body>Signed in \u{2014} you can close this tab.</body></html>"
        let response = """
        HTTP/1.1 200 OK\r
        Content-Type: text/html\r
        Content-Length: \(body.utf8.count)\r
        Connection: close\r
        \r
        \(body)
        """
        response.withCString { pointer in
            _ = send(client, pointer, strlen(pointer), 0)
        }

        finish(.success(Callback(code: code, state: state)))
        closeListener()
    }

    /// Resume the waiter exactly once — `stop()` and `acceptOne()` can race
    /// (user dismisses Safari at the same moment the redirect lands).
    private func finish(_ result: Result<Callback, Error>) {
        lock.lock()
        guard !didResume, let continuation = pendingContinuation else {
            lock.unlock()
            return
        }
        didResume = true
        pendingContinuation = nil
        lock.unlock()
        continuation.resume(with: result)
    }

    func stop() {
        finish(.failure(NativeAuthError.loginCancelled))
        closeListener()
    }

    private func closeListener() {
        lock.lock()
        let fd = listenFD
        listenFD = -1
        lock.unlock()
        if fd >= 0 { close(fd) }
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
