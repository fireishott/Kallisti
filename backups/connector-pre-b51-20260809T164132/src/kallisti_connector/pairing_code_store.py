"""Durable, one-time Kallisti pairing-code records.

Only SHA-256 code digests are stored. Records survive connector restarts long
enough to honor the 10-minute pairing window and same-installation replay.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
from threading import RLock
from typing import Any


class PairingCodeStore:
    def __init__(self, path: Path) -> None:
        self.path = path
        self._lock = RLock()

    def create(self, digest: str, *, expires_at: float, created_at: float) -> None:
        with self._lock:
            records = self._load()
            self._prune(records, now=created_at)
            records[digest] = {
                "expires_at": expires_at,
                "created_at": created_at,
                "redeemed_at": None,
                "installation_id": None,
                "response_payload": None,
            }
            self._save(records)

    def get_live(self, digest: str, *, now: float) -> dict[str, Any] | None:
        with self._lock:
            records = self._load()
            changed = self._prune(records, now=now)
            if changed:
                self._save(records)
            record = records.get(digest)
            return dict(record) if isinstance(record, dict) else None

    def redeem(self, digest: str, *, installation_id: str, payload: dict[str, Any], now: float) -> bool:
        with self._lock:
            records = self._load()
            self._prune(records, now=now)
            record = records.get(digest)
            if not isinstance(record, dict):
                self._save(records)
                return False
            if record.get("redeemed_at") is not None:
                return record.get("installation_id") == installation_id
            record["redeemed_at"] = now
            record["installation_id"] = installation_id
            record["response_payload"] = payload
            self._save(records)
            return True

    def _load(self) -> dict[str, dict[str, Any]]:
        try:
            value = json.loads(self.path.read_text())
            return value if isinstance(value, dict) else {}
        except (FileNotFoundError, json.JSONDecodeError, OSError):
            return {}

    @staticmethod
    def _prune(records: dict[str, dict[str, Any]], *, now: float) -> bool:
        expired = [key for key, value in records.items() if not isinstance(value, dict) or float(value.get("expires_at", 0)) < now]
        for key in expired:
            records.pop(key, None)
        return bool(expired)

    def _save(self, records: dict[str, dict[str, Any]]) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(".tmp")
        with open(tmp, "w", encoding="utf-8") as handle:
            json.dump(records, handle, separators=(",", ":"))
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, self.path)
