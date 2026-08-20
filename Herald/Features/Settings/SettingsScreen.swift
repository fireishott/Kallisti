import AVFoundation
import SwiftUI
import UIKit

struct SettingsScreen: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL
    @Environment(AppContainer.self) private var container
    @Environment(AppSessionStore.self) private var sessionStore
    @Environment(ChatStore.self) private var chatStore
    @Environment(ModelStore.self) private var modelStore
    @Environment(KallistiHostStore.self) private var hostStore
    @Environment(PairingStore.self) private var pairingStore
    @Environment(PermissionsStore.self) private var permissionsStore
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(TabRouter.self) private var router
    @State private var mimoAPIKey: String = ""
    // Build 70: aux service lives on the container (loaded at connection).
    private var auxService: AuxModelService? { container.auxService }
    @State private var showAPIKey: Bool = false
    @State private var isTestingTTS: Bool = false
    @State private var safariURL: URL?
    @State private var showSafari = false
    // Build 70: searchable aux model picker state.
    @State private var auxPickerTask: AuxTask?
    @State private var auxPickerSearchText = ""
    // Build 128.95: enrichment model picker is a sheet (stable List scroll),
    // NOT a live-bound Menu whose content rebuilt on every parent re-render
    // and snapped scroll back to top.
    @State private var isEnrichmentPickerPresented = false
    @State private var enrichmentPickerSearchText = ""
    private let mimoKeychain = KeychainSecureStore(serviceName: "net.fihonline.kallisti.session")
    @Environment(ThemeManager.self) private var themeManager
    @Environment(GatewayControlService.self) private var gatewayControl

    var body: some View {
        ZStack {
            Design.Colors.background
                .ignoresSafeArea()

            GeometryReader { geo in
                ScrollView(.vertical) {
                    VStack(spacing: Design.Spacing.lg) {
                        connectionSection
                        relaySection
                        gatewaySection
                        infrastructureSection
                        if settingsStore.availableEnvironments.count > 1 {
                            environmentSection
                        }
                        appearanceSection
                        preferencesSection
                        agentToolsSection
                        voiceSection
                        // Build 130.4: notes settings live in the note editor's
                        // own toolbar/context menu on iPhone; the Settings rows
                        // stay visible on iPad where there is room for them.
                        if DeviceClass.isPad {
                            notesDefaultsSection
                            notesSection
                        }
                        locationSection
                        privacySection
                        aboutSection
                    }
                    .frame(maxWidth: .infinity)
                    .padding(.horizontal, Design.Spacing.md)
                    .padding(.vertical, Design.Spacing.sm)
                }
                .frame(width: geo.size.width)
            }
        }
        .navigationTitle("Settings")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                if router.activeSheet != nil {
                    Button { dismiss() } label: {
                        Image(systemName: "xmark")
                            .font(.system(size: Design.Size.iconSmall, weight: .semibold))
                            .foregroundStyle(Design.Colors.foreground)
                    }
                }
            }
        }
        .task {
            await hostStore.refresh()
            await permissionsStore.reloadCapabilities()
            // Build 70: latency is monitored at the container level from
            // connection time; Settings just renders the live value.
        }
        .sheet(isPresented: $showSafari) {
            if let url = safariURL {
                SafariView(url: url)
            }
        }
        .sheet(item: $auxPickerTask) { task in
            AuxModelPickerSheet(
                task: task,
                models: modelStore.models,
                searchText: $auxPickerSearchText,
                onSelect: { provider, model in
                    Task { await auxService?.set(task: task.task, provider: provider, model: model) }
                    auxPickerTask = nil
                }
            )
        }
        // Build 128.95: enrichment model picker sheet. Takes a snapshot of
        // modelStore.models so its List stays stable while open.
        .sheet(isPresented: $isEnrichmentPickerPresented) {
            EnrichmentModelPickerSheet(
                models: modelStore.models,
                searchText: $enrichmentPickerSearchText,
                selectedModelName: settingsStore.settings.notesEnrichmentModelName,
                selectedProvider: settingsStore.settings.notesEnrichmentProvider,
                onSelect: { provider, model in
                    settingsStore.settings.notesEnrichmentModelName = model
                    settingsStore.settings.notesEnrichmentProvider = provider
                    isEnrichmentPickerPresented = false
                },
                onSelectDefault: {
                    settingsStore.settings.notesEnrichmentModelName = nil
                    settingsStore.settings.notesEnrichmentProvider = nil
                    isEnrichmentPickerPresented = false
                }
            )
        }
        // Build 33: restart confirmation. Shown only from the
        // .awaitingConfirmation state; Cancel is a pure state reset with
        // zero network calls.
        .alert(
            "Restart Hermes Agent?",
            isPresented: restartConfirmationBinding,
            presenting: awaitingPreflight
        ) { preflight in
            Button("Cancel", role: .cancel) { cancelHermesRestart() }
            Button("Restart Hermes", role: .destructive) {
                Task { await confirmHermesRestart(preflight: preflight) }
            }
        } message: { preflight in
            VStack(alignment: .leading, spacing: 4) {
                Text("Profile: \(preflight.profile)")
                Text("Service unit: \(preflight.unit)")
                if preflight.activeRequestCount > 0 {
                    Text("Hermes is handling \(preflight.activeRequestCount) active request\(preflight.activeRequestCount == 1 ? "" : "s"). Restarting will interrupt them.")
                }
                Text("Active chat, voice, tool activity, and streams will be interrupted.")
                if let notice = stalePreflightNotice {
                    Text(notice)
                        .foregroundStyle(Design.Colors.warning)
                }
            }
        }
    }

    // MARK: - Connection

    /// The effective connection status shown in the settings screen.
    ///
    /// Uses the actual relay connection status from `ChatStore` (which tracks
    /// `LiveHeraldClient.connectionStatus`) when it reflects an error, falling
    /// back to the bootstrap session status. When the host is definitively
    /// online (gateway confirms connectivity), a transient WS "connecting"
    /// state is suppressed so the UI doesn't contradict the gateway dashboard.
    private var effectiveConnectionStatus: ConnectionStatus {
        let relayStatus = chatStore.connectionStatus
        // The host row is fed by hostInfo() over the live gateway socket.
        // When that succeeded, the gateway IS reachable - even if the chat
        // client carries a stale .error from an earlier WS drop. Prefer the
        // definitive host state so Settings doesn't show "Error/Offline"
        // while the gateway status page shows Connected.
        if hostStore.isHostOnline {
            return .connected
        }
        if relayStatus == .error {
            return relayStatus
        }
        // If the host is confirmed online, don't show "connecting" — the WS
        // handshake may just be in progress. The gateway status page already
        // shows the definitive state.
        if relayStatus == .connecting && hostStore.isHostOnline {
            return .connected
        }
        // Build 128.89 (settings "Error" flash): sessionStore.state.
        // connectionStatus is legacy bootstrap state. In native mode
        // bootstrap never runs, so a stale .error persisted from a prior
        // session's failed bootstrap would flash "Error" in Settings for a
        // second on every launch until hostStore.refresh() confirms online.
        // The native client's live status (relayStatus) is authoritative -
        // it never reports .error, only connecting/connected/reconnecting.
        if container.nativeGatewayClient != nil {
            return relayStatus
        }
        return sessionStore.state.connectionStatus
    }

    private var connectionSection: some View {
        SettingsSectionView(title: "Connection") {
            VStack(spacing: 0) {
                settingsRow(
                    icon: effectiveConnectionStatus.displayIcon,
                    iconColor: effectiveConnectionStatus.displayColor,
                    title: "Status",
                    value: effectiveConnectionStatus.displayLabel
                )

                sectionDivider

                // Build 69: live round-trip to the connector. Polled while
                // this screen is visible so it always reflects the current
                // connection, not a stale launch-time value.
                settingsRow(
                    icon: "timer",
                    iconColor: latencyRowColor,
                    title: "Latency",
                    value: latencyRowValue,
                    valueColor: latencyRowValueColor
                )

                sectionDivider

                if pairingStore.pairedRelayConfiguration != nil {
                    NavigationLink(value: Route.connectHost) {
                        HStack(spacing: Design.Spacing.sm) {
                            Image(systemName: hostStatusRowIcon)
                                .font(.system(size: 14))
                                .foregroundStyle(hostStatusRowColor)
                                .frame(width: 20, alignment: .center)

                            Text("Hermes Host")
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.foreground)

                            Spacer()

                            Text(hostStatusRowValue)
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.secondaryForeground)

                            Image(systemName: "chevron.right")
                                .font(.system(size: 12, weight: .semibold))
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                        .frame(minHeight: Design.Size.minTapTarget)
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("settings.heraldHost")

                    sectionDivider
                }

                settingsToggle(
                    icon: "bolt.fill",
                    iconColor: Design.Colors.foreground,
                    title: "Auto-Connect",
                    isOn: autoConnectBinding
                )

                // Manual reset connection: tears down stale transport and
                // starts a fresh authenticated connection. Idempotent.
                // Build 131.5: the button now shows a spinner + disables while
                // resetting so the user sees it actually do something. The
                // prior versions fired a fire-and-forget Task with no visual
                // state, so a reset that succeeded in the background read as
                // "button does nothing" when the status row didn't change fast
                // enough (or at all, if the relay status was already stale).
                if container.nativeGatewayClient != nil {
                    sectionDivider

                    Button {
                        isResettingConnection = true
                        Task {
                            await container.nativeGatewayClient?.resetConnection()
                            isResettingConnection = false
                        }
                    } label: {
                        HStack(spacing: Design.Spacing.sm) {
                            if isResettingConnection {
                                ProgressView()
                                    .controlSize(.small)
                                    .frame(width: 20, alignment: .center)
                            } else {
                                Image(systemName: "arrow.clockwise.circle.fill")
                                    .font(.system(size: 14))
                                    .foregroundStyle(.orange)
                                    .frame(width: 20, alignment: .center)
                            }

                            Text(isResettingConnection ? "Resetting..." : "Reset Connection")
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.foreground)

                            Spacer()
                        }
                        .frame(minHeight: Design.Size.minTapTarget)
                    }
                    .buttonStyle(.plain)
                    .disabled(isResettingConnection)
                    .accessibilityIdentifier("settings.resetConnection")
                }
            }
        }
    }

    @State private var isRestartingGW = false
    @State private var gwRestartTarget: String?
    @State private var gwRestartResult: String?
    @State private var updateCheckResult: String?
    /// Build 131.5: true while Reset Connection is tearing down + reconnecting,
    /// drives the button's spinner/disabled state so the action is visible.
    @State private var isResettingConnection = false
    // Build 70: latency is polled on the AppContainer (starts at connection
    // time), so Settings just reads the live value - no local poller.
    private var latencyMs: Int? { container.connectorLatencyMs }
    @State private var updateAgentResult: String?
    @State private var isCheckingForUpdate = false
    @State private var isUpdatingAgent = false
    // Build 69 (r7): structured update info + changelog window state.
    @State private var updateInfo: NativeKallistiClient.HermesUpdateInfo?
    @State private var isChangelogPresented = false
    @State private var skippedVersion: String?
    // Build 128.49: live update progress streamed from the connector while
    // `hermes update --yes` runs in the background.
    @State private var updateProgressLines: [String] = []
    @State private var updateProgressState: String? // idle | running | done | failed
    private let skippedVersionKey = "kallisti.skippedUpdateVersion"
    private var skippedVersionCurrent: String? {
        UserDefaults.standard.string(forKey: skippedVersionKey)
    }

    // Build 33: restart-safe Hermes agent restart state machine.
    // Restart Hermes Agent → preflight → confirmation → idempotent submit →
    // poll → healthy/failed. The service itself is app-scoped, so an
    // in-flight restart keeps polling even if Settings is dismissed.
    private enum RestartUIState {
        case idle
        case loadingPreflight
        case awaitingConfirmation(preflight: RestartPreflight)
        case inProgress(operation: RestartOperation)
        case healthy(operation: RestartOperation)
        case failed(operation: RestartOperation)
    }

    @State private var restartState: RestartUIState = .idle
    @State private var stalePreflightNotice: String?

    // MARK: - Environment

    private var relaySection: some View {
        SettingsSectionView(title: "Relay") {
            VStack(alignment: .leading, spacing: Design.Spacing.sm) {
                if pairingStore.isPaired {
                    settingsRow(
                        icon: "point.3.connected.trianglepath.dotted",
                        iconColor: Design.Colors.foreground,
                        title: "Active Relay",
                        value: pairingStore.pairedRelayConfiguration?.hostDisplayName ?? relayConfiguration.relayOriginLabel
                    )
                    sectionDivider
                    settingsRow(
                        icon: "link",
                        iconColor: Design.Colors.secondaryForeground,
                        title: "Base URL",
                        value: pairingStore.pairedRelayConfiguration?.baseURLString ?? relayConfiguration.activeBaseURLString ?? "Not configured"
                    )
                    Text("Disconnect Kallisti before changing the relay configuration.")
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .padding(.top, Design.Spacing.xs)
                } else {
                    VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                        Text("CONNECTION MODE").brandEyebrow()

                        Picker("Connection Mode", selection: connectionModeBinding) {
                            ForEach(relayConfiguration.selectableConnectionModes, id: \.self) { mode in
                                Text(mode.compactLabel).tag(mode)
                            }
                        }
                        .pickerStyle(.segmented)
                    }

                    sectionDivider

                    if relayConfiguration.connectionMode.usesCustomRelayURL {
                        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                            TextField(customRelayURLPlaceholder, text: customRelayURLBinding)
                                .textInputAutocapitalization(.never)
                                .keyboardType(.URL)
                                .autocorrectionDisabled()
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.foreground)
                                .padding(Design.Spacing.md)
                                .background(Design.Colors.background)
                                .overlay(
                                    RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                                        .stroke(Design.Colors.border, lineWidth: 1)
                                )
                                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))

                            if let hint = relayConfiguration.connectionMode.relayURLHint {
                                Text(hint)
                                    .font(Design.Typography.helper)
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                                    .fixedSize(horizontal: false, vertical: true)
                            }

                            Text(relayConfiguration.connectionMode.shortDescription)
                                .font(Design.Typography.caption)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                    } else if let hostedRelayBaseURL = relayConfiguration.hostedRelayBaseURL {
                        settingsRow(
                            icon: "cloud",
                            iconColor: Design.Colors.foreground,
                            title: "Hosted Relay",
                            value: hostedRelayBaseURL
                        )
                    }

                    Text(backgroundDeliveryNote)
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .fixedSize(horizontal: false, vertical: true)

                    if let relayValidationMessage {
                        Text(relayValidationMessage)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.warning)
                    }
                }
            }
        }
    }

    private var latencyRowValue: String {
        if let ms = latencyMs {
            return "\(ms)ms"
        }
        if let nativeClient = container.nativeGatewayClient,
           nativeClient.connectionStatus == .connected {
            return "Measuring…"
        }
        return "—"
    }

    /// Build 70: the value itself turns danger-red when the connection is
    /// hot; stays black otherwise (the icon keeps its traffic-light colors).
    private var latencyRowValueColor: Color {
        guard let ms = latencyMs else { return Design.Colors.secondaryForeground }
        return ms >= 300 ? Design.Colors.danger : Design.Colors.foreground
    }

    private var latencyRowColor: Color {
        guard let ms = latencyMs else { return Design.Colors.secondaryForeground }
        if ms < 100 { return Design.Colors.success }
        if ms < 300 { return Design.Colors.warning }
        return Design.Colors.danger
    }

    private var hostStatusRowIcon: String {
        switch hostStore.connectionState {
        case .online:
            return "desktopcomputer"
        case .offline:
            return "desktopcomputer.trianglebadge.exclamationmark"
        case .unreachable:
            return "wifi.exclamationmark"
        case .notConnected:
            return "desktopcomputer"
        }
    }

    private var hostStatusRowColor: Color {
        switch hostStore.connectionState {
        case .online:
            return Design.Colors.success
        case .offline, .unreachable:
            return Design.Colors.warning
        case .notConnected:
            return Design.Colors.secondaryForeground
        }
    }

    private var hostStatusRowValue: String {
        switch hostStore.connectionState {
        case .online, .offline:
            return hostStore.currentHost?.resolvedDisplayName ?? "Hermes Host"
        case .unreachable:
            return "Status unavailable"
        case .notConnected:
            return "Not Connected"
        }
    }

    // MARK: - Gateway

    private var gatewaySection: some View {
        SettingsSectionView(title: "Gateway") {
            VStack(spacing: 0) {
                // Gateway status summary
                NavigationLink(value: Route.gatewayStatus) {
                    HStack(spacing: Design.Spacing.sm) {
                        Image(systemName: hostStore.isHostOnline ? "network" : "network.slash")
                            .font(.system(size: 14))
                            .foregroundStyle(hostStore.isHostOnline ? Design.Colors.success : Design.Colors.warning)
                            .frame(width: 20, alignment: .center)

                        Text("Gateway Status")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.foreground)

                        Spacer()

                        Text(hostStore.isHostOnline ? "Online" : "Offline")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.secondaryForeground)

                        Image(systemName: "chevron.right")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                    .frame(minHeight: Design.Size.minTapTarget)
                }
                .buttonStyle(.plain)

                sectionDivider

                // Restart connector — legacy flow. The facade only allowlists
                // `hermes|connector`, so there is deliberately NO "Restart
                // Relay" row: the relay is embedded in the connector.
                gatewayRestartButton(label: "Restart Connector", target: "connector")

                sectionDivider

                // Restart Hermes agent — Build 33 flow: preflight →
                // confirmation → idempotent submit → poll → healthy/failed.
                hermesRestartRow

                if isHermesRestartActive {
                    sectionDivider
                    restartStatusArea
                }

                sectionDivider

                // View logs entry
                NavigationLink(value: Route.gatewayLogs) {
                    HStack(spacing: Design.Spacing.sm) {
                        Image(systemName: "doc.text.magnifyingglass")
                            .font(.system(size: 14))
                            .foregroundStyle(.blue)
                            .frame(width: 20, alignment: .center)

                        Text("View Logs")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.foreground)

                        Spacer()

                        Image(systemName: "chevron.right")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                    .frame(minHeight: Design.Size.minTapTarget)
                }
                .buttonStyle(.plain)

                sectionDivider

                // Build 128.41: config.yaml editor - fetch/save the Hermes
                // config from the host (connector backs up before writing).
                NavigationLink(value: Route.configEditor) {
                    HStack(spacing: Design.Spacing.sm) {
                        Image(systemName: "curlybraces")
                            .font(.system(size: 14))
                            .foregroundStyle(Design.Brand.accent)
                            .frame(width: 20, alignment: .center)

                        Text("Config Editor")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.foreground)

                        Spacer()

                        Image(systemName: "chevron.right")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                    .frame(minHeight: Design.Size.minTapTarget)
                }
                .buttonStyle(.plain)

                sectionDivider

                // Build 69 (r7): expandable Software Update row.
                softwareUpdateSection
            }
        }
    }

    /// Check for Updates + (when available) an expandable "new version"
    /// bar with a changelog link, Update Now, and Skip actions.
    @ViewBuilder
    private var softwareUpdateSection: some View {
        VStack(spacing: Design.Spacing.xs) {
            gatewayActionButton(
                label: "Check for Updates",
                icon: "arrow.triangle.2.circlepath",
                isLoading: isCheckingForUpdate,
                result: updateCheckResult,
                action: { await checkForUpdates() }
            )

            if let info = updateInfo, info.updateAvailable == true,
               let latest = info.latestVersion, latest != skippedVersionCurrent {
                sectionDivider

                Button {
                    isChangelogPresented = true
                } label: {
                    HStack(spacing: Design.Spacing.sm) {
                        Image(systemName: "sparkles")
                            .font(.system(size: 14))
                            .foregroundStyle(Design.Brand.accent)
                            .frame(width: 20, alignment: .center)

                        VStack(alignment: .leading, spacing: 2) {
                            Text("New version available")
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.foreground)
                            Text("Hermes Agent \(latest)")
                                .font(Design.Typography.caption)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }

                        Spacer()

                        Text("Changelog")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Brand.accent)
                        Image(systemName: "chevron.right")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                    .frame(minHeight: Design.Size.minTapTarget)
                }
                .buttonStyle(.plain)
            }

            if let info = updateInfo, info.updateAvailable == false,
               let latest = info.latestVersion {
                sectionDivider

                HStack(spacing: Design.Spacing.sm) {
                    Image(systemName: "checkmark.circle")
                        .font(.system(size: 14))
                        .foregroundStyle(Design.Colors.success)
                        .frame(width: 20, alignment: .center)
                    Text("Up to date (\(latest))")
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.foreground)
                    Spacer()
                }
                .frame(minHeight: Design.Size.minTapTarget)
            }
        }
        .sheet(isPresented: $isChangelogPresented) {
            if let info = updateInfo {
                UpdateChangelogSheet(
                    info: info,
                    isUpdating: $isUpdatingAgent,
                    progressLines: updateProgressLines,
                    progressState: updateProgressState,
                    onUpdateNow: {
                        Task { await updateAgent() }
                    },
                    onSkip: {
                        if let latest = info.latestVersion {
                            UserDefaults.standard.set(latest, forKey: skippedVersionKey)
                            skippedVersion = latest
                        }
                        isChangelogPresented = false
                    }
                )
            }
        }
    }

    private func gatewayActionButton(
        label: String,
        icon: String,
        isLoading: Bool = false,
        result: String? = nil,
        action: @escaping () async -> Void
    ) -> some View {
        Button {
            Task { await action() }
        } label: {
            HStack(spacing: Design.Spacing.sm) {
                if isLoading {
                    ProgressView()
                        .controlSize(.small)
                        .tint(.blue)
                } else {
                    Image(systemName: icon)
                        .font(.system(size: 14))
                        .foregroundStyle(.blue)
                        .frame(width: 20, alignment: .center)
                }

                Text(label)
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.foreground)

                Spacer()

                if let result {
                    Text(result)
                        .font(Design.Typography.caption)
                        .foregroundStyle(result.hasPrefix("Check failed") || result.hasPrefix("Update failed")
                            ? Design.Colors.danger : Design.Colors.success)
                        .lineLimit(1)
                }
            }
            .frame(minHeight: Design.Size.minTapTarget)
        }
        .buttonStyle(.plain)
    }

    private func checkForUpdates() async {
        isCheckingForUpdate = true
        updateCheckResult = nil

        // Build 69 (r7): native mode reads the connector's structured
        // /v1/gw/update/check payload (cookie/bearer auth) so the row can
        // show the real version + changelog. Falls back to `hermes update
        // --check` text if the connector call returns nothing.
        if let nativeClient = container.nativeGatewayClient {
            if let info = await nativeClient.updateCheck() {
                updateInfo = info
                if let err = info.error, !err.isEmpty {
                    updateCheckResult = "Check failed: \(err)"
                } else if info.updateAvailable == true {
                    updateCheckResult = "Update available"
                } else if info.updateAvailable == false {
                    updateCheckResult = "Up to date"
                } else {
                    updateCheckResult = "Update check complete"
                }
                if info.updateAvailable == true {
                    isCheckingForUpdate = false
                    return
                }
                if info.updateAvailable == false {
                    isCheckingForUpdate = false
                    return
                }
            }
        }
        if let featureClient = container.nativeGatewayClient?.featureClient {
            do {
                let output = try await featureClient.cliExec(argv: ["update", "--check"], timeout: 120)
                let trimmed = output.trimmingCharacters(in: .whitespacesAndNewlines)
                updateCheckResult = trimmed.isEmpty ? "Update check complete" : trimmed
            } catch {
                updateCheckResult = "Check failed: \(error.localizedDescription)"
            }
        } else {
            let relayBase = settingsStore.settings.relayConfiguration.activeBaseURLString
                ?? pairingStore.pairedRelayConfiguration?.baseURLString
            guard let relayBase else { return }
            let token = await sessionStore.currentAccessToken()
            let client = RelayAPIClient { relayBase }

            struct EmptyRequest: Encodable {}
            struct UpdateStatus: Decodable {
                let status: String?
                let message: String?
            }
            do {
                let status: UpdateStatus = try await client.postGateway(
                    path: "gw/update/check",
                    body: EmptyRequest(),
                    accessToken: token ?? ""
                )
                updateCheckResult = status.message ?? status.status ?? "Update check complete"
            } catch {
                updateCheckResult = "Check failed: \(error.localizedDescription)"
            }
        }

        isCheckingForUpdate = false
        // Auto-clear result after 8 seconds
        try? await Task.sleep(for: .seconds(8))
        updateCheckResult = nil
    }

    private func updateAgent() async {
        isUpdatingAgent = true
        updateAgentResult = nil
        updateProgressLines = []
        updateProgressState = nil

        // Build 128.49: NATIVE mode runs `hermes update --yes` on the host in
        // the background (connector survives it) and streams live output via
        // /v1/gw/update/progress - the same "watch it happen" feel the
        // Electron dashboard gets. Falls back to blocking cli.exec only when
        // the connector progress path is unavailable.
        if let nativeClient = container.nativeGatewayClient {
            if let initial = await nativeClient.startUpdate() {
                updateProgressLines = initial.output ?? []
                updateProgressState = initial.state
                // Poll until terminal. The connector keeps the process state
                // for the whole update even if the gateway restarts under it.
                var lastState = initial.state
                var pollCount = 0
                while lastState == "running" && pollCount < 600 {
                    try? await Task.sleep(for: .seconds(1))
                    pollCount += 1
                    guard let progress = await nativeClient.updateProgress() else {
                        // Transient poll failure - keep waiting unless the
                        // socket is clearly dead.
                        continue
                    }
                    if let lines = progress.output {
                        updateProgressLines = lines
                    }
                    lastState = progress.state
                    updateProgressState = progress.state
                    if progress.isDone || progress.isFailed {
                        break
                    }
                }
                if lastState == "done" {
                    updateAgentResult = "Update complete"
                } else if lastState == "failed" {
                    updateAgentResult = "Update failed"
                } else {
                    updateAgentResult = "Update still running"
                }
                isUpdatingAgent = false
                // Auto-refresh the section so it reflects the new version
                // instead of showing a stale "Update available" row.
                await checkForUpdates()
                updateAgentResult = nil
                updateProgressLines = []
                updateProgressState = nil
                isChangelogPresented = false
                try? await Task.sleep(for: .seconds(8))
                updateCheckResult = nil
                return
            }
            // Fall through to legacy blocking path if startUpdate failed.
            do {
                let output = try await container.nativeGatewayClient?.featureClient.cliExec(argv: ["update", "--yes"], timeout: 300)
                let trimmed = (output ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
                updateAgentResult = trimmed.isEmpty ? "Update complete" : trimmed
            } catch {
                updateAgentResult = "Update failed: \(error.localizedDescription)"
            }
        } else {
            let relayBase = settingsStore.settings.relayConfiguration.activeBaseURLString
                ?? pairingStore.pairedRelayConfiguration?.baseURLString
            guard let relayBase else { return }
            let token = await sessionStore.currentAccessToken()
            let client = RelayAPIClient { relayBase }

            struct EmptyRequest: Encodable {}
            struct UpdateStatus: Decodable {
                let status: String?
                let message: String?
        }
        do {
            let status: UpdateStatus = try await client.postGateway(
                path: "gw/update",
                body: EmptyRequest(),
                accessToken: token ?? ""
            )
            updateAgentResult = status.message ?? status.status ?? "Update initiated"
        } catch {
            updateAgentResult = "Update failed: \(error.localizedDescription)"
        }
        }

        isUpdatingAgent = false
        // Build 128.49: refresh the section after the legacy update path too,
        // so the row never sits on a stale "Update available".
        await checkForUpdates()
        updateAgentResult = nil
        try? await Task.sleep(for: .seconds(8))
        updateCheckResult = nil
    }

    private func gatewayRestartButton(label: String, target: String) -> some View {
        Button {
            Task { await restartGateway(target: target) }
        } label: {
            HStack(spacing: Design.Spacing.sm) {
                if isRestartingGW && gwRestartTarget == target {
                    ProgressView()
                        .controlSize(.small)
                        .tint(.orange)
                } else {
                    Image(systemName: "arrow.triangle.2.circlepath")
                        .font(.system(size: 14))
                        .foregroundStyle(.orange)
                        .frame(width: 20)
                }

                Text(isRestartingGW && gwRestartTarget == target ? "Restarting…" : label)
                    .font(Design.Typography.callout)
                    .foregroundStyle(isRestartingGW ? Design.Colors.secondaryForeground : Design.Colors.foreground)

                Spacer()

                if let result = gwRestartResult, gwRestartTarget == target {
                    Text(result)
                        .font(Design.Typography.caption)
                        .foregroundStyle(result.hasPrefix("Failed") || result.hasPrefix("Error")
                                         ? Design.Colors.danger : .green)
                        .lineLimit(1)
                }
            }
            .frame(minHeight: Design.Size.minTapTarget)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(isRestartingGW)
    }

    /// Legacy restart path — used only for the connector target. The
    /// pre-Build-33-style call (no Idempotency-Key) is still supported by the
    /// connector, and `RestartResponse` remains the backward-compat decoder.
    /// The Hermes agent restart uses the new preflight/confirm/operation flow
    /// below instead.
    private func restartGateway(target: String) async {
        isRestartingGW = true
        gwRestartTarget = target
        gwRestartResult = nil

        // NATIVE mode: restart the gateway via `hermes gateway restart`
        // through cli.exec. The gateway runs it locally - no relay token.
        if let featureClient = container.nativeGatewayClient?.featureClient {
            do {
                let output = try await featureClient.cliExec(argv: ["gateway", "restart"], timeout: 60)
                let trimmed = output.trimmingCharacters(in: .whitespacesAndNewlines)
                gwRestartResult = trimmed.isEmpty ? "\(target) restarting…" : trimmed
            } catch {
                gwRestartResult = "Error: \(error.localizedDescription)"
            }
            isRestartingGW = false
            try? await Task.sleep(for: .seconds(3))
            gwRestartResult = nil
            return
        }

        do {
            try await withTimeout(seconds: 15) {
                let relayBase = settingsStore.settings.relayConfiguration.activeBaseURLString
                    ?? pairingStore.pairedRelayConfiguration?.baseURLString
                guard let relayBase else {
                    gwRestartResult = "No relay configured."
                    return
                }
                let token = await sessionStore.currentAccessToken()
                // Build 107: check for nil/empty token before making the request.
                // Passing an empty string causes a 401 error from the connector.
                guard let token, !token.isEmpty else {
                    gwRestartResult = "Not authenticated — please pair your device first."
                    return
                }
                let client = RelayAPIClient { relayBase }

                struct RestartRequest: Encodable { let target: String }

                let body = RestartRequest(target: target)
                let response: RestartResponse = try await client.postGateway(
                    path: "gw/restart",
                    body: body,
                    accessToken: token
                )
                gwRestartResult = response.restarting
                    ? "\(target) restarting…"
                    : "Failed: \(response.error ?? response.message ?? "no reason returned")"
            }
        } catch {
            gwRestartResult = error is TimeoutError
                ? "Request timed out — check the host"
                : "Error: \(error.localizedDescription)"
        }

        isRestartingGW = false
        try? await Task.sleep(for: .seconds(3))
        gwRestartResult = nil
    }

    // MARK: - Hermes Restart Flow (Build 33)

    /// True while the restart has a visible status card.
    private var isHermesRestartActive: Bool {
        switch restartState {
        case .inProgress, .healthy, .failed: return true
        case .idle, .loadingPreflight, .awaitingConfirmation: return false
        }
    }

    /// True while the Hermes row must be inert (a restart is loading or
    /// running). The confirmation state keeps the row tappable so the user
    /// can re-initiate if they cancelled the alert.
    private var isHermesRestartBusy: Bool {
        switch restartState {
        case .loadingPreflight, .inProgress: return true
        case .idle, .awaitingConfirmation, .healthy, .failed: return false
        }
    }

    private var hermesRestartRow: some View {
        Button {
            Task { await beginHermesRestart() }
        } label: {
            HStack(spacing: Design.Spacing.sm) {
                restartRowIcon

                Text(restartRowLabel)
                    .font(Design.Typography.callout)
                    .foregroundStyle(
                        isHermesRestartBusy
                            ? Design.Colors.secondaryForeground
                            : Design.Colors.foreground
                    )

                Spacer()
            }
            .frame(minHeight: Design.Size.minTapTarget)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(isHermesRestartBusy)
        .accessibilityIdentifier("settings.restartHermes")
    }

    @ViewBuilder
    private var restartRowIcon: some View {
        switch restartState {
        case .idle, .awaitingConfirmation:
            Image(systemName: "arrow.triangle.2.circlepath")
                .font(.system(size: 14))
                .foregroundStyle(.orange)
                .frame(width: 20, alignment: .center)
        case .loadingPreflight, .inProgress:
            ProgressView()
                .controlSize(.small)
                .tint(.orange)
        case .healthy:
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 14))
                .foregroundStyle(Design.Colors.success)
                .frame(width: 20, alignment: .center)
        case .failed:
            Image(systemName: "exclamationmark.circle.fill")
                .font(.system(size: 14))
                .foregroundStyle(Design.Colors.danger)
                .frame(width: 20, alignment: .center)
        }
    }

    private var restartRowLabel: String {
        switch restartState {
        case .idle, .awaitingConfirmation, .healthy, .failed:
            return "Restart Hermes Agent"
        case .loadingPreflight:
            return "Loading…"
        case .inProgress:
            return "Restarting…"
        }
    }

    /// Status card under the Hermes row while a restart is active.
    @ViewBuilder
    private var restartStatusArea: some View {
        switch restartState {
        case .inProgress:
            restartProgressCard
        case .healthy:
            restartHealthyCard
        case .failed(let operation):
            restartFailedCard(operation)
        case .idle, .loadingPreflight, .awaitingConfirmation:
            EmptyView()
        }
    }

    /// Live phase + check progress. Reads the service's currentOperation so
    /// every poll tick re-renders through @Observable.
    private var restartProgressCard: some View {
        let operation = gatewayControl.currentOperation
        return VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            HStack(spacing: Design.Spacing.sm) {
                ProgressView()
                    .controlSize(.small)
                    .tint(.orange)
                Text(phaseDisplayLabel(operation?.phase))
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.foreground)
                Spacer()
            }

            if let checks = operation?.checks, !checks.isEmpty {
                ForEach(checks) { check in
                    HStack(spacing: Design.Spacing.xs) {
                        Image(systemName: check.passed ? "checkmark.circle.fill" : "xmark.circle.fill")
                            .font(.system(size: 11))
                            .foregroundStyle(check.passed ? Design.Colors.success : Design.Colors.danger)
                        Text(check.name)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.foreground)
                        Spacer()
                        Text(check.detail)
                            .font(Design.Typography.caption2)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .lineLimit(1)
                    }
                }
            }
        }
        .padding(Design.Spacing.md)
        .background(Design.Colors.backgroundRaised, in: RoundedRectangle(cornerRadius: Design.CornerRadius.md))
    }

    private func phaseDisplayLabel(_ phase: RestartPhase?) -> String {
        switch phase {
        case .accepted: return "Queued…"
        case .stopping: return "Stopping Hermes…"
        case .starting: return "Starting Hermes…"
        case .verifying: return "Verifying Hermes…"
        case .healthy: return "Hermes is ready"
        case .failed: return "Restart failed"
        case nil: return "Restarting…"
        }
    }

    private var restartHealthyCard: some View {
        HStack(spacing: Design.Spacing.sm) {
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 14))
                .foregroundStyle(Design.Colors.success)
            VStack(alignment: .leading, spacing: 2) {
                Text("Hermes is ready")
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.foreground)
                Text("Conversation, models, and logs reloaded.")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
            Spacer()
        }
        .padding(Design.Spacing.md)
        .background(Design.Colors.backgroundRaised, in: RoundedRectangle(cornerRadius: Design.CornerRadius.md))
    }

    /// Typed failure card — stage, recovery action, journal excerpt. Never
    /// raw `DecodingError` text or Python dicts: client-side failures are
    /// folded into `RestartErrorDetail` by `RestartOperation.localFailure`.
    private func restartFailedCard(_ operation: RestartOperation) -> some View {
        let detail = operation.error
        return VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            HStack(spacing: Design.Spacing.sm) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 14))
                    .foregroundStyle(Design.Colors.danger)
                Text("Restart failed")
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.foreground)
                Spacer()
                Button("Dismiss") {
                    gatewayControl.reset()
                    restartState = .idle
                    stalePreflightNotice = nil
                }
                .font(Design.Typography.caption)
            }

            if let detail {
                Text("Stage: \(detail.stage)")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                if let excerpt = detail.journalExcerpt, !excerpt.isEmpty {
                    Text(excerpt)
                        .font(Design.Typography.caption2)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .lineLimit(2)
                }
                if let action = detail.action, !action.isEmpty {
                    Text(action)
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.warning)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            if detail?.retryable != false {
                Button("Try Again") {
                    Task { await beginHermesRestart() }
                }
                .font(Design.Typography.caption.weight(.semibold))
            }
        }
        .padding(Design.Spacing.md)
        .background(Design.Colors.backgroundRaised, in: RoundedRectangle(cornerRadius: Design.CornerRadius.md))
    }

    /// Step 1: fetch preflight, then present the confirmation dialog.
    private func beginHermesRestart() async {
        stalePreflightNotice = nil
        restartState = .loadingPreflight
        do {
            let preflight = try await withTimeout(seconds: 15) {
                try await gatewayControl.fetchPreflight(target: "hermes")
            }
            guard preflight.canRestart else {
                restartState = .failed(operation: RestartOperation.localFailure(
                    stage: "preflight",
                    unit: preflight.unit,
                    action: preflight.blocker
                        ?? "The gateway is not ready to restart right now. Check the host status."
                ))
                return
            }
            restartState = .awaitingConfirmation(preflight: preflight)
        } catch {
            restartState = .failed(operation: RestartOperation.localFailure(
                stage: "preflight",
                action: Self.describeRestartError(error)
            ))
        }
    }

    /// Step 2 (destructive alert button): submit the idempotent restart, then
    /// suspend the chat and poll until terminal.
    private func confirmHermesRestart(preflight: RestartPreflight) async {
        // Move off .awaitingConfirmation immediately so the alert's automatic
        // dismiss cannot be misread as a user cancel (the binding reset below
        // only fires while still in the confirmation state).
        restartState = .loadingPreflight

        let operation: RestartOperation
        do {
            operation = try await withTimeout(seconds: 15) {
                try await gatewayControl.submitRestart(target: "hermes", preflight: preflight)
            }
        } catch GatewayControlError.stalePreflight {
            // The gateway state moved since preflight — re-fetch and require
            // re-confirmation instead of restarting blind. The notice is set
            // AFTER beginHermesRestart (which clears it) so the new
            // confirmation dialog carries it.
            await beginHermesRestart()
            stalePreflightNotice = "The gateway state changed since you confirmed. Review and confirm again."
            return
        } catch {
            restartState = .failed(operation: RestartOperation.localFailure(
                stage: "restart",
                unit: preflight.unit,
                action: Self.describeRestartError(error)
            ))
            return
        }

        restartState = .inProgress(operation: operation)
        chatStore.beginRestartSuspension()

        do {
            let terminal = try await gatewayControl.pollUntilTerminal(operation: operation)
            restartState = terminal.phase == .healthy
                ? .healthy(operation: terminal)
                : .failed(operation: terminal)
            if terminal.phase == .healthy {
                await recoverAfterRestart()
            } else {
                // Gateway came back but verification failed — sends are safe
                // to re-enable so the user can retry through the UI.
                await chatStore.resumeAfterRestart()
            }
        } catch {
            restartState = .failed(operation: RestartOperation.localFailure(
                stage: "verifying",
                unit: operation.unit,
                action: Self.describeRestartError(error)
            ))
            await chatStore.resumeAfterRestart()
        }
    }

    /// Step 3: post-restart recovery when the operation reports healthy.
    /// Re-enables send (drains visibly-queued messages), reloads the current
    /// conversation, reconciles running jobs via the polling safety net,
    /// reloads models, and refreshes host state.
    private func recoverAfterRestart() async {
        await chatStore.resumeAfterRestart()
        await chatStore.reloadConversationAfterRestart()
        await modelStore.loadModels(force: true)
        await hostStore.refresh()
        chatStore.appendLog(level: .info, "Hermes gateway restart verified healthy — state reloaded")

        // Auto-clear the ready card after a moment.
        try? await Task.sleep(for: .seconds(4))
        if case .healthy = restartState { restartState = .idle }
    }

    /// Cancel from the confirmation dialog — zero network calls.
    private func cancelHermesRestart() {
        restartState = .idle
        stalePreflightNotice = nil
    }

    private static func describeRestartError(_ error: Error) -> String {
        if error is TimeoutError {
            return "The gateway did not respond in time. Check that the connector is reachable, then try again."
        }
        if let clientError = error as? RelayAPIClient.ClientError {
            return clientError.errorDescription
                ?? "The gateway request failed. Try again."
        }
        if error is DecodingError {
            // Never surface DecodingError text or raw server payloads.
            return "The gateway returned an unexpected response. Try again, or check the connector version."
        }
        return error.localizedDescription
    }

    /// Preflight to show in the confirmation alert, if any.
    private var awaitingPreflight: RestartPreflight? {
        if case .awaitingConfirmation(let preflight) = restartState {
            return preflight
        }
        return nil
    }

    private var restartConfirmationBinding: Binding<Bool> {
        Binding(
            get: { awaitingPreflight != nil },
            set: { presented in
                guard !presented else { return }
                // Only a dismissal while STILL in the confirmation state is a
                // user cancel. If the destructive button already moved the
                // state machine (submit in flight), let it run.
                if case .awaitingConfirmation = restartState {
                    cancelHermesRestart()
                }
            }
        )
    }

    // MARK: - Infrastructure

    private var infrastructureSection: some View {
        SettingsSectionView(title: "Infrastructure") {
            VStack(spacing: 0) {
                // Hermes Host
                HStack(spacing: Design.Spacing.sm) {
                    Image(systemName: hostStatusRowIcon)
                        .font(.system(size: 14))
                        .foregroundStyle(hostStatusRowColor)
                        .frame(width: 20, alignment: .center)

                    VStack(alignment: .leading, spacing: 2) {
                        Text("Hermes Host")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.foreground)
                        if let host = hostStore.currentHost {
                            Text(host.resolvedDisplayName)
                                .font(Design.Typography.caption)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                    }

                    Spacer()

                    Text(hostStatusRowValue)
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
                .frame(minHeight: Design.Size.minTapTarget)

                sectionDivider

                // Connector Version
                settingsRow(
                    icon: "arrow.triangle.swap",
                    iconColor: .blue,
                    title: "Connector",
                    value: hostStore.currentHost?.connectorVersion ?? "—"
                )

                sectionDivider

                // Hermes Agent Version
                settingsRow(
                    icon: "brain.head.profile",
                    iconColor: .purple,
                    title: "Hermes Agent",
                    value: hostStore.currentHost?.heraldVersion ?? "—"
                )

                sectionDivider

                // Active Model
                settingsRow(
                    icon: "cpu",
                    iconColor: .orange,
                    title: "Active Model",
                    value: modelStore.activeModel?.name
                        ?? chatStore.activeModelName
                        ?? hostStore.currentHost?.heraldModel
                        ?? "—"
                )

                // AUX Model Configuration — Build 30: always render the section
                // so it never disappears because of a load error or empty response.
                SettingsSectionView(title: "Auxiliary Models") {
                    if let aux = auxService {
                        if aux.tasks.isEmpty {
                            HStack {
                                Image(systemName: "sparkles")
                                    .font(.system(size: 14))
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                                Text("All tasks use Auto — configure a host to see options.")
                                    .font(Design.Typography.caption)
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                                Spacer()
                            }
                            .frame(minHeight: Design.Size.minTapTarget)
                        } else {
                            ForEach(aux.tasks) { task in
                                HStack {
                                    VStack(alignment: .leading) {
                                        Text(task.task)
                                            .font(Design.Typography.callout)
                                        Text(task.isAuto ? "Auto" : "\(task.provider) · \(task.model)")
                                            .font(Design.Typography.caption)
                                            .foregroundStyle(Design.Colors.secondaryForeground)
                                    }
                                    Spacer()
                                    Button {
                                        auxPickerTask = task
                                        auxPickerSearchText = ""
                                    } label: {
                                        HStack(spacing: 4) {
                                            Text("Change")
                                                .font(Design.Typography.caption)
                                            Image(systemName: "chevron.up.chevron.down")
                                                .font(.system(size: 10))
                                        }
                                        .foregroundStyle(Design.Colors.secondaryForeground)
                                    }
                                }
                                .frame(minHeight: Design.Size.minTapTarget)

                                if task.task != aux.tasks.last?.task {
                                    sectionDivider
                                }
                            }
                        }
                        if let error = aux.lastError {
                            HStack {
                                Image(systemName: "exclamationmark.triangle")
                                    .font(.system(size: 14))
                                    .foregroundStyle(Design.Colors.warning)
                                Text(error)
                                    .font(Design.Typography.caption)
                                    .foregroundStyle(Design.Colors.warning)
                                Spacer()
                                Button("Retry") {
                                    Task { await aux.load() }
                                }
                                .font(Design.Typography.caption)
                            }
                            .frame(minHeight: Design.Size.minTapTarget)
                        }
                    } else {
                        HStack {
                            ProgressView()
                                .controlSize(.mini)
                            Text("Connecting to host…")
                                .font(Design.Typography.caption)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                            Spacer()
                        }
                        .frame(minHeight: Design.Size.minTapTarget)
                    }
                }

                sectionDivider

                // Relay URL
                settingsRow(
                    icon: "point.3.connected.trianglepath.dotted",
                    iconColor: Design.Colors.foreground,
                    title: "Relay",
                    value: pairingStore.pairedRelayConfiguration?.hostDisplayName
                        ?? settingsStore.settings.relayConfiguration.relayOriginLabel
                )

                sectionDivider

                // Push Notifications Status
                settingsRow(
                    icon: "bell.badge.fill",
                    iconColor: .red,
                    title: "Push",
                    value: sessionStore.state.pushTokenRegistered
                        ? "Registered" : "Not Registered"
                )
            }
        }
    }

    private var environmentSection: some View {
        SettingsSectionView(title: "Internal Environment") {
            VStack(spacing: 0) {
                ForEach(Array(settingsStore.availableEnvironments.enumerated()), id: \.element) { index, env in
                    Button {
                        withAnimation(Design.Motion.quickResponse) {
                            settingsStore.settings.environment = env
                        }
                    } label: {
                        HStack {
                            Text(env.displayLabel)
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.foreground)

                            Spacer()

                            if settingsStore.settings.environment == env {
                                Image(systemName: "checkmark")
                                    .font(.system(size: 14, weight: .semibold))
                                    .foregroundStyle(Design.Brand.accent)
                            }
                        }
                        .frame(minHeight: Design.Size.minTapTarget)
                    }

                    if index < settingsStore.availableEnvironments.count - 1 {
                        sectionDivider
                    }
                }
            }
        }
    }

    // MARK: - Appearance

    private var appearanceSection: some View {
        SettingsSectionView(title: "Appearance") {
            VStack(alignment: .leading, spacing: Design.Spacing.sm) {
                // Herald 2.1 appearances. Each writes both stored axes
                // (preset + color scheme) via KallistiAppearance.
                VStack(spacing: 0) {
                    ForEach(KallistiAppearance.allCases) { appearance in
                        appearanceRow(appearance)
                        if appearance != KallistiAppearance.allCases.last {
                            Divider()
                                .overlay(Design.Colors.divider)
                                .padding(.leading, 34)
                        }
                    }
                }
                .accessibilityIdentifier("settings.appearance.heraldPicker")

                Divider()
                    .overlay(Design.Colors.divider)

                // Pre-2.1 themes, kept as secondary options.
                // Build 123: drop the horizontal ScrollView (it left dead
                // space after the last swatch); spread the swatches evenly
                // across the full row instead.
                VStack(alignment: .leading, spacing: 8) {
                    Text("Other Themes")
                        .brandEyebrow()
                    HStack(spacing: 12) {
                        ForEach(ThemePreset.legacyPresets) { theme in
                            themeSwatch(theme)
                                .frame(maxWidth: .infinity)
                        }
                    }
                }

                // The light/dark/system control only applies to the pre-2.1
                // presets — the Herald appearances above already encode it.
                if !themeManager.preset.isKallistiBrand {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Light / Dark")
                            .brandEyebrow()
                        Picker("Appearance", selection: colorSchemePreferenceBinding) {
                            ForEach(ColorSchemePreference.allCases) { pref in
                                Text(pref.label).tag(pref)
                            }
                        }
                        .pickerStyle(.segmented)
                    }
                }

                Divider()
                    .overlay(Design.Colors.divider)

                // Chat wallpaper entry point
                NavigationLink {
                    WallpaperPickerSheet()
                } label: {
                    HStack(spacing: Design.Spacing.sm) {
                        Image(systemName: "photo.fill")
                            .font(.system(size: 14))
                            .foregroundStyle(Design.Brand.accent)
                            .frame(width: 20, alignment: .center)

                        Text("Chat Wallpaper")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.foreground)

                        Spacer()

                        Text(settingsStore.settings.chatWallpaper.label)
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.secondaryForeground)

                        Image(systemName: "chevron.right")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                    .frame(minHeight: Design.Size.minTapTarget)
                }
                .buttonStyle(.plain)

                // Chat text color. The MessageBubble renderer already reads
                // `chatTextColorHex` and falls back to the theme foreground;
                // this is the write side that was missing (Build 100).
                VStack(alignment: .leading, spacing: 8) {
                    Text("Chat Text Color")
                        .brandEyebrow()
                    HStack(spacing: Design.Spacing.sm) {
                        ColorPicker(
                            "Text color",
                            selection: chatTextColorBinding,
                            supportsOpacity: false
                        )
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.foreground)

                        Spacer()

                        if settingsStore.settings.chatTextColorHex != nil {
                            Button("Reset") {
                                settingsStore.settings.chatTextColorHex = nil
                            }
                            .font(Design.Typography.caption2)
                            .foregroundStyle(Design.Brand.accent)
                        }
                    }
                }
                .accessibilityIdentifier("settings.appearance.chatTextColor")

                Divider()
                    .overlay(Design.Colors.divider)

                // Build 118: app display name — shows in the sessions sidebar
                // header. Defaults to "Kallisti"; users can rename it to match
                // their agent.
                // Build 123: drop the black roundedBorder bar; style like the
                // relay URL field (background capsule + hairline border).
                VStack(alignment: .leading, spacing: 8) {
                    Text("App Name")
                        .brandEyebrow()
                    TextField("Kallisti", text: appDisplayNameBinding)
                        .font(Design.Typography.body)
                        .foregroundStyle(Design.Colors.foreground)
                        .autocorrectionDisabled()
                        .padding(.horizontal, Design.Spacing.md)
                        .padding(.vertical, 10)
                        .background(Design.Colors.background)
                        .overlay(
                            RoundedRectangle(cornerRadius: Design.CornerRadius.lg)
                                .stroke(Design.Colors.border, lineWidth: 1)
                        )
                        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.lg))
                        .accessibilityIdentifier("settings.appearance.appDisplayName")
                }

                Divider()
                    .overlay(Design.Colors.divider)

                // Build 118: alternate app icon picker. Default coin plus the
                // iOS 26 glass UI coin. Uses the asset-catalog alternate-icon
                // API (ASSETCATALOG_COMPILER_ALTERNATE_APPICON_NAMES), so
                // setAlternateIconName swaps the home-screen icon instantly.
                // Build 123: bigger previews fill the row; no dead space
                // beside the icons.
                VStack(alignment: .leading, spacing: 8) {
                    Text("App Icon")
                        .brandEyebrow()
                    HStack(spacing: 24) {
                        appIconButton(name: nil, label: "Default", preview: "AppIconImage")
                        appIconButton(name: "AppIcon-GlassCoin", label: "Glass", preview: "AppIconGlassCoin")
                        Spacer(minLength: 0)
                    }
                }
                .accessibilityIdentifier("settings.appearance.appIcon")
            }
        }
    }

    // MARK: - App Icon (Build 118)

    private var currentAppIconName: String? {
        UIApplication.shared.alternateIconName
    }

    private func appIconButton(name: String?, label: String, preview: String) -> some View {
        let isSelected = currentAppIconName == name
        return Button {
            setAppIcon(name)
        } label: {
            VStack(spacing: 6) {
                Image(preview)
                    .resizable()
                    .frame(width: 72, height: 72)
                    .clipShape(RoundedRectangle(cornerRadius: 16))
                .overlay(
                    RoundedRectangle(cornerRadius: 16)
                        .strokeBorder(
                            isSelected ? Design.Brand.accent : Design.Colors.border,
                            lineWidth: isSelected ? 2.5 : 1
                        )
                )
                Text(label)
                    .font(Design.Typography.caption2)
                    .foregroundStyle(
                        isSelected ? Design.Brand.accent : Design.Colors.secondaryForeground
                    )
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private func setAppIcon(_ name: String?) {
        guard UIApplication.shared.supportsAlternateIcons else { return }
        UIApplication.shared.setAlternateIconName(name) { error in
            if let error {
                NSLog("[Kallisti] setAlternateIconName failed: %@", error.localizedDescription)
            } else {
                settingsStore.settings.appIconName = name
            }
        }
    }

    /// Currently selected Herald appearance, or `nil` when a pre-2.1 preset is active.
    private var selectedAppearance: KallistiAppearance? {
        KallistiAppearance.resolve(
            preset: themeManager.preset,
            colorScheme: themeManager.colorSchemePreference
        )
    }

    private func appearanceRow(_ appearance: KallistiAppearance) -> some View {
        let isSelected = selectedAppearance == appearance
        return Button {
            withAnimation(Design.Motion.quickResponse) {
                themeManager.preset = appearance.preset
                themeManager.colorSchemePreference = appearance.colorScheme
            }
            settingsStore.settings.themePreset = appearance.preset
            settingsStore.settings.colorSchemePreference = appearance.colorScheme
        } label: {
            HStack(spacing: Design.Spacing.sm) {
                // Swatch, ringed so true black stays visible on a dark ground.
                Circle()
                    .fill(appearance.swatch)
                    .frame(width: 22, height: 22)
                    .overlay(
                        Circle().strokeBorder(Design.Colors.borderStrong, lineWidth: 1)
                    )

                VStack(alignment: .leading, spacing: 1) {
                    Text(appearance.label)
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.foreground)
                    Text(appearance.detail)
                        .font(Design.Typography.caption2)
                        .foregroundStyle(Design.Colors.tertiaryForeground)
                }

                Spacer(minLength: Design.Spacing.xs)

                if isSelected {
                    Image(systemName: "checkmark")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Design.Colors.accentHot)
                }
            }
            .frame(minHeight: Design.Size.minTapTarget)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier("settings.appearance.\(appearance.rawValue)")
        .accessibilityAddTraits(isSelected ? [.isButton, .isSelected] : [.isButton])
    }

    private func themeSwatch(_ theme: ThemePreset) -> some View {
        Button {
            withAnimation(Design.Motion.quickResponse) {
                themeManager.preset = theme
            }
            settingsStore.settings.themePreset = theme
        } label: {
            VStack(spacing: 4) {
                Circle()
                    .fill(theme.accent)
                    .frame(width: 32, height: 32)
                    .overlay(
                        Circle()
                            .strokeBorder(
                                themeManager.preset == theme ? Color.white : Color.clear,
                                lineWidth: 2
                            )
                    )
                Text(theme.label)
                    .font(.caption2)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
        }
        .buttonStyle(.plain)
    }

    private var colorSchemePreferenceBinding: Binding<ColorSchemePreference> {
        Binding(
            get: { themeManager.colorSchemePreference },
            set: { newValue in
                themeManager.colorSchemePreference = newValue
                settingsStore.settings.colorSchemePreference = newValue
            }
        )
    }

    private var chatTextColorBinding: Binding<Color> {
        Binding(
            get: {
                guard let hex = settingsStore.settings.chatTextColorHex,
                      let value = UInt(
                        hex.trimmingCharacters(in: CharacterSet(charactersIn: "# ")),
                        radix: 16
                      )
                else { return Design.Colors.foreground }
                return Color(hex: value)
            },
            set: { newColor in
                guard let hex = newColor.hexString else { return }
                settingsStore.settings.chatTextColorHex = hex
            }
        )
    }

    // MARK: - Agent Tools (Build 128.77)

    /// Cron + Skills moved into Settings and wired to their real views.
    /// Previously only reachable from the iPad sidebar; now every platform
    /// reaches them from Settings (and they actually navigate).
    private var agentToolsSection: some View {
        SettingsSectionView(title: "Agent Tools") {
            VStack(spacing: 0) {
                NavigationLink {
                    SkillsBrowserView()
                } label: {
                    HStack(spacing: Design.Spacing.sm) {
                        Image(systemName: "wrench.and.screwdriver")
                            .font(.system(size: Design.Size.iconSmall))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .frame(width: 24)
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Skills")
                                .font(Design.Typography.body)
                                .foregroundStyle(Design.Colors.foreground)
                            Text("Browse and manage agent skills")
                                .font(Design.Typography.footnote)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                        Spacer()
                        Image(systemName: "chevron.right")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                    .padding(.horizontal, Design.Spacing.lg)
                    .padding(.vertical, Design.Spacing.md)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)

                sectionDivider

                NavigationLink {
                    CronManagerView()
                } label: {
                    HStack(spacing: Design.Spacing.sm) {
                        Image(systemName: "clock.badge")
                            .font(.system(size: Design.Size.iconSmall))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .frame(width: 24)
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Cron Jobs")
                                .font(Design.Typography.body)
                                .foregroundStyle(Design.Colors.foreground)
                            Text("Scheduled automations and recurring tasks")
                                .font(Design.Typography.footnote)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                        Spacer()
                        Image(systemName: "chevron.right")
                            .font(.system(size: 12, weight: .semibold))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                    .padding(.horizontal, Design.Spacing.lg)
                    .padding(.vertical, Design.Spacing.md)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
    }

    // MARK: - Preferences

    private var preferencesSection: some View {
        SettingsSectionView(title: "Preferences") {
            VStack(spacing: 0) {
                settingsToggle(
                    icon: "bell.fill",
                    iconColor: Design.Colors.foreground,
                    title: "Notifications",
                    isOn: notificationsBinding
                )

                sectionDivider

                settingsToggle(
                    icon: "hand.tap.fill",
                    iconColor: Design.Colors.foreground,
                    title: "Haptic Feedback",
                    isOn: hapticBinding
                )

                sectionDivider

                settingsToggle(
                    icon: "return",
                    iconColor: Design.Colors.foreground,
                    title: "Enter to Send",
                    isOn: enterToSendBinding
                )

                sectionDivider

                settingsToggle(
                    icon: "steeringwheel",
                    iconColor: Design.Colors.foreground,
                    title: "Long Press to Queue",
                    subtitle: "Hold the send button to queue a message behind the current reply",
                    isOn: longPressToQueueBinding
                )

                sectionDivider

                settingsToggle(
                    icon: "sidebar.left",
                    iconColor: Design.Colors.foreground,
                    title: "Auto-Close Sidebar",
                    subtitle: "Close the sidebar after selecting a conversation",
                    isOn: autoCloseSidebarBinding
                )

                sectionDivider

                settingsToggle(
                    icon: "brain",
                    iconColor: Design.Colors.foreground,
                    title: "Show Reasoning",
                    isOn: showReasoningBinding
                )

                sectionDivider

                settingsToggle(
                    icon: "bolt.fill",
                    iconColor: Design.Colors.foreground,
                    title: "Streaming",
                    isOn: useStreamingBinding
                )

                sectionDivider

                chatDisplayModePicker

                sectionDivider

                reasoningEffortPicker
            }
        }
    }

    // MARK: - Chat Display Mode (Build 128.74)

    private var chatDisplayModePicker: some View {
        HStack {
            Label("Chat Display", systemImage: "terminal.fill")
                .font(Design.Typography.body)
                .foregroundStyle(Design.Colors.foreground)

            Spacer()

            Picker("", selection: chatDisplayModeBinding) {
                ForEach(ChatDisplayMode.allCases, id: \.self) { mode in
                    Text(mode.displayLabel).tag(mode)
                }
            }
            .pickerStyle(.menu)
            .tint(Design.Colors.secondaryForeground)
        }
        .padding(.horizontal, Design.Spacing.lg)
        .padding(.vertical, Design.Spacing.md)
    }

    private var chatDisplayModeBinding: Binding<ChatDisplayMode> {
        Binding(
            get: { settingsStore.settings.chatDisplayMode },
            set: { settingsStore.settings.chatDisplayMode = $0 }
        )
    }

    private var reasoningEffortPicker: some View {
        HStack {
            Label("Reasoning Effort", systemImage: "slider.horizontal.3")
                .font(Design.Typography.body)
                .foregroundStyle(Design.Colors.foreground)

            Spacer()

            Picker("", selection: reasoningEffortBinding) {
                ForEach(ReasoningEffort.allCases, id: \.self) { effort in
                    Text(effort.displayLabel).tag(effort)
                }
            }
            .pickerStyle(.menu)
            .tint(Design.Colors.secondaryForeground)
        }
        .padding(.horizontal, Design.Spacing.lg)
        .padding(.vertical, Design.Spacing.md)
    }

    private var reasoningEffortBinding: Binding<ReasoningEffort> {
        Binding(
            get: { settingsStore.settings.reasoningEffort },
            set: { settingsStore.settings.reasoningEffort = $0 }
        )
    }


    // MARK: - Voice (Mimo TTS)

    private var voiceSection: some View {
        SettingsSectionView(title: "Voice (Mimo TTS)") {
            VStack(spacing: 0) {
                settingsToggle(
                    icon: "speaker.wave.2.fill",
                    iconColor: Design.Brand.accent,
                    title: "Text-to-Speech",
                    isOn: ttsEnabledBinding
                )

                if settingsStore.settings.ttsEnabled {
                    sectionDivider

                    VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                        HStack(spacing: Design.Spacing.sm) {
                            Image(systemName: "key.fill")
                                .font(.system(size: 14))
                                .foregroundStyle(.orange)
                                .frame(width: 20, alignment: .center)

                            if showAPIKey {
                                TextField("Mimo API Key", text: $mimoAPIKey)
                                    .textInputAutocapitalization(.never)
                                    .autocorrectionDisabled()
                                    .font(Design.Typography.callout.monospaced())
                                    .foregroundStyle(Design.Colors.foreground)
                                    .onChange(of: mimoAPIKey) { _, newValue in
                                        Task { await mimoKeychain.store(key: "mimo.apiKey", value: newValue.trimmingCharacters(in: .whitespacesAndNewlines)) }
                                    }
                            } else {
                                SecureField("Mimo API Key", text: $mimoAPIKey)
                                    .textInputAutocapitalization(.never)
                                    .autocorrectionDisabled()
                                    .font(Design.Typography.callout.monospaced())
                                    .foregroundStyle(Design.Colors.foreground)
                                    .onChange(of: mimoAPIKey) { _, newValue in
                                        Task { await mimoKeychain.store(key: "mimo.apiKey", value: newValue.trimmingCharacters(in: .whitespacesAndNewlines)) }
                                    }
                            }

                            Button { showAPIKey.toggle() } label: {
                                Image(systemName: showAPIKey ? "eye.slash" : "eye")
                                    .font(.system(size: 14))
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                            }
                        }

                        Text("Get your key from mimo.mi.com")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                    .padding(.vertical, Design.Spacing.xs)

                    sectionDivider

                    VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                        HStack(spacing: Design.Spacing.sm) {
                            Image(systemName: "person.wave.2.fill")
                                .font(.system(size: 14))
                                .foregroundStyle(.purple)
                                .frame(width: 20, alignment: .center)

                            Text("Voice")
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.foreground)

                            Spacer()

                            Picker("Voice", selection: ttsVoiceBinding) {
                                ForEach(SpeechVoice.allCases, id: \.rawValue) { v in
                                    Text(v.rawValue).tag(v.rawValue)
                                }
                            }
                            .pickerStyle(.menu)
                            .tint(Design.Brand.accent)
                        }

                        Text("English: Mia, Chloe, Milo, Dean — Chinese: 冰糖, 茉莉, 苏打, 白桦")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                    .frame(minHeight: Design.Size.minTapTarget)

                    sectionDivider

                    // Mimo TTS model selector
                    HStack {
                        Text("Mimo model")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.foreground)
                        Spacer()
                        Picker("Mimo model", selection: mimoModelBinding) {
                            Text("Built-in voices").tag("mimo-v2.5-tts")
                            Text("Voice design").tag("mimo-v2.5-tts-voicedesign")
                            Text("Voice clone").tag("mimo-v2.5-tts-voiceclone")
                        }
                        .pickerStyle(.menu)
                        .tint(Design.Brand.accent)
                    }
                    .frame(minHeight: Design.Size.minTapTarget)

                    sectionDivider

                    TextField("Voice style (director notes)", text: mimoVoiceStyleBinding, axis: .vertical)
                        .lineLimit(1...4)
                        .textFieldStyle(.plain)

                    if settingsStore.settings.mimoTTSModel == "mimo-v2.5-tts-voicedesign"
                        && settingsStore.settings.mimoVoiceStyle.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        Text("Voice design requires a style description.")
                            .font(Design.Typography.caption)
                            .foregroundStyle(.orange)
                    }

                    sectionDivider

                    settingsToggle(
                        icon: "waveform",
                        iconColor: .blue,
                        title: "Auto-Speak in Talk",
                        isOn: ttsAutoSpeakBinding
                    )

                    sectionDivider

                    settingsToggle(
                        icon: "text.word.spacing",
                        iconColor: .green,
                        title: "Speak During Streaming",
                        isOn: ttsAutoSpeakDuringStreamingBinding
                    )

                    if settingsStore.settings.ttsAutoSpeakDuringStreaming {
                        Text("Sentences are spoken as they complete during streaming.")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .padding(.horizontal, Design.Spacing.lg)
                    }

                    sectionDivider

                    // Apple TTS Fallback Rate
                    VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                        HStack(spacing: Design.Spacing.sm) {
                            Image(systemName: "speedometer")
                                .font(.system(size: 14))
                                .foregroundStyle(.cyan)
                                .frame(width: 20, alignment: .center)

                            Text("Apple TTS Speed")
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.foreground)

                            Spacer()

                            Text("\(String(format: "%.1f", settingsStore.settings.ttsAppleRate))x")
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }

                        Slider(value: ttsAppleRateBinding, in: 0.4...2.0, step: 0.1)
                            .tint(Design.Brand.accent)
                    }
                    .frame(minHeight: Design.Size.minTapTarget)

                    sectionDivider

                    // Apple TTS Voice Picker
                    VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                        HStack(spacing: Design.Spacing.sm) {
                            Image(systemName: "person.wave.2")
                                .font(.system(size: 14))
                                .foregroundStyle(.purple)
                                .frame(width: 20, alignment: .center)

                            Text("Apple TTS Voice")
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.foreground)

                            Spacer()

                            Picker("Apple TTS Voice", selection: ttsAppleVoiceIdentifierBinding) {
                                ForEach(availableAppleVoices, id: \.identifier) { voice in
                                    Text(voice.name).tag(voice.identifier)
                                }
                            }
                            .pickerStyle(.menu)
                            .tint(Design.Brand.accent)
                        }

                        Text("Select an English voice for Apple TTS fallback")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                    .frame(minHeight: Design.Size.minTapTarget)

                    sectionDivider

                    // Test Voice Button
                    Button {
                        Task { await testAppleTTS() }
                    } label: {
                        HStack(spacing: Design.Spacing.sm) {
                            if isTestingTTS {
                                ProgressView()
                                    .controlSize(.small)
                                    .tint(Design.Brand.accent)
                            } else {
                                Image(systemName: "play.circle.fill")
                                    .font(.system(size: 14))
                                    .foregroundStyle(Design.Brand.accent)
                                    .frame(width: 20, alignment: .center)
                            }

                            Text(isTestingTTS ? "Speaking..." : "Test Voice")
                                .font(Design.Typography.callout)
                                .foregroundStyle(isTestingTTS ? Design.Colors.secondaryForeground : Design.Colors.foreground)

                            Spacer()

                            if !isTestingTTS {
                                Text("Apple TTS")
                                    .font(Design.Typography.caption)
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                            }
                        }
                        .frame(minHeight: Design.Size.minTapTarget)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .disabled(isTestingTTS)
                }
            }
        }
        .task {
            // Migrate from UserDefaults to Keychain (one-time)
            if let legacy = UserDefaults.standard.string(forKey: "mimo.apiKey"),
               await mimoKeychain.retrieve(key: "mimo.apiKey") == nil {
                await mimoKeychain.store(key: "mimo.apiKey", value: legacy)
                UserDefaults.standard.removeObject(forKey: "mimo.apiKey")
            }
            mimoAPIKey = await mimoKeychain.retrieve(key: "mimo.apiKey") ?? ""

            // Build 70: AUX model configuration is loaded on the container
            // at connection time (Infrastructure stays warm). Nothing to
            // create here - container.auxService is already wired.
        }
    }

    // MARK: - Notes Defaults

    /// Build 130.1: notes editing defaults - the "default lines" toggle and
    /// the canvas width slider. Kept separate from Notes Sync so the
    /// creation/editing experience is not buried under sync cadence.
    private var notesDefaultsSection: some View {
        SettingsSectionView(title: "Notes") {
            VStack(alignment: .leading, spacing: Design.Spacing.sm) {
                Toggle(isOn: Binding(
                    get: { settingsStore.settings.notesDefaultLinesEnabled },
                    set: { settingsStore.settings.notesDefaultLinesEnabled = $0 }
                )) {
                    HStack(spacing: Design.Spacing.sm) {
                        Image(systemName: "ruler")
                            .font(.system(size: 14))
                            .foregroundStyle(Design.Brand.primary)
                            .frame(width: 20, alignment: .center)
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Default lines")
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.foreground)
                            Text("New notes start with ruled lines.")
                                .font(Design.Typography.caption)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                    }
                    .frame(minHeight: Design.Size.minTapTarget)
                }
                .tint(Design.Brand.accent)

                sectionDivider

                // Build 130.1: note canvas width scale. Multiplies the
                // editor's available width so the writing column can be
                // narrower or wider than the screen. The trailing label
                // shows the live scale.
                VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                    HStack(spacing: Design.Spacing.sm) {
                        Image(systemName: "arrow.left.and.right")
                            .font(.system(size: 14))
                            .foregroundStyle(Design.Brand.primary)
                            .frame(width: 20, alignment: .center)
                        Text("Note width")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.foreground)
                        Spacer()
                        Text("\(Int(settingsStore.settings.notesCanvasWidthScale * 100))%")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .monospacedDigit()
                    }
                    .frame(minHeight: Design.Size.minTapTarget)

                    Slider(
                        value: Binding(
                            get: { settingsStore.settings.notesCanvasWidthScale },
                            set: { settingsStore.settings.notesCanvasWidthScale = $0 }
                        ),
                        in: 0.5...1.5,
                        step: 0.05
                    )
                    .tint(Design.Brand.accent)
                    .accessibilityLabel("Note width scale")
                }

                sectionDivider

                // Build 130.2: note paper line spacing. 0 = follow the paper
                // style default (fine 20 / medium 24 / wide 32); any other
                // value forces the actual distance between ruled lines.
                VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                    HStack(spacing: Design.Spacing.sm) {
                        Image(systemName: "lines.measurement.horizontal")
                            .font(.system(size: 14))
                            .foregroundStyle(Design.Brand.primary)
                            .frame(width: 20, alignment: .center)
                        Text("Line height")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.foreground)
                        Spacer()
                        Text(lineSpacingLabel)
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .monospacedDigit()
                    }
                    .frame(minHeight: Design.Size.minTapTarget)

                    Slider(
                        value: Binding(
                            get: { settingsStore.settings.notesLineSpacing },
                            set: { settingsStore.settings.notesLineSpacing = $0 }
                        ),
                        in: 12...48,
                        step: 2
                    )
                    .tint(Design.Brand.accent)
                    .accessibilityLabel("Note line height")
                }
            }
        }
    }

    private var lineSpacingLabel: String {
        let value = settingsStore.settings.notesLineSpacing
        if value <= 0 { return "Auto" }
        return "\(Int(value)) pt"
    }

    // MARK: - Notes Sync

    private var notesSection: some View {
        SettingsSectionView(title: "Notes Sync") {
            VStack(alignment: .leading, spacing: Design.Spacing.sm) {
                Menu {
                    ForEach(NotesSyncInterval.allCases, id: \.self) { interval in
                        Button {
                            settingsStore.settings.notesSyncInterval = interval
                        } label: {
                            if settingsStore.settings.notesSyncInterval == interval {
                                Label(interval.displayLabel, systemImage: "checkmark")
                            } else {
                                Text(interval.displayLabel)
                            }
                        }
                    }
                } label: {
                    HStack(spacing: Design.Spacing.sm) {
                        Image(systemName: "arrow.triangle.2.circlepath")
                            .font(.system(size: 14))
                            .foregroundStyle(Design.Brand.primary)
                            .frame(width: 20, alignment: .center)
                        Text("Auto-sync")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.foreground)
                        Spacer()
                        Text(settingsStore.settings.notesSyncInterval.displayLabel)
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                    .frame(minHeight: Design.Size.minTapTarget)
                    .contentShape(Rectangle())
                }

                sectionDivider

                // Build 128.94: AI enrichment toggle + model picker.
                Toggle(isOn: Binding(
                    get: { settingsStore.settings.notesEnrichmentEnabled },
                    set: { settingsStore.settings.notesEnrichmentEnabled = $0 }
                )) {
                    HStack(spacing: Design.Spacing.sm) {
                        Image(systemName: "sparkles")
                            .font(.system(size: 14))
                            .foregroundStyle(Design.Brand.primary)
                            .frame(width: 20, alignment: .center)
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Enrich with AI")
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.foreground)
                            Text("Sync notes to their session so the agent can read, summarize, and enrich them.")
                                .font(Design.Typography.caption)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                    }
                    .frame(minHeight: Design.Size.minTapTarget)
                }
                .tint(Design.Brand.accent)

                if settingsStore.settings.notesEnrichmentEnabled {
                    // Build 128.95: plain row -> sheet picker. The old Menu was
                    // live-bound to modelStore.modelsByProvider; any parent
                    // re-render (lastSyncDate tick, model refresh) rebuilt the
                    // UIMenu and reset its scroll to the top mid-interaction.
                    Button {
                        isEnrichmentPickerPresented = true
                    } label: {
                        HStack(spacing: Design.Spacing.sm) {
                            Image(systemName: "cpu")
                                .font(.system(size: 14))
                                .foregroundStyle(Design.Brand.primary)
                                .frame(width: 20, alignment: .center)
                            Text("Enrichment model")
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.foreground)
                            Spacer()
                            Text(enrichmentModelLabel)
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                            Image(systemName: "chevron.right")
                                .font(.system(size: 12, weight: .semibold))
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                        .frame(minHeight: Design.Size.minTapTarget)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .task {
                        if modelStore.models.isEmpty {
                            await modelStore.loadModels()
                        }
                    }
                }

                sectionDivider

                HStack(spacing: Design.Spacing.sm) {
                    Image(systemName: "doc.text.fill")
                        .font(.system(size: 14))
                        .foregroundStyle(Design.Brand.primary)
                        .frame(width: 20, alignment: .center)
                    Text("Last sync")
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.foreground)
                    Spacer()
                    Text(lastSyncLabel)
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
                .frame(minHeight: Design.Size.minTapTarget)

                Text("Each note syncs to its own chat session titled with the note name. Every edit appends a new message to that session.")
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
        }
    }

    private var enrichmentModelLabel: String {
        guard let name = settingsStore.settings.notesEnrichmentModelName, !name.isEmpty else {
            return "Default"
        }
        if let provider = settingsStore.settings.notesEnrichmentProvider, !provider.isEmpty {
            return "\(provider)/\(name)"
        }
        return name
    }

    private var lastSyncLabel: String {
        guard let date = container.notesSyncEngine.lastSyncDate else { return "Never" }
        return date.formatted(.relative(presentation: .named))
    }

    // MARK: - Location

    private var locationSection: some View {
        SettingsSectionView(title: "Location") {
            VStack(alignment: .leading, spacing: Design.Spacing.sm) {
                settingsRow(
                    icon: "location.fill",
                    iconColor: Design.Brand.primary,
                    title: "Authorization",
                    value: permissionsStore.locationAuthorizationLevel.displayLabel
                )

                sectionDivider

                settingsRow(
                    icon: "scope",
                    iconColor: Design.Brand.primary,
                    title: "Accuracy",
                    value: permissionsStore.locationAccuracyLevel.displayLabel
                )

                sectionDivider

                settingsToggle(
                    icon: "location.circle.fill",
                    iconColor: Design.Brand.primary,
                    title: "Background Location",
                    isOn: backgroundLocationBinding
                )

                Text(backgroundLocationDescription)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
        }
    }

    // MARK: - Privacy

    private var privacySection: some View {
        SettingsSectionView(title: "Privacy") {
            NavigationLink {
                PermissionsScreen()
            } label: {
                HStack(spacing: Design.Spacing.sm) {
                    Image(systemName: "lock.shield.fill")
                        .font(.system(size: 14))
                        .foregroundStyle(Design.Colors.success)
                        .frame(width: 20, alignment: .center)
                    Text("Permissions")
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.foreground)
                    Spacer()
                    Image(systemName: "chevron.right")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
                .frame(minHeight: Design.Size.minTapTarget)
            }
        }
    }

    // MARK: - About

    private var aboutSection: some View {
        SettingsSectionView(title: "About") {
            VStack(spacing: 0) {
                settingsRow(
                    icon: "info.circle",
                    iconColor: Design.Colors.secondaryForeground,
                    title: "Version",
                    value: "\(Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "1.0") (\(Bundle.main.object(forInfoDictionaryKey: kCFBundleVersionKey as String) as? String ?? "1"))"
                )

                sectionDivider

                settingsNavRow(
                    icon: "doc.text",
                    iconColor: Design.Colors.secondaryForeground,
                    title: "Terms of Service"
                ) {
                    openConfiguredURL(settingsStore.buildConfiguration.termsOfServiceURL)
                }

                sectionDivider

                settingsNavRow(
                    icon: "hand.raised",
                    iconColor: Design.Colors.secondaryForeground,
                    title: "Privacy Policy"
                ) {
                    openConfiguredURL(settingsStore.buildConfiguration.privacyPolicyURL)
                }

                if settingsStore.buildConfiguration.supportURL != nil {
                    sectionDivider

                    settingsNavRow(
                        icon: "questionmark.circle",
                        iconColor: Design.Colors.secondaryForeground,
                        title: "Support"
                    ) {
                        openConfiguredURL(settingsStore.buildConfiguration.supportURL)
                    }
                }
            }
        }
    }

    // MARK: - Bindings

    private var ttsEnabledBinding: Binding<Bool> {
        Binding(get: { settingsStore.settings.ttsEnabled }, set: { settingsStore.settings.ttsEnabled = $0 })
    }
    private var ttsVoiceBinding: Binding<String> {
        Binding(get: { settingsStore.settings.ttsVoice }, set: { settingsStore.settings.ttsVoice = $0 })
    }
    private var ttsAutoSpeakBinding: Binding<Bool> {
        Binding(get: { settingsStore.settings.ttsAutoSpeak }, set: { settingsStore.settings.ttsAutoSpeak = $0 })
    }
    private var ttsAutoSpeakDuringStreamingBinding: Binding<Bool> {
        Binding(get: { settingsStore.settings.ttsAutoSpeakDuringStreaming }, set: { settingsStore.settings.ttsAutoSpeakDuringStreaming = $0 })
    }
    private var ttsAppleRateBinding: Binding<Float> {
        Binding(get: { settingsStore.settings.ttsAppleRate }, set: { settingsStore.settings.ttsAppleRate = $0 })
    }
    private var ttsAppleVoiceIdentifierBinding: Binding<String> {
        Binding(get: { settingsStore.settings.ttsAppleVoiceIdentifier }, set: { settingsStore.settings.ttsAppleVoiceIdentifier = $0 })
    }
    private var mimoModelBinding: Binding<String> {
        Binding(get: { settingsStore.settings.mimoTTSModel }, set: { settingsStore.settings.mimoTTSModel = $0 })
    }
    private var mimoVoiceStyleBinding: Binding<String> {
        Binding(get: { settingsStore.settings.mimoVoiceStyle }, set: { settingsStore.settings.mimoVoiceStyle = $0 })
    }

    // Build 118: app display name shown in the sessions sidebar header.
    private var appDisplayNameBinding: Binding<String> {
        Binding(
            get: { settingsStore.settings.appDisplayName },
            set: { settingsStore.settings.appDisplayName = $0 }
        )
    }
    private var availableAppleVoices: [AVSpeechSynthesisVoice] {
        AVSpeechSynthesisVoice.speechVoices()
            .filter { $0.language.hasPrefix("en") }
            .sorted { ($0.quality.rawValue, $0.name) > ($1.quality.rawValue, $1.name) }
    }

    private var autoConnectBinding: Binding<Bool> {
        Binding(
            get: { settingsStore.settings.autoConnectOnLaunch },
            set: { settingsStore.settings.autoConnectOnLaunch = $0 }
        )
    }

    private var notificationsBinding: Binding<Bool> {
        Binding(
            get: { settingsStore.settings.notificationsEnabled },
            set: { newValue in
                settingsStore.settings.notificationsEnabled = newValue
                // Immediately register or deactivate push token on the relay
                Task {
                    await AppContainer.sharedDefault().reregisterStoredPushToken()
                }
            }
        )
    }

    private var hapticBinding: Binding<Bool> {
        Binding(
            get: { settingsStore.settings.hapticFeedbackEnabled },
            set: { settingsStore.settings.hapticFeedbackEnabled = $0 }
        )
    }

    private var enterToSendBinding: Binding<Bool> {
        Binding(
            get: { settingsStore.settings.enterToSend },
            set: { settingsStore.settings.enterToSend = $0 }
        )
    }

    private var longPressToQueueBinding: Binding<Bool> {
        Binding(
            get: { settingsStore.settings.longPressToQueue },
            set: { settingsStore.settings.longPressToQueue = $0 }
        )
    }

    private var autoCloseSidebarBinding: Binding<Bool> {
        Binding(
            get: { settingsStore.settings.autoCloseSidebarOnSelection },
            set: { settingsStore.settings.autoCloseSidebarOnSelection = $0 }
        )
    }

    private var showReasoningBinding: Binding<Bool> {
        Binding(
            get: { settingsStore.settings.showReasoning },
            set: { settingsStore.settings.showReasoning = $0 }
        )
    }
    private var useStreamingBinding: Binding<Bool> {
        Binding(
            get: { settingsStore.settings.useStreaming },
            set: {
                settingsStore.settings.useStreaming = $0
                chatStore.useStreaming = $0
            }
        )
    }

    private var backgroundLocationBinding: Binding<Bool> {
        Binding(
            get: { settingsStore.settings.locationSyncPreference == .backgroundAllowed },
            set: { isEnabled in
                let preference: LocationSyncPreference = isEnabled ? .backgroundAllowed : .foregroundOnly
                settingsStore.settings.locationSyncPreference = preference
                permissionsStore.updateLocationSyncPreference(preference)

                guard isEnabled else { return }

                Task {
                    switch permissionsStore.locationAuthorizationLevel {
                    case .denied, .restricted:
                        permissionsStore.openLocationSystemSettings()
                    case .always, .whenInUse:
                        // Both levels support CLBackgroundActivitySession.
                        // While In Use shows blue indicator; Always does not.
                        await permissionsStore.requestBackgroundLocationAccess()
                    case .notDetermined:
                        await permissionsStore.requestBackgroundLocationAccess()
                    }
                }
            }
        )
    }

    private var relayConfiguration: RelayConfiguration {
        settingsStore.settings.relayConfiguration
    }

    private var relayValidationMessage: String? {
        relayConfiguration.validationMessage
    }

    private var customRelayURLPlaceholder: String {
        switch relayConfiguration.connectionMode {
        case .tailscale:
            return "https://my-mac.tail-scale.ts.net/v1"
        case .selfHostedRelay:
            return "https://your-relay.example.com/v1"
        }
    }

    private var backgroundDeliveryNote: String {
        relayConfiguration.connectionMode.backgroundDeliveryNote
    }

    private var backgroundLocationDescription: String {
        if settingsStore.settings.locationSyncPreference == .backgroundAllowed {
            switch permissionsStore.locationAuthorizationLevel {
            case .always:
                return "Kallisti receives location updates in the background without the blue indicator."
            case .whenInUse:
                return "Kallisti receives background location updates. A blue indicator appears at the top of the screen when active."
            case .notDetermined:
                return "Enabling this will request location access so Kallisti can sync while backgrounded."
            case .denied, .restricted:
                return "Location is blocked at the system level. Open Settings to allow Kallisti to request background updates."
            }
        }

        return "Foreground-only keeps location updates limited to active app use."
    }

    private var connectionModeBinding: Binding<RelayConnectionMode> {
        Binding(
            get: { settingsStore.settings.relayConfiguration.connectionMode },
            set: { newValue in
                var relayConfiguration = settingsStore.settings.relayConfiguration
                relayConfiguration.updateConnectionMode(newValue)
                settingsStore.settings.relayConfiguration = relayConfiguration
            }
        )
    }

    private var customRelayURLBinding: Binding<String> {
        Binding(
            get: { settingsStore.settings.relayConfiguration.customRelayBaseURL },
            set: { newValue in
                var relayConfiguration = settingsStore.settings.relayConfiguration
                relayConfiguration.customRelayBaseURL = newValue.trimmingCharacters(in: .whitespacesAndNewlines)
                settingsStore.settings.relayConfiguration = relayConfiguration
            }
        )
    }

    // MARK: - Row Components

    private var sectionDivider: some View {
        Divider()
            .overlay(Design.Colors.divider)
    }

    private func settingsRow(icon: String, iconColor: Color, title: String, value: String?, valueColor: Color? = nil) -> some View {
        HStack(spacing: Design.Spacing.sm) {
            Image(systemName: icon)
                .font(.system(size: 14))
                .foregroundStyle(iconColor)
                .frame(width: 20, alignment: .center)

            Text(title)
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.foreground)

            Spacer()

            if let value {
                Text(value)
                    .font(Design.Typography.callout)
                    .foregroundStyle(valueColor ?? Design.Colors.secondaryForeground)
            }
        }
        .frame(minHeight: Design.Size.minTapTarget)
    }

    @ViewBuilder
    private func settingsNavRow(
        icon: String,
        iconColor: Color,
        title: String,
        value: String? = nil,
        accessibilityIdentifier: String? = nil,
        action: @escaping () -> Void
    ) -> some View {
        let row = Button(action: action) {
            HStack(spacing: Design.Spacing.sm) {
                Image(systemName: icon)
                    .font(.system(size: 14))
                    .foregroundStyle(iconColor)
                    .frame(width: 20, alignment: .center)

                Text(title)
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.foreground)

                Spacer()

                if let value {
                    Text(value)
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }

                Image(systemName: "chevron.right")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Design.Colors.secondaryForeground)
            }
            .frame(minHeight: Design.Size.minTapTarget)
        }

        if let accessibilityIdentifier {
            row.accessibilityIdentifier(accessibilityIdentifier)
        } else {
            row
        }
    }

    private func settingsToggle(
        icon: String,
        iconColor: Color,
        title: String,
        subtitle: String? = nil,
        isOn: Binding<Bool>
    ) -> some View {
        // Build 118: full-row tap target. A stock Toggle only responds on the
        // switch itself; wrapping the row in a Button makes the whole row
        // tappable (Curtis's "touch the whole row" ask) while keeping a
        // switch-style indicator.
        Button {
            isOn.wrappedValue.toggle()
        } label: {
            HStack(spacing: Design.Spacing.sm) {
                Image(systemName: icon)
                    .font(.system(size: 14))
                    .foregroundStyle(iconColor)
                    .frame(width: 20, alignment: .center)

                VStack(alignment: .leading, spacing: 1) {
                    Text(title)
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.foreground)
                    if let subtitle {
                        Text(subtitle)
                            .font(Design.Typography.caption2)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                }

                Spacer(minLength: Design.Spacing.sm)

                // Switch-style indicator (native Switch look without the
                // native hit-testing limitation).
                ZStack(alignment: isOn.wrappedValue ? .trailing : .leading) {
                    Capsule()
                        .fill(
                            isOn.wrappedValue
                                ? Design.Brand.accent
                                : Design.Colors.border
                        )
                        .frame(width: 46, height: 28)
                    Circle()
                        .fill(.white)
                        .frame(width: 24, height: 24)
                        .padding(2)
                }
                .animation(Design.Motion.quickResponse, value: isOn.wrappedValue)
            }
            .frame(minHeight: Design.Size.minTapTarget)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    private func openConfiguredURL(_ url: URL?) {
        guard let url else {
            // Fallback: the key was missing from Info.plist.
            if let fallback = URL(string: "https://herald.example.com") {
                openURL(fallback)
            }
            return
        }
        safariURL = url
        showSafari = true
    }

    // MARK: - Timeout Helper

    /// Run `operation` with a hard deadline. A true race: whichever task
    /// finishes first wins, the loser is cancelled, and a timeout throws
    /// `TimeoutError` instead of hanging on `bodyTask.value` forever (the old
    /// helper awaited the body unconditionally, so its timeout never fired
    /// while a relay RPC hung).
    @MainActor
    private func withTimeout<T: Sendable>(
        seconds: Double,
        operation: @escaping @MainActor () async throws -> T
    ) async throws -> T {
        try await withThrowingTaskGroup(of: T.self) { group in
            group.addTask { try await operation() }
            group.addTask {
                try await Task.sleep(for: .seconds(seconds))
                throw TimeoutError()
            }
            guard let result = try await group.next() else {
                group.cancelAll()
                throw TimeoutError()
            }
            group.cancelAll()
            return result
        }
    }

    private func testAppleTTS() async {
        isTestingTTS = true
        defer { isTestingTTS = false }

        let appleTTS = AppleTTSService()
        appleTTS.setRate(settingsStore.settings.ttsAppleRate)
        appleTTS.setVoice(identifier: settingsStore.settings.ttsAppleVoiceIdentifier)

        do {
            try await appleTTS.speak(
                "Hello, this is a test of the Apple text to speech voice.",
                voice: settingsStore.settings.ttsVoice,
                context: nil as String?
            )
        } catch {
            // Test failed — user can see the button is no longer speaking
        }
    }
}


// MARK: - Build 69 (r7) · Update Changelog Window

/// Modal changelog window shown from the Software Update row. Lists what
/// changed in the pending Hermes Agent update and offers Update Now / Skip.
private struct UpdateChangelogSheet: View {
    let info: NativeKallistiClient.HermesUpdateInfo
    @Binding var isUpdating: Bool
    // Build 128.49: live update output streamed from the connector.
    let progressLines: [String]
    let progressState: String?
    let onUpdateNow: () -> Void
    let onSkip: () -> Void

    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ZStack {
                Design.Colors.background
                    .ignoresSafeArea()

                ScrollView {
                    VStack(alignment: .leading, spacing: Design.Spacing.lg) {
                        // Version header
                        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                            Text("Update Available")
                                .font(Design.Typography.sectionTitle)
                                .foregroundStyle(Design.Colors.foreground)
                            if let current = info.currentVersion,
                               let latest = info.latestVersion {
                                Text("\(current) → \(latest)")
                                    .font(Design.Typography.callout)
                                    .foregroundStyle(Design.Brand.accent)
                            } else if let latest = info.latestVersion {
                                Text("Latest: \(latest)")
                                    .font(Design.Typography.callout)
                                    .foregroundStyle(Design.Brand.accent)
                            }
                            if let behind = info.behindCount, behind > 0 {
                                Text("\(behind) commit\(behind == 1 ? "" : "s") behind")
                                    .font(Design.Typography.caption)
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                            }
                        }

                        sectionDivider

                        // Changelog body. Build 128.49: prefers the structured
                        // commits (grouped Added / Fixed / Other); falls back
                        // to the legacy plain-text changelog.
                        Text("What's new")
                            .font(Design.Typography.callout.weight(.semibold))
                            .foregroundStyle(Design.Colors.foreground)
                        if let commits = info.commits, !commits.isEmpty {
                            changelogView(commits)
                        } else if let changelog = info.changelog, !changelog.isEmpty {
                            Text(changelog)
                                .font(Design.Typography.body)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                                .textSelection(.enabled)
                        } else {
                            Text("No changelog available.")
                                .font(Design.Typography.body)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }

                        // Build 128.49: live update progress while running.
                        if isUpdating || !progressLines.isEmpty {
                            sectionDivider
                            VStack(alignment: .leading, spacing: Design.Spacing.sm) {
                                HStack(spacing: Design.Spacing.sm) {
                                    if progressState == "running" || progressState == nil {
                                        ProgressView()
                                            .controlSize(.small)
                                    }
                                    Text(progressTitle)
                                        .font(Design.Typography.callout.weight(.semibold))
                                        .foregroundStyle(Design.Colors.foreground)
                                    Spacer()
                                }
                                ScrollView {
                                    VStack(alignment: .leading, spacing: 3) {
                                        ForEach(Array(progressLines.enumerated()), id: \.offset) { _, line in
                                            Text(line)
                                                .font(.system(.caption2, design: .monospaced))
                                                .foregroundStyle(Design.Colors.secondaryForeground)
                                                .textSelection(.enabled)
                                                .frame(maxWidth: .infinity, alignment: .leading)
                                        }
                                    }
                                    .padding(Design.Spacing.sm)
                                }
                                .frame(maxHeight: 220)
                                .background(Design.Colors.background)
                                .overlay(
                                    RoundedRectangle(cornerRadius: Design.CornerRadius.md)
                                        .stroke(Design.Colors.border, lineWidth: 1)
                                )
                                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
                            }
                        }

                        if let urlString = info.releaseURL,
                           let url = URL(string: urlString) {
                            sectionDivider
                            Link(destination: url) {
                                HStack {
                                    Text("View on GitHub")
                                        .font(Design.Typography.callout)
                                        .foregroundStyle(Design.Brand.accent)
                                    Spacer()
                                    Image(systemName: "arrow.up.right")
                                        .font(.system(size: 12, weight: .semibold))
                                        .foregroundStyle(Design.Brand.accent)
                                }
                                .frame(minHeight: Design.Size.minTapTarget)
                            }
                        }

                        sectionDivider

                        // Actions
                        VStack(spacing: Design.Spacing.sm) {
                            Button {
                                onUpdateNow()
                            } label: {
                                HStack {
                                    Spacer()
                                    if isUpdating {
                                        ProgressView()
                                            .controlSize(.small)
                                            .tint(.white)
                                    } else {
                                        Text(updateButtonTitle)
                                            .font(Design.Typography.callout.weight(.semibold))
                                    }
                                    Spacer()
                                }
                                .padding(.vertical, Design.Spacing.sm)
                            }
                            .buttonStyle(.borderedProminent)
                            .tint(progressState == "failed" ? Design.Colors.danger : Design.Brand.accent)
                            .disabled(isUpdating)

                            Button {
                                onSkip()
                            } label: {
                                Text("Skip This Version")
                                    .font(Design.Typography.callout)
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                                    .frame(maxWidth: .infinity)
                                    .padding(.vertical, Design.Spacing.sm)
                            }
                            .buttonStyle(.plain)
                            .disabled(isUpdating)
                        }
                    }
                    .padding(Design.Spacing.lg)
                }
            }
            .navigationTitle("Software Update")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                        .foregroundStyle(Design.Brand.accent)
                }
            }
        }
    }

    private var progressTitle: String {
        switch progressState {
        case "running": return "Updating…"
        case "done": return "Update complete"
        case "failed": return "Update failed"
        default: return "Updating…"
        }
    }

    private var updateButtonTitle: String {
        switch progressState {
        case "done": return "Update Complete"
        case "failed": return "Retry Update"
        default: return "Update Now"
        }
    }

    /// Build 128.49: group structured commits by conventional-commit type
    /// so the user sees "what was added" and "what was fixed" at a glance.
    private func changelogView(_ commits: [NativeKallistiClient.UpdateCommit]) -> some View {
        let added = commits.filter { ($0.summary ?? "").hasPrefix("feat") }
        let fixed = commits.filter { ($0.summary ?? "").hasPrefix("fix") }
        let other = commits.filter { c in
            let s = c.summary ?? ""
            return !s.hasPrefix("feat") && !s.hasPrefix("fix")
        }
        return VStack(alignment: .leading, spacing: Design.Spacing.md) {
            if !added.isEmpty {
                groupSection("Added", commits: added)
            }
            if !fixed.isEmpty {
                groupSection("Fixed", commits: fixed)
            }
            if !other.isEmpty {
                groupSection("Other", commits: other)
            }
        }
    }

    private func groupSection(_ title: String, commits: [NativeKallistiClient.UpdateCommit]) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            Text(title)
                .font(Design.Typography.caption.weight(.semibold))
                .foregroundStyle(Design.Brand.accent)
            ForEach(Array(commits.enumerated()), id: \.offset) { _, commit in
                HStack(alignment: .top, spacing: Design.Spacing.sm) {
                    if let sha = commit.sha, !sha.isEmpty {
                        Text(sha)
                            .font(.system(.caption2, design: .monospaced))
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .frame(width: 54, alignment: .leading)
                    }
                    Text(commit.summary ?? "")
                        .font(Design.Typography.body)
                        .foregroundStyle(Design.Colors.foreground)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
    }

    private var sectionDivider: some View {
        Rectangle()
            .fill(Design.Colors.divider)
            .frame(height: 0.5)
    }
}


