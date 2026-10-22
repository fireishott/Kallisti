//! Schedules: cron, in this daemon, with the transcript as the record.
//!
//! A schedule is a row: a name, a five-field cron expression, a prompt, and a
//! switch. When the clock says it is due, the daemon starts a conversation named
//! after the schedule and runs one ordinary turn in it. That is the whole design,
//! and it is deliberate: a scheduled job is not a second execution path with its
//! own bugs, it is the same turn the composer starts, with the question supplied by
//! the row instead of by the operator.
//!
//! Five rules are enforced here rather than in a route:
//!
//! - **A cron expression is parsed at save time.** A row that cannot be parsed is
//!   refused by name while the operator is looking at the form, not at three in
//!   the morning when it should have fired.
//! - **`next_fire_at` is stored, not guessed.** Every claim recomputes it, so a
//!   daemon that was off for a day fires once when it comes back rather than
//!   replaying the day.
//! - **One claim at a time.** The claim is a conditional update against the value
//!   that was read, so two ticks racing cannot run the same schedule twice.
//! - **A run is a transcript.** The question and the answer are rows in
//!   `chat_messages`, so what a schedule produced is read where everything else
//!   is read, and the run row points at the conversation.
//! - **The cron is read in the operator's clock.** `0 7 * * *` means seven where
//!   the operator lives, and the page states which clock that was.

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::db::{Db, DbError};

/// The `settings` row holding the global switch.
pub const SETTING_ENABLED: &str = "schedules.enabled";

/// How often the ticker looks.
///
/// A minute would be exact and pointless: cron has one-minute resolution, so a
/// thirty-second tick fires a schedule within half a minute of its time.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(30);

/// The longest gap a single recompute will search, in minutes (about 400 days).
pub const MAX_SEARCH_MINUTES: i64 = 400 * 24 * 60;

/// The longest prompt a schedule may carry, in characters.
pub const MAX_PROMPT_CHARS: usize = 8_000;

/// The longest name, in characters.
pub const MAX_NAME_CHARS: usize = 60;

/// How many runs are kept per schedule before the oldest are pruned.
pub const RUNS_KEPT: i64 = 50;

/// How many runs the page shows per schedule.
pub const RUNS_SHOWN: usize = 8;

/// The default expression offered on the form: every morning at seven.
pub const DEFAULT_CRON: &str = "0 7 * * *";

/// The most schedules a daemon will hold. A guard against a runaway form, not a
/// product limit anybody will meet.
pub const MAX_SCHEDULES: usize = 200;

/// Whether scheduling is on at all. On by default: nothing fires until a row
/// exists, so the switch is a stop button rather than an unlock.
#[must_use]
pub fn enabled(stored: Option<&str>) -> bool {
    !matches!(stored.map(str::trim), Some("false"))
}

/// One parsed cron expression: five fields, in the order everybody writes them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cron {
    minute: Field,
    hour: Field,
    day_of_month: Field,
    month: Field,
    day_of_week: Field,
    raw: String,
}

/// A field's allowed values, as a bitset over a small range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Field {
    bits: u64,
    min: u32,
    max: u32,
}

impl Field {
    fn new(min: u32, max: u32) -> Self {
        Self { bits: 0, min, max }
    }

    fn set(&mut self, value: u32) {
        if (self.min..=self.max).contains(&value) {
            self.bits |= 1u64 << (value - self.min);
        }
    }

    fn has(&self, value: u32) -> bool {
        (self.min..=self.max).contains(&value) && self.bits & (1u64 << (value - self.min)) != 0
    }

    fn any(&self) -> bool {
        self.bits != 0
    }

    /// Every bit this field can hold, for telling a wildcard from a restriction.
    fn full(&self) -> u64 {
        let count = self.max - self.min + 1;
        if count >= 64 {
            u64::MAX
        } else {
            (1u64 << count) - 1
        }
    }

