import SwiftUI

// MARK: - Paired surface
//
// A paired device lands here instead of on the pairing form. Four lanes over one
// daemon, presented as a themed tab bar - the Kallisti face - rather than a bare
// segmented picker:
//
//   Chat           turns, streamed
//   Conversations  stored transcripts, readable and continuable
//   Approvals      the calls parked for a person, answerable (admin scope only)
//   Settings       model, sandbox, memory, approval (admin scope only)
//
// Approvals and Settings are admin scope: a device paired from a code minted
// without the administration box gets the daemon's own 403 scope_denied sentence
// rather than a screen that pretends to work.

/// The paired surface: four themed tabs over one daemon.
struct ErisPairedSurface: View {
    let store: ErisDaemonStore

    /// Which tab is selected.
    @State private var lane: Lane = .chat
    /// The conversation whose transcript is open, if any. Held here so switching
    /// tabs and back does not throw the transcript model away mid-turn.
    @State private var openConversation: ErisConversationDetailModel?

    /// The lanes the device listener serves.
    enum Lane: String, CaseIterable, Identifiable, Hashable {
        case chat
        case conversations
        case approvals
        case settings

        var id: String { rawValue }

        var title: String {
            switch self {
            case .chat: return "Chat"
            case .conversations: return "Conversations"
            case .approvals: return "Approvals"
            case .settings: return "Settings"
            }
        }

        var icon: String {
            switch self {
            case .chat: return "bubble.left.and.bubble.right"
            case .conversations: return "list.bullet.rectangle"
            case .approvals: return "checkmark.seal"
            case .settings: return "gearshape"
            }
        }
    }

    var body: some View {
        TabView(selection: $lane) {
            chatTab.tag(Lane.chat)
            conversationsTab.tag(Lane.conversations)
            approvalsTab.tag(Lane.approvals)
            settingsTab.tag(Lane.settings)
        }
        .tint(Design.Brand.accent)
        // Kallisti theming for the system bar: obsidian material, platinum tint.
        .toolbarBackground(Design.Colors.background.opacity(0.92), for: .tabBar)
        .toolbarBackground(.visible, for: .tabBar)
        .toolbarColorScheme(.dark, for: .tabBar)
    }

    // MARK: Tabs

    @ViewBuilder
    private var chatTab: some View {
        NavigationStack {
            if let chat = store.chat {
                ErisChatScreen(store: store, model: chat)
            } else {
                missingModel("chat")
            }
        }
        .tabItem { Label(Lane.chat.title, systemImage: Lane.chat.icon) }
    }

    @ViewBuilder
    private var conversationsTab: some View {
        NavigationStack {
            conversations
        }
        .tabItem { Label(Lane.conversations.title, systemImage: Lane.conversations.icon) }
    }

    @ViewBuilder
    private var approvalsTab: some View {
        NavigationStack {
            if let approvals = store.approvals {
                ErisApprovalsScreen(store: store, model: approvals)
            } else {
                missingModel("approvals")
            }
        }
        .tabItem { Label(Lane.approvals.title, systemImage: Lane.approvals.icon) }
    }

    @ViewBuilder
    private var settingsTab: some View {
        NavigationStack {
            if let settings = store.settings {
                ErisSettingsScreen(store: store, model: settings)
            } else {
                missingModel("settings")
            }
        }
        .tabItem { Label(Lane.settings.title, systemImage: Lane.settings.icon) }
    }

    // MARK: Conversations (list or one open transcript)

    @ViewBuilder
    private var conversations: some View {
        if let detail = openConversation {
            ErisConversationDetailScreen(
                store: store,
                model: detail,
                onBack: {
                    openConversation = nil
                    // Coming back to the list is the moment the daemon's own row
                    // counts are known to be newer than what is on screen.
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
        } else {
            missingModel("conversations")
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
