"""Native-completion to push bridge.

The iOS app submits ``prompt.submit`` directly to Hermes in the native
world, so the connector needs its own watcher to notice turn completions
and fire APNs / Live Activity pushes.  This module provides:

* ``NativeWatchRegistry`` -- maps session_ids to device push tokens.
* ``NativeWatchTokens`` -- self-refreshing access token for the native
  gateway WebSocket.
* ``run_watcher`` -- long-running asyncio task that holds one persistent
  WS connection to the native gateway and fires pushes on terminal events.
"""

from __future__ import annotations

import asyncio
import json
import logging
from pathlib import Path

logger = logging.getLogger(__name__)

CONFIG_PATH = Path.home() / ".config" / "kallisti-native-watch.json"
NATIVE_GATEWAY_HOST = "192.168.10.118"
NATIVE_GATEWAY_PORT = 9119


class NativeWatchRegistry:
    """session_id -> [(device_token, token_kind), ...]. In-memory only."""

    def __init__(self) -> None:
        self._watchers: dict[str, list[tuple[str, str]]] = {}

    def watch(
        self, *, session_id: str, device_token: str, token_kind: str
    ) -> None:
        entries = self._watchers.setdefault(session_id, [])
        if (device_token, token_kind) not in entries:
            entries.append((device_token, token_kind))

    def watchers_for(self, session_id: str) -> list[tuple[str, str]]:
        return list(self._watchers.get(session_id, []))


class NativeWatchTokens:
    """Self-rotating access token from refresh_token stored in CONFIG_PATH."""

    def __init__(self, session) -> None:
        self._session = session
        self._access_token: str | None = None
        self._refresh_token: str | None = None
        self._load()

    def _load(self) -> None:
        data = json.loads(CONFIG_PATH.read_text())
        self._refresh_token = data["refresh_token"]

    async def access_token(self) -> str:
        if self._access_token is None:
            await self._refresh()
        return self._access_token

    async def _refresh(self) -> None:
        url = (
            f"http://{NATIVE_GATEWAY_HOST}:{NATIVE_GATEWAY_PORT}"
            "/auth/native/refresh"
        )
        async with self._session.post(
            url, json={"refresh_token": self._refresh_token}
        ) as resp:
            if resp.status != 200:
                raise RuntimeError(
                    f"native watch token refresh failed: {resp.status}"
                )
            body = await resp.json()
            self._access_token = body["access_token"]
            self._refresh_token = body["refresh_token"]


async def run_watcher(
    registry: NativeWatchRegistry,
    tokens: NativeWatchTokens,
    send_completion_push,
    end_live_activity,
    terminal_event_type: str,
) -> None:
    """Long-running background task -- start once at connector boot.

    Holds one persistent WebSocket connection to the native gateway and
    fires pushes when a terminal event matches a watched session.

    ``terminal_event_type`` defaults to ``"turn.complete"`` in the brief
    but this MUST be confirmed against a live SSE capture -- the actual
    event type emitted by Hermes for a completed turn may differ.
    """
    import websockets  # noqa: F811 -- local import to keep module loadable

    backoff = 1.0
    while True:
        try:
            access_token = await tokens.access_token()
            ticket_url = (
                f"http://{NATIVE_GATEWAY_HOST}:{NATIVE_GATEWAY_PORT}"
                "/api/auth/ws-ticket"
            )
            async with tokens._session.post(
                ticket_url,
                headers={"Authorization": f"Bearer {access_token}"},
            ) as resp:
                ticket = (await resp.json())["ticket"]

            uri = (
                f"ws://{NATIVE_GATEWAY_HOST}:{NATIVE_GATEWAY_PORT}"
                f"/api/ws?ticket={ticket}"
            )
            async with websockets.connect(uri) as ws:
                backoff = 1.0
                async for raw in ws:
                    frame = json.loads(raw)
                    params = frame.get("params")
                    if (
                        not isinstance(params, dict)
                        or params.get("type") != terminal_event_type
                    ):
                        continue
                    session_id = params.get("session_id")
                    if not session_id:
                        continue
                    for device_token, token_kind in registry.watchers_for(
                        session_id
                    ):
                        try:
                            if token_kind == "liveActivity":
                                await end_live_activity(
                                    device_token, status="completed"
                                )
                            else:
                                await send_completion_push(
                                    device_token,
                                    session_id=session_id,
                                )
                        except Exception:
                            logger.exception(
                                "push delivery failed for session=%s "
                                "device=%s",
                                session_id,
                                device_token[:8],
                            )
        except Exception:
            logger.exception(
                "native watch connection dropped, reconnecting in %.1fs",
                backoff,
            )
            await asyncio.sleep(backoff)
            backoff = min(backoff * 2, 30.0)
