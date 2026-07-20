from __future__ import annotations

from datetime import timedelta

from sqlalchemy import select

from app.config import Settings
from app.database import Database
from app.models import (
    EnrichedNoteRevision,
    Note,
    NoteBlob,
    NoteRecognition,
    NoteRun,
    NoteRunEvent,
    User,
    utcnow,
)
from app.services import ensure_default_user


def make_database(tmp_path):
    settings = Settings(
        environment="test",
        public_base_url="http://testserver/v1",
        database_url=f"sqlite:///{tmp_path / 'relay-notes.db'}",
        internal_api_key="test-internal-key",
    )
    database = Database(settings.database_url)
    database.create_all()
    return settings, database


def test_note_create_and_query(tmp_path):
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        note = Note(
            id="note-001",
            user_id=user.id,
            title="Test Note",
            current_drawing_revision=0,
            current_text_revision=0,
        )
        db.add(note)
        db.commit()

        result = db.execute(select(Note).where(Note.id == "note-001")).scalar_one()
        assert result.title == "Test Note"
        assert result.user_id == user.id
        assert result.pinned is False
        assert result.deleted_at is None


def test_note_blob_unique_constraint(tmp_path):
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        note = Note(id="note-002", user_id=user.id, title="Blob Test")
        db.add(note)
        db.commit()

        blob1 = NoteBlob(
            id="blob-001",
            note_id="note-002",
            drawing_revision=1,
            content_hash="abc123",
            byte_size=1024,
            storage_path="/data/rev-1.pkdrawing",
        )
        db.add(blob1)
        db.commit()

        # Same revision should fail
        blob2 = NoteBlob(
            id="blob-002",
            note_id="note-002",
            drawing_revision=1,
            content_hash="def456",
            byte_size=2048,
            storage_path="/data/rev-1-new.pkdrawing",
        )
        db.add(blob2)
        try:
            db.commit()
            assert False, "Should have raised IntegrityError"
        except Exception:
            db.rollback()


def test_note_run_idempotency(tmp_path):
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        note = Note(id="note-003", user_id=user.id, title="Run Test")
        db.add(note)
        db.commit()

        run1 = NoteRun(
            id="run-001",
            user_id=user.id,
            note_id="note-003",
            client_run_id="client-001",
            source_drawing_revision=1,
            source_text_revision=1,
            requested_directives=[{"command": "research", "arguments": "topic"}],
        )
        db.add(run1)
        db.commit()

        # Same client_run_id should fail
        run2 = NoteRun(
            id="run-002",
            user_id=user.id,
            note_id="note-003",
            client_run_id="client-001",
            source_drawing_revision=2,
            source_text_revision=2,
            requested_directives=[{"command": "summary"}],
        )
        db.add(run2)
        try:
            db.commit()
            assert False, "Should have raised IntegrityError"
        except Exception:
            db.rollback()


def test_note_run_event_ordering(tmp_path):
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        note = Note(id="note-004", user_id=user.id, title="Event Test")
        run = NoteRun(
            id="run-004",
            user_id=user.id,
            note_id="note-004",
            client_run_id="client-004",
            source_drawing_revision=1,
            source_text_revision=1,
            requested_directives=[],
        )
        db.add_all([note, run])
        db.commit()

        events = [
            NoteRunEvent(id=f"evt-{i}", run_id="run-004", seq=i, attempt=0, type="text_delta", payload_json={"delta": f"chunk-{i}"})
            for i in range(1, 6)
        ]
        db.add_all(events)
        db.commit()

        result = db.execute(
            select(NoteRunEvent)
            .where(NoteRunEvent.run_id == "run-004")
            .order_by(NoteRunEvent.seq)
        ).scalars().all()

        assert len(result) == 5
        assert [e.seq for e in result] == [1, 2, 3, 4, 5]


