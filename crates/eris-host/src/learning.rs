//! The learning loop: what a finished turn taught, made durable.
//!
//! A turn answers once and is forgotten. This module is the part that refuses to
//! let a real lesson die with the transcript, and it is deliberately narrow:
//!
//! - **Skills are the artifact.** A lesson becomes a `SKILL.md` in the operator's
//!   own library, so the next turn is offered it by name through the same
//!   catalog and `read_skill` tool that already exist. There is no second memory
//!   store: gbrain remains the long-term brain, and a learned fact about a person
//!   belongs there, not here.
//! - **Only agent-written skills are editable.** A skill that was already on
//!   disk when the learner first looked is the operator's, and it is recorded as
//!   `operator`. Nothing here ever edits, archives or restores one. A pinned
//!   skill is off-limits to every automatic actor, including this one.
//! - **Nothing is deleted.** Archiving moves a directory into `.archive/` beside
//!   the library and is reversible with [`restore`]. The daemon never removes a
//!   skill file.
//! - **A lesson is a rule, not a story.** The prompt asks for a class-level
//!   procedure, the parser refuses an incident narrative masquerading as a
//!   name, and the writer caps the body so a skill cannot grow without bound.
//!
//! The background half - when a review is queued, when it runs, and when a
//! maintenance pass is due - lives in [`crate::learning_worker`]. This module is
//! the store, the prompt, the parser, and the writer, and every one of those is
//! a plain function so a test can prove it without a provider.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use rusqlite::params;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::db::{Db, DbError};
use crate::web::store;

// --- Settings ---------------------------------------------------------------

/// The master switch. Off means no sweep, no review, no curator.
pub const SETTING_ENABLED: &str = "learning.enabled";

/// Whether a finished turn may be reviewed at all.
pub const SETTING_REVIEW_ENABLED: &str = "learning.review.enabled";

/// Whether the time-based maintenance pass may run.
pub const SETTING_CURATOR_ENABLED: &str = "learning.curator.enabled";

/// How many characters an answer needs before it is worth a review.
pub const SETTING_MIN_CHARS: &str = "learning.review.min_chars";

/// The review budget: how many runs may be queued in one rolling hour.
pub const SETTING_MAX_PER_HOUR: &str = "learning.review.max_per_hour";

/// Whether a reply that breaks the review's own rules is repaired and re-asked
/// once before the run is failed.
///
/// On by default. The loop used to discard a quarter of its work to a validator
/// that rejected the agent's own output with no retry and no repair, which is
/// waste rather than strictness. Off restores the old behaviour for an operator
/// who would rather see every refusal than a corrected one.
pub const SETTING_REVIEW_REPAIR: &str = "learning.review.repair";

/// The age at which a learned skill becomes stale, in days.
pub const SETTING_STALE_DAYS: &str = "learning.curator.stale_days";

/// The age at which a learned skill is archived, in days.
pub const SETTING_ARCHIVE_DAYS: &str = "learning.curator.archive_days";

/// How often the maintenance pass is allowed to run, in hours.
pub const SETTING_INTERVAL_HOURS: &str = "learning.curator.interval_hours";

/// The newest answer timestamp the sweeper has already considered.
pub const SETTING_WATERMARK: &str = "learning.watermark";

/// When the maintenance pass last ran. Written by the pass, read by the gate.
pub const SETTING_LAST_CURATOR: &str = "learning.curator.last_run_at";

/// The default staleness window.
pub const DEFAULT_STALE_DAYS: i64 = 30;

/// The default archive window. Both follow the shape an agent's own skill
/// library needs: a month unused is a warning, a quarter is an archive.
pub const DEFAULT_ARCHIVE_DAYS: i64 = 90;

/// The default interval between maintenance passes: one day.
///
/// It was a week, and the consequence was a library that aged in steps: a skill
/// unused for a month stayed in the catalog for another week before anything
/// noticed. A daily pass costs one directory walk against a ledger that is already
/// in memory on every turn, and the catalog it curates is paid for on every model
/// call, so the pass is cheaper than the staleness it prevents.
pub const DEFAULT_INTERVAL_HOURS: i64 = 24;

/// The default answer length below which a turn is not worth a review. A
/// "thanks, that worked" answer teaches nothing and a review of it is a billed
/// call for nothing.
pub const DEFAULT_MIN_CHARS: i64 = 200;

/// The default review budget: six reviews an hour. A loop that can spend a
/// day's budget in a minute is a loop that gets switched off.
pub const DEFAULT_MAX_PER_HOUR: i64 = 6;

/// The hard ceiling on a learned body, so one model reply cannot write a novel
/// into the catalog.
pub const MAX_BODY_BYTES: usize = 12 * 1024;

/// The hard ceiling on an appended section.
pub const MAX_APPEND_BYTES: usize = 4 * 1024;

/// The longest description the review may produce. A description longer than
/// this is cut in the catalog, which is why the authoring rule is 60.
pub const MAX_DESCRIPTION_CHARS: usize = 60;

/// The longest name a learned skill may carry.
pub const MAX_NAME_CHARS: usize = 80;

/// The longest reason a review may give for its action.
pub const MAX_REASON_CHARS: usize = 400;

/// How many history copies of one skill are kept before the oldest is pruned.
pub const HISTORY_KEEP: usize = 20;

/// The hidden directory an archived skill is moved into.
pub const ARCHIVE_DIR: &str = ".archive";

// --- Time -------------------------------------------------------------------

/// The current UTC timestamp, in the format the database stamps.
#[must_use]
pub fn now() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map(|text| text.replace("+00:00", "Z"))
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// Parse a stored timestamp. `None` is a row that cannot be reasoned about.
#[must_use]
pub fn parse_timestamp(raw: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(raw.trim(), &Rfc3339).ok()
}

// --- Settings readers -------------------------------------------------------

fn setting(db: &Db, key: &str) -> Option<String> {
    store::settings_get(db, key).ok().flatten()
}

fn flag(db: &Db, key: &str, default: bool) -> bool {
    match setting(db, key).as_deref().map(str::trim) {
        Some("false") => false,
        Some("true") => true,
        _ => default,
    }
}

fn number(db: &Db, key: &str, default: i64) -> i64 {
    setting(db, key)
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

/// Whether the loop is on at all.
#[must_use]
pub fn enabled(db: &Db) -> bool {
    flag(db, SETTING_ENABLED, true)
}

/// Whether a finished turn may be reviewed.
#[must_use]
pub fn review_enabled(db: &Db) -> bool {
    flag(db, SETTING_REVIEW_ENABLED, true)
}

/// Whether a refused reply gets one repair and one re-ask before it is failed.
#[must_use]
pub fn review_repair(db: &Db) -> bool {
    flag(db, SETTING_REVIEW_REPAIR, true)
}

/// Whether the maintenance pass may run.
#[must_use]
pub fn curator_enabled(db: &Db) -> bool {
    flag(db, SETTING_CURATOR_ENABLED, true)
}

/// The review length floor.
#[must_use]
pub fn min_chars(db: &Db) -> i64 {
    number(db, SETTING_MIN_CHARS, DEFAULT_MIN_CHARS).max(0)
}

/// The rolling-hour review budget.
#[must_use]
pub fn max_per_hour(db: &Db) -> i64 {
    number(db, SETTING_MAX_PER_HOUR, DEFAULT_MAX_PER_HOUR).max(1)
}

/// The staleness window.
#[must_use]
pub fn stale_days(db: &Db) -> i64 {
    number(db, SETTING_STALE_DAYS, DEFAULT_STALE_DAYS).max(1)
}

/// The archive window. Always at least the staleness window.
#[must_use]
pub fn archive_days(db: &Db) -> i64 {
    number(db, SETTING_ARCHIVE_DAYS, DEFAULT_ARCHIVE_DAYS).max(stale_days(db) + 1)
}

/// The maintenance interval.
#[must_use]
pub fn interval_hours(db: &Db) -> i64 {
    number(db, SETTING_INTERVAL_HOURS, DEFAULT_INTERVAL_HOURS).max(1)
}

// --- The ledger -------------------------------------------------------------

/// Who wrote a skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Written by the learning loop.
    Agent,
    /// Already on disk when the loop first looked, or written by a person.
    Operator,
}

impl Origin {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Operator => "operator",
        }
    }

    /// Parse a stored spelling; anything outside the set is the operator's,
    /// because the safe default for an unreadable row is "do not edit it".
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        if raw.trim() == "agent" {
            Self::Agent
        } else {
            Self::Operator
        }
    }
}

/// Where a skill is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// In the catalog and offered to turns.
    Active,
    /// Still readable, but flagged as unused for a while.
    Stale,
    /// Moved out of the library and out of the catalog.
    Archived,
}

impl State {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Stale => "stale",
            Self::Archived => "archived",
        }
    }

    /// Parse a stored spelling; unknown text is active, which is the state a
    /// page can recover from by running a pass.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw.trim() {
            "stale" => Self::Stale,
            "archived" => Self::Archived,
            _ => Self::Active,
        }
    }
}

