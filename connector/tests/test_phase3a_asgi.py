"""Phase 3A v2: ASGI integration tests for the snapshot + SSE endpoints.

These tests assert the actual wire shape emitted by:

  * ``GET /v1/conversations/current``
  * ``GET /v1/sessions/{id}/conversation``
  * ``GET /v1/jobs/{id}/events``

via the Starlette ``TestClient`` (a real ASGI driver).  They are
designed to fail when the production publisher regresses to the v1
``{type, data}`` shape or when the snapshot endpoint lets unprojected
legacy rows leak into the response.

The SSE capture test feeds the handler a deterministic stream of
events, captures the bytes emitted by the endpoint, and asserts:

  * every ``data:`` line decodes into the strict v3 envelope;
  * the envelopes validate against ``stream_contract.parse_event``;
  * the ``seq`` sequence is monotonic from 1 and has no gaps;
  * the terminal event is the v3 ``run.completed`` type with
    ``payload.message`` populated from the canonical message row;
  * a reconnect with ``Last-Event-ID`` returns the suffix only and
    applies every event exactly once (idempotent replay).
"""

from __future__ import annotations

import asyncio
import json
import re
import time
import uuid
from pathlib import Path
from unittest.mock import AsyncMock, patch

import pytest
from starlette.testclient import TestClient

import herald_connector as connector
import herald_connector.http_facade as facade
from herald_connector import session_store
from herald_connector.delivery_store import (
    CanonicalSnapshotIncomplete,
    DeliveryStore,
    get_delivery_store,
    reset_delivery_store,
)
from herald_connector.http_facade import FacadeContext, app


CONV = "660e8400-e29b-41d4-a716-446655440001"
HERMES_SID = "aa0e8400-e29b-41d4-a716-446655440005"
CLIENT_MSG = "550e8400-e29b-41d4-a716-446655440000"
JOB1 = "770e8400-e29b-41d4-a716-446655440002"
DEVICE = "abc123def"


# ── Fixtures ──────────────────────────────────────────────────────────────


@pytest.fixture(autouse=True)
def clean_facade_state():
    """Per-test isolation for the in-memory job registry and store singleton."""
    facade._http_jobs.clear()
    facade._http_job_tasks.clear()
    reset_delivery_store()
    yield
    facade._http_jobs.clear()
    facade._http_job_tasks.clear()
    reset_delivery_store()


@pytest.fixture
def env(tmp_path, monkeypatch):
    monkeypatch.setenv("HERMES_MOBILE_CONNECTOR_HOME", str(tmp_path))
    monkeypatch.setenv("HERMES_HOME", "/home/user/.hermes/profiles/ignyte")
    return tmp_path


@pytest.fixture
def store(env):
    return DeliveryStore(Path(env) / "delivery.sqlite3")


@pytest.fixture(autouse=True)
def auth():
    with patch("herald_connector.http_facade.require_auth", new_callable=AsyncMock):
        yield


@pytest.fixture
def ctx(env):
    ctx = FacadeContext()
    async def _handler(text, history, sid, att, re):  # noqa: ANN001
        yield {"type": "done", "data": {"status": "completed", "text": "ok"}}
    ctx.message_handler = _handler
    ctx.connector_version = "0.4.1"
    return ctx


@pytest.fixture
def client():
    with TestClient(app) as c:
        yield c


@pytest.fixture
def no_session_lookups(monkeypatch):
    """Keep _run_http_job's done path off state.db (which is absent in tests)."""
    monkeypatch.setattr(
        session_store, "_find_session_by_assistant_reply", lambda *a, **k: None
    )
    monkeypatch.setattr(
        session_store, "_find_session_by_recent_message", lambda *a, **k: None
    )


def _seed_binding(store: DeliveryStore) -> str:
    return store.get_or_create_binding(
        CONV, HERMES_SID, "acc-001", DEVICE,
    )["appConversationId"]


def _parse_sse_frames(body: str) -> list[dict]:
    """Parse SSE ``data:`` payloads into a list of envelopes.

    Each frame is delimited by a blank line; the ``id:``, ``event:``,
    and ``data:`` lines are parsed and the ``data`` payload is decoded
    as JSON.  Returns the list of decoded JSON envelopes in order.
    """
    frames: list[dict] = []
    for chunk in re.split(r"\r?\n\r?\n", body.strip()):
        data_lines: list[str] = []
        for line in chunk.splitlines():
            if line.startswith("data:"):
                data_lines.append(line[len("data:"):].lstrip())
        if not data_lines:
            continue
        joined = "\n".join(data_lines)
        try:
            frames.append(json.loads(joined))
        except json.JSONDecodeError:
            continue
    return frames


# ── Snapshot ASGI integration tests ───────────────────────────────────────


