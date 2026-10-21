//! Push: device push-token registration and deactivation.
//!
//! The legacy relay served `POST /v1/push/register`, `POST /v1/push/deactivate`
//! and `POST /v1/push/send`, backed by the `push_registrations` model. This is
//! the Eris (Rust) equivalent, stored in its own SQLite file (`push.db`) beside
//! `eris.db` (WAL, `busy_timeout`, `synchronous=FULL`, typed errors).
//!
//! APNs delivery is not wired here: there is no APNs key on this host. Register
//! and deactivate persist real tokens with the legacy shapes; the send route is
//! a 503 `apns_not_configured` (the legacy route's own answer when its
//! `apns_client` was `None`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::Deserialize;

use crate::db::BUSY_TIMEOUT;

/// The legacy `push_registrations` schema, names preserved. `apns_token` and the
/// relay fields stay TEXT/`NULL` the way SQLAlchemy's nullable columns did.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS push_registrations (
    id                  TEXT PRIMARY KEY NOT NULL,
    device_id           TEXT NOT NULL,
    transport           TEXT NOT NULL DEFAULT 'direct',
    apns_token          TEXT,
    push_environment    TEXT NOT NULL,
    bundle_id           TEXT,
    relay_handle        TEXT,
    send_grant          TEXT,
    relay_id            TEXT,
    relay_public_key    TEXT,
    token_debug_suffix  TEXT,
    is_active           INTEGER NOT NULL DEFAULT 1,
    last_registered_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_push_registrations_device ON push_registrations (device_id);
";

/// `POST /v1/push/register`. Field names are the legacy `PushRegisterRequest`.
#[derive(Debug, Deserialize)]
pub struct PushRegisterRequest {
    #[serde(rename = "deviceId", default)]
    pub device_id: Option<String>,
    #[serde(default = "default_transport")]
    pub transport: String,
    #[serde(rename = "apnsToken", default)]
    pub apns_token: Option<String>,
    #[serde(rename = "pushEnvironment", default)]
    pub push_environment: String,
    #[serde(rename = "bundleId", default)]
    pub bundle_id: String,
    #[serde(rename = "relayHandle", default)]
    pub relay_handle: Option<String>,
    #[serde(rename = "sendGrant", default)]
    pub send_grant: Option<String>,
    #[serde(rename = "relayId", default)]
    pub relay_id: Option<String>,
    #[serde(rename = "relayPublicKey", default)]
    pub relay_public_key: Option<String>,
    #[serde(rename = "tokenDebugSuffix", default)]
    pub token_debug_suffix: Option<String>,
}

fn default_transport() -> String {
    "direct".to_owned()
}

/// One active registration: `(apns_token, push_environment, bundle_id)`.
pub type ActivePushRegistration = (Option<String>, String, Option<String>);

/// A push-store failure, always turned into a 503 by the device layer.
#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("push database connection is unusable: a previous holder panicked")]
    Locked,
    #[error("cannot prepare state directory {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("refusing to open the push store: journal_mode is {actual:?}, not wal")]
    NotWal { actual: String },
}

/// A handle to the push database. Cheap to clone; the connection is shared.
#[derive(Clone)]
pub struct PushStore {
    inner: Arc<Inner>,
}

struct Inner {
    conn: Mutex<Connection>,
}

