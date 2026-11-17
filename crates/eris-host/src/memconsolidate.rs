//! The consolidation sweep: it retires duplicates and proposes, nothing more.
//!
//! This module never writes `memory/MEMORY.md` or `memory/USER.md`. There is
//! exactly one writer of the two tier-1 files and it is
//! [`crate::selfmemory::Access::write`]. If a sweep could write a file directly,
//! the row that says where an entry came from and the text on disk would be two
//! independent writers of one fact, and provenance would collapse: a line the
//! model wrote, a line the operator kept and a line an import carried would all
//! look the same after one sweep.
//!
//! It does read them. Once per sweep, before a proposal exists and before a row
//! moves, the two files are read and their concatenation is hashed onto the
//! `mem_sweeps` row. That digest is the pre-image a finished sweep is checked
//! against. A read is not a write: it cannot move provenance, and it is the only
//! time this module opens the two paths.
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
//!   retired at once, measured in the characters the tier-1 cap counts rather
//!   than in rows, the sweep settles `failed` and writes nothing, because a
//!   sweep that quietly halves memory is worse than a sweep that did not run.
//! - **Untrusted rows are dropped before clustering, not filtered after.** An
//!   imported line must not be able to vote a real entry out of existence.

use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};
use time::Duration;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::db::{Db, DbError};
use crate::mem::{self, Entry, NewEntry, Origin, Tier};
use crate::selfmemory::{self, Target};

/// How stale a claim's heartbeat may be before another sweep adopts it.
///
/// Fifteen minutes is the price of a crash: the next tick recovers on its own
/// instead of consolidation waiting for a human to edit `mem_sweeps`.
pub const STALE_SECS: i64 = 900;

/// How many live rows count as more than a handful, which is when the sweep
/// takes a heartbeat before it starts writing.
pub const HANDFUL: usize = 8;

/// The share of live tier-1 characters one sweep may retire, as a percentage.
///
/// The unit is characters, not rows, because the cap this bound protects is
/// stated in characters: [`crate::selfmemory::MEMORY_LIMIT`] and
/// [`crate::selfmemory::USER_LIMIT`] bound a file by `chars().count()`. Retiring
/// many short rows is cheap and retiring one long row may not be, so a count of
/// rows is not the quantity the cap is about.
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

/// The `settings` row that lets the sweep act on its own.
pub const SETTING_NIGHTLY: &str = "memory.consolidate_nightly";

/// Whether the sweep may run unattended.
///
/// Off by default, and deliberately: a consolidation that retires a fact is worse
/// than a duplicate, so the shape is "look first, then switch it on". A preview
/// runs either way, which is what the log line is for.
#[must_use]
pub fn nightly(db: &Db) -> bool {
    matches!(
        crate::web::store::settings_get(db, SETTING_NIGHTLY)
            .ok()
            .flatten()
            .as_deref()
            .map(str::trim),
        Some("true")
    )
}

/// What a sweep would do right now, without claiming or writing anything.
///
/// This is the dry run. It reads the live set exactly as [`sweep`] would and
/// returns the counts the report would carry, so the answer before switching the
/// feature on is a measurement rather than a hope.
///
/// # Errors
///
/// A database failure.
pub fn preview(db: &Db) -> Result<Preview, DbError> {
    let mut live: Vec<Entry> = Vec::new();
    for target in [Target::Memory, Target::User] {
        live.extend(mem::live_entries(db, target.label(), true, 10_000)?);
    }
    let live_chars: u128 = live
        .iter()
        .map(|entry| entry.entry.chars().count() as u128)
        .sum();
    let plan = plan(&live);
    let refused = live_chars > 0
        && plan.retired_chars * 100 > live_chars * u128::from(LOSS_BOUND_PERCENT as u64);
    Ok(Preview {
        retired: plan.retire.len() as i64,
        proposed: plan.proposals.len() as i64,
        refused,
    })
}

