import SwiftUI

struct AppRootView: View {
    @Environment(AppContainer.self) private var container
    @State private var reconnectDebounced = false
    @State private var reconnectStartedAt: Date?

    /// Whether the single opaque loading surface should be shown.
    /// Covers: initial launch, reconnect, auth, verification, restoration,
    /// and any transient error states (suppresses "Cannot connect" and
    /// "Model unavailable" during recovery).
    ///
    /// The loading surface is NOT shown for terminal unrecoverable errors
    /// (authFailure, networkFailure) - those need visible error UI with
    /// recovery actions, not an opaque spinner.
    private var showLoadingSurface: Bool {
        let terminalLegacyFailure: Bool
        switch container.sessionStore.launchState {
        case .authFailure, .networkFailure:
            terminalLegacyFailure = true
        default:
            terminalLegacyFailure = false
        }
        let nativeClient = container.nativeGatewayClient
        let status = nativeClient?.connectionStatus
        return Self.shouldShowLoadingSurface(
            isNative: nativeClient != nil,
            isLaunchReady: container.isLaunchReady,
            isRecovering: status == .connecting || status == .reconnecting,
            hasStoredLogin: nativeClient?.hasStoredLogin ?? false,
            isBootstrapping: container.sessionStore.isBootstrapping,
            hasTerminalLegacyFailure: terminalLegacyFailure,
            reconnectDebounced: reconnectDebounced
        )
    }

    nonisolated static func shouldShowLoadingSurface(
        isNative: Bool,
        isLaunchReady: Bool,
        isRecovering: Bool,
        hasStoredLogin: Bool,
        isBootstrapping: Bool,
        hasTerminalLegacyFailure: Bool,
        reconnectDebounced: Bool = false
    ) -> Bool {
        if isNative {
            if !isLaunchReady { return true }  // always show during launch
            // Mid-session reconnect: show after 1.5s debounce to avoid
            // flashing on quick reconnects (<1.5s).
            if isRecovering && hasStoredLogin {
                return reconnectDebounced
            }
            return false
        }
        return !hasTerminalLegacyFailure && (!isLaunchReady || isBootstrapping)
    }

    /// The stage label for the loading surface.
    private var loadingStage: ConnectionStage {
        guard let nativeClient = container.nativeGatewayClient else {
            return .preparing
        }
        return nativeClient.connectionStage
    }

    private var showResetButton: Bool {
        guard let nativeClient = container.nativeGatewayClient else { return false }
        return nativeClient.hasStoredLogin
            && nativeClient.connectionStatus != .connected
    }

    var body: some View {
        ZStack {
            Design.Colors.background
                .ignoresSafeArea()

            if showLoadingSurface {
                // Single opaque loading surface: covers launch, reconnect,
                // auth, verification, restoration, and initial model/profile
                // readiness. Truthful stages from actual work -- no fake timers.
                LoadingSurface(
                    stage: loadingStage,
                    reconnectAttempt: container.nativeGatewayClient?.currentReconnectAttempt ?? 0,
                    onResetConnection: showResetButton ? {
                        Task { await container.nativeGatewayClient?.resetConnection() }
                    } : nil
                )
                .transition(.opacity)
                .zIndex(10)
            } else {
                Group {
                    if let nativeClient = container.nativeGatewayClient {
                        if nativeClient.connectionStatus == .connected || nativeClient.hasStoredLogin {
                            AdaptiveRootView()
                        } else {
                            OnboardingFlowView(initialStep: .welcome, nativeGatewayClient: container.nativeGatewayClient, installationID: container.sessionStore.state.installationID)
                        }
                    } else if !container.pairingStore.isPaired {
                        OnboardingFlowView(initialStep: .welcome, nativeGatewayClient: container.nativeGatewayClient, installationID: container.sessionStore.state.installationID)
                    } else if container.pairingStore.needsPermissionsOnboarding {
                        OnboardingFlowView(initialStep: .permissions, nativeGatewayClient: container.nativeGatewayClient, installationID: container.sessionStore.state.installationID)
                    } else {
                        switch container.sessionStore.launchState {
                        case .authFailure:
                            authFailureView
                        case .networkFailure(let message):
                            networkFailureView(message: message)
                        default:
                            AdaptiveRootView()
                        }
                    }
                }
                .transition(.opacity)
            }
        }
        .animation(Design.Motion.standard, value: container.pairingStore.isPaired)
        .animation(Design.Motion.standard, value: container.pairingStore.needsPermissionsOnboarding)
        .animation(Design.Motion.standard, value: container.nativeGatewayClient?.connectionStatus)
        .animation(Design.Motion.gentle, value: container.isLaunchReady)
        .onChange(of: container.nativeGatewayClient?.connectionStatus) { _, newStatus in
            if newStatus == .connecting || newStatus == .reconnecting {
                reconnectStartedAt = Date()
                reconnectDebounced = false
                Task {
                    try? await Task.sleep(for: .milliseconds(1500))
                    if reconnectStartedAt != nil {
                        reconnectDebounced = true
                    }
                }
            } else {
                reconnectStartedAt = nil
                reconnectDebounced = false
            }
        }
    }