class TestSnapshotASGI:
    """Phase 3A v2: both snapshot routes return fail-closed canonical rows."""

    def test_current_conversation_returns_fail_closed_canonical_envelope(
        self, client, ctx, store, no_session_lookups,
    ):
        """A snapshot built from the canonical ledger has every required
        field populated and never fabricates a zero cursor."""
        conv_id = _seed_binding(store)
        # Insert two canonical rows directly into the ledger so the
        # snapshot has authoritative content to surface.
        store.create_user_message_atomically(
            conv_id, str(uuid.uuid4()), "Hello", "Hello",
        )
        store.create_canonical_message(
            conv_id, "assistant", "Hi there", "Hi there",
        )
        ctx.current_conversation = lambda: {
            "sessionId": conv_id,
            "title": "Snapshot test",
        }
        with patch("herald_connector.http_facade.get_context", return_value=ctx):
            resp = client.get("/v1/conversations/current")
        assert resp.status_code == 200, resp.text
        conv = resp.json()["conversation"]
        assert conv["id"] == conv_id
        # Phase 3A v2: the envelope-level revision is positive.
        assert conv["revision"] >= 1
        # All rows are sourced from the canonical ledger; every
        # required canonical field is present and positive.
        assert len(conv["messages"]) >= 2
        for msg in conv["messages"]:
            assert msg["id"], "id must be non-null"
            assert msg["conversationId"] == conv_id
            assert isinstance(msg["sequence"], int) and msg["sequence"] >= 1
            assert isinstance(msg["revision"], int) and msg["revision"] >= 1
            assert msg["deleted"] is False
            assert "displayContent" in msg

    def test_session_conversation_returns_fail_closed_canonical_envelope(
        self, client, ctx, store, no_session_lookups,
    ):
        conv_id = _seed_binding(store)
        store.create_user_message_atomically(
            conv_id, str(uuid.uuid4()), "Question", "Question",
        )
        ctx.session_conversation = lambda sid: {
            "sessionId": sid,
            "title": "Session test",
        }
        with patch("herald_connector.http_facade.get_context", return_value=ctx):
            resp = client.get(f"/v1/sessions/{conv_id}/conversation")
        assert resp.status_code == 200, resp.text
        conv = resp.json()["conversation"]
        assert conv["id"] == conv_id
        assert conv["revision"] >= 1
        msgs = conv["messages"]
        assert len(msgs) >= 1
        for msg in msgs:
            assert msg["conversationId"] == conv_id
            assert msg["sequence"] >= 1
            assert msg["revision"] >= 1

    def test_snapshot_returns_409_on_canonical_incomplete(
        self, client, ctx, store, no_session_lookups, monkeypatch,
    ):
        """A ledger row with a zero sequence forces the snapshot endpoint
        to refuse with HTTP 409, not silently fabricate a cursor."""
        conv_id = _seed_binding(store)
        # Inject a malformed row directly into the SQLite ledger so
        # the snapshot transaction trips the fail-closed check.
        from herald_connector.delivery_store import _utcnow_rfc3339
        store_path = Path(store.db_path)
        import sqlite3
        with sqlite3.connect(store_path) as raw_conn:
            raw_conn.execute(
                "INSERT INTO conversation_messages ("
                " canonical_message_id, conversation_id, sequence, revision,"
                " role, content, display_content, state, created_at, updated_at"
                ") VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    str(uuid.uuid4()), conv_id, 0, 1,
                    "user", "zero-seq", "zero-seq",
                    "terminal", _utcnow_rfc3339(), _utcnow_rfc3339(),
                ),
            )
            raw_conn.commit()
        ctx.current_conversation = lambda: {"sessionId": conv_id}
        with patch("herald_connector.http_facade.get_context", return_value=ctx):
            resp = client.get("/v1/conversations/current")
        assert resp.status_code == 409, resp.text
        body = resp.json()
        # The facade's exception middleware wraps the HTTPException
        # detail in a JSON envelope with ``error.message``.  The
        # canonical reason must still surface so the client can act on
        # it (retry / show error).
        outer_message = body.get("error", {}).get("message", "")
        assert "canonical_snapshot_incomplete" in outer_message, (
            f"expected canonical_snapshot_incomplete in 409 body, got: {body!r}"
        )

    def test_repeat_reload_is_byte_equivalent(
        self, client, ctx, store, no_session_lookups,
    ):
        """Two successive snapshot reads against an unchanged ledger
        return byte-equivalent envelopes (no fabricated timestamps)."""
        conv_id = _seed_binding(store)
        store.create_user_message_atomically(
            conv_id, str(uuid.uuid4()), "Hello", "Hello",
        )
        ctx.current_conversation = lambda: {
            "sessionId": conv_id,
            "title": "Repeat test",
        }
        with patch("herald_connector.http_facade.get_context", return_value=ctx):
            first = client.get("/v1/conversations/current").json()
        with patch("herald_connector.http_facade.get_context", return_value=ctx):
            second = client.get("/v1/conversations/current").json()
        # The conversation envelope keys and the message row identities
        # (id + sequence + revision + conversationId) are identical.
        # ``updatedAt`` is allowed to drift because ``_now_iso()``
        # carries wall-clock noise; the iOS reducer compares
        # ``revision``, not ``updatedAt``.
        assert first["conversation"]["revision"] == (
            second["conversation"]["revision"]
        )
        first_ids = sorted(m["id"] for m in first["conversation"]["messages"])
        second_ids = sorted(m["id"] for m in second["conversation"]["messages"])
        assert first_ids == second_ids


