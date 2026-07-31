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


def _has_reasoning_column(conn: sqlite3.Connection) -> bool:
    """True if messages.reasoning_content exists on this state.db."""
    try:
        cols = conn.execute("PRAGMA table_info(messages)").fetchall()
        return any(c["name"] == "reasoning_content" for c in cols)
    except sqlite3.Error:
        return False


# ── Helpers ────────────────────────────────────────────────────────────────

# UUIDv5 namespace for deriving app-facing UUIDs from Hermes session ids.
# Using DNS namespace means the derivation is deterministic across all
# compliant UUIDv5 implementations — the app can compute the same mapping
# independently if desired.
_APP_NAMESPACE = uuid.UUID("6ba7b810-9dad-11d1-80b4-00c04fd430c8")  # == uuid.NAMESPACE_DNS
# ^ NAMESPACE_DNS, matching the paragraph above. It was mislabelled "NAMESPACE_URL"
# for several releases; any tool that reconciles app ids must use DNS (…b810…),
# not URL (…b811…), or it derives a different canonical id for every session.

# B40: title generation runs in a throwaway ``title-<uuid4>`` session (B39 T2).
# Those turns go through the same message handler, so Hermes records them with
# ``source='api_server'`` exactly like a user conversation — B39's assumption
# that the prefix alone kept them out of the session list was wrong, and every
# titled chat gained a phantom "New Chat" sibling in the app's list.  Filter
# them out explicitly wherever sessions are surfaced to the app.
_NOT_INTERNAL_SESSION = "id NOT LIKE 'title-%'"


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


def _canonical_app_id(app_uuid: str) -> str:
    """Return the listable UUID for an app id, following draft aliases.

    ``POST /v1/sessions`` creates a UUID before Hermes has created its own
    session.  Once the turn lands, that UUID is only an alias for the stable
    UUIDv5 derived from Hermes' id; it must never become a second row.
    """
    meta = get_session_meta(app_uuid) or {}
    alias = meta.get("_alias_of")
    if alias and alias != app_uuid:
        return str(alias)
    hermes_id = meta.get("_hermes_id")
    if hermes_id:
        return _coerce_uuid(hermes_id) or _app_uuid(hermes_id)
    return app_uuid


def _display_app_id(hermes_id: str, sidecar: dict) -> str:
    """Return the one client-visible UUID for a Hermes session.

    A new chat starts with a UUID minted by iOS.  Hermes later creates its own
    non-UUID session id, from which the connector can derive a canonical UUID.
    Showing both identifiers as session rows creates duplicate chats with the
    same content.  Prefer the originating draft alias when it exists: it is the
    ID held by the device, works on another device when explicitly selected,
    and still resolves to the Hermes id through the sidecar.
    """
    canonical = _app_uuid(hermes_id)
    aliases = sorted(
        key for key, value in sidecar.items()
        if isinstance(value, dict)
        and value.get("_alias_of") == canonical
        and not value.get("tombstone")
    )
    return aliases[0] if aliases else canonical


def _persist_hermes_mapping(app_uuid: str, hermes_id: str) -> None:
    """Record the app-uuid ↔ hermes-id mapping in the sidecar.

    Idempotent — if the mapping already exists it is re-written to the same
    value.  The app_uuid is deterministic so the sidecar entry is stable.
    """
    canonical = _coerce_uuid(hermes_id) or _app_uuid(hermes_id)
    if app_uuid != canonical:
        # Draft ids remain resolvable for an in-flight client, but aliases are
        # deliberately tombstoned so session_list cannot emit duplicate rows.
        set_session_meta(
            app_uuid,
            _hermes_id=hermes_id,
            _alias_of=canonical,
            tombstone=True,
        )
    if _resolve_hermes_id(canonical) != hermes_id:
        set_session_meta(canonical, _hermes_id=hermes_id)


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


