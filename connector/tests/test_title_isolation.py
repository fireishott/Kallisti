"""Tests for T2: title generation must never touch the user's session.

Covers:
  - _auto_title handler is never called with the user's real hermes_sid
  - session_generate_title handler is never called with the user's session_id
  - No title-prompt content lands in the user's message history
"""

from __future__ import annotations

import asyncio
import uuid
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from kallisti_connector.http_facade import _auto_title, _auto_title_and_persist


class FakeHandler:
    """Records every session_id it is called with and yields a simple title text_delta + done."""

    def __init__(self):
        self.calls: list[dict] = []

    async def __call__(
        self, prompt: str, history: list, session_id: str | None,
        attachments: list | None, reasoning_effort: str | None,
    ):
        self.calls.append({
            "prompt": prompt,
            "session_id": session_id,
        })
        yield {"type": "text_delta", "data": {"delta": "Test"}}
        yield {"type": "text_delta", "data": {"delta": " Title"}}
        yield {"type": "done", "data": {"text": "Test Title"}}


@pytest.mark.asyncio
async def test_auto_title_never_uses_real_session():
    """The handler must never be called with the user's real hermes_sid."""
    handler = FakeHandler()
    user_session = "api-testsession-12345"

    title = await _auto_title(
        handler,
        text="Hello, how are you today?",
        hermes_sid=user_session,
        app_uuid="app-uuid-12345",
    )

    # Handler should have been called (to generate the title)
    assert len(handler.calls) == 1, "Handler should be called once for title generation"

    # But it must NOT have been called with the user's real session id
    actual_session = handler.calls[0]["session_id"]
    assert actual_session != user_session, (
        f"Handler was called with user's real session_id '{user_session}'. "
        f"Title generation must use a throwaway session."
    )

    # The throwaway session should be a title-prefixed UUID or similar isolated ID
    assert actual_session is not None
    assert "title-" in actual_session or "-title-" in actual_session or actual_session.startswith("title"), (
        f"Expected throwaway session to include 'title' marker, got: {actual_session}"
    )

    # Title should have been extracted from the handler response
    assert title == "Test Title"


@pytest.mark.asyncio
async def test_auto_title_and_persist_uses_throwaway_session():
    """_auto_title_and_persist must route through a throwaway session, not the user's."""
    handler = FakeHandler()
    user_session = "20260728_204452_5ffc8e"  # Real session format

    # set_session_meta is imported at call time from session_store
    with patch("kallisti_connector.session_store.set_session_meta") as mock_set_meta:
        await _auto_title_and_persist(
            handler,
            text="Sup G",
            hermes_sid=user_session,
            app_uuid="canonical-app-uuid",
        )

    # Handler should have been called
    assert len(handler.calls) == 1

    # Verify the handler was NOT called with the user's real session
    actual_session = handler.calls[0]["session_id"]
    assert actual_session != user_session, (
        f"_auto_title_and_persist leaked user session '{user_session}' into handler"
    )

    # set_session_meta should still target the correct app_uuid
    mock_set_meta.assert_called_once()
    call_args = mock_set_meta.call_args
    assert call_args[0][0] == "canonical-app-uuid", (
        "set_session_meta must target the real app_uuid, not the throwaway session"
    )


@pytest.mark.asyncio
async def test_no_title_prompt_leaks_to_session_store():
    """The title prompt text must never appear in any persistence path for the user session."""
    handler = FakeHandler()
    user_session = "api-testsession-leakcheck"

    with patch("kallisti_connector.session_store.set_session_meta") as mock_set_meta:
        await _auto_title_and_persist(
            handler,
            text="User's real message",
            hermes_sid=user_session,
            app_uuid="app-leakcheck",
        )

    # set_session_meta should only be called with the real app_uuid and a title,
    # never with the title prompt text
    mock_set_meta.assert_called_once()
    call_args = mock_set_meta.call_args
    assert call_args[0][0] == "app-leakcheck"
    # The title value should be "Test Title", not "Generate a short title..."
    assert "title" in call_args[1]
    assert "Generate a short title" not in str(call_args[1]["title"])


