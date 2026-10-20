//! SQLite state: one file, WAL, embedded migrations.
//!
//! `~/.local/share/kallisti/eris.db` is the whole durable state of the daemon:
//! the setup state that `/healthz` reports, plus the tables the later phases fill
//! in. P5 shipped 0001; P6 adds 0002 with `auth_tokens`; P7 adds 0003 with
//! `admin_sessions`; P8 adds 0004, the turn columns on `jobs` and the idempotency
//! index. Each connection is opened with three pragmas that are not optional:
//!
//! - `journal_mode=WAL`, so a reader never blocks the single writer;
//! - `foreign_keys=ON`, because SQLite defaults it off per connection and the
//!   cascade from `jobs` to `job_events` is only real if it is on;
//! - `busy_timeout`, so a concurrent writer waits instead of returning
//!   `SQLITE_BUSY`.
//!
//! `synchronous=FULL` follows ADR 0002: this daemon acks a turn, so a commit must
//! survive a power cut, not only a process crash.
//!
//! Migrations are an ordered list, each applied inside one transaction, guarded
//! by the highest version recorded in `schema_migrations`. Running the binary
//! twice therefore applies nothing the second time and inserts no duplicate row.
//! The DDL itself is also written `IF NOT EXISTS` with `INSERT OR IGNORE` seeds,
//! so even a hand-run of 0001 is safe.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;

/// Highest schema version this binary understands.
pub const SCHEMA_VERSION: i64 = 14;

/// How long a blocked writer waits before SQLite returns `SQLITE_BUSY`.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The seventeen tables the migrations create. `schema_migrations` is the
/// eighteenth and is created by the runner, not by a migration. 0001 creates the
/// first nine; 0002 adds `auth_tokens`; 0003 adds `admin_sessions`; 0004 adds
/// columns to `jobs` and no table at all; 0005 adds `chat_messages`; 0008 adds
/// `chat_attachments`; 0009 adds `approvals`; 0013 adds `projects`; 0014 adds
/// `subagents`.
pub const TABLES: [&str; 17] = [
    "admin_credential",
    "admin_sessions",
    "app_state",
    "approvals",
    "audit_log",
    "auth_tokens",
    "chat_attachments",
    "chat_conversations",
    "chat_messages",
    "devices",
    "job_events",
    "jobs",
    "pairing_codes",
    "projects",
    "sessions",
    "settings",
    "subagents",
];

/// One forward migration. Never edited once released; add a new version.
struct Migration {
    version: i64,
    name: &'static str,
    sql: &'static str,
}

/// The migration list. Append only.
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "0001_initial",
        sql: MIGRATION_0001,
    },
    Migration {
        version: 2,
        name: "0002_auth_tokens",
        sql: MIGRATION_0002,
    },
    Migration {
        version: 3,
        name: "0003_admin_sessions",
        sql: MIGRATION_0003,
    },
    Migration {
        version: 4,
        name: "0004_turn_columns",
        sql: MIGRATION_0004,
    },
    Migration {
        version: 5,
        name: "0005_chat_messages",
        sql: MIGRATION_0005,
    },
    Migration {
        version: 6,
        name: "0006_chat_accounting",
        sql: MIGRATION_0006,
    },
    Migration {
        version: 7,
        name: "0007_chat_conversations",
        sql: MIGRATION_0007,
    },
    Migration {
        version: 8,
        name: "0008_chat_attachments",
        sql: MIGRATION_0008,
    },
    Migration {
        version: 9,
        name: "0009_approvals",
        sql: MIGRATION_0009,
    },
    Migration {
        version: 10,
        name: "0010_chat_reasoning",
        sql: MIGRATION_0010,
    },
    Migration {
        version: 11,
        name: "0011_chat_tools",
        sql: MIGRATION_0011,
    },
    Migration {
        version: 12,
        name: "0012_approval_advice",
        sql: MIGRATION_0012,
    },
    Migration {
        version: 13,
        name: "0013_projects",
        sql: MIGRATION_0013,
    },
    Migration {
        version: 14,
        name: "0014_subagents",
        sql: MIGRATION_0014,
    },
];

