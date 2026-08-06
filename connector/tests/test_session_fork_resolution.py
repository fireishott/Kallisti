"""B19: resolve a turn's session by its reply, not by the user's prompt.

Regression test for the "no response" bug reproduced live on 2026-07-30.

When Hermes truncates a response it continues itself in a *new run*, and it
names a session after each run id.  The user's prompt stays in the first
session while the continuation prompt and the actual answer land in the
second.  Anchoring the app conversation on the user's text therefore maps it
to a session containing no answer — the reply is not lost, it is filed where
the client never looks.

Captured from the real state.db rows behind the report:

    run_2e81373a…  user      "Big homie.  Give me some fresh hood comedy…"
    run_6ef67ead…  user      "[Your previous response was cut off. …]"
    run_6ef67ead…  assistant "Aight, here's the rotation for tonight. …"
"""

from __future__ import annotations

import sqlite3

import pytest

from kallisti_connector import session_store


USER_TEXT = "Big homie.  Give me some fresh hood comedy to watch tonight."
CONTINUATION = (
    '[Your previous response was cut off. It ended with: "Let me pull up some '
    'good ones for you.". Continue from where you stopped.]'
)
REPLY = (
    "Aight, here's the rotation for tonight. Mixing fresh 2025 drops with some "
    "you might've slept on:\n\nNEW THIS YEAR (2025):\n\n1. One of Them Days"
)
PROMPT_SESSION = "run_2e81373aa0c04f1a8557d1d5a50b2f2d"
REPLY_SESSION = "run_6ef67eadf9cc49bb97b8b54e621229ca"


@pytest.fixture
def forked_db(tmp_path, monkeypatch):
    """A state.db holding one turn split across two run-named sessions."""
    db = tmp_path / "state.db"
    conn = sqlite3.connect(db)
    conn.row_factory = sqlite3.Row
    conn.execute(
        "CREATE TABLE messages ("
        "  id INTEGER PRIMARY KEY AUTOINCREMENT,"
        "  session_id TEXT NOT NULL,"
        "  role TEXT NOT NULL,"
        "  content TEXT,"
        "  timestamp REAL NOT NULL,"
        "  active INTEGER NOT NULL DEFAULT 1)"
    )
    rows = [
        (PROMPT_SESSION, "user", USER_TEXT, 1000.0),
        (REPLY_SESSION, "user", CONTINUATION, 1001.0),
        (REPLY_SESSION, "assistant", REPLY, 1002.0),
    ]
    conn.executemany(
        "INSERT INTO messages (session_id, role, content, timestamp, active) "
        "VALUES (?, ?, ?, ?, 1)",
        rows,
    )
    conn.commit()
    conn.close()

    def _connect():
        c = sqlite3.connect(db)
        c.row_factory = sqlite3.Row
        return c

    monkeypatch.setattr(session_store, "_connect", _connect)
    return db


def test_prompt_lookup_finds_the_session_without_the_answer(forked_db):
    """The old behaviour — documents precisely why replies went missing."""
    assert session_store._find_session_by_recent_message(
        USER_TEXT, since=900.0
    ) == PROMPT_SESSION


def test_reply_lookup_follows_the_fork(forked_db):
    """The fix: anchoring on the reply finds the session holding the answer."""
    assert session_store._find_session_by_assistant_reply(
        REPLY, since=900.0
    ) == REPLY_SESSION


def test_reply_lookup_tolerates_trailing_drift(forked_db):
    """Streamed text can differ from what Hermes persisted at the tail.

    Reasoning stripping and whitespace normalisation mean exact equality is not
    guaranteed, so a truncated tail must still resolve via the prefix match.
    """
    assert session_store._find_session_by_assistant_reply(
        REPLY[:140], since=900.0
    ) == REPLY_SESSION


def test_reply_lookup_respects_the_job_window(forked_db):
    """A reply written before the job started must never be claimed."""
    assert session_store._find_session_by_assistant_reply(
        REPLY, since=5000.0
    ) is None


def test_reply_lookup_returns_none_when_there_is_no_reply(forked_db):
    """Tool-only or reasoning-only turns fall back to the prompt lookup."""
    assert session_store._find_session_by_assistant_reply(
        "", since=900.0
    ) is None
    assert session_store._find_session_by_assistant_reply(
        "text that was never persisted", since=900.0
    ) is None


def test_reply_lookup_does_not_treat_wildcards_as_patterns(forked_db):
    """A reply containing % or _ must not widen the LIKE match."""
    assert session_store._find_session_by_assistant_reply(
        "100% _ nothing that was ever written to this database", since=900.0
    ) is None


def test_short_replies_do_not_prefix_match(forked_db):
    """Very short text is too weak an anchor to risk a prefix match on."""
    assert session_store._find_session_by_assistant_reply(
        "Aight", since=900.0
    ) is None


def test_injected_turns_are_not_shown_as_the_users_own_words(tmp_path, monkeypatch):
    """Agent-injected turns must never render as the person's own bubble.

    Hermes stores both of these with role='user' and display_kind NULL, so the
    projection is the only place they can be distinguished.
    """
    db = tmp_path / "state.db"
    conn = sqlite3.connect(db)
    conn.row_factory = sqlite3.Row
    conn.execute(
        "CREATE TABLE messages ("
        "  id INTEGER PRIMARY KEY AUTOINCREMENT,"
        "  session_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT,"
        "  reasoning_content TEXT, timestamp REAL NOT NULL,"
        "  active INTEGER NOT NULL DEFAULT 1, compacted INTEGER NOT NULL DEFAULT 0)"
    )
    conn.executemany(
        "INSERT INTO messages (session_id, role, content, reasoning_content, "
        "timestamp, active, compacted) VALUES (?, ?, ?, '', ?, 1, 0)",
        [
            (REPLY_SESSION, "user", CONTINUATION, 1001.0),
            (REPLY_SESSION, "user", '[IMPORTANT: The user has invoked the "wedding-board" skill]', 1001.5),
            (REPLY_SESSION, "user", "a real question I actually typed", 1001.8),
            (REPLY_SESSION, "assistant", REPLY, 1002.0),
        ],
    )
    conn.commit()
    conn.close()

    def _connect():
        c = sqlite3.connect(db)
        c.row_factory = sqlite3.Row
        return c

    monkeypatch.setattr(session_store, "_connect", _connect)
    monkeypatch.setattr(session_store, "_canonical_app_id", lambda s: s)
    monkeypatch.setattr(session_store, "_resolve_hermes_id", lambda s: s)

    msgs = session_store.session_messages(REPLY_SESSION, include_reasoning=False)
    texts = [m.get("text") or m.get("content") or "" for m in msgs]

    assert not any(t.startswith("[Your previous response was cut off") for t in texts)
    assert not any(t.startswith("[IMPORTANT: The user has invoked") for t in texts)
    # The real turn and the reply must both survive the filter.
    assert any("a real question I actually typed" in t for t in texts)
    assert any(t.startswith("Aight, here's the rotation") for t in texts)