@pytest.mark.asyncio
async def test_auto_title_handles_handler_error_gracefully():
    """_auto_title falls back to truncation when the handler fails; does not crash."""
    # Use an async generator that raises internally
    async def broken_handler(prompt, history, session_id, attachments, reasoning_effort):
        raise RuntimeError("Handler exploded")
        yield  # unreachable

    title = await _auto_title(
        broken_handler,
        text="Hello, how are you?",
        hermes_sid="api-fail-session",
        app_uuid="app-fail",
    )
    # Should fall back to truncation, not crash
    assert title is not None
    assert title == "Hello, how are you?"


@pytest.mark.asyncio
async def test_two_calls_use_different_throwaway_sessions():
    """Each title generation should use its own isolated session."""
    handler = FakeHandler()

    await _auto_title(handler, "First message", "real-session-1", "app-1")
    await _auto_title(handler, "Second message", "real-session-2", "app-2")

    assert len(handler.calls) == 2
    sid1 = handler.calls[0]["session_id"]
    sid2 = handler.calls[1]["session_id"]

    # Neither should be the real session
    assert sid1 != "real-session-1"
    assert sid2 != "real-session-2"

    # They should be different from each other (each title gen isolated)
    assert sid1 != sid2, "Each title generation must use its own isolated session"


# ── T3: per-session lock serialization ──────────────────────────────────────

import time as _time


class SlowRecordingHandler:
    """A handler that records enter/exit timestamps and takes a controlled delay."""

    def __init__(self, delay: float = 0.05):
        self.enter_times: list[float] = []
        self.exit_times: list[float] = []
        self.session_ids: list[str | None] = []
        self.results: list[str] = []
        self.delay = delay

    async def __call__(
        self, prompt: str, history: list, session_id: str | None,
        attachments: list | None, reasoning_effort: str | None,
        job_id: str | None = None,
    ):
        t0 = _time.monotonic()
        self.enter_times.append(t0)
        self.session_ids.append(session_id)
        await asyncio.sleep(self.delay)
        text = f"Reply to: {prompt[:20]}"
        self.results.append(text)
        yield {"type": "text_delta", "data": {"delta": text}}
        t1 = _time.monotonic()
        self.exit_times.append(t1)
        yield {"type": "done", "data": {"text": text, "sessionId": session_id, "status": "completed"}}


def _make_mock_job(job_id: str, text: str, session_id: str) -> dict:
    """Minimal job record for _run_http_job."""
    return {
        "status": "running",
        "events": [],
        "subscribers": [],
        "updatedAt": _time.time(),
    }


@pytest.mark.asyncio
async def test_same_session_jobs_serialize():
    """Two concurrent jobs on the same session must not overlap in the handler."""
    from kallisti_connector.http_facade import _session_locks, _http_jobs, _run_http_job

    session = "serialize-test-session"
    handler = SlowRecordingHandler(delay=0.05)

    # Create two jobs targeting the same session
    _http_jobs["job-A"] = _make_mock_job("job-A", "Hello A", session)
    _http_jobs["job-B"] = _make_mock_job("job-B", "Hello B", session)

    # Clean up locks from any previous test
    _session_locks.pop(session, None)

    # Dispatch concurrently
    await asyncio.gather(
        _run_http_job("job-A", handler, "Hello A", [], session, None, None),
        _run_http_job("job-B", handler, "Hello B", [], session, None, None),
    )

    # Filter out title-generation handler calls (T2 fires auto_title and uses
    # throwaway sessions — those are not subject to the user-session lock).
    user_calls = [
        (enter, exit_, sid)
        for enter, exit_, sid in zip(handler.enter_times, handler.exit_times, handler.session_ids)
        if sid == session
    ]
    # We should have exactly 2 user-message calls (one for each job)
    assert len(user_calls) == 2, (
        f"Expected 2 user-session handler calls, got {len(user_calls)}: "
        f"session_ids={handler.session_ids}"
    )

    # The second user-message job should have started AFTER the first finished
    enter_a, exit_a = user_calls[0][0], user_calls[0][1]
    enter_b, exit_b = user_calls[1][0], user_calls[1][1]

    assert exit_a <= enter_b + 0.01, (  # small epsilon for scheduling jitter
        f"Job overlap detected! Job-A exit={exit_a:.4f}, Job-B enter={enter_b:.4f}. "
        f"Jobs on the same session must serialize."
    )

    # Cleanup
    _http_jobs.pop("job-A", None)
    _http_jobs.pop("job-B", None)
    _session_locks.pop(session, None)