/// One row of the ledger: a skill and what the daemon knows about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    /// The skill key, which is the directory name.
    pub key: String,
    /// How many times a turn read it.
    pub use_count: i64,
    /// When the row was first written.
    pub created_at: String,
    /// The last read.
    pub last_used_at: Option<String>,
    /// The last read or write, whichever is later.
    pub last_activity_at: Option<String>,
    /// Who wrote it.
    pub created_by: Origin,
    /// Where it is in the lifecycle.
    pub state: State,
    /// Whether automatic work is forbidden from touching it.
    pub pinned: bool,
    /// The description from its frontmatter, for a page that lists it.
    pub description: Option<String>,
    /// The review run that created it, when one did.
    pub run_id: Option<String>,
}

/// Every ledger row, newest activity first.
///
/// # Errors
///
/// [`DbError`] when the table cannot be read.
pub fn rows(db: &Db) -> Result<Vec<Usage>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT key, use_count, created_at, last_used_at, last_activity_at, created_by, \
                    state, pinned, description, run_id \
               FROM skill_usage \
              ORDER BY COALESCE(last_activity_at, created_at) DESC, key",
        )?;
        let mapped = statement.query_map([], |row| {
            Ok(Usage {
                key: row.get(0)?,
                use_count: row.get(1)?,
                created_at: row.get(2)?,
                last_used_at: row.get(3)?,
                last_activity_at: row.get(4)?,
                created_by: Origin::parse(&row.get::<_, String>(5)?),
                state: State::parse(&row.get::<_, String>(6)?),
                pinned: row.get::<_, i64>(7)? != 0,
                description: row.get(8)?,
                run_id: row.get(9)?,
            })
        })?;
        mapped.collect()
    })
}

/// One ledger row by key.
///
/// # Errors
///
/// [`DbError`] when the table cannot be read.
pub fn usage(db: &Db, key: &str) -> Result<Option<Usage>, DbError> {
    Ok(rows(db)?.into_iter().find(|row| row.key == key))
}

/// Record that a turn read a skill.
///
/// Best effort by contract: this is called from [`crate::skills::read`], and a
/// ledger write that failed must never make a skill unreadable. A read
/// reactivates a stale skill but never resurrects an archived one: an archived
/// skill has no directory, so a read of it cannot have happened.
///
/// # Errors
///
/// [`DbError`] when the table cannot be written.
pub fn note_use(db: &Db, key: &str) -> Result<(), DbError> {
    let at = now();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO skill_usage (key, use_count, last_used_at, last_activity_at, created_by, state) \
             VALUES (?1, 1, ?2, ?2, 'operator', 'active') \
             ON CONFLICT(key) DO UPDATE SET \
                use_count = skill_usage.use_count + 1, \
                last_used_at = excluded.last_used_at, \
                last_activity_at = excluded.last_activity_at, \
                state = CASE WHEN skill_usage.state = 'archived' THEN 'archived' ELSE 'active' END",
            params![key, at],
        )?;
        Ok(())
    })
}

/// Bring the ledger in line with what is on disk.
///
/// A directory the ledger has never seen is the operator's: it was there before
/// this loop looked, and inventing a different origin would let a later review
/// edit a hand-written skill. A row whose directory is gone (deleted by hand,
/// or archived outside the daemon) is marked archived so a pass does not keep
/// counting a skill nobody can read.
///
/// # Errors
///
/// [`DbError`] when the ledger cannot be read or written.
pub fn sync_library(db: &Db, dir: &Path) -> Result<usize, DbError> {
    // A library that cannot be listed has nothing to reconcile. That is not an
    // error a sweep should retry: the next pass reads it again.
    let Ok((skills, _)) = crate::skills::list(db, Some(&dir.display().to_string())) else {
        return Ok(0);
    };
    let at = now();
    let on_disk: BTreeMap<String, Option<String>> = skills
        .into_iter()
        .map(|skill| (skill.key, skill.description))
        .collect();
    let mut added = 0usize;
    db.with_conn(|conn| {
        for (key, description) in &on_disk {
            let changed = conn.execute(
                "INSERT INTO skill_usage (key, description, created_at, last_activity_at, created_by, state) \
                 VALUES (?1, ?2, ?3, ?3, 'operator', 'active') \
                 ON CONFLICT(key) DO UPDATE SET description = excluded.description",
                params![key, description, at],
            )?;
            if changed > 0 {
                added += 1;
            }
        }
        let known: Vec<String> = {
            let mut statement = conn.prepare("SELECT key FROM skill_usage")?;
            let mapped = statement.query_map([], |row| row.get::<_, String>(0))?;
            mapped.collect::<Result<Vec<_>, _>>()?
        };
        for key in known {
            if !on_disk.contains_key(&key) {
                conn.execute(
                    "UPDATE skill_usage SET state = 'archived' \
                      WHERE key = ?1 AND state != 'archived'",
                    params![key],
                )?;
            }
        }
        Ok(())
    })?;
    Ok(added)
}

/// Pin or unpin one skill.
///
/// # Errors
///
/// [`DbError`] when the row cannot be updated.
pub fn set_pinned(db: &Db, key: &str, pinned: bool) -> Result<bool, DbError> {
    db.with_conn(|conn| {
        let changed = conn.execute(
            "UPDATE skill_usage SET pinned = ?2 WHERE key = ?1",
            params![key, i64::from(pinned)],
        )?;
        Ok(changed > 0)
    })
}

/// Record that the loop wrote a skill it created.
///
/// # Errors
///
/// [`DbError`] when the row cannot be written.
pub fn record_created(
    db: &Db,
    key: &str,
    description: Option<&str>,
    run_id: Option<&str>,
) -> Result<(), DbError> {
    let at = now();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO skill_usage \
                (key, use_count, created_at, last_activity_at, created_by, state, description, run_id) \
             VALUES (?1, 0, ?2, ?2, 'agent', 'active', ?3, ?4) \
             ON CONFLICT(key) DO UPDATE SET \
                description = COALESCE(excluded.description, skill_usage.description), \
                last_activity_at = excluded.last_activity_at, \
                state = 'active', \
                run_id = excluded.run_id",
            params![key, at, description, run_id],
        )?;
        Ok(())
    })
}

/// Record that the loop appended to (or replaced) an existing skill.
///
/// # Errors
///
/// [`DbError`] when the row cannot be written.
pub fn record_written(db: &Db, key: &str, description: Option<&str>) -> Result<(), DbError> {
    let at = now();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE skill_usage SET last_activity_at = ?2, state = 'active', \
                    description = COALESCE(?3, description) \
              WHERE key = ?1",
            params![key, at, description],
        )?;
        Ok(())
    })
}

/// Mark one skill archived in the ledger.
///
/// # Errors
///
/// [`DbError`] when the row cannot be updated.
pub fn mark_archived(db: &Db, key: &str) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE skill_usage SET state = 'archived' WHERE key = ?1",
            params![key],
        )?;
        Ok(())
    })
}

/// Drop one ledger row. Only for undoing a reservation whose write failed: a
/// skill that stopped existing is archived, never forgotten.
///
/// # Errors
///
/// [`DbError`] when the row cannot be removed.
pub fn forget(db: &Db, key: &str) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute("DELETE FROM skill_usage WHERE key = ?1", params![key])?;
        Ok(())
    })
}

/// Set one skill's lifecycle state.
///
/// # Errors
///
/// [`DbError`] when the row cannot be updated.
pub fn set_state(db: &Db, key: &str, state: State) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE skill_usage SET state = ?2 WHERE key = ?1",
            params![key, state.as_str()],
        )?;
        Ok(())
    })
}

// --- The review contract ----------------------------------------------------

/// What a review asks the loop to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Write a new skill.
    Create,
    /// Add a section to an existing agent-owned skill.
    Append,
    /// Nothing worth saving.
    None,
}

impl Action {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Append => "append",
            Self::None => "none",
        }
    }
}

/// The sections an append may name. A free-form heading is refused, because a
/// skill whose structure the model invents is a skill the next reader cannot
/// scan.
pub const SECTIONS: [&str; 5] = [
    "When to Use",
    "Procedure",
    "Pitfalls",
    "Verification",
    "Quick Reference",
];

/// One parsed review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    /// What to do.
    pub action: Action,
    /// The skill key.
    pub key: String,
    /// The human title.
    pub name: String,
    /// One sentence, at most [`MAX_DESCRIPTION_CHARS`].
    pub description: String,
    /// The section an append adds to.
    pub section: Option<String>,
    /// The body to write, or the text to append.
    pub body: String,
    /// Why the review did it.
    pub reason: String,
}

