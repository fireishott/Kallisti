//! Turns: the durable event log, the worker lease, and the live fan-out.
//!
//! A turn is a row in `jobs` plus a growing series of rows in `job_events`. That
//! is the whole state machine, and it is in SQLite rather than in this process
//! for one reason: the device surface acks a turn with `202` before the provider
//! has said anything, so the record of what happened has to outlive the request
//! that started it.
//!
//! Four rules are enforced here rather than in a handler, because each one is a
//! race a handler cannot win on its own:
//!
//! 1. **The claim is one statement.** `UPDATE ... WHERE status = 'queued'` stamps
//!    `claimed_at`, `lease_expires_at = now + 60s` and `attempts + 1`. The second
//!    worker to try changes zero rows and gives up.
//! 2. **Event appends are guarded.** Every append is
//!    `INSERT ... SELECT ... WHERE EXISTS (job still queued or running)`, so a
//!    terminal frame can never be followed by a delta, even when cancellation
//!    lands between two provider fragments.
//! 3. **A terminal frame is written in the same transaction as the status
//!    change.** Readers therefore never see `succeeded` without its `done`.
//! 4. **`seq` is per job and never reused.** Pruning deletes old rows and keeps
//!    `MAX(seq)` alive, so the counter is `MAX(seq) + 1` and a client can tell
//!    what it missed from the ids alone. A client whose resume point is older
//!    than the oldest retained row is told so, and gets a snapshot URL instead of
//!    a silently partial stream.
//!
//! The provider call never runs on a worker that also writes: the stream sink
//! pushes fragments into an unbounded channel, and one blocking task owns every
//! database write for that job. That is what keeps a slow provider from holding
//! the SQLite mutex, and what makes the deltas ordered without a lock on the
//! request path.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use core_domain::{Capabilities, LlmError, ModelProfile, ProviderProfile};
use core_runtime::prompt::{PromptInputs, compile};
use core_runtime::provider::{
    ChatRequest, OpenAiCompatProvider, ProviderEvent, SecretKey, SecretRef,
};
use rusqlite::{Connection, OptionalExtension, params};
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

use crate::auth::{format_timestamp, now, parse_timestamp};
use crate::db::{Db, DbError};
use crate::onboarding::{SETTING_MODEL, SETTING_PROVIDER_BASE_URL, SETTING_PROVIDER_KEY_REF};
use crate::persona::PersonaConfig;

/// How many events one job keeps by default. Older rows are pruned as new ones
/// land; `seq` keeps climbing.
pub const DEFAULT_SSE_BUFFER: usize = 512;

/// The environment variable that overrides [`DEFAULT_SSE_BUFFER`].
pub const SSE_BUFFER_ENV: &str = "KALLISTI_SSE_BUFFER";

/// Worker lease, in seconds. A claim that is not finished inside this window is
/// expirable by a later sweeper; P8 never re-claims a live job.
pub const LEASE_SECONDS: i64 = 60;

/// How long a device session lives before the device has to start a new one.
pub const SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Which frame this is. The four names are the wire names and the stored kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// One assistant text fragment.
    Delta,
    /// The turn finished, with the assembled text and the usage.
    Done,
    /// The turn failed.
    Error,
    /// The turn was cancelled.
    Cancelled,
}

impl FrameKind {
    /// The stored and wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Delta => "delta",
            Self::Done => "done",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse the stored spelling. Anything else is a corrupt row.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "delta" => Some(Self::Delta),
            "done" => Some(Self::Done),
            "error" => Some(Self::Error),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// Whether this frame ends the stream. The last frame of a job is always one
    /// of these three.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        !matches!(self, Self::Delta)
    }

    /// The job status a terminal frame moves the job to.
    const fn terminal_status(self) -> Option<&'static str> {
        match self {
            Self::Delta => None,
            Self::Done => Some("succeeded"),
            Self::Error => Some("failed"),
            Self::Cancelled => Some("cancelled"),
        }
    }
}

/// One event, as it is stored and as it goes on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnFrame {
    /// 1-based, monotonic per job.
    pub seq: i64,
    /// Which frame this is.
    pub kind: FrameKind,
    /// The `data:` payload.
    pub data: serde_json::Value,
}

impl TurnFrame {
    /// `true` when this frame ends its job.
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        self.kind.is_terminal()
    }
}

/// What a worker fragment turns into, before it is written.
#[derive(Debug)]
enum Signal {
    /// One provider text fragment.
    Delta(String),
    /// The provider finished.
    Done {
        /// The assembled reply.
        text: String,
        /// The usage the provider reported, as JSON.
        usage: serde_json::Value,
    },
    /// The provider failed.
    Failed {
        /// Stable code, from [`LlmError::kind`].
        code: String,
        /// Human-readable detail.
        message: String,
    },
}

