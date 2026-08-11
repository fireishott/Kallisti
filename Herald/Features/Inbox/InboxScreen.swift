import SwiftUI

struct InboxScreen: View {
    @Environment(InboxStore.self) private var inboxStore
    @Environment(TabRouter.self) private var router

    var body: some View {
        ZStack {
            Design.Colors.background
                .ignoresSafeArea()

            if inboxStore.items.isEmpty {
                emptyState
            } else {
                itemList
            }
        }
        .navigationTitle("Inbox")
        .toolbar { toolbarContent }
        .onAppear {
            // Build 67: refresh every time the tab is shown so a response
            // that landed while the user was elsewhere appears immediately.
            Task { await inboxStore.loadInbox() }
        }
    }

    // MARK: - List

    private var itemList: some View {
        ScrollView {
            LazyVStack(spacing: Design.Spacing.sm) {
                ForEach(inboxStore.items) { item in
                    InboxItemRow(
                        item: item,
                        onPrimaryAction: {
                            Task { await inboxStore.performPrimaryAction(for: item) }
                        },
                        onSecondaryAction: {
                            Task { await inboxStore.dismiss(item) }
                        },
                        onOpenDetails: {
                            // Inbox detail navigation deprecated — no-op
                        }
                    )
                }
            }
            .padding(.horizontal, Design.Spacing.md)
            .padding(.vertical, Design.Spacing.sm)
        }
        .redacted(reason: inboxStore.isLoading ? .placeholder : [])
        .modifier(ScrollEdgeEffectModifier())
    }

    // MARK: - Empty State

    private var emptyState: some View {
        ContentUnavailableView(
            "All Caught Up",
            systemImage: "tray",
            description: Text("No new items from Herald. Check back later.")
                .foregroundStyle(Design.Colors.secondaryForeground)
        )
    }

    // MARK: - Toolbar

    @ToolbarContentBuilder
    private var toolbarContent: some ToolbarContent {
        ToolbarItem(placement: .topBarTrailing) {
            if inboxStore.unreadCount > 0 {
                Text("\(inboxStore.unreadCount) new")
                    .brandEyebrow()
            }
        }
    }
}

private struct ScrollEdgeEffectModifier: ViewModifier {
    func body(content: Content) -> some View {
        if #available(iOS 26.0, *) {
            content.scrollEdgeEffectStyle(.soft, for: .top)
        } else {
            content
        }
    }
}