    private var authFailureView: some View {
        VStack(spacing: Design.Spacing.lg) {
            Spacer()
            Image(systemName: "exclamationmark.lock.fill")
                .font(.system(size: 48))
                .foregroundStyle(Design.Colors.danger)
            Text("Authentication Failed")
                .font(Design.Typography.sectionTitle)
                .foregroundStyle(Design.Colors.foreground)
            Text("Your session has expired and could not be renewed.")
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .multilineTextAlignment(.center)
            Button("Re-pair Device") {
                Task { await container.repairFromAuthFailure() }
            }
            .buttonStyle(.borderedProminent)
            .tint(Design.Brand.accent)
            Spacer()
        }
        .padding(.horizontal, Design.Spacing.xl)
    }

    private func networkFailureView(message: String) -> some View {
        VStack(spacing: Design.Spacing.lg) {
            Spacer()
            Image(systemName: "wifi.exclamationmark")
                .font(.system(size: 48))
                .foregroundStyle(Design.Colors.warning)
            Text("Connection Failed")
                .font(Design.Typography.sectionTitle)
                .foregroundStyle(Design.Colors.foreground)
            Text(message)
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .multilineTextAlignment(.center)
            Button("Retry") {
                Task { await container.retryInitialization() }
            }
            .buttonStyle(.borderedProminent)
            .tint(Design.Brand.accent)
            Button("Re-pair Device") {
                Task { await container.repairFromAuthFailure() }
            }
            .buttonStyle(.bordered)
            Spacer()
        }
        .padding(.horizontal, Design.Spacing.xl)
    }
}

/// Single opaque loading surface covering launch, reconnect, auth,
/// verification, restoration, and initial model/profile readiness.
/// Truthful stages from actual work -- no fake timers. Suppresses
/// transient "Cannot connect" and "Model unavailable" states.
struct LoadingSurface: View {
    let stage: ConnectionStage
    let reconnectAttempt: Int
    let onResetConnection: (() -> Void)?

    @State private var showResetButton = false
    @State private var contentOpacity: Double = 0

    var body: some View {
        ZStack {
            // Opaque branded background.
            Design.Colors.background
                .ignoresSafeArea()

            VStack(spacing: Design.Spacing.lg) {
                Spacer()

                // Pulsing Kallisti Coin.
                Image("KallistiSeal")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 72, height: 72)
                    .symbolEffect(.pulse, options: .repeating)

                VStack(spacing: Design.Spacing.xs) {
                    Text("Kallisti")
                        .font(Design.Typography.sectionTitle)
                        .foregroundStyle(Design.Colors.foreground)

                    Text(stage.displayLabel)
                        .font(Design.Typography.body)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .id(stage.displayLabel)
                        .transition(.opacity)
                        .animation(.easeInOut(duration: 0.3), value: stage.displayLabel)
                }

                ProgressView()
                    .tint(Design.Brand.accent)
                    .padding(.top, Design.Spacing.sm)

                if reconnectAttempt >= 5 {
                    Text("Connection struggling - attempt \(reconnectAttempt)")
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.danger)
                } else if reconnectAttempt > 1 {
                    Text("Reconnecting... (\(reconnectAttempt))")
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.tertiaryForeground)
                }

                Spacer()

                // Reset connection button, shown after a short honest delay.
                if let onResetConnection, showResetButton {
                    Button {
                        onResetConnection()
                        showResetButton = false
                    } label: {
                        Text("Reset Connection")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }
                    .padding(.bottom, Design.Spacing.xl)
                    .transition(.opacity)
                }
            }
        }
        .opacity(contentOpacity)
        .onAppear {
            withAnimation(.easeIn(duration: 0.3)) {
                contentOpacity = 1
            }
        }
        .task(id: stage) {
            // Show reset button after a short delay in any non-connected,
            // recoverable state -- including .preparing during repeated
            // failures where the stage never advances.
            showResetButton = false
            guard stage != .connected else { return }
            try? await Task.sleep(for: .seconds(8))
            if stage != .connected {
                withAnimation { showResetButton = true }
            }
        }
    }
}
