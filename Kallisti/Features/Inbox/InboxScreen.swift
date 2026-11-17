import SwiftUI

struct InboxScreen: View {
    @Environment(InboxStore.self) private var inboxStore
    @Environment(TabRouter.self) private var router
    @State private var presentedNotification: InboxItem?

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
        .sheet(item: $presentedNotification) { item in
            NotificationDetailSheet(
                item: item,
                onPrimary: {
                    Task { await inboxStore.performPrimaryAction(for: item) }
                },
                onDismiss: {
                    Task { await inboxStore.dismiss(item) }
                },
                onSnooze: { until in
                    inboxStore.snooze(item, until: until)
                }
            )
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
                            // Build 68: notification-type items (no conversation
                            // reference) open the notification action window
                            // instead of dead-ending on a no-op submitAction.
                            if item.type != .approval,
                               item.payload?["conversationId"] == nil {
                                presentedNotification = item
                            } else {
                                Task { await inboxStore.performPrimaryAction(for: item) }
                            }
                        },
                        onSecondaryAction: {
                            Task { await inboxStore.dismiss(item) }
                        },
                        onOpenDetails: {
                            if item.type != .approval,
                               item.payload?["conversationId"] == nil {
                                presentedNotification = item
                            }
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
            description: Text("No new items from Kallisti. Check back later.")
                .foregroundStyle(Design.Colors.secondaryForeground)
        )
    }

    // MARK: - Toolbar

    @ToolbarContentBuilder
    private var toolbarContent: some ToolbarContent {
        ToolbarItem(placement: .topBarTrailing) {
            if inboxStore.unreadCount > 0 {
                Text("\\(inboxStore.unreadCount) new")
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
