"""Stream Contract v3 — invariants that must hold for every relay run.

Restored 2026-08-01 for Build 108 Phase 3A.  The contract module that
this suite validates was deleted in B7 (7f02f76) and brought back here
alongside the test.  The invariants encoded below are the ones that
regressed silently when the contract was missing: seq-monotonic
ordering from 1, exactly one terminal event and it must be last (or a
single run.requeued closing the attempt), and a single
jobId/conversationId across the whole stream.

Phase 3A v2 correction: bumped to v3, which adds ``conversationRevision``
to every event so the iOS reducer can apply each event against the
server-projected cursor without inventing the cursor from arrival
order.

If this suite ever starts failing because the contract was loosened
again, do NOT widen the assertions — fix the publisher to match.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from herald_connector.stream_contract import (
    JobEventEnvelope,
    TERMINAL_TYPES,
    RunStartedEvent,
    TextDeltaEvent,
    ReasoningDeltaEvent,
    ToolStartedEvent,
    ToolProgressEvent,
    ToolCompletedEvent,
    CommentaryEvent,
    ApprovalRequiredEvent,
    RunCompletedEvent,
    RunFailedEvent,
    RunCancelledEvent,
    RunRequeuedEvent,
)

FIXTURES_DIR = Path(__file__).parent / "fixtures" / "hermes"

# Phase 3A v2 correction: fixtures are named stream_v3_*.json (v3 contract
# with conversationRevision).  v2 fixtures are kept under
# fixtures/hermes/_legacy_v2/ for archival, never discovered here.
FIXTURE_FILES = sorted(FIXTURES_DIR.glob("stream_v3_*.json"))

# Map event type string to the expected Pydantic model class
EVENT_MODEL_MAP = {
    "run.started": RunStartedEvent,
    "text.delta": TextDeltaEvent,
    "reasoning.delta": ReasoningDeltaEvent,
    "tool.started": ToolStartedEvent,
    "tool.progress": ToolProgressEvent,
    "tool.completed": ToolCompletedEvent,
    "commentary": CommentaryEvent,
    "approval.required": ApprovalRequiredEvent,
    "run.completed": RunCompletedEvent,
    "run.failed": RunFailedEvent,
    "run.cancelled": RunCancelledEvent,
    "run.requeued": RunRequeuedEvent,
}


def _parse_fixture(path: Path) -> list[JobEventEnvelope]:
    """Parse a JSON fixture file into validated envelope objects."""
    raw = json.loads(path.read_text())
    events: list[JobEventEnvelope] = []
    for i, item in enumerate(raw):
        type_str = item["type"]
        model_cls = EVENT_MODEL_MAP.get(type_str)
        assert model_cls is not None, f"Unknown event type '{type_str}' at index {i} in {path.name}"
        events.append(model_cls.model_validate(item))
    return events


@pytest.fixture(params=FIXTURE_FILES, ids=lambda p: p.stem)
def fixture_path(request):
    return request.param


def test_contract_version_is_always_three(fixture_path):
    events = _parse_fixture(fixture_path)
    for event in events:
        assert event.contractVersion == 3, (
            f"Expected contractVersion=3, got {event.contractVersion} "
            f"(seq={event.seq}, type={event.type})"
        )


def test_job_id_consistent(fixture_path):
    events = _parse_fixture(fixture_path)
    job_ids = {e.jobId for e in events}
    assert len(job_ids) == 1, f"Multiple jobIds found: {job_ids}"


def test_conversation_id_consistent(fixture_path):
    events = _parse_fixture(fixture_path)
    conv_ids = {e.conversationId for e in events}
    assert len(conv_ids) == 1, f"Multiple conversationIds found: {conv_ids}"


def test_conversation_revision_present_and_consistent(fixture_path):
    """Phase 3A v3: every event carries a positive conversationRevision."""
    events = _parse_fixture(fixture_path)
    revs = {e.conversationRevision for e in events}
    assert revs == {1}, f"Expected all events at conversationRevision=1, got {revs}"


def test_seq_monotonically_increasing(fixture_path):
    events = _parse_fixture(fixture_path)
    seqs = [e.seq for e in events]
    assert seqs == sorted(seqs), f"seq not monotonically increasing: {seqs}"
    assert seqs[0] == 1, f"seq should start at 1, got {seqs[0]}"
    # No gaps
    for i in range(1, len(seqs)):
        assert seqs[i] == seqs[i - 1] + 1, f"seq gap between {seqs[i-1]} and {seqs[i]}"


def test_terminal_or_requeued_must_be_last(fixture_path):
    events = _parse_fixture(fixture_path)
    terminal_events = [e for e in events if e.type in TERMINAL_TYPES]
    requeued_events = [e for e in events if e.type == "run.requeued"]
    last_event = events[-1]

    if requeued_events:
        # Job was requeued — no terminal event expected for this attempt
        assert len(terminal_events) == 0, (
            f"Requeued fixture should have no terminal events, got: "
            f"{[e.type for e in terminal_events]}"
        )
        assert last_event.type == "run.requeued", (
            f"Requeued event must be last, but last is '{last_event.type}'"
        )
    else:
        assert len(terminal_events) == 1, (
            f"Expected exactly 1 terminal event, got {len(terminal_events)}: "
            f"{[e.type for e in terminal_events]}"
        )
        assert terminal_events[0] is last_event, (
            f"Terminal event '{terminal_events[0].type}' must be last, but found at index "
            f"{events.index(terminal_events[0])} (last index is {len(events) - 1})"
        )


def test_attempt_consistent_in_terminal(fixture_path):
    events = _parse_fixture(fixture_path)
    last_event = events[-1]
    started = next((e for e in events if isinstance(e, RunStartedEvent)), None)
    assert started is not None, "Fixture must have a run.started event"
    if last_event.type != "run.requeued":
        assert last_event.attempt == started.attempt, (
            f"Terminal attempt ({last_event.attempt}) must match run.started attempt ({started.attempt})"
        )


def test_fixture_has_minimum_events(fixture_path):
    events = _parse_fixture(fixture_path)
    assert len(events) >= 3, f"Fixture should have at least 3 events, got {len(events)}"


def test_fixture_has_maximum_events(fixture_path):
    events = _parse_fixture(fixture_path)
    assert len(events) <= 8, f"Fixture should have at most 8 events, got {len(events)}"


def test_all_fixture_files_discovered():
    assert len(FIXTURE_FILES) == 8, (
        f"Expected 8 fixture files, found {len(FIXTURE_FILES)}: "
        f"{[f.name for f in FIXTURE_FILES]}"
    )