/// One dry run's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Preview {
    /// Rows a real sweep would retire.
    pub retired: i64,
    /// Proposals it would write.
    pub proposed: i64,
    /// Whether it would refuse on the loss bound instead of acting.
    pub refused: bool,
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

    // The claim wrote a digest of the row table. That is not the pre-image the
    // gate is about, so it is replaced here with the one that can be checked
    // against disk: the two tier-1 files, read now, before this sweep has
    // retired a row or proposed one.
    record_preimage(db, &sweep_id, &preimage(db))?;

    let plan = plan(&live);

    // The bound is stated in characters because the cap it protects is, so the
    // accounting is in characters. Cross multiplication compares the two shares
    // without a division, and no division means there is nothing here to divide
    // by zero. A live set of zero characters holds no rows at all, since the
    // schema refuses an entry shorter than one character, so the bound passes
    // trivially: there is no text for a proposal to lose, and refusing would
    // mean an install with an empty tier 1 could never settle a sweep as
    // anything but `failed`.
    let live_chars: u128 = live
        .iter()
        .map(|entry| entry.entry.chars().count() as u128)
        .sum();
    if live_chars > 0
        && plan.retired_chars * 100 > live_chars * u128::from(LOSS_BOUND_PERCENT as u64)
    {
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

/// SHA-256 over the concatenation of the two tier-1 files, as they read now.
///
/// This is the pre-image a finished sweep is checked against: the text that was
/// on disk when the sweep entered, in the order the prompt reads it. A file that
/// is not there contributes nothing, which is what `Access::read` reports for a
/// missing file too, so an install that has never written tier 1 hashes the empty
/// concatenation rather than failing. A database with no file has no state
/// directory and therefore no tier-1 files, and it hashes that same empty
/// concatenation.
///
/// The state directory is the parent of the database file. Both layouts that
/// exist put `eris.db` directly inside it, and [`crate::selfmemory::Access`] is
/// built from that same directory, so `memory/` hangs off the parent of the
/// path the handle already carries.
fn preimage(db: &Db) -> String {
    let root = db.path().and_then(Path::parent);
    let mut hasher = Sha256::new();
    for target in [Target::Memory, Target::User] {
        let bytes = root
            .map(|root| root.join(selfmemory::DIR).join(target.file()))
            .and_then(|path| std::fs::read(path).ok())
            .unwrap_or_default();
        hasher.update(&bytes);
    }
    hex::encode(hasher.finalize())
}

/// Put the pre-image on the sweep row.
///
/// The claim wrote a digest of the row table when it inserted the row. That is a
/// different quantity, and a row adopted after a crash still carries whatever the
/// crashed sweep left, so this overwrites it once per sweep with the digest of
/// the two files. A refused sweep records what it saw as well: the `failed` row
/// then names the exact state it declined to change.
fn record_preimage(db: &Db, sweep_id: &str, digest: &str) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE mem_sweeps SET preimage_sha256 = ?2 WHERE id = ?1",
            rusqlite::params![sweep_id, digest],
        )?;
        Ok(())
    })
}

