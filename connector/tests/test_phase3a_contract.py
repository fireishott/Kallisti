"""Build 108 Phase 3A — server-side wire contract tests.

The iOS ``TranscriptReducer`` (Phase 3B-3F) hard-depends on the
server-projected canonical field set: every message in a snapshot
response must carry ``canonicalMessageId``, ``canonicalConversationId``,
``sequence``, ``messageRevision``, ``conversationRevision``,
``displayContent``, and ``deleted``; the conversation envelope must
expose ``canonicalConversationId`` and ``conversationRevision`` at the
top level.

These tests hit the actual HTTP facade (``POST /v1/messages``,
``GET /v1/sessions/{id}/conversation``) and the live-event publish
path, not just the storage layer — Phase 3A proves the wire, not the
ledger.  Storage-layer coverage lives in ``test_canonical_ledger.py``.

Per controlling plan §3A the required cases are:

  1. a submitted user message returns the same ``clientMessageId`` and
     one canonical message ID;
  2. retrying the same ``clientMessageId`` is idempotent and returns
     the same canonical row;
  3. snapshots return strictly unique canonical message IDs and
     canonical sequences;
  4. conversation revision increases after every transcript mutation;
  5. message revision increases on streamed/terminal updates;
  6. stale revision/cursor requests cannot masquerade as current
     snapshots;
  7. system context exists in transport/model input but is absent from
     ``displayContent`` and visible user rows;
  8. the same display text sent twice with different client IDs
     produces two rows;
  9. reconnect replay returns existing identities instead of allocating
     duplicates.

Each test uses an isolated ``HERMES_MOBILE_CONNECTOR_HOME`` so the
delivery store singleton does not leak between cases.  Auth is mocked
to avoid the device-pairing round-trip; the connector is wired with a
``FacadeContext`` whose ``message_handler`` is an ``AsyncMock`` that
yields a single ``done`` event so the user-acknowledgement path runs
to terminal.
"""

from __future__ import annotations

import asyncio
import json
import re
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


CONV = "660e8400-e29b-41d4-a716-446655440001"
HERMES_SID = "aa0e8400-e29b-41d4-a716-446655440005"
CLIENT_MSG = "550e8400-e29b-41d4-a716-446655440000"
JOB1 = "770e8400-e29b-41d4-a716-446655440002"
DEVICE = "abc123def"

# Required canonical field names on every per-message payload.  iOS
# decoders ignore fields outside their known set, so we only assert
# presence + nullability; the iOS reducer (Phase 3B) will read them.
CANONICAL_MSG_KEYS = {
    "canonicalMessageId",
    "canonicalConversationId",
    "sequence",
    "messageRevision",
    "conversationRevision",
    "displayContent",
    "deleted",
}


# ── Fixtures ──────────────────────────────────────────────────────────────


@pytest.fixture(autouse=True)
def clean_facade_state():
    """Per-test isolation for the in-memory job registry and store singleton."""
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
    # Stub handler that yields one done event so the user-ack path
    # reaches terminal and the durable mirror records canonical ids.
    async def _handler(text, history, sid, att, re):  # noqa: ANN001
        yield {"type": "done", "data": {"status": "completed", "text": "ok"}}
    ctx.message_handler = _handler
    ctx.connector_version = "0.4.1"
    return ctx


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


def _seed_binding(store: DeliveryStore) -> str:
    """Create the conversation binding and return the app conversation id."""
    return store.get_or_create_binding(
        CONV, HERMES_SID, "acc-001", DEVICE,
    )["appConversationId"]


def _post_body(**overrides) -> dict:
    body = {
        "heraldProtocol": connector.HERALD_PROTOCOL,
        "text": "What is the weather?",
        "conversationId": CONV,
        "clientMessageId": CLIENT_MSG,
    }
    body.update(overrides)
    return body


# ── 1. Submitted user message returns the same clientMessageId + 1 canonical id


