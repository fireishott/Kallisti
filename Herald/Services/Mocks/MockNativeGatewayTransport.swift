import Foundation

/// Thread-safe storage for mock transport state.
private final class MockTransportState: @unchecked Sendable {
    var sentFrames: [Data] = []
    var incomingContinuation: AsyncThrowingStream<Data, Error>.Continuation?
    var queuedBeforeReceive: [Data] = []
    let lock = NSLock()

    func appendSent(_ data: Data) {
        lock.lock()
        sentFrames.append(data)
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

    func connect(url: URL) async throws {}

    func send(_ data: Data) async throws {
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
