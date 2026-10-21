import SwiftUI

// MARK: - Full paired surface
//
// The complete Kallisti frontend over the Eris daemon - the full tab structure
// and identity, not a reduced shell. Chat is live over Eris (/v1/turns + SSE).
// Senses, Settings, Permissions and Approvals are wired to Eris or local iOS.
// Inbox, Talk and Notes have no Eris route yet, so their tabs are fully present
// but clearly marked pending-backend rather than silently removed.

/// The paired surface: five tabs over one daemon.
struct ErisPairedSurface: View {
    let store: ErisDaemonStore

    /// Which tab is selected.
    @State private var lane: Lane = .chat
    /// The conversation whose transcript is open, if any. Held here so switching
    /// tabs and back does not throw the transcript model away mid-turn.
    @State private var openConversation: ErisConversationDetailModel?
    /// Whether the session drawer (the conversation browser) is presented.
    @State private var isSessionDrawerOpen = false

    /// The tabs the app exposes.
    enum Lane: String, CaseIterable, Identifiable, Hashable {
        case chat
        case inbox
        case talk
        case notes
        case settings

        var id: String { rawValue }

        var title: String {
            switch self {
            case .chat: return "Chat"
            case .inbox: return "Inbox"
            case .talk: return "Talk"
            case .notes: return "Notes"
            case .settings: return "Settings"
            }
        }

        var icon: String {
            switch self {
            case .chat: return "bubble.left.and.bubble.right"
            case .inbox: return "tray"
            case .talk: return "waveform"
            case .notes: return "pencil.and.outline"
            case .settings: return "gearshape"
            }
        }
    }

    var body: some View {
        TabView(selection: $lane) {
            chatTab.tag(Lane.chat)
            inboxTab.tag(Lane.inbox)
            talkTab.tag(Lane.talk)
            notesTab.tag(Lane.notes)
            settingsTab.tag(Lane.settings)
        }
        .tint(Design.Brand.accent)
        // Kallisti theming for the system bar: obsidian material, platinum tint.
        .toolbarBackground(Design.Colors.background.opacity(0.92), for: .tabBar)
        .toolbarBackground(.visible, for: .tabBar)
        .toolbarColorScheme(.dark, for: .tabBar)
    }

    // MARK: Chat (live over Eris)

    @ViewBuilder
    private var chatTab: some View {
        NavigationStack {
            if let chat = store.chat {
                ErisChatScreen(store: store, model: chat)
                    .toolbar {
                        ToolbarItem(placement: .topBarLeading) {
                            Button {
                                isSessionDrawerOpen = true
                            } label: {
                                Label("Sessions", systemImage: "sidebar.left")
                                    .font(Design.Typography.footnote)
                            }
                            .accessibilityIdentifier("eris.sessions")
                        }
                    }
            } else {
                missingModel("chat")
            }
        }
        .tabItem { Label(Lane.chat.title, systemImage: Lane.chat.icon) }
        .sheet(isPresented: $isSessionDrawerOpen) {
            sessionDrawer
        }
    }

