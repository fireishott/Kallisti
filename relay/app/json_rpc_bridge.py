"""
JSON-RPC 2.0 Bridge for Hermes Desktop integration.

Translates between the Hermes Desktop JSON-RPC 2.0 protocol (WebSocket
at /api/ws) and Herald's internal REST + job/event model.

Protocol reference: apps/shared/src/json-rpc-gateway.ts in hermes-agent.

No Hermes code is modified — this is a protocol adapter in Herald.
"""

from __future__ import annotations

import asyncio
import logging
import uuid
from datetime import datetime, timezone
from typing import Optional

from fastapi import WebSocket
from sqlalchemy.orm import Session

logger = logging.getLogger("herald.relay.json_rpc")

# ── JSON-RPC 2.0 error codes ──────────────────────────────────────────
PARSE_ERROR = -32700
INVALID_REQUEST = -32600
METHOD_NOT_FOUND = -32601
INVALID_PARAMS = -32602
INTERNAL_ERROR = -32603


def _utcnow() -> datetime:
    return datetime.now(timezone.utc)


def _rpc_error(msg_id, code: int, message: str) -> dict:
    """Build a JSON-RPC 2.0 error response."""
    return {
        "jsonrpc": "2.0",
        "id": msg_id,
        "error": {"code": code, "message": message},
    }


def _rpc_result(msg_id, result) -> dict:
    """Build a JSON-RPC 2.0 success response."""
    return {"jsonrpc": "2.0", "id": msg_id, "result": result}


def _push_event(event_type: str, payload=None, *, session_id=None, profile=None) -> dict:
    """Build a JSON-RPC 2.0 push event (no id)."""
    params: dict = {"type": event_type, "payload": payload or {}}
    if session_id:
        params["session_id"] = session_id
    if profile:
        params["profile"] = profile
    return {"jsonrpc": "2.0", "method": "event", "params": params}


