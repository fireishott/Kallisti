from __future__ import annotations

from dataclasses import dataclass, replace
import hashlib
from typing import Any

from cryptography import x509
from cryptography.exceptions import InvalidSignature
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import ec, ed25519, rsa, padding
from cryptography.x509.oid import ObjectIdentifier

from .push_broker import AppAttestVerificationResult
from .relay_identity import _b64url_decode, _b64url_encode
from .security import utcnow


APP_ATTEST_NONCE_OID = ObjectIdentifier("1.2.840.113635.100.8.2")
DEVELOPMENT_AAGUID = b"appattestdevelop"
PRODUCTION_AAGUID = b"appattest" + (b"\x00" * 7)


class AppAttestVerificationError(ValueError):
    pass


@dataclass(frozen=True)
class AuthenticatorData:
    rp_id_hash: bytes
    flags: int
    sign_count: int
    aaguid: bytes | None = None
    credential_id: bytes | None = None
    credential_public_key: bytes | None = None


def verify_app_attest_attestation(
    *,
    attestation_object: bytes,
    key_id: str,
    challenge: str,
    team_id: str,
    bundle_id: str,
    environment: str,
    trusted_root_certificates: list[bytes],
) -> AppAttestVerificationResult:
    decoded = _decode_cbor(attestation_object)
    if not isinstance(decoded, dict):
        raise AppAttestVerificationError("App Attest attestation object must be a CBOR map.")
    if decoded.get("fmt") != "apple-appattest":
        raise AppAttestVerificationError("App Attest attestation format is invalid.")

    att_stmt = decoded.get("attStmt")
    auth_data_raw = decoded.get("authData")
    if not isinstance(att_stmt, dict) or not isinstance(auth_data_raw, bytes):
        raise AppAttestVerificationError("App Attest attestation is missing attStmt or authData.")

    x5c = att_stmt.get("x5c")
    receipt = att_stmt.get("receipt")
    if not isinstance(x5c, list) or len(x5c) < 2 or not all(isinstance(item, bytes) for item in x5c):
        raise AppAttestVerificationError("App Attest attestation is missing a certificate chain.")
    if receipt is not None and not isinstance(receipt, bytes):
        raise AppAttestVerificationError("App Attest receipt must be bytes.")

    auth_data = parse_authenticator_data(auth_data_raw, require_credential=True)
    _verify_rp_id_hash(auth_data.rp_id_hash, team_id=team_id, bundle_id=bundle_id)
    _verify_counter(auth_data.sign_count, expected=0)
    _verify_aaguid(auth_data.aaguid, environment=environment)

    certificates = [x509.load_der_x509_certificate(raw) for raw in x5c]
    roots = [x509.load_der_x509_certificate(raw) for raw in trusted_root_certificates]
    if not roots:
        raise AppAttestVerificationError("App Attest trusted root certificate set is empty.")
    _verify_certificate_chain(certificates, roots)

    leaf = certificates[0]
    expected_nonce = hashlib.sha256(auth_data_raw + hashlib.sha256(challenge.encode("utf-8")).digest()).digest()
    actual_nonce = _extract_app_attest_nonce(leaf)
    if actual_nonce != expected_nonce:
        raise AppAttestVerificationError("App Attest nonce extension does not match challenge.")

    public_key_bytes = _credential_public_key_bytes(leaf)
    expected_key_id = hashlib.sha256(public_key_bytes).digest()
    if _decode_key_id(key_id) != expected_key_id:
        raise AppAttestVerificationError("App Attest key identifier does not match credential certificate.")
    if auth_data.credential_id != expected_key_id:
        raise AppAttestVerificationError("App Attest credential ID does not match key identifier.")

    return AppAttestVerificationResult(
        key_id=key_id,
        public_key=_b64url_encode(public_key_bytes),
        receipt=_b64url_encode(receipt) if receipt is not None else None,
        sign_count=auth_data.sign_count,
        environment=environment,
    )


def verify_app_attest_assertion(
    *,
    assertion_object: bytes,
    public_key: str,
    client_data: bytes,
    team_id: str,
    bundle_id: str,
    previous_sign_count: int,
) -> int:
    decoded = _decode_cbor(assertion_object)
    if not isinstance(decoded, dict):
        raise AppAttestVerificationError("App Attest assertion object must be a CBOR map.")
    auth_data_raw = decoded.get("authenticatorData")
    signature = decoded.get("signature")
    if not isinstance(auth_data_raw, bytes) or not isinstance(signature, bytes):
        raise AppAttestVerificationError("App Attest assertion is missing authenticatorData or signature.")

    auth_data = parse_authenticator_data(auth_data_raw, require_credential=False)
    _verify_rp_id_hash(auth_data.rp_id_hash, team_id=team_id, bundle_id=bundle_id)
    if auth_data.sign_count <= previous_sign_count:
        raise AppAttestVerificationError("App Attest assertion counter did not advance.")

    key = ec.EllipticCurvePublicKey.from_encoded_point(ec.SECP256R1(), _b64url_decode(public_key))
    nonce = hashlib.sha256(auth_data_raw + hashlib.sha256(client_data).digest()).digest()
    try:
        key.verify(signature, nonce, ec.ECDSA(hashes.SHA256()))
    except InvalidSignature as error:
        raise AppAttestVerificationError("App Attest assertion signature is invalid.") from error
    return auth_data.sign_count


