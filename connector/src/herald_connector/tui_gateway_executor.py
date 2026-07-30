"""Opt-in Hermes dashboard WebSocket transport.

This module deliberately talks only to the dashboard's supported JSON-RPC
surface.  It neither imports nor modifies Hermes.  The normal chat-completions
adapter remains the default; set ``HERALD_TRANSPORT=tui_ws`` only after the
dashboard credentials have been provisioned to the connector environment.
"""

from __future__ import annotations

import asyncio
import itertools
import json
import logging
import os
from collections.abc import AsyncIterator
from dataclasses import dataclass
from urllib.parse import quote

import httpx
import websockets

from .herald_api_executor import StreamEvent
from .herald_runner import HeraldChatResult, HeraldConversationMessage

logger = logging.getLogger(__name__)


def _is_unknown_method_error(exc: RuntimeError, method: str) -> bool:
    """Return whether a gateway rejected an optional RPC as unsupported.

    The dashboard's RPC surface changes independently of Herald.  Optional
    presentation/configuration calls must never prevent ``prompt.submit`` from
    reaching Hermes; authentication, session, and prompt RPC failures remain
    fatal and are intentionally not handled here.
    """
    message = str(exc).lower()
    return method.lower() in message and (
        "unknown method" in message or "method not found" in message
    )


def _payload(frame: dict) -> dict:
    """Accept the gateway's event envelope and its compact variants."""
    value = frame.get("params") or frame.get("data") or frame.get("payload") or frame
    if frame.get("method") == "event" and isinstance(value, dict):
        value = value.get("payload") or value
    return value if isinstance(value, dict) else {}


def _event_name(frame: dict) -> str:
    if frame.get("method") == "event":
        params = frame.get("params") or {}
        if isinstance(params, dict):
            return str(params.get("type") or params.get("event") or "")
    return str(frame.get("method") or frame.get("event") or frame.get("type") or "")


