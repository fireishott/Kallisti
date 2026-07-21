from __future__ import annotations

from datetime import timedelta

from sqlalchemy import select

from app.config import Settings
from app.database import Database
from app.models import AuditLog, AuthSession, HeraldHost, InboxItem, PushRegistration, User, VoiceTurn, utcnow
from app.services import (
    append_message,
    create_voice_session,
    create_inbox_item,
    derive_title_from_message,
    ensure_default_user,
    get_or_create_current_conversation,
    inject_voice_transcript,
    list_inbox_items,
    list_conversation_messages,
    record_audit,
    record_voice_turn,
    refresh_auth_session,
    rename_session,
    rotate_auth_session,
    upsert_device,
    upsert_push_registration,
)


def make_database(tmp_path):
    settings = Settings(
        environment="test",
        public_base_url="http://testserver/v1",
        database_url=f"sqlite:///{tmp_path / 'relay-storage.db'}",
        internal_api_key="test-internal-key",
    )
    database = Database(settings.database_url)
    database.create_all()
    return settings, database


def test_sqlite_database_uses_production_pragmas(tmp_path):
    _, database = make_database(tmp_path)

    with database.engine.connect() as connection:
        journal_mode = connection.exec_driver_sql("PRAGMA journal_mode").scalar()
        busy_timeout = connection.exec_driver_sql("PRAGMA busy_timeout").scalar()
        foreign_keys = connection.exec_driver_sql("PRAGMA foreign_keys").scalar()

    assert journal_mode == "wal"
    assert busy_timeout >= 5000
    assert foreign_keys == 1


def test_device_upsert_token_rotation_and_audit(tmp_path):
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        device = upsert_device(
            db,
            user=user,
            platform="ios",
            installation_id="install-1",
            device_name="Phone",
            device_model="iPhone",
            system_version="26.4",
            app_version="1.0.0",
            build_number="1",
            bundle_id="net.fihonline.herald",
            environment="development",
        )
        auth_session, access_token, refresh_token = rotate_auth_session(db, settings=settings, user=user, device=device)
        refreshed_session, new_access_token, _ = refresh_auth_session(db, settings=settings, refresh_token=refresh_token)

        record_audit(
            db,
            actor_type="app",
            actor_id=device.id,
            action="device.register",
            entity_type="device",
            entity_id=device.id,
        )
        db.commit()

        assert access_token != new_access_token
        assert auth_session.id == refreshed_session.id
        assert db.scalar(select(AuthSession).where(AuthSession.device_id == device.id)) is not None
        assert db.scalar(select(AuditLog).where(AuditLog.entity_id == device.id)) is not None


def test_push_registration_and_inbox_state_transition(tmp_path):
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        device = upsert_device(
            db,
            user=user,
            platform="ios",
            installation_id="install-2",
            device_name="Phone",
            device_model="iPhone",
            system_version="26.4",
            app_version="1.0.0",
            build_number="1",
            bundle_id="net.fihonline.herald",
            environment="development",
        )
        registration = upsert_push_registration(
            db,
            device=device,
            apns_token="deadbeef",
            push_environment="sandbox",
            bundle_id="net.fihonline.herald",
        )
        item = create_inbox_item(
            db,
            user_id=user.id,
            device_id=device.id,
            kind="approval",
            title="Approve calendar change",
            body="Move dinner to 7 PM.",
            priority="normal",
            payload={"requestId": "calendar-1"},
            expires_at=None,
        )
        db.commit()

        assert db.scalar(select(PushRegistration).where(PushRegistration.id == registration.id)) is not None
        assert db.scalar(select(InboxItem).where(InboxItem.id == item.id)).status == "pending"
        assert len(list_inbox_items(db, user_id=user.id)) == 1
        assert db.scalar(select(User).where(User.id == user.id)) is not None


def test_inject_voice_transcript_appends_after_existing_chat_messages(tmp_path):
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        conversation = get_or_create_current_conversation(db, user_id=user.id)

        append_message(
            db,
            conversation=conversation,
            user_id=user.id,
            role="user",
            text="Existing chat message",
            delivery_status="delivered",
        )

        host = HeraldHost(
            id="host-123",
            user_id=user.id,
            display_name="Test Host",
            connector_token_hash="connector-token-hash",
        )
        db.add(host)
        db.commit()
        voice_session, _ = create_voice_session(db, user_id=user.id, host_id=host.id)
        old_turn_time = utcnow() - timedelta(minutes=5)

        first_turn = record_voice_turn(
            db,
            voice_session_id=voice_session.id,
            role="user",
            source="tool",
            text="Voice user turn",
        )
        second_turn = record_voice_turn(
            db,
            voice_session_id=voice_session.id,
            role="assistant",
            source="tool",
            text="Voice assistant turn",
        )

        first_turn.created_at = old_turn_time
        second_turn.created_at = old_turn_time + timedelta(seconds=1)
        db.commit()

        inject_voice_transcript(db, voice_session_id=voice_session.id, user_id=user.id)
        messages = list_conversation_messages(db, conversation_id=conversation.id)

        assert [message.text for message in messages] == [
            "Existing chat message",
            "Voice user turn",
            "Voice assistant turn",
            "[Voice session ended]",
        ]


