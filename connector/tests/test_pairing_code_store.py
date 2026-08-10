from __future__ import annotations

import hashlib

from kallisti_connector.pairing_code_store import PairingCodeStore


def test_code_survives_store_reopen_and_stays_bound_to_installation(tmp_path):
    store = PairingCodeStore(tmp_path / "pairing_codes.json")
    code = "ABCD2345"
    digest = hashlib.sha256(code.encode()).hexdigest()
    store.create(digest, expires_at=2_000_000_000.0, created_at=1_000.0)

    reopened = PairingCodeStore(tmp_path / "pairing_codes.json")
    record = reopened.get_live(digest, now=1_001.0)
    assert record is not None
    assert record["redeemed_at"] is None

    payload = {"auth": {"accessToken": "per-device-token"}}
    assert reopened.redeem(digest, installation_id="install-a", payload=payload, now=1_002.0)

    last_reopen = PairingCodeStore(tmp_path / "pairing_codes.json")
    assert last_reopen.redeem(digest, installation_id="install-a", payload=payload, now=1_003.0)
    assert not last_reopen.redeem(digest, installation_id="install-b", payload=payload, now=1_003.0)
    assert last_reopen.get_live(digest, now=1_003.0)["response_payload"] == payload


def test_expired_code_is_removed(tmp_path):
    store = PairingCodeStore(tmp_path / "pairing_codes.json")
    digest = hashlib.sha256(b"ABCD2345").hexdigest()
    store.create(digest, expires_at=99.0, created_at=1.0)
    assert store.get_live(digest, now=100.0) is None
    assert not (tmp_path / "pairing_codes.json").exists() or digest not in (tmp_path / "pairing_codes.json").read_text()
