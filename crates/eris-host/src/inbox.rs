//! Inbox: the backend half of the app's notification/action tray.
//!
//! The legacy relay served `GET /v1/inbox`, `POST /v1/inbox/{item_id}/action` and
//! `POST /internal/inbox/create`. This is the Eris (Rust) equivalent, stored in a
//! dedicated SQLite file (`inbox.db`) beside `eris.db`, exactly like the sensor
//! store: WAL, `busy_timeout`, `synchronous=FULL`, typed errors, and the same
//! wire field names the legacy `serialize_inbox_item` produced.
//!
//! Identity note: Eris has no `users` table - the authenticated device is the
//! identity. The legacy `user_id` ownership column maps to `device_id` here, so
//! every paired device sees its own inbox. `serialize_inbox_item` never put
//! `userId` on the wire anyway; it emits `id`, `kind`, `title`, `body`,
//! `priority`, `status`, `payload`, `createdAt`, `primaryActionTitle` and
//! `secondaryActionTitle`, and this module keeps exactly those names.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db::BUSY_TIMEOUT;

/// The legacy `inbox_items` + `inbox_actions` schema, names preserved.
///
/// JSON columns are stored as TEXT so a column can hold `NULL` the same way
/// SQLAlchemy's nullable JSON does, and the reader parses them back into
/// `serde_json::Value`. `device_id` is the Eris stand-in for the legacy
/// `user_id`.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS inbox_items (
    id           TEXT PRIMARY KEY NOT NULL,
    device_id    TEXT NOT NULL,
    kind         TEXT NOT NULL,
    title        TEXT NOT NULL,
    body         TEXT NOT NULL,
    priority     TEXT NOT NULL DEFAULT 'normal',
    status       TEXT NOT NULL DEFAULT 'pending',
    payload      TEXT,
    expires_at   TEXT,
    opened_at    TEXT,
    completed_at TEXT,
    dismissed_at TEXT,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_inbox_items_device_created
    ON inbox_items (device_id, created_at DESC);
CREATE TABLE IF NOT EXISTS inbox_actions (
    id            TEXT PRIMARY KEY NOT NULL,
    inbox_item_id TEXT NOT NULL,
    action_id     TEXT NOT NULL,
    actor_type    TEXT NOT NULL,
    payload       TEXT,
    result        TEXT,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_inbox_actions_item ON inbox_actions (inbox_item_id);
";

/// `POST /v1/inbox/{item_id}/action`. Field name is the legacy `actionId`.
#[derive(Debug, Deserialize)]
pub struct InboxActionRequest {
    #[serde(rename = "actionId")]
    pub action_id: String,
}

/// `POST /internal/inbox/create`. `deviceId` is the target in Eris; `userId` is
/// accepted as a legacy alias and mapped to `device_id` the same way.
#[derive(Debug, Deserialize)]
pub struct InternalInboxCreateRequest {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default = "default_priority")]
    pub priority: String,
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
    #[serde(rename = "expiresAt", default)]
    pub expires_at: Option<String>,
    #[serde(rename = "deviceId", default)]
    pub device_id: Option<String>,
    #[serde(rename = "userId", default)]
    pub user_id: Option<String>,
}

fn default_priority() -> String {
    "normal".to_owned()
}

/// The `serialize_inbox_item` wire shape, one to one.
#[derive(Debug, Serialize, PartialEq)]
pub struct InboxItemView {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub priority: String,
    pub status: String,
    pub payload: Option<serde_json::Value>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "primaryActionTitle")]
    pub primary_action_title: Option<String>,
    #[serde(rename = "secondaryActionTitle")]
    pub secondary_action_title: Option<String>,
}

/// A settled inbox action, for the `POST /v1/inbox/{item_id}/action` reply.
///
/// The legacy route returned `itemID`/`actionID` (capital `ID`), not `itemId`.
#[derive(Debug, Serialize)]
pub struct InboxActionResult {
    #[serde(rename = "itemID")]
    pub item_id: String,
    #[serde(rename = "actionID")]
    pub action_id: String,
    pub status: String,
    #[serde(rename = "completedAt")]
    pub completed_at: String,
}

/// The legacy `default_action_titles(kind)` table.
#[must_use]
pub fn default_action_titles(kind: &str) -> (Option<String>, Option<String>) {
    match kind {
        "approval" => (Some("Approve".to_owned()), Some("Dismiss".to_owned())),
        "suggestion" | "notification" | "alert" | "reminder" => {
            (Some("Open".to_owned()), Some("Dismiss".to_owned()))
        }
        _ => (None, Some("Dismiss".to_owned())),
    }
}

/// An inbox-store failure, always turned into a 503 by the device layer.
#[derive(Debug, thiserror::Error)]
pub enum InboxError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no inbox item with that id belongs to this device")]
    NotFound,
    #[error("inbox database connection is unusable: a previous holder panicked")]
    Locked,
    #[error("cannot prepare state directory {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("refusing to open the inbox store: journal_mode is {actual:?}, not wal")]
    NotWal { actual: String },
}