def verify_app_attest_registration_proof(
    *,
    attestation_object: bytes,
    assertion_object: bytes,
    signed_payload: bytes,
    key_id: str,
    challenge: str,
    team_id: str,
    bundle_id: str,
    environment: str,
    trusted_root_certificates: list[bytes],
) -> AppAttestVerificationResult:
    attestation = verify_app_attest_attestation(
        attestation_object=attestation_object,
        key_id=key_id,
        challenge=challenge,
        team_id=team_id,
        bundle_id=bundle_id,
        environment=environment,
        trusted_root_certificates=trusted_root_certificates,
    )
    sign_count = verify_app_attest_assertion(
        assertion_object=assertion_object,
        public_key=attestation.public_key,
        client_data=signed_payload,
        team_id=team_id,
        bundle_id=bundle_id,
        previous_sign_count=attestation.sign_count,
    )
    return replace(attestation, sign_count=sign_count)


def parse_authenticator_data(auth_data: bytes, *, require_credential: bool) -> AuthenticatorData:
    if len(auth_data) < 37:
        raise AppAttestVerificationError("Authenticator data is too short.")
    rp_id_hash = auth_data[:32]
    flags = auth_data[32]
    sign_count = int.from_bytes(auth_data[33:37], "big")
    if not require_credential:
        return AuthenticatorData(rp_id_hash=rp_id_hash, flags=flags, sign_count=sign_count)

    if len(auth_data) < 55:
        raise AppAttestVerificationError("Authenticator data is missing attested credential data.")
    aaguid = auth_data[37:53]
    credential_length = int.from_bytes(auth_data[53:55], "big")
    credential_start = 55
    credential_end = credential_start + credential_length
    if len(auth_data) <= credential_end:
        raise AppAttestVerificationError("Authenticator data credential public key is missing.")
    credential_id = auth_data[credential_start:credential_end]
    credential_public_key = auth_data[credential_end:]
    return AuthenticatorData(
        rp_id_hash=rp_id_hash,
        flags=flags,
        sign_count=sign_count,
        aaguid=aaguid,
        credential_id=credential_id,
        credential_public_key=credential_public_key,
    )


def _verify_rp_id_hash(actual: bytes, *, team_id: str, bundle_id: str) -> None:
    expected = hashlib.sha256(f"{team_id}.{bundle_id}".encode("utf-8")).digest()
    if actual != expected:
        raise AppAttestVerificationError("App Attest RP ID hash does not match app identifier.")


def _verify_counter(actual: int, *, expected: int) -> None:
    if actual != expected:
        raise AppAttestVerificationError("App Attest counter is invalid.")


def _verify_aaguid(actual: bytes | None, *, environment: str) -> None:
    expected = DEVELOPMENT_AAGUID if environment == "development" else PRODUCTION_AAGUID
    if actual != expected:
        raise AppAttestVerificationError("App Attest AAGUID does not match environment.")


def _verify_certificate_chain(certificates: list[x509.Certificate], roots: list[x509.Certificate]) -> None:
    now = utcnow()
    for certificate in certificates:
        if certificate.not_valid_before_utc > now or certificate.not_valid_after_utc < now:
            raise AppAttestVerificationError("App Attest certificate is outside its validity period.")

    for child, issuer in zip(certificates, certificates[1:]):
        _verify_certificate_signature(child, issuer)

    top = certificates[-1]
    for root in roots:
        if root.not_valid_before_utc > now or root.not_valid_after_utc < now:
            continue
        if top.issuer != root.subject:
            continue
        try:
            _verify_certificate_signature(top, root)
            return
        except AppAttestVerificationError:
            continue
    raise AppAttestVerificationError("App Attest certificate chain does not terminate in a trusted root.")


def _verify_certificate_signature(child: x509.Certificate, issuer: x509.Certificate) -> None:
    public_key = issuer.public_key()
    try:
        if isinstance(public_key, ec.EllipticCurvePublicKey):
            public_key.verify(child.signature, child.tbs_certificate_bytes, ec.ECDSA(child.signature_hash_algorithm))
        elif isinstance(public_key, rsa.RSAPublicKey):
            public_key.verify(child.signature, child.tbs_certificate_bytes, padding.PKCS1v15(), child.signature_hash_algorithm)
        elif isinstance(public_key, ed25519.Ed25519PublicKey):
            public_key.verify(child.signature, child.tbs_certificate_bytes)
        else:
            raise AppAttestVerificationError("Unsupported App Attest certificate key type.")
    except InvalidSignature as error:
        raise AppAttestVerificationError("App Attest certificate signature is invalid.") from error


