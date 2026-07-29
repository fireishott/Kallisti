"""HTTP/SSE facade for the iOS Herald app.

Runs inside the connector process using Starlette (no FastAPI dependency).
Serves the same API as the Docker relay. Starlette + uvicorn + sse-starlette
are already installed in the connector's Python environment.

The iOS app talks HTTP/SSE to this server; the gateway talks native relay
WebSocket to HeraldRelayServer on :8765.  This module is the HTTP half.
"""

from __future__ import annotations

import asyncio
import contextlib
import datetime
import inspect
import json
import logging
import os
import time
import uuid
from typing import Any, AsyncIterator, Callable, Coroutine

import httpx
from starlette.applications import Starlette
from starlette.exceptions import HTTPException
from starlette.requests import Request
from starlette.responses import JSONResponse, StreamingResponse
from starlette.routing import Route

logger = logging.getLogger("herald.http_facade")

# ── Process lifetime ──────────────────────────────────────────────────────

_PROCESS_STARTED_AT = time.monotonic()

# ── Journal constants (F-3: Gateway Logs) ─────────────────────────────────

_JOURNAL_PRIORITY = {"error": "3", "warning": "4", "info": "6", "debug": "7"}
_JOURNAL_LEVEL_NAME = {0: "error", 1: "error", 2: "error", 3: "error",
                       4: "warning", 5: "info", 6: "info", 7: "debug"}
_APPLE_EPOCH_OFFSET = 978_307_200.0
_JOURNAL_UNIT = os.getenv("HERALD_JOURNAL_UNIT", "hermes-mobile-connector.service")

# ── Auth helpers ────────────────────────────────────────────────────────


class AccessTokenValidator:
    """Validates Bearer tokens from the iOS app."""

    def __init__(self, valid_tokens: set[str] | None = None) -> None:
        self._tokens: set[str] = valid_tokens or set()

    def add_token(self, token: str) -> None:
        self._tokens.add(token)

    def is_valid(self, token: str) -> bool:
        return token in self._tokens


_default_validator = AccessTokenValidator()


def set_token_validator(validator: AccessTokenValidator) -> None:
    global _default_validator
    _default_validator = validator


async def _extract_token(request: Request) -> str:
    """Extract Bearer token from Authorization header."""
    auth = request.headers.get("Authorization", "")
    if auth.startswith("Bearer "):
        return auth[7:]
    return ""


async def require_auth(request: Request) -> str:
    """Validate the Bearer token. Raises 401 if invalid."""
    token = await _extract_token(request)
    if not token or not _default_validator.is_valid(token):
        raise HTTPException(status_code=401, detail="Invalid or missing access token")
    return token


# ── Model / Profile providers ─────────────────────────────────────────

# The connector's RPC methods take a params dict (client.py:2114, :2196) — the
# same shape the JSON-RPC bridge passes (client.py:1870, :1874).  Passing
# positional args here raised
#   TypeError: _rpc_model_set() takes 2 positional arguments but 3 were given
# which surfaced in the app as a bogus [NOT_FOUND] from the gw fallback.

ModelCatalogProvider = Callable[[], Coroutine[Any, Any, dict]]
ModelSwitchProvider = Callable[[dict], Coroutine[Any, Any, dict]]
ProfileCatalogProvider = Callable[[], Coroutine[Any, Any, dict]]
ProfileSwitchProvider = Callable[[dict], Coroutine[Any, Any, dict]]
MessageHandler = Callable[
    [str, list[dict], str | None, list[dict] | None, str | None],
    Coroutine[Any, Any, AsyncIterator[dict]],
]
JobStatusProvider = Callable[[str], Coroutine[Any, Any, dict]]
JobCancelProvider = Callable[[dict], Coroutine[Any, Any, dict]]
JobEventsProvider = Callable[[str], Coroutine[Any, Any, AsyncIterator[dict]]]
SessionConversationProvider = Callable[[str], Coroutine[Any, Any, dict]]
CurrentConversationProvider = Callable[[], Coroutine[Any, Any, dict]]
ClearConversationProvider = Callable[[], Coroutine[Any, Any, dict]]


class FacadeContext:
    """Mutable context wired by the connector at startup."""

    def __init__(self) -> None:
        self.model_catalog: ModelCatalogProvider | None = None
        self.model_switch: ModelSwitchProvider | None = None
        self.profile_catalog: ProfileCatalogProvider | None = None
        self.profile_switch: ProfileSwitchProvider | None = None
        self.message_handler: MessageHandler | None = None
        self.connector_version: str = "0.0.0"
        self.health_check: Callable[[], Coroutine[Any, Any, bool]] | None = None
        self.paired_device_id: str | None = None
        self.paired_user_id: str | None = None
        self.connector_credential: str | None = None
        self.public_base_url: str = ""
        self.gateway_restart: Callable[[str], Coroutine[Any, Any, dict]] | None = None
        # P0-4: chat critical-path providers
        self.job_status: JobStatusProvider | None = None
        self.job_cancel: JobCancelProvider | None = None
        self.job_events: JobEventsProvider | None = None
        self.auxiliary_list: Callable[[], dict | Coroutine[Any, Any, dict]] | None = None
        self.auxiliary_set: Callable[[dict], dict | Coroutine[Any, Any, dict]] | None = None
        self.session_conversation: SessionConversationProvider | None = None
        self.current_conversation: CurrentConversationProvider | None = None
        self.clear_conversation: ClearConversationProvider | None = None
        self.agent_version: Callable[[], str | None] | None = None


_context = FacadeContext()


def get_context() -> FacadeContext:
    return _context


# ── Facade-local HTTP message jobs ───────────────────────────────────────
#
# The iOS app POSTs /v1/messages and decodes JSON (LiveHeraldClient.swift:223-229
# → MessageResponse).  It NEVER reads an SSE body from this route: returning a
# StreamingResponse here is what produced "The data couldn't be read because it
# isn't in the correct format" (DecodingError.dataCorrupted) on every single send.
#
# Contract: answer immediately with replyState="pending" + jobId, drain the
# connector's async generator in a background task, and serve the result on
# GET /v1/jobs/{id} (polling) and GET /v1/jobs/{id}/events (SSE).  Both are
# already implemented on the app side and need no change.
#
# These jobs are facade-owned and live only in this process.  Jobs created by the
# legacy relay WS path still resolve through ctx.job_* — see the fallback branch
# in job_status()/job_events()/cancel_job().  Do not remove that fallback.

_http_jobs: dict[str, dict] = {}
_http_job_tasks: dict[str, asyncio.Task] = {}
_HTTP_JOB_TTL_SECONDS = 900.0
_conversation_id_singleton: str | None = None


async def _auto_title(handler, text: str, hermes_sid: str, app_uuid: str) -> str | None:
    """Generate a title for a session from its first user message.

    B38 P1-1: called server-side on the first completed turn so the session
    list never shows "New Chat" for a session that has messages.

    Tries the message_handler with a short title prompt first; falls back
    to a truncation of the first user message.
    """
    title_prompt = (
        "Generate a short title (3-8 words) for a conversation that "
        "begins with this message. Return ONLY the title, no quotes, "
        "no punctuation at the end:\n\n" + text[:500]
    )
    # B39 T2: use a throwaway session so the title prompt is never
    # submitted as a real turn in the user's conversation.  The
    # title- prefix ensures these sessions are never surfaced in
    # session_list (which filters to source='api_server').
    title_session_id = f"title-{uuid.uuid4()}"
    try:
        async with asyncio.timeout(15):
            accumulated = ""
            async for event in handler(title_prompt, [], title_session_id, None, None):
                etype = event.get("type", "")
                data = event.get("data", {}) or {}
                if etype == "text_delta":
                    accumulated += data.get("delta", "")
                if etype == "done":
                    accumulated = data.get("text") or accumulated
                    break
            if accumulated:
                title = accumulated.strip()[:120]
                # Strip common wrapping characters
                title = title.strip('"\'.!?;:,*`~ \t\n\r')
                if len(title) >= 3:
                    return title
    except Exception:
        logger.debug("_auto_title: LLM path failed, falling back to truncation")

    # Fallback: first line, first 80 chars
    first_line = text.strip().split("\n")[0].strip()
    if first_line:
        return first_line[:80]
    return None


async def _auto_title_and_persist(handler, text: str, hermes_sid: str, app_uuid: str) -> None:
    """Fire-and-forget wrapper: generate a title and persist it."""
    try:
        title = await _auto_title(handler, text, hermes_sid, app_uuid)
        if title:
            from .session_store import set_session_meta
            set_session_meta(app_uuid, title=title)
            logger.info("_auto_title: set title %r for %s", title, app_uuid)
    except Exception:
        logger.exception("_auto_title_and_persist failed for %s", app_uuid)


def _now_iso() -> str:
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def _stable_conversation_id() -> str:
    """Cold-start fallback conversation id — used ONLY when the device has never
    sent a message and carries no conversationId.

    P0-1: the primary path is now the deterministic _app_uuid(hermes_id).  This
    function is a last-resort fallback for the first-ever message from a fresh
    device, and must not be the normal code path.
    """
    global _conversation_id_singleton
    if _conversation_id_singleton is None:
        _conversation_id_singleton = str(uuid.uuid4())
    return _conversation_id_singleton


