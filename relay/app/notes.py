from __future__ import annotations

import base64
import re
from datetime import datetime, timezone
from pathlib import Path
from uuid import UUID, uuid4

from fastapi import APIRouter, Depends, HTTPException, Request, status
from fastapi.responses import JSONResponse
from sqlalchemy import select
from sqlalchemy.orm import Session

from .config import Settings
from .security import AuthContext, get_auth_context, get_db, get_settings
from .models import (
    EnrichedNoteRevision,
    Note,
    NoteBlob,
    NoteRecognition,
    NoteRun,
    NoteRunEvent,
    utcnow,
)
from .services import append_note_run_event, record_audit


router = APIRouter(prefix="/v1/notes", tags=["notes"])
note_runs_router = APIRouter(prefix="/v1/note-runs", tags=["note-runs"])


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
    """Create a new note.

    ``noteId`` is client-generated per the v1 contract (stable across sync), so
    an id supplied by the app is honored when it is a valid UUID; anything else
    falls back to a server-generated id. Re-posting the same id returns the
    existing note instead of duplicating it, which keeps an app-side retry from
    forking note identity.
    """
    body = await _parse_body(request)
    requested_id = body.get("id") or body.get("noteId")
    if requested_id:
        try:
            note_id = str(UUID(str(requested_id)))
        except (ValueError, AttributeError, TypeError):
            note_id = str(uuid4())
        existing = db.get(Note, note_id)
        if existing is not None:
            if existing.user_id != auth.user.id:
                raise HTTPException(status_code=status.HTTP_409_CONFLICT, detail="Note id already in use.")
            return JSONResponse(content={"data": _note_to_dict(existing)}, status_code=status.HTTP_200_OK)
    else:
        note_id = str(uuid4())

    note = Note(
        id=note_id,
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

    # If-Match check — required for optimistic concurrency
    if_match = request.headers.get("if-match")
    if if_match is None:
        raise HTTPException(
            status_code=status.HTTP_409_CONFLICT,
            detail="If-Match header is required for note updates.",
        )

    expected = f'"{note.revision}"'
    if if_match != expected:
        raise HTTPException(
            status_code=status.HTTP_409_CONFLICT,
            detail="Note revision mismatch.",
        )

    body = await _parse_body(request)
    if "title" in body:
        note.title = body["title"]
    if "folderId" in body:
        note.folder_id = body["folderId"]
    if "pinned" in body:
        note.pinned = body["pinned"]

    note.revision += 1
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


# ── Recognition Endpoints ────────────────────────────────────────


@router.post("/{note_id}/recognitions")
async def create_recognition(
    note_id: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
    db: Session = Depends(get_db),
):
    """Store OCR/handwriting recognition results for a note revision."""
    note = _get_note_or_404(db, note_id, auth.user.id)
    body = await _parse_body(request)

    drawing_revision = body.get("drawingRevision")
    engine = body.get("engine")
    raw_text = body.get("rawText")

    if not drawing_revision or not engine or not raw_text:
        raise HTTPException(
            status_code=status.HTTP_400_BAD_REQUEST,
            detail="drawingRevision, engine, and rawText are required.",
        )

    recognition = NoteRecognition(
        id=str(uuid4()),
        note_id=note_id,
        drawing_revision=drawing_revision,
        engine=engine,
        engine_version=body.get("engineVersion"),
        languages=body.get("languages"),
        raw_text=raw_text,
        user_corrected_text=body.get("userCorrectedText"),
    )
    db.add(recognition)
    db.commit()

    return JSONResponse(
        content={"data": _recognition_to_dict(recognition)},
        status_code=status.HTTP_201_CREATED,
    )


# ── Run Endpoints ────────────────────────────────────────────────


@router.post("/{note_id}/runs")
async def create_run(
    note_id: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
    db: Session = Depends(get_db),
    settings: Settings = Depends(get_settings),
):
    """Start an enrichment run. Idempotent on clientRunId.

    The run row snapshots everything the connector needs to execute the turn:
    the OCR text, the directives, and the staged attachment files (the rendered
    drawing first, then the note's photo/scan files). Attachment bytes are
    written to the run's staging directory and only their metadata is persisted,
    so the row stays small while a retry can still re-dispatch the exact input.
    """
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

    recognized_text = body.get("recognizedText") or ""
    directives = body.get("directives") or []
    allowed_directives = [
        directive
        for directive in directives
        if isinstance(directive, dict)
        and str(directive.get("command", "")).lower() in V1_COMMAND_ALLOWLIST
    ]

    run_id = str(uuid4())
    staged, staging_errors = _stage_attachments(
        settings=settings,
        run_id=run_id,
        attachments=body.get("attachments") or [],
    )
    if staging_errors and not staged:
        raise HTTPException(
            status_code=status.HTTP_413_REQUEST_ENTITY_TOO_LARGE,
            detail="; ".join(staging_errors),
        )

    run = NoteRun(
        id=run_id,
        user_id=auth.user.id,
        note_id=note_id,
        client_run_id=str(client_run_id),
        source_drawing_revision=body.get("sourceDrawingRevision", note.current_drawing_revision),
        source_text_revision=body.get("sourceTextRevision", note.current_text_revision),
        requested_directives=allowed_directives,
        recognized_text=recognized_text,
        request_payload={
            "title": body.get("title") or note.title or "",
            "model": body.get("enrichmentModel") or None,
            "provider": body.get("enrichmentProvider") or None,
            "locale": body.get("locale") or "en-US",
            "timezone": body.get("timezone") or "America/Los_Angeles",
            "attachments": staged,
            "rejectedCommands": [
                str(directive.get("command"))
                for directive in directives
                if isinstance(directive, dict)
                and str(directive.get("command", "")).lower() not in V1_COMMAND_ALLOWLIST
            ],
            "stagingWarnings": staging_errors,
        },
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
        payload={"noteId": note_id, "clientRunId": client_run_id, "attachments": len(staged)},
    )
    db.commit()

    append_note_run_event(
        db,
        run_id=run.id,
        event_type="queued",
        payload={
            "runId": run.id,
            "noteId": note_id,
            "status": "queued",
            "attachments": len(staged),
        },
        attempt=0,
    )

    return JSONResponse(content={"data": _run_to_dict(run)}, status_code=status.HTTP_201_CREATED)


@router.get("/{note_id}/enrichment")
def get_current_enrichment(
    note_id: str,
    auth: AuthContext = Depends(get_auth_context),
    db: Session = Depends(get_db),
):
    """Return the newest enriched revision for a note (the Enriched tab's body).

    A stale revision is only returned when no fresh one exists, and it is
    flagged so the app can badge it instead of silently showing outdated work.
    """
    _get_note_or_404(db, note_id, auth.user.id)

    revision = db.execute(
        select(EnrichedNoteRevision)
        .where(EnrichedNoteRevision.note_id == note_id)
        .order_by(EnrichedNoteRevision.is_stale.asc(), EnrichedNoteRevision.created_at.desc())
    ).scalars().first()

    if revision is None:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="No enrichment yet.")

    return {"data": _enriched_revision_to_dict(revision)}


