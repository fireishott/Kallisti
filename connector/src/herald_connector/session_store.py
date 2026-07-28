"""Read-only access to Hermes state.db for session and message history.

GUARDRAIL G1: state.db is READ-ONLY to the connector. All connections
open with mode=ro. Never INSERT/UPDATE/DELETE on sessions or messages.

The gateway writes to state.db continuously — an insert from the facade
can fail a live gateway write. Everything in this module is SELECT.
Pin/archive/delete state lives in a connector-local JSON sidecar.
"""

from __future__ import annotations

import datetime
import json
import logging
import os
import sqlite3
import uuid
from pathlib import Path
from typing import Any

logger = logging.getLogger("herald.session_store")


# ── Paths ──────────────────────────────────────────────────────────────────

def _db_path() -> Path:
    home = os.getenv("HERMES_HOME") or str(Path.home() / ".hermes")
    return Path(home) / "state.db"


def _profile_name() -> str | None:
    """Extract the profile name from HERMES_HOME (e.g. 'ignyte')."""
    home = os.getenv("HERMES_HOME") or ""
    return Path(home).name or None


def _sidecar_path() -> Path:
    connector_home = os.getenv(
        "HERMES_MOBILE_CONNECTOR_HOME"
    ) or str(Path.home() / ".hermes-mobile")
    return Path(connector_home) / "session_meta.json"


# ── Connection ─────────────────────────────────────────────────────────────

def _connect() -> sqlite3.Connection:
    conn = sqlite3.connect(
        f"file:{_db_path()}?mode=ro", uri=True, timeout=2.0
    )
    conn.row_factory = sqlite3.Row
    return conn


# ── Helpers ────────────────────────────────────────────────────────────────

def _coerce_uuid(value: Any) -> str | None:
    """Return a lowercase UUID string, or None. Never raise."""
    try:
        return str(uuid.UUID(str(value)))
    except (ValueError, TypeError, AttributeError):
        return None


def _deterministic_uuid(prefix: str, value: Any) -> str:
    """Deterministic UUIDv5 for non-UUID integer ids (message rows)."""
    return str(uuid.uuid5(uuid.NAMESPACE_OID, f"{prefix}:{value}"))


def _epoch_to_iso(ts: float) -> str:
    """Convert a float epoch to ISO 8601 UTC."""
    return datetime.datetime.fromtimestamp(
        ts, tz=datetime.timezone.utc
    ).isoformat()


# ── Sidecar (pin / archive / tombstone) ────────────────────────────────────

def _load_sidecar() -> dict:
    """Load session metadata overrides. Never raises."""
    try:
        with open(_sidecar_path()) as f:
            return json.load(f)
    except (FileNotFoundError, json.JSONDecodeError, PermissionError):
        return {}


def _save_sidecar(data: dict) -> None:
    """Atomically write session metadata overrides."""
    _sidecar_path().parent.mkdir(parents=True, exist_ok=True)
    tmp = _sidecar_path().with_suffix(".tmp")
    with open(tmp, "w") as f:
        json.dump(data, f, indent=2)
    tmp.rename(_sidecar_path())


def get_session_meta(session_id: str) -> dict:
    """Return overrides for a single session: {pinned, archived, tombstone, title}."""
    sidecar = _load_sidecar()
    return sidecar.get(session_id, {})


def set_session_meta(session_id: str, **kwargs) -> None:
    """Set overrides for a session. Merges with existing."""
    sidecar = _load_sidecar()
    entry = sidecar.get(session_id, {})
    entry.update(kwargs)
    sidecar[session_id] = entry
    _save_sidecar(sidecar)


# ── Message history ────────────────────────────────────────────────────────

def session_messages(
    session_id: str, limit: int = 200
) -> list[dict]:
    """Return messages for a session in chronological order.

    Filters: user/assistant roles only, non-empty content, active=1,
    not compacted. Maps assistant → herald for the iOS MessageSender decoder.
    """
    conn = _connect()
    try:
        rows = conn.execute(
            """
            SELECT id, role, content, timestamp
            FROM messages
            WHERE session_id = ?
              AND role IN ('user', 'assistant')
              AND content != ''
              AND active = 1
              AND COALESCE(compacted, 0) = 0
            ORDER BY timestamp ASC
            LIMIT ?
            """,
            (session_id, limit),
        ).fetchall()
    finally:
        conn.close()

    return [_message_to_dict(r) for r in rows]


def _message_to_dict(row: sqlite3.Row) -> dict:
    role = "herald" if row["role"] == "assistant" else row["role"]

    # Message ids are ints; the iOS app declares id: UUID.
    msg_id = _coerce_uuid(row["id"])
    if msg_id is None:
        msg_id = _deterministic_uuid("msg", row["id"])

    return {
        "id": msg_id,
        "clientMessageId": None,
        "role": role,
        "text": row["content"],
        "timestamp": _epoch_to_iso(row["timestamp"]),
        "deliveryStatus": "delivered",
        "jobId": None,
        "attachments": None,
    }


