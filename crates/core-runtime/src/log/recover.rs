//! Recovery: turn whatever is on disk into a validated history.
//!
//! This module is where the two failure modes are separated, because getting
//! them the wrong way round is a data-loss bug:
//!
//! | Situation | Treatment |
//! |---|---|
//! | final WAL line has no `\n` | torn tail: truncate to the last valid envelope, record `recovery.truncated` |
//! | final WAL line is not JSON | torn tail: same |
//! | broken line with data after it | corruption: hard error, nothing is trimmed |
//! | `seq` gap, broken `prev_hash`, wrong hash, wrong schema, wrong session | hard error |
//! | sealed segment will not decompress | hard error, never a trim |
//! | WAL prefix already covered by a segment | interrupted seal: drop the duplicate, truncate the WAL, no error and no duplicate `seq` |
//!
//! The interrupted-seal case is what makes the seal protocol safe. A writer that
//! dies after fsyncing `session.v1.<first>-<last>.jsonl.zst` but before
//! truncating `wal.jsonl` leaves the same envelopes in both places. Replay
//! prefers the sealed copy and drops the WAL prefix it covers, so replaying is
//! idempotent: no loss, no duplication, and the next open completes the seal.

use std::path::Path;

use core_domain::SessionId;
use serde_json::json;

use crate::log::envelope::{self, SCHEMA_VERSION};
use crate::log::{Envelope, LogError, WAL_FILE_NAME, segment, wal};

/// What recovery found and, when it repaired something, what it did.
#[derive(Debug)]
pub struct Recovery {
    /// The validated history, sealed segments first, then the live WAL.
    pub envelopes: Vec<Envelope>,
    /// Highest `seq` covered by a sealed segment (`0` when there is none).
    pub sealed_seq: u64,
    /// Hash of the last sealed envelope (`""` when there is none).
    pub sealed_hash: String,
    /// Bytes dropped from a torn WAL tail, if there were any.
    pub torn_tail_bytes: Option<u64>,
}

/// Recover a session directory, repairing a torn tail and an interrupted seal.
///
/// # Errors
///
/// Every corruption error in [`LogError`] except [`LogError::AlreadyLocked`];
/// the caller is expected to hold the session lock before calling this.
pub fn recover(dir: &Path, session_id: SessionId) -> Result<Recovery, LogError> {
    load(dir, session_id, Repair::Yes)
}

/// Read and validate a session directory without repairing anything.
///
/// # Errors
///
/// As [`recover`], plus [`LogError::TornWal`] if the WAL still has a torn tail
/// (that is [`recover`]'s job, not this function's).
pub fn history(dir: &Path, session_id: SessionId) -> Result<Vec<Envelope>, LogError> {
    Ok(load(dir, session_id, Repair::No)?.envelopes)
}

/// Whether [`load`] may write to the session directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Repair {
    /// Repair a torn tail and finish an interrupted seal.
    Yes,
    /// Never write; report a torn tail as an error instead.
    No,
}

/// The hash chain, as it is being rebuilt.
///
/// Sequence numbers are derived from the length of the chain (`seq` starts at 1
/// and is gap-free by definition), so a gap cannot be papered over: it fails the
/// equality check in [`Chain::push`].
#[derive(Debug)]
struct Chain {
    session_id: SessionId,
    envelopes: Vec<Envelope>,
}

impl Chain {
    fn new(session_id: SessionId) -> Self {
        Self {
            session_id,
            envelopes: Vec::new(),
        }
    }

    fn next_seq(&self) -> u64 {
        u64::try_from(self.envelopes.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1)
    }

    fn head_hash(&self) -> &str {
        self.envelopes
            .last()
            .map_or("", |envelope| envelope.hash.as_str())
    }

    fn push(&mut self, envelope: Envelope) -> Result<(), LogError> {
        if envelope.schema_version != SCHEMA_VERSION {
            return Err(LogError::UnsupportedSchema {
                found: envelope.schema_version,
                expected: SCHEMA_VERSION,
            });
        }
        if envelope.session_id != self.session_id {
            return Err(LogError::SessionMismatch {
                seq: envelope.seq,
                expected: self.session_id.to_string(),
                found: envelope.session_id.to_string(),
            });
        }
        let expected = self.next_seq();
        if envelope.seq != expected {
            return Err(LogError::SeqGap {
                expected,
                found: envelope.seq,
            });
        }
        if envelope.prev_hash.as_str() != self.head_hash() {
            return Err(LogError::BrokenChain { seq: envelope.seq });
        }
        if !envelope.has_valid_hash() {
            return Err(LogError::HashMismatch { seq: envelope.seq });
        }
        self.envelopes.push(envelope);
        Ok(())
    }
}

