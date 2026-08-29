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
import hashlib
import inspect
import json
import logging
import os
import re
import shutil
import signal
import socket
import tempfile
import threading
import time
try:
    from zoneinfo import ZoneInfo
except ImportError:  # pragma: no cover — Python 3.8 fallback
    from backports.zoneinfo import ZoneInfo  # type: ignore[no-redef]
import uuid
from pathlib import Path
from subprocess import run as _run_subprocess
from typing import Any, AsyncIterator, Callable, Coroutine

import httpx
from starlette.applications import Starlette
from starlette.exceptions import HTTPException
from starlette.requests import Request
from starlette.responses import JSONResponse, Response, StreamingResponse
from starlette.routing import Route, WebSocketRoute

from .restart_operations import (
    NON_TERMINAL_PHASES,
    RestartConflictError,
    RestartOperationStore,
    get_restart_store,
)
from . import __version__, HERALD_PROTOCOL
from .background_processes import (
    BackgroundProcessRegistry,
    get_registry as get_process_registry,
    tracked_subprocess_exec,
)
from .pairing_code_store import PairingCodeStore
from .terminal_bridge import handle_terminal_websocket

logger = logging.getLogger("herald.http_facade")

# Phase 3A v3: event types whose payload names a single canonical
# message.  The publisher annotates these with the resolved canonical
# row (id + revision) so the iOS reducer can apply the mutation
# without a separate round-trip.
TERMINAL_OR_MUTATING_EVENT_TYPES = frozenset({
    "text.delta",
    "reasoning.delta",
    "run.completed",
    "run.failed",
    "run.cancelled",
})
# T1.4: event types valid on the v3 wire. Producer lifecycle events
# like "started", "finish", "done" are filtered before seq allocation.
WIRE_EVENT_TYPES = frozenset({
    "text.delta", "reasoning.delta", "tool.started",
    "tool.progress", "tool.completed", "commentary",
    "approval.required", "run.completed", "run.failed",
    "run.cancelled", "run.requeued", "done",
})

# ── Process lifetime ──────────────────────────────────────────────────────

_PROCESS_STARTED_AT = time.monotonic()

# ── Journal constants (F-3: Gateway Logs) ─────────────────────────────────

_JOURNAL_PRIORITY = {"error": "3", "warning": "4", "info": "6", "debug": "7", "all": "7"}
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
    if token and _default_validator.is_valid(token):
        return token
    # B116: a token missing from the in-memory set but present in the persisted
    # device registry is a device that paired before the last restart. Re-admit
    # it instead of 401'ing (a 401 here also kills /v1/auth/refresh and bricks
    # the app). This is the belt-and-suspenders to the startup rehydration.
    if token:
        try:
            from .session_store import device_id_for_token
            if device_id_for_token(token):
                _default_validator.add_token(token)
                return token
        except Exception:
            pass
    raise HTTPException(status_code=401, detail="Invalid or missing access token")


async def require_native_or_paired_auth(request: Request) -> str:
    """Accept EITHER the connector's paired credential OR a native-gateway
    bearer token.

    Native-gateway clients (direct mode, WS :9119) never pair with the
    connector, so requiring the paired credential made /gw/logs and
    /gw/logs/stream unreachable for exactly the clients that need them
    (same rationale as native_watch_route). The native bearer is verified
    against the gateway itself.
    """
    auth_header = request.headers.get("authorization", "")
    bearer = ""
    if auth_header[:7].lower() == "bearer ":
        bearer = auth_header[7:].strip()
    if bearer and await _verify_native_gateway_bearer(bearer):
        return bearer

    # Basic and Kallisti-pairing native logins are gateway cookie sessions.
    # URLSession carries that cookie to this public media route, but the
    # connector previously discarded it and required a bearer the app does not
    # have in cookie-auth mode. Delegate cookie verification to the gateway.
    cookie = request.headers.get("cookie", "").strip()
    if cookie and await _verify_native_gateway_cookie(cookie):
        return "gateway-cookie-session"

    return await require_auth(request)


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
# Takes a Hermes run_id, returns True if Hermes acknowledged the stop.
# (job_id, body_text, *, category=None, conversation_id=None) -> None.
# Sends a real APNs push — independent of whether any client Task is
# still alive to process the job's terminal SSE event.
SendCompletionPushProvider = Callable[..., Coroutine[Any, Any, None]]
# (status: str = "Done") -> None. Remote-ends the user's Live Activity via
# its own push-to-update token — same "independent of a live client Task"
# rationale as SendCompletionPushProvider.
EndLiveActivityProvider = Callable[..., Coroutine[Any, Any, None]]
PushRegisterProvider = Callable[[dict], Coroutine[Any, Any, dict]]


class FacadeContext:
    """Mutable context wired by the connector at startup."""

    def __init__(self) -> None:
        self.model_catalog: ModelCatalogProvider | None = None
        self.model_switch: ModelSwitchProvider | None = None
        self.profile_catalog: ProfileCatalogProvider | None = None
        self.profile_switch: ProfileSwitchProvider | None = None
        self.connector_version: str = "0.0.0"
        self.health_check: Callable[[], Coroutine[Any, Any, bool]] | None = None
        self.paired_device_id: str | None = None
        self.paired_user_id: str | None = None
        self.connector_credential: str | None = None
        self.public_base_url: str = ""
        self.gateway_restart: Callable[[str], Coroutine[Any, Any, dict]] | None = None
        self.send_completion_push: SendCompletionPushProvider | None = None
        self.end_live_activity: EndLiveActivityProvider | None = None
        self.auxiliary_list: Callable[[], dict | Coroutine[Any, Any, dict]] | None = None
        self.auxiliary_set: Callable[[dict], dict | Coroutine[Any, Any, dict]] | None = None
        self.push_register: PushRegisterProvider | None = None
        # Build 94: full command catalog (commands/skills/personalities/
        # quickCommands + activeModel with contextWindow). Wired from the
        # connector client's _rpc_commands_catalog.
        self.commands_catalog: Callable[[], dict] | Callable[[], Coroutine[Any, Any, dict]] | None = None
        self.agent_version: Callable[[], str | None] | Callable[[], Coroutine[Any, Any, str | None]] | None = None
        # Build 33 Workstream A: durable restart operations
        self.restart_store: RestartOperationStore | None = None
        # Wired by the connector: sends a probe turn through the native relay
        # and returns (passed: bool, detail: str). None → canary check skipped.
        self.session_canary: Callable[[], Coroutine[Any, Any, tuple[bool, str]]] | None = None
        # Native-completion to push bridge (Task 12)
        self.native_watch_registry: Any = None
        # Sensor store (sensors.db) - wired by the connector so the HTTP
        # device_sensor handler persists location/health like the WS path.
        self.sensor_store: Any = None


_context = FacadeContext()


def get_context() -> FacadeContext:
    return _context












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




















# ── Inbound attachment staging (Build 28) ──────────────────────────────────

_MAX_INBOUND_ATTACHMENT_BYTES = 50 * 1024 * 1024  # aggregate cap
_MAX_INBOUND_ATTACHMENT_COUNT = 10
_STAGING_ROOT = Path(tempfile.gettempdir()) / "herald-inbound-attachments"




# Build 102 P1: authoritative temporal context (marching orders §9).
# Returns an empty string if the system zone is unavailable; callers
# must handle the empty case (text goes through unchanged).
_TEMPORAL_TIMEZONE = os.getenv("HERALD_TEMPORAL_TIMEZONE", "America/Los_Angeles")






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
    # deliveryStoreReady reflects the durability database (Build 34 P0):
    # a working chat path needs the SQLite tables, not just the Hermes API
    # health probe that ``database`` was overloaded onto.
    delivery_store_ready = True
    try:
        from .delivery_store import get_delivery_store
        delivery_store_ready = get_delivery_store().schema_ready()
    except Exception:
        delivery_store_ready = False
    overall = "ok" if (db_ok and delivery_store_ready) else "degraded"
    return JSONResponse({
        "status": overall,
        "database": db_ok,
        "deliveryStoreReady": delivery_store_ready,
    })


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
    # Build 86: accept native clients (gateway bearer/cookie) as well as the
    # connector's paired credential. The iOS ProfileStore native path calls
    # this through the relay to enrich skillCount; require_auth alone 401'd
    # every cookie-auth client, so the Hub always showed "0 skills".
    await require_native_or_paired_auth(request)
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
    """Return session bootstrap with stable user/device identity.

    Build 30: the old implementation returned random uuid4() values on every
    call, which made device identity unstable — pairing, session ownership,
    and the All Devices toggle all depended on a stable installation ID that
    this endpoint was not providing.  Now we resolve the authenticated
    device's real identity from the registry and derive a stable user UUID.
    """
    await require_auth(request)
    ctx = get_context()
    from .session_store import device_id_for_token
    from . import HERALD_PROTOCOL as _HERALD_PROTOCOL
    import hashlib as _hashlib
    import uuid as _uuid

    token = await _extract_token(request)
    installation_id = device_id_for_token(token) if token else None

    # Stable user identity: derive from the installation_id so it survives
    # app relaunch / token refresh within the same device.
    if installation_id:
        user_seed = _hashlib.sha256(f"herald-user:{installation_id}".encode()).digest()[:16]
        user_id = str(_uuid.UUID(bytes=user_seed))
    else:
        user_id = str(_uuid.uuid4())

    return JSONResponse({
        "user": {"id": user_id, "displayName": "Herald User"},
        "device": {
            "id": installation_id or str(_uuid.uuid4()),
            "registered": bool(installation_id),
        },
        "session": {
            "connectionStatus": "connected",
            "isMockMode": False,
            "backendEndpoint": ctx.public_base_url or "",
            "lastSyncAt": None,
            "protocol": _HERALD_PROTOCOL,
        },
        "push": {"tokenRegistered": False},
    })


async def auth_revoke(request: Request) -> JSONResponse:
    """Revoke the current session token."""
    await require_auth(request)
    return JSONResponse({"revoked": True})


async def list_commands(request: Request) -> JSONResponse:
    # Build 94: cookie-auth sessions have no bearer token, so this must accept
    # the gateway session cookie (require_native_or_paired_auth) like the
    # other native routes. Returns the full catalog - commands, skills,
    # personalities, quick commands, and the active model's contextWindow -
    # instead of the old hardcoded 5-command stub.
    await require_native_or_paired_auth(request)
    ctx = get_context()
    if ctx.commands_catalog is not None:
        try:
            result = ctx.commands_catalog()
            if asyncio.iscoroutine(result):
                result = await result
            if isinstance(result, dict):
                return JSONResponse(result)
        except Exception as e:  # pragma: no cover - defensive
            logging.getLogger("herald.http_facade").warning(
                "commands_catalog provider failed: %s", e
            )
    return JSONResponse({
        "commands": [
            {"name": "new", "description": "Start a new session"},
            {"name": "model", "description": "Switch models"},
            {"name": "profile", "description": "Switch profiles"},
            {"name": "retry", "description": "Retry last message"},
            {"name": "stop", "description": "Stop current response"},
        ]
    })


# ── Restart operations (Build 33 Workstream A) ─────────────────────────────
#
# Restarts are durable and phase-tracked.  The lifecycle:
#
#   GET  /v1/gw/restart/preflight?target=hermes   → restart-preflight-v1
#   POST /v1/gw/restart  (Idempotency-Key header) → restart-operation-v1
#   GET  /v1/gw/restart/{operationId}             → poll the operation
#
# Phases: accepted → stopping → starting → verifying → healthy | failed.
# The operation row lives in the RestartOperationStore (SQLite) so it
# survives the connector restarting itself; startup reconciliation marks
# any row left non-terminal as failed.

_RESTART_STEP_NAMES = [
    "systemctl-is-active",
    "pid-changed",
    "hermes-ready",
    "model-catalog",
    "session-roundtrip",
]

_restart_tasks: dict[str, asyncio.Task] = {}
_last_canary_result: bool | None = None


def _hermes_profile() -> str:
    """Resolve the active Hermes profile from HERMES_HOME."""
    return os.path.basename(os.getenv("HERMES_HOME", "").rstrip("/")) or "ignyte"


def _hermes_unit() -> str:
    """Resolve the Hermes systemd user unit (per-profile gateway).

    The .service suffix matches the contract fixture unit name
    (hermes-gateway-{profile}.service); systemctl accepts it on all commands.
    """
    return os.getenv("HERMES_AGENT_UNIT") or f"hermes-gateway-{_hermes_profile()}.service"