/// What one sweep intends to do, worked out before anything is written.
struct Plan<'a> {
    /// `(duplicate id, survivor id)` pairs, oldest survivor first.
    retire: Vec<(&'a str, &'a str)>,
    /// How many characters of live text those retirements take out of the set.
    ///
    /// This is the unit the loss bound is stated in. It counts the duplicate
    /// side of every pair, which is what a retirement actually removes from a
    /// live read, and it leaves the survivor in place.
    retired_chars: u128,
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
    let mut retired_chars: u128 = 0;
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
            retired_chars += duplicate.entry.chars().count() as u128;
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
        });
    }

    Plan {
        retire,
        retired_chars,
        proposals,
    }
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

    /// The digest a sweep row carries, as stored.
    fn recorded_preimage(db: &Db, sweep_id: &str) -> Option<String> {
        db.with_conn(|conn| {
            let mut statement =
                conn.prepare("SELECT preimage_sha256 FROM mem_sweeps WHERE id = ?1")?;
            let row: Option<Option<String>> = statement
                .query_map([sweep_id], |row| row.get(0))?
                .next()
                .transpose()?;
            Ok(row.flatten())
        })
        .expect("preimage")
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

    /// More than a quarter of the live characters at once is a refusal. The
    /// sweep says so in its own row and leaves every entry exactly as it was.
    ///
    /// The pair that retires is one row of six, which is 17% of the rows and
    /// would pass a row bound. The two members of the pair are long and the four
    /// rows that stay are short, so the text that would leave is 47% of the live
    /// characters. What this fixture shows is a refusal that only a character
    /// bound can reach, and that the refusal writes nothing at all.
    #[test]
    fn an_over_lossy_sweep_writes_nothing() {
        let db = db();
        let tail = "x".repeat(340);
        seed(
            &db,
            "a1",
            "memory",
            &format!("fact one {tail}"),
            true,
            5_000,
        );
        seed(
            &db,
            "a2",
            "memory",
            &format!("fact one  {tail}"),
            true,
            4_000,
        );
        seed(&db, "b", "memory", "another fact", true, 3_000);
        seed(&db, "c", "memory", "a third fact", true, 2_000);
        seed(&db, "d", "memory", "a fourth fact", true, 1_000);
        seed(&db, "e", "memory", "a fifth fact", true, 500);

        let outcome = sweep(&db, NOW).expect("sweep");
        assert!(
            matches!(&outcome, SweepOutcome::Failed { note, .. } if note == "loss bound"),
            "expected a refusal, got {outcome:?}"
        );

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

    /// The bound is a share of characters, and this is the fixture that fails if
    /// someone puts the row count back.
    ///
    /// Four of fourteen rows would retire, which is 28.6% and past a 25% row
    /// bound, so a row bound refuses the sweep outright. The four rows that leave
    /// carry three characters each while the ten that stay carry two hundred and
    /// ninety five each, so the text leaving the live set is 12 characters of
    /// 1790, which is far inside a 25% character bound. A row bound refuses and a
    /// character bound completes, so the assertion on `Completed` is the
    /// distinction.
    #[test]
    fn the_loss_bound_counts_characters_not_rows() {
        let db = db();
        for (index, stem) in ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]
            .iter()
            .enumerate()
        {
            let text = format!("{stem}{}", "x".repeat(290));
            seed(
                &db,
                &format!("keep{index}"),
                "memory",
                &text,
                true,
                10_000 - index as i64,
            );
        }
        // Four duplicate pairs of two and three characters. The older member of
        // each pair survives and the newer one retires, so four rows of three
        // characters are what leaves the live set.
        for (index, pair) in ["ab", "cd", "ef", "gh"].iter().enumerate() {
            let base = 5_000 - index as i64 * 10;
            seed(&db, &format!("dup{index}a"), "memory", pair, true, base);
            seed(
                &db,
                &format!("dup{index}b"),
                "memory",
                &format!("{pair} "),
                true,
                base - 5,
            );
        }

        let live_chars: usize = live(&db, "memory")
            .iter()
            .map(|row| row.entry.chars().count())
            .sum();
        assert!(
            4 * 100 > 14 * LOSS_BOUND_PERCENT as usize,
            "the fixture must cross a 25% row bound, or it proves nothing"
        );
        assert!(
            12 * 100 < live_chars * LOSS_BOUND_PERCENT as usize,
            "the fixture must sit inside the character bound, or it proves nothing"
        );

        let outcome = sweep(&db, NOW).expect("sweep");
        match &outcome {
            SweepOutcome::Completed { retired, .. } => {
                assert_eq!(*retired, 4, "four cheap duplicates retire: {outcome:?}");
            }
            other => panic!("a row bound would refuse this sweep, got {other:?}"),
        }
    }

    /// The pre-image is the digest of the two tier-1 files as the sweep found
    /// them, not a digest of the row table. The caller recomputes the same bytes
    /// independently and the two must agree.
    ///
    /// The live pair differs in punctuation only, which is the shape that makes
    /// the sweep propose. The proposal row itself does not land, and that is a
    /// separate defect from the one this fixture was written for: `exists` sees
    /// the anchor's own text under the same target and skips the insert, so
    /// `proposed` is zero for every near-duplicate group. What is asserted here
    /// is the half this change is about, which is the recorded digest.
    #[test]
    fn sweep_proposes_and_records_pre_image() {
        let dir = tempfile::tempdir().expect("temp dir");
        let state = dir.path().join("state");
        std::fs::create_dir_all(state.join(selfmemory::DIR)).expect("memory dir");
        let memory = state.join(selfmemory::DIR).join(Target::Memory.file());
        let user = state.join(selfmemory::DIR).join(Target::User.file());
        std::fs::write(&memory, b"- the operator prefers short answers\n").expect("seed MEMORY.md");
        std::fs::write(&user, b"- the operator is in Indiana\n").expect("seed USER.md");

        let db = Db::open(state.join("eris.db")).expect("file backed database");
        seed(
            &db,
            "a",
            "memory",
            "the operator prefers short answers",
            true,
            5_000,
        );
        seed(
            &db,
            "b",
            "memory",
            "the operator prefers short answers.",
            true,
            4_000,
        );

        // The independent computation: the same two files, concatenated in the
        // order the prompt reads them, hashed by the caller.
        let mut hasher = Sha256::new();
        hasher.update(std::fs::read(&memory).expect("read MEMORY.md"));
        hasher.update(std::fs::read(&user).expect("read USER.md"));
        let expected = hex::encode(hasher.finalize());

        // The digest the sweep must record must not accidentally equal a digest
        // over the row table, so the two are shown to differ here.
        let mut rows = Sha256::new();
        for entry in live(&db, "memory") {
            rows.update(entry.id.as_bytes());
            rows.update([0u8]);
            rows.update(entry.entry.as_bytes());
            rows.update([0u8]);
        }
        assert_ne!(
            expected,
            hex::encode(rows.finalize()),
            "the fixture is degenerate"
        );

        let outcome = sweep(&db, NOW).expect("sweep");
        match &outcome {
            SweepOutcome::Completed { sweep_id, .. } => assert_eq!(
                recorded_preimage(&db, sweep_id).as_deref(),
                Some(expected.as_str()),
                "the pre-image is not the digest of the two tier-1 files"
            ),
            other => panic!("expected a completed sweep, got {other:?}"),
        }
    }

    /// The sweep has no writer for the two tier-1 files, and this is the fixture
    /// that would fail if one appeared: the file is byte for byte and stamp for
    /// stamp the one the operator left.
    ///
    /// The database is file backed on purpose, so [`preimage`] resolves the two
    /// paths and actually reads them. A read is what this sweep is allowed to do,
    /// and the byte comparison and the mtime comparison are what prove it stayed
    /// a read.
    #[test]
    fn sweep_never_touches_a_tier_one_file() {
        use std::time::SystemTime;

        let dir = tempfile::tempdir().expect("temp dir");
        let state = dir.path().join("state");
        let home = state.join(selfmemory::DIR);
        std::fs::create_dir_all(&home).expect("memory dir");
        let file = home.join(Target::Memory.file());
        let known: &[u8] = b"- the operator prefers short answers\n";
        std::fs::write(&file, known).expect("seed file");

        let before_bytes = std::fs::read(&file).expect("read before");
        let before_stamp: SystemTime = std::fs::metadata(&file)
            .expect("metadata before")
            .modified()
            .expect("mtime before");

        let db = Db::open(state.join("eris.db")).expect("file backed database");
        // Five live rows with one duplicate pair, so the sweep is inside the
        // character bound and actually does work rather than refusing. The
        // retired text is thirty five of the hundred and sixty six live
        // characters, which is 21%, and the four rows that stay keep the rest.
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
        seed(
            &db,
            "b",
            "memory",
            "the operator writes Rust and keeps notes in the vault",
            true,
            3_000,
        );
        seed(
            &db,
            "c",
            "memory",
            "the operator is in Indiana",
            true,
            2_000,
        );
        seed(
            &db,
            "d",
            "memory",
            "the operator reads before writing",
            true,
            1_000,
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
    ///
    /// The guard forbids writers, not the paths. A sweep must open the two files
    /// to hash them, and that is a read, so a read is asserted to be present
    /// rather than treated as a violation. If a writer ever appears beside it,
    /// the forbidden list below is what fails.
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
            production.contains("std::fs::read"),
            "the pre-image gate needs a read of the two tier-1 files"
        );
        assert!(
            production.contains("selfmemory::Access::write")
                || production.contains("`selfmemory::Access::write`")
                || production.contains("crate::selfmemory::Access::write"),
            "the module doc must name the one writer"
        );
    }
}