class JsonRpcBridge:
    """Protocol adapter between Hermes Desktop JSON-RPC 2.0 and Herald.

    Instantiated per-WebSocket connection at /api/ws. Method handlers
    are looked up by name and may be sync or async.
    """

    def __init__(
        self,
        settings,
        db_factory,
        *,
        services=None,
        event_fanout=None,
        get_connector_session=None,
        send_connector_rpc=None,
        telemetry_service=None,
        gateway_controller=None,
    ):
        self.settings = settings
        self.db_factory = db_factory
        self._services = services
        self._event_fanout = event_fanout
        self._get_connector_session = get_connector_session
        self._send_connector_rpc = send_connector_rpc
        self._telemetry = telemetry_service
        self._gateway = gateway_controller

        # Track active stream subscriptions for cleanup on disconnect
        self._stream_tasks: list[asyncio.Task] = []

    # ── Message routing ────────────────────────────────────────────────

    async def handle_message(self, ws: WebSocket, raw: dict) -> Optional[dict]:
        """Route a JSON-RPC 2.0 message to the correct handler.

        Returns a response dict if the message had an id (request),
        or None for push events.
        """
        method = raw.get("method")
        msg_id = raw.get("id")
        params = raw.get("params", {})

        # Push events from the desktop — these are sent server→client,
        # but the desktop may also send them for some interactive flows.
        if method == "event":
            await self._handle_push_event(ws, params)
            return None

        if msg_id is None:
            # Notification (no id) — no response expected
            if method:
                await self._handle_notification(ws, method, params)
            return None

        handler = self._method_map().get(method)
        if handler is None:
            return _rpc_error(msg_id, METHOD_NOT_FOUND, f"Method not found: {method}")

        try:
            result = await handler(ws, params)
            return _rpc_result(msg_id, result)
        except Exception as exc:
            logger.exception("JSON-RPC handler error: %s", method)
            return _rpc_error(msg_id, INTERNAL_ERROR, str(exc))

    def _method_map(self) -> dict:
        """Build the method dispatch table."""
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

    # ── Chat ───────────────────────────────────────────────────────────

    async def _prompt_submit(self, ws, params: dict) -> dict:
        """Map prompt.submit → job execution → event streaming."""
        services = self._services
        if services is None:
            raise RuntimeError("Service layer not available")

        session_id = params.get("session_id", "")
        message_text = params.get("message", "")
        profile = params.get("profile")

        if not session_id or not message_text:
            raise ValueError("session_id and message are required")

        db = self.db_factory()
        try:
            # Ensure the session exists
            from .services import get_session, create_session

            session = get_session(db, session_id=session_id)
            if session is None:
                # Auto-create if it doesn't exist
                session = create_session(
                    db,
                    user_id="",  # Will be resolved from auth context
                    title=message_text[:80] if message_text else "New Chat",
                )
                db.commit()
                session_id = session.id

            # Get or create conversation
            from .services import get_or_create_current_conversation

            conversation = get_or_create_current_conversation(
                db, user_id=session.user_id
            )
            db.commit()

            # Create user message
            from .services import append_message

            user_msg = append_message(
                db,
                conversation_id=conversation.id,
                role="user",
                content=message_text,
            )
            db.commit()

            # Create job
            from .services import create_message_job

            job = create_message_job(
                db,
                user_id=session.user_id,
                conversation_id=conversation.id,
                user_message_id=user_msg.id,
                session_id_snapshot=session_id,
            )
            db.commit()

            job_id = job.id
        finally:
            db.close()

        # Start streaming job events back to this WebSocket
        task = asyncio.create_task(
            self._stream_job_events(ws, job_id, session_id, profile),
            name=f"jsonrpc-stream-{job_id}",
        )
        self._stream_tasks.append(task)

        return {"job_id": job_id, "status": "accepted"}

    async def _prompt_cancel(self, ws, params: dict) -> dict:
        """Cancel a running job."""
        session_id = params.get("session_id", "")
        if not session_id:
            raise ValueError("session_id is required")

        # Find the active job for this session and cancel it
        db = self.db_factory()
        try:
            from .models import MessageJob

            job = (
                db.query(MessageJob)
                .filter(
                    MessageJob.session_id_snapshot == session_id,
                    MessageJob.status == "processing",
                )
                .order_by(MessageJob.created_at.desc())
                .first()
            )
            if job is None:
                return {"cancelled": False, "error": "No active job found"}

            from .services import fail_message_job

            fail_message_job(db, job_id=job.id, error_text="Cancelled by user")
            db.commit()

            # Publish cancel event
            if self._event_fanout:
                try:
                    loop = asyncio.get_running_loop()
                    # Wake subscribers
                    from .main import publish_job_event
                except Exception:
                    pass

            return {"cancelled": True, "job_id": job.id}
        finally:
            db.close()

    # ── Gateway control ───────────────────────────────────────────────

    async def _gateway_restart(self, ws, params: dict) -> dict:
        if self._gateway is None:
            return {"restarting": False, "error": "Gateway controller not available"}
        target = params.get("target", "relay")
        return await self._gateway.restart(target=target)

    async def _gateway_status(self, ws, params: dict) -> dict:
        if self._telemetry is None:
            return {"error": "Telemetry not available"}
        snapshot = self._telemetry.snapshot()
        if snapshot is None:
            return {"message": "Telemetry not yet collected"}
        from dataclasses import asdict
        return asdict(snapshot)

    async def _gateway_update(self, ws, params: dict) -> dict:
        if self._gateway is None:
            return {"updating": False, "error": "Gateway controller not available"}
        return await self._gateway.update()

    async def _gateway_update_check(self, ws, params: dict) -> dict:
        if self._gateway is None:
            return {"available": False, "error": "Gateway controller not available"}
        return await self._gateway.update_check()

    async def _gateway_logs(self, ws, params: dict) -> dict:
        # This is handled via the REST endpoint in practice
        return {"lines": [], "count": 0}

    async def _gateway_model_switch(self, ws, params: dict) -> dict:
        if self._gateway is None:
            return {"switched": False, "error": "Gateway controller not available"}
        model = params.get("model", "")
        if not model:
            raise ValueError("model is required")
        return await self._gateway.model_switch(model=model)

    async def _gateway_config_reload(self, ws, params: dict) -> dict:
        if self._gateway is None:
            return {"reloaded": False, "error": "Gateway controller not available"}
        return await self._gateway.config_reload()

    # ── Sessions ──────────────────────────────────────────────────────

    async def _session_list(self, ws, params: dict) -> dict:
        db = self.db_factory()
        try:
            from .services import list_sessions

            profile = params.get("profile")
            sessions = list_sessions(db, profile=profile)
            return {
                "sessions": [
                    {
                        "id": s["id"],
                        "title": s.get("title", ""),
                        "created_at": str(s.get("created_at", "")),
                        "updated_at": str(s.get("updated_at", "")),
                        "message_count": s.get("message_count", 0),
                        "archived": s.get("archived", False),
                        "pinned": s.get("pinned", False),
                    }
                    for s in sessions
                ]
            }
        finally:
            db.close()

    async def _session_get(self, ws, params: dict) -> dict:
        session_id = params.get("session_id", "")
        if not session_id:
            raise ValueError("session_id is required")
        db = self.db_factory()
        try:
            from .services import get_session

            session = get_session(db, session_id=session_id)
            if session is None:
                return {"error": "Session not found"}
            return {
                "id": session.id,
                "title": session.title,
                "created_at": str(session.created_at),
                "updated_at": str(session.updated_at),
            }
        finally:
            db.close()

    async def _session_create(self, ws, params: dict) -> dict:
        db = self.db_factory()
        try:
            from .services import create_session

            title = params.get("title", "New Chat")
            profile = params.get("profile")
            session = create_session(db, user_id="", title=title)
            db.commit()
            return {
                "id": session.id,
                "title": session.title,
                "created_at": str(session.created_at),
            }
        finally:
            db.close()

    async def _session_delete(self, ws, params: dict) -> dict:
        session_id = params.get("session_id", "")
        if not session_id:
            raise ValueError("session_id is required")
        db = self.db_factory()
        try:
            from .services import delete_session

            delete_session(db, session_id=session_id)
            db.commit()
            return {"deleted": True}
        finally:
            db.close()

    async def _session_archive(self, ws, params: dict) -> dict:
        session_id = params.get("session_id", "")
        if not session_id:
            raise ValueError("session_id is required")
        db = self.db_factory()
        try:
            from .services import archive_session

            archive_session(db, session_id=session_id)
            db.commit()
            return {"archived": True}
        finally:
            db.close()

    async def _session_rename(self, ws, params: dict) -> dict:
        session_id = params.get("session_id", "")
        title = params.get("title", "")
        if not session_id:
            raise ValueError("session_id is required")
        db = self.db_factory()
        try:
            from .services import rename_session

            session = rename_session(db, session_id=session_id, title=title)
            db.commit()
            return {
                "id": session.id,
                "title": session.title,
            }
        finally:
            db.close()

    async def _session_messages(self, ws, params: dict) -> dict:
        session_id = params.get("session_id", "")
        if not session_id:
            raise ValueError("session_id is required")
        db = self.db_factory()
        try:
            from .services import get_session, list_conversation_messages

            session = get_session(db, session_id=session_id)
            if session is None:
                return {"messages": []}
            # Get the conversation for this session
            from .models import Conversation

            conversation = (
                db.query(Conversation)
                .filter(Conversation.id == session_id)
                .first()
            )
            if conversation is None:
                return {"messages": []}
            messages = list_conversation_messages(
                db, conversation_id=conversation.id
            )
            return {
                "messages": [
                    {
                        "id": m["id"],
                        "role": m.get("role", ""),
                        "content": m.get("content", ""),
                        "created_at": str(m.get("created_at", "")),
                    }
                    for m in messages
                ]
            }
        finally:
            db.close()

    async def _session_search(self, ws, params: dict) -> dict:
        query = params.get("query", "")
        db = self.db_factory()
        try:
            from .services import search_sessions

            results = search_sessions(db, query=query)
            return {
                "results": [
                    {
                        "id": r["id"],
                        "title": r.get("title", ""),
                        "snippet": r.get("snippet", ""),
                    }
                    for r in results
                ]
            }
        finally:
            db.close()

    # ── Model & profile ───────────────────────────────────────────────

    async def _model_list(self, ws, params: dict) -> dict:
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

    async def _model_info(self, ws, params: dict) -> dict:
        return await self._model_list(ws, params)

    async def _model_set(self, ws, params: dict) -> dict:
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

    async def _profile_list(self, ws, params: dict) -> dict:
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

    # ── Config & env ──────────────────────────────────────────────────

    async def _config_get(self, ws, params: dict) -> dict:
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

    async def _env_list(self, ws, params: dict) -> dict:
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

    async def _skills_list(self, ws, params: dict) -> dict:
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

    # ── Push events / notifications ───────────────────────────────────

    async def _handle_push_event(self, ws, params: dict) -> None:
        """Handle a push event sent from the desktop."""
        event_type = params.get("type", "")
        if event_type == "ping":
            await ws.send_json(_push_event("pong"))
        # Other push events from desktop are informational; log and ignore
        logger.debug("Received desktop push event: %s", event_type)

    async def _handle_notification(self, ws, method: str, params: dict) -> None:
        """Handle a JSON-RPC notification (no id, no response)."""
        # Currently, notifications are informational only
        logger.debug("Received JSON-RPC notification: %s", method)

    # ── Job event streaming ───────────────────────────────────────────

    async def _stream_job_events(
        self,
        ws: WebSocket,
        job_id: str,
        session_id: str,
        profile: Optional[str] = None,
    ) -> None:
        """Stream job events to the WebSocket as JSON-RPC push events.

        Uses the same DB-backed pattern as the existing SSE endpoint:
        1. Subscribe to EventFanout (wake signal)
        2. Poll DB for new events on each wake signal
        3. Translate internal event types → JSON-RPC push events
        """
        if self._event_fanout is None:
            await ws.send_json(
                _push_event(
                    "error",
                    {"code": -32000, "message": "Event fanout not available"},
                    session_id=session_id,
                    profile=profile,
                )
            )
            return

        db = self.db_factory()
        try:
            from .services import get_job_events_after

            # Subscribe to wake signals
            wake_queue = await self._event_fanout.subscribe(job_id)
            last_seq = -1

            try:
                while True:
                    # Poll DB for new events
                    events = get_job_events_after(db, job_id=job_id, after_seq=last_seq)
                    for evt in events:
                        seq = evt.get("seq", 0)
                        if seq > last_seq:
                            last_seq = seq
                        rpc_event = self._translate_event(
                            evt, session_id, profile
                        )
                        if rpc_event:
                            await ws.send_json(rpc_event)

                    # Check if job is terminal
                    from .models import MessageJob

                    job = db.query(MessageJob).filter(MessageJob.id == job_id).first()
                    if job and job.status in ("completed", "failed", "cancelled"):
                        # Send terminal event
                        terminal_type = (
                            "message.complete"
                            if job.status == "completed"
                            else "error"
                        )
                        payload = {}
                        if job.status == "completed":
                            payload = {
                                "session_id": session_id,
                                "usage": job.usage_data or {},
                            }
                        else:
                            payload = {
                                "code": -1,
                                "message": job.error_text or "Job failed",
                            }
                        await ws.send_json(
                            _push_event(
                                terminal_type,
                                payload,
                                session_id=session_id,
                                profile=profile,
                            )
                        )
                        break

                    # Wait for wake signal (timeout for keepalive)
                    try:
                        await asyncio.wait_for(wake_queue.get(), timeout=30.0)
                    except asyncio.TimeoutError:
                        # Send keepalive — the desktop doesn't need it
                        # for JSON-RPC but it helps debug
                        pass
            finally:
                await self._event_fanout.unsubscribe(job_id, wake_queue)
        except Exception:
            logger.exception("Job event stream error for %s", job_id)
            try:
                await ws.send_json(
                    _push_event(
                        "error",
                        {"code": -32000, "message": "Stream error"},
                        session_id=session_id,
                        profile=profile,
                    )
                )
            except Exception:
                pass
        finally:
            db.close()

    def _translate_event(
        self, event: dict, session_id: str, profile: Optional[str]
    ) -> Optional[dict]:
        """Translate an internal job event to a JSON-RPC push event."""
        event_type = event.get("type", "")
        payload = event.get("payload_json") or event.get("payload") or {}

        type_map = {
            "job.started": "message.start",
            "job.progress": "message.delta",
            "tool.started": "tool.start",
            "tool.progress": "tool.progress",
            "tool.completed": "tool.complete",
        }

        rpc_type = type_map.get(event_type)
        if rpc_type is None:
            return None

        return _push_event(
            rpc_type,
            payload,
            session_id=session_id,
            profile=profile,
        )

    # ── Cleanup ────────────────────────────────────────────────────────

    async def cleanup(self) -> None:
        """Cancel all active stream tasks."""
        for task in self._stream_tasks:
            if not task.done():
                task.cancel()
                try:
                    await task
                except asyncio.CancelledError:
                    pass
        self._stream_tasks.clear()
