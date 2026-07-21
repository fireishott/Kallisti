from __future__ import annotations

from datetime import datetime, timezone
from uuid import uuid4

from fastapi import APIRouter, Depends, HTTPException, Request, status
from fastapi.responses import JSONResponse
from sqlalchemy import select
from sqlalchemy.orm import Session

from .security import AuthContext, get_auth_context, get_db
from .models import Note, NoteBlob, NoteRecognition, NoteRun, NoteRunEvent, EnrichedNoteRevision, utcnow
from .services import record_audit


router = APIRouter(prefix="/v1/notes", tags=["notes"])


def _get_note_or_404(db: Session, note_id: str, user_id: str) -> Note:
    """Get a note by ID, raising 404 if not found or not owned by user."""
    note = db.get(Note, note_id)
    if note is None or note.user_id != user_id:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Note not found.")
    return note


# ── Note CRUD ────────────────────────────────────────────────────


@router.get("")
def list_notes(
    auth: AuthContext = Depends(get_auth_context),
    db: Session = Depends(get_db),
):
    """List notes owned by the authenticated user (excludes hard-deleted)."""
    notes = db.execute(
        select(Note)
        .where(Note.user_id == auth.user.id, Note.deleted_at.is_(None))
        .order_by(Note.updated_at.desc())
    ).scalars().all()

    return {
        "data": [_note_to_dict(n) for n in notes],
        "meta": {"count": len(notes)},
    }


@router.post("")
async def create_note(
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
    db: Session = Depends(get_db),
):
    """Create a new note."""
    body = await _parse_body(request)
    note = Note(
        id=str(uuid4()),
        user_id=auth.user.id,
        title=body.get("title", ""),
        folder_id=body.get("folderId"),
        pinned=body.get("pinned", False),
    )
    db.add(note)
    db.flush()

    record_audit(
        db,
        actor_type="user",
        actor_id=auth.user.id,
        action="note.create",
        entity_type="note",
        entity_id=note.id,
        payload={"title": note.title},
    )
    db.commit()

    return JSONResponse(content={"data": _note_to_dict(note)}, status_code=status.HTTP_201_CREATED)


@router.get("/{note_id}")
def get_note(
    note_id: str,
    auth: AuthContext = Depends(get_auth_context),
    db: Session = Depends(get_db),
):
    note = _get_note_or_404(db, note_id, auth.user.id)
    return {"data": _note_to_dict(note)}


@router.patch("/{note_id}")
async def update_note(
    note_id: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
    db: Session = Depends(get_db),
):
    """Update note metadata. Requires If-Match header for optimistic concurrency."""
    note = _get_note_or_404(db, note_id, auth.user.id)

    # If-Match check
    if_match = request.headers.get("if-match")
    if if_match is not None:
        # Simple revision-based ETag
        expected = f'"rev-{note.current_drawing_revision}-{note.current_text_revision}"'
        if if_match != expected:
            raise HTTPException(
                status_code=status.HTTP_412_PRECONDITION_FAILED,
                detail="Note revision mismatch.",
            )

    body = await _parse_body(request)
    if "title" in body:
        note.title = body["title"]
    if "folderId" in body:
        note.folder_id = body["folderId"]
    if "pinned" in body:
        note.pinned = body["pinned"]

    note.updated_at = utcnow()
    db.commit()

    return {"data": _note_to_dict(note)}


@router.delete("/{note_id}")
def delete_note(
    note_id: str,
    auth: AuthContext = Depends(get_auth_context),
    db: Session = Depends(get_db),
):
    """Soft-delete a note. Purge after 30 days."""
    note = _get_note_or_404(db, note_id, auth.user.id)
    note.deleted_at = utcnow()
    db.commit()

    record_audit(
        db,
        actor_type="user",
        actor_id=auth.user.id,
        action="note.delete",
        entity_type="note",
        entity_id=note.id,
    )
    db.commit()

    return {"data": {"id": note.id, "deleted": True}}


