"""Hermes API server executor — HTTP/SSE alternative to the CLI subprocess."""

from __future__ import annotations

import asyncio
import json
import logging
import os
from dataclasses import dataclass, field
from typing import AsyncIterator

import httpx

from .herald_runner import HeraldChatResult, HeraldConversationMessage


DEFAULT_API_SERVER_URL = "http://localhost:8642"
CONNECT_TIMEOUT = 10.0
READ_TIMEOUT = 300.0  # 5 minutes — long enough for Claude thinking, catches dead connections

# All case-insensitive tag variants recognized for inline reasoning blocks.
# Must match reasoning_sanitizer.py tag set and iOS LiveHeraldClient.splitThinkingBlocks.
_REASONING_TAGS = frozenset({
    "think",
    "thinking",
    "reasoning",
    "thought",
    "reasoning_scratchpad",
})

# Precomputed tag strings for each variant
_OPEN_TAGS: dict[str, str] = {name: f"<{name}>" for name in _REASONING_TAGS}
_CLOSE_TAGS: dict[str, str] = {name: f"</{name}>" for name in _REASONING_TAGS}

# Precomputed prefix set for partial-tag-at-end-of-chunk detection
_OPEN_TAG_PREFIXES: frozenset[str] = frozenset(
    tag[:i]
    for tag in _OPEN_TAGS.values()
    for i in range(1, len(tag))
)
_MAX_OPEN_TAG_LEN: int = max(len(t) for t in _OPEN_TAGS.values())

logger = logging.getLogger(__name__)

_INTERRUPT_SENTINELS = (
    "Operation interrupted.",
    "Operation interrupted:",
    "Operation interrupted during retry",
)


def _is_interrupt_sentinel(text: str) -> bool:
    """True only when an assistant turn is Hermes' interruption placeholder."""
    value = (text or "").strip()
    return bool(value) and value.startswith(_INTERRUPT_SENTINELS)


def _split_trailing_sentinel(text: str) -> tuple[str, str | None]:
    """Split a turn into (answer, trailing interrupt sentinel).

    Hermes appends its interruption marker to whatever the model had already
    said, so the sentinel is usually preceded by a preamble ("Let me pull your
    play history…Operation interrupted.").  Testing the whole accumulated turn
    with ``startswith`` only matches when the marker is the very first token,
    which is why real interruptions were delivered to the phone as ordinary
    assistant text.  Match on the trailing segment instead and hand back the
    preamble so it is not discarded.
    """
    value = (text or "").strip()
    if not value:
        return "", None
    if _is_interrupt_sentinel(value):
        return "", value
    # Walk the segment boundaries Hermes actually writes: it starts the marker
    # on its own line, or splices it directly onto the preceding sentence.
    for sentinel in _INTERRUPT_SENTINELS:
        index = value.rfind(sentinel)
        if index <= 0:
            continue
        preceding = value[index - 1]
        if preceding.isspace() or preceding in ".!?":
            return value[:index].rstrip(), value[index:].strip()
    return value, None