    fn restricted(&self) -> bool {
        self.bits != self.full()
    }
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const WEEKDAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

/// Parse one field. The sentence it returns names the field and its range.
fn parse_field(
    raw: &str,
    label: &str,
    min: u32,
    max: u32,
    names: Option<&[&str]>,
) -> Result<Field, String> {
    let mut field = Field::new(min, max);
    // Month names are 1-based (`jan` = 1); weekday names are 0-based (`sun` = 0).
    // The offset carries that difference, so the same name lookup serves both.
    let name_base = if names.is_some() && min == 1 { 1 } else { 0 };
    let value_of = |token: &str| -> Option<u32> {
        if let Some(names) = names {
            let lowered = token.to_ascii_lowercase();
            if let Some(index) = names.iter().position(|name| *name == lowered) {
                return Some(index as u32 + name_base);
            }
        }
        token.parse::<u32>().ok()
    };

    for part in raw.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(format!("the {label} field has an empty list item"));
        }
        let (range, step) = match part.split_once('/') {
            Some((range, step)) => {
                let step: u32 = step
                    .parse()
                    .map_err(|_| format!("`{step}` is not a step in the {label} field"))?;
                if step == 0 {
                    return Err(format!("a step of zero never matches in the {label} field"));
                }
                (range, step)
            }
            None => (part, 1),
        };
        let (from, to) = if range == "*" || range.is_empty() {
            (min, max)
        } else if let Some((from, to)) = range.split_once('-') {
            let from = value_of(from)
                .ok_or_else(|| format!("`{from}` is not a value the {label} field allows"))?;
            let to = value_of(to)
                .ok_or_else(|| format!("`{to}` is not a value the {label} field allows"))?;
            if from > to {
                return Err(format!(
                    "`{range}` runs backwards in the {label} field; write the small end first"
                ));
            }
            (from, to)
        } else {
            let value = value_of(range)
                .ok_or_else(|| format!("`{range}` is not a value the {label} field allows"))?;
            // A bare value with a step means "from here onward".
            if step > 1 {
                (value, max)
            } else {
                (value, value)
            }
        };
        if from < min || to > max {
            return Err(format!(
                "the {label} field allows {min} to {max}; `{part}` is outside that"
            ));
        }
        let mut value = from;
        while value <= to {
            field.set(value);
            value += step;
        }
    }
    if !field.any() {
        return Err(format!("the {label} field matches nothing"));
    }
    Ok(field)
}

impl Cron {
    /// Parse a five-field expression.
    ///
    /// # Errors
    ///
    /// The sentence the form shows next to the field.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err("a schedule needs a cron expression, like `0 7 * * *`".to_owned());
        }
        let fields: Vec<&str> = trimmed.split_whitespace().collect();
        if fields.len() != 5 {
            return Err(format!(
                "a cron expression has five fields (minute hour day month weekday); `{trimmed}` has {}",
                fields.len()
            ));
        }
        Ok(Self {
            minute: parse_field(fields[0], "minute", 0, 59, None)?,
            hour: parse_field(fields[1], "hour", 0, 23, None)?,
            day_of_month: parse_field(fields[2], "day of month", 1, 31, None)?,
            month: parse_field(fields[3], "month", 1, 12, Some(&MONTHS))?,
            day_of_week: parse_field(fields[4], "weekday", 0, 6, Some(&WEEKDAYS))?,
            raw: trimmed.to_owned(),
        })
    }

    /// The expression as it was saved.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// Whether one local minute matches.
    ///
    /// The day rules follow cron's own: when both day fields are restricted, either
    /// one matching is enough.
    fn matches(&self, at: OffsetDateTime) -> bool {
        let weekday = at.weekday().number_days_from_sunday();
        let day_ok = match (
            self.day_of_month.restricted(),
            self.day_of_week.restricted(),
        ) {
            (true, true) => {
                self.day_of_month.has(u32::from(at.day()))
                    || self.day_of_week.has(u32::from(weekday))
            }
            (true, false) => self.day_of_month.has(u32::from(at.day())),
            (false, true) => self.day_of_week.has(u32::from(weekday)),
            (false, false) => true,
        };
        self.minute.has(u32::from(at.minute()))
            && self.hour.has(u32::from(at.hour()))
            && self.month.has(u32::from(u8::from(at.month())))
            && day_ok
    }

    /// The first local minute strictly after `from` that matches.
    #[must_use]
    pub fn next_after(&self, from: OffsetDateTime) -> Option<OffsetDateTime> {
        let mut minute = from.replace_second(0).ok()?.replace_nanosecond(0).ok()?;
        minute += time::Duration::minutes(1);
        for _ in 0..MAX_SEARCH_MINUTES {
            if self.matches(minute) {
                return Some(minute);
            }
            minute += time::Duration::minutes(1);
        }
        None
    }
}

/// One schedule, as the page and the ticker read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    /// Row id.
    pub id: String,
    /// What the operator calls it, and what its conversation is named after.
    pub name: String,
    /// The cron expression, as typed.
    pub cron: String,
    /// The question the run asks.
    pub prompt: String,
    /// Whether it fires.
    pub enabled: bool,
    /// The next fire, RFC 3339 in UTC, when one is inside the search window.
    pub next_fire_at: Option<String>,
    /// When it last fired, UTC.
    pub last_fired_at: Option<String>,
    /// The last run's status: `running`, `ok`, `failed` or `refused`.
    pub last_status: Option<String>,
    /// The last run's failure code, when it failed.
    pub last_error: Option<String>,
    /// How many times it has fired.
    pub fired_count: i64,
}