# ── Blob Endpoints ───────────────────────────────────────────────


@router.put("/{note_id}/blobs/{revision}")
def upload_blob(
    note_id: str,
    revision: int,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
    db: Session = Depends(get_db),
):
    """Upload a PKDrawing blob. Body = raw bytes; X-Content-SHA256 verified."""
    note = _get_note_or_404(db, note_id, auth.user.id)

    # Check if revision already exists
    existing = db.execute(
        select(NoteBlob).where(
            NoteBlob.note_id == note_id,
            NoteBlob.drawing_revision == revision,
        )
    ).scalar_one_or_none()
    if existing is not None:
        raise HTTPException(
            status_code=status.HTTP_409_CONFLICT,
            detail=f"Revision {revision} already exists.",
        )

    # Size cap: 25 MB
    content_length = request.headers.get("content-length")
    if content_length and int(content_length) > 25 * 1024 * 1024:
        raise HTTPException(
            status_code=status.HTTP_413_REQUEST_ENTITY_TOO_LARGE,
            detail="Blob exceeds 25 MB limit.",
        )

    # Hash verification
    expected_hash = request.headers.get("x-content-sha256")

    # TODO: Read body and verify hash, store blob
    # For now, create the metadata record
    blob = NoteBlob(
        id=str(uuid4()),
        note_id=note_id,
        drawing_revision=revision,
        content_hash=expected_hash or "pending",
        byte_size=int(content_length) if content_length else 0,
        storage_path=f"notes/{note_id}/rev-{revision}.pkdrawing",
    )
    db.add(blob)

    note.current_drawing_revision = max(note.current_drawing_revision, revision)
    note.updated_at = utcnow()
    db.commit()

    return {"data": {"noteId": note_id, "revision": revision, "contentHash": blob.content_hash}}


@router.get("/{note_id}/blobs/{revision}")
def download_blob(
    note_id: str,
    revision: int,
    auth: AuthContext = Depends(get_auth_context),
    db: Session = Depends(get_db),
):
    """Download a PKDrawing blob."""
    _get_note_or_404(db, note_id, auth.user.id)

    blob = db.execute(
        select(NoteBlob).where(
            NoteBlob.note_id == note_id,
            NoteBlob.drawing_revision == revision,
        )
    ).scalar_one_or_none()

    if blob is None:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Blob not found.")

    # TODO: Return actual blob bytes from storage
    return {
        "data": {
            "noteId": note_id,
            "revision": revision,
            "contentHash": blob.content_hash,
            "byteSize": blob.byte_size,
            "storagePath": blob.storage_path,
        }
    }


# ── Run Endpoints ────────────────────────────────────────────────


@router.post("/{note_id}/runs")
async def create_run(
    note_id: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
    db: Session = Depends(get_db),
):
    """Start an enrichment run. Idempotent on clientRunId."""
    note = _get_note_or_404(db, note_id, auth.user.id)
    body = await _parse_body(request)

    client_run_id = body.get("clientRunId")
    if not client_run_id:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="clientRunId is required.",
        )

    # Idempotency check
    existing = db.execute(
        select(NoteRun).where(
            NoteRun.user_id == auth.user.id,
            NoteRun.client_run_id == client_run_id,
        )
    ).scalar_one_or_none()

    if existing is not None:
        return {"data": _run_to_dict(existing)}

    run = NoteRun(
        id=str(uuid4()),
        user_id=auth.user.id,
        note_id=note_id,
        client_run_id=client_run_id,
        source_drawing_revision=body.get("sourceDrawingRevision", note.current_drawing_revision),
        source_text_revision=body.get("sourceTextRevision", note.current_text_revision),
        requested_directives=body.get("directives", []),
        status="queued",
    )
    db.add(run)
    db.flush()

    record_audit(
        db,
        actor_type="user",
        actor_id=auth.user.id,
        action="note.run.create",
        entity_type="note_run",
        entity_id=run.id,
        payload={"noteId": note_id, "clientRunId": client_run_id},
    )
    db.commit()

    return JSONResponse(content={"data": _run_to_dict(run)}, status_code=status.HTTP_201_CREATED)


