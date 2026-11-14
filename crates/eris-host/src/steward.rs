//! The steward's durable duties and field notes.
//!
//! A duty is a responsibility the operator handed over in their own words. It
//! outlives the session that named it, the daemon restart and the transcript:
//! the row in `steward_duties` is the duty. Every accepted mutation is a
//! compare-and-set on `revision`; a write at a stale revision is refused with
//! the current revision named, never merged.
//!
//! Field notes are the steward's scratch memory. They are append-only: this
//! module exposes no update and no delete for them.
//!
//! `needs_you` is not settable through [`update_duty`]. It is reached only
//! through [`escalate_needs_you`], which the call path uses.

use std::fmt;
use std::str::FromStr;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

use crate::db::{Db, DbError};
use crate::learning::now;

/// The phase a duty is in. `Closed` is terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Watching,
    Working,
    Waiting,
    NeedsYou,
    Paused,
    Closed,
}

impl Phase {
    /// Every phase, in the order the spec lists them.
    pub const ALL: [Phase; 6] = [
        Phase::Watching,
        Phase::Working,
        Phase::Waiting,
        Phase::NeedsYou,
        Phase::Paused,
        Phase::Closed,
    ];

    /// The stored spelling of the phase.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Phase::Watching => "watching",
            Phase::Working => "working",
            Phase::Waiting => "waiting",
            Phase::NeedsYou => "needs_you",
            Phase::Paused => "paused",
            Phase::Closed => "closed",
        }
    }
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Phase {
    type Err = DutyError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Phase::ALL
            .into_iter()
            .find(|phase| phase.as_str() == text)
            .ok_or_else(|| DutyError::InvalidPhase(text.to_owned()))
    }
}

/// Why a paused duty is paused. Only meaningful while the phase is `paused`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocker {
    pub code: String,
    pub detail: String,
}

/// One stored duty, as read back from `steward_duties`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Duty {
    pub id: String,
    pub revision: i64,
    pub created_at: String,
    pub updated_at: String,
    pub statement: String,
    pub phase: Phase,
    pub every_seconds: Option<i64>,
    pub next_wake_at: Option<String>,
    pub blocker: Option<Blocker>,
    pub closed_outcome: Option<String>,
    pub source_conversation: Option<String>,
}

/// A partial update. `None` leaves a field alone; `Some(None)` clears it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DutyPatch {
    pub statement: Option<String>,
    pub phase: Option<Phase>,
    pub every_seconds: Option<Option<i64>>,
    pub next_wake_at: Option<Option<String>>,
    pub blocker: Option<Option<Blocker>>,
}

/// One field note. Notes are never rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldNote {
    pub id: String,
    pub duty_id: Option<String>,
    pub at: String,
    pub text: String,
    pub tags: Vec<String>,
}

/// Everything the duty and note store can refuse or fail with.
#[derive(Debug, thiserror::Error)]
pub enum DutyError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error("duty {0} not found")]
    NotFound(String),
    #[error("duty {0} is closed")]
    Closed(String),
    #[error("stale revision: the current revision is {current}")]
    StaleRevision { current: i64 },
    #[error("phase needs_you is only set through a call")]
    NeedsYouRequiresCall,
    #[error("phase closed is only set through close_duty")]
    CloseRequiresCloseDuty,
    #[error("unknown phase {0}")]
    InvalidPhase(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("stored duty row is corrupt: {0}")]
    Corrupt(String),
}

