//! Session event log. Skeleton only in Wave 0; behaviour lands in Wave 1 (P2).
//!
//! The layout is fixed now because it is a locked decision, not an
//! implementation detail:
//!
//! - the live WAL is a plain, uncompressed, one-object-per-line UTF-8 file at
//!   `sessions/<session_id>/wal.jsonl`, fsynced before an ack;
//! - sealed segments are immutable `session.v1.jsonl.zst` files, and a WAL is
//!   truncated only after its segment is written and fsynced;
//! - a torn final WAL line is truncated to the last valid envelope and recorded
//!   as a `recovery.truncated` event; a zstd decode error in a sealed segment is
//!   an error, never a trim (locked amendment 7).

/// File name of the live, uncompressed write-ahead log.
pub const WAL_FILE_NAME: &str = "wal.jsonl";

/// Suffix of a sealed, compressed segment.
pub const SEGMENT_SUFFIX: &str = ".jsonl.zst";

/// Name of the per-session advisory lock file.
pub const LOCK_FILE_NAME: &str = ".lock";

/// Directory holding one session's log, relative to the state root.
///
/// Kept as a pure string function so no filesystem access leaks into a
/// platform-neutral core.
#[must_use]
pub fn session_dir(session_id: &core_domain::SessionId) -> String {
    format!("sessions/{session_id}")
}
