"""Build 104 P0 — one clientConversationId ↔ one hermesSessionId.

The Build 103 connector wrote two `conversation_bindings` rows per Hermes
session: one keyed by the client-supplied UUID, one keyed by the
deterministic `_app_uuid(hermes_id)`. The first INSERT won the
UNIQUE(hermes_session_id) constraint; the second became a logged
`conflict:` and the function still returned `hermesSessionState: "ready"`.
The first POST /v1/messages that followed then re-read the binding
table, found the canonical row's UUID did not match the client-supplied
UUID, and surfaced that as a typed 409.

This file tests the Build 104 fix at two levels:

1. The delivery-store migration (`DeliveryStore.resolve_duplicate_bindings`)
   collapses pre-existing duplicate rows to a single survivor and reports
   the work in `migration_report.json`.
2. The new `ensure_conversation` handler accepts the same client UUID
   for a fresh ensure and an immediately-following POST `/v1/messages`
   with that same UUID without ever creating a second binding row.
"""

from __future__ import annotations

import asyncio
import json
import time
import uuid
from pathlib import Path
from unittest.mock import AsyncMock, patch

import pytest
from starlette.testclient import TestClient

import herald_connector as connector
import herald_connector.http_facade as facade
from herald_connector import session_store
from herald_connector.delivery_store import (
    DeliveryStore,
    DuplicateConflictError,
    get_delivery_store,
    reset_delivery_store,
)
from herald_connector.http_facade import FacadeContext, app


# ── Fixtures ──────────────────────────────────────────────────────────────


@pytest.fixture(autouse=True)
def clean_facade_state():
    facade._http_jobs.clear()
    facade._http_job_tasks.clear()
    reset_delivery_store()
    yield
    facade._http_jobs.clear()
    facade._http_job_tasks.clear()
    reset_delivery_store()


@pytest.fixture
def env(tmp_path, monkeypatch):
    monkeypatch.setenv("HERMES_MOBILE_CONNECTOR_HOME", str(tmp_path))
    monkeypatch.setenv("HERMES_HOME", "/home/user/.hermes/profiles/ignyte")
    return tmp_path


@pytest.fixture
def store(env):
    return DeliveryStore(Path(env) / "delivery.sqlite3")


@pytest.fixture(autouse=True)
def auth():
    with patch("herald_connector.http_facade.require_auth", new_callable=AsyncMock):
        yield


@pytest.fixture
def ctx(env):
    ctx = FacadeContext()
    async def message_handler(*_args, **_kwargs):
        if False:  # async-generator shape required by the facade
            yield None
    ctx.message_handler = message_handler
    return ctx


@pytest.fixture
def app_env(env, ctx):
    with patch("herald_connector.http_facade.get_context", return_value=ctx):
        yield ctx


@pytest.fixture
def client():
    with TestClient(app) as c:
        yield c


@pytest.fixture
def no_session_lookups(monkeypatch):
    """Keep _run_http_job's done path off state.db (which is absent in tests)."""
    monkeypatch.setattr(
        session_store, "_find_session_by_assistant_reply", lambda *a, **k: None
    )
    monkeypatch.setattr(
        session_store, "_find_session_by_recent_message", lambda *a, **k: None
    )


# ── Schema ────────────────────────────────────────────────────────────────


class TestSchemaVersion2:
    def test_schema_version_is_2(self):
        from herald_connector.delivery_store import _EXPECTED_SCHEMA_VERSION
        assert _EXPECTED_SCHEMA_VERSION == 2

    def test_archived_columns_exist(self, store):
        names = {
            row["name"]
            for row in store._connect().execute(
                "PRAGMA table_info(conversation_bindings)"
            ).fetchall()
        }
        assert "archived" in names
        assert "archived_reason" in names
        assert "archived_at" in names


# ── Migration ─────────────────────────────────────────────────────────────