fn load(dir: &Path, session_id: SessionId, repair: Repair) -> Result<Recovery, LogError> {
    let mut chain = Chain::new(session_id);
    let mut sealed_seq = 0_u64;
    let mut sealed_hash = String::new();

    // Sealed history first: it is authoritative and it establishes the chain
    // head that the WAL has to continue from.
    for path in segment::list(dir)? {
        for envelope in segment::read(&path)? {
            sealed_seq = envelope.seq;
            sealed_hash.clone_from(&envelope.hash);
            chain.push(envelope)?;
        }
    }

    let wal_path = wal::wal_path(dir);
    let wal_bytes = wal::read(&wal_path)?;
    let scan = wal::scan(&wal_bytes)?;

    // The WAL entries that continue the chain. Anything at or below the sealed
    // high-water mark is a duplicate left behind by an interrupted seal.
    let uncovered_from = chain.envelopes.len();
    let mut covered_prefix = false;
    for envelope in scan.envelopes {
        if envelope.seq <= sealed_seq {
            covered_prefix = true;
            continue;
        }
        chain.push(envelope)?;
    }

    let torn = scan.torn;
    let needs_rewrite = torn.is_some() || covered_prefix;

    if repair == Repair::No {
        if let Some(bytes) = torn {
            return Err(LogError::TornWal { bytes });
        }
        return Ok(Recovery {
            envelopes: chain.envelopes,
            sealed_seq,
            sealed_hash,
            torn_tail_bytes: None,
        });
    }

    if needs_rewrite {
        wal::rewrite(&wal_path, &chain.envelopes[uncovered_from..])?;
    }

    if let Some(dropped_bytes) = torn {
        let envelope = Envelope::new(
            session_id,
            core_domain::EventId::now_v7(),
            chain.next_seq(),
            envelope::now_millis(),
            core_domain::EventKind::RecoveryTruncated { dropped_bytes },
            json!({ "recovered_from": WAL_FILE_NAME }),
            chain.head_hash().to_owned(),
        );
        chain.push(envelope.clone())?;
        wal::append_line(&wal_path, &envelope)?;
    }

    Ok(Recovery {
        envelopes: chain.envelopes,
        sealed_seq,
        sealed_hash,
        torn_tail_bytes: torn,
    })
}

// ---------------------------------------------------------------------------
// Behaviour three: a torn final line is truncated *and recorded*.
//
// Declared at module scope, not inside `mod tests`, so the test id a reviewer
// looks for in the plan is exactly `log::recover::torn_tail_is_truncated_and_recorded`.
// ---------------------------------------------------------------------------

