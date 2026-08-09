from __future__ import annotations

import json
import secrets
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone

from sqlalchemy import select, update
from sqlalchemy.orm import Session

from sqlalchemy.exc import IntegrityError

from .config import Settings
from .models import (
    AppAttestCredential,
    PushBrokerChallenge,
    PushBrokerRegistration,
    PushBrokerSendNonce,
    RelayIdentity,
    utcnow,
)
from .relay_identity import verify_relay_signature
from .security import generate_token, hash_token, normalize_datetime


# A signed send request whose `iat` is more than this many seconds off from
# the broker's clock is rejected as stale/future-dated, regardless of whether
# the nonce has been seen before.
PUSH_BROKER_SEND_IAT_SKEW_SECONDS = 120


class PushBrokerChallengeError(ValueError):
    pass


class PushBrokerSendError(ValueError):
    pass


@dataclass(frozen=True)
class AppAttestVerificationResult:
    key_id: str
    public_key: str
    receipt: str | None
    sign_count: int
    environment: str


@dataclass(frozen=True)
class PushBrokerRegistrationResult:
    registration: PushBrokerRegistration
    relay_handle: str
    send_grant: str
    expires_at: datetime

    def gateway_payload(self) -> dict:
        return {
            "transport": "relay",
            "relayHandle": self.relay_handle,
            "sendGrant": self.send_grant,
            "relayId": self.registration.relay_id,
            "relayPublicKey": self.registration.relay_public_key,
            "installationId": self.registration.installation_id,
            "topic": self.registration.bundle_id,
            "environment": self.registration.apns_environment,
            "tokenDebugSuffix": self.registration.token_debug_suffix,
        }


def create_push_broker_challenge(db: Session, *, settings: Settings) -> PushBrokerChallenge:
    challenge = PushBrokerChallenge(
        challenge=secrets.token_urlsafe(32),
        expires_at=utcnow() + timedelta(seconds=settings.push_broker_challenge_ttl_seconds),
    )
    db.add(challenge)
    db.commit()
    db.refresh(challenge)
    return challenge


def serialize_push_broker_challenge(challenge: PushBrokerChallenge) -> dict:
    return {
        "challengeId": challenge.id,
        "challenge": challenge.challenge,
        "expiresAt": challenge.expires_at,
    }


def consume_push_broker_challenge(
    db: Session,
    *,
    challenge_id: str,
    challenge: str,
) -> PushBrokerChallenge:
    """Atomically claim a push-broker challenge.

    Flushes — does **not** commit. The caller owns the surrounding
    transaction so the challenge claim is rolled back if any later step in
    the register flow fails, preventing stuck "already used" state when
    registration cannot be persisted.
    """
    stored = db.scalar(select(PushBrokerChallenge).where(PushBrokerChallenge.id == challenge_id))
    if stored is None or stored.challenge != challenge:
        raise PushBrokerChallengeError("Push broker challenge is invalid.")
    if stored.used_at is not None:
        raise PushBrokerChallengeError("Push broker challenge was already used.")
    if normalize_datetime(stored.expires_at) <= utcnow():
        raise PushBrokerChallengeError("Push broker challenge expired.")

    # Atomic claim: the WHERE filters used_at IS NULL, so concurrent callers
    # race safely — only one UPDATE returns rowcount 1.
    now = utcnow()
    result = db.execute(
        update(PushBrokerChallenge)
        .where(
            PushBrokerChallenge.id == challenge_id,
            PushBrokerChallenge.used_at.is_(None),
        )
        .values(used_at=now)
    )
    if result.rowcount == 0:
        raise PushBrokerChallengeError("Push broker challenge was already used.")

    db.flush()
    db.refresh(stored)
    return stored


def canonical_push_broker_signed_payload(
    *,
    challenge_id: str,
    installation_id: str,
    bundle_id: str,
    app_version: str | None,
    apns_environment: str,
    apns_token: str,
    relay_id: str,
    relay_public_key: str,
    relay_base_url: str | None,
) -> bytes:
    """Canonical byte sequence hashed into the App Attest assertion.

    The server reconstructs these bytes from the register request fields and
    passes them as `client_data` to App Attest. The client must build the
    exact same byte sequence before signing. Using sorted keys + compact JSON
    separators + UTF-8 gives us a deterministic, cross-language canonical form
    for ASCII-only values (which is all our fields are).

    Nullable fields (``app_version``, ``relay_base_url``) are normalized to an
    empty string so Swift's default `Encodable` nil-omission doesn't drift
    from Python's default dict serialization.
    """
    payload = {
        "challengeId": challenge_id,
        "installationId": installation_id,
        "bundleId": bundle_id,
        "appVersion": app_version or "",
        "apnsEnvironment": apns_environment,
        "apnsToken": apns_token,
        "relayIdentity": {
            "id": relay_id,
            "publicKey": relay_public_key,
            "relayBaseURL": relay_base_url or "",
        },
    }
    return json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def _token_debug_suffix(token: str) -> str | None:
    normalized = token.strip().lower()
    if not normalized:
        return None
    return normalized[-8:]


