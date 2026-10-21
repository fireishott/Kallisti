import Observation
import SwiftUI

// MARK: - Conversation detail view model

/// One conversation: its stored transcript, loaded once, then live turns.
///
/// The chat surface is reused rather than reimplemented: this model owns an
/// `ErisChatViewModel` bound to the conversation id, so a send goes out with
/// `conversation` set and the daemon writes the turn into this transcript. The
/// only thing this model adds is the load: a stored conversation has to be read
/// back before the first message of the session.
@MainActor
@Observable
final class ErisConversationDetailModel {
    /// The conversation every turn is filed under.
    let conversationID: String
    /// The stored title, refreshed by the load.
    private(set) var title: String
    /// The chat surface's model, bound to this conversation.
    let chat: ErisChatViewModel
    /// Whether the transcript is in flight.
    private(set) var isLoading = false
    /// The failure that stopped the transcript from loading.
    private(set) var failure: ErisFailureKind?
    /// Whether the daemon bounded the transcript, so older rows are missing.
    private(set) var isTruncated = false
    /// Whether a transcript has ever loaded.
    private(set) var hasLoaded = false

    /// The transport. An actor, so nothing here blocks the main thread.
    private let client: ErisClient

    init(
        client: ErisClient,
        conversationID: String,
        title: String,
        onSessionID: (@MainActor (String) -> Void)? = nil
    ) {
        self.client = client
        self.conversationID = conversationID
        self.title = title
        self.chat = ErisChatViewModel(
            client: client,
            onSessionID: onSessionID,
            conversationID: conversationID
        )
    }

    /// The title to draw, never an empty row.
    var displayTitle: String {
        let trimmed = title.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "Untitled conversation" : trimmed
    }

    /// Load the transcript unless it is already on screen.
    func loadIfNeeded() async {
        guard !hasLoaded else { return }
        await load()
    }

    /// Read the conversation and replace the visible rows with what is stored.
    ///
    /// A refusal leaves whatever is already on screen alone: a failed re-read must
    /// not wipe a transcript the user is reading.
    func load() async {
        guard !isLoading else { return }
        isLoading = true
        failure = nil
        defer { isLoading = false }
        do {
            let detail = try await client.conversation(id: conversationID)
            title = detail.displayTitle
            isTruncated = detail.truncated
            chat.replaceTranscript(with: detail)
            hasLoaded = true
        } catch {
            failure = ErisFailureKind(error)
        }
    }
}

// MARK: - Conversation detail screen

/// One conversation, as the node between the list and the chat surface.
///
/// Three states, all reachable and none of them a dead end: loading (a spinner),
/// a failed load (the daemon's reason, a retry, and re-pairing when the pair is
/// dead), and loaded (the chat surface, with the conversation id attached so every
/// send is filed under it).
struct ErisConversationDetailScreen: View {
    let store: ErisDaemonStore
    let model: ErisConversationDetailModel
    /// Return to the list. The surface owns which conversation is open, so this
    /// screen never builds a second navigation stack of its own.
    var onBack: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().overlay(Design.Colors.divider)
            content
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Design.Colors.background)
        .task { await model.loadIfNeeded() }
    }

    // MARK: Header

    private var header: some View {
        HStack(spacing: Design.Spacing.xs) {
            Button(action: onBack) {
                HStack(spacing: Design.Spacing.xxxs) {
                    Image(systemName: "chevron.left")
                        .font(.system(size: Design.Size.iconTiny, weight: .semibold))
                    Text("Conversations")
                        .font(Design.Typography.footnote)
                }
                .foregroundStyle(Design.Colors.accent)
                .frame(minHeight: Design.Size.minTapTarget)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("eris.backToConversations")

            Spacer(minLength: Design.Spacing.xs)

            Text(model.displayTitle)
                .font(Design.Typography.footnote)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .padding(.horizontal, Design.Spacing.md)
    }

    // MARK: States

    @ViewBuilder
    private var content: some View {
        if model.isLoading, !model.hasLoaded {
            ErisLoadingView(label: "Loading the transcript…")
            Spacer(minLength: 0)
        } else if let failure = model.failure, !model.hasLoaded {
            VStack(alignment: .leading, spacing: Design.Spacing.sm) {
                ErisFailureView(
                    kind: failure,
                    onRetry: { Task { await model.load() } },
                    onPairAgain: { Task { await store.forget() } }
                )
                if failure.needsPairing == false {
                    Text("The conversation is still there on the daemon. Retrying reads it again.")
                        .font(Design.Typography.helper)
                        .foregroundStyle(Design.Colors.tertiaryForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .padding(Design.Spacing.md)
            Spacer(minLength: 0)
        } else {
            VStack(spacing: 0) {
                if model.isTruncated {
                    Text("This is the newest page of the transcript: older messages are not shown.")
                        .font(Design.Typography.caption2)
                        .foregroundStyle(Design.Colors.tertiaryForeground)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, Design.Spacing.md)
                        .padding(.top, Design.Spacing.xxs)
                }
                ErisChatScreen(store: store, model: model.chat, showsForget: false)
            }
        }
    }
}
