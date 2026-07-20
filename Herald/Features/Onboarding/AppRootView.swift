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
                        MainTabView()
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
                        Text("Connecting to Hermes…")
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
                        Text("This is taking longer than usual.\nCheck that your Hermes host is online.")
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
        .task {
            try? await Task.sleep(for: .seconds(5))
            if !container.isLaunchReady {
                withAnimation { showLongWait = true }
            }
        }
    }
}
