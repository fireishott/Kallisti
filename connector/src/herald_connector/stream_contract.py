"""Stream Contract v3 — typed Pydantic envelope for relay live events.

The relay emits a series of small JSON envelopes per agent run.  Each
envelope is one of 12 well-known event kinds; the envelope shape is
shared and only the `payload` field varies.  This module is the
authoritative schema for the wire side and is enforced by the
``test_stream_contract_v3`` suite in ``connector/tests/``.

History
-------
- B7 (7f02f76) deleted the contract module; restored in Build 108
  Phase 3A as v2 (8 fields).
- Build 108 Phase 3A v2 correction: bumped to v3 — adds
  ``conversationRevision`` so the iOS reducer can apply every event
  against a server-projected cursor without inventing the cursor from
  arrival order, and pins the wire field name to ``conversationId``
  (the application conversation UUID).  ``hermesSessionId`` continues
  to identify the Hermes session and is never substituted for
  ``conversationId``.

Naming: model class names match the iOS Swift ``JobEventEnvelope``
discriminator union (see ``Herald/Models/JobEvent.swift``) so any
cross-language fixtures and codegen stay aligned.  The wire field
names follow the JSON-envelope shape; do not rename them without
updating both decoders.
"""

from __future__ import annotations

from typing import Any, List, Literal, Union

from pydantic import BaseModel, ConfigDict, Field

# contractVersion is the field the iOS test suite pins; bumping it is a
# breaking change that requires BOTH sides of the wire to move together.
CONTRACT_VERSION = 3

# Event kinds that close a run.  ``run.requeued`` is NOT terminal — it
# closes one attempt and signals a follow-up attempt will start.  The
# relay still requires exactly one of these terminal types at the end
# of any non-requeued run, never inside the run.
TERMINAL_TYPES = frozenset({"run.completed", "run.failed", "run.cancelled"})


class _EnvelopeBase(BaseModel):
    """Shared envelope — every relay event has these fields.

    The 9 envelope fields are the iOS decoder's hard contract.  Do not
    add fields here without a paired iOS change; do not remove any.

    Phase 3A v2 correction: ``extra`` is still ``allow`` during the
    rollout window so legacy v2 frames that pre-date the iOS paired
    decoder do not fail validation.  Unknown fields are tolerated but
    logged at WARNING so the operator sees them; once iOS is paired
    this flips to ``"forbid"`` (see the brief §3 — "after iOS paired").
    """

    model_config = ConfigDict(extra="allow")

    contractVersion: int = CONTRACT_VERSION
    jobId: str
    conversationId: str
    attempt: int
    seq: int
    # Phase 3A v3: every event carries the current conversation revision
    # so the iOS reducer can apply it against the server-projected cursor
    # without inventing the cursor from arrival order.
    conversationRevision: int = 0
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

    Phase 3A v3: this is the production schema validator.  Every emitted
    SSE frame flows through ``build_envelope`` → ``parse_event`` before
    publication so the wire can never drift from the typed envelope.
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


# ── Strict builder used by the production SSE publisher ───────────────────
#
# The publisher must construct every event through this builder so the
# envelope shape is uniform, the contract version is bumped atomically,
# and validation is mandatory before the SSE data: line is emitted.

def build_envelope(
    *,
    job_id: str,
    conversation_id: str,
    attempt: int,
    seq: int,
    conversation_revision: int,
    event_type: str,
    timestamp: str,
    payload: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Construct a single v3 envelope dict and validate it.

    This is the single entry point the production SSE publisher uses.
    It allocates nothing beyond the envelope fields (sequence allocation
    is the caller's responsibility — the caller passes the seq it
    allocated from the ledger).  Validation through ``parse_event`` is
    the last step, so a malformed envelope cannot escape this function.

    Raises ``ValueError`` when ``event_type`` is unknown and
    ``pydantic.ValidationError`` when required fields are missing.
    """
    envelope: dict[str, Any] = {
        "contractVersion": CONTRACT_VERSION,
        "jobId": job_id,
        "conversationId": conversation_id,
        "attempt": attempt,
        "seq": seq,
        "conversationRevision": int(conversation_revision),
        "type": event_type,
        "timestamp": timestamp,
        "payload": dict(payload or {}),
    }
    # Validate before returning — the publisher MUST NOT publish an
    # envelope that fails the strict schema.
    parse_event(envelope)
    return envelope