/// Create a duty in phase `watching` at revision 1.
///
/// `every_seconds`, when set, must be positive and arms `next_wake_at` at
/// now plus that many seconds.
pub fn create_duty(
    db: &Db,
    statement: &str,
    every_seconds: Option<i64>,
) -> Result<Duty, DutyError> {
    let statement = statement.trim();
    if statement.is_empty() {
        return Err(DutyError::Invalid("statement is empty".to_owned()));
    }
    check_cadence(every_seconds)?;
    let created_at = now();
    let next_wake_at = every_seconds.map(wake_after);
    let slug = slugify(statement);
    let id = transact(db, |tx| {
        let mut seq: i64 = tx.query_row(
            "SELECT COALESCE(MAX(rowid), 0) + 1 FROM steward_duties",
            [],
            |row| row.get(0),
        )?;
        let id = loop {
            let candidate = format!("duty:{slug}-{seq}");
            if !duty_exists(tx, &candidate)? {
                break candidate;
            }
            seq += 1;
        };
        tx.execute(
            "INSERT INTO steward_duties (
                id, created_at, updated_at, revision, statement, phase,
                every_seconds, next_wake_at
             ) VALUES (?1, ?2, ?2, 1, ?3, 'watching', ?4, ?5)",
            params![id, created_at, statement, every_seconds, next_wake_at],
        )?;
        Ok(id)
    })?;
    get_duty(db, &id)
}

/// Every duty, oldest first, optionally filtered to one phase.
pub fn list_duties(db: &Db, phase: Option<Phase>) -> Result<Vec<Duty>, DutyError> {
    read(db, |conn| {
        let mut statement = conn.prepare(
            "SELECT id, revision, created_at, updated_at, statement, phase,
                    every_seconds, next_wake_at, blocker_code, blocker_detail,
                    closed_outcome, source_conversation
             FROM steward_duties
             WHERE ?1 IS NULL OR phase = ?1
             ORDER BY rowid ASC",
        )?;
        let phase_text = phase.map(Phase::as_str);
        let rows = statement.query_map(params![phase_text], DutyRow::from_row)?;
        let mut duties = Vec::new();
        for row in rows {
            duties.push(row?.into_duty()?);
        }
        Ok(duties)
    })
}

/// One duty by id.
pub fn get_duty(db: &Db, id: &str) -> Result<Duty, DutyError> {
    read(db, |conn| {
        load(conn, id)?.ok_or_else(|| DutyError::NotFound(id.to_owned()))
    })
}

/// Apply a patch at `expected_revision`.
///
/// Succeeds only when the stored revision equals `expected_revision`, and then
/// bumps it by exactly one. A stale revision is refused with
/// [`DutyError::StaleRevision`] naming the current revision. A closed duty is
/// refused. The patch may not set `needs_you` or `closed`.
pub fn update_duty(
    db: &Db,
    id: &str,
    expected_revision: i64,
    patch: &DutyPatch,
) -> Result<Duty, DutyError> {
    match patch.phase {
        Some(Phase::NeedsYou) => return Err(DutyError::NeedsYouRequiresCall),
        Some(Phase::Closed) => return Err(DutyError::CloseRequiresCloseDuty),
        _ => {}
    }
    if let Some(Some(cadence)) = patch.every_seconds {
        check_cadence(Some(cadence))?;
    }
    if let Some(statement) = &patch.statement
        && statement.trim().is_empty()
    {
        return Err(DutyError::Invalid("statement is empty".to_owned()));
    }
    let updated_at = now();
    transact(db, |tx| {
        let current = load(tx, id)?.ok_or_else(|| DutyError::NotFound(id.to_owned()))?;
        if current.phase == Phase::Closed {
            return Err(DutyError::Closed(id.to_owned()));
        }
        if current.revision != expected_revision {
            return Err(DutyError::StaleRevision {
                current: current.revision,
            });
        }
        if current.phase == Phase::NeedsYou {
            // Leaving needs_you requires a human-sourced turn, which this
            // store cannot see. Refuse rather than clear it silently.
            return Err(DutyError::NeedsYouRequiresCall);
        }
        let phase = patch.phase.unwrap_or(current.phase);
        let statement = patch
            .statement
            .as_deref()
            .map_or(current.statement.clone(), |s| s.trim().to_owned());
        let every_seconds = match patch.every_seconds {
            Some(value) => value,
            None => current.every_seconds,
        };
        let next_wake_at = match (&patch.next_wake_at, patch.every_seconds) {
            (Some(explicit), _) => explicit.clone(),
            (None, Some(Some(seconds))) => Some(wake_after(seconds)),
            (None, Some(None)) => None,
            (None, None) => current.next_wake_at.clone(),
        };
        let blocker = match &patch.blocker {
            Some(Some(_)) if phase != Phase::Paused => {
                return Err(DutyError::Invalid(
                    "a blocker may only be set while the phase is paused".to_owned(),
                ));
            }
            Some(value) => value.clone(),
            // Leaving paused drops the blocker; staying paused keeps it.
            None if phase == Phase::Paused => current.blocker.clone(),
            None => None,
        };
        let (blocker_code, blocker_detail) = match blocker {
            Some(b) => (Some(b.code), Some(b.detail)),
            None => (None, None),
        };
        let changed = tx.execute(
            "UPDATE steward_duties
             SET statement = ?3, phase = ?4, every_seconds = ?5, next_wake_at = ?6,
                 blocker_code = ?7, blocker_detail = ?8, updated_at = ?9,
                 revision = revision + 1
             WHERE id = ?1 AND revision = ?2",
            params![
                id,
                expected_revision,
                statement,
                phase.as_str(),
                every_seconds,
                next_wake_at,
                blocker_code,
                blocker_detail,
                updated_at,
            ],
        )?;
        if changed != 1 {
            return Err(stale_or_missing(tx, id)?);
        }
        Ok(())
    })?;
    get_duty(db, id)
}

