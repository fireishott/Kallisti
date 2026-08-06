"""Build 33 Workstream B: durable delivery store — connector side.

Decodes the shared contract fixtures in tests/fixtures/delivery/ and verifies
the connector's SQLite delivery store plus the POST /v1/messages idempotency
wiring:

  * all 6 contract fixtures decode and match the store's row shapes
  * conversation binding create / get / conflict
  * message request idempotency (same hash → existing row, different hash → conflict)
  * accepted → running → terminal lifecycle (+ failure / cancellation)
  * attachment and job-event round-trips
  * stale-job reconciliation (connector restart recovery)
  * session_meta.json → SQLite binding migration
  * /v1/messages: duplicate (200), conflict (409), and lifecycle through the
    real handler
"""

from __future__ import annotations

import datetime
import hashlib
import json
import re
import stat
import time
import uuid
from pathlib import Path
from unittest.mock import AsyncMock, patch

import pytest
from starlette.testclient import TestClient

import kallisti_connector as connector
import kallisti_connector.http_facade as facade
from kallisti_connector import session_store
from kallisti_connector.delivery_store import (
    DeliveryStore,
    DuplicateConflictError,
    get_delivery_store,
    request_sha256,
    reset_delivery_store,
)
from kallisti_connector.http_facade import FacadeContext, app

FIXTURES = Path(__file__).parent / "fixtures" / "delivery"

_RFC3339_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")

CONV = "660e8400-e29b-41d4-a716-446655440001"
HERMES_SID = "aa0e8400-e29b-41d4-a716-446655440005"
CLIENT_MSG = "550e8400-e29b-41d4-a716-446655440000"
JOB1 = "770e8400-e29b-41d4-a716-446655440002"
DEVICE = "abc123def"


def load_fixture(name: str) -> dict:
    return json.loads((FIXTURES / name).read_text())


# ── Shared fixtures ────────────────────────────────────────────────────────


@pytest.fixture(autouse=True)
def clean_facade_state():
    """Isolate module-level facade state + delivery-store singleton."""
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
    with patch("kallisti_connector.http_facade.require_auth", new_callable=AsyncMock):
        yield


@pytest.fixture
def ctx(env):
    ctx = FacadeContext()
    ctx.message_handler = AsyncMock()
    return ctx


@pytest.fixture
def app_env(env, ctx):
    """ctx patched as the live facade context for the whole test."""
    with patch("kallisti_connector.http_facade.get_context", return_value=ctx):
        yield ctx


@pytest.fixture
def client():
    with TestClient(app) as c:
        yield c


@pytest.fixture
def no_session_lookups(monkeypatch):
    """Keep _run_http_job's done path off state.db (which is absent in tests).

    Without these, the reply/user-text anchor lookups raise OperationalError
    on the missing HERMES_HOME state.db and the job is marked failed.
    """
    monkeypatch.setattr(
        session_store, "_find_session_by_assistant_reply", lambda *a, **k: None
    )
    monkeypatch.setattr(
        session_store, "_find_session_by_recent_message", lambda *a, **k: None
    )


def post_message(client, **overrides) -> dict:
    body = {
        "heraldProtocol": connector.HERALD_PROTOCOL,
        "text": "What is the weather?",
        "conversationId": CONV,
        "clientMessageId": CLIENT_MSG,
    }
    body.update(overrides)
    return body


async def completed_handler(text, history, session_id, attachments, reasoning_effort):  # noqa: ANN001, ARG001
    yield {"type": "done", "data": {"status": "completed", "text": "ok"}}
    yield {"type": "text_delta", "data": {"delta": "ok"}}


# ── Contract fixtures ──────────────────────────────────────────────────────


