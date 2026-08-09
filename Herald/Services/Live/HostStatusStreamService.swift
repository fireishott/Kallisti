import Foundation
import os

actor HostStatusStreamService {
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "HostStatusStream")
    private let apiClient: RelayAPIClient
    private let accessTokenProvider: @Sendable () async -> String?
    private let healthCheckProvider: @Sendable () async -> Bool
    private var streamTask: Task<Void, Never>?
    private var wasConnected = false
    
    nonisolated(unsafe) var onConnectionStatusChanged: (@Sendable (ConnectionStatus) -> Void)?
    
    init(
        apiClient: RelayAPIClient,
        accessTokenProvider: @escaping @Sendable () async -> String?,
        healthCheckProvider: @escaping @Sendable () async -> Bool = { true }
    ) {
        self.apiClient = apiClient
        self.accessTokenProvider = accessTokenProvider
        self.healthCheckProvider = healthCheckProvider
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
    }
    
    func updateConnectionStatus(_ status: ConnectionStatus) {
        onConnectionStatusChanged?(status)
    }
    
    private func runStream() async {
        var backoff: TimeInterval = 1.0
        while !Task.isCancelled {
            do {
                let token = await accessTokenProvider()
                onConnectionStatusChanged?(wasConnected ? .reconnecting : .connecting)
                
                let stream = apiClient.streamEvents(
                    path: "connector/events",
                    accessToken: token,
                    lastEventID: nil
                )
                
                for try await event in stream {
                    guard !Task.isCancelled else { return }
                    backoff = 1.0
                    
                    if event.event == "connected" {
                        let healthy = await healthCheckProvider()
                        if healthy {
                            wasConnected = true
                            onConnectionStatusChanged?(.connected)
                        } else {
                            wasConnected = true
                            onConnectionStatusChanged?(.degraded)
                        }
                    } else if event.event == "health_check" {
                        if wasConnected {
                            let healthy = await healthCheckProvider()
                            onConnectionStatusChanged?(healthy ? .connected : .degraded)
                        }
                    }
                }
            } catch {
                logger.warning("Host status stream error: \(error.localizedDescription)")
            }
            
            guard !Task.isCancelled else { return }
            onConnectionStatusChanged?(wasConnected ? .reconnecting : .disconnected)
            
            try? await Task.sleep(for: .seconds(backoff))
            backoff = min(backoff * 2, 30)
        }
    }
}
