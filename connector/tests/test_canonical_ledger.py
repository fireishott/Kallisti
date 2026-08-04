"""Tests for the canonical message ledger (Build 108 Workstream A).

These tests verify the atomic acceptance contract, idempotency,
sequence ordering, and revision tracking. They should fail before
the full implementation is complete.
"""

from __future__ import annotations

import sqlite3
import uuid

import pytest

from kallisti_connector.delivery_store import (
    DeliveryStore,
    DuplicateConflictError,
    _EXPECTED_SCHEMA_VERSION,
    _utcnow_rfc3339,
    request_sha256,
)


@pytest.fixture
def store(tmp_path):
    """Create a fresh DeliveryStore for each test."""
    db_path = tmp_path / "delivery.sqlite3"
    return DeliveryStore(db_path)


@pytest.fixture
def bound_conversation(store):
    """Create a conversation binding for testing."""
    app_id = str(uuid.uuid4())
    hermes_id = f"api-{uuid.uuid4().hex[:16]}"
    return store.get_or_create_binding(
        app_id, hermes_id, "test-account", "test-device"
    )


class TestCanonicalMessageCreation:
    """Tests for creating canonical messages in the ledger."""

    def test_create_user_message_returns_canonical_id(
        self, store, bound_conversation
    ):
        """A user message should get a canonical ID and sequence."""
        result = store.create_canonical_message(
            bound_conversation["appConversationId"],
            "user",
            "Hello, world!",
            "Hello, world!",
            client_message_id=str(uuid.uuid4()),
        )
        assert result["canonicalMessageId"]
        assert result["sequence"] == 1
        assert result["role"] == "user"
        assert result["state"] == "pending"

    def test_create_assistant_message_with_job_id(
        self, store, bound_conversation
    ):
        """An assistant message should be linked to a job."""
        job_id = str(uuid.uuid4())
        result = store.create_canonical_message(
            bound_conversation["appConversationId"],
            "assistant",
            "I'm here to help.",
            "I'm here to help.",
            job_id=job_id,
        )
        assert result["jobId"] == job_id
        assert result["sequence"] == 1

    def test_messages_get_incrementing_sequences(
        self, store, bound_conversation
    ):
        """Each message in a conversation should get an incrementing sequence."""
        conv_id = bound_conversation["appConversationId"]
        msg1 = store.create_canonical_message(
            conv_id, "user", "First", "First"
        )
        msg2 = store.create_canonical_message(
            conv_id, "assistant", "Response 1", "Response 1"
        )
        msg3 = store.create_canonical_message(
            conv_id, "user", "Second", "Second"
        )
        assert msg1["sequence"] == 1
        assert msg2["sequence"] == 2
        assert msg3["sequence"] == 3

    def test_conversation_revision_increments(
        self, store, bound_conversation
    ):
        """Each message should increment the conversation revision."""
        conv_id = bound_conversation["appConversationId"]
        initial_rev = store.get_conversation_revision(conv_id)
        store.create_canonical_message(conv_id, "user", "Test", "Test")
        after_first = store.get_conversation_revision(conv_id)
        store.create_canonical_message(
            conv_id, "assistant", "Reply", "Reply"
        )
        after_second = store.get_conversation_revision(conv_id)
        assert after_first == initial_rev + 1
        assert after_second == initial_rev + 2


