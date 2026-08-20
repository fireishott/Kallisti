import SwiftUI

struct AppRootView: View {
    @Environment(AppContainer.self) private var container
    @Environment(ModelStore.self) private var modelStore
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

    /// Cold-launch bookkeeping for the launch surface. The base gate
    /// (shouldShowLoadingSurface) returns false the instant the socket
    /// connects, but on a fast LAN that can be <1s after launch - the coin
    /// and status flash before the user registers them. These states hold
    /// the surface until the app is genuinely ready (connected + models
    /// loaded) with a minimum display time, capped so a hung model fetch
    /// can never trap the user behind the splash.
    @State private var launchSurfaceFirstAppearedAt: Date?
    private static let launchSurfaceMinDisplay: TimeInterval = 1.5
    private static let launchSurfaceMaxHold: TimeInterval = 12

    /// Build 108 (hang fix): heartbeat that forces the loading surface to
    /// re-evaluate while it is mounted. `showLoadingSurface` is a computed
    /// property - it only re-runs when an observed @State/@Observable value
    /// changes. After the socket connects, the surface could sit on
    /// "Connected" forever because nothing changes again (models loaded
    /// before the 1.5s min-display, or models never load) - the 12s
    /// max-hold was dead code with no timer behind it. This tick re-renders
    /// the view every 0.5s while the surface shows, so the elapsed cap
    /// actually fires.
    @State private var surfaceHeartbeat = Date()

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
        // Build 108 (hang fix): record the first appearance UNCONDITIONALLY,
        // not only when base==true. If the socket connects before the first
        // body evaluation (fast LAN), base is false from the very first
        // render - the old code left launchSurfaceFirstAppearedAt nil, so
        // `appearedAt ?? .now` re-rolled .now on every evaluation and the
        // 12s max-hold could never fire. The surface then sat on
        // "Connected" forever waiting for a state change that never came.
        if launchSurfaceFirstAppearedAt == nil {
            launchSurfaceFirstAppearedAt = .now
        }
        let base = Self.shouldShowLoadingSurface(
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
            hasResolvedStoredLogin: nativeClient?.hasResolvedStoredLogin ?? false,
            isConversationRefreshing: container.chatStore.isLoading,
            hasUsableConversation: !(container.chatStore.conversation?.messages.isEmpty ?? true)
        )
        if base {
            // Build 132.3: ONCE the app has shown the connected UI, the
            // opaque launch surface must NEVER come back for recoverable
            // connecting/reconnecting states. Before this guard, any status
            // churn (foreground reconnect, watchdog reconnect, refresh) re-ran
            // this branch: isRecovering = (status == .connecting || .reconnecting)
            // was true, so the surface re-mounted over the main chat UI -
            // the user saw the app, then the loading screen again with
            // "Connected, <model>" (the model that was already loaded).
            // Reconnect churn is owned by the chat connection banner, per the
            // Build 69 r1 comment on hasShownConnectedApp.
            if hasShownConnectedApp {
                return false
            }
            // Connecting window (or unresolved init): record when the
            // launch surface first appeared and keep it mounted.
            if launchSurfaceFirstAppearedAt == nil {
                launchSurfaceFirstAppearedAt = .now
            }
            // Build 127: the base gate (connecting / reconnecting /
            // conversation refresh) is NOT a hard trap. If a conversation
            // refresh hangs (chatStore.isLoading stuck true with an empty
            // conversation), base stays true forever and the surface pins on
            // "Connected" with a spinner that never retires. Apply the same
            // max-hold as the cold-launch path so a hung refresh degrades to
            // the chat's own connection banner instead of an opaque splash.
            let baseElapsed = surfaceHeartbeat.timeIntervalSince(launchSurfaceFirstAppearedAt ?? .now)
            if baseElapsed > Self.launchSurfaceMaxHold {
                hasShownConnectedApp = true
                return false
            }
            return true
        }
        // Base gate says hide. During a warm reconnect, wait for the connected
        // callback's conversation reload to finish before exposing chat. This
        // prevents a returning user from landing on stale cached history.
        if hasShownConnectedApp {
            // Build 132.3: also bound this by the launch max-hold. A hung
            // conversation refresh after the app already showed must not
            // re-cover the chat with the opaque splash indefinitely.
            let elapsed = surfaceHeartbeat.timeIntervalSince(launchSurfaceFirstAppearedAt ?? .now)
            if elapsed > Self.launchSurfaceMaxHold {
                return false
            }
            return container.chatStore.isLoading
                && (container.chatStore.conversation?.messages.isEmpty ?? true)
        }

