//! The small parameterised reads and writes the dashboard surface needs.
//!
//! Everything here is one statement, bound with `?1` placeholders, and returns
//! the database's own error rather than a rendered string: a caller that cannot
//! answer a page turns it into a 500 with a body, never a silent empty 200.
//!
//! The module exists so the handlers stay readable and so no handler ever
//! interpolates a value into SQL. Counts are whole-table reads on tables that
//! hold tens of rows on this host, which is why they are not cached.

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, params};

use crate::db::{Db, DbError};

/// The five job statuses the dashboard reports, in the schema's own order.
pub const JOB_STATUSES: [&str; 5] = ["queued", "running", "succeeded", "failed", "cancelled"];

/// One `audit_log` row, as the Activity page renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// When the row was written, in the database's own format.
    pub occurred_at: String,
    /// Who did it. `loopback` for everything the admin listener writes.
    pub actor: String,
    /// The stable action name.
    pub action: String,
    /// The non-secret detail, when there is one.
    pub detail: Option<String>,
}

/// One `settings` value, or `None` when the key has never been written.
pub fn settings_get(db: &Db, key: &str) -> Result<Option<String>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            params![key],
            |row| row.get::<_, String>(0),
        )
        .optional()
    })
}

/// Write one `settings` value, creating or replacing the row.
pub fn settings_put(db: &Db, key: &str, value: &str) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2) \
             ON CONFLICT (key) DO UPDATE SET value = excluded.value, \
             updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            params![key, value],
        )?;
        Ok(())
    })
}

/// Every `settings` row, key order.
pub fn settings_all(db: &Db) -> Result<BTreeMap<String, String>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare("SELECT key, value FROM settings ORDER BY key")?;
        let mut rows = statement.query([])?;
        let mut out = BTreeMap::new();
        while let Some(row) = rows.next()? {
            out.insert(row.get::<_, String>(0)?, row.get::<_, String>(1)?);
        }
        Ok(out)
    })
}

/// How many device sessions exist, revoked or not.
pub fn session_count(db: &Db) -> Result<i64, DbError> {
    db.with_conn(|conn| conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0)))
}

/// How many device rows exist, revoked or not.
pub fn device_count(db: &Db) -> Result<i64, DbError> {
    db.with_conn(|conn| conn.query_row("SELECT COUNT(*) FROM devices", [], |row| row.get(0)))
}

/// Jobs by status, every status present, zero filled.
///
/// The dashboard must read "failed 0" rather than omit the row: a status that
/// disappears when it is zero is a status nobody notices going up.
pub fn jobs_by_status(db: &Db) -> Result<Vec<(&'static str, i64)>, DbError> {
    let counts: BTreeMap<String, i64> = db.with_conn(|conn| {
        let mut statement = conn.prepare("SELECT status, COUNT(*) FROM jobs GROUP BY status")?;
        let mut rows = statement.query([])?;
        let mut out = BTreeMap::new();
        while let Some(row) = rows.next()? {
            out.insert(row.get::<_, String>(0)?, row.get::<_, i64>(1)?);
        }
        Ok(out)
    })?;
    Ok(JOB_STATUSES
        .iter()
        .map(|status| (*status, counts.get(*status).copied().unwrap_or(0)))
        .collect())
}

/// The newest `limit` audit rows, newest first.
pub fn audit_recent(db: &Db, limit: i64) -> Result<Vec<AuditEntry>, DbError> {
    let limit = limit.clamp(1, 1000);
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT occurred_at, actor, action, detail FROM audit_log \
             ORDER BY id DESC LIMIT ?1",
        )?;
        let mut rows = statement.query(params![limit])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(AuditEntry {
                occurred_at: row.get(0)?,
                actor: row.get(1)?,
                action: row.get(2)?,
                detail: row.get(3)?,
            });
        }
        Ok(out)
    })
}

/// How many audit rows exist, so the page can say what it is not showing.
pub fn audit_count(db: &Db) -> Result<i64, DbError> {
    db.with_conn(|conn| conn.query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0)))
}

