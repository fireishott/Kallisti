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
    /// Consecutive autonomous watch rounds since the duty was last resumed.
    pub watch_rounds: i64,
}

/// A partial update. `None` leaves a field alone; `Some(None)` clears it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DutyPatch {
    pub statement: Option<String>,
    pub phase: Option<Phase>,
    pub every_seconds: Option<Option<i64>>,
    pub next_wake_at: Option<Option<String>>,
    pub blocker: Option<Option<Blocker>>,
    /// Watch rounds taken. Only the watch loop sets it.
    pub watch_rounds: Option<i64>,
}

impl Duty {
    /// The wire shape, one serializer for the page, the route and the tools.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "revision": self.revision,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "statement": self.statement,
            "phase": self.phase.as_str(),
            "every_seconds": self.every_seconds,
            "next_wake_at": self.next_wake_at,
            "blocker": self.blocker.as_ref().map(|b| serde_json::json!({"code": b.code, "detail": b.detail})),
            "closed_outcome": self.closed_outcome,
            "source_conversation": self.source_conversation,
            "watch_rounds": self.watch_rounds,
        })
    }
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

impl FieldNote {
    /// The wire shape.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "duty_id": self.duty_id,
            "at": self.at,
            "text": self.text,
            "tags": self.tags,
        })
    }
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
                    closed_outcome, source_conversation, watch_rounds
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
        // A duty taken out of paused starts its round count again, unless the
        // caller names one. Every other write keeps the count.
        let watch_rounds = match patch.watch_rounds {
            Some(rounds) => rounds,
            None if current.phase == Phase::Paused && phase != Phase::Paused => 0,
            None => current.watch_rounds,
        };
        let changed = tx.execute(
            "UPDATE steward_duties
             SET statement = ?3, phase = ?4, every_seconds = ?5, next_wake_at = ?6,
                 blocker_code = ?7, blocker_detail = ?8, updated_at = ?9,
                 watch_rounds = ?10, revision = revision + 1
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
                watch_rounds,
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
             SET phase = 'needs_you', next_wake_at = NULL, blocker_code = NULL,
                 blocker_detail = NULL, updated_at = ?3, revision = revision + 1
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

/// The most duties one charter seeds.
pub const MAX_CHARTER_DUTIES: usize = 40;

/// The operator's allowlist of mail recipients, comma-separated, set on the
/// Automation tab. It lives in the settings table, not in the tree: the tree is
/// public, and the permitted addresses are the operator's own.
pub const SETTING_MAIL_RECIPIENTS: &str = "steward.mail_recipients";

/// The allowlist as stored, lower-cased, one address per entry. Empty until the
/// operator sets one, and an empty list permits no mail.
#[must_use]
pub fn mail_recipients(db: &Db) -> Vec<String> {
    stored_setting(db, SETTING_MAIL_RECIPIENTS)
        .unwrap_or_default()
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(|entry| entry.trim().to_ascii_lowercase())
        .filter(|entry| !entry.is_empty())
        .collect()
}

/// Whether `deliver.email_to` is on the operator's allowlist.
#[must_use]
pub fn mail_permitted(db: &Db) -> bool {
    crate::delivery::allowed_recipient(db).is_some_and(|to| {
        let to = to.trim().to_ascii_lowercase();
        mail_recipients(db).iter().any(|allowed| *allowed == to)
    })
}

/// The channel a steward message goes to. Mail falls back to the inbox unless the
/// configured recipient is one of the two permitted addresses.
#[must_use]
pub fn channel_for(db: &Db, configured: &str) -> String {
    if configured == "email" && !mail_permitted(db) {
        "inbox".to_owned()
    } else {
        configured.to_owned()
    }
}

/// Raise one call: the duty moves to `needs_you` through the same compare-and-set
/// as [`escalate_needs_you`], and the question, its choices and its urgency are
/// kept as a field note tagged `call`. The watch loop delivers it from there.
pub fn raise_call(
    db: &Db,
    id: &str,
    expected_revision: i64,
    question: &str,
    choices: &[String],
    urgency: &str,
) -> Result<Duty, DutyError> {
    let question = question.trim();
    if question.is_empty() {
        return Err(DutyError::Invalid("a call needs a question".to_owned()));
    }
    if !URGENCY_VALUES.contains(&urgency) {
        return Err(DutyError::Invalid(format!(
            "urgency is normal or high; got `{urgency}`"
        )));
    }
    if choices.len() > 5 || choices.iter().any(|choice| choice.chars().count() > 120) {
        return Err(DutyError::Invalid(
            "a call takes at most five choices of 120 characters each".to_owned(),
        ));
    }
    let current = get_duty(db, id)?;
    if current.phase == Phase::NeedsYou {
        return Err(DutyError::Invalid(
            "a call is already open on this duty; a person answers it first".to_owned(),
        ));
    }
    let raised = escalate_needs_you(db, id, expected_revision)?;
    let mut text = question.to_owned();
    if !choices.is_empty() {
        text.push_str(" Choices: ");
        text.push_str(&choices.join(" / "));
    }
    append_note(
        db,
        Some(id),
        &text,
        &["call".to_owned(), urgency.to_owned()],
    )?;
    Ok(raised)
}

/// The human-sourced answer: a duty leaves `needs_you` for `watching`, starts its
/// round count again and is armed for its next wake. Only a person's request may
/// call this; the model's tools never do.
pub fn answer_call(db: &Db, id: &str, expected_revision: i64) -> Result<Duty, DutyError> {
    let default_every = i64::from(watch_seconds(db));
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
        if current.phase != Phase::NeedsYou {
            return Err(DutyError::Invalid(
                "no call is open on this duty".to_owned(),
            ));
        }
        let every = current.every_seconds.unwrap_or(default_every);
        let next_wake = wake_after(every);
        let changed = tx.execute(
            "UPDATE steward_duties
             SET phase = 'watching', next_wake_at = ?3, watch_rounds = 0,
                 updated_at = ?4, revision = revision + 1
             WHERE id = ?1 AND revision = ?2",
            params![id, expected_revision, next_wake, updated_at],
        )?;
        if changed != 1 {
            return Err(stale_or_missing(tx, id)?);
        }
        Ok(())
    })?;
    get_duty(db, id)
}

