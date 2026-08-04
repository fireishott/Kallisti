"""Durable restart-operation tracking for Hermes/connector restarts.

Build 33 Workstream A (connector side): restarts are no longer one-shot
fire-and-forget.  Every restart request creates an operation row here, in
SQLite, so the state survives:

  * HTTP round-trips — the app polls GET /v1/gw/restart/{operationId}
  * connector self-restarts — the row is committed BEFORE SIGTERM fires
  * process crashes — startup reconciliation marks stale rows failed

The wire contract (fixtures in connector/tests/fixtures/restart/) is:

  restart-preflight-v1 — GET  /v1/gw/restart/preflight
  restart-operation-v1 — POST /v1/gw/restart  +  GET /v1/gw/restart/{id}
  gateway-health-v1    — GET  /v1/gw/status

GUARDRAIL: never store bearer tokens, credentials, or message content in
this database — it must be safe to sync/back up.  The error path stores a
sanitized journal excerpt only.
"""

from __future__ import annotations

import datetime
import json
import logging
import os
import sqlite3
import threading
from pathlib import Path
from typing import Any

logger = logging.getLogger("herald.restart_operations")

# ── Phases ────────────────────────────────────────────────────────────────

PHASES = ("accepted", "stopping", "starting", "verifying", "healthy", "failed")
NON_TERMINAL_PHASES = frozenset({"accepted", "stopping", "starting", "verifying"})
TERMINAL_PHASES = frozenset({"healthy", "failed"})

# ── Paths / time ──────────────────────────────────────────────────────────


def restart_operations_db_path() -> Path:
    """DB location: $HERMES_MOBILE_CONNECTOR_HOME/restart_operations.sqlite3."""
    home = os.getenv("HERMES_MOBILE_CONNECTOR_HOME") or str(Path.home() / ".hermes-mobile")
    return Path(home) / "restart_operations.sqlite3"


def _utcnow_rfc3339() -> str:
    """RFC 3339 UTC timestamp with a Z suffix (matches the contract fixtures)."""
    return datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


# ── Errors ────────────────────────────────────────────────────────────────


class RestartConflictError(RuntimeError):
    """A new restart operation would collide with an active one.

    Carries the existing (active) operation so the caller can return it in a
    409 response without a second read.
    """

    def __init__(self, target: str, operation: dict) -> None:
        super().__init__(f"Restart already in progress for target {target!r}")
        self.target = target
        self.operation = operation
        self.operation_id = operation["operationId"]


class RestartOperationNotFoundError(KeyError):
    """No operation row exists for the requested operation id."""


# ── Store ─────────────────────────────────────────────────────────────────

_SCHEMA = """
CREATE TABLE IF NOT EXISTS restart_operations (
  operation_id TEXT PRIMARY KEY,
  idempotency_key TEXT UNIQUE NOT NULL,
  target TEXT NOT NULL CHECK(target IN ('hermes','connector')),
  unit TEXT NOT NULL,
  phase TEXT NOT NULL CHECK(phase IN ('accepted','stopping','starting','verifying','healthy','failed')),
  preflight_version TEXT,
  old_main_pid INTEGER,
  old_exec_main_start_timestamp TEXT,
  accepted_at TEXT NOT NULL,
  completed_at TEXT,
  error_stage TEXT,
  error_exit_status INTEGER,
  error_journal_excerpt TEXT,
  error_retryable INTEGER,
  error_action TEXT,
  checks_json TEXT DEFAULT '[]'
)
"""