        // On a cold launch, hold until the app is actually ready (green dot on
        // the model picker = connected + models loaded) and the minimum display
        // time has elapsed, so the launch screen registers instead of flashing.
        // Never hold a fresh install with nothing to connect to behind the
        // splash - those devices must land in onboarding immediately.
        // (Stale keychain auth with no relay is exactly this case: it must
        // NOT trap the user behind a reconnect splash, per StartupUXTests.)
        let hasSomethingToConnect = (relayConfiguredAtLaunch ?? false)
            || (nativeClient?.hasStoredLogin ?? false)
        guard hasSomethingToConnect else { return false }
        let appearedAt = launchSurfaceFirstAppearedAt ?? .now
        // Build 108: use the heartbeat tick as "now" so this elapsed check
        // is re-evaluated even when no other state changes. Without it, a
        // surface that reached .connected with models already loaded (or
        // never loading) froze at the last state change.
        let elapsed = surfaceHeartbeat.timeIntervalSince(appearedAt)
        let minDisplayElapsed = elapsed >= Self.launchSurfaceMinDisplay
        let ready = isAppReady
        if (ready && minDisplayElapsed) || elapsed > Self.launchSurfaceMaxHold {
            // Retire the launch surface for this process. Mid-session
            // reconnects are owned by the chat connection banner.
            hasShownConnectedApp = true
            return false
        }
        return true
    }

    /// Green-dot readiness: socket connected AND an active model known.
    /// Build 132.3 (refine): do NOT require the model catalog refresh to be
    /// idle. A refresh in flight (modelLoading true) with an already-known
    /// activeModel means the app is fully usable - the picker refreshes in
    /// the background. Requiring isLoading to finish made cold start hold on
    /// "Connected, <model>" for seconds after connect, reading as stuck.
    private var isAppReady: Bool {
        let connected = container.nativeGatewayClient?.connectionStatus == .connected
        let modelsReady = modelStore.activeModel != nil
        return connected && modelsReady
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
        hasResolvedStoredLogin: Bool = true,
        isConversationRefreshing: Bool = false,
        hasUsableConversation: Bool = false
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
            // Build 114: after the first verified connect, cover a sustained
            // disconnect/reconnect for returning users. NativeKallistiClient's
            // three-second reconnect grace keeps brief transport churn green,
            // so this does not resurrect the old content/surface respring loop.
            // Once connected, keep the surface through the authoritative chat
            // refresh before revealing cached content.
            return hasStoredLogin
                && (isRecovering || (isConversationRefreshing && !hasUsableConversation))
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
                    // Build 78.6: a stored login means a returning user -
                    // mount chat immediately even before the relay snapshot
                    // freezes or the socket connects. The connection banner
                    // owns the not-yet-connected window. This kills the
                    // cold-launch onboarding flash for configured devices.
                    // Build 131.10: a wiped device (Reset Connection cleared
                    // pairing + stored login) must land in onboarding, even
                    // when a relay URL is still configured in settings. The
                    // relay-configured check alone used to send it back to
                    // chat with no pairing flow.
                    let hasLiveConnection = nativeClient.connectionStatus == .connected
                    let returningUser = nativeClient.hasStoredLogin
                        || (container.pairingStore.isPaired && (relayConfiguredAtLaunch ?? false))
                    if hasLiveConnection || returningUser {
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
                } : nil,
                appDisplayName: container.settingsStore.settings.appDisplayName,
                modelLoading: modelStore.isLoading,
                activeModelName: modelStore.activeModel?.name,
                latencyMs: container.connectorLatencyMs
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
        // Build 108 (hang fix): while the loading surface is mounted, tick
        // the heartbeat every 0.5s so showLoadingSurface's elapsed cap
        // re-evaluates. Without this the surface froze on "Connected"
        // forever once no further state changes occurred. The task id is
        // showLoadingSurface itself: when it flips false the old task is
        // cancelled and this one exits immediately.
        .task(id: showLoadingSurface) {
            guard showLoadingSurface else { return }
            // Build 127: the keyboard is a system window that floats ABOVE
            // the opaque loading surface. If the composer was focused when
            // the surface mounted (reconnect mid-typing), the keyboard stays
            // up and keystrokes still land in the hidden text field — the
            // "connecting screen with the kb still being able to type" bug.
            // Resign first responder globally so the surface is truly modal.
            UIApplication.shared.sendAction(#selector(UIResponder.resignFirstResponder), to: nil, from: nil, for: nil)
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(0.5))
                guard !Task.isCancelled else { break }
                surfaceHeartbeat = Date()
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
    /// Build 118: user-configurable app name (defaults to "Kallisti").
    let appDisplayName: String
    /// Build 108 (realtime status): live detail rendered under the stage
    /// label. modelLoading + activeModelName come straight from ModelStore;
    /// latencyMs comes from AppContainer's 3s latency monitor.
    let modelLoading: Bool
    let activeModelName: String?
    let latencyMs: Int?

    @State private var showResetButton = false
    /// Build 131.5: true while a reset is in flight; shows the spinner and
    /// disables the button. Cleared when the stage changes (reset completes
    /// or fails) via the .task(id: stage) below.
    @State private var isResetting = false
    @State private var contentOpacity: Double = 0
    /// Breathing coin phase. `.symbolEffect(.pulse)` is an SF Symbol effect
    /// and does nothing on a PNG asset, so the coin was static - this drives
    /// a real scale/opacity breathe loop instead.
    @State private var breathe = false

    /// Build 108: realtime status line. Stage label covers the connect
    /// phases; this adds what happens AFTER the socket is up - model
    /// catalog load and live latency - so "Connected" never reads as
    /// "stuck" when the app is actually working.
    private var liveStatus: String? {
        switch stage {
        case .connected:
            if modelLoading {
                return "Loading models..."
            }
            if let activeModelName {
                if let latencyMs {
                    return "\(activeModelName) · \(latencyMs)ms"
                }
                return activeModelName
            }
            return latencyMs.map { "\($0)ms" }
        case .restoring:
            return "Restoring conversations"
        default:
            return nil
        }
    }

    /// Build 127: show the spinner only while actual work is happening.
    /// Once the stage is .connected AND the model catalog finished loading,
    /// a spinning wheel reads as "stuck" — the surface is just holding for
    /// the minimum display time before retiring, so the status line (model
    /// + latency) carries the moment instead.
    private var showSpinner: Bool {
        stage != .connected || modelLoading
    }

    var body: some View {
        ZStack {
            // Opaque branded background.
            Design.Colors.background
                .ignoresSafeArea()

            VStack(spacing: Design.Spacing.lg) {
                Spacer()

                // Breathing Kallisti Coin.
                Image("KallistiSeal")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 140, height: 140)
                    .scaleEffect(breathe ? 1.07 : 0.96)
                    .opacity(breathe ? 1.0 : 0.82)
                    .shadow(
                        color: Design.Brand.accent.opacity(breathe ? 0.45 : 0.15),
                        radius: breathe ? 22 : 10
                    )
                    .onAppear {
                        withAnimation(.easeInOut(duration: 2.2).repeatForever(autoreverses: true)) {
                            breathe = true
                        }
                    }

                VStack(spacing: Design.Spacing.xs) {
                    Text(appDisplayName)
                        .font(Design.Typography.sectionTitle)
                        .foregroundStyle(Design.Colors.foreground)

                    Text(stage.displayLabel)
                        .font(Design.Typography.body)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .id(stage.displayLabel)
                        .transition(.opacity)
                        .animation(.easeInOut(duration: 0.3), value: stage.displayLabel)

                    // Build 108 (realtime status): live detail under the
                    // stage label - model catalog load, active model, and
                    // latency, all fed by real state.
                    if let liveStatus {
                        Text(liveStatus)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.tertiaryForeground)
                            .id(liveStatus)
                            .transition(.opacity)
                            .animation(.easeInOut(duration: 0.3), value: liveStatus)
                    }
                }

                if showSpinner {
                    ProgressView()
                        .tint(Design.Brand.accent)
                        .padding(.top, Design.Spacing.sm)
                }

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
                // Build 131.5: stays visible during the reset with a spinner
                // (the old code hid it instantly on tap, so a multi-second
                // reset read as "button did nothing").
                if let onResetConnection, showResetButton {
                    Button {
                        isResetting = true
                        onResetConnection()
                    } label: {
                        HStack(spacing: Design.Spacing.xs) {
                            if isResetting {
                                ProgressView()
                                    .controlSize(.small)
                            }
                            Text(isResetting ? "Resetting..." : "Reset Connection")
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.secondaryForeground)
                        }
                    }
                    .disabled(isResetting)
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
            isResetting = false
            guard stage != .connected else { return }
            try? await Task.sleep(for: .seconds(8))
            if stage != .connected {
                withAnimation { showResetButton = true }
            }
        }
    }
}