/// Migration 0001: the whole P5 schema in one transaction.
///
/// Rules baked into the columns rather than into a later layer:
/// `admin_credential` has a hash and no plaintext column anywhere;
/// `pairing_codes.code_hash` is a verifier, not a code; `jobs.status` is a
/// closed set; `job_events` is monotonic per job by `UNIQUE (job_id, seq)`.
const MIGRATION_0001: &str = r"
CREATE TABLE IF NOT EXISTS app_state (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    setup_state TEXT    NOT NULL DEFAULT 'setup'
                CHECK (setup_state IN ('setup', 'ready')),
    updated_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

INSERT OR IGNORE INTO app_state (id, setup_state) VALUES (1, 'setup');

CREATE TABLE IF NOT EXISTS admin_credential (
    id            INTEGER PRIMARY KEY CHECK (id = 1),
    password_hash TEXT    NOT NULL,
    algorithm     TEXT    NOT NULL DEFAULT 'argon2id',
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS devices (
    id           TEXT PRIMARY KEY NOT NULL,
    name         TEXT NOT NULL,
    platform     TEXT NOT NULL,
    public_key   TEXT,
    paired_at    TEXT,
    last_seen_at TEXT,
    revoked_at   TEXT,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS sessions (
    id           TEXT PRIMARY KEY NOT NULL,
    device_id    TEXT REFERENCES devices (id) ON DELETE SET NULL,
    user_agent   TEXT,
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    last_seen_at TEXT,
    expires_at   TEXT NOT NULL,
    revoked_at   TEXT
);

CREATE INDEX IF NOT EXISTS idx_sessions_device_id ON sessions (device_id);

CREATE TABLE IF NOT EXISTS pairing_codes (
    id          TEXT PRIMARY KEY NOT NULL,
    code_hash   TEXT NOT NULL,
    device_id   TEXT REFERENCES devices (id) ON DELETE CASCADE,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at  TEXT NOT NULL,
    consumed_at TEXT,
    attempts    INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0)
);

CREATE INDEX IF NOT EXISTS idx_pairing_codes_expires_at ON pairing_codes (expires_at);

CREATE TABLE IF NOT EXISTS jobs (
    id            TEXT PRIMARY KEY NOT NULL,
    kind          TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'queued'
                  CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    attempts      INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    error_code    TEXT,
    error_message TEXT,
    created_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    started_at    TEXT,
    finished_at   TEXT
);

CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs (status);

CREATE TABLE IF NOT EXISTS job_events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id      TEXT    NOT NULL REFERENCES jobs (id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL CHECK (seq >= 1),
    kind        TEXT    NOT NULL,
    payload     TEXT,
    occurred_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    UNIQUE (job_id, seq)
);

CREATE INDEX IF NOT EXISTS idx_job_events_job_id_seq ON job_events (job_id, seq);

CREATE TABLE IF NOT EXISTS settings (
    key        TEXT PRIMARY KEY NOT NULL,
    value      TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE TABLE IF NOT EXISTS audit_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    occurred_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    actor       TEXT NOT NULL,
    action      TEXT NOT NULL,
    detail      TEXT
);

CREATE INDEX IF NOT EXISTS idx_audit_log_occurred_at ON audit_log (occurred_at);
";

/// Migration 0002: the P6 auth token table.
///
/// A row is one minted token: an Argon2id PHC verifier, the device it belongs to,
/// its kind, the scope set, and the three timestamps that decide whether it still
/// verifies. There is no column for the value itself, and no column that could
/// hold a reversible encoding of it. Both indexes exist for the two reads the
/// verifier path makes: all rows of a device, and the expiry sweep.
///
/// Every statement is `IF NOT EXISTS`, so a hand-run of this file is safe.
const MIGRATION_0002: &str = r"
CREATE TABLE IF NOT EXISTS auth_tokens (
    id         TEXT PRIMARY KEY NOT NULL,
    device_id  TEXT NOT NULL REFERENCES devices (id) ON DELETE CASCADE,
    kind       TEXT NOT NULL CHECK (kind IN ('access', 'refresh')),
    token_hash TEXT NOT NULL,
    scope      TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at TEXT NOT NULL,
    rotated_at TEXT,
    revoked_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_auth_tokens_device_kind ON auth_tokens (device_id, kind);
CREATE INDEX IF NOT EXISTS idx_auth_tokens_expires_at ON auth_tokens (expires_at);
";

/// Migration 0003: the admin browser session, for the onboarding wizard and the
/// dashboard.
///
/// The cookie the browser holds is `<row id>.<secret>`. The row keeps an
/// Argon2id verifier of the secret and never the secret itself, so a copy of
/// `eris.db` cannot be replayed as a logged-in session. The id is a lookup key,
/// not a credential: a rotate or a revoke retires the row and the cookie dies
/// immediately because verification re-reads it on every request.
const MIGRATION_0003: &str = r"
CREATE TABLE IF NOT EXISTS admin_sessions (
    id         TEXT PRIMARY KEY NOT NULL,
    token_hash TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    expires_at TEXT NOT NULL,
    revoked_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_admin_sessions_expires_at ON admin_sessions (expires_at);
";

/// Migration 0004: the turn columns `jobs` needs to be a device job.
///
/// Six columns, no new table:
///
/// - `device_id` and `session_id` say whose job it is; both are `ON DELETE SET
///   NULL`, so a revoked device or a pruned session does not take the job history
///   with it;
/// - `idempotency_key` is the client's `Idempotency-Key` header, verbatim;
/// - `request_text` is the prompt, kept so a claim after a restart still has
///   something to send: the device surface is the first caller that needs a job
///   to outlive the request that created it;
/// - `claimed_at` and `lease_expires_at` are the worker lease. An atomic
///   `UPDATE ... WHERE status = 'queued'` stamps both, so two workers cannot run
///   the same job.
///
/// The unique partial index is the idempotency guarantee in the only place it can
/// be trusted - the database - and it is partial because a job with no key must
/// not collide with another job with no key.
///
/// Unlike 0001-0003 this migration is not hand-runnable twice: SQLite has no
/// `ALTER TABLE ... ADD COLUMN IF NOT EXISTS`. The `schema_migrations` row the
/// runner writes is the guard, which is why the version row is inserted in the
/// same transaction as the DDL.
const MIGRATION_0004: &str = r"
ALTER TABLE jobs ADD COLUMN device_id TEXT REFERENCES devices (id) ON DELETE SET NULL;
ALTER TABLE jobs ADD COLUMN session_id TEXT REFERENCES sessions (id) ON DELETE SET NULL;
ALTER TABLE jobs ADD COLUMN idempotency_key TEXT;
ALTER TABLE jobs ADD COLUMN request_text TEXT;
ALTER TABLE jobs ADD COLUMN claimed_at TEXT;
ALTER TABLE jobs ADD COLUMN lease_expires_at TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_jobs_device_idempotency
    ON jobs (device_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
";

/// Migration 0005: the browser chat transcript.
///
/// One row is one message in one conversation. The pair is deliberate: the body
/// is the operator's own words and the model's reply, so the row is the system of
/// record for what was said, and the conversation id is what lets a second
/// surface (a thread list, a shared chat) exist later without a rewrite.
///
/// `state` is a closed set for the same reason `jobs.status` is: a message is
/// either settled, still arriving, or refused, and a fourth value would be a bug
/// rather than a feature. `usage_json` keeps the provider's own token accounting
/// beside the reply it belongs to, and it is nullable because a refused turn has
/// no usage to report.
const MIGRATION_0005: &str = r"
CREATE TABLE IF NOT EXISTS chat_messages (
    id              TEXT PRIMARY KEY NOT NULL,
    conversation_id TEXT NOT NULL,
    role            TEXT NOT NULL CHECK (role IN ('user', 'assistant')),
    body            TEXT NOT NULL DEFAULT '',
    state           TEXT NOT NULL DEFAULT 'complete'
                    CHECK (state IN ('complete', 'streaming', 'failed')),
    error_code      TEXT,
    usage_json      TEXT,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_chat_messages_conversation
    ON chat_messages (conversation_id, created_at);
";

/// Migration 0006: what a turn cost, on the row that was already there.
///
/// Six columns, no new table. `model` is the id that actually answered, which is
/// not always the default: the composer can pick one per turn. The three token
/// columns are the numbers the provider reported, kept beside `usage_json` (the
/// raw record) so a page can sum a transcript without parsing JSON per row.
/// `context_window` is the window the probe declared for that model, frozen onto
/// the row: a later probe against a different gateway must not retroactively
/// change what an old answer's percentage meant. `duration_ms` is wall time from
/// first byte to last, which is the only way to show tokens per second honestly.
const MIGRATION_0006: &str = r"
ALTER TABLE chat_messages ADD COLUMN model TEXT;
ALTER TABLE chat_messages ADD COLUMN prompt_tokens INTEGER;
ALTER TABLE chat_messages ADD COLUMN completion_tokens INTEGER;
ALTER TABLE chat_messages ADD COLUMN cached_tokens INTEGER;
ALTER TABLE chat_messages ADD COLUMN context_window INTEGER;
ALTER TABLE chat_messages ADD COLUMN duration_ms INTEGER;

CREATE INDEX IF NOT EXISTS idx_chat_messages_model ON chat_messages (model);
";

/// Migration 0007: conversations, so the rail has something to list.
///
/// 1.0.1-1.0.3 kept one transcript keyed by a constant. A session list needs real
/// rows: a title, and an `updated_at` the rail can sort by. The seed statement
/// backfills a row for every conversation the transcript already mentions, so an
/// upgrade does not lose the chat that was in progress.
const MIGRATION_0007: &str = r"
CREATE TABLE IF NOT EXISTS chat_conversations (
    id         TEXT PRIMARY KEY NOT NULL,
    title      TEXT NOT NULL DEFAULT 'New chat',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_chat_conversations_updated_at
    ON chat_conversations (updated_at);

INSERT OR IGNORE INTO chat_conversations (id, title, created_at, updated_at)
    SELECT conversation_id, 'Earlier chat', MIN(created_at), MAX(created_at)
      FROM chat_messages
     GROUP BY conversation_id;
";

/// Migration 0008: files a message carries.
///
/// The bytes live in the workspace, not in the database: a row records where the
/// file is, what it was called when it arrived, and how big it was. `message_id`
/// is filled once the message it belonged to is written, so an upload that
/// arrives before its message is still traceable.
const MIGRATION_0008: &str = r"
CREATE TABLE IF NOT EXISTS chat_attachments (
    id              TEXT PRIMARY KEY NOT NULL,
    conversation_id TEXT NOT NULL,
    message_id      TEXT,
    name            TEXT NOT NULL,
    path            TEXT NOT NULL,
    size            INTEGER NOT NULL DEFAULT 0,
    media_type      TEXT,
    created_at      TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_chat_attachments_conversation
    ON chat_attachments (conversation_id, created_at);
";

/// Migration 0009: the approvals queue.
///
/// A row here is a write the model asked for and the operator has not answered.
/// It exists so `approval_policy = ask` is a boundary instead of a sentence: the
/// turn parks the call, says so to the model, and nothing touches the filesystem
/// until a decision is recorded.
///
/// `status` is a closed set, and a decision is one-way: the update that settles a
/// row also requires it to still be `pending`, so two operators racing cannot both
/// approve and deny the same call.
///
/// `arguments` holds what the model asked to do. It is operator data, not a
/// system secret: the same bytes would otherwise have been written to a file.
const MIGRATION_0009: &str = r"
CREATE TABLE IF NOT EXISTS approvals (
    id           TEXT PRIMARY KEY NOT NULL,
    conversation TEXT NOT NULL,
    tool         TEXT NOT NULL,
    arguments    TEXT NOT NULL,
    status       TEXT NOT NULL DEFAULT 'pending'
                 CHECK (status IN ('pending', 'approved', 'denied', 'expired')),
    created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    decided_at   TEXT,
    decided_by   TEXT
);

CREATE INDEX IF NOT EXISTS idx_approvals_status
    ON approvals (status, created_at);
";

/// Migration 0010: the model's own reasoning, kept beside the answer.
///
/// Some providers stream a separate reasoning stream that is not the reply. It is
/// stored so the UI can show it in a collapsed bubble and so a transcript stays
/// honest about how an answer was reached - but it is never part of `body`, never
/// counted as the answer, and never sent back into a later prompt.
///
/// Nullable on purpose: most providers send none, and a turn with none is normal
/// rather than an empty string pretending to be reasoning.
const MIGRATION_0010: &str = r"
ALTER TABLE chat_messages ADD COLUMN reasoning TEXT;
";

/// Migration 0011: what a turn did, beside what it said.
///
/// One JSON array of `{name, arguments, output, is_error}` records, in the order
/// the calls ran. Kept out of `body` for the same reason reasoning is: the answer
/// is what the operator asked for, and the command output is evidence for how it
/// was reached. Null when a turn called nothing, which is most turns.
const MIGRATION_0011: &str = r"
ALTER TABLE chat_messages ADD COLUMN tools TEXT;
";

/// Migration 0012: what the approval role thought of a parked call.
///
/// A recommendation, not a decision. The row still needs a human: the column
/// exists so the person deciding has the model's reading in front of them, which
/// is the whole point of the role. Nothing in the daemon reads this to decide
/// anything.
const MIGRATION_0012: &str = r"
ALTER TABLE approvals ADD COLUMN advice TEXT;
";

/// Migration 0013: named projects, so a conversation can be grouped.
///
/// A project is a name and nothing else: the grouping lives on the conversation,
/// in `chat_conversations.project_id`, because a conversation is in at most one
/// project and a join table would only make that uniqueness something a caller
/// has to remember. `NULL` is the honest default for every conversation that
/// existed before this migration, and for one the operator never filed.
///
/// `name` is `UNIQUE` in the database rather than only in the form handler: two
/// projects with one name would make the rail's headings ambiguous, and the
/// constraint is the only place a concurrent second create can be refused.
///
/// Like 0004 this migration is not hand-runnable twice, because SQLite has no
/// `ADD COLUMN IF NOT EXISTS`; the `schema_migrations` row the runner writes in
/// the same transaction is the guard.
const MIGRATION_0013: &str = r"
CREATE TABLE IF NOT EXISTS projects (
    id         TEXT PRIMARY KEY NOT NULL,
    name       TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

ALTER TABLE chat_conversations ADD COLUMN project_id TEXT;

CREATE INDEX IF NOT EXISTS idx_chat_conversations_project
    ON chat_conversations (project_id);
";

/// Migration 0014: dispatched work, one row per subagent.
///
/// A row is the whole life of one handoff: what the model asked for, which
/// conversation and which answer it belongs to, what it cost, and how it ended.
/// The table is the record rather than a cache of one, so a row that finished
/// while nobody was looking is still readable afterwards.
///
/// Three columns carry rules rather than prose:
///
/// - `status` is a closed set, and the terminal transitions are conditional in
///   the queries, so a row cannot be resurrected by a late write. A watcher that
///   times a child out settles the row; the child's own write then finds nothing
///   to update and does nothing.
/// - `parent_turn` is the parent's answer row id, which is what the per-turn
///   dispatch budget counts. It is nullable because a turn may have no row yet,
///   and because the device surface has no browser answer to point at.
/// - the three token columns are nullable: a provider that reports no usage
///   leaves them null rather than recording a zero it did not measure.
///
/// There is no foreign key to `chat_conversations`, for the same reason
/// `chat_messages` has none: this is an audit record, and deleting a
/// conversation must not silently delete the evidence that work ran under it.
const MIGRATION_0014: &str = r"
CREATE TABLE IF NOT EXISTS subagents (
    id                  TEXT PRIMARY KEY NOT NULL,
    parent_conversation TEXT NOT NULL,
    parent_turn         TEXT,
    label               TEXT NOT NULL,
    prompt              TEXT NOT NULL,
    status              TEXT NOT NULL DEFAULT 'queued'
                        CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    result              TEXT,
    error_code          TEXT,
    model               TEXT NOT NULL,
    prompt_tokens       INTEGER,
    completion_tokens   INTEGER,
    cached_tokens       INTEGER,
    created_at          TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    started_at          TEXT,
    finished_at         TEXT
);

CREATE INDEX IF NOT EXISTS idx_subagents_status ON subagents (status);
CREATE INDEX IF NOT EXISTS idx_subagents_parent_turn ON subagents (parent_turn);
CREATE INDEX IF NOT EXISTS idx_subagents_created_at ON subagents (created_at);
";

/// Where 1.0 lives, for the reader who lands here first.
pub const DEFAULT_DB_RELATIVE_PATH: &str = ".local/share/kallisti/eris.db";

/// `setup` until onboarding completes, `ready` after.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupState {
    /// First boot, onboarding not finished.
    Setup,
    /// Onboarding finished.
    Ready,
}

impl SetupState {
    /// The wire and database spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Ready => "ready",
        }
    }

    /// Parse a stored value. Anything outside the CHECK constraint is a bug or
    /// a hand-edited database, and both deserve a loud error.
    pub fn parse(value: &str) -> Result<Self, DbError> {
        match value {
            "setup" => Ok(Self::Setup),
            "ready" => Ok(Self::Ready),
            other => Err(DbError::UnexpectedSetupState {
                value: other.to_owned(),
            }),
        }
    }
}

/// Everything that can go wrong around the database.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    /// The state directory or another path could not be created.
    #[error("cannot prepare state directory {path}: {source}")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// SQLite itself refused.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The file was written by a newer binary. Refuse rather than corrupt it.
    #[error(
        "database schema version {found} is newer than the {supported} this binary understands; upgrade eris instead of downgrading the database"
    )]
    SchemaAhead {
        /// Version found in the file.
        found: i64,
        /// Version this binary supports.
        supported: i64,
    },
    /// A version row was written but its DDL is missing from the binary.
    #[error("schema version {version} was applied but this binary has no migration for it")]
    MissingMigration {
        /// Version that could not be found.
        version: i64,
    },
    /// `journal_mode=WAL` did not stick.
    #[error(
        "refusing to run: journal_mode is {actual:?}, not wal (SQLite needs a writable file, not memory)"
    )]
    NotWal {
        /// What SQLite reported.
        actual: String,
    },
    /// `app_state` has its one row removed.
    #[error("app_state has no row; the database was modified outside eris")]
    MissingAppState,
    /// `setup_state` holds a value outside the CHECK constraint.
    #[error("app_state.setup_state is {value:?}, which is neither 'setup' nor 'ready'")]
    UnexpectedSetupState {
        /// The value that was found.
        value: String,
    },
    /// The connection mutex was poisoned by a panic in another thread.
    #[error("database connection is unusable: a previous holder panicked")]
    Locked,
    /// A project name is outside the rule: empty, or longer than
    /// [`crate::projects::MAX_NAME_CHARS`].
    #[error("a project name must be {max} characters or fewer, and not blank")]
    InvalidProjectName {
        /// The ceiling that was applied.
        max: usize,
    },
    /// Two projects would carry one name, which the `UNIQUE` constraint refuses.
    #[error("a project named {name:?} already exists")]
    DuplicateProjectName {
        /// The name that collided.
        name: String,
    },
    /// `subagents.status` holds a value outside the CHECK constraint.
    #[error("subagents.status is {value:?}, which is not a status this build knows")]
    UnexpectedSubagentStatus {
        /// The value that was found.
        value: String,
    },
}

/// A handle to the one database. Cheap to clone; the connection is shared.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Inner>,
}

