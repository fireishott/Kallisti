"""Apple App Attest root of trust.

The Apple App Attestation Root CA is bundled inside the relay image at
`app/certs/apple_app_attestation_root_ca.pem`. We pin its DER-form SHA-256
digest so a tampered or silently swapped certificate file (for example via a
Docker layer mount or supply-chain compromise) cannot be accepted as a
trust anchor — boot fails loudly instead.

Fingerprint source: cross-verified via `openssl x509 -fingerprint -sha256`
and `cryptography.x509` on the PEM downloaded from Apple's certificate
authority page, <https://www.apple.com/certificateauthority/>. The cert
itself is self-signed with:

    Subject: CN=Apple App Attestation Root CA, O=Apple Inc., ST=California
    Serial:  0BF3BE0EF1CDD2E0FB8C6E721F621798
    Validity: 2020-03-18 .. 2045-03-15

When Apple rotates the App Attest root, update both the bundled PEM and the
pinned fingerprint in the same commit.
"""

from __future__ import annotations

import hashlib
from pathlib import Path

from cryptography import x509
from cryptography.hazmat.primitives import serialization


APPLE_APP_ATTEST_ROOT_SHA256 = "1cb9823ba28ba6ad2d33a006941de2ae4f513ef1d4e831b9f7e0fa7b6242c932"
BUNDLED_ROOT_PATH = Path(__file__).resolve().parent / "certs" / "apple_app_attestation_root_ca.pem"


class AppAttestTrustAnchorError(RuntimeError):
    pass


def load_bundled_app_attest_roots() -> list[bytes]:
    """Loads the bundled Apple App Attest root CA and returns its DER bytes.

    Raises ``AppAttestTrustAnchorError`` if the bundled cert is missing,
    unparseable, or does not match the pinned SHA-256 fingerprint.
    """
    if not BUNDLED_ROOT_PATH.is_file():
        raise AppAttestTrustAnchorError(
            f"Apple App Attest root CA is missing at {BUNDLED_ROOT_PATH}."
        )
    pem = BUNDLED_ROOT_PATH.read_bytes()
    try:
        certificate = x509.load_pem_x509_certificate(pem)
    except ValueError as error:
        raise AppAttestTrustAnchorError(
            "Apple App Attest root CA could not be parsed."
        ) from error
    der = certificate.public_bytes(serialization.Encoding.DER)
    actual = hashlib.sha256(der).hexdigest()
    if actual != APPLE_APP_ATTEST_ROOT_SHA256:
        raise AppAttestTrustAnchorError(
            "Apple App Attest root CA fingerprint does not match pin "
            f"(expected {APPLE_APP_ATTEST_ROOT_SHA256}, got {actual})."
        )
    return [der]