def test_derive_title_from_message_collapses_whitespace_and_truncates():
    assert derive_title_from_message("hello   world") == "hello world"
    assert derive_title_from_message("line one\nline two\n\tline three") == "line one line two line three"

    long_text = "a" * 50
    derived = derive_title_from_message(long_text)
    assert derived == ("a" * 40) + "…"
    assert len(derived) == 41

    assert derive_title_from_message("short") == "short"


def test_first_user_message_updates_default_conversation_title(tmp_path):
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        conversation = get_or_create_current_conversation(db, user_id=user.id)
        assert conversation.title == "Herald"

        append_message(
            db,
            conversation=conversation,
            user_id=user.id,
            role="user",
            text="What's the weather like tomorrow?",
            delivery_status="sent",
        )

        assert conversation.title == "What's the weather like tomorrow?"


def test_first_user_message_title_is_truncated_when_long(tmp_path):
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        conversation = get_or_create_current_conversation(db, user_id=user.id)

        long_text = "Please   help me\nfigure out how to plan a full week-long itinerary for Japan"
        append_message(
            db,
            conversation=conversation,
            user_id=user.id,
            role="user",
            text=long_text,
            delivery_status="sent",
        )

        expected = derive_title_from_message(long_text)
        assert conversation.title == expected
        assert conversation.title.endswith("…")
        assert len(conversation.title) <= 41


def test_subsequent_user_messages_do_not_overwrite_derived_title(tmp_path):
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        conversation = get_or_create_current_conversation(db, user_id=user.id)

        append_message(
            db,
            conversation=conversation,
            user_id=user.id,
            role="user",
            text="First message sets the title",
            delivery_status="sent",
        )
        first_title = conversation.title
        assert first_title == "First message sets the title"

        append_message(
            db,
            conversation=conversation,
            user_id=user.id,
            role="user",
            text="A completely different second message",
            delivery_status="sent",
        )

        assert conversation.title == first_title


def test_manually_renamed_conversation_is_not_overwritten(tmp_path):
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        conversation = get_or_create_current_conversation(db, user_id=user.id)

        renamed = rename_session(db, session_id=conversation.id, title="My custom title")
        assert renamed is not None
        assert renamed.title == "My custom title"

        append_message(
            db,
            conversation=conversation,
            user_id=user.id,
            role="user",
            text="This should not change the title",
            delivery_status="sent",
        )

        assert conversation.title == "My custom title"


def test_new_chat_title_derives_from_first_message(tmp_path):
    """iOS-created sessions start with title 'New Chat' — the relay must derive."""
    from app.services import create_session

    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        device = upsert_device(
            db,
            user=user,
            platform="ios",
            installation_id="install-new-chat",
            device_name="Phone",
            device_model="iPhone",
            system_version="26.4",
            app_version="1.7.3",
            build_number="39",
            bundle_id="net.fihonline.herald",
            environment="development",
        )
        conversation = create_session(db, user_id=user.id, device_id=device.id)
        assert conversation.title == "New Chat"

        append_message(
            db,
            conversation=conversation,
            user_id=user.id,
            role="user",
            text="Explain quantum computing",
            delivery_status="sent",
        )

        assert conversation.title == "Explain quantum computing"


def test_herald_title_derives_from_first_message(tmp_path):
    """Relay-created sessions start with title 'Herald' — must also derive."""
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        conversation = get_or_create_current_conversation(db, user_id=user.id)
        assert conversation.title == "Herald"

        append_message(
            db,
            conversation=conversation,
            user_id=user.id,
            role="user",
            text="What is Rust?",
            delivery_status="sent",
        )

        assert conversation.title == "What is Rust?"


def test_orphaned_job_with_no_host_is_marked_failed(tmp_path):
    from app.models import MessageJob
    from app.services import log_stale_queued_jobs

    settings = Settings(
        environment="test",
        public_base_url="http://testserver/v1",
        database_url=f"sqlite:///{tmp_path / 'orphan-test.db'}",
        internal_api_key="test-internal-key",
        orphaned_job_expiry_seconds=60,
        stale_job_warning_seconds=30,
    )
    database = Database(settings.database_url)
    database.create_all()

    with database.session() as db:
        user = ensure_default_user(db, settings)
        conversation = get_or_create_current_conversation(db, user_id=user.id)

        # Create a message first
        user_message = append_message(
            db,
            conversation=conversation,
            user_id=user.id,
            role="user",
            text="Hello",
            delivery_status="sent",
        )

        # Create a job with no host (host_id=None)
        job = MessageJob(
            user_id=user.id,
            conversation_id=conversation.id,
            user_message_id=user_message.id,
            host_id=None,
            status="queued",
            created_at=utcnow() - timedelta(seconds=120),  # Old enough to be orphaned
        )
        db.add(job)
        db.commit()

        # Run orphan detection
        log_stale_queued_jobs(db, settings)

        # Job should be marked as failed
        db.refresh(job)
        assert job.status == "failed"
        assert "No active host" in job.error_text
        assert job.completed_at is not None


