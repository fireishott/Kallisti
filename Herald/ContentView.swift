import SwiftUI

struct MainTabView: View {
    @Environment(TabRouter.self) private var router
    @Environment(TalkStore.self) private var talkStore
    @Environment(ChatStore.self) private var chatStore
    @State private var isSessionDrawerOpen = false

    // Build 128.78: unified bottom tab bar. TabView renders tabs at the TOP
    // on iPadOS, which broke the "iPad nav matches iPhone" requirement. This
    // custom bar forces the same bottom placement on every device class.
    private let barTabs: [AppTab] = [.chat, .inbox, .talk, .settings]

    var body: some View {
        @Bindable var router = router
        ZStack {
            tabContent(router.selectedTab)
                .safeAreaInset(edge: .bottom, spacing: 0) {
                    BottomTabBar(selection: $router.selectedTab, tabs: barTabs)
                }

            // Session drawer overlay (swipe from left edge)
            iPhoneSessionDrawer(isOpen: $isSessionDrawerOpen)
        }
        .tint(Design.Brand.accent)
        .sheet(item: $router.activeSheet) { destination in
            sheetDestination(destination)
        }
        .fullScreenCover(isPresented: $router.isVoiceOverlayPresented) {
            VoiceOverlayScreen()
        }
        .onChange(of: talkStore.lastCompletedSession != nil) { _, hasSession in
            if hasSession, let session = talkStore.lastCompletedSession {
                Task {
                    await chatStore.injectVoiceTranscript(
                        voiceSessionId: session.voiceSessionId,
                        duration: session.duration
                    )
                    talkStore.clearLastCompletedSession()
                }
            }
        }
    }

    @ViewBuilder
    private func tabContent(_ tab: AppTab) -> some View {
        Group {
            switch tab {
            case .chat:
                NavigationStack(path: router.binding(for: .chat)) {
                    ChatScreen(isSessionDrawerOpen: $isSessionDrawerOpen)
                        .navigationDestination(for: Route.self) { route in
                            routeDestination(route)
                        }
                }
            case .inbox:
                NavigationStack {
                    InboxScreen()
                }
            case .talk:
                NavigationStack {
                    TalkModeScreen()
                }
            case .notes:
                NavigationStack {
                    NotesWorkspaceView()
                }
            case .settings:
                NavigationStack {
                    SettingsScreen()
                        .navigationDestination(for: Route.self) { route in
                            routeDestination(route)
                        }
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    @ViewBuilder
    private func routeDestination(_ route: Route) -> some View {
        switch route {
        case .permissions:
            PermissionsScreen()
        case .connectHost:
            ConnectKallistiHostScreen()
        case .gatewayStatus:
            GatewayStatusScreen()
        case .gatewayLogs:
            GatewayLogsScreen()
        case .configEditor:
            ConfigEditorScreen()
        }
    }

    @ViewBuilder
    private func sheetDestination(_ destination: SheetDestination) -> some View {
        switch destination {
        case .settings:
            NavigationStack {
                SettingsScreen()
            }
            .presentationDetents([.large])
            .presentationDragIndicator(.visible)
        case .attachments:
            EmptyView()
        case .newChat:
            EmptyView()
        }
    }
}

// MARK: - Custom bottom tab bar

/// iPhone-style bottom tab bar, rendered identically on iPad. Replaces the
/// system TabView because iPadOS routes TabView tabs to the top.
private struct BottomTabBar: View {
    @Binding var selection: AppTab
    let tabs: [AppTab]

    var body: some View {
        HStack(spacing: 0) {
            ForEach(tabs) { tab in
                let isSelected = selection == tab
                Button {
                    selection = tab
                } label: {
                    VStack(spacing: 3) {
                        Image(systemName: tab.icon)
                            .font(.system(size: 20, weight: .medium))
                            .symbolVariant(isSelected ? .fill : .none)
                        Text(tab.title)
                            .font(.system(size: 10, weight: .medium))
                    }
                    .foregroundStyle(
                        isSelected ? Design.Brand.accent : Design.Colors.secondaryForeground
                    )
                    .frame(maxWidth: .infinity)
                    .contentShape(Rectangle())
                    .padding(.top, 8)
                    .padding(.bottom, 2)
                }
                .buttonStyle(.plain)
                .accessibilityLabel(tab.title)
                .accessibilityAddTraits(isSelected ? .isSelected : [])
            }
        }
        .background(.bar)
        .overlay(alignment: .top) {
            Rectangle()
                .fill(Design.Colors.divider.opacity(0.6))
                .frame(height: 0.5)
        }
    }
}