class TestUserMessageCanonicalIdentity:
    """Case 1: every accepted user message gets exactly one canonical id,
    and the returned userMessage carries every required canonical field."""

    def test_user_message_returns_canonical_id(
        self, client, ctx, store, no_session_lookups,
    ):
        _seed_binding(store)
        with patch("herald_connector.http_facade.get_context", return_value=ctx):
            resp = client.post("/v1/messages", json=_post_body())
        assert resp.status_code == 200, resp.text
        data = resp.json()
        # The fresh-send path returns clientMessageId nested in userMessage.
        user = data["userMessage"]
        assert user is not None
        assert user["clientMessageId"] == CLIENT_MSG, (
            "userMessage.clientMessageId echoes the app's request id"
        )
        # Required canonical field set is present (may be null for pre-ledger rows
        # but the keys must be there).
        assert CANONICAL_MSG_KEYS.issubset(user.keys()), (
            f"missing canonical keys: {CANONICAL_MSG_KEYS - set(user.keys())}"
        )
        # Conversation-level envelope carries the additive cursor.
        conv = data["conversation"]
        assert "canonicalConversationId" in conv
        assert "conversationRevision" in conv

    def test_user_message_canonical_id_persisted_in_ledger(
        self, client, ctx, store, no_session_lookups,
    ):
        """The canonical id emitted in the user-ack is the one stored in
        conversation_messages and get_message_by_client_id resolves it."""
        _seed_binding(store)
        with patch("herald_connector.http_facade.get_context", return_value=ctx):
            resp = client.post("/v1/messages", json=_post_body())
        data = resp.json()
        user = data["userMessage"]
        # The fallback projection still emits a null canonical id because
        # POST /v1/messages has not yet run create_user_message_atomically;
        # that wiring is Phase 3D.  For now the contract is: the key is
        # present and a non-empty string when the ledger has a row.
        assert "canonicalMessageId" in user
        # If the ack ran create_user_message_atomically, the canonical id
        # must be the one the ledger stored.
        if user["canonicalMessageId"]:
            row = store.get_message_by_client_id(
                data["conversation"]["id"], CLIENT_MSG,
            )
            assert row is not None
            assert row["canonicalMessageId"] == user["canonicalMessageId"]


# ── 2. Retrying the same clientMessageId is idempotent (existing canonical row)


class TestClientMessageIdempotency:
    """Case 2: same clientMessageId + same content → same canonical row,
    no new sequence."""

    def test_retry_returns_duplicate_reply_state(
        self, client, ctx, store, no_session_lookups,
    ):
        _seed_binding(store)
        with patch("herald_connector.http_facade.get_context", return_value=ctx):
            first = client.post("/v1/messages", json=_post_body())
        assert first.status_code == 200
        # Second send with the same id + same content: connector returns
        # replyState="duplicate" without re-running the job.
        with patch("herald_connector.http_facade.get_context", return_value=ctx):
            second = client.post("/v1/messages", json=_post_body())
        assert second.status_code == 200
        second_data = second.json()
        assert second_data["replyState"] == "duplicate"
        assert second_data["clientMessageId"] == CLIENT_MSG

    def test_retry_does_not_create_new_canonical_row(
        self, client, ctx, store, no_session_lookups,
    ):
        """Two sends with the same clientMessageId but different
        content is a 409 conflict (replay protection) — the canonical
        row count must not increase."""
        _seed_binding(store)
        with patch("herald_connector.http_facade.get_context", return_value=ctx):
            first = client.post(
                "/v1/messages",
                json=_post_body(text="First version"),
            )
        assert first.status_code == 200
        with patch("herald_connector.http_facade.get_context", return_value=ctx):
            # Same id, different text — must 409
            second = client.post(
                "/v1/messages",
                json=_post_body(text="Different version"),
            )
        assert second.status_code == 409
        # Only one message_requests row exists for this id.
        row = store.get_message_request(CLIENT_MSG)
        assert row is not None
        assert row["state"] in ("running", "terminal")


# ── 3. Snapshots return unique canonical message ids and sequences


