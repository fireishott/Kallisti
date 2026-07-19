from __future__ import annotations

from contextlib import contextmanager
from typing import Iterator

from sqlalchemy import create_engine, event, inspect, text
from sqlalchemy.orm import DeclarativeBase, Session, sessionmaker


class Base(DeclarativeBase):
    pass


class Database:
    def __init__(self, database_url: str) -> None:
        self.is_sqlite = database_url.startswith("sqlite")
        connect_args = {"check_same_thread": False, "timeout": 10} if self.is_sqlite else {}
        self.engine = create_engine(database_url, future=True, connect_args=connect_args)
        if self.is_sqlite:
            event.listen(self.engine, "connect", self._configure_sqlite_connection)
        self.session_factory = sessionmaker(
            bind=self.engine,
            autoflush=False,
            autocommit=False,
            expire_on_commit=False,
            class_=Session,
        )

    @staticmethod
    def _configure_sqlite_connection(dbapi_connection, _connection_record) -> None:
        cursor = dbapi_connection.cursor()
        try:
            cursor.execute("PRAGMA journal_mode=WAL")
            cursor.execute("PRAGMA synchronous=NORMAL")
            cursor.execute("PRAGMA foreign_keys=ON")
            cursor.execute("PRAGMA busy_timeout=5000")
        finally:
            cursor.close()

    def create_all(self) -> None:
        from . import models  # noqa: F401

        Base.metadata.create_all(self.engine)
        self._run_migrations()

    def _run_migrations(self) -> None:
        inspector = inspect(self.engine)

        with self.engine.begin() as connection:
            device_columns = {column["name"] for column in inspector.get_columns("devices")}
            if "app_state" not in device_columns:
                connection.execute(text("ALTER TABLE devices ADD COLUMN app_state TEXT"))
            if "app_state_updated_at" not in device_columns:
                if str(self.engine.url).startswith("sqlite"):
                    connection.execute(text("ALTER TABLE devices ADD COLUMN app_state_updated_at DATETIME"))
                else:
                    connection.execute(text("ALTER TABLE devices ADD COLUMN app_state_updated_at TIMESTAMP WITH TIME ZONE"))

            host_columns = {column["name"] for column in inspector.get_columns("hermes_hosts")}
            if "hermes_model" not in host_columns:
                connection.execute(text("ALTER TABLE hermes_hosts ADD COLUMN hermes_model TEXT"))

            push_columns = {column["name"] for column in inspector.get_columns("push_registrations")}
            if "transport" not in push_columns:
                connection.execute(text("ALTER TABLE push_registrations ADD COLUMN transport TEXT DEFAULT 'direct'"))
            if "relay_handle" not in push_columns:
                connection.execute(text("ALTER TABLE push_registrations ADD COLUMN relay_handle TEXT"))
            if "send_grant" not in push_columns:
                connection.execute(text("ALTER TABLE push_registrations ADD COLUMN send_grant TEXT"))
            if "relay_id" not in push_columns:
                connection.execute(text("ALTER TABLE push_registrations ADD COLUMN relay_id TEXT"))
            if "relay_public_key" not in push_columns:
                connection.execute(text("ALTER TABLE push_registrations ADD COLUMN relay_public_key TEXT"))
            if "token_debug_suffix" not in push_columns:
                connection.execute(text("ALTER TABLE push_registrations ADD COLUMN token_debug_suffix TEXT"))

            conversation_columns = {column["name"] for column in inspector.get_columns("conversations")}
            if "hermes_session_id" not in conversation_columns:
                connection.execute(text("ALTER TABLE conversations ADD COLUMN hermes_session_id TEXT"))

            message_columns = {column["name"] for column in inspector.get_columns("messages")}
            if "delivery_status" not in message_columns:
                connection.execute(text("ALTER TABLE messages ADD COLUMN delivery_status TEXT"))
            connection.execute(
                text(
                    "CREATE INDEX IF NOT EXISTS ix_messages_user_client_message_id "
                    "ON messages (user_id, client_message_id)"
                )
            )
            connection.execute(
                text(
                    "CREATE INDEX IF NOT EXISTS ix_voice_sessions_user_status "
                    "ON voice_sessions (user_id, status)"
                )
            )
            connection.execute(
                text(
                    "CREATE INDEX IF NOT EXISTS ix_voice_sessions_tool_token_hash "
                    "ON voice_sessions (relay_tool_token_hash)"
                )
            )
            job_columns = {column["name"] for column in inspector.get_columns("message_jobs")}
            if "usage_data" not in job_columns:
                connection.execute(text("ALTER TABLE message_jobs ADD COLUMN usage_data JSON"))
            if "diff_data" not in job_columns:
                connection.execute(text("ALTER TABLE message_jobs ADD COLUMN diff_data JSON"))

            if "source" not in message_columns:
                connection.execute(text("ALTER TABLE messages ADD COLUMN source TEXT"))

            if "attachments_data" not in message_columns:
                connection.execute(text("ALTER TABLE messages ADD COLUMN attachments_data JSON"))

            connection.execute(
                text(
                    "CREATE INDEX IF NOT EXISTS ix_message_jobs_user_status_created "
                    "ON message_jobs (user_id, status, created_at)"
                )
            )
            connection.execute(
                text(
                    "CREATE INDEX IF NOT EXISTS ix_message_jobs_status_lease "
                    "ON message_jobs (status, lease_expires_at)"
                )
            )
            connection.execute(
                text(
                    "CREATE INDEX IF NOT EXISTS ix_messages_conversation_created "
                    "ON messages (conversation_id, created_at)"
                )
            )
            connection.execute(
                text(
                    "CREATE INDEX IF NOT EXISTS ix_conversations_user_archived "
                    "ON conversations (user_id, is_archived)"
                )
            )

            voice_turn_columns = {column["name"] for column in inspector.get_columns("voice_turns")}
            if "client_turn_id" not in voice_turn_columns:
                connection.execute(text("ALTER TABLE voice_turns ADD COLUMN client_turn_id TEXT"))
            connection.execute(
                text(
                    "CREATE UNIQUE INDEX IF NOT EXISTS ix_voice_turns_session_client_turn_id "
                    "ON voice_turns (voice_session_id, client_turn_id)"
                )
            )

    @contextmanager
    def session(self) -> Iterator[Session]:
        db = self.session_factory()
        try:
            yield db
        finally:
            db.close()
