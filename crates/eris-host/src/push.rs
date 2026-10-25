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

/// Whether a finished reply is announced on the phone it came from.
///
/// On by default: a turn is the only thing this daemon does that a phone wants
/// to hear about, and a host with no APNs key never reaches this setting at all.
/// Stored as a string because every other setting here is.
pub const SETTING_NOTIFY_ON_REPLY: &str = "push.notify_on_reply";

/// The alert body length. A notification is a pointer, not a transcript: long
/// enough to recognise the answer, short enough that the phone does not scroll
/// it in the banner.
const BODY_LIMIT: usize = 160;

/// Whether a value of [`SETTING_NOTIFY_ON_REPLY`] means "yes".
///
/// Absent means yes, because the operator turned the feature on by configuring
/// APNs. Only an explicit negative switches it off, so a setting row that was
/// never written cannot silently disable the thing it exists to enable.
#[must_use]
pub fn notify_on_reply_enabled(raw: Option<&str>) -> bool {
    match raw.map(str::trim) {
        None | Some("") => true,
        Some(value) => !matches!(
            value.to_ascii_lowercase().as_str(),
            "false" | "0" | "no" | "off"
        ),
    }
}

/// The title, body and app payload for a finished reply.
///
/// The keys are the ones `AppEntry.swift` reads: `conversationId` groups the
/// notification, `jobId` lets a tap address the turn, `messageId` the row. The
/// category is `HERALD_MESSAGE_READY`, which is what makes the phone settle a
/// streaming bubble that died and offer Reply from the banner.
#[must_use]
pub fn reply_notification(
    text: &str,
    conversation: Option<&str>,
    job_id: &str,
    message_id: Option<&str>,
) -> (String, String, serde_json::Value) {
    let flat = crate::web::chat::strip_think(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let body = if flat.is_empty() {
        "Your reply is ready.".to_owned()
    } else if flat.chars().count() <= BODY_LIMIT {
        flat
    } else {
        // Cut on a word boundary so the ellipsis does not land mid-word.
        let head: String = flat.chars().take(BODY_LIMIT).collect();
        let cut = head.rfind(' ').unwrap_or(head.len());
        format!("{}...", head[..cut].trim_end())
    };
    let mut info = serde_json::Map::new();
    if let Some(conversation) = conversation.filter(|id| !id.trim().is_empty()) {
        info.insert("conversationId".to_owned(), serde_json::json!(conversation));
    }
    info.insert("jobId".to_owned(), serde_json::json!(job_id));
    if let Some(message) = message_id.filter(|id| !id.trim().is_empty()) {
        info.insert("messageId".to_owned(), serde_json::json!(message));
    }
    ("Kallisti".to_owned(), body, serde_json::Value::Object(info))
}

/// The category the phone expects for a completed reply.
pub const MESSAGE_READY_CATEGORY: &str = "HERALD_MESSAGE_READY";

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

/// One row of [`PushStore::all_active`]: the device, its token, the environment
/// the token belongs to, and the bundle it is for.
pub type PushTarget = (String, Option<String>, String, Option<String>);

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

        // One live registration per token. Ten rows were active for one phone
        // before this existed: every reinstall minted a new device id, the same
        // APNs token was recorded again, and nothing retired the older rows. The
        // phone is one phone, so the old rows are not "other devices", they are
        // the same one wearing an older id.
        if let Some(token) = apns_token.filter(|value| !value.trim().is_empty()) {
            transaction.execute(
                "UPDATE push_registrations SET is_active = 0,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE apns_token = ?1 AND device_id <> ?2 AND is_active = 1",
                rusqlite::params![token, device_id],
            )?;
        }

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

    /// Retire one token: mark the row inactive. Used when APNs says the token
    /// is dead, so the next send does not waste a round trip on it and the
    /// count of live registrations stays honest.
    pub fn deactivate_token(&self, device_id: &str, apns_token: &str) -> Result<usize, PushError> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE push_registrations SET is_active = 0,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE device_id = ?1 AND apns_token = ?2 AND is_active = 1",
            rusqlite::params![device_id, apns_token],
        )?;
        Ok(changed)
    }

    /// Correct the environment recorded for a token.
    ///
    /// Which APNs host a token belongs to is a property of the entitlement it
    /// was minted under, and an app cannot read that reliably: this one guesses
    /// from an App Store receipt and reports `development` for a build whose
    /// entitlements say `production`. The daemon finds out for real the first
    /// time it tries to send, so it writes the answer down here and every later
    /// send goes straight to the right host.
    pub fn set_environment(
        &self,
        device_id: &str,
        apns_token: &str,
        environment: &str,
    ) -> Result<usize, PushError> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE push_registrations SET push_environment = ?1,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE device_id = ?2 AND apns_token = ?3 AND is_active = 1",
            rusqlite::params![environment, device_id, apns_token],
        )?;
        Ok(changed)
    }

    /// Every active registration across every device, as
    /// `(device_id, apns_token, push_environment, bundle_id)`. Used by a
    /// broadcast and by the operator's view of what would actually be reached.
    pub fn all_active(&self) -> Result<Vec<PushTarget>, PushError> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT device_id, apns_token, push_environment, bundle_id
             FROM push_registrations WHERE is_active = 1 ORDER BY device_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
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
    fn deactivate_token_retires_only_that_token() {
        let store = store();
        for token in ["tok-a", "tok-b"] {
            store
                .upsert(
                    "device-1",
                    "direct",
                    Some(token),
                    "production",
                    "net.fihonline.kallisti",
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .expect("insert");
        }
        // The upsert is one row per device, so the second call replaced the
        // first. Both calls are asserted below to keep that rule visible.
        let active = store.active_registrations("device-1").expect("active");
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].0.as_deref(), Some("tok-b"));

        assert_eq!(
            store.deactivate_token("device-1", "tok-b").expect("retire"),
            1
        );
        assert!(
            store
                .active_registrations("device-1")
                .expect("active")
                .is_empty()
        );
        // An unknown token changes nothing rather than erroring.
        assert_eq!(
            store
                .deactivate_token("device-1", "tok-missing")
                .expect("retire"),
            0
        );
    }

    #[test]
    fn registering_one_token_retires_the_same_token_on_an_older_device_id() {
        let store = store();
        // A reinstall: same phone, same APNs token, new device id.
        store
            .upsert(
                "old-device",
                "direct",
                Some("shared-token"),
                "development",
                "net.fihonline.kallisti",
                None,
                None,
                None,
                None,
                None,
            )
            .expect("insert the old row");
        store
            .upsert(
                "new-device",
                "direct",
                Some("shared-token"),
                "development",
                "net.fihonline.kallisti",
                None,
                None,
                None,
                None,
                None,
            )
            .expect("insert the new row");

        assert!(
            store
                .active_registrations("old-device")
                .expect("active")
                .is_empty(),
            "the superseded registration must stop being counted as a live phone"
        );
        assert_eq!(
            store
                .active_registrations("new-device")
                .expect("active")
                .len(),
            1
        );
        assert_eq!(store.all_active().expect("all").len(), 1);
    }

    #[test]
    fn set_environment_corrects_the_recorded_host() {
        let store = store();
        store
            .upsert(
                "device-1",
                "direct",
                Some("tok"),
                "development",
                "net.fihonline.kallisti",
                None,
                None,
                None,
                None,
                None,
            )
            .expect("insert");
        let active = store.active_registrations("device-1").expect("active");
        assert_eq!(active[0].1, "development");

        assert_eq!(
            store
                .set_environment("device-1", "tok", "production")
                .expect("correct"),
            1
        );
        let active = store.active_registrations("device-1").expect("active");
        assert_eq!(
            active[0].1, "production",
            "a send that proved the real host must not have to prove it again"
        );

        // An unknown token changes nothing rather than inventing a row.
        assert_eq!(
            store
                .set_environment("device-1", "other", "production")
                .expect("correct"),
            0
        );
    }

    #[test]
    fn all_active_lists_every_device() {
        let store = store();
        for (device, token) in [("device-1", "tok-1"), ("device-2", "tok-2")] {
            store
                .upsert(
                    device,
                    "direct",
                    Some(token),
                    "development",
                    "net.fihonline.kallisti",
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .expect("insert");
        }
        let all = store.all_active().expect("all");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].0, "device-1");
        assert_eq!(all[1].0, "device-2");
        store.deactivate("device-1").expect("deactivate");
        let all = store.all_active().expect("all");
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "device-2");
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