class InlineThinkParser:
    """Stateful parser that extracts inline reasoning tags from content deltas.

    Recognizes all 5 tag variants from :data:`_REASONING_TAGS`:
      think, thinking, reasoning, thought, REASONING_SCRATCHPAD

    All variants are matched case-insensitively.  Open/close tags must pair
    by name (``<think>`` with ``</think>``, ``<thinking>`` with
    ``</thinking>``, etc.).  Handles markers that span chunk boundaries.
    When inside a recognised reasoning block content is routed to reasoning;
    outside, to text.  If the stream ends with an unclosed block the
    buffered text is emitted as reasoning.
    """

    def __init__(self) -> None:
        self._in_think = False
        self._buf = ""
        self._pending_reasoning = ""
        self._current_tag: str | None = None

    def feed(self, chunk: str) -> tuple[str | None, str | None]:
        """Process a content chunk. Returns (text_delta, reasoning_delta) — either may be None.

        Reasoning is accumulated across chunks and only flushed when the close
        tag arrives or the stream ends (via flush()). This ensures the caller
        receives the complete reasoning block, not fragments.
        """
        self._buf += chunk
        text_parts: list[str] = []
        flushed_reasoning: str | None = None

        while self._buf:
            if self._in_think:
                close_tag = _CLOSE_TAGS[self._current_tag]
                close_idx = self._buf.lower().find(close_tag)
                if close_idx != -1:
                    self._pending_reasoning += self._buf[:close_idx]
                    self._buf = self._buf[close_idx + len(close_tag):]
                    self._in_think = False
                    self._current_tag = None
                    flushed_reasoning = self._pending_reasoning or None
                    self._pending_reasoning = ""
                    continue
                # Check for partial close-tag at the end
                for i in range(len(close_tag) - 1, 0, -1):
                    if self._buf.lower().endswith(close_tag[:i]):
                        self._pending_reasoning += self._buf[:-i]
                        self._buf = self._buf[-i:]
                        return "".join(text_parts) or None, flushed_reasoning
                # No partial — accumulate all as reasoning
                self._pending_reasoning += self._buf
                self._buf = ""
            else:
                # Find the earliest open tag among all variants (case-insensitive)
                buf_lower = self._buf.lower()
                earliest = len(self._buf)
                earliest_tag: str | None = None
                for tag_name, open_tag in _OPEN_TAGS.items():
                    idx = buf_lower.find(open_tag)
                    if idx != -1 and idx < earliest:
                        earliest = idx
                        earliest_tag = tag_name
                if earliest_tag is not None:
                    open_tag = _OPEN_TAGS[earliest_tag]
                    text_parts.append(self._buf[:earliest])
                    self._buf = self._buf[earliest + len(open_tag):]
                    self._in_think = True
                    self._current_tag = earliest_tag
                    continue
                # Check for partial open-tag at the end (any variant)
                for i in range(min(_MAX_OPEN_TAG_LEN - 1, len(self._buf)), 0, -1):
                    if self._buf[-i:].lower() in _OPEN_TAG_PREFIXES:
                        text_parts.append(self._buf[:-i])
                        self._buf = self._buf[-i:]
                        return "".join(text_parts) or None, flushed_reasoning
                # No partial — flush all as text
                text_parts.append(self._buf)
                self._buf = ""

        text = "".join(text_parts) or None
        return text, flushed_reasoning

    def flush(self) -> str | None:
        """Flush any remaining buffer. Returns reasoning text if inside an unclosed block."""
        remaining = self._pending_reasoning + self._buf
        self._pending_reasoning = ""
        self._buf = ""
        if self._in_think:
            self._in_think = False
            self._current_tag = None
            return remaining or None
        return None


@dataclass(frozen=True)
class StreamEvent:
    """A single event from the streaming chat completions endpoint."""

    type: str  # "text_delta" | "reasoning_delta" | "tool_activity" | "finish" | "error" | "stream_interrupted"
    data: str = ""
    label: str = ""
    session_id: str | None = None
    usage: dict | None = None
    error_category: str | None = None