/// One job, as the worker and the routes need it.
#[derive(Debug, Clone)]
pub struct JobRow {
    /// Row id.
    pub id: String,
    /// Owning device, when it has one.
    pub device_id: Option<String>,
    /// Owning session, when it has one.
    pub session_id: Option<String>,
    /// The job status.
    pub status: String,
    /// The prompt this job was created for.
    pub request_text: Option<String>,
}

/// One session row.
#[derive(Debug, Clone)]
pub struct SessionRow {
    /// Row id.
    pub id: String,
    /// Owning device.
    pub device_id: Option<String>,
    /// RFC 3339 expiry.
    pub expires_at: String,
    /// Set when the session was revoked.
    pub revoked_at: Option<String>,
}

/// One turn, as `GET /v1/sessions/{id}` reports it.
#[derive(Debug, Clone)]
pub struct TurnRow {
    /// Job id.
    pub job_id: String,
    /// Job status.
    pub status: String,
    /// When the job was created.
    pub created_at: String,
    /// When it finished, if it did.
    pub finished_at: Option<String>,
    /// The assembled reply, or the partial text of a turn that did not finish.
    pub text: String,
}

/// The provider settings a turn needs, read from `settings` at turn time.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    /// OpenAI-compatible base URL.
    pub base_url: String,
    /// `env:NAME`, never a value.
    pub key_ref: SecretRef,
    /// The default model id.
    pub model: String,
}

/// The adapter plus the key: everything a stream needs.
pub struct PreparedProvider {
    /// The streaming adapter.
    pub provider: OpenAiCompatProvider,
    /// The resolved key. Never printed.
    pub key: SecretKey,
}

/// The environment lookup a hub resolves `env:NAME` against. Injected so a test
/// can prove the turn path without mutating the process environment.
pub type KeyLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// The turn store: one database handle, one retention number.
#[derive(Clone)]
pub struct TurnStore {
    db: Db,
    buffer: usize,
}

impl TurnStore {
    /// Wrap the state database. `buffer` is the per-job retention.
    #[must_use]
    pub fn new(db: Db, buffer: usize) -> Self {
        Self {
            db,
            buffer: buffer.max(1),
        }
    }

    /// The configured retention.
    #[must_use]
    pub fn buffer(&self) -> usize {
        self.buffer
    }

