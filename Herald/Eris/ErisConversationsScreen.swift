import Observation
import SwiftUI

// MARK: - Conversations view model

/// The conversation list for one paired daemon.
///
/// Every state a request can leave this screen in is a property rather than an
/// implicit empty list: a page that has not loaded yet, a page that failed, a
/// page that loaded and is genuinely empty, and a page the daemon cut short. The
/// view renders each of those differently, so none of them can look like "there
/// are no conversations" when the truth is "the daemon did not answer".
@MainActor
@Observable
final class ErisConversationsModel {
    /// The rows, newest first, in the daemon's order.
    private(set) var conversations: [ErisConversationSummary] = []
    /// Project names, keyed by id, so a row shows a name and not a uuid.
    private(set) var projectNames: [String: String] = [:]
    /// Whether the daemon cut the page.
    private(set) var truncated = false
    /// Whether the first page is in flight.
    private(set) var isLoading = false
    /// The failure that stopped the list from loading at all.
    private(set) var failure: ErisFailureKind?
    /// The failure from a clear or a create, which leaves the list usable.
    private(set) var actionFailure: ErisFailureKind?
    /// Whether a create is in flight.
    private(set) var isCreating = false
    /// The row whose clear is in flight.
    private(set) var clearingID: String?
    /// What the last successful clear did, in one line.
    private(set) var notice: String?
    /// Whether a page has ever loaded, so a re-entry does not re-fetch in vain.
    private(set) var hasLoaded = false

    /// The transport. An actor, so nothing here blocks the main thread.
    private let client: ErisClient

    init(client: ErisClient) {
        self.client = client
    }

    // MARK: Loading

    /// Fetch the first page, unless one is already on screen.
    func loadIfNeeded() async {
        guard !hasLoaded else { return }
        await load()
    }

    /// Fetch the list, then resolve the project names it refers to.
    func load() async {
        guard !isLoading else { return }
        isLoading = true
        failure = nil
        defer { isLoading = false }
        do {
            let page = try await client.conversations()
            conversations = page.conversations
            truncated = page.truncated
            hasLoaded = true
            await loadProjectNames()
        } catch {
            failure = ErisFailureKind(error)
        }
    }

    /// Resolve `project_id` to the name a person reads.
    ///
    /// Best effort on purpose, and the only place this file ignores a failure: the
    /// list itself already loaded, so a name lookup that fails must not throw away
    /// rows that are on screen. A row whose project cannot be named says so.
    private func loadProjectNames() async {
        guard conversations.contains(where: { $0.projectID != nil }) else { return }
        do {
            let projects = try await client.projects()
            var names: [String: String] = [:]
            for project in projects where !project.id.isEmpty {
                names[project.id] = project.name
            }
            projectNames = names
        } catch {
            projectNames = [:]
        }
    }

    // MARK: Actions

    /// Start a conversation and put it at the top of the list.
    ///
    /// Returns the row to open, or `nil` when the daemon refused; the refusal is in
    /// `actionFailure`, so the caller does not have to reconstruct it.
    func createConversation() async -> ErisConversationSummary? {
        guard !isCreating else { return nil }
        isCreating = true
        actionFailure = nil
        notice = nil
        defer { isCreating = false }
        do {
            let created = try await client.createConversation()
            let row = ErisConversationSummary(
                id: created.id,
                title: created.title,
                updatedAt: "",
                projectID: nil,
                messageCount: 0
            )
            conversations.insert(row, at: 0)
            hasLoaded = true
            return row
        } catch {
            actionFailure = ErisFailureKind(error)
            return nil
        }
    }

