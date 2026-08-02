"""Stream Contract v2 — typed Pydantic envelope for relay live events.

The relay emits a series of small JSON envelopes per agent run.  Each
envelope is one of 12 well-known event kinds; the envelope shape is
shared and only the `payload` field varies.  This module is the
authoritative schema for the wire side and is enforced by the
``test_stream_contract`` suite in ``connector/tests/``.

History: deleted in B7 (7f02f76), restored 2026-08-01 for Build 108
Phase 3A.  The invariants this contract enforces (seq-monotonic, exactly
one terminal event, single jobId/conversationId per stream) are the
ones that regressed silently in B7 and surfaced as duplicate or
out-of-order bubbles on iOS.

Naming: model class names match the iOS Swift `LiveActivityRunEvent`
discriminator union (see ``Herald/Services/Live/LiveHeraldClient.swift``)
so any cross-language fixtures and codegen stay aligned.  The wire
field names follow the JSON-envelope shape; do not rename them without
updating both decoders.
"""

from __future__ import annotations

from typing import Any, List, Literal, Union

from pydantic import BaseModel, ConfigDict, Field

# contractVersion is the field the iOS test suite pins; bumping it is a
# breaking change that requires BOTH sides of the wire to move together.
CONTRACT_VERSION = 2

# Event kinds that close a run.  ``run.requeued`` is NOT terminal — it
# closes one attempt and signals a follow-up attempt will start.  The
# relay still requires exactly one of these terminal types at the end
# of any non-requeued run, never inside the run.
TERMINAL_TYPES = frozenset({"run.completed", "run.failed", "run.cancelled"})


class _EnvelopeBase(BaseModel):
    """Shared envelope — every relay event has these fields.

    The 8 envelope fields are the iOS decoder's hard contract.  Do not
    add fields here without a paired iOS change; do not remove any.
    """

    model_config = ConfigDict(extra="allow")

    contractVersion: int = CONTRACT_VERSION
    jobId: str
    conversationId: str
    attempt: int
    seq: int
    type: str
    timestamp: str
    payload: dict[str, Any] = Field(default_factory=dict)


# Alias kept for the test-suite import — JobEventEnvelope was the
# canonical name in the original module and the test imports it by
# that name.
JobEventEnvelope = _EnvelopeBase


# ── Per-event payload models ─────────────────────────────────────────────
#
# Each event kind has a typed payload.  The base envelope is reused so
# callers can pass any subclass through a single `isinstance(e, T)`
# test.  ``type`` is a `Literal` so Pydantic rejects mismatches at
# model_validate() time — the test suite relies on that.

class _BaseEvent(_EnvelopeBase):
    """Common discriminator for all event subclasses."""

    type: str  # narrowed in each subclass below


class RunStartedEvent(_BaseEvent):
    type: Literal["run.started"] = "run.started"


class TextDeltaEvent(_BaseEvent):
    type: Literal["text.delta"] = "text.delta"


class ReasoningDeltaEvent(_BaseEvent):
    type: Literal["reasoning.delta"] = "reasoning.delta"


class ToolStartedEvent(_BaseEvent):
    type: Literal["tool.started"] = "tool.started"


class ToolProgressEvent(_BaseEvent):
    type: Literal["tool.progress"] = "tool.progress"


class ToolCompletedEvent(_BaseEvent):
    type: Literal["tool.completed"] = "tool.completed"


class CommentaryEvent(_BaseEvent):
    type: Literal["commentary"] = "commentary"


class ApprovalRequiredEvent(_BaseEvent):
    type: Literal["approval.required"] = "approval.required"


class RunCompletedEvent(_BaseEvent):
    type: Literal["run.completed"] = "run.completed"


class RunFailedEvent(_BaseEvent):
    type: Literal["run.failed"] = "run.failed"


class RunCancelledEvent(_BaseEvent):
    type: Literal["run.cancelled"] = "run.cancelled"


class RunRequeuedEvent(_BaseEvent):
    type: Literal["run.requeued"] = "run.requeued"


# Discriminated union — used by the validator factory below and by any
# caller that wants a single type alias for "any relay event".
RelayEvent = Union[
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
]


EVENT_TYPE_TO_MODEL: dict[str, type[_EnvelopeBase]] = {
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


def parse_event(raw: dict[str, Any]) -> _EnvelopeBase:
    """Validate a single envelope and return the typed subclass instance.

    Raises ``pydantic.ValidationError`` on any contract violation.  Used
    by the live-event publisher in ``_run_http_job`` and by the test
    suite's fixture loader.
    """
    type_str = raw.get("type")
    if not isinstance(type_str, str):
        raise ValueError(f"envelope missing 'type' string: {raw!r}")
    model_cls = EVENT_TYPE_TO_MODEL.get(type_str)
    if model_cls is None:
        raise ValueError(f"unknown relay event type {type_str!r}")
    return model_cls.model_validate(raw)


def parse_stream(events: List[dict[str, Any]]) -> List[_EnvelopeBase]:
    """Validate a list of envelopes in order.

    Returns the typed list; raises ``pydantic.ValidationError`` on the
    first violation.  The test suite calls this with the contents of
    each JSON fixture and asserts the invariants on the typed result.
    """
    return [parse_event(raw) for raw in events]