# ── Message → job correlation ──────────────────────────────────────────────
#
# Hermes writes assistant messages to state.db without a jobId column, so a
# conversation refresh returns rows with ``jobId: None``.  The iOS merge then
# can't match persisted assistant rows to the live streaming placeholder by
# identity, and falls through to content heuristics that are unsafe for
# partial history, repeated phrases, tool boundaries, and reasoning text.
#
# This map records (app_message_uuid → job_id) when a job completes, and
# ``_message_to_dict`` consults it.  It is process-local (dies with the
# connector); the iOS client only needs it for the first conversation refresh
# after a job completes — after that the client has its own correlation.
_message_job_map: dict[str, str] = {}
# TTL guard: entries older than this are dropped so a long-running connector
# doesn't grow unbounded.  The iOS polling cadence is ~30s max; 120s is plenty.
_MESSAGE_JOB_MAP_TTL = 120.0
_message_job_map_timestamps: dict[str, float] = {}


def set_message_job_id(message_id: str, job_id: str) -> None:
    """Record that *message_id* was produced by *job_id*."""
    _message_job_map[message_id] = job_id
    import time as _time
    _message_job_map_timestamps[message_id] = _time.time()


def get_message_job_id(message_id: str) -> str | None:
    """Return the job ID for *message_id*, or None."""
    now = __import__("time").time()
    # Prune expired entries on read
    stale = [
        mid for mid, ts in _message_job_map_timestamps.items()
        if now - ts > _MESSAGE_JOB_MAP_TTL
    ]
    for mid in stale:
        _message_job_map.pop(mid, None)
        _message_job_map_timestamps.pop(mid, None)
    return _message_job_map.get(message_id)


# ── Message history ────────────────────────────────────────────────────────

# Historical reasoning is returned so the collapsed "Thought process" block
# survives a conversation refresh instead of vanishing the moment the stream
# ends.  It is not free: the app re-fetches the *whole* conversation on a ~30 s
# timer, `limit` is 200, and long-running ops sessions carry up to 237 KB of
# chain-of-thought (max single row: 87,771 chars; mean: 553).  Typical phone
# chats are 3-11 messages / 1-3 KB and are unaffected by these caps — they exist
# purely so opening one of the big ops sessions on cellular cannot melt the poll.
_REASONING_MAX_CHARS = 4000
_REASONING_BUDGET_CHARS = 64_000
_REASONING_TRUNCATED = "\n\n… (reasoning truncated)"


def _apply_reasoning_budget(messages: list[dict]) -> list[dict]:
    """Cap per-message reasoning, then spend a whole-conversation budget.

    Walks newest → oldest so the most recent turns — the ones a user actually
    expands — keep their chain-of-thought, and older turns give theirs up once
    the budget is exhausted.  Dropping to "" is what the UI already expects for
    "this message has no reasoning"; it renders no block at all.

    Mutates *messages* in place and returns it.
    """
    spent = 0
    for msg in reversed(messages):
        text = msg.get("reasoning") or ""
        if not text:
            continue
        if len(text) > _REASONING_MAX_CHARS:
            text = text[:_REASONING_MAX_CHARS].rstrip() + _REASONING_TRUNCATED
        if spent + len(text) > _REASONING_BUDGET_CHARS:
            msg["reasoning"] = ""
            continue
        spent += len(text)
        msg["reasoning"] = text
    return messages