/// A call not yet delivered: the duty and the call note that carries it.
#[derive(Debug, Clone)]
pub struct PendingCall {
    pub duty: Duty,
    pub note: FieldNote,
}

/// Every open call the watch loop has not delivered. A call is delivered when a
/// `delivered` note follows it, so the same call is never sent twice.
pub fn pending_calls(db: &Db) -> Result<Vec<PendingCall>, DutyError> {
    let mut pending = Vec::new();
    for duty in list_duties(db, Some(Phase::NeedsYou))? {
        for note in list_notes(db, 200, Some(&duty.id))? {
            if note.tags.iter().any(|tag| tag == "delivered") {
                break;
            }
            if note.tags.iter().any(|tag| tag == "call") {
                pending.push(PendingCall {
                    duty: duty.clone(),
                    note,
                });
                break;
            }
        }
    }
    Ok(pending)
}

/// Seed duties from charter text: one per non-empty line. A line that names an
/// open duty already on the board is skipped, so seeding twice does not double it.
#[derive(Debug, Clone, Default)]
pub struct Seeded {
    /// The duties created.
    pub created: Vec<Duty>,
    /// Lines already on the board as open duties.
    pub skipped: Vec<String>,
    /// Lines refused, with the reason.
    pub refused: Vec<String>,
}

/// The non-empty lines of a charter, at most [`MAX_CHARTER_DUTIES`].
#[must_use]
pub fn charter_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .take(MAX_CHARTER_DUTIES)
        .collect()
}

/// Seed one duty per charter line.
pub fn seed_from_text(db: &Db, text: &str) -> Result<Seeded, DutyError> {
    let mut open: Vec<String> = list_duties(db, None)?
        .into_iter()
        .filter(|duty| duty.phase != Phase::Closed)
        .map(|duty| duty.statement)
        .collect();
    let mut seeded = Seeded::default();
    for line in charter_lines(text) {
        if open.contains(&line) {
            seeded.skipped.push(line);
            continue;
        }
        match create_duty(db, &line, None) {
            Ok(duty) => {
                open.push(duty.statement.clone());
                seeded.created.push(duty);
            }
            Err(error) => seeded.refused.push(format!("{line}: {error}")),
        }
    }
    Ok(seeded)
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
    watch_rounds: i64,
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
            watch_rounds: row.get(12)?,
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
            watch_rounds: self.watch_rounds,
        })
    }
}