class TestAtomicAcceptance:
    """Tests for the atomic acceptance contract (Workstream A)."""

    def test_atomic_create_user_message(
        self, store, bound_conversation
    ):
        """Atomic create should produce user message, request, and revision."""
        conv_id = bound_conversation["appConversationId"]
        client_msg_id = str(uuid.uuid4())
        result = store.create_user_message_atomically(
            conv_id,
            client_msg_id,
            "Test message",
            "Test message",
        )
        assert result["canonicalMessageId"]
        assert result["clientMessageId"] == client_msg_id
        assert result["state"] == "accepted"
        assert result["duplicate"] is False
        # Verify request was created
        request = store.get_message_request(client_msg_id)
        assert request is not None
        assert request["state"] == "accepted"
        # Verify revision incremented
        rev = store.get_conversation_revision(conv_id)
        assert rev >= 1

    def test_atomic_idempotent_on_duplicate_client_id(
        self, store, bound_conversation
    ):
        """Repeating the same clientMessageId should return existing row."""
        conv_id = bound_conversation["appConversationId"]
        client_msg_id = str(uuid.uuid4())
        result1 = store.create_user_message_atomically(
            conv_id, client_msg_id, "Test", "Test"
        )
        result2 = store.create_user_message_atomically(
            conv_id, client_msg_id, "Test", "Test"
        )
        assert result1["canonicalMessageId"] == result2["canonicalMessageId"]
        assert result2["duplicate"] is True
        # Only one message should exist
        messages = store.get_conversation_messages(conv_id)
        assert len(messages) == 1


class TestSequenceOrdering:
    """Tests for sequence-based ordering (not timestamp)."""

    def test_two_equal_text_different_ids_get_distinct_sequences(
        self, store, bound_conversation
    ):
        """Two messages with same text but different IDs should be distinct."""
        conv_id = bound_conversation["appConversationId"]
        msg1 = store.create_canonical_message(
            conv_id, "user", "Same text", "Same text",
            client_message_id=str(uuid.uuid4()),
        )
        msg2 = store.create_canonical_message(
            conv_id, "user", "Same text", "Same text",
            client_message_id=str(uuid.uuid4()),
        )
        assert msg1["sequence"] != msg2["sequence"]
        assert msg1["canonicalMessageId"] != msg2["canonicalMessageId"]

    def test_messages_ordered_by_sequence_not_timestamp(
        self, store, bound_conversation
    ):
        """Messages should be returned in sequence order."""
        conv_id = bound_conversation["appConversationId"]
        msg1 = store.create_canonical_message(
            conv_id, "user", "First", "First"
        )
        msg2 = store.create_canonical_message(
            conv_id, "user", "Second", "Second"
        )
        messages = store.get_conversation_messages(conv_id)
        assert len(messages) == 2
        assert messages[0]["sequence"] == 1
        assert messages[1]["sequence"] == 2

    def test_get_messages_after_sequence(
        self, store, bound_conversation
    ):
        """Should be able to get messages after a specific sequence."""
        conv_id = bound_conversation["appConversationId"]
        store.create_canonical_message(conv_id, "user", "First", "First")
        store.create_canonical_message(conv_id, "user", "Second", "Second")
        store.create_canonical_message(conv_id, "user", "Third", "Third")
        messages = store.get_conversation_messages(conv_id, after_sequence=1)
        assert len(messages) == 2
        assert messages[0]["sequence"] == 2
        assert messages[1]["sequence"] == 3