def session_messages(
    session_id: str, limit: int = 200, include_reasoning: bool = True
) -> list[dict]:
    """Return messages for a session in chronological order.

    Filters: user/assistant roles only, non-empty content, active=1,
    not compacted. Maps assistant → herald for the iOS MessageSender decoder.

    *session_id* may be an app-facing UUID; it is resolved to the Hermes
    session id before querying state.db.

    *include_reasoning* attaches stored chain-of-thought (subject to
    ``_apply_reasoning_budget``).  Callers that only read role/text — the title
    derivation path — pass False and skip the transfer entirely.
    """
    session_id = _canonical_app_id(session_id)
    hermes_id = _resolve_hermes_id(session_id) or session_id
    conn = _connect()
    try:
        # `reasoning_content` is present on current Hermes schemas but selecting
        # it unconditionally makes an older state.db raise OperationalError and
        # take *all* conversation loading down with it.  Probe, don't assume.
        want_reasoning = include_reasoning and _has_reasoning_column(conn)
        reasoning_select = "reasoning_content" if want_reasoning else "'' AS reasoning_content"
        rows = conn.execute(
            f"""
            SELECT id, role, content, {reasoning_select}, timestamp
            FROM messages
            WHERE session_id = ?
              AND role IN ('user', 'assistant')
              AND content != ''
              AND active = 1
              AND COALESCE(compacted, 0) = 0
              -- B39 T6: exclude compaction/summary messages from history.
              -- These are generated by the Hermes agent (Anthropic beta
              -- tool-runner compaction) and are never user-visible
              -- conversation turns.  The exact prefixes are cross-referenced
              -- against the Hermes agent source (_COMPACTION_PREFIXES in
              -- session_search_tool.py) and live state.db inspection.
              AND content NOT LIKE '[Recent Summary (d0,%'
              AND content NOT LIKE '[Session Arc Summary (d1,%'
              AND content NOT LIKE '[Current user objective preserved from compacted history]%'
              AND content NOT LIKE '[CONTEXT COMPACTION%'
              AND content NOT LIKE '[CONTEXT SUMMARY]:%'
              -- B19: two more agent-injected turns that Hermes stores with
              -- role='user' and display_kind NULL, making them
              -- indistinguishable from something the person typed.  Both were
              -- observed rendering as the user's own blue bubble on device:
              --   "[Your previous response was cut off. It ended with: …]"
              --   "[IMPORTANT: The user has invoked the "…" skill, …]"
              -- The first is the self-continuation prompt Hermes issues after
              -- a truncated response; because that continuation also forks a
              -- new session, it was frequently the *only* user-role row in the
              -- session the app resolved to.
              AND content NOT LIKE '[Your previous response was cut off%'
              AND content NOT LIKE '[IMPORTANT: The user has invoked the%'
            ORDER BY timestamp ASC
            LIMIT ?
            """,
            (hermes_id, limit),
        ).fetchall()
    finally:
        conn.close()

    messages = [_message_to_dict(r, include_reasoning=want_reasoning) for r in rows]
    return _apply_reasoning_budget(messages) if want_reasoning else messages


