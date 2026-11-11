//! The memory tiers below the curated files: typed access, no policy.
//!
//! Nothing here decides whether a write is allowed, when an intent is due, or what
//! a sweep should keep. Those live with their own fixtures: `selfmemory` owns the
//! two files, `memintent` owns the cadence gate, `memconsolidate` owns the sweep.
//! This module is the table side, so a query in a caller cannot disagree with
//! migration 0029 about a column name or a CHECK spelling.
//!
//! Two rules are structural rather than documented:
//!
//! - **Provenance is a column.** `origin`, `trusted` and `source_ref` are stored,
//!   so "what the model wrote" and "what the operator kept" are SQL predicates
//!   rather than a Rust filter after a fetch.
//! - **A claim is adopted, not inserted.** `claim_sweep` adopts a stale `running`
//!   row inside one immediate transaction. There is no unique index to make a
//!   crashed sweep a permanent latch.

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::db::{Db, DbError};

/// Which store an entry belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Mirrored from `memory/MEMORY.md` or `memory/USER.md`; the file is
    /// authoritative and this row is the projection that carries provenance.
    Curated,
    /// Written by a consolidation sweep. Never read back into a file.
    Consolidated,
}

impl Tier {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Curated => "curated",
            Self::Consolidated => "consolidated",
        }
    }
}

/// Where an entry came from. A closed set, mirrored by a CHECK in the schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The operator typed it.
    Operator,
    /// A model wrote it through `memory_manage`.
    Model,
    /// A tool result was folded in.
    ToolResult,
    /// Arrived from a file outside this install.
    Import,
    /// A sweep produced it.
    Consolidation,
}

impl Origin {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Model => "model",
            Self::ToolResult => "tool_result",
            Self::Import => "import",
            Self::Consolidation => "consolidation",
        }
    }
}

/// A row on its way in.
#[derive(Debug, Clone)]
pub struct NewEntry<'a> {
    /// Which tier.
    pub tier: Tier,
    /// `"memory"` or `"user"`, the same spelling [`crate::selfmemory::Target`]
    /// uses for its file.
    pub target: &'a str,
    /// The entry itself. The schema caps it at 400 characters.
    pub entry: &'a str,
    /// Where it came from.
    pub origin: Origin,
    /// Which conversation, tool call or file it came from, when there is one.
    pub source_ref: Option<&'a str>,
    /// Whether a recall may surface it. Imported content is never trusted.
    pub trusted: bool,
}

/// A row on its way out.
#[derive(Debug, Clone)]
pub struct Entry {
    /// Primary key.
    pub id: String,
    /// `"curated"` or `"consolidated"`.
    pub tier: String,
    /// `"memory"` or `"user"`.
    pub target: String,
    /// The entry itself.
    pub entry: String,
    /// `origin` as stored.
    pub origin: String,
    /// Where it came from, when known.
    pub source_ref: Option<String>,
    /// Whether a recall may surface it.
    pub trusted: bool,
    /// SHA-256 of the entry text, which is what deduplicates a recall.
    pub content_hash: String,
    /// When it arrived.
    pub created_at: String,
    /// The entry that replaced it, when it was consolidated away.
    pub superseded_by: Option<String>,
}

/// One scheduling intent, as stored.
#[derive(Debug, Clone)]
pub struct Intent {
    /// Primary key.
    pub id: String,
    /// Stable name, unique across the table.
    pub name: String,
    /// The prompt body a fire turns into a turn.
    pub body: String,
    /// How long between fires, in seconds. This is the only cadence column.
    pub cooldown_secs: i64,
    /// How many unsuppressed fires are allowed per window.
    pub budget_per_window: i64,
    /// The window the budget counts over.
    pub window_secs: i64,
}

/// The current UTC timestamp, in the format the database stamps.
#[must_use]
pub fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map(|text| text.replace("+00:00", "Z"))
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// SHA-256 of an entry, hex encoded.
///
/// The same text hashes to the same value under either tier-1 file, and the
/// `UNIQUE (target, content_hash)` pair is what makes a duplicate a no-op rather
/// than a second copy.
#[must_use]
pub fn content_hash(entry: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(entry.as_bytes());
    hex::encode(hasher.finalize())
}