/// Everything the parser can refuse, with a sentence the page can print.
#[derive(Debug, thiserror::Error)]
pub enum ReviewError {
    /// No JSON object could be found in the reply.
    #[error("the review replied without a JSON object")]
    NoJson,
    /// The object was not valid JSON.
    #[error("the review's JSON did not parse: {0}")]
    Json(String),
    /// An action outside the closed set.
    #[error("`{0}` is not an action this loop performs")]
    UnknownAction(String),
    /// A key that is not a plain, lowercase slug.
    #[error("`{0}` is not a usable skill key")]
    BadKey(String),
    /// A name that is blank or too long.
    #[error("the skill name must be 1-{MAX_NAME_CHARS} characters on one line")]
    BadName,
    /// A description that is blank, too long, multi-line, or unterminated.
    #[error(
        "the description must be one sentence of at most {MAX_DESCRIPTION_CHARS} characters, ending in a period"
    )]
    BadDescription,
    /// A body that is blank, too short or too long.
    #[error("the body must be {min}-{MAX_BODY_BYTES} characters for a new skill")]
    BadBody {
        /// The floor that applied.
        min: usize,
    },
    /// An appended section that is blank or too long.
    #[error("an appended section must be 10-{MAX_APPEND_BYTES} characters")]
    BadAppend,
    /// A section name outside [`SECTIONS`].
    #[error("`{0}` is not a section an append may add")]
    BadSection(String),
    /// Text that looks like a credential.
    #[error("the review tried to write something that looks like a secret")]
    Secret,
}

/// Strip a reasoning block and pull the first JSON object out of a reply.
///
/// Models answer with a fence, a preamble, or a think block; a strict
/// `serde_json::from_str` on the whole reply would refuse a correct answer for
/// its packaging. The contract is "one object", so the object is extracted and
/// everything around it ignored.
#[must_use]
pub fn extract_json(raw: &str) -> Option<String> {
    let text = crate::web::chat::strip_think(raw);
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| text[start..=end].to_owned())
}

/// Parse one review reply.
///
/// # Errors
///
/// [`ReviewError`] naming the first rule the reply broke.
pub fn parse_review(raw: &str) -> Result<Review, ReviewError> {
    let json = extract_json(raw).ok_or(ReviewError::NoJson)?;
    let value: serde_json::Value =
        serde_json::from_str(&json).map_err(|error| ReviewError::Json(error.to_string()))?;
    let action_raw = value
        .get("action")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("none")
        .trim()
        .to_ascii_lowercase();
    let action = match action_raw.as_str() {
        "create" => Action::Create,
        "append" => Action::Append,
        "none" | "" => Action::None,
        other => return Err(ReviewError::UnknownAction(other.to_owned())),
    };
    if action == Action::None {
        return Ok(Review {
            action,
            key: String::new(),
            name: String::new(),
            description: String::new(),
            section: None,
            body: String::new(),
            reason: clip(
                value
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("nothing worth saving"),
                MAX_REASON_CHARS,
            ),
        });
    }
    let key = value
        .get("key")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if !valid_review_key(&key) {
        return Err(ReviewError::BadKey(key));
    }
    let name = value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS || name.contains(['\n', '\r']) {
        return Err(ReviewError::BadName);
    }
    let description = value
        .get("description")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if !valid_description(&description) {
        return Err(ReviewError::BadDescription);
    }
    let body = value
        .get("body")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    let section = value
        .get("section")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    match action {
        Action::Create => {
            if body.chars().count() < 40 || body.len() > MAX_BODY_BYTES {
                return Err(ReviewError::BadBody { min: 40 });
            }
            // A model that fills `section` on a create is being helpful, not
            // wrong: the section names where the lesson would go if this were an
            // append, and a new skill's body already carries its own headings.
            // Refusing the whole review over a harmless extra field threw away a
            // real lesson once; the field is dropped instead.
        }
        Action::Append => {
            if body.chars().count() < 10 || body.len() > MAX_APPEND_BYTES {
                return Err(ReviewError::BadAppend);
            }
            let Some(section) = section.as_deref() else {
                return Err(ReviewError::BadSection(String::new()));
            };
            if !SECTIONS.contains(&section) {
                return Err(ReviewError::BadSection(section.to_owned()));
            }
        }
        Action::None => {}
    }
    if looks_like_secret(&name) || looks_like_secret(&description) || looks_like_secret(&body) {
        return Err(ReviewError::Secret);
    }
    Ok(Review {
        action,
        key,
        name,
        description,
        section: (action == Action::Append).then_some(section).flatten(),
        body,
        reason: clip(
            value
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default(),
            MAX_REASON_CHARS,
        ),
    })
}

/// A key the parser accepts: a slug, lowercase, 3-64 characters, starting with
/// a letter or digit. Deliberately stricter than [`crate::skills::valid_key`]
/// because this is a key the model invented, and an uppercase or dotted key is
/// a near-duplicate of an existing skill waiting to happen.
#[must_use]
pub fn valid_review_key(key: &str) -> bool {
    let count = key.chars().count();
    (3..=64).contains(&count)
        && key
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        && key
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_'))
}

/// Whether a description obeys the authoring rule: one line, at most
/// [`MAX_DESCRIPTION_CHARS`] characters, one sentence, ending in a period.
#[must_use]
pub fn valid_description(description: &str) -> bool {
    let count = description.trim().chars().count();
    (3..=MAX_DESCRIPTION_CHARS).contains(&count)
        && description.trim().ends_with('.')
        && !description.contains(['\n', '\r'])
        && !sentence_break(description)
}

/// Whether the text holds a second sentence.
///
/// The rule the error message has always stated is "one sentence", and until now
/// nothing enforced it: a 50-character description holding two sentences passed
/// as long as it ended in a period. A break is a period, space, and then either a
/// capital or a code span, which is what "Do this. Then do that." looks like and
/// what "e.g. this" does not. Abbreviations stay legal so the check costs a re-ask
/// only when the reply really did write two sentences.
fn sentence_break(text: &str) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0;
    while index + 2 < chars.len() {
        if chars[index] == '.' && chars[index + 1] == ' ' {
            let next = chars[index + 2];
            if next.is_ascii_uppercase() || next == '`' {
                return true;
            }
        }
        index += 1;
    }
    false
}

/// Whether a refused review is worth repairing before it is retried.
///
/// A description one word over the line, a name with a newline in it, and an
/// append that came back too short are the model's near-misses rather than a
/// refusal of the work. Correcting them is cheaper than discarding the lesson.
#[must_use]
pub fn is_repairable(error: &ReviewError) -> bool {
    matches!(
        error,
        ReviewError::BadDescription | ReviewError::BadName | ReviewError::BadAppend
    )
}

/// Repair a review whose only problem is arithmetic, and return the JSON a second
/// parse can read.
///
/// Deterministic or nothing. A description that is merely too long is clipped at a
/// word boundary and given the period it should have had; a name carrying a
/// newline has the newline collapsed. Everything else - two sentences, a blank
/// name, a key that is not a slug - is left to the re-ask, because guessing which
/// sentence the model meant is how a loop writes a rule nobody asked for.
#[must_use]
pub fn repair_review(raw: &str, error: &ReviewError) -> Option<String> {
    let json = extract_json(raw)?;
    let mut value: serde_json::Value = serde_json::from_str(&json).ok()?;
    let object = value.as_object_mut()?;
    match error {
        ReviewError::BadDescription => {
            let original = object.get("description")?.as_str()?;
            let fitted = fit_description(original)?;
            if !valid_description(&fitted) {
                return None;
            }
            object.insert("description".to_owned(), serde_json::Value::String(fitted));
        }
        ReviewError::BadAppend => {
            let original = object.get("body")?.as_str()?;
            let flat = original
                .split(['\n', '\r'])
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if flat.chars().count() < 10 {
                return None;
            }
            let fitted = if flat.len() > MAX_APPEND_BYTES {
                let cut: String = flat.chars().take(MAX_APPEND_BYTES).collect();
                match cut.rfind(' ') {
                    Some(index) if index > MAX_APPEND_BYTES / 2 => cut[..index].trim().to_owned(),
                    _ => cut,
                }
            } else {
                flat
            };
            object.insert("body".to_owned(), serde_json::Value::String(fitted));
        }
        ReviewError::BadName => {
            let original = object.get("name")?.as_str()?;
            let flat = original
                .split(['\n', '\r'])
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if flat.is_empty() || flat.chars().count() > MAX_NAME_CHARS {
                return None;
            }
            object.insert("name".to_owned(), serde_json::Value::String(flat));
        }
        _ => return None,
    }
    serde_json::to_string(&value).ok()
}

/// Fit a description into one sentence of [`MAX_DESCRIPTION_CHARS`], ending in a
/// period, cutting at a word boundary so the result still reads as English.
fn fit_description(description: &str) -> Option<String> {
    let flat = description
        .split(['\n', '\r'])
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let flat = flat.trim().trim_end_matches('.').trim();
    if flat.chars().count() < 3 {
        return None;
    }
    // The period is part of the budget, so the body gets one fewer character.
    let budget = MAX_DESCRIPTION_CHARS - 1;
    if flat.chars().count() <= budget {
        return Some(format!("{flat}."));
    }
    let cut: String = flat.chars().take(budget).collect();
    let trimmed = match cut.rfind(' ') {
        Some(index) if index > budget / 2 => &cut[..index],
        _ => cut.as_str(),
    };
    let trimmed = trimmed.trim_end_matches(|ch: char| !ch.is_alphanumeric());
    if trimmed.chars().count() < 3 {
        return None;
    }
    Some(format!("{trimmed}."))
}