class RestartOperationStore:
    """SQLite-backed store for restart operations (WAL, mode 0600).

    Thread-safe for the connector's event loop + background executor:
    every mutation runs under a lock and uses a short busy timeout.
    """

    def __init__(self, db_path: str | Path | None = None) -> None:
        self.db_path: Path = Path(db_path) if db_path is not None else restart_operations_db_path()
        self._lock = threading.Lock()
        self._init_db()

    # ── connection helpers ────────────────────────────────────────────────

    def _connect(self) -> sqlite3.Connection:
        conn = sqlite3.connect(str(self.db_path), timeout=10.0)
        conn.row_factory = sqlite3.Row
        conn.execute("PRAGMA journal_mode=WAL")
        conn.execute("PRAGMA foreign_keys=ON")
        conn.execute("PRAGMA busy_timeout=5000")
        return conn

    def _secure_files(self) -> None:
        """DB + WAL + SHM must be mode 0600 — the rows describe the host."""
        for path in (self.db_path, Path(str(self.db_path) + "-wal"), Path(str(self.db_path) + "-shm")):
            try:
                if path.exists():
                    os.chmod(path, 0o600)
            except OSError:
                logger.debug("restart: could not chmod %s (non-fatal)", path)

    def _init_db(self) -> None:
        self.db_path.parent.mkdir(parents=True, exist_ok=True)
        with self._lock:
            with self._connect() as conn:
                conn.execute(_SCHEMA)
                conn.commit()
        self._secure_files()

    # ── row → contract dict ───────────────────────────────────────────────

    @staticmethod
    def _row_to_operation(row: sqlite3.Row) -> dict:
        """restart-operation-v1 payload — the exact shape the app decodes."""
        error = None
        if row["error_stage"] is not None:
            error = {
                "stage": row["error_stage"],
                "unit": row["unit"],
                "exitStatus": row["error_exit_status"],
                "journalExcerpt": row["error_journal_excerpt"],
                "retryable": bool(row["error_retryable"]),
                "action": row["error_action"],
            }
        try:
            checks = json.loads(row["checks_json"] or "[]")
            if not isinstance(checks, list):
                checks = []
        except (ValueError, TypeError):
            checks = []
        return {
            "$schema": "restart-operation-v1",
            "operationId": row["operation_id"],
            "target": row["target"],
            "unit": row["unit"],
            "phase": row["phase"],
            "acceptedAt": row["accepted_at"],
            "completedAt": row["completed_at"],
            "checks": checks,
            "error": error,
        }

    # ── writes ────────────────────────────────────────────────────────────

    def create_operation(
        self,
        operation_id: str,
        idempotency_key: str,
        target: str,
        unit: str,
        preflight_version: str | None = None,
        old_pid: int | None = None,
        old_start_ts: str | None = None,
    ) -> dict:
        """Atomically create an operation.

        * Same idempotency_key → returns the existing operation (replay-safe,
          even after completion).  No new row.
        * Any non-terminal operation for the same target → RestartConflictError
          carrying the active operation.
        """
        if target not in ("hermes", "connector"):
            raise ValueError(f"Invalid restart target: {target!r}")
        now = _utcnow_rfc3339()
        with self._lock:
            conn = self._connect()
            try:
                existing = conn.execute(
                    "SELECT * FROM restart_operations WHERE idempotency_key = ?",
                    (idempotency_key,),
                ).fetchone()
                if existing is not None:
                    return self._row_to_operation(existing)

                active = conn.execute(
                    "SELECT * FROM restart_operations "
                    "WHERE target = ? AND phase NOT IN ('healthy','failed') "
                    "ORDER BY accepted_at DESC LIMIT 1",
                    (target,),
                ).fetchone()
                if active is not None:
                    raise RestartConflictError(target, self._row_to_operation(active))

                conn.execute(
                    "INSERT INTO restart_operations "
                    "(operation_id, idempotency_key, target, unit, phase, "
                    " preflight_version, old_main_pid, old_exec_main_start_timestamp, "
                    " accepted_at, checks_json) "
                    "VALUES (?, ?, ?, ?, 'accepted', ?, ?, ?, ?, '[]')",
                    (
                        operation_id,
                        idempotency_key,
                        target,
                        unit,
                        preflight_version,
                        old_pid,
                        old_start_ts,
                        now,
                    ),
                )
                conn.commit()
                row = conn.execute(
                    "SELECT * FROM restart_operations WHERE operation_id = ?",
                    (operation_id,),
                ).fetchone()
                logger.info(
                    "restart: operation %s created (target=%s, unit=%s, idempotency=%s)",
                    operation_id, target, unit, idempotency_key[:12],
                )
                return self._row_to_operation(row)
            finally:
                conn.close()

    def update_phase(
        self,
        operation_id: str,
        phase: str,
        checks: list[dict] | None = None,
        error: dict | None = None,
    ) -> dict:
        """Transition to a non-terminal phase, appending any new checks.

        Raises ValueError if the operation is already terminal (use
        complete_operation for the final transition) or if *phase* is
        terminal itself.
        """
        if phase not in PHASES:
            raise ValueError(f"Invalid phase {phase!r} — one of {PHASES}")
        if phase in TERMINAL_PHASES:
            raise ValueError(
                "update_phase cannot set a terminal phase — use complete_operation"
            )
        with self._lock:
            conn = self._connect()
            try:
                row = self._fetch_locked(conn, operation_id)
                if row is None:
                    raise RestartOperationNotFoundError(operation_id)
                if row["phase"] in TERMINAL_PHASES:
                    raise ValueError(
                        f"Operation {operation_id} is already {row['phase']} — "
                        f"cannot transition to {phase}"
                    )
                merged = self._merge_checks_locked(conn, operation_id, checks)
                stage, exit_status, journal, retryable, action = _error_to_cols(error)
                conn.execute(
                    "UPDATE restart_operations SET phase = ?, checks_json = ?, "
                    " error_stage = ?, error_exit_status = ?, error_journal_excerpt = ?, "
                    " error_retryable = ?, error_action = ? WHERE operation_id = ?",
                    (phase, json.dumps(merged), stage, exit_status, journal, retryable, action, operation_id),
                )
                conn.commit()
                logger.info("restart: operation %s → phase %s", operation_id, phase)
                return self._row_to_operation(self._fetch_locked(conn, operation_id))
            finally:
                conn.close()

    def complete_operation(
        self,
        operation_id: str,
        phase: str,
        checks: list[dict] | None = None,
        error: dict | None = None,
    ) -> dict:
        """Set the final phase (healthy|failed), completed_at, and error.

        *checks* REPLACES the stored array — this is the final snapshot the
        client decodes (unlike update_phase, which appends incrementally).
        """
        if phase not in TERMINAL_PHASES:
            raise ValueError(
                f"complete_operation requires a terminal phase, got {phase!r}"
            )
        with self._lock:
            conn = self._connect()
            try:
                row = self._fetch_locked(conn, operation_id)
                if row is None:
                    raise RestartOperationNotFoundError(operation_id)
                merged = [c for c in (checks or []) if isinstance(c, dict)]
                stage, exit_status, journal, retryable, action = _error_to_cols(error)
                conn.execute(
                    "UPDATE restart_operations SET phase = ?, completed_at = ?, "
                    " checks_json = ?, error_stage = ?, error_exit_status = ?, "
                    " error_journal_excerpt = ?, error_retryable = ?, error_action = ? "
                    "WHERE operation_id = ?",
                    (
                        phase,
                        _utcnow_rfc3339(),
                        json.dumps(merged),
                        stage,
                        exit_status,
                        journal,
                        retryable,
                        action,
                        operation_id,
                    ),
                )
                conn.commit()
                logger.info("restart: operation %s complete → %s", operation_id, phase)
                return self._row_to_operation(self._fetch_locked(conn, operation_id))
            finally:
                conn.close()

    def reconcile_stale_operations(self) -> int:
        """Mark every non-terminal operation failed.

        Called at connector startup: a non-terminal operation means the
        previous connector process died (self-restart, crash, kill -9)
        before verification could finish.
        """
        stale: list[sqlite3.Row] = []
        with self._lock:
            conn = self._connect()
            try:
                stale = list(conn.execute(
                    "SELECT * FROM restart_operations WHERE phase NOT IN ('healthy','failed')"
                ).fetchall())
            finally:
                conn.close()
        for row in stale:
            self.complete_operation(
                operation_id=row["operation_id"],
                phase="failed",
                checks=[{
                    "name": "startup-reconciliation",
                    "passed": False,
                    "detail": "connector process restarted mid-operation",
                }],
                error={
                    "stage": row["phase"],
                    "exitStatus": None,
                    "journalExcerpt": None,
                    "retryable": True,
                    "action": (
                        "The connector process restarted before this operation "
                        "could be verified. Retry the restart."
                    ),
                },
            )
        if stale:
            logger.warning(
                "restart: marked %d stale operation(s) failed at startup",
                len(stale),
            )
        return len(stale)

    # ── reads ─────────────────────────────────────────────────────────────

    def get_operation(self, operation_id: str) -> dict | None:
        """restart-operation-v1 payload for the operation, or None."""
        with self._lock:
            conn = self._connect()
            try:
                row = self._fetch_locked(conn, operation_id)
                return self._row_to_operation(row) if row is not None else None
            finally:
                conn.close()

    def get_operation_details(self, operation_id: str) -> dict | None:
        """Full row incl. internal columns (old pid, preflight version, …).

        Used by the background executor — not part of the wire contract.
        """
        with self._lock:
            conn = self._connect()
            try:
                row = self._fetch_locked(conn, operation_id)
                if row is None:
                    return None
                return {
                    "operationId": row["operation_id"],
                    "idempotencyKey": row["idempotency_key"],
                    "target": row["target"],
                    "unit": row["unit"],
                    "phase": row["phase"],
                    "preflightVersion": row["preflight_version"],
                    "oldMainPid": row["old_main_pid"],
                    "oldExecMainStartTimestamp": row["old_exec_main_start_timestamp"],
                    "acceptedAt": row["accepted_at"],
                    "completedAt": row["completed_at"],
                }
            finally:
                conn.close()

    def get_active_operation(self, target: str) -> dict | None:
        """Non-terminal operation for *target*, if any (newest first)."""
        with self._lock:
            conn = self._connect()
            try:
                row = conn.execute(
                    "SELECT * FROM restart_operations "
                    "WHERE target = ? AND phase NOT IN ('healthy','failed') "
                    "ORDER BY accepted_at DESC LIMIT 1",
                    (target,),
                ).fetchone()
                return self._row_to_operation(row) if row is not None else None
            finally:
                conn.close()

    def get_by_idempotency_key(self, idempotency_key: str) -> dict | None:
        """Existing operation for the key — idempotent replay support."""
        with self._lock:
            conn = self._connect()
            try:
                row = conn.execute(
                    "SELECT * FROM restart_operations WHERE idempotency_key = ?",
                    (idempotency_key,),
                ).fetchone()
                return self._row_to_operation(row) if row is not None else None
            finally:
                conn.close()

    # ── internals (caller must hold the lock) ─────────────────────────────

    @staticmethod
    def _fetch_locked(conn: sqlite3.Connection, operation_id: str) -> sqlite3.Row | None:
        return conn.execute(
            "SELECT * FROM restart_operations WHERE operation_id = ?",
            (operation_id,),
        ).fetchone()

    @staticmethod
    def _merge_checks_locked(
        conn: sqlite3.Connection, operation_id: str, checks: list[dict] | None
    ) -> list[dict]:
        """Append *checks* to the stored array (spec: update_phase appends)."""
        row = conn.execute(
            "SELECT checks_json FROM restart_operations WHERE operation_id = ?",
            (operation_id,),
        ).fetchone()
        merged: list[dict] = []
        if row is not None:
            try:
                parsed = json.loads(row["checks_json"] or "[]")
                if isinstance(parsed, list):
                    merged = parsed
            except (ValueError, TypeError):
                merged = []
        for check in checks or []:
            if isinstance(check, dict):
                merged.append(check)
        return merged


def _error_to_cols(error: dict | None) -> tuple[Any, ...]:
    """Map a typed error dict onto the error_* columns (unit comes from the row)."""
    if not error:
        return (None, None, None, None, None)
    return (
        error.get("stage"),
        error.get("exitStatus"),
        error.get("journalExcerpt"),
        1 if error.get("retryable") else 0,
        error.get("action"),
    )


# ── Process-wide singleton ────────────────────────────────────────────────
#
# Both the HTTP facade and the connector's startup reconciliation use one
# store.  The singleton re-binds automatically when HERMES_MOBILE_CONNECTOR_HOME
# changes (tests flip it per case).

_store_singleton: RestartOperationStore | None = None


def get_restart_store() -> RestartOperationStore:
    global _store_singleton
    db_path = restart_operations_db_path()
    if _store_singleton is None or _store_singleton.db_path != db_path:
        _store_singleton = RestartOperationStore(db_path)
    return _store_singleton


def reset_restart_store() -> None:
    """Drop the singleton (tests only — env-scoped stores re-resolve on demand)."""
    global _store_singleton
    _store_singleton = None
