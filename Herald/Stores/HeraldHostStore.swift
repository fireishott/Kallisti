import Foundation

enum HeraldHostConnectionState: Equatable, Sendable {
    case online
    case offline
    case unreachable
    case notConnected
}

@MainActor
@Observable
final class KallistiHostStore {
    var currentHost: HeraldHostStatus?
    var activeEnrollmentCode: HostEnrollmentCode?
    var isLoading = false
    var isWorking = false
    var lastErrorMessage: String?
    var onHostChanged: (@MainActor () -> Void)?

    private let hostService: any KallistiHostServiceProtocol
    private let accessTokenProvider: @MainActor () async -> String?
    /// Native gateway feature client provider (nil when running legacy mode).
    private let nativeFeatureClientProvider: @MainActor () -> NativeGatewayFeatureClient?

    init(
        hostService: any KallistiHostServiceProtocol,
        accessTokenProvider: @escaping @MainActor () async -> String?,
        nativeFeatureClientProvider: @escaping @MainActor () -> NativeGatewayFeatureClient? = { nil }
    ) {
        self.hostService = hostService
        self.accessTokenProvider = accessTokenProvider
        self.nativeFeatureClientProvider = nativeFeatureClientProvider
    }

    var isHostOnline: Bool {
        currentHost?.isOnline == true
    }

    /// Build 128.88 (connection bounce): count consecutive refresh failures.
    /// A single transient hostInfo() miss (socket mid-reconnect, slow
    /// round-trip right after device sleep) must NOT flip the host offline
    /// and strobe the connection banner on the next 10s tick. The host only
    /// goes offline after this many failures in a row.
    private var consecutiveFailures = 0
    private let offlineAfterFailures = 2

    var connectionState: HeraldHostConnectionState {
        if currentHost?.isOnline == true {
            return .online
        }

        if currentHost != nil {
            return lastErrorMessage == nil ? .offline : .unreachable
        }

        if lastErrorMessage != nil {
            return .unreachable
        }

        return .notConnected
    }

    func refresh() async {
        guard !isLoading else { return }

        isLoading = true
        defer { isLoading = false }

        // NATIVE path: the connector REST facade (/hosts/current) rejects
        // native bearer tokens. Build the host row from gateway config.get.
        // hostInfo() succeeding IS proof of connectivity - the call only
        // succeeds when the WS is live. Never gate isOnline on a relay token
        // here: native mode has no relay pairing, so the row would always
        // show offline even with a healthy socket.
        if let featureClient = nativeFeatureClientProvider() {
            do {
                let info = try await featureClient.hostInfo()
                consecutiveFailures = 0
                // NATIVE mode: pull real versions from the gateway + connector
                // facade instead of leaving the Settings rows at "—".
                let connectorVersion = await featureClient.connectorVersion()
                let agentRaw = (try? await featureClient.agentVersion()) ?? ""
                let agentVersion = Self.extractAgentVersion(agentRaw)
                currentHost = HeraldHostStatus(
                    id: UUID(uuidString: "00000000-0000-0000-0000-000000000001") ?? UUID(),
                    displayName: info.model.map { "\(info.provider ?? "hermes")/\($0)" }
                        ?? (info.provider ?? "Hermes Host"),
                    hostname: nil,
                    platform: "native",
                    connectorVersion: connectorVersion,
                    heraldCommand: nil,
                    heraldVersion: agentVersion,
                    heraldModel: info.model,
                    lastSeenAt: .now,
                    lastConnectedAt: .now,
                    isOnline: true
                )
                lastErrorMessage = nil
                onHostChanged?()
                return
            } catch {
                // Build 128.88 (connection bounce): hysteresis. One transient
                // failure keeps the last good online state; only N consecutive
                // failures flip the host offline, so the 10s poll can't strobe
                // the banner after a sleep/wake reconnect.
                consecutiveFailures += 1
                if let ch = currentHost, consecutiveFailures >= offlineAfterFailures {
                    currentHost = HeraldHostStatus(
                        id: ch.id,
                        displayName: ch.displayName,
                        hostname: ch.hostname,
                        platform: ch.platform,
                        connectorVersion: ch.connectorVersion,
                        heraldCommand: ch.heraldCommand,
                        heraldVersion: ch.heraldVersion,
                        heraldModel: ch.heraldModel,
                        lastSeenAt: ch.lastSeenAt,
                        lastConnectedAt: ch.lastConnectedAt,
                        isOnline: false
                    )
                }
                lastErrorMessage = nil
                onHostChanged?()
                return
            }
        }

        do {
            currentHost = try await hostService.fetchCurrentHost(accessToken: await accessTokenProvider())
            lastErrorMessage = nil
            onHostChanged?()
        } catch {
            lastErrorMessage = error.localizedDescription
        }
    }

    func generateEnrollmentCode() async {
        guard !isWorking else { return }

        isWorking = true
        defer { isWorking = false }

        do {
            activeEnrollmentCode = try await hostService.createEnrollmentCode(accessToken: await accessTokenProvider())
            currentHost = try await hostService.fetchCurrentHost(accessToken: await accessTokenProvider())
            lastErrorMessage = nil
            onHostChanged?()
        } catch {
            lastErrorMessage = error.localizedDescription
        }
    }

    func revokeCurrentHost() async {
        guard !isWorking else { return }

        isWorking = true
        defer { isWorking = false }

        do {
            try await hostService.revokeCurrentHost(accessToken: await accessTokenProvider())
            currentHost = nil
            activeEnrollmentCode = nil
            lastErrorMessage = nil
            onHostChanged?()
        } catch {
            lastErrorMessage = error.localizedDescription
        }
    }

    func reset() {
        currentHost = nil
        activeEnrollmentCode = nil
        isLoading = false
        isWorking = false
        lastErrorMessage = nil
        onHostChanged?()
    }

    /// Pull the version line out of `hermes version` output.
    /// Typical output: "Hermes Agent v0.20.0 (2026.8.3) · upstream 3d7dda4c"
    private static func extractAgentVersion(_ raw: String) -> String? {
        let firstLine = raw.split(separator: "\n").first.map(String.init) ?? raw
        let trimmed = firstLine.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        let parts = trimmed.split(separator: "·").map(String.init)
        return parts.first?.trimmingCharacters(in: .whitespaces)
    }
}