/// Move a duty to `needs_you`. This is the only path into that phase.
///
/// The call path uses it. It obeys the same compare-and-set as
/// [`update_duty`].
pub fn escalate_needs_you(db: &Db, id: &str, expected_revision: i64) -> Result<Duty, DutyError> {
    transact(db, |tx| {
        let current = load(tx, id)?.ok_or_else(|| DutyError::NotFound(id.to_owned()))?;
        if current.phase == Phase::Closed {
            return Err(DutyError::Closed(id.to_owned()));
        }
        if current.revision != expected_revision {
            return Err(DutyError::StaleRevision {
                current: current.revision,
            });
        }
        let changed = tx.execute(
            "UPDATE steward_duties
             SET phase = 'needs_you', blocker_code = NULL, blocker_detail = NULL,
                 updated_at = ?3, revision = revision + 1
             WHERE id = ?1 AND revision = ?2",
            params![id, expected_revision, now()],
        )?;
        if changed != 1 {
            return Err(stale_or_missing(tx, id)?);
        }
        Ok(())
    })?;
    get_duty(db, id)
}

/// Close a duty with an outcome. Closing is terminal.
///
/// A duty that is already closed is refused with [`DutyError::Closed`]. The
/// revision check still applies to every open duty.
pub fn close_duty(
    db: &Db,
    id: &str,
    expected_revision: i64,
    outcome: &str,
) -> Result<Duty, DutyError> {
    let outcome = outcome.trim();
    if outcome.is_empty() {
        return Err(DutyError::Invalid("closing outcome is empty".to_owned()));
    }
    transact(db, |tx| {
        let current = load(tx, id)?.ok_or_else(|| DutyError::NotFound(id.to_owned()))?;
        if current.phase == Phase::Closed {
            return Err(DutyError::Closed(id.to_owned()));
        }
        if current.revision != expected_revision {
            return Err(DutyError::StaleRevision {
                current: current.revision,
            });
        }
        let changed = tx.execute(
            "UPDATE steward_duties
             SET phase = 'closed', closed_outcome = ?3, next_wake_at = NULL,
                 blocker_code = NULL, blocker_detail = NULL,
                 updated_at = ?4, revision = revision + 1
             WHERE id = ?1 AND revision = ?2 AND phase <> 'closed'",
            params![id, expected_revision, outcome, now()],
        )?;
        if changed != 1 {
            return Err(stale_or_missing(tx, id)?);
        }
        Ok(())
    })?;
    get_duty(db, id)
}

