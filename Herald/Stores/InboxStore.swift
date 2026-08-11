import Foundation

@MainActor
@Observable
final class InboxStore {
    var items: [InboxItem] = []
    var isLoading = false
    var lastErrorMessage: String?

    private let inboxService: any InboxServiceProtocol
    private let persistence: any AppPersistenceStoreProtocol
    private let sessionStore: AppSessionStore
    private let allowDemoFallback: Bool
    private let isPairedProvider: @MainActor () -> Bool
    private var localState: InboxLocalState {
        didSet { persistence.saveInboxState(localState) }
    }

    /// Called when the user taps "Open" on a notification-type inbox item
    /// that references a conversation. The handler receives the conversation
    /// UUID and should navigate the UI to that chat.
    var onOpenConversation: (@MainActor (UUID) -> Void)?

    /// Called when the user taps "Open" on a notification-type inbox item
    /// that does NOT reference a conversation (test push, system alert).
    /// The handler receives the item and should present a notification
    /// action window (full body, action buttons, dismiss, snooze).
    var onOpenNotification: (@MainActor (InboxItem) -> Void)?

    init(
        inboxService: any InboxServiceProtocol,
        persistence: any AppPersistenceStoreProtocol,
        sessionStore: AppSessionStore,
        allowDemoFallback: Bool = true,
        isPairedProvider: @escaping @MainActor () -> Bool = { false }
    ) {
        self.inboxService = inboxService
        self.persistence = persistence
        self.sessionStore = sessionStore
        self.allowDemoFallback = allowDemoFallback
        self.isPairedProvider = isPairedProvider
        self.localState = persistence.loadInboxState()
    }

    var unreadCount: Int {
        visibleItems.filter { !$0.isRead }.count
    }

    /// Items with snoozed items filtered out.
    private var visibleItems: [InboxItem] {
        let now = Date()
        return items.filter { item in
            guard let until = localState.snoozedItemIDs[item.stableIdentifier] else { return true }
            return until <= now
        }
    }

    /// Items currently hidden by snooze (for potential "undo" or display).
    var snoozedCount: Int {
        let now = Date()
        return items.filter { item in
            guard let until = localState.snoozedItemIDs[item.stableIdentifier] else { return false }
            return until > now
        }.count
    }

    /// True if the item is currently snoozed (hidden) until a future date.
    func isSnoozed(_ item: InboxItem) -> Bool {
        guard let until = localState.snoozedItemIDs[item.stableIdentifier] else { return false }
        return until > Date()
    }

    func loadInbox(force: Bool = false) async {
        if isLoading || (!force && !items.isEmpty) { return }

        isLoading = true
        lastErrorMessage = nil
        defer { isLoading = false }

        do {
            let token = await sessionStore.currentAccessToken()
            let fetchedItems = try await inboxService.fetchInbox(accessToken: token)
            items = applyLocalState(to: fetchedItems)
            pruneExpiredSnoozes()
        } catch {
            lastErrorMessage = error.localizedDescription
            // Never show demo data for paired users — they have a real relay
            // and an empty inbox is preferable to fake items that look like
            // the user's old chats.
            let shouldShowDemo = allowDemoFallback && !isPairedProvider()
            items = shouldShowDemo ? applyLocalState(to: DemoData.sampleInboxItems) : []
        }
    }

    func performPrimaryAction(for item: InboxItem) async {
        // For notification/alert/reminder/suggestion items that reference a
        // conversation, navigate to the chat instead of submitting an action.
        if item.type != .approval, let convIdString = item.payload?["conversationId"],
           let convId = UUID(uuidString: convIdString) {
            // Mark as read but don't submit an action to the relay
            if let idx = items.firstIndex(where: { $0.id == item.id }) {
                items[idx].isRead = true
                items[idx].status = .opened
                items[idx].isActionable = false
                localState.readItemIDs.insert(item.stableIdentifier)
            }
            onOpenConversation?(convId)
            return
        }

        // Notification-type item with no conversation reference: present the
        // notification action window (full body, actions, dismiss, snooze).
        // Build 68: previously this fell through to submitAction("open")
        // which the server treats as a no-op, so Open appeared dead.
        if item.type != .approval {
            if let convIdString = item.payload?["conversationId"],
               let convId = UUID(uuidString: convIdString) {
                // conversation-reference case handled above
            } else {
                onOpenNotification?(item)
                return
            }
        }

        let actionID = item.primaryAction?.id ?? "approve"
        await submitAction(for: item, actionID: actionID)
    }

