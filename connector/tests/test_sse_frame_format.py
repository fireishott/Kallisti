"""SSE frame format — verify _format_sse_frame maps type→event correctly.

The iOS client's parseEnvelope switch matches on the literal SSE ``event:``
line.  This suite asserts that every terminal event type emitted by the
connector's ``_run_http_job`` finally-block round-trips through
``_format_sse_frame`` with the correct ``event:`` line on the wire.

Regression guard for: terminal done-regression (§4 of KALLISTI-0.1.0-RECOVERY-ORDERS).
"""

from __future__ import annotations

import json

import pytest

from kallisti_connector.http_facade import _format_sse_frame


# ── Terminal event type → SSE event line mapping ────────────────────────────

TERMINAL_EVENT_TYPES = [
    ("run.completed", "completed"),
    ("run.failed", "failed"),
    ("run.cancelled", "cancelled"),
    ("done", "completed"),  # legacy backward compat
]


@pytest.mark.parametrize("event_type,status", TERMINAL_EVENT_TYPES, ids=[t[0] for t in TERMINAL_EVENT_TYPES])
def test_format_sse_frame_uses_type_as_event_line(event_type: str, status: str):
    """The SSE ``event:`` line must be the dict's ``type`` value verbatim."""
    event = {
        "contractVersion": 3,
        "jobId": "test-job",
        "conversationId": "test-conv",
        "attempt": 1,
        "seq": 1,
        "type": event_type,
        "timestamp": "2026-08-06T12:00:00Z",
        "payload": {"status": status, "message": None},
    }
    frame = _format_sse_frame(event, seq=1)
    lines = frame.split("\n")
    # Second line should be "event: <type>"
    assert lines[1] == f"event: {event_type}", (
        f"Expected 'event: {event_type}', got '{lines[1]}'"
    )
    # Data line should contain the full JSON envelope
    assert lines[2].startswith("data: ")
    data = json.loads(lines[2][len("data: "):])
    assert data["type"] == event_type


def test_format_sse_frame_progress_fallback():
    """Events without a type field should default to 'progress'."""
    event = {"jobId": "x", "seq": 1, "payload": {}}
    frame = _format_sse_frame(event, seq=1)
    lines = frame.split("\n")
    assert lines[1] == "event: progress"


def test_format_sse_frame_seq_appears_in_id_line():
    """The ``id:`` line must match the seq parameter."""
    event = {"type": "text.delta", "jobId": "x", "seq": 42, "payload": {"delta": "hi"}}
    frame = _format_sse_frame(event, seq=42)
    lines = frame.split("\n")
    assert lines[0] == "id: 42"


def test_wire_event_types_includes_done():
    """'done' must remain in WIRE_EVENT_TYPES for backward compat."""
    from kallisti_connector.http_facade import WIRE_EVENT_TYPES
    assert "done" in WIRE_EVENT_TYPES
    assert "run.completed" in WIRE_EVENT_TYPES
    assert "run.failed" in WIRE_EVENT_TYPES
    assert "run.cancelled" in WIRE_EVENT_TYPES
