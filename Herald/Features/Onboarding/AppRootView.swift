import SwiftUI

struct AppRootView: View {
    @Environment(AppContainer.self) private var container
    /// Build 69 (r2): relay-configured state SNAPSHOTTED at first appearance.
    /// The old gate read `activeBaseURLString` live, so typing a full relay
    /// URL on the onboarding relay step flipped hasConfiguredRelay true and
    /// the loading surface covered onboarding mid-typing. This is captured
    /// once at launch: a fresh install that starts with no relay configured
    /// stays in onboarding no matter what the user types afterward.
    @State private var relayConfiguredAtLaunch: Bool?

    /// Build 69 (r1): the loading surface is a LAUNCH surface. Once
    /// hasConnectedOnce flips true it must never come back; reconnect churn
    /// is owned by the chat connection banner. This never gets reset.
    @State private var hasShownConnectedApp = false

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
        // r2: first time this evaluates, freeze the relay state. Everything
        // after that uses the frozen value, so typing a relay URL during
        // onboarding can never flip the loading surface on.
        if relayConfiguredAtLaunch == nil {
            relayConfiguredAtLaunch = container.settingsStore.settings.relayConfiguration.activeBaseURLString != nil
        }
        return Self.shouldShowLoadingSurface(
            isNative: nativeClient != nil,
            isLaunchReady: container.isLaunchReady,
            isRecovering: status == .connecting || status == .reconnecting,
            hasStoredLogin: nativeClient?.hasStoredLogin ?? false,
            hasConfiguredRelay: relayConfiguredAtLaunch ?? false,
            isBootstrapping: container.sessionStore.isBootstrapping,
            hasTerminalLegacyFailure: terminalLegacyFailure,
            // Build 76: pass the init-resolution gate so the splash glimpse
            // fix is honored at the same evaluation point. Until the init
            // Task finishes probing keychain, the loading surface is the
            // only thing the body should mount - returning users no longer
            // see OnboardingFlowView for one frame while the Task races.
            hasResolvedStoredLogin: nativeClient?.hasResolvedStoredLogin ?? false
        )
    }

    nonisolated static func shouldShowLoadingSurface(
        isNative: Bool,
        isLaunchReady: Bool,
        isRecovering: Bool,
        hasStoredLogin: Bool,
        hasConfiguredRelay: Bool = true,
        isBootstrapping: Bool,
        hasTerminalLegacyFailure: Bool,
        reconnectDebounced: Bool = false,
        hasResolvedStoredLogin: Bool = true
    ) -> Bool {
        if isNative {
            // Build 76: BEFORE the init Task has resolved credential state,
            // AppRootView must keep the loading surface mounted. Otherwise
            // a returning user with a stored login sees hasStoredLogin=false
            // on first render and the body briefly mounts OnboardingFlowView
            // before the init Task catches up and flips hasStoredLogin true.
            // The resolution is a cheap keychain read but it lives on a Task
            // hop, so the first render races it - this gate is the splash
            // glimpse fix.
            if !hasResolvedStoredLogin {
                return !hasStoredLogin
            }
            if !isLaunchReady {
                // Before the first verified connect, show the launch surface
                // ONLY when this device has something to actually connect to:
                // a stored login (set by a verified connect) or a relay
                // configured at launch. A fresh install (neither) goes
                // straight to onboarding - hasConnectedOnce can never become
                // true for a device with nothing to connect to, so without
                // this the surface would block onboarding forever. The relay
                // value here is the r2 launch-time SNAPSHOT, so typing a URL
                // during onboarding cannot resurrect the surface.
                if !hasConfiguredRelay {
                    return false
                }
                return true  // launch surface: configured cold start, connection pending
            }
            // Build 69 (r1): after the first verified connect (isLaunchReady),
            // NEVER show the loading surface again. Mid-session reconnects
            // are communicated by the chat connection banner; the old
            // `isRecovering && hasStoredLogin -> reconnectDebounced` branch
            // re-showed the full opaque surface on every reconnect, producing
            // the launch respring loop (content -> surface -> content 4-10x)
            // whenever the socket connected then dropped during startup.
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

            Group {
                if let nativeClient = container.nativeGatewayClient {
                    // Build 72: a relay configured at launch means a returning
                    // user - cold start must land in the chat UI, never onboarding.
                    // The chat UI owns its own connection banner for the brief
                    // not-yet-connected window.
                    if nativeClient.connectionStatus == .connected
                        || (relayConfiguredAtLaunch ?? false) {
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
            .opacity(showLoadingSurface ? 0 : 1)
            .allowsHitTesting(!showLoadingSurface)

            // Keep the loading surface mounted during the cross-fade so the
            // background is never exposed between launch and app content.
            LoadingSurface(
                stage: loadingStage,
                reconnectAttempt: container.nativeGatewayClient?.currentReconnectAttempt ?? 0,
                onResetConnection: showResetButton ? {
                    Task { await container.nativeGatewayClient?.resetConnection() }
                } : nil
            )
            .opacity(showLoadingSurface ? 1 : 0)
            .allowsHitTesting(showLoadingSurface)
            .accessibilityHidden(!showLoadingSurface)
            .zIndex(10)
        }
        .animation(Design.Motion.standard, value: container.pairingStore.isPaired)
        .animation(Design.Motion.standard, value: container.pairingStore.needsPermissionsOnboarding)
        .animation(Design.Motion.standard, value: container.nativeGatewayClient?.connectionStatus)
        .animation(Design.Motion.gentle, value: container.isLaunchReady)
        // Build 76: animate the resolution gate transition so the loading
        // surface cross-fades into the resolved body (onboarding or chat)
        // rather than snapping off. Only the resolution itself drives this;
        // mid-session reconnect churn never sets the gate.
        .animation(Design.Motion.gentle, value: container.nativeGatewayClient?.hasResolvedStoredLogin ?? false)
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