    /// Snooze a notification item: hide it until `until`, then it reappears
    /// actionable. Local-only; the server item is untouched.
    func snooze(_ item: InboxItem, until: Date) {
        localState.snoozedItemIDs[item.stableIdentifier] = until
        if let idx = items.firstIndex(where: { $0.id == item.id }) {
            items[idx].isRead = true
            items[idx].isActionable = false
        }
    }

    /// Bring a snoozed item back immediately (undo snooze).
    func unsnooze(_ item: InboxItem) {
        localState.snoozedItemIDs.removeValue(forKey: item.stableIdentifier)
        if let idx = items.firstIndex(where: { $0.id == item.id }) {
            items[idx].isRead = false
            items[idx].isActionable = true
            items[idx].status = .pending
        }
    }

    /// Drop snooze entries whose time has passed.
    private func pruneExpiredSnoozes() {
        let now = Date()
        let expired = localState.snoozedItemIDs.filter { $0.value <= now }
        for (id, _) in expired {
            localState.snoozedItemIDs.removeValue(forKey: id)
        }
    }

    func dismiss(_ item: InboxItem) async {
        localState.snoozedItemIDs.removeValue(forKey: item.stableIdentifier)
        await submitAction(for: item, actionID: item.secondaryAction?.id ?? "dismiss")
    }

    private func submitAction(for item: InboxItem, actionID: String) async {
        do {
            let token = await sessionStore.currentAccessToken()
            let targetID = item.serverID ?? item.id
            let result = try await inboxService.submitAction(
                itemID: targetID,
                actionID: actionID,
                accessToken: token
            )

            apply(result: result, to: item)
        } catch {
            lastErrorMessage = error.localizedDescription
            applyLocalAction(actionID, to: item)
        }
    }

    private func apply(result: InboxActionResult, to item: InboxItem) {
        if let index = items.firstIndex(where: { $0.id == item.id }) {
            items[index].isRead = true
            items[index].status = result.status
            items[index].isActionable = result.status == .pending
        }

        updateLocalState(for: item, actionID: result.actionID)
    }

    private func applyLocalAction(_ actionID: String, to item: InboxItem) {
        if let index = items.firstIndex(where: { $0.id == item.id }) {
            items[index].isRead = true
            items[index].status = actionID == "dismiss" ? .dismissed : .completed
            items[index].isActionable = false
        }

        updateLocalState(for: item, actionID: actionID)
    }

    private func updateLocalState(for item: InboxItem, actionID: String) {
        localState.readItemIDs.insert(item.stableIdentifier)
        if actionID == "dismiss" {
            localState.dismissedItemIDs.insert(item.stableIdentifier)
            items.removeAll { $0.id == item.id }
        }
    }

    private func applyLocalState(to items: [InboxItem]) -> [InboxItem] {
        items.compactMap { item in
            guard !localState.dismissedItemIDs.contains(item.stableIdentifier) else { return nil }
            // Snoozed items stay hidden until their time passes.
            if let until = localState.snoozedItemIDs[item.stableIdentifier], until > Date() {
                return nil
            }

            var adjustedItem = item
            if localState.readItemIDs.contains(item.stableIdentifier) {
                adjustedItem.isRead = true
                adjustedItem.status = adjustedItem.status == .pending ? .opened : adjustedItem.status
                adjustedItem.isActionable = adjustedItem.status == .pending
            }
            return adjustedItem
        }
    }

    func reset() {
        items = []
        lastErrorMessage = nil
        localState = InboxLocalState()
        persistence.clearInboxState()
    }

    /// Snooze presets for the detail sheet.
    static let snoozeOptions: [(title: String, interval: TimeInterval)] = [
        ("15 minutes", 15 * 60),
        ("1 hour", 60 * 60),
        ("3 hours", 3 * 60 * 60),
        ("Tomorrow", 24 * 60 * 60),
    ]
}