def create_push_broker_registration(
    db: Session,
    *,
    settings: Settings,
    challenge_id: str,
    challenge: str,
    relay_id: str,
    relay_public_key: str,
    installation_id: str,
    bundle_id: str,
    app_version: str | None,
    apns_environment: str,
    apns_token: str,
    app_attest: AppAttestVerificationResult,
) -> PushBrokerRegistrationResult:
    # Single atomic transaction: challenge claim, credential insert, and
    # registration insert all commit together — if any step fails, the
    # challenge claim rolls back so the client can retry with the same
    # challenge rather than being stuck on "already used."
    try:
        consume_push_broker_challenge(db, challenge_id=challenge_id, challenge=challenge)

        credential = AppAttestCredential(
            installation_id=installation_id,
            bundle_id=bundle_id,
            app_version=app_version,
            environment=app_attest.environment,
            key_id=app_attest.key_id,
            public_key=app_attest.public_key,
            receipt=app_attest.receipt,
            sign_count=app_attest.sign_count,
        )
        db.add(credential)
        db.flush()

        send_grant = generate_token()
        relay_handle = generate_token()
        expires_at = utcnow() + timedelta(seconds=settings.push_broker_grant_ttl_seconds)
        registration = PushBrokerRegistration(
            relay_id=relay_id,
            relay_public_key=relay_public_key,
            app_attest_credential_id=credential.id,
            installation_id=installation_id,
            bundle_id=bundle_id,
            app_version=app_version,
            apns_environment=apns_environment,
            apns_token=apns_token,
            apns_token_hash=hash_token(apns_token),
            token_debug_suffix=_token_debug_suffix(apns_token),
            relay_handle=relay_handle,
            send_grant_hash=hash_token(send_grant),
            expires_at=expires_at,
        )
        db.add(registration)
        db.commit()
    except Exception:
        db.rollback()
        raise
    db.refresh(credential)
    db.refresh(registration)
    return PushBrokerRegistrationResult(
        registration=registration,
        relay_handle=relay_handle,
        send_grant=send_grant,
        expires_at=expires_at,
    )


def verify_push_broker_send_request(
    db: Session,
    *,
    relay_handle: str,
    send_grant: str,
    relay_id: str,
    relay_public_key: str,
    payload: dict,
    signature: str,
    nonce: str,
    iat: int,
    now: datetime | None = None,
) -> PushBrokerRegistration:
    registration = db.scalar(
        select(PushBrokerRegistration).where(PushBrokerRegistration.relay_handle == relay_handle)
    )
    if registration is None:
        raise PushBrokerSendError("Push broker relay handle is invalid.")
    if registration.revoked_at is not None:
        raise PushBrokerSendError("Push broker relay registration is revoked.")
    if normalize_datetime(registration.expires_at) <= utcnow():
        raise PushBrokerSendError("Push broker relay registration expired.")
    if registration.send_grant_hash != hash_token(send_grant):
        raise PushBrokerSendError("Push broker send grant is invalid.")
    if registration.relay_id != relay_id or registration.relay_public_key != relay_public_key:
        raise PushBrokerSendError("Push broker relay identity does not match registration.")
    if not verify_relay_signature(public_key=relay_public_key, payload=payload, signature=signature):
        raise PushBrokerSendError("Push broker relay signature is invalid.")

    current = now or utcnow()
    iat_dt = datetime.fromtimestamp(iat, tz=timezone.utc)
    if abs((current - iat_dt).total_seconds()) > PUSH_BROKER_SEND_IAT_SKEW_SECONDS:
        raise PushBrokerSendError("Push broker send request is outside the allowed time window.")

    # Atomically claim the (relay_handle, nonce) pair. A duplicate raises
    # IntegrityError under the unique constraint — signal a replay attempt.
    nonce_record = PushBrokerSendNonce(
        relay_handle=relay_handle,
        nonce=nonce,
        iat=iat_dt,
    )
    db.add(nonce_record)
    try:
        db.flush()
    except IntegrityError as error:
        db.rollback()
        raise PushBrokerSendError("Push broker send request was already processed.") from error

    # Opportunistic prune — keep the nonce table bounded to the skew window.
    cutoff = current - timedelta(seconds=PUSH_BROKER_SEND_IAT_SKEW_SECONDS * 2)
    db.query(PushBrokerSendNonce).filter(PushBrokerSendNonce.iat < cutoff).delete(synchronize_session=False)
    db.commit()
    return registration
