from __future__ import annotations

import hashlib
import json
from datetime import datetime, timedelta, timezone

from fastapi.testclient import TestClient

from app.config import Settings
from app.main import create_app
from cryptography import x509
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec
from cryptography.x509.oid import NameOID, ObjectIdentifier

from app.app_attest import (
    AppAttestVerificationError,
    _parse_app_attest_nonce_extension,
    verify_app_attest_assertion,
    verify_app_attest_attestation,
)
from app.push_broker import canonical_push_broker_signed_payload
from app.relay_identity import _b64url_encode


TEAM_ID = "TEAMID1234"
BUNDLE_ID = "io.hermesmobile.HermesMobile"
APP_ID = f"{TEAM_ID}.{BUNDLE_ID}"
NONCE_OID = ObjectIdentifier("1.2.840.113635.100.8.2")


def cbor_dumps(value):
    if isinstance(value, dict):
        output = _cbor_type_len(5, len(value))
        for key, item in value.items():
            output += cbor_dumps(key)
            output += cbor_dumps(item)
        return output
    if isinstance(value, list):
        output = _cbor_type_len(4, len(value))
        for item in value:
            output += cbor_dumps(item)
        return output
    if isinstance(value, str):
        raw = value.encode("utf-8")
        return _cbor_type_len(3, len(raw)) + raw
    if isinstance(value, bytes):
        return _cbor_type_len(2, len(value)) + value
    if isinstance(value, int):
        return _cbor_type_len(0, value)
    raise TypeError(f"Unsupported CBOR value: {type(value)!r}")


def _cbor_type_len(major: int, length: int) -> bytes:
    prefix = major << 5
    if length < 24:
        return bytes([prefix | length])
    if length <= 0xFF:
        return bytes([prefix | 24, length])
    if length <= 0xFFFF:
        return bytes([prefix | 25]) + length.to_bytes(2, "big")
    if length <= 0xFFFFFFFF:
        return bytes([prefix | 26]) + length.to_bytes(4, "big")
    return bytes([prefix | 27]) + length.to_bytes(8, "big")


def make_authenticator_data(*, credential_key, counter: int, environment: str, include_credential: bool) -> tuple[bytes, str]:
    public_point = credential_key.public_key().public_bytes(
        serialization.Encoding.X962,
        serialization.PublicFormat.UncompressedPoint,
    )
    credential_id = hashlib.sha256(public_point).digest()
    key_id = _b64url_encode(credential_id)
    rp_id_hash = hashlib.sha256(APP_ID.encode("utf-8")).digest()
    flags = b"\x41" if include_credential else b"\x01"
    sign_count = counter.to_bytes(4, "big")
    if not include_credential:
        return rp_id_hash + flags + sign_count, key_id

    aaguid = b"appattestdevelop" if environment == "development" else b"appattest" + (b"\x00" * 7)
    auth_data = (
        rp_id_hash
        + flags
        + sign_count
        + aaguid
        + len(credential_id).to_bytes(2, "big")
        + credential_id
        + b"synthetic-cose-key"
    )
    return auth_data, key_id


def make_certificate(subject_name: str, issuer_cert, issuer_key, subject_key, *, is_ca: bool, nonce: bytes | None = None):
    now = datetime.now(timezone.utc)
    subject = x509.Name([
        x509.NameAttribute(NameOID.COMMON_NAME, subject_name),
    ])
    issuer = issuer_cert.subject if issuer_cert is not None else subject
    builder = (
        x509.CertificateBuilder()
        .subject_name(subject)
        .issuer_name(issuer)
        .public_key(subject_key.public_key())
        .serial_number(x509.random_serial_number())
        .not_valid_before(now - timedelta(minutes=5))
        .not_valid_after(now + timedelta(days=30))
        .add_extension(x509.BasicConstraints(ca=is_ca, path_length=None), critical=True)
    )
    if nonce is not None:
        # Apple's real App Attest nonce extension is
        # SEQUENCE { [1] EXPLICIT OCTET STRING(32) }, i.e. 30 24 A1 22 04 20 <32>.
        # See developer.apple.com forum threads 663118 and 738923.
        builder = builder.add_extension(
            x509.UnrecognizedExtension(NONCE_OID, b"\x30\x24\xa1\x22\x04\x20" + nonce),
            critical=False,
        )
    return builder.sign(private_key=issuer_key, algorithm=hashes.SHA256())