@dataclass
class TuiGatewayExecutor:
    """Map gateway JSON-RPC events onto the connector's StreamEvent contract."""

    gateway_url: str = "http://127.0.0.1:9119"
    auth_provider: str | None = None
    username: str | None = None
    password: str | None = None

    def __post_init__(self) -> None:
        self.gateway_url = self.gateway_url.rstrip("/")
        self.auth_provider = self.auth_provider or os.getenv("HERALD_GW_AUTH_PROVIDER", "basic")
        self.username = self.username or os.getenv("HERALD_GW_USERNAME")
        self.password = self.password or os.getenv("HERALD_GW_PASSWORD")
        self._cookie: str | None = None
        self._ids = itertools.count(1)

    @property
    def _ws_url(self) -> str:
        if self.gateway_url.startswith("https://"):
            return "wss://" + self.gateway_url.removeprefix("https://") + "/api/ws"
        return "ws://" + self.gateway_url.removeprefix("http://") + "/api/ws"

    async def _login_and_ticket(self) -> str:
        """Return one short-lived, single-use WebSocket ticket.

        Cookies are retained for the dashboard-session lifetime.  Credentials
        are intentionally never included in log messages or exceptions.
        """
        if not self.username or not self.password:
            raise RuntimeError("Gateway WebSocket credentials are not configured")
        async with httpx.AsyncClient(timeout=httpx.Timeout(15.0)) as client:
            if self._cookie:
                client.headers["Cookie"] = self._cookie
            ticket = await client.post(f"{self.gateway_url}/api/auth/ws-ticket")
            if ticket.status_code == 401:
                login = await client.post(
                    f"{self.gateway_url}/auth/password-login",
                    json={
                        "provider": self.auth_provider,
                        "username": self.username,
                        "password": self.password,
                    },
                )
                login.raise_for_status()
                cookie = login.headers.get("set-cookie", "").split(";", 1)[0]
                if not cookie:
                    raise RuntimeError("Gateway login did not return a session cookie")
                self._cookie = cookie
                ticket = await client.post(
                    f"{self.gateway_url}/api/auth/ws-ticket",
                    headers={"Cookie": cookie},
                )
            ticket.raise_for_status()
            value = ticket.json().get("ticket")
            if not value:
                raise RuntimeError("Gateway did not return a WebSocket ticket")
            return str(value)

    async def _connect(self):
        ticket = await self._login_and_ticket()
        ws = await websockets.connect(f"{self._ws_url}?ticket={quote(ticket, safe='')}", open_timeout=20)
        raw = await asyncio.wait_for(ws.recv(), timeout=20)
        try:
            ready = json.loads(raw)
        except (TypeError, json.JSONDecodeError) as exc:
            await ws.close()
            raise RuntimeError("Gateway returned an invalid readiness frame") from exc
        if _event_name(ready) != "gateway.ready":
            await ws.close()
            raise RuntimeError("Gateway did not send gateway.ready")
        return ws

    async def health_check(self) -> bool:
        try:
            ws = await self._connect()
            await ws.close()
            return True
        except Exception as exc:  # availability must fall back safely
            logger.warning("TUI gateway transport unavailable: %s", type(exc).__name__)
            return False

    async def _call(self, ws, method: str, params: dict) -> dict:
        request_id = next(self._ids)
        await ws.send(json.dumps({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}))
        while True:
            raw = await ws.recv()
            frame = json.loads(raw)
            if frame.get("id") != request_id:
                # Calls are only made before prompt.submit; an unsolicited
                # event at this point is harmless and cannot be attributed.
                continue
            if frame.get("error"):
                raise RuntimeError(str(frame["error"].get("message", "Gateway RPC failed")))
            result = frame.get("result") or {}
            return result if isinstance(result, dict) else {}

    async def stream_message(
        self,
        *,
        latest_user_message: str,
        history: list[HeraldConversationMessage] | None = None,
        session_id: str | None = None,
        attachments: list[dict] | None = None,
        reasoning_effort: str | None = None,
    ) -> AsyncIterator[StreamEvent]:
        """Submit a turn and translate live text/reasoning/tool events."""
        del history  # Gateway owns history once a session is resumed.
        ws = await self._connect()
        active_session = session_id
        try:
            if active_session:
                resumed = await self._call(ws, "session.resume", {"session_id": active_session})
                active_session = str(resumed.get("session_id") or resumed.get("id") or active_session)
            else:
                created = await self._call(ws, "session.create", {})
                active_session = str(created.get("session_id") or created.get("id") or "") or None
            if reasoning_effort and reasoning_effort != "off":
                try:
                    await self._call(ws, "agent.reasoning_effort", {"effort": reasoning_effort})
                except RuntimeError as exc:
                    # Older dashboard gateways do not implement this optional
                    # setting.  Preserve their configured/default reasoning
                    # behavior and continue with the actual user turn.
                    if not _is_unknown_method_error(exc, "agent.reasoning_effort"):
                        raise
                    logger.warning(
                        "Gateway does not support agent.reasoning_effort; "
                        "submitting turn with its configured default"
                    )

            params: dict[str, object] = {"text": latest_user_message}
            if active_session:
                params["session_id"] = active_session
            if attachments:
                params["attachments"] = attachments
            await ws.send(json.dumps({"jsonrpc": "2.0", "id": next(self._ids), "method": "prompt.submit", "params": params}))

            async for raw in ws:
                try:
                    frame = json.loads(raw)
                except (TypeError, json.JSONDecodeError):
                    continue
                event = _event_name(frame)
                data = _payload(frame)
                frame_session = data.get("session_id") or data.get("sessionId")
                if frame_session:
                    active_session = str(frame_session)
                if event == "message.delta":
                    text = data.get("delta") or data.get("text") or ""
                    if text:
                        yield StreamEvent(type="text_delta", data=str(text))
                elif event == "reasoning.available":
                    text = data.get("text") or data.get("delta") or ""
                    if text:
                        yield StreamEvent(type="reasoning_delta", data=str(text))
                elif event == "tool.started":
                    yield StreamEvent(type="tool_started", label=str(data.get("tool") or data.get("name") or "tool"), data=json.dumps({"toolCallId": data.get("tool_call_id") or data.get("toolCallId") or "", "argsPreview": data.get("preview") or data.get("args") or "", "emoji": data.get("emoji") or ""}))
                elif event == "tool.completed":
                    yield StreamEvent(type="tool_completed", data=json.dumps({"toolCallId": data.get("tool_call_id") or data.get("toolCallId") or "", "resultPreview": data.get("result_preview") or data.get("resultPreview") or "", "isError": bool(data.get("error")), "durationMs": data.get("duration_ms") or data.get("durationMs")}))
                elif event == "message.complete":
                    yield StreamEvent(type="finish", session_id=active_session, usage=data.get("usage"))
                    return
                elif event in {"message.error", "run.failed"}:
                    yield StreamEvent(type="error", data=str(data.get("error") or "Gateway turn failed"), session_id=active_session, error_category="upstream_interrupted")
                    return
            # EOF without a terminal event is a transport interruption, never
            # a successful completion.
            yield StreamEvent(type="stream_interrupted")
        except websockets.ConnectionClosed:
            yield StreamEvent(type="stream_interrupted")
        finally:
            await ws.close()

    async def send_message(self, **kwargs) -> HeraldChatResult:
        """Compatibility path for non-streaming callers."""
        text: list[str] = []
        session_id = kwargs.get("session_id")
        usage = None
        async for event in self.stream_message(**kwargs):
            if event.type == "text_delta":
                text.append(event.data)
            elif event.type == "finish":
                session_id, usage = event.session_id or session_id, event.usage
            elif event.type == "error":
                raise RuntimeError(event.data)
        return HeraldChatResult(text="".join(text), session_id=session_id, usage=usage)