impl PushStore {
    /// Open (creating if needed), configure, and initialise the push database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PushError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| PushError::Io {
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
    pub fn open_in_memory() -> Result<Self, PushError> {
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

    /// Upsert one registration per device, matching the legacy
    /// `upsert_push_registration` (select the device's row, insert or update).
    /// Returns the row's `updated_at`.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert(
        &self,
        device_id: &str,
        transport: &str,
        apns_token: Option<&str>,
        push_environment: &str,
        bundle_id: &str,
        relay_handle: Option<&str>,
        send_grant: Option<&str>,
        relay_id: Option<&str>,
        relay_public_key: Option<&str>,
        token_debug_suffix: Option<&str>,
    ) -> Result<String, PushError> {
        let mut conn = self.lock()?;
        let transaction = conn.transaction()?;

        let existing: Option<String> = optional(transaction.query_row(
            "SELECT id FROM push_registrations WHERE device_id = ?1",
            [device_id],
            |row| row.get(0),
        ))?;

        if let Some(id) = existing {
            transaction.execute(
                "UPDATE push_registrations SET
                     transport = ?1, apns_token = ?2, push_environment = ?3, bundle_id = ?4,
                     relay_handle = ?5, send_grant = ?6, relay_id = ?7, relay_public_key = ?8,
                     token_debug_suffix = ?9, is_active = 1,
                     last_registered_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE id = ?10",
                rusqlite::params![
                    transport,
                    apns_token,
                    push_environment,
                    bundle_id,
                    relay_handle,
                    send_grant,
                    relay_id,
                    relay_public_key,
                    token_debug_suffix,
                    id,
                ],
            )?;
            let updated_at = transaction.query_row(
                "SELECT updated_at FROM push_registrations WHERE id = ?1",
                [&id],
                |row| row.get(0),
            )?;
            transaction.commit()?;
            return Ok(updated_at);
        }

        let id = uuid::Uuid::now_v7().to_string();
        transaction.execute(
            "INSERT INTO push_registrations
                 (id, device_id, transport, apns_token, push_environment, bundle_id,
                  relay_handle, send_grant, relay_id, relay_public_key, token_debug_suffix)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            rusqlite::params![
                id,
                device_id,
                transport,
                apns_token,
                push_environment,
                bundle_id,
                relay_handle,
                send_grant,
                relay_id,
                relay_public_key,
                token_debug_suffix,
            ],
        )?;
        let updated_at = transaction.query_row(
            "SELECT updated_at FROM push_registrations WHERE id = ?1",
            [&id],
            |row| row.get(0),
        )?;
        transaction.commit()?;
        Ok(updated_at)
    }

    /// Mark every active registration of a device inactive. Returns the count.
    pub fn deactivate(&self, device_id: &str) -> Result<usize, PushError> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE push_registrations SET is_active = 0,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE device_id = ?1 AND is_active = 1",
            [device_id],
        )?;
        Ok(changed)
    }

    /// The active registrations of a device, as `(apns_token, push_environment,
    /// bundle_id)`. Used by tests and, later, the APNs sender.
    pub fn active_registrations(
        &self,
        device_id: &str,
    ) -> Result<Vec<ActivePushRegistration>, PushError> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT apns_token, push_environment, bundle_id
             FROM push_registrations WHERE device_id = ?1 AND is_active = 1",
        )?;
        let rows = statement.query_map([device_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, PushError> {
        self.inner.conn.lock().map_err(|_| PushError::Locked)
    }
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
fn configure(conn: &Connection) -> Result<(), PushError> {
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(PushError::NotWal { actual: mode });
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

    fn store() -> PushStore {
        PushStore::open_in_memory().expect("in-memory push store opens")
    }

    #[test]
    fn upsert_creates_then_updates_one_row() {
        let store = store();
        store
            .upsert(
                "device-1",
                "direct",
                Some("apns-token-1"),
                "production",
                "com.example.app",
                None,
                None,
                None,
                None,
                None,
            )
            .expect("insert");

        let active = store.active_registrations("device-1").expect("active");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].0.as_deref(), Some("apns-token-1"));

        // A second upsert for the same device updates, not inserts.
        store
            .upsert(
                "device-1",
                "direct",
                Some("apns-token-2"),
                "production",
                "com.example.app",
                None,
                None,
                None,
                None,
                None,
            )
            .expect("update");
        let active = store.active_registrations("device-1").expect("active");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].0.as_deref(), Some("apns-token-2"));
    }

    #[test]
    fn deactivate_marks_inactive_and_counts() {
        let store = store();
        store
            .upsert(
                "device-1",
                "direct",
                Some("tok"),
                "production",
                "com.example.app",
                None,
                None,
                None,
                None,
                None,
            )
            .expect("insert");
        assert_eq!(store.deactivate("device-1").expect("deactivate"), 1);
        assert!(
            store
                .active_registrations("device-1")
                .expect("active")
                .is_empty()
        );
        // Idempotent: nothing left to deactivate.
        assert_eq!(store.deactivate("device-1").expect("deactivate"), 0);
    }
}
