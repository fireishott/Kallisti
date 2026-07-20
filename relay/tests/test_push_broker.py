from __future__ import annotations

import secrets
import time

from fastapi.testclient import TestClient
from sqlalchemy import select

from app.config import Settings
from app.main import create_app
from app.models import PushBrokerChallenge, RelayIdentity
from app.push_broker import (
    AppAttestVerificationResult,
    consume_push_broker_challenge,
    create_push_broker_challenge,
    create_push_broker_registration,
    PushBrokerChallengeError,
)
from app.relay_identity import _b64url_encode, ensure_relay_identity, sign_relay_payload
from app.security import hash_token, normalize_datetime, utcnow
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey


def make_fresh_nonce_and_iat() -> tuple[str, int]:
    return secrets.token_urlsafe(24), int(time.time())


def build_client(tmp_path):
    settings = Settings(
        environment="test",
        public_base_url="https://relay.example.test/v1",
        database_url=f"sqlite:///{tmp_path / 'push-broker.db'}",
        internal_api_key="test-internal-key",
    )
    app = create_app(settings)
    return TestClient(app)


def test_push_broker_challenge_endpoint_issues_unique_stored_challenges(tmp_path):
    with build_client(tmp_path) as client:
        first = client.post("/v1/push-broker/challenge", json={})
        second = client.post("/v1/push-broker/challenge", json={})

        assert first.status_code == 200
        assert second.status_code == 200
        first_challenge = first.json()["data"]
        second_challenge = second.json()["data"]
        assert first_challenge["challengeId"] != second_challenge["challengeId"]
        assert first_challenge["challenge"] != second_challenge["challenge"]
        assert first_challenge["expiresAt"]

        with client.app.state.database.session() as db:
            stored = db.scalar(
                select(PushBrokerChallenge).where(PushBrokerChallenge.id == first_challenge["challengeId"])
            )
            assert stored is not None
            assert stored.challenge == first_challenge["challenge"]
            assert stored.used_at is None
            assert normalize_datetime(stored.expires_at) > utcnow()


def test_push_broker_challenge_can_only_be_consumed_once(tmp_path):
    with build_client(tmp_path) as client:
        with client.app.state.database.session() as db:
            challenge = create_push_broker_challenge(db, settings=client.app.state.settings)
            consumed = consume_push_broker_challenge(
                db,
                challenge_id=challenge.id,
                challenge=challenge.challenge,
            )
            assert consumed.used_at is not None

            try:
                consume_push_broker_challenge(
                    db,
                    challenge_id=challenge.id,
                    challenge=challenge.challenge,
                )
            except PushBrokerChallengeError as error:
                assert "already used" in str(error)
            else:
                raise AssertionError("Expected replayed challenge consumption to fail.")


def test_push_broker_challenge_rejects_wrong_or_expired_challenge(tmp_path):
    with build_client(tmp_path) as client:
        with client.app.state.database.session() as db:
            challenge = create_push_broker_challenge(db, settings=client.app.state.settings)
            try:
                consume_push_broker_challenge(
                    db,
                    challenge_id=challenge.id,
                    challenge="wrong-challenge",
                )
            except PushBrokerChallengeError as error:
                assert "invalid" in str(error)
            else:
                raise AssertionError("Expected wrong challenge value to fail.")

            challenge.expires_at = utcnow()
            db.commit()
            try:
                consume_push_broker_challenge(
                    db,
                    challenge_id=challenge.id,
                    challenge=challenge.challenge,
                )
            except PushBrokerChallengeError as error:
                assert "expired" in str(error)
            else:
                raise AssertionError("Expected expired challenge to fail.")


def test_push_broker_registration_consumes_challenge_and_returns_opaque_grant(tmp_path):
    with build_client(tmp_path) as client:
        with client.app.state.database.session() as db:
            settings = client.app.state.settings
            relay_identity = ensure_relay_identity(db, settings=settings)
            challenge = create_push_broker_challenge(db, settings=settings)
            result = create_push_broker_registration(
                db,
                settings=settings,
                challenge_id=challenge.id,
                challenge=challenge.challenge,
                relay_id=relay_identity.id,
                relay_public_key=relay_identity.public_key,
                installation_id="install-123",
                bundle_id="net.fihonline.herald",
                app_version="1.1.0",
                apns_environment="production",
                apns_token="abcd1234efef5678",
                app_attest=AppAttestVerificationResult(
                    key_id="app-attest-key",
                    public_key="app-attest-public-key",
                    receipt="app-attest-receipt",
                    sign_count=1,
                    environment="production",
                ),
            )

            assert result.relay_handle
            assert result.send_grant
            assert result.expires_at > utcnow()

            registration = result.registration
            db.refresh(challenge)
            assert challenge.used_at is not None
            assert registration.relay_id == relay_identity.id
            assert registration.relay_public_key == relay_identity.public_key
            assert registration.installation_id == "install-123"
            assert registration.bundle_id == "net.fihonline.herald"
            assert registration.apns_environment == "production"
            assert registration.apns_token == "abcd1234efef5678"
            assert registration.apns_token_hash == hash_token("abcd1234efef5678")
            assert registration.token_debug_suffix == "efef5678"
            assert registration.send_grant_hash == hash_token(result.send_grant)
            assert registration.send_grant_hash != result.send_grant
            assert result.gateway_payload() == {
                "transport": "relay",
                "relayHandle": result.relay_handle,
                "sendGrant": result.send_grant,
                "relayId": relay_identity.id,
                "relayPublicKey": relay_identity.public_key,
                "installationId": "install-123",
                "topic": "net.fihonline.herald",
                "environment": "production",
                "tokenDebugSuffix": "efef5678",
            }


