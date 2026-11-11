//! The consolidation sweep: it retires duplicates and proposes, nothing more.
//!
//! This module never opens `memory/MEMORY.md` or `memory/USER.md`, and it never
//! writes them. There is exactly one writer of the two tier-1 files and it is
//! [`crate::selfmemory::Access::write`]. If a sweep could write a file directly,
//! the row that says where an entry came from and the text on disk would be two
//! independent writers of one fact, and provenance would collapse: a line the
//! model wrote, a line the operator kept and a line an import carried would all
//! look the same after one sweep.
//!
//! So the sweep proposes. A proposal is a `mem_entries` row with
//! `tier = consolidated`, `origin = consolidation` and `trusted = 0`. Recall
//! reads only `trusted = 1`, so a proposal is stored, inspectable, and invisible
//! to a turn until something accepts it. There is no separate proposals table:
//! one table with a provenance column is the whole design, and a second table
//! would be a second place for the two to disagree.
//!
//! Three behaviours are deliberate and worth reading before changing anything:
//!
//! - **The claim is adopted, not won.** [`crate::mem::claim_sweep`] adopts a
//!   `running` row whose heartbeat has gone stale, in one immediate transaction.
//!   There is no unique index to make a crash a permanent latch.
//! - **A refusal is a real outcome.** When too much of the live set would be
//!   retired at once the sweep settles `failed` and writes nothing, because a
//!   sweep that quietly halves memory is worse than a sweep that did not run.
//! - **Untrusted rows are dropped before clustering, not filtered after.** An
//!   imported line must not be able to vote a real entry out of existence.

use std::collections::BTreeMap;

use time::Duration;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::db::{Db, DbError};
use crate::mem::{self, Entry, NewEntry, Origin, Tier};
use crate::selfmemory::Target;

/// How stale a claim's heartbeat may be before another sweep adopts it.
///
/// Fifteen minutes is the price of a crash: the next tick recovers on its own
/// instead of consolidation waiting for a human to edit `mem_sweeps`.
pub const STALE_SECS: i64 = 900;

/// How many live rows count as more than a handful, which is when the sweep
/// takes a heartbeat before it starts writing.
pub const HANDFUL: usize = 8;

/// The share of the live trusted set one sweep may retire, as a percentage.
///
/// The bound is a refusal, not a rate limit: a sweep that would cross it does
/// nothing at all, so the operator sees a `failed` row with a reason rather than
/// a memory that got smaller for reasons nobody can reconstruct.
pub const LOSS_BOUND_PERCENT: i64 = 25;

/// How one sweep ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SweepOutcome {
    /// Another sweep holds a live claim.
    Busy,
    /// This sweep finished and changed the live set.
    Completed {
        /// The `mem_sweeps` row, which may be one a crash left behind.
        sweep_id: String,
        /// How many rows this sweep retired.
        retired: i64,
        /// How many proposal rows this sweep wrote.
        proposed: i64,
    },
    /// This sweep refused to act and changed nothing.
    Failed {
        /// The `mem_sweeps` row that carries the note.
        sweep_id: String,
        /// Why it refused, in the same words the row uses.
        note: String,
    },
    /// The work was already done by the sweep that held the claim.
    Abandoned {
        /// The reclaimed `mem_sweeps` row, left to its new owner.
        sweep_id: String,
    },
}