/// Why a schedule could not be saved or read.
#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    /// The expression does not parse, or names no minute inside the window.
    #[error("{0}")]
    Cron(String),
    /// A name or prompt is empty, too long, or the library is full.
    #[error("{0}")]
    Text(String),
    /// The id names nothing.
    #[error("no schedule with that id exists")]
    Missing,
    /// The database refused.
    #[error("database: {0}")]
    Db(#[from] DbError),
}

/// Validate a name, a prompt and an expression, in the order a form shows them.
///
/// # Errors
///
/// [`ScheduleError::Text`] or [`ScheduleError::Cron`].
pub fn validate(name: &str, cron: &str, prompt: &str) -> Result<Cron, ScheduleError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ScheduleError::Text(
            "a schedule needs a name; it is what the conversation is called".to_owned(),
        ));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(ScheduleError::Text(format!(
            "a name must be {MAX_NAME_CHARS} characters or fewer"
        )));
    }
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err(ScheduleError::Text(
            "a schedule needs something to ask when it fires".to_owned(),
        ));
    }
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(ScheduleError::Text(format!(
            "the prompt must be {MAX_PROMPT_CHARS} characters or fewer"
        )));
    }
    Cron::parse(cron).map_err(ScheduleError::Cron)
}

// ---------------------------------------------------------------------------
// Storage
// ---------------------------------------------------------------------------

const COLUMNS: &str = "id, name, cron, prompt, enabled, next_fire_at, last_fired_at, \
                       last_status, last_error, fired_count";

fn row_to_schedule(row: &rusqlite::Row<'_>) -> rusqlite::Result<Schedule> {
    Ok(Schedule {
        id: row.get(0)?,
        name: row.get(1)?,
        cron: row.get(2)?,
        prompt: row.get(3)?,
        enabled: row.get::<_, i64>(4)? != 0,
        next_fire_at: row.get(5)?,
        last_fired_at: row.get(6)?,
        last_status: row.get(7)?,
        last_error: row.get(8)?,
        fired_count: row.get(9)?,
    })
}

/// Every schedule, by name.
///
/// # Errors
///
/// A database failure.
pub fn list(db: &Db) -> Result<Vec<Schedule>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM schedules ORDER BY name COLLATE NOCASE, id"
        ))?;
        let rows = statement.query_map([], row_to_schedule)?;
        rows.collect()
    })
}

/// One schedule by id.
///
/// # Errors
///
/// A database failure.
pub fn get(db: &Db, id: &str) -> Result<Option<Schedule>, DbError> {
    db.with_conn(|conn| {
        let mut statement =
            conn.prepare(&format!("SELECT {COLUMNS} FROM schedules WHERE id = ?1"))?;
        let mut rows = statement.query_map(rusqlite::params![id], row_to_schedule)?;
        rows.next().transpose()
    })
}

/// How many schedules exist.
///
/// # Errors
///
/// A database failure.
pub fn count(db: &Db) -> Result<i64, DbError> {
    db.with_conn(|conn| conn.query_row("SELECT COUNT(*) FROM schedules", [], |row| row.get(0)))
}

/// Insert one schedule, computing its first fire time now.
///
/// # Errors
///
/// [`ScheduleError::Cron`] when the expression cannot produce a fire time,
/// [`ScheduleError::Text`] when the library is full, or the database.
pub fn create(
    db: &Db,
    name: &str,
    cron: &str,
    prompt: &str,
    offset: time::UtcOffset,
) -> Result<String, ScheduleError> {
    if count(db)? >= MAX_SCHEDULES as i64 {
        return Err(ScheduleError::Text(format!(
            "this daemon already holds {MAX_SCHEDULES} schedules; delete one first"
        )));
    }
    let parsed = validate(name, cron, prompt)?;
    let next = parsed
        .next_after(OffsetDateTime::now_utc().to_offset(offset))
        .map(|at| stamp(at.to_offset(time::UtcOffset::UTC)))
        .ok_or_else(|| {
            ScheduleError::Cron(format!(
                "`{cron}` does not match any minute in the next year, so it would never fire"
            ))
        })?;
    let id = uuid::Uuid::now_v7().to_string();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO schedules (id, name, cron, prompt, enabled, next_fire_at)
             VALUES (?1, ?2, ?3, ?4, 1, ?5)",
            rusqlite::params![id, name.trim(), cron.trim(), prompt.trim(), next],
        )?;
        Ok(())
    })?;
    Ok(id)
}