class TestFixtures:
    def test_all_six_fixtures_decode(self):
        for name in (
            "conversation_binding.json",
            "message_accepted_response.json",
            "message_conflict_response.json",
            "message_duplicate_response.json",
            "message_request_accepted.json",
            "message_request_completed.json",
        ):
            data = load_fixture(name)
            assert isinstance(data, dict) and data, name

    def test_binding_fixture_matches_store_shape(self, store):
        binding = store.get_or_create_binding(
            CONV, HERMES_SID, "acc-001", DEVICE,
        )
        fixture = load_fixture("conversation_binding.json")
        assert set(binding.keys()) == set(fixture.keys())
        assert binding["$schema"] == "conversation-binding-v1"
        assert binding["appConversationId"] == fixture["appConversationId"]
        assert binding["hermesSessionId"] == fixture["hermesSessionId"]
        assert binding["accountId"] == fixture["accountId"]
        assert binding["ownerDeviceId"] == fixture["ownerDeviceId"]
        assert _RFC3339_RE.match(binding["createdAt"])
        assert _RFC3339_RE.match(binding["updatedAt"])

    def test_request_fixtures_match_store_shape(self, store):
        store.get_or_create_binding(CONV, HERMES_SID, "acc-001", DEVICE)
        accepted = store.create_message_request(
            CLIENT_MSG, CONV, DEVICE, "What is the weather?",
            load_fixture("message_request_accepted.json")["requestSha256"],
        )
        fixture = load_fixture("message_request_accepted.json")
        assert set(accepted.keys()) == set(fixture.keys())
        assert accepted["$schema"] == "message-request-v1"
        assert accepted["clientMessageId"] == CLIENT_MSG
        assert accepted["conversationId"] == CONV
        assert accepted["installationId"] == DEVICE
        assert accepted["cleanText"] == "What is the weather?"
        assert accepted["state"] == "accepted"
        assert accepted["jobId"] is None
        assert accepted["canonicalUserMessageId"] is None
        assert accepted["terminalMessageId"] is None
        assert accepted["errorCategory"] is None
        assert _RFC3339_RE.match(accepted["createdAt"])

        # complete_message_request is running→terminal: accept first, the way
        # send_message does.
        store.accept_message_request(CLIENT_MSG, JOB1)
        done = store.complete_message_request(
            CLIENT_MSG,
            canonical_user_message_id="880e8400-e29b-41d4-a716-446655440003",
            terminal_message_id="990e8400-e29b-41d4-a716-446655440004",
        )
        fixture = load_fixture("message_request_completed.json")
        assert set(done.keys()) == set(fixture.keys())
        assert done["state"] == "terminal"
        assert done["canonicalUserMessageId"] == fixture["canonicalUserMessageId"]
        assert done["terminalMessageId"] == fixture["terminalMessageId"]

    def test_request_sha256_is_stable_and_content_sensitive(self):
        h1 = request_sha256("What is the weather?", None)
        h2 = request_sha256("What is the weather?", [])
        h3 = request_sha256("What is the weather?", [{"type": "image", "filename": "a.png", "mimeType": "image/png"}])
        h4 = request_sha256("What is the weather?", [{"type": "image", "filename": "a.png", "mimeType": "image/png"}])
        expected = hashlib.sha256(
            json.dumps({"text": "What is the weather?", "attachments": []},
                       sort_keys=True).encode()
        ).hexdigest()
        assert h1 == h2 == expected
        assert h3 == h4
        assert h3 != h1
        # Attachment order must not change the hash.
        assert request_sha256("t", [{"id": "b"}, {"id": "a"}]) == \
               request_sha256("t", [{"id": "a"}, {"id": "b"}])


# ── Conversation bindings ──────────────────────────────────────────────────


