from __future__ import annotations

import asyncio
import base64
from contextlib import asynccontextmanager
from dataclasses import dataclass
from datetime import datetime, timezone
import httpx
import inspect
import logging
import time
import uuid

logger = logging.getLogger("herald.relay")

import json

from fastapi import Body, Depends, FastAPI, Header, HTTPException, Request, WebSocket, WebSocketDisconnect, status
from fastapi.encoders import jsonable_encoder
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse, Response, StreamingResponse
from pydantic import BaseModel
from sqlalchemy import select
from sqlalchemy.orm import Session

from .apns import PushResult, create_apns_client
from .app_attest import AppAttestVerificationError, verify_app_attest_registration_proof
from .app_attest_trust import AppAttestTrustAnchorError, load_bundled_app_attest_roots
from .config import Settings
from .database import Database
from .herald_adapter import build_herald_adapter
from .models import Conversation, HeraldHost, Message, PushRegistration, utcnow
from .pairing import HostSetupCodePayload, format_phone_pairing_code, build_host_setup_code
from .push_broker import create_push_broker_challenge, serialize_push_broker_challenge
from .push_broker import (
    PushBrokerChallengeError,
    PushBrokerSendError,
    canonical_push_broker_signed_payload,
    create_push_broker_registration,
    verify_push_broker_send_request,
)
from .rate_limit import PhonePairingRateLimiter
from .relay_identity import _b64url_decode, ensure_relay_identity, serialize_relay_identity, sign_relay_payload
from .schemas import (
    ConnectorSetupRequest,
    CreateSessionBody,
    CronCreateRequest,
    CronUpdateRequest,
    DeviceRegisterRequest,
    DeviceAppStateRequest,
    HostEnrollmentCodeCreateRequest,
    HostRedeemRequest,
    InboxActionRequest,
    InternalInboxCreateRequest,
    MessageCreateRequest,
    ModelSetRequest,
    PairingRedeemRequest,
    PhonePairingRedeemRequest,
    RenameSessionBody,
    SensorHealthRequest,
    SensorLocationRequest,
    PushRegisterRequest,
    PushBrokerRegisterRequest,
    PushBrokerSendRequest,
    RefreshRequest,
    VoiceTurnCreateRequest,
)
from .security import AuthContext, get_auth_context, get_db, get_settings, normalize_datetime, require_internal_key
from .streaming import EventFanout
from .services import (
    activate_herald_host_connection,
    append_job_event,
    append_message,
    archive_current_conversation,
    archive_session,
    authenticate_herald_host,
    build_connector_websocket_url,
    claim_next_message_job,
    complete_message_job,
    conversation_history_before_message,
    create_phone_pairing_code,
    create_session,
    create_voice_session,
    create_host_enrollment_invite,
    create_inbox_item,
    create_message_job,
    current_herald_host_for_user,
    active_push_registrations_for_user,
    deactivate_herald_host_connection,
    delete_session,
    device_is_foreground,
    end_voice_session,
    ensure_default_user,
    fail_message_job,
    get_job_events_after,
    get_job_last_seq,
    get_session,
    get_inbox_item_for_user,
    get_message_job,
    get_message_job_for_user_message,
    get_or_create_current_conversation,
    get_voice_session,
    inject_voice_transcript,
    get_user_message_by_client_message_id,
    herald_host_is_online,
    list_conversation_messages,
    list_inbox_actions,
    list_inbox_items,
    list_message_jobs_for_conversation,
    list_sessions,
    record_audit,
    record_inbox_action,
    redeem_phone_pairing_code,
    redeem_host_enrollment_invite,
    redeem_pairing_invite,
    refresh_auth_session,
    rename_session,
    revoke_auth_session,
    revoke_current_herald_host,
    renew_message_job_lease,
    rotate_auth_session,
    search_sessions,
    serialize_conversation,
    serialize_herald_host,
    serialize_inbox_item,
    serialize_message,
    serialize_session_summary,
    serialize_voice_session,
    serialize_voice_turn,
    setup_connector_account,
    toggle_pin_session,
    touch_herald_host_connection,
    update_device_app_state,
    upsert_device,
    upsert_push_registration,
    mark_voice_session_started,
    process_message_job_with_adapter,
    record_voice_turn,
)
from .talk_mcp import register_talk_mcp_routes


def success(data: dict) -> dict:
    return {
        "data": data,
        "meta": {
            "requestId": str(uuid.uuid4()),
            "timestamp": datetime.now(timezone.utc).isoformat(),
        },
    }


def success_response(data: dict, *, status_code: int = status.HTTP_200_OK) -> JSONResponse:
    return JSONResponse(status_code=status_code, content=jsonable_encoder(success(data)))


def parse_bearer_token(authorization_header: str | None) -> str | None:
    if authorization_header is None:
        return None
    prefix = "Bearer "
    if not authorization_header.startswith(prefix):
        return None
    return authorization_header[len(prefix) :].strip() or None


def reply_state_for_job(job_status: str) -> str:
    if job_status == "completed":
        return "delivered"
    if job_status == "failed":
        return "failed"
    if job_status == "cancelled":
        return "failed"
    return "pending"


def broker_http2_supported() -> bool:
    try:
        import h2  # noqa: F401
        return True
    except ImportError:
        return False


def decode_b64url_field(value: str, *, field_name: str) -> bytes:
    try:
        return _b64url_decode(value)
    except Exception as error:  # noqa: BLE001 - any decode failure is a bad client payload
        raise HTTPException(status_code=status.HTTP_400_BAD_REQUEST, detail=f"Invalid base64url field: {field_name}.") from error


@dataclass
class ConnectorSession:
    websocket: WebSocket
    host_id: str
    connection_nonce: str
    busy: bool = False


