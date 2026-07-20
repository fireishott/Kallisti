import SwiftUI

/// Root view that adapts to device class:
/// - iPad: NavigationSplitView with sidebar + detail + optional right panel
/// - iPhone landscape: iPad layout scaled down (verticalSizeClass = .compact)
/// - iPhone portrait: TabView with slide-out session drawer
struct AdaptiveRootView: View {
    @State private var selectedSection: SidebarSection = .chat
    @State private var isRightPanelOpen = false
    @State private var rightPanelTab: RightPanelTab = .logs
    @Environment(\.verticalSizeClass) private var verticalSizeClass

    private var useIPadLayout: Bool {
        DeviceClass.isPad || verticalSizeClass == .compact
    }

    var body: some View {
        if useIPadLayout {
            iPadLayout
        } else {
            MainTabView()
        }
    }

    // MARK: - iPad Layout

    private var iPadLayout: some View {
        NavigationSplitView {
            iPadSidebarView(
                selectedSection: $selectedSection,
                isRightPanelOpen: $isRightPanelOpen
            )
        } detail: {
            detailContent
                .overlay(alignment: .trailing) {
                    if isRightPanelOpen {
                        iPadRightPanelView(
                            isOpen: $isRightPanelOpen,
                            selectedTab: $rightPanelTab
                        )
                        .frame(width: 300)
                        .background(Design.Colors.surface)
                        .transition(.move(edge: .trailing))
                    }
                }
        }
    }

    @ViewBuilder
    private var detailContent: some View {
        switch selectedSection {
        case .chat:
            NavigationStack {
                ChatScreen(isSessionDrawerOpen: .constant(false))
                    .toolbar {
                        ToolbarItem(placement: .topBarTrailing) {
                            rightPanelToggle
                        }
                    }
            }
        case .inbox:
            NavigationStack {
                InboxScreen()
                    .toolbar {
                        ToolbarItem(placement: .topBarTrailing) {
                            rightPanelToggle
                        }
                    }
            }
        case .talk:
            NavigationStack {
                TalkModeScreen()
            }
        case .settings:
            NavigationStack {
                SettingsScreen()
            }
        }
    }

    private var rightPanelToggle: some View {
        Button {
            withAnimation(Design.Motion.standard) {
                isRightPanelOpen.toggle()
            }
        } label: {
            Image(systemName: "sidebar.right")
                .font(.system(size: 16, weight: .medium))
                .foregroundStyle(
                    isRightPanelOpen ? Design.Brand.accent : Design.Colors.secondaryForeground
                )
        }
        .buttonStyle(.plain)
    }
}
