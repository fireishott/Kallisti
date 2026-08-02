"""JSON-RPC 2.0 WebSocket endpoint for iOS Herald.

Provides a WebSocket endpoint at /api/ws that speaks the same JSON-RPC 2.0
protocol as the Hermes Desktop gateway. iOS connects here instead of using
the REST/SSE path.

This is a protocol adapter — it translates between the iOS JSON-RPC frames
and the connector's internal services. It does NOT create a second chat
protocol; it reuses the same session/message/job infrastructure.

Protocol reference: apps/shared/src/json-rpc-gateway.ts in hermes-agent.
Bridge reference: relay/app/json_rpc_bridge.py
"""

from __future__ import annotations

import asyncio
import itertools
import json
import logging
from typing import Any, Optional

from starlette.websockets import WebSocket, WebSocketDisconnect

logger = logging.getLogger("herald.json_rpc_ws")

# ── JSON-RPC 2.0 error codes ──────────────────────────────────────
PARSE_ERROR = -32700
INVALID_REQUEST = -32600
METHOD_NOT_FOUND = -32601
INVALID_PARAMS = -32602
INTERNAL_ERROR = -32603


def _rpc_error(msg_id: Any, code: int, message: str) -> dict:
    return {"jsonrpc": "2.0", "id": msg_id, "error": {"code": code, "message": message}}


def _rpc_result(msg_id: Any, result: Any) -> dict:
    return {"jsonrpc": "2.0", "id": msg_id, "result": result}


def _push_event(event_type: str, payload: Any = None, *, session_id: str = None, profile: str = None) -> dict:
    params: dict = {"type": event_type, "payload": payload or {}}
    if session_id:
        params["session_id"] = session_id
    if profile:
        params["profile"] = profile
    return {"jsonrpc": "2.0", "method": "event", "params": params}