class TestBindings:
    def test_create_and_get_binding(self, store):
        binding = store.get_or_create_binding(CONV, HERMES_SID, "acc-1", DEVICE)
        assert store.get_binding(CONV) == binding
        assert store.get_binding("nope") is None
        by_hermes = store.get_binding_by_hermes(HERMES_SID)
        assert by_hermes == binding
        assert store.get_binding_by_hermes("nope") is None

    def test_same_binding_is_idempotent(self, store):
        first = store.get_or_create_binding(CONV, HERMES_SID, "acc-1", DEVICE)
        second = store.get_or_create_binding(CONV, HERMES_SID, "acc-1", DEVICE)
        assert second == first
        assert store.get_binding(CONV)["hermesSessionId"] == HERMES_SID

    def test_different_hermes_session_on_same_conversation_fails(self, store):
        store.get_or_create_binding(CONV, HERMES_SID, "acc-1", DEVICE)
        with pytest.raises(DuplicateConflictError):
            store.get_or_create_binding(
                CONV, "bb0e8400-e29b-41d4-a716-446655440006", "acc-1", DEVICE,
            )

    def test_same_hermes_session_on_different_conversation_fails(self, store):
        store.get_or_create_binding(CONV, HERMES_SID, "acc-1", DEVICE)
        with pytest.raises(DuplicateConflictError):
            store.get_or_create_binding(
                "cc0e8400-e29b-41d4-a716-446655440007", HERMES_SID,
                "acc-1", DEVICE,
            )


# ── Message requests ───────────────────────────────────────────────────────


class TestMessageRequests:
    def test_create_and_get(self, store):
        store.get_or_create_binding(CONV, HERMES_SID, "acc-1", DEVICE)
        row = store.create_message_request(
            CLIENT_MSG, CONV, DEVICE, "hello", request_sha256("hello", None),
        )
        assert row["state"] == "accepted"
        assert row["jobId"] is None
        assert store.get_message_request(CLIENT_MSG) == row
        assert store.get_message_request("nope") is None

    def test_duplicate_with_same_hash_returns_existing(self, store):
        store.get_or_create_binding(CONV, HERMES_SID, "acc-1", DEVICE)
        first = store.create_message_request(
            CLIENT_MSG, CONV, DEVICE, "hello", request_sha256("hello", None),
        )
        replay = store.create_message_request(
            CLIENT_MSG, CONV, DEVICE, "hello", request_sha256("hello", None),
        )
        assert replay == first
        assert store.get_message_request(CLIENT_MSG) == first

    def test_duplicate_with_different_hash_raises_conflict(self, store):
        store.get_or_create_binding(CONV, HERMES_SID, "acc-1", DEVICE)
        store.create_message_request(
            CLIENT_MSG, CONV, DEVICE, "hello", request_sha256("hello", None),
        )
        with pytest.raises(DuplicateConflictError) as exc:
            store.create_message_request(
                CLIENT_MSG, CONV, DEVICE, "goodbye", request_sha256("goodbye", None),
            )
        assert exc.value.client_message_id == CLIENT_MSG

    def test_accept_complete_lifecycle(self, store):
        store.get_or_create_binding(CONV, HERMES_SID, "acc-1", DEVICE)
        store.create_message_request(
            CLIENT_MSG, CONV, DEVICE, "hello", request_sha256("hello", None),
        )
        running = store.accept_message_request(CLIENT_MSG, JOB1)
        assert running["state"] == "running"
        assert running["jobId"] == JOB1
        assert store.get_message_request_by_job(JOB1) == running
        assert store.get_message_request_by_job("nope") is None

        done = store.complete_message_request(
            CLIENT_MSG,
            canonical_user_message_id="880e8400-e29b-41d4-a716-446655440003",
            terminal_message_id="990e8400-e29b-41d4-a716-446655440004",
        )
        assert done["state"] == "terminal"
        assert done["canonicalUserMessageId"] == "880e8400-e29b-41d4-a716-446655440003"
        assert done["terminalMessageId"] == "990e8400-e29b-41d4-a716-446655440004"

    def test_terminal_is_immutable(self, store):
        store.get_or_create_binding(CONV, HERMES_SID, "acc-1", DEVICE)
        store.create_message_request(
            CLIENT_MSG, CONV, DEVICE, "hello", request_sha256("hello", None),
        )
        store.accept_message_request(CLIENT_MSG, JOB1)
        store.complete_message_request(CLIENT_MSG)
        # No-op transitions return the row unchanged.
        assert store.complete_message_request(CLIENT_MSG)["state"] == "terminal"
        assert store.fail_message_request(CLIENT_MSG)["state"] == "terminal"
        assert store.cancel_message_request(CLIENT_MSG)["state"] == "terminal"
        assert store.accept_message_request(CLIENT_MSG, "new-job")["state"] == "terminal"

    def test_fail_and_cancel_transitions(self, store):
        store.get_or_create_binding(CONV, HERMES_SID, "acc-1", DEVICE)
        store.create_message_request(
            CLIENT_MSG, CONV, DEVICE, "hello", request_sha256("hello", None),
        )
        store.accept_message_request(CLIENT_MSG, JOB1)
        failed = store.fail_message_request(CLIENT_MSG, "upstream_interrupted")
        assert failed["state"] == "permanent_failure"
        assert failed["errorCategory"] == "upstream_interrupted"

        store.create_message_request(
            "550e8400-e29b-41d4-a716-446655440010", CONV, DEVICE,
            "two", request_sha256("two", None),
        )
        store.accept_message_request("550e8400-e29b-41d4-a716-446655440010", "job-2")
        cancelled = store.cancel_message_request("550e8400-e29b-41d4-a716-446655440010")
        assert cancelled["state"] == "cancelled"

    def test_failed_row_can_be_retried(self, store):
        """A transport retry of a failed send must be able to run again."""
        store.get_or_create_binding(CONV, HERMES_SID, "acc-1", DEVICE)
        store.create_message_request(
            CLIENT_MSG, CONV, DEVICE, "hello", request_sha256("hello", None),
        )
        store.accept_message_request(CLIENT_MSG, JOB1)
        store.fail_message_request(CLIENT_MSG, "timeout")
        retried = store.accept_message_request(CLIENT_MSG, "job-2")
        assert retried["state"] == "running"
        assert retried["jobId"] == "job-2"
        assert store.get_message_request_by_job(JOB1) is None


