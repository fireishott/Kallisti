//! Session event log: an append-only, hash-chained, crash-safe history.
//!
//! The layout is a locked decision, not an implementation detail:
//!
//! - the live WAL is a plain, uncompressed, one-object-per-line UTF-8 file at
//!   `sessions/<session_id>/wal.jsonl`, fsynced before an ack. It is **never**
//!   compressed, because a compressed append target cannot be appended to and
//!   truncated safely;
//! - sealed segments are immutable, compressed `<name>.jsonl.zst` files. A WAL
//!   is truncated only *after* its segment is written and fsynced, so the
//!   crash window between the two steps replays without loss or duplication;
//! - a torn final WAL line is truncated to the last valid envelope and recorded
//!   as a `recovery.truncated` event; a zstd decode error in a sealed segment is
//!   an error, never a trim (locked amendment 7). The distinction is the whole
//!   point: the WAL is a lossy recording medium at its tail, a sealed segment is
//!   not;
//! - one writer per session, enforced by an `fs4` advisory lock on
//!   `sessions/<id>/.lock`. A second writer gets [`LogError::AlreadyLocked`].
//!
//! Every envelope carries its own timestamp and a hash over a canonical field
//! order ([`envelope`]), so a replay recomputes the same bytes it wrote and
//! detects any edit, reordering, or dropped envelope in the middle.
//!
//! # Writing, sealing, and the crash window
//!
//! ```text
//! append  -> wal.jsonl (+fsync)                    ack to the caller
//! seal    -> session.v1.<first>-<last>.jsonl.zst   written + fsynced
//!         -> wal.jsonl truncated                    only after the segment is durable
//! ```
//!
//! A process that dies between the last two steps leaves the same envelopes in
//! both places. [`recover::recover`] keeps the sealed copy, drops the WAL prefix
//! a segment already covers, and truncates the WAL itself: idempotent, no
//! duplicate `seq`, and a second open finishes the interrupted seal.

pub mod envelope;
pub mod lock;
pub mod recover;
pub mod segment;
pub mod wal;

pub use envelope::{Envelope, SCHEMA_VERSION};
pub use lock::SessionLock;
pub use recover::Recovery;
pub use segment::SealedSegment;

use std::fs::File;
use std::path::{Path, PathBuf};

/// File name of the live, uncompressed write-ahead log.
pub const WAL_FILE_NAME: &str = "wal.jsonl";

/// Suffix of a sealed, compressed segment.
pub const SEGMENT_SUFFIX: &str = ".jsonl.zst";

/// Name of the per-session advisory lock file.
pub const LOCK_FILE_NAME: &str = ".lock";

/// zstd level used for sealed segments. Sealing is a background durability
/// step, so a mid-range level keeps it cheap; it is never on the append path.
pub const SEGMENT_COMPRESSION_LEVEL: i32 = 3;

/// Directory holding one session's log, relative to the state root.
///
/// Kept as a pure string function so no filesystem access leaks into a
/// platform-neutral core.
#[must_use]
pub fn session_dir(session_id: &core_domain::SessionId) -> String {
    format!("sessions/{session_id}")
}

/// Absolute directory holding one session's log under `root`.
#[must_use]
pub fn session_path(root: &Path, session_id: &core_domain::SessionId) -> PathBuf {
    root.join("sessions").join(session_id.to_string())
}

/// A seam invoked at the single dangerous point of the seal protocol: after the
/// segment is durable, before the WAL is truncated.
///
/// Production always uses [`NoFault`]. The crash-window test injects a hook that
/// kills the process there, which is the only way to prove that step order -
/// rather than a lock, a flag, or a retry - is what makes the window safe.
pub trait SealFault {
    /// Called with the path of the segment that is already on disk.
    fn before_wal_truncate(&self, segment: &Path);
}

/// The production [`SealFault`]: never fires.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoFault;

impl SealFault for NoFault {
    fn before_wal_truncate(&self, _segment: &Path) {}
}

