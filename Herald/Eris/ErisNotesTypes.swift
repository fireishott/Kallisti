import Foundation

// MARK: - Eris notes wire types
//
// Notes use notes.py's own envelope shapes (NOT the `success` envelope): the
// list is `{data:[...],meta:{count:N}}` and the detail is `{data:{...}}`.
// `PATCH` is optimistically-concurrency gated by the quoted `If-Match` revision.

/// One note, as `data` / `data[]` return it.
struct ErisNote: Decodable, Sendable, Identifiable {
    let id: String
    let userId: String
    let title: String
    let folderId: String?
    let pinned: Bool
    let revision: Int64
    let currentDrawingRevision: Int64
    let currentTextRevision: Int64
    let createdAt: String
    let updatedAt: String
    let deletedAt: String?
}

/// `GET /v1/notes` body.
struct ErisNotesListEnvelope: Decodable, Sendable {
    struct Meta: Decodable, Sendable {
        let count: Int
    }

    let data: [ErisNote]
    let meta: Meta
}

/// `POST /v1/notes`, `GET /v1/notes/{id}` and `PATCH /v1/notes/{id}` body.
struct ErisNoteEnvelope: Decodable, Sendable {
    let data: ErisNote
}

/// `POST /v1/notes` request body. `folderId`/`pinned` are optional and omitted
/// when nil (the daemon defaults them).
struct ErisNoteCreatePayload: Encodable, Sendable {
    let title: String
    let folderId: String?
    let pinned: Bool?

    init(title: String, folderId: String? = nil, pinned: Bool? = nil) {
        self.title = title
        self.folderId = folderId
        self.pinned = pinned
    }
}

/// `PATCH /v1/notes/{id}` request body. nil fields are omitted (no change).
struct ErisNotePatchPayload: Encodable, Sendable {
    let title: String?
    let folderId: String?
    let pinned: Bool?
}