/// A handle to the inbox database. Cheap to clone; the connection is shared.
#[derive(Clone)]
pub struct InboxStore {
    inner: Arc<Inner>,
}

struct Inner {
    conn: Mutex<Connection>,
}

impl InboxStore {
    /// Open (creating if needed), configure, and initialise the inbox database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, InboxError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| InboxError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let conn = Connection::open(path)?;
        configure(&conn)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            inner: Arc::new(Inner {
                conn: Mutex::new(conn),
            }),
        })
    }

    /// An in-memory database for tests. WAL is unavailable to `:memory:`.
    pub fn open_in_memory() -> Result<Self, InboxError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(&format!(
            "PRAGMA foreign_keys = ON; PRAGMA busy_timeout = {};",
            BUSY_TIMEOUT.as_millis()
        ))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            inner: Arc::new(Inner {
                conn: Mutex::new(conn),
            }),
        })
    }

    /// Create one inbox item for a device (the internal-create path).
    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &self,
        device_id: &str,
        kind: &str,
        title: &str,
        body: &str,
        priority: &str,
        payload: Option<&serde_json::Value>,
        expires_at: Option<&str>,
    ) -> Result<InboxItemView, InboxError> {
        let id = uuid::Uuid::now_v7().to_string();
        let payload_text = match payload {
            Some(value) => Some(serde_json::to_string(value)?),
            None => None,
        };
        {
            let conn = self.lock()?;
            conn.execute(
                "INSERT INTO inbox_items
                     (id, device_id, kind, title, body, priority, payload, expires_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                rusqlite::params![
                    id,
                    device_id,
                    kind,
                    title,
                    body,
                    priority,
                    payload_text,
                    expires_at,
                ],
            )?;
        }
        self.item(device_id, &id)
    }

    /// List the items a device should see: not dismissed, not expired, newest
    /// first. Expiry is compared with `julianday` so any ISO 8601 spelling the
    /// client sent still sorts correctly.
    pub fn list(&self, device_id: &str) -> Result<Vec<InboxItemView>, InboxError> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT id, kind, title, body, priority, status, payload, created_at
             FROM inbox_items
             WHERE device_id = ?1
               AND status != 'dismissed'
               AND (expires_at IS NULL OR julianday(expires_at) > julianday('now'))
             ORDER BY created_at DESC",
        )?;
        let rows = statement.query_map([device_id], row_to_view)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// One item, if it belongs to the device.
    pub fn item(&self, device_id: &str, item_id: &str) -> Result<InboxItemView, InboxError> {
        let conn = self.lock()?;
        let view = optional(conn.query_row(
            "SELECT id, kind, title, body, priority, status, payload, created_at
             FROM inbox_items WHERE id = ?1 AND device_id = ?2",
            rusqlite::params![item_id, device_id],
            row_to_view,
        ))?;
        view.ok_or(InboxError::NotFound)
    }

    /// Apply an action to one item and record it. The status transition and the
    /// action row land in one transaction so the two can never disagree.
    pub fn record_action(
        &self,
        device_id: &str,
        item_id: &str,
        action_id: &str,
        actor_type: &str,
    ) -> Result<InboxActionResult, InboxError> {
        let mut conn = self.lock()?;
        let transaction = conn.transaction()?;

        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM inbox_items WHERE id = ?1 AND device_id = ?2)",
            rusqlite::params![item_id, device_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(InboxError::NotFound);
        }

        let status = match action_id {
            "dismiss" => "dismissed",
            "approve" | "confirm" => "completed",
            _ => "opened",
        };
        transaction.execute(
            "UPDATE inbox_items
             SET status = ?1,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                 opened_at = CASE WHEN ?1 = 'opened' THEN strftime('%Y-%m-%dT%H:%M:%fZ','now') ELSE opened_at END,
                 completed_at = CASE WHEN ?1 = 'completed' THEN strftime('%Y-%m-%dT%H:%M:%fZ','now') ELSE completed_at END,
                 dismissed_at = CASE WHEN ?1 = 'dismissed' THEN strftime('%Y-%m-%dT%H:%M:%fZ','now') ELSE dismissed_at END
             WHERE id = ?2",
            rusqlite::params![status, item_id],
        )?;

        let action_row_id = uuid::Uuid::now_v7().to_string();
        transaction.execute(
            "INSERT INTO inbox_actions (id, inbox_item_id, action_id, actor_type, result)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                action_row_id,
                item_id,
                action_id,
                actor_type,
                serde_json::to_string(&serde_json::json!({ "status": status }))?,
            ],
        )?;
        let created_at: String = transaction.query_row(
            "SELECT created_at FROM inbox_actions WHERE id = ?1",
            [&action_row_id],
            |row| row.get(0),
        )?;
        transaction.commit()?;

        Ok(InboxActionResult {
            item_id: item_id.to_owned(),
            action_id: action_id.to_owned(),
            status: status.to_owned(),
            completed_at: created_at,
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, InboxError> {
        self.inner.conn.lock().map_err(|_| InboxError::Locked)
    }
}

/// Read one row into the wire view, resolving the two action titles.
fn row_to_view(row: &rusqlite::Row<'_>) -> rusqlite::Result<InboxItemView> {
    let kind: String = row.get(1)?;
    let (primary_action_title, secondary_action_title) = default_action_titles(&kind);
    let payload_json: Option<String> = row.get(6)?;
    let payload = payload_json
        .as_deref()
        .and_then(|text| serde_json::from_str(text).ok());
    Ok(InboxItemView {
        id: row.get(0)?,
        kind,
        title: row.get(2)?,
        body: row.get(3)?,
        priority: row.get(4)?,
        status: row.get(5)?,
        payload,
        created_at: row.get(7)?,
        primary_action_title,
        secondary_action_title,
    })
}

/// `Option<T>` for a query that may match no row.
fn optional<T>(result: Result<T, rusqlite::Error>) -> Result<Option<T>, rusqlite::Error> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(other) => Err(other),
    }
}