/// Everything the log can refuse to do.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    /// A filesystem call failed, with the path that failed.
    #[error("log i/o failed at {path}: {source}")]
    Io {
        /// Path being touched.
        path: PathBuf,
        /// Underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// An envelope could not be encoded.
    #[error("envelope serialization failed: {source}")]
    Serialize {
        /// Underlying failure.
        #[source]
        source: serde_json::Error,
    },

    /// A WAL holds an incomplete or unparseable line that is *not* the final
    /// one. Data after a broken line cannot be trusted, so this is fatal rather
    /// than trimmable.
    #[error("wal is corrupt at byte {offset}: a broken line is followed by more data")]
    CorruptWal {
        /// Byte offset of the first broken line.
        offset: u64,
    },

    /// A sealed segment did not parse as JSONL. Sealed segments are immutable,
    /// so this is always an error, never a trim.
    #[error("sealed segment {path} is not valid jsonl")]
    CorruptSegment {
        /// Offending segment.
        path: PathBuf,
    },

    /// A sealed segment failed to decompress. Hard error: trimming a sealed
    /// segment would silently discard acknowledged history.
    #[error("sealed segment {path} failed to decompress: {source}")]
    SegmentDecode {
        /// Offending segment.
        path: PathBuf,
        /// Underlying zstd failure.
        #[source]
        source: std::io::Error,
    },

    /// A sealed segment could not be compressed.
    #[error("sealed segment {path} failed to compress: {source}")]
    SegmentEncode {
        /// Target segment.
        path: PathBuf,
        /// Underlying zstd failure.
        #[source]
        source: std::io::Error,
    },

    /// The next expected sequence number did not match.
    #[error("event sequence gap: expected {expected}, found {found}")]
    SeqGap {
        /// Sequence number the chain requires.
        expected: u64,
        /// Sequence number actually found.
        found: u64,
    },

    /// `prev_hash` did not match the previous envelope's hash.
    #[error("hash chain is broken at seq {seq}: prev_hash does not match")]
    BrokenChain {
        /// Sequence number of the offending envelope.
        seq: u64,
    },

    /// A stored hash does not match the hash recomputed from its fields.
    #[error("envelope hash mismatch at seq {seq}")]
    HashMismatch {
        /// Sequence number of the offending envelope.
        seq: u64,
    },

    /// The envelope schema version is not one this build understands.
    #[error("unsupported envelope schema version {found}, expected {expected}")]
    UnsupportedSchema {
        /// Version found.
        found: u16,
        /// Version understood.
        expected: u16,
    },

    /// An envelope belongs to a different session than the one being replayed.
    #[error("envelope at seq {seq} belongs to session {found}, expected {expected}")]
    SessionMismatch {
        /// Sequence number of the offending envelope.
        seq: u64,
        /// Session the log directory is for.
        expected: String,
        /// Session the envelope claims.
        found: String,
    },

    /// Another writer holds the session lock.
    #[error("session log {path} is locked by another writer")]
    AlreadyLocked {
        /// Lock file that is held.
        path: PathBuf,
    },

    /// The WAL has a torn tail while sealing. Recovery must run first: sealing
    /// would otherwise either compress junk or silently drop acknowledged bytes.
    #[error("wal has a {bytes}-byte torn tail; recovery must run before sealing")]
    TornWal {
        /// Bytes of torn tail observed.
        bytes: u64,
    },

    /// A WAL envelope does not match the chain the log already validated.
    #[error("wal entry at seq {seq} does not match the validated log")]
    WalNotValidated {
        /// Sequence number of the offending envelope.
        seq: u64,
    },
}

impl LogError {
    /// Build an [`LogError::Io`] with path context.
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}

/// fsync a directory so a `rename` or `create` inside it is durable.
///
/// Opening a directory read-only and calling `sync_all` is the portable-enough
/// way to do this on Linux, which is the only target of this wave.
pub(crate) fn fsync_dir(dir: &Path) -> Result<(), LogError> {
    let handle = File::open(dir).map_err(|source| LogError::io(dir, source))?;
    handle
        .sync_all()
        .map_err(|source| LogError::io(dir, source))
}