@note_runs_router.get("/{run_id}")
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


@note_runs_router.get("/{run_id}/events")
def get_run_events(
    run_id: str,
    request: Request,
    auth: AuthContext = Depends(get_auth_context),
    db: Session = Depends(get_db),
):
    """Get run events with cursor-based pagination.

    Supports durable replay via Last-Event-ID header (preferred) or
    ?after= query param (fallback).
    """
    run = db.get(NoteRun, run_id)
    if run is None or run.user_id != auth.user.id:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Run not found.")

    # Determine replay cursor: Last-Event-ID header takes precedence
    cursor: int | None = None
    last_event_id = request.headers.get("last-event-id")
    if last_event_id is not None:
        try:
            cursor = int(last_event_id)
        except (ValueError, TypeError):
            pass

    if cursor is None:
        after_seq = request.query_params.get("after")
        if after_seq is not None:
            try:
                cursor = int(after_seq)
            except (ValueError, TypeError):
                pass

    limit = min(int(request.query_params.get("limit", "100")), 500)

    query = select(NoteRunEvent).where(NoteRunEvent.run_id == run_id)
    if cursor is not None:
        query = query.where(NoteRunEvent.seq > cursor)
    query = query.order_by(NoteRunEvent.seq).limit(limit)

    events = db.execute(query).scalars().all()

    return {
        "data": [_event_to_dict(e) for e in events],
        "meta": {"count": len(events), "runId": run_id},
    }


@note_runs_router.post("/{run_id}/cancel")
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

# v1 command allowlist — enforced here, relay-side, before any dispatch
# (mirrors connector/kallisti_connector/note_contract.py; the relay owns the
# rejection so an unknown tag is data and never reaches the model as intent).
V1_COMMAND_ALLOWLIST = frozenset({
    "research",
    "search",
    "talkingpoints",
    "summary",
    "actions",
    "questions",
})

