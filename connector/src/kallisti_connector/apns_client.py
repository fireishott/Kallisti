"""APNs (Apple Push Notification service) client for sending push notifications.

Uses HTTP/2 via httpx with JWT bearer token authentication.
Requires a .p8 key file from Apple Developer Portal.

Environment variables:
    APNS_KEY_PATH         — path to the .p8 private key file
    APNS_KEY_ID           — 10-char key identifier from Apple
    APNS_TEAM_ID          — 10-char team identifier from Apple
    APNS_BUNDLE_ID        — default app bundle identifier (net.fihonline.herald)
    APNS_ENVIRONMENT      — default environment: "development" or "production"
"""

from __future__ import annotations

from enum import Enum
import logging
import os
import time
from pathlib import Path

import httpx

logger = logging.getLogger("herald.apns")

APNS_DEVELOPMENT_URL = "https://api.development.push.apple.com"
APNS_PRODUCTION_URL = "https://api.push.apple.com"


class PushResult(Enum):
    SENT = "sent"
    TOKEN_INVALID = "token_invalid"
    REJECTED = "rejected"
    TRANSIENT = "transient"


class APNsClient:
    def __init__(self):
        self.key_id = os.getenv("APNS_KEY_ID", "")
        self.team_id = os.getenv("APNS_TEAM_ID", "")
        self.default_bundle_id = os.getenv("APNS_BUNDLE_ID", "net.fihonline.herald")
        self.default_environment = os.getenv("APNS_ENVIRONMENT", "production")
        
        key_path = os.getenv("APNS_KEY_PATH", "")
        if not key_path or not Path(key_path).exists():
            raise FileNotFoundError(f"APNs key file not found: {key_path}")
        self._private_key = Path(key_path).read_text().strip()
        
        self._token: str | None = None
        self._token_issued_at: float = 0
        self._client: httpx.AsyncClient | None = None

    async def _get_client(self) -> httpx.AsyncClient:
        if self._client is None or self._client.is_closed:
            self._client = httpx.AsyncClient(http2=True, timeout=30.0)
        return self._client

    def _build_jwt(self) -> str:
        import jwt
        now = int(time.time())
        return jwt.encode(
            {"iss": self.team_id, "iat": now},
            self._private_key,
            algorithm="ES256",
            headers={"alg": "ES256", "kid": self.key_id},
        )

    def _get_token(self) -> str:
        now = time.time()
        if self._token is None or (now - self._token_issued_at) > 3000:
            self._token = self._build_jwt()
            self._token_issued_at = now
        return self._token

    def _base_url_for(self, environment: str | None) -> str:
        env = environment or self.default_environment
        return APNS_PRODUCTION_URL if env == "production" else APNS_DEVELOPMENT_URL

    async def send_alert_push(
        self,
        device_token: str,
        *,
        title: str,
        body: str,
        category: str | None = None,
        bundle_id: str | None = None,
        environment: str | None = None,
        user_info: dict | None = None,
    ) -> PushResult:
        topic = bundle_id or self.default_bundle_id
        base_url = self._base_url_for(environment)
        url = f"{base_url}/3/device/{device_token}"

        headers = {
            "authorization": f"bearer {self._get_token()}",
            "apns-topic": topic,
            "apns-push-type": "alert",
            "apns-priority": "10",
        }
        aps: dict = {"alert": {"title": title, "body": body}, "sound": "default"}
        # Build 83: mutable-content runs the UNNotificationServiceExtension so
        # it can attach a downloaded media thumbnail. Without it the extension
        # never executes and mediaUrl in user_info is dead weight.
        aps["mutable-content"] = 1
        if category:
            aps["category"] = category
        payload = {"aps": aps}
        if user_info:
            payload.update(user_info)

        return await self._send(url, headers, payload, device_token)

    async def send_live_activity_update(
        self,
        activity_push_token: str,
        *,
        content_state: dict,
        event: str = "update",
        bundle_id: str | None = None,
        environment: str | None = None,
        dismissal_seconds_from_now: float | None = None,
    ) -> PushResult:
        """Remote-update or end a Live Activity — the ActivityKit push type.

        content_state must match KallistiActivityAttributes.ContentState's
        Codable shape (KallistiWidgets/KallistiActivityAttributes.swift):
        status/elapsedSeconds/sessionType are required, everything else is
        decodeIfPresent and safe to omit. event is "update" while a turn is
        still running, or "end" on terminal state — mirrors what
        LiveActivityService.endActivity() does locally, except this reaches
        the lock screen even when the app process cannot run any code at
        all, which endActivity() can never do.
        """
        topic = f"{bundle_id or self.default_bundle_id}.push-type.liveactivity"
        base_url = self._base_url_for(environment)
        url = f"{base_url}/3/device/{activity_push_token}"

        headers = {
            "authorization": f"bearer {self._get_token()}",
            "apns-topic": topic,
            "apns-push-type": "liveactivity",
            "apns-priority": "10",
        }
        aps: dict = {
            "timestamp": int(time.time()),
            "event": event,
            "content-state": content_state,
        }
        if event == "end":
            aps["dismissal-date"] = int(time.time() + (dismissal_seconds_from_now or 0))
        payload = {"aps": aps}

        return await self._send(url, headers, payload, activity_push_token)

    async def _send(self, url: str, headers: dict, payload: dict, device_token: str) -> PushResult:
        try:
            client = await self._get_client()
            response = await client.post(url, headers=headers, json=payload)

            if response.status_code == 200:
                logger.info("APNs push sent to %s...", device_token[:8])
                return PushResult.SENT
            if response.status_code == 410:
                logger.info("APNs token %s... permanently invalid (410)", device_token[:8])
                return PushResult.TOKEN_INVALID
            if response.status_code >= 500:
                logger.warning("APNs server error %d for %s...", response.status_code, device_token[:8])
                return PushResult.TRANSIENT
            logger.warning("APNs rejected %d for %s...: %s", response.status_code, device_token[:8], response.text)
            return PushResult.REJECTED
        except (httpx.TimeoutException, httpx.ConnectError, OSError) as e:
            logger.error("APNs transient error for %s...: %s", device_token[:8], e)
            return PushResult.TRANSIENT
        except Exception as e:
            logger.error("APNs unexpected error for %s...: %s", device_token[:8], e)
            return PushResult.REJECTED

    async def close(self):
        if self._client and not self._client.is_closed:
            await self._client.aclose()