/// One open, exclusively locked session log.
///
/// Opening recovers the session (torn-tail truncation, interrupted-seal
/// cleanup) and then holds the advisory lock until the value is dropped.
///
/// The log does not keep decoded history in memory: it tracks only the chain
/// head, and [`SessionLog::history`] re-reads and re-validates on demand.
#[derive(Debug)]
pub struct SessionLog {
    session_id: core_domain::SessionId,
    dir: PathBuf,
    lock: SessionLock,
    last_seq: u64,
    last_hash: String,
    wal_base_seq: u64,
    wal_base_hash: String,
}

impl SessionLog {
    /// Open (or create) the log for `session_id` under `root` and take the lock.
    ///
    /// # Errors
    ///
    /// [`LogError::AlreadyLocked`] when another writer holds the session, plus
    /// every corruption error [`recover::recover`] can raise.
    pub fn open(root: &Path, session_id: core_domain::SessionId) -> Result<Self, LogError> {
        let dir = session_path(root, &session_id);
        std::fs::create_dir_all(&dir).map_err(|source| LogError::io(&dir, source))?;

        let lock = SessionLock::acquire(&dir)?;
        let recovery = recover::recover(&dir, session_id)?;

        let last_seq = recovery.envelopes.last().map_or(0, |envelope| envelope.seq);
        let last_hash = recovery
            .envelopes
            .last()
            .map_or_else(String::new, |envelope| envelope.hash.clone());

        Ok(Self {
            session_id,
            dir,
            lock,
            last_seq,
            last_hash,
            wal_base_seq: recovery.sealed_seq,
            wal_base_hash: recovery.sealed_hash,
        })
    }

    /// Session this log belongs to.
    #[must_use]
    pub fn session_id(&self) -> core_domain::SessionId {
        self.session_id
    }

    /// Directory holding this session's WAL and segments.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path of the advisory lock file held by this log.
    #[must_use]
    pub fn lock_path(&self) -> &Path {
        self.lock.path()
    }

    /// Sequence number of the newest envelope, or `0` for an empty session.
    #[must_use]
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    /// Hash of the newest envelope, or `""` for an empty session.
    #[must_use]
    pub fn last_hash(&self) -> &str {
        &self.last_hash
    }

    /// Append one event to the WAL and return only once it is durable.
    ///
    /// The returned envelope is the acknowledgement: it exists on disk, with a
    /// trailing newline and an fsync behind it, before this function returns.
    ///
    /// # Errors
    ///
    /// Any filesystem failure on the WAL. The chain head is only advanced after
    /// the write and fsync succeed, so a failed append cannot leave the log
    /// claiming an envelope it does not have.
    pub fn append(
        &mut self,
        kind: core_domain::EventKind,
        payload: serde_json::Value,
    ) -> Result<Envelope, LogError> {
        let seq = self.last_seq + 1;
        let envelope = Envelope::new(
            self.session_id,
            core_domain::EventId::now_v7(),
            seq,
            envelope::now_millis(),
            kind,
            payload,
            self.last_hash.clone(),
        );

        wal::append_line(&self.wal_path(), &envelope)?;

        self.last_seq = seq;
        self.last_hash.clone_from(&envelope.hash);
        Ok(envelope)
    }

    /// Seal the current WAL into a segment, then truncate the WAL.
    ///
    /// Returns `None` when the WAL is empty, which is the normal state right
    /// after an earlier seal.
    ///
    /// # Errors
    ///
    /// [`LogError::TornWal`] if the WAL has a torn tail (recovery has not run,
    /// or something else wrote the file). Sealing never trims.
    pub fn seal(&mut self) -> Result<Option<SealedSegment>, LogError> {
        self.seal_with(&NoFault)
    }