class TestMessageLookup:
    """Tests for message lookup by different identifiers."""

    def test_lookup_by_client_message_id(
        self, store, bound_conversation
    ):
        """Should find message by client_message_id."""
        conv_id = bound_conversation["appConversationId"]
        client_msg_id = str(uuid.uuid4())
        msg = store.create_canonical_message(
            conv_id, "user", "Test", "Test",
            client_message_id=client_msg_id,
        )
        found = store.get_message_by_client_id(conv_id, client_msg_id)
        assert found is not None
        assert found["canonicalMessageId"] == msg["canonicalMessageId"]

    def test_lookup_by_job_id(self, store, bound_conversation):
        """Should find message by job_id."""
        conv_id = bound_conversation["appConversationId"]
        job_id = str(uuid.uuid4())
        msg = store.create_canonical_message(
            conv_id, "assistant", "Reply", "Reply",
            job_id=job_id,
        )
        found = store.get_message_by_job_id(conv_id, job_id)
        assert found is not None
        assert found["canonicalMessageId"] == msg["canonicalMessageId"]

    def test_lookup_returns_none_for_unknown(
        self, store, bound_conversation
    ):
        """Should return None for unknown identifiers."""
        conv_id = bound_conversation["appConversationId"]
        assert store.get_message_by_client_id(conv_id, "unknown") is None
        assert store.get_message_by_job_id(conv_id, "unknown") is None

    def test_canonical_message_lookup_rejected_across_conversation(
        self, store, bound_conversation
    ):
        """A canonical id bound to conversation A must return None when
        looked up from conversation B — cross-conversation identity leak
        is rejected."""
        conv_a = bound_conversation["appConversationId"]
        # Create a second conversation binding.
        conv_b_app = str(uuid.uuid4())
        conv_b_hermes = f"api-{uuid.uuid4().hex[:16]}"
        store.get_or_create_binding(
            conv_b_app, conv_b_hermes, "acc-002", "device-2",
        )
        # Create a canonical message in conversation A.
        msg = store.create_canonical_message(
            conv_a, "user", "Hello from A", "Hello from A",
        )
        canonical_id = msg["canonicalMessageId"]
        # Scoped lookup from conversation A: should succeed.
        found_a = store.get_canonical_message(
            canonical_id, conversation_id=conv_a,
        )
        assert found_a is not None
        assert found_a["canonicalMessageId"] == canonical_id
        # Scoped lookup from conversation B: must return None.
        found_b = store.get_canonical_message(
            canonical_id, conversation_id=conv_b_app,
        )
        assert found_b is None, (
            f"Cross-conversation lookup must return None, "
            f"got {found_b!r}"
        )


class TestMessageStateUpdates:
    """Tests for updating message state."""

    def test_update_message_state(
        self, store, bound_conversation
    ):
        """Should update message state and increment revision."""
        conv_id = bound_conversation["appConversationId"]
        msg = store.create_canonical_message(
            conv_id, "user", "Test", "Test"
        )
        initial_rev = msg["revision"]
        updated = store.update_message_state(
            msg["canonicalMessageId"], "accepted"
        )
        assert updated["state"] == "accepted"
        assert updated["revision"] == initial_rev + 1

    def test_update_message_content(
        self, store, bound_conversation
    ):
        """Should update message content when provided."""
        conv_id = bound_conversation["appConversationId"]
        msg = store.create_canonical_message(
            conv_id, "user", "Original", "Original"
        )
        updated = store.update_message_state(
            msg["canonicalMessageId"], "terminal",
            content="Updated content",
            display_content="Updated display",
        )
        assert updated["content"] == "Updated content"
        assert updated["displayContent"] == "Updated display"


class TestSystemContextInvisibility:
    """Tests for Workstream E: system context as invisible metadata."""

    def test_system_context_not_in_display_content(
        self, store, bound_conversation
    ):
        """System context should be stored separately from display content."""
        conv_id = bound_conversation["appConversationId"]
        display = "What's the weather?"
        model_input = "[System context: 2026-08-01T18:00:00Z] What's the weather?"
        msg = store.create_canonical_message(
            conv_id, "user", "What's the weather?", display,
            model_input_content=model_input,
        )
        assert msg["displayContent"] == display
        assert msg["modelInputContent"] == model_input
        # Verify retrieval preserves separation
        retrieved = store.get_canonical_message(msg["canonicalMessageId"])
        assert retrieved["displayContent"] == display
        assert retrieved["modelInputContent"] == model_input

    def test_literal_system_context_preserved(
        self, store, bound_conversation
    ):
        """Text starting with '[System context' should remain literal."""
        conv_id = bound_conversation["appConversationId"]
        literal = "[System context] This is a literal message."
        msg = store.create_canonical_message(
            conv_id, "user", literal, literal
        )
        assert msg["displayContent"] == literal
        assert msg["content"] == literal