/// Turn one schedule on or off.
///
/// Switching one on recomputes its next fire, so a schedule that was off over its
/// slot waits for the next one instead of firing immediately.
///
/// # Errors
///
/// [`ScheduleError::Missing`] or the database.
pub fn set_enabled(
    db: &Db,
    id: &str,
    on: bool,
    offset: time::UtcOffset,
) -> Result<(), ScheduleError> {
    let Some(schedule) = get(db, id)? else {
        return Err(ScheduleError::Missing);
    };
    let next = if on {
        Cron::parse(&schedule.cron)
            .ok()
            .and_then(|parsed| parsed.next_after(OffsetDateTime::now_utc().to_offset(offset)))
            .map(|at| stamp(at.to_offset(time::UtcOffset::UTC)))
    } else {
        None
    };
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE schedules
                SET enabled = ?2, next_fire_at = ?3,
                    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
              WHERE id = ?1",
            rusqlite::params![id, i64::from(on), next],
        )?;
        Ok(())
    })?;
    Ok(())
}

/// Delete one schedule and its run history.
///
/// # Errors
///
/// [`ScheduleError::Missing`] or the database.
pub fn delete(db: &Db, id: &str) -> Result<(), ScheduleError> {
    let changed = db.with_conn(|conn| {
        conn.execute(
            "DELETE FROM schedule_runs WHERE schedule_id = ?1",
            rusqlite::params![id],
        )?;
        conn.execute("DELETE FROM schedules WHERE id = ?1", rusqlite::params![id])
    })?;
    if changed == 0 {
        return Err(ScheduleError::Missing);
    }
    Ok(())
}

/// The schedules that are due at `now` (UTC), oldest fire first.
///
/// # Errors
///
/// A database failure.
pub fn due(db: &Db, now_utc: &str) -> Result<Vec<Schedule>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&format!(
            "SELECT {COLUMNS} FROM schedules
              WHERE enabled = 1 AND next_fire_at IS NOT NULL AND next_fire_at <= ?1
              ORDER BY next_fire_at, id"
        ))?;
        let rows = statement.query_map(rusqlite::params![now_utc], row_to_schedule)?;
        rows.collect()
    })
}

/// Claim one due schedule by moving its next fire forward.
///
/// The update is conditional on the exact value that was read, so two ticks that
/// overlap cannot both claim the same fire. `Ok(None)` means somebody else got
/// there first.
///
/// # Errors
///
/// [`ScheduleError::Cron`] when the row's expression no longer parses, or the
/// database.
pub fn claim(
    db: &Db,
    schedule: &Schedule,
    offset: time::UtcOffset,
) -> Result<Option<Option<String>>, ScheduleError> {
    let parsed = Cron::parse(&schedule.cron).map_err(ScheduleError::Cron)?;
    let next = parsed
        .next_after(OffsetDateTime::now_utc().to_offset(offset))
        .map(|at| stamp(at.to_offset(time::UtcOffset::UTC)));
    let changed = db.with_conn(|conn| {
        conn.execute(
            "UPDATE schedules SET next_fire_at = ?3
              WHERE id = ?1 AND next_fire_at IS ?2",
            rusqlite::params![schedule.id, schedule.next_fire_at, next],
        )
    })?;
    Ok((changed == 1).then_some(next))
}

/// Open a run row, so a run that is still going is visible while it goes.
///
/// # Errors
///
/// A database failure.
pub fn open_run(db: &Db, schedule_id: &str) -> Result<String, DbError> {
    let id = uuid::Uuid::now_v7().to_string();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO schedule_runs (id, schedule_id, status) VALUES (?1, ?2, 'running')",
            rusqlite::params![id, schedule_id],
        )?;
        Ok(())
    })?;
    Ok(id)
}

/// Settle every run row a previous process left behind.
///
/// A `running` row at boot belongs to a turn this process never started, so
/// left alone it sits in the history looking like work that is still happening,
/// and its schedule's `last_status` says `running` with no process behind it.
/// Every such row is settled through the same conditional update a cancelled
/// turn takes, and each touched schedule's own status follows it, so the page
/// and the history agree after a crash.
///
/// Returns how many run rows were settled.
///
/// # Errors
///
/// A database failure.
pub fn fail_orphaned(db: &Db) -> Result<usize, DbError> {
    db.with_conn(|conn| {
        let transaction = conn.unchecked_transaction()?;
        let mut schedules: Vec<String> = Vec::new();
        {
            let mut statement = transaction.prepare(
                "SELECT DISTINCT schedule_id FROM schedule_runs WHERE status = 'running'",
            )?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                schedules.push(row.get(0)?);
            }
        }
        if schedules.is_empty() {
            return Ok(0);
        }
        let changed = transaction.execute(
            "UPDATE schedule_runs
                SET status = 'failed',
                    error_code = 'interrupted',
                    summary = 'the daemon stopped before this run finished',
                    finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
              WHERE status = 'running'",
            [],
        )?;
        for id in &schedules {
            transaction.execute(
                "UPDATE schedules
                    SET last_status = 'failed',
                        last_error = 'interrupted',
                        updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                  WHERE id = ?1 AND last_status = 'running'",
                rusqlite::params![id],
            )?;
        }
        transaction.commit()?;
        Ok(changed)
    })
}