/// A cheap credential scan. It is not a secret detector; it is a guard against a
/// model pasting an API key into a procedure, which is the one failure mode that
/// would turn this loop into a leak.
#[must_use]
pub fn looks_like_secret(text: &str) -> bool {
    const MARKERS: [&str; 6] = ["-----BEGIN", "sk-", "ghp_", "github_pat_", "AKIA", "xoxb-"];
    if MARKERS.iter().any(|marker| text.contains(marker)) {
        return true;
    }
    // A long unbroken token carrying both letters and digits is the shape of a
    // credential and not the shape of a sentence.
    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 40)
        .any(|token| {
            token.chars().any(|ch| ch.is_ascii_digit())
                && token.chars().any(|ch| ch.is_ascii_alphabetic())
        })
}

fn clip(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_owned();
    }
    let cut: String = trimmed.chars().take(limit).collect();
    format!("{cut}...")
}

// --- The writer -------------------------------------------------------------

/// Everything that can go wrong writing a skill.
#[derive(Debug, thiserror::Error)]
pub enum LearningError {
    /// The database refused.
    #[error("database: {0}")]
    Db(#[from] DbError),
    /// A filesystem operation failed.
    #[error("{path}: {detail}")]
    Io {
        /// The path involved.
        path: String,
        /// The underlying message.
        detail: String,
    },
    /// The operation was understood and refused, with a reason.
    #[error("{0}")]
    Refused(String),
}

impl LearningError {
    fn io(path: &Path, error: std::io::Error) -> Self {
        Self::Io {
            path: path.display().to_string(),
            detail: error.to_string(),
        }
    }
}

/// What one applied review did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// The action that was carried out, or `none` when nothing was.
    pub action: &'static str,
    /// The skill touched, when one was.
    pub key: Option<String>,
    /// One line for the run row and the page.
    pub note: String,
}

/// The library, the ledger, and the history directory a write needs.
#[derive(Debug, Clone)]
pub struct Shelf {
    /// The skill library directory.
    pub dir: PathBuf,
    /// Where superseded copies are kept, outside the library.
    pub history: PathBuf,
}

impl Shelf {
    /// A shelf for a library directory, with history kept beside the state
    /// database so a backup of state carries the undo trail.
    #[must_use]
    pub fn new(dir: PathBuf, history: PathBuf) -> Self {
        Self { dir, history }
    }

    /// The shelf an engine's settings resolve to: the operator's library, with
    /// history beside the state database. `None` when no home directory resolves,
    /// which is a host this daemon cannot write to anyway.
    #[must_use]
    pub fn for_engine(engine: &crate::onboarding::SetupEngine) -> Option<Self> {
        let db = engine.db();
        let stored = engine.setting(crate::skills::SETTING_DIR).ok().flatten();
        let dir = crate::skills::library_dir(stored.as_deref())?;
        Some(Self::new(dir, Self::history_beside(db)))
    }

    /// The history directory derived from the state database's own directory.
    #[must_use]
    pub fn history_beside(db: &Db) -> PathBuf {
        db.path()
            .and_then(|path| path.parent().map(|parent| parent.join("learning/history")))
            .unwrap_or_else(|| PathBuf::from("learning/history"))
    }
}

/// Write the file for a new skill. The directory must not already exist.
///
/// # Errors
///
/// [`LearningError::Refused`] when the key is unusable or the directory exists,
/// [`LearningError::Io`] when the write fails.
pub fn create_skill(
    shelf: &Shelf,
    key: &str,
    name: &str,
    description: &str,
    body: &str,
) -> Result<PathBuf, LearningError> {
    if !crate::skills::valid_key(key) {
        return Err(LearningError::Refused(format!(
            "`{key}` is not a usable skill key"
        )));
    }
    let directory = shelf.dir.join(key);
    if directory.exists() {
        return Err(LearningError::Refused(format!(
            "a skill named `{key}` already exists"
        )));
    }
    std::fs::create_dir_all(&directory).map_err(|error| LearningError::io(&directory, error))?;
    let text = format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n");
    let path = directory.join(crate::skills::FILE_NAME);
    write_atomic(&path, &text)?;
    Ok(path)
}

/// Append a section to an existing skill body.
///
/// The current file is copied into the history directory first, so one bad
/// append is one file to restore rather than a lost procedure. The heading is
/// found when it already exists and the text lands under it; otherwise a new
/// heading is added at the end.
///
/// # Errors
///
/// [`LearningError::Refused`] when the skill is missing or the result would
/// exceed [`MAX_BODY_BYTES`].
pub fn append_section(
    shelf: &Shelf,
    key: &str,
    section: &str,
    text: &str,
) -> Result<PathBuf, LearningError> {
    if !SECTIONS.contains(&section) {
        return Err(LearningError::Refused(format!(
            "`{section}` is not a section an append may add"
        )));
    }
    let path = shelf.dir.join(key).join(crate::skills::FILE_NAME);
    let existing =
        std::fs::read_to_string(&path).map_err(|error| LearningError::io(&path, error))?;
    if existing.trim().contains(text.trim()) {
        return Err(LearningError::Refused(
            "that text is already in the skill".to_owned(),
        ));
    }
    let updated = insert_section(&existing, section, text);
    if updated.len() > MAX_BODY_BYTES {
        return Err(LearningError::Refused(format!(
            "the result would be {} bytes, over the {MAX_BODY_BYTES}-byte ceiling",
            updated.len()
        )));
    }
    save_history(shelf, key, &existing)?;
    write_atomic(&path, &updated)?;
    Ok(path)
}

/// Insert text under a section heading, creating the heading when absent.
#[must_use]
pub fn insert_section(existing: &str, section: &str, text: &str) -> String {
    let heading = format!("## {section}");
    let text = text.trim();
    if let Some(at) = existing.find(&heading) {
        let after_heading = at + heading.len();
        let rest = &existing[after_heading..];
        let next = rest
            .find("\n## ")
            .map(|offset| after_heading + offset)
            .unwrap_or(existing.len());
        let (head, tail) = existing.split_at(next);
        let tail = tail.trim_start_matches('\n');
        if tail.is_empty() {
            format!("{}\n\n{}\n", head.trim_end(), text)
        } else {
            format!("{}\n\n{}\n\n{}", head.trim_end(), text, tail)
        }
    } else {
        format!("{}\n\n{heading}\n\n{text}\n", existing.trim_end())
    }
}

/// A filename-safe UTC stamp. Kept apart from [`now`] so a directory name never
/// carries a colon, which is legal on Linux and awkward everywhere else.
fn file_stamp() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
        .replace("+00:00", "Z")
        .replace(':', "-")
}

fn save_history(shelf: &Shelf, key: &str, text: &str) -> Result<(), LearningError> {
    let directory = shelf.history.join(key);
    std::fs::create_dir_all(&directory).map_err(|error| LearningError::io(&directory, error))?;
    let stamp = file_stamp();
    let path = directory.join(format!("{stamp}.md"));
    write_atomic(&path, text)?;
    // Keep the trail bounded: the newest HISTORY_KEEP copies are enough to undo
    // any recent bad write, and an unbounded trail is a disk leak.
    let mut copies: Vec<PathBuf> = std::fs::read_dir(&directory)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "md"))
                .collect()
        })
        .unwrap_or_default();
    copies.sort();
    while copies.len() > HISTORY_KEEP {
        let oldest = copies.remove(0);
        let _ = std::fs::remove_file(oldest);
    }
    Ok(())
}

fn write_atomic(path: &Path, text: &str) -> Result<(), LearningError> {
    let temporary = path.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&temporary)
            .map_err(|error| LearningError::io(&temporary, error))?;
        file.write_all(text.as_bytes())
            .map_err(|error| LearningError::io(&temporary, error))?;
        file.sync_all()
            .map_err(|error| LearningError::io(&temporary, error))?;
    }
    std::fs::rename(&temporary, path).map_err(|error| LearningError::io(path, error))
}

/// Move a skill out of the library and into `.archive/`.
///
/// # Errors
///
/// [`LearningError::Refused`] when the skill is missing, or
/// [`LearningError::Io`] when the move fails.
pub fn archive(shelf: &Shelf, key: &str) -> Result<PathBuf, LearningError> {
    let source = shelf.dir.join(key);
    if !source.is_dir() {
        return Err(LearningError::Refused(format!(
            "no skill named `{key}` is in this library"
        )));
    }
    let archive = shelf.dir.join(ARCHIVE_DIR);
    std::fs::create_dir_all(&archive).map_err(|error| LearningError::io(&archive, error))?;
    let stamp = file_stamp();
    let mut target = archive.join(format!("{key}-{stamp}"));
    let mut suffix = 1;
    while target.exists() {
        suffix += 1;
        target = archive.join(format!("{key}-{stamp}-{suffix}"));
    }
    std::fs::rename(&source, &target).map_err(|error| LearningError::io(&source, error))?;
    Ok(target)
}