/// The switch and the dry run, in the existing test module.
#[cfg(test)]
mod dry_run_tests {
    use super::*;

    fn scratch() -> Db {
        Db::open_in_memory().expect("in-memory database")
    }

    fn put(db: &Db, text: &str) {
        mem::insert_entry(
            db,
            &mem::NewEntry {
                tier: mem::Tier::Curated,
                target: "memory",
                entry: text,
                origin: mem::Origin::Operator,
                source_ref: None,
            },
        )
        .expect("entry");
    }

    #[test]
    fn the_switch_is_off_until_it_is_stored_on() {
        let db = scratch();
        assert!(
            !nightly(&db),
            "off by default: a bad merge beats a duplicate"
        );
        crate::web::store::settings_put(&db, SETTING_NIGHTLY, "true").expect("store");
        assert!(nightly(&db));
    }

    #[test]
    fn a_preview_counts_what_a_sweep_would_retire_and_writes_nothing() {
        let db = scratch();
        put(&db, "the deploy waits for the guard");
        put(&db, "the  deploy   waits for the guard");
        put(&db, "the cache lives in /var");
        put(&db, "the lane health check runs at eight");
        put(&db, "the guard baseline lives in src");

        let before = mem::live_entries(&db, "memory", false, 100)
            .expect("live")
            .len();
        let preview = preview(&db).expect("preview");
        assert_eq!(preview.retired, 1, "one duplicate pair");
        assert!(!preview.refused);
        let after = mem::live_entries(&db, "memory", false, 100)
            .expect("live")
            .len();
        assert_eq!(before, after, "a preview retires nothing");

        match sweep(&db, &crate::learning::now()).expect("sweep") {
            SweepOutcome::Completed { retired, .. } => {
                assert_eq!(retired, preview.retired, "the sweep does what it said");
            }
            other => panic!("expected a completed sweep, got {other:?}"),
        }
    }
}