/// Run one consolidation sweep.
///
/// # Errors
///
/// A database failure. A refusal is an outcome, not an error.
pub fn sweep(db: &Db, now: &str) -> Result<SweepOutcome, DbError> {
    let Some(current) = parse(now) else {
        // Without a readable clock there is no staleness bound to claim against,
        // and guessing one would adopt somebody else's live sweep.
        return Ok(SweepOutcome::Busy);
    };
    let stale_before = stamp(current - Duration::seconds(STALE_SECS));

    // The claim carries the recovery: a `running` row older than the bound is
    // adopted in place and its attempt counter moves, so a crash costs one
    // interval instead of blocking consolidation until someone edits the table.
    let Some(sweep_id) = mem::claim_sweep(db, &stale_before)? else {
        return Ok(SweepOutcome::Busy);
    };

    // Only an entry nothing has superseded and something has vouched for may
    // contribute to a decision. Untrusted rows are excluded here, structurally,
    // before any clustering: an imported line from another install must not be
    // able to outvote a real entry and retire it.
    let mut live: Vec<Entry> = Vec::new();
    for target in [Target::Memory, Target::User] {
        live.extend(mem::live_entries(db, target.label(), true, 10_000)?);
    }

    let plan = plan(&live);

    let live_count = i64::try_from(live.len()).unwrap_or(i64::MAX);
    let retire_count = i64::try_from(plan.retire.len()).unwrap_or(i64::MAX);
    if retire_count * 100 > live_count * LOSS_BOUND_PERCENT {
        // Refusing is the point of the bound. Nothing has been written yet, so
        // the live set is exactly what it was before this call.
        mem::finish(db, &sweep_id, "failed", 0, Some("loss bound"))?;
        return Ok(SweepOutcome::Failed {
            sweep_id,
            note: "loss bound".to_owned(),
        });
    }

    let mut retired = 0i64;
    let big = live.len() > HANDFUL;

    // A big sweep checks in before it starts writing. A `false` here means the
    // claim was adopted out from under this run, so the work belongs to whoever
    // took it and this run must stop rather than write over that owner.
    if big {
        // First half only: the heartbeat is meant to land mid-run, so a reclaim
        // costs at most the writes that had not happened yet.
        let split = plan.retire.len() / 2;
        for (duplicate, survivor) in &plan.retire[..split] {
            if mem::supersede_entry(db, duplicate, survivor)? {
                retired += 1;
            }
        }
        if !mem::heartbeat(db, &sweep_id)? {
            return Ok(SweepOutcome::Abandoned { sweep_id });
        }
        for (duplicate, survivor) in &plan.retire[split..] {
            if mem::supersede_entry(db, duplicate, survivor)? {
                retired += 1;
            }
        }
    } else {
        for (duplicate, survivor) in &plan.retire {
            if mem::supersede_entry(db, duplicate, survivor)? {
                retired += 1;
            }
        }
    }

    let mut proposed = 0i64;
    for proposal in &plan.proposals {
        // The proposal carries the survivor's own text, in the survivor's own
        // target, so the unique `(target, content_hash)` key is what makes a
        // repeat sweep a no-op. The pre-check says that out loud instead of
        // leaning on the conflict clause to hide it, and it keeps `proposed`
        // meaning "rows this sweep actually wrote".
        if exists(db, proposal.target, proposal.entry)? {
            continue;
        }
        mem::insert_entry(db, proposal)?;
        proposed += 1;
    }

    mem::finish(db, &sweep_id, "completed", retired, None)?;
    Ok(SweepOutcome::Completed {
        sweep_id,
        retired,
        proposed,
    })
}

/// What one sweep intends to do, worked out before anything is written.
struct Plan<'a> {
    /// `(duplicate id, survivor id)` pairs, oldest survivor first.
    retire: Vec<(&'a str, &'a str)>,
    /// Proposal rows, one per near-duplicate group.
    proposals: Vec<NewEntry<'a>>,
}

