import CarPlay
import os

/// Manages the conversation list shown on the CarPlay head unit.
/// Uses CPMessageListItem — tapping auto-invokes Siri for read/reply/compose.
/// No custom handler blocks needed — Siri manages all voice interaction.
@MainActor
final class CarPlayConversationListManager {
    private static let logger = Logger(subsystem: "net.fihonline.herald", category: "CarPlay")
    private static let maxConversations = 20

    private let interfaceController: CPInterfaceController
    private var observationTask: Task<Void, Never>?

    private var sessionStore: SessionListStore {
        AppContainer.sharedDefault().sessionListStore
    }

    private(set) var listTemplate: CPListTemplate?

    init(interfaceController: CPInterfaceController) {
        self.interfaceController = interfaceController
    }

    func configure() {
        refreshList()

        observationTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                self.refreshList()
                await withObservationTracking {
                    _ = self.sessionStore.pinnedSessions.count
                    _ = self.sessionStore.recentSessions.count
                } onChange: {}
            }
        }
    }

    func tearDown() {
        observationTask?.cancel()
        observationTask = nil
    }

    private func refreshList() {
        let allSessions = sessionStore.pinnedSessions + sessionStore.recentSessions
        let sessions = allSessions.prefix(Self.maxConversations)

        // CPMessageListItem: tapping auto-invokes Siri
        // - Has trailingText → Siri launches "read message" flow
        // - Has phoneOrEmailAddress → Siri launches "compose" flow
        // - Otherwise → Siri launches "reply" flow
        let items: [CPMessageListItem] = sessions.map { session in
            let leadingConfig = CPMessageListItemLeadingConfiguration(
                leadingItem: session.isPinned ? .pin : .none,
                leadingImage: nil,
                unread: false
            )
            let trailingConfig = CPMessageListItemTrailingConfiguration(
                trailingItem: .mute,
                trailingImage: nil
            )
            let item = CPMessageListItem(
                conversationIdentifier: session.id.uuidString,
                text: session.title,
                leadingConfiguration: leadingConfig,
                trailingConfiguration: trailingConfig,
                detailText: session.previewText.isEmpty ? nil : String(session.previewText.prefix(60)),
                trailingText: nil // If set, Siri reads messages; if nil, Siri offers compose
            )
            return item
        }

        // Section: Recent Conversations
        let conversationsSection = CPListSection(
            items: items,
            header: "Recent Conversations",
            sectionIndexTitle: nil
        )

        // Action: New Chat
        let newChatItem = CPListItem(
            text: "New Chat",
            detailText: "Start a new conversation with Herald",
            image: UIImage(systemName: "plus.bubble") ?? UIImage()
        )
        newChatItem.handler = { [weak self] _, completion in
            Task {
                await self?.sessionStore.createNewSession()
            }
            completion()
        }
        let actionsSection = CPListSection(items: [newChatItem], header: nil, sectionIndexTitle: nil)

        listTemplate = CPListTemplate(
            title: "Herald",
            sections: [actionsSection, conversationsSection]
        )
    }
}
