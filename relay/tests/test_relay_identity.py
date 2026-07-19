from __future__ import annotations

from fastapi.testclient import TestClient

from app.config import Settings
from app.database import Database
from app.main import create_app
from app.relay_identity import ensure_relay_identity, sign_relay_payload, verify_relay_signature


def build_client(tmp_path):
    settings = Settings(
        environment="test",
        public_base_url="https://relay.example.test/v1",
        database_url=f"sqlite:///{tmp_path / 'relay-identity.db'}",
        internal_api_key="test-internal-key",
    )
    app = create_app(settings)
    return TestClient(app), settings


def test_relay_identity_endpoint_is_stable_for_database(tmp_path):
    client, _ = build_client(tmp_path)
    with client:
        first = client.get("/v1/relay/identity")
        second = client.get("/v1/relay/identity")

    restarted, _ = build_client(tmp_path)
    with restarted:
        after_restart = restarted.get("/v1/relay/identity")

    assert first.status_code == 200
    assert second.status_code == 200
    assert after_restart.status_code == 200
    first_identity = first.json()["data"]["identity"]
    assert first_identity == second.json()["data"]["identity"]
    assert first_identity == after_restart.json()["data"]["identity"]
    assert first_identity["algorithm"] == "ed25519"
    assert first_identity["publicKey"]
    assert "private" not in first_identity


def test_relay_identity_signatures_verify_with_public_key(tmp_path):
    _, settings = build_client(tmp_path)
    database = Database(settings.database_url)
    database.create_all()

    with database.session() as db:
        identity = ensure_relay_identity(db, settings=settings)
        payload = {
            "relayId": identity.id,
            "grantId": "grant-123",
            "requestId": "request-456",
        }
        signature = sign_relay_payload(identity, payload)

    assert verify_relay_signature(
        public_key=identity.public_key,
        payload=payload,
        signature=signature,
    )
    assert not verify_relay_signature(
        public_key=identity.public_key,
        payload=payload | {"requestId": "request-tampered"},
        signature=signature,
    )
