import SwiftUI

/// Real-time gateway telemetry dashboard.
/// Polls the relay's /gw/status endpoint and displays system health.
struct GatewayStatusScreen: View {
    @Environment(KallistiHostStore.self) private var hostStore
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(PairingStore.self) private var pairingStore
    @Environment(AppSessionStore.self) private var sessionStore
    @Environment(\.dismiss) private var dismiss

    @State private var telemetry: GatewayTelemetry?
    @State private var errorMessage: String?
    @State private var isLoading = true
    @State private var refreshTask: Task<Void, Never>?

    var body: some View {
        ZStack {
            Design.Colors.background.ignoresSafeArea()

            if isLoading && telemetry == nil {
                ProgressView("Loading gateway status…")
                    .foregroundStyle(Design.Colors.secondaryForeground)
            } else if let t = telemetry {
                ScrollView {
                    VStack(spacing: Design.Spacing.lg) {
                        // Connection status cards
                        connectionCards(t)

                        // System stats
                        systemStatsSection(t)

                        if t.jobs.active > 0 || t.jobs.queued > 0 {
                            jobsSection(t.jobs)
                        }

                        if !t.reasons.isEmpty {
                            reasonsSection(t.reasons)
                        }
                    }
                    .padding(.horizontal, Design.Spacing.md)
                    .padding(.vertical, Design.Spacing.sm)
                }
                .refreshable { await fetchTelemetry() }
            } else if let error = errorMessage {
                VStack(spacing: Design.Spacing.md) {
                    Image(systemName: "exclamationmark.triangle")
                        .font(.system(size: 40))
                        .foregroundStyle(Design.Colors.warning)
                    Text("Unable to load gateway status")
                        .font(Design.Typography.headline)
                        .foregroundStyle(Design.Colors.foreground)
                    Text(error)
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                    Button("Retry") {
                        Task { await fetchTelemetry() }
                    }
                    .buttonStyle(.borderedProminent)
                }
                .padding()
            }
        }
        .navigationTitle("Gateway Status")
        .navigationBarTitleDisplayMode(.inline)
        .task {
            await fetchTelemetry()
            startAutoRefresh()
        }
        .onDisappear {
            refreshTask?.cancel()
        }
    }

    // MARK: - Connection Cards

    private func connectionCards(_ t: GatewayTelemetry) -> some View {
        HStack(spacing: Design.Spacing.sm) {
            statusCard(
                label: "Relay",
                isOnline: t.connector.state == "healthy",
                detail: "v\(t.connector.version) · \(formatUptime(t.connector.uptimeSeconds))"
            )
            statusCard(
                label: "Connector",
                isOnline: t.connectorConnected && t.connector.singleton && t.connector.portsOwned,
                detail: t.connector.version
            )
            statusCard(
                label: "Hermes",
                isOnline: t.hermes.state == "healthy",
                detail: t.hermes.activeModel ?? t.hermes.installedVersion
            )
        }
    }