/// Record a finished run and settle the schedule's own counters.
///
/// Two writes in one transaction: a schedule that says `ok` with no run row would
/// be a lie, and a run row with no counter bump would hide the history. History is
/// pruned to [`RUNS_KEPT`] per schedule.
///
/// # Errors
///
/// A database failure.
pub fn settle_run(
    db: &Db,
    schedule_id: &str,
    run_id: &str,
    status: &str,
    conversation: Option<&str>,
    summary: Option<&str>,
    error_code: Option<&str>,
) -> Result<(), DbError> {
    db.with_conn(|conn| {
        let transaction = conn.unchecked_transaction()?;
        transaction.execute(
            "UPDATE schedule_runs
                SET finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                    status = ?2, conversation_id = ?3, summary = ?4, error_code = ?5
              WHERE id = ?1",
            rusqlite::params![run_id, status, conversation, summary, error_code],
        )?;
        transaction.execute(
            "UPDATE schedules
                SET last_fired_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                    last_status = ?2, last_error = ?3, fired_count = fired_count + 1,
                    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
              WHERE id = ?1",
            rusqlite::params![schedule_id, status, error_code],
        )?;
        transaction.execute(
            "DELETE FROM schedule_runs
              WHERE schedule_id = ?1 AND id NOT IN (
                  SELECT id FROM schedule_runs WHERE schedule_id = ?1
                   ORDER BY started_at DESC LIMIT ?2)",
            rusqlite::params![schedule_id, RUNS_KEPT],
        )?;
        transaction.commit()?;
        Ok(())
    })
}

/// One run, as the page shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// Row id.
    pub id: String,
    /// When it started, UTC.
    pub started_at: String,
    /// When it finished, UTC.
    pub finished_at: Option<String>,
    /// `running`, `ok`, `failed` or `refused`.
    pub status: String,
    /// The conversation it wrote into.
    pub conversation_id: Option<String>,
    /// The first line of what it said, or the failure.
    pub summary: Option<String>,
    /// The failure code, when it failed.
    pub error_code: Option<String>,
}

/// The newest runs of one schedule.
///
/// # Errors
///
/// A database failure.
pub fn runs(db: &Db, schedule_id: &str, limit: usize) -> Result<Vec<Run>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, started_at, finished_at, status, conversation_id, summary, error_code
               FROM schedule_runs WHERE schedule_id = ?1
              ORDER BY started_at DESC, id DESC LIMIT ?2",
        )?;
        let rows = statement.query_map(rusqlite::params![schedule_id, limit as i64], |row| {
            Ok(Run {
                id: row.get(0)?,
                started_at: row.get(1)?,
                finished_at: row.get(2)?,
                status: row.get(3)?,
                conversation_id: row.get(4)?,
                summary: row.get(5)?,
                error_code: row.get(6)?,
            })
        })?;
        rows.collect()
    })
}

/// The operator's clock, as an offset, resolved by SQLite rather than by a table of
/// ours that would go stale twice a year.
///
/// A read that fails is UTC: a schedule firing an hour off still fires, and the page
/// states which clock was used.
#[must_use]
pub fn local_offset(db: &Db, modifier: &str) -> time::UtcOffset {
    let seconds: Option<i64> = db
        .with_conn(|conn| {
            let local: Option<i64> = conn
                .query_row(
                    "SELECT CAST(strftime('%s', 'now', ?1) AS INTEGER)",
                    rusqlite::params![modifier],
                    |row| row.get(0),
                )
                .ok();
            let now: Option<i64> = conn
                .query_row("SELECT CAST(strftime('%s', 'now') AS INTEGER)", [], |row| {
                    row.get(0)
                })
                .ok();
            Ok(match (local, now) {
                (Some(local), Some(now)) => Some(local - now),
                _ => None,
            })
        })
        .ok()
        .flatten();
    let minutes = seconds.map_or(0, |delta| delta / 60);
    time::UtcOffset::from_whole_seconds((minutes * 60) as i32).unwrap_or(time::UtcOffset::UTC)
}

/// Render a UTC instant in the daemon's stored form: RFC 3339, UTC.
fn stamp(at: OffsetDateTime) -> String {
    at.format(&Rfc3339).unwrap_or_else(|_| {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            at.year(),
            u8::from(at.month()),
            at.day(),
            at.hour(),
            at.minute(),
            at.second()
        )
    })
}

