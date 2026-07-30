"""Hermes API server executor — HTTP/SSE alternative to the CLI subprocess."""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import AsyncIterator

import httpx

from .herald_runner import HeraldChatResult, HeraldConversationMessage


DEFAULT_API_SERVER_URL = "http://localhost:8642"
CONNECT_TIMEOUT = 10.0
READ_TIMEOUT = 300.0  # 5 minutes — long enough for Claude thinking, catches dead connections

_OPEN_TAG = "<think>"
_CLOSE_TAG = "</think>"


class InlineThinkParser:
    """Stateful parser that extracts inline <think>...</think> blocks from content deltas.

    Handles markers that span chunk boundaries. When inside a think block,
    content is routed to reasoning; outside, to text. If the stream ends
    with an unclosed think block, the buffered text is emitted as reasoning.
    """

    def __init__(self) -> None:
        self._in_think = False
        self._buf = ""
        self._pending_reasoning = ""

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
                close_idx = self._buf.find(_CLOSE_TAG)
                if close_idx != -1:
                    self._pending_reasoning += self._buf[:close_idx]
                    self._buf = self._buf[close_idx + len(_CLOSE_TAG):]
                    self._in_think = False
                    flushed_reasoning = self._pending_reasoning or None
                    self._pending_reasoning = ""
                    continue
                # Check for partial close tag at the end
                for i in range(len(_CLOSE_TAG) - 1, 0, -1):
                    if self._buf.endswith(_CLOSE_TAG[:i]):
                        self._pending_reasoning += self._buf[:-i]
                        self._buf = self._buf[-i:]
                        return "".join(text_parts) or None, flushed_reasoning
                # No partial — accumulate all as reasoning
                self._pending_reasoning += self._buf
                self._buf = ""
            else:
                open_idx = self._buf.find(_OPEN_TAG)
                if open_idx != -1:
                    text_parts.append(self._buf[:open_idx])
                    self._buf = self._buf[open_idx + len(_OPEN_TAG):]
                    self._in_think = True
                    continue
                # Check for partial open tag at the end
                for i in range(len(_OPEN_TAG) - 1, 0, -1):
                    if self._buf.endswith(_OPEN_TAG[:i]):
                        text_parts.append(self._buf[:-i])
                        self._buf = self._buf[-i:]
                        return "".join(text_parts) or None, flushed_reasoning
                # No partial — flush all as text
                text_parts.append(self._buf)
                self._buf = ""

        text = "".join(text_parts) or None
        return text, flushed_reasoning

    def flush(self) -> str | None:
        """Flush any remaining buffer. Returns reasoning text if inside an unclosed think block."""
        remaining = self._pending_reasoning + self._buf
        self._pending_reasoning = ""
        self._buf = ""
        if self._in_think:
            self._in_think = False
            return remaining or None
        return None