struct Inner {
    conn: Mutex<Connection>,
    path: Option<PathBuf>,
}

impl Db {
    /// Open (creating if needed), configure, and migrate the database file.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            prepare_state_dir(parent)?;
        }
        let mut conn = Connection::open(path)?;
        configure(&conn)?;
        migrate(&mut conn)?;
        Ok(Self {
            inner: Arc::new(Inner {
                conn: Mutex::new(conn),
                path: Some(path.to_path_buf()),
            }),
        })
    }

    /// An in-memory database, for tests. WAL is not available to `:memory:`, so
    /// the two pragmas that are still meaningful are applied and the WAL check
    /// is skipped.
    pub fn open_in_memory() -> Result<Self, DbError> {
        let mut conn = Connection::open_in_memory()?;
        conn.execute_batch(&format!(
            "PRAGMA foreign_keys = ON; PRAGMA busy_timeout = {};",
            BUSY_TIMEOUT.as_millis()
        ))?;
        migrate(&mut conn)?;
        Ok(Self {
            inner: Arc::new(Inner {
                conn: Mutex::new(conn),
                path: None,
            }),
        })
    }

    /// The file this database lives in, when it is a file.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.inner.path.as_deref()
    }

    /// Run one closure against the connection.
    ///
    /// This is the escape hatch for the modules that own their own tables (the
    /// auth store in P6). It hands out the live connection, so a caller must not
    /// stash it, and it maps a poisoned mutex to [`DbError::Locked`] rather than
    /// panicking a request thread.
    pub fn with_conn<T>(
        &self,
        run: impl FnOnce(&mut Connection) -> rusqlite::Result<T>,
    ) -> Result<T, DbError> {
        let mut conn = self.inner.conn.lock().map_err(|_| DbError::Locked)?;
        run(&mut conn).map_err(DbError::Sqlite)
    }

    /// `"setup"` or `"ready"`, read from `app_state`.
    pub fn setup_state(&self) -> Result<SetupState, DbError> {
        let conn = self.inner.conn.lock().map_err(|_| DbError::Locked)?;
        let value: Option<String> = conn
            .query_row(
                "SELECT setup_state FROM app_state WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        match value {
            Some(value) => SetupState::parse(&value),
            None => Err(DbError::MissingAppState),
        }
    }

    /// Move the setup state. There is no route that calls this in P5; P7 owns
    /// that transition.
    pub fn set_setup_state(&self, state: SetupState) -> Result<(), DbError> {
        let conn = self.inner.conn.lock().map_err(|_| DbError::Locked)?;
        let changed = conn.execute(
            "UPDATE app_state SET setup_state = ?1, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now') WHERE id = 1",
            [state.as_str()],
        )?;
        if changed != 1 {
            return Err(DbError::MissingAppState);
        }
        Ok(())
    }

    /// The highest applied migration version.
    pub fn schema_version(&self) -> Result<i64, DbError> {
        let conn = self.inner.conn.lock().map_err(|_| DbError::Locked)?;
        let version: i64 = conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?;
        Ok(version)
    }

    /// Number of rows in `schema_migrations`. One per applied migration.
    pub fn applied_migration_count(&self) -> Result<i64, DbError> {
        let conn = self.inner.conn.lock().map_err(|_| DbError::Locked)?;
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })?;
        Ok(count)
    }

    /// The effective journal mode, lowercased.
    pub fn journal_mode(&self) -> Result<String, DbError> {
        let conn = self.inner.conn.lock().map_err(|_| DbError::Locked)?;
        let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
        Ok(mode.to_ascii_lowercase())
    }

    /// Whether this connection enforces foreign keys.
    pub fn foreign_keys_enabled(&self) -> Result<bool, DbError> {
        let conn = self.inner.conn.lock().map_err(|_| DbError::Locked)?;
        let enabled: i64 = conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?;
        Ok(enabled == 1)
    }

    /// The durability pragma: 0 `off`, 1 `normal`, 2 `full`, 3 `extra`.
    pub fn synchronous_level(&self) -> Result<i64, DbError> {
        let conn = self.inner.conn.lock().map_err(|_| DbError::Locked)?;
        let level: i64 = conn.query_row("PRAGMA synchronous", [], |row| row.get(0))?;
        Ok(level)
    }

    /// The configured busy timeout, in milliseconds.
    pub fn busy_timeout_ms(&self) -> Result<i64, DbError> {
        let conn = self.inner.conn.lock().map_err(|_| DbError::Locked)?;
        let timeout: i64 = conn.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?;
        Ok(timeout)
    }

    /// User table names, sorted, with SQLite's internal tables excluded.
    pub fn table_names(&self) -> Result<Vec<String>, DbError> {
        let conn = self.inner.conn.lock().map_err(|_| DbError::Locked)?;
        let mut statement = conn.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' ORDER BY name",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut names = Vec::new();
        for name in rows {
            names.push(name?);
        }
        Ok(names)
    }

    /// Column names of one table, in declaration order.
    pub fn column_names(&self, table: &str) -> Result<Vec<String>, DbError> {
        let conn = self.inner.conn.lock().map_err(|_| DbError::Locked)?;
        let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
        let mut names = Vec::new();
        for name in rows {
            names.push(name?);
        }
        Ok(names)
    }

    /// Row count of one table.
    pub fn row_count(&self, table: &str) -> Result<i64, DbError> {
        let conn = self.inner.conn.lock().map_err(|_| DbError::Locked)?;
        let count: i64 = conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })?;
        Ok(count)
    }

    /// `PRAGMA quick_check`: `"ok"` or the first problem SQLite reports.
    pub fn quick_check(&self) -> Result<String, DbError> {
        let conn = self.inner.conn.lock().map_err(|_| DbError::Locked)?;
        let result: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
        Ok(result)
    }
}