    /// The database underneath.
    #[must_use]
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Retention from the environment, with the documented default.
    #[must_use]
    pub fn buffer_from_env() -> usize {
        std::env::var(SSE_BUFFER_ENV)
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_SSE_BUFFER)
            .max(1)
    }

    /// Create one queued job.
    pub fn create_job(
        &self,
        device_id: &str,
        session_id: &str,
        idempotency_key: Option<&str>,
        request_text: &str,
    ) -> Result<String, DbError> {
        let id = Uuid::now_v7().to_string();
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO jobs (id, kind, status, device_id, session_id, idempotency_key, request_text) \
                 VALUES (?1, 'chat', 'queued', ?2, ?3, ?4, ?5)",
                params![id, device_id, session_id, idempotency_key, request_text],
            )?;
            Ok(())
        })?;
        Ok(id)
    }

    /// The job a client already created for this `(device, key)` pair.
    pub fn find_idempotent(
        &self,
        device_id: &str,
        idempotency_key: &str,
    ) -> Result<Option<String>, DbError> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT id FROM jobs WHERE device_id = ?1 AND idempotency_key = ?2",
                params![device_id, idempotency_key],
                |row| row.get(0),
            )
            .optional()
        })
    }

    /// Claim a queued job and stamp the lease. `false` means somebody else has
    /// it, or it is no longer queued.
    pub fn claim(&self, job_id: &str) -> Result<bool, DbError> {
        let lease = format!("+{LEASE_SECONDS} seconds");
        let changed = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE jobs \
                    SET status = 'running', \
                        attempts = attempts + 1, \
                        claimed_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                        lease_expires_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?2), \
                        updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
                  WHERE id = ?1 AND status = 'queued'",
                params![job_id, lease],
            )
        })?;
        Ok(changed == 1)
    }

    /// One job, when it exists.
    pub fn job(&self, job_id: &str) -> Result<Option<JobRow>, DbError> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT id, device_id, session_id, status, request_text FROM jobs WHERE id = ?1",
                params![job_id],
                |row| {
                    Ok(JobRow {
                        id: row.get(0)?,
                        device_id: row.get(1)?,
                        session_id: row.get(2)?,
                        status: row.get(3)?,
                        request_text: row.get(4)?,
                    })
                },
            )
            .optional()
        })
    }

    /// One job's status.
    pub fn status(&self, job_id: &str) -> Result<Option<String>, DbError> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT status FROM jobs WHERE id = ?1",
                params![job_id],
                |row| row.get(0),
            )
            .optional()
        })
    }

    /// The lowest retained `seq` for a job. `None` means nothing was recorded
    /// yet, which is not the same as "everything was pruned".
    pub fn min_seq(&self, job_id: &str) -> Result<Option<i64>, DbError> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT MIN(seq) FROM job_events WHERE job_id = ?1",
                params![job_id],
                |row| row.get(0),
            )
            .optional()
            .map(Option::flatten)
        })
    }

    /// Every retained event with `seq > after`, ascending.
    pub fn events_after(&self, job_id: &str, after: i64) -> Result<Vec<TurnFrame>, DbError> {
        self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT seq, kind, payload FROM job_events \
                  WHERE job_id = ?1 AND seq > ?2 ORDER BY seq",
            )?;
            let mut rows = statement.query(params![job_id, after])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                let seq: i64 = row.get(0)?;
                let kind: String = row.get(1)?;
                let payload: Option<String> = row.get(2)?;
                out.push(TurnFrame {
                    seq,
                    kind: FrameKind::parse(&kind).unwrap_or(FrameKind::Error),
                    data: payload
                        .map(|text| serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
                        .unwrap_or(serde_json::Value::Null),
                });
            }
            Ok(out)
        })
    }

    /// Append one non-terminal event. `None` means the job was already terminal
    /// and nothing was written, which is the normal way a late delta is dropped.
    pub fn append(
        &self,
        job_id: &str,
        kind: FrameKind,
        data: &serde_json::Value,
    ) -> Result<Option<i64>, DbError> {
        if kind.is_terminal() {
            // A terminal frame goes through `finish`, which moves the status in
            // the same transaction. Reaching this branch is a bug in the caller.
            return Ok(None);
        }
        let payload = data.to_string();
        let buffer = i64::try_from(self.buffer).unwrap_or(i64::MAX);
        self.db.with_conn(|conn| {
            let seq: Option<i64> = conn
                .query_row(APPEND_SQL, params![job_id, kind.as_str(), payload], |row| {
                    row.get(0)
                })
                .optional()?;
            if let Some(seq) = seq {
                prune(conn, job_id, seq, buffer)?;
            }
            Ok(seq)
        })
    }

    /// Write the terminal frame and move the status, in one transaction.
    pub fn finish(
        &self,
        job_id: &str,
        kind: FrameKind,
        data: &serde_json::Value,
    ) -> Result<Option<i64>, DbError> {
        let Some(status) = kind.terminal_status() else {
            return Ok(None);
        };
        let payload = data.to_string();
        let buffer = i64::try_from(self.buffer).unwrap_or(i64::MAX);
        self.db
            .with_conn(|conn| finish_transaction(conn, job_id, kind, &payload, status, buffer))
    }

    /// The device's live session, or a new one. A device gets one session at a
    /// time so `GET /v1/sessions/{id}` has more than one turn to report.
    pub fn ensure_session(&self, device_id: &str) -> Result<String, DbError> {
        let existing: Option<(String, String)> = self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT id, expires_at FROM sessions \
                  WHERE device_id = ?1 AND revoked_at IS NULL \
                  ORDER BY created_at DESC, id DESC LIMIT 1",
                params![device_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
        })?;
        if let Some((id, expires_at)) = existing {
            let live = parse_timestamp(&expires_at)
                .map(|at| now() < at)
                .unwrap_or(false);
            if live {
                return Ok(id);
            }
        }
        let id = Uuid::now_v7().to_string();
        let expires_at =
            format_timestamp(now() + time::Duration::seconds(SESSION_TTL.as_secs() as i64));
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO sessions (id, device_id, expires_at, last_seen_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, device_id, expires_at, format_timestamp(now())],
            )?;
            Ok(())
        })?;
        Ok(id)
    }

    /// One session, when it exists.
    pub fn session(&self, session_id: &str) -> Result<Option<SessionRow>, DbError> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT id, device_id, expires_at, revoked_at FROM sessions WHERE id = ?1",
                params![session_id],
                |row| {
                    Ok(SessionRow {
                        id: row.get(0)?,
                        device_id: row.get(1)?,
                        expires_at: row.get(2)?,
                        revoked_at: row.get(3)?,
                    })
                },
            )
            .optional()
        })
    }

    /// Every turn of a session, oldest first, with the text to show for it.
    pub fn session_turns(&self, session_id: &str) -> Result<Vec<TurnRow>, DbError> {
        let rows = self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, status, created_at, finished_at FROM jobs \
                  WHERE session_id = ?1 ORDER BY created_at, id",
            )?;
            let mut rows = statement.query(params![session_id])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ));
            }
            Ok(out)
        })?;
        let mut turns = Vec::with_capacity(rows.len());
        for (job_id, status, created_at, finished_at) in rows {
            let text = self.reply_text(&job_id)?;
            turns.push(TurnRow {
                job_id,
                status,
                created_at,
                finished_at,
                text,
            });
        }
        Ok(turns)
    }

    /// The reply to show for a job: the `done` payload when there is one, else
    /// the retained deltas joined.
    pub fn reply_text(&self, job_id: &str) -> Result<String, DbError> {
        let done: Option<String> = self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT payload FROM job_events \
                  WHERE job_id = ?1 AND kind = 'done' ORDER BY seq DESC LIMIT 1",
                params![job_id],
                |row| row.get(0),
            )
            .optional()
        })?;
        if let Some(payload) = done {
            let parsed: serde_json::Value =
                serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null);
            if let Some(text) = parsed.get("text").and_then(serde_json::Value::as_str) {
                return Ok(text.to_owned());
            }
        }
        let mut text = String::new();
        self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT payload FROM job_events \
                  WHERE job_id = ?1 AND kind = 'delta' ORDER BY seq",
            )?;
            let mut rows = statement.query(params![job_id])?;
            while let Some(row) = rows.next()? {
                let payload: Option<String> = row.get(0)?;
                let parsed: serde_json::Value = payload
                    .map(|text| serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
                    .unwrap_or(serde_json::Value::Null);
                if let Some(fragment) = parsed.get("text").and_then(serde_json::Value::as_str) {
                    text.push_str(fragment);
                }
            }
            Ok(())
        })?;
        Ok(text)
    }

    /// The provider settings a turn needs. `None` means onboarding never stored
    /// them, which is a configuration failure rather than a provider one.
    pub fn provider_config(&self) -> Result<Option<ProviderConfig>, DbError> {
        let read = |conn: &mut Connection, key: &str| -> rusqlite::Result<Option<String>> {
            conn.query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
        };
        let (base_url, key_ref, model) = self.db.with_conn(|conn| {
            Ok((
                read(conn, SETTING_PROVIDER_BASE_URL)?,
                read(conn, SETTING_PROVIDER_KEY_REF)?,
                read(conn, SETTING_MODEL)?,
            ))
        })?;
        let (Some(base_url), Some(key_ref), Some(model)) = (base_url, key_ref, model) else {
            return Ok(None);
        };
        let Ok(key_ref) = SecretRef::try_from(key_ref.as_str()) else {
            return Ok(None);
        };
        Ok(Some(ProviderConfig {
            base_url,
            key_ref,
            model,
        }))
    }

    /// The operator's persona switch, as the dashboard last saved it.
    ///
    /// `None` means no row: the `[persona] enabled` key in `config.toml` decides.
    /// Read per turn rather than cached, so the dashboard toggle takes effect on
    /// the next turn without a restart.
    pub fn persona_enabled(&self) -> Result<Option<String>, DbError> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![crate::persona::SETTING_PERSONA_ENABLED],
                |row| row.get(0),
            )
            .optional()
        })
    }
}

