import Foundation

/// Errors specific to NativeGatewayClient.
enum NativeGatewayClientError: Error, Equatable {
    case notConnected
    case requestTimeout
    case encodingFailed
    case transportClosed
    case unexpectedFrame
}

extension NativeGatewayClientError: LocalizedError {
    /// Without this, NSError's fallback ("The operation couldn't be
    /// completed. (Kallisti.NativeGatewayClientError error N.)") is what
    /// reaches any UI that reads error.localizedDescription.
    var errorDescription: String? {
        switch self {
        case .notConnected:
            return "Not connected to the gateway."
        case .requestTimeout:
            return "The gateway didn't respond in time."
        case .encodingFailed:
            return "Couldn't prepare the request."
        case .transportClosed:
            return "Connection to the gateway was lost."
        case .unexpectedFrame:
            return "Received an unexpected response from the gateway."
        }
    }
}

/// A JSON-RPC 2.0 client over a NativeGatewayTransport.
///
/// Handles request/response correlation by numeric id, dispatches
/// server-pushed "event" notifications to registered handlers.
actor NativeGatewayClient {
    private let transport: NativeGatewayTransport
    private var nextId: Int = 1
    private var pendingRequests: [Int: CheckedContinuation<NativeGatewayResponse, Error>] = [:]
    private var eventHandlers: [@Sendable (NativeGatewayEvent) -> Void] = []
    private var receiveTask: Task<Void, Never>?
    private var disconnectHandler: (@Sendable () -> Void)?
    /// Set by close() so a deliberate teardown doesn't trigger a reconnect.
    private var isClosingDeliberately = false
    private let requestTimeoutNanos: UInt64

    init(transport: NativeGatewayTransport, requestTimeout: Duration = .seconds(60)) {
        self.transport = transport
        self.requestTimeoutNanos = UInt64(requestTimeout.components.seconds * 1_000_000_000)
            + UInt64(requestTimeout.components.attoseconds / 1_000_000_000)
    }

    /// Connect and start the receive loop.
    func connect(url: URL) async throws {
        try await transport.connect(url: url)
        startReceiveLoop()
    }

    /// Send a JSON-RPC request and await its correlated response.
    func send<P: Encodable>(method: String, params: P) async throws -> NativeGatewayResponse {
        try await send(method: method, params: params, timeoutNanos: requestTimeoutNanos)
    }

    /// Send a JSON-RPC request with an explicit timeout override.
    ///
    /// The default 60s request timeout is correct for prompt.submit, where the
    /// gateway can legitimately take tens of seconds to finish an LLM call.
    /// It is wrong for liveness probes: a phantom socket (iOS suspended the
    /// WS in background and receive() never surfaced the error) makes a probe
    /// hang the FULL 60s before the caller can decide the socket is dead and
    /// force a fresh connect - which is exactly the ~85s app-side delay seen
    /// when sending a follow-up after backgrounding. Probes use a 5s budget so
    /// a dead socket is detected fast and the message doesn't wait behind it.
    func send<P: Encodable>(method: String, params: P, timeoutNanos: UInt64) async throws -> NativeGatewayResponse {
        let id = nextId
        nextId += 1

        let request = JSONRPCRequest(id: id, method: method, params: params)
        let data: Data
        do {
            data = try JSONEncoder().encode(request)
        } catch {
            throw NativeGatewayClientError.encodingFailed
        }

        // Send the frame first
        do {
            try await transport.send(data)
        } catch {
            throw NativeGatewayClientError.transportClosed
        }

        // Await response with timeout
        return try await withThrowingTaskGroup(of: NativeGatewayResponse.self) { group in
            group.addTask {
                try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<NativeGatewayResponse, Error>) in
                    Task {
                        await self.registerPending(id: id, continuation: continuation)
                    }
                }
            }
            group.addTask {
                try await Task.sleep(nanoseconds: timeoutNanos)
                // Clean up the pending request on timeout
                await self.timeoutPending(id: id)
                throw NativeGatewayClientError.requestTimeout
            }
            let result = try await group.next()!
            group.cancelAll()
            return result
        }
    }

    /// Register a handler for server-pushed events (message.delta, thinking.delta, etc.).
    func onEvent(_ handler: @escaping @Sendable (NativeGatewayEvent) -> Void) {
        eventHandlers.append(handler)
    }

    /// Fires when the receive loop ends — i.e. the socket died. The owner
    /// uses this to re-mint a ticket and reconnect; without it a single
    /// drop leaves every later request failing with transportClosed until
    /// the app is force-quit.
    func onDisconnect(_ handler: @escaping @Sendable () -> Void) {
        disconnectHandler = handler
    }

    func close() async {
        isClosingDeliberately = true
        receiveTask?.cancel()
        receiveTask = nil
        for (_, cont) in pendingRequests {
            cont.resume(throwing: NativeGatewayClientError.transportClosed)
        }
        pendingRequests.removeAll()
        await transport.close()
    }

    // MARK: - Private

    private func registerPending(id: Int, continuation: CheckedContinuation<NativeGatewayResponse, Error>) {
        pendingRequests[id] = continuation
    }

    private func removePending(id: Int) {
        pendingRequests.removeValue(forKey: id)
    }

    private func timeoutPending(id: Int) {
        if let cont = pendingRequests.removeValue(forKey: id) {
            cont.resume(throwing: NativeGatewayClientError.requestTimeout)
        }
    }

    private func startReceiveLoop() {
        receiveTask = Task {
            let stream = transport.receive()
            do {
                for try await frame in stream {
                    if Task.isCancelled { break }
                    handleFrame(frame)
                }
                // Stream ended without throwing — the socket still closed.
                await self.handleDisconnect()
            } catch {
                await self.handleDisconnect()
            }
        }
    }

    private func handleDisconnect() {
        failAllPending(error: NativeGatewayClientError.transportClosed)
        guard !isClosingDeliberately, !Task.isCancelled else { return }
        disconnectHandler?()
    }

    private nonisolated func handleFrame(_ data: Data) {
        // Try to decode as a response (has "id")
        if let response = try? JSONDecoder().decode(NativeGatewayResponse.self, from: data) {
            Task {
                await self.resumePending(id: response.id, response: response)
            }
            return
        }

        // Try to decode as an event notification (has "method": "event")
        if let event = try? JSONDecoder().decode(NativeGatewayEvent.self, from: data) {
            Task {
                await self.dispatchEvent(event)
            }
            return
        }

        // Unknown frame — ignore
    }

    private func resumePending(id: Int, response: NativeGatewayResponse) {
        if let cont = pendingRequests.removeValue(forKey: id) {
            cont.resume(returning: response)
        }
    }

    private func failAllPending(error: Error) {
        for (_, cont) in pendingRequests {
            cont.resume(throwing: error)
        }
        pendingRequests.removeAll()
    }

    private func dispatchEvent(_ event: NativeGatewayEvent) {
        for handler in eventHandlers {
            handler(event)
        }
    }
}
