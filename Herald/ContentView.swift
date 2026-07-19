import SwiftUI

struct MainTabView: View {
    @Environment(TabRouter.self) private var router
    @Environment(TalkStore.self) private var talkStore
    @Environment(ChatStore.self) private var chatStore
    @State private var isSessionDrawerOpen = false

    var body: some View {
        @Bindable var router = router
        ZStack {
            TabView(selection: $router.selectedTab) {
                // ── Chat Tab ──
                NavigationStack(path: router.binding(for: .chat)) {
                    ChatScreen(isSessionDrawerOpen: $isSessionDrawerOpen)
                        .navigationDestination(for: Route.self) { route in
                            routeDestination(route)
                        }
                }
                .tabItem { Label(AppTab.chat.title, systemImage: AppTab.chat.icon) }
                .tag(AppTab.chat)

                // ── Inbox Tab ──
                NavigationStack {
                    InboxScreen()
                }
                .tabItem { Label(AppTab.inbox.title, systemImage: AppTab.inbox.icon) }
                .tag(AppTab.inbox)

                // ── Talk Tab ──
                NavigationStack {
                    TalkModeScreen()
                }
                .tabItem { Label(AppTab.talk.title, systemImage: AppTab.talk.icon) }
                .tag(AppTab.talk)

                // ── Settings Tab ──
                NavigationStack {
                    SettingsScreen()
                        .navigationDestination(for: Route.self) { route in
                            routeDestination(route)
                        }
                }
                .tabItem { Label(AppTab.settings.title, systemImage: AppTab.settings.icon) }
                .tag(AppTab.settings)
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

            // Session drawer overlay (swipe from left edge)
            iPhoneSessionDrawer(isOpen: $isSessionDrawerOpen)
        }
    }

    @ViewBuilder
    private func routeDestination(_ route: Route) -> some View {
        switch route {
        case .permissions:
            PermissionsScreen()
        case .capture:
            CaptureScreen()
        case .connectHost:
            ConnectHeraldHostScreen()
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