/// `Option<T>` for a query that may match no row, without a crate for it.
trait OptionalRow<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalRow<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(other) => Err(other),
        }
    }
}

/// Create the state directory if needed and make it owner-only.
fn prepare_state_dir(dir: &Path) -> Result<(), DbError> {
    let io_error = |path: &Path, source: std::io::Error| DbError::Io {
        path: path.to_path_buf(),
        source,
    };
    fs::create_dir_all(dir).map_err(|source| io_error(dir, source))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // The database holds session and pairing verifiers. Nobody else on the
        // box needs to list it.
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))
            .map_err(|source| io_error(dir, source))?;
    }
    Ok(())
}

/// The three pragmas that are not optional.
fn configure(conn: &Connection) -> Result<(), DbError> {
    // `journal_mode` returns the mode it settled on, so it needs a query, not an
    // execute. On a file database that must be `wal`.
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(DbError::NotWal { actual: mode });
    }
    conn.execute_batch(&format!(
        "PRAGMA foreign_keys = ON; PRAGMA busy_timeout = {}; PRAGMA synchronous = FULL;",
        BUSY_TIMEOUT.as_millis()
    ))?;
    Ok(())
}

/// Apply every migration newer than the recorded version, one transaction each.
fn migrate(conn: &mut Connection) -> Result<(), DbError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY NOT NULL,
             name       TEXT    NOT NULL,
             applied_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
         );",
    )?;

    let current: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    )?;

    if current > SCHEMA_VERSION {
        return Err(DbError::SchemaAhead {
            found: current,
            supported: SCHEMA_VERSION,
        });
    }

    for migration in MIGRATIONS.iter().filter(|m| m.version > current) {
        let transaction = conn.transaction()?;
        transaction.execute_batch(migration.sql)?;
        transaction.execute(
            "INSERT INTO schema_migrations (version, name) VALUES (?1, ?2)",
            rusqlite::params![migration.version, migration.name],
        )?;
        transaction.commit()?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_state_round_trips_and_rejects_junk() {
        assert_eq!(
            SetupState::parse("setup").expect("setup parses"),
            SetupState::Setup
        );
        assert_eq!(
            SetupState::parse("ready").expect("ready parses"),
            SetupState::Ready
        );
        assert!(matches!(
            SetupState::parse("onboarding"),
            Err(DbError::UnexpectedSetupState { .. })
        ));
        assert_eq!(SetupState::Setup.as_str(), "setup");
        assert_eq!(SetupState::Ready.as_str(), "ready");
    }
}
