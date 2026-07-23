import Foundation
import os

actor HostStatusStreamService {
    private let logger = Logger(subsystem: "net.fihonline.herald", category: "HostStatusStream")
    private let apiClient: RelayAPIClient
    private let accessTokenProvider: @Sendable () async -> String?
    private var streamTask: Task<Void, Never>?
    
    var onStatusChanged: (@Sendable (HostStatusEvent) -> Void)?
    
    struct HostStatusEvent: Sendable {
        let isOnline: Bool
        let modelName: String?
        let profileName: String?
    }
    
    init(apiClient: RelayAPIClient, accessTokenProvider: @escaping @Sendable () async -> String?) {
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
    }
    
    private func runStream() async {
        var backoff: TimeInterval = 1.0
        while !Task.isCancelled {
            do {
                let token = await accessTokenProvider()
                let stream = apiClient.streamEvents(
                    path: "host/events",
                    accessToken: token,
                    lastEventID: nil
                )
                for try await event in stream {
                    guard !Task.isCancelled else { return }
                    backoff = 1.0
                    if let status = parseHostStatus(from: event) {
                        onStatusChanged?(status)
                    }
                }
            } catch {
                logger.warning("Host status stream error: \(error.localizedDescription)")
            }
            guard !Task.isCancelled else { return }
            try? await Task.sleep(for: .seconds(backoff))
            backoff = min(backoff * 2, 30)
        }
    }
    
    private func parseHostStatus(from event: SSEEvent) -> HostStatusEvent? {
        guard let data = event.data.data(using: .utf8),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        return HostStatusEvent(
            isOnline: json["is_online"] as? Bool ?? false,
            modelName: json["model_name"] as? String,
            profileName: json["profile_name"] as? String
        )
    }
}
