# Herald Notes v1 — Contract & SDK Reference

**Prepared:** 2026-07-20
**Baseline:** HEAD `1147c68` (`v1.7.1 / 37`)

---

## 1. SDK Availability Table (N0.2 — Xcode 26.6 / iOS SDK 26.5)

All annotations below are from the **installed SDK headers**, not documentation inference. Deployment target is iOS 18.0 (`project.yml:18`).

| API | Availability | Notes |
|-----|-------------|-------|
| `PKCanvasView` | iOS 13.0+ | Core canvas; `drawingPolicy` iOS 14.0+ |
| `PKCanvasViewDelegate` | iOS 13.0+ | `canvasViewDrawingDidChange:`, `canvasViewDidBeginUsingTool:`, `canvasViewDidEndUsingTool:` |
| `PKDrawing` | iOS 13.0+ | `dataRepresentation()`, `init(data:)`, `imageFromRect:scale:`, `drawingByAppendingStrokes:` (iOS 14.0+) |
| `PKStroke` | iOS 14.0+ | `init(path:ink:transform:mask:)` |
| `PKInkingTool` | iOS 14.0+ | `init(inkType:color:width:)` |
| `PKEraserTool` | iOS 14.0+ | `.vector` and `.bitmap` eraser types |
| `PKLassoTool` | iOS 14.0+ | Selection tool |
| `PKToolPicker` | iOS 14.0+ | `shared(for:)`, `stateAutosaveName`, `addObserver(_:)` |
| `PKToolPickerItem` | iOS 18.0+ | Base class for tool picker items |
| `PKToolPickerCustomItem` | iOS 18.0+ | Custom tool items with `imageProvider`, `viewControllerProvider`, width/color controls |
| `PKToolPickerCustomItemConfiguration` | iOS 18.0+ | `ControlOptions` (.none, .width, .opacity), `defaultWidth`, `widthVariants`, `allowsColorSelection` |
| `PKToolPickerInkingItem` | iOS 14.0+ | Built-in inking tool item |
| `PKToolPickerEraserItem` | iOS 14.0+ | Built-in eraser tool item |
| `PKToolPickerRulerItem` | iOS 14.0+ | Ruler tool item |
| `PKToolPickerScribbleItem` | iOS 14.0+ | Scribble tool item |
| `PKCanvasView.drawingEnabled` | iOS 18.0+ | Controls if drawing input is enabled |
| `PKContentVersion` | iOS 17.0+ | `maximumSupportedContentVersion` on canvas/tool picker |
| `UIPencilInteraction` | iOS 12.1+ | `preferredTapAction`, `prefersPencilOnlyDrawing` |
| `UIPencilInteraction.preferredSqueezeAction` | iOS 17.5+ | Pencil Pro squeeze gesture |
| `UIPencilInteraction.Phase` | iOS 17.5+ | `.began`, `.changed`, `.ended`, `.cancelled` |
| `UIPencilInteraction.prefersHoverToolPreview` | iOS 17.5+ | Hover preview preference |
| `UIPencilInteraction(delegate:)` | iOS 17.5+ | Delegate-based interaction |
| `VNRecognizeTextRequest` | iOS 13.0+ | Vision framework text recognition (fallback path) |
| `VNRecognizedTextObservation` | iOS 13.0+ | Recognition results with `topCandidates(_:)` |
| **`PKStrokeRecognizer`** | **Not in public SDK** | **Not available in iOS SDK 26.5 headers. No public PencilKit handwriting recognition API exists.** The spec's mention is either forward-looking or based on an internal API. Use Vision framework (`VNRecognizeTextRequest`) as the recognition path. |

### Key Findings

1. **`PKStrokeRecognizer` does not exist in the public SDK.** The spec says "gate iOS 26-era APIs (`PKStrokeRecognizer`) behind `#available`" but this type is absent from the PencilKit headers entirely. Recognition must use the **Vision framework** path (`VNRecognizeTextRequest` + `PKDrawing.imageFromRect:scale:`) as the primary (and only) on-device recognition engine.

2. **`PKToolPickerCustomItem` is iOS 18.0+.** This is within the deployment target, so custom tool picker items (e.g., an "Enrich" button) can be used without availability guards.

3. **`UIPencilInteraction` squeeze is iOS 17.5+.** Also within the deployment target. The `preferredSqueezeAction` property and `Phase` enum are available.