/// Move the newest archived copy of a skill back into the library.
///
/// # Errors
///
/// [`LearningError::Refused`] when no archive exists or the key is occupied.
pub fn restore(shelf: &Shelf, key: &str) -> Result<PathBuf, LearningError> {
    if !crate::skills::valid_key(key) {
        return Err(LearningError::Refused(format!(
            "`{key}` is not a usable skill key"
        )));
    }
    if shelf.dir.join(key).exists() {
        return Err(LearningError::Refused(format!(
            "`{key}` is already in the library"
        )));
    }
    let archive = shelf.dir.join(ARCHIVE_DIR);
    let prefix = format!("{key}-");
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(&archive)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| {
                    path.is_dir()
                        && path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with(&prefix))
                })
                .collect()
        })
        .unwrap_or_default();
    candidates.sort();
    let Some(source) = candidates.pop() else {
        return Err(LearningError::Refused(format!(
            "no archived copy of `{key}` exists"
        )));
    };
    let target = shelf.dir.join(key);
    std::fs::rename(&source, &target).map_err(|error| LearningError::io(&source, error))?;
    Ok(target)
}

/// Apply one parsed review.
///
/// The policy lives here, not in the prompt: a review cannot edit an operator
/// skill, cannot touch a pinned one, cannot create over an existing key, and
/// cannot append to a skill that is archived.
///
/// # Errors
///
/// [`LearningError`] for a database or filesystem failure. A policy refusal is
/// `Ok(Applied)` with a `none` action and the reason, because it is an outcome
/// a page should show, not an error the worker should retry.
/// Keep a lesson that changed no skill in the memory store.
///
/// A review that says `none` used to end at the run row: the sentence was
/// written, read once by the operator, and then only in a page that scrolls. The
/// memory store is where a constraint belongs, and an entry there is visible to
/// recall and to the phone.
///
/// The origin is [`mem::Origin::Model`], so the row is stored for inspection and
/// is not recallable until something promotes it: a loop that could promote its
/// own writing would quietly rewrite what recall believes.
///
/// Returns whether an entry landed. A duplicate is not an error and does not
/// report a write.
pub fn remember_lesson(db: &Db, review: &Review, run_id: Option<&str>) -> bool {
    let text = review.description.trim();
    if text.is_empty() {
        return false;
    }
    let clipped: String = text.chars().take(400).collect();
    crate::mem::insert_entry(
        db,
        &crate::mem::NewEntry {
            tier: crate::mem::Tier::Curated,
            target: "memory",
            entry: &clipped,
            origin: crate::mem::Origin::Model,
            source_ref: run_id,
        },
    )
    .is_ok()
}

pub fn apply(
    db: &Db,
    shelf: &Shelf,
    review: &Review,
    run_id: Option<&str>,
) -> Result<Applied, LearningError> {
    match review.action {
        Action::None => Ok(Applied {
            action: "none",
            key: None,
            note: review.reason.clone(),
        }),
        Action::Create => {
            let existing = usage(db, &review.key)?;
            if let Some(row) = existing {
                return Ok(refused_create(&row));
            }
            // Reserve the key before the directory exists. The curator reads the
            // ledger from another thread, so a file that appears first would be
            // claimed as the operator's before this row could say otherwise.
            record_created(db, &review.key, Some(&review.description), run_id)?;
            if let Err(error) = create_skill(
                shelf,
                &review.key,
                &review.name,
                &review.description,
                &review.body,
            ) {
                let _ = forget(db, &review.key);
                return Err(error);
            }
            Ok(Applied {
                action: "create",
                key: Some(review.key.clone()),
                note: review.reason.clone(),
            })
        }
        Action::Append => {
            let Some(row) = usage(db, &review.key)? else {
                return Ok(Applied {
                    action: "none",
                    key: Some(review.key.clone()),
                    note: "no skill with that key exists, so there was nothing to append to"
                        .to_owned(),
                });
            };
            if row.pinned {
                return Ok(Applied {
                    action: "none",
                    key: Some(review.key.clone()),
                    note: "the skill is pinned, so automatic work cannot change it".to_owned(),
                });
            }
            if row.created_by == Origin::Operator {
                return Ok(Applied {
                    action: "none",
                    key: Some(review.key.clone()),
                    note: "the skill is the operator's own, so the loop will not edit it"
                        .to_owned(),
                });
            }
            if row.state == State::Archived || !shelf.dir.join(&review.key).is_dir() {
                return Ok(Applied {
                    action: "none",
                    key: Some(review.key.clone()),
                    note: "the skill is archived, so there was nothing to append to".to_owned(),
                });
            }
            let section = review.section.as_deref().unwrap_or("Pitfalls");
            append_section(shelf, &review.key, section, &review.body)?;
            record_written(db, &review.key, None)?;
            Ok(Applied {
                action: "append",
                key: Some(review.key.clone()),
                note: review.reason.clone(),
            })
        }
    }
}

fn refused_create(row: &Usage) -> Applied {
    let note = if row.created_by == Origin::Operator {
        "a skill with that key is the operator's own, so the loop will not replace it"
    } else {
        "a skill with that key already exists; an append is the right action for one that does"
    };
    Applied {
        action: "none",
        key: Some(row.key.clone()),
        note: note.to_owned(),
    }
}

// --- The review prompt ------------------------------------------------------

/// The system message a review runs under: class-level, rule-not-story, no
/// incident narration, no environment failures, no secrets.
pub const SYSTEM: &str = "You are the background skill reviewer for one operator's agent daemon. \
You read one finished exchange and decide whether it taught a durable, reusable procedure. \
You are terse, you never flatter, and you never narrate the incident.\n\n\
What a skill is: the instructions for doing a CLASS of task the most efficient and correct way \
for this operator - the procedure, the commands and tools that worked, the order, the \
preferences the operator expressed, and the pitfalls that cost time. A future session must be \
able to follow it and get the result right on the first try.\n\n\
Write a rule, not a story. A pitfall is an imperative rule plus one clause of WHY. \
No dates, request ids, ticket numbers, or quoted chatter as content. The same lesson stated \
twice is one rule. Do not restate what the repository's AGENTS.md or a tool schema already \
teaches; a skill carries the workflow and the pitfalls.\n\n\
Never capture: environment setup failures or missing binaries (the operator can fix those), \
negative claims about tools (\"X is broken\"), transient errors that were retried away, \
one-off task narratives, or an unresolved failure dressed up as a working method. Never capture \
credentials, tokens, passwords, or personal facts about the operator; a personal fact is \
memory, not a procedure.\n\n\
Prefer APPEND to an existing learned skill when one already covers the class: add only the new \
rule under one of the given sections. CREATE only when no learned skill covers the class. If the \
exchange taught nothing durable, answer action none. Most ordinary exchanges teach nothing.\n\n\
Reply with ONE JSON object and nothing else:\n\
{\"action\":\"create|append|none\",\"key\":\"lowercase-hyphenated\",\"name\":\"Human Title\",\
\"description\":\"One sentence, at most 60 characters, ends with a period.\",\
\"section\":\"Pitfalls|Procedure|When to Use|Verification|Quick Reference\",\
\"body\":\"markdown\",\"reason\":\"one short sentence\"}\n\n\
For append, body is only the text to add under section. For create, omit section entirely and put \
the whole procedure in body, with '## When to Use' and '## Procedure' headings and short bullets. \
For none, all other fields may be omitted.";

/// The user message: the exchange, then the skills an append may target.
#[must_use]
pub fn user_prompt(digest: &str, learned: &[Usage]) -> String {
    let mut text = String::from("THE FINISHED EXCHANGE\n\n");
    text.push_str(digest.trim());
    text.push_str("\n\nLEARNED SKILLS AN APPEND MAY TARGET\n\n");
    let targets: Vec<&Usage> = learned
        .iter()
        .filter(|row| {
            row.created_by == Origin::Agent && !row.pinned && row.state != State::Archived
        })
        .collect();
    if targets.is_empty() {
        text.push_str("None yet. Use create, or none.\n");
    } else {
        for row in targets {
            text.push_str("- `");
            text.push_str(&row.key);
            text.push('`');
            if let Some(description) = &row.description {
                text.push_str(": ");
                text.push_str(description);
            }
            text.push('\n');
        }
    }
    text
}

// --- The digest -------------------------------------------------------------

/// How much of one answer the review reads.
pub const DIGEST_ANSWER_CHARS: usize = 5_000;

/// How much of the question the review reads.
pub const DIGEST_QUESTION_CHARS: usize = 2_000;

/// How much of one tool call the review reads.
pub const DIGEST_TOOL_CHARS: usize = 600;

