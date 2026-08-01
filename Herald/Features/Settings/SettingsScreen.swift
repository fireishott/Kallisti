import AVFoundation
import SwiftUI

struct SettingsScreen: View {
    @Environment(\.dismiss) private var dismiss
    @Environment(\.openURL) private var openURL
    @Environment(AppSessionStore.self) private var sessionStore
    @Environment(ChatStore.self) private var chatStore
    @Environment(ModelStore.self) private var modelStore
    @Environment(HeraldHostStore.self) private var hostStore
    @Environment(PairingStore.self) private var pairingStore
    @Environment(PermissionsStore.self) private var permissionsStore
    @Environment(SettingsStore.self) private var settingsStore
    @Environment(TabRouter.self) private var router
    @State private var mimoAPIKey: String = ""
    @State private var auxService: AuxModelService?
    @State private var showAPIKey: Bool = false
    @State private var isTestingTTS: Bool = false
    @State private var safariURL: URL?
    @State private var showSafari = false
    private let mimoKeychain = KeychainSecureStore(serviceName: "net.fihonline.herald.session")
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
                        voiceSection
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
        }
        .sheet(isPresented: $showSafari) {
            if let url = safariURL {
                SafariView(url: url)
            }
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
        if relayStatus == .error {
            return relayStatus
        }
        // If the host is confirmed online, don't show "connecting" — the WS
        // handshake may just be in progress. The gateway status page already
        // shows the definitive state.
        if relayStatus == .connecting && hostStore.isHostOnline {
            return .connected
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
            }
        }
    }

    @State private var isRestartingGW = false
    @State private var gwRestartTarget: String?
    @State private var gwRestartResult: String?
    @State private var updateCheckResult: String?
    @State private var updateAgentResult: String?
    @State private var isCheckingForUpdate = false
    @State private var isUpdatingAgent = false

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
                    Text("Disconnect Herald before changing the relay configuration.")
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

                // Check for updates
                gatewayActionButton(
                    label: "Check for Updates",
                    icon: "arrow.triangle.2.circlepath",
                    isLoading: isCheckingForUpdate,
                    result: updateCheckResult,
                    action: { await checkForUpdates() }
                )

                // Update agent
                gatewayActionButton(
                    label: "Update Agent",
                    icon: "arrow.down.to.line",
                    isLoading: isUpdatingAgent,
                    result: updateAgentResult,
                    action: { await updateAgent() }
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
        let relayBase = settingsStore.settings.relayConfiguration.activeBaseURLString
            ?? pairingStore.pairedRelayConfiguration?.baseURLString
        guard let relayBase else { return }
        let token = await sessionStore.currentAccessToken()
        let client = RelayAPIClient { relayBase }

        isCheckingForUpdate = true
        updateCheckResult = nil

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

        isCheckingForUpdate = false
        // Auto-clear result after 8 seconds
        try? await Task.sleep(for: .seconds(8))
        updateCheckResult = nil
    }

    private func updateAgent() async {
        let relayBase = settingsStore.settings.relayConfiguration.activeBaseURLString
            ?? pairingStore.pairedRelayConfiguration?.baseURLString
        guard let relayBase else { return }
        let token = await sessionStore.currentAccessToken()
        let client = RelayAPIClient { relayBase }

        isUpdatingAgent = true
        updateAgentResult = nil

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

        isUpdatingAgent = false
        try? await Task.sleep(for: .seconds(8))
        updateAgentResult = nil
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
                                    Menu {
                                        Button("Auto") {
                                            Task { await aux.set(task: task.task, provider: "auto", model: "auto") }
                                        }
                                        ForEach(modelStore.models, id: \.name) { m in
                                            Button(m.name) {
                                                Task { await aux.set(task: task.task, provider: m.provider, model: m.name) }
                                            }
                                        }
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
                // (preset + color scheme) via HeraldAppearance.
                VStack(spacing: 0) {
                    ForEach(HeraldAppearance.allCases) { appearance in
                        appearanceRow(appearance)
                        if appearance != HeraldAppearance.allCases.last {
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
                VStack(alignment: .leading, spacing: 8) {
                    Text("Other Themes")
                        .brandEyebrow()
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 12) {
                            ForEach(ThemePreset.legacyPresets) { theme in
                                themeSwatch(theme)
                            }
                        }
                    }
                }

                // The light/dark/system control only applies to the pre-2.1
                // presets — the Herald appearances above already encode it.
                if !themeManager.preset.isHeraldBrand {
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
            }
        }
    }

    /// Currently selected Herald appearance, or `nil` when a pre-2.1 preset is active.
    private var selectedAppearance: HeraldAppearance? {
        HeraldAppearance.resolve(
            preset: themeManager.preset,
            colorScheme: themeManager.colorSchemePreference
        )
    }

    private func appearanceRow(_ appearance: HeraldAppearance) -> some View {
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

                reasoningEffortPicker
            }
        }
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

            // Load AUX model configuration
            if let relayBase = settingsStore.settings.relayConfiguration.activeBaseURLString {
                let client = RelayAPIClient { relayBase }
                let svc = AuxModelService(apiClient: client) {
                    await sessionStore.currentAccessToken()
                }
                auxService = svc
                await svc.load()
            }
        }
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
                return "Herald receives location updates in the background without the blue indicator."
            case .whenInUse:
                return "Herald receives background location updates. A blue indicator appears at the top of the screen when active."
            case .notDetermined:
                return "Enabling this will request location access so Herald can sync while backgrounded."
            case .denied, .restricted:
                return "Location is blocked at the system level. Open Settings to allow Herald to request background updates."
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

    private func settingsRow(icon: String, iconColor: Color, title: String, value: String?) -> some View {
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
        isOn: Binding<Bool>
    ) -> some View {
        Toggle(isOn: isOn) {
            HStack(spacing: Design.Spacing.sm) {
                Image(systemName: icon)
                    .font(.system(size: 14))
                    .foregroundStyle(iconColor)
                    .frame(width: 20, alignment: .center)

                Text(title)
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.foreground)
            }
        }
        .tint(Design.Brand.accent)
        .frame(minHeight: Design.Size.minTapTarget)
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