_UNSAFE_FILENAME_RE = re.compile(r"[^A-Za-z0-9._-]+")


def _safe_filename(name: str, fallback: str) -> str:
    """Collapse a client-supplied filename to something safe to write to disk."""
    candidate = _UNSAFE_FILENAME_RE.sub("_", (name or "").strip())[:120]
    return candidate or fallback


def _staging_root(settings: Settings) -> Path:
    """Directory that holds per-run staged attachment files."""
    if settings.note_run_staging_dir:
        return Path(settings.note_run_staging_dir).expanduser()
    if settings.database_url.startswith("sqlite"):
        raw = settings.database_url.split("sqlite:///", 1)[-1].split("?", 1)[0]
        if raw:
            return Path(raw).expanduser().resolve().parent / "note_runs"
    return Path.home() / ".hermes-kallisti-relay" / "note_runs"


def _stage_attachments(
    *,
    settings: Settings,
    run_id: str,
    attachments: list,
) -> tuple[list[dict], list[str]]:
    """Write note attachments to the run's staging directory.

    Returns (staged metadata, warnings). A rejected attachment yields a warning
    and the rest still run — an oversized photo must not block the drawing.
    """
    staged: list[dict] = []
    warnings: list[str] = []
    if not attachments:
        return staged, warnings

    root = _staging_root(settings) / run_id
    try:
        root.mkdir(parents=True, exist_ok=True)
    except OSError as error:
        return [], [f"staging directory unavailable: {error}"]

    for index, attachment in enumerate(attachments, start=1):
        if not isinstance(attachment, dict):
            warnings.append(f"attachment {index} is not an object")
            continue
        if len(staged) >= settings.note_run_max_attachments:
            warnings.append(
                f"attachment limit ({settings.note_run_max_attachments}) reached; extra attachments ignored"
            )
            break

        data_b64 = attachment.get("data") or ""
        if not data_b64:
            warnings.append(f"attachment {index} had no data")
            continue
        try:
            raw = base64.b64decode(data_b64, validate=True)
        except Exception:  # noqa: BLE001
            warnings.append(f"attachment {index} was not valid base64")
            continue

        if len(raw) > settings.note_run_max_attachment_bytes:
            warnings.append(
                f"attachment {index} exceeded {settings.note_run_max_attachment_bytes} bytes"
            )
            continue

        mime_type = str(attachment.get("mimeType") or "application/octet-stream")
        suffix = ""
        if "/" in mime_type:
            default_ext = {
                "image/jpeg": ".jpg",
                "image/png": ".png",
                "image/heic": ".heic",
                "application/pdf": ".pdf",
                "text/plain": ".txt",
            }.get(mime_type, "")
            suffix = default_ext
        filename = _safe_filename(
            str(attachment.get("filename") or f"attachment-{index}{suffix}"),
            f"attachment-{index}{suffix}",
        )
        path = root / filename
        try:
            path.write_bytes(raw)
        except OSError as error:
            warnings.append(f"attachment {index} could not be written: {error}")
            continue

        staged.append({
            "filename": filename,
            "mimeType": mime_type,
            "byteSize": len(raw),
            "path": str(path),
        })

    return staged, warnings


def _enriched_revision_to_dict(revision: EnrichedNoteRevision) -> dict:
    return {
        "id": revision.id,
        "noteId": revision.note_id,
        "runId": revision.run_id,
        "sourceDrawingRevision": revision.source_drawing_revision,
        "sourceTextRevision": revision.source_text_revision,
        "schemaVersion": revision.schema_version,
        "title": revision.title,
        "markdown": revision.markdown,
        "sections": revision.structured_sections or [],
        "citations": revision.citations or [],
        "commandResults": revision.command_results or [],
        "isStale": revision.is_stale,
        "createdAt": revision.created_at.isoformat() if revision.created_at else None,
    }


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
        "revision": note.revision,
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


def _recognition_to_dict(rec: NoteRecognition) -> dict:
    return {
        "id": rec.id,
        "noteId": rec.note_id,
        "drawingRevision": rec.drawing_revision,
        "engine": rec.engine,
        "engineVersion": rec.engine_version,
        "languages": rec.languages,
        "rawText": rec.raw_text,
        "userCorrectedText": rec.user_corrected_text,
        "createdAt": rec.created_at.isoformat() if rec.created_at else None,
    }