/// The guarded append. One statement, so it is atomic against a concurrent
/// terminal write from the same connection.
const APPEND_SQL: &str = "
INSERT INTO job_events (job_id, seq, kind, payload)
SELECT ?1, COALESCE((SELECT MAX(seq) FROM job_events WHERE job_id = ?1), 0) + 1, ?2, ?3
 WHERE EXISTS (SELECT 1 FROM jobs WHERE id = ?1 AND status IN ('queued', 'running'))
RETURNING seq";

/// Keep at most `buffer` rows per job, newest kept. `newest` is the seq that was
/// just written, so `MAX(seq)` can never be pruned away and the counter survives.
fn prune(conn: &Connection, job_id: &str, newest: i64, buffer: i64) -> rusqlite::Result<()> {
    let cutoff = newest.saturating_sub(buffer);
    if cutoff < 1 {
        return Ok(());
    }
    conn.execute(
        "DELETE FROM job_events WHERE job_id = ?1 AND seq <= ?2",
        params![job_id, cutoff],
    )?;
    Ok(())
}

/// Append the terminal frame and move the status together.
fn finish_transaction(
    conn: &mut Connection,
    job_id: &str,
    kind: FrameKind,
    payload: &str,
    status: &str,
    buffer: i64,
) -> rusqlite::Result<Option<i64>> {
    let transaction = conn.transaction()?;
    let seq: Option<i64> = transaction
        .query_row(APPEND_SQL, params![job_id, kind.as_str(), payload], |row| {
            row.get(0)
        })
        .optional()?;
    let Some(seq) = seq else {
        // Already terminal: nothing was written and nothing may be.
        return Ok(None);
    };
    prune(&transaction, job_id, seq, buffer)?;
    let changed = transaction.execute(
        "UPDATE jobs SET status = ?2, finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), \
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') \
          WHERE id = ?1 AND status IN ('queued', 'running')",
        params![job_id, status],
    )?;
    if changed == 0 {
        return Ok(None);
    }
    transaction.commit()?;
    Ok(Some(seq))
}

/// The live fan-out and the worker.
///
/// One `broadcast` sender per running job, created before the `202` is answered,
/// so a subscriber that connects immediately always sees the whole stream. The
/// sender is dropped once the job reaches a terminal frame, which is what ends
/// every subscriber's stream without a "is it done yet" loop.
pub struct TurnHub {
    store: TurnStore,
    channels: Mutex<HashMap<String, broadcast::Sender<TurnFrame>>>,
    key_lookup: KeyLookup,
    persona: Mutex<Option<PersonaConfig>>,
}

