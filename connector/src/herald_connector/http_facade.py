"""HTTP/SSE facade for the iOS Herald app.

Runs inside the connector process using Starlette (no FastAPI dependency).
Serves the same API as the Docker relay. Starlette + uvicorn + sse-starlette
are already installed in the connector's Python environment.

The iOS app talks HTTP/SSE to this server; the gateway talks native relay
WebSocket to HeraldRelayServer on :8765.  This module is the HTTP half.
"""

from __future__ import annotations

import asyncio
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


ModelCatalogProvider = Callable[[], Coroutine[Any, Any, dict]]
ModelSwitchProvider = Callable[[str, str | None], Coroutine[Any, Any, dict]]
ProfileCatalogProvider = Callable[[], Coroutine[Any, Any, dict]]
ProfileSwitchProvider = Callable[[str], Coroutine[Any, Any, dict]]
MessageHandler = Callable[
    [str, list[dict], str | None, list[dict] | None, str | None],
    Coroutine[Any, Any, AsyncIterator[dict]],
]


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


_context = FacadeContext()


def get_context() -> FacadeContext:
    return _context


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
    body = await request.json()
    name = body.get("name") or body.get("model", "")
    provider = body.get("provider")
    if not name:
        raise HTTPException(status_code=400, detail="name is required")
    result = ctx.model_switch(name, provider)
    if inspect.isawaitable(result):
        result = await result
    return JSONResponse(result)


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
    body = await request.json()
    name = body.get("name", "")
    if not name:
        raise HTTPException(status_code=400, detail="name is required")
    result = ctx.profile_switch(name)
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


async def send_message(request: Request) -> StreamingResponse:
    """Send a message and stream the response via SSE."""
    await require_auth(request)
    ctx = get_context()
    if ctx.message_handler is None:
        raise HTTPException(status_code=503, detail="Message handler not available")

    body = await request.json()
    text = body.get("text", "")
    history = body.get("history") or []
    session_id = body.get("sessionId")
    attachments = body.get("attachments")
    reasoning_effort = body.get("reasoningEffort")

    async def event_stream() -> AsyncIterator[str]:
        job_id = str(uuid.uuid4())
        seq = 0
        yield f"id: {seq}\nevent: messageSent\ndata: {json.dumps({'jobId': job_id})}\n\n"
        seq += 1

        try:
            async for event in ctx.message_handler(
                text, history, session_id, attachments, reasoning_effort
            ):
                if await request.is_disconnected():
                    break
                event_type = event.get("type", "progress")
                sse_data = json.dumps(event.get("data", event))
                yield f"id: {seq}\nevent: {event_type}\ndata: {sse_data}\n\n"
                seq += 1
        except asyncio.CancelledError:
            yield f"id: {seq}\nevent: cancelled\ndata: {json.dumps({'jobId': job_id})}\n\n"
        except Exception as exc:
            logger.exception("Message streaming error")
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
    """Redeem a phone pairing code. Returns access + refresh tokens."""
    import time as _time
    ctx = get_context()
    body = await request.json()
    logger.info("Redeem body keys: %s, code raw: %s", list(body.keys()), body.get("code","?")[:20])
    code = (body.get("code") or "").upper().replace("-", "").replace(" ", "")
    logger.info("Redeem normalized code: %s", code)
    hashed = _hash_code(code)
    stored = _pending_pairing_codes.pop(hashed, None)
    if stored is None or stored["expires_at"] < _time.time():
        raise HTTPException(status_code=401, detail="Invalid or expired pairing code")
    # The connector credential IS the access token
    token = ctx.connector_credential or ctx.paired_device_id or "herald-connector"
    _default_validator.add_token(token)
    import uuid as _uuid
    return JSONResponse({
        "user": {"id": ctx.paired_user_id or str(_uuid.uuid4()), "displayName": "Herald User"},
        "deviceId": str(_uuid.uuid4()),
        "deviceRegistered": True,
        "session": {"connectionStatus": "connected", "isMockMode": False, "backendEndpoint": ctx.paired_device_id or ctx.public_base_url, "lastSyncAt": None},
        "auth": {"accessToken": token, "refreshToken": token, "expiresAt": datetime.datetime.now(datetime.timezone.utc).isoformat()},
    })


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
    """Return session list (stub)."""
    await require_auth(request)
    return JSONResponse({"sessions": []})


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
    """Return current host info."""
    await require_auth(request)
    ctx = get_context()
    return JSONResponse({"id": ctx.paired_device_id or "host", "displayName": "Herald Host", "isOnline": True})


async def host_enrollment_codes(request: Request) -> JSONResponse:
    """Create a host enrollment code."""
    await require_auth(request)
    code, display = _generate_pairing_code()
    return JSONResponse({"code": code, "displayCode": display})


# ── Application ─────────────────────────────────────────────────────────


routes = [
    Route("/v1/health", health_endpoint, methods=["GET"]),
    Route("/health", health_alias, methods=["GET"]),
    Route("/v1/version", version_endpoint, methods=["GET"]),
    Route("/v1/models", list_models, methods=["GET"]),
    Route("/v1/model", switch_model, methods=["POST"]),
    Route("/v1/profiles", list_profiles, methods=["GET"]),
    Route("/v1/profile", switch_profile, methods=["POST"]),
    Route("/v1/messages", send_message, methods=["POST"]),
    Route("/v1/session", get_session, methods=["GET"]),
    Route("/v1/commands", list_commands, methods=["GET"]),
    Route("/v1/capabilities", capabilities_endpoint, methods=["GET"]),
    # Pairing & Auth
    Route("/v1/connector/phone-pairing-codes", create_phone_pairing_code, methods=["POST"]),
    Route("/v1/phone-pairing/redeem", redeem_phone_pairing, methods=["POST"]),
    Route("/v1/pairing/redeem", redeem_pairing, methods=["POST"]),
    Route("/v1/auth/refresh", refresh_auth, methods=["POST"]),
    Route("/v1/auth/revoke", auth_revoke, methods=["POST"]),
    Route("/v1/device/register", register_device, methods=["POST"]),
    Route("/v1/connector/events", connector_events, methods=["GET"]),
    Route("/v1/sessions", get_sessions, methods=["GET"]),
    Route("/v1/inbox", get_inbox, methods=["GET"]),
    Route("/v1/push/register", push_register, methods=["POST"]),
    Route("/v1/hosts/current", host_current, methods=["GET"]),
    Route("/v1/hosts/enrollment-codes", host_enrollment_codes, methods=["POST"]),
]

app = Starlette(debug=False, routes=routes)


# ── ASGI middleware ─────────────────────────────────────────────────────


async def log_middleware(scope, receive, send):
    """Log every HTTP request."""
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

        await app(scope, receive, send_wrapper)
    else:
        await app(scope, receive, send)


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
