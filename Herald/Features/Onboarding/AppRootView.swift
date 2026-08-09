import SwiftUI

struct AppRootView: View {
    @Environment(AppContainer.self) private var container
    @State private var showLongWait = false

    /// Whether the connection overlay should be shown.
    private var showConnectionOverlay: Bool {
        guard let nativeClient = container.nativeGatewayClient else { return false }
        let status = nativeClient.connectionStatus
        // Show overlay during connecting/reconnecting with stored login,
        // or during initial connecting before isLaunchReady.
        return (status == .connecting || status == .reconnecting)
            && nativeClient.hasStoredLogin
    }

    var body: some View {
        ZStack {
            Design.Colors.background
                .ignoresSafeArea()

            if container.isLaunchReady {
                Group {
                    if let nativeClient = container.nativeGatewayClient {
                        // Native gateway: pairing is irrelevant (no connector
                        // relay/pairing-code handshake). Gate on hasStoredLogin,
                        // not raw connectionStatus -- a socket that drops after
                        // a successful login (idle reap, cell handoff, a flaky
                        // reconnect) must not bounce the user back through
                        // onboarding and a redundant Nous OAuth login while
                        // NativeKallistiClient's own background reconnect is
                        // already retrying with the stored token. Onboarding
                        // is for devices that have never logged in at all.
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

                // Full-screen connection overlay above app content.
                if showConnectionOverlay, let nativeClient = container.nativeGatewayClient {
                    ConnectionOverlay(
                        stage: nativeClient.connectionStage,
                        reconnectAttempt: nativeClient.currentReconnectAttempt,
                        isRecoverable: nativeClient.hasStoredLogin,
                        onResetConnection: nativeClient.hasStoredLogin ? {
                            Task { await nativeClient.resetConnection() }
                        } : nil
                    )
                    .transition(.opacity)
                    .zIndex(10)
                }
            } else {
                // Connecting screen while app initializes
                VStack(spacing: Design.Spacing.lg) {
                    Spacer()

                    // Pulsing Herald icon
                    Image(systemName: "bubble.left.and.bubble.right.fill")
                        .font(.system(size: 48))
                        .foregroundStyle(Design.Brand.accent)
                        .symbolEffect(.pulse, options: .repeating)

                    VStack(spacing: Design.Spacing.xs) {
                        Text("Connecting to Kallisti…")
                            .font(Design.Typography.sectionTitle)
                            .foregroundStyle(Design.Colors.foreground)

                        Text("Establishing secure connection")
                            .font(Design.Typography.callout)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                    }

                    ProgressView()
                        .tint(Design.Brand.accent)
                        .padding(.top, Design.Spacing.sm)

                    if showLongWait {
                        Text("This is taking longer than usual.\nCheck that your Kallisti host is online.")
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .multilineTextAlignment(.center)
                            .padding(.top, Design.Spacing.md)
                            .transition(.opacity)
                    }

                    Spacer()
                }
                .transition(.opacity)
            }
        }
        .animation(Design.Motion.standard, value: container.pairingStore.isPaired)
        .animation(Design.Motion.standard, value: container.pairingStore.needsPermissionsOnboarding)
        .animation(Design.Motion.standard, value: container.nativeGatewayClient?.connectionStatus)
        .animation(Design.Motion.gentle, value: container.isLaunchReady)
        .animation(Design.Motion.standard, value: container.sessionStore.launchState)
        .task {
            try? await Task.sleep(for: .seconds(5))
            if !container.isLaunchReady {
                withAnimation { showLongWait = true }
            }
        }
    }

    private var authFailureView: some View {
        VStack(spacing: Design.Spacing.lg) {
            Spacer()

            Image(systemName: "exclamationmark.lock.fill")
                .font(.system(size: 48))
                .foregroundStyle(Design.Colors.danger)

            VStack(spacing: Design.Spacing.xs) {
                Text("Authentication Failed")
                    .font(Design.Typography.sectionTitle)
                    .foregroundStyle(Design.Colors.foreground)

                Text("Your session has expired and could not be renewed.")
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .multilineTextAlignment(.center)
            }

            VStack(spacing: Design.Spacing.md) {
                Button {
                    Task { await container.repairFromAuthFailure() }
                } label: {
                    Text("Re-pair Device")
                        .font(Design.Typography.body)
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .tint(Design.Brand.accent)
            }
            .padding(.horizontal, Design.Spacing.xl)

            Spacer()
        }
    }

    private func networkFailureView(message: String) -> some View {
        VStack(spacing: Design.Spacing.lg) {
            Spacer()

            Image(systemName: "wifi.exclamationmark")
                .font(.system(size: 48))
                .foregroundStyle(Design.Colors.warning)

            VStack(spacing: Design.Spacing.xs) {
                Text("Connection Failed")
                    .font(Design.Typography.sectionTitle)
                    .foregroundStyle(Design.Colors.foreground)

                Text(message)
                    .font(Design.Typography.callout)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, Design.Spacing.xl)
            }

            VStack(spacing: Design.Spacing.md) {
                Button {
                    Task { await container.retryInitialization() }
                } label: {
                    Text("Retry")
                        .font(Design.Typography.body)
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.borderedProminent)
                .tint(Design.Brand.accent)

                Button {
                    Task { await container.repairFromAuthFailure() }
                } label: {
                    Text("Re-pair Device")
                        .font(Design.Typography.body)
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.bordered)
            }
            .padding(.horizontal, Design.Spacing.xl)

            Spacer()
        }
    }
}

/// Full-screen opaque overlay shown while the native gateway is
/// connecting, reconnecting, or relaunching with stored credentials.
/// Suppresses transient error messages (like "Cannot connect to gateway")
/// by presenting real-time truthful stages derived from actual connection
/// work, not fake timers.
struct ConnectionOverlay: View {
    let stage: ConnectionStage
    let reconnectAttempt: Int
    let isRecoverable: Bool
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

                // Pulsing Kallisti icon.
                Image(systemName: "bubble.left.and.bubble.right.fill")
                    .font(.system(size: 48))
                    .foregroundStyle(Design.Brand.accent)
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

                if reconnectAttempt > 1 {
                    Text("Reconnect attempt \(reconnectAttempt)")
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.tertiaryForeground)
                        .transition(.opacity)
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
            guard isRecoverable, stage != .connected else { return }
            try? await Task.sleep(for: .seconds(5))
            if isRecoverable && stage != .connected {
                withAnimation { showResetButton = true }
            }
        }
    }
}