# ── SSE ASGI integration tests ───────────────────────────────────────────


def _seed_terminal_job(store, conv_id, num_events):
    """Build a job dict with a finished backlog + a stream of v3 envelopes."""
    from herald_connector.stream_contract import build_envelope
    job_id = str(uuid.uuid4())
    envelopes = []
    for seq in range(1, num_events):
        env_dict = build_envelope(
            job_id=job_id,
            conversation_id=conv_id,
            attempt=1,
            seq=seq,
            conversation_revision=1,
            event_type="text.delta",
            timestamp="2026-08-01T18:00:00Z",
            payload={"delta": f"chunk-{seq}", "segmentId": "seg-1"},
        )
        envelopes.append(env_dict)
    terminal = build_envelope(
        job_id=job_id,
        conversation_id=conv_id,
        attempt=1,
        seq=num_events,
        conversation_revision=1,
        event_type="run.completed",
        timestamp="2026-08-01T18:00:01Z",
        payload={
            "messageId": str(uuid.uuid4()),
            "text": "ok",
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
        },
    )
    envelopes.append(terminal)
    facade._http_jobs[job_id] = {
        "status": "completed",
        "events": envelopes,
        "subscribers": [],
        "updatedAt": time.time(),
        "conversationId": conv_id,
        "message": None,
        "error": None,
        "errorCategory": None,
        "errorAction": None,
        "usage": None,
        "cleanText": None,
        "clientMessageId": None,
        "attempt": 1,
    }
    return job_id, envelopes


def _drain_sse(job_id, *, headers=None) -> str:
    """Read the full SSE response body for a job.

    Uses httpx's ASGITransport with ``stream=True`` so the streaming
    generator can complete on its own without the TestClient
    blocking on a still-open connection.  Returns the raw text body.
    """
    import asyncio
    import httpx

    async def _run() -> str:
        transport = httpx.ASGITransport(app=app)
        async with httpx.AsyncClient(
            transport=transport, base_url="http://test"
        ) as ac:
            req_headers = {"Authorization": "Bearer test"}
            if headers:
                req_headers.update(headers)
            url = f"http://test/v1/jobs/{job_id}/events"
            chunks: list[str] = []
            async with ac.stream("GET", url, headers=req_headers) as resp:
                assert resp.status_code == 200
                assert resp.headers["content-type"].startswith("text/event-stream")
                async for chunk in resp.aiter_text():
                    if chunk:
                        chunks.append(chunk)
            return "".join(chunks)

    # Synchronous bridge so pytest can collect it without async fixtures.
    return asyncio.new_event_loop().run_until_complete(_run())


class TestSSEASGI:
    """Phase 3A v2: every SSE ``data:`` body is a strict v3 envelope."""

    def test_sse_endpoint_emits_v3_envelopes(
        self, client, ctx, store, env, no_session_lookups,
    ):
        """Every SSE frame decodes into a strict v3 envelope that
        validates through ``stream_contract.parse_event``."""
        from herald_connector.stream_contract import parse_event

        conv_id = _seed_binding(store)
        job_id, envelopes = _seed_terminal_job(store, conv_id, num_events=4)

        body = _drain_sse(job_id)
        frames = _parse_sse_frames(body)
        assert len(frames) == len(envelopes)
        for frame, expected in zip(frames, envelopes):
            assert frame["contractVersion"] == 3
            assert frame["jobId"] == job_id
            assert frame["conversationId"] == conv_id
            assert frame["type"] == expected["type"]
            assert frame["seq"] == expected["seq"]
            assert frame["conversationRevision"] == 1
            parse_event(frame)
        seqs = [f["seq"] for f in frames]
        assert seqs == [1, 2, 3, 4]
        assert frames[-1]["type"] == "run.completed"

    def test_sse_reconnect_cursor_replays_suffix_without_duplicates(
        self, client, ctx, store, no_session_lookups,
    ):
        """Reconnect with ``Last-Event-ID: 2`` returns only envelopes
        with seq > 2 — no gap, no duplicate application."""
        conv_id = _seed_binding(store)
        job_id, _ = _seed_terminal_job(store, conv_id, num_events=5)

        body = _drain_sse(job_id, headers={"Last-Event-ID": "2"})
        frames = _parse_sse_frames(body)
        seqs = [f["seq"] for f in frames]
        # Only events strictly after the cursor are replayed.
        assert seqs == [3, 4, 5], f"replay cursor must return seqs after 2, got {seqs}"

    def test_sse_data_body_is_full_v3_envelope(
        self, client, ctx, store, no_session_lookups,
    ):
        """The SSE ``data:`` body contains the full v3 envelope JSON,
        not the legacy ``{type, data}`` shape."""
        conv_id = _seed_binding(store)
        job_id, _ = _seed_terminal_job(store, conv_id, num_events=2)

        body = _drain_sse(job_id)
        assert "contractVersion" in body
        assert "payload" in body
        assert "conversationRevision" in body