class JsonRpcWsHandler:
    """Handles a single WebSocket connection speaking JSON-RPC 2.0.

    Instantiated per-connection. Routes methods to the connector's
    internal services.
    """

    def __init__(
        self,
        *,
        services=None,
        event_fanout=None,
        get_connector_session=None,
        send_connector_rpc=None,
        gateway_controller=None,
        telemetry_service=None,
    ):
        self._services = services
        self._event_fanout = event_fanout
        self._get_connector_session = get_connector_session
        self._send_connector_rpc = send_connector_rpc
        self._gateway = gateway_controller
        self._telemetry = telemetry_service
        self._stream_tasks: list[asyncio.Task] = []
        self._ids = itertools.count(1)

    async def handle(self, ws: WebSocket) -> None:
        """Main handler for a WebSocket connection."""
        await ws.accept()
        logger.info("JSON-RPC WebSocket client connected")

        try:
            while True:
                try:
                    raw = await ws.receive_json()
                except WebSocketDisconnect:
                    break
                except Exception as exc:
                    logger.error("Failed to receive message: %s", exc)
                    break

                response = await self._route(raw)
                if response is not None:
                    try:
                        await ws.send_json(response)
                    except WebSocketDisconnect:
                        break
                    except Exception as exc:
                        logger.error("Failed to send response: %s", exc)
                        break
        finally:
            await self._cleanup()
            logger.info("JSON-RPC WebSocket client disconnected")

    async def _route(self, raw: dict) -> Optional[dict]:
        """Route a JSON-RPC message to the correct handler."""
        method = raw.get("method")
        msg_id = raw.get("id")
        params = raw.get("params", {})

        # Push events from client (e.g., ping)
        if method == "event":
            event_type = params.get("type", "")
            if event_type == "ping":
                return _push_event("pong")
            return None

        # Notification (no id) — no response expected
        if msg_id is None:
            logger.debug("Notification: %s", method)
            return None

        handler = self._method_map().get(method)
        if handler is None:
            return _rpc_error(msg_id, METHOD_NOT_FOUND, f"Method not found: {method}")

        try:
            result = await handler(params)
            return _rpc_result(msg_id, result)
        except Exception as exc:
            logger.exception("Handler error: %s", method)
            return _rpc_error(msg_id, INTERNAL_ERROR, str(exc))

    def _method_map(self) -> dict:
        return {
            # Chat
            "prompt.submit": self._prompt_submit,
            "prompt.cancel": self._prompt_cancel,
            # Gateway control
            "gateway.restart": self._gateway_restart,
            "gateway.status": self._gateway_status,
            "gateway.update": self._gateway_update,
            "gateway.update_check": self._gateway_update_check,
            "gateway.logs": self._gateway_logs,
            "gateway.model_switch": self._gateway_model_switch,
            "gateway.config_reload": self._gateway_config_reload,
            # Sessions
            "session.list": self._session_list,
            "session.get": self._session_get,
            "session.create": self._session_create,
            "session.delete": self._session_delete,
            "session.archive": self._session_archive,
            "session.rename": self._session_rename,
            "session.messages": self._session_messages,
            "session.search": self._session_search,
            # Model & profile
            "model.list": self._model_list,
            "model.info": self._model_info,
            "model.set": self._model_set,
            "profile.list": self._profile_list,
            # Config & env
            "config.get": self._config_get,
            "env.list": self._env_list,
            # Skills
            "skills.list": self._skills_list,
        }

    # ── Chat ───────────────────────────────────────────────────────

    async def _prompt_submit(self, params: dict) -> dict:
        if self._services is None:
            raise RuntimeError("Service layer not available")

        session_id = params.get("session_id", "")
        message_text = params.get("message", "")
        profile = params.get("profile")

        if not session_id or not message_text:
            raise ValueError("session_id and message are required")

        # Delegate to the connector's existing message pipeline
        # This reuses the same infrastructure as the REST /v1/messages endpoint
        result = await self._services.submit_message(
            session_id=session_id,
            message=message_text,
            profile=profile,
        )
        return result

    async def _prompt_cancel(self, params: dict) -> dict:
        session_id = params.get("session_id", "")
        if not session_id:
            raise ValueError("session_id is required")

        if self._services is None:
            raise RuntimeError("Service layer not available")

        result = await self._services.cancel_active_job(session_id)
        return result

    # ── Gateway control ────────────────────────────────────────────

    async def _gateway_restart(self, params: dict) -> dict:
        if self._gateway is None:
            return {"restarting": False, "error": "Gateway controller not available"}
        target = params.get("target", "relay")
        return await self._gateway.restart(target=target)

    async def _gateway_status(self, params: dict) -> dict:
        if self._telemetry is None:
            return {"error": "Telemetry not available"}
        snapshot = self._telemetry.snapshot()
        if snapshot is None:
            return {"message": "Telemetry not yet collected"}
        from dataclasses import asdict
        return asdict(snapshot)

    async def _gateway_update(self, params: dict) -> dict:
        if self._gateway is None:
            return {"updating": False, "error": "Gateway controller not available"}
        return await self._gateway.update()

    async def _gateway_update_check(self, params: dict) -> dict:
        if self._gateway is None:
            return {"available": False, "error": "Gateway controller not available"}
        return await self._gateway.update_check()

    async def _gateway_logs(self, params: dict) -> dict:
        return {"lines": [], "count": 0}

    async def _gateway_model_switch(self, params: dict) -> dict:
        if self._gateway is None:
            return {"switched": False, "error": "Gateway controller not available"}
        model = params.get("model", "")
        if not model:
            raise ValueError("model is required")
        return await self._gateway.model_switch(model=model)

    async def _gateway_config_reload(self, params: dict) -> dict:
        if self._gateway is None:
            return {"reloaded": False, "error": "Gateway controller not available"}
        return await self._gateway.config_reload()

    # ── Sessions ───────────────────────────────────────────────────

    async def _session_list(self, params: dict) -> dict:
        if self._services is None:
            return {"sessions": []}
        profile = params.get("profile")
        sessions = await self._services.list_sessions(profile=profile)
        return {"sessions": sessions}

    async def _session_get(self, params: dict) -> dict:
        session_id = params.get("session_id", "")
        if not session_id:
            raise ValueError("session_id is required")
        if self._services is None:
            return {"error": "Service layer not available"}
        return await self._services.get_session(session_id)

    async def _session_create(self, params: dict) -> dict:
        if self._services is None:
            raise RuntimeError("Service layer not available")
        title = params.get("title", "New Chat")
        profile = params.get("profile")
        return await self._services.create_session(title=title, profile=profile)

    async def _session_delete(self, params: dict) -> dict:
        session_id = params.get("session_id", "")
        if not session_id:
            raise ValueError("session_id is required")
        if self._services is None:
            raise RuntimeError("Service layer not available")
        return await self._services.delete_session(session_id)

    async def _session_archive(self, params: dict) -> dict:
        session_id = params.get("session_id", "")
        if not session_id:
            raise ValueError("session_id is required")
        if self._services is None:
            raise RuntimeError("Service layer not available")
        return await self._services.archive_session(session_id)

    async def _session_rename(self, params: dict) -> dict:
        session_id = params.get("session_id", "")
        title = params.get("title", "")
        if not session_id:
            raise ValueError("session_id is required")
        if self._services is None:
            raise RuntimeError("Service layer not available")
        return await self._services.rename_session(session_id, title)

    async def _session_messages(self, params: dict) -> dict:
        session_id = params.get("session_id", "")
        if not session_id:
            raise ValueError("session_id is required")
        if self._services is None:
            return {"messages": []}
        return await self._services.get_session_messages(session_id)

    async def _session_search(self, params: dict) -> dict:
        query = params.get("query", "")
        if self._services is None:
            return {"results": []}
        return await self._services.search_sessions(query)

    # ── Model & profile ────────────────────────────────────────────

    async def _model_list(self, params: dict) -> dict:
        if not self._send_connector_rpc:
            return {"models": [], "activeModel": None}
        try:
            session = self._get_connector_session and self._get_connector_session()
            if session is None:
                return {"models": [], "activeModel": None}
            result = await self._send_connector_rpc(
                session.user_id,
                method="models.list",
                timeout_seconds=5.0,
            )
            return result
        except Exception:
            return {"models": [], "activeModel": None}

    async def _model_info(self, params: dict) -> dict:
        return await self._model_list(params)

    async def _model_set(self, params: dict) -> dict:
        model = params.get("model", "")
        if not model:
            raise ValueError("model is required")
        if self._gateway:
            return await self._gateway.model_switch(model=model)
        if not self._send_connector_rpc:
            return {"switched": False, "error": "No connector RPC available"}
        session = self._get_connector_session and self._get_connector_session()
        if session is None:
            return {"switched": False, "error": "No connector session"}
        try:
            result = await self._send_connector_rpc(
                session.user_id,
                method="model.set",
                params={"model": model},
                timeout_seconds=10.0,
            )
            return {"switched": True, "model": model, "result": result}
        except Exception as exc:
            return {"switched": False, "error": str(exc)}

    async def _profile_list(self, params: dict) -> dict:
        if not self._send_connector_rpc:
            return {"profiles": []}
        try:
            session = self._get_connector_session and self._get_connector_session()
            if session is None:
                return {"profiles": []}
            result = await self._send_connector_rpc(
                session.user_id,
                method="profiles.list",
                timeout_seconds=5.0,
            )
            return result
        except Exception:
            return {"profiles": []}

    # ── Config & env ───────────────────────────────────────────────

    async def _config_get(self, params: dict) -> dict:
        if not self._send_connector_rpc:
            return {"config": {}}
        try:
            session = self._get_connector_session and self._get_connector_session()
            if session is None:
                return {"config": {}}
            result = await self._send_connector_rpc(
                session.user_id,
                method="config.get",
                timeout_seconds=5.0,
            )
            return result
        except Exception:
            return {"config": {}}

    async def _env_list(self, params: dict) -> dict:
        if not self._send_connector_rpc:
            return {"env": {}}
        try:
            session = self._get_connector_session and self._get_connector_session()
            if session is None:
                return {"env": {}}
            result = await self._send_connector_rpc(
                session.user_id,
                method="env.list",
                timeout_seconds=5.0,
            )
            return result
        except Exception:
            return {"env": {}}

    # ── Skills ─────────────────────────────────────────────────────

    async def _skills_list(self, params: dict) -> dict:
        if not self._send_connector_rpc:
            return {"skills": []}
        try:
            session = self._get_connector_session and self._get_connector_session()
            if session is None:
                return {"skills": []}
            result = await self._send_connector_rpc(
                session.user_id,
                method="skills.list",
                params={"profile": params.get("profile")} if params.get("profile") else None,
                timeout_seconds=5.0,
            )
            return result
        except Exception:
            return {"skills": []}

    # ── Cleanup ────────────────────────────────────────────────────

    async def _cleanup(self) -> None:
        for task in self._stream_tasks:
            if not task.done():
                task.cancel()
                try:
                    await task
                except asyncio.CancelledError:
                    pass
        self._stream_tasks.clear()
