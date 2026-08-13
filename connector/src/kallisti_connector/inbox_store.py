"""Inbox item persistence for Kallisti.

Completed chat turns land here as inbox items (per owning device) so the
app's Inbox tab has real content to show - not just the transient push
notification. Items carry the conversation id in their payload so both the
Inbox tap and the notification tap can deep-link into the session.

Build 67: previously `GET /v1/inbox` was a stub returning {"items": []}.
"""

from __future__ import annotations

import datetime
import json
import os
import sqlite3
import threading
import uuid
from pathlib import Path

try:
    from typing import Any
except ImportError:  # pragma: no cover
    pass


def _inbox_db_path() -> Path:
    connector_home = os.getenv(
        "HERMES_MOBILE_CONNECTOR_HOME"
    ) or str(Path.home() / ".hermes-mobile")
    return Path(connector_home) / "inbox.db"


_SCHEMA = """
CREATE TABLE IF NOT EXISTS inbox_items (
    id TEXT PRIMARY KEY,
    installation_id TEXT NOT NULL,
    conversation_id TEXT,
    kind TEXT NOT NULL DEFAULT 'notification',
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    payload TEXT,
    attachments TEXT,
    status TEXT NOT NULL DEFAULT 'pending',
    priority TEXT NOT NULL DEFAULT 'normal',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_inbox_device_created
    ON inbox_items (installation_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_inbox_conv
    ON inbox_items (conversation_id);
"""


def _utcnow_rfc3339() -> str:
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


class InboxStore:
    """SQLite-backed inbox. Each row belongs to one installation (device)."""

    def __init__(self, db_path: str | Path | None = None) -> None:
        self.db_path = Path(db_path) if db_path else _inbox_db_path()
        self._lock = threading.RLock()
        self._init_db()

    def _connect(self) -> sqlite3.Connection:
        self.db_path.parent.mkdir(parents=True, exist_ok=True)
        conn = sqlite3.connect(str(self.db_path))
        conn.row_factory = sqlite3.Row
        return conn

    def _init_db(self) -> None:
        with self._lock:
            conn = self._connect()
            try:
                conn.executescript(_SCHEMA)
                # Migration check for attachments column
                cursor = conn.execute("PRAGMA table_info(inbox_items)")
                cols = [row["name"] for row in cursor.fetchall()]
                if "attachments" not in cols:
                    conn.execute("ALTER TABLE inbox_items ADD COLUMN attachments TEXT")
                conn.commit()
            finally:
                conn.close()

    def add_item(
        self,
        *,
        installation_id: str,
        title: str,
        body: str,
        kind: str = "notification",
        conversation_id: str | None = None,
        payload: dict[str, Any] | None = None,
        attachments: list[dict[str, Any]] | None = None,
        priority: str = "normal",
        item_id: str | None = None,
    ) -> str:
        """Insert a new inbox item; returns its id."""
        now = _utcnow_rfc3339()
        item_id = item_id or str(uuid.uuid4())
        payload_json = json.dumps(payload or {})
        attachments_json = json.dumps(attachments or []) if attachments else None
        with self._lock:
            conn = self._connect()
            try:
                conn.execute(
                    "INSERT INTO inbox_items "
                    "(id, installation_id, conversation_id, kind, title, body, "
                    " payload, attachments, status, priority, created_at, updated_at) "
                    "VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?, ?)",
                    (
                        item_id,
                        installation_id,
                        conversation_id,
                        kind,
                        title,
                        body,
                        payload_json,
                        attachments_json,
                        priority,
                        now,
                        now,
                    ),
                )
                conn.commit()
            finally:
                conn.close()
        return item_id

    def list_items(self, installation_id: str, limit: int = 100) -> list[dict[str, Any]]:
        """Return items for one device, newest first."""
        with self._lock:
            conn = self._connect()
            try:
                rows = conn.execute(
                    "SELECT * FROM inbox_items WHERE installation_id = ? "
                    "ORDER BY created_at DESC LIMIT ?",
                    (installation_id, int(limit)),
                ).fetchall()
            finally:
                conn.close()
        return [self._row_to_dict(r) for r in rows]

    def get_item(self, item_id: str) -> dict[str, Any] | None:
        with self._lock:
            conn = self._connect()
            try:
                row = conn.execute(
                    "SELECT * FROM inbox_items WHERE id = ?", (item_id,)
                ).fetchone()
            finally:
                conn.close()
        return self._row_to_dict(row) if row is not None else None

    def set_status(
        self, item_id: str, installation_id: str, status: str
    ) -> dict[str, Any] | None:
        """Mark an item opened/dismissed. Only the owning device may mutate it."""
        if status not in {"pending", "opened", "completed", "dismissed"}:
            return None
        now = _utcnow_rfc3339()
        with self._lock:
            conn = self._connect()
            try:
                cur = conn.execute(
                    "UPDATE inbox_items SET status = ?, updated_at = ? "
                    "WHERE id = ? AND installation_id = ?",
                    (status, now, item_id, installation_id),
                )
                conn.commit()
            finally:
                conn.close()
        if cur.rowcount == 0:
            return None
        return self.get_item(item_id)

    @staticmethod
    def _row_to_dict(row: sqlite3.Row) -> dict[str, Any]:
        payload: dict[str, Any] = {}
        try:
            payload = json.loads(row["payload"]) if row["payload"] else {}
        except (ValueError, TypeError):
            payload = {}
        attachments: list[dict[str, Any]] = []
        try:
            if "attachments" in row.keys() and row["attachments"]:
                attachments = json.loads(row["attachments"])
        except (ValueError, TypeError):
            attachments = []
        return {
            "id": row["id"],
            "installationId": row["installation_id"],
            "conversationId": row["conversation_id"],
            "kind": row["kind"],
            "title": row["title"],
            "body": row["body"],
            "payload": payload,
            "attachments": attachments,
            "status": row["status"],
            "priority": row["priority"],
            "createdAt": row["created_at"],
            "updatedAt": row["updated_at"],
        }


_store: InboxStore | None = None


def get_inbox_store() -> InboxStore:
    global _store
    if _store is None:
        _store = InboxStore()
    return _store