# ── Attachments + job events ───────────────────────────────────────────────


class TestAttachmentsAndEvents:
    def test_attachment_round_trip(self, store):
        store.add_attachment(
            "msg-1", 0, "att-1", "image", "a.png", "image/png",
            10, "h1", "/tmp/a.png", "/tmp/a_th.png",
        )
        store.add_attachment(
            "msg-1", 1, "att-2", "file", "b.txt", "text/plain",
            5, "h2", None, None,
        )
        atts = store.get_attachments("msg-1")
        assert len(atts) == 2
        assert [a["ordinal"] for a in atts] == [0, 1]
        assert atts[0]["attachment_id"] == "att-1"
        assert atts[0]["mime_type"] == "image/png"
        assert atts[0]["thumbnail_path"] == "/tmp/a_th.png"
        assert atts[1]["blob_path"] is None
        assert store.get_attachments("nope") == []

    def test_job_events_round_trip(self, store):
        store.append_job_event(JOB1, 0, json.dumps({"type": "text_delta", "data": {"delta": "hi"}}))
        store.append_job_event(JOB1, 1, json.dumps({"type": "done", "data": {}}))
        events = store.get_job_events(JOB1)
        assert [e["seq"] for e in events] == [0, 1]
        assert events[0]["event"]["type"] == "text_delta"
        assert events[0]["event"]["data"]["delta"] == "hi"
        assert events[1]["event"]["type"] == "done"
        assert all(_RFC3339_RE.match(e["createdAt"]) for e in events)
        assert store.get_job_events("nope") == []

    def test_job_events_survive_unparsable_json(self, store):
        store.append_job_event(JOB1, 0, "not json{{{")
        events = store.get_job_events(JOB1)
        assert events[0]["event"] == {"raw": "not json{{{"}


# ── Restart recovery ───────────────────────────────────────────────────────