/// Now, in the form [`due`] compares against.
///
/// Second resolution, so the string comparison against a stored `next_fire_at` is
/// sound: both sides are RFC 3339 with a `Z`, which sorts as time does.
#[must_use]
pub fn now_utc() -> String {
    stamp(
        OffsetDateTime::now_utc()
            .replace_nanosecond(0)
            .unwrap_or_else(|_| OffsetDateTime::now_utc()),
    )
}

/// Run due schedules on a thirty-second tick until the daemon stops.
///
/// The tick reads the database on this daemon's own runtime, then drives each
/// due schedule through the same turn path the composer uses. It never blocks the
/// listener: the provider round trips run in the turn's own blocking task, and
/// this loop only claims, opens and awaits.
pub async fn tick(state: crate::admin::AppState) {
    let mut interval = tokio::time::interval(TICK);
    // The first tick fires immediately; a schedule saved between ticks does not
    // wait an extra thirty seconds for nothing.
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let db = state.engine.db();
        let enabled = enabled(
            state
                .engine
                .setting(SETTING_ENABLED)
                .ok()
                .flatten()
                .as_deref(),
        );
        if !enabled {
            continue;
        }
        let zone = crate::zone::Zone::parse(
            state
                .engine
                .setting(crate::zone::SETTING_TIMEZONE)
                .ok()
                .flatten()
                .as_deref(),
        );
        let offset = local_offset(db, &zone.modifier());
        let now = now_utc();
        let due = match due(db, &now) {
            Ok(rows) => rows,
            Err(error) => {
                tracing::error!(%error, "scheduler: cannot read due schedules");
                continue;
            }
        };
        for schedule in due {
            // Claim before opening: two ticks racing on one schedule must not
            // both fire it, and the conditional update is that guarantee.
            match claim(db, &schedule, offset) {
                Ok(Some(Some(_))) => {}
                Ok(_) => continue,
                Err(error) => {
                    tracing::error!(%error, schedule = %schedule.id, "scheduler: cannot claim a due schedule");
                    continue;
                }
            }
            let run_id = match open_run(db, &schedule.id) {
                Ok(id) => id,
                Err(error) => {
                    tracing::error!(%error, schedule = %schedule.id, "scheduler: cannot open a run");
                    continue;
                }
            };
            tracing::info!(schedule = %schedule.id, name = %schedule.name, "scheduler: firing");
            // The run is awaited inline so a slow turn cannot stack a second run
            // of the same schedule behind it; a schedule whose turn runs long
            // simply misses its next slot, which is the right answer for a cron
            // that starts billed provider calls.
            crate::web::routes::run_scheduled(&state, schedule, run_id).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> OffsetDateTime {
        OffsetDateTime::parse(text, &Rfc3339).expect("a timestamp")
    }

    fn db() -> Db {
        Db::open_in_memory().expect("in-memory db")
    }

    #[test]
    fn a_plain_expression_parses() {
        let cron = Cron::parse("0 7 * * *").expect("parses");
        assert_eq!(cron.as_str(), "0 7 * * *");
    }

    #[test]
    fn a_wrong_field_count_is_refused_by_count() {
        let error = Cron::parse("0 7 *").expect_err("refuses");
        assert!(error.contains("five fields"), "{error}");
        assert!(error.contains("has 3"), "{error}");
    }

    #[test]
    fn a_value_outside_its_field_is_refused_by_name() {
        let error = Cron::parse("70 7 * * *").expect_err("refuses");
        assert!(error.contains("minute"), "{error}");
        assert!(error.contains("0 to 59"), "{error}");
    }

    #[test]
    fn steps_ranges_lists_and_names_parse() {
        assert!(Cron::parse("*/15 6-9 * * mon-fri").is_ok());
        assert!(Cron::parse("0 0 1,15 * *").is_ok());
        assert!(Cron::parse("30 2 * jan,jul sun").is_ok());
        assert!(Cron::parse("0 9 * * MON").is_ok(), "case does not matter");
        assert!(Cron::parse("0 0 29 2 *").is_ok(), "valid, just rare");
    }

    #[test]
    fn a_backwards_range_is_refused() {
        let error = Cron::parse("0 17-7 * * *").expect_err("refuses");
        assert!(error.contains("backwards"), "{error}");
    }

    #[test]
    fn a_step_of_zero_is_refused() {
        let error = Cron::parse("*/0 * * * *").expect_err("refuses");
        assert!(error.contains("step of zero"), "{error}");
    }

    #[test]
    fn the_next_fire_is_the_next_matching_minute() {
        let cron = Cron::parse("0 7 * * *").expect("parses");
        let next = cron
            .next_after(at("2026-10-22T02:59:00Z"))
            .expect("a next minute");
        assert_eq!(
            next.to_offset(time::UtcOffset::UTC),
            at("2026-10-22T07:00:00Z")
        );
    }

    #[test]
    fn a_fire_that_has_just_passed_rolls_to_tomorrow() {
        let cron = Cron::parse("0 7 * * *").expect("parses");
        let next = cron
            .next_after(at("2026-10-22T07:00:30Z"))
            .expect("a next minute");
        assert_eq!(
            next.to_offset(time::UtcOffset::UTC),
            at("2026-10-23T07:00:00Z")
        );
    }

    #[test]
    fn the_search_starts_at_the_next_whole_minute() {
        let cron = Cron::parse("* * * * *").expect("parses");
        let next = cron
            .next_after(at("2026-10-22T07:00:30Z"))
            .expect("a next minute");
        assert_eq!(
            next.to_offset(time::UtcOffset::UTC),
            at("2026-10-22T07:01:00Z")
        );
    }

    #[test]
    fn weekday_and_day_of_month_are_read_from_the_date() {
        // 2026-10-22 is a Thursday.
        let cron = Cron::parse("0 7 * * thu").expect("parses");
        assert!(cron.matches(at("2026-10-22T07:00:00Z")));
        assert!(!cron.matches(at("2026-10-21T07:00:00Z")), "Wednesday");
        let monthly = Cron::parse("0 7 22 * *").expect("parses");
        assert!(monthly.matches(at("2026-10-22T07:00:00Z")));
        assert!(!monthly.matches(at("2026-10-23T07:00:00Z")));
    }

    #[test]
    fn a_restricted_day_and_weekday_either_matching_is_enough() {
        // cron's own rule: both restricted means either may match.
        let cron = Cron::parse("0 0 1 * mon").expect("parses");
        assert!(
            cron.matches(at("2026-10-01T00:00:00Z")),
            "the first, a Thursday"
        );
        assert!(cron.matches(at("2026-10-05T00:00:00Z")), "a Monday");
        assert!(!cron.matches(at("2026-10-06T00:00:00Z")), "a Tuesday");
    }

    #[test]
    fn a_name_an_expression_and_a_prompt_are_all_required() {
        assert!(validate("", "0 7 * * *", "do it").is_err());
        assert!(validate("Morning", "0 7 * * *", "  ").is_err());
        assert!(validate("Morning", "nope", "do it").is_err());
        assert!(validate("Morning", "0 7 * * *", "do it").is_ok());
    }

    #[test]
    fn a_schedule_round_trips_and_keeps_its_next_fire() {
        let db = db();
        let offset = time::UtcOffset::UTC;
        let id = create(
            &db,
            "Morning brief",
            "0 7 * * *",
            "what is the plan",
            offset,
        )
        .expect("created");
        let rows = list(&db).expect("listed");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Morning brief");
        assert!(rows[0].enabled);
        assert!(rows[0].next_fire_at.is_some());

        set_enabled(&db, &id, false, offset).expect("off");
        let off = get(&db, &id).expect("read").expect("a row");
        assert!(!off.enabled);
        assert_eq!(
            off.next_fire_at, None,
            "a disabled schedule has no next fire"
        );

        set_enabled(&db, &id, true, offset).expect("on");
        let on = get(&db, &id).expect("read").expect("a row");
        assert!(on.next_fire_at.is_some());

        delete(&db, &id).expect("deleted");
        assert!(list(&db).expect("listed").is_empty());
    }

    #[test]
    fn a_due_schedule_is_claimed_once() {
        let db = db();
        let offset = time::UtcOffset::UTC;
        let id = create(&db, "Now", "* * * * *", "go", offset).expect("created");
        // Forced due by hand: the clock is not this test's subject.
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE schedules SET next_fire_at = '2020-01-01T00:00:00Z' WHERE id = ?1",
                rusqlite::params![id],
            )?;
            Ok(())
        })
        .expect("forced");

        let due_now = due(&db, "2026-10-22T00:00:00Z").expect("due");
        assert_eq!(due_now.len(), 1);
        let first = claim(&db, &due_now[0], offset).expect("claim");
        assert!(first.is_some(), "the first claim wins");
        assert!(first.unwrap().is_some(), "and it moves the fire forward");

        // A second tick holding a stale copy of the row loses the race.
        let second = claim(&db, &due_now[0], offset).expect("claim");
        assert!(second.is_none(), "the second claim changes nothing");
        assert!(due(&db, "2026-10-22T00:00:00Z").expect("due").is_empty());
    }

    #[test]
    fn a_run_is_recorded_and_the_counters_move() {
        let db = db();
        let offset = time::UtcOffset::UTC;
        crate::web::chat::seed_conversation(&db, "conv-1").expect("a conversation row");
        let id = create(&db, "Morning", "0 7 * * *", "go", offset).expect("created");
        let run = open_run(&db, &id).expect("a run row");
        settle_run(&db, &id, &run, "ok", Some("conv-1"), Some("all good"), None).expect("settled");

        let schedule = get(&db, &id).expect("read").expect("a row");
        assert_eq!(schedule.last_status.as_deref(), Some("ok"));
        assert_eq!(schedule.fired_count, 1);
        assert!(schedule.last_fired_at.is_some());
        let history = runs(&db, &id, 10).expect("history");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].status, "ok");
        assert_eq!(history[0].conversation_id.as_deref(), Some("conv-1"));
        assert!(history[0].finished_at.is_some());
    }

    #[test]
    fn a_failed_run_keeps_its_code() {
        let db = db();
        let offset = time::UtcOffset::UTC;
        let id = create(&db, "Broken", "0 7 * * *", "go", offset).expect("created");
        let run = open_run(&db, &id).expect("a run row");
        settle_run(&db, &id, &run, "failed", None, None, Some("no_provider")).expect("settled");
        let schedule = get(&db, &id).expect("read").expect("a row");
        assert_eq!(schedule.last_status.as_deref(), Some("failed"));
        assert_eq!(schedule.last_error.as_deref(), Some("no_provider"));
    }

    #[test]
    fn history_is_pruned_to_the_kept_window() {
        let db = db();
        let offset = time::UtcOffset::UTC;
        let id = create(&db, "Chatty", "* * * * *", "go", offset).expect("created");
        for _ in 0..(RUNS_KEPT + 5) {
            let run = open_run(&db, &id).expect("a run row");
            settle_run(&db, &id, &run, "ok", None, None, None).expect("settled");
        }
        let history = runs(&db, &id, 1_000).expect("history");
        assert_eq!(history.len() as i64, RUNS_KEPT);
    }

    /// A run row left `running` by a dead process is settled at boot, and so is
    /// the schedule's own status, so neither lies about work still happening.
    #[test]
    fn a_run_the_previous_process_left_running_is_settled_at_boot() {
        let db = db();
        let offset = time::UtcOffset::UTC;
        let crashed = create(&db, "Crashed", "0 7 * * *", "go", offset).expect("created");
        let settled = create(&db, "Settled", "30 7 * * *", "go", offset).expect("created");

        let open = open_run(&db, &crashed).expect("a run row");
        let done = open_run(&db, &settled).expect("a run row");
        settle_run(&db, &settled, &done, "ok", None, None, None).expect("settled");
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE schedules SET last_status = 'running' WHERE id = ?1",
                rusqlite::params![crashed],
            )?;
            Ok(())
        })
        .expect("marked running");

        assert_eq!(fail_orphaned(&db).expect("swept"), 1, "only the open row");
        let row = runs(&db, &crashed, 10).expect("history");
        assert_eq!(row.len(), 1);
        assert_eq!(row[0].status, "failed");
        assert_eq!(row[0].error_code.as_deref(), Some("interrupted"));
        assert!(row[0].finished_at.is_some(), "the row has an ending");

        let schedule = get(&db, &crashed).expect("read").expect("a row");
        assert_eq!(schedule.last_status.as_deref(), Some("failed"));
        assert_eq!(schedule.last_error.as_deref(), Some("interrupted"));
        assert_eq!(
            schedule.fired_count, 0,
            "an interrupted run is not a run that fired"
        );

        // The finished schedule is untouched, and a second sweep finds nothing.
        let untouched = get(&db, &settled).expect("read").expect("a row");
        assert_eq!(untouched.last_status.as_deref(), Some("ok"));
        assert_eq!(untouched.last_error, None);
        assert_eq!(fail_orphaned(&db).expect("swept"), 0);
        let _ = open;
    }

    #[test]
    fn the_offset_is_read_from_the_database_rather_than_assumed() {
        let db = db();
        // UTC is always zero, which pins the plumbing without pinning a zone.
        assert_eq!(local_offset(&db, "utc"), time::UtcOffset::UTC);
    }

    #[test]
    fn the_timestamp_form_is_second_resolution_utc() {
        let now = now_utc();
        assert!(now.ends_with('Z'), "{now}");
        assert_eq!(now.len(), 20, "{now}");
        // It parses as the same format the rows store.
        assert!(OffsetDateTime::parse(&now, &Rfc3339).is_ok(), "{now}");
    }
}
