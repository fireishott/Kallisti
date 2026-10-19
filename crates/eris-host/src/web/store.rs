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
