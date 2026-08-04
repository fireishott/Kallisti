import Foundation

/// Batches incoming streaming tokens into periodic flushes to reduce UI update frequency.
///
/// Instead of updating the UI 50-80 times per second (one per token), tokens are
/// accumulated into 50ms windows and flushed as a batch, reducing UI updates to
/// ~20 per second. This matches common patterns in AI chat streaming clients.
///
/// Thread-safe via actor isolation.
public actor TokenBatcher {
    private var buffer: [String] = []
    private var flushTask: Task<Void, Never>?
    private let flushInterval: TimeInterval
    private let onFlush: @Sendable ([String]) -> Void

    /// - Parameters:
    ///   - flushInterval: How often to flush the buffer (default 0.050 = 50ms).
    ///   - onFlush: Called on each flush with the accumulated tokens.
    public init(
        flushInterval: TimeInterval = 0.050,
        onFlush: @escaping @Sendable ([String]) -> Void
    ) {
        self.flushInterval = flushInterval
        self.onFlush = onFlush
    }

    /// Enqueue a single token. The token will be included in the next flush.
    public func enqueue(_ token: String) {
        buffer.append(token)
        if flushTask == nil {
            scheduleFlush()
        }
    }

    /// Flush immediately, sending all buffered tokens.
    public func flushNow() {
        flushTask?.cancel()
        flushTask = nil
        let batch = buffer
        buffer = []
        if !batch.isEmpty {
            onFlush(batch)
        }
    }

    /// Cancel any pending flush. Buffered tokens remain but won't be
    /// flushed until the next `enqueue()` call.
    public func cancelPending() {
        flushTask?.cancel()
        flushTask = nil
    }

    private func scheduleFlush() {
        let interval = flushInterval
        flushTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
            await self?.flushNow()
        }
    }
}
