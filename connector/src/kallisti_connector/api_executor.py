"""OpenAI-compatible executor against the Hermes gateway api_server.

The connector's ``HeraldAPIRuntimeAdapter`` was written against an executor
that never existed, so every message fell through to ``HeraldCLIExecutor`` —
a fresh ``hermes`` subprocess per turn whose cold start (plugin discovery
alone is 60-90s) made latency swing between ~16s and 200s+ and blew past the
relay's job deadline on real turns.

This talks to the already-running gateway api_server instead: one persistent
process, ~2s per simple turn, with real SSE deltas and reasoning.
"""

from __future__ import annotations

import json
import os
from dataclasses import dataclass
from typing import AsyncIterator

import httpx

from .herald_runner import (
    HeraldChatResult,
    HeraldConversationMessage,
    StreamEvent,
)


@dataclass(frozen=True)
class HeraldAPIExecutorSettings:
    """Connection settings for the gateway api_server."""

    base_url: str
    api_key: str
    model: str = "hermes-agent"
    request_timeout_seconds: float = 900.0

    @classmethod
    def from_env(
        cls,
        *,
        base_url: str | None = None,
        api_key: str | None = None,
        model: str | None = None,
    ) -> "HeraldAPIExecutorSettings":
        resolved_url = (
            base_url
            or os.getenv("HERMES_API_SERVER_URL")
            or "http://127.0.0.1:9119"
        ).rstrip("/")
        resolved_key = api_key or os.getenv("HERMES_API_SERVER_KEY") or ""
        resolved_model = model or os.getenv("HERMES_API_SERVER_MODEL") or "hermes-agent"
        timeout_raw = os.getenv("HERMES_API_SERVER_TIMEOUT_SECONDS", "")
        try:
            timeout = float(timeout_raw) if timeout_raw else 900.0
        except ValueError:
            timeout = 900.0
        return cls(
            base_url=resolved_url,
            api_key=resolved_key,
            model=resolved_model,
            request_timeout_seconds=timeout,
        )