impl TurnHub {
    /// The real key resolution: the daemon's own environment.
    #[must_use]
    pub fn new(db: Db, buffer: usize) -> Self {
        Self::with_key_lookup(db, buffer, Arc::new(|name| std::env::var(name).ok()))
    }

    /// A hub with an injected environment lookup, so tests can prove a turn path
    /// without mutating the process environment (which edition 2024 makes
    /// `unsafe`, and this workspace forbids unsafe).
    #[must_use]
    pub fn with_key_lookup(db: Db, buffer: usize, key_lookup: KeyLookup) -> Self {
        Self {
            store: TurnStore::new(db, buffer),
            channels: Mutex::new(HashMap::new()),
            key_lookup,
            persona: Mutex::new(None),
        }
    }

    /// Give this hub an identity. Without one there is no system message, which
    /// is the right default for a hub a test built for another purpose.
    #[must_use]
    pub fn with_persona(self, persona: PersonaConfig) -> Self {
        self.set_persona(persona);
        self
    }

    /// Replace the identity. Takes effect on the next turn's file read.
    pub fn set_persona(&self, persona: PersonaConfig) {
        if let Ok(mut slot) = self.persona.lock() {
            *slot = Some(persona);
        }
    }

    /// The identity this hub would use, when it has one.
    #[must_use]
    pub fn persona(&self) -> Option<PersonaConfig> {
        self.persona.lock().ok().and_then(|slot| slot.clone())
    }

    /// The store underneath.
    #[must_use]
    pub fn store(&self) -> &TurnStore {
        &self.store
    }

    /// Register a job's channel. Called once per turn, before the `202`.
    pub fn begin(&self, job_id: &str) -> broadcast::Sender<TurnFrame> {
        let (sender, _) = broadcast::channel(256);
        if let Ok(mut channels) = self.channels.lock() {
            channels.insert(job_id.to_owned(), sender.clone());
        }
        sender
    }

    /// Subscribe to a job's live frames. `None` means no worker is publishing for
    /// it (it finished, or it is a job from a previous process), so the caller
    /// answers from the log alone.
    #[must_use]
    pub fn subscribe(&self, job_id: &str) -> Option<broadcast::Receiver<TurnFrame>> {
        let channels = self.channels.lock().ok()?;
        channels.get(job_id).map(broadcast::Sender::subscribe)
    }

    /// Cancel a job and tell whoever is listening right now. `Some(seq)` when
    /// this call is the one that cancelled it, `None` when the job was already
    /// terminal (which is what makes a second cancel a `409`).
    pub fn cancel(&self, job_id: &str) -> Result<Option<i64>, DbError> {
        let data = serde_json::json!({ "reason": "cancel" });
        let seq = self.store.finish(job_id, FrameKind::Cancelled, &data)?;
        if let Some(seq) = seq
            && let Ok(channels) = self.channels.lock()
            && let Some(sender) = channels.get(job_id).cloned()
        {
            publish(&sender, seq, FrameKind::Cancelled, data);
        }
        Ok(seq)
    }

    /// Drop a job's channel once its worker is done.
    fn retire(&self, job_id: &str) {
        if let Ok(mut channels) = self.channels.lock() {
            channels.remove(job_id);
        }
    }

    /// Claim the job, run the provider, and write every event.
    ///
    /// The provider stream is awaited on the runtime; every database write
    /// happens on a blocking task fed by a channel, so a slow provider never
    /// holds the connection and a slow disk never holds the provider.
    pub fn spawn(self: &Arc<Self>, job_id: String, sender: broadcast::Sender<TurnFrame>) {
        let hub = Arc::clone(self);
        tokio::spawn(async move {
            hub.run(job_id, sender).await;
        });
    }

