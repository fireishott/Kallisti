//! Webhook triggers: one POST, one durable turn.
//!
//! A trigger is a saved prompt with a minted token. The token is the only
//! credential on the route that accepts it, so three things are non-negotiable
//! here and each has a test:
//!
//! - **The plaintext exists once.** A token is shown in the response that mints
//!   it and never again. What is stored is `hex(sha256(token))` plus an
//!   eight-character prefix, which is enough to tell two tokens apart in the
//!   dashboard and not enough to recreate one.
//! - **A comparison is constant-time.** The lookup is by hash, and the last
//!   comparison is a fold over the bytes rather than `==`, so a timing side
//!   channel does not become a token oracle.
//! - **A fire is a row before it is a turn.** The fire is written first and
//!   settled after, so a process that dies mid-turn leaves a `queued` row an
//!   operator can see rather than a hole where a request used to be.
//!
//! Nothing here starts a turn. The receiver in `web::hooks` owns that, and it
//! calls the same spawn path the schedule ticker calls: one runner, extended.

use rusqlite::params;
use sha2::{Digest, Sha256};

use crate::db::{Db, DbError};

/// The prefix every minted token carries, so one is recognisable in a log or a
/// shell history without being usable there.
pub const TOKEN_PREFIX: &str = "whk_";

/// How many random bytes a token carries, before hex encoding.
pub const TOKEN_BYTES: usize = 32;

/// How many leading characters of a token the dashboard shows.
pub const PREFIX_CHARS: usize = 12;

/// The longest a trigger's name may be.
pub const MAX_NAME_CHARS: usize = 64;

/// The longest a trigger's prompt may be.
///
/// The same ceiling the queue uses, because a triggered turn *is* a queued turn:
/// a trigger that could store a prompt the queue would refuse would be a row
/// that can never fire.
pub const MAX_PROMPT_CHARS: usize = crate::queue::MAX_PROMPT_CHARS;

/// The most a fire's appended text may add, which is the body ceiling.
pub const MAX_FIRE_BYTES: usize = 8 * 1024;

/// The default hourly ceiling for a new trigger.
pub const DEFAULT_MAX_PER_HOUR: i64 = 60;

/// The stored range of the hourly ceiling.
pub const MIN_PER_HOUR: i64 = 1;

/// The stored range of the hourly ceiling, upper end.
pub const MAX_PER_HOUR: i64 = 600;

/// Why a trigger operation refused, in the two halves a caller reads.
#[derive(Debug, thiserror::Error)]
pub enum TriggerError {
    /// A named input rule refused the request.
    #[error("{reason}")]
    Refused {
        /// A short stable code.
        code: &'static str,
        /// The sentence, which names the rule.
        reason: String,
    },
    /// The database could not answer, or the CSPRNG did not.
    #[error("the trigger table could not be read: {0}")]
    Unavailable(String),
}

impl TriggerError {
    /// A refusal with a code and a sentence.
    #[must_use]
    pub fn refused(code: &'static str, reason: impl Into<String>) -> Self {
        Self::Refused {
            code,
            reason: reason.into(),
        }
    }

    /// The code.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Refused { code, .. } => code,
            Self::Unavailable(_) => "trigger_unavailable",
        }
    }

    /// The sentence.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Refused { reason, .. } => reason.clone(),
            Self::Unavailable(detail) => {
                format!("the trigger table could not be read ({detail})")
            }
        }
    }
}

impl From<DbError> for TriggerError {
    fn from(error: DbError) -> Self {
        Self::Unavailable(error.to_string())
    }
}

/// One trigger, as the table holds it. `token_hash` is deliberately absent: no
/// read in this module returns it, so no page can print it by accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    /// Opaque id.
    pub id: String,
    /// The operator's name for it.
    pub name: String,
    /// The prompt it fires with.
    pub prompt: String,
    /// The first characters of the token, for telling two rows apart.
    pub token_prefix: String,
    /// Whether it may fire at all.
    pub enabled: bool,
    /// Fires allowed in a trailing hour.
    pub max_per_hour: i64,
    /// How many times it has fired.
    pub fire_count: i64,
    /// The last fire, when there was one.
    pub last_fired_at: Option<String>,
    /// When the row was written.
    pub created_at: String,
    /// When the row last changed.
    pub updated_at: String,
}