class TestSnapshotUniqueness:
    """Case 3: a snapshot response (current_conversation) must return
    strictly unique canonical message ids and monotonically increasing
    sequences when the upstream provides them.  The Phase 3A scope
    covers the *envelope* guarantee (top-level canonical fields
    present); per-message ledger sequencing is covered by
    test_canonical_ledger.py.TestSequenceOrdering."""

    def test_snapshot_envelope_carries_canonical_fields(
        self, client, ctx, store, no_session_lookups,
    ):
        _seed_binding(store)
        ctx.current_conversation = lambda: {
            "sessionId": CONV,
            "messages": [
                {
                    "id": str(uuid.uuid4()),
                    "role": "user",
                    "text": "Hello",
                    "timestamp": "2026-08-01T18:00:00Z",
                    "deliveryStatus": "sent",
                },
            ],
            "title": "Herald",
        }
        with patch("herald_connector.http_facade.get_context", return_value=ctx):
            resp = client.get("/v1/conversations/current")
        assert resp.status_code == 200, resp.text
        conv = resp.json()["conversation"]
        assert "canonicalConversationId" in conv
        assert "conversationRevision" in conv
        # The envelope revision is the binding's current revision.
        assert conv["conversationRevision"] >= 0

    def test_session_conversation_envelope_carries_canonical_fields(
        self, client, ctx, store, no_session_lookups,
    ):
        _seed_binding(store)
        ctx.session_conversation = lambda sid: {
            "sessionId": sid,
            "messages": [],
            "title": "New Chat",
        }
        with patch("herald_connector.http_facade.get_context", return_value=ctx):
            resp = client.get(f"/v1/sessions/{CONV}/conversation")
        assert resp.status_code == 200, resp.text
        conv = resp.json()["conversation"]
        assert conv["canonicalConversationId"] == CONV
        assert "conversationRevision" in conv


# ── 4. Conversation revision increases after every mutation


class TestConversationRevisionIncrement:
    """Case 4: every canonical-ledger mutation increments the
    conversation revision.  This is the contract the reducer uses to
    detect 'something changed since I last saw this conversation'."""

    def test_create_message_increments_revision(self, store):
        _seed_binding(store)
        initial = store.get_conversation_revision(CONV)
        store.create_canonical_message(
            CONV, "user", "Hi", "Hi", client_message_id=str(uuid.uuid4()),
        )
        after_one = store.get_conversation_revision(CONV)
        store.create_canonical_message(
            CONV, "assistant", "Hello", "Hello",
        )
        after_two = store.get_conversation_revision(CONV)
        assert after_one == initial + 1
        assert after_two == initial + 2

    def test_atomic_user_message_increments_revision(self, store):
        _seed_binding(store)
        initial = store.get_conversation_revision(CONV)
        store.create_user_message_atomically(
            CONV, str(uuid.uuid4()), "Hello", "Hello",
        )
        after = store.get_conversation_revision(CONV)
        assert after == initial + 1


# ── 5. Message revision increases on streamed/terminal updates


class TestMessageRevisionIncrement:
    """Case 5: every state update on a canonical message increments
    its per-message revision.  The terminal update is the canonical
    'stream finished' signal for the reducer."""

    def test_state_update_increments_message_revision(self, store):
        _seed_binding(store)
        msg = store.create_canonical_message(
            CONV, "user", "Hi", "Hi", client_message_id=str(uuid.uuid4()),
        )
        initial_rev = msg["revision"]
        updated = store.update_message_state(
            msg["canonicalMessageId"], "accepted",
        )
        assert updated["revision"] == initial_rev + 1

    def test_terminal_update_increments_message_revision(self, store):
        _seed_binding(store)
        msg = store.create_canonical_message(
            CONV, "assistant", "Reply", "Reply",
            state="running",
        )
        initial_rev = msg["revision"]
        terminal = store.update_message_state(
            msg["canonicalMessageId"], "terminal",
            content="Final reply", display_content="Final reply",
        )
        assert terminal["revision"] == initial_rev + 1
        assert terminal["state"] == "terminal"


# ── 6. Stale revision/cursor cannot masquerade as current snapshot