4. **`PKCanvasView.drawingEnabled` is iOS 18.0+.** Available at the deployment target for toggling drawing input.

5. **All other PencilKit APIs** needed for the Notes feature (canvas, drawing, strokes, tool picker, eraser, lasso) are available since iOS 14.0, well within the iOS 18.0 baseline.

---

## 2. Recognition Strategy (revised from spec)

Given that `PKStrokeRecognizer` is not available:

| Path | Engine | Availability | Accuracy | Speed |
|------|--------|-------------|----------|-------|
| **Primary** | `VNRecognizeTextRequest` (`.accurate`) | iOS 13.0+ | High for printed/mixed | ~1-3s per page |
| **Fast** | `VNRecognizeTextRequest` (`.fast`) | iOS 13.0+ | Lower for cursive | ~0.3-1s per page |

Both paths render the `PKDrawing` to an image via `PKDrawing.imageFromRect:scale:` at Letter/A4 point scale, then feed to Vision. No PencilKit-native recognition API is available to prefer.

**Recommendation for Phase 2:** Use `.accurate` as default, with `.fast` as an option for large drawings. Surface confidence scores from `VNRecognizedText.topCandidates(3)` to allow the UI to flag low-confidence regions.

---

## 3. Physical Spike Results (N0.3 — pending hardware)

> **Status:** BLOCKED — requires physical iPad with Apple Pencil. Operator must run these tests:
>
> 1. `PKDrawing.dataRepresentation()` round-trip fidelity on a multi-page drawing
> 2. Recognition quality on representative handwriting at Letter/A4 point scale
> 3. Language support list for `VNRecognizeTextRequest`
> 4. Recognition cancellation behavior
> 5. Memory profile with a large multi-page drawing (10+ pages)
> 6. Vision fallback accuracy vs. any future native recognition

---

## 4. Request/Response Schemas

### 4.1 Enrichment Request (client → relay → connector)

```json
{
  "schemaVersion": 1,
  "noteId": "uuid",
  "clientRunId": "uuid",
  "sourceDrawingRevision": 12,
  "sourceTextRevision": 8,
  "recognizedText": "corrected OCR text",
  "directives": [
    {
      "id": "stable-id",
      "command": "research",
      "arguments": "battery supply chain",
      "sourceRange": {
        "location": 94,
        "length": 39
      }
    }
  ],
  "locale": "en-US",
  "timezone": "America/Los_Angeles"
}
```

**Field notes:**
- `noteId`: UUID v4, client-generated, stable across sync
- `clientRunId`: UUID v4, client-generated, used for idempotency (deduplication)
- `sourceDrawingRevision` / `sourceTextRevision`: revision fence — result applies only if these still match at completion
- `recognizedText`: the `userCorrectedText` if available, else raw OCR output
- `directives`: parser-produced directives from the recognized text (never raw OCR tags)
- `locale` / `timezone`: for command execution context

### 4.2 Enrichment Result (connector → relay → client)

```json
{
  "schemaVersion": 1,
  "title": "Battery Supply Chain Research",
  "markdown": "# Battery Supply Chain Research\n\n## Summary\n...",
  "sections": [
    {
      "kind": "summary",
      "title": "Summary",
      "markdown": "..."
    },
    {
      "kind": "command_result",
      "title": "Research: battery supply chain",
      "markdown": "..."
    }
  ],
  "commandResults": [
    {
      "directiveId": "stable-id",
      "status": "completed",
      "sectionIndex": 1
    }
  ],
  "citations": [
    {
      "title": "Article Title",
      "url": "https://example.com/article",
      "accessedAt": "2026-07-20T12:00:00Z"
    }
  ],
  "warnings": []
}
```

**Field notes:**
- `schemaVersion`: always 1 for v1
- `title`: generated title for the enriched document
- `markdown`: full rendered document
- `sections`: structured sections; `kind` is one of `summary`, `command_result`, `freeform`
- `commandResults`: maps each directive to its result status and section index
- `citations`: web results require citations with access dates; no claimed source without a tool result
- `warnings`: non-fatal issues (e.g., unsupported command, low-confidence OCR)

### 4.3 Directive Grammar (v1)

```ebnf
directive     = line-start, ws*, "#", command, [ws, arguments], line-end ;
command       = <case-insensitive ASCII token from v1 allowlist> ;
arguments     = <rest of line after command, trimmed> ;
```