/// Append a field note. `duty_id` of `None` writes a note about no duty.
pub fn append_note(
    db: &Db,
    duty_id: Option<&str>,
    text: &str,
    tags: &[String],
) -> Result<FieldNote, DutyError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(DutyError::Invalid("note text is empty".to_owned()));
    }
    let id = format!("note:{}", uuid::Uuid::now_v7().simple());
    let at = now();
    let tags_json = serde_json::to_string(tags)
        .map_err(|error| DutyError::Invalid(format!("tags: {error}")))?;
    transact(db, |tx| {
        if let Some(duty) = duty_id
            && !duty_exists(tx, duty)?
        {
            return Err(DutyError::NotFound(duty.to_owned()));
        }
        tx.execute(
            "INSERT INTO steward_field_notes (id, duty_id, at, text, tags)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, duty_id, at, text, tags_json],
        )?;
        Ok(())
    })?;
    Ok(FieldNote {
        id,
        duty_id: duty_id.map(str::to_owned),
        at,
        text: text.to_owned(),
        tags: tags.to_vec(),
    })
}

/// Field notes, newest first, at most `limit`, optionally for one duty.
pub fn list_notes(
    db: &Db,
    limit: usize,
    duty_id: Option<&str>,
) -> Result<Vec<FieldNote>, DutyError> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    read(db, |conn| {
        // rowid is the insertion order; `at` has one-second resolution and
        // cannot order two notes written in the same second.
        let mut statement = conn.prepare(
            "SELECT id, duty_id, at, text, tags
             FROM steward_field_notes
             WHERE ?1 IS NULL OR duty_id = ?1
             ORDER BY rowid DESC
             LIMIT ?2",
        )?;
        let rows = statement.query_map(params![duty_id, limit], |row| {
            let tags: Option<String> = row.get(4)?;
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                tags,
            ))
        })?;
        let mut notes = Vec::new();
        for row in rows {
            let (id, duty_id, at, text, tags) = row?;
            let tags = match tags {
                Some(json) => serde_json::from_str(&json)
                    .map_err(|error| DutyError::Corrupt(format!("note {id} tags: {error}")))?,
                None => Vec::new(),
            };
            notes.push(FieldNote {
                id,
                duty_id,
                at,
                text,
                tags,
            });
        }
        Ok(notes)
    })
}

// ---- internals ----

struct DutyRow {
    id: String,
    revision: i64,
    created_at: String,
    updated_at: String,
    statement: String,
    phase: String,
    every_seconds: Option<i64>,
    next_wake_at: Option<String>,
    blocker_code: Option<String>,
    blocker_detail: Option<String>,
    closed_outcome: Option<String>,
    source_conversation: Option<String>,
}

impl DutyRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            revision: row.get(1)?,
            created_at: row.get(2)?,
            updated_at: row.get(3)?,
            statement: row.get(4)?,
            phase: row.get(5)?,
            every_seconds: row.get(6)?,
            next_wake_at: row.get(7)?,
            blocker_code: row.get(8)?,
            blocker_detail: row.get(9)?,
            closed_outcome: row.get(10)?,
            source_conversation: row.get(11)?,
        })
    }

    fn into_duty(self) -> Result<Duty, DutyError> {
        let phase = Phase::from_str(&self.phase)
            .map_err(|_| DutyError::Corrupt(format!("duty {} phase {}", self.id, self.phase)))?;
        let blocker = match (self.blocker_code, self.blocker_detail) {
            (Some(code), Some(detail)) => Some(Blocker { code, detail }),
            (None, None) => None,
            _ => {
                return Err(DutyError::Corrupt(format!(
                    "duty {} has a half-written blocker",
                    self.id
                )));
            }
        };
        Ok(Duty {
            id: self.id,
            revision: self.revision,
            created_at: self.created_at,
            updated_at: self.updated_at,
            statement: self.statement,
            phase,
            every_seconds: self.every_seconds,
            next_wake_at: self.next_wake_at,
            blocker,
            closed_outcome: self.closed_outcome,
            source_conversation: self.source_conversation,
        })
    }
}