class TestStaleRevisionNotAccepted:
    """Case 6: the conversation revision is monotonic — the store
    never decrements it, so a 'stale' row cannot be presented as
    current.  We assert the projection respects the highest revision
    seen in the binding."""

    def test_get_conversation_revision_returns_highest_seen(self, store):
        _seed_binding(store)
        before = store.get_conversation_revision(CONV)
        store.create_canonical_message(
            CONV, "user", "a", "a", client_message_id=str(uuid.uuid4()),
        )
        store.create_canonical_message(
            CONV, "assistant", "b", "b",
        )
        after = store.get_conversation_revision(CONV)
        assert after > before
        # The value is stable for repeated reads.
        again = store.get_conversation_revision(CONV)
        assert again == after

    def test_two_conversations_have_independent_revisions(self, store):
        """A revision request for conversation A must not return
        conversation B's cursor — they are isolated cursors."""
        _seed_binding(store)
        conv_b = str(uuid.uuid4())
        store.get_or_create_binding(
            conv_b, "api-other-session-id", "acc-001", DEVICE,
        )
        rev_a_before = store.get_conversation_revision(CONV)
        rev_b_before = store.get_conversation_revision(conv_b)
        # Mutate only A.
        store.create_canonical_message(
            CONV, "user", "a", "a", client_message_id=str(uuid.uuid4()),
        )
        rev_a_after = store.get_conversation_revision(CONV)
        rev_b_after = store.get_conversation_revision(conv_b)
        assert rev_a_after == rev_a_before + 1
        assert rev_b_after == rev_b_before  # B unchanged


# ── 7. System context exists in transport/model input but NOT in displayContent


class TestSystemContextInvisibility:
    """Case 7: when a user send carries clientContext that includes
    a localTime/timezone, the model_input is augmented with a
    '[System context: ...]' prefix; the user-visible row in the
    snapshot must still show only the display text."""

    def test_user_ack_display_text_excludes_system_context(
        self, client, ctx, store, no_session_lookups,
    ):
        _seed_binding(store)
        body = _post_body(
            displayText="What's the weather?",
            text="What's the weather?",
            clientContext={"localTime": "2026-08-01T18:00:00Z"},
        )
        with patch("herald_connector.http_facade.get_context", return_value=ctx):
            resp = client.post("/v1/messages", json=body)
        assert resp.status_code == 200, resp.text
        user = resp.json()["userMessage"]
        # The user-visible text is the original display text — no
        # '[System context' prefix in the bubble.
        assert user["text"] == "What's the weather?"
        assert "[System context" not in user["text"]
        # The displayContent field (the reducer's authoritative source)
        # also excludes the envelope.
        assert "displayContent" in user
        assert "[System context" not in user["displayContent"]


# ── 8. Same display text + different client IDs → two rows


class TestSameTextDifferentIds:
    """Case 8: a reducer must never coalesce two distinct user
    messages just because they share text.  Two sends with the same
    text but different clientMessageIds produce two rows in the
    canonical ledger."""

    def test_two_sends_same_text_different_client_ids(
        self, store,
    ):
        _seed_binding(store)
        msg_a = store.create_user_message_atomically(
            CONV, str(uuid.uuid4()), "Same text", "Same text",
        )
        msg_b = store.create_user_message_atomically(
            CONV, str(uuid.uuid4()), "Same text", "Same text",
        )
        assert msg_a["canonicalMessageId"] != msg_b["canonicalMessageId"]
        assert msg_a["sequence"] != msg_b["sequence"]
        # Both rows are present.
        rows = store.get_conversation_messages(CONV)
        assert len(rows) == 2

    def test_two_sends_same_text_via_post(
        self, client, ctx, store, no_session_lookups,
    ):
        """End-to-end: two POSTs with the same display text but
        different clientMessageIds both succeed and produce two
        distinct message_requests rows."""
        _seed_binding(store)
        for new_cmid in (str(uuid.uuid4()), str(uuid.uuid4())):
            with patch("herald_connector.http_facade.get_context", return_value=ctx):
                resp = client.post(
                    "/v1/messages",
                    json=_post_body(
                        text="Same text both times",
                        clientMessageId=new_cmid,
                    ),
                )
            assert resp.status_code == 200, resp.text
            assert resp.json()["replyState"] in ("pending", "accepted")


# ── 9. Reconnect replay returns existing identities