class TestReconcile:
    def test_reconcile_stale_jobs(self, store):
        store.get_or_create_binding(CONV, HERMES_SID, "acc-1", DEVICE)
        store.create_message_request(
            CLIENT_MSG, CONV, DEVICE, "hello", request_sha256("hello", None),
        )
        store.accept_message_request(CLIENT_MSG, JOB1)
        # New running row — untouched.
        store.create_message_request(
            "550e8400-e29b-41d4-a716-446655440010", CONV, DEVICE,
            "two", request_sha256("two", None),
        )
        store.accept_message_request("550e8400-e29b-41d4-a716-446655440010", "job-2")

        # Age the first row past the 10-minute staleness window.
        old = (datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(minutes=11)).strftime("%Y-%m-%dT%H:%M:%SZ")
        conn = store._connect()
        try:
            conn.execute(
                "UPDATE message_requests SET updated_at = ? "
                "WHERE client_message_id = ?",
                (old, CLIENT_MSG),
            )
            conn.commit()
        finally:
            conn.close()

        assert store.reconcile_stale_jobs() is None  # spec: returns None
        stale = store.get_message_request(CLIENT_MSG)
        assert stale["state"] == "permanent_failure"
        assert stale["errorCategory"] == "connector_restart"
        fresh = store.get_message_request("550e8400-e29b-41d4-a716-446655440010")
        assert fresh["state"] == "running"
        # Terminal rows are untouched by design.
        assert store.reconcile_stale_jobs() is None

    def test_db_mode_0600_and_wal(self, store):
        assert stat.S_IMODE(store.db_path.stat().st_mode) == 0o600
        conn = store._connect()
        try:
            assert conn.execute("PRAGMA journal_mode").fetchone()[0].lower() == "wal"
            assert conn.execute("PRAGMA foreign_keys").fetchone()[0] == 1
        finally:
            conn.close()

    def test_singleton_tracks_env(self, tmp_path, monkeypatch):
        monkeypatch.setenv("HERMES_MOBILE_CONNECTOR_HOME", str(tmp_path))
        first = get_delivery_store()
        monkeypatch.setenv("HERMES_MOBILE_CONNECTOR_HOME", str(tmp_path / "other"))
        second = get_delivery_store()
        assert first is not second
        assert second.db_path == tmp_path / "other" / "delivery.sqlite3"


# ── Sidecar migration ──────────────────────────────────────────────────────


class TestMigration:
    def _sidecar(self, tmp_path: Path) -> Path:
        return tmp_path / "session_meta.json"

    def test_migrates_valid_bindings_and_renames_sidecar(self, store, tmp_path):
        sidecar = self._sidecar(tmp_path)
        canonical = str(uuid.uuid5(uuid.NAMESPACE_DNS, HERMES_SID))
        sidecar.write_text(json.dumps({
            CONV: {"_hermes_id": HERMES_SID, "title": "Existing chat"},
            canonical: {"_hermes_id": HERMES_SID},  # same session, canonical key wins
            "not-a-uuid": {"_hermes_id": "run_skip_me"},
            "550e8400-e29b-41d4-a716-446655440000": {"title": "no binding"},
            "junk": "string entry",
        }))
        migrated = store.migrate_bindings_from_sidecar(sidecar_path=sidecar)

        # Canonical row only — the draft alias is the same session and the
        # hermes_session_id column is UNIQUE.
        assert migrated == 1
        binding = store.get_binding(canonical)
        assert binding is not None
        assert binding["hermesSessionId"] == HERMES_SID
        assert store.get_binding(CONV) is None
        assert store.get_binding("not-a-uuid") is None
        # The sidecar is renamed after a successful migration.
        assert not sidecar.exists()
        assert (tmp_path / "session_meta.json.migrated").exists()

    def test_migration_respects_existing_rows(self, store, tmp_path):
        store.get_or_create_binding(CONV, HERMES_SID, "acc-1", DEVICE)
        sidecar = self._sidecar(tmp_path)
        sidecar.write_text(json.dumps({
            CONV: {"_hermes_id": HERMES_SID},
        }))
        assert store.migrate_bindings_from_sidecar(sidecar_path=sidecar) == 1
        assert store.get_binding(CONV)["ownerDeviceId"] == DEVICE

    def test_migration_without_bindings_leaves_sidecar_alone(self, store, tmp_path):
        sidecar = self._sidecar(tmp_path)
        sidecar.write_text(json.dumps({
            "550e8400-e29b-41d4-a716-446655440000": {"title": "no binding"},
            "not-a-uuid": {"_hermes_id": "run_skip_me"},
        }))
        assert store.migrate_bindings_from_sidecar(sidecar_path=sidecar) == 0
        assert sidecar.exists()
        assert not (tmp_path / "session_meta.json.migrated").exists()

    def test_migration_missing_or_unreadable_sidecar(self, store, tmp_path):
        assert store.migrate_bindings_from_sidecar(
            sidecar_path=tmp_path / "nope.json"
        ) == 0
        bad = tmp_path / "bad.json"
        bad.write_text("{not json")
        assert store.migrate_bindings_from_sidecar(sidecar_path=bad) == 0


