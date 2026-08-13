"""B1/B2/B4 regressions: a turn that did not finish must never report success.

Before the fix, `_run_http_job`'s `finally` promoted any generator that ended
without a `done` event to status="completed", so the phone showed a delivered
check, a green dot and a completion haptic for a dead turn.

Phase 3A v2 correction: the events in ``job["events"]`` are now v3
envelopes (with ``contractVersion``, ``jobId``, ``conversationId``,
``type``, ``payload``, ...) rather than the legacy ``{type, data}``
shape.  Tests locate the terminal event by its ``type`` and read the
typed fields off the v3 envelope (``payload`` for the producer data,
``type`` for the event kind).
"""
import pytest

from kallisti_connector import http_facade as hf
from kallisti_connector.herald_api_executor import (
    _is_interrupt_sentinel,
    _split_trailing_sentinel,
)


_TERMINAL_TYPES = {"run.completed", "run.failed", "run.cancelled"}


def _new_job(job_id):
    hf._http_jobs[job_id] = {
        "status": "running", "events": [], "subscribers": [],
        "updatedAt": 0.0,
        # Phase 3A v2: the v3 envelope requires a non-null
        # conversationId on every event.  Tests must supply one or the
        # publisher drops events as malformed.
        "conversationId": "11111111-2222-3333-4444-555555555555",
    }
    return hf._http_jobs[job_id]


async def _drain(job_id, handler):
    job = _new_job(job_id)
    await hf._run_http_job(job_id, handler, "hi", None, None, None, None)
    # Phase 3A v2: locate the terminal v3 envelope.  The publisher
    # also emits a legacy ``done`` sentinel for the iOS coordinator
    # — we use the typed terminal (``run.completed``/etc.) for our
    # assertions so the test is not coupled to the iOS legacy path.
    terminal = [e for e in job["events"] if e.get("type") in _TERMINAL_TYPES][-1]
    return job, terminal["payload"]


# ── B1: the facade must not synthesize success ────────────────────────────


@pytest.mark.asyncio
async def test_reconnect_without_done_is_not_success():
    """Transport dropped mid-turn — retryable failure, not a delivered answer."""
    async def handler(text, history, session_id, attachments, reasoning_effort):
        yield {"type": "text_delta", "data": {"delta": "Let me pull your play history"}}
        yield {"type": "reconnecting", "data": {"reason": "run_events_closed"}}

    job, data = await _drain("J1", handler)
    assert job["status"] == "failed"
    assert data["status"] == "failed"
    assert data["errorCategory"] == "upstream_interrupted"
    assert data["errorAction"] == "retry"
    assert data["error"]


@pytest.mark.asyncio
async def test_silent_generator_is_not_success():
    """Nothing streamed at all must not be reported as a completed turn."""
    async def handler(text, history, session_id, attachments, reasoning_effort):
        return
        yield  # pragma: no cover - makes this an async generator

    job, data = await _drain("J2", handler)
    assert job["status"] == "failed"
    assert data["errorCategory"] == "empty_response"


@pytest.mark.asyncio
async def test_partial_text_then_silence_is_not_success():
    async def handler(text, history, session_id, attachments, reasoning_effort):
        yield {"type": "text_delta", "data": {"delta": "Working on it"}}

    job, data = await _drain("J3", handler)
    assert job["status"] == "failed"
    assert data["errorCategory"] == "upstream_interrupted"


@pytest.mark.asyncio
async def test_explicit_done_still_wins():
    """The fix must not turn genuine successes into failures."""
    async def handler(text, history, session_id, attachments, reasoning_effort):
        yield {"type": "text_delta", "data": {"delta": "All set."}}
        yield {"type": "done", "data": {"status": "completed", "text": "All set."}}

    job, data = await _drain("J4", handler)
    assert job["status"] == "completed"
    assert data["status"] == "completed"
    assert data["text"] == "All set."


@pytest.mark.asyncio
async def test_reconnect_followed_by_done_is_success():
    """A reconnect that later resolves must not be held against the turn."""
    async def handler(text, history, session_id, attachments, reasoning_effort):
        yield {"type": "reconnecting", "data": {"reason": "transport"}}
        yield {"type": "text_delta", "data": {"delta": "Recovered."}}
        yield {"type": "done", "data": {"status": "completed", "text": "Recovered."}}

    job, data = await _drain("J5", handler)
    assert job["status"] == "completed"


# ── B2: sentinel detection on the trailing segment ────────────────────────


def test_sentinel_after_preamble_is_detected():
    """The screenshot's exact shape: preamble, then the marker."""
    text = "Let me pull your play history and grab those recs. Operation interrupted."
    answer, sentinel = _split_trailing_sentinel(text)
    assert sentinel == "Operation interrupted."
    assert answer == "Let me pull your play history and grab those recs."
    # The old startswith predicate missed exactly this case.
    assert not _is_interrupt_sentinel(text)


def test_sentinel_on_its_own_line_is_detected():
    answer, sentinel = _split_trailing_sentinel(
        "Checking.\n\nOperation interrupted: handling API error (500)"
    )
    assert sentinel.startswith("Operation interrupted:")
    assert answer == "Checking."


def test_sentinel_only_turn():
    answer, sentinel = _split_trailing_sentinel("Operation interrupted.")
    assert sentinel == "Operation interrupted."
    assert answer == ""


def test_ordinary_prose_is_not_a_sentinel():
    text = "The operation interrupted earlier, but it is fixed now."
    answer, sentinel = _split_trailing_sentinel(text)
    assert sentinel is None
    assert answer == text


def test_empty_turn():
    assert _split_trailing_sentinel("") == ("", None)