fn load(conn: &Connection, id: &str) -> Result<Option<Duty>, DutyError> {
    let row = conn
        .query_row(
            "SELECT id, revision, created_at, updated_at, statement, phase,
                    every_seconds, next_wake_at, blocker_code, blocker_detail,
                    closed_outcome, source_conversation
             FROM steward_duties WHERE id = ?1",
            params![id],
            DutyRow::from_row,
        )
        .optional()?;
    row.map(DutyRow::into_duty).transpose()
}

fn duty_exists(conn: &Connection, id: &str) -> Result<bool, DutyError> {
    let found: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM steward_duties WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(found.is_some())
}

/// After a guarded UPDATE touched no row, say why: missing, closed or stale.
fn stale_or_missing(conn: &Connection, id: &str) -> Result<DutyError, DutyError> {
    Ok(match load(conn, id)? {
        None => DutyError::NotFound(id.to_owned()),
        Some(duty) if duty.phase == Phase::Closed => DutyError::Closed(id.to_owned()),
        Some(duty) => DutyError::StaleRevision {
            current: duty.revision,
        },
    })
}

fn check_cadence(every_seconds: Option<i64>) -> Result<(), DutyError> {
    match every_seconds {
        Some(seconds) if seconds <= 0 => Err(DutyError::Invalid(
            "every_seconds must be positive".to_owned(),
        )),
        _ => Ok(()),
    }
}

/// An absolute RFC 3339 UTC instant `seconds` from now, in the `now()` format.
fn wake_after(seconds: i64) -> String {
    (OffsetDateTime::now_utc() + Duration::seconds(seconds))
        .format(&Rfc3339)
        .map(|text| text.replace("+00:00", "Z"))
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// A readable id stem: the first few words of the statement, lowercased and
/// hyphen-joined, capped in length.
fn slugify(statement: &str) -> String {
    let mut slug = String::new();
    for word in statement
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(5)
    {
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word.to_ascii_lowercase());
    }
    slug.truncate(40);
    let slug = slug.trim_end_matches('-').to_owned();
    if slug.is_empty() {
        "duty".to_owned()
    } else {
        slug
    }
}

/// Run a read against the connection.
fn read<T>(db: &Db, run: impl FnOnce(&Connection) -> Result<T, DutyError>) -> Result<T, DutyError> {
    db.with_conn(|conn| Ok(run(conn))).map_err(DutyError::Db)?
}