// ---------------------------------------------------------------------------
// Sessions, jobs and devices
// ---------------------------------------------------------------------------

/// One `sessions` row joined to its device, as the list and the detail page
/// render it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// Session id.
    pub id: String,
    /// The device, when the row still has one.
    pub device_id: Option<String>,
    /// The device's display name, `None` when the device row is gone.
    pub device_name: Option<String>,
    /// The agent string the session was opened with.
    pub user_agent: Option<String>,
    /// When it was opened.
    pub created_at: String,
    /// The last request it carried a request, when it has.
    pub last_seen_at: Option<String>,
    /// When it stops being valid.
    pub expires_at: String,
    /// When it was revoked, if it was.
    pub revoked_at: Option<String>,
    /// `live`, `expired` or `revoked`, decided by the database's own clock.
    pub state: String,
    /// How many jobs name this session.
    pub job_count: i64,
}

/// The select both session reads share. The state is computed against SQLite's
/// clock rather than the process clock, so a page cannot disagree with a query,
/// and the comparison is on the fixed-width second prefix both writers use.
const SESSION_SELECT: &str = "\
SELECT s.id, s.device_id, d.name, s.user_agent, s.created_at, s.last_seen_at, s.expires_at, \
s.revoked_at, \
CASE \
WHEN s.revoked_at IS NOT NULL THEN 'revoked' \
WHEN substr(s.expires_at, 1, 19) <= strftime('%Y-%m-%dT%H:%M:%SZ', 'now') THEN 'expired' \
ELSE 'live' END AS state, \
(SELECT COUNT(*) FROM jobs j WHERE j.session_id = s.id) AS job_count \
FROM sessions s LEFT JOIN devices d ON d.id = s.device_id";

/// One row of the session select.
fn session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        id: row.get(0)?,
        device_id: row.get(1)?,
        device_name: row.get(2)?,
        user_agent: row.get(3)?,
        created_at: row.get(4)?,
        last_seen_at: row.get(5)?,
        expires_at: row.get(6)?,
        revoked_at: row.get(7)?,
        state: row.get(8)?,
        job_count: row.get(9)?,
    })
}

/// Every session, newest first.
pub fn sessions_list(db: &Db) -> Result<Vec<SessionRow>, DbError> {
    let sql = format!("{SESSION_SELECT} ORDER BY s.created_at DESC, s.id DESC");
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&sql)?;
        let mut rows = statement.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(session_row(row)?);
        }
        Ok(out)
    })
}

/// One session by id, or `None` when no such row exists.
pub fn session_get(db: &Db, id: &str) -> Result<Option<SessionRow>, DbError> {
    let sql = format!("{SESSION_SELECT} WHERE s.id = ?1");
    db.with_conn(|conn| conn.query_row(&sql, params![id], session_row).optional())
}

/// One `jobs` row, as a session page renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRow {
    /// Job id.
    pub id: String,
    /// The kind the worker will run.
    pub kind: String,
    /// One of [`JOB_STATUSES`].
    pub status: String,
    /// How many times it was attempted.
    pub attempts: i64,
    /// What the caller asked for, as submitted.
    pub request_text: Option<String>,
    /// When it was queued.
    pub created_at: String,
    /// When it reached a terminal status.
    pub finished_at: Option<String>,
    /// The stable failure code, when it failed.
    pub error_code: Option<String>,
    /// The message a person reads, when it failed.
    pub error_message: Option<String>,
    /// The session that started it, when it has one.
    pub session_id: Option<String>,
    /// Its events, in `seq` order. Filled by [`job_events`].
    pub events: Vec<JobEventRow>,
}

impl JobRow {
    /// True only for a job a cancel can still change.
    #[must_use]
    pub fn is_cancellable(&self) -> bool {
        matches!(self.status.as_str(), "queued" | "running")
    }
}

/// One `job_events` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobEventRow {
    /// The job-local sequence number.
    pub seq: i64,
    /// The event kind.
    pub kind: String,
    /// The payload, as stored.
    pub payload: Option<String>,
    /// When it was recorded.
    pub occurred_at: String,
}