def _coerce_uuid(value: Any) -> str | None:
    """Return a lowercase UUID string, or None. Never raise.

    RelayMessage.clientMessageId / .jobId are UUID? on the app side
    (LiveHeraldClient.swift:41,45) — a non-UUID string is a hard decode failure,
    whereas null decodes fine.
    """
    try:
        return str(uuid.UUID(str(value)))
    except (ValueError, TypeError, AttributeError):
        return None


def _relay_message(role: str, text: str, *, client_message_id: Any = None,
                   job_id: Any = None) -> dict:
    """Build one RelayMessage (LiveHeraldClient.swift:39-48).

    id / role / text / timestamp are non-optional on the app side.  `role` accepts
    "user", "herald", "system" — and "assistant"/"hermes" are aliased to .herald
    by MessageSender.init(from:) (Herald/Models/MessageSender.swift:13).
    """
    return {
        "id": str(uuid.uuid4()),
        "clientMessageId": _coerce_uuid(client_message_id),
        "role": role,
        "text": text,
        "timestamp": _now_iso(),
        "deliveryStatus": "delivered",
        "jobId": _coerce_uuid(job_id),
        "attachments": None,
    }


def _prune_http_jobs() -> None:
    now = time.time()
    stuck_timeout = 2 * int(os.getenv("HERALD_JOB_TIMEOUT_SECONDS", "170"))
    for jid, job in list(_http_jobs.items()):
        if job["status"] in {"completed", "failed", "cancelled"} and \
                now - job["updatedAt"] > _HTTP_JOB_TTL_SECONDS:
            _http_jobs.pop(jid, None)
        elif job["status"] not in {"completed", "failed", "cancelled"} and \
                now - job["updatedAt"] > stuck_timeout:
            logger.warning("Pruning stuck non-terminal job %s (status=%s, age=%ds)",
                           jid, job["status"], int(now - job["updatedAt"]))
            job["status"] = "failed"
            job["error"] = "Job timed out — no progress in over %ds." % stuck_timeout
            job["errorCategory"] = "timeout"
            job["errorAction"] = "retry"


async def _run_http_job(job_id: str, handler, text, history, session_id,
                        attachments, reasoning_effort) -> None:
    """Drain the connector's message generator into the job record."""
    job = _http_jobs[job_id]
    accumulated = ""

    def _publish(event: dict) -> None:
        job["events"].append(event)
        job["updatedAt"] = time.time()
        for queue in list(job["subscribers"]):
            queue.put_nowait(event)

    timeout_seconds = int(os.getenv("HERALD_JOB_TIMEOUT_SECONDS", "170"))
    try:
        async with asyncio.timeout(timeout_seconds):
            async for event in handler(text, history, session_id, attachments, reasoning_effort):
                etype = event.get("type", "progress")
                data = event.get("data", {}) or {}
                if etype == "text_delta":
                    accumulated += data.get("delta", "")
                if etype == "done":
                    # The connector's own terminal event (client.py:1695-1717) carries
                    # the final text and, on failure, the error + category/action.
                    accumulated = data.get("text") or accumulated
                    job["status"] = data.get("status", "completed")
                    job["error"] = data.get("error")
                    job["errorCategory"] = data.get("errorCategory")
                    job["errorAction"] = data.get("errorAction")
                    job["usage"] = data.get("usage")
                    # Record the Hermes session id so the app UUID → session-id
                    # mapping survives connector restarts.  The handler returns
                    # the real Hermes session id (e.g. "api-9af38ce…") even when
                    # the facade was called with an app-facing UUID.
                    hermes_sid = data.get("sessionId")
                    if not hermes_sid:
                        # Cold start: session_id was None and the runtime didn't
                        # return one.  Fall back to querying state.db for the
                        # session that received the user message.
                        from .session_store import _find_session_by_recent_message
                        hermes_sid = _find_session_by_recent_message(text)
                    if hermes_sid:
                        from .session_store import _app_uuid, _persist_hermes_mapping
                        # Record the canonical mapping: app_uuid → hermes_id
                        canonical_app_id = _app_uuid(hermes_sid)
                        _persist_hermes_mapping(canonical_app_id, hermes_sid)
                        # B38 P0-2: record the mapping under BOTH the app's id
                        # and the canonical id so future lookups resolve either
                        # way.  Do NOT mutate job["conversationId"] — the app
                        # already received it in the POST response, and changing
                        # it later causes replies to land in the wrong thread.
                        response_conv_id = job.get("conversationId")
                        _persist_hermes_mapping(canonical_app_id, hermes_sid)
                        if response_conv_id and response_conv_id != canonical_app_id:
                            _persist_hermes_mapping(response_conv_id, hermes_sid)

                        # B38 P1-1: auto-generate a title if the session
                        # has none.  Fire-and-forget — don't delay the
                        # job completion for title generation.
                        from .session_store import get_session_meta, set_session_meta
                        meta = get_session_meta(canonical_app_id)
                        if not meta.get("title"):
                            asyncio.create_task(
                                _auto_title_and_persist(
                                    handler, text, hermes_sid, canonical_app_id
                                )
                            )
                    continue          # re-emitted with jobId in the finally block
                _publish({"type": etype, "data": data})
    except TimeoutError:
        job["status"] = "failed"
        job["error"] = "The model did not respond in time."
        job["errorCategory"] = "timeout"
        job["errorAction"] = "retry"
    except asyncio.CancelledError:
        job["status"] = "cancelled"
        raise
    except Exception as exc:                      # noqa: BLE001 — must not kill the task
        logger.exception("HTTP message job %s failed", job_id)
        job["status"] = "failed"
        job["error"] = str(exc)
    finally:
        if job["status"] == "running":
            job["status"] = "completed"
        if job["status"] == "completed":
            job["message"] = _relay_message("herald", accumulated, job_id=job_id)
        terminal = {
            "type": "done",
            "data": {
                "jobId": job_id,
                "status": job["status"],
                "text": accumulated,
                "error": job.get("error"),
                "errorCategory": job.get("errorCategory"),
                "errorAction": job.get("errorAction"),
                "usage": job.get("usage"),
            },
        }
        _publish(terminal)
        for queue in list(job["subscribers"]):
            queue.put_nowait(None)                # sentinel: close the SSE stream
        job["updatedAt"] = time.time()


# ── Journal helpers (F-3: Gateway Logs) ──────────────────────────────────


def _journal_line(entry: dict, *, timestamp_as_number: bool) -> dict:
    """One LogLine (GatewayLogsScreen.swift:317-323).

    timestamp_as_number is load-bearing: the batch route is decoded with
    RelayCoders (ISO-8601 string) and the SSE route with a bare JSONDecoder
    (.deferredToDate → Apple-reference seconds). See the table in F-3.
    """
    micros = float(entry.get("__REALTIME_TIMESTAMP", 0) or 0)
    unix_seconds = micros / 1_000_000.0
    priority = int(entry.get("PRIORITY", 6) or 6)
    message = entry.get("MESSAGE", "")
    if isinstance(message, list):                       # journald returns bytes as int lists
        message = bytes(message).decode("utf-8", "replace")
    return {
        "timestamp": (unix_seconds - _APPLE_EPOCH_OFFSET) if timestamp_as_number
                     else datetime.datetime.fromtimestamp(
                         unix_seconds, datetime.timezone.utc).isoformat(),
        "level": _JOURNAL_LEVEL_NAME.get(priority, "info"),
        "message": message,
        "source": entry.get("SYSLOG_IDENTIFIER") or entry.get("_COMM"),
    }


# ── Route handlers ──────────────────────────────────────────────────────


async def health_endpoint(request: Request) -> JSONResponse:
    ctx = get_context()
    db_ok = True
    if ctx.health_check is not None:
        try:
            db_ok = await ctx.health_check()
        except Exception:
            db_ok = False
    return JSONResponse({"status": "ok" if db_ok else "degraded", "database": db_ok})


async def health_alias(request: Request) -> JSONResponse:
    return await health_endpoint(request)


async def version_endpoint(request: Request) -> JSONResponse:
    ctx = get_context()
    return JSONResponse({
        "version": ctx.connector_version,
        "platform": "herald",
        "connector": True,
    })


async def list_models(request: Request) -> JSONResponse:
    await require_auth(request)
    ctx = get_context()
    if ctx.model_catalog is None:
        return JSONResponse({"models": [], "activeModel": None})
    result = ctx.model_catalog()
    if inspect.isawaitable(result):
        result = await result
    return JSONResponse(result)


async def switch_model(request: Request) -> JSONResponse:
    await require_auth(request)
    ctx = get_context()
    if ctx.model_switch is None:
        raise HTTPException(status_code=503, detail="Model switching not available")
    try:
        body = await request.json()
    except Exception:
        raise HTTPException(status_code=400, detail="Request body must be JSON")
    if not isinstance(body, dict):
        raise HTTPException(status_code=400, detail="Request body must be a JSON object")
    name = body.get("name") or body.get("model", "")
    provider = body.get("provider")
    if not name:
        raise HTTPException(status_code=400, detail="name is required")
    result = ctx.model_switch({"name": name, "provider": provider})
    if inspect.isawaitable(result):
        result = await result
    return JSONResponse(result)


