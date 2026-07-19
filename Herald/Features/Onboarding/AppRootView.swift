import SwiftUI

struct AppRootView: View {
    @Environment(AppContainer.self) private var container

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
            }
        }
        .animation(Design.Motion.standard, value: container.pairingStore.isPaired)
        .animation(Design.Motion.standard, value: container.pairingStore.needsPermissionsOnboarding)
        .animation(Design.Motion.gentle, value: container.isLaunchReady)
    }
}