/// One `jobs` row by id, without its events.
fn job_by_id(db: &Db, id: &str) -> Result<Option<JobRow>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT id, kind, status, attempts, request_text, created_at, finished_at, \
             error_code, error_message, session_id \
             FROM jobs WHERE id = ?1",
            params![id],
            |row| {
                Ok(JobRow {
                    id: row.get(0)?,
                    kind: row.get(1)?,
                    status: row.get(2)?,
                    attempts: row.get(3)?,
                    request_text: row.get(4)?,
                    created_at: row.get(5)?,
                    finished_at: row.get(6)?,
                    error_code: row.get(7)?,
                    error_message: row.get(8)?,
                    session_id: row.get(9)?,
                    events: Vec::new(),
                })
            },
        )
        .optional()
    })
}

/// One job by id, with its events. `None` when the id names no row.
pub fn job_get(db: &Db, id: &str) -> Result<Option<JobRow>, DbError> {
    let Some(mut job) = job_by_id(db, id)? else {
        return Ok(None);
    };
    job.events = job_events(db, id)?;
    Ok(Some(job))
}

/// The jobs that name one session, newest first, each with its events.
pub fn jobs_for_session(db: &Db, session_id: &str) -> Result<Vec<JobRow>, DbError> {
    let mut jobs = db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, kind, status, attempts, request_text, created_at, finished_at, \
             error_code, error_message, session_id \
             FROM jobs WHERE session_id = ?1 ORDER BY created_at DESC, id DESC",
        )?;
        let mut rows = statement.query(params![session_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(JobRow {
                id: row.get(0)?,
                kind: row.get(1)?,
                status: row.get(2)?,
                attempts: row.get(3)?,
                request_text: row.get(4)?,
                created_at: row.get(5)?,
                finished_at: row.get(6)?,
                error_code: row.get(7)?,
                error_message: row.get(8)?,
                session_id: row.get(9)?,
                events: Vec::new(),
            });
        }
        Ok(out)
    })?;
    for job in &mut jobs {
        job.events = job_events(db, &job.id)?;
    }
    Ok(jobs)
}

/// Every event for one job, in `seq` order.
pub fn job_events(db: &Db, job_id: &str) -> Result<Vec<JobEventRow>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT seq, kind, payload, occurred_at FROM job_events \
             WHERE job_id = ?1 ORDER BY seq ASC",
        )?;
        let mut rows = statement.query(params![job_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(JobEventRow {
                seq: row.get(0)?,
                kind: row.get(1)?,
                payload: row.get(2)?,
                occurred_at: row.get(3)?,
            });
        }
        Ok(out)
    })
}

/// Cancel one job, if it is still cancellable.
///
/// The update is conditional on the status, so two cancels racing cannot both
/// win and a job that finished while the page was open is not overwritten. The
/// return value is whether this call was the one that changed the row.
pub fn cancel_job(db: &Db, id: &str, at: &str) -> Result<bool, DbError> {
    let changed = db.with_conn(|conn| {
        conn.execute(
            "UPDATE jobs SET status = 'cancelled', updated_at = ?2 \
             WHERE id = ?1 AND status IN ('queued', 'running')",
            params![id, at],
        )
    })?;
    Ok(changed > 0)
}

/// One `devices` row with the pairing fact the page needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceOverview {
    /// Row id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Platform the device reported.
    pub platform: String,
    /// When the row was registered.
    pub paired_at: Option<String>,
    /// When the device last spoke.
    pub last_seen_at: Option<String>,
    /// When it was revoked.
    pub revoked_at: Option<String>,
    /// True when a live token exists or a code was redeemed. The same two facts
    /// the dashboard's own count uses.
    pub paired: bool,
}

impl DeviceOverview {
    /// The one-word state the table shows.
    #[must_use]
    pub fn state_label(&self) -> &'static str {
        if self.revoked_at.is_some() {
            "revoked"
        } else if self.paired {
            "paired"
        } else {
            "awaiting code"
        }
    }
}