def test_push_broker_registration_rejects_replayed_challenge(tmp_path):
    with build_client(tmp_path) as client:
        with client.app.state.database.session() as db:
            settings = client.app.state.settings
            relay_identity = ensure_relay_identity(db, settings=settings)
            challenge = create_push_broker_challenge(db, settings=settings)
            app_attest = AppAttestVerificationResult(
                key_id="app-attest-key",
                public_key="app-attest-public-key",
                receipt="app-attest-receipt",
                sign_count=1,
                environment="production",
            )
            create_push_broker_registration(
                db,
                settings=settings,
                challenge_id=challenge.id,
                challenge=challenge.challenge,
                relay_id=relay_identity.id,
                relay_public_key=relay_identity.public_key,
                installation_id="install-123",
                bundle_id="net.fihonline.herald",
                app_version="1.1.0",
                apns_environment="production",
                apns_token="abcd1234efef5678",
                app_attest=app_attest,
            )

            try:
                create_push_broker_registration(
                    db,
                    settings=settings,
                    challenge_id=challenge.id,
                    challenge=challenge.challenge,
                    relay_id=relay_identity.id,
                    relay_public_key=relay_identity.public_key,
                    installation_id="install-123",
                    bundle_id="net.fihonline.herald",
                    app_version="1.1.0",
                    apns_environment="production",
                    apns_token="abcd1234efef5678",
                    app_attest=app_attest,
                )
            except PushBrokerChallengeError as error:
                assert "already used" in str(error)
            else:
                raise AssertionError("Expected push broker registration replay to fail.")


def test_push_broker_registration_can_bind_external_self_hosted_relay_identity(tmp_path):
    with build_client(tmp_path) as client:
        with client.app.state.database.session() as db:
            settings = client.app.state.settings
            challenge = create_push_broker_challenge(db, settings=settings)
            result = create_push_broker_registration(
                db,
                settings=settings,
                challenge_id=challenge.id,
                challenge=challenge.challenge,
                relay_id="self-hosted-relay-123",
                relay_public_key="self-hosted-relay-public-key",
                installation_id="install-123",
                bundle_id="net.fihonline.herald",
                app_version="1.1.0",
                apns_environment="production",
                apns_token="abcd1234efef5678",
                app_attest=AppAttestVerificationResult(
                    key_id="app-attest-key",
                    public_key="app-attest-public-key",
                    receipt="app-attest-receipt",
                    sign_count=1,
                    environment="production",
                ),
            )

            assert result.gateway_payload()["relayId"] == "self-hosted-relay-123"
            assert result.gateway_payload()["relayPublicKey"] == "self-hosted-relay-public-key"


def test_push_broker_send_endpoint_accepts_valid_signed_grant(tmp_path):
    class StubAPNsClient:
        def __init__(self) -> None:
            self.alerts = []

        async def send_alert_push(self, token: str, *, title: str, body: str, category: str | None = None, bundle_id: str | None = None, environment: str | None = None):
            self.alerts.append({
                "token": token,
                "title": title,
                "body": body,
                "category": category,
                "bundle_id": bundle_id,
                "environment": environment,
            })
            return "sent"

    with build_client(tmp_path) as client:
        client.app.state.apns_client = StubAPNsClient()
        with client.app.state.database.session() as db:
            settings = client.app.state.settings
            challenge = create_push_broker_challenge(db, settings=settings)
            private_key = Ed25519PrivateKey.generate()
            relay_identity = RelayIdentity(
                id="self-hosted-relay-123",
                algorithm="ed25519",
                public_key=_b64url_encode(private_key.public_key().public_bytes_raw()),
                private_key=_b64url_encode(private_key.private_bytes_raw()),
            )
            registration = create_push_broker_registration(
                db,
                settings=settings,
                challenge_id=challenge.id,
                challenge=challenge.challenge,
                relay_id=relay_identity.id,
                relay_public_key=relay_identity.public_key,
                installation_id="install-123",
                bundle_id="net.fihonline.herald",
                app_version="1.1.0",
                apns_environment="production",
                apns_token="abcd1234efef5678",
                app_attest=AppAttestVerificationResult(
                    key_id="app-attest-key",
                    public_key="app-attest-public-key",
                    receipt="app-attest-receipt",
                    sign_count=1,
                    environment="production",
                ),
            )

        nonce, iat = make_fresh_nonce_and_iat()
        signed_payload = {
            "relayHandle": registration.relay_handle,
            "sendGrant": registration.send_grant,
            "relayId": relay_identity.id,
            "relayPublicKey": relay_identity.public_key,
            "pushType": "alert",
            "title": "Herald",
            "body": "Ready.",
            "conversationId": "conv-123",
            "messageId": "msg-456",
            "jobId": "job-789",
            "category": "HERALD_MESSAGE_READY",
            "nonce": nonce,
            "iat": iat,
        }
        signature = sign_relay_payload(relay_identity, signed_payload)
        response = client.post(
            "/v1/push-broker/send",
            json=signed_payload | {"signature": signature},
        )

        assert response.status_code == 200
        assert response.json()["data"]["sent"] is True
        assert client.app.state.apns_client.alerts == [{
            "token": "abcd1234efef5678",
            "title": "Herald",
            "body": "Ready.",
            "category": "HERALD_MESSAGE_READY",
            "bundle_id": "net.fihonline.herald",
            "environment": "production",
            "user_info": {
                "conversationId": "conv-123",
                "messageId": "msg-456",
                "jobId": "job-789",
            },
        }]