    /// Clear one conversation's messages and say how many went.
    ///
    /// The row stays: the daemon's clear drops messages and keeps the
    /// conversation, so removing the row here would be a lie about the store.
    func clear(_ summary: ErisConversationSummary) async {
        guard clearingID == nil else { return }
        clearingID = summary.id
        actionFailure = nil
        notice = nil
        defer { clearingID = nil }
        do {
            let cleared = try await client.clearConversation(id: summary.id)
            notice = cleared == 1 ? "Cleared 1 message." : "Cleared \(cleared) messages."
            if let index = conversations.firstIndex(where: { $0.id == summary.id }) {
                let row = conversations[index]
                conversations[index] = ErisConversationSummary(
                    id: row.id,
                    title: row.title,
                    updatedAt: row.updatedAt,
                    projectID: row.projectID,
                    messageCount: 0
                )
            }
        } catch {
            actionFailure = ErisFailureKind(error)
        }
    }

    // MARK: Presentation

    /// The project label for a row, or `nil` when it is filed under nothing.
    func projectName(for summary: ErisConversationSummary) -> String? {
        guard let id = summary.projectID, !id.isEmpty else { return nil }
        let name = (projectNames[id] ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        if !name.isEmpty { return name }
        // The daemon holds a project this page could not name: show enough of the
        // id that the row is still identifiable rather than inventing a label.
        return String(id.prefix(8))
    }
}

// MARK: - Conversations screen

/// The conversation list, with a create action and a per-row clear.
struct ErisConversationsScreen: View {
    let store: ErisDaemonStore
    let model: ErisConversationsModel
    /// Open one conversation. The caller owns what "open" means; this screen does
    /// not push anything itself, so it cannot become a second navigation stack
    /// inside the surface.
    var onOpen: (ErisConversationSummary) -> Void
    /// Start a conversation and open it.
    var onCreate: (ErisConversationSummary) -> Void

