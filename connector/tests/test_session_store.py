"""Tests for T6: compaction summary filtering in session_messages().

Covers:
  - [Recent Summary (d0, node N)] rows are excluded
  - [Session Arc Summary (d1, node N)] rows are excluded
  - [Current user objective preserved from compacted history] rows are excluded
  - Normal messages are NOT filtered out
  - [SILENT] messages are NOT filtered (they're functional, not compaction)
"""

from __future__ import annotations

import sqlite3
import tempfile
from pathlib import Path
from unittest.mock import patch

import pytest

from herald_connector import session_store


def _make_test_db(db_path: Path) -> sqlite3.Connection:
    """Create a test state.db with messages including compaction summaries."""
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
    conn.commit()
    return conn


def _insert_messages(conn: sqlite3.Connection, session_id: str, messages: list[tuple[str, str, str]]):
    """Insert messages: (role, content, timestamp_offset)."""
    for role, content, ts in messages:
        conn.execute(
            "INSERT INTO messages (session_id, role, content, timestamp, active) VALUES (?, ?, ?, ?, 1)",
            (session_id, role, content, ts),
        )
    conn.commit()


@pytest.fixture
def test_db():
    """Temporary state.db with known messages including compaction summaries."""
    with tempfile.TemporaryDirectory() as tmpdir:
        db_path = Path(tmpdir) / "state.db"
        conn = _make_test_db(db_path)
        sess = "test-session-compaction"

        _insert_messages(conn, sess, [
            ("user", "Hello, how are you?", 1000.0),
            ("assistant", "I'm doing great, thanks!", 1001.0),
            ("user", "What's the weather?", 1002.0),
            # Compaction summary — should be EXCLUDED
            ("assistant", "[Recent Summary (d0, node 458)]\n[USER]: Hello\n[ASSISTANT]: Hi there", 1003.0),
            ("assistant", "The weather is sunny today.", 1004.0),
            # Session arc summary — should be EXCLUDED
            ("assistant", "[Session Arc Summary (d1, node 500)]\nSession overview...", 1005.0),
            # Objective preserved — should be EXCLUDED
            ("assistant", "[Current user objective preserved from compacted history]\nThe objective is to write tests.", 1006.0),
            # CONTEXT COMPACTION — should be EXCLUDED
            ("assistant", "[CONTEXT COMPACTION: d0, node 123]\nSummary of context...", 1007.0),
            # CONTEXT SUMMARY — should be EXCLUDED
            ("assistant", "[CONTEXT SUMMARY]:\nSummary content here.", 1008.0),
            # [SILENT] — NOT a compaction summary, must be KEPT
            ("assistant", "[SILENT]", 1009.0),
            ("user", "Final message", 1010.0),
            ("assistant", "Final reply", 1011.0),
        ])

        yield db_path, sess


def test_compaction_summaries_excluded(test_db):
    """Compaction summary rows must be absent from session_messages() output."""
    db_path, session_id = test_db

    with patch("herald_connector.session_store._db_path", return_value=db_path):
        messages = session_store.session_messages(session_id)

    # All normal user/assistant messages should be present
    texts = [m["text"] for m in messages]
    roles = [m["role"] for m in messages]

    # User messages
    assert "Hello, how are you?" in texts
    assert "What's the weather?" in texts
    assert "Final message" in texts

    # Normal assistant messages
    assert "I'm doing great, thanks!" in texts
    assert "The weather is sunny today." in texts
    assert "Final reply" in texts

    # [SILENT] must be kept (functional marker, not compaction)
    assert "[SILENT]" in texts

    # Compaction summaries MUST NOT appear
    for banned_prefix in [
        "[Recent Summary (d0,",
        "[Session Arc Summary (d1,",
        "[Current user objective preserved",
        "[CONTEXT COMPACTION",
        "[CONTEXT SUMMARY]:",
    ]:
        for t in texts:
            assert not t.startswith(banned_prefix), (
                f"Compaction message should be excluded but found: {t[:80]}"
            )


def test_compaction_summaries_also_excluded_in_mixed_session(test_db):
    """Even when mixed with normal messages, all summary types are excluded."""
    db_path, session_id = test_db

    with patch("herald_connector.session_store._db_path", return_value=db_path):
        messages = session_store.session_messages(session_id)

    # Count: should be 8 messages (4 user + 4 real assistant + 0 compaction)
    # 3 user messages: Hello, Weather, Final
    # 4 real assistant: I'm doing great, The weather, [SILENT], Final reply
    # 5 compaction: all excluded
    assert len(messages) == 7, (
        f"Expected 7 messages (3 user + 4 assistant incl [SILENT]), "
        f"got {len(messages)}: {[m['text'][:50] for m in messages]}"
    )

    # Verify no compaction content appears in the texts
    for m in messages:
        assert "[Recent Summary" not in m["text"], f"Leaked compaction: {m['text'][:60]}"
        assert "[Session Arc Summary" not in m["text"], f"Leaked compaction: {m['text'][:60]}"
        assert "compacted history" not in m["text"], f"Leaked compaction: {m['text'][:60]}"
        assert "[CONTEXT COMPACTION" not in m["text"], f"Leaked compaction: {m['text'][:60]}"
        assert "[CONTEXT SUMMARY]" not in m["text"], f"Leaked compaction: {m['text'][:60]}"