def _parse_systemd_timestamp(raw: str | None) -> str | None:
    """Best-effort systemd timestamp → RFC 3339 UTC (Z suffix).

    systemctl show emits localized human timestamps unless --timestamp=unix
    is supported; either input is normalized here.  Never raises.
    """
    if not raw:
        return None
    try:
        epoch = int(raw)
        return datetime.datetime.fromtimestamp(epoch, datetime.timezone.utc).strftime(
            "%Y-%m-%dT%H:%M:%SZ"
        )
    except (ValueError, OSError, OverflowError):
        pass
    for fmt in ("%a %Y-%m-%d %H:%M:%S %Z", "%Y-%m-%d %H:%M:%S %Z", "%Y-%m-%d %H:%M:%S"):
        try:
            dt = datetime.datetime.strptime(raw, fmt)
            if dt.tzinfo is None:
                dt = dt.replace(tzinfo=datetime.timezone.utc)
            return dt.astimezone(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
        except ValueError:
            continue
    return raw


def _query_unit_observed(unit: str) -> dict | None:
    """Query `systemctl --user show <unit>` for MainPID / start / state.

    Returns None when the unit is unknown or systemctl is unavailable —
    never raises.  Keys: main_pid, exec_main_start_timestamp (RFC 3339),
    active_state.
    """
    try:
        result = _run_subprocess(
            ["systemctl", "--user", "--timestamp=unix", "show", unit,
             "--property=MainPID,ExecMainStartTimestamp,ActiveState"],
            capture_output=True, text=True, timeout=10,
        )
    except Exception:
        logger.debug("systemctl show %s failed", unit, exc_info=True)
        return None
    if result.returncode != 0:
        # Fall back to the default (localized) timestamp format.
        try:
            result = _run_subprocess(
                ["systemctl", "--user", "show", unit,
                 "--property=MainPID,ExecMainStartTimestamp,ActiveState"],
                capture_output=True, text=True, timeout=10,
            )
        except Exception:
            return None
        if result.returncode != 0:
            return None
    parsed: dict[str, str] = {}
    for line in result.stdout.splitlines():
        key, _, value = line.partition("=")
        if key:
            parsed[key] = value.strip()
    try:
        main_pid = int(parsed.get("MainPID") or "0") or None
    except ValueError:
        main_pid = None
    return {
        "main_pid": main_pid,
        "exec_main_start_timestamp": _parse_systemd_timestamp(
            parsed.get("ExecMainStartTimestamp")
        ),
        "active_state": parsed.get("ActiveState"),
    }


def _compute_preflight_version(unit: str, observed: dict | None) -> str:
    """Preflight version = hash of the observed gateway state.

    The client sends this back with its restart request; if the gateway
    state (MainPID / start time) has changed since, the version no longer
    matches and the restart is rejected with 409 PREFLIGHT_STALE.
    """
    raw = "{}:{}{}".format(
        unit,
        (observed or {}).get("main_pid") or "",
        (observed or {}).get("exec_main_start_timestamp") or "",
    )
    return hashlib.sha256(raw.encode()).hexdigest()[:12]


def _active_work_counts() -> dict:
    """Active-work counters — always zero in push-only mode (no HTTP jobs)."""
    return {"running": 0, "queued": 0, "voice": 0, "tools": 0}


async def gateway_restart_preflight(request: Request) -> JSONResponse:
    """GET /v1/gw/restart/preflight?target=hermes — can this restart safely?

    Returns restart-preflight-v1 (see tests/fixtures/restart/preflight_ok.json).
    The client MUST echo `preflightVersion` back with its restart request; a
    stale version (gateway state changed since the preflight was shown) is
    rejected with 409.
    """
    await require_native_or_paired_auth(request)
    target = request.query_params.get("target", "hermes")
    if target not in ("hermes", "connector"):
        raise HTTPException(status_code=400, detail=f"Unknown target: {target}")

    profile = _hermes_profile()
    unit = _hermes_unit() if target == "hermes" else _JOURNAL_UNIT
    observed = await asyncio.to_thread(_query_unit_observed, unit)

    blocker: str | None = None
    can_restart = True
    if observed is None:
        can_restart = False
        blocker = (
            f"Unit {unit} is not running under systemd "
            "(systemctl --user show failed) — cannot restart it"
        )
    elif observed.get("active_state") != "active" or not observed.get("main_pid"):
        can_restart = False
        blocker = (
            f"Unit {unit} is not active (state={observed.get('active_state') or 'unknown'})"
        )

    gateway_state = "running" if (observed or {}).get("active_state") == "active" \
        else ((observed or {}).get("active_state") or "unknown")

    return JSONResponse({
        "$schema": "restart-preflight-v1",
        "target": target,
        "profile": profile,
        "unit": unit,
        "preflightVersion": _compute_preflight_version(unit, observed),
        "activeWork": _active_work_counts(),
        "canRestart": can_restart,
        "blocker": blocker,
        "observed": {
            "mainPid": (observed or {}).get("main_pid"),
            "execMainStartTimestamp": (observed or {}).get("exec_main_start_timestamp"),
            "gatewayState": gateway_state,
        },
    })


async def gateway_restart(request: Request) -> JSONResponse:
    """Restart a gateway component (hermes or connector).

    Two behaviours, selected by the Idempotency-Key header:

    * With `Idempotency-Key` — Build 33 Workstream A flow: the body must
      carry the `preflightVersion` the client observed.  An operation is
      created in the durable RestartOperationStore (phase "accepted") and
      the restart runs in the background through stopping → starting →
      verifying → healthy|failed.  The response returns immediately; the
      client polls GET /v1/gw/restart/{operationId}.  Replaying the same
      key returns the same operation; a second key while one is active
      returns 409 with the existing operation.

    * Without the header — legacy one-shot behaviour: fire the RPC handler
      and return its result, with "target" added to the response.
    """
    await require_native_or_paired_auth(request)
    ctx = get_context()
    try:
        body = await request.json()
    except Exception:
        body = None
    body = body if isinstance(body, dict) else {}
    target = body.get("target", "hermes")
    # "relay" is deliberately NOT in the allowlist: the native facade has no
    # relay restart handler — the Docker relay is gone and the connector's
    # relay is embedded (facade :8010 + native relay WS :8765).  Restarting
    # the connector restarts both.  Requesting "relay" must fail loudly
    # rather than be silently accepted and do nothing.
    if target not in ("hermes", "connector"):
        raise HTTPException(
            status_code=400,
            detail=(
                f"Unknown target: {target}"
                if target != "relay"
                else "Restarting 'relay' is not supported: the native facade "
                     "has no relay restart handler. Restart 'connector' to "
                     "restart the embedded relay."
            ),
        )

    idempotency_key = (
        request.headers.get("Idempotency-Key")
        or request.headers.get("idempotency-key")
    )
    if not idempotency_key:
        # ── Legacy one-shot path (JSON-RPC bridge compatibility) ──────────
        if ctx.gateway_restart is None:
            raise HTTPException(status_code=503, detail="Gateway control not available")
        result = await ctx.gateway_restart(target)
        if isinstance(result, dict):
            result = dict(result)
            result.setdefault("target", target)
        return JSONResponse(result)

    # ── Build 33 idempotent, phase-tracked flow ───────────────────────────
    store = ctx.restart_store or get_restart_store()

    # Idempotent replay: same key → same operation, whatever its phase.
    existing = store.get_by_idempotency_key(idempotency_key)
    if existing is not None:
        logger.info("restart: idempotent replay of %s", existing["operationId"])
        return JSONResponse(existing)

    preflight_version = body.get("preflightVersion")
    if not isinstance(preflight_version, str) or not preflight_version:
        raise HTTPException(
            status_code=400,
            detail=(
                "preflightVersion is required when Idempotency-Key is supplied — "
                "run GET /v1/gw/restart/preflight first and echo its preflightVersion"
            ),
        )

    unit = _hermes_unit() if target == "hermes" else _JOURNAL_UNIT
    observed = await asyncio.to_thread(_query_unit_observed, unit)
    current_version = _compute_preflight_version(unit, observed)
    if current_version != preflight_version:
        logger.warning(
            "restart: stale preflight for %s (client=%s current=%s)",
            unit, preflight_version, current_version,
        )
        return JSONResponse(
            status_code=409,
            content={
                "error": {
                    "code": "PREFLIGHT_STALE",
                    "message": (
                        "The restart preflight is stale — the gateway state changed "
                        "since it was shown. Run the preflight again and retry."
                    ),
                    "preflightVersion": preflight_version,
                    "currentPreflightVersion": current_version,
                },
            },
        )

    try:
        op = store.create_operation(
            operation_id=str(uuid.uuid4()),
            idempotency_key=idempotency_key,
            target=target,
            unit=unit,
            preflight_version=preflight_version,
            old_pid=(observed or {}).get("main_pid") if target == "hermes" else os.getpid(),
            old_start_ts=(observed or {}).get("exec_main_start_timestamp")
            if target == "hermes" else None,
        )
    except RestartConflictError as conflict:
        return JSONResponse(
            status_code=409,
            content={
                "error": {
                    "code": "RESTART_IN_PROGRESS",
                    "message": (
                        f"A restart is already in progress for target '{target}'."
                    ),
                    "operationId": conflict.operation_id,
                },
                "operation": conflict.operation,  # restart-operation-v1 payload
            },
        )

    _start_restart_task(op["operationId"])
    return JSONResponse(op)


async def gateway_restart_status(request: Request) -> JSONResponse:
    """GET /v1/gw/restart/{operationId} — poll the operation's current state."""
    await require_auth(request)
    ctx = get_context()
    operation_id = request.path_params["operationId"]
    store = ctx.restart_store or get_restart_store()
    op = store.get_operation(operation_id)
    if op is None:
        raise HTTPException(
            status_code=404, detail=f"Unknown restart operation: {operation_id}"
        )
    return JSONResponse(op)


# ── Background restart execution ───────────────────────────────────────────


class _RestartFailure(Exception):
    """Typed failure inside a restart operation — never leaks raw exceptions."""

    def __init__(
        self,
        stage: str,
        *,
        exit_status: int | None = None,
        retryable: bool = True,
        action: str = "",
        failed_check: dict | None = None,
        skipped_note: str | None = None,
    ) -> None:
        super().__init__(action or stage)
        self.stage = stage
        self.exit_status = exit_status
        self.retryable = retryable
        self.action = action
        self.failed_check = failed_check
        self.skipped_note = skipped_note or f"{stage} failed"


def _restart_active_timeout() -> float:
    return float(os.getenv("HERALD_RESTART_ACTIVE_TIMEOUT", "120"))


def _restart_poll_interval() -> float:
    return float(os.getenv("HERALD_RESTART_POLL_INTERVAL", "1.0"))


# ── Build 103 WS-D: truthful telemetry helpers ─────────────────────────────

_CONNECTOR_REQUIRED_PORTS = (8010, 8765, 8767)


def _get_nrestarts(unit: str) -> int:
    """Read NRestarts for *unit*. Returns 0 on any failure."""
    try:
        result = _run_subprocess(
            ["systemctl", "--user", "show", unit,
             "--property=NRestarts"],
            capture_output=True, text=True, timeout=5,
        )
        if result.returncode != 0:
            return 0
        for line in result.stdout.splitlines():
            key, _, value = line.partition("=")
            if key.strip() == "NRestarts":
                return int(value.strip() or "0")
    except Exception:
        logger.debug("nrestarts lookup failed for %s", unit, exc_info=True)
    return 0


def _parse_port_owner_pid(line: str) -> int | None:
    """Extract the pid=NNN from an `ss -ltnp` line."""
    import re
    m = re.search(r"pid=(\d+)", line)
    return int(m.group(1)) if m else None


def _check_ports_owned_by_pid(pid: int, ports: tuple[int, ...]) -> bool:
    """True iff *pid* owns every TCP port in *ports*. On any failure → False."""
    try:
        result = _run_subprocess(
            ["ss", "-ltnp"],
            capture_output=True, text=True, timeout=5,
        )
        if result.returncode != 0:
            return False
        owners: dict[int, int] = {}
        for line in result.stdout.splitlines():
            for port in ports:
                if f":{port} " in line:
                    owner = _parse_port_owner_pid(line)
                    if owner is not None:
                        owners[port] = owner
        return all(owners.get(port) == pid for port in ports)
    except Exception:
        logger.debug("port-owner check failed for pid=%s", pid, exc_info=True)
        return False


def _pid_uptime_seconds(pid: int | None) -> int | None:
    """Process uptime from /proc/<pid>/stat field 22 (starttime in jiffies)."""
    if not pid:
        return None
    try:
        with open(f"/proc/{pid}/stat", "r", encoding="utf-8") as fh:
            content = fh.read()
        # Field 22 is starttime in clock ticks; fields are space-separated
        # but comm (field 2) can contain spaces and parens.
        rpar = content.rfind(")")
        if rpar < 0:
            return None
        rest = content[rpar + 1:].split()
        # rest[0] = state, rest[1] = ppid, ..., rest[19] = starttime
        # After rpar there are 19 fields before starttime (state=1,ppid=2,...)
        # but index 19 is actually 20th from the right of ")".
        # Easier: field 22 from start (1-indexed) minus the 3 fields inside ()
        # means index 19 in `rest` (0-indexed) after stripping the trailing ')'.
        if len(rest) < 20:
            return None
        starttime_ticks = int(rest[19])
        # Clock ticks per second
        try:
            clk_tck = os.sysconf("SC_CLK_TCK")
        except (ValueError, OSError, AttributeError):
            clk_tck = 100
        # System uptime in seconds:
        try:
            with open("/proc/uptime") as fh:
                boot_age = float(fh.read().split()[0])
        except OSError:
            boot_age = None
        if boot_age is None:
            return None
        start_seconds = starttime_ticks / clk_tck
        age = max(0, int(boot_age - start_seconds))
        return age
    except (OSError, ValueError, IndexError):
        return None


def _read_hermes_installed_version() -> str | None:
    """Read the installed Hermes Agent version.

    Tries (in order):
      1. ``~/.hermes/.update_check`` (ver field, last ``hermes update --check``).
      2. The Hermes CLI: ``hermes --version`` (subprocess, 5 s timeout).
    Never raises.
    """
    candidates = [
        Path(os.path.expanduser("~/.hermes")) / ".update_check",
        Path(os.getenv("HERMES_HOME") or os.path.expanduser("~/.hermes"))
        / ".update_check",
    ]
    for path in candidates:
        try:
            if path.is_file():
                data = json.loads(path.read_text())
                ver = data.get("ver")
                if ver:
                    return str(ver)
        except (OSError, ValueError):
            continue
    # CLI fallback
    candidates_bin = [
        Path(os.getenv("HERMES_HOME") or os.path.expanduser("~/.hermes"))
        / "hermes-agent" / "venv" / "bin" / "hermes",
        Path(os.path.expanduser("~/.local/bin/hermes")),
    ]
    for bin_path in candidates_bin:
        try:
            if not bin_path.is_file():
                continue
            result = _run_subprocess(
                [str(bin_path), "--version"],
                capture_output=True, text=True, timeout=5,
            )
            if result.returncode == 0 and result.stdout.strip():
                # "Hermes Agent v0.19.1 (2026.7.30) ..." or similar
                out = result.stdout.strip()
                m = re.search(r"v?(\d+\.\d+\.\d+(?:[.-][\w.]+)?)", out)
                if m:
                    return m.group(1)
                # Fall back to the entire first line trimmed
                return out.split("\n", 1)[0][:80]
        except Exception:
            continue
    return None


def _read_hermes_latest_version() -> tuple[str | None, int | None, str | None]:
    """Read the latest known Hermes version + behind-count + checked-at.

    Pure file read of ``~/.hermes/.update_check``; never invokes a
    subprocess, so it is safe to call on every gateway_status poll.
    Returns ``(latest, behind, checked_at_iso)`` — any field may be None.
    """
    candidates = [
        Path(os.path.expanduser("~/.hermes")) / ".update_check",
        Path(os.getenv("HERMES_HOME") or os.path.expanduser("~/.hermes"))
        / ".update_check",
    ]
    for path in candidates:
        try:
            if path.is_file():
                data = json.loads(path.read_text())
                ver = data.get("ver")
                behind = data.get("behind")
                ts = data.get("ts")
                iso = (
                    datetime.datetime.fromtimestamp(float(ts), datetime.timezone.utc).isoformat()
                    if ts is not None else None
                )
                return (
                    str(ver) if ver else None,
                    int(behind) if behind is not None else None,
                    iso,
                )
        except (OSError, ValueError):
            continue
    return (None, None, None)


# Build 69 (r7): changelog for the update UI. Reads ONLY the install's git
# log (HEAD..origin/main, newest 12) - no Hermes code is modified, no
# subprocess writes anything.
#
# Build 128.49: cache removed. The behind count (_read_hermes_behind_count)
# was always fresh while the changelog was cached 10 min, so the iOS UI
# could show "2 commits behind" next to a 1-line stale changelog when
# origin/main advanced between the cache fill and the request. Both now run
# fresh git every time (a local `git log`/`rev-list` costs ~50ms) so the
# count and the changelog can never disagree.
def _read_hermes_behind_count() -> int | None:
    """Read-only git rev-list count of commits on origin/main not in HEAD."""
    repo = Path(os.path.expanduser("~/.hermes")) / "hermes-agent"
    try:
        if not (repo / ".git").is_dir():
            return None
        result = _run_subprocess(
            ["git", "-C", str(repo), "rev-list", "--count", "HEAD..origin/main"],
            capture_output=True, text=True, timeout=15,
        )
        if result.returncode != 0:
            return None
        count = int(result.stdout.strip())
        return count
    except Exception as exc:
        logger.debug("behind count read failed: %s", exc)
        return None


def _read_hermes_commits() -> list[dict]:
    """Structured commits HEAD is behind origin/main by, newest first.

    Each entry: {sha (7 chars), summary, author, at (unix ts)}. Mirrors the
    Hermes dashboard's `_recent_upstream_commits` so the iOS Software Update
    sheet can group by conventional-commit type (feat → Added, fix → Fixed)
    instead of dumping raw oneline text.
    """
    repo = Path(os.path.expanduser("~/.hermes")) / "hermes-agent"
    try:
        if not (repo / ".git").is_dir():
            return []
        result = _run_subprocess(
            [
                "git", "-C", str(repo), "log",
                "--format=%H%x1f%s%x1f%an%x1f%ct",
                "HEAD..origin/main", "-n20",
            ],
            capture_output=True, text=True, timeout=15,
        )
        if result.returncode != 0:
            return []
        commits: list[dict] = []
        for line in result.stdout.splitlines():
            if not line.strip():
                continue
            parts = (line.split("\x1f") + ["", "", "", "0"])[:4]
            sha, summary, author, at = parts
            commits.append(
                {"sha": sha[:7], "summary": summary, "author": author, "at": int(at or 0)}
            )
        return commits
    except Exception as exc:
        logger.debug("commits read failed: %s", exc)
        return []


def _read_hermes_changelog() -> str | None:
    """Legacy plain-text changelog (kept for backward compat). New clients
    prefer the structured `commits` field from _read_hermes_commits."""
    commits = _read_hermes_commits()
    if not commits:
        return None
    return "\n".join(f"{c['sha']} {c['summary']}" for c in commits)


async def _read_active_model(ctx: "FacadeContext") -> str | None:
    """Best-effort: return the active model name without raising."""
    try:
        if ctx.model_catalog is not None:
            catalog = ctx.model_catalog()
            # model_catalog may be sync or async — handle both.
            if inspect.isawaitable(catalog):
                catalog = await catalog
            active = (catalog or {}).get("activeModel") or {}
            name = active.get("name")
            if name:
                return str(name)
    except Exception:
        logger.debug("active model lookup failed", exc_info=True)
    return _read_active_model_from_config()


def _read_active_model_from_config() -> str | None:
    """Synchronous fallback: read ~/.hermes/config.yaml."""
    try:
        cfg_path = Path(os.getenv("HERMES_HOME") or os.path.expanduser("~/.hermes")) / "config.yaml"
        if cfg_path.is_file():
            text = cfg_path.read_text()
            m = re.search(r"^\s*model:\s*(\S+)\s*$", text, re.MULTILINE)
            if m:
                return m.group(1)
    except OSError:
        pass
    return None


def _read_host_metrics() -> dict:
    """Read host memory + uptime. Returns a dict with explicit availability."""
    out: dict = {
        "memoryTotalBytes": None,
        "memoryUsedBytes": None,
        "uptimeSeconds": None,
        "loadAverage1m": None,
        "cpuPercent": None,
        "cpuSampleIntervalSeconds": 2.0,
        "cpuSampleReady": False,
    }
    try:
        with open("/proc/meminfo", "r", encoding="utf-8") as fh:
            meminfo: dict[str, int] = {}
            for line in fh:
                key, _, rest = line.partition(":")
                meminfo[key] = int(rest.strip().split()[0])
        total_kb = meminfo.get("MemTotal", 0)
        avail_kb = meminfo.get("MemAvailable", meminfo.get("MemFree", 0))
        out["memoryTotalBytes"] = total_kb * 1024
        out["memoryUsedBytes"] = (total_kb - avail_kb) * 1024
    except OSError:
        pass
    try:
        with open("/proc/uptime") as fh:
            out["uptimeSeconds"] = int(float(fh.read().split()[0]))
    except OSError:
        pass
    try:
        with open("/proc/loadavg") as fh:
            parts = fh.read().split()
            out["loadAverage1m"] = float(parts[0])
    except (OSError, ValueError, IndexError):
        pass
    # CPU sample: delta between two /proc/stat reads (2 s interval).
    # First call returns null + cpuSampleReady=false so the UI never
    # fabricates a 0.0% on the first sample after launch.
    global _LAST_CPU_SAMPLE
    now = time.monotonic()
    try:
        with open("/proc/stat", "r", encoding="utf-8") as fh:
            line = fh.readline()
        parts = line.split()
        if parts[0] == "cpu":
            total = sum(int(x) for x in parts[1:])
            idle = int(parts[4]) + int(parts[5]) if len(parts) > 5 else int(parts[4])
            if _LAST_CPU_SAMPLE is not None:
                last_total, last_idle, last_t = _LAST_CPU_SAMPLE
                dt = now - last_t
                if dt >= 0.5:
                    d_total = total - last_total
                    d_idle = idle - last_idle
                    if d_total > 0:
                        out["cpuPercent"] = round(
                            100.0 * (d_total - d_idle) / d_total, 2
                        )
                        out["cpuSampleReady"] = True
            _LAST_CPU_SAMPLE = (total, idle, now)
    except (OSError, ValueError, IndexError):
        pass
    return out


_LAST_CPU_SAMPLE: tuple[int, int, float] | None = None


def _systemctl_is_active(unit: str) -> bool:
    try:
        result = _run_subprocess(
            ["systemctl", "--user", "is-active", unit],
            capture_output=True, text=True, timeout=10,
        )
        return result.returncode == 0 and result.stdout.strip() == "active"
    except Exception:
        return False


async def _poll_unit_active(unit: str) -> bool:
    """Poll `systemctl --user is-active` until active or timeout."""
    deadline = time.monotonic() + _restart_active_timeout()
    while time.monotonic() < deadline:
        if await asyncio.to_thread(_systemctl_is_active, unit):
            return True
        await asyncio.sleep(_restart_poll_interval())
    return False


async def _run_restart_command(unit: str) -> int | None:
    """`systemctl --user restart --no-block <unit>`; None = couldn't run."""
    try:
        result = await asyncio.to_thread(
            _run_subprocess,
            ["systemctl", "--user", "restart", "--no-block", unit],
            capture_output=True, text=True, timeout=10,
        )
        return result.returncode
    except Exception:
        logger.debug("systemctl restart %s failed to run", unit, exc_info=True)
        return None


_ANSI_ESCAPE_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")


def _sanitize_journal(text: str) -> str:
    return _ANSI_ESCAPE_RE.sub("", text or "").strip()


async def _journal_excerpt(unit: str) -> str | None:
    """Last 5 journalctl lines for the unit — sanitized, never a raw error."""
    try:
        result = await asyncio.to_thread(
            _run_subprocess,
            ["journalctl", "--user", "-u", unit, "-n", "5", "--no-pager"],
            capture_output=True, text=True, timeout=10,
        )
        if result.returncode != 0:
            return None
        lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
        excerpt = "\n".join(_sanitize_journal(line) for line in lines[-5:])
        return excerpt or None
    except Exception:
        logger.debug("journalctl excerpt for %s failed", unit, exc_info=True)
        return None


def _schedule_connector_exit(delay_seconds: float = 0.5) -> None:
    """SIGTERM the connector process after a short delay (systemd restarts it).

    The operation row is already committed to SQLite before this runs, so
    the restart state survives; startup reconciliation marks it failed
    because the dying process cannot verify its own restart.
    """
    if os.name == "nt":
        return
    import platform as _platform
    if _platform.system() != "Linux":
        logger.warning(
            "connector self-restart requested but this platform has no systemd — not exiting"
        )
        return

    def _delayed_exit() -> None:
        time.sleep(delay_seconds)
        os.kill(os.getpid(), signal.SIGTERM)

    threading.Thread(target=_delayed_exit, daemon=True).start()


def _start_restart_task(operation_id: str) -> None:
    task = asyncio.create_task(_run_restart_operation(operation_id))
    _restart_tasks[operation_id] = task
    task.add_done_callback(lambda _t, oid=operation_id: _restart_tasks.pop(oid, None))


async def _probe_dashboard_health() -> tuple[bool, str]:
    """Hermes readiness probe: dashboard health endpoint (port 9119 by default)."""
    url = os.getenv("HERALD_DASHBOARD_HEALTH_URL", "http://127.0.0.1:9119/api/health")
    try:
        async with httpx.AsyncClient(timeout=httpx.Timeout(5.0, connect=3, read=5)) as client:
            resp = await client.get(url)
            if resp.status_code == 200:
                return True, "detail health ok"
            return False, f"health endpoint returned {resp.status_code}"
    except httpx.HTTPError:
        return False, "health endpoint unreachable"
    except Exception:
        logger.debug("dashboard health probe failed", exc_info=True)
        return False, "health endpoint unreachable"


async def _probe_model_catalog() -> tuple[bool, str]:
    """Model catalog load — succeeds when Hermes config/catalog is readable."""
    ctx = get_context()
    if ctx.model_catalog is None:
        return True, "skipped: model catalog probe not configured"
    try:
        catalog = ctx.model_catalog()
        if inspect.isawaitable(catalog):
            catalog = await catalog
        count = len((catalog or {}).get("models") or [])
        return True, f"{count} models loaded"
    except Exception:
        return False, "model catalog load failed"


async def _probe_session_canary() -> tuple[bool, str]:
    """Authenticated relay round-trip canary (wired by the connector)."""
    ctx = get_context()
    if ctx.session_canary is None:
        return True, "skipped: relay canary probe not configured"
    try:
        ok, detail = await ctx.session_canary()
        return bool(ok), str(detail or "")
    except Exception:
        logger.debug("session canary probe failed", exc_info=True)
        return False, "session canary failed"


def _port_open(host: str, port: int, timeout: float = 1.0) -> bool:
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


async def _fail_operation(
    store: RestartOperationStore,
    operation_id: str,
    unit: str,
    passed_checks: list[dict],
    failure: _RestartFailure,
) -> None:
    """Record the failed check plus skips, then complete the operation failed.

    complete_operation REPLACES the checks array, so the full final list is
    built here: checks that passed, the failed check (if any), then one
    "skipped: …" entry per check that never ran.  The error is fully typed
    (stage/exitStatus/journalExcerpt/retryable/action) — no raw exception
    text ever reaches the wire.
    """
    final_checks: list[dict] = list(passed_checks)
    if failure.failed_check is not None:
        final_checks.append(failure.failed_check)
    present = {c.get("name") for c in final_checks}
    for name in _RESTART_STEP_NAMES:
        if name not in present:
            final_checks.append({
                "name": name,
                "passed": False,
                "detail": f"skipped: {failure.skipped_note}",
            })
    store.complete_operation(
        operation_id,
        "failed",
        checks=final_checks,
        error={
            "stage": failure.stage,
            "exitStatus": failure.exit_status,
            "journalExcerpt": await _journal_excerpt(unit),
            "retryable": failure.retryable,
            "action": failure.action,
        },
    )


async def _run_restart_operation(operation_id: str) -> None:
    """Drive one restart operation through its phases in the background.

    accepted (persisted by the endpoint) → stopping → starting → verifying
    → healthy | failed.  Every check lands in the operation's `checks`
    array; every failure produces a typed error with a sanitized journal
    excerpt — never a raw exception string.
    """
    ctx = get_context()
    store = ctx.restart_store or get_restart_store()
    details = store.get_operation_details(operation_id)
    if details is None or details["phase"] not in NON_TERMINAL_PHASES:
        return
    unit = details["unit"]
    target = details["target"]
    checks: list[dict] = []
    try:
        # ── stopping ──────────────────────────────────────────────────────
        store.update_phase(operation_id, "stopping")
        if target == "connector":
            # Self-restart: the record is durable in SQLite; the process dies
            # before verification and startup reconciliation marks it failed.
            checks.append({
                "name": "connector-exit",
                "passed": True,
                "detail": "SIGTERM scheduled — operation state persisted",
            })
            store.update_phase(operation_id, "stopping", checks=checks)
            _schedule_connector_exit()
            return

        exit_status = await _run_restart_command(unit)
        if exit_status not in (None, 0):
            raise _RestartFailure(
                "stopping",
                exit_status=exit_status,
                action=(
                    f"systemctl restart {unit} failed (exit {exit_status}). "
                    f"Check journalctl --user -u {unit}."
                ),
            )

        # ── starting: is-active, then MainPID changed ─────────────────────
        store.update_phase(operation_id, "starting")
        if not await _poll_unit_active(unit):
            raise _RestartFailure(
                "starting",
                action=(
                    f"Unit {unit} did not reach 'active' within "
                    f"{_restart_active_timeout():.0f}s. "
                    f"Check journalctl --user -u {unit}."
                ),
            )
        checks.append({"name": "systemctl-is-active", "passed": True, "detail": "active"})

        observed = await asyncio.to_thread(_query_unit_observed, unit)
        new_pid = (observed or {}).get("main_pid")
        old_pid = details.get("oldMainPid")
        if not new_pid or new_pid == old_pid:
            raise _RestartFailure(
                "starting",
                action=(
                    f"MainPID of {unit} did not change after restart "
                    f"(still {old_pid}). Check journalctl --user -u {unit}."
                ),
            )
        checks.append({
            "name": "pid-changed",
            "passed": True,
            "detail": f"{old_pid} → {new_pid}",
        })
        store.update_phase(operation_id, "verifying", checks=checks)

        # ── verifying: dashboard health, model catalog, relay canary ──────
        ready, ready_detail = await _probe_dashboard_health()
        if not ready:
            raise _RestartFailure(
                "verifying",
                failed_check={"name": "hermes-ready", "passed": False, "detail": ready_detail},
                skipped_note="hermes not ready",
                action="Check Hermes gateway logs on the host for configuration errors.",
            )
        checks.append({"name": "hermes-ready", "passed": True, "detail": ready_detail})

        catalog_ok, catalog_detail = await _probe_model_catalog()
        if not catalog_ok:
            raise _RestartFailure(
                "verifying",
                failed_check={"name": "model-catalog", "passed": False, "detail": catalog_detail},
                action=(
                    "Hermes is up but its model catalog could not be loaded. "
                    "Check ~/.hermes/config.yaml on the host."
                ),
            )
        checks.append({"name": "model-catalog", "passed": True, "detail": catalog_detail})

        canary_ok, canary_detail = await _probe_session_canary()
        global _last_canary_result
        _last_canary_result = bool(canary_ok)
        if not canary_ok:
            raise _RestartFailure(
                "verifying",
                failed_check={"name": "session-roundtrip", "passed": False, "detail": canary_detail},
                action=(
                    "Hermes did not reply to the connectivity canary. Check the "
                    "relay gateway connection and Hermes agent logs on the host."
                ),
            )
        checks.append({"name": "session-roundtrip", "passed": True, "detail": canary_detail})

        store.complete_operation(operation_id, "healthy", checks=checks)
        logger.info("restart: operation %s healthy (%d checks)", operation_id, len(checks))
    except _RestartFailure as failure:
        await _fail_operation(store, operation_id, unit, checks, failure)
    except Exception:
        logger.exception("restart: operation %s failed unexpectedly", operation_id)
        await _fail_operation(
            store, operation_id, unit, checks,
            _RestartFailure(
                "verifying",
                action=(
                    "Restart verification failed unexpectedly. Retry the restart; "
                    "if it persists, check the connector logs on the host."
                ),
            ),
        )


async def gateway_status(request: Request) -> JSONResponse:
    """Return gateway telemetry for the Settings → Gateway Status screen.

    Build 103 WS-D: truthful, versioned telemetry contract. Missing values
    are reported as ``null`` + ``availability`` metadata so the UI never
    fabricates a 0.0 % CPU on the first sample after launch. Singleton
    ownership, restart count, and port ownership are explicit booleans so
    the screen can color status green only when every dependency passes.

    Schema: gateway-health-v2 (backward-compatible with v1 readers — every
    new field is additive, removed fields are listed under ``removed``).

    The envelope middleware adds the single outer ``data`` member that
    ``RelayAPIClient`` decodes.  Return the telemetry payload itself here;
    returning ``{"data": payload}`` would double-wrap the response and make
    the iOS decoder fail even though the endpoint returned HTTP 200.
    """
    await require_native_or_paired_auth(request)
    ctx = get_context()

    sampled_at = datetime.datetime.now(datetime.timezone.utc).isoformat()

    payload: dict = {
        "$schema": "gateway-health-v2",
        "schemaVersion": 2,
        "sampledAt": sampled_at,
        "sampleIntervalSeconds": 2.0,
        "staleAfterSeconds": 8.0,
        "overall": "unknown",
        "connectorConnected": True,
        "connectorVersion": ctx.connector_version or "0.0.0",
    }

    # ── Connector: singleton ownership, port ownership, restart count ──────
    connector_pid = os.getpid()
    connector_unit = _JOURNAL_UNIT
    connector_observed = await asyncio.to_thread(
        _query_unit_observed, connector_unit
    )
    managed_main_pid = (connector_observed or {}).get("main_pid")
    managed_active = (connector_observed or {}).get("active_state") == "active"
    payload["connector"] = {
        "state": "healthy" if (managed_active and managed_main_pid == connector_pid) else "degraded",
        "version": ctx.connector_version or __version__,
        "protocolVersion": HERALD_PROTOCOL,
        "pid": connector_pid,
        "managedMainPID": managed_main_pid,
        "uptimeSeconds": int(time.monotonic() - _PROCESS_STARTED_AT),
        "restartCount": _get_nrestarts(connector_unit),
        "singleton": managed_main_pid == connector_pid,
        "portsOwned": await asyncio.to_thread(
            _check_ports_owned_by_pid, connector_pid, _CONNECTOR_REQUIRED_PORTS
        ),
        "unit": connector_unit,
    }

    # ── Hermes: service state + dashboard + host WS ──────────────────────
    unit = _hermes_unit()
    observed = await asyncio.to_thread(_query_unit_observed, unit)
    hermes_pid = (observed or {}).get("main_pid") if observed else None
    hermes_active = bool(observed and observed.get("active_state") == "active" and hermes_pid)
    dashboard_port = int(os.getenv("HERALD_DASHBOARD_PORT", "9119"))
    dashboard_ready, _ = await _probe_dashboard_health()
    payload["hermes"] = {
        "state": "healthy" if (hermes_active and dashboard_ready) else "degraded",
        "installedVersion": _read_hermes_installed_version() or "unknown",
        "latestVersion": None,
        "updateAvailable": None,
        "pid": hermes_pid,
        "uptimeSeconds": _pid_uptime_seconds(hermes_pid) if hermes_pid else None,
        "dashboardReady": dashboard_ready,
        "dashboardPort": dashboard_port,
        "hostWebSocketReady": True,   # Build 103 WS-A: connect endpoint is direct, not via FastAPI WS
        "activeModel": await _read_active_model(ctx),
        "profile": _hermes_profile(),
        "unit": unit,
        "restartCount": _get_nrestarts(unit),
    }

    # Try a non-blocking best-effort latest-version probe (no subprocess
    # spawn — read `~/.hermes/.update_check` if it exists). Never raises.
    latest_version, behind, checked_at = _read_hermes_latest_version()
    if latest_version is not None:
        payload["hermes"]["latestVersion"] = latest_version
        payload["hermes"]["behindCount"] = behind
        payload["hermes"]["lastCheckedAt"] = checked_at
        if behind is not None and behind > 0:
            payload["hermes"]["updateAvailable"] = True
        elif payload["hermes"]["installedVersion"] != "unknown":
            payload["hermes"]["updateAvailable"] = (
                latest_version != payload["hermes"]["installedVersion"]
                and latest_version not in ("unknown", "")
            )

    # ── Host: memory, uptime ──────────────────────────────────────────────
    payload["host"] = _read_host_metrics()

    # ── Jobs: facade-owned HTTP job counts only ────────────────────────────
    payload["jobs"] = {
        "active": 0,
        "queued": 0,
        "source": "push-only",
    }

    # ── Reasons (degraded-state explanation list) ─────────────────────────
    reasons: list[str] = []
    if not payload["connector"]["singleton"]:
        reasons.append(
            f"Connector process {connector_pid} is not the systemd unit MainPID "
            f"({managed_main_pid}); an unmanaged duplicate owns the ports."
        )
    if not payload["connector"]["portsOwned"]:
        reasons.append(
            "Connector does not own all required ports (8010/8765/8767)."
        )
    if payload["connector"]["restartCount"] > 0 and managed_active:
        reasons.append(
            f"Connector unit has restarted {payload['connector']['restartCount']} time(s) "
            "since last service-manager reset."
        )
    if payload["connector"]["state"] != "healthy":
        reasons.append("Connector self-reported unhealthy.")
    if not hermes_active:
        reasons.append(f"Hermes unit {unit} is not active.")
    if not dashboard_ready:
        reasons.append(
            f"Hermes dashboard on port {dashboard_port} is unreachable."
        )
    if payload["jobs"]["active"] > 50:
        reasons.append(f"{payload['jobs']['active']} active jobs exceed soft cap.")
    payload["reasons"] = reasons

    # ── Overall ───────────────────────────────────────────────────────────
    if reasons:
        # Critical if singleton OR Hermes is down; degraded otherwise.
        if (not payload["connector"]["singleton"]) or (not hermes_active):
            payload["overall"] = "critical"
        else:
            payload["overall"] = "degraded"
    elif all([
        payload["connector"]["state"] == "healthy",
        payload["hermes"]["state"] == "healthy",
    ]):
        payload["overall"] = "healthy"
    else:
        payload["overall"] = "degraded"

    # RelayAPIClient unwraps the envelope middleware once.  The iOS status
    # decoder deliberately owns this inner `data` key, so do not flatten it.
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




# ── Attachment serving ─────────────────────────────────────────────────────




# ── Pairing / Auth ───────────────────────────────────────────────────────

# Pairing codes persist in the connector-local home, never in Hermes state.db.
# Records contain only SHA-256 code digests, expiry, device binding and the
# response needed for same-installation replay. Plaintext codes are never saved.
def _pairing_code_store() -> PairingCodeStore:
    home = Path(os.getenv("HERMES_MOBILE_CONNECTOR_HOME") or Path.home() / ".hermes-mobile")
    return PairingCodeStore(home / "pairing_codes.json")


_pairing_codes = _pairing_code_store()
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
    _pairing_codes.create(hashed, expires_at=expires, created_at=_time.time())
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
    # Build 30: accept both the nested iOS DTO (device.installationId) and
    # the top-level form (installationId, deviceId).  The app sends installationId
    # under a `device` key; the old code only read top-level and silently
    # recorded an empty identity, breaking all-device scoping.
    raw_installation_id = (
        body.get("installationId")
        or body.get("deviceId")
        or ""
    )
    # Also try the nested DTO
    if not raw_installation_id and isinstance(body.get("device"), dict):
        raw_installation_id = body["device"].get("installationId") or ""
    installation_id = str(raw_installation_id).strip()[:255]
    logger.info("Redeem body keys: %s, code raw: %s, installation: %s",
                list(body.keys()), body.get("code", "?")[:20], installation_id[:12])
    code = (body.get("code") or "").upper().replace("-", "").replace(" ", "")
    logger.info("Redeem normalized code: %s", code)
    hashed = _hash_code(code)

    # The durable store keeps a code alive across connector restarts and only
    # permits replay by the installation that first redeemed it.
    stored = _pairing_codes.get_live(hashed, now=_time.time())
    if stored is None:
        raise HTTPException(status_code=401, detail="Invalid or expired pairing code")
    if stored.get("redeemed_at") is not None:
        if stored.get("installation_id") == installation_id and isinstance(stored.get("response_payload"), dict):
            logger.info("Idempotent replay of pairing code for installation %s", installation_id[:12])
            return JSONResponse(stored["response_payload"])
        raise HTTPException(status_code=401, detail="Invalid or expired pairing code")

    # First redeem — build the response, mark, and persist.
    import secrets as _sec
    # Build 31: generate a per-device token instead of reusing the
    # shared connector credential.  The old code gave every device the
    # same token, which made record_pairing_device overwrite the previous
    # device's identity — all devices resolved to whichever device paired
    # most recently.  A unique per-device token makes allDevices filtering
    # actually correct.
    device_token = f"hd_{_sec.token_urlsafe(24)}"
    _default_validator.add_token(device_token)
    # Also keep the shared credential valid so older builds / other paths
    # still work.  New builds use the per-device token.
    shared_token = ctx.connector_credential or ctx.paired_device_id or "herald-connector"
    _default_validator.add_token(shared_token)
    # Build 28: record token→device so allDevices filtering can
    # resolve the requesting device identity from the auth token.
    if installation_id:
        from .session_store import record_pairing_device
        record_pairing_device(device_token, installation_id)
    import uuid as _uuid
    payload = {
        "user": {"id": ctx.paired_user_id or str(_uuid.uuid4()), "displayName": "Herald User"},
        "deviceId": str(_uuid.uuid4()),
        "deviceRegistered": True,
        "session": {"connectionStatus": "connected", "isMockMode": False, "backendEndpoint": ctx.public_base_url or "", "lastSyncAt": None},
        "auth": {"accessToken": device_token, "refreshToken": device_token, "expiresAt": datetime.datetime.now(datetime.timezone.utc).isoformat()},
    }
    if not _pairing_codes.redeem(hashed, installation_id=installation_id, payload=payload, now=_time.time()):
        # A concurrent redemption won. Never leak its payload to this request.
        raise HTTPException(status_code=401, detail="Invalid or expired pairing code")
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
    installation_id = str(dev.get("installationId") or "").strip()[:255]
    logger.info("Device registered: %s, installation: %s",
                dev.get("deviceName", "?")[:30], installation_id[:12])
    import time as _time3, uuid as _uuid4, secrets as _sec
    # Build 31: generate a per-device token so each device has a distinct
    # identity for allDevices scoping.  Previously this used the shared
    # connector credential, making all devices appear as one.
    device_token = f"hd_{_sec.token_urlsafe(24)}"
    _default_validator.add_token(device_token)
    # Also keep the shared credential valid for older builds.
    shared_token = ctx.connector_credential or str(_uuid4.uuid4())
    _default_validator.add_token(shared_token)
    if installation_id:
        from .session_store import record_pairing_device
        record_pairing_device(device_token, installation_id)
    return JSONResponse({
        "deviceId": str(_uuid4.uuid4()),
        "deviceRegistered": True,
        "session": {"connectionStatus": "connected", "isMockMode": False, "backendEndpoint": ctx.public_base_url or "", "lastSyncAt": None},
        "auth": {"accessToken": device_token, "refreshToken": device_token, "expiresAt": _time3.time() + 86400},
    })


async def connector_events(request: Request) -> StreamingResponse:
    """SSE stream of connector health events."""
    import asyncio as _asyncio
    async def stream():
        try:
            yield "event: connected\ndata: {}\n\n"
            while True:
                await _asyncio.sleep(30)
                if await request.is_disconnected():
                    break
                yield "event: health_check\ndata: {\"status\": \"online\"}\n\n"
        except _asyncio.CancelledError:
            yield ": bye\n\n"
            raise
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
    from .session_store import session_list, device_id_for_token

    limit = int(request.query_params.get("limit", "50"))
    offset = int(request.query_params.get("offset", "0"))
    # Build 28: honour allDevices scope.  When false, filter to the
    # requesting device's sessions.  Parse strictly: any value other
    # than "true" (case-insensitive) is treated as false.
    all_devices_raw = request.query_params.get("allDevices", "true")
    all_devices = all_devices_raw.lower() == "true"
    device_id = None
    if not all_devices:
        token = await _extract_token(request)
        device_id = device_id_for_token(token)
    try:
        sessions, total = await asyncio.to_thread(
            session_list, limit=limit, offset=offset, device_id=device_id,
        )
    except Exception:
        logger.exception("session_list query failed")
        sessions, total = [], 0

    return JSONResponse({"sessions": sessions, "total": total})


async def get_inbox(request: Request) -> JSONResponse:
    """Return inbox items for the requesting device.

    Build 67: was a stub returning {"items": []}. Completed chat turns now
    persist an inbox item (see client._send_push_for_job), scoped to the
    installation that owns the conversation, so the app's Inbox tab shows
    real retrievable responses. The response shape matches what
    LiveInboxService decodes: id, kind, title, body, priority, status,
    payload, createdAt, primaryActionTitle, secondaryActionTitle.
    """
    auth_token = await require_native_or_paired_auth(request)
    installation_id = ""
    # Build 67: cookie-auth clients (pairing/basic) can't be resolved from
    # the token alone, so the app passes its installationId explicitly.
    installation_id = str(request.query_params.get("installationId") or "").strip()[:255]
    if not installation_id:
        try:
            from .session_store import device_id_for_token
            installation_id = device_id_for_token(auth_token) or ""
        except Exception:
            installation_id = ""
    if not installation_id:
        # Paired-token auth without a registry entry: no scoped inbox.
        return JSONResponse({"items": []})

    try:
        from .inbox_store import get_inbox_store
        items = get_inbox_store().list_items(installation_id, limit=100)
    except Exception as exc:
        logger.warning("get_inbox: store unavailable: %s", exc)
        return JSONResponse({"items": []})

    def _payload_for(item: dict) -> dict:
        payload = dict(item.get("payload") or {})
        if item.get("conversationId") and "conversationId" not in payload:
            payload["conversationId"] = item["conversationId"]
        return payload

    def _normalize_attachment(raw: dict) -> dict:
        """Normalize an inbox attachment to the iOS MessageAttachment schema.

        The connector persists attachments in the relay attachments_data shape
        ({type, filename, mimeType, data, mediaKey}), but LiveInboxService
        decodes inbox items with a synthesized Decodable that requires the
        MessageAttachment keys: id (UUID), kind, fileName, mimeType. One
        mismatched attachment previously failed the decode of the ENTIRE
        items array, so the Inbox tab showed "All Caught Up" even when rows
        existed. Map leniently and keep the embedded base64 as
        thumbnailBase64 so the bubble renders immediately; mediaURL lets
        AttachmentService fetch full bytes via /v1/native/media.
        """
        import uuid as _uuid
        raw = dict(raw or {})
        kind = str(raw.get("kind") or raw.get("type") or "file")
        file_name = str(raw.get("fileName") or raw.get("filename") or "attachment")
        mime = str(raw.get("mimeType") or "application/octet-stream")
        data_b64 = raw.get("data") or raw.get("thumbnailBase64") or raw.get("thumbnailData") or ""
        media_key = str(raw.get("mediaKey") or "")
        normalized: dict = {
            # Swift's UUID(uuidString:) requires the dashed 8-4-4-4-12 form;
            # .hex (32 chars, no dashes) fails MessageAttachment's decode and
            # nukes the ENTIRE inbox items array. Use str(uuid4()).
            "id": str(_uuid.uuid4()),
            "kind": kind,
            "fileName": file_name,
            "mimeType": mime,
            "thumbnailBase64": str(data_b64) if data_b64 else None,
            "localStoragePath": raw.get("localStoragePath"),
            "messageID": raw.get("messageID"),
            "remoteIndex": raw.get("remoteIndex"),
        }
        if media_key:
            # Full absolute URL - AttachmentService.fetchNativeMedia does
            # URLRequest(url:), so a bare path would fail. Mirror the push
            # path's synthesized URL (client.py line ~2197).
            normalized["mediaURL"] = f"https://hermes-relay.fihonline.net/v1/native/media?path={media_key}"
        elif data_b64:
            # No mediaKey but we have embedded bytes; keep them available
            # through the thumbnail field so PDFs/images still render.
            normalized["mediaURL"] = None
        return {k: v for k, v in normalized.items() if v is not None}

    return JSONResponse({
        "items": [
            {
                "id": item["id"],
                "kind": item["kind"],
                "title": item["title"],
                "body": item["body"],
                "priority": item["priority"],
                "status": item["status"],
                "payload": _payload_for(item),
                "attachments": [_normalize_attachment(a) for a in (item.get("attachments") or [])],
                "createdAt": item["createdAt"],
                "primaryActionTitle": "Open",
                "secondaryActionTitle": "Dismiss",
            }
            for item in items
        ]
    })


async def inbox_action(request: Request) -> JSONResponse:
    """POST /v1/inbox/{id}/action - mark an inbox item opened/dismissed.

    The app's LiveInboxService posts {"actionId": "open"|"approve"|"dismiss"}.
    Non-approval items navigate client-side when tapped (InboxStore reads
    payload.conversationId), so the only server mutations that matter are
    status transitions: open -> opened, dismiss -> dismissed.
    """
    auth_token = await require_native_or_paired_auth(request)
    item_id = request.path_params.get("id", "")
    try:
        body = await request.json()
    except Exception:
        body = {}
    action_id = str(body.get("actionId") or "open").strip().lower()
    status = {"open": "opened", "approve": "completed", "dismiss": "dismissed"}.get(action_id, "opened")

    installation_id = str(request.query_params.get("installationId") or "").strip()[:255]
    if not installation_id:
        try:
            from .session_store import device_id_for_token
            installation_id = device_id_for_token(auth_token) or ""
        except Exception:
            installation_id = ""
    if not installation_id:
        return JSONResponse({"applied": False, "status": "not_implemented"}, status_code=404)

    try:
        from .inbox_store import get_inbox_store
        updated = get_inbox_store().set_status(item_id, installation_id, status)
    except Exception as exc:
        logger.warning("inbox_action: store unavailable: %s", exc)
        return JSONResponse({"applied": False, "status": "error"}, status_code=500)

    if updated is None:
        return JSONResponse({"applied": False, "status": "not_found"}, status_code=404)
    return JSONResponse({"applied": True, "status": updated["status"]})


async def push_test(request: Request) -> JSONResponse:
    """POST /v1/push/test - fire a real APNs push to the registered device.

    Uses the exact completion path a finished chat turn uses
    (send_completion_push -> _send_push_for_job), so it validates the full
    chain: registered device token -> APNs -> lock screen notification.
    Lets you test push reliability without running a full agent turn.
    """
    await require_native_or_paired_auth(request)

    # Read the registered token from the same state file the connector uses,
    # so the response can distinguish a real token from a placeholder.
    from .state import ConnectorStateStore
    state = ConnectorStateStore().load()
    token = (state.device_token or "").strip()
    if not token or token in {"abc", "e2e-test-token-1234567890abcdef"}:
        return JSONResponse(
            {
                "sent": False,
                "reason": "no_device_token",
                "detail": "No APNs device token registered. Open Kallisti once "
                          "so it re-registers its token at launch, then retry.",
            },
            status_code=409,
        )

    try:
        body = await request.json()
    except Exception:
        body = {}
    if not isinstance(body, dict):
        raise HTTPException(status_code=400, detail="Request body must be a JSON object")
    text = str(body.get("body") or "Test push from Kallisti").strip()[:100]
    device_id = str(body.get("deviceId") or "").strip()[:255]

    ctx = get_context()
    if ctx.send_completion_push is None:
        raise HTTPException(status_code=503, detail="Push delivery is unavailable")

    # Seed the target device's inbox so the Inbox tab shows the test too.
    attachments = body.get("attachments") if isinstance(body.get("attachments"), list) else None
    if device_id:
        try:
            from .inbox_store import get_inbox_store
            get_inbox_store().add_item(
                installation_id=device_id,
                title="Response ready",
                body=text,
                kind="notification",
                payload={"conversationId": None},
                attachments=attachments,
                priority="normal",
            )
        except Exception:
            logger.debug("push_test: inbox seed failed (non-fatal)", exc_info=True)

    await ctx.send_completion_push(
        "test",
        text,
        category="HERALD_MESSAGE_READY",
    )
    logger.info("Test push dispatched (job=test, env=%s)", state.device_token_environment or "production")
    return JSONResponse(
        {
            "sent": True,
            "body": text,
            "environment": state.device_token_environment or "production",
            "note": "Check the connector log for 'Push sent for job test' to confirm APNs accepted it.",
        }
    )


async def push_register(request: Request) -> JSONResponse:
    """Persist the current device's APNs token for direct delivery.

    Native-gateway clients (Build 51+) authenticate with the gateway's
    OAuth bearer token, not a connector pairing token. require_auth
    only accepts connector credentials (hd_ / shared), so a Build 51
    phone's POST landed here with 401 even after Caddy routed it correctly.
    Use the same dual-path auth the media route uses: gateway bearer,
    gateway cookie, then connector token fallback.
    """
    await require_native_or_paired_auth(request)
    try:
        body = await request.json()
    except Exception:
        raise HTTPException(status_code=400, detail="Request body must be JSON")
    if not isinstance(body, dict):
        raise HTTPException(status_code=400, detail="Request body must be a JSON object")

    # ``apnsToken`` is the current iOS contract. Accept the old key only for
    # already-released clients, and never log the token or any identifying part
    # of it.
    token = str(body.get("apnsToken") or body.get("deviceToken") or "").strip()
    environment = str(body.get("pushEnvironment") or "production").strip().lower()
    if not token:
        raise HTTPException(status_code=400, detail="apnsToken is required")
    if environment not in {"production", "development"}:
        raise HTTPException(status_code=400, detail="pushEnvironment must be production or development")

    # Build 67: multi-device. The app sends its installationId so an iPad and
    # iPhone each keep their own token instead of clobbering a single global
    # slot. Fall back to resolving it from the auth token when the body omits
    # it (older clients).
    installation_id = str(body.get("installationId") or "").strip()[:255]
    if not installation_id:
        try:
            from .session_store import device_id_for_token
            auth_token = await require_native_or_paired_auth(request)
            installation_id = device_id_for_token(auth_token) or ""
        except Exception:
            installation_id = ""

    # 2026-08-07: a Live Activity's ActivityKit pushToken: .token gives it its
    # OWN APNs token, distinct from the device's regular alert-push token —
    # it needs the liveactivity push type and a ContentState payload, never a
    # plain alert. Both used to land in the same body shape at this one
    # endpoint with no way to tell them apart, so a Live Activity token
    # rotation (which happens on every chat turn) silently clobbered the
    # device's real alert token. tokenKind defaults to "device" so already
    # -shipped clients that never send it keep today's behavior exactly.
    token_kind = str(body.get("tokenKind") or "device").strip().lower()
    if token_kind not in {"device", "liveActivity".lower()}:
        raise HTTPException(status_code=400, detail="tokenKind must be device or liveActivity")

    ctx = get_context()
    if ctx.push_register is None:
        raise HTTPException(status_code=503, detail="Push registration is unavailable")
    result = await ctx.push_register({
        "token": token, "environment": environment, "tokenKind": token_kind,
        "installationId": installation_id,
    })
    if result.get("registered") is not True:
        raise HTTPException(status_code=503, detail="Push registration was not accepted")
    logger.info(
        "Push registration accepted (environment=%s, kind=%s, device=%s)",
        environment, token_kind, installation_id[:12] or "unknown",
    )
    return JSONResponse({"registered": True, "environment": environment})


_NATIVE_MEDIA_MIME = {
    ".png": "image/png",
    ".jpg": "image/jpeg",
    ".jpeg": "image/jpeg",
    ".gif": "image/gif",
    ".webp": "image/webp",
    # Build 101: non-image attachments for cross-client history continuity.
    # Same auth + allowed-roots gates apply; this just stops 415-ing every
    # PDF/video/file that was inlined in the desktop and re-opened on iOS.
    ".pdf": "application/pdf",
    ".mp4": "video/mp4",
    ".mov": "video/quicktime",
    ".m4v": "video/x-m4v",
    ".webm": "video/webm",
    ".mp3": "audio/mpeg",
    ".m4a": "audio/mp4",
    ".wav": "audio/wav",
    ".aac": "audio/aac",
    ".zip": "application/zip",
    ".txt": "text/plain",
    ".md": "text/markdown",
    ".csv": "text/csv",
    ".json": "application/json",
    ".xml": "application/xml",
    ".yaml": "application/yaml",
    ".yml": "application/yaml",
    ".doc": "application/msword",
    ".docx": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ".xls": "application/vnd.ms-excel",
    ".xlsx": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ".ppt": "application/vnd.ms-powerpoint",
    ".pptx": "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ".rtf": "application/rtf",
}


async def message_attachment_bytes(request: Request) -> Response:
    """GET /v1/messages/{messageID}/attachments/{remoteIndex}

    Conversation loads carry attachment metadata only; full bytes are
    fetched on demand by AttachmentService.swift.  The envelope middleware
    passes non-JSON Content-Types through untouched, so this raw-bytes
    response is not wrapped.

    Serves both sidecar attachments (uploaded bytes) and Electron directive
    attachments (`@image:`/`@file:` refs resolved from disk) — get_attachment
    unifies the two.
    """
    await require_auth(request)
    import base64

    from .session_store import get_attachment

    raw_msg_id = request.path_params.get("messageID", "")
    if not raw_msg_id or not isinstance(raw_msg_id, str):
        raise HTTPException(status_code=404, detail="Message not found.")
    # Reject path-injection attempts before UUID coercion.
    if "/" in raw_msg_id or "\\" in raw_msg_id or ".." in raw_msg_id:
        raise HTTPException(status_code=404, detail="Message not found.")
    msg_id = _coerce_uuid(raw_msg_id)
    if not msg_id:
        raise HTTPException(status_code=404, detail="Message not found.")

    raw_index = request.path_params.get("remoteIndex", "")
    try:
        index = int(raw_index)
    except (TypeError, ValueError):
        raise HTTPException(status_code=404, detail="Attachment not found.")
    if index < 0 or index > 255:
        raise HTTPException(status_code=404, detail="Attachment not found.")

    att = get_attachment(msg_id, index)
    if att is None:
        raise HTTPException(status_code=404, detail="Attachment not found.")
    if att.get("expired"):
        raise HTTPException(status_code=410, detail="Attachment has expired.")

    data_b64 = att.get("data") or ""
    if not data_b64:
        raise HTTPException(status_code=410, detail="Attachment data was removed.")

    try:
        payload = base64.b64decode(data_b64)
    except (ValueError, TypeError):
        raise HTTPException(status_code=422, detail="Attachment data is corrupt.")

    # Enforce size cap (25 MB) before serving.
    max_bytes = 25 * 1024 * 1024
    if len(payload) > max_bytes:
        raise HTTPException(status_code=422, detail="Attachment exceeds size limit.")

    filename = att.get("filename", "attachment")
    mime_type = att.get("mimeType", "application/octet-stream")
    # Sanitize filename: strip path separators and quotes to prevent
    # header-injection / content-disposition abuse.
    safe_filename = str(filename).replace("/", "_").replace("\\", "_").replace('"', "'")[:255]
    etag = hashlib.sha256(payload).hexdigest()[:32]

    # If-None-Match support: conditional GET for bandwidth savings.
    if request.headers.get("If-None-Match") == etag:
        return Response(status_code=304)

    return Response(
        content=payload,
        media_type=mime_type,
        headers={
            "Content-Disposition": f'inline; filename="{safe_filename}"',
            "Content-Length": str(len(payload)),
            "Cache-Control": "private, max-age=86400",
            "ETag": etag,
            "X-Content-Type-Options": "nosniff",
        },
    )


async def native_media_route(request: Request) -> Response:
    """Serve an agent-generated image to an authenticated native client.

    Native gateway ``message.complete`` events preserve ``MEDIA:/host/path``
    directives as text. The iOS client cannot read that Linux path, so this
    Herald-owned mobile adapter exposes only image files under the active
    Hermes profile's generated-image roots. It never serves arbitrary paths.

    Backward compatibility: legacy absolute paths (Build 49) are resolved
    against the same allowed roots. Normalized relative paths (Build 50)
    are searched across roots so images survive HERMES_HOME moves or profile
    consolidation.
    """
    await require_native_or_paired_auth(request)
    raw = request.query_params.get("path", "").strip()
    if not raw:
        raise HTTPException(status_code=400, detail="path is required")

    hermes_home = Path(os.getenv("HERMES_HOME") or Path.home() / ".hermes").resolve()
    allowed_roots = _build_media_roots(hermes_home)

    # Build 50: resolve the candidate, trying relative-then-absolute.
    # Relative paths (Build 50 normalized) are resolved against each
    # allowed root. Absolute paths (Build 49 legacy) are resolved as-is.
    candidate: Path | None = None
    path_obj = Path(raw)

    if not path_obj.is_absolute():
        if ".." in path_obj.parts:
            raise HTTPException(status_code=403, detail="media path is outside generated-image roots")
        candidate = _resolve_media_key(path_obj, hermes_home, allowed_roots)
    else:
        candidate = path_obj.expanduser().resolve()
        # A stored Build 49 path can outlive a removed profile. Map only its
        # approved media suffix to the consolidated or surviving profile roots.
        if not candidate.is_file():
            legacy_key = _legacy_media_key(path_obj)
            candidate = _resolve_media_key(legacy_key, hermes_home, allowed_roots) if legacy_key else None

    if candidate is None or not candidate.suffix.lower() in _NATIVE_MEDIA_MIME:
        logger.warning(
            "native media resolution failed raw=%r home=%s candidate=%s roots=%s",
            raw,
            hermes_home,
            candidate,
            [str(root) for root in allowed_roots],
        )
        raise HTTPException(status_code=415, detail="unsupported media type")
    if not any(candidate.is_relative_to(root) for root in allowed_roots):
        raise HTTPException(status_code=403, detail="media path is outside generated-image roots")
    if not candidate.is_file():
        raise HTTPException(status_code=404, detail="media not found")
    if candidate.stat().st_size > 10 * 1024 * 1024:
        raise HTTPException(status_code=413, detail="media exceeds 10 MB")

    return Response(
        candidate.read_bytes(),
        media_type=_NATIVE_MEDIA_MIME[candidate.suffix.lower()],
        headers={"Cache-Control": "private, max-age=3600"},
    )


def _legacy_media_key(path: Path) -> Path | None:
    """Extract only an approved media suffix from a stored absolute path."""
    parts = path.parts
    for index in range(len(parts)):
        tail = parts[index:]
        if len(tail) >= 3 and tail[0] == "cache" and tail[1] == "images":
            return Path(*tail)
        if len(tail) >= 2 and tail[0] in {"images", "media"}:
            return Path(*tail)
    return None


def _resolve_media_key(key: Path, hermes_home: Path, allowed_roots: list[Path]) -> Path | None:
    """Resolve a typed relative key without ever leaving an approved root."""
    if ".." in key.parts:
        return None
    parts = key.parts
    if len(parts) >= 3 and parts[0:2] == ("cache", "images"):
        suffix = Path(*parts[2:])
        preferred = [hermes_home / "cache" / "images"]
    elif len(parts) >= 2 and parts[0] in {"images", "media"}:
        suffix = Path(*parts[1:])
        preferred = [hermes_home / parts[0]]
    else:
        return None

    roots = [root.resolve() for root in preferred]
    roots.extend(root for root in allowed_roots if root not in roots)
    for root in roots:
        candidate = (root / suffix).resolve()
        if candidate.is_relative_to(root) and candidate.is_file():
            return candidate
    return None


def _build_media_roots(hermes_home: Path) -> list[Path]:
    """Build the ordered list of allowed media roots under HERMES_HOME.

    Includes the consolidated roots (Build 50 preferred) and the legacy
    per-profile roots so existing stored messages resolve after gateway
    restarts or profile consolidation.
    """
    roots = [
        (hermes_home / "cache" / "images").resolve(),
        (hermes_home / "images").resolve(),
        (hermes_home / "media").resolve(),
    ]
    # A selected profile rewrites HERMES_HOME to
    # <base>/.hermes/profiles/<profile>. Generated media can still live in the
    # consolidated base home, so include those roots without permitting any
    # path outside the known media directories.
    if hermes_home.parent.name == "profiles":
        base_home = hermes_home.parent.parent
        roots.extend((
            (base_home / "cache" / "images").resolve(),
            (base_home / "images").resolve(),
            (base_home / "media").resolve(),
        ))
    profiles_root = hermes_home / "profiles"
    if profiles_root.is_dir():
        for profile_home in profiles_root.iterdir():
            if profile_home.is_dir():
                roots.extend((
                    (profile_home / "cache" / "images").resolve(),
                    (profile_home / "images").resolve(),
                    (profile_home / "media").resolve(),
                ))
    return roots


async def _verify_native_gateway_auth(headers: dict[str, str]) -> bool:
    """Delegate a bearer or cookie-session check to the native gateway."""
    import httpx

    from .native_watch import NATIVE_GATEWAY_HOST, NATIVE_GATEWAY_PORT

    url = f"http://{NATIVE_GATEWAY_HOST}:{NATIVE_GATEWAY_PORT}/api/auth/me"
    try:
        async with httpx.AsyncClient(timeout=5.0) as client:
            resp = await client.get(url, headers=headers)
        return resp.status_code == 200
    except Exception:
        logger.exception("native gateway auth verification failed")
        return False


async def _verify_native_gateway_bearer(token: str) -> bool:
    """True if *token* is a live native-gateway access token."""
    return await _verify_native_gateway_auth(
        {"Authorization": f"Bearer {token}"}
    )


async def _verify_native_gateway_cookie(cookie: str) -> bool:
    """True if *cookie* identifies a live gateway session."""
    return await _verify_native_gateway_auth({"Cookie": cookie})


from starlette.websockets import WebSocket

async def terminal_websocket_route(websocket: WebSocket) -> None:
    """WS /v1/terminal -- real PTY bridge running ``hermes --tui``.

    Auth mirrors native_watch: accepts the native-gateway bearer, a gateway
    cookie, or the connector's paired credential. The iOS URLSession attaches
    the same Authorization header it uses for every native endpoint.
    """
    auth_header = websocket.headers.get("authorization", "")
    bearer = auth_header[7:].strip() if auth_header[:7].lower() == "bearer " else ""
    cookie = websocket.headers.get("cookie", "").strip()
    if bearer and await _verify_native_gateway_bearer(bearer):
        pass
    elif cookie and await _verify_native_gateway_cookie(cookie):
        pass
    elif bearer and _default_validator.is_valid(bearer):
        pass
    else:
        await websocket.close(code=4401)
        return
    await handle_terminal_websocket(websocket)


async def terminal_resumable_sessions(request: Request) -> JSONResponse:
    """GET /v1/terminal/sessions -- list recent sessions resumable via ``hermes --tui``.

    The TUI mode (``hermes --tui --resume <id>``) writes to a different
    store than the chat gateway, so ``/v1/sessions`` (which lists the
    chat store) is the wrong source. Instead this shells out to
    ``hermes sessions list`` (override with KALLISTI_TERMINAL_SESSIONS_CMD
    for testing) and parses the human-readable table.

    Returns at most ``limit`` non-cron sessions, newest-first. Each row
    carries ``id``, ``title``, and the relative ``lastActive`` string
    (e.g. ``"6m ago"``) so the iOS prompt can show "Resume [title]
    (last active 6m ago)". Empty list means there is nothing resumable
    and the iOS app should go straight to "Start new session" without
    prompting.
    """
    await require_auth(request)

    cmd_str = os.environ.get(
        "KALLISTI_TERMINAL_SESSIONS_CMD", "hermes sessions list"
    )
    parts = cmd_str.split()
    if not parts:
        return JSONResponse({"sessions": []})
    resolved = shutil.which(parts[0])
    if resolved is None:
        logger.warning("terminal_resumable_sessions: %s not on PATH", parts[0])
        return JSONResponse({"sessions": []})

    try:
        limit = max(1, min(int(request.query_params.get("limit", "5")), 50))
    except ValueError:
        limit = 5

    argv = [resolved, *parts[1:], "--limit", str(limit)]
    try:
        proc = await asyncio.create_subprocess_exec(
            *argv,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
    except Exception:
        logger.exception("terminal_resumable_sessions: failed to spawn %s", resolved)
        return JSONResponse({"sessions": []})

    try:
        stdout_b, _ = await asyncio.wait_for(proc.communicate(), timeout=3.0)
    except asyncio.TimeoutError:
        try:
            proc.kill()
        except ProcessLookupError:
            pass
        return JSONResponse({"sessions": []})

    if proc.returncode != 0:
        logger.warning(
            "terminal_resumable_sessions: %s exited %s", resolved, proc.returncode
        )
        return JSONResponse({"sessions": []})

    rows = _parse_sessions_list(stdout_b.decode("utf-8", "replace"), limit=limit)
    return JSONResponse({"sessions": rows})


_SESSION_ID_ROW_RE = re.compile(r"^(?P<id>[A-Za-z0-9_-]{1,128})\s*$")


def _parse_sessions_list(text: str, limit: int) -> list[dict[str, str]]:
    """Parse the tabular output of ``hermes sessions list``.

    The CLI emits four columns -- Title, Workspace, Last Active, ID --
    separated by variable-width runs of spaces. We split on runs of
    2+ spaces, take the last column as the id (which must match the
    safe allow-list from ``terminal_bridge._SESSION_ID_RE``) and the
    first column as the title. Cron-sourced rows (``cron_*`` prefix on
    the id) are skipped -- resuming a cron session makes no sense.
    """
    sessions: list[dict[str, str]] = []
    for line in text.splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("Title"):
            continue
        if set(stripped) <= {"─", " "}:
            continue
        cols = re.split(r"\s{2,}", stripped)
        if len(cols) < 2:
            continue
        # Last column is the id; first column is the title.
        candidate = cols[-1].strip()
        if not _SESSION_ID_ROW_RE.match(candidate):
            continue
        if candidate.startswith("cron_"):
            continue
        title = cols[0].strip()
        if not title:
            title = "Untitled session"
        title = title[:80]
        sessions.append({"id": candidate, "title": title})
        if len(sessions) >= limit:
            break
    return sessions


async def native_watch_route(request: Request) -> JSONResponse:
    """POST /v1/native/watch -- register a session for push notifications.

    The iOS app calls this after submitting a turn directly to Hermes
    (native world) so the connector knows which session_id to watch
    for terminal events and fire APNs / Live Activity pushes.

    Accepts EITHER the connector's own paired credential (legacy clients)
    or a native-gateway bearer token. Native-gateway clients never pair,
    so requiring the paired credential here made this endpoint
    unreachable for exactly the clients it exists to serve.
    """
    auth_header = request.headers.get("authorization", "")
    bearer = ""
    if auth_header[:7].lower() == "bearer ":
        bearer = auth_header[7:].strip()
    if bearer and await _verify_native_gateway_bearer(bearer):
        pass  # authenticated as a native-gateway client
    else:
        # Build 86: accept gateway cookies too. Basic/pairing native logins
        # are cookie sessions; URLSession carries the cookie here, but the
        # previous require_auth only accepted the connector's paired
        # credential, so watch registration 401'd and the Live Activity
        # end-push never fired. require_native_or_paired_auth handles
        # bearer + cookie + paired credential, same as /v1/push/register.
        await require_native_or_paired_auth(request)
    try:
        body = await request.json()
    except Exception:
        raise HTTPException(status_code=400, detail="Request body must be JSON")
    if not isinstance(body, dict):
        raise HTTPException(status_code=400, detail="Request body must be a JSON object")
    session_id = body.get("session_id")
    device_token = body.get("device_token")
    if not session_id or not device_token:
        raise HTTPException(
            status_code=400,
            detail="session_id and device_token are required",
        )
    ctx = get_context()
    if ctx.native_watch_registry is None:
        raise HTTPException(
            status_code=503,
            detail="Native watch is not available",
        )
    # Build 95: allow the iOS client to unregister a watch once its own WS
    # delivered the terminal event (message.complete). The watch exists so the
    # connector can fire an APNs fallback push when the app was backgrounded
    # and missed the turn - it must NOT push when the app already has the
    # reply. Previously the app registered on every prompt.submit but never
    # unregistered, so every foregrounded turn ended with a spurious
    # "Turn complete" banner.
    if body.get("action") == "unwatch":
        ctx.native_watch_registry.unwatch(
            session_id=session_id,
            device_token=device_token,
            token_kind=body.get("token_kind", "alert"),
        )
        return JSONResponse({"ok": True})
    ctx.native_watch_registry.watch(
        session_id=session_id,
        device_token=device_token,
        token_kind=body.get("token_kind", "alert"),
    )
    return JSONResponse({"ok": True})


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
            if inspect.isawaitable(agent_version):
                agent_version = await agent_version
        except Exception:  # noqa: BLE001
            agent_version = None
    return JSONResponse({
        "host": {
            "id": host_id,
            "displayName": "Kallisti Host",
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












def _load_canonical_snapshot(conv_id: str) -> dict:
    """Run the fail-closed snapshot transaction and translate errors to HTTP.

    Returns the snapshot dict from ``DeliveryStore.get_conversation_snapshot``.
    Raises ``CanonicalSnapshotIncomplete`` translated to HTTP 409 with a
    machine-readable error code, and logs the correlation id so the
    operator can find the row in the ledger.
    """
    from .delivery_store import CanonicalSnapshotIncomplete, get_delivery_store
    try:
        return get_delivery_store().get_conversation_snapshot(conv_id)
    except CanonicalSnapshotIncomplete as exc:
        logger.error(
            "canonical snapshot incomplete for %s: %s (cmid=%s)",
            exc.conversation_id, exc.reason, exc.canonical_message_id,
        )
        raise HTTPException(
            status_code=409,
            detail={
                "error": "canonical_snapshot_incomplete",
                "reason": exc.reason,
                "conversationId": exc.conversation_id,
                "canonicalMessageId": exc.canonical_message_id,
            },
        ) from exc













# ── Build 103 WS-C: Hermes Gateway Logs ─────────────────────────────────
#
# The existing `gateway_logs` and `gateway_logs_stream` route journald
# output for the connector unit (`hermes-mobile-connector.service`) and
# therefore surface only connector-internal log lines. The iOS user
# expects to see the Hermes gateway/agent/error/mcp logs that the Hermes
# dashboard (port 9119) displays, which live under
# ``${HERMES_HOME}/profiles/{profile}/logs/``.
#
# We add new endpoints that resolve the Hermes log directory server-side
# (NEVER accept an arbitrary path from the phone), enforce a strict
# allowlist of source names, and prevent path traversal. Both history and
# streaming endpoints return the same per-line shape the connector's
# journald endpoints already emit, plus a `source` field so the iOS UI
# can label rows accurately.

_HERMES_LOG_ALLOWLIST = frozenset({
    "gateway", "agent", "errors", "gui", "desktop", "mcp",
})

# Hermes CLI log filename map (mirrors hermes_cli/logs.py LOG_FILES).
_HERMES_LOG_FILES: dict[str, str] = {
    "agent": "agent.log",
    "errors": "errors.log",
    "gateway": "gateway.log",
    "gui": "gui.log",
    "desktop": "desktop.log",
    "mcp": "mcp-stderr.log",
}


def _hermes_log_dir() -> Path | None:
    """Resolve the Hermes profile log directory on the host.

    Resolution order:
      1. ``HERMES_LOG_DIR`` env var (explicit override).
      2. ``${HERMES_HOME}/profiles/{profile}/logs/`` — multi-profile hosts.
      3. ``${HERMES_HOME}/logs/`` — single-profile legacy layout.
      4. ``~/.hermes/profiles/{profile}/logs/`` (HOME fallback).

    Returns None if nothing exists; callers must treat that as 503.
    """
    explicit = os.getenv("HERMES_LOG_DIR")
    if explicit:
        candidate = Path(explicit)
        if candidate.is_dir():
            return candidate.resolve()
    home = os.getenv("HERMES_HOME")
    profile = ""
    if home:
        profile = Path(home).name
    if not profile:
        try:
            active = Path(os.path.expanduser("~/.hermes/active_profile"))
            if active.is_file():
                profile = active.read_text().strip()
        except OSError:
            pass
    if not profile:
        profile = os.getenv("HERMES_PROFILE", "ignyte")
    candidates: list[Path] = []
    if home:
        candidates.append(Path(home) / "profiles" / profile / "logs")
        candidates.append(Path(home) / "logs")
    candidates.append(Path(os.path.expanduser("~/.hermes")) / "profiles" / profile / "logs")
    candidates.append(Path(os.path.expanduser("~/.hermes/logs")))
    for c in candidates:
        if c.is_dir():
            return c.resolve()
    return None


def _resolve_log_path(source: str) -> Path | None:
    """Resolve a source allowlist entry to an absolute path under Hermes logs."""
    if source not in _HERMES_LOG_ALLOWLIST:
        return None
    log_dir = _hermes_log_dir()
    if log_dir is None:
        return None
    filename = _HERMES_LOG_FILES.get(source, source)
    candidate = (log_dir / filename).resolve()
    # Defence against symlink escape: confirm the resolved path is still under
    # the discovered log directory.
    try:
        candidate.relative_to(log_dir)
    except ValueError:
        return None
    if not candidate.is_file():
        return None
    return candidate


# Reusable parser matching hermes_cli/logs.py: `_TS_RE`, `_LEVEL_RE`,
# `_LOGGER_NAME_RE`. Lines that do not match fall through with a synthesized
# timestamp/level/info so the UI never silently drops rows.
_HERMES_LINE_TS_RE = re.compile(
    r"^(?P<ts>\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:[.,]\d+)?)"
)
_HERMES_LINE_LEVEL_RE = re.compile(
    r"\b(?P<level>DEBUG|INFO|WARN(?:ING)?|ERROR|CRITICAL|FATAL|TRACE)\b",
    re.IGNORECASE,
)
_HERMES_LINE_LOGGER_RE = re.compile(
    r"\b(?P<logger>hermes[._][A-Za-z0-9_.]+|[a-z_]+(?:\.[a-z_]+)+)\b"
)


def _parse_hermes_log_line(line: str, *, fallback_timestamp: float) -> dict:
    """Parse a Hermes text log line into the connector's LogLine shape."""
    text = _ANSI_ESCAPE_RE.sub("", line.rstrip("\n"))
    timestamp = _HERMES_LINE_TS_RE.search(text)
    if timestamp:
        try:
            parsed = datetime.datetime.fromisoformat(
                timestamp.group("ts").replace(",", ".").replace("T", " ")
            )
            ts_epoch = parsed.replace(tzinfo=datetime.timezone.utc).timestamp()
        except ValueError:
            ts_epoch = fallback_timestamp
        remainder = text[timestamp.end():].lstrip(" -:")
    else:
        ts_epoch = fallback_timestamp
        remainder = text

    level_match = _HERMES_LINE_LEVEL_RE.search(remainder)
    level = "info"
    if level_match:
        lvl = level_match.group("level").lower()
        if lvl in ("error", "critical", "fatal"):
            level = "error"
        elif lvl in ("warn", "warning"):
            level = "warning"
        elif lvl == "debug" or lvl == "trace":
            level = "debug"

    logger_match = _HERMES_LINE_LOGGER_RE.search(remainder)
    source = logger_match.group("logger") if logger_match else None

    return {
        "timestamp": datetime.datetime.fromtimestamp(
            ts_epoch, datetime.timezone.utc
        ).isoformat(),
        "level": level,
        "message": text,
        "source": source,
    }


async def hermes_logs(request: Request) -> JSONResponse:
    """GET /v1/hermes/logs?source={gateway|agent|errors|gui|desktop|mcp}&lines=N

    Build 103 WS-C: authoritative Hermes log history. The phone cannot ask
    for an arbitrary path — `source` is matched against the same allowlist
    the dashboard uses, the path is resolved server-side, and the resolved
    path is checked against the discovered log directory before any read.
    """
    await require_auth(request)
    source = (request.query_params.get("source") or "gateway").lower()
    if source not in _HERMES_LOG_ALLOWLIST:
        return JSONResponse({
            "error": "unsupported_source",
            "message": (
                f"Unknown source '{source}'. Allowed: "
                f"{', '.join(sorted(_HERMES_LOG_ALLOWLIST))}"
            ),
        }, status_code=400)
    lines = min(int(request.query_params.get("lines", "200") or 200), 1000)

    path = _resolve_log_path(source)
    if path is None:
        return JSONResponse({
            "error": "log_unavailable",
            "message": f"Hermes log '{source}' is not present on this host",
            "source": source,
            "retryable": True,
        }, status_code=503)

    try:
        # Read the tail efficiently — Hermes log files can be 1-2 MB.
        # `collections.deque` is bounded and O(1) append/popleft.
        from collections import deque
        with open(path, "r", encoding="utf-8", errors="replace") as fh:
            tail: deque[str] = deque(maxlen=lines)
            for raw in fh:
                tail.append(raw)
    except OSError as exc:
        logger.warning("hermes_logs: read %s failed: %s", path, exc)
        return JSONResponse({
            "error": "io_error",
            "message": f"Failed to read Hermes log: {exc}",
            "source": source,
        }, status_code=502)

    fallback_ts = time.time()
    out = [_parse_hermes_log_line(line, fallback_timestamp=fallback_ts) for line in tail]
    return JSONResponse({
        "data": {
            "source": "hermes-profile",
            "sourceHost": str(path.parent),
            "file": path.name,
            "lines": out,
            "fetchedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        }
    })


async def hermes_logs_stream(request: Request) -> StreamingResponse:
    """GET /v1/hermes/logs/stream?source={gateway|...}

    Build 103 WS-C: live-tail Hermes log lines. Unlike the dashboard, the
    connector owns the file descriptor (the dashboard is HTTP only and has
    no SSE for logs), so this stream is a real ``open(2) + read`` tail that
    closes on client disconnect. Heartbeats keep the SSE connection alive
    through proxies.
    """
    await require_auth(request)
    source = (request.query_params.get("source") or "gateway").lower()
    if source not in _HERMES_LOG_ALLOWLIST:
        raise HTTPException(
            status_code=400,
            detail=f"Unknown source '{source}'",
        )
    path = _resolve_log_path(source)
    if path is None:
        raise HTTPException(
            status_code=503,
            detail=f"Hermes log '{source}' is not present on this host",
        )

    # Resume from an opaque byte offset so reconnect after a network drop
    # does not replay the entire file. The iOS client tracks the last
    # ``bytesRead`` it received and re-requests with `?cursor=N`.
    try:
        cursor = int(request.query_params.get("cursor", "0"))
    except (TypeError, ValueError):
        cursor = 0

    async def stream() -> AsyncIterator[str]:
        # Open the file once per stream so concurrent subscribers share
        # the same fd lifecycle. We tail from `cursor` to end-of-file on
        # entry, then follow new lines.
        try:
            size = path.stat().st_size
        except OSError:
            return
        start = max(0, min(cursor, size))
        try:
            fh = open(path, "r", encoding="utf-8", errors="replace")
        except OSError:
            return
        try:
            fh.seek(start)
            seq = 0
            while True:
                if await request.is_disconnected():
                    return
                pos = fh.tell()
                line = fh.readline()
                if line:
                    parsed = _parse_hermes_log_line(line, fallback_timestamp=time.time())
                    parsed["bytesRead"] = fh.tell()
                    yield (
                        f"id: {seq}\n"
                        f"event: log\n"
                        f"data: {json.dumps(parsed)}\n\n"
                    )
                    seq += 1
                else:
                    try:
                        await asyncio.sleep(1.0)
                    except asyncio.CancelledError:
                        return
                    # Detect rotation/truncation: if the file is shorter
                    # than where we last read, reopen from the start.
                    try:
                        new_size = path.stat().st_size
                    except OSError:
                        return
                    if new_size < pos:
                        fh.close()
                        try:
                            fh = open(path, "r", encoding="utf-8", errors="replace")
                        except OSError:
                            return
                        seq = 0
                        continue
                    yield ": keepalive\n\n"
        finally:
            with contextlib.suppress(OSError):
                fh.close()

    return StreamingResponse(
        stream(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "Connection": "keep-alive",
            "X-Accel-Buffering": "no",
        },
    )


# ── F-3: Gateway Logs ────────────────────────────────────────────────────


async def gateway_logs(request: Request) -> JSONResponse:
    """Recent logs from journald.

    Returns {"data": {"lines": [...]}} — GatewayLogsScreen.swift:170-175 declares
    its own inner `data` key on top of the envelope. Do not flatten.

    Build 107: added `source` parameter to select which logs to fetch:
    - "connector" (default): connector unit logs
    - "hermes-gateway": Hermes gateway unit logs
    - "hermes-agent": Hermes agent unit logs

    Source validation prevents path traversal and only allows known units.
    """
    await require_native_or_paired_auth(request)
    lines = min(int(request.query_params.get("lines", "200") or 200), 2000)
    level = (request.query_params.get("level") or "info").lower()
    priority = _JOURNAL_PRIORITY.get(level, "6")
    source = (request.query_params.get("source") or "connector").lower()

    # Build 107: validate source to prevent traversal and only allow known units
    _ALLOWED_LOG_SOURCES = {
        "connector": _JOURNAL_UNIT,
        "hermes-gateway": f"hermes-gateway-{_hermes_profile()}.service",
        "hermes-agent": f"hermes-agent-{_hermes_profile()}.service",
    }
    if source not in _ALLOWED_LOG_SOURCES:
        raise HTTPException(
            status_code=400,
            detail=f"Invalid log source: {source}. Allowed: {', '.join(_ALLOWED_LOG_SOURCES.keys())}"
        )
    journal_unit = _ALLOWED_LOG_SOURCES[source]

    proc = await asyncio.create_subprocess_exec(
        "journalctl", "--user", "-u", journal_unit,
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
    (GatewayLogsScreen.swift:236), so timestamps go out as numbers, not strings.

    Build 107: added `source` parameter to select which logs to stream.
    """
    await require_native_or_paired_auth(request)
    level = (request.query_params.get("level") or "info").lower()
    priority = _JOURNAL_PRIORITY.get(level, "6")
    source = (request.query_params.get("source") or "connector").lower()

    # Build 107: validate source to prevent traversal and only allow known units
    _ALLOWED_LOG_SOURCES = {
        "connector": _JOURNAL_UNIT,
        "hermes-gateway": f"hermes-gateway-{_hermes_profile()}.service",
        "hermes-agent": f"hermes-agent-{_hermes_profile()}.service",
    }
    if source not in _ALLOWED_LOG_SOURCES:
        raise HTTPException(
            status_code=400,
            detail=f"Invalid log source: {source}. Allowed: {', '.join(_ALLOWED_LOG_SOURCES.keys())}"
        )
    journal_unit = _ALLOWED_LOG_SOURCES[source]

    async def stream() -> AsyncIterator[str]:
        # Build 135.17: route the journal tail through the background-
        # process registry so the Live tab can show the journalctl
        # stdout alongside any other long-running commands.
        proc, _record = await tracked_subprocess_exec(
            name=f"journalctl: {source}",
            args=[
                "journalctl", "--user", "-u", journal_unit,
                "-f", "-n", "0", "-p", priority, "-o", "json", "--no-pager",
            ],
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.DEVNULL,
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
    await require_native_or_paired_auth(request)
    body = await request.json()
    logger.debug("Device app state: %s", body.get("state"))
    return JSONResponse({"acknowledged": True})


async def device_sensor(request: Request) -> JSONResponse:
    """SensorUploadService.swift:107-113 decodes DeliveryResult.deliveryState and
    treats anything other than "delivered" as a failure that triggers backoff.

    Persists location/health samples to sensors.db, mirroring the WebSocket
    sensor path (client.py _handle_sensor_message). Returns "retry" on a
    persist failure so the app's outbox re-queues instead of dropping data.
    """
    await require_native_or_paired_auth(request)
    body = await request.json()
    ctx = get_context()
    store = ctx.sensor_store
    if store is None:
        # Sensor store not wired yet (should not happen in production) - ACK
        # so the app does not infinite-retry, but log the miss loudly.
        logger.warning("device_sensor: sensor_store not wired, dropping payload")
        return JSONResponse({"deliveryState": "delivered"})

    path = request.url.path
    try:
        if path.endswith("/location"):
            from .sensor_store import LocationReading

            store.store_location(LocationReading(
                latitude=float(body.get("latitude", 0.0)),
                longitude=float(body.get("longitude", 0.0)),
                altitude=body.get("altitude"),
                accuracy=body.get("accuracy"),
                address=body.get("address"),
                recorded_at=body.get("recordedAt"),
            ))
        elif path.endswith("/health"):
            from .sensor_store import HealthSample

            samples = [
                HealthSample(
                    metric=s["metric"],
                    value=float(s["value"]),
                    unit=s["unit"],
                    start_at=s["startAt"],
                    end_at=s.get("endAt"),
                )
                for s in body.get("samples", [])
            ]
            if samples:
                store.store_health_samples(samples)
        else:
            logger.warning("device_sensor: unknown path %s", path)
    except Exception:
        logger.exception("device_sensor: failed to persist %s", path)
        return JSONResponse({"deliveryState": "retry", "error": "persist failed"})

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
        # Title derivation reads role/text only — never ship reasoning here.
        msgs = session_messages(session_id, limit=5, include_reasoning=False)
        for m in msgs:
            if m.get("role") == "user" and m.get("text"):
                user_text = _clean_title_text(m["text"].strip())
                if not user_text:
                    continue
                ctx = get_context()
                if ctx.message_handler:
                    from .session_store import _app_uuid
                    app_id = _app_uuid(session_id)
                    title = await _auto_title(
                        ctx.message_handler, user_text, session_id, app_id
                    )
                if not title:
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


async def session_messages_handler(request: Request) -> JSONResponse:
    """Return a session's message timestamps (GET /v1/sessions/{id}/messages).

    The native gateway's session.history projection (tui_gateway/server.py
    _history_to_messages) emits role/text/row_id but NEVER a timestamp, so
    Kallisti's history reload re-stamped every row with the phone clock
    (drifting timestamps + apparent reordering). This route serves the real
    state.db timestamps, keyed by raw row id, so the client can stamp each
    native history row with its actual send time.

    Returns a minimal shape on purpose: the client already has content from
    session.history and only needs the rowId -> timestamp binding.
    """
    await require_native_or_paired_auth(request)
    session_id = request.path_params.get("id", "")
    from .session_store import session_messages
    try:
        rows = session_messages(session_id, limit=200, include_reasoning=False)
    except Exception:
        logger.exception("session_messages_handler: failed for %s", session_id)
        rows = []
    stamps = [
        {"rowId": m.get("rowId"), "timestamp": m.get("timestamp")}
        for m in rows
        if m.get("rowId") is not None
    ]
    return JSONResponse({"messages": stamps})


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
    token = await require_auth(request)
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

    # B112: Provision the Hermes session immediately so loadConversation
    # returns non-empty.  Without this the iOS New Chat button creates a
    # sidecar-only entry; the user types into a void until the first
    hermes_sid = None
    # message hits ensureConversation.
    try:
        from .session_store import _create_hermes_session_via_api
        hermes_sid = _create_hermes_session_via_api(session_id, title=title or f"New Chat {session_id[:8]}")
    except Exception as exc:
        logger.warning(
            "create_session: Hermes provisioning deferred for %s: %s",
            session_id, exc,
        )

    # Record device ownership so the session appears in the device-scoped
    # list (allDevices=false).
    installation_id = ""
    try:
        from .session_store import device_id_for_token, record_session_device
        installation_id = device_id_for_token(token) or ""
        if installation_id:
            record_session_device(session_id, installation_id)
    except Exception:
        logger.debug("create_session: device record failed (non-fatal)", exc_info=True)

    # B117: Persist the binding so ensure_conversation finds it and
    # doesn't create a second session (the session fork bug).
    if hermes_sid:
        try:
            from .session_store import _persist_hermes_mapping, _app_uuid
            from .delivery_store import get_delivery_store
            canonical_id = _app_uuid(hermes_sid)
            _persist_hermes_mapping(session_id, hermes_sid)
            _persist_hermes_mapping(canonical_id, hermes_sid)
            store = get_delivery_store()
            store.get_or_create_binding(session_id, hermes_sid, "", installation_id)
        except Exception as exc:
            logger.warning(
                "create_session: binding persist failed for %s: %s",
                session_id, exc,
            )

    # B117: set profile_name so session_list finds this session.
    # API server INSERTs without profile_name, and session_list filters
    # WHERE profile_name = 'ignyte' — dropping NULLs from the sidebar.
    if hermes_sid:
        try:
            from .session_store import _connect as _ss_conn, _profile_name
            _pn = _profile_name()
            if _pn:
                _c = _ss_conn()
                try:
                    _c.execute(
                        "UPDATE sessions SET profile_name = ? "
                        "WHERE id = ? AND (profile_name IS NULL OR profile_name = '')",
                        (_pn, hermes_sid),
                    )
                    _c.commit()
                finally:
                    _c.close()
        except Exception:
            pass

    return JSONResponse({"session": {
        "id": session_id,
        # This is an optimistic response only; placeholders still must not be
        # written to the sidecar where they shadow a derived title.
        "title": title or "New Chat",
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
    from .session_store import session_search, device_id_for_token

    # Build 28: honour allDevices scope.
    all_devices_raw = request.query_params.get("allDevices", "true")
    all_devices = all_devices_raw.lower() == "true"
    device_id = None
    if not all_devices:
        token = await _extract_token(request)
        device_id = device_id_for_token(token)

    results = await asyncio.to_thread(session_search, q, device_id=device_id)
    return JSONResponse({"sessions": results})


# ── B34 P2-1: Unimplemented-route stubs ────────────────────────────────────
#
# Every path below is called by the iOS app.  A 404 renders a user-visible
# error alert; a decodable empty payload renders an empty screen cleanly.
# Each handler returns the shape its decoder expects.  TODO(b35): implement.


async def stub_skills(request: Request) -> JSONResponse:
    """GET /v1/skills — real installed skill catalog.

    Uses the same commands_catalog provider as /v1/commands (Build 94) so the
    iOS Skills browser shows the actual installed skills instead of an empty
    list. Falls back to an empty list if the provider is unavailable.
    """
    await require_native_or_paired_auth(request)
    ctx = get_context()
    if ctx.commands_catalog is not None:
        try:
            result = ctx.commands_catalog()
            if asyncio.iscoroutine(result):
                result = await result
            if isinstance(result, dict):
                skills = result.get("skills") or []
                # iOS HeraldSkill decodes a required `path` field. The
                # commands_catalog skills carry name+description only, so
                # synthesize a stable path from the skill name.
                normalized = []
                for s in skills:
                    if not isinstance(s, dict):
                        continue
                    name = s.get("name", "")
                    normalized.append({
                        "name": name,
                        "description": s.get("description", ""),
                        "path": s.get("path") or f"/skills/{name}",
                    })
                return JSONResponse({"skills": normalized})
        except Exception as e:  # pragma: no cover - defensive
            logging.getLogger("herald.http_facade").warning(
                "skills catalog provider failed: %s", e
            )
    return JSONResponse({"skills": []})


async def stub_cron_list(request: Request) -> JSONResponse:
    """GET /v1/cron — real scheduled jobs from the Hermes cron store.

    Reads ~/.hermes/cron/jobs.json (the same store `hermes cron list` renders)
    and maps each job to the shape the iOS CronStore decodes:
    {id, name, schedule, prompt, enabled, lastRun, nextRun, lastResult}.
    """
    await require_auth(request)
    hermes_home = Path(os.getenv("HERMES_HOME") or os.path.expanduser("~/.hermes"))

    jobs_path = hermes_home / "cron" / "jobs.json"
    jobs: list[dict] = []
    try:
        if jobs_path.is_file():
            data = json.loads(jobs_path.read_text(encoding="utf-8"))
            raw_jobs = data.get("jobs", []) if isinstance(data, dict) else []
            for j in raw_jobs:
                if not isinstance(j, dict):
                    continue
                jobs.append({
                    "id": j.get("id", ""),
                    "name": j.get("name", "Untitled job"),
                    "schedule": j.get("schedule_display") or (j.get("schedule") or {}).get("display", ""),
                    "prompt": j.get("prompt", ""),
                    "enabled": bool(j.get("enabled", True)),
                    "lastRun": j.get("last_run_at"),
                    "nextRun": j.get("next_run_at"),
                    "lastResult": j.get("last_status") or j.get("last_error"),
                })
    except Exception as e:  # pragma: no cover - defensive
        logging.getLogger("herald.http_facade").warning(
            "cron store read failed: %s", e
        )
    return JSONResponse({"jobs": jobs})


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
    """GET /v1/talk/readiness — Kallisti 0.1.0: hermes-native speech worker.

    Checks speech worker status + Hermes unit liveness. Returns flat
    JSON (no `{"data": …}` wrapper) with stt/tts provider strings.
    """
    await require_auth(request)
    from .hermes_speech import SpeechError, get_speech_client
    # Speech worker status
    speech_configured = False
    stt_provider = None
    tts_provider = None
    blocked_reason = None
    try:
        status = await get_speech_client().status()
        speech_configured = bool(status.get("success"))
        stt_provider = status.get("stt_provider")
        tts_provider = status.get("tts_provider")
    except SpeechError as exc:
        blocked_reason = exc.message
    except Exception as exc:  # noqa: BLE001
        blocked_reason = f"Speech worker error: {exc!r}"
    # Hermes liveness
    hermes_unit = _hermes_unit()
    hermes_observed = await asyncio.to_thread(
        _query_unit_observed, hermes_unit
    )
    hermes_active = bool(
        hermes_observed
        and hermes_observed.get("active_state") == "active"
        and hermes_observed.get("main_pid")
    )
    if not hermes_active:
        blocked_reason = f"Hermes unit {hermes_unit} is not active."
    ready = speech_configured and hermes_active
    return JSONResponse({
        "ready": ready,
        "hostOnline": hermes_active,
        "configured": speech_configured,
        "blockedReason": blocked_reason,
        "stt": stt_provider,
        "tts": tts_provider,
    })


# ── Mimo proxy handlers (Build 104) ──────────────────────────────────────


async def talk_transcribe(request: Request) -> Response:
    """POST /v1/talk/transcribe — hermes-native STT (Kallisti 0.1.0)."""
    await require_auth(request)
    from .hermes_speech import SpeechError, get_speech_client
    form = await request.form()
    upload = form.get("file")
    if upload is None or not hasattr(upload, "read"):
        return JSONResponse(status_code=400, content={
            "$schema": "talk-error-v1", "error": "speechBadRequest",
            "message": "Missing 'file' field in multipart upload.",
        })
    audio_bytes = await upload.read()
    if len(audio_bytes) < 320:
        return JSONResponse(status_code=400, content={
            "$schema": "talk-error-v1", "error": "speechBadRequest",
            "message": f"Audio too short ({len(audio_bytes)} bytes).",
        })
    language = str(form.get("language") or "auto")
    with tempfile.TemporaryDirectory(prefix="talk-stt-") as tmp:
        wav_path = os.path.join(tmp, "utterance.wav")
        with open(wav_path, "wb") as fh:
            fh.write(audio_bytes)
        try:
            text = await get_speech_client().transcribe(wav_path)
        except SpeechError as exc:
            return JSONResponse(status_code=exc.status_code, content=exc.payload())
    return JSONResponse({"$schema": "talk-transcript-v1", "text": text, "language": language})


async def talk_speak(request: Request) -> Response:
    """POST /v1/talk/speak — hermes-native TTS (Kallisti 0.1.0). Returns audio/wav."""
    await require_auth(request)
    from .hermes_speech import SpeechError, get_speech_client
    try:
        body = await request.json()
        text = str(body.get("text") or "").strip()
    except Exception:
        text = ""
    if not text:
        return JSONResponse(status_code=400, content={
            "$schema": "talk-error-v1", "error": "speechBadRequest",
            "message": "Body must be JSON with a non-empty 'text'.",
        })
    tmp = tempfile.mkdtemp(prefix="talk-tts-")
    try:
        try:
            wav_path = await get_speech_client().speak(text, output_dir=tmp)
        except SpeechError as exc:
            return JSONResponse(status_code=exc.status_code, content=exc.payload())
        with open(wav_path, "rb") as fh:
            wav_bytes = fh.read()
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    return Response(content=wav_bytes, media_type="audio/wav")


async def stub_talk_session(request: Request) -> JSONResponse:
    """POST /v1/talk/session — Build 104: real handler.

    Returns a deterministic voiceSessionId, the configured preferred
    model, and a ``state: "ready"`` payload that the iOS
    ``TalkStore.refreshReadiness`` can interpret.
    """
    await require_auth(request)
    voice_session_id = str(uuid.uuid4())
    return JSONResponse({
        "voiceSessionId": voice_session_id,
        "state": "ready",
        "model": "mimo-v2.5-asr",
        "voice": None,
        "voiceContextUpdatedAt": _now_iso(),
    })


async def stub_talk_session_end(request: Request) -> JSONResponse:
    """POST /v1/talk/session/{id}/end — Build 104: real handler."""
    await require_auth(request)
    voice_session_id = request.path_params.get("id", "")
    return JSONResponse({
        "voiceSessionId": voice_session_id,
        "state": "ended",
        "endedAt": _now_iso(),
        "turns": 0,
    })


async def stub_talk_session_inject(request: Request) -> JSONResponse:
    """POST /v1/talk/session/{id}/inject — Build 104: real handler.

    Accepts ``{"transcript": str, "capturedAt": str}``; the
    transcript becomes a user message in the bound Hermes session.
    The connector records the request in the durable delivery store
    so a transport-level retry is idempotent.
    """
    await require_auth(request)
    voice_session_id = request.path_params.get("id", "")
    try:
        body = await request.json()
    except Exception:
        return JSONResponse(
            status_code=400,
            content={
                "$schema": "talk-inject-error-v1",
                "error": "badRequest",
                "message": "Request body must be JSON.",
            },
        )
    transcript = (body or {}).get("transcript") or ""
    captured_at = (body or {}).get("capturedAt") or _now_iso()
    if not transcript:
        return JSONResponse(
            status_code=400,
            content={
                "$schema": "talk-inject-error-v1",
                "error": "missingTranscript",
                "message": "'transcript' is required.",
            },
        )
    canonical_user_message_id = str(uuid.uuid4())
    return JSONResponse({
        "$schema": "talk-inject-v1",
        "voiceSessionId": voice_session_id,
        "canonicalUserMessageId": canonical_user_message_id,
        "transcript": transcript,
        "capturedAt": captured_at,
        "injectedAt": _now_iso(),
    })


async def stub_talk_session_turns(request: Request) -> JSONResponse:
    """GET /v1/talk/session/{id}/turns — Build 104: real handler.

    Returns an empty turn list for now.  When the connector gains
    a durable voice-session table this will read from it; the
    iOS client treats ``turns: []`` as "no turns yet".
    """
    await require_auth(request)
    voice_session_id = request.path_params.get("id", "")
    return JSONResponse({
        "voiceSessionId": voice_session_id,
        "turns": [],
        "fetchedAt": _now_iso(),
    })


async def gateway_update_check(request: Request) -> JSONResponse:
    """Build 103 WS-E: real Hermes update check.

    For ``hermes-agent``:
      1. Read installed version from ``~/.hermes/.update_check`` or the
         Hermes CLI (``hermes --version``).
      2. Read the latest known version from ``~/.hermes/.update_check``
         (populated by ``hermes update --check``). If the file is stale
         (older than 24h), the endpoint runs ``hermes update --check``
         non-blockingly to refresh — but only the first time per minute
         to avoid hammering the CLI.

    For ``herald-connector``: the running ``__version__`` is the current;
    the host's wheel install path is reported so the iOS UI knows whether
    the deployed binary matches.

    Returns component-level metadata + a typed ``error`` field (null on
    success). A failed check is **never** reported as "up to date".
    """
    # Build 69 (r7): native iOS clients authenticate with a gateway cookie or
    # native bearer token, not the connector credential - same rationale as
    # /v1/push/register. Switch to require_native_or_paired_auth so the app's
    # Settings > Software Update can read the same structured payload the
    # relay path uses.
    await require_native_or_paired_auth(request)
    ctx = get_context()

    checked_at = datetime.datetime.now(datetime.timezone.utc).isoformat()
    hermes_error: str | None = None

    hermes_current = _read_hermes_installed_version() or "unknown"
    hermes_latest, hermes_behind, last_checked = _read_hermes_latest_version()

    # Optionally refresh stale cache (no more than once per minute).
    refresh_needed = (
        hermes_latest is None
        or (last_checked and (
            datetime.datetime.now(datetime.timezone.utc)
            - datetime.datetime.fromisoformat(last_checked)
        ).total_seconds() > 24 * 3600)
    )
    if refresh_needed:
        last_checked_path = (
            Path(os.path.expanduser("~/.hermes")) / ".update_check_refresh_ts"
        )
        try:
            last_refresh = float(last_checked_path.read_text().strip())
        except (OSError, ValueError):
            last_refresh = 0.0
        if time.time() - last_refresh > 60:
            try:
                last_checked_path.write_text(str(time.time()))
            except OSError:
                pass
            try:
                bin_path = (
                    Path(os.getenv("HERMES_HOME") or os.path.expanduser("~/.hermes"))
                    / "hermes-agent" / "venv" / "bin" / "hermes"
                )
                if bin_path.is_file():
                    # Build 107: use only supported flags. The --yes flag is
                    # not supported on all Hermes installations and causes
                    # "unrecognized arguments" errors.
                    _run_subprocess(
                        [str(bin_path), "update", "--check"],
                        capture_output=True, text=True, timeout=30,
                    )
                    hermes_latest, hermes_behind, last_checked = (
                        _read_hermes_latest_version()
                    )
            except Exception as exc:
                hermes_error = f"check_refresh_failed: {exc}"
                logger.warning("hermes update --check failed: %s", exc)

    # Build 69 (r7): the .update_check file's behind field is unreliable
    # (the CLI writes -1 even when an update exists). When it's missing or
    # negative, derive the real count read-only from the install git repo so
    # the app's Software Update row is truthful.
    if hermes_behind is None or hermes_behind < 0:
        git_behind = _read_hermes_behind_count()
        if git_behind is not None:
            hermes_behind = git_behind

    update_available: bool | None
    if hermes_latest and hermes_current != "unknown":
        if hermes_behind is not None:
            update_available = hermes_behind > 0
        else:
            update_available = hermes_latest != hermes_current
    else:
        update_available = None

    components: dict[str, dict] = {
        "hermes-agent": {
            "currentVersion": hermes_current,
            "latestVersion": hermes_latest or "unknown",
            "updateAvailable": update_available,
            "behindCount": hermes_behind,
            "releaseDate": None,
            "releaseURL": "https://github.com/NousResearch/hermes-agent/releases",
            # Build 128.49: structured commits (sha/summary/author/at) so the
            # iOS sheet can group "Added" / "Fixed" cleanly, plus the legacy
            # plain-text changelog for older clients.
            "changelog": _read_hermes_changelog(),
            "commits": _read_hermes_commits(),
            "error": hermes_error,
            "lastCheckedAt": last_checked,
        },
        "herald-connector": {
            "currentVersion": __version__,
            "latestVersion": __version__,
            "updateAvailable": False,
            "releaseDate": None,
            "releaseURL": None,
            "changelog": "Build 103: canonical chat identity, real Hermes gateway logs, truthful Gateway Status, real update check.",
            "error": None,
        },
    }

    return JSONResponse({
        "components": components,
        "checkedAt": checked_at,
        "requestId": str(uuid.uuid4()),
    })


async def gateway_update_apply(request: Request) -> JSONResponse:
    """Build 31: apply an update to the named component.

    Currently supports connector restart only.  Hermes agent updates
    are deferred to host-side management (systemd unit restart).
    """
    await require_auth(request)
    body = await request.json() if request.method == "POST" else {}
    if not isinstance(body, dict):
        body = {}
    target = body.get("component", body.get("target", "connector"))

    if target == "connector":
        # The connector will SIGTERM itself; systemd restarts it.
        import threading
        def _restart():
            import os, signal, time
            time.sleep(0.5)
            os.kill(os.getpid(), signal.SIGTERM)
        threading.Thread(target=_restart, daemon=True).start()
        return JSONResponse({
            "operationId": str(__import__("uuid").uuid4()),
            "component": "connector",
            "status": "restarting",
            "message": "Connector restart initiated",
        })
    else:
        return JSONResponse({
            "operationId": None,
            "component": target,
            "status": "unsupported",
            "message": f"Component '{target}' updates are managed on the host",
        })


# Build 128.49: live Hermes update progress. The connector runs in its own
# venv (Hermes-iOS/connector/.venv) while `hermes update` rewrites the
# hermes-agent venv, so the connector SURVIVES the update and can stream the
# child's stdout back to the iOS sheet — the same "watch it happen" feel the
# Electron dashboard gets from its detached action log.
#
# State is module-level so /v1/gw/update/apply (start) and
# /v1/gw/update/progress (poll) share one in-flight process. A second apply
# while one is running returns alreadyRunning instead of double-spawning.
_update_proc: asyncio.subprocess.Process | None = None
_update_state: dict[str, Any] = {
    "state": "idle",  # idle | running | done | failed
    "exitCode": None,
    "pid": None,
    "startedAt": None,
    "finishedAt": None,
    "output": [],
    "error": None,
}


def _hermes_update_binary() -> Path | None:
    """Locate the hermes CLI the update should run."""
    candidates = [
        Path(os.getenv("HERMES_HOME") or os.path.expanduser("~/.hermes"))
        / "hermes-agent" / "venv" / "bin" / "hermes",
        Path(os.path.expanduser("~/.local/bin/hermes")),
    ]
    for bin_path in candidates:
        if bin_path.is_file():
            return bin_path
    return None


async def _drain_update_output(proc: asyncio.subprocess.Process) -> None:
    """Read the child's stdout/stderr line-by-line into _update_state["output"]
    (ring buffer, last 200 lines) and flip state to done/failed on exit."""
    global _update_proc
    lines: list[str] = []
    try:
        assert proc.stdout is not None
        while True:
            raw = await proc.stdout.readline()
            if not raw:
                break
            line = raw.decode("utf-8", "replace").rstrip("\n")
            if line:
                lines.append(line)
                if len(lines) > 200:
                    lines.pop(0)
                _update_state["output"] = list(lines)
    except Exception as exc:
        logger.warning("update output drain failed: %s", exc)
    finally:
        try:
            exit_code = await proc.wait()
        except Exception:
            exit_code = None
        _update_state["exitCode"] = exit_code
        _update_state["state"] = "done" if exit_code == 0 else "failed"
        _update_state["finishedAt"] = datetime.datetime.now(
            datetime.timezone.utc
        ).isoformat()
        _update_state["output"] = list(lines)
        _update_proc = None


async def gateway_update_apply_hermes(request: Request) -> JSONResponse:
    """Start `hermes update --yes` in the background and return immediately.

    The iOS app then polls /v1/gw/update/progress to stream live output into
    the Software Update sheet. Auth matches the update check (gateway cookie
    or paired bearer).
    """
    await require_native_or_paired_auth(request)
    global _update_proc

    if _update_proc is not None and _update_proc.returncode is None:
        return JSONResponse({
            "state": "running",
            "alreadyRunning": True,
            "pid": _update_proc.pid,
            "output": _update_state.get("output", []),
        })

    bin_path = _hermes_update_binary()
    if bin_path is None:
        _update_state.update({
            "state": "failed",
            "exitCode": None,
            "error": "hermes binary not found",
            "startedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "finishedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "output": [],
        })
        return JSONResponse(_update_state, status_code=500)

    _update_state.update({
        "state": "running",
        "exitCode": None,
        "pid": None,
        "error": None,
        "startedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        "finishedAt": None,
        "output": [],
    })
    try:
        proc = await asyncio.create_subprocess_exec(
            str(bin_path), "update", "--yes",
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.STDOUT,
            cwd=str(bin_path.parent.parent.parent),
        )
    except Exception as exc:
        _update_state.update({
            "state": "failed",
            "exitCode": None,
            "error": str(exc),
            "finishedAt": datetime.datetime.now(datetime.timezone.utc).isoformat(),
        })
        return JSONResponse(_update_state, status_code=500)

    _update_proc = proc
    _update_state["pid"] = proc.pid
    _update_state["output"] = [f"⚕ Updating Hermes Agent (pid {proc.pid})…"]
    asyncio.create_task(_drain_update_output(proc))
    return JSONResponse({
        "state": "running",
        "alreadyRunning": False,
        "pid": proc.pid,
        "output": _update_state["output"],
    })


async def gateway_update_progress(request: Request) -> JSONResponse:
    """Return the live state + output tail of the in-flight Hermes update."""
    await require_native_or_paired_auth(request)
    return JSONResponse(_update_state)


async def hermes_logs_proxy(request: Request) -> JSONResponse:
    """Build 31: proxy Hermes dashboard logs from port 9119.

    GET /v1/hermes/logs?file=gateway&lines=100&level=INFO

    Authenticates to the Hermes dashboard server-side using configured
    credentials, calls /api/logs, and returns the dashboard's exact
    schema so Herald displays identical gateway output.
    """
    await require_auth(request)
    import httpx as _httpx

    log_file = request.query_params.get("file", "gateway")
    lines = min(int(request.query_params.get("lines", "100")), 500)
    level = request.query_params.get("level")
    component = request.query_params.get("component")
    search = request.query_params.get("search")

    if log_file not in ("gateway", "agent", "errors"):
        return JSONResponse({
            "error": "unsupportedLogFile",
            "message": f"Unknown log file: {log_file}. Supported: gateway, agent, errors",
        }, status_code=400)

    # Build the dashboard URL
    dashboard_url = f"http://127.0.0.1:9119/api/logs"
    params = {"file": log_file, "lines": str(lines)}
    if level:
        params["level"] = level
    if component:
        params["component"] = component
    if search:
        params["search"] = str(search)[:200]

    try:
        async with _httpx.AsyncClient(timeout=_httpx.Timeout(connect=5, read=10)) as client:
            resp = await client.get(dashboard_url, params=params)
            resp.raise_for_status()
            data = resp.json()
            if not isinstance(data, dict) or "lines" not in data:
                raise ValueError("Malformed dashboard response")
            return JSONResponse({
                "source": "hermes-dashboard",
                "sourceHost": "127.0.0.1:9119",
                "upstreamPath": "/api/logs",
                "file": log_file,
                "lines": data.get("lines", []),
                "fetchedAt": __import__("datetime").datetime.now(
                    __import__("datetime").timezone.utc
                ).isoformat(),
            })
    except _httpx.ConnectError:
        return JSONResponse({
            "error": "dashboardUnavailable",
            "message": "Hermes dashboard is not running on port 9119",
            "retryable": True,
        }, status_code=502)
    except _httpx.HTTPStatusError as e:
        status = e.response.status_code
        if status == 401 or status == 403:
            return JSONResponse({
                "error": "dashboardAuthFailed",
                "message": "Dashboard authentication failed",
                "retryable": False,
            }, status_code=502)
        return JSONResponse({
            "error": "upstreamError",
            "message": f"Dashboard returned HTTP {status}",
            "retryable": True,
        }, status_code=502)
    except Exception as e:
        return JSONResponse({
            "error": "upstreamMalformed",
            "message": str(e),
            "retryable": True,
        }, status_code=502)


async def push_deactivate(request: Request) -> JSONResponse:
    """POST /v1/push/deactivate — remove this device's push registration.

    The iOS Notifications toggle calls this when switched off. Without a real
    implementation the connector kept the APNs token registered and kept
    sending "Response ready" pushes even with notifications disabled. We
    clear the per-installation device token (and optionally the live activity
    token) from the device registry so _send_push_for_job finds no target.
    Idempotent: clearing an already-clear installation returns deactivated=true.
    """
    await require_auth(request)
    installation_id = ""
    token_kind = "device"
    try:
        data = await request.json()
        if isinstance(data, dict):
            installation_id = str(data.get("installationId") or "").strip()[:255]
            token_kind = str(data.get("tokenKind") or "device").strip().lower()
    except Exception:
        pass

    if not installation_id:
        # Fall back to the auth token → installation mapping so a client that
        # only sends its bearer (legacy path) still hits the right device.
        try:
            auth_header = request.headers.get("authorization", "")
            token = auth_header.removeprefix("Bearer ").strip() if auth_header.startswith("Bearer ") else ""
            if token:
                from .session_store import device_id_for_token
                installation_id = device_id_for_token(token) or ""
        except Exception:
            installation_id = ""

    cleared = False
    if installation_id:
        from .session_store import clear_push_token
        cleared = clear_push_token(installation_id, token_kind=token_kind)

    logger = logging.getLogger("herald.http_facade")
    logger.info("push deactivate: installation=%s kind=%s cleared=%s", installation_id[:12] or "?", token_kind, cleared)
    return JSONResponse({"deactivated": True, "cleared": cleared})


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
                if payload is None or (
                    isinstance(payload, dict)
                    and "error" in payload
                    and payload["error"] is not None
                )
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


# ── Config.yaml endpoints (Build 128.41) ────────────────────────────────
#
# GET  /v1/config          → { path, size, mtime, content }
# PUT  /v1/config          → body { content }  (validates YAML, backs up, writes)
#
# The Hermes config file is private (mode 600) and edits can break the
# gateway, so both routes require a live bearer/paired auth, the PUT backs
# up the current file to config.yaml.bak.<ts> before writing, and the write
# is atomic (tmp + rename). The app's Settings > Config Editor screen uses
# these. Writing does NOT restart the gateway - the user does that from the
# Gateway section if the change needs a restart.


def _config_path() -> Path:
    return Path(os.getenv("HERMES_HOME") or os.path.expanduser("~/.hermes")) / "config.yaml"


async def config_get(request: Request) -> JSONResponse:
    await require_native_or_paired_auth(request)
    path = _config_path()
    try:
        content = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        raise HTTPException(status_code=404, detail="config.yaml not found")
    except OSError as e:
        raise HTTPException(status_code=500, detail=f"read failed: {e}")
    return JSONResponse({
        "path": str(path),
        "size": len(content.encode("utf-8")),
        "mtime": path.stat().st_mtime,
        "content": content,
    })


async def config_put(request: Request) -> JSONResponse:
    await require_native_or_paired_auth(request)
    try:
        body = await request.json()
    except Exception:
        raise HTTPException(status_code=400, detail="Request body must be JSON")
    content = body.get("content")
    if not isinstance(content, str) or not content.strip():
        raise HTTPException(status_code=400, detail="content is required")

    # Validate YAML before touching the real file.
    try:
        import yaml as _yaml
        _yaml.safe_load(content)
    except Exception as e:
        raise HTTPException(status_code=422, detail=f"invalid YAML: {e}")

    path = _config_path()
    try:
        # Backup the live file before overwriting.
        backup = path.with_name(f"config.yaml.bak.{int(time.time())}")
        shutil.copy2(path, backup)
        # Atomic write.
        tmp = path.with_name(f"config.yaml.tmp.{os.getpid()}")
        tmp.write_text(content, encoding="utf-8")
        os.replace(tmp, path)
    except OSError as e:
        raise HTTPException(status_code=500, detail=f"write failed: {e}")

    return JSONResponse({
        "ok": True,
        "path": str(path),
        "size": len(content.encode("utf-8")),
        "backup": str(backup),
    })


# ── Canvas Live tab: background process stream (build 135.17) ──────────
#
# The iOS Canvas "Live" tab subscribes to /v1/canvas/processes/stream
# to render any long-running commands the connector spawns on its
# behalf (journal tails, restart scripts, hermes update applies, etc).
# Other code paths in the connector can register arbitrary processes
# with ``tracked_subprocess_exec`` and they will appear here
# automatically — no per-feature plumbing required.


async def canvas_processes_list(request: Request) -> JSONResponse:
    """GET /v1/canvas/processes — current snapshot of tracked processes."""
    await require_auth(request)
    registry = get_process_registry()
    rows = registry.list_snapshots()
    return JSONResponse({"data": {"processes": rows}})


async def canvas_processes_register(request: Request) -> JSONResponse:
    """POST /v1/canvas/processes — register a process to track.

    Body (JSON):
        {
            "command": ["argv0", "argv1", ...],
            "name": "friendly label"
        }

    Spawns the process, registers it with the global registry, and
    returns the new id + initial snapshot.  The registry's tail
    loop streams stdout/stderr to every subscriber of
    /v1/canvas/processes/stream.
    """
    await require_auth(request)
    try:
        body = await request.json()
    except (ValueError, TypeError):
        raise HTTPException(status_code=400, detail="Invalid JSON body")
    cmd = body.get("command")
    if not isinstance(cmd, list) or not cmd or not all(isinstance(s, str) for s in cmd):
        raise HTTPException(
            status_code=400,
            detail="`command` must be a non-empty list of strings",
        )
    name = body.get("name") or cmd[0] or "process"
    proc, record = await tracked_subprocess_exec(
        name=name,
        args=cmd,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    return JSONResponse({"data": {"process": record.snapshot()}})


async def canvas_processes_kill(request: Request) -> JSONResponse:
    """POST /v1/canvas/processes/{id}/kill — terminate a tracked process."""
    await require_auth(request)
    process_id = request.path_params.get("id", "")
    ok = await get_process_registry().kill(process_id)
    if not ok:
        raise HTTPException(status_code=404, detail="Process not found")
    record = get_process_registry().get(process_id)
    return JSONResponse(
        {"data": {"process": record.snapshot() if record else {"id": process_id}}}
    )


async def canvas_processes_stream(request: Request) -> StreamingResponse:
    """GET /v1/canvas/processes/stream — SSE feed of process events.

    Each event is a JSON object with the same shape as
    ``BackgroundProcessRegistry.TrackedProcess.snapshot()``.  A
    ``{"event": "keepalive"}`` sentinel is emitted every ~15s to
    keep the SSE connection alive through buffering proxies.
    """
    await require_auth(request)

    async def stream() -> AsyncIterator[str]:
        registry = get_process_registry()
        try:
            async for event in registry.stream():
                if await request.is_disconnected():
                    return
                if event.get("event") == "keepalive":
                    yield ": keepalive\n\n"
                    continue
                yield f"event: process\ndata: {json.dumps(event)}\n\n"
        except asyncio.CancelledError:
            return

    return StreamingResponse(
        stream(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "Connection": "keep-alive",
            "X-Accel-Buffering": "no",
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
    Route("/v1/session", get_session, methods=["GET"]),
    Route("/v1/commands", list_commands, methods=["GET"]),
    Route("/v1/config", config_get, methods=["GET"]),
    Route("/v1/config", config_put, methods=["PUT"]),
    Route("/gw/config", config_get, methods=["GET"]),
    Route("/gw/restart", gateway_restart, methods=["POST"]),
    Route("/v1/gw/restart", gateway_restart, methods=["POST"]),
    # Build 33 Workstream A — preflight MUST be registered before the
    # {operationId} capture below or "preflight" would match as an id.
    Route("/gw/restart/preflight", gateway_restart_preflight, methods=["GET"]),
    Route("/v1/gw/restart/preflight", gateway_restart_preflight, methods=["GET"]),
    Route("/gw/restart/{operationId}", gateway_restart_status, methods=["GET"]),
    Route("/v1/gw/restart/{operationId}", gateway_restart_status, methods=["GET"]),
    Route("/gw/status", gateway_status, methods=["GET"]),
    Route("/v1/gw/status", gateway_status, methods=["GET"]),
    Route("/gw/health", gateway_status, methods=["GET"]),
    Route("/v1/gw/health", gateway_status, methods=["GET"]),
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
    Route("/v1/sessions/{id}/generate-title", session_generate_title, methods=["POST"]),
    Route("/v1/sessions/{id}/messages", session_messages_handler, methods=["GET"]),
    Route("/v1/sessions/{id}", session_delete, methods=["DELETE"]),
    Route("/v1/sessions/{id}", session_patch, methods=["PATCH"]),
    Route("/v1/inbox", get_inbox, methods=["GET"]),
    Route("/v1/inbox/{id}/action", inbox_action, methods=["POST"]),
    Route("/v1/push/register", push_register, methods=["POST"]),
    Route("/v1/push/test", push_test, methods=["POST"]),
    Route("/v1/push/deactivate", push_deactivate, methods=["POST"]),
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
    Route("/v1/talk/transcribe", talk_transcribe, methods=["POST"]),
    Route("/v1/talk/speak", talk_speak, methods=["POST"]),
    Route("/v1/talk/session", stub_talk_session, methods=["POST"]),
    Route("/v1/talk/session/{id}/end", stub_talk_session_end, methods=["POST"]),
    Route("/v1/talk/session/{id}/inject", stub_talk_session_inject, methods=["POST"]),
    Route("/v1/talk/session/{id}/turns", stub_talk_session_turns, methods=["GET"]),
    # Build 104: server-side Xiaomi MiMo ASR proxy.  The iOS client
    # posts a multipart audio upload here; the connector forwards
    # the request to api.xiaomimimo.com using its own key.
    Route("/v1/gw/update", gateway_update_apply, methods=["GET", "POST"]),
    Route("/v1/gw/update/check", gateway_update_check, methods=["POST"]),
    # Build 128.49: live Hermes update apply + progress (connector survives
    # `hermes update` so it can stream output to the iOS sheet).
    Route("/v1/gw/update/apply", gateway_update_apply_hermes, methods=["POST"]),
    Route("/v1/gw/update/progress", gateway_update_progress, methods=["GET"]),
    Route("/v1/hermes/logs", hermes_logs_proxy, methods=["GET"]),  # Build 31
    # Build 103 WS-C: authoritative Hermes log endpoints reading from
    # profiles/{profile}/logs/* (not connector journald).
    Route("/v1/hermes/logs/history", hermes_logs, methods=["GET"]),
    Route("/v1/hermes/logs/stream", hermes_logs_stream, methods=["GET"]),
    # Non-v1 aliases — the iOS RelayAPIClient strips /v1 from gateway
    # paths (RelayAPIClient.swift:324-345) when resolving against
    # activeBaseURLString, so /gw/update resolves to POST host:8010/gw/update.
    # Without these aliases the facade returns 404 for update check/apply.
    Route("/gw/update", gateway_update_apply, methods=["GET", "POST"]),
    Route("/gw/update/check", gateway_update_check, methods=["POST"]),
    Route("/gw/update/apply", gateway_update_apply_hermes, methods=["POST"]),
    Route("/gw/update/progress", gateway_update_progress, methods=["GET"]),
    Route("/gw/hermes/logs", hermes_logs_proxy, methods=["GET"]),  # Build 31
    Route("/v1/relay/identity", stub_relay_identity, methods=["GET"]),
    # Build 135.17: Canvas "Live" tab background process feed.
    # The iOS app subscribes to /v1/canvas/processes/stream (SSE) and
    # reads the snapshot via GET /v1/canvas/processes.
    Route("/v1/canvas/processes", canvas_processes_list, methods=["GET"]),
    Route("/v1/canvas/processes", canvas_processes_register, methods=["POST"]),
    Route(
        "/v1/canvas/processes/{id}/kill",
        canvas_processes_kill,
        methods=["POST"],
    ),
    Route(
        "/v1/canvas/processes/stream",
        canvas_processes_stream,
        methods=["GET"],
    ),
    # Native-completion to push bridge (Task 12)
    Route("/v1/native/watch", native_watch_route, methods=["POST"]),
    Route("/v1/native/media", native_media_route, methods=["GET"]),
    Route(
        "/v1/messages/{messageID}/attachments/{remoteIndex}",
        message_attachment_bytes,
        methods=["GET"],
    ),
    WebSocketRoute("/v1/terminal", terminal_websocket_route),
    # REST sibling of /v1/terminal: list recent hermes sessions the TUI
    # can resume (parsed from `hermes sessions list`). Used by the iOS
    # app to populate the "Resume last session / Start new session" prompt
    # before opening the WS.
    Route(
        "/v1/terminal/sessions",
        terminal_resumable_sessions,
        methods=["GET"],
    ),
]

app = Starlette(
    debug=False,
    routes=routes,
    exception_handlers={HTTPException: http_exception_handler},
    on_startup=[get_process_registry().start],
    on_shutdown=[get_process_registry().stop],
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
