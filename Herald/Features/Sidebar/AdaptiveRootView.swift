import SwiftUI

/// Root view that adapts to device class:
/// - iPad: Three-column NavigationSplitView (sidebar + content + detail)
/// - iPhone landscape: iPad layout scaled down (verticalSizeClass = .compact)
/// - iPhone portrait: TabView with slide-out session drawer
struct AdaptiveRootView: View {
    @Environment(TabRouter.self) private var router
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
                .onAppear { installRouterBinding() }
                .onDisappear { removeRouterBinding() }
        } else {
            MainTabView()
        }
    }

    // MARK: - iPad Layout (Three Columns)

    private var iPadLayout: some View {
        NavigationSplitView {
            // Sidebar: session browser + section picker
            iPadSidebarView(
                selectedSection: $selectedSection,
                isRightPanelOpen: $isRightPanelOpen
            )
        } content: {
            // Main content area
            contentColumn
        } detail: {
            // Right panel: Logs/Terminal/Tools/Canvas
            if isRightPanelOpen {
                iPadRightPanelView(
                    isOpen: $isRightPanelOpen,
                    selectedTab: $rightPanelTab
                )
                .frame(minWidth: 280, idealWidth: 320)
                .background(Design.Colors.surface)
            } else {
                // Placeholder when panel is closed
                VStack {
                    Image(systemName: "sidebar.right")
                        .font(.system(size: 32))
                        .foregroundStyle(Design.Colors.tertiaryForeground)
                    Text("Open the detail panel")
                        .font(Design.Typography.caption)
                        .foregroundStyle(Design.Colors.tertiaryForeground)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Design.Colors.background)
            }
        }
    }

    @ViewBuilder
    private var contentColumn: some View {
        switch selectedSection {
        case .chat:
            ChatScreen(isSessionDrawerOpen: .constant(false))
                .toolbar {
                    ToolbarItem(placement: .topBarTrailing) {
                        rightPanelToggle
                    }
                }
        case .inbox:
            InboxScreen()
                .toolbar {
                    ToolbarItem(placement: .topBarTrailing) {
                        rightPanelToggle
                    }
                }
        case .talk:
            TalkModeScreen()
        case .settings:
            SettingsScreen()
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
        .accessibilityLabel("Toggle detail panel")
    }

    // MARK: - Router Synchronization

    private func installRouterBinding() {
        router.oniPadSectionSwitch = { [self] section in
            withAnimation {
                selectedSection = section
            }
        }
        // Sync initial state
        syncRouterToSection()
    }

    private func removeRouterBinding() {
        router.oniPadSectionSwitch = nil
    }

    private func syncRouterToSection() {
        // Map router tab to sidebar section
        switch router.selectedTab {
        case .chat: selectedSection = .chat
        case .inbox: selectedSection = .inbox
        case .talk: selectedSection = .talk
        case .settings: selectedSection = .settings
        }
    }
}