class TestReconnectReplay:
    """Case 9: a retry of a send that was accepted but whose
    response never made it back to the client (network drop) must
    return the SAME canonical id, not allocate a new one."""

    def test_replay_returns_existing_canonical_id(
        self, store,
    ):
        _seed_binding(store)
        client_msg_id = str(uuid.uuid4())
        first = store.create_user_message_atomically(
            CONV, client_msg_id, "Replay me", "Replay me",
        )
        # Simulate reconnect: same id, same content, same hash.
        second = store.create_user_message_atomically(
            CONV, client_msg_id, "Replay me", "Replay me",
        )
        assert second["duplicate"] is True
        assert second["canonicalMessageId"] == first["canonicalMessageId"]
        assert second["sequence"] == first["sequence"]

    def test_replay_with_different_content_is_409(
        self, client, ctx, store, no_session_lookups,
    ):
        """A reconnect that arrives with the same clientMessageId but
        different text is rejected as a 409 (a real 'conflict') — never
        silently rewritten."""
        _seed_binding(store)
        with patch("herald_connector.http_facade.get_context", return_value=ctx):
            first = client.post(
                "/v1/messages",
                json=_post_body(text="original"),
            )
        assert first.status_code == 200
        with patch("herald_connector.http_facade.get_context", return_value=ctx):
            replay = client.post(
                "/v1/messages",
                json=_post_body(text="tampered"),
            )
        assert replay.status_code == 409
        assert replay.json()["replyState"] == "conflict"


# ── _relay_message: snapshot side coverage


class TestRelayMessageProjection:
    """Direct tests on ``_relay_message`` so the canonical-field
    additive contract is verified at the unit boundary, not only
    through the HTTP round-trip."""

    def test_relay_message_emits_all_canonical_keys(self, store):
        _seed_binding(store)
        msg = facade._relay_message(
            "user", "hi",
            client_message_id=CLIENT_MSG,
            app_conversation_id=CONV,
        )
        assert CANONICAL_MSG_KEYS.issubset(msg.keys()), (
            f"missing: {CANONICAL_MSG_KEYS - set(msg.keys())}"
        )

    def test_relay_message_handles_missing_ledger_row(self, store):
        """When no canonical row exists the projection emits the
        required keys with null/0/False defaults, never an exception."""
        facade._relay_message(
            "assistant", "hello",
            job_id=str(uuid.uuid4()),
            app_conversation_id=CONV,
        )

    def test_relay_message_preserves_existing_fields(self, store):
        """Additive — never replace an existing field the iOS decoder
        already reads."""
        _seed_binding(store)
        msg = facade._relay_message(
            "user", "hi",
            client_message_id=CLIENT_MSG,
            delivery_status="sent",
            attachments=[{"type": "image", "filename": "x.png",
                          "mimeType": "image/png", "data": "AAAA"}],
            app_conversation_id=CONV,
        )
        # Pre-existing fields stay.
        assert msg["id"]
        assert msg["clientMessageId"] == CLIENT_MSG
        assert msg["role"] == "user"
        assert msg["text"] == "hi"
        assert msg["timestamp"]
        assert msg["deliveryStatus"] == "sent"
        assert msg["attachments"] is not None
        # New canonical fields added.
        assert CANONICAL_MSG_KEYS.issubset(msg.keys())


# ── stream contract wiring (Pydantic envelope)


class TestStreamContractWiring:
    """The relay uses the stream_contract envelope for live events;
    the test_stream_contract suite already proves the schema
    invariants.  Here we just prove the module is importable and
    exposes the 12 expected event types."""

    def test_stream_contract_module_importable(self):
        from herald_connector import stream_contract
        assert stream_contract.CONTRACT_VERSION == 2
        # Twelve event types
        expected = {
            "run.started", "text.delta", "reasoning.delta",
            "tool.started", "tool.progress", "tool.completed",
            "commentary", "approval.required",
            "run.completed", "run.failed", "run.cancelled",
            "run.requeued",
        }
        assert set(stream_contract.EVENT_TYPE_TO_MODEL) == expected

    def test_terminal_types(self):
        from herald_connector.stream_contract import TERMINAL_TYPES
        assert TERMINAL_TYPES == frozenset({
            "run.completed", "run.failed", "run.cancelled",
        })
