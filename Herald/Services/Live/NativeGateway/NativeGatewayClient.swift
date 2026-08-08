import Foundation

/// Errors specific to NativeGatewayClient.
enum NativeGatewayClientError: Error, Equatable {
    case notConnected
    case requestTimeout
    case encodingFailed
    case transportClosed
    case unexpectedFrame
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
                try await Task.sleep(nanoseconds: self.requestTimeoutNanos)
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