/// Store one entry. A duplicate in the same target changes nothing.
///
/// # Errors
///
/// A database failure, or a row the CHECK constraints refuse.
pub fn insert_entry(db: &Db, new: &NewEntry<'_>) -> Result<(), DbError> {
    let hash = content_hash(new.entry);
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO mem_entries
                 (id, tier, target, entry, origin, source_ref, trusted, content_hash, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (target, content_hash) DO NOTHING",
            params![
                uuid::Uuid::now_v7().to_string(),
                new.tier.as_str(),
                new.target,
                new.entry,
                new.origin.as_str(),
                new.source_ref,
                i64::from(new.trusted),
                hash,
                now(),
            ],
        )?;
        Ok(())
    })
}

/// The live entries for one target, newest first.
///
/// A live entry is one nothing has superseded. `trusted_only` is the recall
/// predicate: imported content stays in the table and stays out of an answer.
///
/// # Errors
///
/// A database failure, or a stored row the mapping cannot read.
pub fn live_entries(
    db: &Db,
    target: &str,
    trusted_only: bool,
    limit: usize,
) -> Result<Vec<Entry>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, tier, target, entry, origin, source_ref, trusted, content_hash,
                    created_at, superseded_by
               FROM mem_entries
              WHERE target = ?1 AND superseded_by IS NULL
                AND (?2 = 0 OR trusted = 1)
              ORDER BY created_at DESC, id DESC
              LIMIT ?3",
        )?;
        let rows = statement.query_map(
            params![target, i64::from(trusted_only), limit as i64],
            |row| {
                Ok(Entry {
                    id: row.get(0)?,
                    tier: row.get(1)?,
                    target: row.get(2)?,
                    entry: row.get(3)?,
                    origin: row.get(4)?,
                    source_ref: row.get(5)?,
                    trusted: row.get::<_, i64>(6)? != 0,
                    content_hash: row.get(7)?,
                    created_at: row.get(8)?,
                    superseded_by: row.get(9)?,
                })
            },
        )?;
        let mut entries = Vec::new();
        for row in rows {
            entries.push(row?);
        }
        Ok(entries)
    })
}

/// Retire one entry in favour of another. Returns whether a row moved.
///
/// # Errors
///
/// A database failure.
pub fn supersede_entry(db: &Db, id: &str, by: &str) -> Result<bool, DbError> {
    db.with_conn(|conn| {
        let moved = conn.execute(
            "UPDATE mem_entries SET superseded_by = ?2
              WHERE id = ?1 AND superseded_by IS NULL",
            params![id, by],
        )?;
        Ok(moved > 0)
    })
}