**v1 command allowlist (read-only):**
- `#research` — web research on a topic
- `#search` — web search for information
- `#talkingpoints` — generate talking points
- `#summary` — summarize the note content
- `#actions` — extract action items
- `#questions` — generate questions from the content

**Rules:**
- `#` mid-sentence, in URLs, in code blocks, or in struck-through regions is text, not a directive
- Unknown tags (not in the allowlist) render as "unrecognized tag" and are never sent as intent
- Parser runs on `userCorrectedText` when present, else raw OCR text
- Each directive gets a stable ID and a normalized fingerprint over `(noteId, sourceTextRevision, command, arguments, sourceRange)`
- OCR churn and save retries cannot duplicate execution (fingerprint deduplication)

---

## 5. Backend Decision (Gate 0.C)

> **Status:** PENDING — operator must decide between:
>
> **Option A — SwiftData (recommended):**
> - iOS 18.0 baseline supports it
> - No existing Core Data/SwiftData in the repo (deliberate introduction)
> - Automatic persistence, undo support, CloudKit-ready if needed later
> - Note: SwiftData is only for the local note index/metadata; drawing blobs are always atomic files in Application Support
>
> **Option B — SQLite (custom repository):**
> - More control over schema and migrations
> - Consistent with the relay's SQLite backend
> - Requires manual CRUD implementation
>
> **Regardless of choice:**
> - `PKDrawing` bytes are always atomic files in `Application Support/Herald/Notes/<noteId>/rev-<n>.pkdrawing`
> - Rendered page images (if cached) are also files, never UserDefaults
> - The `HeraldCanvasStore` pattern (one string artifact per session in UserDefaults) is explicitly off-limits

---

## 6. Relay Schema (Phase 3 preview)

New tables — ride `Base.metadata.create_all`; no ALTERs to existing tables.

```sql
-- notes: local mirror of iOS note metadata
CREATE TABLE notes (
    id TEXT PRIMARY KEY,              -- UUID v4
    user_id TEXT NOT NULL REFERENCES users(id),
    title TEXT NOT NULL DEFAULT '',
    folder_id TEXT,
    pinned INTEGER NOT NULL DEFAULT 0,
    current_drawing_revision INTEGER NOT NULL DEFAULT 0,
    current_text_revision INTEGER NOT NULL DEFAULT 0,
    created_at DATETIME NOT NULL DEFAULT (datetime('now')),
    updated_at DATETIME NOT NULL DEFAULT (datetime('now')),
    deleted_at DATETIME
);

-- note_blobs: PKDrawing binary storage
CREATE TABLE note_blobs (
    id TEXT PRIMARY KEY,
    note_id TEXT NOT NULL REFERENCES notes(id),
    drawing_revision INTEGER NOT NULL,
    content_hash TEXT NOT NULL,        -- SHA-256 hex
    byte_size INTEGER NOT NULL,
    storage_path TEXT NOT NULL,
    created_at DATETIME NOT NULL DEFAULT (datetime('now')),
    UNIQUE (note_id, drawing_revision)
);

-- note_recognitions: immutable OCR snapshot
CREATE TABLE note_recognitions (
    id TEXT PRIMARY KEY,
    note_id TEXT NOT NULL REFERENCES notes(id),
    drawing_revision INTEGER NOT NULL,
    engine TEXT NOT NULL,              -- "vn_accurate" | "vn_fast"
    engine_version TEXT,
    languages TEXT,                    -- JSON array of ISO codes
    raw_text TEXT NOT NULL,
    user_corrected_text TEXT,
    created_at DATETIME NOT NULL DEFAULT (datetime('now'))
);

-- note_runs: enrichment execution tracking
CREATE TABLE note_runs (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES users(id),
    note_id TEXT NOT NULL REFERENCES notes(id),
    client_run_id TEXT NOT NULL,       -- idempotency key
    source_drawing_revision INTEGER NOT NULL,
    source_text_revision INTEGER NOT NULL,
    requested_directives TEXT NOT NULL, -- JSON array
    status TEXT NOT NULL DEFAULT 'queued',  -- queued|claimed|completed|failed|cancelled
    attempt INTEGER NOT NULL DEFAULT 0,
    lease_expires_at DATETIME,
    error_text TEXT,
    result TEXT,                       -- JSON enrichment result
    created_at DATETIME NOT NULL DEFAULT (datetime('now')),
    completed_at DATETIME,
    UNIQUE (user_id, client_run_id)
);

-- note_run_events: durable event log (mirrors job_events)
CREATE TABLE note_run_events (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES note_runs(id),
    seq INTEGER NOT NULL,
    attempt INTEGER NOT NULL,
    source_seq INTEGER,
    type TEXT NOT NULL,
    payload_json TEXT NOT NULL,        -- JSON
    created_at DATETIME NOT NULL DEFAULT (datetime('now'))
);

-- enriched_note_revisions: immutable enriched document history
CREATE TABLE enriched_note_revisions (
    id TEXT PRIMARY KEY,
    note_id TEXT NOT NULL REFERENCES notes(id),
    run_id TEXT NOT NULL REFERENCES note_runs(id),
    source_drawing_revision INTEGER NOT NULL,
    source_text_revision INTEGER NOT NULL,
    schema_version INTEGER NOT NULL DEFAULT 1,
    title TEXT NOT NULL,
    markdown TEXT NOT NULL,
    structured_sections TEXT NOT NULL,  -- JSON
    citations TEXT,                    -- JSON
    command_results TEXT,              -- JSON
    is_stale INTEGER NOT NULL DEFAULT 0,
    created_at DATETIME NOT NULL DEFAULT (datetime('now'))
);
```