def test_push_broker_send_endpoint_rejects_invalid_signature(tmp_path):
    with build_client(tmp_path) as client:
        with client.app.state.database.session() as db:
            settings = client.app.state.settings
            challenge = create_push_broker_challenge(db, settings=settings)
            private_key = Ed25519PrivateKey.generate()
            relay_identity = RelayIdentity(
                id="self-hosted-relay-123",
                algorithm="ed25519",
                public_key=_b64url_encode(private_key.public_key().public_bytes_raw()),
                private_key=_b64url_encode(private_key.private_bytes_raw()),
            )
            registration = create_push_broker_registration(
                db,
                settings=settings,
                challenge_id=challenge.id,
                challenge=challenge.challenge,
                relay_id=relay_identity.id,
                relay_public_key=relay_identity.public_key,
                installation_id="install-123",
                bundle_id="net.fihonline.herald",
                app_version="1.1.0",
                apns_environment="production",
                apns_token="abcd1234efef5678",
                app_attest=AppAttestVerificationResult(
                    key_id="app-attest-key",
                    public_key="app-attest-public-key",
                    receipt="app-attest-receipt",
                    sign_count=1,
                    environment="production",
                ),
            )

        nonce, iat = make_fresh_nonce_and_iat()
        response = client.post(
            "/v1/push-broker/send",
            json={
                "relayHandle": registration.relay_handle,
                "sendGrant": registration.send_grant,
                "relayId": relay_identity.id,
                "relayPublicKey": relay_identity.public_key,
                "pushType": "alert",
                "title": "Herald",
                "body": "Ready.",
                "nonce": nonce,
                "iat": iat,
                "signature": "invalid-signature",
            },
        )

        assert response.status_code == 401


def test_push_broker_send_endpoint_rejects_expired_registration(tmp_path):
    with build_client(tmp_path) as client:
        with client.app.state.database.session() as db:
            settings = client.app.state.settings
            challenge = create_push_broker_challenge(db, settings=settings)
            private_key = Ed25519PrivateKey.generate()
            relay_identity = RelayIdentity(
                id="self-hosted-relay-123",
                algorithm="ed25519",
                public_key=_b64url_encode(private_key.public_key().public_bytes_raw()),
                private_key=_b64url_encode(private_key.private_bytes_raw()),
            )
            registration = create_push_broker_registration(
                db,
                settings=settings,
                challenge_id=challenge.id,
                challenge=challenge.challenge,
                relay_id=relay_identity.id,
                relay_public_key=relay_identity.public_key,
                installation_id="install-123",
                bundle_id="net.fihonline.herald",
                app_version="1.1.0",
                apns_environment="production",
                apns_token="abcd1234efef5678",
                app_attest=AppAttestVerificationResult(
                    key_id="app-attest-key",
                    public_key="app-attest-public-key",
                    receipt="app-attest-receipt",
                    sign_count=1,
                    environment="production",
                ),
            )
            registration.registration.expires_at = utcnow()
            db.commit()

        nonce, iat = make_fresh_nonce_and_iat()
        signed_payload = {
            "relayHandle": registration.relay_handle,
            "sendGrant": registration.send_grant,
            "relayId": relay_identity.id,
            "relayPublicKey": relay_identity.public_key,
            "pushType": "alert",
            "title": "Herald",
            "body": "Ready.",
            "nonce": nonce,
            "iat": iat,
        }
        signature = sign_relay_payload(relay_identity, signed_payload)
        response = client.post(
            "/v1/push-broker/send",
            json=signed_payload | {"signature": signature},
        )

        assert response.status_code == 409