/// How much earlier context the review reads.
pub const DIGEST_CONTEXT_CHARS: usize = 1_200;

/// Build the review digest from a transcript and the answer it belongs to.
///
/// The question, the answer, the calls the turn made, and the two exchanges
/// before it. Bounded at every field, because a transcript can hold megabytes of
/// tool output and a review that carried it would cost more than the turn.
#[must_use]
pub fn digest(messages: &[crate::web::chat::Message], answer_id: &str) -> Option<String> {
    let at = messages
        .iter()
        .position(|message| message.id == answer_id)?;
    let answer = &messages[at];
    let question = messages[..at]
        .iter()
        .rev()
        .find(|message| message.role == crate::web::chat::Role::User)
        .map(|message| clip(&message.body, DIGEST_QUESTION_CHARS))
        .unwrap_or_default();
    let mut text = format!("THE OPERATOR ASKED:\n{question}\n\nTHE ASSISTANT ANSWERED:\n");
    text.push_str(&clip(&answer.body, DIGEST_ANSWER_CHARS));
    if !answer.tools.is_empty() {
        text.push_str("\n\nTOOLS THE TURN RAN:\n");
        for tool in &answer.tools {
            text.push_str("- `");
            text.push_str(&tool.name);
            text.push('`');
            if !tool.arguments.trim().is_empty() {
                text.push(' ');
                text.push_str(&clip(&tool.arguments, DIGEST_TOOL_CHARS / 2));
            }
            let status = if tool.is_error { " (failed)" } else { "" };
            text.push_str(status);
            text.push('\n');
        }
    }
    // The two exchanges before this one, oldest first, so the review can tell a
    // correction from a first attempt.
    let earlier: Vec<&crate::web::chat::Message> = messages[..at]
        .iter()
        .rev()
        .skip_while(|message| message.id != question_id(messages, at))
        .skip(1)
        .take(4)
        .collect();
    if !earlier.is_empty() {
        text.push_str("\n\nEARLIER IN THIS CONVERSATION (oldest first):\n");
        for message in earlier.into_iter().rev() {
            let who = match message.role {
                crate::web::chat::Role::User => "operator",
                crate::web::chat::Role::Assistant => "assistant",
            };
            text.push_str(&format!(
                "- {who}: {}\n",
                clip(&message.body, DIGEST_CONTEXT_CHARS / 2)
            ));
        }
    }
    Some(text)
}

fn question_id(messages: &[crate::web::chat::Message], answer_at: usize) -> String {
    messages[..answer_at]
        .iter()
        .rev()
        .find(|message| message.role == crate::web::chat::Role::User)
        .map(|message| message.id.clone())
        .unwrap_or_default()
}

// --- Presentation -----------------------------------------------------------

/// One row of the learning page's run table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Run id.
    pub id: String,
    /// When it was queued.
    pub created_at: String,
    /// `queued`, `running`, `done`, `skipped` or `failed`.
    pub status: String,
    /// What it did, when it did anything.
    pub action: Option<String>,
    /// The skill it touched.
    pub skill_key: Option<String>,
    /// The one-line reason or error.
    pub summary: Option<String>,
    /// The model that reviewed.
    pub model: Option<String>,
    /// The conversation the answer belonged to.
    pub conversation_id: String,
}

/// The newest runs, for the page.
///
/// # Errors
///
/// [`DbError`] when the table cannot be read.
pub fn recent_runs(db: &Db, limit: usize) -> Result<Vec<Run>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, created_at, status, action, skill_key, COALESCE(summary, error_text), \
                    model, conversation_id \
               FROM learning_runs ORDER BY created_at DESC LIMIT ?1",
        )?;
        let mapped = statement.query_map([limit as i64], |row| {
            Ok(Run {
                id: row.get(0)?,
                created_at: row.get(1)?,
                status: row.get(2)?,
                action: row.get(3)?,
                skill_key: row.get(4)?,
                summary: row.get(5)?,
                model: row.get(6)?,
                conversation_id: row.get(7)?,
            })
        })?;
        mapped.collect()
    })
}

/// One curator pass, as the page reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuratorReport {
    /// When it started.
    pub started_at: String,
    /// When it finished.
    pub finished_at: Option<String>,
    /// How many learned skills it looked at.
    pub checked: i64,
    /// How many it marked stale.
    pub marked_stale: i64,
    /// How many it archived.
    pub archived: i64,
    /// How many it brought back from stale.
    pub reactivated: i64,
}

/// The last maintenance pass, or `None` when one has never run.
///
/// # Errors
///
/// [`DbError`] when the table cannot be read.
pub fn last_curator(db: &Db) -> Result<Option<CuratorReport>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT started_at, finished_at, checked, marked_stale, archived, reactivated \
               FROM curator_runs ORDER BY started_at DESC LIMIT 1",
            [],
            |row| {
                Ok(CuratorReport {
                    started_at: row.get(0)?,
                    finished_at: row.get(1)?,
                    checked: row.get(2)?,
                    marked_stale: row.get(3)?,
                    archived: row.get(4)?,
                    reactivated: row.get(5)?,
                })
            },
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
    })
}

/// Counts for the page header.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Learned skills in the catalog.
    pub active: i64,
    /// Learned skills flagged unused.
    pub stale: i64,
    /// Learned skills out of the catalog.
    pub archived: i64,
    /// Runs still queued.
    pub pending: i64,
    /// Runs this window repaired rather than dropped.
    pub repaired: i64,
    /// Runs this window dropped on an invalid review.
    pub dropped: i64,
}

/// The counts the page shows.
///
/// # Errors
///
/// [`DbError`] when either table cannot be read.
pub fn totals(db: &Db) -> Result<Totals, DbError> {
    let mut totals = Totals::default();
    for row in rows(db)? {
        if row.created_by != Origin::Agent {
            continue;
        }
        match row.state {
            State::Active => totals.active += 1,
            State::Stale => totals.stale += 1,
            State::Archived => totals.archived += 1,
        }
    }
    // Repair and drop are counted apart on purpose: one number cannot say
    // whether the loop is losing lessons or saving them.
    totals.repaired = db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM learning_runs
              WHERE attempts > 0 AND status IN ('done', 'skipped')",
            [],
            |row| row.get(0),
        )
    })?;
    totals.dropped = db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM learning_runs WHERE status = 'failed'",
            [],
            |row| row.get(0),
        )
    })?;
    totals.pending = db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM learning_runs WHERE status IN ('queued', 'running')",
            [],
            |row| row.get(0),
        )
    })?;
    Ok(totals)
}

/// The settings a page saves, normalized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Switches {
    /// The master switch.
    pub enabled: bool,
    /// Review new turns.
    pub review: bool,
    /// Run maintenance.
    pub curator: bool,
}

impl Switches {
    /// Read the three switches.
    #[must_use]
    pub fn read(db: &Db) -> Self {
        Self {
            enabled: enabled(db),
            review: review_enabled(db),
            curator: curator_enabled(db),
        }
    }
}

