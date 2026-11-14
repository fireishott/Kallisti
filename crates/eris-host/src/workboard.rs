//! The workboard: tasks that outlive the session that named them.
//!
//! A conversation remembers what was said and forgets what was going to be done.
//! The board is the other half: one row, a status, a project, and a due date. It
//! is deliberately not a planner - there is no ordering, no nesting and no
//! assignment - because a board an agent cannot finish quickly is a board it stops
//! writing to.
//!
//! Status is a closed set. "Is this done" is the only question the board exists to
//! answer, and a free-text status makes that unanswerable across two clients.
//!
//! The board is stored in `eris.db` rather than a file so a restart, a backup and
//! the phone all read the same rows: a task list that only exists in a running
//! process is the failure this module is for.

use crate::db::{Db, DbError};
use crate::learning::now;

/// The `settings` row that pre-fills a new row's project.
pub const SETTING_DEFAULT_PROJECT: &str = "workboard.default_project";

/// The statuses a row may hold, in the order the page offers them.
pub const STATUSES: [&str; 4] = ["open", "doing", "blocked", "done"];

/// The owners a row may have.
pub const OWNERS: [&str; 2] = ["operator", "agent"];

/// The longest title the board takes.
pub const MAX_TITLE_CHARS: usize = 200;

/// One row on the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// Row id.
    pub id: String,
    /// What has to happen.
    pub title: String,
    /// Where it is in the flow.
    pub status: String,
    /// What it belongs to, when anything does.
    pub project: Option<String>,
    /// Who put it here.
    pub owner_kind: String,
    /// When it was written.
    pub created_at: String,
    /// When it last moved.
    pub updated_at: String,
    /// When it is due, RFC 3339, when a date was given.
    pub due: Option<String>,
    /// The conversation that named it, when one did.
    pub source_conversation: Option<String>,
    /// How many times it has been moved. A row that keeps moving is a row that
    /// keeps being reopened, and the count is the cheapest way to see that.
    pub checks: i64,
}

impl Item {
    /// The wire shape. One serializer, so the page, the route and the phone agree.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "title": self.title,
            "status": self.status,
            "project": self.project,
            "owner_kind": self.owner_kind,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "due": self.due,
            "source_conversation": self.source_conversation,
            "checks": self.checks,
        })
    }
}

/// What a write names.
#[derive(Debug, Clone, Default)]
pub struct Draft {
    /// The title. Required on create.
    pub title: String,
    /// The starting status; `open` when absent.
    pub status: Option<String>,
    /// The project.
    pub project: Option<String>,
    /// Who owns it; the caller's own kind when absent.
    pub owner_kind: Option<String>,
    /// A due date, as typed.
    pub due: Option<String>,
    /// The conversation that named it.
    pub source_conversation: Option<String>,
}

/// The default project from the settings, when one is stored.
#[must_use]
pub fn default_project(db: &Db) -> Option<String> {
    crate::web::store::settings_get(db, SETTING_DEFAULT_PROJECT)
        .ok()
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Every row, newest activity first, optionally narrowed to one status.
///
/// # Errors
///
/// [`DbError`] when the table cannot be read.
pub fn list(db: &Db, status: Option<&str>, limit: usize) -> Result<Vec<Item>, DbError> {
    const SQL: &str = "
        SELECT id, title, status, project, owner_kind, created_at, updated_at, due,
               source_conversation, checks
          FROM workboard_items
         WHERE (?1 IS NULL OR status = ?1)
         ORDER BY CASE status WHEN 'doing' THEN 0 WHEN 'open' THEN 1
                              WHEN 'blocked' THEN 2 ELSE 3 END,
                  updated_at DESC, id
         LIMIT ?2";
    db.with_conn(|conn| {
        let mut statement = conn.prepare(SQL)?;
        let rows = statement.query_map(rusqlite::params![status, limit as i64], |row| {
            Ok(Item {
                id: row.get(0)?,
                title: row.get(1)?,
                status: row.get(2)?,
                project: row.get(3)?,
                owner_kind: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
                due: row.get(7)?,
                source_conversation: row.get(8)?,
                checks: row.get(9)?,
            })
        })?;
        rows.collect()
    })
}

/// One row by id.
///
/// # Errors
///
/// [`DbError`] when the table cannot be read.
pub fn get(db: &Db, id: &str) -> Result<Option<Item>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, title, status, project, owner_kind, created_at, updated_at, due, \
                    source_conversation, checks
               FROM workboard_items WHERE id = ?1",
        )?;
        let mut rows = statement.query_map(rusqlite::params![id], |row| {
            Ok(Item {
                id: row.get(0)?,
                title: row.get(1)?,
                status: row.get(2)?,
                project: row.get(3)?,
                owner_kind: row.get(4)?,
                created_at: row.get(5)?,
                updated_at: row.get(6)?,
                due: row.get(7)?,
                source_conversation: row.get(8)?,
                checks: row.get(9)?,
            })
        })?;
        rows.next().transpose()
    })
}