async def aux_list(request: Request) -> JSONResponse:
    """GET /v1/aux — per-task auxiliary model routing."""
    await require_auth(request)
    ctx = get_context()
    if ctx.auxiliary_list is None:
        raise HTTPException(status_code=503, detail="Auxiliary config not available")
    result = ctx.auxiliary_list()
    if inspect.isawaitable(result):
        result = await result
    return JSONResponse(result or {"tasks": []})


async def aux_set(request: Request) -> JSONResponse:
    """POST /v1/aux — set auxiliary.<task>.provider/model."""
    await require_auth(request)
    ctx = get_context()
    if ctx.auxiliary_set is None:
        raise HTTPException(status_code=503, detail="Auxiliary config not available")
    try:
        body = await request.json()
    except Exception:
        raise HTTPException(status_code=400, detail="Request body must be JSON")
    result = ctx.auxiliary_set(body)
    if inspect.isawaitable(result):
        result = await result
    return JSONResponse(result or {"ok": False})


async def list_profiles(request: Request) -> JSONResponse:
    await require_auth(request)
    ctx = get_context()
    if ctx.profile_catalog is None:
        return JSONResponse({"profiles": [], "activeProfile": None})
    result = ctx.profile_catalog()
    if inspect.isawaitable(result):
        result = await result
    return JSONResponse(result)


async def switch_profile(request: Request) -> JSONResponse:
    await require_auth(request)
    ctx = get_context()
    if ctx.profile_switch is None:
        raise HTTPException(status_code=503, detail="Profile switching not available")
    try:
        body = await request.json()
    except Exception:
        raise HTTPException(status_code=400, detail="Request body must be JSON")
    if not isinstance(body, dict):
        raise HTTPException(status_code=400, detail="Request body must be a JSON object")
    name = body.get("name") or body.get("profile", "")
    if not name:
        raise HTTPException(status_code=400, detail="name is required")
    result = ctx.profile_switch({"name": name})
    if inspect.isawaitable(result):
        result = await result
    return JSONResponse(result)


async def get_session(request: Request) -> JSONResponse:
    await require_auth(request)
    ctx = get_context()
    import uuid as _uuid3
    return JSONResponse({
        "user": {"id": str(_uuid3.uuid4()), "displayName": "Herald User"},
        "device": {"id": str(_uuid3.uuid4()), "registered": True},
        "session": {"connectionStatus": "connected", "isMockMode": False, "backendEndpoint": ctx.public_base_url or "", "lastSyncAt": None},
        "push": {"tokenRegistered": False},
    })


async def auth_revoke(request: Request) -> JSONResponse:
    """Revoke the current session token."""
    await require_auth(request)
    return JSONResponse({"revoked": True})


async def list_commands(request: Request) -> JSONResponse:
    await require_auth(request)
    return JSONResponse({
        "commands": [
            {"name": "new", "description": "Start a new session"},
            {"name": "model", "description": "Switch models"},
            {"name": "profile", "description": "Switch profiles"},
            {"name": "compress", "description": "Compress session context"},
            {"name": "retry", "description": "Retry last message"},
            {"name": "stop", "description": "Stop current response"},
        ]
    })


async def gateway_restart(request: Request) -> JSONResponse:
    """Restart a gateway component (hermes or connector)."""
    await require_auth(request)
    ctx = get_context()
    if ctx.gateway_restart is None:
        raise HTTPException(status_code=503, detail="Gateway control not available")
    body = await request.json()
    target = body.get("target", "hermes")
    if target not in ("hermes", "connector"):
        raise HTTPException(status_code=400, detail=f"Unknown target: {target}")
    result = await ctx.gateway_restart(target)
    return JSONResponse(result)


async def gateway_status(request: Request) -> JSONResponse:
    """Return gateway telemetry for the Settings → Gateway Status screen.

    The iOS decoder (GatewayStatusScreen.swift:319-336) expects camelCase keys
    with no keyDecodingStrategy. Every field is optional — partial data renders
    gracefully; a 500 does not.

    Returns {"data": {...}} — GatewayStatusScreen.swift:275-277 declares its own
    inner `data` key on top of the envelope the middleware adds.  Do not flatten.
    """
    await require_auth(request)
    ctx = get_context()

    payload: dict = {
        "connectorConnected": True,
        "connectorVersion": ctx.connector_version or "0.0.0",
    }

    # Relay status — best-effort from the paired device id.
    payload["relayConnected"] = bool(ctx.paired_device_id)

    # Hermes status — best-effort from active jobs.
    if ctx.gateway_restart is not None:
        payload["hermesConnected"] = True
        # Report the connector version as the relay version since the
        # legacy relay is not running; avoids a confusing "—" in the UI.
        payload["version"] = ctx.connector_version or "0.0.0"

    # Uptime — connector process lifetime, not host uptime.
    try:
        payload["uptimeSeconds"] = int(time.monotonic() - _PROCESS_STARTED_AT)
    except Exception:
        pass

    # modelName — Hermes pill + System→Model. Reuses the catalog the app already reads.
    try:
        if ctx.model_catalog is not None:
            catalog = ctx.model_catalog()
            if inspect.isawaitable(catalog):
                catalog = await catalog
            active = (catalog or {}).get("activeModel") or {}
            if active.get("name"):
                payload["modelName"] = active["name"]
    except Exception:
        logger.debug("gw/status: model name unavailable", exc_info=True)

    # cpuPercent / memory — /proc, no psutil dependency.
    try:
        with open("/proc/meminfo", "r", encoding="utf-8") as fh:
            meminfo = {}
            for line in fh:
                key, _, rest = line.partition(":")
                meminfo[key] = int(rest.strip().split()[0])     # kB
        total_gb = meminfo["MemTotal"] / 1024 / 1024
        avail_gb = meminfo["MemAvailable"] / 1024 / 1024
        payload["memoryTotalGb"] = round(total_gb, 2)
        payload["memoryUsedGb"] = round(total_gb - avail_gb, 2)
    except Exception:
        logger.debug("gw/status: meminfo unavailable", exc_info=True)

    payload["activeJobs"] = len([j for j in _http_jobs.values() if j["status"] == "running"])

    # Omit alerts — we have no alert source yet, and a malformed timestamp
    # in an alert would cause dataCorrupted for the entire response.
    # Omit activeJobsList — requires connector-backed job tracking (b32).

    # Double-wrapped ON PURPOSE: GatewayStatusScreen.swift:275-277 declares its own
    # inner `data` key on top of the envelope the middleware adds. Flattening this
    # yields "The data couldn't be read because it is missing."
    return JSONResponse({"data": payload})


async def capabilities_endpoint(request: Request) -> JSONResponse:
    return JSONResponse({
        "supportsStreaming": False,
        "supportsModels": True,
        "supportsProfiles": True,
        "supportsAttachments": True,
        "supportsVoice": True,
        "supportsCron": False,
        "supportsMemories": False,
        "maxMessageLength": 4096,
    })


async def send_message(request: Request) -> JSONResponse:
    """Accept a chat message and return a pending job.

    RETURNS JSON, NOT SSE.  See the registry comment above — the app decodes this
    body with JSONDecoder and an SSE body is an unconditional dataCorrupted error.
    The streaming experience is preserved via GET /v1/jobs/{id}/events, which
    JobStreamCoordinator (JobStreamCoordinator.swift:98) subscribes to as soon as
    it sees replyState == "pending".
    """
    await require_auth(request)
    ctx = get_context()
    if ctx.message_handler is None:
        raise HTTPException(status_code=503, detail="Message handler not available")

    try:
        body = await request.json()
    except Exception:
        raise HTTPException(status_code=400, detail="Request body must be JSON")
    if not isinstance(body, dict):
        raise HTTPException(status_code=400, detail="Request body must be a JSON object")
    text = body.get("text", "")
    history = body.get("history") or []
    session_id = body.get("sessionId")
    attachments = body.get("attachments")
    reasoning_effort = body.get("reasoningEffort")
    client_message_id = body.get("clientMessageId")
    raw_conversation_id = body.get("conversationId")

    # Resolve the app's conversation UUID to a Hermes session id.
    # P0-1: instead of a random uuid4() that maps to nothing, use the
    # deterministic app_uuid ↔ hermes_id reverse index.  This makes
    # GET /v1/sessions/{id}/conversation return the messages that were
    # actually written to the database.
    from .session_store import _app_uuid, _resolve_hermes_id, _coerce_uuid as _store_coerce
    hermes_session_id: str | None = None
    app_conversation_id: str | None = None

    if raw_conversation_id is not None:
        cid = _coerce_uuid(raw_conversation_id)
        if cid:
            # Does this UUID already map to a Hermes session?
            resolved = _resolve_hermes_id(cid)
            if resolved:
                hermes_session_id = resolved
                app_conversation_id = cid
            else:
                # B38 P0-2: echo the app-supplied UUID verbatim even when
                # the sidecar mapping doesn't exist yet.  B37 silently
                # discarded it and fell through to the process singleton —
                # that collapsed every conversation onto one id.
                app_conversation_id = cid

    # If the caller sent a sessionId (Hermes-side), use it directly and
    # derive the app UUID from it.
    if hermes_session_id is None and session_id:
        hermes_session_id = str(session_id)
        app_conversation_id = _app_uuid(hermes_session_id)

    if app_conversation_id is None:
        # B38 P0-2: _stable_conversation_id() is now the fourth-choice
        # fallback only.  Log a warning so this can never regress silently.
        logger.warning(
            "_stable_conversation_id fallback used — no conversationId, "
            "no sessionId, no sidecar mapping. body=%s",
            {k: v for k, v in body.items() if k != "history"}
        )
        app_conversation_id = _stable_conversation_id()

    job_id = str(uuid.uuid4())
    user_message = _relay_message("user", text, client_message_id=client_message_id)

    _http_jobs[job_id] = {
        "jobId": job_id,
        "status": "running",
        "conversationId": app_conversation_id,
        "message": None,
        "error": None,
        "errorCategory": None,
        "errorAction": None,
        "usage": None,
        "events": [],
        "subscribers": [],
        "updatedAt": time.time(),
    }
    _prune_http_jobs()

    task = asyncio.create_task(
        _run_http_job(job_id, ctx.message_handler, text, history,
                      hermes_session_id, attachments, reasoning_effort)
    )
    _http_job_tasks[job_id] = task
    task.add_done_callback(lambda _t, jid=job_id: _http_job_tasks.pop(jid, None))

    # MessageResponse (LiveHeraldClient.swift:12-21): replyState and conversation
    # are non-optional.  RelayConversation.title is a non-optional String and
    # .updatedAt a non-optional Date — null in either is a decode failure.
    return JSONResponse({
        "replyState": "pending",
        "jobId": job_id,
        "conversation": {
            "id": app_conversation_id,
            "title": "Herald",
            "updatedAt": _now_iso(),
            "messages": [user_message],
            "latestUsage": None,
            "latestContext": None,
        },
        "userMessage": user_message,
        "message": None,
        "usage": None,
        "context": None,
        "diff": None,
    })


