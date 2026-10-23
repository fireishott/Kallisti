import Foundation

// MARK: - Turn identity on the wire
//
// Three values were failing to reach anywhere useful. Two of them were never
// sent, and the third was sent and then discarded.

// ---------------------------------------------------------------------------
// 1. Idempotency: the header that stops a retry becoming a second turn
// ---------------------------------------------------------------------------
//
// `POST /v1/turns` has read an `idempotency-key` header since the device surface
// landed, and backs it with a unique index:
//
//     const IDEMPOTENCY_HEADER: &str = "idempotency-key";
//     (crates/eris-host/src/device/handlers.rs:49)
//
//     CREATE UNIQUE INDEX idx_jobs_device_idempotency
//         ON jobs (device_id, idempotency_key)
//         WHERE idempotency_key IS NOT NULL;
//     (crates/eris-host/src/db.rs, migration 0004)
//
// A second `POST /v1/turns` carrying the same key does not queue a second turn.
// It answers `202` with the *same* job id. The client never sent the header, so
// every retry in `ChatStore.runAttemptLoop` queued a new turn, wrote a new
// question row and a new answer row, and the transcript grew a duplicate pair -
// which is exactly what the screenshots show.
//
// The daemon side of this is already correct and needs no change.

/// The daemon's header name for a turn's idempotency key.
///
/// Spelling read off the daemon, not invented. Header names are
/// case-insensitive per RFC 9110, so the lowercase `k` is belt and braces rather
/// than a gamble.
enum ErisTurnWire {
    /// The header carrying a turn's idempotency key.
    static let idempotencyHeader = "idempotency-key"

    /// The body field carrying the sender's own id for the message.
    static let clientMessageIDField = "client_message_id"

    /// A stable idempotency key for one send.
    ///
    /// Derived from the message's own client id rather than minted fresh per
    /// attempt. That is the whole point: a fresh key on each retry would queue
    /// one job per retry, which is the bug. The same message retried any number
    /// of times presents the same key and resolves to the same job.
    ///
    /// The prefix keeps it from being mistaken for a raw UUID in a log line and
    /// names the namespace the daemon's index is keyed on: one device, one key,
    /// one turn.
    static func idempotencyKey(for clientMessageID: UUID) -> String {
        "turn-\(clientMessageID.uuidString.lowercased())"
    }
}