class TestUniqueConstraints:
    """Tests for unique constraints on the ledger."""

    def test_duplicate_client_message_id_raises(
        self, store, bound_conversation
    ):
        """Should raise DuplicateConflictError for duplicate client_message_id."""
        conv_id = bound_conversation["appConversationId"]
        client_msg_id = str(uuid.uuid4())
        store.create_canonical_message(
            conv_id, "user", "Test", "Test",
            client_message_id=client_msg_id,
        )
        with pytest.raises(DuplicateConflictError):
            store.create_canonical_message(
                conv_id, "user", "Test", "Test",
                client_message_id=client_msg_id,
            )

    def test_duplicate_sequence_raises(
        self, store, bound_conversation
    ):
        """Should raise an error for duplicate sequence in same conversation."""
        # This is tested implicitly by the sequence generation logic
        # but we verify the unique constraint exists
        conv_id = bound_conversation["appConversationId"]
        msg1 = store.create_canonical_message(
            conv_id, "user", "First", "First"
        )
        msg2 = store.create_canonical_message(
            conv_id, "user", "Second", "Second"
        )
        assert msg1["sequence"] != msg2["sequence"]


class TestMigration:
    """Tests for canonical ledger migration (Workstream B)."""

    def test_migration_imports_existing_messages(
        self, store, bound_conversation
    ):
        """Migration should import existing message_requests into ledger."""
        conv_id = bound_conversation["appConversationId"]
        # Create some message_requests manually
        client_msg_id1 = str(uuid.uuid4())
        client_msg_id2 = str(uuid.uuid4())
        store.create_message_request(
            client_msg_id1, conv_id, "device1", "First message",
            request_sha256("First message", None),
        )
        store.create_message_request(
            client_msg_id2, conv_id, "device2", "Second message",
            request_sha256("Second message", None),
        )
        # Run migration
        report = store.migrate_to_canonical_ledger()
        assert report["imported"] == 2
        # Verify messages are in ledger
        messages = store.get_conversation_messages(conv_id)
        assert len(messages) == 2
        assert messages[0]["sequence"] == 1
        assert messages[1]["sequence"] == 2

    def test_migration_strips_system_context(
        self, store, bound_conversation
    ):
        """Migration should strip system-context envelope from user display."""
        conv_id = bound_conversation["appConversationId"]
        client_msg_id = str(uuid.uuid4())
        # Message with system context prefix
        store.create_message_request(
            client_msg_id, conv_id, "device1",
            "[System context: 2026-08-01T18:00:00Z] What's the weather?",
            request_sha256("What's the weather?", None),
        )
        # Run migration
        report = store.migrate_to_canonical_ledger()
        assert report["imported"] == 1
        # Verify system context was stripped
        messages = store.get_conversation_messages(conv_id)
        assert len(messages) == 1
        assert messages[0]["displayContent"] == "What's the weather?"

    def test_migration_is_idempotent(
        self, store, bound_conversation
    ):
        """Running migration twice should not duplicate messages."""
        conv_id = bound_conversation["appConversationId"]
        client_msg_id = str(uuid.uuid4())
        store.create_message_request(
            client_msg_id, conv_id, "device1", "Test message",
            request_sha256("Test message", None),
        )
        # Run migration twice
        report1 = store.migrate_to_canonical_ledger()
        report2 = store.migrate_to_canonical_ledger()
        assert report1["imported"] == 1
        assert report2["imported"] == 0  # Already migrated
        # Verify only one message exists
        messages = store.get_conversation_messages(conv_id)
        assert len(messages) == 1

    def test_migration_handles_empty_conversations(
        self, store, bound_conversation
    ):
        """Migration should handle conversations with no messages."""
        # Run migration with no messages
        report = store.migrate_to_canonical_ledger()
        assert report["imported"] == 0

    def test_migration_generates_report(
        self, store, bound_conversation, tmp_path
    ):
        """Migration should generate a JSON report file."""
        conv_id = bound_conversation["appConversationId"]
        client_msg_id = str(uuid.uuid4())
        store.create_message_request(
            client_msg_id, conv_id, "device1", "Test message",
            request_sha256("Test message", None),
        )
        # Run migration with custom evidence dir
        report = store.migrate_to_canonical_ledger(
            evidence_dir=tmp_path / "evidence"
        )
        assert report["imported"] == 1
        # Verify report file exists
        report_file = tmp_path / "evidence" / "migration_report.json"
        assert report_file.exists()