# ── POST /v1/messages idempotency ──────────────────────────────────────────


class TestSendMessageIdempotency:
    def test_duplicate_send_returns_existing_job(self, app_env, client):
        store = get_delivery_store()
        store.get_or_create_binding(CONV, HERMES_SID, "", DEVICE)
        store.create_message_request(
            CLIENT_MSG, CONV, DEVICE, "What is the weather?",
            request_sha256("What is the weather?", None),
        )
        store.accept_message_request(CLIENT_MSG, JOB1)

        resp = client.post(
            "/v1/messages",
            json=post_message(client, conversationId=CONV),
        )
        assert resp.status_code == 200, resp.text
        data = resp.json()  # no envelope middleware on the module-level app
        assert data["replyState"] == "duplicate"
        assert data["clientMessageId"] == CLIENT_MSG
        assert data["jobId"] == JOB1
        assert data["existingState"] == "running"
        assert data["state"] == "accepted"
        assert data["conversation"]["id"] == CONV
        assert data["userMessage"] is None
        # No second job was created.
        assert len(facade._http_jobs) == 0

    def test_duplicate_terminal_send_returns_existing_job(self, app_env, client):
        store = get_delivery_store()
        store.get_or_create_binding(CONV, HERMES_SID, "", DEVICE)
        store.create_message_request(
            CLIENT_MSG, CONV, DEVICE, "What is the weather?",
            request_sha256("What is the weather?", None),
        )
        store.accept_message_request(CLIENT_MSG, JOB1)
        store.complete_message_request(CLIENT_MSG)

        resp = client.post(
            "/v1/messages",
            json=post_message(client, conversationId=CONV),
        )
        assert resp.status_code == 200, resp.text
        data = resp.json()
        assert data["replyState"] == "duplicate"
        assert data["jobId"] == JOB1
        assert data["existingState"] == "terminal"

    def test_conflicting_send_returns_409(self, app_env, client):
        store = get_delivery_store()
        store.get_or_create_binding(CONV, HERMES_SID, "", DEVICE)
        store.create_message_request(
            CLIENT_MSG, CONV, DEVICE, "What is the weather?",
            request_sha256("What is the weather?", None),
        )
        store.accept_message_request(CLIENT_MSG, JOB1)

        resp = client.post(
            "/v1/messages",
            json=post_message(client, text="A totally different question"),
        )
        assert resp.status_code == 409, resp.text
        body = resp.json()  # contains "error" → passes through the middleware
        assert body["replyState"] == "conflict"
        assert body["clientMessageId"] == CLIENT_MSG
        assert body["jobId"] is None
        assert body["error"] == "sameClientIdDifferentHash"
        assert len(facade._http_jobs) == 0

    def test_happy_path_lifecycle_through_handler(self, app_env, client, no_session_lookups):
        app_env.message_handler = completed_handler
        get_delivery_store().get_or_create_binding(CONV, HERMES_SID, "", DEVICE)

        resp = client.post(
            "/v1/messages",
            json=post_message(client, conversationId=CONV),
        )
        assert resp.status_code == 200, resp.text
        data = resp.json()  # no envelope middleware on the module-level app
        assert data["replyState"] == "pending"
        job_id = data["jobId"]
        assert job_id

        # The request was accepted + assigned the job synchronously; the
        # background handler run then moves it to terminal.  Poll for the
        # durable outcome.
        store = get_delivery_store()
        deadline = time.monotonic() + 10.0
        row = None
        while time.monotonic() < deadline:
            row = store.get_message_request_by_job(job_id)
            if row is not None and row["state"] == "terminal":
                break
            time.sleep(0.02)
        assert row is not None, "request row never created"
        assert row["state"] == "terminal"
        assert row["clientMessageId"] == CLIENT_MSG
        assert row["conversationId"] == CONV
        assert row["installationId"] == DEVICE or row["installationId"] == ""
        assert row["errorCategory"] is None

    def test_failed_send_marks_request_permanent_failure(self, app_env, client, no_session_lookups):
        async def failing_handler(text, history, session_id, attachments, reasoning_effort):  # noqa: ANN001, ARG001
            yield {"type": "done", "data": {
                "status": "failed", "error": "boom",
                "errorCategory": "upstream_interrupted", "errorAction": "retry",
            }}

        app_env.message_handler = failing_handler
        get_delivery_store().get_or_create_binding(CONV, HERMES_SID, "", DEVICE)
        resp = client.post(
            "/v1/messages",
            json=post_message(client, conversationId=CONV),
        )
        assert resp.status_code == 200, resp.text
        job_id = resp.json()["jobId"]

        store = get_delivery_store()
        deadline = time.monotonic() + 10.0
        row = None
        while time.monotonic() < deadline:
            row = store.get_message_request_by_job(job_id)
            if row is not None and row["state"] == "permanent_failure":
                break
            time.sleep(0.02)
        assert row is not None
        assert row["state"] == "permanent_failure"
        assert row["errorCategory"] == "upstream_interrupted"

    def test_unbound_conversation_send_auto_binds_and_succeeds(self, app_env, client, no_session_lookups):
        """Build 118 Hotfix Bug 2: a send for an unbound conversation with no
        real Hermes session_id now auto-binds via _resolve_hermes_id/_app_uuid
        fallback instead of returning 409. This lets follow-up sends work
        without requiring a separate POST /v1/conversations/ensure first.

        Replaces the legacy test_unbound_conversation_send_returns_typed_error
        which expected a 409 — that blocking path caused the "no follow-up
        reply" regression on Build 118.
        """
        app_env.message_handler = completed_handler
        resp = client.post(
            "/v1/messages",
            json=post_message(client, conversationId=CONV),
        )
        assert resp.status_code == 200, resp.text
        data = resp.json()
        assert data["replyState"] == "pending"
        assert data["jobId"] is not None
        # A binding was auto-created for the unbound conversation.
        binding = get_delivery_store().get_binding(CONV)
        assert binding is not None

    def test_job_status_durable_fallback_after_restart(self, app_env, client):
        """A job accepted by a previous connector process still answers polls."""
        store = get_delivery_store()
        store.get_or_create_binding(CONV, HERMES_SID, "", DEVICE)
        store.create_message_request(
            CLIENT_MSG, CONV, DEVICE, "What is the weather?",
            request_sha256("What is the weather?", None),
        )
        store.accept_message_request(CLIENT_MSG, JOB1)

        resp = client.get(f"/v1/jobs/{JOB1}")
        assert resp.status_code == 200, resp.text
        data = resp.json()["data"]
        assert data["jobId"] == JOB1
        assert data["status"] == "running"
        assert data["conversationId"] == CONV

        store.complete_message_request(CLIENT_MSG)
        resp = client.get(f"/v1/jobs/{JOB1}")
        assert resp.json()["data"]["status"] == "completed"

    def test_job_status_unknown_job_keeps_legacy_fallback(self, app_env, client):
        resp = client.get("/v1/jobs/550e8400-e29b-41d4-a716-446655440000")
        # Legacy path without a job service → 503.
        assert resp.status_code == 503