# ── Pairing / Auth ───────────────────────────────────────────────────────

# Phone pairing codes stored in-memory (no Postgres needed).
# Maps normalized code → {expires_at, created_at}
_pending_pairing_codes: dict[str, dict] = {}
import hashlib, secrets as _secrets


def _hash_code(code: str) -> str:
    return hashlib.sha256(code.encode()).hexdigest()


def _generate_pairing_code() -> tuple[str, str]:
    """Returns (normalized_code, display_code). 8 alphanumeric chars."""
    chars = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789"
    code = "".join(_secrets.choice(chars) for _ in range(8))
    return code, f"{code[:4]}-{code[4:]}"


async def create_phone_pairing_code(request: Request) -> JSONResponse:
    """Create a phone pairing code. No auth needed — the connector calls this."""
    import time as _time
    code, display = _generate_pairing_code()
    hashed = _hash_code(code)
    expires = _time.time() + 600  # 10 minute expiry
    _pending_pairing_codes[hashed] = {"expires_at": expires, "created_at": _time.time()}
    # Clean expired codes
    for k in list(_pending_pairing_codes):
        if _pending_pairing_codes[k]["expires_at"] < _time.time():
            del _pending_pairing_codes[k]
    logger.info("Created pairing code: %s", display)
    return JSONResponse({"code": code, "displayCode": display, "expiresAt": expires})


async def redeem_phone_pairing(request: Request) -> JSONResponse:
    """Redeem a phone pairing code. Returns access + refresh tokens.

    Idempotent: a repeat redeem with the same installationId inside the TTL
    returns the same payload instead of 401.  A different installation
    redeeming an already-used code still gets 401.
    """
    import time as _time
    ctx = get_context()
    body = await request.json()
    installation_id = body.get("installationId") or body.get("deviceId") or ""
    logger.info("Redeem body keys: %s, code raw: %s, installation: %s",
                list(body.keys()), body.get("code", "?")[:20], installation_id[:12])
    code = (body.get("code") or "").upper().replace("-", "").replace(" ", "")
    logger.info("Redeem normalized code: %s", code)
    hashed = _hash_code(code)

    # Look up WITHOUT popping — idempotent replay needs the stored record.
    stored = _pending_pairing_codes.get(hashed)
    if stored is None or stored["expires_at"] < _time.time():
        raise HTTPException(status_code=401, detail="Invalid or expired pairing code")

    # Already redeemed by this installation → replay the saved payload.
    if stored.get("redeemed_at") is not None:
        if stored.get("installation_id") == installation_id:
            logger.info("Idempotent replay of pairing code %s for installation %s",
                        code, installation_id[:12])
            return JSONResponse(stored["_response_payload"])
        raise HTTPException(status_code=401, detail="Invalid or expired pairing code")

    # First redeem — build the response, mark, and persist.
    token = ctx.connector_credential or ctx.paired_device_id or "herald-connector"
    _default_validator.add_token(token)
    import uuid as _uuid
    payload = {
        "user": {"id": ctx.paired_user_id or str(_uuid.uuid4()), "displayName": "Herald User"},
        "deviceId": str(_uuid.uuid4()),
        "deviceRegistered": True,
        "session": {"connectionStatus": "connected", "isMockMode": False, "backendEndpoint": ctx.public_base_url or "", "lastSyncAt": None},
        "auth": {"accessToken": token, "refreshToken": token, "expiresAt": datetime.datetime.now(datetime.timezone.utc).isoformat()},
    }
    stored["redeemed_at"] = _time.time()
    stored["installation_id"] = installation_id
    stored["_response_payload"] = payload
    return JSONResponse(payload)


async def redeem_pairing(request: Request) -> JSONResponse:
    """Redeem a host setup code (HC1:...). Returns access token if valid."""
    import time as _time
    ctx = get_context()
    body = await request.json()
    raw = (body.get("code") or body.get("setupCode") or "").strip()

    if raw.startswith("HC1:"):
        import base64 as _b64
        try:
            encoded = raw[4:]
            padding = "=" * (-len(encoded) % 4)
            decoded = _b64.urlsafe_b64decode((encoded + padding).encode()).decode()
            payload = json.loads(decoded)
            enrollment_token = payload.get("enrollment_token", "")
            # Accept if enrollment token matches connector credential
            expected = ctx.connector_credential or ctx.paired_device_id or ""
            if enrollment_token and enrollment_token == expected:
                token = enrollment_token
                _default_validator.add_token(token)
                import uuid as _uuid2
                return JSONResponse({
                    "user": {"id": str(_uuid2.uuid4()), "displayName": "Herald User"},
                    "deviceId": str(_uuid2.uuid4()),
                    "deviceRegistered": True,
                    "session": {"connectionStatus": "connected", "isMockMode": False, "backendEndpoint": payload.get("relay_url", ctx.public_base_url), "lastSyncAt": None},
                    "auth": {"accessToken": token, "refreshToken": token, "expiresAt": datetime.datetime.now(datetime.timezone.utc).isoformat()},
                })
        except Exception:
            pass

    raise HTTPException(status_code=401, detail="Invalid setup code")


async def refresh_auth(request: Request) -> JSONResponse:
    """Refresh an access token. The connector credential never expires."""
    await require_auth(request)
    import time as _time
    return JSONResponse({"accessToken": await _extract_token(request), "expiresAt": datetime.datetime.now(datetime.timezone.utc).isoformat()})


async def register_device(request: Request) -> JSONResponse:
    """Register a device. Returns session + auth matching DeviceRegisterResponse."""
    await require_auth(request)
    ctx = get_context()
    body = await request.json()
    dev = body.get("device", {})
    logger.info("Device registered: %s", dev.get("deviceName", "?")[:30])
    import time as _time3, uuid as _uuid4
    token = ctx.connector_credential or str(_uuid4.uuid4())
    return JSONResponse({
        "deviceId": str(_uuid4.uuid4()),
        "deviceRegistered": True,
        "session": {"connectionStatus": "connected", "isMockMode": False, "backendEndpoint": ctx.public_base_url or "", "lastSyncAt": None},
        "auth": {"accessToken": token, "refreshToken": token, "expiresAt": _time3.time() + 86400},
    })


async def connector_events(request: Request) -> StreamingResponse:
    """SSE stream of connector health events."""
    import asyncio as _asyncio
    async def stream():
        yield "event: connected\ndata: {}\n\n"
        while True:
            try:
                await _asyncio.sleep(30)
                if await request.is_disconnected():
                    break
                yield "event: health_check\ndata: {\"status\": \"online\"}\n\n"
            except Exception:
                break
    return StreamingResponse(stream(), media_type="text/event-stream",
        headers={"Cache-Control": "no-cache", "Connection": "keep-alive", "X-Accel-Buffering": "no"})


