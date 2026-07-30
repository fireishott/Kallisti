"""B40 tests: session attribution and title resolution.

Covers the two defects that made a completed turn look answered but blank, and
left every chat titled "New Chat":

  - `_find_session_by_recent_message` must honour a `since` bound so a turn is
    never attributed to an older session containing the same text.  Hermes'
    api_server echoes back an `X-Hermes-Session-Id` it could not actually
    resume, so state.db is the authority on where a message landed.
  - `title-<uuid4>` sessions (B39 T2's throwaway title-generation turns) are
    recorded by Hermes with source='api_server' and must never surface in the
    session list.
  - A session with messages must never report "New Chat"/None as its title —
    the opening user message is the fallback.
  - `session_title` reads the sidecar first; sessions.title is NULL for every
    api_server row.
"""

from __future__ import annotations

import json
import sqlite3
import tempfile
from pathlib import Path
from unittest.mock import patch

import pytest

from herald_connector import session_store
from herald_connector.herald_api_executor import _is_interrupt_sentinel


def _make_db(db_path: Path) -> sqlite3.Connection:
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row
    conn.execute("""
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY,
            session_id TEXT,
            role TEXT,
            content TEXT,
            timestamp REAL,
            active INTEGER DEFAULT 1,
            compacted INTEGER DEFAULT 0
        )
    """)
    conn.execute("""
        CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            source TEXT,
            title TEXT,
            display_name TEXT,
            started_at REAL,
            ended_at REAL,
            message_count INTEGER DEFAULT 0,
            archived INTEGER DEFAULT 0,
            pinned INTEGER DEFAULT 0,
            profile_name TEXT
        )
    """)
    conn.commit()
    return conn


@pytest.fixture
def env():
    """state.db + sidecar mirroring the live shape that produced the bug."""
    with tempfile.TemporaryDirectory() as tmpdir:
        tmp = Path(tmpdir)
        db_path = tmp / "state.db"
        conn = _make_db(db_path)

        # An old session that already contains the exact text the user is about
        # to send again — this is what the stale echoed id pointed at.
        conn.execute(
            "INSERT INTO sessions (id, source, title, display_name, started_at, "
            "ended_at, message_count) VALUES (?, 'api_server', NULL, NULL, 1000.0, NULL, 2)",
            ("api-old",),
        )
        conn.execute(
            "INSERT INTO messages (session_id, role, content, timestamp, active) "
            "VALUES ('api-old', 'user', 'Sup homie.', 1000.0, 1)"
        )
        # The session the turn actually landed in.
        conn.execute(
            "INSERT INTO sessions (id, source, title, display_name, started_at, "
            "ended_at, message_count) VALUES (?, 'api_server', NULL, NULL, 5000.0, NULL, 2)",
            ("api-live",),
        )
        conn.execute(
            "INSERT INTO messages (session_id, role, content, timestamp, active) "
            "VALUES ('api-live', 'user', 'Sup homie.', 5000.0, 1)"
        )
        # A throwaway title-generation session (B39 T2).
        conn.execute(
            "INSERT INTO sessions (id, source, title, display_name, started_at, "
            "ended_at, message_count) VALUES (?, 'api_server', NULL, NULL, 5001.0, NULL, 2)",
            ("title-48b4687b-da34-4244-83e6-ef20039e27c4",),
        )
        conn.execute(
            "INSERT INTO messages (session_id, role, content, timestamp, active) "
            "VALUES ('title-48b4687b-da34-4244-83e6-ef20039e27c4', 'user', "
            "'Generate a short title (3-8 words) for a conversation', 5001.0, 1)"
        )
        conn.commit()
        conn.close()

        sidecar_path = tmp / "session_meta.json"
        sidecar_path.write_text(json.dumps({}))
        yield db_path, sidecar_path


def _patched(db_path: Path, sidecar_path: Path):
    return (
        patch("herald_connector.session_store._db_path", return_value=db_path),
        patch("herald_connector.session_store._sidecar_path", return_value=sidecar_path),
        patch("herald_connector.session_store._profile_name", return_value=None),
    )


def test_recent_message_lookup_ignores_older_duplicate(env):
    """A `since` bound must exclude an identical message from an old session."""
    db_path, _ = env
    with patch("herald_connector.session_store._db_path", return_value=db_path):
        assert session_store._find_session_by_recent_message(
            "Sup homie.", since=4000.0
        ) == "api-live"
        # Without the bound the newest match still wins, but the bound is what
        # makes the attribution safe when clocks or ordering are ambiguous.
        assert session_store._find_session_by_recent_message("Sup homie.") == "api-live"


