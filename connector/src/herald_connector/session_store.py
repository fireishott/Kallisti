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

# UUIDv5 namespace for deriving app-facing UUIDs from Hermes session ids.
# Using DNS namespace means the derivation is deterministic across all
# compliant UUIDv5 implementations — the app can compute the same mapping
# independently if desired.
_APP_NAMESPACE = uuid.UUID("6ba7b810-9dad-11d1-80b4-00c04fd430c8")  # NAMESPACE_URL


def _app_uuid(hermes_id: str) -> str:
    """Derive a deterministic, stable app-facing UUID from a Hermes session id.

    Hermes sessions use ids like ``api-9af38ce4fa5ba1f4`` (from api_server) or
    ``20260716_083812_5c0381`` (legacy).  The iOS decoders require UUIDs, so we
    emit uuid5(NAMESPACE_URL, hermes_id) — stable across connector restarts,
    no schema change, no write to state.db.
    """
    return str(uuid.uuid5(_APP_NAMESPACE, str(hermes_id)))


def _coerce_uuid(value: Any) -> str | None:
    """Return a lowercase UUID string, or None. Never raise."""
    try:
        return str(uuid.UUID(str(value)))
    except (ValueError, TypeError, AttributeError):
        return None


def _resolve_hermes_id(app_uuid: str) -> str | None:
    """Reverse-lookup: app-facing UUID → Hermes session id.

    The mapping is persisted in the JSON sidecar under the ``_hermes_id`` key.
    If no mapping is recorded (cold start, sidecar missing), returns *None*.
    """
    meta = get_session_meta(app_uuid)
    return meta.get("_hermes_id") if meta else None


def _persist_hermes_mapping(app_uuid: str, hermes_id: str) -> None:
    """Record the app-uuid ↔ hermes-id mapping in the sidecar.

    Idempotent — if the mapping already exists it is re-written to the same
    value.  The app_uuid is deterministic so the sidecar entry is stable.
    """
    existing = _resolve_hermes_id(app_uuid)
    if existing == hermes_id:
        return  # Already recorded, avoid unnecessary writes.
    set_session_meta(app_uuid, _hermes_id=hermes_id)


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

    *session_id* may be an app-facing UUID; it is resolved to the Hermes
    session id before querying state.db.
    """
    hermes_id = _resolve_hermes_id(session_id) or session_id
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
            (hermes_id, limit),
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
    Converts every Hermes session id (``api-…``, legacy) to a deterministic
    app-facing UUID via ``_app_uuid()`` so all 736 sessions become visible.
    Pin/archive state is overlaid from the local sidecar; tombstoned ids
    (deleted) are dropped.

    **total** matches the count of *emittable* rows (after tombstone filter),
    not the raw ``SELECT COUNT(*)`` — this fixes the "Load more" bar that was
    permanently visible because ``total`` reported 287 while only 3 rows passed
    the old UUID coercion.
    """
    profile = _profile_name()
    sidecar = _load_sidecar()

    conn = _connect()
    try:
        # Count total rows matching the filter — after P0-1 this IS the
        # emittable count because we no longer drop non-UUID ids.
        if profile:
            db_total_row = conn.execute(
                "SELECT COUNT(*) FROM sessions "
                "WHERE source = 'api_server' AND profile_name = ?",
                (profile,),
            ).fetchone()
        else:
            db_total_row = conn.execute(
                "SELECT COUNT(*) FROM sessions WHERE source = 'api_server'"
            ).fetchone()
        db_total = db_total_row[0] if db_total_row else 0

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
        hermes_id = r["id"]
        app_id = _coerce_uuid(hermes_id) or _app_uuid(hermes_id)

        # Persist the reverse mapping in the sidecar for session_messages /
        # session_title lookups later.
        _persist_hermes_mapping(app_id, hermes_id)

        meta = sidecar.get(app_id, {})
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
            "id": app_id,
            "title": title,
            "previewText": None,
            "updatedAt": updated_at,
            "source": r["source"] or "api_server",
            "isPinned": bool(meta.get("pinned", r["pinned"])),
            "isArchived": bool(meta.get("archived", r["archived"])),
        })

        if len(sessions) >= limit:
            break

    # Total = db rows - tombstones that would be dropped.
    # After P0-1, db_total already equals the emittable count because we
    # no longer skip non-UUID ids — the only rows dropped are tombstones.
    # Count all tombstones in the sidecar (across all keys, not just this page).
    total_tombstones = sum(
        1 for v in sidecar.values()
        if isinstance(v, dict) and v.get("tombstone")
    )
    total = max(0, db_total - total_tombstones)
    return sessions, total


def session_title(session_id: str) -> str | None:
    """Return the title for a session, or None if not found.

    *session_id* may be an app-facing UUID; it is resolved to the Hermes
    session id before querying state.db.
    """
    hermes_id = _resolve_hermes_id(session_id) or session_id
    conn = _connect()
    try:
        row = conn.execute(
            "SELECT title, display_name FROM sessions WHERE id = ?",
            (hermes_id,),
        ).fetchone()
    finally:
        conn.close()

    if row is None:
        return None
    return row["title"] or row["display_name"] or None


def _find_session_by_recent_message(text: str) -> str | None:
    """Find the Hermes session id for a recently-written user message.

    Used as a fallback when the runtime's finish event doesn't carry a
    session_id (cold start).  Matches on message content and returns
    the session_id of the most recently inserted match.

    Returns *None* if no matching message is found.
    """
    conn = _connect()
    try:
        row = conn.execute(
            """
            SELECT session_id FROM messages
            WHERE role = 'user' AND content = ? AND active = 1
            ORDER BY timestamp DESC
            LIMIT 1
            """,
            (text,),
        ).fetchone()
    finally:
        conn.close()
    return row["session_id"] if row else None


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
        hermes_id = r["id"]
        app_id = _coerce_uuid(hermes_id) or _app_uuid(hermes_id)
        _persist_hermes_mapping(app_id, hermes_id)

        meta = sidecar.get(app_id, {})
        if meta.get("tombstone"):
            continue

        sessions.append({
            "id": app_id,
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
