import Foundation

// MARK: - Eris inbox service
//
// `InboxServiceProtocol` backed by `GET /v1/inbox` and `POST /v1/inbox/{id}/
// action`. The `accessToken` parameter is ignored: Eris authenticates through
// its own bearer (the keychain pair), not the legacy relay token.

@MainActor
final class ErisInboxService: InboxServiceProtocol {
    private let clientProvider: @MainActor () async -> ErisClient?

    init(clientProvider: @escaping @MainActor () async -> ErisClient?) {
        self.clientProvider = clientProvider
    }

    func fetchInbox(accessToken: String?) async throws -> [InboxItem] {
        guard let client = await clientProvider() else {
            throw ErisHeraldError.notConnected
        }
        let envelope = try await client.inbox()
        return envelope.data.items.map { Self.item(from: $0) }
    }

    func submitAction(
        itemID: UUID,
        actionID: String,
        accessToken: String?
    ) async throws -> InboxActionResult {
        guard let client = await clientProvider() else {
            throw ErisHeraldError.notConnected
        }
        // The Eris item id is a UUID string, so `serverID` round-trips through
        // its own uuidString (see `item(from:)`).
        let envelope = try await client.inboxAction(itemID: itemID.uuidString.lowercased(), action: actionID)
        let data = envelope.data
        return InboxActionResult(
            itemID: UUID(uuidString: data.itemID) ?? itemID,
            actionID: data.actionID,
            status: Self.status(from: data.status),
            completedAt: ErisTimestamp.date(from: data.completedAt) ?? .now
        )
    }

    // MARK: Mapping

    static func item(from e: ErisInboxItem) -> InboxItem {
        let serverID = UUID(uuidString: e.id)
        return InboxItem(
            id: serverID ?? UUID(),
            serverID: serverID,
            type: type(from: e.kind),
            title: e.title,
            body: e.body,
            timestamp: ErisTimestamp.date(from: e.createdAt) ?? .now,
            isRead: e.status != "pending",
            isActionable: e.status == "pending",
            status: status(from: e.status),
            priority: priority(from: e.priority),
            primaryAction: e.primaryActionTitle.map {
                InboxActionDescriptor(id: primaryActionID(kind: e.kind, title: $0), title: $0)
            },
            secondaryAction: e.secondaryActionTitle.map {
                InboxActionDescriptor(id: "dismiss", title: $0, isDestructive: true)
            }
        )
    }

    static func primaryActionID(kind: String, title: String) -> String {
        kind == "approval" ? "approve" : "ack"
    }

    static func type(from kind: String) -> InboxItemType {
        switch kind {
        case "approval": return .approval
        case "reminder": return .reminder
        case "suggestion": return .suggestion
        case "alert": return .alert
        default: return .notification
        }
    }

    static func status(from raw: String) -> InboxItemStatus {
        switch raw {
        case "completed": return .completed
        case "dismissed": return .dismissed
        case "opened", "read", "acknowledged": return .opened
        default: return .pending
        }
    }

    static func priority(from raw: String) -> InboxItemPriority {
        switch raw {
        case "low": return .low
        case "high": return .high
        case "urgent": return .urgent
        default: return .normal
        }
    }
}