def test_recent_message_lookup_returns_none_when_nothing_is_recent(env):
    """No match inside the window means 'unknown', never a stale session."""
    db_path, _ = env
    with patch("herald_connector.session_store._db_path", return_value=db_path):
        assert session_store._find_session_by_recent_message(
            "Sup homie.", since=9000.0
        ) is None


def test_title_sessions_excluded_from_session_list(env):
    """Throwaway title- sessions must never appear as chats in the app."""
    db_path, sidecar_path = env
    db, side, prof = _patched(db_path, sidecar_path)
    with db, side, prof:
        sessions, total = session_store.session_list(limit=50)

    hermes_ids = [s["id"] for s in sessions]
    assert len(hermes_ids) == 2, f"expected only the two real chats, got {sessions}"
    assert total == 2, "total must not count title- sessions either"
    for s in sessions:
        assert not s["title"].startswith("Generate a short title"), (
            f"a title-generation session leaked into the list: {s}"
        )


def test_session_with_messages_never_reports_new_chat(env):
    """The opening user message is the fallback title, not the placeholder."""
    db_path, sidecar_path = env
    db, side, prof = _patched(db_path, sidecar_path)
    with db, side, prof:
        sessions, _ = session_store.session_list(limit=50)

    titles = {s["title"] for s in sessions}
    assert "New Chat" not in titles, f"placeholder survived: {sessions}"
    assert "Sup homie." in titles


def test_session_title_prefers_sidecar_over_null_db_column(env):
    """sessions.title is NULL for every api_server row; the sidecar holds it."""
    db_path, sidecar_path = env
    app_uuid = session_store._app_uuid("api-live")
    sidecar_path.write_text(json.dumps({
        app_uuid: {"_hermes_id": "api-live", "title": "Casual opening chat"},
    }))

    db, side, prof = _patched(db_path, sidecar_path)
    with db, side, prof:
        assert session_store.session_title(app_uuid) == "Casual opening chat"


def test_session_title_falls_back_to_first_user_message(env):
    """With no stored title anywhere, derive one rather than returning None."""
    db_path, sidecar_path = env
    app_uuid = session_store._app_uuid("api-live")
    sidecar_path.write_text(json.dumps({app_uuid: {"_hermes_id": "api-live"}}))

    db, side, prof = _patched(db_path, sidecar_path)
    with db, side, prof:
        assert session_store.session_title(app_uuid) == "Sup homie."


def test_draft_mapping_becomes_an_alias_without_changing_list_total(env):
    """A compose UUID resolves, but cannot become a second list row."""
    db_path, sidecar_path = env
    draft_id = "00000000-0000-4000-8000-000000000001"
    canonical_id = session_store._app_uuid("api-live")
    db, side, prof = _patched(db_path, sidecar_path)
    with db, side, prof:
        session_store._persist_hermes_mapping(draft_id, "api-live")
        assert session_store._canonical_app_id(draft_id) == canonical_id
        assert session_store._resolve_hermes_id(draft_id) == "api-live"
        messages = session_store.session_messages(draft_id)
        sessions, total = session_store.session_list(limit=50)

    sidecar = json.loads(sidecar_path.read_text())
    assert sidecar[draft_id]["_alias_of"] == canonical_id
    assert sidecar[draft_id]["tombstone"] is True
    assert [message["text"] for message in messages] == ["Sup homie."]
    assert [session["id"] for session in sessions].count(canonical_id) == 1
    assert total == 2


@pytest.mark.parametrize("text", [
    "Operation interrupted.",
    " Operation interrupted: handling API error (500) ",
    "Operation interrupted during retry",
])
def test_interrupt_sentinels_are_not_treated_as_answers(text):
    assert _is_interrupt_sentinel(text)


def test_normal_reply_mentioning_interruption_is_not_a_sentinel():
    assert not _is_interrupt_sentinel("The operation interrupted earlier, but it is fixed now.")


def test_derived_title_is_none_for_an_empty_session(env):
    """A session with no user messages has nothing to derive from."""
    db_path, _ = env
    with patch("herald_connector.session_store._db_path", return_value=db_path):
        assert session_store._derived_title("api-does-not-exist") is None


def test_derived_title_truncates_long_openers(env):
    """Long first messages are cut to a list-friendly length."""
    db_path, _ = env
    conn = sqlite3.connect(str(db_path))
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp, active) "
        "VALUES ('api-long', 'user', ?, 6000.0, 1)",
        ("word " * 60,),
    )
    conn.commit()
    conn.close()

    with patch("herald_connector.session_store._db_path", return_value=db_path):
        title = session_store._derived_title("api-long")

    assert title is not None
    assert len(title) <= 61  # 60 chars + the ellipsis
    assert title.endswith("…")