async def get_sessions(request: Request) -> JSONResponse:
    """Return session list backed by state.db (B34 P1-1).

    ``total`` is REQUIRED by the iOS decoder — LiveHeraldClient.swift declares
    SessionListAPIResponse.total as non-optional Int, so omitting it raises
    DecodingError.keyNotFound and the app shows "The data couldn't be read
    because it is missing." Never drop this key.
    """
    await require_auth(request)
    from .session_store import session_list

    limit = int(request.query_params.get("limit", "50"))
    offset = int(request.query_params.get("offset", "0"))
    try:
        sessions, total = await asyncio.to_thread(session_list, limit=limit, offset=offset)
    except Exception:
        logger.exception("session_list query failed")
        sessions, total = [], 0

    return JSONResponse({"sessions": sessions, "total": total})


async def get_inbox(request: Request) -> JSONResponse:
    """Return inbox (stub)."""
    await require_auth(request)
    return JSONResponse({"items": []})


async def push_register(request: Request) -> JSONResponse:
    """Register for push notifications."""
    await require_auth(request)
    body = await request.json()
    logger.info("Push registration: %s", body.get("deviceToken", "?")[:16])
    return JSONResponse({"registered": True})


async def host_current(request: Request) -> JSONResponse:
    """Return current host info.

    The iOS decoder expects `{host: {id: UUID, displayName, isOnline}}` —
    LiveHeraldHostService.swift:13-15 decodes CurrentHostResponse.host as
    RelayHost?, and RelayHost.id is a non-optional UUID. A bare object
    (missing the "host" key) decodes to nil → "No Hermes host connected".
    """
    await require_auth(request)
    ctx = get_context()
    import uuid as _uuid
    host_id = str(_uuid.uuid5(_uuid.NAMESPACE_DNS, "herald-host"))
    raw_id = ctx.paired_device_id
    if raw_id:
        try:
            _uuid.UUID(raw_id)
            host_id = raw_id
        except (ValueError, AttributeError):
            host_id = str(_uuid.uuid5(_uuid.NAMESPACE_DNS, str(raw_id)))
    agent_version = None
    if getattr(ctx, "agent_version", None) is not None:
        try:
            agent_version = ctx.agent_version()
        except Exception:  # noqa: BLE001
            agent_version = None
    return JSONResponse({
        "host": {
            "id": host_id,
            "displayName": "Herald Host",
            "isOnline": True,
            # RelayHost decodes these as optional String? — LiveHeraldHostService
            # RelayHost / HeraldHostStatus.swift:8,10. Omitting them rendered
            # "—" in the Settings → Infrastructure rows.
            "connectorVersion": ctx.connector_version,
            "heraldVersion": agent_version,
        }
    })


async def host_enrollment_codes(request: Request) -> JSONResponse:
    """Create a host enrollment code."""
    await require_auth(request)
    code, display = _generate_pairing_code()
    return JSONResponse({"code": code, "displayCode": display})


# ── P0-4: Chat critical-path endpoints ────────────────────────────────────


async def session_conversation(request: Request) -> JSONResponse:
    """Get message history for a session."""
    await require_auth(request)
    ctx = get_context()
    session_id = request.path_params.get("id", "")
    if ctx.session_conversation is None:
        raise HTTPException(status_code=503, detail="Session history not available")
    result = ctx.session_conversation(session_id)
    if inspect.isawaitable(result):
        result = await result
    return JSONResponse(result)


async def current_conversation(request: Request) -> JSONResponse:
    """Get the active conversation on launch.

    Shape is dictated by ConversationResponse/RelayConversation
    (LiveHeraldClient.swift:23-30) — `conversation` is required and its id/title/
    updatedAt/messages are all non-optional. The connector stub returns a flat
    {sessionId, messages, title}, so normalize here.
    """
    await require_auth(request)
    ctx = get_context()
    if ctx.current_conversation is None:
        raise HTTPException(status_code=503, detail="Conversation service not available")
    result = ctx.current_conversation()
    if inspect.isawaitable(result):
        result = await result
    result = result or {}
    if "conversation" in result:
        return JSONResponse(result)
    return JSONResponse({"conversation": {
        "id": _coerce_uuid(result.get("sessionId")) or _stable_conversation_id(),
        "title": result.get("title") or "Herald",
        "updatedAt": _now_iso(),
        "messages": result.get("messages") or [],
        "latestUsage": None,
        "latestContext": None,
    }})


async def clear_current_conversation(request: Request) -> JSONResponse:
    """Clear the active conversation (/new)."""
    await require_auth(request)
    ctx = get_context()
    if ctx.clear_conversation is None:
        raise HTTPException(status_code=503, detail="Conversation service not available")
    result = ctx.clear_conversation()
    if inspect.isawaitable(result):
        result = await result
    return JSONResponse(result)


async def job_status(request: Request) -> JSONResponse:
    """Poll job status.

    Returns {"data": {...}} — the app declares its own inner `data` key
    (LiveHeraldClient.swift:827-829) *in addition to* the envelope the middleware
    adds, so this handler double-wraps on purpose.  Do not flatten it.
    """
    await require_auth(request)
    ctx = get_context()
    job_id = request.path_params.get("id", "")

    job = _http_jobs.get(job_id)
    if job is not None:
        return JSONResponse({"data": {
            "jobId": job_id,
            "status": job["status"],
            "conversationId": job["conversationId"],
            "error": job["error"],
            "errorCategory": job["errorCategory"],
            "errorAction": job["errorAction"],
            "usage": job.get("usage"),
            "context": None,
            "diff": None,
            "message": job["message"],
            "attempt": 0,
            "lastSeq": max(len(job["events"]) - 1, 0),
        }})

    # Fallback: jobs created by the legacy relay WS path. Do not remove.
    if ctx.job_status is None:
        raise HTTPException(status_code=503, detail="Job service not available")
    result = ctx.job_status(job_id)
    if inspect.isawaitable(result):
        result = await result
    return JSONResponse({"data": result})


async def job_events(request: Request) -> StreamingResponse:
    """SSE stream of job events."""
    await require_auth(request)
    ctx = get_context()
    job_id = request.path_params.get("id", "")

    job = _http_jobs.get(job_id)
    if job is not None:
        async def facade_stream() -> AsyncIterator[str]:
            queue: asyncio.Queue = asyncio.Queue()
            # Resume from Last-Event-ID so a reconnect does not renumber the
            # backlog from 0.  JobStreamCoordinator drops events at or below
            # its cursor, so restarting at 0 made the replayed terminal event
            # look like a duplicate and the stream hung.
            try:
                cursor = int(request.headers.get("Last-Event-ID", "-1"))
            except (TypeError, ValueError):
                cursor = -1
            backlog = list(job["events"])
            job["subscribers"].append(queue)
            try:
                seq = cursor + 1
                for event in backlog[cursor + 1:]:
                    yield f"id: {seq}\nevent: {event['type']}\ndata: {json.dumps(event['data'])}\n\n"
                    seq += 1
                if job["status"] != "running" and backlog and backlog[-1]["type"] == "done":
                    return
                while True:
                    event = await queue.get()
                    if event is None:
                        return
                    if await request.is_disconnected():
                        return
                    yield f"id: {seq}\nevent: {event['type']}\ndata: {json.dumps(event['data'])}\n\n"
                    seq += 1
            finally:
                if queue in job["subscribers"]:
                    job["subscribers"].remove(queue)

        return StreamingResponse(
            facade_stream(),
            media_type="text/event-stream",
            headers={"Cache-Control": "no-cache", "Connection": "keep-alive",
                     "X-Accel-Buffering": "no"},
        )

    # Fallback: jobs created by the legacy relay WS path. Do not remove.
    if ctx.job_events is None:
        raise HTTPException(status_code=503, detail="Job event streaming not available")

    async def event_stream() -> AsyncIterator[str]:
        seq = 0
        try:
            async for event in ctx.job_events(job_id):
                if await request.is_disconnected():
                    break
                event_type = event.get("type", "progress")
                sse_data = json.dumps(event.get("data", event))
                yield f"id: {seq}\nevent: {event_type}\ndata: {sse_data}\n\n"
                seq += 1
        except asyncio.CancelledError:
            pass
        except Exception as exc:
            logger.exception("Job event stream error for %s", job_id)
            yield f"id: {seq}\nevent: failed\ndata: {json.dumps({'error': str(exc), 'jobId': job_id})}\n\n"

    return StreamingResponse(
        event_stream(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "Connection": "keep-alive",
            "X-Accel-Buffering": "no",
        },
    )


async def cancel_job(request: Request) -> JSONResponse:
    """Cancel a running job (/stop)."""
    await require_auth(request)
    ctx = get_context()
    job_id = request.path_params.get("id", "")

    task = _http_job_tasks.get(job_id)
    if task is not None:
        task.cancel()
        job = _http_jobs.get(job_id)
        if job is not None:
            job["status"] = "cancelled"
            job["updatedAt"] = time.time()
        return JSONResponse({"jobId": job_id, "status": "cancelled"})

    # Fallback: jobs created by the legacy relay WS path. Do not remove.
    if ctx.job_cancel is None:
        raise HTTPException(status_code=503, detail="Job cancellation not available")
    result = ctx.job_cancel({"jobId": job_id})
    if inspect.isawaitable(result):
        result = await result
    return JSONResponse(result)


# ── F-3: Gateway Logs ────────────────────────────────────────────────────