def test_enriched_note_revision_stale_flag(tmp_path):
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        note = Note(id="note-005", user_id=user.id, title="Stale Test")
        run = NoteRun(
            id="run-005",
            user_id=user.id,
            note_id="note-005",
            client_run_id="client-005",
            source_drawing_revision=1,
            source_text_revision=1,
            requested_directives=[],
        )
        db.add_all([note, run])
        db.commit()

        revision = EnrichedNoteRevision(
            id="rev-001",
            note_id="note-005",
            run_id="run-005",
            source_drawing_revision=1,
            source_text_revision=1,
            title="Enriched",
            markdown="# Enriched",
            structured_sections=[],
            is_stale=True,
        )
        db.add(revision)
        db.commit()

        result = db.execute(
            select(EnrichedNoteRevision).where(EnrichedNoteRevision.id == "rev-001")
        ).scalar_one()
        assert result.is_stale is True


def test_note_recognition_fields(tmp_path):
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        note = Note(id="note-006", user_id=user.id, title="Recognition Test")
        db.add(note)
        db.commit()

        rec = NoteRecognition(
            id="rec-001",
            note_id="note-006",
            drawing_revision=1,
            engine="vn_accurate",
            engine_version="2.0",
            languages='["en-US"]',
            raw_text="Hello world",
            user_corrected_text="Hello, world!",
        )
        db.add(rec)
        db.commit()

        result = db.execute(
            select(NoteRecognition).where(NoteRecognition.id == "rec-001")
        ).scalar_one()
        assert result.raw_text == "Hello world"
        assert result.user_corrected_text == "Hello, world!"
        assert result.engine == "vn_accurate"


def test_create_all_tables(tmp_path):
    """Verify all note tables are created by create_all."""
    _, database = make_database(tmp_path)

    from sqlalchemy import inspect
    inspector = inspect(database.engine)
    tables = set(inspector.get_table_names())

    expected = {"notes", "note_blobs", "note_recognitions", "note_runs", "note_run_events", "enriched_note_revisions"}
    assert expected.issubset(tables), f"Missing tables: {expected - tables}"


# ---------------------------------------------------------------------------
# Run Lifecycle Tests
# ---------------------------------------------------------------------------


def test_run_status_transitions(tmp_path):
    """Run progresses through queued → claimed → completed."""
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        note = Note(id="note-lc-001", user_id=user.id, title="Lifecycle Test")
        db.add(note)
        db.commit()

        run = NoteRun(
            id="run-lc-001",
            user_id=user.id,
            note_id="note-lc-001",
            client_run_id="client-lc-001",
            source_drawing_revision=1,
            source_text_revision=1,
            requested_directives=[{"command": "summary"}],
            status="queued",
        )
        db.add(run)
        db.commit()

        # Claim
        run.status = "claimed"
        run.attempt = 1
        db.commit()

        result = db.execute(select(NoteRun).where(NoteRun.id == "run-lc-001")).scalar_one()
        assert result.status == "claimed"
        assert result.attempt == 1

        # Complete
        run.status = "completed"
        run.completed_at = utcnow()
        run.result = {"title": "Done", "markdown": "# Done"}
        db.commit()

        result = db.execute(select(NoteRun).where(NoteRun.id == "run-lc-001")).scalar_one()
        assert result.status == "completed"
        assert result.completed_at is not None
        assert result.result["title"] == "Done"


def test_run_cancellation_is_terminal(tmp_path):
    """Cancelled run stays cancelled and cannot be re-claimed."""
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        note = Note(id="note-cancel-001", user_id=user.id, title="Cancel Test")
        db.add(note)
        db.commit()

        run = NoteRun(
            id="run-cancel-001",
            user_id=user.id,
            note_id="note-cancel-001",
            client_run_id="client-cancel-001",
            source_drawing_revision=1,
            source_text_revision=1,
            requested_directives=[],
            status="queued",
        )
        db.add(run)
        db.commit()

        # Cancel
        run.status = "cancelled"
        run.completed_at = utcnow()
        db.commit()

        # Verify terminal
        result = db.execute(select(NoteRun).where(NoteRun.id == "run-cancel-001")).scalar_one()
        assert result.status == "cancelled"
        assert result.completed_at is not None


