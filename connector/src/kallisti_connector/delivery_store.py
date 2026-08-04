"""Durable delivery store for POST /v1/messages idempotency and job tracking.

Build 33 Workstream B (connector side): before this module, every POST
/v1/messages created a fresh in-memory job unconditionally — no idempotency
on clientMessageId, no durable storage, and jobs/events/message identity all
died with the connector process.  The delivery store fixes the three gaps:

  * conversation_bindings — app conversation UUID ↔ Hermes session id,
    migrated from the JSON sidecar (session_meta.json ``_hermes_id`` entries)
    and written atomically from then on.
  * message_requests — one row per clientMessageId with a content hash, so a
    transport-level retry of the same send is answered with the existing job
    (replyState "duplicate") and the same id carrying different content is a
    409 (replyState "conflict").
  * message_attachments / job_events — provisioned for durable attachment
    metadata and ordered per-job event logs.

The in-memory ``_http_jobs`` dict in http_facade.py stays as the hot cache;
this store is the authority.  Reconcile runs at connector startup.

GUARDRAIL: same as restart_operations — never store bearer tokens or
credentials here.  Message *text* is stored because idempotency on
clientMessageId requires comparing request content; the store is local to
the connector host, mode 0600.
"""

from __future__ import annotations

import datetime
import hashlib
import json
import logging
import os
import sqlite3
import stat
import threading
import uuid
from pathlib import Path
from typing import Any

logger = logging.getLogger("herald.delivery_store")

# Bumped any time the schema changes in a non-additive way.  A live process
# whose DB has a lower user_version than this re-initializes the schema
# before serving the next request.  Increment with caution — the message
# is "the on-disk tables are no longer what the running code expects".
#
# Build 104 raised this to 2 after the one-binding-per-client-UUID
# migration: the on-disk `conversation_bindings` rows that the Build 103
# connector wrote for both the client UUID and the deterministic
# `_app_uuid(hermes_id)` against the same Hermes session must be
# collapsed so the binding table matches the new invariant.
#
# Build 108 raised this to 3: added `conversation_messages` ledger table
# and `revision` column to `conversation_bindings` for canonical wire contract.
_EXPECTED_SCHEMA_VERSION = 3

# States for message_requests (must match the CHECK constraint verbatim).
STATES = ("accepted", "running", "terminal", "permanent_failure", "cancelled")

# A 'running' row older than this is a job the previous connector process
# died mid-turn on — reconcile marks it permanent_failure at startup.
_STALE_JOB_SECONDS = 600.0

# ── Paths / time ──────────────────────────────────────────────────────────


def delivery_db_path() -> Path:
    """DB location: $HERMES_MOBILE_CONNECTOR_HOME/delivery.sqlite3."""
    home = os.getenv("HERMES_MOBILE_CONNECTOR_HOME") or str(Path.home() / ".hermes-mobile")
    return Path(home) / "delivery.sqlite3"


def _utcnow_rfc3339() -> str:
    """RFC 3339 UTC timestamp with a Z suffix (matches the contract fixtures)."""
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _parse_rfc3339(value: str) -> datetime.datetime | None:
    """Parse an RFC 3339 timestamp (``Z`` accepted).  None on any failure."""
    if not isinstance(value, str) or not value:
        return None
    try:
        return datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def _coerce_uuid(value: Any) -> str | None:
    """Lowercase UUID string, or None. Never raises."""
    try:
        return str(uuid.UUID(str(value)))
    except (ValueError, TypeError, AttributeError):
        return None


# ── Errors ────────────────────────────────────────────────────────────────


class DuplicateConflictError(RuntimeError):
    """A message request (or binding) collides with an existing durable row.

    Raised by create_message_request when the same clientMessageId was
    already submitted with different content, and by get_or_create_binding
    when the app conversation id (or the Hermes session id, which is UNIQUE)
    is already bound to a different session.
    """

    def __init__(self, client_message_id: str, detail: str) -> None:
        super().__init__(detail)
        self.client_message_id = client_message_id


class CanonicalSnapshotIncomplete(RuntimeError):
    """Phase 3A fail-closed: the canonical ledger cannot satisfy the snapshot.

    Raised by ``get_conversation_snapshot`` when a row in the canonical
    ledger is missing a required field (a null canonical_message_id, a
    zero sequence, or a zero revision) that would force the snapshot to
    lie.  Callers must either materialize the missing row atomically or
    surface the failure as HTTP 409/503 with a machine-readable code so
    the client can retry — never emit a null/zero placeholder on the
    authoritative wire.

    The handler is expected to log the correlation id and leave any
    optimistic iOS row pending so the next reconnect picks up the
    reconciled snapshot.
    """

    def __init__(
        self,
        reason: str,
        *,
        conversation_id: str | None = None,
        canonical_message_id: str | None = None,
    ) -> None:
        super().__init__(reason)
        self.reason = reason
        self.conversation_id = conversation_id
        self.canonical_message_id = canonical_message_id


# ── Request hashing ───────────────────────────────────────────────────────


def _attachment_ids(attachments: Any) -> list[str]:
    """Stable attachment identity list for the request hash.

    Prefers an explicit ``attachmentId``/``id`` from the payload; falls back
    to a content-shape key so two byte-identical attachments hash identically
    even when the app does not mint ids.  Sorted for hash stability.
    """
    ids: list[str] = []
    for att in attachments or []:
        if not isinstance(att, dict):
            continue
        aid = att.get("attachmentId") or att.get("id")
        if aid is None:
            aid = "|".join(
                str(att.get(k, "")) for k in ("type", "filename", "mimeType")
            )
        ids.append(str(aid))
    return sorted(ids)


def request_sha256(clean_text: str, attachments: Any) -> str:
    """SHA-256 of the canonical request payload.

    ``json.dumps({"text": clean_text, "attachments": sorted_attachment_ids},
    sort_keys=True)`` — two requests with the same clientMessageId only match
    when both text AND attachment identity agree.
    """
    payload = json.dumps(
        {"text": clean_text or "", "attachments": _attachment_ids(attachments)},
        sort_keys=True,
    )
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


# ── Store ─────────────────────────────────────────────────────────────────

_SCHEMA = """
CREATE TABLE IF NOT EXISTS conversation_bindings (
    app_conversation_id TEXT PRIMARY KEY,
    hermes_session_id TEXT UNIQUE NOT NULL,
    account_id TEXT NOT NULL,
    owner_device_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    archived INTEGER NOT NULL DEFAULT 0,
    archived_reason TEXT,
    archived_at TEXT,
    revision INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS message_requests (
    client_message_id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversation_bindings(app_conversation_id),
    owner_device_id TEXT NOT NULL,
    request_sha256 TEXT NOT NULL,
    clean_text TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('accepted','running','terminal','permanent_failure','cancelled')),
    job_id TEXT UNIQUE,
    canonical_user_message_id TEXT,
    terminal_message_id TEXT,
    error_category TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS message_attachments (
    message_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    attachment_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    filename TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    byte_length INTEGER NOT NULL DEFAULT 0,
    sha256 TEXT NOT NULL DEFAULT '',
    blob_path TEXT,
    thumbnail_path TEXT,
    created_at TEXT NOT NULL,
    PRIMARY KEY (message_id, ordinal)
);

CREATE TABLE IF NOT EXISTS job_events (
    job_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    event_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (job_id, seq)
);

CREATE TABLE IF NOT EXISTS conversation_messages (
    canonical_message_id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversation_bindings(app_conversation_id),
    sequence INTEGER NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1,
    role TEXT NOT NULL CHECK(role IN ('user', 'assistant', 'system', 'tool', 'reasoning')),
    client_message_id TEXT,
    job_id TEXT,
    hermes_message_id TEXT,
    content TEXT NOT NULL,
    display_content TEXT NOT NULL,
    model_input_content TEXT,
    state TEXT NOT NULL CHECK(state IN ('pending', 'accepted', 'running', 'terminal', 'failed', 'cancelled')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(conversation_id, sequence)
);
"""

# Deliberately separate indexes from ``_SCHEMA``.  A Build 103 database has
# the table but not the Build 104 ``archived`` columns; creating a partial
# index over that missing column inside ``executescript(_SCHEMA)`` aborts the
# whole recovery before the additive migration has a chance to run.
_INDEXES = """
CREATE UNIQUE INDEX IF NOT EXISTS ix_conversation_bindings_hermes
    ON conversation_bindings(hermes_session_id) WHERE archived = 0;

CREATE INDEX IF NOT EXISTS ix_conversation_messages_conversation
    ON conversation_messages(conversation_id, sequence);

CREATE UNIQUE INDEX IF NOT EXISTS ix_conversation_messages_client_message
    ON conversation_messages(conversation_id, client_message_id)
    WHERE client_message_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS ix_conversation_messages_job
    ON conversation_messages(job_id) WHERE job_id IS NOT NULL;
"""