def make_attestation_fixture(*, challenge: str = "server-challenge", environment: str = "development"):
    credential_key = ec.generate_private_key(ec.SECP256R1())
    auth_data, key_id = make_authenticator_data(
        credential_key=credential_key,
        counter=0,
        environment=environment,
        include_credential=True,
    )
    client_data_hash = hashlib.sha256(challenge.encode("utf-8")).digest()
    nonce = hashlib.sha256(auth_data + client_data_hash).digest()

    root_key = ec.generate_private_key(ec.SECP256R1())
    root = make_certificate("Synthetic App Attest Root", None, root_key, root_key, is_ca=True)
    intermediate_key = ec.generate_private_key(ec.SECP256R1())
    intermediate = make_certificate(
        "Synthetic App Attest Intermediate",
        root,
        root_key,
        intermediate_key,
        is_ca=True,
    )
    leaf = make_certificate(
        "Synthetic App Attest Credential",
        intermediate,
        intermediate_key,
        credential_key,
        is_ca=False,
        nonce=nonce,
    )
    attestation = cbor_dumps({
        "fmt": "apple-appattest",
        "attStmt": {
            "x5c": [
                leaf.public_bytes(serialization.Encoding.DER),
                intermediate.public_bytes(serialization.Encoding.DER),
            ],
            "receipt": b"synthetic-receipt",
        },
        "authData": auth_data,
    })
    return {
        "attestation": attestation,
        "key_id": key_id,
        "challenge": challenge,
        "root_der": root.public_bytes(serialization.Encoding.DER),
        "credential_key": credential_key,
    }


def test_parse_app_attest_nonce_extension_accepts_apple_real_format():
    # Real Apple attestations wrap the 32-byte nonce in a [1] EXPLICIT OCTET STRING
    # inside an outer SEQUENCE: 30 24 A1 22 04 20 <32 bytes>.
    nonce = bytes(range(32))
    raw = b"\x30\x24\xa1\x22\x04\x20" + nonce
    assert _parse_app_attest_nonce_extension(raw) == nonce


def test_parse_app_attest_nonce_extension_accepts_bare_octet_string():
    # Tolerant-but-correct fallback: some legacy fixtures emit SEQUENCE { OCTET STRING }
    # without the [1] EXPLICIT wrapper. The parser must still accept it.
    nonce = bytes(range(32))
    raw = b"\x30\x22\x04\x20" + nonce
    assert _parse_app_attest_nonce_extension(raw) == nonce


def test_parse_app_attest_nonce_extension_rejects_unexpected_inner_tag():
    nonce = bytes(range(32))
    # Inner tag 0x05 is neither [1] EXPLICIT (0xA1) nor OCTET STRING (0x04).
    raw = b"\x30\x22\x05\x20" + nonce
    try:
        _parse_app_attest_nonce_extension(raw)
    except AppAttestVerificationError as error:
        assert "inner tag" in str(error) or "octet string" in str(error)
    else:
        raise AssertionError("Expected nonce parser to reject an unknown inner tag.")


def test_parse_app_attest_nonce_extension_rejects_truncated_payload():
    truncated = b"\x30\x24\xa1\x22\x04\x20" + bytes(16)
    try:
        _parse_app_attest_nonce_extension(truncated)
    except AppAttestVerificationError as error:
        assert "truncated" in str(error)
    else:
        raise AssertionError("Expected nonce parser to reject a truncated payload.")


def test_verify_app_attest_attestation_accepts_valid_synthetic_fixture():
    fixture = make_attestation_fixture()

    result = verify_app_attest_attestation(
        attestation_object=fixture["attestation"],
        key_id=fixture["key_id"],
        challenge=fixture["challenge"],
        team_id=TEAM_ID,
        bundle_id=BUNDLE_ID,
        environment="development",
        trusted_root_certificates=[fixture["root_der"]],
    )

    assert result.key_id == fixture["key_id"]
    assert result.receipt == _b64url_encode(b"synthetic-receipt")
    assert result.sign_count == 0
    assert result.environment == "development"
    assert result.public_key