/// A tidy title: one line, trimmed, inside the cap.
///
/// # Errors
///
/// A sentence naming the rule when the title is blank or too long.
pub fn tidy_title(raw: &str) -> Result<String, String> {
    let flat = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return Err("a task needs a title".to_owned());
    }
    if flat.chars().count() > MAX_TITLE_CHARS {
        return Err(format!(
            "a title is at most {MAX_TITLE_CHARS} characters; this one is {}",
            flat.chars().count()
        ));
    }
    Ok(flat)
}

/// Add one row.
///
/// # Errors
///
/// A refused title, an unknown status, or a database failure.
pub fn create(db: &Db, draft: &Draft) -> Result<String, String> {
    let title = tidy_title(&draft.title)?;
    let status = normalise_status(draft.status.as_deref())?;
    let owner = normalise_owner(draft.owner_kind.as_deref());
    let project = draft
        .project
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .or_else(|| default_project(db));
    let due = draft
        .due
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let id = uuid::Uuid::now_v7().to_string();
    let stamp = now();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO workboard_items
               (id, title, status, project, owner_kind, created_at, updated_at, due,
                source_conversation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6, ?7, ?8)",
            rusqlite::params![
                id,
                title,
                status,
                project,
                owner,
                stamp,
                due,
                draft.source_conversation
            ],
        )?;
        Ok(())
    })
    .map_err(|error| error.to_string())?;
    Ok(id)
}

/// What a change names. Every field is optional; absent means untouched.
#[derive(Debug, Clone, Default)]
pub struct Change {
    /// A new title.
    pub title: Option<String>,
    /// A new status.
    pub status: Option<String>,
    /// A new project, or an empty string to clear it.
    pub project: Option<String>,
    /// A new due date, or an empty string to clear it.
    pub due: Option<String>,
}

/// Apply one change.
///
/// Returns `false` when the id names no row. Every field is written only when it
/// was named, and the move counter only moves for a status change: bumping it for
/// a title edit would make the count meaningless.
///
/// # Errors
///
/// A refused title, an unknown status, or a database failure.
pub fn patch(db: &Db, id: &str, change: &Change) -> Result<bool, String> {
    let Some(current) = get(db, id).map_err(|error| error.to_string())? else {
        return Ok(false);
    };
    let title = match &change.title {
        Some(raw) => Some(tidy_title(raw)?),
        None => None,
    };
    let status = match &change.status {
        Some(raw) => Some(normalise_status(Some(raw))?),
        None => None,
    };
    let moved = status.as_deref().is_some_and(|next| next != current.status);
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE workboard_items SET
                title      = COALESCE(?2, title),
                status     = COALESCE(?3, status),
                project    = CASE WHEN ?4 IS NULL THEN project
                                  WHEN ?4 = '' THEN NULL ELSE ?4 END,
                due        = CASE WHEN ?5 IS NULL THEN due
                                  WHEN ?5 = '' THEN NULL ELSE ?5 END,
                updated_at = ?6,
                checks     = checks + ?7
              WHERE id = ?1",
            rusqlite::params![
                id,
                title,
                status,
                change.project.as_deref(),
                change.due.as_deref(),
                now(),
                i64::from(moved)
            ],
        )?;
        Ok(())
    })
    .map_err(|error| error.to_string())?;
    Ok(true)
}