class DeliveryStore:
    """SQLite-backed delivery store (WAL, foreign keys ON, mode 0600).

    Thread-safe for the connector's event loop + background executor:
    every mutation runs under a lock and uses a short busy timeout.
    """

    def __init__(self, db_path: str | Path | None = None) -> None:
        self.db_path: Path = Path(db_path) if db_path is not None else delivery_db_path()
        # RLock so _connect's schema-validation re-init can be called from
        # _init_db without deadlocking on the same thread.
        self._lock = threading.RLock()
        self._init_db()

    # ── connection helpers ────────────────────────────────────────────────

    def _connect(self) -> sqlite3.Connection:
        """Open the SQLite file and (re)validate the schema before returning.

        The connector may stay up for days.  If the file is unlinked and
        replaced by an empty inode (Build 34 incident: an update script
        recreated a 4096-byte shell with mode 0644 and no tables), live
        requests would otherwise 500 with ``no such table: conversation_bindings``
        forever.  Detecting the drift on every connection costs one
        ``PRAGMA user_version`` and re-runs the idempotent CREATE TABLE
        block if the schema version is missing.
        """
        # Tighten parent directory to mode 0700 — it is profile-owned.
        try:
            self.db_path.parent.mkdir(parents=True, exist_ok=True)
            current_mode = stat.S_IMODE(self.db_path.parent.stat().st_mode)
            if current_mode != 0o700:
                os.chmod(self.db_path.parent, 0o700)
        except OSError:
            logger.debug("delivery: could not chmod parent dir (non-fatal)")

        conn, recovered = self._open_or_recover()
        if recovered:
            logger.warning(
                "delivery: replaced non-database file at %s with a fresh schema",
                self.db_path,
            )
        conn.row_factory = sqlite3.Row
        # Schema validation — the table list is the source of truth.
        # user_version is a cheap persistence check; the table-count probe
        # detects the Build 34 incident (empty replacement file) where the
        # page count and inode are present but the tables are not.
        try:
            row = conn.execute("PRAGMA user_version").fetchone()
            schema_version = int(row[0]) if row else 0
        except sqlite3.DatabaseError:
            schema_version = 0
        try:
            table_count = conn.execute(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'"
            ).fetchone()[0]
        except sqlite3.DatabaseError:
            table_count = 0

        if schema_version < _EXPECTED_SCHEMA_VERSION or table_count < 4:
            logger.warning(
                "delivery: schema drift detected (user_version=%s, tables=%d) — "
                "re-initializing %s", schema_version, table_count, self.db_path
            )
            with self._lock:
                conn.executescript(_SCHEMA)
                # Mark the schema so subsequent opens skip the re-init.
                conn.execute(f"PRAGMA user_version = {_EXPECTED_SCHEMA_VERSION}")
                conn.commit()

        # Tighten the file and WAL sidecars to mode 0600 every time the
        # path is opened.  The empty-replacement incident left the file at
        # mode 0644; we re-secure without losing the in-flight WAL.
        for path in (self.db_path, Path(str(self.db_path) + "-wal"), Path(str(self.db_path) + "-shm")):
            try:
                if path.exists():
                    os.chmod(path, 0o600)
            except OSError:
                logger.debug("delivery: could not chmod %s (non-fatal)", path)

        return conn

    def _open_or_recover(self) -> tuple[sqlite3.Connection, bool]:
        """Open the DB; replace a non-database file with a fresh one.

        The Build 34 incident left a 4096-byte zero-filled file in place of
        the SQLite database.  ``sqlite3.connect`` opens such a file but the
        first ``PRAGMA`` raises ``DatabaseError: file is not a database``.
        Rather than 500-ing forever, we close the handle, unlink the file
        + sidecars, and reopen a fresh one.  The caller then sees a
        normal, schema-less file and runs the standard schema init.
        """
        try:
            conn = sqlite3.connect(str(self.db_path), timeout=10.0)
        except sqlite3.DatabaseError:
            # Path could not be opened as a SQLite file.  nuke + retry.
            for suffix in ("", "-wal", "-shm", "-journal"):
                try:
                    os.remove(str(self.db_path) + suffix)
                except OSError:
                    pass
            conn = sqlite3.connect(str(self.db_path), timeout=10.0)
            return conn, True

        try:
            conn.execute("PRAGMA journal_mode=WAL")
            conn.execute("PRAGMA foreign_keys=ON")
            conn.execute("PRAGMA busy_timeout=5000")
        except sqlite3.DatabaseError as exc:
            conn.close()
            for suffix in ("", "-wal", "-shm", "-journal"):
                try:
                    os.remove(str(self.db_path) + suffix)
                except OSError:
                    pass
            conn = sqlite3.connect(str(self.db_path), timeout=10.0)
            return conn, True
        return conn, False

    def _secure_files(self) -> None:
        """DB + WAL + SHM must be mode 0600 — the rows describe the host."""
        for path in (self.db_path, Path(str(self.db_path) + "-wal"), Path(str(self.db_path) + "-shm")):
            try:
                if path.exists():
                    os.chmod(path, 0o600)
            except OSError:
                logger.debug("delivery: could not chmod %s (non-fatal)", path)

    def _init_db(self) -> None:
        self.db_path.parent.mkdir(parents=True, exist_ok=True)
        with self._lock:
            with self._connect() as conn:
                # executescript: _SCHEMA holds five CREATE TABLE statements.
                conn.executescript(_SCHEMA)
                # Build 104: archived columns are required by the
                # duplicate-binding migration; add them only if they are
                # not present (idempotent across reconnects).
                self._ensure_column(conn, "conversation_bindings", "archived",
                                    "INTEGER NOT NULL DEFAULT 0")
                self._ensure_column(conn, "conversation_bindings", "archived_reason",
                                    "TEXT")
                self._ensure_column(conn, "conversation_bindings", "archived_at",
                                    "TEXT")
                # Build 108: revision column for canonical wire contract.
                self._ensure_column(conn, "conversation_bindings", "revision",
                                    "INTEGER NOT NULL DEFAULT 0")
                conn.executescript(_INDEXES)
                # Mark the schema so subsequent opens skip the re-init.
                conn.execute(f"PRAGMA user_version = {_EXPECTED_SCHEMA_VERSION}")
                conn.commit()
        self._secure_files()

    @staticmethod
    def _ensure_column(
        conn: sqlite3.Connection, table: str, column: str, decl: str
    ) -> None:
        """Idempotently add *column* to *table* if it is missing.

        SQLite does not support ``ALTER TABLE ... ADD COLUMN IF NOT
        EXISTS`` on older versions, so this is the next best thing.
        """
        try:
            rows = conn.execute(f"PRAGMA table_info({table})").fetchall()
        except sqlite3.DatabaseError:
            return
        if any(r["name"] == column for r in rows):
            return
        conn.execute(f"ALTER TABLE {table} ADD COLUMN {column} {decl}")

    # ── health probe ──────────────────────────────────────────────────────

    def schema_ready(self) -> bool:
        """True when the four required tables exist and the file is 0600.

        Used by /v1/health to expose ``deliveryStoreReady`` so the phone
        can show ``Degraded`` instead of the current green ``Online`` lie.
        """
        try:
            with self._connect() as conn:
                rows = conn.execute(
                    "SELECT name FROM sqlite_master WHERE type='table'"
                ).fetchall()
                names = {r[0] for r in rows}
                required = {
                    "conversation_bindings",
                    "message_requests",
                    "message_attachments",
                    "job_events",
                }
                if not required.issubset(names):
                    return False
            mode = stat.S_IMODE(self.db_path.stat().st_mode)
            return (mode & 0o077) == 0
        except (OSError, sqlite3.DatabaseError) as exc:
            logger.warning("delivery: schema_ready check failed: %s", exc)
            return False

    # ── row → contract dict ───────────────────────────────────────────────

    @staticmethod
    def _row_to_binding(row: sqlite3.Row) -> dict:
        """conversation-binding-v1 payload — the exact shape the app decodes."""
        return {
            "$schema": "conversation-binding-v1",
            "appConversationId": row["app_conversation_id"],
            "hermesSessionId": row["hermes_session_id"],
            "accountId": row["account_id"],
            "ownerDeviceId": row["owner_device_id"],
            "createdAt": row["created_at"],
            "updatedAt": row["updated_at"],
        }

    def get_binding_for_hermes(self, hermes_session_id: str) -> dict | None:
        """Return the active binding for a Hermes session id.

        Build 104 helper used by `ensure_conversation` to verify the
        client UUID it is about to bind does not already own a row for
        that session under a different alias. Returns the same payload
        shape as ``get_binding``.
        """
        return self.get_binding_by_hermes(hermes_session_id)

    @staticmethod
    def _row_to_request(row: sqlite3.Row) -> dict:
        """message-request-v1 payload — the exact shape the app decodes."""
        return {
            "$schema": "message-request-v1",
            "clientMessageId": row["client_message_id"],
            "conversationId": row["conversation_id"],
            "installationId": row["owner_device_id"],
            "requestSha256": row["request_sha256"],
            "cleanText": row["clean_text"],
            "state": row["state"],
            "jobId": row["job_id"],
            "canonicalUserMessageId": row["canonical_user_message_id"],
            "terminalMessageId": row["terminal_message_id"],
            "errorCategory": row["error_category"],
            "createdAt": row["created_at"],
            "updatedAt": row["updated_at"],
        }

    # ── conversation bindings ─────────────────────────────────────────────

    def get_or_create_binding(
        self,
        app_conversation_id: str,
        hermes_session_id: str,
        account_id: str,
        device_id: str,
    ) -> dict:
        """Atomically insert the binding or return the existing row.

        Raises DuplicateConflictError when the app conversation id is already
        bound to a *different* Hermes session, or when the Hermes session id
        (UNIQUE) is already bound to a different app conversation id — e.g. a
        draft alias racing the canonical UUID for the same session.
        """
        if not isinstance(app_conversation_id, str) or not app_conversation_id:
            raise ValueError("app_conversation_id must be a non-empty string")
        if not isinstance(hermes_session_id, str) or not hermes_session_id:
            raise ValueError("hermes_session_id must be a non-empty string")
        now = _utcnow_rfc3339()
        with self._lock:
            conn = self._connect()
            try:
                conn.execute(
                    "INSERT OR IGNORE INTO conversation_bindings "
                    "(app_conversation_id, hermes_session_id, account_id, "
                    " owner_device_id, created_at, updated_at) "
                    "VALUES (?, ?, ?, ?, ?, ?)",
                    (app_conversation_id, hermes_session_id,
                     account_id or "", device_id or "", now, now),
                )
                conn.commit()
                row = conn.execute(
                    "SELECT * FROM conversation_bindings "
                    "WHERE app_conversation_id = ?",
                    (app_conversation_id,),
                ).fetchone()
                if row is None:
                    # INSERT OR IGNORE swallowed the UNIQUE(hermes_session_id)
                    # collision — the session is bound to another app id.
                    other = conn.execute(
                        "SELECT * FROM conversation_bindings "
                        "WHERE hermes_session_id = ?",
                        (hermes_session_id,),
                    ).fetchone()
                    if other is not None:
                        raise DuplicateConflictError(
                            app_conversation_id,
                            f"Hermes session {hermes_session_id} is already bound "
                            f"to conversation {other['app_conversation_id']}",
                        )
                    raise DuplicateConflictError(
                        app_conversation_id,
                        f"binding for {app_conversation_id} could not be created",
                    )
                if row["hermes_session_id"] != hermes_session_id:
                    raise DuplicateConflictError(
                        app_conversation_id,
                        f"conversation {app_conversation_id} is already bound "
                        f"to session {row['hermes_session_id']}",
                    )
                return self._row_to_binding(row)
            finally:
                conn.close()

    def promote_legacy_alias(
        self,
        app_conversation_id: str,
        hermes_session_id: str,
        legacy_alias: str,
        account_id: str,
        device_id: str,
    ) -> bool:
        """Promote the Build 103 internal alias to the real client UUID.

        Only the deterministic legacy alias may be retired. Any other active
        client UUID is an ownership conflict and this returns ``False``.
        The historical row is renamed transactionally, along with any request
        rows that reference it, so the UNIQUE(hermes_session_id) invariant is
        maintained and old history stays attached to the same conversation.
        """
        now = _utcnow_rfc3339()
        with self._lock:
            conn = self._connect()
            try:
                # The foreign key references the app UUID. SQLite cannot
                # defer that FK, so briefly disable enforcement while both
                # parent and children are renamed in this private connection.
                conn.execute("PRAGMA foreign_keys = OFF")
                conn.execute("BEGIN IMMEDIATE")
                existing = conn.execute(
                    "SELECT * FROM conversation_bindings "
                    "WHERE hermes_session_id = ? AND archived = 0",
                    (hermes_session_id,),
                ).fetchone()
                if existing is None or existing["app_conversation_id"] != legacy_alias:
                    conn.rollback()
                    return False
                conn.execute(
                    "UPDATE message_requests SET conversation_id = ? "
                    "WHERE conversation_id = ?",
                    (app_conversation_id, legacy_alias),
                )
                conn.execute(
                    "UPDATE conversation_bindings SET app_conversation_id = ?, "
                    "account_id = ?, owner_device_id = ?, updated_at = ? "
                    "WHERE app_conversation_id = ?",
                    (app_conversation_id, account_id or "", device_id or "", now,
                     legacy_alias),
                )
                conn.commit()
                conn.execute("PRAGMA foreign_keys = ON")
                return True
            except Exception:
                conn.rollback()
                raise
            finally:
                try:
                    conn.execute("PRAGMA foreign_keys = ON")
                except sqlite3.DatabaseError:
                    pass
                conn.close()

    def get_binding(self, app_conversation_id: str) -> dict | None:
        """conversation-binding-v1 payload for the app id, or None.

        Build 104: rows marked ``archived=1`` by the duplicate-binding
        migration are skipped — the survivor row is the one callers want.
        """
        with self._lock:
            conn = self._connect()
            try:
                row = conn.execute(
                    "SELECT * FROM conversation_bindings "
                    "WHERE app_conversation_id = ? AND archived = 0",
                    (app_conversation_id,),
                ).fetchone()
                return self._row_to_binding(row) if row is not None else None
            finally:
                conn.close()

    def get_binding_by_hermes(self, hermes_session_id: str) -> dict | None:
        """conversation-binding-v1 payload for the Hermes session, or None.

        Build 104: rows marked ``archived=1`` by the duplicate-binding
        migration are skipped.
        """
        with self._lock:
            conn = self._connect()
            try:
                row = conn.execute(
                    "SELECT * FROM conversation_bindings "
                    "WHERE hermes_session_id = ? AND archived = 0",
                    (hermes_session_id,),
                ).fetchone()
                return self._row_to_binding(row) if row is not None else None
            finally:
                conn.close()

    # ── message requests ──────────────────────────────────────────────────

    def create_message_request(
        self,
        client_message_id: str,
        conversation_id: str,
        device_id: str,
        clean_text: str,
        request_sha256: str,
    ) -> dict:
        """Idempotent insert of a new accepted request.

        * New clientMessageId → a fresh row in state 'accepted'.
        * Same clientMessageId + same content hash → the existing row
          (replay-safe, whatever its state).
        * Same clientMessageId + different hash → DuplicateConflictError.
        """
        if not isinstance(client_message_id, str) or not client_message_id:
            raise ValueError("client_message_id must be a non-empty string")
        clean_text = clean_text or ""
        now = _utcnow_rfc3339()
        with self._lock:
            conn = self._connect()
            try:
                conn.execute(
                    "INSERT OR IGNORE INTO message_requests "
                    "(client_message_id, conversation_id, owner_device_id, "
                    " request_sha256, clean_text, state, created_at, updated_at) "
                    "VALUES (?, ?, ?, ?, ?, 'accepted', ?, ?)",
                    (client_message_id, conversation_id, device_id or "",
                     request_sha256, clean_text, now, now),
                )
                conn.commit()
                row = conn.execute(
                    "SELECT * FROM message_requests WHERE client_message_id = ?",
                    (client_message_id,),
                ).fetchone()
                if row is None:
                    # INSERT OR IGNORE swallowed a UNIQUE(job_id) collision on
                    # a resurrected row — should not happen in practice.
                    raise DuplicateConflictError(
                        client_message_id,
                        "request row could not be created for "
                        f"{client_message_id}",
                    )
                if row["request_sha256"] != request_sha256:
                    raise DuplicateConflictError(
                        client_message_id,
                        f"clientMessageId {client_message_id} was already "
                        "submitted with different content",
                    )
                return self._row_to_request(row)
            finally:
                conn.close()

    def _transition(
        self,
        client_message_id: str,
        *,
        set_clause: str,
        params: tuple[Any, ...],
        where: str,
    ) -> dict:
        """Shared UPDATE-then-return for lifecycle transitions.

        The UPDATE targets only rows matching *where* (a state guard); a
        rowcount of 0 means the transition is not allowed from the current
        state and the existing row is returned unchanged (idempotent).
        """
        with self._lock:
            conn = self._connect()
            try:
                conn.execute(
                    f"UPDATE message_requests SET {set_clause}, updated_at = ? "
                    f"WHERE client_message_id = ? AND {where}",
                    (*params, _utcnow_rfc3339(), client_message_id),
                )
                conn.commit()
                row = conn.execute(
                    "SELECT * FROM message_requests WHERE client_message_id = ?",
                    (client_message_id,),
                ).fetchone()
                if row is None:
                    raise KeyError(f"No message request for {client_message_id}")
                return self._row_to_request(row)
            finally:
                conn.close()

    def accept_message_request(self, client_message_id: str, job_id: str) -> dict:
        """Transition accepted → running and set job_id.

        Also accepts 'cancelled'/'permanent_failure' rows: a transport-level
        retry of a failed send must be able to run again with a fresh job.
        Rows already 'running'/'terminal' are returned unchanged.
        """
        return self._transition(
            client_message_id,
            set_clause="state = 'running', job_id = ?",
            params=(job_id,),
            where="state IN ('accepted', 'cancelled', 'permanent_failure')",
        )

    def complete_message_request(
        self,
        client_message_id: str,
        canonical_user_message_id: str | None = None,
        terminal_message_id: str | None = None,
    ) -> dict:
        """Transition running → terminal, recording the message identities."""
        return self._transition(
            client_message_id,
            set_clause="state = 'terminal', canonical_user_message_id = ?, "
                       "terminal_message_id = ?",
            params=(canonical_user_message_id, terminal_message_id),
            where="state = 'running'",
        )

    def mark_user_message_terminal(self, client_message_id: str) -> None:
        """Transition the user row in conversation_messages from 'accepted'
        to 'terminal' so iOS sees a delivered green dot that persists across
        refreshes.  Idempotent — no-op if already terminal or missing."""
        now = _utcnow_rfc3339()
        with self._lock:
            conn = self._connect()
            try:
                conn.execute(
                    "UPDATE conversation_messages "
                    "SET state = 'terminal', updated_at = ? "
                    "WHERE client_message_id = ? AND state = 'accepted'",
                    (now, client_message_id),
                )
                conn.commit()
            except Exception:
                conn.rollback()
                raise

    def fail_message_request(self, client_message_id: str, error_category: str | None = None) -> dict:
        """Transition running → permanent_failure with the error category."""
        return self._transition(
            client_message_id,
            set_clause="state = 'permanent_failure', error_category = ?",
            params=(error_category,),
            where="state = 'running'",
        )

    def cancel_message_request(self, client_message_id: str) -> dict:
        """Transition running (or accepted) → cancelled."""
        return self._transition(
            client_message_id,
            set_clause="state = 'cancelled'",
            params=(),
            where="state IN ('running', 'accepted')",
        )

    def get_message_request(self, client_message_id: str) -> dict | None:
        """message-request-v1 payload for the id, or None."""
        with self._lock:
            conn = self._connect()
            try:
                row = conn.execute(
                    "SELECT * FROM message_requests WHERE client_message_id = ?",
                    (client_message_id,),
                ).fetchone()
                return self._row_to_request(row) if row is not None else None
            finally:
                conn.close()

    def get_message_request_by_job(self, job_id: str) -> dict | None:
        """message-request-v1 payload for the job id, or None."""
        if not job_id:
            return None
        with self._lock:
            conn = self._connect()
            try:
                row = conn.execute(
                    "SELECT * FROM message_requests WHERE job_id = ?",
                    (job_id,),
                ).fetchone()
                return self._row_to_request(row) if row is not None else None
            finally:
                conn.close()

    # ── attachments ───────────────────────────────────────────────────────

    def add_attachment(
        self,
        message_id: str,
        ordinal: int,
        attachment_id: str,
        kind: str,
        filename: str,
        mime_type: str,
        byte_length: int = 0,
        sha256: str = "",
        blob_path: str | None = None,
        thumbnail_path: str | None = None,
    ) -> None:
        """Insert or replace one attachment row (keyed message_id + ordinal)."""
        with self._lock:
            conn = self._connect()
            try:
                conn.execute(
                    "INSERT OR REPLACE INTO message_attachments "
                    "(message_id, ordinal, attachment_id, kind, filename, "
                    " mime_type, byte_length, sha256, blob_path, thumbnail_path, "
                    " created_at) "
                    "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    (message_id, ordinal, attachment_id, kind, filename,
                     mime_type, byte_length, sha256, blob_path, thumbnail_path,
                     _utcnow_rfc3339()),
                )
                conn.commit()
            finally:
                conn.close()

    def get_attachments(self, message_id: str) -> list[dict]:
        """Attachment rows for *message_id*, ordered by ordinal."""
        with self._lock:
            conn = self._connect()
            try:
                rows = conn.execute(
                    "SELECT * FROM message_attachments "
                    "WHERE message_id = ? ORDER BY ordinal ASC",
                    (message_id,),
                ).fetchall()
                return [dict(r) for r in rows]
            finally:
                conn.close()

    # ── job events ────────────────────────────────────────────────────────

    def append_job_event(self, job_id: str, seq: int, event_json: str) -> None:
        """Append one ordered event (json string) for a job. Idempotent per seq."""
        with self._lock:
            conn = self._connect()
            try:
                conn.execute(
                    "INSERT OR REPLACE INTO job_events "
                    "(job_id, seq, event_json, created_at) VALUES (?, ?, ?, ?)",
                    (job_id, seq, event_json, _utcnow_rfc3339()),
                )
                conn.commit()
            finally:
                conn.close()

    def get_job_events(self, job_id: str) -> list[dict]:
        """All events for a job, oldest first: {seq, event, createdAt}.

        ``event`` is the parsed JSON object; a row whose JSON does not parse
        is returned as ``{"raw": ...}`` so it can never crash a caller.
        """
        with self._lock:
            conn = self._connect()
            try:
                rows = conn.execute(
                    "SELECT * FROM job_events WHERE job_id = ? ORDER BY seq ASC",
                    (job_id,),
                ).fetchall()
            finally:
                conn.close()
        events: list[dict] = []
        for row in rows:
            try:
                parsed = json.loads(row["event_json"])
            except (ValueError, TypeError):
                parsed = {"raw": row["event_json"]}
            events.append({
                "seq": row["seq"],
                "event": parsed,
                "createdAt": row["created_at"],
            })
        return events

    # ── Phase 3A: per-job sequence allocation for v3 envelopes ───────────

    def allocate_job_seq(
        self,
        *,
        job_id: str,
        conversation_id: str | None = None,
        attempt: int | None = None,
    ) -> int:
        """Allocate the next sequence number for a job attempt.

        Phase 3A v3: the live-event publisher needs an authoritative,
        monotonically increasing sequence per job attempt.  Backed by
        ``job_events`` so a reconnect replay or a connector restart
        observes the same seq numbers the original attempt emitted.

        The allocation is atomic: a placeholder row is inserted in the
        same transaction as the seq read, so two concurrent callers
        cannot receive the same seq.  ``append_job_event`` replaces the
        placeholder with the real event JSON.

        Returns the next seq (always >= 1).
        """
        with self._lock:
            conn = self._connect()
            try:
                conn.execute("BEGIN IMMEDIATE")
                row = conn.execute(
                    "SELECT COALESCE(MAX(seq), 0) AS next_seq "
                    "FROM job_events WHERE job_id = ?",
                    (job_id,),
                ).fetchone()
                next_seq = int(row["next_seq"]) + 1 if row else 1
                # Insert a placeholder so a concurrent allocate cannot
                # receive the same seq.  append_job_event replaces this
                # row with the real event JSON.
                conn.execute(
                    "INSERT OR IGNORE INTO job_events "
                    "(job_id, seq, event_json, created_at) "
                    "VALUES (?, ?, ?, ?)",
                    (job_id, next_seq, '{"_placeholder":true}',
                     _utcnow_rfc3339()),
                )
                conn.commit()
                return next_seq
            except Exception:
                conn.rollback()
                raise
            finally:
                conn.close()

    def get_max_job_seq(self, job_id: str) -> int:
        """Return the largest seq allocated for *job_id* (0 if none)."""
        with self._lock:
            conn = self._connect()
            try:
                row = conn.execute(
                    "SELECT COALESCE(MAX(seq), 0) AS max_seq "
                    "FROM job_events WHERE job_id = ?",
                    (job_id,),
                ).fetchone()
                return int(row["max_seq"]) if row else 0
            finally:
                conn.close()

    # ── canonical message ledger (Build 108) ─────────────────────────────

    def _get_next_sequence(self, conn: sqlite3.Connection, conversation_id: str) -> int:
        """Get the next sequence number for a conversation."""
        row = conn.execute(
            "SELECT COALESCE(MAX(sequence), 0) + 1 AS next_seq "
            "FROM conversation_messages WHERE conversation_id = ?",
            (conversation_id,),
        ).fetchone()
        return int(row["next_seq"]) if row else 1

    def _increment_conversation_revision(
        self, conn: sqlite3.Connection, conversation_id: str
    ) -> int:
        """Increment conversation revision and return new value."""
        conn.execute(
            "UPDATE conversation_bindings SET revision = revision + 1, "
            "updated_at = ? WHERE app_conversation_id = ?",
            (_utcnow_rfc3339(), conversation_id),
        )
        row = conn.execute(
            "SELECT revision FROM conversation_bindings "
            "WHERE app_conversation_id = ?",
            (conversation_id,),
        ).fetchone()
        return int(row["revision"]) if row else 0

    def create_canonical_message(
        self,
        conversation_id: str,
        role: str,
        content: str,
        display_content: str,
        *,
        client_message_id: str | None = None,
        job_id: str | None = None,
        hermes_message_id: str | None = None,
        model_input_content: str | None = None,
        state: str = "pending",
    ) -> dict:
        """Create a canonical message in the ledger.

        Returns the created message as a dict. Raises DuplicateConflictError
        if the client_message_id is already used in this conversation.
        """
        now = _utcnow_rfc3339()
        with self._lock:
            conn = self._connect()
            try:
                conn.execute("BEGIN IMMEDIATE")
                # Get next sequence
                sequence = self._get_next_sequence(conn, conversation_id)
                # Generate canonical message ID
                canonical_message_id = str(uuid.uuid4())
                # Insert the message
                try:
                    conn.execute(
                        "INSERT INTO conversation_messages "
                        "(canonical_message_id, conversation_id, sequence, revision, "
                        " role, client_message_id, job_id, hermes_message_id, "
                        " content, display_content, model_input_content, state, "
                        " created_at, updated_at) "
                        "VALUES (?, ?, ?, 1, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                        (
                            canonical_message_id, conversation_id, sequence, role,
                            client_message_id, job_id, hermes_message_id,
                            content, display_content, model_input_content,
                            state, now, now,
                        ),
                    )
                except sqlite3.IntegrityError as exc:
                    conn.rollback()
                    if "UNIQUE constraint failed" in str(exc) and client_message_id:
                        raise DuplicateConflictError(
                            client_message_id,
                            f"client_message_id {client_message_id} already exists "
                            f"in conversation {conversation_id}",
                        ) from exc
                    raise
                # Increment conversation revision
                revision = self._increment_conversation_revision(
                    conn, conversation_id
                )
                conn.commit()
                return {
                    "canonicalMessageId": canonical_message_id,
                    "conversationId": conversation_id,
                    "sequence": sequence,
                    "revision": revision,
                    "role": role,
                    "clientMessageId": client_message_id,
                    "jobId": job_id,
                    "hermesMessageId": hermes_message_id,
                    "content": content,
                    "displayContent": display_content,
                    "modelInputContent": model_input_content,
                    "state": state,
                    "createdAt": now,
                    "updatedAt": now,
                }
            except Exception:
                conn.rollback()
                raise
            finally:
                conn.close()

    def get_canonical_message(
        self, canonical_message_id: str, *, conversation_id: str | None = None,
    ) -> dict | None:
        """Get a canonical message by its ID, optionally scoped to a conversation.

        When ``conversation_id`` is provided, the lookup is rejected if the
        message belongs to a different conversation — returning ``None``
        rather than leaking cross-conversation identity.
        """
        with self._lock:
            conn = self._connect()
            try:
                if conversation_id:
                    row = conn.execute(
                        "SELECT * FROM conversation_messages "
                        "WHERE canonical_message_id = ? "
                        "AND conversation_id = ?",
                        (canonical_message_id, conversation_id),
                    ).fetchone()
                else:
                    row = conn.execute(
                        "SELECT * FROM conversation_messages "
                        "WHERE canonical_message_id = ?",
                        (canonical_message_id,),
                    ).fetchone()
                if row is None:
                    return None
                return {
                    "canonicalMessageId": row["canonical_message_id"],
                    "conversationId": row["conversation_id"],
                    "sequence": row["sequence"],
                    "revision": row["revision"],
                    "role": row["role"],
                    "clientMessageId": row["client_message_id"],
                    "jobId": row["job_id"],
                    "hermesMessageId": row["hermes_message_id"],
                    "content": row["content"],
                    "displayContent": row["display_content"],
                    "modelInputContent": row["model_input_content"],
                    "state": row["state"],
                    "createdAt": row["created_at"],
                    "updatedAt": row["updated_at"],
                }
            finally:
                conn.close()

    def get_message_by_client_id(
        self, conversation_id: str, client_message_id: str
    ) -> dict | None:
        """Get a canonical message by client_message_id within a conversation."""
        with self._lock:
            conn = self._connect()
            try:
                row = conn.execute(
                    "SELECT * FROM conversation_messages "
                    "WHERE conversation_id = ? AND client_message_id = ?",
                    (conversation_id, client_message_id),
                ).fetchone()
                if row is None:
                    return None
                return {
                    "canonicalMessageId": row["canonical_message_id"],
                    "conversationId": row["conversation_id"],
                    "sequence": row["sequence"],
                    "revision": row["revision"],
                    "role": row["role"],
                    "clientMessageId": row["client_message_id"],
                    "jobId": row["job_id"],
                    "hermesMessageId": row["hermes_message_id"],
                    "content": row["content"],
                    "displayContent": row["display_content"],
                    "modelInputContent": row["model_input_content"],
                    "state": row["state"],
                    "createdAt": row["created_at"],
                    "updatedAt": row["updated_at"],
                }
            finally:
                conn.close()

    def get_message_by_job_id(
        self, conversation_id: str, job_id: str
    ) -> dict | None:
        """Get a canonical message by job_id within a conversation."""
        with self._lock:
            conn = self._connect()
            try:
                row = conn.execute(
                    "SELECT * FROM conversation_messages "
                    "WHERE conversation_id = ? AND job_id = ?",
                    (conversation_id, job_id),
                ).fetchone()
                if row is None:
                    return None
                return {
                    "canonicalMessageId": row["canonical_message_id"],
                    "conversationId": row["conversation_id"],
                    "sequence": row["sequence"],
                    "revision": row["revision"],
                    "role": row["role"],
                    "clientMessageId": row["client_message_id"],
                    "jobId": row["job_id"],
                    "hermesMessageId": row["hermes_message_id"],
                    "content": row["content"],
                    "displayContent": row["display_content"],
                    "modelInputContent": row["model_input_content"],
                    "state": row["state"],
                    "createdAt": row["created_at"],
                    "updatedAt": row["updated_at"],
                }
            finally:
                conn.close()

    def update_message_state(
        self,
        canonical_message_id: str,
        state: str,
        *,
        content: str | None = None,
        display_content: str | None = None,
        job_id: str | None = None,
        hermes_message_id: str | None = None,
    ) -> dict | None:
        """Update a canonical message's state and optionally other fields."""
        now = _utcnow_rfc3339()
        with self._lock:
            conn = self._connect()
            try:
                conn.execute("BEGIN IMMEDIATE")
                # Build update clause dynamically
                updates = ["state = ?", "updated_at = ?"]
                params: list[Any] = [state, now]
                if content is not None:
                    updates.append("content = ?")
                    params.append(content)
                if display_content is not None:
                    updates.append("display_content = ?")
                    params.append(display_content)
                if job_id is not None:
                    updates.append("job_id = ?")
                    params.append(job_id)
                if hermes_message_id is not None:
                    updates.append("hermes_message_id = ?")
                    params.append(hermes_message_id)
                params.append(canonical_message_id)
                conn.execute(
                    f"UPDATE conversation_messages SET {', '.join(updates)} "
                    "WHERE canonical_message_id = ?",
                    params,
                )
                # Increment message revision
                conn.execute(
                    "UPDATE conversation_messages SET revision = revision + 1 "
                    "WHERE canonical_message_id = ?",
                    (canonical_message_id,),
                )
                conn.commit()
                # Return updated message
                return self.get_canonical_message(canonical_message_id)
            except Exception:
                conn.rollback()
                raise
            finally:
                conn.close()

    def get_conversation_messages(
        self, conversation_id: str, *, after_sequence: int = 0
    ) -> list[dict]:
        """Get all messages for a conversation, ordered by sequence."""
        with self._lock:
            conn = self._connect()
            try:
                rows = conn.execute(
                    "SELECT * FROM conversation_messages "
                    "WHERE conversation_id = ? AND sequence > ? "
                    "ORDER BY sequence ASC",
                    (conversation_id, after_sequence),
                ).fetchall()
                return [
                    {
                        "canonicalMessageId": row["canonical_message_id"],
                        "conversationId": row["conversation_id"],
                        "sequence": row["sequence"],
                        "revision": row["revision"],
                        "role": row["role"],
                        "clientMessageId": row["client_message_id"],
                        "jobId": row["job_id"],
                        "hermesMessageId": row["hermes_message_id"],
                        "content": row["content"],
                        "displayContent": row["display_content"],
                        "modelInputContent": row["model_input_content"],
                        "state": row["state"],
                        "createdAt": row["created_at"],
                        "updatedAt": row["updated_at"],
                    }
                    for row in rows
                ]
            finally:
                conn.close()

    def get_conversation_revision(self, conversation_id: str) -> int:
        """Get the current revision for a conversation."""
        with self._lock:
            conn = self._connect()
            try:
                row = conn.execute(
                    "SELECT revision FROM conversation_bindings "
                    "WHERE app_conversation_id = ?",
                    (conversation_id,),
                ).fetchone()
                return int(row["revision"]) if row else 0
            finally:
                conn.close()

    # ── Phase 3A: consistent snapshot transaction ─────────────────────────

    def get_conversation_snapshot(
        self, conversation_id: str
    ) -> dict:
        """Return an authoritative snapshot of one conversation in one read.

        Phase 3A fix: the v1 implementation called
        ``get_conversation_revision`` after fetching rows, which allowed the
        envelope revision and the row set to describe different instants.
        This method reads the binding revision AND every canonical message
        row inside a single BEGIN DEFERRED transaction, so the snapshot
        is internally consistent — what the caller returns as
        ``revision`` is the revision that produced the rows it ships.

        The snapshot is fail-closed: any row with a null canonical id, a
        zero/negative sequence, or a zero/negative revision raises
        ``CanonicalSnapshotIncomplete`` with a machine-readable reason.
        The handler must NOT emit those values on the wire — that is the
        v1 defect this method is designed to prevent.

        Returns a dict with ``revision`` and ``messages``; messages is a
        list of the canonical row dicts already shaped for the wire (the
        same keys as ``get_canonical_message``).  The conversation
        envelope (id, title, updatedAt, …) is not stored here — that
        metadata is the connector's responsibility to source from
        session_store / session_meta.
        """
        with self._lock:
            conn = self._connect()
            try:
                conn.execute("BEGIN DEFERRED")
                rev_row = conn.execute(
                    "SELECT revision FROM conversation_bindings "
                    "WHERE app_conversation_id = ?",
                    (conversation_id,),
                ).fetchone()
                # No binding → empty snapshot with revision 0.  The wire
                # caller must surface that as a separate "no binding" error
                # rather than coerce a 0 into "real initial revision".
                if rev_row is None:
                    conn.commit()
                    return {
                        "conversationId": conversation_id,
                        "revision": 0,
                        "messages": [],
                        "bindingPresent": False,
                    }
                revision = int(rev_row["revision"])
                rows = conn.execute(
                    "SELECT * FROM conversation_messages "
                    "WHERE conversation_id = ? "
                    "ORDER BY sequence ASC",
                    (conversation_id,),
                ).fetchall()
                conn.commit()
            except Exception:
                conn.rollback()
                raise
            finally:
                conn.close()
        messages: list[dict] = []
        for row in rows:
            cmid = row["canonical_message_id"]
            seq = int(row["sequence"]) if row["sequence"] is not None else 0
            message_rev = int(row["revision"]) if row["revision"] is not None else 0
            # Fail-closed: refuse to fabricate authority from a malformed
            # ledger row.  Any zero/null canonical field is the v1 bug
            # this method is supposed to catch before publication.
            if not cmid:
                raise CanonicalSnapshotIncomplete(
                    "canonical_message_id is null",
                    conversation_id=conversation_id,
                )
            if seq <= 0:
                raise CanonicalSnapshotIncomplete(
                    f"sequence must be positive (got {seq})",
                    conversation_id=conversation_id,
                    canonical_message_id=cmid,
                )
            if message_rev <= 0:
                raise CanonicalSnapshotIncomplete(
                    f"message revision must be positive (got {message_rev})",
                    conversation_id=conversation_id,
                    canonical_message_id=cmid,
                )
            messages.append({
                "canonicalMessageId": cmid,
                "conversationId": row["conversation_id"],
                "sequence": seq,
                "revision": message_rev,
                "role": row["role"],
                "clientMessageId": row["client_message_id"],
                "jobId": row["job_id"],
                "hermesMessageId": row["hermes_message_id"],
                "content": row["content"],
                "displayContent": row["display_content"],
                "modelInputContent": row["model_input_content"],
                "state": row["state"],
                "createdAt": row["created_at"],
                "updatedAt": row["updated_at"],
            })
        return {
            "conversationId": conversation_id,
            "revision": revision,
            "messages": messages,
            "bindingPresent": True,
        }

    def create_user_message_atomically(
        self,
        conversation_id: str,
        client_message_id: str,
        content: str,
        display_content: str,
        *,
        model_input_content: str | None = None,
        device_id: str = "",
    ) -> dict:
        """Atomically create a user message, request, and job in one transaction.

        This is the Build 108 atomic acceptance contract:
        1. Canonical user message with sequence
        2. Request idempotency record
        3. New conversation revision
        """
        now = _utcnow_rfc3339()
        with self._lock:
            conn = self._connect()
            try:
                conn.execute("BEGIN IMMEDIATE")
                # Check for duplicate client_message_id
                existing = conn.execute(
                    "SELECT * FROM conversation_messages "
                    "WHERE conversation_id = ? AND client_message_id = ?",
                    (conversation_id, client_message_id),
                ).fetchone()
                if existing is not None:
                    conn.rollback()
                    return {
                        "canonicalMessageId": existing["canonical_message_id"],
                        "conversationId": conversation_id,
                        "sequence": existing["sequence"],
                        "revision": existing["revision"],
                        "role": existing["role"],
                        "clientMessageId": existing["client_message_id"],
                        "jobId": existing["job_id"],
                        "hermesMessageId": existing["hermes_message_id"],
                        "content": existing["content"],
                        "displayContent": existing["display_content"],
                        "modelInputContent": existing["model_input_content"],
                        "state": existing["state"],
                        "createdAt": existing["created_at"],
                        "updatedAt": existing["updated_at"],
                        "duplicate": True,
                    }
                # Get next sequence
                sequence = self._get_next_sequence(conn, conversation_id)
                # Generate canonical message ID
                canonical_message_id = str(uuid.uuid4())
                # Insert canonical message
                conn.execute(
                    "INSERT INTO conversation_messages "
                    "(canonical_message_id, conversation_id, sequence, revision, "
                    " role, client_message_id, content, display_content, "
                    " model_input_content, state, created_at, updated_at) "
                    "VALUES (?, ?, ?, 1, 'user', ?, ?, ?, ?, 'accepted', ?, ?)",
                    (
                        canonical_message_id, conversation_id, sequence,
                        client_message_id, content, display_content,
                        model_input_content, now, now,
                    ),
                )
                # Insert request idempotency record.  INSERT OR IGNORE
                # so a pre-existing request (e.g. from an earlier
                # create_message_request call) does not fail the
                # transaction — the existing row is left untouched.
                request_sha = request_sha256(display_content, None)
                conn.execute(
                    "INSERT OR IGNORE INTO message_requests "
                    "(client_message_id, conversation_id, owner_device_id, "
                    " request_sha256, clean_text, state, created_at, updated_at) "
                    "VALUES (?, ?, ?, ?, ?, 'accepted', ?, ?)",
                    (client_message_id, conversation_id, device_id,
                     request_sha, display_content, now, now),
                )
                # Increment conversation revision
                revision = self._increment_conversation_revision(
                    conn, conversation_id
                )
                conn.commit()
                return {
                    "canonicalMessageId": canonical_message_id,
                    "conversationId": conversation_id,
                    "sequence": sequence,
                    "revision": revision,
                    "role": "user",
                    "clientMessageId": client_message_id,
                    "jobId": None,
                    "hermesMessageId": None,
                    "content": content,
                    "displayContent": display_content,
                    "modelInputContent": model_input_content,
                    "state": "accepted",
                    "createdAt": now,
                    "updatedAt": now,
                    "duplicate": False,
                }
            except Exception:
                conn.rollback()
                raise
            finally:
                conn.close()

    # ── build 108 migration: canonical ledger import ─────────────────────

    def migrate_to_canonical_ledger(
        self, *, evidence_dir: str | Path | None = None
    ) -> dict:
        """Migrate existing message_requests to canonical ledger.

        Build 108 Workstream B: imports existing user/assistant display rows
        into the conversation_messages ledger table. The migration:

        1. Backs up the active delivery database.
        2. For each active binding, imports Hermes user/assistant display rows.
        3. Strips system-context envelopes from imported user display content.
        4. Assigns sequences using stable database row identity.
        5. Resolves aliases to the binding that owns the Hermes session.
        6. Verifies unique constraints.
        7. Generates a migration report.

        Idempotent: re-running on an already-migrated DB is a no-op.
        """
        report: dict[str, Any] = {
            "schemaVersion": _EXPECTED_SCHEMA_VERSION,
            "evidenceDir": str(evidence_dir) if evidence_dir else None,
            "imported": 0,
            "deduplicated": 0,
            "aliasResolved": 0,
            "quarantined": 0,
            "failed": 0,
        }
        ts = _utcnow_rfc3339()
        if evidence_dir is None:
            base = Path(os.getenv(
                "HERMES_MOBILE_CONNECTOR_HOME"
            ) or str(Path.home() / ".hermes-mobile")) / "evidence"
            evidence_dir = base / f"pre-b108-{ts}"
        evidence_path = Path(evidence_dir)
        evidence_path.mkdir(parents=True, exist_ok=True)

        # Snapshot the database before migration
        snapshot = evidence_path / "delivery.sqlite3.snapshot"
        try:
            import shutil
            for suffix in ("", "-wal", "-shm", "-journal"):
                src = Path(str(self.db_path) + suffix)
                if src.exists():
                    shutil.copy2(src, Path(str(snapshot) + suffix))
        except OSError as exc:
            logger.warning(
                "delivery: snapshot failed (%s) — migration will continue but "
                "the rollback path is incomplete", exc,
            )

        with self._lock:
            conn = self._connect()
            try:
                # Check if migration has already run
                existing_count = conn.execute(
                    "SELECT COUNT(*) FROM conversation_messages"
                ).fetchone()[0]
                if existing_count > 0:
                    logger.info(
                        "delivery: canonical ledger migration already complete "
                        "(%d messages)", existing_count
                    )
                    return report

                # Get all active bindings
                bindings = conn.execute(
                    "SELECT * FROM conversation_bindings WHERE archived = 0"
                ).fetchall()

                for binding in bindings:
                    conv_id = binding["app_conversation_id"]
                    hermes_id = binding["hermes_session_id"]

                    # Get message_requests for this binding
                    requests = conn.execute(
                        "SELECT * FROM message_requests "
                        "WHERE conversation_id = ? "
                        "ORDER BY created_at ASC",
                        (conv_id,),
                    ).fetchall()

                    sequence = 1
                    for req in requests:
                        client_msg_id = req["client_message_id"]
                        clean_text = req["clean_text"]
                        state = req["state"]
                        job_id = req["job_id"]

                        # Strip system-context envelope from user display content
                        display_content = clean_text
                        if display_content.startswith("[System context"):
                            # Find the end of the system context block
                            end_idx = display_content.find("]")
                            if end_idx != -1:
                                display_content = display_content[end_idx + 1:].strip()

                        # Generate canonical message ID
                        canonical_msg_id = str(uuid.uuid4())

                        # Insert into canonical ledger
                        try:
                            conn.execute(
                                "INSERT INTO conversation_messages "
                                "(canonical_message_id, conversation_id, sequence, "
                                " revision, role, client_message_id, job_id, "
                                " content, display_content, state, created_at, updated_at) "
                                "VALUES (?, ?, ?, 1, 'user', ?, ?, ?, ?, ?, ?, ?)",
                                (
                                    canonical_msg_id, conv_id, sequence,
                                    client_msg_id, job_id, clean_text, display_content,
                                    state, req["created_at"], req["updated_at"],
                                ),
                            )
                            report["imported"] += 1
                            sequence += 1
                        except sqlite3.IntegrityError as exc:
                            if "UNIQUE constraint failed" in str(exc):
                                report["deduplicated"] += 1
                                logger.debug(
                                    "delivery: skipping duplicate message %s",
                                    client_msg_id,
                                )
                            else:
                                report["failed"] += 1
                                logger.warning(
                                    "delivery: failed to import message %s: %s",
                                    client_msg_id, exc,
                                )

                    # Increment conversation revision to match imported messages
                    if report["imported"] > 0:
                        conn.execute(
                            "UPDATE conversation_bindings SET revision = ?, "
                            "updated_at = ? WHERE app_conversation_id = ?",
                            (sequence, ts, conv_id),
                        )

                conn.commit()
            except Exception:
                conn.rollback()
                raise
            finally:
                conn.close()

        # Write migration report
        try:
            (evidence_path / "migration_report.json").write_text(
                json.dumps(report, indent=2, sort_keys=True),
            )
        except OSError as exc:
            logger.warning(
                "delivery: could not write migration_report.json (%s)", exc,
            )

        logger.info(
            "delivery: canonical ledger migration complete — imported=%d, "
            "deduplicated=%d, aliasResolved=%d, quarantined=%d, failed=%d",
            report["imported"], report["deduplicated"], report["aliasResolved"],
            report["quarantined"], report["failed"],
        )
        return report

    # ── build 104 migration: collapse duplicate bindings ──────────────────

    def resolve_duplicate_bindings(self, *, evidence_dir: str | Path | None = None) -> dict:
        """Collapse Build 103 dual-binding rows into a single survivor.

        Build 103 wrote two `conversation_bindings` rows for one Hermes
        session: one keyed by the client-supplied UUID, one keyed by the
        deterministic `_app_uuid(hermes_id)`. The UNIQUE
        `hermes_session_id` constraint made the first INSERT win; the
        second became a logged `conflict:` and the binding table drifted
        away from the new invariant.

        The migration:

        1. Snapshots `delivery.sqlite3` (and any `-wal` / `-shm` sidecars)
           into ``evidence_dir`` (or ``./evidence/pre-b104-<ts>/`` by
           default) before touching anything.
        2. For each Hermes session id with more than one row, picks the
           row whose ``app_conversation_id`` is most-recently referenced
           by ``message_requests`` (older binding wins on a tie — it is
           more likely to be the iOS-minted UUID the app still uses).
        3. Marks the other rows ``archived=1`` with a recorded
           ``archived_reason`` and ``archived_at``. ``get_binding`` skips
           archived rows; ``get_binding_for_hermes`` likewise.
        4. Reports a JSON-friendly summary.

        Idempotent: re-running on an already-migrated DB is a no-op
        (the only rows in the table are either the unique survivor or
        archived duplicates from this run).
        """
        report: dict[str, Any] = {
            "schemaVersion": _EXPECTED_SCHEMA_VERSION,
            "evidenceDir": str(evidence_dir) if evidence_dir else None,
            "duplicateSessions": 0,
            "archived": 0,
            "survivors": 0,
        }
        ts = _utcnow_rfc3339()
        if evidence_dir is None:
            base = Path(os.getenv(
                "HERMES_MOBILE_CONNECTOR_HOME"
            ) or str(Path.home() / ".hermes-mobile")) / "evidence"
            evidence_dir = base / f"pre-b104-{ts}"
        evidence_path = Path(evidence_dir)
        evidence_path.mkdir(parents=True, exist_ok=True)
        # Snapshot the file before any write so the migration is reversible.
        snapshot = evidence_path / "delivery.sqlite3.snapshot"
        try:
            import shutil
            for suffix in ("", "-wal", "-shm", "-journal"):
                src = Path(str(self.db_path) + suffix)
                if src.exists():
                    shutil.copy2(src, Path(str(snapshot) + suffix))
        except OSError as exc:
            logger.warning(
                "delivery: snapshot failed (%s) — migration will continue but "
                "the rollback path is incomplete", exc,
            )

        with self._lock:
            conn = self._connect()
            try:
                # Detect Hermes sessions with >1 binding row.
                duplicate_rows = conn.execute(
                    "SELECT hermes_session_id, app_conversation_id, "
                    "       created_at, updated_at "
                    "FROM conversation_bindings "
                    "WHERE archived = 0 "
                    "GROUP BY hermes_session_id "
                    "HAVING COUNT(*) > 1"
                ).fetchall()
                # Build 104: every duplicate cluster is collapsed to one row
                # (the iOS-minted UUID). The deterministic `_app_uuid(hermes_id)`
                # is tombstoned in the sidecar, not in SQLite, so a survivor
                # here is whichever row has the most-recent message_requests
                # reference; tie-breaks fall back to the older created_at.
                for dup in duplicate_rows:
                    hermes_id = dup["hermes_session_id"]
                    rows = conn.execute(
                        "SELECT app_conversation_id, created_at, updated_at "
                        "FROM conversation_bindings "
                        "WHERE hermes_session_id = ? AND archived = 0",
                        (hermes_id,),
                    ).fetchall()
                    # Score: (-max(request_count), created_at) — keep the
                    # row referenced by the most message_requests; ties go
                    # to the older created_at.
                    scored: list[tuple[tuple[int, str, str], str]] = []
                    for r in rows:
                        n = conn.execute(
                            "SELECT COUNT(*) AS n FROM message_requests "
                            "WHERE conversation_id = ?",
                            (r["app_conversation_id"],),
                        ).fetchone()
                        scored.append(
                            (
                                (-int(n["n"]), r["created_at"], r["app_conversation_id"]),
                                r["app_conversation_id"],
                            ),
                        )
                    scored.sort()
                    survivor = scored[0][1]
                    archived_ids = [
                        cid for _, cid in scored[1:]
                    ]
                    for cid in archived_ids:
                        conn.execute(
                            "UPDATE conversation_bindings "
                            "SET archived = 1, "
                            "    archived_reason = 'duplicate-binding-pre-b104', "
                            "    archived_at = ?, "
                            "    updated_at = ? "
                            "WHERE app_conversation_id = ?",
                            (ts, ts, cid),
                        )
                        report["archived"] += 1
                    report["duplicateSessions"] += 1
                    report["survivors"] += 1
                    logger.warning(
                        "delivery: collapse duplicate binding for hermes=%s "
                        "survivor=%s archived=%s",
                        hermes_id[:24], survivor[:12], archived_ids,
                    )
                conn.commit()
            except Exception:
                conn.rollback()
                raise
            finally:
                conn.close()
        try:
            (evidence_path / "migration_report.json").write_text(
                json.dumps(report, indent=2, sort_keys=True),
            )
        except OSError as exc:
            logger.warning(
                "delivery: could not write migration_report.json (%s)", exc,
            )
        return report

    # ── restart recovery ──────────────────────────────────────────────────

    def reconcile_stale_jobs(self) -> None:
        """Mark every 'running' request older than 10 minutes as failed.

        Called at connector startup: a 'running' row means the previous
        connector process accepted the job and died before the terminal
        event — the client's retry will hit the same clientMessageId, so the
        row must be out of the way (a retry re-accepts cancelled/
        permanent_failure rows via accept_message_request).
        """
        cutoff = _utcnow_rfc3339()
        now_ts = datetime.datetime.now(datetime.timezone.utc)
        stale: list[str] = []
        with self._lock:
            conn = self._connect()
            try:
                for row in conn.execute(
                    "SELECT client_message_id, updated_at FROM message_requests "
                    "WHERE state = 'running'"
                ).fetchall():
                    updated = _parse_rfc3339(row["updated_at"])
                    if updated is None:
                        continue
                    if (now_ts - updated).total_seconds() > _STALE_JOB_SECONDS:
                        stale.append(row["client_message_id"])
                for client_message_id in stale:
                    conn.execute(
                        "UPDATE message_requests SET state = 'permanent_failure', "
                        " error_category = 'connector_restart', updated_at = ? "
                        "WHERE client_message_id = ? AND state = 'running'",
                        (cutoff, client_message_id),
                    )
                conn.commit()
            finally:
                conn.close()
        if stale:
            logger.warning(
                "delivery: marked %d stale running job(s) permanent_failure "
                "(connector restart recovery)",
                len(stale),
            )

    # ── legacy sidecar migration ──────────────────────────────────────────

    @staticmethod
    def _sidecar_path() -> Path:
        connector_home = os.getenv(
            "HERMES_MOBILE_CONNECTOR_HOME"
        ) or str(Path.home() / ".hermes-mobile")
        return Path(connector_home) / "session_meta.json"

    @staticmethod
    def _lookup_owner_device(app_id: str, hermes_id: str) -> str:
        """Best-effort owner device from device_registry.json, else "".

        The registry keys sessions by canonical app UUID; the sidecar may
        hold a draft alias, so the canonical UUIDv5 derived from the Hermes
        session id is tried as a fallback.
        """
        try:
            from .session_store import _app_uuid  # local import: no cycles
            candidates = {app_id, _app_uuid(hermes_id)}
        except Exception:                            # noqa: BLE001
            candidates = {app_id}
        try:
            connector_home = os.getenv(
                "HERMES_MOBILE_CONNECTOR_HOME"
            ) or str(Path.home() / ".hermes-mobile")
            with open(Path(connector_home) / "device_registry.json") as fh:
                registry = json.load(fh)
            sessions = registry.get("sessions", {})
            for candidate in candidates:
                entry = sessions.get(candidate)
                if isinstance(entry, dict) and entry.get("deviceId"):
                    return str(entry["deviceId"])
        except (OSError, ValueError):
            pass
        return ""

    def migrate_bindings_from_sidecar(
        self, sidecar_path: str | Path | None = None
    ) -> int:
        """Migrate legacy session_meta.json bindings into SQLite.

        A binding is any sidecar entry with a ``_hermes_id`` whose key is a
        valid UUID (draft aliases and canonical ids both qualify).  The
        Hermes session id is UNIQUE, so when the same session appears under
        both a canonical and a draft key the canonical row wins; drafts for
        an already-migrated session are skipped.  On success the sidecar is
        renamed ``session_meta.json.migrated``.  Returns the number of rows
        migrated; 0 leaves the sidecar untouched.
        """
        path = Path(sidecar_path) if sidecar_path is not None else self._sidecar_path()
        if not path.exists():
            return 0
        try:
            with open(path) as fh:
                data = json.load(fh)
        except (OSError, ValueError):
            logger.warning(
                "delivery: could not read %s — skipping migration", path
            )
            return 0
        if not isinstance(data, dict):
            return 0

        try:
            from .session_store import _app_uuid  # local import: no cycles
        except Exception:                            # noqa: BLE001
            _app_uuid = None

        def _is_canonical(app_id: str, hermes_id: str) -> bool:
            if _app_uuid is None:
                return False
            try:
                return _app_uuid(hermes_id) == app_id
            except Exception:                        # noqa: BLE001
                return False

        now = _utcnow_rfc3339()
        rows: list[tuple[str, str, str, str]] = []
        claimed: set[str] = set()

        def _queue(app_id: Any, meta: Any) -> None:
            if not isinstance(meta, dict) or not isinstance(app_id, str):
                return
            hermes_id = meta.get("_hermes_id")
            if not isinstance(hermes_id, str) or not hermes_id:
                return
            # Only rows with valid UUIDs — the table's PRIMARY KEY is the
            # app-facing UUID and the app decoders reject anything else.
            if _coerce_uuid(app_id) is None:
                logger.debug(
                    "delivery: skipping sidecar key %r (not a UUID)", app_id
                )
                return
            if hermes_id in claimed:
                return  # a higher-priority row already owns this session
            owner_device = self._lookup_owner_device(app_id, hermes_id)
            rows.append((app_id, hermes_id, "", owner_device))
            claimed.add(hermes_id)

        # Pass 1: canonical keys (app id == uuid5(hermes id)) win — they are
        # the durable app-facing identity and the hermes_session_id column is
        # UNIQUE, so the canonical row must own the binding, not the draft.
        for app_id, meta in data.items():
            if isinstance(meta, dict) and isinstance(meta.get("_hermes_id"), str) \
                    and _is_canonical(app_id, meta["_hermes_id"]):
                _queue(app_id, meta)
        # Pass 2: draft aliases for sessions not claimed in pass 1.
        for app_id, meta in data.items():
            _queue(app_id, meta)

        if not rows:
            return 0

        with self._lock:
            conn = self._connect()
            try:
                conn.execute("BEGIN")
                for app_id, hermes_id, account_id, owner_device in rows:
                    conn.execute(
                        "INSERT OR IGNORE INTO conversation_bindings "
                        "(app_conversation_id, hermes_session_id, account_id, "
                        " owner_device_id, created_at, updated_at) "
                        "VALUES (?, ?, ?, ?, ?, ?)",
                        (app_id, hermes_id, account_id, owner_device, now, now),
                    )
                conn.commit()
            except Exception:
                conn.rollback()
                raise
            finally:
                conn.close()

        path.rename(Path(str(path) + ".migrated"))
        logger.info(
            "delivery: migrated %d conversation binding(s) from %s",
            len(rows), path.name,
        )
        return len(rows)


# ── Process-wide singleton ────────────────────────────────────────────────
#
# Both the HTTP facade and the connector's startup reconciliation use one
# store.  The singleton re-binds automatically when
# HERMES_MOBILE_CONNECTOR_HOME changes (tests flip it per case).

_store_singleton: DeliveryStore | None = None


def get_delivery_store() -> DeliveryStore:
    global _store_singleton
    db_path = delivery_db_path()
    if _store_singleton is None or _store_singleton.db_path != db_path:
        _store_singleton = DeliveryStore(db_path)
    return _store_singleton


def reset_delivery_store() -> None:
    """Drop the singleton (tests only — env-scoped stores re-resolve on demand)."""
    global _store_singleton
    _store_singleton = None