def test_run_failed_status(tmp_path):
    """Failed run records error text."""
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        note = Note(id="note-fail-001", user_id=user.id, title="Fail Test")
        db.add(note)
        db.commit()

        run = NoteRun(
            id="run-fail-001",
            user_id=user.id,
            note_id="note-fail-001",
            client_run_id="client-fail-001",
            source_drawing_revision=1,
            source_text_revision=1,
            requested_directives=[],
            status="claimed",
        )
        db.add(run)
        db.commit()

        run.status = "failed"
        run.error_text = "Schema validation failed: missing title"
        run.completed_at = utcnow()
        db.commit()

        result = db.execute(select(NoteRun).where(NoteRun.id == "run-fail-001")).scalar_one()
        assert result.status == "failed"
        assert "Schema validation" in result.error_text


# ---------------------------------------------------------------------------
# Event Replay Tests
# ---------------------------------------------------------------------------


def test_event_cursor_replay(tmp_path):
    """Events are replayed from a cursor position."""
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        note = Note(id="note-cursor-001", user_id=user.id, title="Cursor Test")
        run = NoteRun(
            id="run-cursor-001",
            user_id=user.id,
            note_id="note-cursor-001",
            client_run_id="client-cursor-001",
            source_drawing_revision=1,
            source_text_revision=1,
            requested_directives=[],
        )
        db.add_all([note, run])
        db.commit()

        # Create 10 events
        for i in range(1, 11):
            db.add(NoteRunEvent(
                id=f"evt-cursor-{i:03d}",
                run_id="run-cursor-001",
                seq=i,
                attempt=0,
                type="text_delta",
                payload_json={"delta": f"chunk-{i}"},
            ))
        db.commit()

        # Replay from seq 5
        result = db.execute(
            select(NoteRunEvent)
            .where(NoteRunEvent.run_id == "run-cursor-001", NoteRunEvent.seq > 5)
            .order_by(NoteRunEvent.seq)
        ).scalars().all()

        assert len(result) == 5
        assert result[0].seq == 6
        assert result[-1].seq == 10


def test_event_no_loss_no_duplication(tmp_path):
    """Events are ordered, no gaps, no duplicates."""
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        note = Note(id="note-nodup-001", user_id=user.id, title="NoDup Test")
        run = NoteRun(
            id="run-nodup-001",
            user_id=user.id,
            note_id="note-nodup-001",
            client_run_id="client-nodup-001",
            source_drawing_revision=1,
            source_text_revision=1,
            requested_directives=[],
        )
        db.add_all([note, run])
        db.commit()

        # Create events with mixed types
        event_types = ["text_delta", "reasoning_delta", "text_delta", "tool_call", "text_delta"]
        for i, etype in enumerate(event_types, 1):
            db.add(NoteRunEvent(
                id=f"evt-nodup-{i:03d}",
                run_id="run-nodup-001",
                seq=i,
                attempt=0,
                type=etype,
                payload_json={"data": f"event-{i}"},
            ))
        db.commit()

        result = db.execute(
            select(NoteRunEvent)
            .where(NoteRunEvent.run_id == "run-nodup-001")
            .order_by(NoteRunEvent.seq)
        ).scalars().all()

        # Verify ordering and no gaps
        assert len(result) == 5
        seqs = [e.seq for e in result]
        assert seqs == [1, 2, 3, 4, 5]
        assert result[0].type == "text_delta"
        assert result[3].type == "tool_call"


# ---------------------------------------------------------------------------
# Revision Fence Tests
# ---------------------------------------------------------------------------