    /// The session drawer: the conversation browser, or one open transcript.
    @ViewBuilder
    private var sessionDrawer: some View {
        NavigationStack {
            if let detail = openConversation {
                ErisConversationDetailScreen(
                    store: store,
                    model: detail,
                    onBack: {
                        openConversation = nil
                        Task { await store.conversations?.load() }
                    }
                )
                .id(detail.conversationID)
            } else if let model = store.conversations {
                ErisConversationsScreen(
                    store: store,
                    model: model,
                    onOpen: { summary in
                        openConversation = store.detailModel(
                            conversationID: summary.id,
                            title: summary.title
                        )
                    },
                    onCreate: { summary in
                        openConversation = store.detailModel(
                            conversationID: summary.id,
                            title: summary.title
                        )
                    }
                )
                .navigationTitle("Sessions")
                .toolbar {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button("Done") { isSessionDrawerOpen = false }
                            .font(Design.Typography.footnote)
                    }
                }
            } else {
                missingModel("conversations")
            }
        }
        .presentationDetents([.large])
        .presentationDragIndicator(.visible)
    }

    // MARK: Inbox / Talk / Notes (present, pending Eris backend)

    private var inboxTab: some View {
        NavigationStack {
            if let inbox = store.inbox {
                ErisInboxScreen(model: inbox)
            } else {
                missingModel("inbox")
            }
        }
        .tabItem { Label(Lane.inbox.title, systemImage: Lane.inbox.icon) }
    }

    private var talkTab: some View {
        NavigationStack {
            if let talk = store.talk {
                ErisTalkScreen(model: talk)
            } else {
                missingModel("talk")
            }
        }
        .tabItem { Label(Lane.talk.title, systemImage: Lane.talk.icon) }
    }

    private var notesTab: some View {
        NavigationStack {
            if let notes = store.notes {
                ErisNotesScreen(model: notes)
            } else {
                missingModel("notes")
            }
        }
        .tabItem { Label(Lane.notes.title, systemImage: Lane.notes.icon) }
    }

    // MARK: Settings (hub)

    private var settingsTab: some View {
        NavigationStack {
            List {
                Section("Eris") {
                    NavigationLink {
                        daemonSettings
                    } label: {
                        settingsRow("Daemon Settings", "gearshape.2", Design.Colors.foreground)
                    }

                    NavigationLink {
                        senses
                    } label: {
                        settingsRow("Senses", "heart.text.square", Design.Colors.foreground)
                    }

                    NavigationLink {
                        approvals
                    } label: {
                        settingsRow("Approvals", "checkmark.seal", Design.Colors.foreground)
                    }
                }

                Section("Device") {
                    NavigationLink {
                        PermissionsScreen()
                    } label: {
                        settingsRow("Permissions", "hand.raised", Design.Colors.foreground)
                    }
                }

                Section("Legacy - pending Eris backend") {
                    NavigationLink {
                        PendingBackendView(
                            title: "Connect Host",
                            icon: "link",
                            message: "Connect Host configured the legacy relay gateway. Eris replaces that transport, so this surface is present but waiting on the backend."
                        )
                    } label: {
                        settingsRow("Connect Host", "link", Design.Colors.tertiaryForeground)
                    }

                    NavigationLink {
                        PendingBackendView(
                            title: "Gateway Status",
                            icon: "antenna.radiowaves.left.and.right",
                            message: "Gateway Status reported on the legacy connector. Eris reports its own health, so this surface is present but waiting on the backend."
                        )
                    } label: {
                        settingsRow("Gateway Status", "antenna.radiowaves.left.and.right", Design.Colors.tertiaryForeground)
                    }

                    NavigationLink {
                        PendingBackendView(
                            title: "Gateway Logs",
                            icon: "doc.text",
                            message: "Gateway Logs streamed the legacy connector's logs. Eris has no log-stream route yet, so this surface is present but waiting on the backend."
                        )
                    } label: {
                        settingsRow("Gateway Logs", "doc.text", Design.Colors.tertiaryForeground)
                    }
                }
            }
            .scrollContentBackground(.hidden)
            .background(Design.Colors.background)
            .navigationTitle("Settings")
        }
        .tabItem { Label(Lane.settings.title, systemImage: Lane.settings.icon) }
    }

    @ViewBuilder
    private var daemonSettings: some View {
        if let settings = store.settings {
            ErisSettingsScreen(store: store, model: settings)
        } else {
            missingModel("settings")
        }
    }

    @ViewBuilder
    private var senses: some View {
        if let senses = store.senses {
            ErisSensesScreen(model: senses)
        } else {
            missingModel("senses")
        }
    }

    @ViewBuilder
    private var approvals: some View {
        if let approvals = store.approvals {
            ErisApprovalsScreen(store: store, model: approvals)
        } else {
            missingModel("approvals")
        }
    }

    private func settingsRow(_ title: String, _ icon: String, _ color: Color) -> some View {
        HStack(spacing: Design.Spacing.sm) {
            Image(systemName: icon)
                .foregroundStyle(color)
                .frame(width: 28)
            Text(title)
                .font(Design.Typography.body)
                .foregroundStyle(color)
        }
    }

    // MARK: Missing model

    /// A lane whose model is missing. Unreachable through the paired phase, but a
    /// blank pane is never the right answer to anything.
    private func missingModel(_ lane: String) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            Text("This device is not attached to a daemon")
                .font(Design.Typography.headline)
                .foregroundStyle(Design.Colors.foreground)
            Text("The \(lane) surface needs a paired daemon. Forget this one and pair again.")
                .font(Design.Typography.footnote)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .fixedSize(horizontal: false, vertical: true)
            Button("Forget this daemon") {
                Task { await store.forget() }
            }
            .font(Design.Typography.footnote)
            .foregroundStyle(Design.Colors.danger)
            .frame(minHeight: Design.Size.minTapTarget)
        }
        .padding(Design.Spacing.md)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: - Pending backend surface

/// A full-identity placeholder for a surface whose backend Eris does not serve
/// yet. Present (the tab and screen exist, themed correctly) but honest about
/// being pending - not a silent removal and not a fake no-op.
struct PendingBackendView: View {
    let title: String
    let icon: String
    let message: String

    var body: some View {
        VStack(spacing: Design.Spacing.lg) {
            Image(systemName: icon)
                .font(.system(size: 44, weight: .regular))
                .foregroundStyle(Design.Colors.secondaryForeground)
            Text(title)
                .font(Design.Typography.screenTitle2)
                .foregroundStyle(Design.Colors.foreground)
            Text(message)
                .font(Design.Typography.body)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            Label("Pending Eris backend", systemImage: "hourglass")
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.tertiaryForeground)
                .padding(.horizontal, Design.Spacing.md)
                .padding(.vertical, Design.Spacing.xs)
                .background(Design.Colors.backgroundRaised)
                .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
        }
        .padding(Design.Spacing.xl)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Design.Colors.background.ignoresSafeArea())
        .navigationTitle(title)
        .navigationBarTitleDisplayMode(.inline)
    }
}
