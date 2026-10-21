//! Notes: the full `/v1/notes` + `/v1/note-runs` router, Eris edition.
//!
//! This is the Rust port of the legacy `relay/app/notes.py` router and the
//! `Note`, `NoteBlob`, `NoteRecognition`, `NoteRun`, `NoteRunEvent` and
//! `EnrichedNoteRevision` models. It lives in its own SQLite file (`notes.db`)
//! beside `eris.db`, following the sensor-store pattern (WAL, `busy_timeout`,
//! `synchronous=FULL`, typed errors).
//!
//! The enrichment "run" execution is deliberately a stub: `create_run` records a
//! `queued` run (and the run machinery - events, cancel, idempotency - is real),
//! but nothing here calls an AI model. That execution is a separate, deferred
//! worker; the contract is what matters to the frontend.
//!
//! Identity note: Eris has no `users` table. The legacy `user_id` ownership
//! column maps to `device_id`, and the `userId` field the note/run serializers
//! emit is the authenticated device id, so the frontend still sees a real UUID
//! in that field.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::Serialize;

use crate::db::BUSY_TIMEOUT;

/// The legacy notes schema, names preserved. JSON is stored as TEXT; `device_id`
/// is the Eris stand-in for the legacy `user_id`.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS notes (
    id                       TEXT PRIMARY KEY NOT NULL,
    device_id                TEXT NOT NULL,
    title                    TEXT NOT NULL DEFAULT '',
    folder_id                TEXT,
    pinned                   INTEGER NOT NULL DEFAULT 0,
    revision                 INTEGER NOT NULL DEFAULT 1,
    current_drawing_revision INTEGER NOT NULL DEFAULT 0,
    current_text_revision    INTEGER NOT NULL DEFAULT 0,
    created_at               TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    updated_at               TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    deleted_at               TEXT
);
CREATE INDEX IF NOT EXISTS idx_notes_device_updated ON notes (device_id, updated_at DESC);
CREATE TABLE IF NOT EXISTS note_blobs (
    id               TEXT PRIMARY KEY NOT NULL,
    note_id          TEXT NOT NULL,
    drawing_revision INTEGER NOT NULL,
    content_hash     TEXT NOT NULL,
    byte_size        INTEGER NOT NULL,
    storage_path     TEXT NOT NULL,
    created_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    UNIQUE (note_id, drawing_revision)
);
CREATE TABLE IF NOT EXISTS note_recognitions (
    id                   TEXT PRIMARY KEY NOT NULL,
    note_id              TEXT NOT NULL,
    drawing_revision     INTEGER NOT NULL,
    engine               TEXT NOT NULL,
    engine_version       TEXT,
    languages            TEXT,
    raw_text             TEXT NOT NULL,
    user_corrected_text  TEXT,
    created_at           TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE TABLE IF NOT EXISTS note_runs (
    id                       TEXT PRIMARY KEY NOT NULL,
    device_id                TEXT NOT NULL,
    note_id                  TEXT NOT NULL,
    client_run_id            TEXT NOT NULL,
    source_drawing_revision  INTEGER NOT NULL,
    source_text_revision     INTEGER NOT NULL,
    requested_directives     TEXT NOT NULL,
    status                   TEXT NOT NULL DEFAULT 'queued',
    attempt                  INTEGER NOT NULL DEFAULT 0,
    lease_expires_at         TEXT,
    error_text               TEXT,
    result                   TEXT,
    created_at               TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    completed_at             TEXT,
    UNIQUE (device_id, client_run_id)
);
CREATE TABLE IF NOT EXISTS note_run_events (
    id          TEXT PRIMARY KEY NOT NULL,
    run_id      TEXT NOT NULL,
    seq         INTEGER NOT NULL,
    attempt     INTEGER NOT NULL,
    source_seq  INTEGER,
    type        TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_note_run_events_run_seq ON note_run_events (run_id, seq);
CREATE TABLE IF NOT EXISTS enriched_note_revisions (
    id                       TEXT PRIMARY KEY NOT NULL,
    note_id                  TEXT NOT NULL,
    run_id                   TEXT NOT NULL,
    source_drawing_revision  INTEGER NOT NULL,
    source_text_revision     INTEGER NOT NULL,
    schema_version           INTEGER NOT NULL DEFAULT 1,
    title                    TEXT NOT NULL,
    markdown                 TEXT NOT NULL,
    structured_sections      TEXT NOT NULL,
    citations                TEXT,
    command_results          TEXT,
    is_stale                 INTEGER NOT NULL DEFAULT 0,
    created_at               TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
";

/// The `_note_to_dict` wire shape.
#[derive(Debug, Serialize, PartialEq)]
pub struct NoteView {
    pub id: String,
    #[serde(rename = "userId")]
    pub user_id: String,
    pub title: String,
    #[serde(rename = "folderId")]
    pub folder_id: Option<String>,
    pub pinned: bool,
    pub revision: i64,
    #[serde(rename = "currentDrawingRevision")]
    pub current_drawing_revision: i64,
    #[serde(rename = "currentTextRevision")]
    pub current_text_revision: i64,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "updatedAt")]
    pub updated_at: String,
    #[serde(rename = "deletedAt")]
    pub deleted_at: Option<String>,
}

/// The `_run_to_dict` wire shape.
#[derive(Debug, Serialize)]
pub struct NoteRunView {
    pub id: String,
    #[serde(rename = "userId")]
    pub user_id: String,
    #[serde(rename = "noteId")]
    pub note_id: String,
    #[serde(rename = "clientRunId")]
    pub client_run_id: String,
    #[serde(rename = "sourceDrawingRevision")]
    pub source_drawing_revision: i64,
    #[serde(rename = "sourceTextRevision")]
    pub source_text_revision: i64,
    #[serde(rename = "requestedDirectives")]
    pub requested_directives: serde_json::Value,
    pub status: String,
    pub attempt: i64,
    #[serde(rename = "leaseExpiresAt")]
    pub lease_expires_at: Option<String>,
    #[serde(rename = "errorText")]
    pub error_text: Option<String>,
    pub result: Option<serde_json::Value>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
    #[serde(rename = "completedAt")]
    pub completed_at: Option<String>,
}

/// The `_event_to_dict` wire shape. `type` is a Rust keyword, so the field is
/// `type_` and serde renames it back to the wire name.
#[derive(Debug, Serialize)]
pub struct NoteRunEventView {
    pub id: String,
    #[serde(rename = "runId")]
    pub run_id: String,
    pub seq: i64,
    pub attempt: i64,
    #[serde(rename = "sourceSeq")]
    pub source_seq: Option<i64>,
    #[serde(rename = "type")]
    pub type_: String,
    pub payload: serde_json::Value,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

/// The `_recognition_to_dict` wire shape.
#[derive(Debug, Serialize)]
pub struct RecognitionView {
    pub id: String,
    #[serde(rename = "noteId")]
    pub note_id: String,
    #[serde(rename = "drawingRevision")]
    pub drawing_revision: i64,
    pub engine: String,
    #[serde(rename = "engineVersion")]
    pub engine_version: Option<String>,
    pub languages: Option<serde_json::Value>,
    #[serde(rename = "rawText")]
    pub raw_text: String,
    #[serde(rename = "userCorrectedText")]
    pub user_corrected_text: Option<String>,
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

/// The blob upload reply: `noteId`, `revision`, `contentHash`.
#[derive(Debug, Serialize)]
pub struct BlobUploadView {
    #[serde(rename = "noteId")]
    pub note_id: String,
    pub revision: i64,
    #[serde(rename = "contentHash")]
    pub content_hash: String,
}

/// The blob download reply: `noteId`, `revision`, `contentHash`, `byteSize`,
/// `storagePath`.
#[derive(Debug, Serialize)]
pub struct BlobDownloadView {
    #[serde(rename = "noteId")]
    pub note_id: String,
    pub revision: i64,
    #[serde(rename = "contentHash")]
    pub content_hash: String,
    #[serde(rename = "byteSize")]
    pub byte_size: i64,
    #[serde(rename = "storagePath")]
    pub storage_path: String,
}

/// The outcome of a note patch: one of updated, missing, or a revision clash.
#[derive(Debug)]
pub enum PatchOutcome {
    Updated(NoteView),
    NoteNotFound,
    RevisionMismatch,
}

/// The outcome of a blob upload.
#[derive(Debug)]
pub enum BlobUploadOutcome {
    Ok(BlobUploadView),
    NoteNotFound,
    RevisionExists,
}

/// The outcome of a blob download.
#[derive(Debug)]
pub enum BlobDownloadOutcome {
    Ok(BlobDownloadView),
    NoteNotFound,
    BlobNotFound,
}

/// The outcome of a recognition create.
#[derive(Debug)]
pub enum RecognitionOutcome {
    Ok(Box<RecognitionView>),
    NoteNotFound,
}

/// The outcome of a run create.
#[derive(Debug)]
pub enum RunCreateOutcome {
    Created(NoteRunView),
    Existing(NoteRunView),
    NoteNotFound,
}

/// Everything that can go wrong in the notes store.
#[derive(Debug, thiserror::Error)]
pub enum NotesError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("notes database connection is unusable: a previous holder panicked")]
    Locked,
    #[error("cannot prepare state directory {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("refusing to open the notes store: journal_mode is {actual:?}, not wal")]
    NotWal { actual: String },
}

/// A handle to the notes database. Cheap to clone; the connection is shared.
#[derive(Clone)]
pub struct NotesStore {
    inner: Arc<Inner>,
}

struct Inner {
    conn: Mutex<Connection>,
}

impl NotesStore {
    /// Open (creating if needed), configure, and initialise the notes database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, NotesError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| NotesError::Io {
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
    pub fn open_in_memory() -> Result<Self, NotesError> {
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

    /// List a device's live notes, newest first (hard-deleted excluded).
    pub fn list(&self, device_id: &str) -> Result<Vec<NoteView>, NotesError> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT id, device_id, title, folder_id, pinned, revision,
                    current_drawing_revision, current_text_revision, created_at, updated_at, deleted_at
             FROM notes WHERE device_id = ?1 AND deleted_at IS NULL ORDER BY updated_at DESC",
        )?;
        let rows = statement.query_map([device_id], note_to_view)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// One note, owned by the device, regardless of soft-delete (matching the
    /// legacy `_get_note_or_404`, which never checks `deleted_at`).
    pub fn get(&self, device_id: &str, note_id: &str) -> Result<Option<NoteView>, NotesError> {
        let conn = self.lock()?;
        let view = optional(conn.query_row(
            "SELECT id, device_id, title, folder_id, pinned, revision,
                    current_drawing_revision, current_text_revision, created_at, updated_at, deleted_at
             FROM notes WHERE id = ?1 AND device_id = ?2",
            rusqlite::params![note_id, device_id],
            note_to_view,
        ))?;
        Ok(view)
    }

    /// Create a note. `revision` starts at 1, both source revisions at 0.
    pub fn create(
        &self,
        device_id: &str,
        title: &str,
        folder_id: Option<&str>,
        pinned: bool,
    ) -> Result<NoteView, NotesError> {
        let id = uuid::Uuid::now_v7().to_string();
        {
            let conn = self.lock()?;
            conn.execute(
                "INSERT INTO notes (id, device_id, title, folder_id, pinned)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, device_id, title, folder_id, pinned],
            )?;
        }
        Ok(self
            .get(device_id, &id)?
            .expect("the note was just written"))
    }

    /// Patch a note with optimistic concurrency. The `If-Match` header value is
    /// compared against `"<revision>"` (with quotes) exactly as the legacy route
    /// did; a mismatch refuses rather than clobbering a concurrent write.
    #[allow(clippy::too_many_arguments)]
    pub fn patch(
        &self,
        device_id: &str,
        note_id: &str,
        if_match: &str,
        title: Option<&str>,
        folder_id: Option<Option<String>>,
        pinned: Option<bool>,
    ) -> Result<PatchOutcome, NotesError> {
        let mut conn = self.lock()?;
        let transaction = conn.transaction()?;

        let revision: Option<i64> = optional(transaction.query_row(
            "SELECT revision FROM notes WHERE id = ?1 AND device_id = ?2",
            rusqlite::params![note_id, device_id],
            |row| row.get(0),
        ))?;
        let Some(revision) = revision else {
            return Ok(PatchOutcome::NoteNotFound);
        };
        let expected = format!("\"{revision}\"");
        if if_match != expected {
            return Ok(PatchOutcome::RevisionMismatch);
        }

        transaction.execute(
            "UPDATE notes SET
                 title = COALESCE(?1, title),
                 folder_id = CASE WHEN ?2 = 1 THEN ?3 ELSE folder_id END,
                 pinned = COALESCE(?4, pinned),
                 revision = revision + 1,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id = ?5",
            rusqlite::params![
                title,
                i64::from(folder_id.is_some()),
                folder_id.flatten(),
                pinned,
                note_id,
            ],
        )?;
        transaction.commit()?;
        drop(conn);

        Ok(PatchOutcome::Updated(
            self.get(device_id, note_id)?
                .expect("the note was just updated"),
        ))
    }

    /// Soft-delete a note. Returns the id on success, `None` if not owned.
    pub fn delete(&self, device_id: &str, note_id: &str) -> Result<bool, NotesError> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE notes SET deleted_at = strftime('%Y-%m-%dT%H:%M:%fZ','now'),
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id = ?1 AND device_id = ?2",
            rusqlite::params![note_id, device_id],
        )?;
        Ok(changed == 1)
    }

    /// Record a drawing blob for a note revision. A revision that already exists
    /// refuses with [`BlobUploadOutcome::RevisionExists`].
    pub fn upload_blob(
        &self,
        device_id: &str,
        note_id: &str,
        revision: i64,
        content_hash: &str,
        byte_size: i64,
        storage_path: &str,
    ) -> Result<BlobUploadOutcome, NotesError> {
        let mut conn = self.lock()?;
        let transaction = conn.transaction()?;

        let note_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM notes WHERE id = ?1 AND device_id = ?2)",
            rusqlite::params![note_id, device_id],
            |row| row.get(0),
        )?;
        if !note_exists {
            return Ok(BlobUploadOutcome::NoteNotFound);
        }
        let exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM note_blobs WHERE note_id = ?1 AND drawing_revision = ?2)",
            rusqlite::params![note_id, revision],
            |row| row.get(0),
        )?;
        if exists {
            return Ok(BlobUploadOutcome::RevisionExists);
        }

        let id = uuid::Uuid::now_v7().to_string();
        transaction.execute(
            "INSERT INTO note_blobs (id, note_id, drawing_revision, content_hash, byte_size, storage_path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, note_id, revision, content_hash, byte_size, storage_path],
        )?;
        transaction.execute(
            "UPDATE notes SET
                 current_drawing_revision = MAX(current_drawing_revision, ?1),
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE id = ?2",
            rusqlite::params![revision, note_id],
        )?;
        transaction.commit()?;

        Ok(BlobUploadOutcome::Ok(BlobUploadView {
            note_id: note_id.to_owned(),
            revision,
            content_hash: content_hash.to_owned(),
        }))
    }

    /// Fetch one drawing blob's metadata.
    pub fn download_blob(
        &self,
        device_id: &str,
        note_id: &str,
        revision: i64,
    ) -> Result<BlobDownloadOutcome, NotesError> {
        let conn = self.lock()?;
        let note_exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM notes WHERE id = ?1 AND device_id = ?2)",
            rusqlite::params![note_id, device_id],
            |row| row.get(0),
        )?;
        if !note_exists {
            return Ok(BlobDownloadOutcome::NoteNotFound);
        }
        let blob = optional(conn.query_row(
            "SELECT content_hash, byte_size, storage_path
             FROM note_blobs WHERE note_id = ?1 AND drawing_revision = ?2",
            rusqlite::params![note_id, revision],
            |row| {
                Ok(BlobDownloadView {
                    note_id: note_id.to_owned(),
                    revision,
                    content_hash: row.get(0)?,
                    byte_size: row.get(1)?,
                    storage_path: row.get(2)?,
                })
            },
        ))?;
        Ok(match blob {
            Some(blob) => BlobDownloadOutcome::Ok(blob),
            None => BlobDownloadOutcome::BlobNotFound,
        })
    }

    /// Store an OCR/handwriting recognition for a note revision.
    #[allow(clippy::too_many_arguments)]
    pub fn create_recognition(
        &self,
        device_id: &str,
        note_id: &str,
        drawing_revision: i64,
        engine: &str,
        engine_version: Option<&str>,
        languages: Option<&serde_json::Value>,
        raw_text: &str,
        user_corrected_text: Option<&str>,
    ) -> Result<RecognitionOutcome, NotesError> {
        let mut conn = self.lock()?;
        let transaction = conn.transaction()?;
        let note_exists: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM notes WHERE id = ?1 AND device_id = ?2)",
            rusqlite::params![note_id, device_id],
            |row| row.get(0),
        )?;
        if !note_exists {
            return Ok(RecognitionOutcome::NoteNotFound);
        }

        let id = uuid::Uuid::now_v7().to_string();
        let languages_text = match languages {
            Some(value) => Some(serde_json::to_string(value)?),
            None => None,
        };
        transaction.execute(
            "INSERT INTO note_recognitions
                 (id, note_id, drawing_revision, engine, engine_version, languages, raw_text, user_corrected_text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                id,
                note_id,
                drawing_revision,
                engine,
                engine_version,
                languages_text,
                raw_text,
                user_corrected_text,
            ],
        )?;
        transaction.commit()?;
        drop(conn);

        let conn = self.lock()?;
        let view = conn.query_row(
            "SELECT id, note_id, drawing_revision, engine, engine_version, languages,
                    raw_text, user_corrected_text, created_at
             FROM note_recognitions WHERE id = ?1",
            [&id],
            recognition_to_view,
        )?;
        Ok(RecognitionOutcome::Ok(Box::new(view)))
    }

    /// Create an enrichment run, idempotent on `clientRunId` per device.
    #[allow(clippy::too_many_arguments)]
    pub fn create_run(
        &self,
        device_id: &str,
        note_id: &str,
        client_run_id: &str,
        source_drawing_revision: Option<i64>,
        source_text_revision: Option<i64>,
        requested_directives: serde_json::Value,
    ) -> Result<RunCreateOutcome, NotesError> {
        let mut conn = self.lock()?;
        let transaction = conn.transaction()?;

        let note: Option<(i64, i64)> = optional(transaction.query_row(
            "SELECT current_drawing_revision, current_text_revision
             FROM notes WHERE id = ?1 AND device_id = ?2",
            rusqlite::params![note_id, device_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ))?;
        let Some((current_drawing, current_text)) = note else {
            return Ok(RunCreateOutcome::NoteNotFound);
        };

        let existing: Option<String> = optional(transaction.query_row(
            "SELECT id FROM note_runs WHERE device_id = ?1 AND client_run_id = ?2",
            rusqlite::params![device_id, client_run_id],
            |row| row.get(0),
        ))?;
        if let Some(existing_id) = existing {
            let view = transaction.query_row(
                "SELECT id, device_id, note_id, client_run_id, source_drawing_revision,
                        source_text_revision, requested_directives, status, attempt,
                        lease_expires_at, error_text, result, created_at, completed_at
                 FROM note_runs WHERE id = ?1",
                [&existing_id],
                run_to_view,
            )?;
            transaction.commit()?;
            return Ok(RunCreateOutcome::Existing(view));
        }

        let id = uuid::Uuid::now_v7().to_string();
        let drawing = source_drawing_revision.unwrap_or(current_drawing);
        let text = source_text_revision.unwrap_or(current_text);
        transaction.execute(
            "INSERT INTO note_runs
                 (id, device_id, note_id, client_run_id, source_drawing_revision,
                  source_text_revision, requested_directives)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                id,
                device_id,
                note_id,
                client_run_id,
                drawing,
                text,
                serde_json::to_string(&requested_directives)?,
            ],
        )?;
        transaction.commit()?;
        drop(conn);

        Ok(RunCreateOutcome::Created(
            self.get_run(device_id, &id)?
                .expect("the run was just written"),
        ))
    }

    /// One run, owned by the device.
    pub fn get_run(
        &self,
        device_id: &str,
        run_id: &str,
    ) -> Result<Option<NoteRunView>, NotesError> {
        let conn = self.lock()?;
        let view = optional(conn.query_row(
            "SELECT id, device_id, note_id, client_run_id, source_drawing_revision,
                    source_text_revision, requested_directives, status, attempt,
                    lease_expires_at, error_text, result, created_at, completed_at
             FROM note_runs WHERE id = ?1 AND device_id = ?2",
            rusqlite::params![run_id, device_id],
            run_to_view,
        ))?;
        Ok(view)
    }

    /// The events of a run after a cursor, oldest first, bounded by `limit`.
    /// Returns `None` when the run does not belong to the device.
    pub fn run_events(
        &self,
        device_id: &str,
        run_id: &str,
        after_seq: Option<i64>,
        limit: i64,
    ) -> Result<Option<Vec<NoteRunEventView>>, NotesError> {
        let conn = self.lock()?;
        let owned: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM note_runs WHERE id = ?1 AND device_id = ?2)",
            rusqlite::params![run_id, device_id],
            |row| row.get(0),
        )?;
        if !owned {
            return Ok(None);
        }
        let mut statement = conn.prepare(
            "SELECT id, run_id, seq, attempt, source_seq, type, payload_json, created_at
             FROM note_run_events
             WHERE run_id = ?1 AND (?2 IS NULL OR seq > ?2)
             ORDER BY seq ASC LIMIT ?3",
        )?;
        let rows = statement.query_map(rusqlite::params![run_id, after_seq, limit], |row| {
            event_to_view(row)
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(Some(out))
    }

    /// Cancel a run. Terminal runs are returned unchanged (idempotent).
    pub fn cancel_run(
        &self,
        device_id: &str,
        run_id: &str,
    ) -> Result<Option<NoteRunView>, NotesError> {
        let mut conn = self.lock()?;
        let transaction = conn.transaction()?;
        let status: Option<String> = optional(transaction.query_row(
            "SELECT status FROM note_runs WHERE id = ?1 AND device_id = ?2",
            rusqlite::params![run_id, device_id],
            |row| row.get(0),
        ))?;
        let Some(status) = status else {
            return Ok(None);
        };
        if !matches!(status.as_str(), "completed" | "failed" | "cancelled") {
            transaction.execute(
                "UPDATE note_runs SET status = 'cancelled',
                     completed_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
                 WHERE id = ?1",
                [run_id],
            )?;
        }
        transaction.commit()?;
        drop(conn);
        self.get_run(device_id, run_id)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, NotesError> {
        self.inner.conn.lock().map_err(|_| NotesError::Locked)
    }
}

fn note_to_view(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteView> {
    Ok(NoteView {
        id: row.get(0)?,
        user_id: row.get(1)?,
        title: row.get(2)?,
        folder_id: row.get(3)?,
        pinned: row.get(4)?,
        revision: row.get(5)?,
        current_drawing_revision: row.get(6)?,
        current_text_revision: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        deleted_at: row.get(10)?,
    })
}

fn run_to_view(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteRunView> {
    let requested_directives: String = row.get(6)?;
    let result_json: Option<String> = row.get(11)?;
    Ok(NoteRunView {
        id: row.get(0)?,
        user_id: row.get(1)?,
        note_id: row.get(2)?,
        client_run_id: row.get(3)?,
        source_drawing_revision: row.get(4)?,
        source_text_revision: row.get(5)?,
        requested_directives: serde_json::from_str(&requested_directives)
            .unwrap_or(serde_json::Value::Array(Vec::new())),
        status: row.get(7)?,
        attempt: row.get(8)?,
        lease_expires_at: row.get(9)?,
        error_text: row.get(10)?,
        result: result_json
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok()),
        created_at: row.get(12)?,
        completed_at: row.get(13)?,
    })
}

fn event_to_view(row: &rusqlite::Row<'_>) -> rusqlite::Result<NoteRunEventView> {
    let payload_json: String = row.get(6)?;
    Ok(NoteRunEventView {
        id: row.get(0)?,
        run_id: row.get(1)?,
        seq: row.get(2)?,
        attempt: row.get(3)?,
        source_seq: row.get(4)?,
        type_: row.get(5)?,
        payload: serde_json::from_str(&payload_json).unwrap_or(serde_json::Value::Null),
        created_at: row.get(7)?,
    })
}

fn recognition_to_view(row: &rusqlite::Row<'_>) -> rusqlite::Result<RecognitionView> {
    let languages_json: Option<String> = row.get(5)?;
    Ok(RecognitionView {
        id: row.get(0)?,
        note_id: row.get(1)?,
        drawing_revision: row.get(2)?,
        engine: row.get(3)?,
        engine_version: row.get(4)?,
        languages: languages_json
            .as_deref()
            .and_then(|text| serde_json::from_str(text).ok()),
        raw_text: row.get(6)?,
        user_corrected_text: row.get(7)?,
        created_at: row.get(8)?,
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
fn configure(conn: &Connection) -> Result<(), NotesError> {
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(NotesError::NotWal { actual: mode });
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

    fn store() -> NotesStore {
        NotesStore::open_in_memory().expect("in-memory notes store opens")
    }

    fn json_arr(items: &[&str]) -> serde_json::Value {
        serde_json::Value::Array(
            items
                .iter()
                .map(|s| serde_json::Value::String((*s).to_owned()))
                .collect(),
        )
    }

    #[test]
    fn create_list_get_round_trip() {
        let store = store();
        let note = store
            .create("device-1", "Meeting", Some("folder-a"), false)
            .expect("create");
        assert_eq!(note.title, "Meeting");
        assert_eq!(note.revision, 1);
        assert_eq!(note.current_drawing_revision, 0);

        let list = store.list("device-1").expect("list");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0], note);

        let fetched = store
            .get("device-1", &note.id)
            .expect("get")
            .expect("present");
        assert_eq!(fetched, note);
        assert!(store.get("device-2", &note.id).expect("get").is_none());
    }

    #[test]
    fn patch_requires_the_quoted_revision() {
        let store = store();
        let note = store.create("device-1", "T", None, false).expect("create");

        // Wrong If-Match value (revision 1, but we present "2").
        assert!(matches!(
            store.patch("device-1", &note.id, "\"2\"", Some("New"), None, None),
            Ok(PatchOutcome::RevisionMismatch)
        ));

        let updated = match store
            .patch("device-1", &note.id, "\"1\"", Some("New"), None, Some(true))
            .expect("patch")
        {
            PatchOutcome::Updated(view) => view,
            other => panic!("expected Updated, got {other:?}"),
        };
        assert_eq!(updated.title, "New");
        assert!(updated.pinned);
        assert_eq!(updated.revision, 2);

        // Missing note is distinct from a revision clash.
        assert!(matches!(
            store.patch("device-1", "nope", "\"1\"", None, None, None),
            Ok(PatchOutcome::NoteNotFound)
        ));
    }

    #[test]
    fn delete_is_soft_and_hides_from_list_but_not_get() {
        let store = store();
        let note = store.create("device-1", "T", None, false).expect("create");
        assert!(store.delete("device-1", &note.id).expect("delete"));
        assert!(store.list("device-1").expect("list").is_empty());
        // get still returns it (matching legacy `_get_note_or_404`).
        assert!(store.get("device-1", &note.id).expect("get").is_some());
    }

    #[test]
    fn blob_upload_conflicts_on_revision_and_download_round_trips() {
        let store = store();
        let note = store.create("device-1", "T", None, false).expect("create");

        let up = match store
            .upload_blob(
                "device-1",
                &note.id,
                1,
                "hash123",
                42,
                "notes/x/rev-1.pkdrawing",
            )
            .expect("upload")
        {
            BlobUploadOutcome::Ok(view) => view,
            other => panic!("expected Ok, got {other:?}"),
        };
        assert_eq!(up.revision, 1);
        assert_eq!(up.content_hash, "hash123");

        // Same revision again refuses.
        assert!(matches!(
            store.upload_blob("device-1", &note.id, 1, "hash123", 42, "p"),
            Ok(BlobUploadOutcome::RevisionExists)
        ));

        let down = match store
            .download_blob("device-1", &note.id, 1)
            .expect("download")
        {
            BlobDownloadOutcome::Ok(view) => view,
            other => panic!("expected Ok, got {other:?}"),
        };
        assert_eq!(down.byte_size, 42);
        assert_eq!(down.storage_path, "notes/x/rev-1.pkdrawing");
        assert!(matches!(
            store.download_blob("device-1", &note.id, 2),
            Ok(BlobDownloadOutcome::BlobNotFound)
        ));
    }

    #[test]
    fn recognition_round_trips() {
        let store = store();
        let note = store.create("device-1", "T", None, false).expect("create");
        let langs = json_arr(&["en", "fr"]);
        let rec = match store
            .create_recognition(
                "device-1",
                &note.id,
                1,
                "vn_accurate",
                Some("1.0"),
                Some(&langs),
                "hello world",
                Some("hello world!"),
            )
            .expect("recognition")
        {
            RecognitionOutcome::Ok(view) => view,
            other => panic!("expected Ok, got {other:?}"),
        };
        assert_eq!(rec.engine, "vn_accurate");
        assert_eq!(rec.raw_text, "hello world");
        assert_eq!(rec.languages, Some(langs));
    }

    #[test]
    fn run_is_idempotent_and_cancellable() {
        let store = store();
        let note = store.create("device-1", "T", None, false).expect("create");

        let created = match store
            .create_run(
                "device-1",
                &note.id,
                "client-run-1",
                None,
                None,
                json_arr(&[]),
            )
            .expect("run")
        {
            RunCreateOutcome::Created(view) => view,
            other => panic!("expected Created, got {other:?}"),
        };
        assert_eq!(created.status, "queued");
        assert_eq!(created.requested_directives, json_arr(&[]));

        // Same clientRunId returns the existing run, not a second one.
        let again = match store
            .create_run(
                "device-1",
                &note.id,
                "client-run-1",
                None,
                None,
                json_arr(&[]),
            )
            .expect("run")
        {
            RunCreateOutcome::Existing(view) => view,
            other => panic!("expected Existing, got {other:?}"),
        };
        assert_eq!(again.id, created.id);

        // Events start empty; cancel is idempotent.
        let events = store
            .run_events("device-1", &created.id, None, 100)
            .expect("events")
            .expect("owned");
        assert!(events.is_empty());
        let cancelled = store
            .cancel_run("device-1", &created.id)
            .expect("cancel")
            .expect("present");
        assert_eq!(cancelled.status, "cancelled");
        assert!(cancelled.completed_at.is_some());
        let again = store
            .cancel_run("device-1", &created.id)
            .expect("cancel")
            .expect("present");
        assert_eq!(again.status, "cancelled");

        // A run does not belong to another device.
        assert!(
            store
                .get_run("device-2", &created.id)
                .expect("get")
                .is_none()
        );
    }
}