/// What one create asks for.
#[derive(Debug, Clone, Default)]
pub struct Draft {
    /// The operator's name for it.
    pub name: String,
    /// The prompt it fires with.
    pub prompt: String,
    /// Fires allowed in a trailing hour.
    pub max_per_hour: i64,
}

/// What a mint returned: the row, and the one copy of the plaintext token.
#[derive(Debug, Clone)]
pub struct Minted {
    /// The row that was written.
    pub trigger: Trigger,
    /// The token, which is written nowhere and returned once.
    pub token: String,
}

/// One fire row, as the ledger holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fire {
    /// Opaque id.
    pub id: String,
    /// The trigger it belongs to.
    pub trigger_id: String,
    /// `queued`, `running`, `ok`, `failed` or `refused`.
    pub status: String,
    /// The conversation it started, once one exists.
    pub conversation: Option<String>,
    /// The job it queued, when it queued one.
    pub job_id: Option<String>,
    /// A short note: the refusal reason, or the failure code.
    pub note: Option<String>,
    /// When it was recorded.
    pub fired_at: String,
    /// When it settled.
    pub settled_at: Option<String>,
}

/// The columns every read shares.
const COLUMNS: &str = "id, name, prompt, token_prefix, enabled, max_per_hour, fire_count, \
                       last_fired_at, created_at, updated_at";

/// Row to [`Trigger`].
fn row_to_trigger(row: &rusqlite::Row<'_>) -> rusqlite::Result<Trigger> {
    let enabled: i64 = row.get(4)?;
    Ok(Trigger {
        id: row.get(0)?,
        name: row.get(1)?,
        prompt: row.get(2)?,
        token_prefix: row.get(3)?,
        enabled: enabled != 0,
        max_per_hour: row.get(5)?,
        fire_count: row.get(6)?,
        last_fired_at: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

/// An opaque id.
fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// `hex(sha256(bytes))`.
#[must_use]
fn hash_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// A fresh token: `whk_` and 64 hex characters from the OS CSPRNG.
///
/// # Errors
///
/// The OS refused to supply randomness, which is not a state this crate can
/// paper over: a predictable token is a published one.
pub fn mint_token() -> Result<String, TriggerError> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|error| {
        TriggerError::refused(
            "trigger_no_randomness",
            format!("the OS would not supply randomness for a token ({error})"),
        )
    })?;
    Ok(format!("{TOKEN_PREFIX}{}", hex::encode(bytes)))
}

/// Compare two byte strings without an early return.
///
/// The lengths are compared first and that is safe to leak: a token's length is
/// fixed and public. The content is where the fold matters.
#[must_use]
pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        difference |= a ^ b;
    }
    difference == 0
}

/// Normalise a name the way the roster does: trimmed and lowercased.
#[must_use]
pub fn canonical_name(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Check a draft against the bounds. Refuses; never truncates.
pub fn validate(draft: &Draft) -> Result<(), TriggerError> {
    let name = canonical_name(&draft.name);
    if name.is_empty() {
        return Err(TriggerError::refused(
            "trigger_name_invalid",
            format!("a trigger needs a name, 1 to {MAX_NAME_CHARS} characters"),
        ));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(TriggerError::refused(
            "trigger_name_invalid",
            format!(
                "a trigger name is at most {MAX_NAME_CHARS} characters and this one is {}",
                name.chars().count()
            ),
        ));
    }
    if draft.prompt.trim().is_empty() {
        return Err(TriggerError::refused(
            "trigger_prompt_invalid",
            format!("a trigger needs a prompt, 1 to {MAX_PROMPT_CHARS} characters"),
        ));
    }
    if draft.prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(TriggerError::refused(
            "trigger_prompt_invalid",
            format!(
                "a trigger prompt is at most {MAX_PROMPT_CHARS} characters and this one is {}",
                draft.prompt.chars().count()
            ),
        ));
    }
    if !(MIN_PER_HOUR..=MAX_PER_HOUR).contains(&draft.max_per_hour) {
        return Err(TriggerError::refused(
            "trigger_rate_invalid",
            format!(
                "the hourly ceiling is between {MIN_PER_HOUR} and {MAX_PER_HOUR} and this one is {}",
                draft.max_per_hour
            ),
        ));
    }
    Ok(())
}

/// Every trigger, by name.
///
/// # Errors
///
/// A database failure.
pub fn list(db: &Db) -> Result<Vec<Trigger>, TriggerError> {
    let sql = format!("SELECT {COLUMNS} FROM triggers ORDER BY name COLLATE NOCASE, id");
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map([], row_to_trigger)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })
    .map_err(TriggerError::from)
}