# ── Session list ───────────────────────────────────────────────────────────

def session_list(
    limit: int = 50, offset: int = 0
) -> tuple[list[dict], int]:
    """Return (sessions_page, total_count) for the app's sessions.

    Filters to source='api_server' and the connector's profile.
    Non-UUID session ids are skipped.
    Pin/archive state is overlaid from the local sidecar; tombstoned ids
    (deleted) are dropped.
    """
    profile = _profile_name()
    sidecar = _load_sidecar()

    conn = _connect()
    try:
        if profile:
            total_row = conn.execute(
                "SELECT COUNT(*) FROM sessions "
                "WHERE source = 'api_server' AND profile_name = ?",
                (profile,),
            ).fetchone()
        else:
            total_row = conn.execute(
                "SELECT COUNT(*) FROM sessions WHERE source = 'api_server'"
            ).fetchone()
        db_total = total_row[0] if total_row else 0

        # Fetch more than requested to account for tombstoned rows we'll drop.
        fetch_limit = min(limit * 3, 1000)
        if profile:
            rows = conn.execute(
                """
                SELECT id, title, display_name, started_at, ended_at,
                       message_count, source, archived, pinned
                FROM sessions
                WHERE source = 'api_server' AND profile_name = ?
                ORDER BY started_at DESC
                LIMIT ? OFFSET ?
                """,
                (profile, fetch_limit, offset),
            ).fetchall()
        else:
            rows = conn.execute(
                """
                SELECT id, title, display_name, started_at, ended_at,
                       message_count, source, archived, pinned
                FROM sessions
                WHERE source = 'api_server'
                ORDER BY started_at DESC
                LIMIT ? OFFSET ?
                """,
                (fetch_limit, offset),
            ).fetchall()
    finally:
        conn.close()

    sessions: list[dict] = []
    tombstoned_count = 0
    for r in rows:
        sid = _coerce_uuid(r["id"])
        if sid is None:
            continue

        meta = sidecar.get(sid, {})
        if meta.get("tombstone"):
            tombstoned_count += 1
            continue

        title = (
            meta.get("title")
            or r["title"]
            or r["display_name"]
            or "New Chat"
        )
        updated_at = _epoch_to_iso(
            r["ended_at"] if r["ended_at"] else r["started_at"]
        )

        sessions.append({
            "id": sid,
            "title": title,
            "previewText": None,
            "updatedAt": updated_at,
            "source": r["source"] or "api_server",
            "isPinned": bool(meta.get("pinned", r["pinned"])),
            "isArchived": bool(meta.get("archived", r["archived"])),
        })

        if len(sessions) >= limit:
            break

    total = max(0, db_total - sum(
        1 for v in sidecar.values() if v.get("tombstone")
    ))
    return sessions, total


def session_title(session_id: str) -> str | None:
    """Return the title for a session, or None if not found."""
    conn = _connect()
    try:
        row = conn.execute(
            "SELECT title, display_name FROM sessions WHERE id = ?",
            (session_id,),
        ).fetchone()
    finally:
        conn.close()

    if row is None:
        return None
    return row["title"] or row["display_name"] or None


# ── Session search ─────────────────────────────────────────────────────────

def session_search(query: str, limit: int = 20) -> list[dict]:
    """Full-text-like search across session titles.

    Simple LIKE search since state.db has no FTS index on sessions.
    """
    profile = _profile_name()
    sidecar = _load_sidecar()
    pattern = f"%{query}%"

    conn = _connect()
    try:
        if profile:
            rows = conn.execute(
                """
                SELECT id, title, display_name, started_at, ended_at,
                       message_count, source, archived, pinned
                FROM sessions
                WHERE source = 'api_server'
                  AND profile_name = ?
                  AND (title LIKE ? OR display_name LIKE ?)
                ORDER BY started_at DESC
                LIMIT ?
                """,
                (profile, pattern, pattern, limit),
            ).fetchall()
        else:
            rows = conn.execute(
                """
                SELECT id, title, display_name, started_at, ended_at,
                       message_count, source, archived, pinned
                FROM sessions
                WHERE source = 'api_server'
                  AND (title LIKE ? OR display_name LIKE ?)
                ORDER BY started_at DESC
                LIMIT ?
                """,
                (pattern, pattern, limit),
            ).fetchall()
    finally:
        conn.close()

    sessions: list[dict] = []
    for r in rows:
        sid = _coerce_uuid(r["id"])
        if sid is None:
            continue

        meta = sidecar.get(sid, {})
        if meta.get("tombstone"):
            continue

        sessions.append({
            "id": sid,
            "title": (
                meta.get("title")
                or r["title"]
                or r["display_name"]
                or "New Chat"
            ),
            "previewText": None,
            "updatedAt": _epoch_to_iso(
                r["ended_at"] if r["ended_at"] else r["started_at"]
            ),
            "source": r["source"] or "api_server",
            "isPinned": bool(meta.get("pinned", r["pinned"])),
            "isArchived": bool(meta.get("archived", r["archived"])),
        })

    return sessions