    /// The worker body. Split out so the spawned future stays small.
    async fn run(self: Arc<Self>, job_id: String, sender: broadcast::Sender<TurnFrame>) {
        let store = self.store.clone();
        let claim = {
            let store = store.clone();
            let job_id = job_id.clone();
            tokio::task::spawn_blocking(move || store.claim(&job_id)).await
        };
        let claimed = matches!(claim, Ok(Ok(true)));
        if !claimed {
            // Somebody else has it, or it was cancelled before it started.
            self.retire(&job_id);
            return;
        }

        let job = {
            let store = store.clone();
            let job_id = job_id.clone();
            tokio::task::spawn_blocking(move || store.job(&job_id)).await
        };
        let Ok(Ok(Some(job))) = job else {
            let _ = self
                .fail_early(
                    &job_id,
                    &sender,
                    "job_unreadable",
                    "the job row disappeared",
                )
                .await;
            self.retire(&job_id);
            return;
        };

        let config = {
            let store = store.clone();
            tokio::task::spawn_blocking(move || store.provider_config()).await
        };
        let config = match config {
            Ok(Ok(Some(config))) => config,
            _ => {
                let _ = self
                    .fail_early(
                        &job_id,
                        &sender,
                        "provider_not_configured",
                        "no provider is stored: finish onboarding and pick an endpoint and model",
                    )
                    .await;
                self.retire(&job_id);
                return;
            }
        };

        let prepared = match prepare_provider(&config, self.key_lookup.as_ref()) {
            Ok(prepared) => prepared,
            Err(error) => {
                let _ = self
                    .fail_early(&job_id, &sender, error.kind(), &error.to_string())
                    .await;
                self.retire(&job_id);
                return;
            }
        };

        // The identity: read from the file now, on this turn, so an edit is live
        // without a restart. `None` is the ordinary "no persona" case and the
        // turn goes ahead with no system message at all.
        let stored_enabled = {
            let store = store.clone();
            tokio::task::spawn_blocking(move || store.persona_enabled()).await
        };
        let stored_enabled = match stored_enabled {
            Ok(Ok(value)) => value,
            _ => {
                let _ = self
                    .fail_early(
                        &job_id,
                        &sender,
                        "persona_setting_unreadable",
                        "the persona switch could not be read from the settings table",
                    )
                    .await;
                self.retire(&job_id);
                return;
            }
        };
        let persona_text = match self.persona() {
            None => None,
            Some(persona) => {
                match tokio::task::spawn_blocking(move || persona.load(stored_enabled.as_deref()))
                    .await
                {
                    Ok(Ok(text)) => text,
                    Ok(Err(error)) => {
                        // A persona file that exists and cannot be used is a
                        // configuration mistake, not an absent persona: refuse
                        // the turn with the numbers instead of sending a prompt
                        // nobody wrote.
                        let _ = self
                            .fail_early(&job_id, &sender, error.code(), &error.to_string())
                            .await;
                        self.retire(&job_id);
                        return;
                    }
                    Err(join) => {
                        let _ = self
                            .fail_early(
                                &job_id,
                                &sender,
                                "persona_task_failed",
                                &format!("the persona read did not finish: {join}"),
                            )
                            .await;
                        self.retire(&job_id);
                        return;
                    }
                }
            }
        };

        let inputs = PromptInputs {
            persona: persona_text,
            ..PromptInputs::default()
        };
        let compiled = match compile(&inputs) {
            Ok(compiled) => compiled,
            Err(error) => {
                let _ = self
                    .fail_early(&job_id, &sender, "persona_too_large", &error.to_string())
                    .await;
                self.retire(&job_id);
                return;
            }
        };
        // A length, never the text: the persona is the operator's own words and
        // does not belong in a log line.
        tracing::debug!(
            system_bytes = compiled.bytes(),
            has_persona = !compiled.is_empty(),
            "the turn system prompt is compiled"
        );
        let system = (!compiled.is_empty()).then(|| compiled.as_str().to_owned());

        // The writer: one blocking task owns every row this job writes.
        let (tx, mut rx) = mpsc::unbounded_channel::<Signal>();
        let writer = {
            let store = store.clone();
            let sender = sender.clone();
            let writer_job = job_id.clone();
            tokio::task::spawn_blocking(move || {
                while let Some(signal) = rx.blocking_recv() {
                    match signal {
                        Signal::Delta(text) => {
                            let data = serde_json::json!({ "text": text });
                            if let Ok(Some(seq)) =
                                store.append(&writer_job, FrameKind::Delta, &data)
                            {
                                publish(&sender, seq, FrameKind::Delta, data);
                            }
                        }
                        Signal::Done { text, usage } => {
                            let data = serde_json::json!({ "text": text, "usage": usage });
                            if let Ok(Some(seq)) = store.finish(&writer_job, FrameKind::Done, &data)
                            {
                                publish(&sender, seq, FrameKind::Done, data);
                            }
                        }
                        Signal::Failed { code, message } => {
                            let data = serde_json::json!({ "code": code, "message": message });
                            if let Ok(Some(seq)) =
                                store.finish(&writer_job, FrameKind::Error, &data)
                            {
                                publish(&sender, seq, FrameKind::Error, data);
                            }
                        }
                    }
                }
            })
        };

        // The prompt: what the client asked, from the row rather than from the
        // request, so a claim after a restart carries the same text.
        let prompt = job.request_text.unwrap_or_default();

        // The provider runs on its own blocking thread with its own single-thread
        // runtime, and feeds fragments back over the channel. That is deliberate:
        // `stream_chat` takes `&mut dyn FnMut`, which is not `Send`, so a sink
        // cannot be held across an await on a multi-threaded worker. Running the
        // call to completion on one dedicated thread keeps the request path free
        // and the sink local, and the writer task above still owns every row.
        let provider_task = tokio::task::spawn_blocking(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    let _ = tx.send(Signal::Failed {
                        code: "runtime_unavailable".to_owned(),
                        message: format!("cannot start the turn runtime: {error}"),
                    });
                    return;
                }
            };
            let request = ChatRequest::with_system(system, prompt);
            let mut sink = |event: ProviderEvent| {
                if let ProviderEvent::Text(fragment) = event
                    && !fragment.is_empty()
                {
                    let _ = tx.send(Signal::Delta(fragment));
                }
            };
            let outcome = runtime.block_on(prepared.provider.stream_chat(
                &request,
                &prepared.key,
                &mut sink,
            ));
            match outcome {
                Ok(completion) => {
                    let usage =
                        serde_json::to_value(completion.usage).unwrap_or(serde_json::Value::Null);
                    let _ = tx.send(Signal::Done {
                        text: completion.text,
                        usage,
                    });
                }
                Err(error) => {
                    let _ = tx.send(Signal::Failed {
                        code: error.kind().to_owned(),
                        message: error.to_string(),
                    });
                }
            }
        });

        let _ = provider_task.await;
        let _ = writer.await;
        self.retire(&job_id);
    }

    /// Write a terminal frame for a job that never reached the provider, and
    /// publish it, so a subscriber that connected between the `202` and the
    /// failure is not left with a stream that closes without saying why.
    async fn fail_early(
        &self,
        job_id: &str,
        sender: &broadcast::Sender<TurnFrame>,
        code: &str,
        message: &str,
    ) -> Option<i64> {
        let store = self.store.clone();
        let owner = job_id.to_owned();
        let data = serde_json::json!({ "code": code, "message": message });
        let written_data = data.clone();
        let written = tokio::task::spawn_blocking(move || {
            store.finish(&owner, FrameKind::Error, &written_data)
        })
        .await;
        match written {
            Ok(Ok(Some(seq))) => {
                publish(sender, seq, FrameKind::Error, data);
                Some(seq)
            }
            _ => None,
        }
    }
}

