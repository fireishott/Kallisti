"""Hermes Gateway Executor — structured event protocol adapter.

Speaks the Hermes Desktop JSON-RPC WS protocol and maps events to the v2 vocabulary.
Falls back to the Runs API HTTP adapter when the gateway is unreachable.
"""

from __future__ import annotations

import asyncio
import json
import logging
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import AsyncIterator

import httpx
import websockets

from .herald_api_executor import StreamEvent
from .stream_contract import (
    CommentaryPayload,
    JobEventEnvelope,
    ReasoningDeltaPayload,
    RunCancelledPayload,
    RunCompletedPayload,
    RunFailedPayload,
    RunRequeuedPayload,
    RunStartedPayload,
    TextDeltaPayload,
    ToolCompletedPayload,
    ToolProgressPayload,
    ToolStartedPayload,
)

logger = logging.getLogger(__name__)


class HermesGatewayError(Exception):
    """Raised when the gateway is unreachable or returns an error."""


@dataclass
class GatewayConfig:
    gateway_url: str = "ws://localhost:8642/api/ws"
    runs_api_url: str = "http://localhost:8642"
    connect_timeout: float = 10.0
    read_timeout: float = 300.0


class HermesEventAdapter:
    """Maps raw Hermes events to v2 JobEventEnvelope.

    Handles both gateway (JSON-RPC WS) and runs API (HTTP/SSE) sources,
    ensuring a consistent v2 vocabulary output.
    """

    def __init__(self, job_id: str, conversation_id: str, attempt: int = 0):
        self.job_id = job_id
        self.conversation_id = conversation_id
        self.attempt = attempt
        self._source_seq = 0
        self._segment_counter = 0
        self._active_tool_id: str | None = None
        self._seen_terminal = False

    def _next_source_seq(self) -> int:
        self._source_seq += 1
        return self._source_seq

    def _next_segment_id(self) -> str:
        self._segment_counter += 1
        return f"seg-{self._segment_counter}"

    def make_envelope(
        self,
        event_type: str,
        payload: dict,
        source_seq: int | None = None,
    ) -> JobEventEnvelope:
        from .stream_contract import (
            CommentaryEvent,
            ReasoningDeltaEvent,
            RunCancelledEvent,
            RunCompletedEvent,
            RunFailedEvent,
            RunRequeuedEvent,
            RunStartedEvent,
            TextDeltaEvent,
            ToolCompletedEvent,
            ToolProgressEvent,
            ToolStartedEvent,
        )

        base = dict(
            contractVersion=2,
            jobId=self.job_id,
            conversationId=self.conversation_id,
            attempt=self.attempt,
            seq=0,  # Will be assigned by the relay's append_job_event
            sourceSeq=source_seq or self._next_source_seq(),
            type=event_type,
            timestamp=datetime.now(timezone.utc),
            payload=payload,
        )

        event_cls_map = {
            "run.started": RunStartedEvent,
            "text.delta": TextDeltaEvent,
            "reasoning.delta": ReasoningDeltaEvent,
            "tool.started": ToolStartedEvent,
            "tool.progress": ToolProgressEvent,
            "tool.completed": ToolCompletedEvent,
            "commentary": CommentaryEvent,
            "run.completed": RunCompletedEvent,
            "run.failed": RunFailedEvent,
            "run.cancelled": RunCancelledEvent,
            "run.requeued": RunRequeuedEvent,
        }

        cls = event_cls_map.get(event_type)
        if cls is None:
            raise ValueError(f"Unknown event type: {event_type}")
        return cls(**base)

    def adapt_run_started(self, phase: str = "executing") -> JobEventEnvelope:
        return self.make_envelope(
            "run.started",
            RunStartedPayload(phase=phase, attempt=self.attempt).model_dump(),
        )

    def adapt_text_delta(self, delta: str, segment_id: str | None = None) -> JobEventEnvelope:
        return self.make_envelope(
            "text.delta",
            TextDeltaPayload(delta=delta, segmentId=segment_id or self._next_segment_id()).model_dump(),
        )

    def adapt_reasoning_delta(self, delta: str, segment_id: str | None = None) -> JobEventEnvelope:
        return self.make_envelope(
            "reasoning.delta",
            ReasoningDeltaPayload(delta=delta, segmentId=segment_id or "reasoning").model_dump(),
        )

    def adapt_tool_started(self, tool_call_id: str, name: str, args: str = "") -> JobEventEnvelope:
        self._active_tool_id = tool_call_id
        return self.make_envelope(
            "tool.started",
            ToolStartedPayload(toolCallId=tool_call_id, name=name, args=args).model_dump(),
        )

    def adapt_tool_progress(self, tool_call_id: str, label: str) -> JobEventEnvelope:
        return self.make_envelope(
            "tool.progress",
            ToolProgressPayload(toolCallId=tool_call_id, label=label).model_dump(),
        )

    def adapt_tool_completed(self, tool_call_id: str, output: str = "") -> JobEventEnvelope:
        self._active_tool_id = None
        return self.make_envelope(
            "tool.completed",
            ToolCompletedPayload(toolCallId=tool_call_id, output=output).model_dump(),
        )

    def adapt_commentary(self, text: str) -> JobEventEnvelope:
        return self.make_envelope(
            "commentary",
            CommentaryPayload(text=text).model_dump(),
        )

    def adapt_run_completed(
        self,
        message_id: str,
        text: str,
        usage: dict | None = None,
        diff: dict | None = None,
    ) -> JobEventEnvelope:
        self._seen_terminal = True
        return self.make_envelope(
            "run.completed",
            RunCompletedPayload(
                messageId=message_id,
                text=text,
                usage=usage,
                diff=diff,
            ).model_dump(),
        )

    def adapt_run_failed(self, error: str, retryable: bool = False) -> JobEventEnvelope:
        self._seen_terminal = True
        return self.make_envelope(
            "run.failed",
            RunFailedPayload(error=error, retryable=retryable).model_dump(),
        )

    def adapt_run_cancelled(self, reason: str = "") -> JobEventEnvelope:
        self._seen_terminal = True
        return self.make_envelope(
            "run.cancelled",
            RunCancelledPayload(reason=reason).model_dump(),
        )

    def adapt_run_requeued(self, from_attempt: int, to_attempt: int) -> JobEventEnvelope:
        return self.make_envelope(
            "run.requeued",
            RunRequeuedPayload(fromAttempt=from_attempt, toAttempt=to_attempt).model_dump(),
        )

    def adapt_stream_event(self, event: StreamEvent) -> JobEventEnvelope | None:
        """Adapt a StreamEvent from the Runs API to a v2 envelope."""
        if self.fence_late_event(event.type):
            return None

        if event.type == "text_delta":
            return self.adapt_text_delta(event.data)
        elif event.type == "reasoning_delta":
            return self.adapt_reasoning_delta(event.data)
        elif event.type == "tool_activity":
            # Generate a stable toolCallId from the label
            tool_id = f"tool-{hash(event.label) & 0xFFFFFFFF:08x}"
            return self.adapt_tool_progress(tool_id, event.label)
        elif event.type == "finish":
            return None  # Terminal is handled separately
        return None

    def is_terminal(self) -> bool:
        return self._seen_terminal

    def fence_late_event(self, event_type: str) -> bool:
        """Return True if this event should be discarded (arrived after terminal or attempt rollover)."""
        if self._seen_terminal:
            logger.debug("Fencing late event %s after terminal", event_type)
            return True
        return False