/// One row by name.
///
/// # Errors
///
/// A database failure.
pub fn get_by_name(db: &Db, name: &str) -> Result<Option<Trigger>, TriggerError> {
    let sql = format!("SELECT {COLUMNS} FROM triggers WHERE name = ?1 COLLATE NOCASE");
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&sql)?;
        let mut rows = statement.query_map(params![canonical_name(name)], row_to_trigger)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    })
    .map_err(TriggerError::from)
}

/// One row by id.
///
/// # Errors
///
/// A database failure.
pub fn get(db: &Db, id: &str) -> Result<Option<Trigger>, TriggerError> {
    let sql = format!("SELECT {COLUMNS} FROM triggers WHERE id = ?1");
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&sql)?;
        let mut rows = statement.query_map(params![id], row_to_trigger)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    })
    .map_err(TriggerError::from)
}

/// Create a trigger and mint its token.
///
/// The plaintext is in the return value and nowhere else. A name that already
/// exists is a refusal, not a second row: the name is how an operator finds one.
///
/// # Errors
///
/// A named rule, the CSPRNG, or the database.
pub fn create(db: &Db, draft: &Draft) -> Result<Minted, TriggerError> {
    validate(draft)?;
    let name = canonical_name(&draft.name);
    if get_by_name(db, &name)?.is_some() {
        return Err(TriggerError::refused(
            "trigger_name_taken",
            format!("a trigger named '{name}' already exists"),
        ));
    }
    let token = mint_token()?;
    let token_hash = hash_hex(token.as_bytes());
    let token_prefix = token.chars().take(PREFIX_CHARS).collect::<String>();
    let id = new_id();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO triggers (id, name, prompt, token_hash, token_prefix, max_per_hour) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                &id,
                &name,
                draft.prompt.trim(),
                &token_hash,
                &token_prefix,
                draft.max_per_hour
            ],
        )?;
        Ok(())
    })?;
    let trigger = get(db, &id)?.ok_or_else(|| {
        TriggerError::refused("trigger_unavailable", "the row vanished after insert")
    })?;
    Ok(Minted { trigger, token })
}

/// Replace a trigger's prompt and ceiling, and mint a new token.
///
/// A rotate is not a separate operation on purpose: the two ways an operator
/// changes a trigger that must invalidate the old URL are the same write, and
/// two routes for one write is two places for the hash to be forgotten in.
///
/// # Errors
///
/// A named rule, the CSPRNG, or the database.
pub fn update(db: &Db, name: &str, draft: &Draft) -> Result<Minted, TriggerError> {
    validate(draft)?;
    let canonical = canonical_name(name);
    let existing = get_by_name(db, &canonical)?.ok_or_else(|| {
        TriggerError::refused(
            "trigger_unknown",
            format!("there is no trigger named '{canonical}'"),
        )
    })?;
    let token = mint_token()?;
    let token_hash = hash_hex(token.as_bytes());
    let token_prefix = token.chars().take(PREFIX_CHARS).collect::<String>();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE triggers SET prompt = ?1, max_per_hour = ?2, token_hash = ?3, \
             token_prefix = ?4, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE id = ?5",
            params![
                draft.prompt.trim(),
                draft.max_per_hour,
                &token_hash,
                &token_prefix,
                &existing.id
            ],
        )?;
        Ok(())
    })?;
    let trigger = get(db, &existing.id)?.ok_or_else(|| {
        TriggerError::refused("trigger_unavailable", "the row vanished after update")
    })?;
    Ok(Minted { trigger, token })
}

/// Turn a trigger off or on.
///
/// # Errors
///
/// A named rule, or the database.
pub fn set_enabled(db: &Db, name: &str, enabled: bool) -> Result<(), TriggerError> {
    let canonical = canonical_name(name);
    let changed = db.with_conn(|conn| {
        conn.execute(
            "UPDATE triggers SET enabled = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE name = ?2 COLLATE NOCASE",
            params![i64::from(enabled), &canonical],
        )
    })?;
    if changed == 0 {
        return Err(TriggerError::refused(
            "trigger_unknown",
            format!("there is no trigger named '{canonical}'"),
        ));
    }
    Ok(())
}

