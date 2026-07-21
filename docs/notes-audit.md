# Notes Implementation Audit

Audit of all Notes-related components for Herald 1.8.0 planning.

## 1. Summary Table

| File | Type | Status | Issues |
|------|------|--------|--------|
| **Models** | | | |
| HeraldNote.swift | HeraldNote | ✅ Complete | Custom decoder for backward compat |
| HeraldNote.swift | NoteDrawingRevision | ⚠️ Dead | Defined but never instantiated |
| HeraldNote.swift | NoteSyncState | ✅ Complete | — |
| HeraldNote.swift | NotePageStyle | ✅ Complete | — |
| HeraldNote.swift | NoteFolder | ⚠️ Dead | Defined but never used |
| HeraldNote.swift | NoteAttachment | ✅ Complete | — |
| HeraldNote.swift | NoteAttachmentType | ✅ Complete | — |
| NoteDirective.swift | NoteDirective | ✅ Complete | — |
| NoteDirective.swift | NoteCommand | ✅ Complete | v1 allowlist: 6 commands |
| NoteDirective.swift | DirectiveStatus | ✅ Complete | — |
| NoteDirective.swift | NoteCommandResult | ✅ Complete | — |
| NoteRecognition.swift | NoteRecognition | ✅ Complete | — |
| NoteRecognition.swift | RecognitionEngine | ✅ Complete | — |
| **Views** | | | |
| NotesWorkspaceView.swift | NotesWorkspaceView | ✅ Complete | Wired via AdaptiveRootView |
| NotesListView.swift | NotesListView | ✅ Complete | Search, sort, context menu |
| NoteEditorView.swift | NoteEditorView | ✅ Complete | Canvas + attachments |
| PencilCanvasRepresentable.swift | PencilCanvasRepresentable | ✅ Complete | KVO paper sync |
| NotePaperBackground.swift | NotePaperBackground | ⚠️ Dead | Unused; canvas uses NotePaperUIView |
| RecognizedTextReviewView.swift | RecognizedTextReviewView | ✅ Complete | — |
| **Services/Stores** | | | |
| NotesStore.swift | NotesStore | ✅ Complete | @Observable, injected via AppContainer |
| NotesRepository.swift | NotesRepository | ✅ Complete | Atomic writes, SHA-256, monotonic rev |
| NoteDirectiveParser.swift | NoteDirectiveParser | ✅ Complete | Versioned allowlist, fingerprints |
| NoteRecognitionCoordinator.swift | NoteRecognitionCoordinator | ✅ Complete | Revision-tied, cancel stale |
| LiveNotesClient.swift | LiveNotesClient | ⚠️ Partial | Missing updateNote, deleteNote |
| — | NotesClient protocol | ❌ Missing | No protocol; concrete actor only |
| **Relay** | | | |
| relay/app/notes.py | CRUD endpoints | ⚠️ Partial | Blob upload/download are stubs |
| relay/app/notes.py | Run endpoints | ✅ Complete | Create, get, events, cancel |
| relay/app/notes.py | Optimistic concurrency | ✅ Complete | If-Match on PATCH |
| relay/app/models.py | Note models | ✅ Complete | 6 tables with constraints |
| relay/app/database.py | Migrations | ⚠️ Partial | Inline SQL, no Alembic |
| **Connector** | | | |
| note_contract.py | EnrichmentRequest | ✅ Complete | Validation, from_dict |
| note_contract.py | EnrichmentResult | ✅ Complete | Validation, round-trip |
| note_contract.py | V1_COMMAND_ALLOWLIST | ✅ Complete | 6 commands |
| client.py | _rpc_note_enrich | ✅ Complete | Wired into dispatch |
| **Tests** | | | |
| NoteDirectiveParserTests.swift | 14 tests | ✅ Complete | All v1 commands covered |
| NotesRepositoryTests.swift | 12 tests | ✅ Complete | CRUD, blobs, hash, recovery |
| relay/tests/test_notes.py | 12 tests | ✅ Complete | Lifecycle, events, fence |
| connector/tests/test_note_contract.py | 9 tests | ✅ Complete | Fixtures, validation |