def test_verify_app_attest_attestation_rejects_wrong_challenge():
    fixture = make_attestation_fixture()

    try:
        verify_app_attest_attestation(
            attestation_object=fixture["attestation"],
            key_id=fixture["key_id"],
            challenge="wrong-challenge",
            team_id=TEAM_ID,
            bundle_id=BUNDLE_ID,
            environment="development",
            trusted_root_certificates=[fixture["root_der"]],
        )
    except AppAttestVerificationError as error:
        assert "nonce" in str(error)
    else:
        raise AssertionError("Expected App Attest nonce validation to fail.")


def test_verify_app_attest_assertion_checks_signature_and_counter():
    fixture = make_attestation_fixture()
    attestation_result = verify_app_attest_attestation(
        attestation_object=fixture["attestation"],
        key_id=fixture["key_id"],
        challenge=fixture["challenge"],
        team_id=TEAM_ID,
        bundle_id=BUNDLE_ID,
        environment="development",
        trusted_root_certificates=[fixture["root_der"]],
    )
    auth_data, _ = make_authenticator_data(
        credential_key=fixture["credential_key"],
        counter=2,
        environment="development",
        include_credential=False,
    )
    client_data = b'{"challenge":"assertion-challenge"}'
    nonce = hashlib.sha256(auth_data + hashlib.sha256(client_data).digest()).digest()
    signature = fixture["credential_key"].sign(nonce, ec.ECDSA(hashes.SHA256()))
    assertion = cbor_dumps({
        "authenticatorData": auth_data,
        "signature": signature,
    })

    next_counter = verify_app_attest_assertion(
        assertion_object=assertion,
        public_key=attestation_result.public_key,
        client_data=client_data,
        team_id=TEAM_ID,
        bundle_id=BUNDLE_ID,
        previous_sign_count=1,
    )

    assert next_counter == 2
    try:
        verify_app_attest_assertion(
            assertion_object=assertion,
            public_key=attestation_result.public_key,
            client_data=client_data,
            team_id=TEAM_ID,
            bundle_id=BUNDLE_ID,
            previous_sign_count=2,
        )
    except AppAttestVerificationError as error:
        assert "counter" in str(error)
    else:
        raise AssertionError("Expected assertion counter validation to fail.")


def test_push_broker_register_endpoint_verifies_app_attest_proof(tmp_path):
    settings = Settings(
        environment="test",
        public_base_url="https://relay.example.test/v1",
        database_url=f"sqlite:///{tmp_path / 'app-attest-register.db'}",
        internal_api_key="test-internal-key",
        apns_team_id=TEAM_ID,
    )
    app = create_app(settings)

    with TestClient(app) as client:
        challenge_response = client.post("/v1/push-broker/challenge", json={})
        challenge_data = challenge_response.json()["data"]
        fixture = make_attestation_fixture(challenge=challenge_data["challenge"])
        client.app.state.app_attest_trusted_roots = [fixture["root_der"]]
        relay_identity = client.get("/v1/relay/identity").json()["data"]["identity"]
        signed_payload_bytes = canonical_push_broker_signed_payload(
            challenge_id=challenge_data["challengeId"],
            installation_id="install-123",
            bundle_id=BUNDLE_ID,
            app_version="1.1.0",
            apns_environment="development",
            apns_token="abcd1234efef5678",
            relay_id=relay_identity["id"],
            relay_public_key=relay_identity["publicKey"],
            relay_base_url=relay_identity["relayBaseURL"],
        )
        auth_data, _ = make_authenticator_data(
            credential_key=fixture["credential_key"],
            counter=1,
            environment="development",
            include_credential=False,
        )
        nonce = hashlib.sha256(auth_data + hashlib.sha256(signed_payload_bytes).digest()).digest()
        signature = fixture["credential_key"].sign(nonce, ec.ECDSA(hashes.SHA256()))
        assertion = cbor_dumps({
            "authenticatorData": auth_data,
            "signature": signature,
        })

        response = client.post(
            "/v1/push-broker/register",
            json={
                "challengeId": challenge_data["challengeId"],
                "challenge": challenge_data["challenge"],
                "relayIdentity": {
                    "id": relay_identity["id"],
                    "publicKey": relay_identity["publicKey"],
                    "relayBaseURL": relay_identity["relayBaseURL"],
                },
                "installationId": "install-123",
                "bundleId": BUNDLE_ID,
                "appVersion": "1.1.0",
                "apnsEnvironment": "development",
                "apnsToken": "abcd1234efef5678",
                "appAttest": {
                    "keyId": fixture["key_id"],
                    "attestationObject": _b64url_encode(fixture["attestation"]),
                    "assertion": _b64url_encode(assertion),
                },
            },
        )

        assert response.status_code == 200
        payload = response.json()["data"]
        assert payload["transport"] == "relay"
        assert payload["relayHandle"]
        assert payload["sendGrant"]
        assert payload["relayId"] == relay_identity["id"]
        assert payload["relayPublicKey"] == relay_identity["publicKey"]
        assert payload["installationId"] == "install-123"
        assert payload["topic"] == BUNDLE_ID
        assert payload["environment"] == "development"
        assert payload["tokenDebugSuffix"] == "efef5678"