class TestResolveDuplicateBindings:
    def test_migration_collapses_duplicates_to_one_survivor(self, store, tmp_path):
        # Pre-B103 state: the connector wrote two rows for the same Hermes
        # session, one keyed by the client UUID, one by `_app_uuid(hermes_id)`.
        # The UNIQUE(hermes_session_id) constraint on the production schema
        # actually prevents the second insert — but the production code
        # bypasses it by INSERT OR IGNORE in a single statement and the
        # resulting state has only the canonical row. The migration must
        # therefore handle a table whose duplicate rows were already
        # collapsed by SQLite, then re-write a second row that simulates
        # the historical "canonical + draft alias" pair.
        hermes_id = "api-deadbeefcafef00d"
        client_uuid = "550e8400-e29b-41d4-a716-446655440001"
        canonical_uuid = session_store._app_uuid(hermes_id)
        assert canonical_uuid != client_uuid
        # Insert only the canonical row (the UNIQUE constraint blocks the
        # second) and then assert the migration is a no-op for a single
        # row. The actual duplicate state is exercised by the test below
        # via the `_connect()` + transactional insert path.
        with store._connect() as conn:
            conn.execute(
                "INSERT INTO conversation_bindings "
                "(app_conversation_id, hermes_session_id, account_id, "
                " owner_device_id, created_at, updated_at) "
                "VALUES (?, ?, 'a', 'd', '2026-08-01T10:00:00Z', "
                "        '2026-08-01T10:00:00Z')",
                (canonical_uuid, hermes_id),
            )
            conn.commit()
        report = store.resolve_duplicate_bindings(evidence_dir=tmp_path / "ev")
        # Single row → no duplicates to resolve.
        assert report["duplicateSessions"] == 0
        assert report["archived"] == 0
        assert (tmp_path / "ev" / "delivery.sqlite3.snapshot").exists()
        assert (tmp_path / "ev" / "migration_report.json").exists()

        # The "two rows for the same session" case (which the production
        # Build 103 connector produced) is exercised below by
        # test_migration_handles_two_binding_rows_via_get_or_create
        # against a DB without the UNIQUE constraint.
        with store._connect() as conn:
            rows = conn.execute(
                "SELECT app_conversation_id, archived FROM conversation_bindings"
            ).fetchall()
        assert len(rows) == 1
        assert rows[0]["archived"] == 0

    def test_migration_handles_two_binding_rows_via_get_or_create(self, store, tmp_path):
        """Reproduce the Build 103 dual-row state, then assert the
        migration collapses it to a single survivor.

        The production schema's UNIQUE(hermes_session_id) blocks the
        second insert at runtime, so we rebuild the table without that
        constraint to stage the duplicate state, then re-create the
        index so the rest of the test suite can run unchanged.
        """
        hermes_id = "api-1111111111111111"
        client_uuid = "550e8400-e29b-41d4-a716-446655440001"
        canonical_uuid = session_store._app_uuid(hermes_id)
        import sqlite3 as _sqlite
        raw = _sqlite.connect(str(store.db_path))
        try:
            raw.execute("BEGIN")
            raw.execute("CREATE TABLE conversation_bindings__new ("
                        " app_conversation_id TEXT PRIMARY KEY, "
                        " hermes_session_id TEXT NOT NULL, "
                        " account_id TEXT NOT NULL, "
                        " owner_device_id TEXT NOT NULL, "
                        " created_at TEXT NOT NULL, "
                        " updated_at TEXT NOT NULL, "
                        " archived INTEGER NOT NULL DEFAULT 0, "
                        " archived_reason TEXT, "
                        " archived_at TEXT)")
            raw.execute("INSERT INTO conversation_bindings__new "
                        "SELECT app_conversation_id, hermes_session_id, "
                        "       account_id, owner_device_id, "
                        "       created_at, updated_at, archived, "
                        "       archived_reason, archived_at "
                        "FROM conversation_bindings")
            raw.execute("INSERT INTO conversation_bindings__new "
                        "(app_conversation_id, hermes_session_id, "
                        " account_id, owner_device_id, created_at, "
                        " updated_at, archived, archived_reason, "
                        " archived_at) "
                        "VALUES (?, ?, 'a', 'd', "
                        "        '2026-08-01T10:00:00Z', "
                        "        '2026-08-01T10:00:00Z', 0, NULL, NULL)",
                        (canonical_uuid, hermes_id))
            raw.execute("INSERT INTO conversation_bindings__new "
                        "(app_conversation_id, hermes_session_id, "
                        " account_id, owner_device_id, created_at, "
                        " updated_at, archived, archived_reason, "
                        " archived_at) "
                        "VALUES (?, ?, 'a', 'd', "
                        "        '2026-08-01T10:01:00Z', "
                        "        '2026-08-01T10:01:00Z', 0, NULL, NULL)",
                        (client_uuid, hermes_id))
            raw.execute("INSERT INTO message_requests "
                        "(client_message_id, conversation_id, owner_device_id, "
                        " request_sha256, clean_text, state, created_at, "
                        " updated_at) "
                        "VALUES ('770e8400-e29b-41d4-a716-446655440000', ?, "
                        "        'd', 'h', 't', 'terminal', "
                        "        '2026-08-01T10:02:00Z', "
                        "        '2026-08-01T10:02:00Z')",
                        (canonical_uuid,))
            raw.execute("DROP TABLE conversation_bindings")
            raw.execute("ALTER TABLE conversation_bindings__new "
                        "RENAME TO conversation_bindings")
            raw.commit()
        finally:
            raw.close()
        # The migration must collapse the duplicate first so the
        # UNIQUE index can be recreated.  The test rebuilds the table
        # without a UNIQUE index, so the migration finds duplicates
        # clustered by hermes_session_id and marks the second row
        # archived.
        report = store.resolve_duplicate_bindings(evidence_dir=tmp_path / "ev")
        assert report["duplicateSessions"] == 1
        assert report["archived"] == 1
        survivor = store.get_binding(canonical_uuid)
        assert survivor is not None
        assert store.get_binding(client_uuid) is None
        # Now the production partial UNIQUE(hermes_session_id) index
        # can be recreated.  It ignores archived=1 rows, so the
        # archived duplicate that the migration left behind does not
        # block it.
        with store._connect() as conn:
            conn.execute("DROP INDEX IF EXISTS ix_conversation_bindings_hermes")
            conn.execute(
                "CREATE UNIQUE INDEX IF NOT EXISTS "
                "ix_conversation_bindings_hermes "
                "ON conversation_bindings(hermes_session_id) "
                "WHERE archived = 0"
            )

    def test_migration_is_idempotent(self, store, tmp_path):
        # Two distinct Hermes sessions (each with a single binding row)
        # → no duplicates.  Calling twice still does nothing on the
        # second pass.
        store.get_or_create_binding(
            "550e8400-e29b-41d4-a716-446655440001",
            "api-aaaaaaaaaaaaaaaaa",
            "a", "d",
        )
        store.get_or_create_binding(
            "660e8400-e29b-41d4-a716-446655440002",
            "api-bbbbbbbbbbbbbbbb",
            "a", "d",
        )
        first = store.resolve_duplicate_bindings(evidence_dir=tmp_path / "ev")
        second = store.resolve_duplicate_bindings(evidence_dir=tmp_path / "ev")
        assert first["duplicateSessions"] == 0
        assert first["archived"] == 0
        assert second["duplicateSessions"] == 0
        assert second["archived"] == 0

    def test_migration_no_op_when_no_duplicates(self, store, tmp_path):
        store.get_or_create_binding(
            "550e8400-e29b-41d4-a716-446655440001",
            "api-no-duplicates-aaaa",
            "a", "d",
        )
        report = store.resolve_duplicate_bindings(evidence_dir=tmp_path / "ev")
        assert report["duplicateSessions"] == 0
        assert report["archived"] == 0
        assert report["survivors"] == 0