def _message_to_dict(row: sqlite3.Row, include_reasoning: bool = True) -> dict:
    role = "herald" if row["role"] == "assistant" else row["role"]

    # Message ids are ints; the iOS app declares id: UUID.
    msg_id = _coerce_uuid(row["id"])
    if msg_id is None:
        msg_id = _deterministic_uuid("msg", row["id"])

    # Consult the job map — if this message was produced by a known job,
    # return its jobId so the iOS client can correlate assistant rows
    # with the live streaming placeholder by identity, not content heuristics.
    resolved_job_id = get_message_job_id(msg_id)

    return {
        "id": msg_id,
        "clientMessageId": None,
        "role": role,
        "text": row["content"],
        "timestamp": _epoch_to_iso(row["timestamp"]),
        "deliveryStatus": "delivered",
        "jobId": resolved_job_id,
        "attachments": None,
        "reasoning": (row["reasoning_content"] or "") if include_reasoning else "",
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
                "WHERE source = 'api_server' AND profile_name = ? "
                f"AND {_NOT_INTERNAL_SESSION}",
                (profile,),
            ).fetchone()
        else:
            db_total_row = conn.execute(
                "SELECT COUNT(*) FROM sessions WHERE source = 'api_server' "
                f"AND {_NOT_INTERNAL_SESSION}"
            ).fetchone()
        db_total = db_total_row[0] if db_total_row else 0

        # Fetch more than requested to account for tombstoned rows we'll drop.
        fetch_limit = min(limit * 3, 1000)
        if profile:
            rows = conn.execute(
                f"""
                SELECT id, title, display_name, started_at, ended_at,
                       message_count, source, archived, pinned
                FROM sessions
                WHERE source = 'api_server' AND profile_name = ?
                  AND {_NOT_INTERNAL_SESSION}
                ORDER BY started_at DESC
                LIMIT ? OFFSET ?
                """,
                (profile, fetch_limit, offset),
            ).fetchall()
        else:
            rows = conn.execute(
                f"""
                SELECT id, title, display_name, started_at, ended_at,
                       message_count, source, archived, pinned
                FROM sessions
                WHERE source = 'api_server'
                  AND {_NOT_INTERNAL_SESSION}
                ORDER BY started_at DESC
                LIMIT ? OFFSET ?
                """,
                (fetch_limit, offset),
            ).fetchall()
    finally:
        conn.close()

    sessions: list[dict] = []
    tombstoned_count = 0

    # One connection reused for the derived-title lookups below, opened only
    # for the page we actually emit.
    title_conn = _connect()
    try:
        for r in rows:
            hermes_id = r["id"]
            canonical_id = _coerce_uuid(hermes_id) or _app_uuid(hermes_id)

            # Persist the reverse mapping in the sidecar for session_messages /
            # session_title lookups later.
            _persist_hermes_mapping(canonical_id, hermes_id)
            sidecar = _load_sidecar()
            app_id = _display_app_id(hermes_id, sidecar)

            meta = sidecar.get(app_id, {})
            if meta.get("tombstone"):
                tombstoned_count += 1
                continue

            # B40: fall back to the opening user message before the "New Chat"
            # placeholder.  sessions.title and display_name are NULL for every
            # api_server row, so a session whose generated title hadn't landed
            # in the sidecar yet (or was written under a different id) showed
            # the placeholder permanently.
            title = (
                meta.get("title")
                or r["title"]
                or r["display_name"]
                or _derived_title(hermes_id, conn=title_conn)
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
    finally:
        title_conn.close()

    # Total = db rows - tombstones that would be dropped.
    # After P0-1, db_total already equals the emittable count because we
    # no longer skip non-UUID ids — the only rows dropped are tombstones.
    # Count all tombstones in the sidecar (across all keys, not just this page).
    total_tombstones = sum(
        1 for v in sidecar.values()
        if isinstance(v, dict) and v.get("tombstone") and not v.get("_alias_of")
    )
    total = max(0, db_total - total_tombstones)
    return sessions, total


def session_title(session_id: str) -> str | None:
    """Return the title for a session, or None if not found.

    *session_id* may be an app-facing UUID; it is resolved to the Hermes
    session id before querying state.db.

    B40: the sidecar is consulted first.  ``sessions.title`` and
    ``display_name`` are NULL for every ``source='api_server'`` row, so
    reading state.db alone made this return None even for sessions whose
    generated title was sitting in the sidecar — which is what put
    ``"title": null`` on GET /v1/sessions/{id}/conversation and left the
    thread showing a placeholder forever.
    """
    session_id = _canonical_app_id(session_id)
    meta = get_session_meta(session_id)
    if meta.get("title"):
        return meta["title"]

    hermes_id = _resolve_hermes_id(session_id) or session_id
    conn = _connect()
    try:
        row = conn.execute(
            "SELECT title, display_name FROM sessions WHERE id = ?",
            (hermes_id,),
        ).fetchone()
    finally:
        conn.close()

    if row is not None and (row["title"] or row["display_name"]):
        return row["title"] or row["display_name"]

    # Last resort: derive from the opening user message rather than reporting
    # "no title".  A session that has messages always has something better to
    # show than a placeholder.
    return _derived_title(hermes_id)


def _derived_title(hermes_id: str, conn: sqlite3.Connection | None = None) -> str | None:
    """Build a title from a session's first user message. None if it has none.

    Deterministic and free — used whenever no stored title exists so the app
    never has to render a placeholder for a session that has real content.
    """
    owned = conn is None
    conn = conn or _connect()
    try:
        row = conn.execute(
            """
            SELECT content FROM messages
            WHERE session_id = ?
              AND role = 'user'
              AND content != ''
              AND active = 1
            ORDER BY timestamp ASC
            LIMIT 1
            """,
            (hermes_id,),
        ).fetchone()
    finally:
        if owned:
            conn.close()

    if row is None:
        return None
    first_line = str(row["content"]).strip().split("\n")[0].strip()
    if len(first_line) < 3:
        return None
    return first_line[:60].rstrip() + ("…" if len(first_line) > 60 else "")


def _find_session_by_recent_message(
    text: str, since: float | None = None
) -> str | None:
    """Find the Hermes session id that a user message was actually written to.

    B40: this is no longer only a cold-start fallback — it is the authority on
    where a turn landed.  Hermes' api_server echoes back the
    ``X-Hermes-Session-Id`` it was handed even when it could not resume that
    session and silently routed the turn into its default session instead
    (verified live 2026-07-29: posting with ``api-32bede44b7d6813f`` returned
    that same id while the message was written to ``20260722_144605_795809``).
    Trusting the echoed id filed replies under a session the app could never
    read back.

    *since* bounds the match to messages written at or after that epoch, so a
    turn is never attributed to an older session that happens to contain the
    same text.

    Returns *None* if no matching message is found.
    """
    conn = _connect()
    try:
        if since is None:
            row = conn.execute(
                """
                SELECT session_id FROM messages
                WHERE role = 'user' AND content = ? AND active = 1
                ORDER BY timestamp DESC
                LIMIT 1
                """,
                (text,),
            ).fetchone()
        else:
            row = conn.execute(
                """
                SELECT session_id FROM messages
                WHERE role = 'user' AND content = ? AND active = 1
                  AND timestamp >= ?
                ORDER BY timestamp DESC
                LIMIT 1
                """,
                (text, since),
            ).fetchone()
    finally:
        conn.close()
    return row["session_id"] if row else None


def _find_session_by_assistant_reply(
    text: str, since: float | None = None
) -> str | None:
    """Find the Hermes session an assistant *reply* was actually written to.

    B19: ``_find_session_by_recent_message`` locates a turn by its user text,
    which silently breaks when Hermes forks mid-job.  When a response is
    truncated the agent continues itself in a **new run**, and Hermes names a
    session after each run id.  The user's text stays in the first session
    while the continuation prompt and the real answer land in the second:

        run_2e81373a…  user      "Big homie. Give me some fresh hood comedy"
        run_6ef67ead…  user      "[Your previous response was cut off. …]"
        run_6ef67ead…  assistant "Aight, here's the rotation for tonight. …"

    Resolving by user text maps the conversation to the first session, so the
    app reads back a session that contains no answer — the reply is not lost,
    it is filed where the client never looks.  That is the "no response"
    report, and the fork is also why an internal continuation prompt shows up
    as the user's own bubble.

    The reply is the message that has to be readable, so it is the correct
    anchor.  The connector already holds the accumulated text it streamed, so
    it can ask where that text landed.  Exact match first; a prefix match then
    covers reasoning-stripping and trailing-whitespace drift between what was
    streamed and what Hermes persisted.

    Returns *None* if no matching assistant message is found, in which case the
    caller should fall back to the user-text lookup.
    """
    stripped = (text or "").strip()
    if not stripped:
        return None

    conn = _connect()
    try:
        clauses = ["role = 'assistant'", "active = 1"]
        params: list = []
        if since is not None:
            clauses.append("timestamp >= ?")
            params.append(since)
        where = " AND ".join(clauses)

        row = conn.execute(
            f"SELECT session_id FROM messages WHERE {where} AND content = ? "
            "ORDER BY timestamp DESC LIMIT 1",
            (*params, stripped),
        ).fetchone()
        if row:
            return row["session_id"]

        # Prefix match. Long enough to stay unique against boilerplate
        # openers, short enough to survive a truncated tail.
        prefix = stripped[:120]
        if len(prefix) < 24:
            return None
        escaped = prefix.replace("\\", r"\\").replace("%", r"\%").replace("_", r"\_")
        row = conn.execute(
            f"SELECT session_id FROM messages WHERE {where} "
            "AND content LIKE ? ESCAPE '\\' "
            "ORDER BY timestamp DESC LIMIT 1",
            (*params, escaped + "%"),
        ).fetchone()
        return row["session_id"] if row else None
    finally:
        conn.close()


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
                f"""
                SELECT id, title, display_name, started_at, ended_at,
                       message_count, source, archived, pinned
                FROM sessions
                WHERE source = 'api_server'
                  AND profile_name = ?
                  AND {_NOT_INTERNAL_SESSION}
                  AND (title LIKE ? OR display_name LIKE ?)
                ORDER BY started_at DESC
                LIMIT ?
                """,
                (profile, pattern, pattern, limit),
            ).fetchall()
        else:
            rows = conn.execute(
                f"""
                SELECT id, title, display_name, started_at, ended_at,
                       message_count, source, archived, pinned
                FROM sessions
                WHERE source = 'api_server'
                  AND {_NOT_INTERNAL_SESSION}
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
        canonical_id = _coerce_uuid(hermes_id) or _app_uuid(hermes_id)
        _persist_hermes_mapping(canonical_id, hermes_id)
        sidecar = _load_sidecar()
        app_id = _display_app_id(hermes_id, sidecar)

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