class HermesGatewayExecutor:
    """Executor that speaks the Hermes Desktop JSON-RPC WS protocol.

    Connects to the Hermes gateway via WebSocket, sends a chat request,
    and yields v2-adapted events. Falls back to the Runs API HTTP adapter
    when the gateway is unreachable.
    """

    ADAPTER_MODE = "gateway_v2"

    def __init__(self, config: GatewayConfig | None = None):
        self.config = config or GatewayConfig()

    async def health_check(self) -> bool:
        """Return True if the gateway WebSocket is reachable."""
        try:
            async with asyncio.timeout(self.config.connect_timeout):
                async with websockets.connect(
                    self.config.gateway_url,
                    close_timeout=5,
                ) as ws:
                    # Send a ping and expect a pong
                    pong = await ws.ping()
                    await asyncio.wait_for(pong, timeout=5)
                    return True
        except Exception:  # noqa: BLE001
            return False

    async def stream_chat(
        self,
        *,
        job_id: str,
        conversation_id: str,
        attempt: int,
        messages: list[dict],
        session_id: str | None = None,
    ) -> AsyncIterator[JobEventEnvelope]:
        """Stream a chat completion via the Hermes gateway JSON-RPC WS protocol.

        Yields v2 JobEventEnvelope instances. Falls back to Runs API if
        the gateway is unreachable.
        """
        adapter = HermesEventAdapter(job_id, conversation_id, attempt)

        try:
            async with asyncio.timeout(self.config.connect_timeout):
                async with websockets.connect(
                    self.config.gateway_url,
                    close_timeout=5,
                ) as ws:
                    yield adapter.adapt_run_started()

                    # Send JSON-RPC request
                    request_id = str(uuid.uuid4())
                    await ws.send(json.dumps({
                        "jsonrpc": "2.0",
                        "id": request_id,
                        "method": "chat.stream",
                        "params": {
                            "messages": messages,
                            "sessionId": session_id,
                            "stream": True,
                        },
                    }))

                    async for raw_msg in ws:
                        if adapter.is_terminal():
                            break

                        try:
                            msg = json.loads(raw_msg)
                        except json.JSONDecodeError:
                            continue

                        # JSON-RPC error
                        if "error" in msg:
                            error = msg["error"]
                            yield adapter.adapt_run_failed(
                                error=error.get("message", "Unknown gateway error"),
                                retryable=False,
                            )
                            return

                        # JSON-RPC result or notification
                        result = msg.get("result") or msg.get("params")
                        if not result:
                            continue

                        event_kind = result.get("event")
                        if event_kind == "text_delta":
                            yield adapter.adapt_text_delta(result.get("delta", ""))
                        elif event_kind == "reasoning_delta":
                            yield adapter.adapt_reasoning_delta(result.get("delta", ""))
                        elif event_kind == "tool_started":
                            yield adapter.adapt_tool_started(
                                tool_call_id=result.get("toolCallId", str(uuid.uuid4())),
                                name=result.get("name", ""),
                                args=result.get("args", ""),
                            )
                        elif event_kind == "tool_progress":
                            yield adapter.adapt_tool_progress(
                                tool_call_id=result.get("toolCallId", ""),
                                label=result.get("label", ""),
                            )
                        elif event_kind == "tool_completed":
                            yield adapter.adapt_tool_completed(
                                tool_call_id=result.get("toolCallId", ""),
                                output=result.get("output", ""),
                            )
                        elif event_kind == "commentary":
                            yield adapter.adapt_commentary(result.get("text", ""))
                        elif event_kind == "completed":
                            yield adapter.adapt_run_completed(
                                message_id=result.get("messageId", str(uuid.uuid4())),
                                text=result.get("text", ""),
                                usage=result.get("usage"),
                                diff=result.get("diff"),
                            )
                        elif event_kind == "failed":
                            yield adapter.adapt_run_failed(
                                error=result.get("error", "Unknown error"),
                                retryable=result.get("retryable", False),
                            )
                        elif event_kind == "cancelled":
                            yield adapter.adapt_run_cancelled(
                                reason=result.get("reason", ""),
                            )

        except (OSError, websockets.exceptions.WebSocketException, asyncio.TimeoutError) as exc:
            logger.warning("Gateway unreachable (%s), falling back to Runs API", exc)
            async for event in self._fallback_to_runs_api(
                adapter=adapter,
                messages=messages,
                session_id=session_id,
            ):
                yield event

    async def _fallback_to_runs_api(
        self,
        *,
        adapter: HermesEventAdapter,
        messages: list[dict],
        session_id: str | None = None,
    ) -> AsyncIterator[JobEventEnvelope]:
        """Fallback: stream via the Runs API HTTP/SSE endpoint."""
        from .herald_api_executor import HeraldAPIExecutor

        executor = HeraldAPIExecutor(api_server_url=self.config.runs_api_url)

        # Extract the latest user message from the messages list
        latest_user_message = ""
        history = []
        for msg in messages:
            if msg.get("role") == "user":
                latest_user_message = msg.get("content", "")
            else:
                from .herald_runner import HeraldConversationMessage
                history.append(HeraldConversationMessage(
                    role=msg.get("role", "user"),
                    text=msg.get("content", ""),
                ))

        yield adapter.adapt_run_started(phase="executing")

        async for stream_event in executor.stream_message(
            latest_user_message=latest_user_message,
            history=history if history else None,
            session_id=session_id,
        ):
            envelope = adapter.adapt_stream_event(stream_event)
            if envelope is not None:
                yield envelope

            if stream_event.type == "finish" and not adapter.is_terminal():
                yield adapter.adapt_run_completed(
                    message_id=str(uuid.uuid4()),
                    text="",  # Text is accumulated by the caller
                    usage=stream_event.usage,
                )