// Build 70: searchable model picker for auxiliary model assignment.
private struct AuxModelPickerSheet: View {
    let task: AuxTask
    let models: [ModelStore.HeraldModel]
    @Binding var searchText: String
    let onSelect: (String, String) -> Void

    @Environment(\.dismiss) private var dismiss

    private var filteredModels: [ModelStore.HeraldModel] {
        let q = searchText.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !q.isEmpty else { return models }
        return models.filter {
            $0.name.lowercased().contains(q)
                || $0.provider.lowercased().contains(q)
                || $0.displayProviderName.lowercased().contains(q)
        }
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Button {
                        onSelect("auto", "auto")
                    } label: {
                        HStack {
                            Text("Auto (server default)")
                                .foregroundStyle(Design.Colors.foreground)
                            Spacer()
                            if task.isAuto {
                                Image(systemName: "checkmark")
                                    .font(.system(size: 12, weight: .semibold))
                                    .foregroundStyle(Design.Brand.accent)
                            }
                        }
                    }
                }

                Section("Models") {
                    if filteredModels.isEmpty {
                        Text("No models match \"\(searchText)\"")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    } else {
                        ForEach(filteredModels) { m in
                            Button {
                                onSelect(m.provider, m.name)
                            } label: {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(m.name)
                                        .font(Design.Typography.callout)
                                        .foregroundStyle(Design.Colors.foreground)
                                    Text(m.displayProviderName)
                                        .font(Design.Typography.caption)
                                        .foregroundStyle(Design.Colors.secondaryForeground)
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                            }
                        }
                    }
                }
            }
            .navigationTitle("Model for \(task.task)")
            .navigationBarTitleDisplayMode(.inline)
            .searchable(text: $searchText, placement: .navigationBarDrawer(displayMode: .always), prompt: "Filter models")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Cancel") { dismiss() }
                        .foregroundStyle(Design.Brand.accent)
                }
            }
        }
        // Build 128.96: scrolls take precedence over sheet resize. Without
        // this, dragging inside the model list can resize the sheet
        // (medium<->large) instead of scrolling - the touch-scroll vs
        // touch-select fight. Content interaction .scrolls hands drags to
        // the List; the grabber still resizes/dismisses.
        .presentationDetents([.medium, .large])
        .presentationContentInteraction(.scrolls)
    }
}

