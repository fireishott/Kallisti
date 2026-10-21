import SwiftUI

// MARK: - Inbox model

/// The device's inbox over `GET /v1/inbox` + `POST /v1/inbox/{id}/action`.
@MainActor
@Observable
final class ErisInboxModel {
    private(set) var items: [ErisInboxItem] = []
    private(set) var isLoading = false
    private(set) var hasLoaded = false
    private(set) var failure: String?
    private(set) var actingOnIDs: Set<String> = []

    private let client: ErisClient

    init(client: ErisClient) {
        self.client = client
    }

    func loadIfNeeded() async {
        guard !hasLoaded else { return }
        await load()
    }

    func load() async {
        isLoading = true
        failure = nil
        defer { isLoading = false; hasLoaded = true }
        do {
            items = try await client.inbox().data.items
        } catch {
            items = []
            failure = (error as? ErisError)?.errorDescription ?? error.localizedDescription
        }
    }

    /// Perform one action on an item, then refresh the list.
    func performAction(_ action: String, on item: ErisInboxItem) async {
        guard !actingOnIDs.contains(item.id) else { return }
        actingOnIDs.insert(item.id)
        failure = nil
        defer { actingOnIDs.remove(item.id) }
        do {
            _ = try await client.inboxAction(itemID: item.id, action: action)
            await load()
        } catch {
            failure = (error as? ErisError)?.errorDescription ?? error.localizedDescription
        }
    }

    /// The action id the primary action title maps to, or nil.
    static func primaryActionID(for item: ErisInboxItem) -> String? {
        guard let title = item.primaryActionTitle, !title.isEmpty else { return nil }
        return item.kind == "approval" ? "approve" : "ack"
    }
}

// MARK: - Inbox screen

/// The inbox tray: items and their actions, Kallisti-themed.
struct ErisInboxScreen: View {
    let model: ErisInboxModel

    var body: some View {
        Group {
            if model.isLoading, !model.hasLoaded {
                ErisLoadingView(label: "Reading your inbox…")
            } else if let failure = model.failure, model.items.isEmpty {
                failureView(failure)
            } else if model.items.isEmpty {
                emptyView
            } else {
                itemList
            }
        }
        .background(Design.Colors.background)
        .navigationTitle("Inbox")
        .navigationBarTitleDisplayMode(.inline)
        .task { await model.loadIfNeeded() }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    Task { await model.load() }
                } label: {
                    Label("Reload", systemImage: "arrow.clockwise")
                        .font(Design.Typography.footnote)
                }
                .disabled(model.isLoading)
            }
        }
    }

    private var itemList: some View {
        List {
            ForEach(model.items) { item in
                inboxRow(item)
            }
        }
        .listStyle(.plain)
        .scrollContentBackground(.hidden)
    }

    private func inboxRow(_ item: ErisInboxItem) -> some View {
        VStack(alignment: .leading, spacing: Design.Spacing.xs) {
            HStack {
                Text(item.title)
                    .font(Design.Typography.headline)
                    .foregroundStyle(Design.Colors.foreground)
                Spacer()
                Text(item.status)
                    .font(Design.Typography.caption)
                    .foregroundStyle(Design.Colors.tertiaryForeground)
            }
            if !item.body.isEmpty {
                Text(item.body)
                    .font(Design.Typography.body)
                    .foregroundStyle(Design.Colors.secondaryForeground)
                    .fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: Design.Spacing.sm) {
                if let actionID = ErisInboxModel.primaryActionID(for: item),
                   let title = item.primaryActionTitle {
                    Button(title) {
                        Task { await model.performAction(actionID, on: item) }
                    }
                    .font(Design.Typography.footnote)
                    .buttonStyle(.borderedProminent)
                    .tint(Design.Brand.accent)
                }
                if let secondaryTitle = item.secondaryActionTitle {
                    Button(secondaryTitle) {
                        Task { await model.performAction("dismiss", on: item) }
                    }
                    .font(Design.Typography.footnote)
                    .buttonStyle(.bordered)
                    .tint(Design.Colors.secondaryForeground)
                }
            }
            .disabled(model.actingOnIDs.contains(item.id))
        }
        .padding(.vertical, Design.Spacing.xs)
    }

    private var emptyView: some View {
        VStack(spacing: Design.Spacing.md) {
            Image(systemName: "tray")
                .font(.system(size: 40))
                .foregroundStyle(Design.Colors.tertiaryForeground)
            Text("No inbox items")
                .font(Design.Typography.headline)
                .foregroundStyle(Design.Colors.foreground)
            Text("Completed answers and notifications will land here.")
                .font(Design.Typography.footnote)
                .foregroundStyle(Design.Colors.secondaryForeground)
                .multilineTextAlignment(.center)
        }
        .padding(Design.Spacing.xl)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func failureView(_ message: String) -> some View {
        VStack(spacing: Design.Spacing.md) {
            ErisFailureView(
                kind: .other(message: message, code: nil),
                onRetry: { Task { await model.load() } },
                onPairAgain: nil
            )
        }
        .padding(Design.Spacing.xl)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
