from __future__ import annotations

import base64
import json
from typing import Any

from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey, Ed25519PublicKey
from cryptography.hazmat.primitives.serialization import Encoding, PrivateFormat, PublicFormat, NoEncryption
from sqlalchemy import select
from sqlalchemy.orm import Session

from .config import Settings
from .models import RelayIdentity


ALGORITHM = "ed25519"


def _b64url_encode(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")


def _b64url_decode(encoded: str) -> bytes:
    padding = "=" * (-len(encoded) % 4)
    return base64.urlsafe_b64decode((encoded + padding).encode("ascii"))


def canonical_payload_bytes(payload: dict[str, Any]) -> bytes:
    return json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")


def ensure_relay_identity(db: Session, *, settings: Settings) -> RelayIdentity:
    identity = db.scalar(select(RelayIdentity).limit(1))
    if identity is not None:
        return identity

    private_key = Ed25519PrivateKey.generate()
    public_key = private_key.public_key()
    private_key_raw = private_key.private_bytes(
        encoding=Encoding.Raw,
        format=PrivateFormat.Raw,
        encryption_algorithm=NoEncryption(),
    )
    public_key_raw = public_key.public_bytes(
        encoding=Encoding.Raw,
        format=PublicFormat.Raw,
    )
    identity = RelayIdentity(
        algorithm=ALGORITHM,
        public_key=_b64url_encode(public_key_raw),
        private_key=_b64url_encode(private_key_raw),
    )
    db.add(identity)
    db.commit()
    db.refresh(identity)
    return identity


def serialize_relay_identity(identity: RelayIdentity, *, settings: Settings) -> dict:
    return {
        "id": identity.id,
        "algorithm": identity.algorithm,
        "publicKey": identity.public_key,
        "relayBaseURL": settings.public_base_url,
        "createdAt": identity.created_at,
        "updatedAt": identity.updated_at,
    }


def sign_relay_payload(identity: RelayIdentity, payload: dict[str, Any]) -> str:
    if identity.algorithm != ALGORITHM:
        raise ValueError(f"Unsupported relay identity algorithm: {identity.algorithm}")
    private_key = Ed25519PrivateKey.from_private_bytes(_b64url_decode(identity.private_key))
    signature = private_key.sign(canonical_payload_bytes(payload))
    return _b64url_encode(signature)


def verify_relay_signature(*, public_key: str, payload: dict[str, Any], signature: str) -> bool:
    try:
        key = Ed25519PublicKey.from_public_bytes(_b64url_decode(public_key))
        key.verify(_b64url_decode(signature), canonical_payload_bytes(payload))
        return True
    except (InvalidSignature, ValueError):
        return False
