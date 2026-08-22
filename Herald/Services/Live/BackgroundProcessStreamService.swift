import Foundation
import os

/// Streams background process events from the connector's
/// /v1/canvas/processes/stream SSE endpoint into a callback the Canvas
/// store consumes. Mirrors HostStatusStreamService's reconnect/backoff
/// pattern so the Live tab survives connector restarts and network
/// blips.
actor BackgroundProcessStreamService {
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "BackgroundProcessStream")
    private let apiClient: RelayAPIClient
    private let accessTokenProvider: @Sendable () async -> String?
    private var streamTask: Task<Void, Never>?
    private var wasConnected = false

    /// Called with each decoded BackgroundProcess event (snapshot or
    /// incremental chunk). Main-actor hops happen in the store.
    nonisolated(unsafe) var onProcessEvent: (@Sendable (BackgroundProcess) -> Void)?

    init(
        apiClient: RelayAPIClient,
        accessTokenProvider: @escaping @Sendable () async -> String?
    ) {
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
    }

    func start() {
        guard streamTask == nil else { return }
        streamTask = Task { [weak self] in
            guard let self else { return }
            await self.runStream()
        }
    }

    func stop() {
        streamTask?.cancel()
        streamTask = nil
        wasConnected = false
    }

    private func runStream() async {
        var backoff: TimeInterval = 1.0
        while !Task.isCancelled {
            do {
                let token = await accessTokenProvider()
                let stream = apiClient.streamEvents(
                    path: "canvas/processes/stream",
                    accessToken: token
                )
                wasConnected = true
                backoff = 1.0
                for try await event in stream {
                    guard !Task.isCancelled else { return }
                    if event.event == "process",
                       let data = event.data.data(using: .utf8),
                       let process = try? JSONDecoder().decode(BackgroundProcess.self, from: data) {
                        onProcessEvent?(process)
                    }
                }
            } catch {
                logger.warning("background process stream error: \(error.localizedDescription)")
            }
            if Task.isCancelled { return }
            try? await Task.sleep(nanoseconds: UInt64(backoff * 1_000_000_000))
            backoff = min(backoff * 2, 30)
        }
    }
}