class HeraldAPIExecutor:
    """Calls ``POST {base_url}/v1/chat/completions`` on the running gateway.

    Mirrors ``HeraldCLIExecutor``'s public surface (``send_message`` /
    ``stream_message``) so ``HeraldAPIRuntimeAdapter`` can wrap it unchanged.
    """

    def __init__(self, settings: HeraldAPIExecutorSettings) -> None:
        self.settings = settings

    # ── helpers ────────────────────────────────────────────────────────

    def _headers(self, *, session_id: str | None, stream: bool) -> dict[str, str]:
        headers = {
            "Authorization": f"Bearer {self.settings.api_key}",
            "Content-Type": "application/json",
            "Accept": "text/event-stream" if stream else "application/json",
        }
        # Continues an existing gateway session (history from state.db) rather
        # than replaying the transcript. Same contract the OpenAI route reads.
        if session_id:
            headers["X-Hermes-Session-Id"] = session_id
        return headers

    def _payload(
        self,
        *,
        latest_user_message: str,
        history: list[HeraldConversationMessage],
        stream: bool,
        reasoning_effort: str | None = None,
        model: str | None = None,
    ) -> dict:
        messages: list[dict[str, str]] = []
        for item in history:
            role = item.role if item.role in ("user", "assistant", "system") else "user"
            messages.append({"role": role, "content": item.text})
        messages.append({"role": "user", "content": latest_user_message})

        payload: dict = {
            "model": model or self.settings.model,
            "messages": messages,
            "stream": stream,
        }
        if reasoning_effort:
            payload["reasoning_effort"] = reasoning_effort
        return payload

    @staticmethod
    def _usage_from(payload: dict | None) -> dict | None:
        """Map an OpenAI usage block onto the client's wire keys.

        The iOS ``TokenUsage`` type declares CodingKeys prompt_tokens /
        completion_tokens / total_tokens, so emitting camelCase here (as an
        earlier revision did) made every response carrying usage fail to
        decode with \"The data couldn't be read because it is missing.\"
        """
        if not isinstance(payload, dict):
            return None
        usage = payload.get("usage")
        if not isinstance(usage, dict):
            return None
        return {
            "prompt_tokens": usage.get("prompt_tokens"),
            "completion_tokens": usage.get("completion_tokens"),
            "total_tokens": usage.get("total_tokens"),
        }

    # ── non-streaming ──────────────────────────────────────────────────

    async def send_message(
        self,
        *,
        latest_user_message: str,
        history: list[HeraldConversationMessage],
        session_id: str | None = None,
        model: str | None = None,
    ) -> HeraldChatResult:
        url = f"{self.settings.base_url}/v1/chat/completions"
        body = self._payload(
            latest_user_message=latest_user_message,
            history=history,
            stream=False,
            model=model,
        )
        async with httpx.AsyncClient(timeout=self.settings.request_timeout_seconds) as client:
            response = await client.post(
                url,
                json=body,
                headers=self._headers(session_id=session_id, stream=False),
            )
            response.raise_for_status()
            data = response.json()

        choices = data.get("choices") or []
        text = ""
        if choices:
            text = (choices[0].get("message") or {}).get("content") or ""
        text = text.strip()
        if not text:
            raise RuntimeError("Hermes API server returned an empty response.")

        return HeraldChatResult(
            text=text,
            session_id=response.headers.get("X-Hermes-Session-Id") or session_id,
            usage=self._usage_from(data),
        )

    # ── streaming ──────────────────────────────────────────────────────

    @staticmethod
    def _tool_progress_event(payload: dict, *, session_id: str | None) -> "StreamEvent | None":
        """Map one ``hermes.tool.progress`` frame onto a tool StreamEvent.

        The gateway sends status=running with the tool name, emoji and an
        argument preview (``agent.display.build_tool_preview``), then
        status=completed with the same toolCallId. ``event.data`` carries the
        JSON the connector's job path forwards: the relay maps it onto the
        dotted keys the iOS client decodes (tool_call_id / name / args / emoji).
        """
        tool_call_id = str(payload.get("toolCallId") or payload.get("tool_call_id") or "")
        tool = str(payload.get("tool") or payload.get("name") or "tool")
        status = str(payload.get("status") or "running").lower()
        if status in ("completed", "complete", "failed", "error"):
            return StreamEvent(
                type="tool_completed",
                data=json.dumps({"toolCallId": tool_call_id, "name": tool}),
                label=tool,
                session_id=session_id,
            )
        preview = str(payload.get("label") or payload.get("args") or tool)
        return StreamEvent(
            type="tool_started",
            data=json.dumps({
                "toolCallId": tool_call_id,
                "name": tool,
                "argsPreview": preview,
                "emoji": payload.get("emoji") or "",
            }),
            label=preview,
            session_id=session_id,
        )

    async def stream_message(
        self,
        *,
        latest_user_message: str,
        history: list[HeraldConversationMessage],
        session_id: str | None = None,
        attachments: list[dict] | None = None,
        reasoning_effort: str | None = None,
        job_id: str | None = None,
        model: str | None = None,
    ) -> AsyncIterator[StreamEvent]:
        """Yield StreamEvents from the SSE response.

        ``attachments`` is accepted for interface parity; staging already
        happened upstream in the connector, so the text carries the context.
        ``model`` overrides the configured model for this turn only (note
        enrichment can be pointed at a vision-capable model without changing
        the gateway's default).
        """
        del attachments, job_id  # handled before this layer

        url = f"{self.settings.base_url}/v1/chat/completions"
        body = self._payload(
            latest_user_message=latest_user_message,
            history=history,
            stream=True,
            reasoning_effort=reasoning_effort,
            model=model,
        )

        accumulated: list[str] = []
        final_usage: dict | None = None
        resolved_session = session_id

        try:
            async with httpx.AsyncClient(timeout=self.settings.request_timeout_seconds) as client:
                async with client.stream(
                    "POST",
                    url,
                    json=body,
                    headers=self._headers(session_id=session_id, stream=True),
                ) as response:
                    response.raise_for_status()
                    resolved_session = (
                        response.headers.get("X-Hermes-Session-Id") or session_id
                    )
                    sse_event = ""
                    async for raw_line in response.aiter_lines():
                        if raw_line.startswith("event:"):
                            sse_event = raw_line[6:].strip()
                            continue
                        if not raw_line.startswith("data:"):
                            continue
                        chunk = raw_line[5:].strip()
                        if not chunk or chunk == "[DONE]":
                            continue
                        try:
                            event = json.loads(chunk)
                        except json.JSONDecodeError:
                            continue

                        # The gateway writes tool lifecycle as a NAMED SSE frame
                        # ("event: hermes.tool.progress") whose payload has no
                        # choices[]. The delta loop below therefore dropped every
                        # tool frame, which is why job_events held zero tool rows
                        # and the app could never render tool activity.
                        frame_event, sse_event = sse_event, ""
                        if frame_event == "hermes.tool.progress" or (
                            not event.get("choices") and "toolCallId" in event
                        ):
                            tool_event = self._tool_progress_event(event, session_id=resolved_session)
                            if tool_event is not None:
                                yield tool_event
                            continue

                        if isinstance(event.get("usage"), dict):
                            final_usage = self._usage_from(event)

                        for choice in event.get("choices") or []:
                            delta = choice.get("delta") or {}
                            reasoning = delta.get("reasoning_content") or delta.get("reasoning")
                            if reasoning:
                                yield StreamEvent(
                                    type="reasoning_delta",
                                    data=str(reasoning),
                                    session_id=resolved_session,
                                )
                            content = delta.get("content")
                            if content:
                                accumulated.append(str(content))
                                yield StreamEvent(
                                    type="text_delta",
                                    data=str(content),
                                    session_id=resolved_session,
                                )
        except httpx.HTTPStatusError as error:
            yield StreamEvent(
                type="error",
                data=f"Hermes API server returned HTTP {error.response.status_code}.",
                error_category="upstream_http_error",
                session_id=resolved_session,
            )
            return
        except Exception as error:  # noqa: BLE001
            yield StreamEvent(
                type="error",
                data=str(error),
                error_category="upstream_transport_error",
                session_id=resolved_session,
            )
            return

        terminal_text = "".join(accumulated).strip()
        if not terminal_text:
            yield StreamEvent(
                type="error",
                data="Hermes API server streamed no content.",
                error_category="empty_response",
                session_id=resolved_session,
            )
            return

        yield StreamEvent(
            type="finish",
            data=terminal_text,
            output=terminal_text,
            session_id=resolved_session,
            usage=final_usage,
        )
