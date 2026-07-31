"""Historical reasoning in session_messages() — F3c "persisted but collapsed".

Chain-of-thought is returned for past assistant turns so the collapsed
"Thought process" block survives a conversation refresh instead of vanishing
when the stream ends.  Because the app re-fetches the whole conversation on a
~30 s timer, the payload is capped per message and budgeted per conversation.

Covers:
  - reasoning is attached when the column exists
  - a state.db WITHOUT reasoning_content still loads (no OperationalError)
  - include_reasoning=False skips the transfer
  - per-message cap truncates and marks
  - per-conversation budget spends newest-first
"""

from __future__ import annotations

import sqlite3
import tempfile
from pathlib import Path
from unittest.mock import patch

import pytest

from herald_connector import session_store


def _make_db(db_path: Path, *, with_reasoning: bool) -> sqlite3.Connection:
    conn = sqlite3.connect(str(db_path))
    conn.row_factory = sqlite3.Row
    reasoning_col = "reasoning_content TEXT," if with_reasoning else ""
    conn.execute(f"""
        CREATE TABLE messages (
            id INTEGER PRIMARY KEY,
            session_id TEXT,
            role TEXT,
            content TEXT,
            {reasoning_col}
            timestamp REAL,
            active INTEGER DEFAULT 1,
            compacted INTEGER DEFAULT 0
        )
    """)
    conn.commit()
    return conn


def _insert(conn, session_id, rows, *, with_reasoning: bool):
    """rows: (role, content, reasoning, timestamp)."""
    for role, content, reasoning, ts in rows:
        if with_reasoning:
            conn.execute(
                "INSERT INTO messages (session_id, role, content, reasoning_content, "
                "timestamp, active) VALUES (?, ?, ?, ?, ?, 1)",
                (session_id, role, content, reasoning, ts),
            )
        else:
            conn.execute(
                "INSERT INTO messages (session_id, role, content, timestamp, active) "
                "VALUES (?, ?, ?, ?, 1)",
                (session_id, role, content, ts),
            )
    conn.commit()


def _load(db_path: Path, session_id: str, **kwargs):
    with patch.object(session_store, "_db_path", return_value=db_path), \
         patch.object(session_store, "_resolve_hermes_id", return_value=None):
        return session_store.session_messages(session_id, **kwargs)


SESSION = "api-reasoning-test"


def test_reasoning_attached_when_column_present():
    with tempfile.TemporaryDirectory() as tmp:
        db = Path(tmp) / "state.db"
        conn = _make_db(db, with_reasoning=True)
        _insert(conn, SESSION, [
            ("user", "why is the sky blue?", None, 1000.0),
            ("assistant", "Rayleigh scattering.", "Let me recall the physics…", 1001.0),
        ], with_reasoning=True)

        msgs = _load(db, SESSION)

        assert [m["role"] for m in msgs] == ["user", "herald"]
        assert msgs[1]["reasoning"] == "Let me recall the physics…"
        # A user turn never carries reasoning.
        assert msgs[0]["reasoning"] == ""


def test_db_without_reasoning_column_still_loads():
    """An older state.db must not take all conversation loading down."""
    with tempfile.TemporaryDirectory() as tmp:
        db = Path(tmp) / "state.db"
        conn = _make_db(db, with_reasoning=False)
        _insert(conn, SESSION, [
            ("user", "hello", None, 1000.0),
            ("assistant", "hi there", None, 1001.0),
        ], with_reasoning=False)

        msgs = _load(db, SESSION)

        assert len(msgs) == 2
        assert all(m["reasoning"] == "" for m in msgs)


def test_include_reasoning_false_skips_transfer():
    with tempfile.TemporaryDirectory() as tmp:
        db = Path(tmp) / "state.db"
        conn = _make_db(db, with_reasoning=True)
        _insert(conn, SESSION, [
            ("assistant", "answer", "a long private deliberation", 1001.0),
        ], with_reasoning=True)

        msgs = _load(db, SESSION, include_reasoning=False)

        assert msgs[0]["reasoning"] == ""


def test_per_message_cap_truncates_and_marks():
    long_reasoning = "x" * (session_store._REASONING_MAX_CHARS + 5000)
    with tempfile.TemporaryDirectory() as tmp:
        db = Path(tmp) / "state.db"
        conn = _make_db(db, with_reasoning=True)
        _insert(conn, SESSION, [
            ("assistant", "answer", long_reasoning, 1001.0),
        ], with_reasoning=True)

        msgs = _load(db, SESSION)
        got = msgs[0]["reasoning"]

        assert got.endswith(session_store._REASONING_TRUNCATED)
        assert len(got) == session_store._REASONING_MAX_CHARS + len(
            session_store._REASONING_TRUNCATED
        )


def test_budget_is_spent_newest_first():
    """Oldest turns give up their reasoning once the budget is exhausted."""
    per_msg = session_store._REASONING_MAX_CHARS
    # Enough messages to blow the whole-conversation budget several times over.
    count = (session_store._REASONING_BUDGET_CHARS // per_msg) + 4
    rows = [
        ("assistant", f"answer {i}", "y" * per_msg, 1000.0 + i)
        for i in range(count)
    ]
    with tempfile.TemporaryDirectory() as tmp:
        db = Path(tmp) / "state.db"
        conn = _make_db(db, with_reasoning=True)
        _insert(conn, SESSION, rows, with_reasoning=True)

        msgs = _load(db, SESSION)

        assert len(msgs) == count
        total = sum(len(m["reasoning"]) for m in msgs)
        assert total <= session_store._REASONING_BUDGET_CHARS
        # Newest kept, oldest dropped — the turns a user would expand survive.
        assert msgs[-1]["reasoning"] != ""
        assert msgs[0]["reasoning"] == ""


def test_budget_leaves_typical_conversation_untouched():
    """A normal phone chat (3-11 msgs, ~553 chars mean) is never trimmed."""
    rows = [
        ("assistant", f"answer {i}", "z" * 553, 1000.0 + i)
        for i in range(11)
    ]
    with tempfile.TemporaryDirectory() as tmp:
        db = Path(tmp) / "state.db"
        conn = _make_db(db, with_reasoning=True)
        _insert(conn, SESSION, rows, with_reasoning=True)

        msgs = _load(db, SESSION)

        assert all(len(m["reasoning"]) == 553 for m in msgs)