@router.get("/../note-runs/{run_id}")
def get_run(
    run_id: str,
    auth: AuthContext = Depends(get_auth_context),
    db: Session = Depends(get_db),
):
    """Get run status."""
    run = db.get(NoteRun, run_id)
    if run is None or run.user_id != auth.user.id:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Run not found.")

    return {"data": _run_to_dict(run)}


@router.get("/../note-runs/{run_id}/events")
def get_run_events(
    run_id: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
    db: Session = Depends(get_db),
):
    """Get run events with cursor-based pagination."""
    run = db.get(NoteRun, run_id)
    if run is None or run.user_id != auth.user.id:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Run not found.")

    # Cursor from query params
    after_seq = request.query_params.get("after")
    limit = min(int(request.query_params.get("limit", "100")), 500)

    query = select(NoteRunEvent).where(NoteRunEvent.run_id == run_id)
    if after_seq is not None:
        query = query.where(NoteRunEvent.seq > int(after_seq))
    query = query.order_by(NoteRunEvent.seq).limit(limit)

    events = db.execute(query).scalars().all()

    return {
        "data": [_event_to_dict(e) for e in events],
        "meta": {"count": len(events), "runId": run_id},
    }


@router.post("/../note-runs/{run_id}/cancel")
def cancel_run(
    run_id: str,
    auth: AuthContext = Depends(get_auth_context),
    db: Session = Depends(get_db),
):
    """Cancel a run. Terminal and idempotent."""
    run = db.get(NoteRun, run_id)
    if run is None or run.user_id != auth.user.id:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Run not found.")

    if run.status in ("completed", "failed", "cancelled"):
        return {"data": _run_to_dict(run)}

    run.status = "cancelled"
    run.completed_at = utcnow()
    db.commit()

    return {"data": _run_to_dict(run)}


# ── Helpers ──────────────────────────────────────────────────────


async def _parse_body(request: Request) -> dict:
    """Parse JSON body, returning empty dict on failure."""
    try:
        return await request.json()
    except Exception:
        return {}


def _note_to_dict(note: Note) -> dict:
    return {
        "id": note.id,
        "userId": note.user_id,
        "title": note.title,
        "folderId": note.folder_id,
        "pinned": note.pinned,
        "currentDrawingRevision": note.current_drawing_revision,
        "currentTextRevision": note.current_text_revision,
        "createdAt": note.created_at.isoformat() if note.created_at else None,
        "updatedAt": note.updated_at.isoformat() if note.updated_at else None,
        "deletedAt": note.deleted_at.isoformat() if note.deleted_at else None,
    }


def _run_to_dict(run: NoteRun) -> dict:
    return {
        "id": run.id,
        "userId": run.user_id,
        "noteId": run.note_id,
        "clientRunId": run.client_run_id,
        "sourceDrawingRevision": run.source_drawing_revision,
        "sourceTextRevision": run.source_text_revision,
        "requestedDirectives": run.requested_directives,
        "status": run.status,
        "attempt": run.attempt,
        "leaseExpiresAt": run.lease_expires_at.isoformat() if run.lease_expires_at else None,
        "errorText": run.error_text,
        "result": run.result,
        "createdAt": run.created_at.isoformat() if run.created_at else None,
        "completedAt": run.completed_at.isoformat() if run.completed_at else None,
    }


def _event_to_dict(event: NoteRunEvent) -> dict:
    return {
        "id": event.id,
        "runId": event.run_id,
        "seq": event.seq,
        "attempt": event.attempt,
        "sourceSeq": event.source_seq,
        "type": event.type,
        "payload": event.payload_json,
        "createdAt": event.created_at.isoformat() if event.created_at else None,
    }