---

## 7. API Endpoints (Phase 3 preview)

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/v1/notes` | List notes (owner-scoped, excludes hard-deleted) |
| `POST` | `/v1/notes` | Create a note |
| `GET` | `/v1/notes/{noteId}` | Get note metadata |
| `PATCH` | `/v1/notes/{noteId}` | Update note (requires `If-Match` on revision → 412 on mismatch) |
| `DELETE` | `/v1/notes/{noteId}` | Soft delete; purge after 30 days |
| `PUT` | `/v1/notes/{noteId}/blobs/{revision}` | Upload PKDrawing bytes; `X-Content-SHA256` verified; 25 MB cap |
| `GET` | `/v1/notes/{noteId}/blobs/{revision}` | Download PKDrawing bytes |
| `POST` | `/v1/notes/{noteId}/recognitions` | Store OCR recognition result |
| `POST` | `/v1/notes/{noteId}/runs` | Start enrichment run (idempotent on `clientRunId`) |
| `GET` | `/v1/note-runs/{runId}` | Get run status |
| `GET` | `/v1/note-runs/{runId}/events` | Cursor + replay (same semantics as job events) |
| `POST` | `/v1/note-runs/{runId}/cancel` | Cancel run (terminal, idempotent) |

---

## 8. Run Lifecycle & Revision Fence

1. **Submit:** `POST …/runs` snapshots corrected OCR text + directives into the run row (immutable input)
2. **Claim:** connector claims via lease (same as `message_jobs` claim semantics)
3. **Execute:** connector dispatches to Hermes with the enrichment prompt
4. **Complete:** result becomes the current `EnrichedNoteRevision` **only if** `(source_drawing_revision, source_text_revision)` still match the note's current revisions; otherwise `is_stale = true`
5. **Cancel:** terminal; no further processing
6. **Lease expiry:** requeue follows `requeue_expired_message_jobs` semantics — accepted-but-slow is not failure

---

## 9. Invariants (v1)

1. Ink is canonical — OCR/enrichment can never overwrite or delete drawing data
2. Atomic local save — crash yields prior or next complete revision, never a partial blob
3. Revision fence — results apply only to the exact source revisions they declare
4. Idempotent run — one `clientRunId` ⇒ at most one remote execution
5. Transport is not execution — disconnect/timeout neither respawns nor false-fails a live run
6. OCR detection alone never triggers a remote tool
7. Allowlist before prompt — enforced relay-side; unknown tags are data
8. v1 tools are read-only
9. Least disclosure — remote payload is corrected OCR + directive context; rendered image only when recognition needs visual disambiguation, and opt-in
10. Auditable provenance — every generated section retains source revisions, directive, citations, run id
11. Private logs — no blobs, full OCR, prompts, or generated docs in logs/telemetry
12. Graceful degradation — writing and local viewing work with no Pencil, no host, no network, unsupported language