@dataclass
class HeraldAPIExecutor:
    """Talks to the Herald API server at ``/v1/chat/completions``."""

    api_server_url: str = DEFAULT_API_SERVER_URL
    api_server_key: str | None = None

    def _base_url(self) -> str:
        return self.api_server_url.rstrip("/")

    def _is_llama_backend(self) -> bool:
        """True if the backend is a llama.cpp/llama-server instance.

        llama-server doesn't recognize the ``think`` parameter (it's a
        hermes-agent convention). Thinking tokens from llama.cpp appear
        inline as ``<think>...</think>`` blocks and are handled by
        InlineThinkParser — the ``think`` param must be skipped for
        these backends to avoid HTTP 400 errors.
        """
        url_lower = self.api_server_url.lower()
        return "llama" in url_lower or "11435" in url_lower

    def _auth_headers(self) -> dict[str, str]:
        headers: dict[str, str] = {}
        if self.api_server_key:
            headers["Authorization"] = f"Bearer {self.api_server_key}"
        return headers

    @staticmethod
    def _api_role(role: str) -> str:
        if role in ("hermes", "voice_hermes"):
            return "assistant"
        if role == "voice_user":
            return "user"
        return role

    def _messages_payload(
        self,
        *,
        latest_user_message: str,
        history: list[HeraldConversationMessage] | None,
        attachments: list[dict] | None = None,
    ) -> list[dict]:
        messages: list[dict] = [
            {"role": self._api_role(message.role), "content": message.text}
            for message in history or []
            if message.text.strip()
        ]

        # Build the final user message — may be multipart if attachments are present
        if attachments:
            content_parts: list[dict] = []
            if latest_user_message.strip():
                content_parts.append({"type": "text", "text": latest_user_message})
            for att in attachments:
                att_type = att.get("type", "file")
                mime_type = att.get("mimeType", "application/octet-stream")
                b64_data = att.get("data", "")
                if att_type == "image" or mime_type.startswith("image/"):
                    content_parts.append({
                        "type": "image_url",
                        "image_url": {
                            "url": f"data:{mime_type};base64,{b64_data}",
                        },
                    })
                else:
                    # For non-image files, try to decode as text; skip truly binary files
                    filename = att.get("filename", "file")
                    text_mimes = {
                        "text/", "application/json", "application/xml",
                        "application/yaml", "application/x-yaml",
                    }
                    is_text_like = any(mime_type.startswith(prefix) for prefix in text_mimes)
                    if is_text_like:
                        try:
                            import base64
                            decoded = base64.b64decode(b64_data).decode("utf-8")
                        except (UnicodeDecodeError, Exception):
                            decoded = f"[Could not decode file: {filename}]"
                        content_parts.append({
                            "type": "text",
                            "text": f"--- Attached file: {filename} ({mime_type}) ---\n{decoded}",
                        })
                    elif mime_type == "application/pdf":
                        # PDFs can't be passed as text — note their presence
                        content_parts.append({
                            "type": "text",
                            "text": f"[Attached PDF: {filename} — PDF content analysis is not yet supported through this path]",
                        })
                    else:
                        content_parts.append({
                            "type": "text",
                            "text": f"[Attached file: {filename} ({mime_type}) — binary file content not readable]",
                        })
            messages.append({"role": "user", "content": content_parts})
        else:
            messages.append({"role": "user", "content": latest_user_message})

        return messages

    # ------------------------------------------------------------------
    # Health check
    # ------------------------------------------------------------------

    async def health_check(self) -> bool:
        """Return True if the API server is reachable and healthy."""
        for attempt in range(3):
            try:
                async with httpx.AsyncClient(timeout=CONNECT_TIMEOUT) as client:
                    response = await client.get(f"{self._base_url()}/v1/health", headers=self._auth_headers())
                    if response.status_code == 200:
                        body = response.json()
                        return body.get("status") == "ok" or body.get("data", {}).get("status") == "ok"
            except Exception as exc:  # noqa: BLE001
                logger.warning("health check attempt %s/3 failed: %s", attempt + 1, exc)
            if attempt < 2:
                await asyncio.sleep(1)
        return False

    # ------------------------------------------------------------------
    # Non-streaming send
    # ------------------------------------------------------------------

    async def send_message(
        self,
        *,
        latest_user_message: str,
        history: list[HeraldConversationMessage] | None = None,
        session_id: str | None = None,
        attachments: list[dict] | None = None,
        reasoning_effort: str | None = None,
    ) -> HeraldChatResult:
        """Send a single message and wait for the full response."""
        headers = {
            **self._auth_headers(),
            "Content-Type": "application/json",
        }
        if session_id:
            headers["X-Hermes-Session-Id"] = session_id

        payload = {
            "model": "hermes-agent",
            "messages": self._messages_payload(
                latest_user_message=latest_user_message,
                history=history,
                attachments=attachments,
            ),
            "stream": False,
        }

        # Build 28: thinking (reasoning) is off by default. Only enable it
        # when reasoning_effort is explicitly set to a non-"off" value.
        # Skip for llama-server backends — they don't recognize the think
        # param (it's a hermes-agent convention) and would return HTTP 400.
        if not self._is_llama_backend():
            if reasoning_effort and reasoning_effort != "off":
                payload["think"] = True
            else:
                payload["think"] = False

        async with httpx.AsyncClient(
            timeout=httpx.Timeout(connect=CONNECT_TIMEOUT, read=READ_TIMEOUT, write=30.0, pool=30.0),
        ) as client:
            response = await client.post(
                f"{self._base_url()}/v1/chat/completions",
                headers=headers,
                json=payload,
            )
            response.raise_for_status()

            body = response.json()
            result_session_id = response.headers.get("X-Hermes-Session-Id") or session_id

            text = ""
            choices = body.get("choices", [])
            if choices:
                message = choices[0].get("message", {})
                text = message.get("content", "")

            usage = body.get("usage")

            return HeraldChatResult(
                text=text.strip(),
                session_id=result_session_id,
                usage=usage,
            )

    # ------------------------------------------------------------------
    # Streaming send
    # ------------------------------------------------------------------

    async def stream_message(
        self,
        *,
        latest_user_message: str,
        history: list[HeraldConversationMessage] | None = None,
        session_id: str | None = None,
        attachments: list[dict] | None = None,
        reasoning_effort: str | None = None,
    ) -> AsyncIterator[StreamEvent]:
        """Stream a chat completion, yielding events as they arrive.

        Prefers /v1/runs (reasoning + tool events) when available,
        falls back to /v1/chat/completions. Gate on env var
        HERALD_RUNS_STREAMING_ENABLED (default '0' — opt-in).
        """
        # Build 16: /v1/runs is the canonical production chat path.
        # The chat/completions fallback has been removed — it used a different
        # session contract (header-only X-Hermes-Session-Id) and could silently
        # bypass the JSON session_id continuity that /v1/runs guarantees.
        import os
        runs_enabled = os.environ.get("HERALD_RUNS_STREAMING_ENABLED", "1") == "1"
        if runs_enabled and await self._runs_available():
            async for event in self.stream_message_runs(
                latest_user_message=latest_user_message,
                history=history,
                session_id=session_id,
                attachments=attachments,
                reasoning_effort=reasoning_effort,
            ):
                yield event
            return

        # /v1/runs is unavailable — fail explicitly rather than silently
        # falling back to a different executor with different semantics.
        logger.error(
            "runs_unavailable: /v1/runs endpoint is not reachable. "
            "Chat cannot proceed without the runs API."
        )
        yield StreamEvent(
            type="error",
            data="/v1/runs is unavailable. Please ensure the Hermes API server supports the runs endpoint.",
            session_id=session_id,
            error_category="runs_unavailable",
        )
        return
        headers = {
            **self._auth_headers(),
            "Content-Type": "application/json",
        }
        if session_id:
            headers["X-Hermes-Session-Id"] = session_id

        payload = {
            "model": "hermes-agent",
            "messages": self._messages_payload(
                latest_user_message=latest_user_message,
                history=history,
                attachments=attachments,
            ),
            "stream": True,
        }

        # Control thinking based on reasoning_effort.
        # Build 28: thinking is off by default — same logic as send_message.
        if not self._is_llama_backend():
            if reasoning_effort and reasoning_effort != "off":
                payload["think"] = True
            else:
                payload["think"] = False

        async with httpx.AsyncClient(
            timeout=httpx.Timeout(connect=CONNECT_TIMEOUT, read=READ_TIMEOUT, write=30.0, pool=30.0),
        ) as client:
            result_session_id = session_id
            accumulated_usage: dict | None = None
            think_parser = InlineThinkParser()
            accumulated_content = ""

            async with client.stream(
                "POST",
                f"{self._base_url()}/v1/chat/completions",
                headers=headers,
                json=payload,
            ) as response:
                response.raise_for_status()
                result_session_id = response.headers.get("X-Hermes-Session-Id") or session_id

                current_sse_event = None  # Track SSE event type for hermes.tool.progress
                seen_tool_calls = False  # D1.4: track tool-call state

                async for raw_line in response.aiter_lines():
                    line = raw_line.strip()
                    if not line:
                        # Blank line = end of SSE event, reset
                        current_sse_event = None
                        continue
                    if line.startswith(":"):
                        # SSE comment (keepalive), skip
                        continue
                    if line.startswith("event: "):
                        current_sse_event = line[7:].strip()
                        continue
                    if line == "data: [DONE]":
                        break
                    if not line.startswith("data: "):
                        continue

                    json_str = line[6:]  # strip "data: " prefix
                    try:
                        chunk = json.loads(json_str)
                    except json.JSONDecodeError:
                        continue

                    # Handle hermes.tool.progress custom SSE events
                    if current_sse_event == "hermes.tool.progress":
                        tool_name = chunk.get("tool", "")
                        tool_call_id = chunk.get("toolCallId", "")
                        if chunk.get("status", "running") == "running" and tool_name:
                            yield StreamEvent(
                                type="tool_started" if tool_call_id else "tool_activity",
                                label=tool_name,
                                data=json.dumps({
                                    "toolCallId": tool_call_id,
                                    "argsPreview": chunk.get("label", ""),
                                    "emoji": chunk.get("emoji", ""),
                                }),
                            )
                        elif tool_call_id:
                            yield StreamEvent(
                                type="tool_completed",
                                data=json.dumps({
                                    "toolCallId": tool_call_id,
                                    "isError": bool(chunk.get("error")),
                                    "resultPreview": chunk.get("resultPreview", ""),
                                    "durationMs": chunk.get("durationMs"),
                                }),
                            )
                        continue

                    choices = chunk.get("choices", [])
                    if not choices:
                        continue

                    choice = choices[0]
                    delta = choice.get("delta", {})
                    finish_reason = choice.get("finish_reason")

                    # Capture usage from the finish chunk
                    chunk_usage = chunk.get("usage")
                    if chunk_usage:
                        accumulated_usage = chunk_usage

                    # Track whether this chunk produced any user-visible event.
                    # If not, we emit a keepalive so the client watchdog
                    # doesn't fire during tool-execution / subagent windows.
                    yielded_event = False

                    # Reasoning delta — models like mimo/deepseek/qwen/glm expose
                    # chain-of-thought under `reasoning_content` (vLLM/DeepSeek
                    # convention) or `reasoning` (OpenRouter). Stream it on a
                    # separate channel so the app can show it dimmed and collapse
                    # it once the final answer arrives.
                    reasoning = delta.get("reasoning_content") or delta.get("reasoning")
                    if reasoning:
                        yield StreamEvent(
                            type="reasoning_delta",
                            data=reasoning,
                        )
                        yielded_event = True

                    # Content delta — pass through inline think parser to
                    # separate any <think>...</think> blocks from answer text.
                    content = delta.get("content")
                    if content:
                        text_part, reason_part = think_parser.feed(content)
                        if reason_part:
                            yield StreamEvent(type="reasoning_delta", data=reason_part)
                            yielded_event = True
                        if text_part:
                            accumulated_content += text_part
                            yield StreamEvent(type="text_delta", data=text_part)
                            yielded_event = True

                    # Tool-call deltas — the model is invoking tools.
                    tool_calls = delta.get("tool_calls")
                    if tool_calls:
                        seen_tool_calls = True
                        for tc in tool_calls:
                            func = tc.get("function", {})
                            name = func.get("name", "")
                            if name:
                                yield StreamEvent(
                                    type="tool_activity",
                                    label=name,
                                )
                                yielded_event = True

                    # Keepalive: the upstream sent a chunk (so the connection
                    # is alive) but nothing user-visible was in it (e.g.
                    # role-only delta, empty content during subagent work).
                    if not yielded_event and finish_reason != "stop":
                        yield StreamEvent(type="keepalive")

                    if finish_reason == "stop":
                        # Flush any unclosed think block as reasoning
                        remaining_reasoning = think_parser.flush()
                        if remaining_reasoning:
                            yield StreamEvent(type="reasoning_delta", data=remaining_reasoning)
                        # Hermes writes this marker to close a dangling tool
                        # sequence after an upstream failure.  It is transcript
                        # hygiene, not a successful answer.  Classify it before
                        # the tool-call branch below: an interrupted turn is a
                        # failure whether or not tools ran, and letting the
                        # reconnect path win here reports it as a success.
                        _, trailing_sentinel = _split_trailing_sentinel(accumulated_content)
                        if trailing_sentinel:
                            yield StreamEvent(
                                type="error",
                                data=trailing_sentinel,
                                session_id=result_session_id,
                                usage=accumulated_usage,
                                error_category="upstream_interrupted",
                            )
                        elif seen_tool_calls:
                            # D1.4: Tool calls were seen in this stream —
                            # "stop" means end-of-segment, not end-of-turn.
                            # The agentic loop may still be executing on the
                            # host. Yield interrupted so the client can reconnect.
                            #
                            # Note: Hermes' api_server does not emit OpenAI
                            # `tool_calls` deltas at all — it reports tool work
                            # through `event: hermes.tool.progress` and sends a
                            # single terminal `finish_reason: stop` for the whole
                            # turn.  This branch is therefore inert against the
                            # current backend and is kept only for OpenAI-shaped
                            # upstreams.
                            yield StreamEvent(type="stream_interrupted")
                            return
                        else:
                            yield StreamEvent(
                                type="finish",
                                session_id=result_session_id,
                                usage=accumulated_usage,
                            )
                        return

            # The stream ended without a terminal finish_reason.
            # The run may still be executing on the host (B4/D1).
            remaining_reasoning = think_parser.flush()
            if remaining_reasoning:
                yield StreamEvent(type="reasoning_delta", data=remaining_reasoning)
            yield StreamEvent(type="stream_interrupted")

    # ------------------------------------------------------------------
    # /v1/runs streaming (reasoning + tool events)
    # ------------------------------------------------------------------

    async def _runs_available(self) -> bool:
        """Check if /v1/runs endpoint is available on the API server.

        D5.6: Probe with GET /v1/capabilities (which is registered) instead
        of GET /v1/runs (which is POST-only and always returns 405/401,
        making the old check return True unconditionally).
        """
        try:
            async with httpx.AsyncClient(timeout=httpx.Timeout(5.0, connect=5.0)) as client:
                resp = await client.get(
                    f"{self._base_url()}/v1/capabilities",
                    headers=self._auth_headers(),
                )
                if resp.status_code == 200:
                    data = resp.json()
                    available = bool(data.get("features", {}).get("run_events_sse")) \
                        or "runs" in data.get("endpoints", {})
                    logger.info("runs_available=%s", available)
                    return available
                return False
        except Exception as exc:  # noqa: BLE001 - availability must not break chat
            logger.warning("runs availability probe failed: %s", exc)
            return False

    async def _parse_runs_sse(self, lines) -> AsyncIterator[StreamEvent]:
        """Parse SSE events from /v1/runs/{run_id}/events.

        Maps to the existing StreamEvent vocabulary so client.py:1688-1723
        and the iOS app remain unchanged.

        Accepts either an async iterable (aiter_lines) or a sync iterable.
        """
        current_event = None
        current_data_lines = []
        # Models reached over /v1/runs may still embed reasoning inline in
        # assistant deltas instead of emitting reasoning.available.  Route
        # those through the same parser the chat-completions path uses so the
        # thought bubble is fed and the tags never reach the visible answer.
        think_parser = InlineThinkParser()

        # Support both sync and async iterables
        if hasattr(lines, '__aiter__'):
            line_iter = lines
        else:
            async def _async_wrap():
                for item in lines:
                    yield item
            line_iter = _async_wrap()

        async for raw_line in line_iter:
            line = raw_line.rstrip("\n\r")

            if line.startswith("event: "):
                current_event = line[7:].strip()
                continue

            if line.startswith("data: "):
                current_data_lines.append(line[6:])
                continue

            if line.startswith("id: "):
                # SSE id — skip for now
                continue

            if line.strip() == "":
                # Blank line = dispatch event
                if current_data_lines:
                    data_str = "\n".join(current_data_lines)
                    try:
                        data = json.loads(data_str)
                    except json.JSONDecodeError:
                        data = {}

                    event_name = current_event or data.get("event", "")
                    if event_name == "reasoning.available":
                        yield StreamEvent(
                            type="reasoning_delta",
                            data=data.get("text", ""),
                        )
                    elif event_name in ("assistant.delta", "message.delta"):
                        text = data.get("text", data.get("delta", ""))
                        if text:
                            # Defensively strip inline reasoning tags
                            text_part, reason_part = think_parser.feed(text)
                            if reason_part:
                                yield StreamEvent(type="reasoning_delta", data=reason_part)
                            if text_part:
                                yield StreamEvent(type="text_delta", data=text_part)
                    elif event_name == "tool.started":
                        yield StreamEvent(
                            type="tool_started",
                            label=data.get("tool", ""),
                            data=json.dumps({"toolCallId": data.get("toolCallId", ""), "argsPreview": data.get("preview", "")}),
                        )
                    elif event_name == "tool.completed":
                        yield StreamEvent(type="tool_completed", data=json.dumps({"toolCallId": data.get("toolCallId", ""), "resultPreview": data.get("resultPreview", ""), "isError": bool(data.get("error")), "durationMs": data.get("durationMs")}))
                    elif event_name == "approval.request":
                        yield StreamEvent(type="tool_activity", label=data.get("prompt", "Approval required"))
                    elif event_name in ("subagent.start", "subagent.complete"):
                        yield StreamEvent(
                            type="tool_activity",
                            label=data.get("name", data.get("tool", "")),
                        )
                    elif event_name == "run.completed":
                        # Flush any unclosed reasoning block
                        remaining = think_parser.flush()
                        if remaining:
                            yield StreamEvent(type="reasoning_delta", data=remaining)
                        yield StreamEvent(
                            type="finish",
                            session_id=data.get("session_id"),
                            usage=data.get("usage"),
                        )
                        return
                    elif event_name in ("run.failed", "run.cancelled"):
                        remaining = think_parser.flush()
                        if remaining:
                            yield StreamEvent(type="reasoning_delta", data=remaining)
                        # A failed run is not a delivered answer.  Emitting
                        # `finish` here made client.py:1673 report
                        # status="completed" and drop `data` on the floor, so
                        # the phone showed a delivered check for a dead turn.
                        yield StreamEvent(
                            type="error",
                            data=data.get("error") or "The run did not finish.",
                            session_id=data.get("session_id"),
                            usage=data.get("usage"),
                            error_category=data.get("error_category")
                            or "upstream_interrupted",
                        )
                        return
                    else:
                        # Unmapped event → keepalive
                        yield StreamEvent(type="keepalive")

                current_event = None
                current_data_lines = []

        # The events stream ended without run.completed/run.failed.
        # The run may still be executing on the host — reporting "completed"
        # here is what made the app show a delivered check and fire the
        # completion haptic mid-turn (B4/D1).
        remaining = think_parser.flush()
        if remaining:
            yield StreamEvent(type="reasoning_delta", data=remaining)
        yield StreamEvent(type="stream_interrupted")

    @staticmethod
    def _runs_request_payload(*, latest_user_message: str, session_id: str | None) -> dict:
        """Build the documented `/v1/runs` request body.

        The runs API owns its session binding in JSON, unlike the legacy
        chat-completions API which reads the continuity header.
        """
        payload: dict = {
            "model": "hermes-agent",
            "input": latest_user_message,
            "stream": True,
        }
        if session_id:
            payload["session_id"] = session_id
        return payload

    async def stream_message_runs(
        self,
        *,
        latest_user_message: str,
        history: list[HeraldConversationMessage] | None = None,
        session_id: str | None = None,
        attachments: list[dict] | None = None,
        reasoning_effort: str | None = None,
    ) -> AsyncIterator[StreamEvent]:
        """Stream via /v1/runs — exposes reasoning + tool events.

        Falls back to /v1/chat/completions if /v1/runs is unavailable.
        """
        headers = {
            **self._auth_headers(),
            "Content-Type": "application/json",
        }
        if session_id:
            headers["X-Hermes-Session-Id"] = session_id

        # /v1/runs is NOT chat-completions shaped: it takes a single `input`
        # string and rejects a `messages` array outright with
        # {"error": {"message": "Missing 'input' field"}}, which raised on
        # raise_for_status() and failed every turn the moment the runs path was
        # enabled.  History is carried by the session, not the payload.
        payload = self._runs_request_payload(
            latest_user_message=latest_user_message,
            session_id=session_id,
        )
        # `/v1/runs` does not use the chat-completions continuity header.
        # It binds an existing Hermes transcript only from this JSON field.
        # Sending the id only as X-Hermes-Session-Id silently created a new
        # `run_…` session for every Herald turn, which both duplicated sidebar
        # rows and made the agent lose the conversation it had just answered.

        if not self._is_llama_backend():
            if reasoning_effort and reasoning_effort != "off":
                payload["think"] = True
            else:
                payload["think"] = False

        async with httpx.AsyncClient(
            timeout=httpx.Timeout(connect=CONNECT_TIMEOUT, read=READ_TIMEOUT, write=30.0, pool=30.0),
        ) as client:
            # Start a run
            resp = await client.post(
                f"{self._base_url()}/v1/runs",
                headers=headers,
                json=payload,
            )
            resp.raise_for_status()
            run_data = resp.json()
            run_id = run_data.get("run_id") or run_data.get("id")

            if not run_id:
                raise ValueError("No run_id in /v1/runs response")

            # Stream events through the canonical SSE parser
            async with client.stream(
                "GET",
                f"{self._base_url()}/v1/runs/{run_id}/events",
                headers=self._auth_headers(),
            ) as event_response:
                event_response.raise_for_status()
                async for event in self._parse_runs_sse(event_response.aiter_lines()):
                    if event.type == "stream_interrupted":
                        break  # Fall through to resume logic below
                    yield event
                else:
                    # _parse_runs_sse returned normally (finish/failed)
                    return

            # The events stream ended without run.completed/run.failed.
            # The run may still be executing on the host — attempt to
            # reconnect before giving up (B4/D1).
            backoffs = [2, 4, 8]
            for attempt, delay in enumerate(backoffs):
                await asyncio.sleep(delay)
                try:
                    status_resp = await client.get(
                        f"{self._base_url()}/v1/runs/{run_id}",
                        headers=self._auth_headers(),
                    )
                    status_data = status_resp.json()
                    run_status = status_data.get("status", "")
                    if run_status == "completed":
                        yield StreamEvent(
                            type="finish",
                            session_id=status_data.get("session_id"),
                            usage=status_data.get("usage"),
                        )
                        return
                    if run_status in ("failed", "cancelled"):
                        # Previously lumped in with "completed", which reported
                        # a dead run to the phone as a delivered answer.
                        yield StreamEvent(
                            type="error",
                            data=status_data.get("error") or f"The run {run_status}.",
                            session_id=status_data.get("session_id"),
                            usage=status_data.get("usage"),
                            error_category="upstream_interrupted",
                        )
                        return
                    # Run still active — try to reconnect to /events
                    async with client.stream(
                        "GET",
                        f"{self._base_url()}/v1/runs/{run_id}/events",
                        headers=self._auth_headers(),
                    ) as retry_response:
                        retry_response.raise_for_status()
                        async for event in self._parse_runs_sse(retry_response.aiter_lines()):
                            if event.type == "stream_interrupted":
                                break  # Try next attempt
                            yield event
                        else:
                            # _parse_runs_sse returned normally
                            return
                except Exception:
                    continue

            # All reconnect attempts exhausted
            yield StreamEvent(type="stream_interrupted")