@pytest.mark.asyncio
async def test_different_sessions_run_concurrently():
    """Jobs on different sessions should be able to run in parallel."""
    from kallisti_connector.http_facade import _session_locks, _http_jobs, _run_http_job

    session_a = "parallel-session-A"
    session_b = "parallel-session-B"
    handler = SlowRecordingHandler(delay=0.05)

    _http_jobs["job-A"] = _make_mock_job("job-A", "Hello A", session_a)
    _http_jobs["job-B"] = _make_mock_job("job-B", "Hello B", session_b)

    _session_locks.pop(session_a, None)
    _session_locks.pop(session_b, None)

    t0 = _time.monotonic()
    await asyncio.gather(
        _run_http_job("job-A", handler, "Hello A", [], session_a, None, None),
        _run_http_job("job-B", handler, "Hello B", [], session_b, None, None),
    )
    elapsed = _time.monotonic() - t0

    # Count only user-session handler calls (title gen may fire too — T2)
    user_calls = [s for s in handler.session_ids if s in (session_a, session_b)]
    assert len(user_calls) == 2, (
        f"Expected 2 user-session calls, got {len(user_calls)}: {handler.session_ids}"
    )

    # Different sessions SHOULD run concurrently, so total time should be
    # closer to one delay than two delays (allow generous margin).
    assert elapsed < handler.delay * 2.5, (
        f"Different sessions ran sequentially: {elapsed:.3f}s vs expected <{handler.delay*2.5:.3f}s"
    )

    # Cleanup
    _http_jobs.pop("job-A", None)
    _http_jobs.pop("job-B", None)
    _session_locks.pop(session_a, None)
    _session_locks.pop(session_b, None)


@pytest.mark.asyncio
async def test_job_with_null_session_skips_lock():
    """Jobs with session_id=None should not acquire a lock (cold-start path)."""
    from kallisti_connector.http_facade import _session_locks, _http_jobs, _run_http_job

    handler = SlowRecordingHandler(delay=0.02)
    _http_jobs["job-null"] = _make_mock_job("job-null", "Hello", "none")

    # Track locks before and after — only care about new locks for None session
    locks_before = set(k for k, v in _session_locks.items())

    await _run_http_job("job-null", handler, "Hello", [], None, None, None)

    locks_after = set(k for k, v in _session_locks.items())
    new_locks = locks_after - locks_before
    assert len(new_locks) == 0, (
        f"No lock should be created for session_id=None, but got new locks: {new_locks}"
    )

    _http_jobs.pop("job-null", None)


@pytest.mark.asyncio
async def test_auto_title_persists_under_every_app_id():
    """B40: one conversation is addressable under two ids — title both.

    The session list keys off the canonical `_app_uuid(hermes_sid)` while the
    open thread is keyed by the UUID the app minted and sent as
    `conversationId`. Writing the title to only one of them left the other view
    showing a placeholder forever.
    """
    handler = FakeHandler()
    canonical = "9aed5edd-5d03-58bb-bd2c-781737ec34ff"
    app_sent = "62ccd628-9d71-491e-96c5-fed98658b0a4"

    with patch("kallisti_connector.session_store.set_session_meta") as mock_set_meta:
        await _auto_title_and_persist(
            handler,
            text="Sup homie.",
            hermes_sid="api-live",
            app_uuid=[canonical, app_sent],
        )

    targeted = {call[0][0] for call in mock_set_meta.call_args_list}
    assert targeted == {canonical, app_sent}, (
        f"title must land on both conversation ids, got {targeted}"
    )
    for call in mock_set_meta.call_args_list:
        assert call[1]["title"] == "Test Title"


@pytest.mark.asyncio
async def test_auto_title_still_accepts_a_single_id():
    """The string form stays supported — callers outside _run_http_job use it."""
    handler = FakeHandler()

    with patch("kallisti_connector.session_store.set_session_meta") as mock_set_meta:
        await _auto_title_and_persist(
            handler,
            text="Sup homie.",
            hermes_sid="api-live",
            app_uuid="canonical-app-uuid",
        )

    mock_set_meta.assert_called_once()
    assert mock_set_meta.call_args[0][0] == "canonical-app-uuid"
