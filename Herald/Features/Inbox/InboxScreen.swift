import SwiftUI

struct InboxScreen: View {
    @Environment(InboxStore.self) private var inboxStore
    @Environment(TabRouter.self) private var router
    @State private var presentedNotification: InboxItem?
    @State private var isSelectionMode = false
    @State private var selectedIDs: Set<UUID> = []
    @State private var isBulkDismissing = false

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
        .safeAreaInset(edge: .bottom, spacing: 0) {
            if isSelectionMode {
                bulkActionBar
            }
        }
        .animation(.easeInOut(duration: 0.2), value: isSelectionMode)
        .onAppear {
            // Build 67: refresh every time the tab is shown so a response
            // that landed while the user was elsewhere appears immediately.
            Task { await inboxStore.loadInbox() }
        }
        .onChange(of: Set(inboxStore.items.map(\.id))) { _, valid in
            // Drop selections that no longer correspond to a visible item
            // (e.g. after a successful bulk dismiss removed rows).
            if !selectedIDs.isEmpty {
                selectedIDs.formIntersection(valid)
            }
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
                        },
                        selectionState: isSelectionMode
                            ? InboxItemRowSelection(id: item.id, isSelected: selectedIDs.contains(item.id))
                            : nil,
                        onToggleSelection: { toggleSelection(for: item.id) }
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
        ToolbarItem(placement: .topBarLeading) {
            if isSelectionMode {
                Button("Done") { exitSelectionMode() }
                    .font(Design.Typography.headline)
                    .foregroundStyle(Design.Brand.accent)
                    .accessibilityLabel("Exit selection mode")
            }
        }
        ToolbarItem(placement: .topBarTrailing) {
            if isSelectionMode {
                Button(allSelected ? "Deselect All" : "Select All") {
                    toggleSelectAll()
                }
                .font(Design.Typography.headline)
                .foregroundStyle(Design.Brand.accent)
                .disabled(inboxStore.items.isEmpty)
                .accessibilityLabel(allSelected ? "Deselect all inbox items" : "Select all inbox items")
            } else if inboxStore.unreadCount > 0 {
                Button("Edit") { enterSelectionMode() }
                    .font(Design.Typography.headline)
                    .foregroundStyle(Design.Brand.accent)
                    .accessibilityLabel("Enter selection mode")
            } else {
                Button("Edit") { enterSelectionMode() }
                    .font(Design.Typography.headline)
                    .foregroundStyle(Design.Colors.tertiaryForeground)
                    .disabled(inboxStore.items.isEmpty)
                    .accessibilityLabel("Enter selection mode")
            }
        }
    }

    // MARK: - Bulk action bar

    private var bulkActionBar: some View {
        HStack(spacing: Design.Spacing.sm) {
            Text("\(selectedIDs.count) selected")
                .brandEyebrow(Design.Colors.tertiaryForeground)

            Spacer()

            Button {
                Task { await performBulkDismiss() }
            } label: {
                HStack(spacing: Design.Spacing.xs) {
                    if isBulkDismissing {
                        ProgressView()
                            .progressViewStyle(.circular)
                            .tint(Design.Colors.background)
                    }
                    Text(isBulkDismissing ? "Dismissing..." : "Dismiss (\(selectedIDs.count))")
                        .brandEyebrow(Design.Colors.background)
                }
                .padding(.horizontal, Design.Spacing.md)
                .padding(.vertical, Design.Spacing.xs)
            }
            .background(selectedIDs.isEmpty ? Design.Colors.surface : Design.Brand.accent)
            .overlay(
                Capsule().stroke(Design.Colors.border, lineWidth: 1)
            )
            .clipShape(Capsule())
            .disabled(selectedIDs.isEmpty || isBulkDismissing)
            .accessibilityLabel("Dismiss \(selectedIDs.count) selected items")
        }
        .padding(.horizontal, Design.Spacing.md)
        .padding(.vertical, Design.Spacing.sm)
        .background(Design.Colors.backgroundRaised)
        .overlay(
            Rectangle()
                .fill(Design.Colors.divider)
                .frame(height: 0.5),
            alignment: .top
        )
    }

    // MARK: - Selection helpers

    private var allSelected: Bool {
        !inboxStore.items.isEmpty
            && selectedIDs.count == inboxStore.items.count
    }

    private func toggleSelection(for id: UUID) {
        if selectedIDs.contains(id) {
            selectedIDs.remove(id)
        } else {
            selectedIDs.insert(id)
        }
    }

    private func toggleSelectAll() {
        if allSelected {
            selectedIDs.removeAll()
        } else {
            selectedIDs = Set(inboxStore.items.map(\.id))
        }
    }

    private func enterSelectionMode() {
        selectedIDs.removeAll()
        isSelectionMode = true
    }

    private func exitSelectionMode() {
        isSelectionMode = false
        selectedIDs.removeAll()
    }

    private func performBulkDismiss() async {
        guard !selectedIDs.isEmpty else { return }
        let snapshot = inboxStore.items.filter { selectedIDs.contains($0.id) }
        isBulkDismissing = true
        defer {
            isBulkDismissing = false
            exitSelectionMode()
        }
        // Sequential dismiss. The store is @MainActor and each `dismiss` is
        // an independent network call; serial keeps mutations on the main
        // actor without region-checker trips, and the user already opted in
        // to a bulk action.
        for item in snapshot {
            await inboxStore.dismiss(item)
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