/// Every device, newest first, revoked ones included.
pub fn devices_list(db: &Db) -> Result<Vec<DeviceOverview>, DbError> {
    const SQL: &str = "\
SELECT d.id, d.name, d.platform, d.paired_at, d.last_seen_at, d.revoked_at, \
( \
  EXISTS (SELECT 1 FROM auth_tokens t \
          WHERE t.device_id = d.id AND t.revoked_at IS NULL AND t.rotated_at IS NULL) \
  OR EXISTS (SELECT 1 FROM pairing_codes p \
             WHERE p.device_id = d.id AND p.consumed_at IS NOT NULL) \
) AS paired \
FROM devices d ORDER BY d.created_at DESC, d.id DESC";
    db.with_conn(|conn| {
        let mut statement = conn.prepare(SQL)?;
        let mut rows = statement.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(DeviceOverview {
                id: row.get(0)?,
                name: row.get(1)?,
                platform: row.get(2)?,
                paired_at: row.get(3)?,
                last_seen_at: row.get(4)?,
                revoked_at: row.get(5)?,
                paired: row.get(6)?,
            });
        }
        Ok(out)
    })
}

/// The id of a live device with this name, so a second mint binds a code to the
/// device that is already there instead of registering a second row with the
/// same name.
pub fn device_id_by_name(db: &Db, name: &str) -> Result<Option<String>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT id FROM devices WHERE name = ?1 AND revoked_at IS NULL \
             ORDER BY created_at DESC LIMIT 1",
            params![name],
            |row| row.get::<_, String>(0),
        )
        .optional()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("an in-memory database")
    }

    #[test]
    fn a_setting_round_trips_and_reports_absence() {
        let db = db();
        assert_eq!(settings_get(&db, "workspace.path").expect("read"), None);
        settings_put(&db, "workspace.path", "/tmp/ws").expect("write");
        assert_eq!(
            settings_get(&db, "workspace.path")
                .expect("read")
                .as_deref(),
            Some("/tmp/ws")
        );
        settings_put(&db, "workspace.path", "/tmp/ws2").expect("rewrite");
        assert_eq!(
            settings_get(&db, "workspace.path")
                .expect("read")
                .as_deref(),
            Some("/tmp/ws2"),
            "a second write replaces the row"
        );
        let all = settings_all(&db).expect("all");
        assert_eq!(all.len(), 1);
        assert_eq!(
            all.get("workspace.path").map(String::as_str),
            Some("/tmp/ws2")
        );
    }

    #[test]
    fn the_job_counts_are_zero_filled() {
        let db = db();
        let counts = jobs_by_status(&db).expect("counts");
        assert_eq!(counts.len(), JOB_STATUSES.len());
        assert!(counts.iter().all(|(_, count)| *count == 0), "{counts:?}");

        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO jobs (id, kind, status) VALUES (?1, ?2, ?3)",
                params!["job-1", "turn", "failed"],
            )?;
            Ok(())
        })
        .expect("insert");
        let counts = jobs_by_status(&db).expect("counts");
        let failed = counts
            .iter()
            .find(|(status, _)| *status == "failed")
            .map(|(_, count)| *count);
        assert_eq!(failed, Some(1));
        assert_eq!(counts.len(), JOB_STATUSES.len(), "still every status");
    }

    #[test]
    fn the_audit_read_is_newest_first_and_capped() {
        let db = db();
        for index in 0..5 {
            db.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO audit_log (actor, action, detail) VALUES (?1, ?2, ?3)",
                    params![
                        "loopback",
                        format!("action.{index}"),
                        Option::<String>::None
                    ],
                )?;
                Ok(())
            })
            .expect("insert");
        }
        let rows = audit_recent(&db, 3).expect("rows");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].action, "action.4", "newest first");
        assert_eq!(rows[2].action, "action.2");
        assert_eq!(audit_count(&db).expect("count"), 5);
        assert_eq!(session_count(&db).expect("count"), 0);
        assert_eq!(device_count(&db).expect("count"), 0);
    }
}
