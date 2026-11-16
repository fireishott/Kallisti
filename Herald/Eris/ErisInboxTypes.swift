import Foundation

// MARK: - Eris inbox wire types
//
// The inbox routes use the legacy `success` envelope: `{data, meta:{requestId,
// timestamp}}`. `data` carries `{items:[...]}` for the list and the settled
// `itemID`/`actionID`/`status`/`completedAt` row for an action.

/// One inbox item, as `data.items[]` returns it.
struct ErisInboxItem: Decodable, Sendable, Identifiable {
    let id: String
    let kind: String
    let title: String
    let body: String
    let priority: String
    let status: String
    let createdAt: String
    let primaryActionTitle: String?
    let secondaryActionTitle: String?
}

/// `GET /v1/inbox` body.
struct ErisInboxEnvelope: Decodable, Sendable {
    struct Data: Decodable, Sendable {
        let items: [ErisInboxItem]
    }
    struct Meta: Decodable, Sendable {
        let requestId: String
        let timestamp: String
    }

    let data: Data
    let meta: Meta
}

/// `POST /v1/inbox/{item_id}/action` request body.
struct ErisInboxActionPayload: Encodable, Sendable {
    let actionId: String
}

/// `POST /v1/inbox/{item_id}/action` response body.
struct ErisInboxActionEnvelope: Decodable, Sendable {
    struct Meta: Decodable, Sendable {
        let requestId: String
        let timestamp: String
    }

    let data: ErisInboxActionResult
    let meta: Meta
}

/// The settled action row (`data` of the action envelope).
struct ErisInboxActionResult: Decodable, Sendable {
    let itemID: String
    let actionID: String
    let status: String
    let completedAt: String
}

// MARK: - Talk readiness

/// `GET /v1/talk/readiness` body. The voice lane is a placeholder: `ready` is
/// `false` until the backend ships a real route.
struct ErisTalkReadiness: Decodable, Sendable {
    let ready: Bool
    let reason: String?
}