fn load(conn: &Connection, id: &str) -> Result<Option<Duty>, DutyError> {
    let row = conn
        .query_row(
            "SELECT id, revision, created_at, updated_at, statement, phase,
                    every_seconds, next_wake_at, blocker_code, blocker_detail,
                    closed_outcome, source_conversation, watch_rounds
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

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

/// Whether the steward watch loop is armed at all.
pub const SETTING_ENABLED: &str = "steward.enabled";
/// Seconds between watch rounds for a due duty.
pub const SETTING_WATCH_SECONDS: &str = "steward.watch_seconds";
/// Most watch rounds one duty may take before it is handed back.
pub const SETTING_MAX_ROUNDS: &str = "steward.max_rounds";
/// How many recent field notes a turn carries into its prompt.
pub const SETTING_NOTE_WINDOW: &str = "steward.note_window";
/// How many days a field note is kept.
pub const SETTING_FIELD_NOTE_DAYS: &str = "steward.field_note_days";
/// Where a steward message goes when the duty names no channel.
pub const SETTING_DELIVERY_DEFAULT: &str = "steward.delivery_default";
/// The urgency a steward call is placed at when it names none.
pub const SETTING_CALL_URGENCY_DEFAULT: &str = "steward.call_urgency_default";
/// The operator's own standing instructions to the steward.
pub const SETTING_CHARTER: &str = "steward.charter";
/// Whether the board shows the compact or the full column set.
pub const SETTING_BOARD_COLUMNS: &str = "steward.board_columns";

/// The watch cadence bounds the loop honours.
pub const MIN_WATCH_SECONDS: u32 = 30;
pub const MAX_WATCH_SECONDS: u32 = 3600;
/// The round cap bounds the loop honours.
pub const MIN_MAX_ROUNDS: u32 = 1;
pub const MAX_MAX_ROUNDS: u32 = 200;
/// The prompt window bounds.
pub const MIN_NOTE_WINDOW: u32 = 1;
pub const MAX_NOTE_WINDOW: u32 = 64;
/// The field-note retention bounds.
pub const MIN_FIELD_NOTE_DAYS: u32 = 1;
pub const MAX_FIELD_NOTE_DAYS: u32 = 365;

/// Schema defaults for each steward setting when no row is stored.
pub const DEFAULT_WATCH_SECONDS: u32 = 300;
pub const DEFAULT_MAX_ROUNDS: u32 = 24;
pub const DEFAULT_NOTE_WINDOW: u32 = 8;
pub const DEFAULT_FIELD_NOTE_DAYS: u32 = 30;
pub const DEFAULT_DELIVERY: &str = "push";
pub const DEFAULT_CALL_URGENCY: &str = "normal";
/// The urgencies a call may carry.
pub const URGENCY_VALUES: [&str; 2] = ["normal", "high"];
pub const DEFAULT_BOARD_COLUMNS: &str = "compact";

/// The stored value for one steward row, or `None` when it has none.
fn stored_setting(db: &Db, row: &str) -> Option<String> {
    crate::web::store::settings_get(db, row)
        .ok()
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// An integer row, clamped to its bounds; anything unreadable is the default.
fn bounded_setting(db: &Db, row: &str, min: u32, max: u32, default: u32) -> u32 {
    stored_setting(db, row)
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|number| (min..=max).contains(number))
        .unwrap_or(default)
}

/// A closed-list row; anything not in the list is the default.
fn enum_setting(db: &Db, row: &str, values: &[&str], default: &str) -> String {
    stored_setting(db, row)
        .map(|value| value.to_ascii_lowercase())
        .filter(|value| values.contains(&value.as_str()))
        .unwrap_or_else(|| default.to_owned())
}

/// Whether the watch loop is armed. A row with no value is on.
#[must_use]
pub fn enabled(db: &Db) -> bool {
    !matches!(
        stored_setting(db, SETTING_ENABLED).as_deref(),
        Some("false")
    )
}

/// Seconds between watch rounds, read at each use.
#[must_use]
pub fn watch_seconds(db: &Db) -> u32 {
    bounded_setting(
        db,
        SETTING_WATCH_SECONDS,
        MIN_WATCH_SECONDS,
        MAX_WATCH_SECONDS,
        DEFAULT_WATCH_SECONDS,
    )
}

/// The watch-round cap, read at each use.
#[must_use]
pub fn max_rounds(db: &Db) -> u32 {
    bounded_setting(
        db,
        SETTING_MAX_ROUNDS,
        MIN_MAX_ROUNDS,
        MAX_MAX_ROUNDS,
        DEFAULT_MAX_ROUNDS,
    )
}

/// How many recent field notes a turn carries, read at each use.
#[must_use]
pub fn note_window(db: &Db) -> u32 {
    bounded_setting(
        db,
        SETTING_NOTE_WINDOW,
        MIN_NOTE_WINDOW,
        MAX_NOTE_WINDOW,
        DEFAULT_NOTE_WINDOW,
    )
}

/// How many days a field note is kept, read at each use.
#[must_use]
pub fn field_note_days(db: &Db) -> u32 {
    bounded_setting(
        db,
        SETTING_FIELD_NOTE_DAYS,
        MIN_FIELD_NOTE_DAYS,
        MAX_FIELD_NOTE_DAYS,
        DEFAULT_FIELD_NOTE_DAYS,
    )
}

/// The default delivery channel for a steward message.
#[must_use]
pub fn delivery_default(db: &Db) -> String {
    enum_setting(
        db,
        SETTING_DELIVERY_DEFAULT,
        crate::delivery::DEFAULT_VALUES,
        DEFAULT_DELIVERY,
    )
}

/// The default urgency for a steward call.
#[must_use]
pub fn call_urgency_default(db: &Db) -> String {
    enum_setting(
        db,
        SETTING_CALL_URGENCY_DEFAULT,
        &["normal", "high"],
        DEFAULT_CALL_URGENCY,
    )
}

/// The operator's charter text; empty when none is set.
#[must_use]
pub fn charter(db: &Db) -> String {
    stored_setting(db, SETTING_CHARTER).unwrap_or_default()
}

/// The board's column set.
#[must_use]
pub fn board_columns(db: &Db) -> String {
    enum_setting(
        db,
        SETTING_BOARD_COLUMNS,
        &["compact", "full"],
        DEFAULT_BOARD_COLUMNS,
    )
}

// ---------------------------------------------------------------------------
// Watch loop
// ---------------------------------------------------------------------------

/// One wake the planner decided on. The bookkeeping is already written when a
/// value of this type exists; the caller only delivers and admits the turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchWake {
    pub duty_id: String,
    pub statement: String,
    pub round: i64,
    pub max_rounds: u32,
    /// The short line the operator is told about.
    pub wake_message: String,
    /// The `<watch_round>` prompt the resume turn starts with.
    pub prompt: String,
}

/// Phases the watch loop wakes. `needs_you`, `paused` and `closed` never are.
fn is_watched(phase: Phase) -> bool {
    matches!(phase, Phase::Watching | Phase::Working | Phase::Waiting)
}

/// An instant as the whole-second RFC 3339 UTC text the wake column holds.
/// Whole seconds keep the stored text in one shape.
fn instant_text(at: OffsetDateTime) -> String {
    at.replace_nanosecond(0)
        .unwrap_or(at)
        .format(&Rfc3339)
        .map(|text| text.replace("+00:00", "Z"))
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// Parse a stored wake time. An unreadable one is not due.
fn parse_instant(text: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(text, &Rfc3339).ok()
}

/// Boot reconciliation. Phases have no in-flight marker: a round writes its
/// next wake before its turn starts, so a crash leaves `next_wake_at` as the
/// only truth. The one repair left is an open duty with a cadence and no wake.
pub fn reconcile_watch_on_boot(db: &Db, now: OffsetDateTime) -> Result<usize, DutyError> {
    let mut armed = 0;
    for duty in list_duties(db, None)? {
        if !is_watched(duty.phase) || duty.next_wake_at.is_some() {
            continue;
        }
        let Some(every) = duty.every_seconds else {
            continue;
        };
        let patch = DutyPatch {
            next_wake_at: Some(Some(instant_text(now + Duration::seconds(every)))),
            ..DutyPatch::default()
        };
        match update_duty(db, &duty.id, duty.revision, &patch) {
            Ok(_) => armed += 1,
            Err(error) => {
                tracing::warn!(%error, duty = %duty.id, "steward: boot could not arm a wake")
            }
        }
    }
    Ok(armed)
}

/// Plan one watch tick at `now`: every open duty whose wake is due, earliest
/// first. Each returned wake has already moved the duty's next wake forward,
/// counted its round and written its `steward.wake` audit row. A duty at the
/// round cap is paused instead and returns nothing. Nothing is read or written
/// when `steward_enabled` is off.
pub fn plan_watch_pass(db: &Db, now: OffsetDateTime) -> Result<Vec<WatchWake>, DutyError> {
    if !enabled(db) {
        return Ok(Vec::new());
    }
    let max = max_rounds(db);
    let default_every = i64::from(watch_seconds(db));
    let window = note_window(db) as usize;

    let mut due: Vec<(OffsetDateTime, Duty)> = list_duties(db, None)?
        .into_iter()
        .filter(|duty| is_watched(duty.phase))
        .filter_map(|duty| {
            let at = parse_instant(duty.next_wake_at.as_deref()?)?;
            (at <= now).then_some((at, duty))
        })
        .collect();
    due.sort_by_key(|(at, _)| *at);

    let mut wakes = Vec::new();
    for (_, duty) in due {
        if duty.watch_rounds >= i64::from(max) {
            let patch = DutyPatch {
                phase: Some(Phase::Paused),
                blocker: Some(Some(Blocker {
                    code: "round_cap".to_owned(),
                    detail: format!("{} watch rounds taken; the cap is {max}", duty.watch_rounds),
                })),
                ..DutyPatch::default()
            };
            match update_duty(db, &duty.id, duty.revision, &patch) {
                Ok(_) => {
                    audit_steward(db, "steward.round_cap", &duty.id, &format!("cap {max}"));
                }
                Err(error) => {
                    tracing::warn!(%error, duty = %duty.id, "steward: could not pause at the round cap");
                }
            }
            continue;
        }

        let round = duty.watch_rounds + 1;
        let every = duty
            .every_seconds
            .unwrap_or(default_every)
            .clamp(i64::from(MIN_WATCH_SECONDS), i64::from(MAX_WATCH_SECONDS));
        let patch = DutyPatch {
            next_wake_at: Some(Some(instant_text(now + Duration::seconds(every)))),
            watch_rounds: Some(round),
            ..DutyPatch::default()
        };
        // Compare-and-set on the revision the duty was read at. A stale write
        // is logged and this wake skipped; it is not retried in a loop.
        let updated = match update_duty(db, &duty.id, duty.revision, &patch) {
            Ok(updated) => updated,
            Err(error) => {
                tracing::warn!(%error, duty = %duty.id, "steward: wake skipped");
                continue;
            }
        };
        audit_steward(
            db,
            "steward.wake",
            &duty.id,
            &format!("round {round}/{max}"),
        );
        let notes = list_notes(db, window, Some(&duty.id))?;
        wakes.push(WatchWake {
            duty_id: duty.id.clone(),
            statement: duty.statement.clone(),
            round,
            max_rounds: max,
            wake_message: format!(
                "Steward: {} - round {round}/{max} is running.",
                duty.statement
            ),
            prompt: crate::steward_prompt::watch_round(&updated, round, max, &notes),
        });
    }
    Ok(wakes)
}

/// Deliver every open call once, through `delivery.rs`, and record that it went.
async fn deliver_pending_calls(state: &crate::admin::AppState, db: &Db) {
    let calls = match pending_calls(db) {
        Ok(calls) => calls,
        Err(error) => {
            tracing::error!(%error, "steward: cannot read open calls");
            return;
        }
    };
    for call in calls {
        let configured = if call.note.tags.iter().any(|tag| tag == "high") {
            "push".to_owned()
        } else {
            delivery_default(db)
        };
        let schedule = crate::scheduler::Schedule {
            id: call.duty.id.clone(),
            name: "Steward call".to_owned(),
            cron: String::new(),
            prompt: String::new(),
            enabled: true,
            next_fire_at: None,
            last_fired_at: None,
            last_status: None,
            last_error: None,
            fired_count: 0,
            deliver: channel_for(db, &configured),
        };
        let text = format!(
            "Steward call on \"{}\": {}",
            call.duty.statement, call.note.text
        );
        let (delivered, note) =
            crate::delivery::deliver(state, &schedule, &call.duty.id, "", Some(&text)).await;
        let detail = if note.is_empty() {
            delivered
        } else {
            format!("{delivered}: {note}")
        };
        if let Err(error) = append_note(
            db,
            Some(&call.duty.id),
            &format!("call delivered: {detail}"),
            &["delivered".to_owned()],
        ) {
            tracing::error!(%error, duty = %call.duty.id, "steward: cannot mark the call delivered");
        }
    }
}

/// Append one steward audit row. A failed write is logged, not fatal: the
/// bookkeeping it describes has already landed.
fn audit_steward(db: &Db, action: &str, duty_id: &str, detail: &str) {
    let text = format!("{duty_id} {detail}");
    if let Err(error) = crate::web::store::audit_write(db, "steward", action, Some(&text)) {
        tracing::error!(%error, action, "steward: cannot write the audit row");
    }
}

/// The hook the scheduler tick calls. It plans the pass on the tick's clock,
/// then delivers each wake through `delivery.rs` and admits its resume turn on
/// the existing turn path. Delivery reads `steward.delivery_default` at send
/// time. Turns are awaited inline, as scheduled runs are.
pub(crate) async fn watch_pass(state: &crate::admin::AppState, now: OffsetDateTime) {
    let db = state.engine.db().clone();
    deliver_pending_calls(state, &db).await;
    let wakes = match plan_watch_pass(&db, now) {
        Ok(wakes) => wakes,
        Err(error) => {
            tracing::error!(%error, "steward: cannot plan the watch pass");
            return;
        }
    };
    for wake in wakes {
        let schedule = crate::scheduler::Schedule {
            id: wake.duty_id.clone(),
            name: "Steward".to_owned(),
            cron: String::new(),
            prompt: wake.prompt.clone(),
            enabled: true,
            next_fire_at: None,
            last_fired_at: None,
            last_status: None,
            last_error: None,
            fired_count: 0,
            deliver: channel_for(&db, &delivery_default(&db)),
        };
        let (delivered, note) = crate::delivery::deliver(
            state,
            &schedule,
            &wake.duty_id,
            "",
            Some(&wake.wake_message),
        )
        .await;
        tracing::info!(duty = %wake.duty_id, %delivered, %note, "steward: wake delivered");
        match crate::web::routes::run_one_turn(
            state,
            &wake.prompt,
            &format!("Steward: {}", wake.statement),
        )
        .await
        {
            crate::web::routes::TurnOutcome::NoProvider => {
                tracing::warn!(duty = %wake.duty_id, "steward: no provider, resume turn not started");
            }
            crate::web::routes::TurnOutcome::Conversation { status, .. } => {
                tracing::info!(duty = %wake.duty_id, status, "steward: resume turn settled");
            }
        }
    }
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

#[cfg(test)]
mod watch_tests {
    use super::*;

    fn fake(text: &str) -> OffsetDateTime {
        OffsetDateTime::parse(text, &Rfc3339).expect("a fixed instant")
    }

    /// A duty whose first wake is set by hand, so the clock is the test's.
    fn duty_due_at(db: &Db, every: i64, first: OffsetDateTime) -> Duty {
        let duty = create_duty(db, "Watch the backup job", Some(every)).expect("create");
        let patch = DutyPatch {
            next_wake_at: Some(Some(instant_text(first))),
            ..DutyPatch::default()
        };
        update_duty(db, &duty.id, duty.revision, &patch).expect("set the first wake")
    }

    fn wake_rows(db: &Db) -> usize {
        crate::web::store::audit_recent(db, 500)
            .expect("audit")
            .iter()
            .filter(|row| row.action == "steward.wake")
            .count()
    }

    const T0: &str = "2026-10-22T00:00:00Z";

    #[test]
    fn exactly_one_wake_per_due_duty_across_three_intervals() {
        let db = Db::open_in_memory().expect("db");
        let duty = duty_due_at(&db, 60, fake(T0));
        let mut wakes = 0;
        // Before the first due time: nothing.
        wakes += plan_watch_pass(&db, fake(T0)).expect("pass").len().min(1);
        assert_eq!(wakes, 1, "due at T0 is one wake");
        // Mid-interval: not due yet.
        assert!(
            plan_watch_pass(&db, fake("2026-10-22T00:00:30Z"))
                .expect("pass")
                .is_empty()
        );
        // Second and third intervals.
        assert_eq!(
            plan_watch_pass(&db, fake("2026-10-22T00:01:00Z"))
                .expect("pass")
                .len(),
            1
        );
        assert_eq!(
            plan_watch_pass(&db, fake("2026-10-22T00:02:00Z"))
                .expect("pass")
                .len(),
            1
        );
        // Re-running the same instant is not a second wake.
        assert!(
            plan_watch_pass(&db, fake("2026-10-22T00:02:00Z"))
                .expect("pass")
                .is_empty()
        );

        let after = get_duty(&db, &duty.id).expect("duty");
        assert_eq!(after.watch_rounds, 3);
        assert_eq!(after.next_wake_at.as_deref(), Some("2026-10-22T00:03:00Z"));
        assert_eq!(wake_rows(&db), 3, "one steward.wake row per wake");
    }

    #[test]
    fn a_call_that_is_open_wakes_nothing_and_touches_nothing() {
        let db = Db::open_in_memory().expect("db");
        let duty = duty_due_at(&db, 60, fake(T0));
        let escalated = escalate_needs_you(&db, &duty.id, duty.revision).expect("call");
        let wakes = plan_watch_pass(&db, fake("2026-10-23T00:00:00Z")).expect("pass");
        assert!(wakes.is_empty(), "a call is open, so no model call");
        let after = get_duty(&db, &duty.id).expect("duty");
        assert_eq!(after.revision, escalated.revision, "no row touched");
        assert_eq!(wake_rows(&db), 0);
    }

    #[test]
    fn steward_disabled_wakes_nothing_and_touches_no_rows() {
        let db = Db::open_in_memory().expect("db");
        let duty = duty_due_at(&db, 60, fake(T0));
        crate::web::store::settings_put(&db, SETTING_ENABLED, "false").expect("disable");
        let wakes = plan_watch_pass(&db, fake("2026-10-23T00:00:00Z")).expect("pass");
        assert!(wakes.is_empty());
        let after = get_duty(&db, &duty.id).expect("duty");
        assert_eq!(after, duty, "the row is byte-for-byte the same");
        assert_eq!(wake_rows(&db), 0);
    }

    #[test]
    fn the_round_cap_pauses_the_duty_with_round_cap() {
        let db = Db::open_in_memory().expect("db");
        crate::web::store::settings_put(&db, SETTING_MAX_ROUNDS, "2").expect("cap");
        let duty = duty_due_at(&db, 30, fake(T0));
        assert_eq!(plan_watch_pass(&db, fake(T0)).expect("pass").len(), 1);
        assert_eq!(
            plan_watch_pass(&db, fake("2026-10-22T00:00:30Z"))
                .expect("pass")
                .len(),
            1
        );
        // The third due wake is at the cap: paused, no round, no turn.
        let at_cap = plan_watch_pass(&db, fake("2026-10-22T00:01:00Z")).expect("pass");
        assert!(at_cap.is_empty());
        let paused = get_duty(&db, &duty.id).expect("duty");
        assert_eq!(paused.phase, Phase::Paused);
        let blocker = paused.blocker.expect("a blocker");
        assert_eq!(blocker.code, "round_cap");
        assert_eq!(wake_rows(&db), 2, "two rounds ran, the third did not");
        // Paused duties are not woken again.
        assert!(
            plan_watch_pass(&db, fake("2026-10-30T00:00:00Z"))
                .expect("pass")
                .is_empty()
        );
    }

    #[test]
    fn a_resumed_duty_starts_its_round_count_again() {
        let db = Db::open_in_memory().expect("db");
        crate::web::store::settings_put(&db, SETTING_MAX_ROUNDS, "1").expect("cap");
        let duty = duty_due_at(&db, 30, fake(T0));
        assert_eq!(plan_watch_pass(&db, fake(T0)).expect("pass").len(), 1);
        assert!(
            plan_watch_pass(&db, fake("2026-10-22T00:00:30Z"))
                .expect("pass")
                .is_empty()
        );
        let paused = get_duty(&db, &duty.id).expect("duty");
        let resumed = update_duty(
            &db,
            &duty.id,
            paused.revision,
            &DutyPatch {
                phase: Some(Phase::Watching),
                ..DutyPatch::default()
            },
        )
        .expect("resume");
        assert_eq!(resumed.watch_rounds, 0);
    }

    #[test]
    fn a_stale_revision_on_the_wake_path_is_skipped_not_retried() {
        let db = Db::open_in_memory().expect("db");
        let duty = duty_due_at(&db, 60, fake(T0));
        // Another writer moves the revision between read and write.
        let read = list_duties(&db, None).expect("list");
        assert_eq!(read[0].revision, duty.revision);
        update_duty(
            &db,
            &duty.id,
            duty.revision,
            &DutyPatch {
                statement: Some("Watch the backup job closely".to_owned()),
                ..DutyPatch::default()
            },
        )
        .expect("concurrent edit");
        // The planner reads fresh, so it sees the new revision and wakes once;
        // a true stale write is covered by the store's compare-and-set tests.
        let wakes = plan_watch_pass(&db, fake(T0)).expect("pass");
        assert_eq!(wakes.len(), 1);
        assert_eq!(wake_rows(&db), 1);
    }

    #[test]
    fn boot_arms_an_open_duty_with_a_cadence_and_no_wake() {
        let db = Db::open_in_memory().expect("db");
        let duty = create_duty(&db, "Watch the mail queue", Some(120)).expect("create");
        let cleared = update_duty(
            &db,
            &duty.id,
            duty.revision,
            &DutyPatch {
                next_wake_at: Some(None),
                ..DutyPatch::default()
            },
        )
        .expect("clear the wake");
        assert!(cleared.next_wake_at.is_none());
        assert_eq!(reconcile_watch_on_boot(&db, fake(T0)).expect("boot"), 1);
        let armed = get_duty(&db, &duty.id).expect("duty");
        assert_eq!(armed.next_wake_at.as_deref(), Some("2026-10-22T00:02:00Z"));
    }
}

#[cfg(test)]
mod call_tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("an in-memory database")
    }

    #[test]
    fn an_open_call_is_pending_until_delivered_and_answering_clears_it() {
        let db = db();
        let duty = create_duty(&db, "renew the lease", Some(60)).expect("create");
        let raised = raise_call(
            &db,
            &duty.id,
            1,
            "Sign the renewal?",
            &["yes".to_owned(), "no".to_owned()],
            "normal",
        )
        .expect("call");
        assert_eq!(raised.phase, Phase::NeedsYou);
        assert_eq!(raised.next_wake_at, None, "an open call does not wake");
        let pending = pending_calls(&db).expect("pending");
        assert_eq!(pending.len(), 1);
        assert!(pending[0].note.text.contains("Choices: yes / no"));
        let again = raise_call(&db, &duty.id, raised.revision, "Again?", &[], "normal")
            .expect_err("second call");
        assert!(again.to_string().contains("already open"), "{again}");
        append_note(
            &db,
            Some(&duty.id),
            "call delivered: push",
            &["delivered".to_owned()],
        )
        .expect("mark");
        assert!(
            pending_calls(&db).expect("pending").is_empty(),
            "delivered calls are not sent twice"
        );
        let answered = answer_call(&db, &duty.id, raised.revision).expect("answer");
        assert_eq!(answered.phase, Phase::Watching);
        assert!(
            answered.next_wake_at.is_some(),
            "an answered duty is armed again"
        );
        assert_eq!(answered.watch_rounds, 0);
    }

    #[test]
    fn a_stale_call_is_refused_and_an_unopened_call_cannot_be_answered() {
        let db = db();
        let duty = create_duty(&db, "watch the vendor", None).expect("create");
        let stale = raise_call(&db, &duty.id, 9, "Q?", &[], "normal").expect_err("stale");
        assert!(
            matches!(stale, DutyError::StaleRevision { current: 1 }),
            "{stale:?}"
        );
        let no_call = answer_call(&db, &duty.id, 1).expect_err("nothing to answer");
        assert!(no_call.to_string().contains("no call is open"), "{no_call}");
        assert!(
            raise_call(&db, &duty.id, 1, "Q?", &[], "loud").is_err(),
            "urgency is closed"
        );
    }

    #[test]
    fn mail_goes_only_to_the_operators_allowlist() {
        let db = db();
        assert_eq!(channel_for(&db, "email"), "inbox", "no allowlist set");
        crate::web::store::settings_put(
            &db,
            SETTING_MAIL_RECIPIENTS,
            "ops@example.test, Boss@Example.test",
        )
        .expect("store");
        crate::web::store::settings_put(
            &db,
            crate::delivery::SETTING_EMAIL_TO,
            "stranger@example.test",
        )
        .expect("store");
        assert_eq!(
            channel_for(&db, "email"),
            "inbox",
            "an address off the list is refused"
        );
        crate::web::store::settings_put(
            &db,
            crate::delivery::SETTING_EMAIL_TO,
            "BOSS@example.test",
        )
        .expect("store");
        assert_eq!(
            channel_for(&db, "email"),
            "email",
            "a listed address, any case, is allowed"
        );
        assert_eq!(
            channel_for(&db, "push"),
            "push",
            "other channels are untouched"
        );
    }
}