/// Run a write in one transaction. A refusal rolls the whole thing back.
fn transact<T>(
    db: &Db,
    run: impl FnOnce(&Transaction<'_>) -> Result<T, DutyError>,
) -> Result<T, DutyError> {
    db.with_conn(|conn| {
        let tx = conn.transaction()?;
        match run(&tx) {
            Ok(value) => {
                tx.commit()?;
                Ok(Ok(value))
            }
            Err(error) => Ok(Err(error)),
        }
    })
    .map_err(DutyError::Db)?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory_db() -> Db {
        Db::open_in_memory().expect("in-memory db")
    }

    #[test]
    fn create_starts_watching_at_revision_one_with_stable_id() {
        let db = memory_db();
        let duty = create_duty(&db, "Watch the backup job", None).expect("create");
        assert!(
            duty.id.starts_with("duty:watch-the-backup-job-"),
            "{}",
            duty.id
        );
        assert_eq!(duty.revision, 1);
        assert_eq!(duty.phase, Phase::Watching);
        assert_eq!(duty.next_wake_at, None);
        let other = create_duty(&db, "Watch the backup job", None).expect("second");
        assert_ne!(duty.id, other.id, "ids are never reused");
    }

    #[test]
    fn cadence_arms_next_wake_and_rejects_non_positive() {
        let db = memory_db();
        let duty = create_duty(&db, "Check the queue", Some(300)).expect("create");
        assert_eq!(duty.every_seconds, Some(300));
        assert!(duty.next_wake_at.is_some());
        assert!(matches!(
            create_duty(&db, "Check the queue", Some(0)),
            Err(DutyError::Invalid(_))
        ));
    }

    #[test]
    fn revision_increments_by_exactly_one_per_mutation() {
        let db = memory_db();
        let duty = create_duty(&db, "Track the renewal", None).expect("create");
        let first = update_duty(
            &db,
            &duty.id,
            1,
            &DutyPatch {
                phase: Some(Phase::Working),
                ..DutyPatch::default()
            },
        )
        .expect("first update");
        assert_eq!(first.revision, 2);
        let second = update_duty(
            &db,
            &duty.id,
            2,
            &DutyPatch {
                statement: Some("Track the renewal date".to_owned()),
                ..DutyPatch::default()
            },
        )
        .expect("second update");
        assert_eq!(second.revision, 3);
        assert_eq!(second.statement, "Track the renewal date");
    }

    #[test]
    fn stale_revision_is_refused_and_names_current_revision() {
        let db = memory_db();
        let duty = create_duty(&db, "Keep the inbox short", None).expect("create");
        update_duty(
            &db,
            &duty.id,
            1,
            &DutyPatch {
                phase: Some(Phase::Waiting),
                ..DutyPatch::default()
            },
        )
        .expect("advance to 2");
        let stale = update_duty(
            &db,
            &duty.id,
            1,
            &DutyPatch {
                phase: Some(Phase::Working),
                ..DutyPatch::default()
            },
        );
        match stale {
            Err(DutyError::StaleRevision { current }) => assert_eq!(current, 2),
            other => panic!("expected StaleRevision, got {other:?}"),
        }
        let unchanged = get_duty(&db, &duty.id).expect("read");
        assert_eq!(unchanged.revision, 2, "a refused write changes nothing");
        assert_eq!(unchanged.phase, Phase::Waiting);
    }

    #[test]
    fn needs_you_is_not_settable_through_update_or_stale_escalation() {
        let db = memory_db();
        let duty = create_duty(&db, "Approve the invoice run", None).expect("create");
        let refused = update_duty(
            &db,
            &duty.id,
            1,
            &DutyPatch {
                phase: Some(Phase::NeedsYou),
                ..DutyPatch::default()
            },
        );
        assert!(matches!(refused, Err(DutyError::NeedsYouRequiresCall)));
        let raised = escalate_needs_you(&db, &duty.id, 1).expect("escalate");
        assert_eq!(raised.phase, Phase::NeedsYou);
        assert_eq!(raised.revision, 2);
        assert!(matches!(
            escalate_needs_you(&db, &duty.id, 1),
            Err(DutyError::StaleRevision { current: 2 })
        ));
    }

    #[test]
    fn blocker_requires_paused_phase() {
        let db = memory_db();
        let duty = create_duty(&db, "Sync the calendar", None).expect("create");
        let blocker = Blocker {
            code: "auth".to_owned(),
            detail: "token expired".to_owned(),
        };
        assert!(matches!(
            update_duty(
                &db,
                &duty.id,
                1,
                &DutyPatch {
                    blocker: Some(Some(blocker.clone())),
                    ..DutyPatch::default()
                },
            ),
            Err(DutyError::Invalid(_))
        ));
        let paused = update_duty(
            &db,
            &duty.id,
            1,
            &DutyPatch {
                phase: Some(Phase::Paused),
                blocker: Some(Some(blocker.clone())),
                ..DutyPatch::default()
            },
        )
        .expect("pause with blocker");
        assert_eq!(paused.blocker, Some(blocker));
    }

    #[test]
    fn close_is_terminal_and_closing_a_closed_duty_is_refused() {
        let db = memory_db();
        let duty = create_duty(&db, "Renew the domain", Some(3600)).expect("create");
        let closed = close_duty(&db, &duty.id, 1, "renewed through 2027").expect("close");
        assert_eq!(closed.phase, Phase::Closed);
        assert_eq!(closed.revision, 2);
        assert_eq!(
            closed.closed_outcome.as_deref(),
            Some("renewed through 2027")
        );
        assert_eq!(closed.next_wake_at, None);

        assert!(matches!(
            close_duty(&db, &duty.id, 2, "again"),
            Err(DutyError::Closed(_))
        ));
        assert!(matches!(
            update_duty(
                &db,
                &duty.id,
                2,
                &DutyPatch {
                    phase: Some(Phase::Working),
                    ..DutyPatch::default()
                },
            ),
            Err(DutyError::Closed(_))
        ));
        assert!(matches!(
            update_duty(
                &db,
                &duty.id,
                2,
                &DutyPatch {
                    phase: Some(Phase::Closed),
                    ..DutyPatch::default()
                },
            ),
            Err(DutyError::CloseRequiresCloseDuty)
        ));
        assert_eq!(get_duty(&db, &duty.id).expect("read").revision, 2);
    }

    #[test]
    fn list_duties_filters_by_phase() {
        let db = memory_db();
        let open = create_duty(&db, "Open one", None).expect("create");
        let done = create_duty(&db, "Done one", None).expect("create");
        close_duty(&db, &done.id, 1, "done").expect("close");
        let watching = list_duties(&db, Some(Phase::Watching)).expect("list");
        assert_eq!(watching.len(), 1);
        assert_eq!(watching[0].id, open.id);
        assert_eq!(list_duties(&db, None).expect("all").len(), 2);
    }

    #[test]
    fn notes_are_append_only_and_listed_newest_first() {
        let db = memory_db();
        let duty = create_duty(&db, "Watch the lease", None).expect("create");
        let first =
            append_note(&db, Some(&duty.id), "first", &["lease".to_owned()]).expect("first note");
        let second = append_note(&db, Some(&duty.id), "second", &[]).expect("second note");
        let loose = append_note(&db, None, "unattached", &[]).expect("loose note");

        let all = list_notes(&db, 10, None).expect("all notes");
        let ids: Vec<&str> = all.iter().map(|note| note.id.as_str()).collect();
        assert_eq!(
            ids,
            [loose.id.as_str(), second.id.as_str(), first.id.as_str()]
        );
        assert_eq!(all[2].tags, vec!["lease".to_owned()]);

        let scoped = list_notes(&db, 10, Some(&duty.id)).expect("scoped");
        assert_eq!(scoped.len(), 2);
        assert_eq!(scoped[0].text, "second");
        assert_eq!(list_notes(&db, 1, None).expect("limited").len(), 1);

        assert!(matches!(
            append_note(&db, Some("duty:missing-99"), "orphan", &[]),
            Err(DutyError::NotFound(_))
        ));
    }

    #[test]
    fn state_survives_reopening_the_database_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("eris.db");
        let (duty_id, note_id) = {
            let db = Db::open(&path).expect("open");
            let duty = create_duty(&db, "Keep the badge list current", Some(900)).expect("create");
            let updated = update_duty(
                &db,
                &duty.id,
                1,
                &DutyPatch {
                    phase: Some(Phase::Waiting),
                    ..DutyPatch::default()
                },
            )
            .expect("update");
            assert_eq!(updated.revision, 2);
            let note = append_note(&db, Some(&duty.id), "waiting on HR", &[]).expect("note");
            (duty.id, note.id)
        };
        let db = Db::open(&path).expect("reopen");
        let duty = get_duty(&db, &duty_id).expect("duty after reopen");
        assert_eq!(duty.revision, 2);
        assert_eq!(duty.phase, Phase::Waiting);
        assert_eq!(duty.every_seconds, Some(900));
        let notes = list_notes(&db, 5, Some(&duty_id)).expect("notes after reopen");
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, note_id);
    }
}