/// Write the three switches.
///
/// # Errors
///
/// [`DbError`] when a row cannot be written.
pub fn save_switches(db: &Db, switches: Switches) -> Result<(), DbError> {
    for (key, on) in [
        (SETTING_ENABLED, switches.enabled),
        (SETTING_REVIEW_ENABLED, switches.review),
        (SETTING_CURATOR_ENABLED, switches.curator),
    ] {
        store::settings_put(db, key, if on { "true" } else { "false" })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::chat::{Message, Role, State as ChatState, ToolRecord};

    fn db() -> Db {
        Db::open_in_memory().expect("in-memory db")
    }

    fn shelf() -> (tempfile::TempDir, Shelf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let library = dir.path().join("skills");
        std::fs::create_dir_all(&library).expect("library");
        let history = dir.path().join("history");
        (dir, Shelf::new(library, history))
    }

    fn message(id: &str, role: Role, body: &str) -> Message {
        Message {
            id: id.to_owned(),
            role,
            body: body.to_owned(),
            state: ChatState::Complete,
            error_code: None,
            created_at: "2026-11-05T00:00:00.000Z".to_owned(),
            model: None,
            prompt_tokens: None,
            completion_tokens: None,
            cached_tokens: None,
            context_tokens: None,
            context_window: None,
            duration_ms: None,
            reasoning: None,
            tools: Vec::new(),
            client_message_id: None,
            job_id: None,
            rounds: None,
            capped: None,
            wrapped_up: None,
            quota: None,
        }
    }

    fn reviewed(action: &str) -> String {
        let mut value = serde_json::json!({
            "action": action,
            "key": "deploy-host",
            "name": "Deploy The Host",
            "description": "Ship the daemon safely.",
            "body": "Check the guard baseline first; a drifted neighbour is a stop. Run the deploy script; it waits for idle turns.",
            "reason": "the operator corrected the deploy order",
        });
        if action == "append" {
            value["section"] = serde_json::json!("Pitfalls");
            value["body"] = serde_json::json!(
                "Snapshot first, check second, deploy third; never edit the baseline to make drift pass."
            );
        }
        value.to_string()
    }

    #[test]
    fn a_review_is_parsed_out_of_a_fenced_think_block() {
        let raw = format!("<think>hmm</think>\n```json\n{}\n```", reviewed("create"));
        let review = parse_review(&raw).expect("a review");
        assert_eq!(review.action, Action::Create);
        assert_eq!(review.key, "deploy-host");
        assert_eq!(review.section, None);
    }

    #[test]
    fn a_create_may_carry_a_section_and_the_section_is_dropped() {
        let mut value: serde_json::Value = serde_json::from_str(&reviewed("create")).unwrap();
        value["section"] = serde_json::json!("Procedure");
        let review = parse_review(&value.to_string()).expect("a create");
        assert_eq!(review.action, Action::Create);
        assert_eq!(review.section, None);
    }

    #[test]
    fn an_append_accepts_every_section_the_library_names() {
        for section in SECTIONS {
            let mut value: serde_json::Value = serde_json::from_str(&reviewed("append")).unwrap();
            value["section"] = serde_json::json!(section);
            let review = parse_review(&value.to_string())
                .unwrap_or_else(|error| panic!("{section}: {error}"));
            assert_eq!(review.section.as_deref(), Some(section));
        }
    }

    #[test]
    fn a_none_review_needs_no_other_field() {
        let review = parse_review(r#"{"action":"none","reason":"small talk"}"#).expect("none");
        assert_eq!(review.action, Action::None);
        assert!(review.key.is_empty());
    }

    #[test]
    fn a_key_that_is_not_a_lowercase_slug_is_refused() {
        for key in ["Deploy-Host", "deploy host", "../x", "a", "deploy.host"] {
            let raw = reviewed("create").replace("deploy-host", key);
            assert!(parse_review(&raw).is_err(), "{key} must be refused");
        }
    }

    #[test]
    fn a_description_over_sixty_characters_is_refused() {
        let raw = reviewed("create").replace(
            "Ship the daemon safely.",
            "A very long description that is clearly longer than sixty characters in total.",
        );
        assert!(matches!(
            parse_review(&raw),
            Err(ReviewError::BadDescription)
        ));
    }

    #[test]
    fn a_body_that_looks_like_a_credential_is_refused() {
        let raw = reviewed("create").replace(
            "Check the guard baseline first",
            "Use sk-abcdefghijklmnopqrstuvwxyz1234567890 and check",
        );
        assert!(matches!(parse_review(&raw), Err(ReviewError::Secret)));
    }

    #[test]
    fn an_append_needs_a_section_the_library_knows() {
        let mut value: serde_json::Value = serde_json::from_str(&reviewed("append")).unwrap();
        value["section"] = serde_json::json!("Random Heading");
        assert!(matches!(
            parse_review(&value.to_string()),
            Err(ReviewError::BadSection(_))
        ));
        value["section"] = serde_json::json!("Pitfalls");
        assert!(parse_review(&value.to_string()).is_ok());
    }

    #[test]
    fn a_section_lands_under_its_existing_heading_instead_of_a_second_one() {
        let existing =
            "---\nname: Deploy\n---\n\n## Procedure\n\n1. build\n\n## Pitfalls\n\nold rule\n";
        let updated = insert_section(existing, "Pitfalls", "new rule");
        assert_eq!(updated.matches("## Pitfalls").count(), 1, "{updated}");
        assert!(updated.contains("old rule\n\nnew rule"), "{updated}");
    }

    #[test]
    fn a_create_writes_frontmatter_and_body_and_records_an_agent_skill() {
        let (_dir, shelf) = shelf();
        let db = db();
        let review = parse_review(&reviewed("create")).expect("a review");
        let applied = apply(&db, &shelf, &review, Some("run-1")).expect("apply");
        assert_eq!(applied.action, "create");
        let text = std::fs::read_to_string(shelf.dir.join("deploy-host").join("SKILL.md"))
            .expect("the file");
        assert!(text.starts_with(
            "---\nname: Deploy The Host\ndescription: Ship the daemon safely.\n---\n"
        ));
        let row = usage(&db, "deploy-host").expect("db").expect("a row");
        assert_eq!(row.created_by, Origin::Agent);
        assert_eq!(row.description.as_deref(), Some("Ship the daemon safely."));
    }

    #[test]
    fn a_create_over_an_operator_skill_is_refused_and_the_file_is_untouched() {
        let (_dir, shelf) = shelf();
        let db = db();
        std::fs::create_dir_all(shelf.dir.join("deploy-host")).unwrap();
        std::fs::write(
            shelf.dir.join("deploy-host").join("SKILL.md"),
            "---\nname: Mine\ndescription: Mine.\n---\n\nmine\n",
        )
        .unwrap();
        sync_library(&db, &shelf.dir).expect("sync");
        let review = parse_review(&reviewed("create")).expect("a review");
        let applied = apply(&db, &shelf, &review, None).expect("apply");
        assert_eq!(applied.action, "none");
        assert!(applied.note.contains("operator"), "{}", applied.note);
        let text = std::fs::read_to_string(shelf.dir.join("deploy-host").join("SKILL.md")).unwrap();
        assert!(text.contains("mine"), "{text}");
    }

    #[test]
    fn an_append_to_a_pinned_or_operator_skill_is_refused() {
        let (_dir, shelf) = shelf();
        let db = db();
        std::fs::create_dir_all(shelf.dir.join("deploy-host")).unwrap();
        std::fs::write(
            shelf.dir.join("deploy-host").join("SKILL.md"),
            "---\nname: Mine\ndescription: Mine.\n---\n\n## Pitfalls\n\nold\n",
        )
        .unwrap();
        sync_library(&db, &shelf.dir).expect("sync");
        let review = parse_review(&reviewed("append")).expect("a review");
        let applied = apply(&db, &shelf, &review, None).expect("apply");
        assert_eq!(applied.action, "none");
        assert!(applied.note.contains("operator"), "{}", applied.note);

        // The operator's row stays the operator's; pinning it is the second gate.
        set_pinned(&db, "deploy-host", true).expect("pin");
        let applied = apply(&db, &shelf, &review, None).expect("apply");
        assert_eq!(applied.action, "none");
        assert!(applied.note.contains("pinned"), "{}", applied.note);
    }

    #[test]
    fn an_append_moves_the_last_activity_and_keeps_a_history_copy() {
        let (_dir, shelf) = shelf();
        let db = db();
        let review = parse_review(&reviewed("create")).expect("a review");
        apply(&db, &shelf, &review, None).expect("create");
        let append = parse_review(&reviewed("append")).expect("a review");
        let applied = apply(&db, &shelf, &append, None).expect("append");
        assert_eq!(applied.action, "append");
        let text = std::fs::read_to_string(shelf.dir.join("deploy-host").join("SKILL.md")).unwrap();
        assert!(text.contains("## Pitfalls"), "{text}");
        let copies: Vec<_> = std::fs::read_dir(shelf.history.join("deploy-host"))
            .expect("history")
            .flatten()
            .collect();
        assert_eq!(copies.len(), 1, "the pre-append copy is kept");
    }

    #[test]
    fn archiving_moves_the_directory_and_restoring_brings_it_back() {
        let (_dir, shelf) = shelf();
        let db = db();
        let review = parse_review(&reviewed("create")).expect("a review");
        apply(&db, &shelf, &review, None).expect("create");
        let archived = archive(&shelf, "deploy-host").expect("archive");
        assert!(!shelf.dir.join("deploy-host").exists());
        assert!(archived.is_dir());
        let restored = restore(&shelf, "deploy-host").expect("restore");
        assert!(restored.is_dir());
        assert_eq!(restored, shelf.dir.join("deploy-host"));
    }

    #[test]
    fn a_use_bumps_the_count_and_reactivates_a_stale_skill_without_resurrecting_an_archived_one() {
        let db = db();
        note_use(&db, "one").expect("first use");
        note_use(&db, "one").expect("second use");
        let row = usage(&db, "one").unwrap().unwrap();
        assert_eq!(row.use_count, 2);
        assert_eq!(row.state, State::Active);

        set_state(&db, "one", State::Stale).expect("stale");
        note_use(&db, "one").expect("use again");
        assert_eq!(usage(&db, "one").unwrap().unwrap().state, State::Active);

        set_state(&db, "one", State::Archived).expect("archived");
        note_use(&db, "one").expect("a stray read");
        assert_eq!(usage(&db, "one").unwrap().unwrap().state, State::Archived);
    }

    #[test]
    fn sync_claims_on_disk_skills_as_the_operators_and_archives_vanished_ones() {
        let (_dir, shelf) = shelf();
        let db = db();
        std::fs::create_dir_all(shelf.dir.join("mine")).unwrap();
        std::fs::write(
            shelf.dir.join("mine").join("SKILL.md"),
            "---\nname: Mine\ndescription: Mine.\n---\n\nbody\n",
        )
        .unwrap();
        sync_library(&db, &shelf.dir).expect("sync");
        let row = usage(&db, "mine").unwrap().unwrap();
        assert_eq!(row.created_by, Origin::Operator);

        record_created(&db, "gone", None, None).expect("a learned row");
        sync_library(&db, &shelf.dir).expect("sync again");
        assert_eq!(usage(&db, "gone").unwrap().unwrap().state, State::Archived);
    }

    #[test]
    fn the_digest_carries_the_question_answer_tools_and_earlier_context() {
        let mut answer = message("a2", Role::Assistant, "It works now.");
        answer.tools.push(ToolRecord {
            name: "run_command".to_owned(),
            arguments: "{\"command\":\"cargo test\"}".to_owned(),
            output: "ok".to_owned(),
            is_error: false,
            asked: None,
        });
        let messages = vec![
            message("u1", Role::User, "first question"),
            message("a1", Role::Assistant, "first answer"),
            message("u2", Role::User, "why does the deploy fail"),
            answer,
        ];
        let digest = digest(&messages, "a2").expect("a digest");
        assert!(digest.contains("why does the deploy fail"), "{digest}");
        assert!(digest.contains("run_command"), "{digest}");
        assert!(digest.contains("first answer"), "{digest}");
    }

    #[test]
    fn a_missing_answer_id_has_no_digest() {
        let messages = vec![message("u1", Role::User, "hello")];
        assert!(digest(&messages, "nope").is_none());
    }

    #[test]
    fn the_switches_default_on_and_round_trip() {
        let db = db();
        assert_eq!(
            Switches::read(&db),
            Switches {
                enabled: true,
                review: true,
                curator: true
            }
        );
        save_switches(
            &db,
            Switches {
                enabled: false,
                review: true,
                curator: false,
            },
        )
        .expect("save");
        assert!(!Switches::read(&db).enabled);
        assert!(!Switches::read(&db).curator);
        assert!(Switches::read(&db).review);
    }

    /// The failure that killed a quarter of this loop's runs: a description one
    /// word over a sixty-character line, thrown away with no repair.
    #[test]
    fn an_over_length_description_is_clipped_at_a_word_boundary_and_accepted() {
        let long =
            "Run the guard script first then deploy then check the host then snapshot the diff";
        assert!(
            long.chars().count() > MAX_DESCRIPTION_CHARS,
            "the fixture must actually be over the line: {} chars",
            long.chars().count()
        );
        assert!(!valid_description(long), "the raw text is over the line");

        let fitted = fit_description(long).expect("it can be clipped");
        assert!(fitted.chars().count() <= MAX_DESCRIPTION_CHARS, "{fitted}");
        assert!(fitted.ends_with('.'), "{fitted}");
        assert!(
            valid_description(&fitted),
            "the clip must pass the rule: {fitted}"
        );
        // The cut is at a word boundary, so nothing is truncated mid-word.
        let body = fitted.trim_end_matches('.');
        assert!(long.starts_with(body), "the clip is a prefix: {fitted}");
        assert!(
            body.chars().last().is_some_and(char::is_alphanumeric),
            "a clip never ends on punctuation: {fitted}"
        );
    }

    #[test]
    fn a_two_sentence_description_is_refused_and_never_guessed_at() {
        let two = "Run the guard first. Then deploy the build.";
        assert!(
            sentence_break(two),
            "a capital after a full stop is a break"
        );
        assert!(
            !valid_description(two),
            "two sentences are not one sentence"
        );
        // Clipping cannot fix this one: which sentence the model meant is exactly
        // the guess the re-ask exists to avoid.
        let raw = format!(
            r#"{{"action":"create","key":"guard","name":"Guard","description":"{two}","body":"{}"}}"#,
            "x".repeat(40)
        );
        assert!(repair_review(&raw, &ReviewError::BadDescription).is_none());
    }

    #[test]
    fn repair_clips_the_description_field_and_the_result_parses() {
        let long =
            "Run the guard script first then deploy then check the host then snapshot the diff";
        let raw = format!(
            r#"{{"action":"create","key":"guard","name":"Guard","description":"{long}","body":"{}"}}"#,
            "Run the guard, then deploy.".repeat(3)
        );
        let error = parse_review(&raw).expect_err("the raw reply is refused");
        assert!(matches!(error, ReviewError::BadDescription));
        let fixed = repair_review(&raw, &error).expect("a deterministic repair exists");
        let review = parse_review(&fixed).expect("the repaired reply parses");
        assert!(review.description.chars().count() <= MAX_DESCRIPTION_CHARS);
        assert_eq!(review.key, "guard");
    }

    #[test]
    fn repair_flattens_a_name_carrying_a_newline_and_leaves_a_blank_one_alone() {
        let raw = r#"{"action":"create","key":"guard","name":"Guard\nscript","description":"Ship it behind the guard.","body":"Run the guard first, then deploy the build safely."}"#;
        let fixed = repair_review(raw, &ReviewError::BadName).expect("flattened");
        let review = parse_review(&fixed).expect("parses");
        assert_eq!(review.name, "Guard script");

        let blank = r#"{"action":"create","key":"guard","name":"  ","description":"Ship it behind the guard.","body":"Run the guard first, then deploy the build safely."}"#;
        assert!(repair_review(blank, &ReviewError::BadName).is_none());
    }

    /// The two lengths the guide names: ninety characters and one character over
    /// the sixty-character line. Both must repair to something the parser accepts.
    #[test]
    fn a_ninety_character_and_a_sixty_one_character_description_both_repair() {
        for length in [90usize, 61] {
            let words = "deploy guard snapshot host lane diff check widen timeout record";
            let mut long = String::new();
            while long.chars().count() < length {
                for word in words.split(' ') {
                    if long.chars().count() >= length {
                        break;
                    }
                    if !long.is_empty() {
                        long.push(' ');
                    }
                    long.push_str(word);
                }
            }
            let raw = format!(
                r#"{{"action":"create","key":"guard","name":"Guard","description":"{long}","body":"{}"}}"#,
                "Run the guard, then deploy.".repeat(3)
            );
            let error = parse_review(&raw).expect_err("over the line");
            assert!(matches!(error, ReviewError::BadDescription));
            let fixed = repair_review(&raw, &error).expect("repairable");
            let review = parse_review(&fixed).expect("the repair parses");
            assert!(
                review.description.chars().count() <= MAX_DESCRIPTION_CHARS,
                "{} chars",
                review.description.chars().count()
            );
        }
    }

    #[test]
    fn repair_flattens_an_over_long_append_and_keeps_it_readable() {
        let paragraph = "The lane health check runs before the deploy; ".repeat(120);
        let raw = format!(
            r#"{{"action":"append","key":"guard","name":"Guard","description":"Ship it behind the guard.","section":"Pitfalls","body":"{paragraph}"}}"#
        );
        let error = parse_review(&raw).expect_err("over the append limit");
        assert!(matches!(error, ReviewError::BadAppend), "{error}");
        let fixed = repair_review(&raw, &error).expect("repairable");
        let review = parse_review(&fixed).expect("the repair parses");
        assert!(review.body.len() <= MAX_APPEND_BYTES);
        assert!(review.body.chars().count() >= 10);
        assert!(!review.body.contains('\n'));
    }

    #[test]
    fn a_lesson_that_changed_no_skill_lands_in_memory() {
        let db = db();
        let review = Review {
            action: Action::None,
            key: String::new(),
            name: String::new(),
            description: "Always run the guard before the deploy.".to_owned(),
            section: None,
            body: "Guarding is not optional".to_owned(),
            reason: "a correction".to_owned(),
        };
        assert!(remember_lesson(&db, &review, Some("run-7")));
        let live = crate::mem::live_entries(&db, "memory", false, 10).expect("live");
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].entry, "Always run the guard before the deploy.");
        assert_eq!(live[0].origin, "model");
        assert_eq!(live[0].source_ref.as_deref(), Some("run-7"));
        // A duplicate is not a second row.
        assert!(remember_lesson(&db, &review, Some("run-7")));
        assert_eq!(
            crate::mem::live_entries(&db, "memory", false, 10)
                .expect("live")
                .len(),
            1
        );
    }

    #[test]
    fn only_arithmetic_refusals_are_repairable() {
        for error in [
            ReviewError::BadDescription,
            ReviewError::BadName,
            ReviewError::BadAppend,
        ] {
            assert!(is_repairable(&error), "{error} is arithmetic");
        }
        for error in [
            ReviewError::NoJson,
            ReviewError::BadKey("Nope".to_owned()),
            ReviewError::UnknownAction("delete".to_owned()),
            ReviewError::BadBody { min: 10 },
        ] {
            assert!(!is_repairable(&error), "{error} is not arithmetic");
        }
    }

    #[test]
    fn the_repair_switch_defaults_on_and_can_be_turned_off() {
        let db = db();
        assert!(review_repair(&db), "repair is on by default");
        crate::web::store::settings_put(&db, SETTING_REVIEW_REPAIR, "false").expect("off");
        assert!(!review_repair(&db));
    }
}