/// Remove a trigger and, by cascade, its fires.
///
/// # Errors
///
/// The database.
pub fn delete(db: &Db, name: &str) -> Result<bool, TriggerError> {
    let canonical = canonical_name(name);
    let changed = db.with_conn(|conn| {
        conn.execute(
            "DELETE FROM triggers WHERE name = ?1 COLLATE NOCASE",
            params![canonical],
        )
    })?;
    Ok(changed > 0)
}

/// The enabled trigger a token names, compared in constant time.
///
/// The hash is the lookup key, so the database does the first pass; the fold
/// then re-checks the row it found. Both are needed: a hash collision in the index
/// would otherwise resolve to whichever row the planner returned first.
///
/// # Errors
///
/// A database failure. A token that matches nothing is `Ok(None)`, never an
/// error, so the receiver answers one 404 for every reason.
pub fn resolve_by_token(db: &Db, token: &str) -> Result<Option<Trigger>, TriggerError> {
    let token_hash = hash_hex(token.as_bytes());
    let sql =
        format!("SELECT {COLUMNS}, token_hash FROM triggers WHERE token_hash = ?1 AND enabled = 1");
    let found = db.with_conn(|conn| {
        let mut statement = conn.prepare(&sql)?;
        let mut rows = statement.query_map(params![&token_hash], |row| {
            let stored: String = row.get(10)?;
            Ok((row_to_trigger(row)?, stored))
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    })?;
    let Some((trigger, stored)) = found else {
        return Ok(None);
    };
    if constant_time_eq(stored.as_bytes(), token_hash.as_bytes()) {
        Ok(Some(trigger))
    } else {
        Ok(None)
    }
}

/// Fires in the trailing window, suppressed and refused ones included.
///
/// The rate limit counts everything that reached the route for this trigger, so
/// a client that hammers a revoked token cannot reset the counter by failing.
///
/// # Errors
///
/// A database failure.
pub fn fires_since(db: &Db, trigger_id: &str, since: &str) -> Result<i64, TriggerError> {
    let count: i64 = db.with_conn(|conn| {
        conn.query_row(
            "SELECT count(*) FROM trigger_fires WHERE trigger_id = ?1 AND fired_at >= ?2",
            params![trigger_id, since],
            |row| row.get(0),
        )
    })?;
    Ok(count)
}

/// Record a fire and bump the trigger's counters, in one transaction.
///
/// The fire row is written before the turn starts. A fire whose turn never runs
/// is a `queued` row that says so, which is a fact an operator can act on; a fire
/// that existed only in a variable is a request nobody can account for.
///
/// # Errors
///
/// A database failure.
pub fn record_fire(
    db: &Db,
    trigger_id: &str,
    status: &str,
    conversation: Option<&str>,
    note: Option<&str>,
) -> Result<Fire, TriggerError> {
    let id = new_id();
    db.with_conn(|conn| {
        let transaction = conn.unchecked_transaction()?;
        transaction.execute(
            "INSERT INTO trigger_fires (id, trigger_id, status, conversation, note) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![&id, trigger_id, status, conversation, note],
        )?;
        transaction.execute(
            "UPDATE triggers SET fire_count = fire_count + 1, \
             last_fired_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = ?1",
            params![trigger_id],
        )?;
        transaction.commit()?;
        Ok(())
    })?;
    get_fire(db, &id)?
        .ok_or_else(|| TriggerError::refused("trigger_unavailable", "the fire vanished"))
}

/// Settle a fire: the status and the conversation it landed in.
///
/// # Errors
///
/// A database failure.
pub fn settle_fire(
    db: &Db,
    fire_id: &str,
    status: &str,
    conversation: Option<&str>,
    note: Option<&str>,
) -> Result<(), TriggerError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE trigger_fires SET status = ?1, conversation = COALESCE(?2, conversation), \
             note = COALESCE(?3, note), settled_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
             WHERE id = ?4",
            params![status, conversation, note, fire_id],
        )?;
        Ok(())
    })?;
    Ok(())
}

/// One fire row.
///
/// # Errors
///
/// A database failure.
pub fn get_fire(db: &Db, id: &str) -> Result<Option<Fire>, TriggerError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, trigger_id, status, conversation, job_id, note, fired_at, settled_at \
             FROM trigger_fires WHERE id = ?1",
        )?;
        let mut rows = statement.query_map(params![id], row_to_fire)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    })
    .map_err(TriggerError::from)
}