// Build 128.95: searchable enrichment model picker. Sheet-based (stable List,
// scroll position survives state changes) - the previous Menu rebuilt its
// UIMenu on every parent re-render and snapped back to the top.
private struct EnrichmentModelPickerSheet: View {
    let models: [ModelStore.HeraldModel]
    @Binding var searchText: String
    let selectedModelName: String?
    let selectedProvider: String?
    let onSelect: (String, String) -> Void
    let onSelectDefault: () -> Void

    @Environment(\.dismiss) private var dismiss
    @Environment(SettingsStore.self) private var settingsStore

    private func fireSelectionHaptic() {
        // Build 128.96: tactile confirmation when a model is picked.
        // Gated by the user's haptic preference like chat sends.
        guard settingsStore.settings.hapticFeedbackEnabled else { return }
        HapticEngine.messageSent()
    }

    private var filteredModels: [ModelStore.HeraldModel] {
        let q = searchText.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !q.isEmpty else { return models }
        return models.filter {
            $0.name.lowercased().contains(q)
                || $0.provider.lowercased().contains(q)
                || $0.displayProviderName.lowercased().contains(q)
        }
    }

    private var isDefaultSelected: Bool {
        selectedModelName == nil || selectedModelName?.isEmpty == true
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Button {
                        fireSelectionHaptic()
                        onSelectDefault()
                    } label: {
                        HStack {
                            Text("Default (chat model)")
                                .foregroundStyle(Design.Colors.foreground)
                            Spacer()
                            if isDefaultSelected {
                                Image(systemName: "checkmark")
                                    .font(.system(size: 12, weight: .semibold))
                                    .foregroundStyle(Design.Brand.accent)
                            }
                        }
                    }
                }

                Section("Models") {
                    if filteredModels.isEmpty {
                        Text("No models match \"\(searchText)\"")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    } else {
                        ForEach(filteredModels) { m in
                            Button {
                                fireSelectionHaptic()
                                onSelect(m.provider, m.name)
                            } label: {
                                HStack {
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(m.name)
                                            .font(Design.Typography.callout)
                                            .foregroundStyle(Design.Colors.foreground)
                                        Text(m.displayProviderName)
                                            .font(Design.Typography.caption)
                                            .foregroundStyle(Design.Colors.secondaryForeground)
                                    }
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    if selectedModelName == m.name
                                        && selectedProvider == m.provider {
                                        Image(systemName: "checkmark")
                                            .font(.system(size: 12, weight: .semibold))
                                            .foregroundStyle(Design.Brand.accent)
                                    }
                                }
                            }
                        }
                    }
                }
            }
            .navigationTitle("Enrichment model")
            .navigationBarTitleDisplayMode(.inline)
            .searchable(text: $searchText, placement: .navigationBarDrawer(displayMode: .always), prompt: "Filter models")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Cancel") { dismiss() }
                        .foregroundStyle(Design.Brand.accent)
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationContentInteraction(.scrolls)
    }
}

