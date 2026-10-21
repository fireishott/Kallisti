import SwiftUI

// MARK: - Paired surface
//
// A paired device lands here instead of on the pairing form. One segmented control
// over the three lanes the device listener actually serves:
//
//   Chat           turns, streamed
//   Conversations  stored transcripts, readable and continuable
//   Approvals      the calls parked for a person, answerable (admin scope only)
//   Settings       model, sandbox, memory, approval (admin scope only)
//
// Approvals used to be absent from this surface because the device listener had no
// route for them. It has two now, both behind the admin scope, and asking to
// administer a device is a box the operator ticks when minting its pairing code.
// A lane that answers a queue is only honest when the queue is real, and it is.

/// The paired surface: three lanes over one daemon.
struct ErisPairedSurface: View {
    let store: ErisDaemonStore

    /// Which lane is showing.
    @State private var lane: Lane = .chat
    /// The conversation whose transcript is open, if any. Held here so switching
    /// lanes and back does not throw the transcript model away mid-turn.
    @State private var openConversation: ErisConversationDetailModel?

    /// The lanes the device listener serves.
    enum Lane: String, CaseIterable, Identifiable {
        case chat
        case conversations
        case approvals
        case settings

        var id: String { rawValue }

        /// The label on the segment.
        var title: String {
            switch self {
            case .chat: return "Chat"
            case .conversations: return "Conversations"
            case .approvals: return "Approvals"
            case .settings: return "Settings"
            }
        }
    }

    var body: some View {
        VStack(spacing: 0) {
            lanePicker
            Divider().overlay(Design.Colors.divider)
            content
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Design.Colors.background)
    }

    // MARK: Lane control

    private var lanePicker: some View {
        Picker("Eris lane", selection: $lane) {
            ForEach(Lane.allCases) { lane in
                Text(lane.title).tag(lane)
            }
        }
        .pickerStyle(.segmented)
        .padding(.horizontal, Design.Spacing.md)
        .padding(.vertical, Design.Spacing.xs)
        .accessibilityIdentifier("eris.lane")
    }

    // MARK: Content

    @ViewBuilder
    private var content: some View {
        switch lane {
        case .chat:
            if let chat = store.chat {
                ErisChatScreen(store: store, model: chat)
            } else {
                missingModel("chat")
            }
        case .conversations:
            conversations
        case .approvals:
            if let approvals = store.approvals {
                ErisApprovalsScreen(store: store, model: approvals)
            } else {
                missingModel("approvals")
            }
        case .settings:
            if let settings = store.settings {
                ErisSettingsScreen(store: store, model: settings)
            } else {
                missingModel("settings")
            }
        }
    }

    /// The conversations lane: the list, or one open transcript.
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