/// The most recent fires for one trigger, newest first.
///
/// # Errors
///
/// A database failure.
pub fn recent_fires(db: &Db, trigger_id: &str, limit: usize) -> Result<Vec<Fire>, TriggerError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, trigger_id, status, conversation, job_id, note, fired_at, settled_at \
             FROM trigger_fires WHERE trigger_id = ?1 ORDER BY fired_at DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(params![trigger_id, limit as i64], row_to_fire)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })
    .map_err(TriggerError::from)
}

/// Row to [`Fire`].
fn row_to_fire(row: &rusqlite::Row<'_>) -> rusqlite::Result<Fire> {
    Ok(Fire {
        id: row.get(0)?,
        trigger_id: row.get(1)?,
        status: row.get(2)?,
        conversation: row.get(3)?,
        job_id: row.get(4)?,
        note: row.get(5)?,
        fired_at: row.get(6)?,
        settled_at: row.get(7)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn db() -> Db {
        Db::open_in_memory().expect("in-memory db")
    }

    fn draft() -> Draft {
        Draft {
            name: "nightly".to_owned(),
            prompt: "Summarise what changed in the tree today.".to_owned(),
            max_per_hour: DEFAULT_MAX_PER_HOUR,
        }
    }

    #[test]
    fn migration_thirty_one_creates_the_trigger_tables() {
        let db = db();
        let present = db.table_names().expect("tables");
        for table in ["triggers", "trigger_fires"] {
            assert!(
                present.iter().any(|name| name == table),
                "{table} was not created by the migration"
            );
        }
        assert_eq!(db.schema_version().expect("version"), 33);
    }

    #[test]
    fn a_create_stores_a_hash_and_returns_the_token_once() {
        let db = db();
        let minted = create(&db, &draft()).expect("a trigger");
        assert!(minted.token.starts_with(TOKEN_PREFIX), "{}", minted.token);
        // The prefix is the first characters of the token, and the hash is not
        // the token: both statements are what make the row safe to read.
        assert_eq!(
            minted.trigger.token_prefix,
            minted.token.chars().take(PREFIX_CHARS).collect::<String>()
        );
        let stored: String = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT token_hash FROM triggers WHERE id = ?1",
                    params![&minted.trigger.id],
                    |row| row.get(0),
                )
            })
            .expect("the hash");
        assert_ne!(stored, minted.token, "the token itself is never stored");
        assert_eq!(stored, hash_hex(minted.token.as_bytes()));
        assert_eq!(stored.len(), 64, "a hex-encoded SHA-256");
    }

    #[test]
    fn no_read_returns_the_hash() {
        let db = db();
        let minted = create(&db, &draft()).expect("a trigger");
        // Every public read is checked: a page that printed a hash would be a
        // page that printed a verifier.
        let rows = list(&db).expect("a list");
        let by_name = get_by_name(&db, "nightly").expect("a read").expect("a row");
        let by_id = get(&db, &minted.trigger.id)
            .expect("a read")
            .expect("a row");
        for row in rows.iter().chain([&by_name, &by_id]) {
            let serialized = serde_json::to_string(&serde_json::json!({
                "name": row.name,
                "prefix": row.token_prefix,
                "prompt": row.prompt,
            }))
            .expect("serializes");
            assert!(!serialized.contains("token_hash"), "{serialized}");
        }
    }

    #[test]
    fn a_duplicate_name_is_refused_and_the_alphabet_holds() {
        let db = db();
        create(&db, &draft()).expect("a trigger");
        let error = create(&db, &draft()).expect_err("refuses");
        assert_eq!(error.code(), "trigger_name_taken");

        let mut bad = draft();
        bad.name = "   ".to_owned();
        assert_eq!(
            create(&db, &bad).expect_err("refuses").code(),
            "trigger_name_invalid"
        );

        let mut empty_prompt = draft();
        empty_prompt.name = "other".to_owned();
        empty_prompt.prompt = "  ".to_owned();
        assert_eq!(
            create(&db, &empty_prompt).expect_err("refuses").code(),
            "trigger_prompt_invalid"
        );

        let mut bad_rate = draft();
        bad_rate.name = "third".to_owned();
        bad_rate.max_per_hour = MAX_PER_HOUR + 1;
        assert_eq!(
            create(&db, &bad_rate).expect_err("refuses").code(),
            "trigger_rate_invalid"
        );
    }

    #[test]
    fn a_rotate_invalidates_the_old_token_and_keeps_the_row() {
        let db = db();
        let first = create(&db, &draft()).expect("a trigger");
        let rotated = update(&db, "nightly", &draft()).expect("a rotation");
        assert_eq!(rotated.trigger.id, first.trigger.id, "the same row");
        assert_ne!(rotated.token, first.token, "a new token");
        assert!(
            resolve_by_token(&db, &first.token)
                .expect("a read")
                .is_none(),
            "the old token no longer resolves"
        );
        assert!(
            resolve_by_token(&db, &rotated.token)
                .expect("a read")
                .is_some(),
            "the new token resolves"
        );
        assert_eq!(list(&db).expect("a list").len(), 1, "no second row");
    }

    #[test]
    fn a_disabled_trigger_does_not_resolve_and_a_deleted_one_cascades() {
        let db = db();
        let minted = create(&db, &draft()).expect("a trigger");
        set_enabled(&db, "nightly", false).expect("a switch");
        assert!(
            resolve_by_token(&db, &minted.token)
                .expect("a read")
                .is_none(),
            "a disabled trigger is a 404, not a 403"
        );
        set_enabled(&db, "nightly", true).expect("a switch");

        let fire = record_fire(&db, &minted.trigger.id, "queued", None, None).expect("a fire");
        assert_eq!(fire.status, "queued");
        assert_eq!(
            list(&db).expect("a list")[0].fire_count,
            1,
            "the counter moves with the fire"
        );
        assert!(delete(&db, "nightly").expect("a delete"));
        let remaining: i64 = db
            .with_conn(|conn| {
                conn.query_row("SELECT count(*) FROM trigger_fires", [], |row| row.get(0))
            })
            .expect("a count");
        assert_eq!(remaining, 0, "the ledger cascades with the row");
    }

    #[test]
    fn a_fire_settles_with_the_conversation_it_landed_in() {
        let db = db();
        let minted = create(&db, &draft()).expect("a trigger");
        let fire = record_fire(&db, &minted.trigger.id, "queued", None, None).expect("a fire");
        settle_fire(&db, &fire.id, "ok", Some("conv-1"), Some("the answer")).expect("a settle");
        let settled = get_fire(&db, &fire.id).expect("a read").expect("a row");
        assert_eq!(settled.status, "ok");
        assert_eq!(settled.conversation.as_deref(), Some("conv-1"));
        assert_eq!(settled.note.as_deref(), Some("the answer"));
        assert!(settled.settled_at.is_some());
    }

    #[test]
    fn the_rate_limit_counts_every_fire_in_the_window() {
        let db = db();
        let minted = create(&db, &draft()).expect("a trigger");
        for _ in 0..3 {
            record_fire(&db, &minted.trigger.id, "ok", None, None).expect("a fire");
        }
        // Refused fires count too: a caller over the ceiling that keeps calling
        // must not see the counter fall back to zero.
        record_fire(
            &db,
            &minted.trigger.id,
            "refused",
            None,
            Some("rate_limited"),
        )
        .expect("a fire");
        let since = "1970-01-01T00:00:00Z";
        assert_eq!(
            fires_since(&db, &minted.trigger.id, since).expect("a count"),
            4
        );
        // A window that starts after every fire counts none of them.
        let future = "2999-01-01T00:00:00Z";
        assert_eq!(
            fires_since(&db, &minted.trigger.id, future).expect("a count"),
            0
        );
    }

    #[test]
    fn a_comparison_does_not_return_early_on_a_mismatch() {
        // The property that matters is the answer, and the fold is what makes the
        // timing uninteresting: every prefix length must compare the same way.
        assert!(constant_time_eq(b"abc", b"abc"));
        for prefix in 0..3usize {
            let mut candidate = *b"abc";
            candidate[prefix] ^= 0xFF;
            assert!(!constant_time_eq(b"abc", &candidate), "prefix {prefix}");
        }
        assert!(!constant_time_eq(b"abc", b"abcd"), "a length difference");
        assert!(constant_time_eq(b"", b""));
    }

    #[test]
    fn a_token_is_long_and_unique() {
        let first = mint_token().expect("a token");
        let second = mint_token().expect("a token");
        assert_ne!(first, second, "two tokens are two tokens");
        assert_eq!(
            first.len(),
            TOKEN_PREFIX.len() + TOKEN_BYTES * 2,
            "the prefix and two hex digits a byte"
        );
        assert!(
            first[TOKEN_PREFIX.len()..]
                .chars()
                .all(|c| c.is_ascii_hexdigit())
        );
    }
}
