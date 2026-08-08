import Foundation

/// Thread-safe storage for mock transport state.
private final class MockTransportState: @unchecked Sendable {
    var sentFrames: [Data] = []
    var incomingContinuation: AsyncThrowingStream<Data, Error>.Continuation?
    var queuedBeforeReceive: [Data] = []
    /// When set, send() throws this instead of recording the frame --
    /// simulates a socket that looks open (connect() never throws, see
    /// URLSessionWebSocketTransport) but is actually dead.
    var sendError: Error?
    let lock = NSLock()

    func appendSent(_ data: Data) {
        lock.lock()
        sentFrames.append(data)
        lock.unlock()
    }

    func getSendError() -> Error? {
        lock.lock()
        defer { lock.unlock() }
        return sendError
    }

    func setSendError(_ error: Error?) {
        lock.lock()
        sendError = error
        lock.unlock()
    }

    func getSentFrames() -> [Data] {
        lock.lock()
        defer { lock.unlock() }
        return sentFrames
    }

    func queueOrYield(_ data: Data) {
        lock.lock()
        if let cont = incomingContinuation {
            lock.unlock()
            cont.yield(data)
        } else {
            queuedBeforeReceive.append(data)
            lock.unlock()
        }
    }

    func setContinuationAndDrain(_ c: AsyncThrowingStream<Data, Error>.Continuation) {
        lock.lock()
        let queued = queuedBeforeReceive
        queuedBeforeReceive = []
        incomingContinuation = c
        lock.unlock()
        for frame in queued {
            c.yield(frame)
        }
    }

    func close() {
        lock.lock()
        incomingContinuation?.finish()
        incomingContinuation = nil
        lock.unlock()
    }
}

final class MockNativeGatewayTransport: NativeGatewayTransport {
    private let state = MockTransportState()

    var sentFrames: [Data] { state.getSentFrames() }

    /// When set, send() throws this instead of recording the frame --
    /// simulates a socket that looks open but is actually dead.
    var sendError: Error? {
        get { state.getSendError() }
        set { state.setSendError(newValue) }
    }

    func connect(url: URL) async throws {}

    func send(_ data: Data) async throws {
        if let sendError = state.getSendError() {
            throw sendError
        }
        state.appendSent(data)
    }

    func queueIncoming(_ data: Data) {
        state.queueOrYield(data)
    }

    func receive() -> AsyncThrowingStream<Data, Error> {
        AsyncThrowingStream { continuation in
            self.state.setContinuationAndDrain(continuation)
        }
    }

    func close() async {
        state.close()
    }
}