@dataclass(frozen=True)
class StreamEvent:
    """A single event from the streaming chat completions endpoint."""

    type: str  # "text_delta" | "reasoning_delta" | "tool_activity" | "finish"
    data: str = ""
    label: str = ""
    session_id: str | None = None
    usage: dict | None = None


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
        try:
            async with httpx.AsyncClient(timeout=CONNECT_TIMEOUT) as client:
                response = await client.get(
                    f"{self._base_url()}/v1/health",
                    headers=self._auth_headers(),
                )
                if response.status_code == 200:
                    body = response.json()
                    return body.get("status") == "ok" or body.get("data", {}).get("status") == "ok"
        except Exception:  # noqa: BLE001
            pass
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
        HERALD_RUNS_STREAMING_ENABLED (default '1').
        """
        # Try /v1/runs first if enabled
        import os
        runs_enabled = os.environ.get("HERALD_RUNS_STREAMING_ENABLED", "1") != "0"
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

            async with client.stream(
                "POST",
                f"{self._base_url()}/v1/chat/completions",
                headers=headers,
                json=payload,
            ) as response:
                response.raise_for_status()
                result_session_id = response.headers.get("X-Hermes-Session-Id") or session_id

                current_sse_event = None  # Track SSE event type for hermes.tool.progress

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
                        if tool_name:
                            yield StreamEvent(
                                type="tool_activity",
                                label=tool_name,
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
                            yield StreamEvent(type="text_delta", data=text_part)
                            yielded_event = True

                    # Tool-call deltas — the model is invoking tools.
                    tool_calls = delta.get("tool_calls")
                    if tool_calls:
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
                        yield StreamEvent(
                            type="finish",
                            session_id=result_session_id,
                            usage=accumulated_usage,
                        )
                        return

            # If we exited the stream without a finish event, emit one
            remaining_reasoning = think_parser.flush()
            if remaining_reasoning:
                yield StreamEvent(type="reasoning_delta", data=remaining_reasoning)
            yield StreamEvent(
                type="finish",
                session_id=result_session_id,
                usage=accumulated_usage,
            )

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
            async with httpx.AsyncClient(timeout=httpx.Timeout(connect=5.0, read=5.0)) as client:
                resp = await client.get(
                    f"{self._base_url()}/v1/capabilities",
                    headers=self._auth_headers(),
                )
                if resp.status_code == 200:
                    data = resp.json()
                    return "runs" in data.get("capabilities", [])
                return False
        except Exception:
            return False

    async def _parse_runs_sse(self, lines) -> AsyncIterator[StreamEvent]:
        """Parse SSE events from /v1/runs/{run_id}/events.

        Maps to the existing StreamEvent vocabulary so client.py:1688-1723
        and the iOS app remain unchanged.
        """
        current_event = None
        current_data_lines = []

        for raw_line in lines:
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
                if current_event and current_data_lines:
                    data_str = "\n".join(current_data_lines)
                    try:
                        data = json.loads(data_str)
                    except json.JSONDecodeError:
                        data = {}

                    if current_event == "reasoning.available":
                        yield StreamEvent(
                            type="reasoning_delta",
                            data=data.get("text", ""),
                        )
                    elif current_event == "assistant.delta":
                        text = data.get("text", "")
                        if text:
                            yield StreamEvent(type="text_delta", data=text)
                    elif current_event == "tool.started":
                        yield StreamEvent(
                            type="tool_activity",
                            label=data.get("tool", ""),
                        )
                    elif current_event == "tool.completed":
                        # Tool done — could yield a keepalive or skip
                        pass
                    elif current_event in ("subagent.start", "subagent.complete"):
                        yield StreamEvent(
                            type="tool_activity",
                            label=data.get("name", data.get("tool", "")),
                        )
                    elif current_event == "run.completed":
                        yield StreamEvent(
                            type="finish",
                            session_id=data.get("session_id"),
                            usage=data.get("usage"),
                        )
                        return
                    elif current_event == "run.failed":
                        yield StreamEvent(
                            type="finish",
                            data=data.get("error", "Run failed"),
                        )
                        return
                    else:
                        # Unmapped event → keepalive
                        yield StreamEvent(type="keepalive")

                current_event = None
                current_data_lines = []

        # If we exited without a finish event, emit one
        yield StreamEvent(type="finish")

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

        payload = {
            "model": "hermes-agent",
            "messages": self._messages_payload(
                latest_user_message=latest_user_message,
                history=history,
                attachments=attachments,
            ),
            "stream": True,
        }

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

            # Stream events
            async with client.stream(
                "GET",
                f"{self._base_url()}/v1/runs/{run_id}/events",
                headers=self._auth_headers(),
            ) as event_response:
                event_response.raise_for_status()
                async for raw_line in event_response.aiter_lines():
                    line = raw_line.strip()
                    if not line:
                        continue
                    if line.startswith(":"):
                        continue
                    # Parse SSE properly
                    if line.startswith("event: "):
                        current_event = line[7:].strip()
                        continue
                    if line.startswith("data: "):
                        data_str = line[6:]
                        try:
                            data = json.loads(data_str)
                        except json.JSONDecodeError:
                            data = {}

                        if current_event == "reasoning.available":
                            yield StreamEvent(
                                type="reasoning_delta",
                                data=data.get("text", ""),
                            )
                        elif current_event == "assistant.delta":
                            text = data.get("text", "")
                            if text:
                                yield StreamEvent(type="text_delta", data=text)
                        elif current_event == "tool.started":
                            yield StreamEvent(
                                type="tool_activity",
                                label=data.get("tool", ""),
                            )
                        elif current_event in ("subagent.start", "subagent.complete"):
                            yield StreamEvent(
                                type="tool_activity",
                                label=data.get("name", data.get("tool", "")),
                            )
                        elif current_event == "run.completed":
                            yield StreamEvent(
                                type="finish",
                                session_id=data.get("session_id"),
                                usage=data.get("usage"),
                            )
                            return
                        elif current_event == "run.failed":
                            yield StreamEvent(
                                type="finish",
                                data=data.get("error", "Run failed"),
                            )
                            return
                        else:
                            yield StreamEvent(type="keepalive")

            # If we exited without a finish event, emit one
            yield StreamEvent(type="finish")