    private func statusCard(label: String, isOnline: Bool, detail: String) -> some View {
        VStack(spacing: 4) {
            Circle()
                .fill(isOnline ? Design.Colors.success : Design.Colors.danger)
                .frame(width: 10, height: 10)
            Text(label)
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.foreground)
            Text(detail)
                .font(.caption2)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .lineLimit(1)
        }
        .frame(maxWidth: .infinity)
        .padding(Design.Spacing.sm)
        .background(Design.Colors.surface)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
    }

    // MARK: - System Stats

    private func systemStatsSection(_ t: GatewayTelemetry) -> some View {
        SettingsSectionView(title: "System") {
            VStack(spacing: 0) {
                statRow(label: "CPU", value: t.host.cpuPercent.map { String(format: "%.1f%%", $0) } ?? "Sampling…")
                sectionDivider
                statRow(label: "Memory", value: memoryDescription(t.host))
                sectionDivider
                statRow(label: "Uptime", value: t.host.uptimeSeconds.map(formatUptime) ?? "Unavailable")
                sectionDivider
                statRow(label: "Active Jobs", value: "\(t.jobs.active)")
                sectionDivider
                statRow(label: "Connector", value: t.connector.version)
                sectionDivider
                statRow(label: "Hermes", value: t.hermes.installedVersion)
                if let model = t.hermes.activeModel {
                    sectionDivider
                    statRow(label: "Model", value: model)
                }
            }
        }
    }

    private func statRow(label: String, value: String) -> some View {
        HStack {
            Text(label)
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.foreground)
            Spacer()
            Text(value)
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.secondaryForeground)
        }
        .frame(minHeight: Design.Size.minTapTarget)
    }

    // MARK: - Jobs

    private func jobsSection(_ jobs: GatewayTelemetry.Jobs) -> some View {
        SettingsSectionView(title: "Jobs") {
            VStack(spacing: 0) {
                statRow(label: "Active", value: "\(jobs.active)")
                sectionDivider
                statRow(label: "Queued", value: "\(jobs.queued)")
            }
        }
    }

    // MARK: - Alerts

    private func reasonsSection(_ reasons: [String]) -> some View {
        SettingsSectionView(title: "Host Warnings") {
            VStack(spacing: 0) {
                ForEach(reasons.indices, id: \.self) { i in
                    HStack(spacing: Design.Spacing.sm) {
                        Image(systemName: "exclamationmark.triangle")
                            .font(.system(size: 12))
                            .foregroundStyle(Design.Colors.warning)

                        VStack(alignment: .leading, spacing: 2) {
                            Text(reasons[i])
                                .font(Design.Typography.caption)
                                .foregroundStyle(Design.Colors.foreground)
                        }
                    }
                    .frame(minHeight: Design.Size.minTapTarget)

                    if i < reasons.count - 1 {
                        sectionDivider
                    }
                }
            }
        }
    }

    // MARK: - Helpers

    private var sectionDivider: some View {
        Divider().overlay(Design.Colors.divider)
    }

    private func fetchTelemetry() async {
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }

        // Time out after 10 seconds — the relay's gw/status endpoint can hang
        // when the connector is wedged, and the user needs to know.
        let result = await withTimeout(seconds: 10) {
            await fetchTelemetryInner()
        }
        if case .timedOut = result, errorMessage == nil {
            errorMessage = "Gateway status request timed out. The host may be overloaded — try again."
        }
    }

    private enum TimeoutResult {
        case completed
        case timedOut
    }

    @MainActor
    private func withTimeout(seconds: TimeInterval, body: @escaping @MainActor () async -> Void) async -> TimeoutResult {
        let bodyTask = Task { @MainActor in
            await body()
            return TimeoutResult.completed
        }
        let timeoutTask = Task {
            try? await Task.sleep(for: .seconds(seconds))
            return TimeoutResult.timedOut
        }
        return await withTaskCancellationHandler {
            let result = await bodyTask.value
            timeoutTask.cancel()
            return result
        } onCancel: {
            bodyTask.cancel()
            timeoutTask.cancel()
        }
    }

    private func fetchTelemetryInner() async {
        do {
            let relayBase = settingsStore.settings.relayConfiguration.activeBaseURLString
                ?? pairingStore.pairedRelayConfiguration?.baseURLString
            guard let relayBase else {
                errorMessage = "No relay configured. Add your relay URL in Settings."
                return
            }
            let token = await sessionStore.currentAccessToken()
            let client = RelayAPIClient { relayBase }

            struct Response: Decodable {
                let data: GatewayTelemetry
            }
            let response: Response = try await client.get(path: "gw/status", accessToken: token)
            telemetry = response.data

            // Update shared state for Control Center
            GatewayState.shared.update(
                connected: response.data.overall == "healthy",
                activeJobs: response.data.jobs.active,
                model: response.data.hermes.activeModel,
                version: response.data.connector.version,
                uptimeSeconds: response.data.host.uptimeSeconds,
                cpuPercent: response.data.host.cpuPercent,
                memoryUsedGb: response.data.host.memoryUsedBytes.map { Double($0) / 1_073_741_824 },
                memoryTotalGb: response.data.host.memoryTotalBytes.map { Double($0) / 1_073_741_824 },
                alertCount: response.data.reasons.count
            )
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func startAutoRefresh() {
        refreshTask = Task {
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(10))
                await fetchTelemetry()
            }
        }
    }

    private func formatUptime(_ seconds: Int) -> String {
        let hours = seconds / 3600
        let minutes = (seconds % 3600) / 60
        if hours > 24 {
            return "\(hours / 24)d \(hours % 24)h"
        }
        return "\(hours)h \(minutes)m"
    }

    private func memoryDescription(_ host: GatewayTelemetry.Host) -> String {
        guard let used = host.memoryUsedBytes, let total = host.memoryTotalBytes else {
            return "Unavailable"
        }
        return String(format: "%.1f / %.1f GB", Double(used) / 1_073_741_824, Double(total) / 1_073_741_824)
    }
}

// MARK: - Models

struct GatewayTelemetry: Decodable {
    struct Connector: Decodable {
        let state: String
        let version: String
        let uptimeSeconds: Int
        let singleton: Bool
        let portsOwned: Bool
    }
    struct Hermes: Decodable {
        let state: String
        let installedVersion: String
        let activeModel: String?
    }
    struct Host: Decodable {
        let cpuPercent: Double?
        let cpuSampleReady: Bool
        let memoryTotalBytes: Int64?
        let memoryUsedBytes: Int64?
        let uptimeSeconds: Int?
    }
    struct Jobs: Decodable {
        let active: Int
        let queued: Int
    }

    let overall: String
    let connectorConnected: Bool
    let connector: Connector
    let hermes: Hermes
    let host: Host
    let jobs: Jobs
    let reasons: [String]
}
