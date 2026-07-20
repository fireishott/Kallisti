"""Pydantic v2 models for the Herald Stream Contract v2."""

from __future__ import annotations

from datetime import datetime
from typing import Literal, Union

from pydantic import BaseModel, Field


# ---------------------------------------------------------------------------
# Per-event payloads
# ---------------------------------------------------------------------------


class RunStartedPayload(BaseModel):
    phase: str
    attempt: int


class TextDeltaPayload(BaseModel):
    delta: str
    segmentId: str


class ReasoningDeltaPayload(BaseModel):
    delta: str
    segmentId: str


class ToolStartedPayload(BaseModel):
    toolCallId: str
    name: str
    args: str


class ToolProgressPayload(BaseModel):
    toolCallId: str
    label: str


class ToolCompletedPayload(BaseModel):
    toolCallId: str
    output: str


class CommentaryPayload(BaseModel):
    text: str


class ApprovalRequiredPayload(BaseModel):
    toolCallId: str
    prompt: str


class Usage(BaseModel):
    prompt_tokens: int | None = None
    completion_tokens: int | None = None
    total_tokens: int | None = None


class DiffFile(BaseModel):
    path: str
    status: str


class Diff(BaseModel):
    files: list[DiffFile]
    summary: str


class RunCompletedPayload(BaseModel):
    messageId: str
    text: str
    usage: Usage | None = None
    diff: Diff | None = None


class RunFailedPayload(BaseModel):
    error: str
    retryable: bool


class RunCancelledPayload(BaseModel):
    reason: str


class RunRequeuedPayload(BaseModel):
    fromAttempt: int
    toAttempt: int


# ---------------------------------------------------------------------------
# Typed event envelopes
# ---------------------------------------------------------------------------


class RunStartedEvent(BaseModel):
    contractVersion: Literal[2] = 2
    jobId: str
    conversationId: str
    attempt: int
    seq: int
    type: Literal["run.started"]
    timestamp: datetime
    payload: RunStartedPayload


class TextDeltaEvent(BaseModel):
    contractVersion: Literal[2] = 2
    jobId: str
    conversationId: str
    attempt: int
    seq: int
    type: Literal["text.delta"]
    timestamp: datetime
    payload: TextDeltaPayload


class ReasoningDeltaEvent(BaseModel):
    contractVersion: Literal[2] = 2
    jobId: str
    conversationId: str
    attempt: int
    seq: int
    type: Literal["reasoning.delta"]
    timestamp: datetime
    payload: ReasoningDeltaPayload


class ToolStartedEvent(BaseModel):
    contractVersion: Literal[2] = 2
    jobId: str
    conversationId: str
    attempt: int
    seq: int
    type: Literal["tool.started"]
    timestamp: datetime
    payload: ToolStartedPayload


class ToolProgressEvent(BaseModel):
    contractVersion: Literal[2] = 2
    jobId: str
    conversationId: str
    attempt: int
    seq: int
    type: Literal["tool.progress"]
    timestamp: datetime
    payload: ToolProgressPayload


class ToolCompletedEvent(BaseModel):
    contractVersion: Literal[2] = 2
    jobId: str
    conversationId: str
    attempt: int
    seq: int
    type: Literal["tool.completed"]
    timestamp: datetime
    payload: ToolCompletedPayload


class CommentaryEvent(BaseModel):
    contractVersion: Literal[2] = 2
    jobId: str
    conversationId: str
    attempt: int
    seq: int
    type: Literal["commentary"]
    timestamp: datetime
    payload: CommentaryPayload


class ApprovalRequiredEvent(BaseModel):
    contractVersion: Literal[2] = 2
    jobId: str
    conversationId: str
    attempt: int
    seq: int
    type: Literal["approval.required"]
    timestamp: datetime
    payload: ApprovalRequiredPayload


class RunCompletedEvent(BaseModel):
    contractVersion: Literal[2] = 2
    jobId: str
    conversationId: str
    attempt: int
    seq: int
    type: Literal["run.completed"]
    timestamp: datetime
    payload: RunCompletedPayload


class RunFailedEvent(BaseModel):
    contractVersion: Literal[2] = 2
    jobId: str
    conversationId: str
    attempt: int
    seq: int
    type: Literal["run.failed"]
    timestamp: datetime
    payload: RunFailedPayload


class RunCancelledEvent(BaseModel):
    contractVersion: Literal[2] = 2
    jobId: str
    conversationId: str
    attempt: int
    seq: int
    type: Literal["run.cancelled"]
    timestamp: datetime
    payload: RunCancelledPayload


class RunRequeuedEvent(BaseModel):
    contractVersion: Literal[2] = 2
    jobId: str
    conversationId: str
    attempt: int
    seq: int
    type: Literal["run.requeued"]
    timestamp: datetime
    payload: RunRequeuedPayload


# ---------------------------------------------------------------------------
# Discriminated union
# ---------------------------------------------------------------------------

JobEventEnvelope = Union[
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

TERMINAL_TYPES = {"run.completed", "run.failed", "run.cancelled"}
