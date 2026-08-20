import SwiftUI

struct MainTabView: View {
    @Environment(TabRouter.self) private var router
    @Environment(TalkStore.self) private var talkStore
    @Environment(ChatStore.self) private var chatStore
    @Environment(SettingsStore.self) private var settingsStore
    @State private var isSessionDrawerOpen = false

    var body: some View {
        @Bindable var router = router
        ZStack {
            // Build 128.81: system tab bar restored. The iPad top nav bar
            // size was right - it just needed Kallisti theming, not a
            // custom replacement. Notes rides as a tab with the handwriting
            // icon on both platforms.
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

                // ── Notes Tab (handwriting icon) ──
                // Build 128.88: iPad only. Notes is a full handwriting /
                // sketch workspace that makes no sense on the phone's tab
                // bar; it was crowding the iPhone tabs down to 5.
                if !DeviceClass.isPhone {
                    NavigationStack {
                        NotesWorkspaceView()
                    }
                    .tabItem { Label(AppTab.notes.title, systemImage: AppTab.notes.icon) }
                    .tag(AppTab.notes)
                }

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
            // Kallisti theming for the system bar: obsidian material, platinum
            // tint. On iPad this is the top nav bar; on iPhone the bottom bar.
            .toolbarBackground(Design.Colors.background.opacity(0.92), for: .tabBar)
            .toolbarBackground(.visible, for: .tabBar)
            .toolbarColorScheme(.dark, for: .tabBar)
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

            // Session drawer overlay (swipe from left edge) - hidden entirely
            // in TUI mode: the terminal owns the whole chat surface.
            if settingsStore.settings.chatDisplayMode == .rich {
                iPhoneSessionDrawer(isOpen: $isSessionDrawerOpen)
            }
        }
        // Build 131.18: left-edge swipe-back when NOT in rich chat. In rich
        // mode the left edge is owned by the session drawer, so this only
        // fires in terminal/TUI mode and on non-chat tabs. Pops the current
        // tab's navigation stack first; if at root, switches to the previous
        // tab (chat <- inbox <- talk <- notes <- settings).
        .simultaneousGesture(
            DragGesture(minimumDistance: 24, coordinateSpace: .global)
                .onEnded { value in
                    // Only treat as back-swipe when the drag STARTS at the
                    // left edge and moves right (natural back direction).
                    guard value.startLocation.x < 32,
                          value.translation.width > 60,
                          abs(value.translation.height) < 80 else { return }
                    // Skip in rich chat: drawer owns the edge.
                    if router.selectedTab == .chat,
                       settingsStore.settings.chatDisplayMode == .rich {
                        return
                    }
                    if !router.path().isEmpty {
                        router.popToRoot()
                    } else {
                        let all: [AppTab] = [.chat, .inbox, .talk, .notes, .settings]
                        guard let idx = all.firstIndex(of: router.selectedTab),
                              idx > 0 else { return }
                        router.switchToTab(all[idx - 1])
                    }
                }
        )
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