/// The three pragmas that are not optional, mirroring [`crate::db::configure`].
fn configure(conn: &Connection) -> Result<(), InboxError> {
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(InboxError::NotWal { actual: mode });
    }
    conn.execute_batch(&format!(
        "PRAGMA foreign_keys = ON; PRAGMA busy_timeout = {}; PRAGMA synchronous = FULL;",
        BUSY_TIMEOUT.as_millis()
    ))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> InboxStore {
        InboxStore::open_in_memory().expect("in-memory inbox store opens")
    }

    fn count(store: &InboxStore, sql: &str) -> i64 {
        let conn = store.inner.conn.lock().expect("lock");
        conn.query_row(sql, [], |row| row.get(0)).expect("count")
    }

    #[test]
    fn create_and_list_round_trip() {
        let store = store();
        store
            .create(
                "device-1",
                "approval",
                "Approve?",
                "Someone wants in",
                "normal",
                None,
                None,
            )
            .expect("create");
        let items = store.list("device-1").expect("list");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "approval");
        assert_eq!(items[0].title, "Approve?");
        assert_eq!(items[0].status, "pending");
        assert_eq!(items[0].primary_action_title.as_deref(), Some("Approve"));
        assert_eq!(items[0].secondary_action_title.as_deref(), Some("Dismiss"));
        // Another device sees nothing.
        assert!(store.list("device-2").expect("list").is_empty());
    }

    #[test]
    fn action_sets_status_and_records_a_row() {
        let store = store();
        store
            .create(
                "device-1",
                "notification",
                "Hi",
                "Body",
                "normal",
                None,
                None,
            )
            .expect("create");
        let item = store.list("device-1").expect("list").pop().expect("one");

        let result = store
            .record_action("device-1", &item.id, "dismiss", "user")
            .expect("dismiss");
        assert_eq!(result.status, "dismissed");
        assert_eq!(result.action_id, "dismiss");
        assert_eq!(result.item_id, item.id);
        assert!(!result.completed_at.is_empty());
        assert_eq!(count(&store, "SELECT COUNT(*) FROM inbox_actions"), 1);
        // Dismissed items leave the list.
        assert!(store.list("device-1").expect("list").is_empty());
    }

    #[test]
    fn approve_completes_and_read_opens() {
        let store = store();
        store
            .create("device-1", "approval", "T", "B", "normal", None, None)
            .expect("create");
        let item = store.list("device-1").expect("list").pop().expect("one");
        let result = store
            .record_action("device-1", &item.id, "approve", "user")
            .expect("approve");
        assert_eq!(result.status, "completed");

        store
            .create("device-1", "alert", "T", "B", "normal", None, None)
            .expect("create");
        let second = store.list("device-1").expect("list").pop().expect("one");
        let result = store
            .record_action("device-1", &second.id, "read", "user")
            .expect("read");
        assert_eq!(result.status, "opened");
    }

    #[test]
    fn action_on_unknown_item_is_not_found() {
        let store = store();
        assert!(matches!(
            store.record_action("device-1", "nope", "dismiss", "user"),
            Err(InboxError::NotFound)
        ));
    }

    #[test]
    fn default_action_titles_match_legacy() {
        assert_eq!(
            default_action_titles("approval"),
            (Some("Approve".to_owned()), Some("Dismiss".to_owned()))
        );
        assert_eq!(
            default_action_titles("notification"),
            (Some("Open".to_owned()), Some("Dismiss".to_owned()))
        );
        assert_eq!(
            default_action_titles("something-else"),
            (None, Some("Dismiss".to_owned()))
        );
    }
}
