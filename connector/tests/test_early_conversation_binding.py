"""B20: bind a conversation to its Hermes session at run start, not completion.

Regression test for the split-conversation bug seen on 2026-07-30 23:30.

POST /v1/messages resolves an incoming conversationId through the
app_uuid → hermes_id mapping.  That mapping used to be written only when a job
*completed*, so a follow-up sent while the agent was still working resolved to
nothing, went out with no session_id, and Hermes minted a brand new session:

    23:30:04  run_f97c917b  "What's sole funny hood classics missing…"
    23:30:27  run_3e6bd5c1  "What's some funny hood classics missing"   <- forked
    23:31:09  run_3e6bd5c1  reply
    23:33:02  run_f97c917b  reply                                       <- out of order

One chat, two Hermes sessions, replies interleaved in the transcript.
"""

from __future__ import annotations

import sqlite3

import pytest

from kallisti_connector import http_facade, session_store


HERMES_SID = "run_f97c917b0219421f8e7f4da5786db3bc"
USER_TEXT = "What's sole funny hood classics missing from the NinjaFlix library?"
CONV_ID = "d542e200-b786-4dda-8913-65edacb32f5e"


@pytest.fixture
def live_db(tmp_path, monkeypatch):
    """state.db with the user turn already written, as at run start.

    B33 WS B: _bind_conversation_early also mirrors the binding into the
    SQLite delivery store, so its home directory must be env-isolated or
    the tests would write to the real host store.
    """
    monkeypatch.setenv("HERMES_MOBILE_CONNECTOR_HOME", str(tmp_path))
    db = tmp_path / "state.db"
    conn = sqlite3.connect(db)
    conn.row_factory = sqlite3.Row
    conn.execute(
        "CREATE TABLE messages ("
        "  id INTEGER PRIMARY KEY AUTOINCREMENT, session_id TEXT NOT NULL,"
        "  role TEXT NOT NULL, content TEXT, timestamp REAL NOT NULL,"
        "  active INTEGER NOT NULL DEFAULT 1)"
    )
    conn.execute(
        "INSERT INTO messages (session_id, role, content, timestamp, active) "
        "VALUES (?, 'user', ?, 1000.0, 1)",
        (HERMES_SID, USER_TEXT),
    )
    conn.commit()
    conn.close()

    def _connect():
        c = sqlite3.connect(db)
        c.row_factory = sqlite3.Row
        return c

    monkeypatch.setattr(session_store, "_connect", _connect)
    persisted: dict[str, str] = {}
    monkeypatch.setattr(
        session_store, "_persist_hermes_mapping",
        lambda app_id, hermes_id: persisted.__setitem__(app_id, hermes_id),
    )
    return persisted


def test_binds_from_the_reported_session_id(live_db):
    """The run event carries the session id — bind immediately."""
    job = {"conversationId": CONV_ID}
    assert http_facade._bind_conversation_early(
        job, USER_TEXT, 900.0, {"sessionId": HERMES_SID}
    ) is True
    assert live_db[CONV_ID] == HERMES_SID


def test_binds_from_state_db_when_the_event_carries_no_session(live_db):
    """Hermes writes the user turn at run start, so state.db can answer."""
    job = {"conversationId": CONV_ID}
    assert http_facade._bind_conversation_early(
        job, USER_TEXT, 900.0, {}
    ) is True
    assert live_db[CONV_ID] == HERMES_SID


def test_reports_unbound_when_the_session_does_not_exist_yet(live_db):
    """Caller must keep retrying rather than record a wrong mapping."""
    job = {"conversationId": CONV_ID}
    assert http_facade._bind_conversation_early(
        job, "a message that was never written", 900.0, {}
    ) is False
    assert live_db == {}


def test_respects_the_job_window(live_db):
    """A turn from before this job started must not be claimed."""
    job = {"conversationId": CONV_ID}
    assert http_facade._bind_conversation_early(
        job, USER_TEXT, 5000.0, {}
    ) is False


def test_binding_makes_a_follow_up_resolve_to_the_same_session(live_db):
    """The point of the fix: the next message must not fork a new session."""
    job = {"conversationId": CONV_ID}
    http_facade._bind_conversation_early(job, USER_TEXT, 900.0, {})
    # POST /v1/messages resolves the conversation id through this mapping;
    # before the fix it returned None here and Hermes minted a new session.
    assert live_db.get(CONV_ID) == HERMES_SID