/// Claim the next sweep, adopting a stale one instead of failing on it.
///
/// Returns the sweep id, or `None` when another sweep is live and healthy. The
/// whole decision is one immediate transaction, so two workers cannot both decide
/// they are the claimant.
///
/// # Errors
///
/// A database failure.
pub fn claim_sweep(db: &Db, stale_before: &str) -> Result<Option<String>, DbError> {
    db.with_conn(|conn| {
        let transaction = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let stamp = now();

        // A claim whose heartbeat is older than the caller's freshness bound is
        // the row a crash left behind. Adopting it is the recovery: no manual SQL,
        // and the attempt counter records that it happened.
        let stale: Option<String> = transaction
            .query_row(
                "SELECT id FROM mem_sweeps
                  WHERE outcome = 'running' AND heartbeat_at < ?1
                  ORDER BY heartbeat_at ASC
                  LIMIT 1",
                [stale_before],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = stale {
            transaction.execute(
                "UPDATE mem_sweeps
                    SET claimed_at = ?1, heartbeat_at = ?1, attempt = attempt + 1
                  WHERE id = ?2 AND outcome = 'running'",
                params![stamp, id],
            )?;
            transaction.commit()?;
            return Ok(Some(id));
        }

        // A running row inside the bound belongs to somebody alive.
        let live: Option<String> = transaction
            .query_row(
                "SELECT id FROM mem_sweeps WHERE outcome = 'running' LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if live.is_some() {
            transaction.commit()?;
            return Ok(None);
        }

        // The pre-image is taken at claim time, over what is live right now, so a
        // finished sweep can be checked against the entries it says it saw.
        let (preimage, live_count) = preimage(&transaction)?;
        let id = uuid::Uuid::now_v7().to_string();
        transaction.execute(
            "INSERT INTO mem_sweeps
                 (id, outcome, claimed_at, heartbeat_at, preimage_sha256, entries_in,
                  entries_out, attempt, note)
             VALUES (?1, 'running', ?2, ?2, ?3, ?4, 0, 0, NULL)",
            params![id, stamp, preimage, live_count],
        )?;
        transaction.commit()?;
        Ok(Some(id))
    })
}

/// SHA-256 over the live entry set, and how many rows fed it.
fn preimage(conn: &Connection) -> rusqlite::Result<(String, i64)> {
    let mut statement = conn.prepare(
        "SELECT id, entry FROM mem_entries
          WHERE superseded_by IS NULL
          ORDER BY id ASC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut hasher = Sha256::new();
    let mut count = 0i64;
    for row in rows {
        let (id, entry) = row?;
        hasher.update(id.as_bytes());
        hasher.update([0u8]);
        hasher.update(entry.as_bytes());
        hasher.update([0u8]);
        count += 1;
    }
    Ok((hex::encode(hasher.finalize()), count))
}

/// Move a running sweep's heartbeat forward. Returns whether the row was still
/// running, which is how a sweep learns it was reclaimed out from under it.
///
/// # Errors
///
/// A database failure.
pub fn heartbeat(db: &Db, id: &str) -> Result<bool, DbError> {
    db.with_conn(|conn| {
        let touched = conn.execute(
            "UPDATE mem_sweeps SET heartbeat_at = ?2
              WHERE id = ?1 AND outcome = 'running'",
            params![id, now()],
        )?;
        Ok(touched > 0)
    })
}

/// Settle a sweep. `outcome` is one of `completed`, `failed` or `abandoned`.
///
/// # Errors
///
/// A database failure, or an outcome outside the closed set.
pub fn finish(
    db: &Db,
    id: &str,
    outcome: &str,
    entries_out: i64,
    note: Option<&str>,
) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE mem_sweeps
                SET outcome = ?2, finished_at = ?3, heartbeat_at = ?3,
                    entries_out = ?4, note = ?5
              WHERE id = ?1",
            params![id, outcome, now(), entries_out, note],
        )?;
        Ok(())
    })
}

/// Every enabled interval intent, in creation order.
///
/// Manual intents are excluded here rather than in the caller: an intent that
/// fires only when the operator asks is not a candidate for a timer, and a
/// caller that had to remember that would eventually forget.
///
/// # Errors
///
/// A database failure, or a row the mapping cannot read.
pub fn interval_intents(db: &Db) -> Result<Vec<Intent>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, name, body, cooldown_secs, budget_per_window, window_secs
               FROM mem_intents
              WHERE enabled = 1 AND trigger_kind = 'interval'
              ORDER BY created_at ASC, id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Intent {
                id: row.get(0)?,
                name: row.get(1)?,
                body: row.get(2)?,
                cooldown_secs: row.get(3)?,
                budget_per_window: row.get(4)?,
                window_secs: row.get(5)?,
            })
        })?;
        let mut intents = Vec::new();
        for row in rows {
            intents.push(row?);
        }
        Ok(intents)
    })
}

/// The most recent fire for an intent, suppressed or not.
///
/// This is the cooldown question: any fire resets the clock, because a suppressed
/// fire is still the moment the intent was evaluated and found not yet due.
///
/// # Errors
///
/// A database failure.
pub fn last_fire_at(db: &Db, intent_id: &str) -> Result<Option<String>, DbError> {
    db.with_conn(|conn| {
        let stamp: Option<String> = conn
            .query_row(
                "SELECT MAX(fired_at) FROM mem_intents_fires WHERE intent_id = ?1",
                [intent_id],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        Ok(stamp)
    })
}

/// How many fires actually started a turn since `since`.
///
/// Suppressed rows are excluded here on purpose: a fire the cooldown held back did
/// not spend budget, or an intent that is never allowed to fire would still burn
/// its window on the ticks that refused it.
///
/// # Errors
///
/// A database failure.
pub fn unsuppressed_fires_since(db: &Db, intent_id: &str, since: &str) -> Result<i64, DbError> {
    db.with_conn(|conn| {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM mem_intents_fires
              WHERE intent_id = ?1 AND suppressed = 0 AND fired_at >= ?2",
            params![intent_id, since],
            |row| row.get(0),
        )?;
        Ok(count)
    })
}

