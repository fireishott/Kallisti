import SwiftUI

/// Real-time gateway telemetry dashboard.
/// Polls the relay's /gw/status endpoint and displays system health.
struct GatewayStatusScreen: View {
    @Environment(HeraldHostStore.self) private var hostStore
    @Environment(SettingsStore.self) private var settingsStore
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

                        // Active jobs
                        if let jobs = t.activeJobs, !jobs.isEmpty {
                            jobsSection(jobs)
                        }

                        // Alerts
                        if let alerts = t.alerts, !alerts.isEmpty {
                            alertsSection(alerts)
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
                isOnline: t.relayConnected ?? true,
                detail: "v\(t.version ?? "?") · \(formatUptime(t.uptimeSeconds ?? 0))"
            )
            statusCard(
                label: "Connector",
                isOnline: t.connectorConnected ?? false,
                detail: t.connectorVersion ?? "—"
            )
            statusCard(
                label: "Hermes",
                isOnline: t.hermesConnected ?? false,
                detail: t.modelName ?? "—"
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
                statRow(label: "CPU", value: String(format: "%.1f%%", t.cpuPercent ?? 0))
                sectionDivider
                statRow(label: "Memory", value: "\(String(format: "%.1f", t.memoryUsedGb ?? 0)) / \(String(format: "%.1f", t.memoryTotalGb ?? 0)) GB")
                sectionDivider
                statRow(label: "Uptime", value: formatUptime(t.uptimeSeconds ?? 0))
                sectionDivider
                statRow(label: "Active Jobs", value: "\(t.activeJobs ?? 0)")
                sectionDivider
                statRow(label: "Version", value: t.version ?? "—")
                if let model = t.modelName {
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

    private func jobsSection(_ jobs: [GatewayJobInfo]) -> some View {
        SettingsSectionView(title: "Active Jobs (\(jobs.count))") {
            VStack(spacing: 0) {
                ForEach(jobs.indices, id: \.self) { i in
                    let job = jobs[i]
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(job.id.prefix(8))
                                .font(.caption.monospaced())
                                .foregroundStyle(Design.Colors.foreground)
                            if let model = job.model {
                                Text(model)
                                    .font(.caption2)
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                            }
                        }
                        Spacer()
                        Text(job.status.capitalized)
                            .font(Design.Typography.caption)
                            .foregroundStyle(job.status == "running" ? Design.Colors.success : Design.Colors.secondaryForeground)
                    }
                    .frame(minHeight: Design.Size.minTapTarget)

                    if i < jobs.count - 1 {
                        sectionDivider
                    }
                }
            }
        }
    }

    // MARK: - Alerts

    private func alertsSection(_ alerts: [GatewayAlert]) -> some View {
        SettingsSectionView(title: "Alerts (\(alerts.count))") {
            VStack(spacing: 0) {
                ForEach(alerts.indices, id: \.self) { i in
                    let alert = alerts[i]
                    HStack(spacing: Design.Spacing.sm) {
                        Image(systemName: alert.severity == "critical" ? "xmark.octagon" : "exclamationmark.triangle")
                            .font(.system(size: 12))
                            .foregroundStyle(alert.severity == "critical" ? Design.Colors.danger : Design.Colors.warning)

                        VStack(alignment: .leading, spacing: 2) {
                            Text(alert.message)
                                .font(Design.Typography.caption)
                                .foregroundStyle(Design.Colors.foreground)
                            Text(alert.timestamp, style: .relative)
                                .font(.caption2)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                    }
                    .frame(minHeight: Design.Size.minTapTarget)

                    if i < alerts.count - 1 {
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

        do {
            let relayBase = settingsStore.settings.relayConfiguration.activeBaseURLString
                ?? "https://herald-host.internal:8010/v1"
            let token = await sessionStore.currentAccessToken()
            let client = RelayAPIClient { relayBase }

            struct Response: Decodable {
                let data: GatewayTelemetry
            }
            let response: Response = try await client.get(path: "gw/status", accessToken: token)
            telemetry = response.data

            // Update shared state for Control Center
            GatewayState.shared.update(
                connected: response.data.relayConnected ?? true,
                activeJobs: response.data.activeJobs,
                model: response.data.modelName,
                version: response.data.version,
                uptimeSeconds: response.data.uptimeSeconds,
                cpuPercent: response.data.cpuPercent,
                memoryUsedGb: response.data.memoryUsedGb,
                memoryTotalGb: response.data.memoryTotalGb,
                alertCount: response.data.alerts?.count
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
}

// MARK: - Models

struct GatewayTelemetry: Decodable {
    let relayConnected: Bool?
    let connectorConnected: Bool?
    let hermesConnected: Bool?
    let version: String?
    let connectorVersion: String?
    let uptimeSeconds: Int?
    let activeJobs: Int?
    let modelName: String?
    let cpuPercent: Double?
    let memoryUsedGb: Double?
    let memoryTotalGb: Double?
    let activeJobsList: [GatewayJobInfo]?
    let alerts: [GatewayAlert]?

    var activeJobs: [GatewayJobInfo]? { activeJobsList }
}

struct GatewayJobInfo: Decodable {
    let id: String
    let status: String
    let model: String?
}

struct GatewayAlert: Decodable {
    let message: String
    let severity: String
    let timestamp: Date
}