async def gateway_logs(request: Request) -> JSONResponse:
    """Recent connector logs from journald.

    Returns {"data": {"lines": [...]}} — GatewayLogsScreen.swift:170-175 declares
    its own inner `data` key on top of the envelope. Do not flatten.
    """
    await require_auth(request)
    lines = min(int(request.query_params.get("lines", "200") or 200), 1000)
    level = (request.query_params.get("level") or "info").lower()
    priority = _JOURNAL_PRIORITY.get(level, "6")

    proc = await asyncio.create_subprocess_exec(
        "journalctl", "--user", "-u", _JOURNAL_UNIT,
        "-n", str(lines), "-p", priority, "-o", "json", "--no-pager",
        stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL,
    )
    try:
        stdout, _ = await asyncio.wait_for(proc.communicate(), timeout=10.0)
    except asyncio.TimeoutError:
        proc.kill()
        raise HTTPException(status_code=504, detail="journalctl timed out")

    out: list[dict] = []
    for raw in stdout.decode("utf-8", "replace").splitlines():
        try:
            out.append(_journal_line(json.loads(raw), timestamp_as_number=False))
        except (ValueError, KeyError, TypeError):
            continue
    return JSONResponse({"data": {"lines": out}})


async def gateway_logs_stream(request: Request) -> StreamingResponse:
    """Live tail. SSE `data:` is decoded by a BARE JSONDecoder on the app side
    (GatewayLogsScreen.swift:236), so timestamps go out as numbers, not strings."""
    await require_auth(request)
    level = (request.query_params.get("level") or "info").lower()
    priority = _JOURNAL_PRIORITY.get(level, "6")

    async def stream() -> AsyncIterator[str]:
        proc = await asyncio.create_subprocess_exec(
            "journalctl", "--user", "-u", _JOURNAL_UNIT,
            "-f", "-n", "0", "-p", priority, "-o", "json", "--no-pager",
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.DEVNULL,
        )
        try:
            while True:
                try:
                    raw = await asyncio.wait_for(proc.stdout.readline(), timeout=25.0)
                except asyncio.TimeoutError:
                    yield ": keepalive\n\n"          # parsed as a comment, ignored by the app
                    continue
                if not raw:
                    return
                if await request.is_disconnected():
                    return
                try:
                    line = _journal_line(json.loads(raw), timestamp_as_number=True)
                except (ValueError, KeyError, TypeError):
                    continue
                yield f"event: log\ndata: {json.dumps(line)}\n\n"
        finally:
            with contextlib.suppress(ProcessLookupError):
                proc.kill()

    return StreamingResponse(stream(), media_type="text/event-stream",
        headers={"Cache-Control": "no-cache", "Connection": "keep-alive",
                 "X-Accel-Buffering": "no"})


# ── F-6: Device telemetry & session stubs ────────────────────────────────


async def device_app_state(request: Request) -> JSONResponse:
    """AppContainer.swift:1139 decodes an empty struct — any JSON object works."""
    await require_auth(request)
    body = await request.json()
    logger.debug("Device app state: %s", body.get("state"))
    return JSONResponse({"acknowledged": True})


async def device_sensor(request: Request) -> JSONResponse:
    """SensorUploadService.swift:107-113 decodes DeliveryResult.deliveryState and
    treats anything other than "delivered" as a failure that triggers backoff."""
    await require_auth(request)
    await request.json()
    return JSONResponse({"deliveryState": "delivered"})


async def session_generate_title(request: Request) -> JSONResponse:
    """Generate a title for a session from its first user message.

    B38 P1-1: tries the LLM via the message_handler first (3-8 word title);
    falls back to truncation when the handler is unavailable.
    """
    await require_auth(request)
    session_id = request.path_params.get("id", "")
    from .session_store import session_messages, set_session_meta

    title = None
    try:
        msgs = session_messages(session_id, limit=5)
        for m in msgs:
            if m.get("role") == "user" and m.get("text"):
                user_text = m["text"].strip()
                ctx = get_context()
                if ctx.message_handler:
                    from .session_store import _app_uuid
                    app_id = _app_uuid(session_id)
                    title = await _auto_title(
                        ctx.message_handler, user_text, session_id, app_id
                    )
                if not title:
                    # Fallback: first line, first 80 chars
                    first_line = user_text.split("\n")[0].strip()
                    title = first_line[:80] if first_line else None
                break
    except Exception:
        logger.exception("session_generate_title: failed for %s", session_id)

    if title:
        try:
            set_session_meta(session_id, title=title)
        except Exception:
            logger.exception("session_generate_title: set_session_meta failed for %s", session_id)
    else:
        title = "New Chat"

    return JSONResponse({"title": title})


# B38 P1-1: placeholder titles that must never be persisted.
# Once written to the sidecar, they permanently shadow any generated title
# because meta.get("title") is checked FIRST in session_list.
_PLACEHOLDER_TITLES = frozenset({
    "", "new chat", "untitled", "herald",
    "new chat", "New Chat", "Untitled", "Herald",
})


async def session_patch(request: Request) -> JSONResponse:
    """Rename a session (PATCH /v1/sessions/{id}).

    B38 P1-1: rejects placeholder titles so a generated title can win.
    Only a genuine user rename or a server-generated title gets persisted.
    """
    await require_auth(request)
    body = await request.json()
    session_id = request.path_params.get("id", "")
    raw_title = (body.get("title") or "").strip()
    from .session_store import set_session_meta

    if raw_title.lower() in _PLACEHOLDER_TITLES:
        # The app sent a placeholder — do NOT persist it.  Return the
        # requested title so the client doesn't error, but keep the
        # sidecar clean for server-side generation.
        logger.info("session_patch: refusing placeholder title %r for %s", raw_title, session_id)
        title = raw_title
    else:
        title = raw_title[:200]
        set_session_meta(session_id, title=title)
    return JSONResponse({"session": {
        "id": _coerce_uuid(session_id) or str(uuid.uuid4()),
        "title": title,
        "previewText": None,
        "updatedAt": _now_iso(),
        "source": None,
        "isPinned": None,
        "isArchived": None,
    }})


# ── B34 P0-3: Session CRUD ─────────────────────────────────────────────────


async def create_session(request: Request) -> JSONResponse:
    """Create a new session (POST /v1/sessions).

    Does NOT write to state.db (G1) — Hermes materialises the row itself
    on the first message carrying X-Hermes-Session-Id.  We just mint an id
    and record an optimistic title in the local sidecar.

    SessionAPIResponse.session is a non-optional SessionAPIEntry
    (LiveHeraldClient.swift:709-724).  Every key the decoder reads must
    be present and of the declared type.
    """
    await require_auth(request)
    try:
        body = await request.json()
    except (json.JSONDecodeError, ValueError):
        body = {}
    if not isinstance(body, dict):
        body = {}
    session_id = str(uuid.uuid4())
    raw_title = (body.get("title") or "").strip()
    # B38 P1-1: don't persist placeholder titles — they permanently shadow
    # server-generated titles.
    title = raw_title[:200] if raw_title.lower() not in _PLACEHOLDER_TITLES else ""
    from .session_store import set_session_meta

    if title:
        set_session_meta(session_id, title=title)
    return JSONResponse({"session": {
        "id": session_id,
        "title": title,
        "previewText": "",
        "updatedAt": _now_iso(),
        "source": "api_server",
        "isPinned": False,
        "isArchived": False,
    }})


async def session_delete(request: Request) -> JSONResponse:
    """Soft-delete a session (DELETE /v1/sessions/{id}).

    Tombstones in the local sidecar only (G1).  The session row stays in
    state.db — Hermes owns it.
    """
    await require_auth(request)
    session_id = request.path_params.get("id", "")
    from .session_store import set_session_meta

    set_session_meta(session_id, tombstone=True)
    return JSONResponse({"deleted": True})


async def session_pin(request: Request) -> JSONResponse:
    """Toggle pin state (POST /v1/sessions/{id}/pin).

    Writes to the local sidecar (G1).  The body carries {"pinned": bool}.
    """
    await require_auth(request)
    session_id = request.path_params.get("id", "")
    try:
        body = await request.json()
    except (json.JSONDecodeError, ValueError):
        body = {}
    if not isinstance(body, dict):
        body = {}
    pinned = bool(body.get("pinned", True))
    from .session_store import set_session_meta

    set_session_meta(session_id, pinned=pinned)
    return JSONResponse({"id": _coerce_uuid(session_id) or session_id, "isPinned": pinned})


async def session_archive(request: Request) -> JSONResponse:
    """Toggle archive state (POST /v1/sessions/{id}/archive).

    Writes to the local sidecar (G1).  The body carries {"archived": bool}.
    """
    await require_auth(request)
    session_id = request.path_params.get("id", "")
    try:
        body = await request.json()
    except (json.JSONDecodeError, ValueError):
        body = {}
    if not isinstance(body, dict):
        body = {}
    archived = bool(body.get("archived", True))
    from .session_store import set_session_meta

    set_session_meta(session_id, archived=archived)
    return JSONResponse({"id": _coerce_uuid(session_id) or session_id, "isArchived": archived})