/// Cluster the live set by target and normalised text, then decide.
fn plan(live: &[Entry]) -> Plan<'_> {
    type Key<'a> = (&'a str, String);

    let mut clusters: BTreeMap<Key<'_>, Vec<&Entry>> = BTreeMap::new();
    for entry in live {
        clusters
            .entry((entry.target.as_str(), normalise(&entry.entry)))
            .or_default()
            .push(entry);
    }

    let mut retire = Vec::new();
    let mut survivors: Vec<&Entry> = Vec::new();
    for mut members in clusters.into_values() {
        // The row that has been there longest wins, tie-broken by id. A sweep
        // that preferred the newest copy would silently rewrite history: the
        // duplicate is usually the later one, and retyping a fact would become
        // a way to change what memory says.
        members.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        let survivor = members[0];
        survivors.push(survivor);
        for duplicate in &members[1..] {
            retire.push((duplicate.id.as_str(), survivor.id.as_str()));
        }
    }

    // A near duplicate is text that differs in punctuation only. It is not the
    // same fact with certainty, so the sweep will not pick one and retire the
    // other; it records the older text as a proposal and leaves both live.
    let mut near: BTreeMap<Key<'_>, Vec<&Entry>> = BTreeMap::new();
    for survivor in &survivors {
        near.entry((survivor.target.as_str(), loose(&survivor.entry)))
            .or_default()
            .push(survivor);
    }
    let mut proposals = Vec::new();
    for mut group in near.into_values() {
        if group.len() < 2 {
            continue;
        }
        group.sort_by(|left, right| {
            left.created_at
                .cmp(&right.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        let anchor = group[0];
        proposals.push(NewEntry {
            tier: Tier::Consolidated,
            target: anchor.target.as_str(),
            entry: anchor.entry.as_str(),
            origin: Origin::Consolidation,
            source_ref: Some("memconsolidate"),
            trusted: false,
        });
    }

    Plan { retire, proposals }
}

/// Lowercase, with every whitespace run collapsed to one space.
fn normalise(entry: &str) -> String {
    entry
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// [`normalise`] with trailing punctuation removed from each word.
fn loose(entry: &str) -> String {
    normalise(entry)
        .split(' ')
        .map(|word| word.trim_end_matches(['.', ',', ';', ':', '!', '?']))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether this text is already stored for this target.
fn exists(db: &Db, target: &str, entry: &str) -> Result<bool, DbError> {
    let hash = mem::content_hash(entry);
    db.with_conn(|conn| {
        let found: i64 = conn.query_row(
            "SELECT COUNT(*) FROM mem_entries WHERE target = ?1 AND content_hash = ?2",
            rusqlite::params![target, hash],
            |row| row.get(0),
        )?;
        Ok(found > 0)
    })
}

/// Parse a stored stamp.
fn parse(raw: &str) -> Option<OffsetDateTime> {
    crate::learning::parse_timestamp(raw)
}

/// Format a stamp the way [`crate::mem::now`] formats one.
///
/// The sweep's staleness bound is compared against a text column, so it has to
/// carry the same spelling as the rows it is compared against.
fn stamp(at: OffsetDateTime) -> String {
    at.format(&Rfc3339)
        .map(|text| text.replace("+00:00", "Z"))
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("in-memory database")
    }

    const NOW: &str = "2026-01-01T12:00:00Z";

    fn at(seconds_ago: i64) -> String {
        let current = parse(NOW).expect("fixed stamp");
        stamp(current - Duration::seconds(seconds_ago))
    }

    /// Seed one entry with an explicit stamp, because the survivor rule is about
    /// `created_at` and a test that inserted through `mem::now()` would be
    /// asserting on whichever second the suite happened to reach.
    fn seed(db: &Db, id: &str, target: &str, text: &str, trusted: bool, seconds_ago: i64) {
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO mem_entries
                     (id, tier, target, entry, origin, source_ref, trusted, content_hash,
                      created_at, superseded_by)
                 VALUES (?1, 'consolidated', ?2, ?3, 'consolidation', NULL, ?4, ?5, ?6, NULL)",
                rusqlite::params![
                    id,
                    target,
                    text,
                    i64::from(trusted),
                    mem::content_hash(text),
                    at(seconds_ago),
                ],
            )?;
            Ok(())
        })
        .expect("seed entry");
    }

    /// Seed a `running` sweep row whose heartbeat is `seconds_ago` old.
    fn claim_row(db: &Db, id: &str, seconds_ago: i64) {
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO mem_sweeps
                     (id, outcome, claimed_at, heartbeat_at, finished_at, preimage_sha256,
                      entries_in, entries_out, attempt, note)
                 VALUES (?1, 'running', ?2, ?2, NULL, NULL, 0, 0, 0, NULL)",
                rusqlite::params![id, at(seconds_ago)],
            )?;
            Ok(())
        })
        .expect("seed sweep");
    }

    fn sweeps(db: &Db) -> Vec<(String, String, i64)> {
        db.with_conn(|conn| {
            let mut statement =
                conn.prepare("SELECT id, outcome, attempt FROM mem_sweeps ORDER BY id ASC")?;
            let rows =
                statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
        .expect("sweeps")
    }

    fn live(db: &Db, target: &str) -> Vec<Entry> {
        mem::live_entries(db, target, false, 100).expect("live")
    }

    /// A crash leaves a `running` row whose heartbeat stops. The next sweep
    /// adopts that row rather than creating a second one, and the attempt
    /// counter is the record that recovery happened.
    #[test]
    fn a_claim_is_adopted_after_a_crash() {
        let db = db();
        seed(
            &db,
            "a",
            "memory",
            "the operator prefers short answers",
            true,
            3_600,
        );
        claim_row(&db, "crashed", 3_600);

        let outcome = sweep(&db, NOW).expect("sweep");
        match &outcome {
            SweepOutcome::Completed { sweep_id, .. } => assert_eq!(sweep_id, "crashed"),
            other => panic!("expected an adopted claim to complete, got {other:?}"),
        }

        let rows = sweeps(&db);
        assert_eq!(
            rows.len(),
            1,
            "adoption must not create a second row: {rows:?}"
        );
        assert_eq!(rows[0].0, "crashed");
        assert_eq!(rows[0].1, "completed");
        assert_eq!(rows[0].2, 1, "the attempt counter records the adoption");
    }

    /// A healthy claim belongs to somebody alive. Touching it would let two
    /// sweeps write the same rows.
    #[test]
    fn a_live_claim_is_not_adopted() {
        let db = db();
        seed(&db, "a", "memory", "a fact", true, 3_600);
        claim_row(&db, "alive", 0);

        assert_eq!(sweep(&db, NOW).expect("sweep"), SweepOutcome::Busy);
        let rows = sweeps(&db);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].1, "running", "a live claim is left alone");
    }

    /// The reclaim boundary is 900 seconds. Two rows, either side of it, and the
    /// stale one is adopted while the fresh one keeps running.
    #[test]
    fn a_heartbeat_older_than_fifteen_minutes_is_stale() {
        let db = db();
        seed(&db, "a", "memory", "a fact", true, 3_600);
        claim_row(&db, "fresh", STALE_SECS - 1);
        claim_row(&db, "stale", STALE_SECS + 1);

        let outcome = sweep(&db, NOW).expect("sweep");
        match &outcome {
            SweepOutcome::Completed { sweep_id, .. } => {
                assert_eq!(sweep_id, "stale", "the old heartbeat is the adoptable one");
            }
            other => panic!("expected the stale claim to be adopted, got {other:?}"),
        }

        let rows = sweeps(&db);
        assert_eq!(rows.len(), 2, "{rows:?}");
        let fresh = rows.iter().find(|row| row.0 == "fresh").expect("fresh row");
        assert_eq!(fresh.1, "running", "the fresh claim was not adopted");
        assert_eq!(fresh.2, 0);
        let stale = rows.iter().find(|row| row.0 == "stale").expect("stale row");
        assert_eq!(stale.2, 1);
    }

    /// An untrusted row is dropped before clustering, so its age cannot make it
    /// a survivor and it cannot retire the trusted row it duplicates.
    #[test]
    fn untrusted_entries_cannot_survive() {
        let db = db();
        // The untrusted row is older and normalises to the same cluster as the
        // trusted one. If it took part in clustering it would win on age and the
        // trusted row would be the duplicate.
        seed(
            &db,
            "imported",
            "memory",
            "a fact  worth keeping",
            false,
            3_600,
        );
        seed(&db, "kept", "memory", "a fact worth keeping", true, 1_800);

        let outcome = sweep(&db, NOW).expect("sweep");
        match &outcome {
            SweepOutcome::Completed { retired, .. } => assert_eq!(*retired, 0, "{outcome:?}"),
            other => panic!("expected a completed sweep, got {other:?}"),
        }

        let rows = live(&db, "memory");
        let kept = rows.iter().find(|row| row.id == "kept").expect("kept row");
        assert!(
            kept.superseded_by.is_none(),
            "a trusted entry was retired by an untrusted duplicate: {rows:?}"
        );
    }

    /// More than a quarter of the live set at once is a refusal. The sweep says
    /// so in its own row and leaves every entry exactly as it was.
    #[test]
    fn an_over_lossy_sweep_writes_nothing() {
        let db = db();
        // Six live trusted rows, four of them duplicates, so three would retire:
        // half the set, well past the bound.
        seed(&db, "a1", "memory", "one fact", true, 5_000);
        seed(&db, "a2", "memory", "one fact ", true, 4_000);
        seed(&db, "b1", "memory", "another fact", true, 3_000);
        seed(&db, "b2", "memory", "another  fact", true, 2_000);
        seed(&db, "c", "memory", "a third fact", true, 1_000);
        seed(&db, "d", "memory", "a fourth fact", true, 500);

        let outcome = sweep(&db, NOW).expect("sweep");
        match &outcome {
            SweepOutcome::Failed { note, .. } => assert_eq!(note, "loss bound"),
            other => panic!("expected a refusal, got {other:?}"),
        }

        let rows = live(&db, "memory");
        assert_eq!(rows.len(), 6, "a refusal changes nothing: {rows:?}");
        assert!(
            rows.iter().all(|row| row.superseded_by.is_none()),
            "a refused sweep retired rows: {rows:?}"
        );

        let proposals = live(&db, "memory")
            .into_iter()
            .filter(|row| row.tier == "consolidated" && !row.trusted)
            .count();
        assert_eq!(proposals, 0, "a refused sweep wrote a proposal");
    }

    /// The sweep has no writer for the two tier-1 files, and this is the fixture
    /// that would fail if one appeared: the file is byte for byte and stamp for
    /// stamp the one the operator left.
    #[test]
    fn sweep_never_touches_a_tier_one_file() {
        use std::time::SystemTime;

        let dir = tempfile::tempdir().expect("temp dir");
        let home = dir.path().join(crate::selfmemory::DIR);
        std::fs::create_dir_all(&home).expect("memory dir");
        let file = home.join(Target::Memory.file());
        let known: &[u8] = b"- the operator prefers short answers\n";
        std::fs::write(&file, known).expect("seed file");

        let before_bytes = std::fs::read(&file).expect("read before");
        let before_stamp: SystemTime = std::fs::metadata(&file)
            .expect("metadata before")
            .modified()
            .expect("mtime before");

        let db = db();
        // Four live rows with one duplicate pair, so the sweep is inside the
        // loss bound and actually does work rather than refusing.
        seed(
            &db,
            "a1",
            "memory",
            "the operator prefers short answers",
            true,
            5_000,
        );
        seed(
            &db,
            "a2",
            "memory",
            "the operator prefers short answers ",
            true,
            4_000,
        );
        seed(&db, "b", "memory", "the operator writes Rust", true, 3_000);
        seed(
            &db,
            "c",
            "memory",
            "the operator is in Indiana",
            true,
            2_000,
        );

        let outcome = sweep(&db, NOW).expect("sweep");
        match &outcome {
            SweepOutcome::Completed { retired, .. } => assert_eq!(*retired, 1, "{outcome:?}"),
            other => panic!("expected a completed sweep to prove it ran, got {other:?}"),
        }

        let after_bytes = std::fs::read(&file).expect("read after");
        let after_stamp: SystemTime = std::fs::metadata(&file)
            .expect("metadata after")
            .modified()
            .expect("mtime after");

        assert_eq!(before_bytes, after_bytes, "the file was rewritten");
        assert_eq!(
            before_bytes, known,
            "the file no longer holds the seeded bytes"
        );
        assert_eq!(before_stamp, after_stamp, "the file was touched");
    }

    /// A sweep that reads like it writes files would be one line away from
    /// writing provenance away, so the check is mechanical. The search stops at
    /// the test module: the fixture above legitimately writes a file, and this
    /// guard is about the code that runs in production.
    #[test]
    fn the_sweep_source_has_no_file_writers() {
        let source = include_str!("memconsolidate.rs");
        let split = source.find("#[cfg(test)]").expect("a test module");
        let production = &source[..split];

        for forbidden in ["std::fs::write", "std::fs::create_dir_all"] {
            assert!(
                !production.contains(forbidden),
                "the sweep module contains {forbidden}; the tier-1 files have one writer"
            );
        }
        assert!(
            production.contains("selfmemory::Access::write")
                || production.contains("`selfmemory::Access::write`")
                || production.contains("crate::selfmemory::Access::write"),
            "the module doc must name the one writer"
        );
    }
}