/// Remove one row. Returns `false` when the id names no row.
///
/// # Errors
///
/// A database failure.
pub fn remove(db: &Db, id: &str) -> Result<bool, DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "DELETE FROM workboard_items WHERE id = ?1",
            rusqlite::params![id],
        )
        .map(|changed| changed > 0)
    })
}

fn normalise_status(raw: Option<&str>) -> Result<String, String> {
    let value = raw.unwrap_or("open").trim().to_ascii_lowercase();
    if value.is_empty() {
        return Ok("open".to_owned());
    }
    if !STATUSES.contains(&value.as_str()) {
        return Err(format!(
            "`{value}` is not a status; one of: {}",
            STATUSES.join(", ")
        ));
    }
    Ok(value)
}

fn normalise_owner(raw: Option<&str>) -> String {
    let value = raw.unwrap_or("operator").trim().to_ascii_lowercase();
    if OWNERS.contains(&value.as_str()) {
        value
    } else {
        "operator".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("an in-memory database")
    }

    fn draft(title: &str) -> Draft {
        Draft {
            title: title.to_owned(),
            ..Draft::default()
        }
    }

    #[test]
    fn a_row_survives_a_reopen_and_a_move_counts() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("board.db");
        let id = {
            let db = Db::open(&path).expect("db");
            let id = create(&db, &draft("  widen the   timeout ")).expect("create");
            let row = get(&db, &id).expect("get").expect("row");
            assert_eq!(row.title, "widen the timeout", "whitespace is collapsed");
            assert_eq!(row.status, "open");
            assert_eq!(row.checks, 0);
            id
        };
        // A second connection is what a restart looks like to this table.
        let db = Db::open(&path).expect("reopen");
        let kept = get(&db, &id).expect("get").expect("still there");
        assert_eq!(kept.title, "widen the timeout");
        assert!(
            patch(
                &db,
                &id,
                &Change {
                    status: Some("done".to_owned()),
                    ..Change::default()
                }
            )
            .expect("patch")
        );
        let moved = get(&db, &id).expect("get").expect("row");
        assert_eq!(moved.status, "done");
        assert_eq!(moved.checks, 1, "a status move is counted");
        // A title edit is not a move.
        patch(
            &db,
            &id,
            &Change {
                title: Some("widen it".to_owned()),
                ..Change::default()
            },
        )
        .expect("retitle");
        assert_eq!(get(&db, &id).expect("get").expect("row").checks, 1);
    }

    #[test]
    fn the_default_project_applies_and_an_empty_string_clears_one() {
        let db = db();
        crate::web::store::settings_put(&db, SETTING_DEFAULT_PROJECT, "kallisti").expect("store");
        let id = create(&db, &draft("ship it")).expect("create");
        assert_eq!(
            get(&db, &id).expect("get").expect("row").project.as_deref(),
            Some("kallisti")
        );
        patch(
            &db,
            &id,
            &Change {
                project: Some(String::new()),
                ..Change::default()
            },
        )
        .expect("clear");
        assert!(get(&db, &id).expect("get").expect("row").project.is_none());
    }

    #[test]
    fn the_status_set_is_closed_and_the_title_is_bounded() {
        let db = db();
        let error = create(
            &db,
            &Draft {
                title: "x".to_owned(),
                status: Some("sideways".to_owned()),
                ..Draft::default()
            },
        )
        .expect_err("unknown status");
        assert!(error.contains("not a status"), "{error}");
        let long = "x".repeat(MAX_TITLE_CHARS + 1);
        assert!(create(&db, &draft(&long)).is_err());
        assert!(create(&db, &draft("   ")).is_err());
    }

    #[test]
    fn the_list_orders_doing_first_then_open() {
        let db = db();
        create(&db, &draft("later")).expect("open row");
        let busy = create(&db, &draft("now")).expect("doing row");
        patch(
            &db,
            &busy,
            &Change {
                status: Some("doing".to_owned()),
                ..Change::default()
            },
        )
        .expect("move");
        let rows = list(&db, None, 10).expect("list");
        assert_eq!(rows[0].title, "now", "in-flight work leads the board");
        assert_eq!(list(&db, Some("open"), 10).expect("filter").len(), 1);
        assert!(remove(&db, &busy).expect("delete"));
        assert!(!remove(&db, &busy).expect("second delete"));
    }
}