async def session_search_handler(request: Request) -> JSONResponse:
    """Search sessions by title (GET /v1/sessions/search?q=…).

    SessionSearchAPIResponse.sessions is [SessionSearchResult] —
    each result needs id, title, and updatedAt (LiveHeraldClient.swift).
    """
    await require_auth(request)
    q = request.query_params.get("q", "").strip()
    if not q:
        return JSONResponse({"sessions": []})
    from .session_store import session_search

    results = await asyncio.to_thread(session_search, q)
    return JSONResponse({"sessions": results})


# ── B34 P2-1: Unimplemented-route stubs ────────────────────────────────────
#
# Every path below is called by the iOS app.  A 404 renders a user-visible
# error alert; a decodable empty payload renders an empty screen cleanly.
# Each handler returns the shape its decoder expects.  TODO(b35): implement.


async def stub_skills(request: Request) -> JSONResponse:
    """GET /v1/skills — return empty skill list."""
    await require_auth(request)
    return JSONResponse({"skills": []})


async def stub_cron_list(request: Request) -> JSONResponse:
    """GET /v1/cron — return empty job list."""
    await require_auth(request)
    return JSONResponse({"jobs": []})


async def stub_cron_detail(request: Request) -> JSONResponse:
    """GET/DELETE /v1/cron/{id} — not implemented."""
    await require_auth(request)
    return JSONResponse({"status": "not_implemented"}, status_code=501)


async def stub_notes_list(request: Request) -> JSONResponse:
    """GET /v1/notes — return empty note list."""
    await require_auth(request)
    return JSONResponse({"notes": []})


async def stub_notes_detail(request: Request) -> JSONResponse:
    """GET /v1/notes/{id} — not implemented."""
    await require_auth(request)
    return JSONResponse({"status": "not_implemented"}, status_code=501)


async def stub_notes_recognitions(request: Request) -> JSONResponse:
    """GET /v1/notes/{id}/recognitions — not implemented."""
    await require_auth(request)
    return JSONResponse({"recognitions": []})


async def stub_notes_runs(request: Request) -> JSONResponse:
    """GET /v1/notes/{id}/runs — not implemented."""
    await require_auth(request)
    return JSONResponse({"runs": []})


async def stub_note_runs_detail(request: Request) -> JSONResponse:
    """GET /v1/note-runs/{id} — not implemented."""
    await require_auth(request)
    return JSONResponse({"status": "not_implemented"}, status_code=501)


async def stub_note_runs_cancel(request: Request) -> JSONResponse:
    """POST /v1/note-runs/{id}/cancel — not implemented."""
    await require_auth(request)
    return JSONResponse({"cancelled": False, "status": "not_implemented"}, status_code=501)


async def stub_note_runs_events(request: Request) -> JSONResponse:
    """GET /v1/note-runs/{id}/events — not implemented."""
    await require_auth(request)
    return JSONResponse({"status": "not_implemented"}, status_code=501)


async def stub_talk_readiness(request: Request) -> JSONResponse:
    """GET /v1/talk/readiness — not configured, but shape-complete.

    TalkReadinessResponse (LiveVoiceSessionService.swift:20-29) declares
    hostOnline and configured as non-optional, so omitting them makes the app
    fail with a decode error instead of showing an unavailable state.
    """
    await require_auth(request)
    return JSONResponse({
        "ready": False,
        "hostOnline": True,
        "configured": False,
        "blockedReason": "Realtime Talk is not configured on this host.",
        "preferredModels": None,
        "selectedModel": None,
        "voice": None,
        "voiceContextUpdatedAt": None,
    })


async def stub_talk_session(request: Request) -> JSONResponse:
    """POST /v1/talk/session — not implemented."""
    await require_auth(request)
    return JSONResponse({"status": "not_implemented"}, status_code=501)


async def stub_talk_session_end(request: Request) -> JSONResponse:
    """POST /v1/talk/session/{id}/end — not implemented."""
    await require_auth(request)
    return JSONResponse({"status": "not_implemented"}, status_code=501)


async def stub_talk_session_inject(request: Request) -> JSONResponse:
    """POST /v1/talk/session/{id}/inject — not implemented."""
    await require_auth(request)
    return JSONResponse({"status": "not_implemented"}, status_code=501)


async def stub_talk_session_turns(request: Request) -> JSONResponse:
    """GET /v1/talk/session/{id}/turns — not implemented."""
    await require_auth(request)
    return JSONResponse({"turns": []})


async def stub_gw_update(request: Request) -> JSONResponse:
    """GET /v1/gw/update — return no-update."""
    await require_auth(request)
    return JSONResponse({"updateAvailable": False})


async def stub_gw_update_check(request: Request) -> JSONResponse:
    """POST /v1/gw/update/check — return no-update."""
    await require_auth(request)
    return JSONResponse({"updateAvailable": False})


async def stub_push_deactivate(request: Request) -> JSONResponse:
    """POST /v1/push/deactivate — not implemented."""
    await require_auth(request)
    return JSONResponse({"deactivated": False, "status": "not_implemented"}, status_code=501)


async def stub_push_broker_challenge(request: Request) -> JSONResponse:
    """POST /v1/push-broker/challenge — not implemented."""
    await require_auth(request)
    return JSONResponse({"status": "not_implemented"}, status_code=501)


async def stub_push_broker_register(request: Request) -> JSONResponse:
    """POST /v1/push-broker/register — not implemented."""
    await require_auth(request)
    return JSONResponse({"status": "not_implemented"}, status_code=501)


async def stub_relay_identity(request: Request) -> JSONResponse:
    """GET /v1/relay/identity — not implemented."""
    await require_auth(request)
    return JSONResponse({"status": "not_implemented"}, status_code=501)


async def stub_hosts_current_revoke(request: Request) -> JSONResponse:
    """POST /v1/hosts/current/revoke — not implemented."""
    await require_auth(request)
    return JSONResponse({"revoked": False, "status": "not_implemented"}, status_code=501)


async def stub_inbox_action(request: Request) -> JSONResponse:
    """POST /v1/inbox/{id}/action — not implemented."""
    await require_auth(request)
    return JSONResponse({"status": "not_implemented"}, status_code=501)


# ── Envelope middleware ──────────────────────────────────────────────────


def envelope(data: Any) -> dict:
    """Wrap a JSON payload in the relay envelope the iOS app decodes.

    RelayAPIClient.swift:522 unconditionally unwraps {"data":…,"meta":…}.
    Every JSON response MUST pass through here.  SSE, error dicts, and
    non-JSON pass through the middleware unchanged.
    """
    return {
        "data": data,
        "meta": {
            "requestId": str(uuid.uuid4()),
            "timestamp": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        },
    }


async def envelope_middleware(scope, receive, send):
    """Wrap JSON bodies as {"data":…,"meta":…} to match the relay contract.

    SSE (text/event-stream) and non-JSON responses pass through untouched.
    Error responses (dicts that already contain an "error" key) are also
    passed through so the exception handler's envelope isn't double-wrapped.
    """
    if scope["type"] != "http":
        await app(scope, receive, send)
        return

    start_message: dict | None = None
    chunks: list[bytes] = []
    passthrough = False

    async def send_wrapper(message):
        nonlocal start_message, passthrough
        if message["type"] == "http.response.start":
            headers = {k.decode().lower(): v.decode() for k, v in message["headers"]}
            ct = headers.get("content-type", "")
            passthrough = not ct.startswith("application/json")
            start_message = message
            if passthrough:
                await send(message)
            return
        if message["type"] == "http.response.body":
            if passthrough:
                await send(message)
                return
            chunks.append(message.get("body", b""))
            if message.get("more_body"):
                return
            raw = b"".join(chunks)
            try:
                payload = json.loads(raw) if raw else None
            except ValueError:
                payload = None
            body = (
                raw
                if payload is None or ("error" in payload if isinstance(payload, dict) else False)
                else json.dumps(envelope(payload)).encode()
            )
            headers = [
                (k, v)
                for k, v in start_message["headers"]
                if k.decode().lower() != "content-length"
            ]
            headers.append((b"content-length", str(len(body)).encode()))
            await send({**start_message, "headers": headers})
            await send({"type": "http.response.body", "body": body})

    await app(scope, receive, send_wrapper)


# ── Error handling ──────────────────────────────────────────────────────


async def http_exception_handler(request: Request, exc: HTTPException) -> JSONResponse:
    """Emit the relay error envelope so the app can decode 4xx/5xx responses.

    Matches the FastAPI relay's error shape (relay/app/main.py:270-306).
    """
    code = {
        400: "BAD_REQUEST", 401: "UNAUTHORIZED", 403: "FORBIDDEN",
        404: "NOT_FOUND", 409: "CONFLICT", 422: "VALIDATION_ERROR",
        429: "RATE_LIMITED", 500: "INTERNAL_ERROR",
        503: "SERVICE_UNAVAILABLE",
    }.get(exc.status_code, "ERROR")
    return JSONResponse(
        status_code=exc.status_code,
        content={
            "error": {
                "code": code,
                "message": str(exc.detail),
                "requestId": str(uuid.uuid4()),
                "timestamp": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            },
        },
    )


# ── Application ─────────────────────────────────────────────────────────


