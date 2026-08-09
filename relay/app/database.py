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
        table_names = set(inspector.get_table_names())

        def _exec_safe(sql: str) -> None:
            """Execute a statement in its own transaction so failures don't poison later steps."""
            try:
                with self.engine.begin() as conn:
                    conn.execute(text(sql))
            except Exception:
                pass

        # Herald rebrand migration: rename hermes_hosts table and columns
        if "hermes_hosts" in table_names and "herald_hosts" not in table_names:
            _exec_safe("ALTER TABLE hermes_hosts RENAME TO herald_hosts")
            for old_col, new_col in [
                ("hermes_command", "herald_command"),
                ("hermes_version", "herald_version"),
                ("hermes_model", "herald_model"),
            ]:
                _exec_safe(f"ALTER TABLE herald_hosts RENAME COLUMN {old_col} TO {new_col}")
        elif "hermes_hosts" in table_names and "herald_hosts" in table_names:
            if self.is_sqlite:
                # SQLite cannot DROP/ADD foreign-key constraints with ALTER
                # TABLE. Older rebrand builds therefore left dependent tables
                # referencing the now-obsolete hermes_hosts table. Repair only
                # those dependent CREATE statements; never rewrite the
                # hermes_hosts table's own declaration.
                with self.engine.begin() as connection:
                    schema_version = connection.execute(text("PRAGMA schema_version")).scalar_one()
                    connection.execute(text("PRAGMA writable_schema=ON"))
                    connection.execute(
                        text(
                            "UPDATE sqlite_master "
                            "SET sql = replace(sql, 'REFERENCES hermes_hosts', 'REFERENCES herald_hosts') "
                            "WHERE type = 'table' AND name != 'hermes_hosts' "
                            "AND instr(sql, 'REFERENCES hermes_hosts') > 0"
                        )
                    )
                    connection.execute(text(f"PRAGMA schema_version={schema_version + 1}"))
                    connection.execute(text("PRAGMA writable_schema=OFF"))
                inspector = inspect(self.engine)

            # Both exist — drop FKs, drop old table, re-add FKs
            for q in [
                "ALTER TABLE host_enrollment_invites DROP CONSTRAINT IF EXISTS host_enrollment_invites_redeemed_host_id_fkey",
                "ALTER TABLE phone_pairing_codes DROP CONSTRAINT IF EXISTS phone_pairing_codes_host_id_fkey",
                "ALTER TABLE phone_pairing_codes DROP CONSTRAINT IF EXISTS phone_pairing_codes_created_by_host_id_fkey",
                "ALTER TABLE message_jobs DROP CONSTRAINT IF EXISTS message_jobs_host_id_fkey",
                "ALTER TABLE voice_sessions DROP CONSTRAINT IF EXISTS voice_sessions_host_id_fkey",
            ]:
                _exec_safe(q)
            _exec_safe("DROP TABLE IF EXISTS hermes_hosts")
            for q in [
                "ALTER TABLE host_enrollment_invites ADD CONSTRAINT host_enrollment_invites_redeemed_host_id_fkey FOREIGN KEY (redeemed_host_id) REFERENCES herald_hosts(id)",
                "ALTER TABLE phone_pairing_codes ADD CONSTRAINT phone_pairing_codes_host_id_fkey FOREIGN KEY (host_id) REFERENCES herald_hosts(id)",
                "ALTER TABLE phone_pairing_codes ADD CONSTRAINT phone_pairing_codes_created_by_host_id_fkey FOREIGN KEY (created_by_host_id) REFERENCES herald_hosts(id)",
                "ALTER TABLE message_jobs ADD CONSTRAINT message_jobs_host_id_fkey FOREIGN KEY (host_id) REFERENCES herald_hosts(id)",
                "ALTER TABLE voice_sessions ADD CONSTRAINT voice_sessions_host_id_fkey FOREIGN KEY (host_id) REFERENCES herald_hosts(id)",
            ]:
                _exec_safe(q)
            host_cols = {c["name"] for c in inspector.get_columns("herald_hosts")}
            for old_col, new_col in [
                ("hermes_command", "herald_command"),
                ("hermes_version", "herald_version"),
                ("hermes_model", "herald_model"),
            ]:
                if old_col in host_cols:
                    _exec_safe(f"ALTER TABLE herald_hosts RENAME COLUMN {old_col} TO {new_col}")

        # Rename hermes_session_id on conversations
        if "conversations" in table_names:
            conv_cols = {c["name"] for c in inspector.get_columns("conversations")}
            if "hermes_session_id" in conv_cols:
                _exec_safe("ALTER TABLE conversations RENAME COLUMN hermes_session_id TO herald_session_id")

        # All remaining migrations use individual safe transactions
        with self.engine.begin() as connection:
            device_columns = {column["name"] for column in inspector.get_columns("devices")}
            if "app_state" not in device_columns:
                connection.execute(text("ALTER TABLE devices ADD COLUMN app_state TEXT"))
            if "app_state_updated_at" not in device_columns:
                if str(self.engine.url).startswith("sqlite"):
                    connection.execute(text("ALTER TABLE devices ADD COLUMN app_state_updated_at DATETIME"))
                else:
                    connection.execute(text("ALTER TABLE devices ADD COLUMN app_state_updated_at TIMESTAMP WITH TIME ZONE"))

            host_columns = {column["name"] for column in inspector.get_columns("herald_hosts")}
            if "herald_model" not in host_columns:
                connection.execute(text("ALTER TABLE herald_hosts ADD COLUMN herald_model TEXT"))

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
            if "herald_session_id" not in conversation_columns:
                connection.execute(text("ALTER TABLE conversations ADD COLUMN herald_session_id TEXT"))

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
            # NOTE: context_data column must be added manually:
            # ALTER TABLE message_jobs ADD COLUMN context_data JSON;
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

            # --- job_events table (Phase A-1a) ---
            # NOTE: SQLite uses TEXT for JSON/DATETIME. Postgres deployment will
            # use JSONB for payload_json and TIMESTAMPTZ for created_at.
            _exec_safe("""
                CREATE TABLE IF NOT EXISTS job_events (
                    id            TEXT PRIMARY KEY,
                    job_id        TEXT NOT NULL REFERENCES message_jobs(id),
                    seq           BIGINT NOT NULL,
                    attempt       INTEGER NOT NULL,
                    source_seq    BIGINT NOT NULL,
                    type          TEXT NOT NULL,
                    payload_json  TEXT NOT NULL,
                    created_at    DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
                )
            """)
            _exec_safe("CREATE UNIQUE INDEX IF NOT EXISTS ux_job_events_job_seq ON job_events(job_id, seq)")
            _exec_safe("CREATE UNIQUE INDEX IF NOT EXISTS ux_job_events_job_attempt_src ON job_events(job_id, attempt, source_seq)")
            _exec_safe("CREATE INDEX IF NOT EXISTS ix_job_events_job_seq ON job_events(job_id, seq)")
            if "attempt" not in job_columns:
                _exec_safe("ALTER TABLE message_jobs ADD COLUMN attempt INTEGER NOT NULL DEFAULT 0")

            if "reasoning_effort" not in job_columns:
                _exec_safe("ALTER TABLE message_jobs ADD COLUMN reasoning_effort TEXT")

    @contextmanager
    def session(self) -> Iterator[Session]:
        db = self.session_factory()
        try:
            yield db
        finally:
            db.close()