def _extract_app_attest_nonce(leaf: x509.Certificate) -> bytes:
    try:
        extension = leaf.extensions.get_extension_for_oid(APP_ATTEST_NONCE_OID).value
    except x509.ExtensionNotFound as error:
        raise AppAttestVerificationError("App Attest nonce extension is missing.") from error
    if not isinstance(extension, x509.UnrecognizedExtension):
        raise AppAttestVerificationError("App Attest nonce extension has unexpected type.")
    return _parse_app_attest_nonce_extension(extension.value)


# Apple's App Attest nonce extension (OID 1.2.840.113635.100.8.2) is DER-encoded
# as `SEQUENCE { [1] EXPLICIT OCTET STRING }` — the 32-byte nonce is wrapped in
# a context-specific `[1]` explicit tag (0xA1) inside an outer SEQUENCE. We also
# tolerate the legacy `SEQUENCE { OCTET STRING }` form because earlier fixtures
# used it; real Apple-produced certs always carry the explicit tag.
def _parse_app_attest_nonce_extension(raw: bytes) -> bytes:
    pos = 0
    if pos >= len(raw) or raw[pos] != 0x30:
        raise AppAttestVerificationError("App Attest nonce extension is not a DER sequence.")
    pos += 1
    _, pos = _read_der_length(raw, pos)
    if pos >= len(raw):
        raise AppAttestVerificationError("App Attest nonce extension is truncated.")

    tag = raw[pos]
    if tag == 0xA1:
        pos += 1
        _, pos = _read_der_length(raw, pos)
        if pos >= len(raw) or raw[pos] != 0x04:
            raise AppAttestVerificationError("App Attest nonce extension inner tag is not an octet string.")
        pos += 1
    elif tag == 0x04:
        pos += 1
    else:
        raise AppAttestVerificationError("App Attest nonce extension has unexpected inner tag.")

    length, pos = _read_der_length(raw, pos)
    value = raw[pos:pos + length]
    if len(value) != length:
        raise AppAttestVerificationError("App Attest nonce extension is truncated.")
    return value


def _read_der_length(raw: bytes, pos: int) -> tuple[int, int]:
    if pos >= len(raw):
        raise AppAttestVerificationError("DER length is truncated.")
    first = raw[pos]
    pos += 1
    if first < 0x80:
        return first, pos
    count = first & 0x7F
    if count == 0 or count > 4 or pos + count > len(raw):
        raise AppAttestVerificationError("DER length is invalid.")
    return int.from_bytes(raw[pos:pos + count], "big"), pos + count


def _credential_public_key_bytes(leaf: x509.Certificate) -> bytes:
    public_key = leaf.public_key()
    if not isinstance(public_key, ec.EllipticCurvePublicKey):
        raise AppAttestVerificationError("App Attest credential certificate key must be EC.")
    return public_key.public_bytes(
        serialization.Encoding.X962,
        serialization.PublicFormat.UncompressedPoint,
    )


def _decode_key_id(key_id: str) -> bytes:
    try:
        return _b64url_decode(key_id)
    except ValueError as error:
        raise AppAttestVerificationError("App Attest key identifier is not valid base64url.") from error


class _CBORReader:
    def __init__(self, data: bytes) -> None:
        self.data = data
        self.pos = 0

    def read(self) -> Any:
        if self.pos >= len(self.data):
            raise AppAttestVerificationError("CBOR data is truncated.")
        head = self.data[self.pos]
        self.pos += 1
        major = head >> 5
        additional = head & 0x1F
        value = self._read_argument(additional)

        if major == 0:
            return value
        if major == 2:
            return self._read_bytes(value)
        if major == 3:
            return self._read_bytes(value).decode("utf-8")
        if major == 4:
            return [self.read() for _ in range(value)]
        if major == 5:
            result = {}
            for _ in range(value):
                key = self.read()
                result[key] = self.read()
            return result
        raise AppAttestVerificationError(f"Unsupported CBOR major type: {major}.")

    def _read_argument(self, additional: int) -> int:
        if additional < 24:
            return additional
        if additional == 24:
            return int.from_bytes(self._read_bytes(1), "big")
        if additional == 25:
            return int.from_bytes(self._read_bytes(2), "big")
        if additional == 26:
            return int.from_bytes(self._read_bytes(4), "big")
        if additional == 27:
            return int.from_bytes(self._read_bytes(8), "big")
        raise AppAttestVerificationError("Indefinite CBOR lengths are not supported.")

    def _read_bytes(self, length: int) -> bytes:
        end = self.pos + length
        if end > len(self.data):
            raise AppAttestVerificationError("CBOR byte string is truncated.")
        value = self.data[self.pos:end]
        self.pos = end
        return value


def _decode_cbor(data: bytes) -> Any:
    reader = _CBORReader(data)
    value = reader.read()
    if reader.pos != len(data):
        raise AppAttestVerificationError("CBOR data has trailing bytes.")
    return value