    /// [`SessionLog::seal`] with a [`SealFault`] seam between the two steps.
    ///
    /// # Errors
    ///
    /// As [`SessionLog::seal`].
    pub fn seal_with(&mut self, fault: &dyn SealFault) -> Result<Option<SealedSegment>, LogError> {
        let wal_path = self.wal_path();
        let bytes = wal::read(&wal_path)?;
        let scan = wal::scan(&bytes)?;

        if let Some(bytes) = scan.torn {
            return Err(LogError::TornWal { bytes });
        }

        let (Some(first), Some(last)) = (scan.envelopes.first(), scan.envelopes.last()) else {
            return Ok(None);
        };

        // Validate the range against the chain the log already knows before any
        // byte of it is allowed to become an immutable segment.
        let mut prev_hash = self.wal_base_hash.clone();
        for (expected_seq, envelope) in (self.wal_base_seq + 1..).zip(scan.envelopes.iter()) {
            if envelope.seq != expected_seq {
                return Err(LogError::SeqGap {
                    expected: expected_seq,
                    found: envelope.seq,
                });
            }
            if envelope.prev_hash != prev_hash {
                return Err(LogError::BrokenChain { seq: envelope.seq });
            }
            if !envelope.has_valid_hash() {
                return Err(LogError::HashMismatch { seq: envelope.seq });
            }
            prev_hash.clone_from(&envelope.hash);
        }

        let segment = segment::write(&self.dir, first.seq, last.seq, &scan.envelopes)?;

        // The single hazardous window: the segment is durable, the WAL is not yet
        // truncated. Anything that dies here replays cleanly because the WAL
        // still holds a copy and recovery drops the covered prefix.
        fault.before_wal_truncate(&segment.path);

        wal::truncate(&wal_path)?;

        self.wal_base_seq = last.seq;
        self.wal_base_hash.clone_from(&last.hash);
        Ok(Some(segment))
    }

    /// Re-read and re-validate the whole session history.
    ///
    /// # Errors
    ///
    /// Any corruption error [`recover::history`] can raise.
    pub fn history(&self) -> Result<Vec<Envelope>, LogError> {
        recover::history(&self.dir, self.session_id)
    }

    fn wal_path(&self) -> PathBuf {
        self.dir.join(WAL_FILE_NAME)
    }
}

#[cfg(test)]
mod tests {
    use core_domain::{EventKind, SessionId};
    use serde_json::json;

    use super::{SessionLog, WAL_FILE_NAME, session_path};

    // Behaviour one: an append acknowledges only once its bytes are durable,
    // and a replay returns that same envelope.
    #[test]
    fn append_ack_is_durable_and_replays_identical() {
        let root = tempfile::tempdir().expect("tempdir");
        let session_id = SessionId::now_v7();
        let dir = session_path(root.path(), &session_id);

        let envelope = {
            let mut log = SessionLog::open(root.path(), session_id).expect("open");
            let envelope = log
                .append(
                    EventKind::TurnRequested {
                        text: String::from("hello"),
                    },
                    json!({ "turn": "t-1" }),
                )
                .expect("append");

            // The ack is the envelope, and the line it names is already complete
            // and terminated on disk when append returns: an fsynced record, not
            // a buffered one.
            let on_disk = std::fs::read(dir.join(WAL_FILE_NAME)).expect("read wal");
            let expected = format!("{}\n", envelope.to_line().expect("line"));
            assert_eq!(
                on_disk,
                expected.as_bytes(),
                "the WAL must be complete and newline-terminated at ack time"
            );
            assert_eq!(envelope.seq, 1);
            assert_eq!(envelope.prev_hash, "");
            assert!(envelope.has_valid_hash());
            envelope
        };

        let reopened = SessionLog::open(root.path(), session_id).expect("reopen");
        assert_eq!(reopened.history().expect("history"), vec![envelope.clone()]);
        assert_eq!(reopened.last_seq(), 1);
        assert_eq!(reopened.last_hash(), envelope.hash.as_str());

        let replayed = super::recover::recover(&dir, session_id).expect("recover");
        assert_eq!(replayed.envelopes, vec![envelope]);
        assert_eq!(replayed.torn_tail_bytes, None);
    }
}