/// Record one fire. Suppressed rows are recorded too: the cooldown gate reads
/// them, and an intent that keeps refusing is a fact worth having in the table.
///
/// # Errors
///
/// A database failure.
pub fn record_fire(
    db: &Db,
    intent_id: &str,
    suppressed: bool,
    reason: Option<&str>,
    turn_id: Option<&str>,
) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO mem_intents_fires (id, intent_id, fired_at, suppressed, reason, turn_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                uuid::Uuid::now_v7().to_string(),
                intent_id,
                now(),
                i64::from(suppressed),
                reason,
                turn_id,
            ],
        )?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("in-memory database")
    }

    fn new_entry<'a>(entry: &'a str, trusted: bool) -> NewEntry<'a> {
        NewEntry {
            tier: Tier::Consolidated,
            target: "memory",
            entry,
            origin: Origin::Consolidation,
            source_ref: Some("sweep-1"),
            trusted,
        }
    }

    /// Provenance is a column, so the recall predicate is SQL: an untrusted row
    /// stays in the table and stays out of the trusted read, and the same text
    /// stored twice in one target is one row.
    #[test]
    fn provenance_is_a_column_and_a_duplicate_is_a_no_op() {
        let db = db();
        insert_entry(&db, &new_entry("the operator prefers short answers", true)).expect("insert");
        insert_entry(&db, &new_entry("imported from another install", false)).expect("insert");
        insert_entry(&db, &new_entry("imported from another install", false)).expect("duplicate");
        insert_entry(
            &db,
            &NewEntry {
                tier: Tier::Curated,
                target: "user",
                entry: "imported from another install",
                origin: Origin::Import,
                source_ref: Some("/tmp/USER.md"),
                trusted: false,
            },
        )
        .expect("same text, other target");

        let trusted = live_entries(&db, "memory", true, 50).expect("trusted");
        assert_eq!(trusted.len(), 1, "{trusted:?}");
        assert_eq!(trusted[0].origin, "consolidation");
        assert!(trusted[0].trusted);

        let all = live_entries(&db, "memory", false, 50).expect("all");
        assert_eq!(all.len(), 2, "the untrusted row is stored, not dropped");

        let user = live_entries(&db, "user", false, 50).expect("user");
        assert_eq!(user.len(), 1, "the unique key is per target, not per text");
        assert_eq!(user[0].origin, "import");
    }

    /// A live claim is somebody else's sweep. A stale one is adopted, in place,
    /// with the attempt counter moving and no second row created.
    #[test]
    fn a_live_claim_is_refused_and_a_stale_one_is_adopted() {
        let db = db();
        insert_entry(&db, &new_entry("a fact the sweep will see", true)).expect("insert");

        let first = claim_sweep(&db, "1970-01-01T00:00:00Z")
            .expect("claim")
            .expect("a fresh table has no live claim");
        assert!(
            claim_sweep(&db, "1970-01-01T00:00:00Z")
                .expect("claim")
                .is_none(),
            "a healthy running sweep must not be adopted"
        );

        // Everything is stale at that bound: the same row comes back, not a new one.
        let adopted = claim_sweep(&db, "2999-01-01T00:00:00Z").expect("claim");
        assert_eq!(
            adopted.as_deref(),
            Some(first.as_str()),
            "adoption reuses the row"
        );
        assert!(heartbeat(&db, &first).expect("heartbeat"));
        finish(&db, &first, "completed", 1, None).expect("finish");

        let sweeps: i64 = db
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM mem_sweeps", [], |row| row.get(0))
            })
            .expect("count");
        assert_eq!(sweeps, 1, "claims are adopted, never duplicated");
    }
}