routes = [
    Route("/v1/health", health_endpoint, methods=["GET"]),
    Route("/health", health_alias, methods=["GET"]),
    Route("/v1/version", version_endpoint, methods=["GET"]),
    Route("/gw/version", version_endpoint, methods=["GET"]),
    Route("/v1/gw/version", version_endpoint, methods=["GET"]),
    Route("/v1/models", list_models, methods=["GET"]),
    Route("/v1/model", switch_model, methods=["POST"]),
    Route("/gw/model/switch", switch_model, methods=["POST"]),
    Route("/v1/gw/model/switch", switch_model, methods=["POST"]),
    Route("/v1/aux", aux_list, methods=["GET"]),
    Route("/v1/aux", aux_set, methods=["POST"]),
    Route("/aux", aux_list, methods=["GET"]),
    Route("/aux", aux_set, methods=["POST"]),
    Route("/v1/gw/aux", aux_list, methods=["GET"]),
    Route("/v1/gw/aux", aux_set, methods=["POST"]),
    Route("/v1/profiles", list_profiles, methods=["GET"]),
    Route("/v1/profile", switch_profile, methods=["POST"]),
    Route("/gw/profile/switch", switch_profile, methods=["POST"]),
    Route("/v1/gw/profile/switch", switch_profile, methods=["POST"]),
    Route("/v1/messages", send_message, methods=["POST"]),
    Route("/v1/session", get_session, methods=["GET"]),
    Route("/v1/commands", list_commands, methods=["GET"]),
    Route("/gw/restart", gateway_restart, methods=["POST"]),
    Route("/v1/gw/restart", gateway_restart, methods=["POST"]),
    Route("/gw/status", gateway_status, methods=["GET"]),
    Route("/v1/gw/status", gateway_status, methods=["GET"]),
    Route("/gw/logs", gateway_logs, methods=["GET"]),
    Route("/v1/gw/logs", gateway_logs, methods=["GET"]),
    Route("/gw/logs/stream", gateway_logs_stream, methods=["GET"]),
    Route("/v1/gw/logs/stream", gateway_logs_stream, methods=["GET"]),
    Route("/v1/capabilities", capabilities_endpoint, methods=["GET"]),
    # Pairing & Auth
    Route("/v1/connector/phone-pairing-codes", create_phone_pairing_code, methods=["POST"]),
    Route("/v1/phone-pairing/redeem", redeem_phone_pairing, methods=["POST"]),
    Route("/v1/pairing/redeem", redeem_pairing, methods=["POST"]),
    Route("/v1/auth/refresh", refresh_auth, methods=["POST"]),
    Route("/v1/auth/revoke", auth_revoke, methods=["POST"]),
    Route("/v1/device/register", register_device, methods=["POST"]),
    Route("/v1/connector/events", connector_events, methods=["GET"]),
    # B34 P0-3: Sessions — POST + GET, and search MUST precede {id}
    Route("/v1/sessions", create_session, methods=["POST"]),
    Route("/v1/sessions", get_sessions, methods=["GET"]),
    Route("/v1/sessions/search", session_search_handler, methods=["GET"]),
    Route("/v1/sessions/{id}/pin", session_pin, methods=["POST"]),
    Route("/v1/sessions/{id}/archive", session_archive, methods=["POST"]),
    Route("/v1/sessions/{id}/conversation", session_conversation, methods=["GET"]),
    Route("/v1/sessions/{id}/generate-title", session_generate_title, methods=["POST"]),
    Route("/v1/sessions/{id}", session_delete, methods=["DELETE"]),
    Route("/v1/sessions/{id}", session_patch, methods=["PATCH"]),
    Route("/v1/inbox", get_inbox, methods=["GET"]),
    Route("/v1/inbox/{id}/action", stub_inbox_action, methods=["POST"]),
    Route("/v1/push/register", push_register, methods=["POST"]),
    Route("/v1/push/deactivate", stub_push_deactivate, methods=["POST"]),
    Route("/v1/push-broker/challenge", stub_push_broker_challenge, methods=["POST"]),
    Route("/v1/push-broker/register", stub_push_broker_register, methods=["POST"]),
    Route("/v1/hosts/current", host_current, methods=["GET"]),
    Route("/v1/hosts/current/revoke", stub_hosts_current_revoke, methods=["POST"]),
    Route("/v1/hosts/enrollment-codes", host_enrollment_codes, methods=["POST"]),
    # Device telemetry
    Route("/v1/device/app-state", device_app_state, methods=["POST"]),
    Route("/v1/device/sensor/location", device_sensor, methods=["POST"]),
    Route("/v1/device/sensor/health", device_sensor, methods=["POST"]),
    # P0-4: chat critical path
    Route("/v1/conversations/current", current_conversation, methods=["GET"]),
    Route("/v1/conversations/current/clear", clear_current_conversation, methods=["POST"]),
    Route("/v1/jobs/{id}", job_status, methods=["GET"]),
    Route("/v1/jobs/{id}/events", job_events, methods=["GET"]),
    Route("/v1/jobs/{id}/cancel", cancel_job, methods=["POST"]),
    # B34 P2-1: Unimplemented-route stubs — decodable payloads, no 404s
    Route("/v1/skills", stub_skills, methods=["GET"]),
    Route("/v1/cron", stub_cron_list, methods=["GET"]),
    Route("/v1/cron/{id}", stub_cron_detail, methods=["GET", "DELETE"]),
    Route("/v1/notes", stub_notes_list, methods=["GET"]),
    Route("/v1/notes/{id}", stub_notes_detail, methods=["GET"]),
    Route("/v1/notes/{id}/recognitions", stub_notes_recognitions, methods=["GET"]),
    Route("/v1/notes/{id}/runs", stub_notes_runs, methods=["GET"]),
    Route("/v1/note-runs/{id}", stub_note_runs_detail, methods=["GET"]),
    Route("/v1/note-runs/{id}/cancel", stub_note_runs_cancel, methods=["POST"]),
    Route("/v1/note-runs/{id}/events", stub_note_runs_events, methods=["GET"]),
    Route("/v1/talk/readiness", stub_talk_readiness, methods=["GET"]),
    Route("/v1/talk/session", stub_talk_session, methods=["POST"]),
    Route("/v1/talk/session/{id}/end", stub_talk_session_end, methods=["POST"]),
    Route("/v1/talk/session/{id}/inject", stub_talk_session_inject, methods=["POST"]),
    Route("/v1/talk/session/{id}/turns", stub_talk_session_turns, methods=["GET"]),
    Route("/v1/gw/update", stub_gw_update, methods=["GET"]),
    Route("/v1/gw/update/check", stub_gw_update_check, methods=["POST"]),
    Route("/v1/relay/identity", stub_relay_identity, methods=["GET"]),
]

app = Starlette(
    debug=False,
    routes=routes,
    exception_handlers={HTTPException: http_exception_handler},
)


# ── ASGI middleware ─────────────────────────────────────────────────────


async def log_middleware(scope, receive, send):
    """Log every HTTP request.  Delegates to envelope_middleware → app."""
    if scope["type"] == "http":
        start = time.monotonic()
        path = scope.get("path", "")

        async def send_wrapper(message):
            if message["type"] == "http.response.start":
                elapsed = time.monotonic() - start
                logger.info(
                    "%s %s → %d (%.0fms)",
                    scope.get("method", "?"), path,
                    message.get("status", 0), elapsed * 1000,
                )
            await send(message)

        await envelope_middleware(scope, receive, send_wrapper)
    else:
        await envelope_middleware(scope, receive, send)


# ── Wiring ──────────────────────────────────────────────────────────────


def create_app(
    *,
    model_catalog: ModelCatalogProvider | None = None,
    model_switch: ModelSwitchProvider | None = None,
    profile_catalog: ProfileCatalogProvider | None = None,
    profile_switch: ProfileSwitchProvider | None = None,
    message_handler: MessageHandler | None = None,
    connector_version: str = "0.0.0",
    health_check: Callable[[], Coroutine[Any, Any, bool]] | None = None,
    job_status: JobStatusProvider | None = None,
    job_cancel: JobCancelProvider | None = None,
    job_events: JobEventsProvider | None = None,
    session_conversation: SessionConversationProvider | None = None,
    current_conversation: CurrentConversationProvider | None = None,
    clear_conversation: ClearConversationProvider | None = None,
) -> Starlette:
    """Wire the facade context with connector callbacks."""
    ctx = get_context()
    ctx.model_catalog = model_catalog
    ctx.model_switch = model_switch
    ctx.profile_catalog = profile_catalog
    ctx.profile_switch = profile_switch
    ctx.message_handler = message_handler
    ctx.connector_version = connector_version
    ctx.health_check = health_check
    ctx.job_status = job_status
    ctx.job_cancel = job_cancel
    ctx.job_events = job_events
    ctx.session_conversation = session_conversation
    ctx.current_conversation = current_conversation
    ctx.clear_conversation = clear_conversation
    return app


async def serve(host: str = "0.0.0.0", port: int = 8010) -> None:
    """Start the HTTP facade server (blocking)."""
    import uvicorn

    config = uvicorn.Config(
        log_middleware,
        host=host,
        port=port,
        log_level="info",
        access_log=False,
    )
    server = uvicorn.Server(config)
    await server.serve()
