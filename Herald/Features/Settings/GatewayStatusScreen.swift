import SwiftUI

/// Real-time gateway telemetry dashboard.
/// Polls the NATIVE gateway's JSON-RPC methods (config.get, session.active_list,
/// system.battery) over the existing WebSocket connection. The old connector
/// REST facade (/gw/status) is dead for native clients - the native bearer
/// token is only valid on the gateway, and Caddy routes /gw/* to the connector.
struct GatewayStatusScreen: View {
    @Environment(AppContainer.self) private var container
    @Environment(KallistiHostStore.self) private var hostStore
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(PairingStore.self) private var pairingStore
    @Environment(AppSessionStore.self) private var sessionStore
    @Environment(\.dismiss) private var dismiss

    @State private var snapshot: NativeGatewayFeatureClient.GatewayStatusSnapshot?
    @State private var errorMessage: String?
    @State private var isLoading = true
    @State private var refreshTask: Task<Void, Never>?

    var body: some View {
        ZStack {
            Design.Colors.background.ignoresSafeArea()

            if isLoading && snapshot == nil {
                ProgressView("Loading gateway status…")
                    .foregroundStyle(Design.Colors.secondaryForeground)
            } else if let s = snapshot {
                ScrollView {
                    VStack(spacing: Design.Spacing.lg) {
                        // Connection status cards
                        connectionCards(s)

                        // Gateway stats
                        systemStatsSection(s)
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
                        .multilineTextAlignment(.center)
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

    private func connectionCards(_ s: NativeGatewayFeatureClient.GatewayStatusSnapshot) -> some View {
        HStack(spacing: Design.Spacing.sm) {
            statusCard(
                label: "Gateway",
                isOnline: container.nativeGatewayClient?.connectionStatus == .connected,
                detail: connectionDetail()
            )
            statusCard(
                label: "Hermes",
                isOnline: s.model != nil,
                detail: s.model ?? "No model"
            )
            statusCard(
                label: "Provider",
                isOnline: s.provider != nil,
                detail: s.provider ?? "Unset"
            )
        }
    }

    private func connectionDetail() -> String {
        switch container.nativeGatewayClient?.connectionStatus {
        case .connected: return "Connected"
        case .connecting: return "Connecting…"
        case .reconnecting: return "Reconnecting…"
        case .degraded: return "Degraded"
        case .error: return "Error"
        case .restarting: return "Restarting…"
        case .disconnected: return "Offline"
        case .none: return "Not configured"
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

    private func systemStatsSection(_ s: NativeGatewayFeatureClient.GatewayStatusSnapshot) -> some View {
        SettingsSectionView(title: "Gateway") {
            VStack(spacing: 0) {
                statRow(label: "Model", value: s.model ?? "Unavailable")
                sectionDivider
                statRow(label: "Provider", value: s.provider ?? "Unavailable")
                sectionDivider
                statRow(label: "Providers", value: "\(s.availableProviders.count)")
                sectionDivider
                statRow(label: "Active Sessions", value: "\(s.activeSessionCount)")
                sectionDivider
                statRow(label: "Usage", value: s.usageAvailable ? "Available" : "Unavailable")
                if s.batteryAvailable {
                    sectionDivider
                    statRow(
                        label: "Host Battery",
                        value: batteryDescription(s)
                    )
                }
                if let home = s.hermesHome, !home.isEmpty {
                    sectionDivider
                    statRow(label: "HERMES_HOME", value: home)
                }
            }
        }
    }

    private func batteryDescription(_ s: NativeGatewayFeatureClient.GatewayStatusSnapshot) -> String {
        guard let percent = s.batteryPercent else { return "Unavailable" }
        let plug = s.batteryPlugged ? " · plugged" : ""
        return "\(percent)%\(plug)"
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
                .lineLimit(2)
                .multilineTextAlignment(.trailing)
        }
        .frame(minHeight: Design.Size.minTapTarget)
    }

    private var sectionDivider: some View {
        Divider().overlay(Design.Colors.divider)
    }

    // MARK: - Fetch

    private func fetchTelemetry() async {
        isLoading = true
        errorMessage = nil
        defer { isLoading = false }

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
        guard let nativeClient = container.nativeGatewayClient else {
            errorMessage = "Native gateway unavailable. Check that Kallisti is connected."
            return
        }
        do {
            let featureClient = nativeClient.featureClient
            snapshot = try await featureClient.gatewayStatus()

            // Update shared state for Control Center
            if let s = snapshot {
                GatewayState.shared.update(
                    connected: container.nativeGatewayClient?.connectionStatus == .connected,
                    activeJobs: s.activeSessionCount,
                    model: s.model,
                    version: nil,
                    uptimeSeconds: nil,
                    cpuPercent: nil,
                    memoryUsedGb: nil,
                    memoryTotalGb: nil,
                    alertCount: 0
                )
            }
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
}