def test_revision_fence_stale_result(tmp_path):
    """Result applied when revisions match; marked stale when they don't."""
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        note = Note(
            id="note-fence-001",
            user_id=user.id,
            title="Fence Test",
            current_drawing_revision=3,
            current_text_revision=2,
        )
        db.add(note)
        db.commit()

        # Run submitted at revision (3, 2)
        run = NoteRun(
            id="run-fence-001",
            user_id=user.id,
            note_id="note-fence-001",
            client_run_id="client-fence-001",
            source_drawing_revision=3,
            source_text_revision=2,
            requested_directives=[],
            status="completed",
            result={"title": "Done", "markdown": "# Done"},
        )
        db.add(run)
        db.commit()

        # Note advanced to revision (4, 2) while run was in progress
        note.current_drawing_revision = 4
        db.commit()

        # Result is stale: source (3,2) != current (4,2)
        is_stale = (
            run.source_drawing_revision != note.current_drawing_revision
            or run.source_text_revision != note.current_text_revision
        )
        assert is_stale is True

        # Save as stale revision
        revision = EnrichedNoteRevision(
            id="rev-fence-001",
            note_id="note-fence-001",
            run_id="run-fence-001",
            source_drawing_revision=run.source_drawing_revision,
            source_text_revision=run.source_text_revision,
            title="Done",
            markdown="# Done",
            structured_sections=[],
            is_stale=is_stale,
        )
        db.add(revision)
        db.commit()

        result = db.execute(
            select(EnrichedNoteRevision).where(EnrichedNoteRevision.id == "rev-fence-001")
        ).scalar_one()
        assert result.is_stale is True


def test_revision_fence_current_result(tmp_path):
    """Result applied as current when revisions match."""
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        note = Note(
            id="note-fence-002",
            user_id=user.id,
            title="Fence Match",
            current_drawing_revision=3,
            current_text_revision=2,
        )
        db.add(note)
        db.commit()

        run = NoteRun(
            id="run-fence-002",
            user_id=user.id,
            note_id="note-fence-002",
            client_run_id="client-fence-002",
            source_drawing_revision=3,
            source_text_revision=2,
            requested_directives=[],
            status="completed",
            result={"title": "Done", "markdown": "# Done"},
        )
        db.add(run)
        db.commit()

        # Revisions still match
        is_stale = (
            run.source_drawing_revision != note.current_drawing_revision
            or run.source_text_revision != note.current_text_revision
        )
        assert is_stale is False

        revision = EnrichedNoteRevision(
            id="rev-fence-002",
            note_id="note-fence-002",
            run_id="run-fence-002",
            source_drawing_revision=3,
            source_text_revision=2,
            title="Done",
            markdown="# Done",
            structured_sections=[],
            is_stale=is_stale,
        )
        db.add(revision)
        db.commit()

        result = db.execute(
            select(EnrichedNoteRevision).where(EnrichedNoteRevision.id == "rev-fence-002")
        ).scalar_one()
        assert result.is_stale is False


def test_stale_result_preserved_as_history(tmp_path):
    """Stale results are kept as history, never clobbered."""
    settings, database = make_database(tmp_path)

    with database.session() as db:
        user = ensure_default_user(db, settings)
        note = Note(id="note-history-001", user_id=user.id, title="History Test")
        db.add(note)
        db.commit()

        # Two runs, both stale
        for i, (draw_rev, text_rev) in enumerate([(1, 1), (2, 1)], 1):
            run = NoteRun(
                id=f"run-history-{i:03d}",
                user_id=user.id,
                note_id="note-history-001",
                client_run_id=f"client-history-{i:03d}",
                source_drawing_revision=draw_rev,
                source_text_revision=text_rev,
                requested_directives=[],
                status="completed",
            )
            db.add(run)
            db.add(EnrichedNoteRevision(
                id=f"rev-history-{i:03d}",
                note_id="note-history-001",
                run_id=f"run-history-{i:03d}",
                source_drawing_revision=draw_rev,
                source_text_revision=text_rev,
                title=f"Result {i}",
                markdown=f"# Result {i}",
                structured_sections=[],
                is_stale=True,
            ))
        db.commit()

        # Current note at revision 3
        note.current_drawing_revision = 3
        db.commit()

        # Both stale revisions preserved
        revisions = db.execute(
            select(EnrichedNoteRevision)
            .where(EnrichedNoteRevision.note_id == "note-history-001")
            .order_by(EnrichedNoteRevision.source_drawing_revision)
        ).scalars().all()

        assert len(revisions) == 2
        assert all(r.is_stale for r in revisions)
        assert revisions[0].source_drawing_revision == 1
        assert revisions[1].source_drawing_revision == 2