#[cfg(test)]
#[test]
fn torn_tail_is_truncated_and_recorded() {
    use crate::log::{SessionLog, session_path};

    let root = tempfile::tempdir().expect("tempdir");
    let session_id = SessionId::now_v7();
    let dir = session_path(root.path(), &session_id);

    let (first, second) = {
        let mut log = SessionLog::open(root.path(), session_id).expect("open");
        let first = log
            .append(
                core_domain::EventKind::TurnRequested {
                    text: String::from("hello"),
                },
                json!({ "turn": 1 }),
            )
            .expect("append one");
        let second = log
            .append(
                core_domain::EventKind::TurnCompleted {
                    text: String::from("hi back"),
                },
                json!({ "turn": 1 }),
            )
            .expect("append two");
        (first, second)
    };

    // A writer killed mid-append leaves a partial line with no terminator: the
    // bytes of an envelope that was never acknowledged.
    let wal_path = dir.join(WAL_FILE_NAME);
    let unacked = Envelope::new(
        session_id,
        core_domain::EventId::now_v7(),
        second.seq + 1,
        envelope::now_millis(),
        core_domain::EventKind::TurnCompleted {
            text: String::from("never acknowledged"),
        },
        json!({ "turn": 1 }),
        second.hash.clone(),
    );
    let partial = unacked.to_line().expect("line");
    let half = partial.len() / 2;
    let torn_bytes = u64::try_from(half).expect("fits");
    {
        use std::io::Write as _;

        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&wal_path)
            .expect("open wal");
        file.write_all(&partial.as_bytes()[..half])
            .expect("append half a line");
        file.sync_all().expect("fsync the torn write");
    }

    let recovery = recover(&dir, session_id).expect("recovery accepts a torn tail");

    assert_eq!(recovery.torn_tail_bytes, Some(torn_bytes));
    assert_eq!(recovery.envelopes.len(), 3);
    assert_eq!(recovery.envelopes[0], first);
    assert_eq!(recovery.envelopes[1], second);
    match &recovery.envelopes[2].kind {
        core_domain::EventKind::RecoveryTruncated { dropped_bytes } => {
            assert_eq!(*dropped_bytes, torn_bytes);
        }
        other => panic!("expected a recovery.truncated event, found {other:?}"),
    }
    assert_eq!(recovery.envelopes[2].seq, 3);

    // The WAL on disk now ends at the recovery event and has no torn tail.
    let rescan = wal::scan(&std::fs::read(&wal_path).expect("read wal")).expect("rescan");
    assert!(rescan.torn.is_none(), "the torn tail must be gone");
    assert_eq!(rescan.envelopes.len(), 3);

    // Second pass is a no-op: recovery is idempotent and the event is durable.
    let again = recover(&dir, session_id).expect("second recovery");
    assert_eq!(again.torn_tail_bytes, None);
    assert_eq!(again.envelopes.len(), 3);
    assert_eq!(again.envelopes[2].hash, recovery.envelopes[2].hash);

    // Same rule for a terminated-but-unparseable final line.
    let mut bytes = std::fs::read(&wal_path).expect("read wal");
    bytes.extend_from_slice(b"{ this is not json }\n");
    std::fs::write(&wal_path, &bytes).expect("write unparseable tail");
    let unparseable_bytes = 21_u64;
    let recovery = recover(&dir, session_id).expect("recovery accepts an unparseable tail");
    assert_eq!(recovery.torn_tail_bytes, Some(unparseable_bytes));
    assert_eq!(recovery.envelopes.len(), 4);
    match &recovery.envelopes[3].kind {
        core_domain::EventKind::RecoveryTruncated { dropped_bytes } => {
            assert_eq!(*dropped_bytes, unparseable_bytes);
        }
        other => panic!("expected a recovery.truncated event, found {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use core_domain::{EventKind, SessionId};
    use serde_json::json;

    use super::recover;
    use crate::log::{Envelope, LogError, SessionLog, WAL_FILE_NAME, session_path};

    fn turn(text: &str) -> EventKind {
        EventKind::TurnCompleted {
            text: text.to_owned(),
        }
    }

    // Behaviour two: a sequence gap is rejected, and the file is not touched.
    #[test]
    fn seq_gap_is_rejected() {
        let root = tempfile::tempdir().expect("tempdir");
        let session_id = SessionId::now_v7();
        let dir = session_path(root.path(), &session_id);

        let first = {
            let mut log = SessionLog::open(root.path(), session_id).expect("open");
            log.append(turn("one"), json!({})).expect("append")
        };

        // A gap that everything else about is valid: correct prev_hash, correct
        // hash over the new seq. Only the missing seq 2 is wrong.
        let gap = Envelope::new(
            session_id,
            core_domain::EventId::now_v7(),
            3,
            crate::log::envelope::now_millis(),
            turn("three"),
            json!({}),
            first.hash.clone(),
        );
        assert!(gap.has_valid_hash(), "the gap envelope is otherwise valid");

        let wal_path = dir.join(WAL_FILE_NAME);
        let planted = format!(
            "{}\n{}\n",
            first.to_line().expect("line"),
            gap.to_line().expect("line")
        );
        std::fs::write(&wal_path, planted.as_bytes()).expect("plant wal");

        let error = recover(&dir, session_id).expect_err("a sequence gap must be rejected");
        assert!(
            matches!(
                error,
                LogError::SeqGap {
                    expected: 2,
                    found: 3
                }
            ),
            "expected SeqGap {{ 2, 3 }}, got {error:?}"
        );

        // A rejected replay must not rewrite the evidence.
        assert_eq!(
            std::fs::read(&wal_path).expect("read wal"),
            planted.as_bytes()
        );
    }

    // Behaviour four: the seal-then-truncate window, with a real process death
    // in it. The child seals (segment written and fsynced) and dies from the
    // fault hook before the WAL truncation; the parent then replays.
    #[test]
    fn seal_then_truncate_crash_window_replays_clean() {
        const CRASH_ROOT: &str = "KALLISTI_LOG_CRASH_ROOT";
        const CRASH_SESSION: &str = "KALLISTI_LOG_CRASH_SESSION";
        const TEST_ID: &str = "log::recover::tests::seal_then_truncate_crash_window_replays_clean";

        struct AbortInWindow;

        impl crate::log::SealFault for AbortInWindow {
            fn before_wal_truncate(&self, _segment: &Path) {
                // Everything after this point in the seal is what the test is
                // about, so the process dies exactly here: no destructors, no
                // WAL truncation, lock released by the kernel.
                std::process::abort();
            }
        }

        let root = tempfile::tempdir().expect("tempdir");
        let session_id = SessionId::now_v7();
        let dir = session_path(root.path(), &session_id);

        // Child mode: seal until the fault fires.
        if let Ok(child_root) = std::env::var(CRASH_ROOT) {
            let child_session: SessionId = std::env::var(CRASH_SESSION)
                .expect("child session id")
                .parse()
                .expect("valid session id");
            let mut log = SessionLog::open(Path::new(&child_root), child_session)
                .expect("child opens the log");
            let _ = log.seal_with(&AbortInWindow);
            unreachable!("the injected fault must not let the seal finish");
        }

        // Parent: three durable envelopes, then hand the seal to a child process.
        let appended = {
            let mut log = SessionLog::open(root.path(), session_id).expect("open");
            let appended: Vec<Envelope> = (1..=3)
                .map(|index| {
                    log.append(turn(&format!("turn {index}")), json!({ "index": index }))
                        .expect("append")
                })
                .collect();
            appended
        };

        let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([TEST_ID, "--exact", "--test-threads=1"])
            .env(CRASH_ROOT, root.path())
            .env(CRASH_SESSION, session_id.to_string())
            .output()
            .expect("spawn the crash child");

        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt as _;
            assert_eq!(
                output.status.signal(),
                Some(6),
                "the child must die from the injected fault (SIGABRT), not exit normally\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        #[cfg(not(unix))]
        assert!(
            !output.status.success(),
            "the child must die in the seal window"
        );

        // The window is genuinely open: segment durable, WAL not truncated.
        let segments = crate::log::segment::list(&dir).expect("list segments");
        assert_eq!(segments.len(), 1, "the child must have written one segment");
        let wal_path = dir.join(WAL_FILE_NAME);
        let wal_after_crash = std::fs::read(&wal_path).expect("read wal");
        assert!(
            !wal_after_crash.is_empty(),
            "the WAL must still hold the sealed range after the crash"
        );

        // Replay: same envelopes, no duplicate seq, chain intact.
        let mut reopened = SessionLog::open(root.path(), session_id).expect("reopen after crash");
        let history = reopened.history().expect("history");
        assert_eq!(history.len(), appended.len(), "no loss and no duplication");
        assert_eq!(
            history
                .iter()
                .map(|envelope| envelope.seq)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        for (replayed, original) in history.iter().zip(&appended) {
            assert_eq!(replayed.event_id, original.event_id);
            assert_eq!(replayed.hash, original.hash);
        }
        assert_eq!(reopened.last_seq(), 3);

        // Recovery finished the interrupted seal rather than repeating it.
        assert!(
            std::fs::read(&wal_path).expect("read wal").is_empty(),
            "recovery must truncate the WAL prefix the segment already covers"
        );
        assert!(
            recover(&dir, session_id)
                .expect("recovering again")
                .torn_tail_bytes
                .is_none(),
            "nothing was lost, so nothing is recorded as truncated"
        );

        // And the session keeps working with no gap.
        let fourth = reopened
            .append(turn("turn 4"), json!({ "index": 4 }))
            .expect("append after recovery");
        assert_eq!(fourth.seq, 4);
        assert_eq!(fourth.prev_hash, history[2].hash);
        assert_eq!(reopened.history().expect("history").len(), 4);
    }

    // Behaviour six: a sealed segment that will not decompress is an error. It
    // is never trimmed, and nothing downstream of it is "repaired".
    #[test]
    fn sealed_segment_decode_error_is_an_error_not_a_trim() {
        let root = tempfile::tempdir().expect("tempdir");
        let session_id = SessionId::now_v7();
        let dir = session_path(root.path(), &session_id);

        let sealed = {
            let mut log = SessionLog::open(root.path(), session_id).expect("open");
            for index in 1..=3 {
                log.append(turn(&format!("turn {index}")), json!({ "index": index }))
                    .expect("append");
            }
            log.seal().expect("seal").expect("a segment was written")
        };
        assert_eq!((sealed.first_seq, sealed.last_seq), (1, 3));

        // Break the frame. Whatever produced this, it is not a zstd frame any more.
        let corrupted = b"this is not a zstd frame";
        std::fs::write(&sealed.path, corrupted).expect("corrupt segment");

        let error = SessionLog::open(root.path(), session_id)
            .expect_err("a sealed decode error must surface as an error");
        assert!(
            matches!(error, LogError::SegmentDecode { .. }),
            "expected SegmentDecode, got {error:?}"
        );

        // Nothing was trimmed, rewritten, or "recovered".
        assert_eq!(
            std::fs::read(&sealed.path).expect("read segment"),
            corrupted,
            "the sealed segment must be left exactly as found"
        );
        let wal_path = dir.join(WAL_FILE_NAME);
        assert!(
            !wal_path.exists() || std::fs::read(&wal_path).expect("read wal").is_empty(),
            "a sealed decode error must not write to the WAL"
        );
        assert_eq!(
            crate::log::segment::list(&dir).expect("list").len(),
            1,
            "no replacement segment may be written"
        );
        // `.lock`, the emptied WAL after the seal, and the damaged segment.
        let entries: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("read dir")
            .map(|entry| entry.expect("entry").path())
            .collect();
        assert_eq!(
            entries.len(),
            3,
            "nothing may be added beyond lock, empty WAL, segment: {entries:?}"
        );
    }

    // Behaviour seven: the checked-in fixture replays byte for byte.
    #[test]
    fn fixture_replay_is_byte_identical() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../contract/fixtures/session-fixture.jsonl");
        let bytes = std::fs::read(&fixture).unwrap_or_else(|error| {
            panic!(
                "checked-in fixture {} is unreadable: {error}",
                fixture.display()
            )
        });
        let text = std::str::from_utf8(&bytes).expect("fixture is utf-8");

        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 20, "the fixture holds 20 envelopes");

        let mut parsed = Vec::new();
        for line in &lines {
            let envelope = Envelope::from_line(line.as_bytes())
                .unwrap_or_else(|error| panic!("fixture line does not parse: {error}\n{line}"));
            assert_eq!(
                envelope.to_line().expect("re-encode"),
                *line,
                "re-encoding an envelope must reproduce the fixture line byte for byte"
            );
            assert!(
                envelope.has_valid_hash(),
                "fixture hash does not verify at seq {}",
                envelope.seq
            );
            parsed.push(envelope);
        }

        assert_eq!(
            parsed
                .iter()
                .map(|envelope| envelope.seq)
                .collect::<Vec<_>>(),
            (1..=20).collect::<Vec<_>>()
        );
        assert_eq!(parsed[0].prev_hash, "", "seq 1 has no predecessor");
        for pair in parsed.windows(2) {
            assert_eq!(pair[1].prev_hash, pair[0].hash, "fixture chain is linked");
        }

        // Replay the fixture as a live WAL and compare the bytes on both sides.
        let session_id = parsed[0].session_id;
        let root = tempfile::tempdir().expect("tempdir");
        let dir = session_path(root.path(), &session_id);
        std::fs::create_dir_all(&dir).expect("session dir");
        let wal_path = dir.join(WAL_FILE_NAME);
        std::fs::write(&wal_path, &bytes).expect("seed wal");

        let recovery = recover(&dir, session_id).expect("the fixture replays");
        assert_eq!(recovery.torn_tail_bytes, None);
        assert_eq!(
            recovery.envelopes, parsed,
            "replay returns the same envelopes"
        );
        assert_eq!(
            std::fs::read(&wal_path).expect("read wal"),
            bytes,
            "a clean WAL is never rewritten"
        );

        // And the replayed side re-encodes to the same bytes.
        let mut replayed = Vec::new();
        for envelope in &recovery.envelopes {
            replayed.extend_from_slice(envelope.to_line().expect("line").as_bytes());
            replayed.push(b'\n');
        }
        assert_eq!(replayed, bytes, "replay is byte-identical");
    }
}