## 2. Complete Behaviors (Ready for 1.8.0)

- **Note metadata CRUD**: HeraldNote with all fields, soft-delete, 30-day purge
- **Drawing persistence**: Atomic blob writes, SHA-256 hashing, monotonic revision numbers
- **Directive parsing**: Pure parser with versioned allowlist, fingerprint deduplication
- **Handwriting recognition**: Revision-tied coordinator, stale task cancellation
- **Attachment support**: Photo picker, document scanner, thumbnail strip
- **Paper styles**: 7 styles (blank, 3 ruled, 3 grid), dark/light mode
- **Note list UI**: Search, sort (3 modes), pin, delete/restore, context menu
- **Note editor UI**: Title editing, canvas, paper picker, attachment menu
- **Connector enrichment**: Full request/response contract, validation, dispatch
- **Test coverage**: 47 tests across iOS, relay, connector

## 3. Partial Behaviors (Need Completion)

- **LiveNotesClient**: Has list/create/get but missing update/delete endpoints
  - Impact: iOS cannot sync note updates or deletions to relay
  - Fix: Add `updateNote()` and `deleteNote()` methods

- **Relay blob upload** (`PUT /notes/{id}/blobs/{revision}`): Creates metadata record but has TODO for actual byte storage and hash verification
  - Impact: Blobs are tracked but bytes aren't persisted
  - Fix: Implement storage backend (S3 or local), read body, verify X-Content-SHA256

- **Relay blob download** (`GET /notes/{id}/blobs/{revision}`): Returns metadata only, has TODO for actual byte return
  - Impact: iOS cannot download blobs from relay
  - Fix: Return blob bytes from storage with proper content-type

- **Relay migrations**: Uses `create_all()` + inline SQL in `_run_migrations()`, no Alembic
  - Impact: Schema changes require manual migration code
  - Note: Works for now; may need Alembic for production

## 4. Dead/Unwired Code

- **NoteDrawingRevision** (HeraldNote.swift:76–108): Defined with full properties but never instantiated. The repository tracks revisions via blob filenames (`rev-{n}.pkdrawing`) without creating NoteDrawingRevision instances. Either wire it into the repository or remove.

- **NoteFolder** (HeraldNote.swift:193–210): Defined but `folderId` on HeraldNote is always nil. No UI for creating/managing folders. Either implement folder support or remove.

- **NotePaperBackground** (NotePaperBackground.swift): SwiftUI Canvas version of paper background. PencilCanvasRepresentable uses `NotePaperUIView` (UIKit) instead. Either use this for SwiftUI previews or remove.

## 5. Missing Components (Need New Implementation)

- **NotesClient protocol**: No abstraction over LiveNotesClient. Tests use concrete NotesRepository directly. If testing or mocking is needed, define a protocol.

- **Sync engine**: NoteSyncState enum exists with 5 cases but no actual sync logic. Notes are local-only. The LiveNotesClient exists but isn't called from NotesStore.

- **Enrichment result display**: RecognizedTextReviewView shows directives but no view for displaying enrichment results (markdown, sections, citations).

- **Run progress UI**: No view for showing enrichment run status (queued/claimed/completed/failed).

## 6. Key Architectural Notes

- **Repository pattern**: NotesRepository is the ONLY writer for note data (enforced by actor isolation)
- **Atomic writes**: All file writes use `.atomic` option — crash yields prior or next complete state
- **Revision model**: Caller-managed monotonic integers, not server-assigned
- **Fingerprint dedup**: Directive fingerprints are SHA-256 over (noteId, revision, command, arguments, range)
- **Environment injection**: NotesStore injected via `@Environment(NotesStore.self)`, created in AppContainer
- **No protocol for NotesClient**: LiveNotesClient is concrete; no mock/stub for testing