def create_app(settings: Settings | None = None) -> FastAPI:
    settings = settings or Settings.from_env()

    if settings.internal_api_key == "replace-me":
        if settings.environment not in ("development", "test"):
            raise RuntimeError(
                "INTERNAL_API_KEY is set to the default 'replace-me'. "
                "Set a strong random value via the INTERNAL_API_KEY env var before "
                "running in production. This is a security requirement."
            )
        logger.warning(
            "SECURITY: INTERNAL_API_KEY is set to the default 'replace-me'. "
            "This is acceptable in development but must be changed for production."
        )

    database = Database(settings.database_url)

    @asynccontextmanager
    async def lifespan(app: FastAPI):
        database.create_all()
        app.state.apns_client = create_apns_client(settings)
        app.state.push_broker_http_client = httpx.AsyncClient(http2=broker_http2_supported(), timeout=30.0)

        try:
            yield
        finally:
            await app.state.push_broker_http_client.aclose()
            if app.state.apns_client:
                close_method = getattr(app.state.apns_client, "close", None)
                if callable(close_method):
                    close_result = close_method()
                    if inspect.isawaitable(close_result):
                        await close_result

    app = FastAPI(title=settings.service_name, version=settings.version, lifespan=lifespan)

    @app.exception_handler(RequestValidationError)
    async def _log_validation_errors(request: Request, exc: RequestValidationError) -> JSONResponse:
        logging.getLogger("herald.relay").warning(
            "422 validation error on %s %s: %s", request.method, request.url.path, exc.errors()
        )
        return JSONResponse(status_code=422, content={"detail": exc.errors()})

    app.state.settings = settings
    app.state.database = database
    app.state.herald_adapter = build_herald_adapter(settings)
    # Load the bundled + pinned Apple App Attest root CA. If the fingerprint
    # doesn't match or the file is missing, loudly surface the misconfiguration
    # rather than silently falling back to accepting any chain. The push-broker
    # register endpoint still returns 503 when trusted_roots is empty, so
    # logging here is enough — we don't want a boot failure to take down all
    # other endpoints (pairing, SSE, etc.) on an older image that lacks the PEM.
    try:
        app.state.app_attest_trusted_roots = load_bundled_app_attest_roots()
    except AppAttestTrustAnchorError as error:
        logger.error("App Attest trust anchor unavailable: %s", error)
        app.state.app_attest_trusted_roots = []
    app.state.phone_pairing_rate_limiter = PhonePairingRateLimiter(
        max_attempts=settings.phone_pairing_max_attempts_per_ip,
        window_seconds=settings.phone_pairing_rate_limit_window_seconds,
    )
    app.state.connector_sessions: dict[str, ConnectorSession] = {}
    app.state.sensor_delivery_waiters: dict[str, asyncio.Future[bool]] = {}
    app.state.connector_rpc_waiters: dict[str, asyncio.Future[dict]] = {}
    app.state.event_fanout = EventFanout()

    async def subscribe_job_events(job_id: str) -> asyncio.Queue:
        """Subscribe to wake signals via EventFanout (DB-backed replay)."""
        return await app.state.event_fanout.subscribe(job_id)

    async def unsubscribe_job_events(job_id: str, queue: asyncio.Queue) -> None:
        """Unsubscribe from EventFanout."""
        await app.state.event_fanout.unsubscribe(job_id, queue)

    def publish_job_event(job_id: str, event: dict) -> None:
        # Persist to durable DB log + wake EventFanout subscribers
        try:
            event_type = event.get("event", "progress")
            payload = event.get("data", {})
            source_seq = event.get("sourceSeq")
            if source_seq is None:
                logger.warning("Legacy unsequenced event for job %s: %s", job_id, event_type)
            with database.session() as db:
                job = get_message_job(db, job_id=job_id)
                attempt = job.attempt if job is not None else 0
                result = append_job_event(
                    db,
                    job_id=job_id,
                    event_type=event_type,
                    payload=payload if isinstance(payload, dict) else {},
                    source_seq=source_seq,
                    attempt=attempt,
                )
                db.commit()
                if result is not None:
                    event["eventId"] = result["seq"]
        except Exception:
            logger.warning("Failed to persist job event for %s", job_id, exc_info=True)

        # Wake EventFanout subscribers
        try:
            loop = asyncio.get_event_loop()
            loop.call_soon_threadsafe(app.state.event_fanout.wake, job_id)
        except RuntimeError:
            pass

    def require_connector_host(
        authorization: str | None = Header(default=None),
        db: Session = Depends(get_db),
    ) -> HeraldHost:
        connector_token = parse_bearer_token(authorization)
        if connector_token is None:
            raise HTTPException(status_code=status.HTTP_401_UNAUTHORIZED, detail="Missing connector credential.")
        return authenticate_herald_host(db, connector_token=connector_token)

    async def wait_for_job_completion(job_id: str, timeout_seconds: int) -> object | None:
        deadline = asyncio.get_running_loop().time() + timeout_seconds
        while asyncio.get_running_loop().time() < deadline:
            with database.session() as db:
                job = get_message_job(db, job_id=job_id)
                if job is None or job.status in {"completed", "failed", "cancelled"}:
                    return job
            await asyncio.sleep(0.25)
        return None

    def connector_session_for_user(user_id: str) -> ConnectorSession | None:
        return app.state.connector_sessions.get(user_id)

    def set_connector_session(user_id: str, session: ConnectorSession) -> None:
        app.state.connector_sessions[user_id] = session

    def clear_connector_session(user_id: str | None, connection_nonce: str | None) -> None:
        if user_id is None or connection_nonce is None:
            return
        session = connector_session_for_user(user_id)
        if session is None or session.connection_nonce != connection_nonce:
            return
        app.state.connector_sessions.pop(user_id, None)

    async def default_push_broker_sender(
        *,
        registration: PushRegistration,
        title: str,
        body: str,
        conversation_id: str | None = None,
        message_id: str | None = None,
        job_id: str | None = None,
        category: str | None = None,
    ) -> bool:
        import secrets as _secrets
        import time as _time

        with database.session() as db:
            identity = ensure_relay_identity(db, settings=settings)
        broker_base_url = (settings.push_broker_base_url or settings.public_base_url).rstrip("/")
        payload = {
            "relayHandle": registration.relay_handle,
            "sendGrant": registration.send_grant,
            "relayId": registration.relay_id or identity.id,
            "relayPublicKey": registration.relay_public_key or identity.public_key,
            "pushType": "alert",
            "title": title,
            "body": body,
            "nonce": _secrets.token_urlsafe(24),
            "iat": int(_time.time()),
        }
        # Include notification metadata if provided
        if conversation_id is not None:
            payload["conversationId"] = conversation_id
        if message_id is not None:
            payload["messageId"] = message_id
        if job_id is not None:
            payload["jobId"] = job_id
        if category is not None:
            payload["category"] = category
        signature = sign_relay_payload(identity, payload)
        response = await app.state.push_broker_http_client.post(
            f"{broker_base_url}/push-broker/send",
            json=payload | {"signature": signature},
        )
        return response.status_code == 200 and bool(response.json().get("data", {}).get("sent"))

    app.state.push_broker_sender = default_push_broker_sender

    async def maybe_send_message_push(
        *,
        db: Session,
        user_id: str,
        conversation_id: str,
        message_id: str,
        message_text: str,
        job_id: str | None = None,
        category: str | None = None,
    ) -> None:
        apns_client = app.state.apns_client
        if apns_client is None:
            return

        preview = " ".join(message_text.split()).strip()
        if not preview:
            return
        preview = preview[:160]

        for device, registration in active_push_registrations_for_user(db, user_id=user_id):
            if device_is_foreground(device, stale_seconds=settings.app_presence_stale_seconds):
                continue
            if registration.transport == "relay":
                sender = app.state.push_broker_sender
                try:
                    sent = await sender(
                        registration=registration,
                        title="Herald",
                        body=preview,
                        conversation_id=conversation_id,
                        message_id=message_id,
                        job_id=job_id,
                        category=category,
                    )
                except Exception:
                    logger.warning("Push broker delivery failed for device %s", device.id, exc_info=True)
                    continue
                if not sent:
                    logger.warning("Push broker delivery not accepted for device %s", device.id)
                continue

            user_info: dict = {
                "conversationId": conversation_id,
                "messageId": message_id,
            }
            if job_id is not None:
                user_info["jobId"] = job_id
            result = await apns_client.send_alert_push(
                registration.apns_token,
                title="Herald",
                body=preview,
                category=category,
                bundle_id=registration.bundle_id,
                environment=registration.push_environment,
                user_info=user_info,
            )
            if result == PushResult.TOKEN_INVALID:
                registration.is_active = False
                db.commit()
                logger.info("Deactivated invalid APNs token for device %s", device.id)
            elif result != PushResult.SENT:
                logger.warning("APNs delivery %s for device %s", result.value, device.id)

        # Create inbox item so the app shows it in the inbox tab
        create_inbox_item(
            db,
            user_id=user_id,
            device_id=None,
            kind="notification",
            title="Herald",
            body=preview,
            priority="normal",
            payload={"conversationId": conversation_id, "messageId": message_id},
            expires_at=None,
        )
        db.commit()

    def resolve_sensor_delivery(delivery_id: str | None, *, delivered: bool) -> None:
        if delivery_id is None:
            return
        waiter = app.state.sensor_delivery_waiters.pop(delivery_id, None)
        if waiter is not None and not waiter.done():
            waiter.set_result(delivered)

    def resolve_connector_rpc_response(
        request_id: str | None,
        *,
        success: bool,
        result: dict | None = None,
        error: str | None = None,
    ) -> None:
        if request_id is None:
            return
        waiter = app.state.connector_rpc_waiters.pop(request_id, None)
        if waiter is None or waiter.done():
            return
        if success:
            waiter.set_result(result or {})
        else:
            waiter.set_exception(RuntimeError(error or "Connector RPC failed."))

    async def send_connector_rpc(
        user_id: str,
        *,
        method: str,
        params: dict | None = None,
        timeout_seconds: float | None = None,
    ) -> dict:
        session = connector_session_for_user(user_id)
        if session is None:
            raise HTTPException(status_code=status.HTTP_409_CONFLICT, detail="Hermes host is offline.")

        request_id = str(uuid.uuid4())
        waiter: asyncio.Future[dict] = asyncio.get_running_loop().create_future()
        app.state.connector_rpc_waiters[request_id] = waiter

        try:
            await session.websocket.send_json(
                {
                    "type": "rpc.request",
                    "version": 1,
                    "requestId": request_id,
                    "method": method,
                    "params": params or {},
                }
            )
        except Exception as error:
            clear_connector_session(user_id, session.connection_nonce)
            app.state.connector_rpc_waiters.pop(request_id, None)
            raise HTTPException(status_code=status.HTTP_409_CONFLICT, detail="Hermes host is unavailable.") from error

        try:
            return await asyncio.wait_for(waiter, timeout_seconds or settings.connector_rpc_timeout_seconds)
        except asyncio.TimeoutError as error:
            app.state.connector_rpc_waiters.pop(request_id, None)
            raise HTTPException(status_code=status.HTTP_504_GATEWAY_TIMEOUT, detail="Hermes host did not respond in time.") from error

    async def forward_sensor_payload(
        *,
        user_id: str,
        payload: dict,
        ack_timeout_seconds: float,
    ) -> JSONResponse:
        session = connector_session_for_user(user_id)
        if session is None or session.busy:
            return success_response({"deliveryState": "retry"}, status_code=status.HTTP_202_ACCEPTED)

        delivery_id = str(uuid.uuid4())
        message = dict(payload)
        message["deliveryId"] = delivery_id
        waiter: asyncio.Future[bool] = asyncio.get_running_loop().create_future()
        app.state.sensor_delivery_waiters[delivery_id] = waiter

        try:
            await session.websocket.send_json(message)
        except Exception:
            clear_connector_session(user_id, session.connection_nonce)
            app.state.sensor_delivery_waiters.pop(delivery_id, None)
            return success_response({"deliveryState": "retry"}, status_code=status.HTTP_202_ACCEPTED)

        try:
            delivered = await asyncio.wait_for(waiter, timeout=ack_timeout_seconds)
        except asyncio.TimeoutError:
            app.state.sensor_delivery_waiters.pop(delivery_id, None)
            return success_response({"deliveryState": "retry"}, status_code=status.HTTP_202_ACCEPTED)

        status_code = status.HTTP_200_OK if delivered else status.HTTP_202_ACCEPTED
        delivery_state = "delivered" if delivered else "retry"
        return success_response({"deliveryState": delivery_state}, status_code=status_code)

    app.state.send_connector_rpc = send_connector_rpc
    register_talk_mcp_routes(app)

    def build_message_response_payload(db: Session, *, conversation_id: str, job_id: str) -> tuple[dict, int]:
        job = get_message_job(db, job_id=job_id)
        if job is None:
            raise HTTPException(status_code=status.HTTP_500_INTERNAL_SERVER_ERROR, detail="Message job not found.")

        conversation = db.get(Conversation, conversation_id)
        if conversation is None:
            raise HTTPException(status_code=status.HTTP_500_INTERNAL_SERVER_ERROR, detail="Conversation not found.")

        user_message = db.get(Message, job.user_message_id)
        messages = list_conversation_messages(db, conversation_id=conversation_id)
        jobs = list_message_jobs_for_conversation(db, conversation_id=conversation_id)

        payload = {
            "replyState": reply_state_for_job(job.status),
            "jobId": job.id,
            "conversation": serialize_conversation(conversation, messages, jobs=jobs),
            "userMessage": serialize_message(user_message, job=job) if user_message else None,
        }

        if job.usage_data:
            payload["usage"] = job.usage_data

        if job.diff_data:
            payload["diff"] = job.diff_data

        if job.result_message_id:
            result_message = db.get(Message, job.result_message_id)
            if result_message is not None:
                payload["message"] = serialize_message(result_message, job=job)

        status_code = status.HTTP_202_ACCEPTED if job.status in {"queued", "running"} else status.HTTP_200_OK
        return payload, status_code

    def strip_current_job_result_from_pending_payload(payload_data: dict, *, job_id: str) -> None:
        payload_data.pop("message", None)
        conversation_payload = payload_data.get("conversation")
        if not isinstance(conversation_payload, dict):
            return

        messages = conversation_payload.get("messages")
        if not isinstance(messages, list):
            return

        conversation_payload["messages"] = [
            message
            for message in messages
            if not (message.get("jobId") == job_id and message.get("role") != "user")
        ]

    _ROLE_MAP = {
        "voice_user": "user",
        "voice_hermes": "herald",
    }

    def _normalize_role(role: str) -> str:
        """Map voice transcript roles to standard roles for the connector."""
        return _ROLE_MAP.get(role, role)

    def build_job_execute_payload(db: Session, *, job_id: str) -> dict:
        job = get_message_job(db, job_id=job_id)
        if job is None:
            raise RuntimeError("Message job not found.")

        user_message = db.get(Message, job.user_message_id)
        if user_message is None:
            raise RuntimeError("User message not found for job.")

        history = conversation_history_before_message(
            db,
            conversation_id=job.conversation_id,
            message_id=user_message.id,
        )

        # Extract voice transcript messages so they can be injected into the
        # Herald agent's context even when it uses its own session history.
        voice_transcript_lines: list[str] = []
        regular_history: list[dict] = []
        for message in history:
            if message.source == "voice_transcript" and message.role != "system":
                speaker = "User" if message.role in ("voice_user", "user") else "Herald"
                voice_transcript_lines.append(f"{speaker}: {message.text}")
            else:
                regular_history.append({
                    "role": _normalize_role(message.role),
                    "text": message.text,
                })

        job_data: dict = {
            "id": job.id,
            "attempt": job.attempt,
            "conversationId": job.conversation_id,
            "latestUserMessage": user_message.text,
            "history": regular_history,
            "sessionId": job.session_id_snapshot,
            "timeoutSeconds": settings.connector_job_lease_seconds,
        }
        if job.reasoning_effort:
            job_data["reasoningEffort"] = job.reasoning_effort
        if voice_transcript_lines:
            job_data["voiceTranscriptContext"] = "\n".join(voice_transcript_lines)
        if user_message.attachments_data:
            job_data["attachments"] = user_message.attachments_data
        return {
            "type": "job.execute",
            "version": 1,
            "job": job_data,
        }

    @app.get("/v1/health")
    def health() -> dict:
        return success({"status": "ok"})

    @app.get("/v1/version")
    def version(request_settings: Settings = Depends(get_settings)) -> dict:
        return success(
            {
                "service": request_settings.service_name,
                "version": request_settings.version,
                "environment": request_settings.environment,
            }
        )

    @app.get("/v1/relay/identity")
    def relay_identity(
        db: Session = Depends(get_db),
        request_settings: Settings = Depends(get_settings),
    ) -> dict:
        identity = ensure_relay_identity(db, settings=request_settings)
        return success({"identity": serialize_relay_identity(identity, settings=request_settings)})

    @app.post("/v1/push-broker/challenge")
    def push_broker_challenge(
        db: Session = Depends(get_db),
        request_settings: Settings = Depends(get_settings),
    ) -> dict:
        challenge = create_push_broker_challenge(db, settings=request_settings)
        return success(serialize_push_broker_challenge(challenge))

    @app.post("/v1/push-broker/register")
    def push_broker_register(
        payload: PushBrokerRegisterRequest,
        request: Request,
        db: Session = Depends(get_db),
        request_settings: Settings = Depends(get_settings),
    ) -> dict:
        team_id = request_settings.apns_team_id
        if not team_id:
            raise HTTPException(status_code=status.HTTP_503_SERVICE_UNAVAILABLE, detail="Push broker App Attest team ID is not configured.")
        trusted_roots = getattr(request.app.state, "app_attest_trusted_roots", None)
        if not trusted_roots:
            raise HTTPException(status_code=status.HTTP_503_SERVICE_UNAVAILABLE, detail="Push broker App Attest roots are not configured.")

        # Build the signed-payload bytes server-side so the client never gets
        # to dictate the exact byte sequence verified by App Attest. The client
        # must produce the same canonical bytes before signing.
        signed_payload = canonical_push_broker_signed_payload(
            challenge_id=payload.challengeId,
            installation_id=payload.installationId,
            bundle_id=payload.bundleId,
            app_version=payload.appVersion,
            apns_environment=payload.apnsEnvironment,
            apns_token=payload.apnsToken,
            relay_id=payload.relayIdentity.id,
            relay_public_key=payload.relayIdentity.publicKey,
            relay_base_url=payload.relayIdentity.relayBaseURL,
        )

        try:
            app_attest = verify_app_attest_registration_proof(
                attestation_object=decode_b64url_field(payload.appAttest.attestationObject, field_name="appAttest.attestationObject"),
                assertion_object=decode_b64url_field(payload.appAttest.assertion, field_name="appAttest.assertion"),
                signed_payload=signed_payload,
                key_id=payload.appAttest.keyId,
                challenge=payload.challenge,
                team_id=team_id,
                bundle_id=payload.bundleId,
                environment="development" if payload.apnsEnvironment in {"development", "sandbox"} else "production",
                trusted_root_certificates=trusted_roots,
            )
        except AppAttestVerificationError as error:
            raise HTTPException(status_code=status.HTTP_401_UNAUTHORIZED, detail=str(error)) from error

        try:
            result = create_push_broker_registration(
                db,
                settings=request_settings,
                challenge_id=payload.challengeId,
                challenge=payload.challenge,
                relay_id=payload.relayIdentity.id,
                relay_public_key=payload.relayIdentity.publicKey,
                installation_id=payload.installationId,
                bundle_id=payload.bundleId,
                app_version=payload.appVersion,
                apns_environment=payload.apnsEnvironment,
                apns_token=payload.apnsToken,
                app_attest=app_attest,
            )
        except PushBrokerChallengeError as error:
            raise HTTPException(status_code=status.HTTP_400_BAD_REQUEST, detail=str(error)) from error
        return success(result.gateway_payload() | {"expiresAt": result.expires_at})

    @app.post("/v1/push-broker/send")
    async def push_broker_send(
        payload: PushBrokerSendRequest,
        request: Request,
        db: Session = Depends(get_db),
    ) -> dict:
        signed_payload = {
            "relayHandle": payload.relayHandle,
            "sendGrant": payload.sendGrant,
            "relayId": payload.relayId,
            "relayPublicKey": payload.relayPublicKey,
            "pushType": payload.pushType,
            "title": payload.title,
            "body": payload.body,
            "nonce": payload.nonce,
            "iat": payload.iat,
        }
        # Include metadata fields in signed payload if present
        if payload.conversationId is not None:
            signed_payload["conversationId"] = payload.conversationId
        if payload.messageId is not None:
            signed_payload["messageId"] = payload.messageId
        if payload.jobId is not None:
            signed_payload["jobId"] = payload.jobId
        if payload.category is not None:
            signed_payload["category"] = payload.category
        try:
            registration = verify_push_broker_send_request(
                db,
                relay_handle=payload.relayHandle,
                send_grant=payload.sendGrant,
                relay_id=payload.relayId,
                relay_public_key=payload.relayPublicKey,
                payload=signed_payload,
                signature=payload.signature,
                nonce=payload.nonce,
                iat=payload.iat,
            )
        except PushBrokerSendError as error:
            detail = str(error)
            status_code = status.HTTP_401_UNAUTHORIZED
            if "expired" in detail or "revoked" in detail:
                status_code = status.HTTP_409_CONFLICT
            elif "invalid." in detail and "handle" in detail:
                status_code = status.HTTP_404_NOT_FOUND
            raise HTTPException(status_code=status_code, detail=detail) from error

        apns_client = request.app.state.apns_client
        if apns_client is None:
            raise HTTPException(status_code=status.HTTP_503_SERVICE_UNAVAILABLE, detail="APNs client is not configured.")

        if payload.pushType == "silent":
            result = await apns_client.send_silent_push(
                registration.apns_token,
                bundle_id=registration.bundle_id,
                environment=registration.apns_environment,
            )
        else:
            user_info: dict = {}
            if payload.conversationId is not None:
                user_info["conversationId"] = payload.conversationId
            if payload.messageId is not None:
                user_info["messageId"] = payload.messageId
            if payload.jobId is not None:
                user_info["jobId"] = payload.jobId
            result = await apns_client.send_alert_push(
                registration.apns_token,
                title=payload.title or "Herald",
                body=payload.body or "",
                category=payload.category,
                bundle_id=registration.bundle_id,
                environment=registration.apns_environment,
                user_info=user_info if user_info else None,
            )
        sent = getattr(result, "value", result) == "sent"
        return success({"sent": sent, "relayHandle": registration.relay_handle})

    @app.post("/v1/connector/setup")
    def connector_setup(
        payload: ConnectorSetupRequest,
        db: Session = Depends(get_db),
        request_settings: Settings = Depends(get_settings),
    ) -> dict:
        # If a setup secret is configured, require it. Open access in dev when unset.
        if request_settings.connector_setup_secret:
            if payload.installationSecret != request_settings.connector_setup_secret:
                raise HTTPException(status_code=status.HTTP_403_FORBIDDEN, detail="Invalid or missing installation secret.")

        user, host, connector_token = setup_connector_account(
            db,
            settings=request_settings,
            platform=payload.connector.platform,
            hostname=payload.connector.hostname,
            herald_command=payload.connector.heraldCommand,
            herald_version=payload.connector.heraldVersion,
            connector_version=payload.connector.connectorVersion,
        )
        record_audit(
            db,
            actor_type="connector",
            actor_id=host.id,
            action="connector.setup",
            entity_type="herald_host",
            entity_id=host.id,
            payload={"userId": user.id},
        )
        db.commit()
        return success(
            {
                "user": {
                    "id": user.id,
                    "displayName": user.display_name,
                },
                "host": {
                    "id": host.id,
                    "userId": host.user_id,
                },
                "connectorCredential": connector_token,
                "webSocketURL": build_connector_websocket_url(request_settings.public_base_url),
                "relayURL": request_settings.public_base_url,
            }
        )

    @app.post("/v1/connector/phone-pairing-codes")
    def create_phone_pairing(
        host: HeraldHost = Depends(require_connector_host),
        db: Session = Depends(get_db),
        request_settings: Settings = Depends(get_settings),
    ) -> dict:
        pairing_code, normalized_code = create_phone_pairing_code(
            db,
            settings=request_settings,
            host=host,
        )
        display_code = format_phone_pairing_code(normalized_code)
        record_audit(
            db,
            actor_type="connector",
            actor_id=host.id,
            action="phone_pairing_code.create",
            entity_type="phone_pairing_code",
            entity_id=pairing_code.id,
            payload={"displayCode": display_code},
        )
        db.commit()
        return success(
            {
                "code": normalized_code,
                "displayCode": display_code,
                "expiresAt": pairing_code.expires_at,
            }
        )

    @app.post("/v1/phone-pairing/redeem")
    def redeem_phone_pairing(
        payload: PhonePairingRedeemRequest,
        request: Request,
        db: Session = Depends(get_db),
        request_settings: Settings = Depends(get_settings),
    ) -> dict:
        client_host = request.client.host if request.client else ""
        rate_limiter: PhonePairingRateLimiter = request.app.state.phone_pairing_rate_limiter
        if rate_limiter.is_limited(client_host):
            raise HTTPException(status_code=status.HTTP_429_TOO_MANY_REQUESTS, detail="Too many pairing attempts. Try again later.")

        try:
            pairing_code, user, device, auth_session, access_token, refresh_token = redeem_phone_pairing_code(
                db,
                settings=request_settings,
                raw_code=payload.code,
                platform=payload.device.platform,
                installation_id=str(payload.device.installationId),
                device_name=payload.device.deviceName,
                device_model=payload.device.deviceModel,
                system_version=payload.device.systemVersion,
                app_version=payload.device.appVersion,
                build_number=payload.device.buildNumber,
                bundle_id=payload.device.bundleId,
                environment=payload.client.environment,
            )
        except HTTPException as error:
            rate_limiter.register_failure(client_host)
            raise error

        record_audit(
            db,
            actor_type="app",
            actor_id=device.id,
            action="phone_pairing.redeem",
            entity_type="phone_pairing_code",
            entity_id=pairing_code.id,
            payload={"installationId": str(payload.device.installationId)},
        )
        db.commit()

        return success(
            {
                "user": {
                    "id": user.id,
                    "displayName": user.display_name,
                },
                "deviceId": device.id,
                "deviceRegistered": True,
                "session": {
                    "connectionStatus": "connected",
                    "isMockMode": False,
                    "backendEndpoint": request_settings.public_base_url,
                    "lastSyncAt": auth_session.updated_at,
                },
                "auth": {
                    "accessToken": access_token,
                    "refreshToken": refresh_token,
                    "expiresAt": auth_session.access_expires_at,
                },
            }
        )

    # ── Sensor data forwarding ─────────────────────────────────

    @app.post("/v1/device/sensor/location")
    async def sensor_location(
        payload: SensorLocationRequest,
        request_settings: Settings = Depends(get_settings),
        auth: AuthContext = Depends(get_auth_context),
    ) -> JSONResponse:
        return await forward_sensor_payload(
            user_id=auth.user.id,
            ack_timeout_seconds=request_settings.connector_sensor_ack_timeout_seconds,
            payload={
                "type": "sensor.location",
                "latitude": payload.latitude,
                "longitude": payload.longitude,
                "altitude": payload.altitude,
                "accuracy": payload.accuracy,
                "address": payload.address,
                "recordedAt": payload.recordedAt,
            },
        )

    @app.post("/v1/device/sensor/health")
    async def sensor_health(
        payload: SensorHealthRequest,
        request_settings: Settings = Depends(get_settings),
        auth: AuthContext = Depends(get_auth_context),
    ) -> JSONResponse:
        return await forward_sensor_payload(
            user_id=auth.user.id,
            ack_timeout_seconds=request_settings.connector_sensor_ack_timeout_seconds,
            payload={
                "type": "sensor.health",
                "samples": [
                    {
                        "metric": s.metric,
                        "value": s.value,
                        "unit": s.unit,
                        "startAt": s.startAt,
                        "endAt": s.endAt,
                    }
                    for s in payload.samples
                ],
            },
        )

    def _meta() -> dict:
        return {"requestId": str(uuid.uuid4()), "timestamp": datetime.now(timezone.utc).isoformat()}

    @app.post("/v1/device/register")
    def register_device(
        payload: DeviceRegisterRequest,
        db: Session = Depends(get_db),
        request_settings: Settings = Depends(get_settings),
    ) -> dict:
        user = ensure_default_user(db, request_settings)
        device = upsert_device(
            db,
            user=user,
            platform=payload.device.platform,
            installation_id=str(payload.device.installationId),
            device_name=payload.device.deviceName,
            device_model=payload.device.deviceModel,
            system_version=payload.device.systemVersion,
            app_version=payload.device.appVersion,
            build_number=payload.device.buildNumber,
            bundle_id=payload.device.bundleId,
            environment=payload.client.environment,
        )
        auth_session, access_token, refresh_token = rotate_auth_session(
            db,
            settings=request_settings,
            user=user,
            device=device,
        )

        record_audit(
            db,
            actor_type="app",
            actor_id=device.id,
            action="device.register",
            entity_type="device",
            entity_id=device.id,
            payload={"installationId": str(payload.device.installationId)},
        )
        db.commit()

        return success(
            {
                "deviceId": device.id,
                "deviceRegistered": True,
                "session": {
                    "connectionStatus": "connected",
                    "isMockMode": False,
                    "backendEndpoint": request_settings.public_base_url,
                    "lastSyncAt": None,
                },
                "auth": {
                    "accessToken": access_token,
                    "refreshToken": refresh_token,
                    "expiresAt": auth_session.access_expires_at,
                },
            }
        )

    @app.post("/v1/pairing/redeem")
    def redeem_pairing(
        payload: PairingRedeemRequest,
        db: Session = Depends(get_db),
        request_settings: Settings = Depends(get_settings),
    ) -> dict:
        invite, user, device, auth_session, access_token, refresh_token = redeem_pairing_invite(
            db,
            settings=request_settings,
            invite_token=payload.inviteToken,
            display_name=payload.displayName,
            platform=payload.device.platform,
            installation_id=str(payload.device.installationId),
            device_name=payload.device.deviceName,
            device_model=payload.device.deviceModel,
            system_version=payload.device.systemVersion,
            app_version=payload.device.appVersion,
            build_number=payload.device.buildNumber,
            bundle_id=payload.device.bundleId,
            environment=payload.client.environment,
        )

        record_audit(
            db,
            actor_type="app",
            actor_id=device.id,
            action="pairing.redeem",
            entity_type="pairing_invite",
            entity_id=invite.id,
            payload={"installationId": str(payload.device.installationId)},
        )
        db.commit()

        return success(
            {
                "user": {
                    "id": user.id,
                    "displayName": user.display_name,
                },
                "deviceId": device.id,
                "deviceRegistered": True,
                "session": {
                    "connectionStatus": "connected",
                    "isMockMode": False,
                    "backendEndpoint": request_settings.public_base_url,
                    "lastSyncAt": auth_session.updated_at,
                },
                "auth": {
                    "accessToken": access_token,
                    "refreshToken": refresh_token,
                    "expiresAt": auth_session.access_expires_at,
                },
            }
        )

    @app.post("/v1/hosts/enrollment-codes")
    def create_host_enrollment_code(
        payload: HostEnrollmentCodeCreateRequest | None = Body(default=None),
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
        request_settings: Settings = Depends(get_settings),
    ) -> dict:
        invite, invite_token = create_host_enrollment_invite(db, settings=request_settings, user_id=auth.user.id)
        setup_code = build_host_setup_code(
            HostSetupCodePayload(
                relay_url=request_settings.public_base_url,
                enrollment_token=invite_token,
                expires_at=invite.expires_at,
            )
        )
        host = current_herald_host_for_user(db, user_id=auth.user.id)
        record_audit(
            db,
            actor_type="user",
            actor_id=auth.user.id,
            action="host.enrollment_code.create",
            entity_type="host_enrollment_invite",
            entity_id=invite.id,
            payload={"displayName": payload.displayName if payload else None},
        )
        db.commit()
        return success(
            {
                "setupCode": setup_code,
                "expiresAt": invite.expires_at,
                "relayHost": request_settings.public_base_url,
                "host": serialize_herald_host(db, host=host, settings=request_settings),
            }
        )

    @app.get("/v1/hosts/current")
    def current_host(
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
        request_settings: Settings = Depends(get_settings),
    ) -> dict:
        host = current_herald_host_for_user(db, user_id=auth.user.id)
        return success({"host": serialize_herald_host(db, host=host, settings=request_settings)})

    @app.get("/v1/commands")
    async def command_catalog(
        auth: AuthContext = Depends(get_auth_context),
    ) -> dict:
        """Return the full slash command catalog from the connected Hermes host.

        Includes built-in gateway commands and installed skill commands.
        The iOS app uses this to populate its slash command autocomplete menu.
        """
        try:
            result = await send_connector_rpc(
                auth.user.id,
                method="commands.catalog",
                timeout_seconds=10.0,
            )
            return success(result)
        except HTTPException:
            # Host offline — return empty catalog, iOS falls back to built-in list
            return success({"commands": [], "skills": []})

    @app.get("/v1/models")
    async def model_catalog(
        auth: AuthContext = Depends(get_auth_context),
    ) -> dict:
        """Return the models configured on the connected Hermes host.

        The iOS model selector shows this list grouped by provider. Switching
        is done via POST /v1/model, which edits the host's global default
        model directly (not dispatched through chat).
        """
        try:
            result = await send_connector_rpc(
                auth.user.id,
                method="models.list",
                timeout_seconds=10.0,
            )
            return success(result)
        except HTTPException as exc:
            logger.warning("models.list RPC failed: %s", exc.detail)
            return success({"models": [], "activeModel": None})
        except Exception as exc:
            logger.warning("models.list RPC failed: %s", exc)
            return success({"models": [], "activeModel": None})

    @app.post("/v1/model")
    async def set_active_model(
        body: ModelSetRequest,
        auth: AuthContext = Depends(get_auth_context),
    ) -> dict:
        """Set the global default model on the connected Hermes host.

        This edits the host's config.yaml directly via the connector's
        model.set RPC — equivalent to `/model <name> --global` in the TUI.
        There is no session-scoped override available through this path.
        """
        try:
            result = await send_connector_rpc(
                auth.user.id,
                method="model.set",
                params=body.model_dump(),
                timeout_seconds=10.0,
            )
            return success(result)
        except HTTPException as exc:
            raise exc
        except Exception as exc:
            raise HTTPException(status_code=500, detail=str(exc))

    @app.get("/v1/profiles")
    async def profile_catalog(
        auth: AuthContext = Depends(get_auth_context),
    ) -> dict:
        try:
            result = await send_connector_rpc(
                auth.user.id, method="profiles.list", timeout_seconds=10.0,
            )
            return success(result)
        except HTTPException:
            return success({"profiles": [], "activeProfile": None})
        except Exception:
            return success({"profiles": [], "activeProfile": None})

    @app.get("/v1/skills")
    async def skill_catalog(
        auth: AuthContext = Depends(get_auth_context),
    ) -> dict:
        try:
            result = await send_connector_rpc(
                auth.user.id, method="skills.list", timeout_seconds=10.0,
            )
            return success(result)
        except HTTPException:
            return success({"skills": []})
        except Exception:
            return success({"skills": []})

    @app.get("/v1/cron")
    async def cron_list(
        auth: AuthContext = Depends(get_auth_context),
    ) -> dict:
        try:
            result = await send_connector_rpc(
                auth.user.id, method="cron.list", timeout_seconds=10.0,
            )
            return success(result)
        except HTTPException:
            return success({"jobs": []})
        except Exception:
            return success({"jobs": []})

    @app.post("/v1/cron")
    async def cron_create(
        body: CronCreateRequest,
        auth: AuthContext = Depends(get_auth_context),
    ) -> dict:
        try:
            result = await send_connector_rpc(
                auth.user.id,
                method="cron.create",
                params=body.model_dump(),
                timeout_seconds=10.0,
            )
            return success(result)
        except HTTPException as exc:
            raise exc
        except Exception as exc:
            raise HTTPException(status_code=500, detail=str(exc))

    @app.patch("/v1/cron/{job_id}")
    async def cron_update(
        job_id: str,
        body: CronUpdateRequest,
        auth: AuthContext = Depends(get_auth_context),
    ) -> dict:
        try:
            result = await send_connector_rpc(
                auth.user.id,
                method="cron.update",
                params={"id": job_id, **body.model_dump()},
                timeout_seconds=10.0,
            )
            return success(result)
        except HTTPException as exc:
            raise exc
        except Exception as exc:
            raise HTTPException(status_code=500, detail=str(exc))

    @app.delete("/v1/cron/{job_id}")
    async def cron_delete(
        job_id: str,
        auth: AuthContext = Depends(get_auth_context),
    ) -> dict:
        try:
            result = await send_connector_rpc(
                auth.user.id,
                method="cron.delete",
                params={"id": job_id},
                timeout_seconds=10.0,
            )
            return success(result)
        except HTTPException as exc:
            raise exc
        except Exception as exc:
            raise HTTPException(status_code=500, detail=str(exc))

    @app.get("/v1/memories")
    async def memory_list(
        auth: AuthContext = Depends(get_auth_context),
    ) -> dict:
        try:
            result = await send_connector_rpc(
                auth.user.id, method="memories.list", timeout_seconds=10.0,
            )
            return success(result)
        except HTTPException:
            return success({"memories": []})
        except Exception:
            return success({"memories": []})

    @app.get("/v1/tools")
    async def tool_list(
        auth: AuthContext = Depends(get_auth_context),
    ) -> dict:
        try:
            result = await send_connector_rpc(
                auth.user.id, method="tools.list", timeout_seconds=10.0,
            )
            return success(result)
        except HTTPException:
            return success({"tools": []})
        except Exception:
            return success({"tools": []})

    @app.post("/v1/hosts/current/revoke")
    def revoke_current_host(
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        host = revoke_current_herald_host(db, user_id=auth.user.id)
        record_audit(
            db,
            actor_type="user",
            actor_id=auth.user.id,
            action="host.revoke",
            entity_type="herald_host",
            entity_id=host.id if host else None,
        )
        db.commit()
        return success({"revoked": host is not None})

    @app.get("/v1/talk/readiness")
    async def talk_readiness(
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
        request_settings: Settings = Depends(get_settings),
    ) -> dict:
        host = current_herald_host_for_user(db, user_id=auth.user.id)
        host_data = serialize_herald_host(db, host=host, settings=request_settings)
        if host is None:
            return success(
                {
                    "ready": False,
                    "hostOnline": False,
                    "configured": False,
                    "blockedReason": "Connect a Hermes host before starting talk mode.",
                    "host": host_data,
                }
            )
        if not herald_host_is_online(db, host=host, settings=request_settings):
            return success(
                {
                    "ready": False,
                    "hostOnline": False,
                    "configured": False,
                    "blockedReason": "Your Hermes host is offline.",
                    "host": host_data,
                }
            )

        prewarm = await send_connector_rpc(auth.user.id, method="talk.prewarm")
        blocked_reason = prewarm.get("blockedReason") or prewarm.get("lastValidationError")
        return success(
            {
                "ready": bool(prewarm.get("configured")) and not blocked_reason,
                "hostOnline": True,
                "configured": bool(prewarm.get("configured")),
                "blockedReason": blocked_reason,
                "host": host_data,
                "preferredModels": prewarm.get("preferredModels"),
                "selectedModel": prewarm.get("selectedModel"),
                "voice": prewarm.get("voice"),
                "voiceContextUpdatedAt": prewarm.get("voiceContextUpdatedAt"),
            }
        )

    @app.post("/v1/talk/session")
    async def create_talk_session(
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
        request_settings: Settings = Depends(get_settings),
    ) -> JSONResponse:
        host = current_herald_host_for_user(db, user_id=auth.user.id)
        if host is None or not herald_host_is_online(db, host=host, settings=request_settings):
            raise HTTPException(status_code=status.HTTP_409_CONFLICT, detail="Your Hermes host must be online to start talk mode.")

        voice_session, relay_tool_token = create_voice_session(
            db,
            user_id=auth.user.id,
            host_id=host.id,
        )
        relay_mcp_url = f"{request_settings.public_base_url}/talk/mcp?token={relay_tool_token}"

        try:
            bootstrap = await send_connector_rpc(
                auth.user.id,
                method="talk.session.create",
                params={
                    "voiceSessionId": voice_session.id,
                    "relayMcpURL": relay_mcp_url,
                },
                timeout_seconds=request_settings.connector_rpc_timeout_seconds,
            )
        except HTTPException:
            with database.session() as cleanup_db:
                end_voice_session(cleanup_db, voice_session_id=voice_session.id)
            raise
        except RuntimeError as error:
            with database.session() as cleanup_db:
                end_voice_session(cleanup_db, voice_session_id=voice_session.id, last_error=str(error))
            raise HTTPException(status_code=status.HTTP_409_CONFLICT, detail=str(error)) from error
        except Exception as error:  # noqa: BLE001
            with database.session() as cleanup_db:
                end_voice_session(cleanup_db, voice_session_id=voice_session.id, last_error=str(error))
            raise

        started = mark_voice_session_started(
            db,
            voice_session_id=voice_session.id,
            realtime_session_id=(bootstrap.get("session") or {}).get("id"),
            realtime_model=bootstrap.get("model"),
            realtime_voice=bootstrap.get("voice"),
        )
        record_audit(
            db,
            actor_type="user",
            actor_id=auth.user.id,
            action="talk.session.create",
            entity_type="voice_session",
            entity_id=voice_session.id,
            payload={"model": bootstrap.get("model"), "voice": bootstrap.get("voice")},
        )
        db.commit()
        return success_response(
            {
                "voiceSession": serialize_voice_session(started or voice_session),
                "bootstrap": {
                    "clientSecret": bootstrap.get("clientSecret"),
                    "expiresAt": bootstrap.get("expiresAt"),
                    "session": bootstrap.get("session") or {},
                    "model": bootstrap.get("model"),
                    "voice": bootstrap.get("voice"),
                },
            }
        )

    @app.post("/v1/talk/session/{voice_session_id}/end")
    async def end_talk_session(
        voice_session_id: str,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        voice_session = get_voice_session(db, voice_session_id=voice_session_id)
        if voice_session is None or voice_session.user_id != auth.user.id:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Talk session not found.")

        if voice_session.status == "active":
            try:
                await send_connector_rpc(
                    auth.user.id,
                    method="talk.session.end",
                    params={"voiceSessionId": voice_session.id},
                )
            except HTTPException:
                pass
            end_voice_session(db, voice_session_id=voice_session.id)

        record_audit(
            db,
            actor_type="user",
            actor_id=auth.user.id,
            action="talk.session.end",
            entity_type="voice_session",
            entity_id=voice_session.id,
        )
        db.commit()
        return success({"ended": True, "voiceSession": serialize_voice_session(voice_session)})

    @app.post("/v1/talk/session/{voice_session_id}/turns")
    def create_talk_turn(
        voice_session_id: str,
        payload: VoiceTurnCreateRequest,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        voice_session = get_voice_session(db, voice_session_id=voice_session_id)
        if voice_session is None or voice_session.user_id != auth.user.id:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Talk session not found.")

        turn = record_voice_turn(
            db,
            voice_session_id=voice_session.id,
            client_turn_id=str(payload.clientTurnId) if payload.clientTurnId else None,
            role=payload.role,
            source=payload.source,
            text=payload.text.strip(),
        )
        record_audit(
            db,
            actor_type="user",
            actor_id=auth.user.id,
            action="talk.turn.create",
            entity_type="voice_turn",
            entity_id=turn.id,
            payload={
                "voiceSessionId": voice_session.id,
                "role": payload.role,
                "source": payload.source,
                "clientTurnId": str(payload.clientTurnId) if payload.clientTurnId else None,
            },
        )
        db.commit()
        return success({"turn": serialize_voice_turn(turn)})

    @app.post("/v1/talk/session/{voice_session_id}/inject")
    def inject_talk_transcript(
        voice_session_id: str,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        """Inject finalized voice turns into the current chat conversation."""
        voice_session = get_voice_session(db, voice_session_id=voice_session_id)
        if voice_session is None or voice_session.user_id != auth.user.id:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Talk session not found.")

        conversation = inject_voice_transcript(
            db,
            voice_session_id=voice_session.id,
            user_id=auth.user.id,
        )

        record_audit(
            db,
            actor_type="user",
            actor_id=auth.user.id,
            action="talk.transcript.inject",
            entity_type="voice_session",
            entity_id=voice_session.id,
        )
        db.commit()

        messages = list_conversation_messages(db, conversation_id=conversation.id)
        return success({
            "conversation": serialize_conversation(conversation=conversation, messages=messages),
        })

    @app.post("/v1/hosts/redeem")
    def redeem_host(
        payload: HostRedeemRequest,
        db: Session = Depends(get_db),
        request_settings: Settings = Depends(get_settings),
    ) -> dict:
        invite, host, connector_token = redeem_host_enrollment_invite(
            db,
            settings=request_settings,
            invite_token=payload.enrollmentToken,
            connector_display_name=payload.displayName,
            platform=payload.connector.platform,
            hostname=payload.connector.hostname,
            herald_command=payload.connector.heraldCommand,
            herald_version=payload.connector.heraldVersion,
            connector_version=payload.connector.connectorVersion,
        )
        record_audit(
            db,
            actor_type="connector",
            actor_id=host.id,
            action="host.redeem",
            entity_type="host_enrollment_invite",
            entity_id=invite.id,
            payload={"hostname": payload.connector.hostname},
        )
        db.commit()
        return success(
            {
                "host": {
                    "id": host.id,
                    "userId": host.user_id,
                },
                "connectorCredential": connector_token,
                "webSocketURL": build_connector_websocket_url(request_settings.public_base_url),
                "relayURL": request_settings.public_base_url,
            }
        )

    @app.get("/v1/session")
    def session(
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
        request_settings: Settings = Depends(get_settings),
    ) -> dict:
        push_registered = db.scalar(
            select(PushRegistration).where(
                PushRegistration.device_id == auth.device.id,
                PushRegistration.is_active.is_(True),
            )
        )

        return success(
            {
                "user": {
                    "id": auth.user.id,
                    "displayName": auth.user.display_name,
                },
                "device": {
                    "id": auth.device.id,
                    "registered": True,
                },
                "session": {
                    "connectionStatus": "connected",
                    "isMockMode": False,
                    "backendEndpoint": request_settings.public_base_url,
                    "lastSyncAt": auth.device.last_seen_at,
                },
                "push": {
                    "tokenRegistered": push_registered is not None,
                },
            }
        )

    @app.post("/v1/auth/refresh")
    def refresh_auth(
        payload: RefreshRequest,
        db: Session = Depends(get_db),
        request_settings: Settings = Depends(get_settings),
    ) -> dict:
        auth_session, access_token, refresh_token = refresh_auth_session(
            db,
            settings=request_settings,
            refresh_token=payload.refreshToken,
        )
        record_audit(
            db,
            actor_type="app",
            actor_id=auth_session.device_id,
            action="auth.refresh",
            entity_type="auth_session",
            entity_id=auth_session.id,
        )
        db.commit()

        return success(
            {
                "accessToken": access_token,
                "refreshToken": refresh_token,
                "expiresAt": auth_session.access_expires_at,
            }
        )

    @app.post("/v1/auth/revoke")
    def revoke_auth(
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        revoke_auth_session(db, auth_session=auth.auth_session)
        record_audit(
            db,
            actor_type="app",
            actor_id=auth.device.id,
            action="auth.revoke",
            entity_type="auth_session",
            entity_id=auth.auth_session.id,
        )
        db.commit()

        return success({"revoked": True})

    @app.post("/v1/push/register")
    def register_push(
        payload: PushRegisterRequest,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
        settings: Settings = Depends(get_settings),
    ) -> dict:
        if str(payload.deviceId) != auth.device.id:
            raise HTTPException(status_code=status.HTTP_403_FORBIDDEN, detail="Cannot register push token for another device.")

        registration = upsert_push_registration(
            db,
            device=auth.device,
            transport=payload.transport,
            apns_token=payload.apnsToken,
            push_environment=settings.apns_environment,  # Override: use relay env, not app-reported
            bundle_id=payload.bundleId,
            relay_handle=payload.relayHandle,
            send_grant=payload.sendGrant,
            relay_id=payload.relayId,
            relay_public_key=payload.relayPublicKey,
            token_debug_suffix=payload.tokenDebugSuffix,
        )
        record_audit(
            db,
            actor_type="app",
            actor_id=auth.device.id,
            action="push.register",
            entity_type="push_registration",
            entity_id=registration.id,
        )
        db.commit()

        return success(
            {
                "registered": True,
                "updatedAt": registration.updated_at,
            }
        )

    @app.post("/v1/push/deactivate")
    def deactivate_push(
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        """Deactivate push registrations for this device.

        Called when the user disables notifications in-app. Marks all
        active push registrations for the device as inactive so the
        relay stops sending pushes.
        """
        registrations = db.scalars(
            select(PushRegistration).where(
                PushRegistration.device_id == auth.device.id,
                PushRegistration.is_active == True,
            )
        ).all()
        for registration in registrations:
            registration.is_active = False
        if registrations:
            record_audit(
                db,
                actor_type="app",
                actor_id=auth.device.id,
                action="push.deactivate",
                entity_type="push_registration",
                entity_id=registrations[0].id,
            )
            db.commit()

        return success({"deactivated": True, "count": len(registrations)})

    @app.post("/v1/device/app-state")
    def device_app_state(
        payload: DeviceAppStateRequest,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        device = update_device_app_state(db, device=auth.device, state=payload.state)
        record_audit(
            db,
            actor_type="app",
            actor_id=auth.device.id,
            action="device.app_state",
            entity_type="device",
            entity_id=device.id,
            payload={"state": payload.state},
        )
        db.commit()
        return success(
            {
                "deviceId": device.id,
                "state": device.app_state,
                "updatedAt": device.app_state_updated_at,
            }
        )

    @app.post("/v1/push/send")
    async def send_push(
        request: Request,
        payload: dict = Body(...),
        db: Session = Depends(get_db),
        _internal: None = Depends(require_internal_key),
    ) -> dict:
        """Send a push notification to all active devices for a user.

        Called by the connector (via internal API key) when the agent has
        a proactive message or wants to wake the iOS app.

        Body:
            user_id: str — target user ID
            type: "silent" | "alert" (default: silent)
            title: str (for alert type)
            body: str (for alert type)
        """
        apns_client = request.app.state.apns_client
        if apns_client is None:
            raise HTTPException(
                status_code=status.HTTP_503_SERVICE_UNAVAILABLE,
                detail="APNs not configured. Set APNS_KEY_PATH, APNS_KEY_ID, APNS_TEAM_ID.",
            )

        user_id = payload.get("user_id")
        if not user_id:
            raise HTTPException(status_code=400, detail="user_id is required")

        push_type = payload.get("type", "silent")

        from .models import Device
        device_ids = [
            d.id for d in db.scalars(
                select(Device).where(Device.user_id == user_id)
            ).all()
        ]

        registrations = db.scalars(
            select(PushRegistration).where(
                PushRegistration.device_id.in_(device_ids),
                PushRegistration.is_active == True,
            )
        ).all()

        if not registrations:
            return success({"sent": 0, "reason": "no active push registrations"})

        sent = 0
        for reg in registrations:
            if push_type == "alert":
                title = payload.get("title", "Herald")
                body_text = payload.get("body", "New message from Herald")
                result = await apns_client.send_alert_push(
                    reg.apns_token,
                    title=title,
                    body=body_text,
                    bundle_id=reg.bundle_id,
                    environment=reg.push_environment,
                )
            else:
                result = await apns_client.send_silent_push(
                    reg.apns_token,
                    bundle_id=reg.bundle_id,
                    environment=reg.push_environment,
                )

            if result == PushResult.SENT:
                sent += 1
            elif result == PushResult.TOKEN_INVALID:
                reg.is_active = False
                db.commit()

        record_audit(
            db,
            actor_type="internal",
            actor_id="relay",
            action="push.send",
            entity_type="user",
            entity_id=user_id,
        )
        db.commit()

        # Create inbox item for each push sent
        if sent > 0:
            create_inbox_item(
                db,
                user_id=user_id,
                device_id=None,
                kind="notification",
                title=payload.get("title", "Herald"),
                body=payload.get("body", "New message")[:200],
                priority="normal",
                payload={"conversationId": payload.get("conversationId")},
                expires_at=None,
            )
            db.commit()
        return success({"sent": sent, "total": len(registrations)})

    @app.get("/v1/conversations/current")
    def current_conversation(auth: AuthContext = Depends(get_auth_context), db: Session = Depends(get_db)) -> dict:
        conversation = get_or_create_current_conversation(db, user_id=auth.user.id, device_id=auth.device.id)
        messages = list_conversation_messages(db, conversation_id=conversation.id)
        jobs = list_message_jobs_for_conversation(db, conversation_id=conversation.id)
        return success({"conversation": serialize_conversation(conversation, messages, jobs=jobs)})

    @app.post("/v1/conversations/current/clear")
    def clear_current_conversation(
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        archive_current_conversation(db, user_id=auth.user.id, device_id=auth.device.id)
        conversation = get_or_create_current_conversation(db, user_id=auth.user.id, device_id=auth.device.id)
        messages = list_conversation_messages(db, conversation_id=conversation.id)
        record_audit(
            db,
            actor_type="user",
            actor_id=auth.user.id,
            action="chat.conversation.clear",
            entity_type="conversation",
            entity_id=conversation.id,
        )
        db.commit()
        return success({"conversation": serialize_conversation(conversation, messages)})

    # ── Session Management ──────────────────────────────────────────

    @app.get("/v1/sessions")
    def list_user_sessions(
        limit: int = 50,
        offset: int = 0,
        allDevices: bool = False,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        sessions, total = list_sessions(
            db,
            user_id=auth.user.id,
            device_id=None if allDevices else auth.device.id,
            limit=limit,
            offset=offset,
        )
        return success({
            "sessions": [serialize_session_summary(s) for s in sessions],
            "total": total,
        })

    @app.get("/v1/sessions/search")
    def search_user_sessions(
        q: str,
        allDevices: bool = False,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        sessions = search_sessions(
            db,
            user_id=auth.user.id,
            query=q,
            device_id=None if allDevices else auth.device.id,
        )
        return success({
            "sessions": [serialize_session_summary(s) for s in sessions],
            "total": len(sessions),
        })

    @app.post("/v1/sessions", status_code=status.HTTP_201_CREATED)
    def create_user_session(
        body: CreateSessionBody,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        session = create_session(
            db,
            user_id=auth.user.id,
            device_id=auth.device.id,
            title=body.title,
        )
        return success({"session": serialize_session_summary(session)})

    @app.get("/v1/sessions/{session_id}")
    def get_user_session(
        session_id: str,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        session = get_session(db, session_id=session_id)
        if session is None or session.user_id != auth.user.id:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Session not found.")
        return success({"session": serialize_session_summary(session)})

    @app.get("/v1/sessions/{session_id}/conversation")
    def get_session_conversation(
        session_id: str,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        conversation = get_session(db, session_id=session_id)
        if conversation is None or conversation.user_id != auth.user.id:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Session not found.")
        messages = list_conversation_messages(db, conversation_id=conversation.id)
        jobs = list_message_jobs_for_conversation(db, conversation_id=conversation.id)
        return success({"conversation": serialize_conversation(conversation, messages, jobs=jobs)})

    @app.delete("/v1/sessions/{session_id}")
    def delete_user_session(
        session_id: str,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        session = get_session(db, session_id=session_id)
        if session is None or session.user_id != auth.user.id:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Session not found.")
        delete_session(db, session_id=session_id)
        record_audit(
            db,
            actor_type="user",
            actor_id=auth.user.id,
            action="session.delete",
            entity_type="session",
            entity_id=session_id,
        )
        db.commit()
        return success({"deleted": True})

    @app.post("/v1/sessions/{session_id}/archive")
    def archive_user_session(
        session_id: str,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        session = get_session(db, session_id=session_id)
        if session is None or session.user_id != auth.user.id:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Session not found.")
        updated = archive_session(db, session_id=session_id)
        return success({"session": serialize_session_summary(updated) if updated else None})

    @app.post("/v1/sessions/{session_id}/pin")
    def toggle_pin_user_session(
        session_id: str,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        session = get_session(db, session_id=session_id)
        if session is None or session.user_id != auth.user.id:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Session not found.")
        updated = toggle_pin_session(db, session_id=session_id)
        return success({"session": serialize_session_summary(updated) if updated else None})

    @app.patch("/v1/sessions/{session_id}")
    def rename_user_session(
        session_id: str,
        body: RenameSessionBody,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        session = get_session(db, session_id=session_id)
        if session is None or session.user_id != auth.user.id:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Session not found.")
        updated = rename_session(db, session_id=session_id, title=body.title)
        return success({"session": serialize_session_summary(updated) if updated else None})

    @app.get("/v1/messages/{message_id}/attachments/{index}")
    def message_attachment_bytes(
        message_id: str,
        index: int,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ):
        """Return the raw bytes of a single message attachment.

        Conversation loads only carry attachment metadata (and small
        thumbnails); the heavy base64 payload lives in `attachments_data`.
        The app fetches full-resolution images and file bodies here on demand
        for inline display, full-screen viewing, and opening/saving files.
        """
        message = db.get(Message, message_id)
        if message is None:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Message not found.")
        conversation = db.get(Conversation, message.conversation_id)
        if conversation is None or conversation.user_id != auth.user.id:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Message not found.")

        attachments = message.attachments_data or []
        if index < 0 or index >= len(attachments):
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Attachment not found.")

        attachment = attachments[index]
        raw = attachment.get("data")
        if not raw:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Attachment has no stored data.")
        try:
            payload = base64.b64decode(raw)
        except (ValueError, TypeError):
            raise HTTPException(status_code=status.HTTP_422_UNPROCESSABLE_ENTITY, detail="Attachment data is corrupt.")

        filename = attachment.get("filename", "attachment")
        mime_type = attachment.get("mimeType", "application/octet-stream")
        return Response(
            content=payload,
            media_type=mime_type,
            headers={
                "Content-Disposition": f'inline; filename="{filename}"',
                "Cache-Control": "private, max-age=86400",
            },
        )

    # ── Messages ────────────────────────────────────────────────────
    @app.post("/v1/messages")
    async def create_message(
        payload: MessageCreateRequest,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
        request_settings: Settings = Depends(get_settings),
    ) -> JSONResponse:
        client_message_id = str(payload.clientMessageId) if payload.clientMessageId else None
        if client_message_id is not None:
            existing_user_message = get_user_message_by_client_message_id(
                db,
                user_id=auth.user.id,
                client_message_id=client_message_id,
            )
            if existing_user_message is not None:
                existing_job = get_message_job_for_user_message(db, user_message_id=existing_user_message.id)
                if existing_job is not None:
                    payload_data, status_code = build_message_response_payload(
                        db,
                        conversation_id=existing_user_message.conversation_id,
                        job_id=existing_job.id,
                    )
                    record_audit(
                        db,
                        actor_type="user",
                        actor_id=auth.user.id,
                        action="chat.message.create",
                        entity_type="conversation",
                        entity_id=existing_user_message.conversation_id,
                        payload={
                            "jobId": existing_job.id,
                            "replyState": payload_data["replyState"],
                            "deduplicated": True,
                            "clientMessageId": client_message_id,
                        },
                    )
                    db.commit()
                    return success_response(payload_data, status_code=status_code)

        if payload.conversationId is not None:
            conversation = get_session(db, session_id=str(payload.conversationId))
            if conversation is None or conversation.user_id != auth.user.id:
                raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Conversation not found.")
        else:
            conversation = get_or_create_current_conversation(db, user_id=auth.user.id, device_id=auth.device.id)
        initial_delivery_status = "pending" if request_settings.herald_adapter == "connector" else "sent"
        attachments_raw = (
            [att.model_dump() for att in payload.attachments]
            if payload.attachments
            else None
        )
        user_message = append_message(
            db,
            conversation=conversation,
            user_id=auth.user.id,
            role="user",
            text=payload.text,
            client_message_id=client_message_id,
            delivery_status=initial_delivery_status,
            attachments_data=attachments_raw,
        )

        job = create_message_job(
            db,
            user_id=auth.user.id,
            conversation_id=conversation.id,
            user_message_id=user_message.id,
            session_id_snapshot=conversation.herald_session_id,
            reasoning_effort=payload.reasoningEffort,
        )

        if request_settings.herald_adapter == "connector":
            host = current_herald_host_for_user(db, user_id=auth.user.id)
            if host is not None and herald_host_is_online(db, host=host, settings=request_settings):
                await wait_for_job_completion(job.id, request_settings.connector_sync_wait_seconds)
        else:
            process_message_job_with_adapter(
                db,
                job_id=job.id,
                adapter=app.state.herald_adapter,
            )
            db.expire_all()
            completed_job = get_message_job(db, job_id=job.id)
            if completed_job is not None and completed_job.status == "completed" and completed_job.result_message_id:
                result_message = db.get(Message, completed_job.result_message_id)
                if result_message is not None:
                    await maybe_send_message_push(
                        db=db,
                        user_id=auth.user.id,
                        conversation_id=conversation.id,
                        message_id=result_message.id,
                        message_text=result_message.text,
                        job_id=job.id,
                        category="HERALD_MESSAGE_READY",
                    )

        db.expire_all()
        payload_data, status_code = build_message_response_payload(db, conversation_id=conversation.id, job_id=job.id)
        # For connector-backed jobs, ALWAYS return "pending" so the iOS client
        # opens an SSE connection for streaming. If we return "delivered" here
        # (because the connector finished before db.expire_all), the client
        # skips SSE and never sees streaming events.
        if request_settings.herald_adapter == "connector" and payload_data["replyState"] != "failed":
            payload_data["replyState"] = "pending"
            strip_current_job_result_from_pending_payload(payload_data, job_id=job.id)
            status_code = status.HTTP_202_ACCEPTED
        record_audit(
            db,
            actor_type="user",
            actor_id=auth.user.id,
            action="chat.message.create",
            entity_type="conversation",
            entity_id=conversation.id,
            payload={"jobId": job.id, "replyState": payload_data["replyState"]},
        )
        db.commit()
        return success_response(payload_data, status_code=status_code)

    # ------------------------------------------------------------------
    # Job Status Snapshot
    # ------------------------------------------------------------------

    @app.get("/v1/jobs/{job_id}")
    async def get_job_status(
        job_id: str,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ):
        """Return the authoritative status of a job. Used for recovery after SSE gaps."""
        from .models import MessageJob

        job = get_message_job(db, job_id=job_id)
        if job is None or job.user_id != auth.user.id:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Job not found.")

        result: dict = {
            "jobId": job.id,
            "status": job.status,
            "conversationId": job.conversation_id,
            "attempt": job.attempt,
            "lastSeq": get_job_last_seq(db, job.id),
            "createdAt": job.created_at.isoformat() if job.created_at else None,
            "claimedAt": job.claimed_at.isoformat() if job.claimed_at else None,
            "leaseExpiresAt": job.lease_expires_at.isoformat() if job.lease_expires_at else None,
            "completedAt": job.completed_at.isoformat() if job.completed_at else None,
            "retryable": job.retryable,
        }
        if job.result_message_id:
            result_message = db.get(Message, job.result_message_id)
            if result_message is not None:
                result["message"] = serialize_message(result_message, job=job)
        if job.error_text:
            result["error"] = job.error_text
        if job.usage_data:
            result["usage"] = job.usage_data
        if job.diff_data:
            result["diff"] = job.diff_data
        return success_response(result)

    @app.post("/v1/jobs/{job_id}/cancel")
    async def cancel_job(
        job_id: str,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ):
        """Cancel a running or queued job."""
        from .models import MessageJob

        job = get_message_job(db, job_id=job_id)
        if job is None or job.user_id != auth.user.id:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Job not found.")

        # Already terminal — return idempotent success
        if job.status in ("completed", "failed", "cancelled"):
            return success_response({"jobId": job_id, "status": job.status})

        # If queued, cancel directly without connector dispatch
        if job.status == "queued":
            job.status = "cancelled"
            job.error_text = "Cancelled by user"
            job.updated_at = utcnow()
            db.commit()
            publish_job_event(job_id, {
                "event": "done",
                "data": {"jobId": job_id, "status": "cancelled", "error": "Cancelled by user", "message": None},
            })
            return success_response({"jobId": job_id, "status": "cancelled"})

        # If running, dispatch connector RPC to cancel
        if job.status == "running":
            # Try to dispatch cancel to connector
            connector_session = connector_session_for_user(auth.user.id)
            if connector_session is not None:
                try:
                    rpc_result = await send_connector_rpc(
                        auth.user.id,
                        method="jobs.cancel",
                        params={"jobId": job_id},
                    )
                    if rpc_result and rpc_result.get("status") == "cancelled":
                        job.status = "cancelled"
                        job.error_text = "Cancelled by user"
                        job.updated_at = utcnow()
                        db.commit()
                        publish_job_event(job_id, {
                            "event": "done",
                            "data": {"jobId": job_id, "status": "cancelled", "error": "Cancelled by user", "message": None},
                        })
                        return success_response({"jobId": job_id, "status": "cancelled"})
                except Exception:
                    logger.warning("Failed to dispatch cancel RPC for job %s", job_id, exc_info=True)

            # If connector dispatch failed or no connector, mark as cancelled in DB
            # The connector will see the job is cancelled when it tries to complete it
            job.status = "cancelled"
            job.error_text = "Cancelled by user"
            job.updated_at = utcnow()
            db.commit()
            publish_job_event(job_id, {
                "event": "done",
                "data": {"jobId": job_id, "status": "cancelled", "error": "Cancelled by user", "message": None},
            })
            return success_response({"jobId": job_id, "status": "cancelled"})

        raise HTTPException(status_code=status.HTTP_400_BAD_REQUEST, detail=f"Cannot cancel job in status: {job.status}")

    # ------------------------------------------------------------------
    # Job Events SSE
    # ------------------------------------------------------------------

    @app.get("/v1/jobs/{job_id}/events")
    async def job_events_stream(
        job_id: str,
        request: Request,
        after: int = 0,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ):
        from .models import MessageJob

        job = get_message_job(db, job_id=job_id)
        if job is None or job.user_id != auth.user.id:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Job not found.")

        # Determine replay cursor from Last-Event-ID header or ?after= query param
        cursor = after
        last_event_id = request.headers.get("last-event-id")
        if last_event_id is not None:
            try:
                cursor = int(last_event_id)
            except (ValueError, TypeError):
                pass

        async def event_stream():
            last_seq = cursor

            def emit_db_event(evt: dict) -> str:
                """Format a DB event dict as an SSE frame with id: line."""
                nonlocal last_seq
                event_type = evt.get("type", "progress")
                seq = evt.get("seq", 0)
                last_seq = seq
                payload = evt.get("payload", {})
                data = json.dumps(jsonable_encoder(payload))
                return f"id: {seq}\nevent: {event_type}\ndata: {data}\n\n"

            def build_terminal_event(status_val: str, job_row) -> str:
                """Build a terminal SSE frame (completed/failed/cancelled).

                Persists the terminal event to the durable log before emitting,
                ensuring replays return the same terminal sequence and payload.
                """
                nonlocal last_seq

                # Persist terminal event through append_job_event
                terminal_payload: dict = {"jobId": job_id, "status": status_val}
                if status_val == "completed":
                    if job_row.usage_data:
                        terminal_payload["usage"] = job_row.usage_data
                    if job_row.diff_data:
                        terminal_payload["diff"] = job_row.diff_data
                    if job_row.result_message_id:
                        with database.session() as db2:
                            result_msg = db2.get(Message, job_row.result_message_id)
                            if result_msg is not None:
                                terminal_payload["message"] = serialize_message(result_msg, job=job_row)
                elif status_val == "failed":
                    if job_row.error_text:
                        terminal_payload["error"] = job_row.error_text
                    if job_row.diff_data:
                        terminal_payload["diff"] = job_row.diff_data
                    if job_row.result_message_id:
                        with database.session() as db2:
                            result_msg = db2.get(Message, job_row.result_message_id)
                            if result_msg is not None:
                                terminal_payload["message"] = serialize_message(result_msg, job=job_row)
                elif status_val == "cancelled":
                    if job_row.error_text:
                        terminal_payload["error"] = job_row.error_text

                # Persist the terminal event
                with database.session() as db:
                    result = append_job_event(
                        db,
                        job_id=job_id,
                        event_type=status_val,
                        payload=terminal_payload,
                        source_seq=None,
                        attempt=job_row.attempt,
                    )
                    db.commit()
                    if result is not None:
                        terminal_seq = result["seq"]
                    else:
                        # Already persisted (e.g. race) — read the existing seq
                        terminal_seq = get_job_last_seq(db, job_id)

                last_seq = terminal_seq
                return f"id: {terminal_seq}\nevent: done\ndata: {json.dumps(jsonable_encoder(terminal_payload))}\n\n"

            # --- Phase 1: Replay persisted events from DB ---
            with database.session() as replay_db:
                persisted = get_job_events_after(replay_db, job_id, after_seq=cursor)

            for evt in persisted:
                yield emit_db_event(evt)
                if evt.get("type") in ("completed", "failed", "cancelled"):
                    return

            # --- Phase 1b: If job already terminal, emit terminal event and close ---
            with database.session() as check_db:
                current_job = get_message_job(check_db, job_id=job_id)
                if current_job is not None and current_job.status in ("completed", "failed", "cancelled"):
                    yield build_terminal_event(current_job.status, current_job)
                    return

            # --- Phase 2: Subscribe to EventFanout for live events ---
            wake_queue = await subscribe_job_events(job_id)

            try:
                while True:
                    try:
                        await asyncio.wait_for(wake_queue.get(), timeout=float(settings.sse_keepalive_seconds))
                    except asyncio.TimeoutError:
                        yield ": keepalive\n\n"
                        continue

                    # Got a signal — query DB for new events
                    with database.session() as live_db:
                        new_events = get_job_events_after(live_db, job_id, after_seq=last_seq)

                    for evt in new_events:
                        yield emit_db_event(evt)

                    # Check if job became terminal
                    with database.session() as check_db:
                        current_job = get_message_job(check_db, job_id=job_id)
                        if current_job is not None and current_job.status in ("completed", "failed", "cancelled"):
                            # Drain any remaining events
                            with database.session() as drain_db:
                                remaining = get_job_events_after(drain_db, job_id, after_seq=last_seq)
                            for evt in remaining:
                                yield emit_db_event(evt)
                            yield build_terminal_event(current_job.status, current_job)
                            return
            finally:
                await unsubscribe_job_events(job_id, wake_queue)

        return StreamingResponse(
            event_stream(),
            media_type="text/event-stream",
            headers={"Cache-Control": "no-cache", "X-Accel-Buffering": "no"},
        )

    # ------------------------------------------------------------------
    # Inbox
    # ------------------------------------------------------------------

    @app.get("/v1/inbox")
    def inbox(auth: AuthContext = Depends(get_auth_context), db: Session = Depends(get_db)) -> dict:
        items = [serialize_inbox_item(item) for item in list_inbox_items(db, user_id=auth.user.id)]
        return success({"items": items})

    @app.post("/v1/inbox/{item_id}/action")
    def inbox_action(
        item_id: str,
        payload: InboxActionRequest,
        auth: AuthContext = Depends(get_auth_context),
        db: Session = Depends(get_db),
    ) -> dict:
        item = get_inbox_item_for_user(db, item_id=item_id, user_id=auth.user.id)
        action = record_inbox_action(db, item=item, action_id=payload.actionId, actor_type="user")
        record_audit(
            db,
            actor_type="user",
            actor_id=auth.user.id,
            action=f"inbox.{payload.actionId}",
            entity_type="inbox_item",
            entity_id=item.id,
        )
        db.commit()

        return success(
            {
                "itemID": item.id,
                "actionID": action.action_id,
                "status": item.status,
                "completedAt": action.created_at,
            }
        )

    @app.post("/internal/inbox/create", dependencies=[Depends(require_internal_key)])
    def internal_create_inbox(
        payload: InternalInboxCreateRequest,
        db: Session = Depends(get_db),
        request_settings: Settings = Depends(get_settings),
    ) -> dict:
        user = ensure_default_user(db, request_settings)
        target_user_id = str(payload.userId) if payload.userId else user.id
        target_device_id = str(payload.deviceId) if payload.deviceId else None
        item = create_inbox_item(
            db,
            user_id=target_user_id,
            device_id=target_device_id,
            kind=payload.kind,
            title=payload.title,
            body=payload.body,
            priority=payload.priority,
            payload=payload.payload,
            expires_at=payload.expiresAt,
        )
        record_audit(
            db,
            actor_type="herald",
            action="internal.inbox.create",
            entity_type="inbox_item",
            entity_id=item.id,
        )
        db.commit()

        return success({"item": serialize_inbox_item(item)})

    @app.get("/internal/inbox/{item_id}/actions", dependencies=[Depends(require_internal_key)])
    def internal_inbox_actions(item_id: str, db: Session = Depends(get_db)) -> dict:
        actions = list_inbox_actions(db, item_id=item_id)
        return success(
            {
                "actions": [
                    {
                        "id": action.id,
                        "actionId": action.action_id,
                        "actorType": action.actor_type,
                        "result": action.result,
                        "createdAt": action.created_at,
                    }
                    for action in actions
                ]
            }
        )

    @app.websocket("/v1/hosts/ws")
    async def hosts_websocket(websocket: WebSocket) -> None:
        await websocket.accept()
        authorization = websocket.headers.get("authorization")
        connector_token = parse_bearer_token(authorization)
        if connector_token is None:
            await websocket.close(code=4401)
            return

        try:
            hello_message = await websocket.receive_json()
        except WebSocketDisconnect:
            return

        if hello_message.get("type") != "hello":
            await websocket.close(code=4400)
            return

        connector_info = hello_message.get("connector") or {}
        connection_nonce = str(uuid.uuid4())
        host_id: str | None = None
        user_id: str | None = None
        # Track the job currently being executed so the `finally` block can
        # surface a `done`/failed event if the connector drops mid-response.
        # Without this, iOS SSE clients hang until their own request timeout.
        in_flight_job_id: str | None = None

        try:
            with database.session() as db:
                host = authenticate_herald_host(db, connector_token=connector_token)
                activated_host = activate_herald_host_connection(
                    db,
                    host=host,
                    connection_nonce=connection_nonce,
                    connector_version=connector_info.get("connectorVersion", "unknown"),
                    platform=connector_info.get("platform", "unknown"),
                    hostname=connector_info.get("hostname", "unknown"),
                    herald_command=connector_info.get("heraldCommand", "hermes"),
                    herald_version=connector_info.get("heraldVersion"),
                    herald_model=connector_info.get("heraldModel"),
                    display_name=connector_info.get("displayName"),
                )
                host_id = activated_host.id
                user_id = activated_host.user_id
                set_connector_session(
                    user_id,
                    ConnectorSession(
                        websocket=websocket,
                        host_id=host_id,
                        connection_nonce=connection_nonce,
                    ),
                )
                ready_payload = {
                    "type": "ready",
                    "version": 1,
                    "host": serialize_herald_host(db, host=activated_host, settings=settings),
                }

            await websocket.send_json(jsonable_encoder(ready_payload))

            while True:
                with database.session() as db:
                    host = db.get(HeraldHost, host_id)
                    if host is None or host.active_connection_nonce != connection_nonce or host.revoked_at is not None:
                        await websocket.close(code=4401)
                        return

                    claimed_job = claim_next_message_job(
                        db,
                        host=host,
                        connection_nonce=connection_nonce,
                        settings=settings,
                    )

                    if claimed_job is not None:
                        job_payload = build_job_execute_payload(db, job_id=claimed_job.id)
                    else:
                        job_payload = None

                if job_payload is not None:
                    session = connector_session_for_user(user_id)
                    if session is not None and session.connection_nonce == connection_nonce:
                        session.busy = True

                    in_flight_job_id = claimed_job.id

                    def fail_stuck_job(reason: str) -> None:
                        with database.session() as db:
                            fail_message_job(
                                db,
                                job_id=claimed_job.id,
                                connection_nonce=connection_nonce,
                                error_text=reason,
                                retryable=True,
                            )
                        publish_job_event(claimed_job.id, {
                            "event": "done",
                            "data": {
                                "jobId": claimed_job.id,
                                "status": "failed",
                                "error": reason,
                                "message": None,
                            },
                        })

                    try:
                        await websocket.send_json(job_payload)

                        # Use the database lease_expires_at as the authoritative
                        # deadline. job.started, job.heartbeat, and job.progress
                        # messages from the connector renew this lease. Generic
                        # heartbeats and RPC traffic do NOT renew it.
                        while True:
                            # Check lease expiry by reading from database
                            with database.session() as db:
                                current_job = get_message_job(db, job_id=claimed_job.id)
                                if current_job is None or current_job.status not in ("running", "queued"):
                                    # Job was completed/failed externally
                                    in_flight_job_id = None
                                    break
                                if (
                                    current_job.lease_expires_at
                                    and utcnow() >= normalize_datetime(current_job.lease_expires_at)
                                ):
                                    fail_stuck_job("Hermes host stopped responding.")
                                    await websocket.close(code=1011)
                                    return

                            try:
                                incoming = await asyncio.wait_for(
                                    websocket.receive_json(),
                                    timeout=settings.connector_heartbeat_timeout_seconds,
                                )
                            except asyncio.TimeoutError:
                                # No message received — check lease again next iteration
                                continue

                            message_type = incoming.get("type")
                            if message_type == "heartbeat":
                                with database.session() as db:
                                    touched = touch_herald_host_connection(db, host_id=host_id, connection_nonce=connection_nonce)
                                if touched is None:
                                    await websocket.close(code=4401)
                                    return
                                continue

                            if message_type == "job.started" and incoming.get("jobId") == claimed_job.id:
                                with database.session() as db:
                                    renew_message_job_lease(db, job_id=claimed_job.id, connection_nonce=connection_nonce, settings=settings)
                                started_event: dict = {
                                    "event": "started",
                                    "data": {
                                        "jobId": claimed_job.id,
                                        "phase": incoming.get("phase", "starting"),
                                    },
                                }
                                if "sourceSeq" in incoming:
                                    started_event["sourceSeq"] = incoming["sourceSeq"]
                                publish_job_event(claimed_job.id, started_event)
                                continue

                            if message_type == "job.heartbeat" and incoming.get("jobId") == claimed_job.id:
                                with database.session() as db:
                                    renew_message_job_lease(db, job_id=claimed_job.id, connection_nonce=connection_nonce, settings=settings)
                                heartbeat_event: dict = {
                                    "event": "heartbeat",
                                    "data": {
                                        "jobId": claimed_job.id,
                                        "phase": incoming.get("phase", "unknown"),
                                    },
                                }
                                if "sourceSeq" in incoming:
                                    heartbeat_event["sourceSeq"] = incoming["sourceSeq"]
                                publish_job_event(claimed_job.id, heartbeat_event)
                                continue

                            if message_type == "sensor.ack":
                                resolve_sensor_delivery(
                                    incoming.get("deliveryId"),
                                    delivered=incoming.get("deliveryState", "delivered") == "delivered",
                                )
                                continue

                            if message_type == "rpc.response":
                                resolve_connector_rpc_response(
                                    incoming.get("requestId"),
                                    success=bool(incoming.get("success", False)),
                                    result=incoming.get("result"),
                                    error=incoming.get("error"),
                                )
                                continue

                            if message_type == "job.progress" and incoming.get("jobId") == claimed_job.id:
                                with database.session() as db:
                                    renew_message_job_lease(db, job_id=claimed_job.id, connection_nonce=connection_nonce, settings=settings)
                                progress_event: dict = {
                                    "event": incoming.get("kind", "progress"),
                                    "data": {
                                        "jobId": claimed_job.id,
                                        "kind": incoming.get("kind"),
                                        "delta": incoming.get("delta"),
                                        "label": incoming.get("label"),
                                    },
                                }
                                if "sourceSeq" in incoming:
                                    progress_event["sourceSeq"] = incoming["sourceSeq"]
                                publish_job_event(claimed_job.id, progress_event)
                                continue

                            if message_type == "job.result" and incoming.get("jobId") == claimed_job.id:
                                with database.session() as db:
                                    completed = complete_message_job(
                                        db,
                                        job_id=claimed_job.id,
                                        connection_nonce=connection_nonce,
                                        text=incoming.get("text", "").strip(),
                                        session_id=incoming.get("sessionId"),
                                        usage=incoming.get("usage"),
                                        diff=incoming.get("diff"),
                                        attachments=incoming.get("attachments"),
                                    )
                                    if completed is None:
                                        await websocket.close(code=1011)
                                        return
                                    result_message = (
                                        serialize_message(db.get(Message, completed.result_message_id), job=completed)
                                        if completed.result_message_id and db.get(Message, completed.result_message_id) is not None
                                        else None
                                    )
                                    if completed.result_message_id:
                                        completed_message = db.get(Message, completed.result_message_id)
                                        if completed_message is not None:
                                            await maybe_send_message_push(
                                                db=db,
                                                user_id=completed.user_id,
                                                conversation_id=completed.conversation_id,
                                                message_id=completed_message.id,
                                                message_text=completed_message.text,
                                                job_id=claimed_job.id,
                                                category="HERALD_MESSAGE_READY",
                                            )
                                done_event_data: dict = {
                                    "jobId": claimed_job.id,
                                    "status": "completed",
                                    "usage": completed.usage_data,
                                    "message": result_message,
                                }
                                if completed.diff_data:
                                    done_event_data["diff"] = completed.diff_data
                                publish_job_event(claimed_job.id, {
                                    "event": "done",
                                    "data": done_event_data,
                                })
                                in_flight_job_id = None
                                break

                            if message_type == "job.failed" and incoming.get("jobId") == claimed_job.id:
                                with database.session() as db:
                                    failed = fail_message_job(
                                        db,
                                        job_id=claimed_job.id,
                                        connection_nonce=connection_nonce,
                                        error_text=incoming.get("error", "Hermes connector failed."),
                                        retryable=bool(incoming.get("retryable", False)),
                                    )
                                    if failed is None:
                                        await websocket.close(code=1011)
                                        return
                                    result_message = (
                                        serialize_message(db.get(Message, failed.result_message_id), job=failed)
                                        if failed.result_message_id and db.get(Message, failed.result_message_id) is not None
                                        else None
                                    )
                                if failed.status != "queued":
                                    publish_job_event(claimed_job.id, {
                                        "event": "done",
                                        "data": {
                                            "jobId": claimed_job.id,
                                            "status": "failed",
                                            "error": incoming.get("error"),
                                            "message": result_message,
                                        },
                                    })
                                in_flight_job_id = None
                                break

                            await websocket.close(code=4400)
                            return
                    finally:
                        session = connector_session_for_user(user_id)
                        if session is not None and session.connection_nonce == connection_nonce:
                            session.busy = False

                    continue

                try:
                    incoming = await asyncio.wait_for(
                        websocket.receive_json(),
                        timeout=settings.connector_idle_poll_interval_seconds,
                    )
                except asyncio.TimeoutError:
                    continue

                if incoming.get("type") == "heartbeat":
                    with database.session() as db:
                        touched = touch_herald_host_connection(db, host_id=host_id, connection_nonce=connection_nonce)
                    if touched is None:
                        await websocket.close(code=4401)
                        return
                    continue

                if incoming.get("type") == "sensor.ack":
                    resolve_sensor_delivery(
                        incoming.get("deliveryId"),
                        delivered=incoming.get("deliveryState", "delivered") == "delivered",
                    )
                    with database.session() as db:
                        touched = touch_herald_host_connection(db, host_id=host_id, connection_nonce=connection_nonce)
                    if touched is None:
                        await websocket.close(code=4401)
                        return
                    continue

                if incoming.get("type") == "rpc.response":
                    resolve_connector_rpc_response(
                        incoming.get("requestId"),
                        success=bool(incoming.get("success", False)),
                        result=incoming.get("result"),
                        error=incoming.get("error"),
                    )
                    with database.session() as db:
                        touched = touch_herald_host_connection(db, host_id=host_id, connection_nonce=connection_nonce)
                    if touched is None:
                        await websocket.close(code=4401)
                        return
                    continue

                await websocket.close(code=4400)
                return
        except HTTPException:
            await websocket.close(code=4401)
            return
        except WebSocketDisconnect:
            return
        finally:
            if in_flight_job_id is not None:
                # Connector dropped mid-response. Instead of immediately failing
                # the job, publish a reconnecting event. The job's lease_expires_at
                # will cause it to be failed/requeued if the connector doesn't
                # reconnect in time.
                logger.info(
                    "Connector disconnected with in-flight job %s — lease governs recovery",
                    in_flight_job_id,
                )
                publish_job_event(in_flight_job_id, {
                    "event": "reconnecting",
                    "data": {
                        "jobId": in_flight_job_id,
                        "reason": "Connector disconnected — waiting for reconnection",
                    },
                })

            if host_id is not None:
                clear_connector_session(user_id, connection_nonce)
                with database.session() as db:
                    deactivate_herald_host_connection(db, host_id=host_id, connection_nonce=connection_nonce)

    return app


app = create_app()