    var body: some View {
        VStack(spacing: 0) {
            content
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(Design.Colors.background)
        .task { await model.loadIfNeeded() }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    Task {
                        if let row = await model.createConversation() {
                            onCreate(row)
                        }
                    }
                } label: {
                    Label("New conversation", systemImage: "square.and.pencil")
                        .font(Design.Typography.footnote)
                }
                .disabled(model.isCreating)
                .accessibilityIdentifier("eris.newConversation")
            }
        }
    }

    // MARK: States

    @ViewBuilder
    private var content: some View {
        if model.isLoading, model.conversations.isEmpty {
            ErisLoadingView(label: "Loading conversations…")
            Spacer(minLength: 0)
        } else if let failure = model.failure, model.conversations.isEmpty {
            VStack(alignment: .leading, spacing: Design.Spacing.sm) {
                ErisFailureView(
                    kind: failure,
                    onRetry: { Task { await model.load() } },
                    onPairAgain: { Task { await store.forget() } }
                )
            }
            .padding(Design.Spacing.md)
            Spacer(minLength: 0)
        } else if model.conversations.isEmpty {
            emptyState
        } else {
            list
        }
    }

    private var emptyState: some View {
        VStack(alignment: .leading, spacing: Design.Spacing.sm) {
            Text("No conversations yet")
                .font(Design.Typography.sectionTitle)
                .foregroundStyle(Design.Colors.foreground)
            Text("A conversation keeps a turn's question, its answer, its reasoning and every tool it ran, so a later turn can pick it up.")
                .font(Design.Typography.footnote)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .fixedSize(horizontal: false, vertical: true)
            Button {
                Task {
                    if let row = await model.createConversation() {
                        onCreate(row)
                    }
                }
            } label: {
                Text(model.isCreating ? "Starting…" : "New conversation")
                    .font(Design.Typography.headline)
                    .foregroundStyle(Design.Colors.background)
                    .padding(.horizontal, Design.Spacing.lg)
                    .frame(minHeight: Design.Size.minTapTarget)
                    .background(Design.Brand.accent)
                    .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.sm))
            }
            .buttonStyle(.plain)
            .disabled(model.isCreating)
            .accessibilityIdentifier("eris.startConversation")

            if let failure = model.actionFailure {
                ErisFailureView(
                    kind: failure,
                    onRetry: nil,
                    onPairAgain: failure.needsPairing ? { Task { await store.forget() } } : nil
                )
            }
        }
        .padding(Design.Spacing.md)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityIdentifier("eris.conversationsEmpty")
    }

    private var list: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: Design.Spacing.xs) {
                if model.truncated {
                    Text("Showing the newest conversations only: the daemon bounds the page.")
                        .font(Design.Typography.caption2)
                        .foregroundStyle(Design.Colors.tertiaryForeground)
                        .padding(.horizontal, Design.Spacing.md)
                        .padding(.top, Design.Spacing.xs)
                }
                if let notice = model.notice {
                    Text(notice)
                        .font(Design.Typography.footnote)
                        .foregroundStyle(Design.Colors.success)
                        .padding(.horizontal, Design.Spacing.md)
                        .accessibilityIdentifier("eris.conversationsNotice")
                }
                if let failure = model.actionFailure {
                    ErisFailureView(
                        kind: failure,
                        onRetry: nil,
                        onPairAgain: failure.needsPairing ? { Task { await store.forget() } } : nil
                    )
                    .padding(.horizontal, Design.Spacing.md)
                }
                ForEach(model.conversations) { summary in
                    row(summary)
                }
                Color.clear.frame(height: Design.Spacing.lg)
            }
            .padding(.vertical, Design.Spacing.xs)
        }
        .refreshable { await model.load() }
    }

    private func row(_ summary: ErisConversationSummary) -> some View {
        HStack(alignment: .top, spacing: Design.Spacing.sm) {
            Button {
                onOpen(summary)
            } label: {
                VStack(alignment: .leading, spacing: Design.Spacing.xxxs) {
                    Text(summary.displayTitle)
                        .font(Design.Typography.headline)
                        .foregroundStyle(Design.Colors.foreground)
                        .multilineTextAlignment(.leading)
                        .lineLimit(2)
                    HStack(spacing: Design.Spacing.xxs) {
                        Text(summary.messageCountText)
                            .font(Design.Typography.caption2)
                            .foregroundStyle(Design.Colors.tertiaryForeground)
                        if let project = model.projectName(for: summary) {
                            Text("·")
                                .font(Design.Typography.caption2)
                                .foregroundStyle(Design.Colors.tertiaryForeground)
                            Text(project)
                                .font(Design.Typography.caption2)
                                .foregroundStyle(Design.Colors.accent)
                                .lineLimit(1)
                        }
                    }
                    if !summary.updatedAt.isEmpty {
                        Text(summary.updatedAt)
                            .font(Design.Typography.caption2)
                            .foregroundStyle(Design.Colors.tertiaryForeground)
                            .lineLimit(1)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityIdentifier("eris.conversation.\(summary.id)")

            if model.clearingID == summary.id {
                ProgressView().tint(Design.Brand.accent)
                    .frame(width: Design.Size.iconSmall, height: Design.Size.iconSmall)
            } else {
                Menu {
                    Button("Clear messages", role: .destructive) {
                        Task { await model.clear(summary) }
                    }
                    Text("The conversation stays: clearing drops its messages.")
                } label: {
                    Image(systemName: "ellipsis")
                        .font(.system(size: Design.Size.iconSmall, weight: .semibold))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                        .frame(width: Design.Size.minTapTarget, height: Design.Size.minTapTarget)
                        .contentShape(Rectangle())
                }
                .accessibilityLabel("Actions for \(summary.displayTitle)")
                .accessibilityIdentifier("eris.conversationActions.\(summary.id)")
            }
        }
        .padding(.horizontal, Design.Spacing.md)
        .padding(.vertical, Design.Spacing.xs)
        .background(Design.Colors.surface)
        .clipShape(RoundedRectangle(cornerRadius: Design.CornerRadius.md))
        .padding(.horizontal, Design.Spacing.md)
    }
}
