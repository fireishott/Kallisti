import SwiftUI

// MARK: - Search
//
// The transcript is the one thing this product had a lot of and no way to read.
// Every answer ever given is in the daemon's database, and until 1.6.0 the only
// way to reach one was to remember which conversation it was in and scroll.
//
// The daemon answers from an FTS5 index over the messages themselves, so a search
// costs no model call and no tokens. That shapes this screen: results arrive in
// milliseconds, so the field searches as you type, debounced rather than submitted.
//
// One row per conversation, not per message. A conversation where eight messages
// matched is one line with a count, because the question is "where did we work
// this out", not "show me nine paragraphs".

struct SearchScreen: View {
    @Environment(ChatStore.self) private var chatStore
    @Environment(TabRouter.self) private var router

    @State private var query = ""
    @State private var hits: [ErisSearchHit] = []
    @State private var isLoading = false
    @State private var errorMessage: String?
    @State private var opening: String?
    /// The in-flight debounce, cancelled when another keystroke lands.
    @State private var searchTask: Task<Void, Never>?

    var body: some View {
        ZStack {
            Design.Colors.background.ignoresSafeArea()

            ScrollView(.vertical) {
                VStack(spacing: Design.Spacing.lg) {
                    if let errorMessage {
                        Text(errorMessage)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.danger)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.horizontal, Design.Spacing.lg)
                    }

                    if query.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        prompt
                    } else if isLoading && hits.isEmpty {
                        ProgressView()
                            .tint(Design.Colors.accent)
                            .padding(.top, Design.Spacing.xl)
                    } else if hits.isEmpty {
                        noResults
                    } else {
                        results
                    }
                }
                .padding(.vertical, Design.Spacing.lg)
            }
        }
        .navigationTitle("Search")
        .navigationBarTitleDisplayMode(.inline)
        .searchable(
            text: $query,
            placement: .navigationBarDrawer(displayMode: .always),
            prompt: "What did we work out about ..."
        )
        .onChange(of: query) { _, newValue in
            schedule(newValue)
        }
        .onDisappear { searchTask?.cancel() }
    }

    // MARK: - Pieces

    private var prompt: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            Text("Every past conversation, indexed")
                .font(Design.Typography.callout)
                .foregroundStyle(Design.Colors.foreground)
            Text("Search is answered by the daemon's own index, so it costs nothing to try. Every word is tried first, then any of them, so a half-remembered phrase still finds the conversation.")
                .font(Design.Typography.caption)
                .foregroundStyle(Design.Colors.secondaryForeground)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, Design.Spacing.lg)
    }

    private var noResults: some View {
        Text("Nothing matched “\(query.trimmingCharacters(in: .whitespacesAndNewlines))”.")
            .font(Design.Typography.callout)
            .foregroundStyle(Design.Colors.secondaryForeground)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, Design.Spacing.lg)
    }

    private var results: some View {
        VStack(spacing: 0) {
            ForEach(hits) { hit in
                Button {
                    Task { await open(hit) }
                } label: {
                    VStack(alignment: .leading, spacing: Design.Spacing.xs) {
                        HStack(spacing: Design.Spacing.sm) {
                            Text(hit.title)
                                .font(Design.Typography.callout)
                                .foregroundStyle(Design.Colors.foreground)
                                .lineLimit(1)
                            Spacer(minLength: 0)
                            if opening == hit.conversation {
                                ProgressView().tint(Design.Colors.accent)
                            } else {
                                Text(count(hit.matches))
                                    .font(Design.Typography.caption)
                                    .foregroundStyle(Design.Colors.secondaryForeground)
                            }
                        }
                        Text(hit.snippet)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground)
                            .lineLimit(3)
                            .multilineTextAlignment(.leading)
                        Text(hit.createdAt)
                            .font(Design.Typography.caption)
                            .foregroundStyle(Design.Colors.secondaryForeground.opacity(0.7))
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, Design.Spacing.lg)
                    .padding(.vertical, Design.Spacing.sm)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(opening != nil)
                .accessibilityIdentifier("search.hit.\(hit.conversation)")

                Rectangle()
                    .fill(Design.Colors.border)
                    .frame(height: 1)
            }
        }
    }

    // MARK: - Wire

    private func count(_ matches: Int) -> String {
        matches == 1 ? "1 message" : "\(matches) messages"
    }

    /// Debounce: 250 ms after the last keystroke, and only then ask.
    private func schedule(_ text: String) {
        searchTask?.cancel()
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            hits = []
            errorMessage = nil
            isLoading = false
            return
        }
        searchTask = Task {
            try? await Task.sleep(for: .milliseconds(250))
            guard !Task.isCancelled else { return }
            await search(trimmed)
        }
    }

    private func erisClient() async -> ErisClient? {
        guard let eris = chatStore.heraldClient as? ErisHeraldClient else { return nil }
        return await eris.client()
    }

    private func search(_ text: String) async {
        guard let client = await erisClient() else {
            errorMessage = "No Eris daemon is paired."
            return
        }
        isLoading = true
        defer { isLoading = false }
        do {
            let page = try await client.searchTranscript(query: text, limit: 20)
            guard !Task.isCancelled else { return }
            hits = page.hits
            errorMessage = nil
        } catch {
            guard !Task.isCancelled else { return }
            errorMessage = (error as? ErisError)?.errorDescription ?? error.localizedDescription
        }
    }

    /// Open the conversation a hit belongs to.
    ///
    /// The daemon's conversation ids are UUIDs (v7), so a hit's id is the same id
    /// the chat surface takes. Loading it first means the transcript is in place
    /// before the tab switches, rather than switching to a conversation mid-load.
    private func open(_ hit: ErisSearchHit) async {
        guard opening == nil, let id = UUID(uuidString: hit.conversation) else { return }
        opening = hit.conversation
        defer { opening = nil }
        if let conversation = try? await chatStore.heraldClient.loadConversation(id: id) {
            chatStore.conversation = chatStore.mergeConversationMetadata(
                from: chatStore.conversation,
                into: conversation
            )
            chatStore.lastTokenUsage = conversation.latestUsage
        }
        router.selectedTab = .chat
    }
}
