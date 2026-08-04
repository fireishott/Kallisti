import SwiftUI

struct AppRootView: View {
    @Environment(AppContainer.self) private var container
    @State private var showLongWait = false

    var body: some View {
        ZStack {
            Design.Colors.background
                .ignoresSafeArea()

            if container.isLaunchReady {
                Group {
                    if !container.pairingStore.isPaired {
                        OnboardingFlowView(initialStep: .welcome)
                    } else if container.pairingStore.needsPermissionsOnboarding {
                        OnboardingFlowView(initialStep: .permissions)
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
                        Text("Connecting to Herald…")
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
                        Text("This is taking longer than usual.\nCheck that your Herald host is online.")
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