def test_orphaned_job_with_revoked_host_is_marked_failed(tmp_path):
    from app.models import MessageJob
    from app.services import log_stale_queued_jobs

    settings = Settings(
        environment="test",
        public_base_url="http://testserver/v1",
        database_url=f"sqlite:///{tmp_path / 'orphan-revoked.db'}",
        internal_api_key="test-internal-key",
        orphaned_job_expiry_seconds=60,
        stale_job_warning_seconds=30,
    )
    database = Database(settings.database_url)
    database.create_all()

    with database.session() as db:
        user = ensure_default_user(db, settings)
        conversation = get_or_create_current_conversation(db, user_id=user.id)
        host = HeraldHost(
            user_id=user.id,
            display_name="Test Host",
            revoked_at=utcnow(),  # Host is revoked
        )
        db.add(host)
        db.flush()

        user_message = append_message(
            db,
            conversation=conversation,
            user_id=user.id,
            role="user",
            text="Hello",
            delivery_status="sent",
        )

        job = MessageJob(
            user_id=user.id,
            conversation_id=conversation.id,
            user_message_id=user_message.id,
            host_id=host.id,
            status="queued",
            created_at=utcnow() - timedelta(seconds=120),
        )
        db.add(job)
        db.commit()

        log_stale_queued_jobs(db, settings)

        db.refresh(job)
        assert job.status == "failed"
        assert "No active host" in job.error_text


def test_job_with_offline_host_stays_queued(tmp_path):
    from app.models import MessageJob
    from app.services import log_stale_queued_jobs

    settings = Settings(
        environment="test",
        public_base_url="http://testserver/v1",
        database_url=f"sqlite:///{tmp_path / 'offline-host.db'}",
        internal_api_key="test-internal-key",
        orphaned_job_expiry_seconds=60,
        stale_job_warning_seconds=30,
    )
    database = Database(settings.database_url)
    database.create_all()

    with database.session() as db:
        user = ensure_default_user(db, settings)
        conversation = get_or_create_current_conversation(db, user_id=user.id)
        host = HeraldHost(
            user_id=user.id,
            display_name="Offline Host",
            active_connection_nonce=None,  # Host is offline
        )
        db.add(host)
        db.flush()

        user_message = append_message(
            db,
            conversation=conversation,
            user_id=user.id,
            role="user",
            text="Hello",
            delivery_status="sent",
        )

        job = MessageJob(
            user_id=user.id,
            conversation_id=conversation.id,
            user_message_id=user_message.id,
            host_id=host.id,
            status="queued",
            created_at=utcnow() - timedelta(seconds=120),
        )
        db.add(job)
        db.commit()

        log_stale_queued_jobs(db, settings)

        # Job should still be queued (host exists but offline)
        db.refresh(job)
        assert job.status == "queued"


def test_job_with_connected_host_stays_queued(tmp_path):
    from app.models import MessageJob
    from app.services import log_stale_queued_jobs

    settings = Settings(
        environment="test",
        public_base_url="http://testserver/v1",
        database_url=f"sqlite:///{tmp_path / 'connected-host.db'}",
        internal_api_key="test-internal-key",
        orphaned_job_expiry_seconds=60,
        stale_job_warning_seconds=30,
    )
    database = Database(settings.database_url)
    database.create_all()

    with database.session() as db:
        user = ensure_default_user(db, settings)
        conversation = get_or_create_current_conversation(db, user_id=user.id)
        host = HeraldHost(
            user_id=user.id,
            display_name="Connected Host",
            active_connection_nonce="some-nonce",  # Host is connected
        )
        db.add(host)
        db.flush()

        user_message = append_message(
            db,
            conversation=conversation,
            user_id=user.id,
            role="user",
            text="Hello",
            delivery_status="sent",
        )

        job = MessageJob(
            user_id=user.id,
            conversation_id=conversation.id,
            user_message_id=user_message.id,
            host_id=host.id,
            status="queued",
            created_at=utcnow() - timedelta(seconds=120),
        )
        db.add(job)
        db.commit()

        log_stale_queued_jobs(db, settings)

        # Job should still be queued (host is connected)
        db.refresh(job)
        assert job.status == "queued"