def test_push_broker_register_endpoint_rejects_tampered_signed_payload(tmp_path):
    settings = Settings(
        environment="test",
        public_base_url="https://relay.example.test/v1",
        database_url=f"sqlite:///{tmp_path / 'app-attest-register-tamper.db'}",
        internal_api_key="test-internal-key",
        apns_team_id=TEAM_ID,
    )
    app = create_app(settings)

    with TestClient(app) as client:
        challenge_data = client.post("/v1/push-broker/challenge", json={}).json()["data"]
        fixture = make_attestation_fixture(challenge=challenge_data["challenge"])
        client.app.state.app_attest_trusted_roots = [fixture["root_der"]]
        relay_identity = client.get("/v1/relay/identity").json()["data"]["identity"]
        # Sign the canonical bytes for the legitimate token.
        legit_signed_payload_bytes = canonical_push_broker_signed_payload(
            challenge_id=challenge_data["challengeId"],
            installation_id="install-123",
            bundle_id=BUNDLE_ID,
            app_version="1.1.0",
            apns_environment="development",
            apns_token="abcd1234efef5678",
            relay_id=relay_identity["id"],
            relay_public_key=relay_identity["publicKey"],
            relay_base_url=relay_identity["relayBaseURL"],
        )
        auth_data, _ = make_authenticator_data(
            credential_key=fixture["credential_key"],
            counter=1,
            environment="development",
            include_credential=False,
        )
        nonce = hashlib.sha256(auth_data + hashlib.sha256(legit_signed_payload_bytes).digest()).digest()
        signature = fixture["credential_key"].sign(nonce, ec.ECDSA(hashes.SHA256()))
        assertion = cbor_dumps({"authenticatorData": auth_data, "signature": signature})

        # Send the request with a tampered apnsToken. The server canonicalizes
        # the signed bytes from request fields, so the bytes it hashes include
        # `"tampered-token"` — which the App Attest assertion was not signed
        # over. Verification fails with a signature error.
        response = client.post(
            "/v1/push-broker/register",
            json={
                "challengeId": challenge_data["challengeId"],
                "challenge": challenge_data["challenge"],
                "relayIdentity": {
                    "id": relay_identity["id"],
                    "publicKey": relay_identity["publicKey"],
                    "relayBaseURL": relay_identity["relayBaseURL"],
                },
                "installationId": "install-123",
                "bundleId": BUNDLE_ID,
                "appVersion": "1.1.0",
                "apnsEnvironment": "development",
                "apnsToken": "tampered-token",
                "appAttest": {
                    "keyId": fixture["key_id"],
                    "attestationObject": _b64url_encode(fixture["attestation"]),
                    "assertion": _b64url_encode(assertion),
                },
            },
        )

        assert response.status_code == 401
        assert "signature" in response.json()["detail"].lower()