/// Publish one frame to whoever is listening. A send with no receivers is not an
/// error: a client that has not connected yet reads the log instead.
fn publish(
    sender: &broadcast::Sender<TurnFrame>,
    seq: i64,
    kind: FrameKind,
    data: serde_json::Value,
) {
    let _ = sender.send(TurnFrame { seq, kind, data });
}

/// Build the adapter and resolve the key: the same shape `eris probe` uses, from
/// the settings the wizard wrote rather than from `eris.env`.
fn prepare_provider(
    config: &ProviderConfig,
    lookup: &(dyn Fn(&str) -> Option<String> + Send + Sync),
) -> Result<PreparedProvider, LlmError> {
    let profile = ProviderProfile::new(
        "device-turn",
        config.base_url.clone(),
        config.key_ref.clone(),
    )?;
    let provider = OpenAiCompatProvider::new(
        profile,
        ModelProfile::new(config.model.clone()),
        Capabilities::openai_compatible(),
    )?;
    let key = core_runtime::provider::resolve_api_key_with(&config.key_ref, |name| lookup(name))?;
    Ok(PreparedProvider { provider, key })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(buffer: usize) -> (TurnStore, String) {
        let db = Db::open_in_memory().expect("in-memory database");
        // `sessions.device_id` is a real foreign key, so the device has to exist
        // before a session can point at it.
        let device = crate::auth::AuthStore::new(db.clone())
            .register_device("phone", "ios")
            .expect("device row");
        (TurnStore::new(db, buffer), device)
    }

    fn queued(store: &TurnStore, device: &str) -> (String, String) {
        let session = store.ensure_session(device).expect("session");
        let job = store
            .create_job(device, &session, None, "reply with the single word: ok")
            .expect("job");
        (job, session)
    }

    #[test]
    fn the_claim_is_atomic_and_stamps_a_lease() {
        let (store, device) = store(DEFAULT_SSE_BUFFER);
        let (job, _) = queued(&store, &device);

        assert!(store.claim(&job).expect("first claim wins"));
        assert!(
            !store.claim(&job).expect("second claim must lose"),
            "a claimed job is not queued any more"
        );

        let (claimed_at, lease_expires_at, attempts): (String, String, i64) = store
            .db()
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT claimed_at, lease_expires_at, attempts FROM jobs WHERE id = ?1",
                    params![job],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
            })
            .expect("lease columns");

        assert_eq!(attempts, 1, "one claim is one attempt");
        let claimed = parse_timestamp(&claimed_at).expect("claimed_at parses");
        let lease = parse_timestamp(&lease_expires_at).expect("lease_expires_at parses");
        let window = lease - claimed;
        assert!(
            window.whole_seconds() >= LEASE_SECONDS - 1
                && window.whole_seconds() <= LEASE_SECONDS + 1,
            "the lease is {LEASE_SECONDS}s, got {window:?}"
        );
    }

    #[test]
    fn an_idempotency_key_cannot_produce_two_jobs() {
        let (store, device) = store(DEFAULT_SSE_BUFFER);
        let session = store.ensure_session(&device).expect("session");
        let first = store
            .create_job(&device, &session, Some("turn-1"), "ok")
            .expect("first job");
        let refused = store
            .create_job(&device, &session, Some("turn-1"), "ok")
            .expect_err("the partial unique index must refuse a duplicate");
        assert!(matches!(refused, DbError::Sqlite(_)), "{refused:?}");
        assert_eq!(
            store.find_idempotent(&device, "turn-1").expect("lookup"),
            Some(first),
            "the original job is the one the key names"
        );
        // A second device may use the same key: the index is per device.
        let other = crate::auth::AuthStore::new(store.db().clone())
            .register_device("tablet", "ios")
            .expect("second device");
        let other_session = store.ensure_session(&other).expect("session");
        store
            .create_job(&other, &other_session, Some("turn-1"), "ok")
            .expect("another device's key space is separate");
        // And two jobs with no key do not collide.
        store
            .create_job(&device, &session, None, "ok")
            .expect("a keyless job");
        store
            .create_job(&device, &session, None, "ok")
            .expect("a second keyless job");
    }

    #[test]
    fn pruning_keeps_the_newest_rows_and_the_sequence_keeps_climbing() {
        let (store, device) = store(2);
        let (job, _) = queued(&store, &device);
        for index in 1..=5 {
            let data = serde_json::json!({ "text": format!("f{index}") });
            let seq = store
                .append(&job, FrameKind::Delta, &data)
                .expect("append")
                .expect("a model that is not terminal accepts a delta");
            assert_eq!(seq, index, "seq is monotonic from 1");
        }
        let retained = store.events_after(&job, 0).expect("events");
        assert_eq!(retained.len(), 2, "retention keeps only the newest rows");
        assert_eq!(
            retained.iter().map(|frame| frame.seq).collect::<Vec<_>>(),
            vec![4, 5]
        );
        assert_eq!(store.min_seq(&job).expect("min seq"), Some(4));

        // The counter survives pruning: the next event is 6, not 3.
        let data = serde_json::json!({ "text": "f6" });
        assert_eq!(
            store.append(&job, FrameKind::Delta, &data).expect("append"),
            Some(6)
        );
    }

    #[test]
    fn a_terminal_frame_is_never_followed_by_a_delta() {
        let (store, device) = store(DEFAULT_SSE_BUFFER);
        let (job, _) = queued(&store, &device);
        assert!(store.claim(&job).expect("claim"));

        let delta = serde_json::json!({ "text": "ok" });
        assert!(
            store
                .append(&job, FrameKind::Delta, &delta)
                .expect("append")
                .is_some()
        );

        let done = serde_json::json!({ "text": "ok", "usage": {} });
        let seq = store
            .finish(&job, FrameKind::Done, &done)
            .expect("finish")
            .expect("the first terminal write wins");
        assert_eq!(seq, 2);

        // A provider fragment that arrives after the terminal frame is dropped,
        // which is how a cancellation that lands between two deltas stays final.
        assert_eq!(
            store
                .append(&job, FrameKind::Delta, &delta)
                .expect("append"),
            None,
            "nothing may follow a terminal frame"
        );
        assert_eq!(
            store
                .finish(&job, FrameKind::Done, &done)
                .expect("second finish"),
            None
        );
        assert_eq!(
            store.status(&job).expect("status").as_deref(),
            Some("succeeded")
        );
        let frames = store.events_after(&job, 0).expect("events");
        assert_eq!(frames.last().map(|frame| frame.kind), Some(FrameKind::Done));
    }

    #[test]
    fn only_one_cancel_wins() {
        let (store, device) = store(DEFAULT_SSE_BUFFER);
        let (job, _) = queued(&store, &device);
        assert!(store.claim(&job).expect("claim"));
        let data = serde_json::json!({ "reason": "cancel" });
        assert!(
            store
                .finish(&job, FrameKind::Cancelled, &data)
                .expect("cancel")
                .is_some()
        );
        assert_eq!(
            store
                .finish(&job, FrameKind::Cancelled, &data)
                .expect("second cancel"),
            None
        );
        assert_eq!(
            store.status(&job).expect("status").as_deref(),
            Some("cancelled")
        );
    }

    #[test]
    fn a_job_without_provider_settings_reports_no_config() {
        let (store, _device) = store(DEFAULT_SSE_BUFFER);
        assert!(
            store.provider_config().expect("read settings").is_none(),
            "a fresh database has no provider"
        );
        let db = store.db().clone();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)",
                params![SETTING_PROVIDER_BASE_URL, "http://127.0.0.1:20128/v1"],
            )?;
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)",
                params![SETTING_PROVIDER_KEY_REF, "env:NINEROUTER_API_KEY"],
            )?;
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)",
                params![SETTING_MODEL, "ds/deepseek-v4-flash"],
            )?;
            Ok(())
        })
        .expect("settings rows");
        let config = store.provider_config().expect("read").expect("a config");
        assert_eq!(config.base_url, "http://127.0.0.1:20128/v1");
        assert_eq!(config.model, "ds/deepseek-v4-flash");
    }
}