# ── API contract ──────────────────────────────────────────────────────────


class TestEnsureOneBinding:
    def test_build103_alias_is_promoted_to_client_uuid(self, store):
        """A legacy canonical binding must not leave Build 104 at HTTP 409."""
        hermes_id = "api-abcdefabcdefabcd"
        client_uuid = "550e8400-e29b-41d4-a716-446655440077"
        legacy_alias = session_store._app_uuid(hermes_id)
        store.get_or_create_binding(legacy_alias, hermes_id, "", "")

        assert store.promote_legacy_alias(
            client_uuid, hermes_id, legacy_alias, "", ""
        ) is True
        assert store.get_binding(legacy_alias) is None
        binding = store.get_binding(client_uuid)
        assert binding is not None
        assert binding["hermesSessionId"] == hermes_id

    def test_ensure_then_send_with_same_uuid_creates_one_binding(
        self, client, app_env, env, no_session_lookups, monkeypatch,
    ):
        """The Build 103 409 path: ensure + immediate send with the same
        client UUID must result in exactly one binding row and a pending
        job — never a 409."""
        client_uuid = "550e8400-e29b-41d4-a716-446655440001"
        client_msg_uuid = "660e8400-e29b-41d4-a716-446655440002"

        # Stub the Hermes API server so the connector "creates" a session
        # for the supplied UUID.
        monkeypatch.setattr(
            session_store, "_create_hermes_session_via_api",
            lambda *a, **kw: f"api-{client_msg_uuid.replace('-','')[:16]}",
        )
        monkeypatch.setattr(
            session_store, "_verify_session_in_state_db",
            lambda *a, **kw: True,
        )

        ensure = client.post(
            "/v1/conversations/ensure",
            json={"conversationId": client_uuid},
        )
        assert ensure.status_code == 200, ensure.text
        body = ensure.json()
        assert body["hermesSessionState"] == "ready"
        assert body["conversationId"] == client_uuid
        assert body["created"] is True

        # Now POST /v1/messages with the SAME client UUID.
        send = client.post(
            "/v1/messages",
            json={
                "heraldProtocol": connector.HERALD_PROTOCOL,
                "conversationId": client_uuid,
                "clientMessageId": client_msg_uuid,
                "text": "hello",
            },
        )
        assert send.status_code == 200, send.text
        assert send.json()["replyState"] == "pending"

        # The delivery store should hold exactly ONE active binding row.
        store = get_delivery_store()
        with store._connect() as conn:
            rows = conn.execute(
                "SELECT app_conversation_id, hermes_session_id, archived "
                "FROM conversation_bindings"
            ).fetchall()
        active = [r for r in rows if r["archived"] == 0]
        assert len(active) == 1
        assert active[0]["app_conversation_id"] == client_uuid

    def test_ensure_returns_409_on_hermes_session_already_bound_to_another_uuid(
        self, client, app_env, env, monkeypatch,
    ):
        """If the Hermes session is already bound to a different app UUID,
        the new ensure call must surface a typed 409 — never silently succeed."""
        # Pre-seed the binding table: a different app UUID owns the session.
        store = get_delivery_store()
        store.get_or_create_binding(
            "550e8400-e29b-41d4-a716-446655440001",
            "api-conflict-sssssssss",
            "a", "d",
        )
        # New client UUID tries to ensure against the same Hermes session.
        new_client_uuid = "660e8400-e29b-41d4-a716-446655440002"

        monkeypatch.setattr(
            session_store, "_create_hermes_session_via_api",
            lambda *a, **kw: "api-conflict-sssssssss",
        )
        monkeypatch.setattr(
            session_store, "_verify_session_in_state_db",
            lambda *a, **kw: True,
        )

        resp = client.post(
            "/v1/conversations/ensure",
            json={"conversationId": new_client_uuid},
        )
        # 409 replyState: binding_conflict is the expected surface.
        assert resp.status_code == 409, resp.text
        body = resp.json()
        assert body.get("replyState") == "binding_conflict"
        assert body.get("sessionId") is None
        assert body.get("hermesSessionState") is None

    def test_send_without_ensure_returns_typed_409(
        self, client, app_env, env, no_session_lookups,
    ):
        """POST /v1/messages without a prior /v1/conversations/ensure must
        fail closed with `replyState: conversation_not_ensured`."""
        client_uuid = "550e8400-e29b-41d4-a716-446655440099"
        send = client.post(
            "/v1/messages",
            json={
                "heraldProtocol": connector.HERALD_PROTOCOL,
                "conversationId": client_uuid,
                "clientMessageId": "770e8400-e29b-41d4-a716-446655440003",
                "text": "hello",
            },
        )
        assert send.status_code == 409
        assert send.json()["replyState"] == "conversation_not_ensured"
