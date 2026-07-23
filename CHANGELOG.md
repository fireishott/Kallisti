# Changelog

## [2.1.0] - 2026-07-22

### Added

- **Remote MCP server** — Herald's MCP server now runs Streamable HTTP
  transport (port 8767) instead of stdio, enabling Hermes to connect over
  the network without SSH tunnels or the herald-mcp binary on the Hermes
  host.
- **MCP lifecycle integration** — MCP HTTP server starts automatically
  with the connector alongside the WebSocket relay.
- **Remote MCP registration** — `herald configure-mcp` now defaults to
  remote HTTP mode; `--no-remote` preserves legacy stdio registration.
- **MCP HTTP validation** — `herald validate-mcp` can health-check
  remote HTTP MCP servers.

### Changed

- `herald-mcp` CLI defaults to `streamable-http` transport. Use
  `--transport stdio` for legacy mode.
- Connector version bumped to 0.2.0.

## [2.0.4] - 2026-07-22

### Fixed

- **Streaming visibility** (B1/B6): `scrollToBottom()` now targets the last
  message in the conversation (not the last user message), so streaming
  responses are visible as they arrive. Added scroll-during-streaming logic
  to keep the viewport following the growing response.
- **Phantom thinking** (B2): App now detects and recovers stalled SSE streams
  on foreground return. If the server completed a response while the app was
  backgrounded, it's immediately displayed instead of showing perpetual
  "Thinking...".
- **Chat open scroll** (B3): Opening a conversation now scrolls to the absolute
  bottom of activity, not the last sent message.
- **Send scroll guard** (B8): Removed `isComposerFocused` guard that could
  prevent scroll-to-bottom after sending a message.

### Added

- **Notes infinite scroll** (B4): PencilKit canvas now auto-extends as the
  user approaches the bottom, matching Apple Notes infinite-scroll behavior.
  Paper lines/grid extend with the canvas.
- **Apple Pencil menu fix** (B5): Migrated from deprecated
  `PKToolPicker.shared(for:)` to instance-based API. Tool picker now
  positions correctly on all iPad configurations and survives app lifecycle.
- **Live gateway state** (B7): Real-time SSE subscription for host status
  while foregrounded (replaces 10s polling). Connection state, model, and
  profile changes reflect within 1-2 seconds.

## [2.0.2] - 2026-07-22

### Fixed

- **Health permissions N/A** (B1): Entitlement probe no longer misidentifies a
  fresh install as missing entitlements. `.errorAuthorizationNotDetermined` is
  now treated as a valid pre-prompt state, not an entitlement failure.
- **Talk mode crash** (B2): Audio capture guards against double-start, API key
  holder refreshes before readiness check, playback prepare failures are caught
  instead of crashing.
- **Permissions copy** (B5): Updated "hermes works best" to "herald and hermes
  work together best" on the Permissions screen.
- **Motion activity not persisting** (B6): `reloadCapabilities()` now re-reads
  CoreMotion authorization status, so motion permission survives screen
  transitions.
- **Connection verification** (B7): Confirmed v2.0.1 relay URL fix is in HEAD;
  added re-pair guidance for devices with cached stale URLs.

### Added

- **Streaming toggle** (B4): New "Streaming" toggle in Settings → Preferences
  lets users choose between progressive streaming and full-response delivery.

### Changed

- **Default conversation title** (B3): New conversations are titled "New Chat"
  instead of "Herald" to avoid confusion with the app name.

## [2.0.1] - 2026-07-22

### Fixed

- Fix connection failure: restore relay URL from Gemini wrapper endpoint to production relay

## [2.0.0] - 2026-07-22

### Architecture

- **Native Hermes Relay Channel**: Replaced the FastAPI relay (Docker, SQLite)
  with a WebSocket server inside the connector that speaks the Hermes Relay
  Protocol. The Hermes gateway now dials directly into the connector on
  port 8765. No custom platform code in Hermes. No fork.
- **Launch fix**: App no longer gets stuck on "Connecting to Herald..." after
  re-pair. The `.networkFailure` pattern match now handles any error message,
  not just the empty string.

### Changed

- Connector now runs as a WebSocket server (`relay_server.py`) instead of
  polling the relay for jobs.
- Removed dead modules: `hermes_runner.py`, `hermes_gateway_executor.py`,
  `stream_contract.py`.
- `APP_HOSTED_RELAY_URL` now points to connector HTTP API on port 8766.
- Bumped version to 1.9.0 (build 46).

### Fixed

- **B-LAUNCH**: `isLaunchReady` in `AppContainer.swift` now matches any
  `.networkFailure` case, not just `.networkFailure("")`. The retry/re-pair
  screen appears correctly when the relay is unreachable.

## [1.8.2] - 2026-07-22

Point release addressing 15 bugs across Settings, Chat, Notes, Talk,
theme, push notifications, and onboarding.

### Fixed

- **Settings X close button** (B25): X button now only appears when
  Settings is presented as a sheet; hidden when accessed via tab bar.
- **Host selection navigation** (B26): "Hermes Host" row now uses
  NavigationLink within the Settings NavigationStack instead of pushing
  onto the Chat tab's stack.
- **Light/Dark/System theme** (B19): Design.Colors now reads from
  ThemeManager palette. ThemeAwareRootView applies .preferredColorScheme
  based on user preference.
- **Session disappearance guard** (B27): loadConversation no longer
  silently replaces a loaded conversation with a different one from the
  relay.
- **Talk mode crash guard** (B28): startSession now checks coordinator
  state after startListeningWithVAD and surfaces errors to the UI.
- **Reasoning toggle** (B29): .thinking segments are now filtered when
  showReasoning is false, matching the toggle's intent.
- **Scroll-to-bottom on send** (B14): Added delay before scrolling to
  let SwiftUI insert the new message. Also scrolls to bottom on initial
  chat load.
- **Notes switching** (B30): NoteEditorView now uses .id(noteId) to
  force view recreation on note switch, preventing stale @State.
- **Notes scrolling** (B30): Wrapped editor content in ScrollView.
- **Notes title size** (B30): Reduced from .title2 to .headline with
  tighter padding.
- **Notes line spacing** (B30): Increased medium spacing from 24pt to
  30pt to match native Notes feel.
- **Custom wallpaper** (B31): Increased scrim opacity for custom photos
  to 0.65 for text legibility. Cached decoded UIImage to prevent
  re-decoding on every render.
- **Rebrand onboarding** (B32): Changed "HERALD · iOS" to "HERMES · iOS",
  "Herald" to "Hermes" in display text, removed HeraldMark and
  AppIconWatermark from welcome screen.
- **Health permission** (B33): Added border to N/A status pill in
  onboarding for better visibility.
- **Privacy/TOS URLs** (B34/B35): Added APP_PRIVACY_URL and
  APP_TERMS_URL to project.yml and Info.plist.

### Build

- Version: 1.8.2 (Build 45)
- Target: iOS 18.0+
- Swift: 6.2

## [1.8.1] - 2026-07-21

Patch release with Swift 6 concurrency fixes for Xcode 26 compatibility.
Resolves build failures when archiving with Swift 6.3.3 strict concurrency
checking enabled.

### Fixed

- **EmptyResponse type ambiguity**: Made `EmptyResponse` private in both
  `LiveNotesClient` and `LiveHeraldHostService` to resolve duplicate
  type definitions causing ambiguous lookup errors.
- **Main actor isolation in LiveNotesClient**: Wrapped `makeRequest` call
  in `MainActor.run` within `getRunEvents` to satisfy Swift 6 actor
  isolation requirements for `@MainActor`-isolated API client methods.
- **Sendable conformance**: Added `Sendable` to `RunEventsResponse`,
  `RunEventDTO`, and `AnyCodable` types to satisfy Swift 6 strict
  concurrency checking when crossing actor boundaries.

### Build

- Version: 1.8.1 (Build 44)
- Target: iOS 18.0+
- Swift: 6.3.3 (Xcode 26.6)
- TestFlight Delivery UUID: d28b6b36-efd9-4be5-a10a-b99d52b3e560

## [1.8.0] - 2026-07-21

Herald 1.8 introduces a local-first Notes workspace for iPad with native
PencilKit drawing tools, durable editable ink, on-device handwriting
recognition, reviewed note commands, and revision-safe Hermes enrichment.
This release also includes reliability fixes for multi-tool streaming,
push notifications, model picker, chat titles, and permission handling.

### Added

- **iPad Notes workspace** (PR1–PR5): Local-first notes with native
  PencilKit canvas, system tool picker, undo/redo, and full CRUD.
  Seven paper styles (blank, ruled small/medium/large, grid
  small/medium/large). Photo and document scanner attachments.
- **On-device handwriting recognition**: Vision framework recognition
  with debounced, cancellable, revision-versioned pipeline. Recognized
  text is always a derived layer — original ink is never modified.
- **Note directive parser**: Deterministic, allowlisted `#command`
  parsing from corrected recognition (`#research`, `#search`,
  `#talkingpoints`, `#summary`, `#actions`, `#questions`). Unknown
  tags are data, never intent.
- **Hermes enrichment for notes**: Idempotent, revision-fenced note
  runs with event replay, cancellation, and stale-result fencing.
- **Enriched document UI**: Ink / Recognized / Enriched view modes,
  directive progress, citations, immutable run history, and export
  (ink PDF, recognized text, enriched Markdown).
- **Notes relay API**: Full CRUD with If-Match optimistic concurrency,
  blob storage with SHA-256 verification, run lifecycle management,
  and cursor-based event replay.

### Fixed

- **Multi-tool streaming** (B1): Terminal content is now extracted from
  the documented payload and flushed after all pending deltas. SSE
  comments maintain transport liveness without advancing event
  sequence. Heartbeats continue through long tool gaps. Disconnect
  queries authoritative job status and resumes from the last durable
  cursor. Tool activity, reasoning, answer text, and terminal data
  remain typed and separate.
- **APNs push registration** (B7/B8): Re-registration on launch even
  when the local token hasn't changed. Per-environment routing
  preserves separate development and production token paths.
  Foreground suppression is per-device. Relaunch recovers server-
  inactive registrations without manual intervention.
- **iPad model picker** (B2): Model status now shows distinct loading,
  unavailable, and loaded states. No more silent `"..."` failure.
- **Profile-aware failure copy** (B3): Error messages now use the
  active profile name; falls back to "Herald" when unavailable.
- **Chat titles** (B4): Single title owner with deterministic local
  fallback, timeout/retry, and session-list sync. Older completions
  cannot overwrite user renames.
- **Speech service** (B10): Availability-guards both construction and
  API use. Unavailable services show a disabled, readable state
  instead of crashing.
- **HealthKit** (B6): Availability-gated at runtime. No longer infers
  read authorization from an empty query result.
- **Action Center** (B13): Inbox refreshes on push wake; dismissed
  and expired items are filtered from display.
- **Canvas stability**: Note-switching no longer recreates the
  `PKCanvasView`, preserving undo history. PDF export guards against
  force-unwrap crashes.

### Changed

- **Managed Relay removed** (B5): The unsupported Managed Relay choice
  is removed from onboarding and settings. Legacy `managedRelay` raw
  values decode safely and migrate to self-hosted.

### Known Limitations

- Handwriting recognition uses the Vision framework. Apple Notes
  Smart Script and handwriting refinement are not available as public
  APIs and are not embedded in Herald 1.8.
- HealthKit availability depends on distribution-signing entitlements;
  TestFlight builds may show unavailable state.

### Operational Notes

- Required relay minimum: commit containing the Notes API schema and
  `/v1/notes/*` endpoints.
- Required connector minimum: revision supporting `note.enrich` RPC
  and the Notes contract schema.
- APNs environment routing is per-registration; ensure the public
  relay's `.env` distinguishes TestFlight (production) from dev builds
  (sandbox) via `APNS_ENVIRONMENT`.

All notable changes to Hermes iOS are documented here.

## [1.8.1] - 2026-07-21

### Fix: Chat opens to most recent messages (B14)

- **Scroll-to-bottom on load** (`ChatScreen.swift`): Chat now scrolls to the most recent message when the screen appears, instead of showing the top of conversation history.

### Added: Scroll-to-bottom button (B15)

- **Floating chevron button** (`ChatScreen.swift`): When scrolled up in chat, a chevron-down button appears at the bottom center to quickly return to the latest messages.

### Fix: Thinking bubbles persist until response completes (B16)

- **Extended watchdog tolerance** (`ChatStore.swift`): Thinking dots and "Thinking... Xs" timer now persist as long as the relay reports the job is still active, instead of disappearing after 120s. Supports the known slow-host scenario.
- **Live Activity stays alive** (`ChatStore.swift`): Lock screen Live Activity no longer ends prematurely on watchdog timeout.

### Fix: PDF viewer fullscreen and close button (B17/B18)

- **Fullscreen presentation** (`MessageAttachmentsView.swift`): PDF viewer now uses `.fullScreenCover` instead of `.sheet`, filling the iPad screen and preventing rotation-triggered dismissals on iPhone.
- **Explicit close button** (`MessageAttachmentsView.swift`): "Done" button always visible in the PDF viewer navigation bar on both iPhone and iPad.

### Fix: Light mode and theme selector (B19)

- **Dynamic Design.Colors** (`Design.swift`): All `Design.Colors` properties now read from `ThemeManager`'s active palette instead of hardcoded dark hex values.
- **preferredColorScheme applied** (`AppEntry.swift`): Root view now applies `.preferredColorScheme` so system chrome (status bar, keyboard, alerts) matches the user's in-app theme selection.
- **System/Light/Dark all functional** (`ThemeManager.swift`): Appearance selector in Settings now correctly switches between system, light, and dark modes.

### Fix: Save-to-files keyboard conflict on iPad (B20)

- **Stable share presentation** (`MessageAttachmentsView.swift`): Share/save dialogs use popover anchoring on iPad to prevent keyboard appearance from dismissing the dialog.

### Fix: Attachment size increased for iPhone photos (B21)

- **Larger per-attachment limit** (`PendingAttachment.swift`): Increased from 350KB to 800KB, with max dimension from 768px to 1024px.
- **5 attachments per message** (`PendingAttachment.swift`): Increased from 4 to 5.
- **User feedback on failure** (`ChatScreen.swift`): System message shown when an attachment exceeds the size limit.
- **Relay body limit increased** (`relay/Dockerfile`): Request body limit raised to 5MB to support larger payloads.

### Fix: Image attachment loading reliability (B22)

- **Disk cache** (`AttachmentService.swift`): Fetched attachment images are now cached on disk, surviving app restart and memory pressure.
- **Automatic retry** (`MessageAttachmentsView.swift`): Failed image loads retry up to 2 times with a 2-second delay.

## [1.7.5] - 2026-07-21

### Fix: Note selection broken (B1)

- **Force view identity on note switch** (`NotesWorkspaceView.swift`): Added `.id(selectedId)` to `NoteEditorView` so SwiftUI destroys and recreates the editor when switching notes. Previously `.onAppear` only fired once per view lifecycle.

### Fix: Chat session titles (B2)

- **LLM-generated titles** (`ChatStore.swift`, `LiveHeraldClient.swift`, `HeraldClientProtocol.swift`): New `generateSessionTitle` RPC sends first user+assistant messages to connector for a 3-6 word title. Falls back to truncated first message if LLM fails.
- **Title on cancel/failure** (`ChatStore.swift`): `autoTitleIfNeeded()` now runs in `.cancelled` and `.failed` handlers, not just `.finished`.
- **Relay endpoint** (`relay/app/main.py`): New `POST /v1/sessions/{id}/generate-title` proxies to connector RPC.
- **Connector RPC** (`connector/client.py`): New `session.generateTitle` handler dispatches to Hermes API.

### Fix: Unreliable responses (B3)

- **Grace period before fail** (`ChatStore.swift`): `runAttemptLoop` now waits 30s after watchdog fires, refreshes conversation to check for late responses, then calls `failStalledMessage` if still empty. Shows "tap to retry" instead of hanging forever.

### Fix: Missing push notifications (B4)

- **Force push for slow responses** (`relay/app/main.py`): `maybe_send_message_push` gains `force` param. Jobs taking >60s bypass the foreground-stale check.
- **Reduced stale window** (deploy `.env`): `APP_PRESENCE_STALE_SECONDS` 120→30.
- **App state logging** (`AppContainer.swift`): `reportAppStateIfNeeded` now logs errors instead of silently discarding.

### Fix: Dynamic Island emoji (B5)

- **Emoji field in ContentState** (`HeraldActivityAttributes.swift` × 2): Added `emoji: String?` to `ContentState`. Both app and widget copies updated.
- **Phase-to-emoji mapping** (`LiveActivityService.swift`): New `emojiForPhase()` maps phases to contextual emojis (🧠 thinking, 💬 responding, ⚡ working, 🎤 listening, 🔍 searching).
- **Emoji in Dynamic Island** (`HeraldLiveActivity.swift`): Compact and minimal DI regions render emoji when available, fall back to Herald logo.

### Environment: Relay logging (E2)

- **Uvicorn log level** (`relay/Dockerfile`): Added `--log-level info` so app-level logger output appears in container logs.

## [1.7.3] - 2026-07-20

### Fix: Double thinking indicator (F4)

- **Single thinking indicator** (`ChatScreen.swift`, `MessageBubble.swift`): Removed standalone `ThinkingIndicatorView` from ChatScreen. Consolidated into the assistant placeholder bubble which now shows `TypingDotsView` + elapsed time ("Thinking… Ns") driven by `message.timestamp` via `TimelineView`. One indicator, anchored where the answer will appear.

### Fix: Push notifications (F5a/F5b)

- **aps-environment entitlement** (`Herald.entitlements`, `project.yml`): Added `com.apple.developer.aps-environment` to Herald target. Required for APNs delivery to registered device tokens.

- **Push delivery logging** (`relay/app/main.py`): Added `logger.info` on all three push decision paths — foreground skip, relay broker delivery, and APNs delivery. Enables debugging push failures from container logs.

### Feature: Live Activity phases (F5c)

- **Phase-tracked Live Activity** (`LiveActivityService.swift`, `ChatStore.swift`): Dynamic Island / Lock Screen now shows progression: thinking → reasoning → responding → tool activity → done. Activity starts at `.messageSent` instead of only on `.toolActivity`.

### Feature: Notes paper styles and scrolling (F6)

- **Seven paper styles** (`HeraldNote.swift`, `NotePaperBackground.swift`): blank, lines (small/medium/large), grid (small/medium/large). Theme-aware ink (dark: white.opacity(0.12), light: systemGray3). Red margin line only for ruled styles. Style persists per note.

- **Scrolling canvas** (`PencilCanvasRepresentable.swift`): Paper view installed behind PKCanvasView content at subview index 0. KVO observer on `contentSize` keeps paper sized to scroll content. Paper scrolls and zooms with ink — eliminates the shear bug.

- **Photo/scan attachments** (`NoteEditorView.swift`, `NotesRepository.swift`, `NotesStore.swift`): PHPickerViewController for photo library, VNDocumentCameraViewController for document scanning. SHA-256 blob storage with atomic writes. Attachment strip above canvas with thumbnails and delete.

### Infra: Deploy procedure (F1b)

- **Deploy directory** (`deploy/`): Docker Compose for relay + Postgres sidecar, Dockerfile, deploy.sh with dry-run mode, Caddyfile.example, .env.example template.

- **MAINTAINER_NOTES.md**: Full rewrite covering architecture, deploy procedures for relay/connector/iOS, schema change workflow, environment variables, known gotchas.

## [1.7.2] - 2026-07-20

### Feature: iPad Notes (PencilKit + Hermes Enrichment) — Phase 1–3

- **Local-first Notes with PencilKit canvas** (`Herald/Models/HeraldNote.swift`, `Herald/Features/Notes/`): New `SidebarSection.notes` case, `NotesStore`, `NotesRepository` (atomic file writes, SHA-256 hash verification), `PencilCanvasRepresentable` (PKCanvasView + PKToolPicker), `NoteEditorView` with debounced persistence (300–750ms settle + background/resign-active hooks). Notes are iPad-only in v1; iPhone read-only tab deferred to Phase 4.

- **On-device handwriting recognition** (`Herald/Services/Live/VisionHandwritingRecognizer.swift`): Vision framework `VNRecognizeTextRequest` with `.accurate` and `.fast` levels. `NoteRecognitionCoordinator` actor manages per-note recognition lifecycle, cancels stale tasks, drops results from superseded drawing revisions.

- **Directive parser** (`Herald/Features/Notes/NoteDirectiveParser.swift`): Pure, deterministic, Sendable parser for `#command` directives in recognized text. v1 allowlist: `#research`, `#search`, `#talkingpoints`, `#summary`, `#actions`, `#questions`. Unknown tags are data, never sent as intent. Stable fingerprints prevent duplicate execution across OCR churn.

- **Relay Notes API** (`relay/app/notes.py`, `relay/app/models.py`): New tables (`notes`, `note_blobs`, `note_recognitions`, `note_runs`, `note_run_events`, `enriched_note_revisions`). CRUD endpoints with ownership checks, If-Match optimistic concurrency, blob upload with SHA-256 verification and 25 MB cap, run lifecycle (queued→claimed→completed/failed/cancelled), cursor-based event replay. Registered via `app.include_router(notes_router)`.

- **Connector note.enrich RPC** (`connector/src/herald_connector/client.py`, `connector/src/herald_connector/note_contract.py`): New `note.enrich` RPC method dispatches to Hermes via the existing runtime adapter pipeline (API streaming or CLI fallback). Enrichment request/response schema-validated in both directions. v1 command allowlist enforced relay-side before prompt dispatch.

- **Run lifecycle services** (`relay/app/services.py`): `requeue_expired_note_runs()`, `claim_next_note_run()`, `complete_note_run()` with revision fence (stale results preserved as history), `fail_note_run()`, `append_note_run_event()`. Same lease/requeue semantics as `message_jobs`.

- **SDK availability note**: `PKStrokeRecognizer` is NOT in the public iOS SDK 26.5 headers. Recognition uses Vision framework exclusively. Documented in `docs/NOTES_CONTRACT_V1.md`.

- **Contract and fixtures** (`docs/NOTES_CONTRACT_V1.md`, tri-location fixtures): Enrichment request/response schemas, directive grammar, recognition strategy, relay schema, API endpoints, run lifecycle, invariants. Fixtures byte-identical across `HeraldTests/Fixtures/Notes/`, `relay/tests/fixtures/notes/`, `connector/tests/fixtures/notes/`.

### Fix: Version reconciliation

- **Align version across all targets** (`project.yml:80-81,140-141`, `README.md:14`): Both app and widget targets now consistently report `1.7.2 / 38`. Previous `1147c68` landed under `1.7.1` without a version bump.

## [1.7.1] - 2026-07-20

### Fix: Talk startup crash and production wiring

- **Prevent microphone startup crash** (`Herald/Services/Live/TalkAudioCapture.swift`, `Herald/Services/Live/HermesTalkCoordinator.swift`): Talk now resolves microphone permission before recording, rejects zero-rate or zero-channel input formats before installing an audio tap, and prepares the audio engine before starting it. Missing permission or input now produces a recoverable, visible error instead of terminating the app after the orb becomes active.

- **Wire the production Hermes Talk pipeline** (`Herald/Stores/AppContainer.swift`, `Herald/Stores/TalkStore.swift`): The production container now constructs and attaches capture, MiMo ASR/TTS, Hermes turn streaming, and PCM playback dependencies. Readiness reports missing coordinator or MiMo credentials instead of silently accepting Start Talking.

- **Reset failed Talk sessions** (`Herald/Stores/TalkStore.swift`): Capture failures return the UI to an inactive failed state so Start Talking does not leave a misleading connected orb or an unendable session.

### Fix: Production connectivity and app recovery

- **Use the hosted production relay by default** (`project.yml`, `Herald/Resources/Info.plist`, `Herald/Models/UserSettings.swift`): v1.7.1 points new installs at `https://hermes-relay.fihonline.net/v1` and migrates the stale DEBUG localhost default without replacing intentional custom relay choices.

- **Remove runtime mock fallback outside UI tests** (`Herald/Stores/AppContainer.swift`, `Herald/Stores/AppSessionStore.swift`): Failed production pairing and network calls can no longer be masked by demo data. Bootstrap also repairs sessions left in mock mode.

- **Refresh the active connector profile** (`Herald/Features/Chat/ChatScreen.swift`): Chat forces a profile refresh when it becomes active, preventing stale pre-pairing profile names after reconnects.

- **Restore iPad Settings navigation and notification replies** (`Herald/Features/Sidebar/AdaptiveRootView.swift`, `Herald/Stores/AppContainer.swift`): iPad Settings destinations route correctly, and notification reply text survives cold-launch routing.

### Feature: Reasoning effort selection

- **Add a user-facing reasoning selector** (`Herald/Models/UserSettings.swift`, `Herald/Features/Settings/SettingsScreen.swift`): Users can choose Off, Low, Medium, or High reasoning effort, with Medium used for older saved settings.

- **Persist and relay the selected effort** (`Herald/Services/Live/LiveHeraldClient.swift`, `relay/app/models.py`, `relay/app/schemas.py`, `relay/app/services.py`, `relay/app/main.py`): The setting is captured with each job and forwarded to the connector execution frame.

### Fix: Relay database compatibility

- **Repair legacy SQLite schemas** (`relay/app/database.py`, `relay/app/services.py`): Startup repairs foreign keys left on the pre-rebrand host table, adds the reasoning-effort column, normalizes naive timestamps during orphan cleanup, and supplies collision-free surrogate source sequences for older NOT NULL event tables.

- **Report only genuine streaming** (`Herald/Services/Live/LiveHeraldClient.swift`): Already-completed synchronous replies are no longer split into fake word-by-word deltas.

### Fix: Terminal result plumbing from coordinator to client

- **Add TerminalResult to RunResult** (`Herald/Services/Live/JobStreamCoordinator.swift`): Extended `RunResult` to carry terminal payload (text, usage tokens, error) from the done event. This allows the caller to pass terminal data into `resolveFinalMessage` instead of always supplying `nil`.

- **Pass terminal payload to resolveFinalMessage** (`Herald/Services/Live/LiveHeraldClient.swift`): The terminal result from the coordinator is now converted to a `StreamDonePayload` and passed to `resolveFinalMessage`, enabling proper error display and usage reporting.

### Fix: Talk API key reads from Keychain instead of UserDefaults (T2)

- **Add APIKeyHolder** (`Herald/Stores/AppContainer.swift`): Added a MainActor-safe cached holder that reads the MiMo API key from Keychain once and caches it. Refreshes when Settings writes/deletes the key.

- **Fix TTS service key source** (`Herald/Stores/AppContainer.swift`): Changed `MimoTTSService` to read the API key from Keychain via `APIKeyHolder` instead of reading directly from UserDefaults. This prevents Settings from removing the only value the provider reads.

- **Add apiKeyHolder to TalkStore** (`Herald/Stores/TalkStore.swift`): Added `apiKeyHolder` property so TalkStore can refresh the cached key after Settings changes.

### Fix: SSE stream simplification and terminal event persistence (S2)

- **Remove SSE-layer delta coalescing** (`relay/app/main.py`): Removed the delta coalescing logic that was consuming durable sequence positions while hiding their cursors. Each event is now emitted directly as it comes from the DB, preserving the 1:1 relationship between durable log entries and SSE frames.

- **Persist terminal event before emitting** (`relay/app/main.py`): The terminal done event is now persisted through `append_job_event` before being emitted to SSE subscribers. This ensures replays return the same terminal sequence and payload, and prevents synthesizing `last_seq + 1` on each connection.

- **Update streaming test** (`relay/tests/test_streaming.py`): Updated test to expect individual text_delta events instead of coalesced ones.

### Fix: Relay source sequencing (S1)

- **Pass sourceSeq from connector through relay** (`relay/app/main.py`): The connector sends `sourceSeq` on `job.started`, `job.heartbeat`, and `job.progress` frames, but the relay was looking for `eventId` which never existed. Now `sourceSeq` flows through to `publish_job_event` when present.

- **Deduplicate only when source_seq is provided** (`relay/app/services.py`, `relay/app/models.py`): Changed `source_seq` column to nullable and made `append_job_event` skip source-based deduplication when `source_seq is None`. This prevents legacy connectors without sequencing from having all their events collide at sequence 0.

- **Fix SSE delta coalescing** (`relay/app/main.py`): Moved delta flush logic into `emit_db_event` so adjacent text_delta events are properly coalesced before being yielded. Previously the flush happened before every event, causing each text_delta to be flushed individually instead of being merged with adjacent ones.

## [1.7.0] - 2026-07-20

### Deprecated - Legacy OpenAI Realtime Talk (Phase B-T6)

- **Removed `useHermesNativeTalk` flag** (`Herald/Stores/TalkStore.swift`): The feature flag introduced in B-T1 is gone. `HermesTalkCoordinator` is now the sole Talk path. All session methods (`startSession`, `endSession`, `toggleMute`, `interruptAssistant`) route directly through the coordinator.

- **Simplified `TalkStore` initializer** (`Herald/Stores/TalkStore.swift`, `Herald/Stores/AppContainer.swift`): `TalkStore` no longer accepts a `VoiceSessionServiceProtocol` dependency. The legacy event-subscription and snapshot-syncing code paths are removed.

- **Deprecated `VoiceSessionServiceProtocol`** (`Herald/Services/Protocols/VoiceSessionServiceProtocol.swift`): Marked `@available(*, deprecated)` with guidance to use `HermesTalkCoordinator`.

- **Deprecated `LiveVoiceSessionService`** (`Herald/Services/Live/LiveVoiceSessionService.swift`): The WebRTC-based OpenAI Realtime implementation is marked deprecated. Retained for one release behind `USE_LEGACY_REALTIME_TALK` compatibility flag.

- **Deprecated `MockVoiceSessionService`** (`Herald/Services/Mocks/MockVoiceSessionService.swift`): Marked deprecated alongside the protocol.

- **Deprecated `hermes_delegate` MCP tool** (`relay/app/talk_mcp.py`): The `hermes_delegate` tool and the `/v1/talk/mcp` endpoint are marked deprecated. Will be removed in the next release.

- **Deprecated `_create_openai_realtime_session`** (`connector/src/herald_connector/client.py`): The OpenAI Realtime session creation function and `talk.delegate` RPC handler are marked deprecated.

- **Removed legacy tests** (`HeraldTests/AppStoresTests.swift`): Removed `RecordingVoiceSessionService` test helper and two tests (`talkStoreReflectsBlockedReadinessState`, `talkStoreUpdatesFromVoiceEventStream`) that tested the removed legacy code paths.

## [1.6.2] - 2026-07-20

### Safe Early Speech Synthesis (Phase B-T3)

- **Sentence boundary detection** (`Herald/Services/Live/SpeechTextRenderer.swift`): Added `SentenceBoundary` struct and `findSentenceBoundaries(in:)` method that identifies stable sentence boundaries (`.!?。！？` followed by whitespace or end-of-string) in streaming text. Also added `findAllSentences(in:)` for post-hoc divergence comparison.

- **Early TTS synthesis** (`Herald/Services/Live/HermesTalkCoordinator.swift`): Modified the thinking state to start TTS synthesis as soon as a complete sentence is available, rather than waiting for the full canonical response. Segmentation halts when tool/reasoning boundaries are encountered (text instability).

- **Divergence tracking** (`Herald/Services/Live/SpeechTextRenderer.swift`): Added `SpeechDivergenceMetrics` struct tracking sentences/characters spoken vs total, and `hadDivergence` flag. Logged when early-spoken text diverges from the final canonical response.

- **Fallback path**: When no sentence boundary is found during streaming (e.g., single-sentence or tool-heavy responses), falls back to full-text synthesis after completion — no latency regression for short responses.

## [1.6.1] - 2026-07-20

### Automatic Turn-Taking + Barge-In (Phase B-T2)

- **VAD endpointing** (`Herald/Services/Live/TalkAudioCapture.swift`): Added `startListeningWithVAD()` returning `AsyncStream<Void>` that fires when sustained silence (1.5s below -40 dBFS) is detected after speech. Configurable `silenceThreshold` and `silenceDuration`. Added `startBargeInMonitoring()` for detecting speech above -30 dBFS during playback.

- **Auto turn-taking** (`Herald/Services/Live/HermesTalkCoordinator.swift`): After TTS playback drains, automatically resumes listening via VAD endpointing — no manual tap needed for continuous conversation. Controlled by `autoTurnTaking` flag (default `true`).

- **Barge-in support** (`Herald/Services/HermesTalkCoordinator.swift`): During TTS playback, monitors mic energy for speech. When user speaks above -30 dBFS, playback is flushed and recording begins immediately. The barge-in utterance is processed via the full pipeline (ASR → Hermes → TTS) and auto-turn-taking resumes.

- **Audio session hardening** (`Herald/Services/HermesTalkCoordinator.swift`): Changed audio mode from `.default` to `.voiceChat` for better echo cancellation. Added `handleAudioRouteChange()` for Bluetooth HFP connect/disconnect (interrupts on device unavailable, reconfigures on new device). Added `handleInterruption()` for phone calls and system audio interruptions. Observers registered/unregistered with session lifecycle.

- **TalkAudioCapture cleanup** (`Herald/Services/Live/TalkAudioCapture.swift`): `cancel()` now stops VAD monitoring to prevent leaked tasks.

## [1.6.0] - 2026-07-20

### Hermes-Native Push-to-Talk (Phase B-T1)

- **SpeechRecognizing protocol** (`Herald/Services/Protocols/SpeechRecognizing.swift`): New provider-neutral ASR protocol with `RecordedUtterance`, `SpeechLanguage`, and `TranscriptUpdate` types.

- **SpeechSynthesizing protocol** (`Herald/Services/Protocols/SpeechSynthesizing.swift`): New provider-neutral streaming TTS protocol with `PCMChunk`, `AudioFormat`, and `SpeechVoice` types.

- **TalkAudioCapture** (`Herald/Services/Live/TalkAudioCapture.swift`): Mic buffer capture via `AVAudioEngine` with push-to-talk endpointing, WAV finalization with resampling to 24 kHz mono int16, power metering for VoiceOrb, and 10 MB / 60s byte/duration caps.

- **MimoASRService** (`Herald/Services/Live/MimoASRService.swift`): Streaming ASR via MiMo `mimo-v2.5-asr` model. Multipart form upload with SSE delta/final response parsing. Uses `api-key` header (T5 fix).

- **MimoTTSService streaming** (`Herald/Services/Live/MimoTTSService.swift`): Added `audioStream(for:voice:style:)` for streaming PCM16 chunks via SSE. Auth header fixed from `Authorization: Bearer` to `api-key` (T5 fix). Conforms to `SpeechSynthesizing`.

- **PCMPlaybackQueue** (`Herald/Services/Live/PCMPlaybackQueue.swift`): `AVAudioEngine` + `AVAudioPlayerNode` queue for scheduling PCM16 buffers at 24 kHz mono. Supports flush-on-cancel and drain completion callbacks.

- **SpeechTextRenderer** (`Herald/Services/Live/SpeechTextRenderer.swift`): Converts canonical Hermes message text to speakable text by stripping code blocks, URLs, Markdown syntax, and raw JSON.

- **TalkTurnClient** (`Herald/Services/Live/TalkTurnClient.swift`): Thin wrapper over `HeraldClientProtocol.sendStreaming()` that projects Hermes text/tool-activity into Talk transcript updates.

- **HermesTalkCoordinator** (`Herald/Services/HermesTalkCoordinator.swift`): State machine orchestrating the full pipeline: capture → ASR → Hermes → TTS → playback. Sole `AVAudioSession` owner during Talk (T6 fix). States: idle → preparing → listening → endpointing → transcribing → thinking → synthesizing → speaking → idle.

- **VoiceState additions** (`Herald/Models/VoiceState.swift`): Added `transcribing` and `synthesizing` states with display labels, icons, and colors.

- **TalkStore Hermes integration** (`Herald/Stores/TalkStore.swift`): Added `useHermesNativeTalk` flag (default `true`). Added `attachHermesCoordinator()`, `startListening()`, `stopListeningAndProcess()` for push-to-talk flow. All session methods delegate to coordinator when flag is enabled; legacy Realtime path preserved behind flag.

## [1.5.2] - 2026-07-20

### Keychain Migration + Encryption Declaration (Phase B-T0 Security)

- **MiMo API key moved to Keychain** (`Herald/Features/Settings/SettingsScreen.swift`): Replaced `UserDefaults` reads/writes for `mimo.apiKey` with `KeychainSecureStore`. Includes idempotent one-time migration from UserDefaults on first launch after update.

- **Encryption declaration** (`Herald/Resources/Info.plist`): Added `ITSAppUsesNonExemptEncryption = false` — Herald uses standard encryption (TLS, CryptoKit SHA-256, WebRTC DTLS-SRTP) and qualifies for exemption.

## [1.5.1] - 2026-07-20

### MiMo Contract Spike + Fixtures (Phase B-T0)

- **T5 auth header fix** (`Herald/Services/Live/MimoTTSService.swift:67`): Documented that MiMo API uses `api-key` header, not `Authorization: Bearer`. Fix deferred to Phase B implementation.

- **MiMo API fixtures** (`HeraldTests/Fixtures/Mimo/`): Created ASR streaming, TTS streaming, and TTS error response fixtures for parser tests.

- **Fixture tests** (`HeraldTests/MimoFixtureTests.swift`): Added tests validating JSON fixture parsing for ASR deltas, TTS audio events, and error responses.

## [1.5.0] - 2026-07-20

### Remove v1 Streaming (Phase A-4)

- **Removed in-memory replay buffers** (`relay/app/main.py`): Deleted `app.state.job_event_buffers`, `app.state.job_event_sequence`, and `app.state.job_event_queues`. All event delivery now goes through the DB-backed `EventFanout` path exclusively.

- **Removed v1 `subscribe_job_events` / `unsubscribe_job_events`** (`relay/app/main.py`): The legacy in-memory queue subscription functions are gone. `subscribe_job_events` now delegates to `EventFanout.subscribe()` (DB-backed replay).

- **Removed `ensure_job_event_buffer`** (`relay/app/main.py`): No longer needed — events are persisted to DB by `publish_job_event` and replayed on SSE connect.

- **Simplified `publish_job_event`** (`relay/app/main.py`): Removed the in-memory buffer fallback. Events are now always persisted to DB and wake `EventFanout` subscribers. The `eventId` is now assigned from the DB sequence instead of an in-memory counter.

- **Simplified SSE endpoint** (`relay/app/main.py`): Removed the dual `asyncio.wait` pattern that raced legacy and v2 queues. Now waits on a single `EventFanout` wake queue.

- **Removed stale queue cleanup task** (`relay/app/main.py`): The `_cleanup_stale_job_queues` periodic task is no longer needed.

- **Removed `TOOL_PROGRESS_RE` marker parser** (`connector/src/herald_connector/herald_api_executor.py`): Deleted the `TOOL_PROGRESS_RE` regex, `_could_be_marker_prefix` helper, and `use_v1_marker_parser` field. Text deltas are now emitted directly without marker scraping.

- **Deprecated `parseV1Fallback`** (`Herald/Services/Live/JobStreamCoordinator.swift`): Added deprecation comments. The v1 fallback decoder is kept temporarily for backward compat during rollout but will be removed once metrics confirm v1 usage is negligible.

## [1.4.6] - 2026-07-20

### Server-Owned Attempts + Lease Retry (Phase A-3.5)

- **Atomic attempt increment** (`relay/app/services.py`): `claim_next_message_job()` now increments `MessageJob.attempt` atomically via `attempt=MessageJob.attempt + 1` in the UPDATE statement, so each lease claim is tracked server-side.

- **`attempt` in execute frame** (`relay/app/main.py`): `build_job_execute_payload()` now includes `attempt` in the `job.execute` payload so connectors know which attempt they are executing.

- **Max attempts cap** (`relay/app/config.py`, `relay/app/services.py`): New `max_job_attempts` setting (default 3). `requeue_expired_message_jobs()` now checks if a job has exhausted its attempts before requeuing — if so, the job is marked `failed` with a terminal error instead of being requeued.

- **Lease fence validation** (`relay/app/services.py`): `append_job_event()` rejects events from expired attempts via `job.attempt != attempt` check, preventing stale connector events from corrupting the log.

## [1.4.5] - 2026-07-20

### Structured Hermes Connector Adapter (Phase A-3)

- **`HermesGatewayExecutor`** (`connector/src/herald_connector/hermes_gateway_executor.py`): New executor that speaks the Hermes Desktop JSON-RPC WS protocol. Connects to the gateway via WebSocket, sends chat requests, and yields v2-adapted events. Falls back to the Runs API HTTP adapter when the gateway is unreachable.

- **`HermesEventAdapter`** (`connector/src/herald_connector/hermes_gateway_executor.py`): Maps raw Hermes events to v2 `JobEventEnvelope`. Handles both gateway (JSON-RPC WS) and runs API (HTTP/SSE) sources, ensuring consistent v2 vocabulary output. Includes late-event fencing after terminal events and stable `toolCallId` correlation.

- **`attempt` + `source_seq` on all frames** (`connector/src/herald_connector/client.py`): Every `job.progress` WebSocket frame now includes `attempt` and `sourceSeq` fields (fixes D9). Monotonically increasing `source_seq` per job execution.

- **v1 marker parser flag** (`connector/src/herald_connector/herald_api_executor.py`): Added `use_v1_marker_parser` flag (default `False`). When disabled, text deltas are emitted directly without `TOOL_PROGRESS_RE` marker parsing (fixes D6). Enable only for legacy v1 compatibility.

- **Active adapter diagnostics** (`connector/src/herald_connector/client.py`): `status_lines()` now reports which adapter is active: `gateway_v2` (Hermes Gateway JSON-RPC WS), `runs_v2` (Runs API HTTP/SSE with v2 events), or `openai_v1_fallback` (legacy OpenAI-compatible path).

## [1.4.4] - 2026-07-20

### Pure JobEventReducer + Watchdog Fix

- **`JobEventReducer`** (`Services/Live/JobEventReducer.swift`): New pure, `Sendable` reducer that converts `JobEventEnvelope` events into a `JobProjection`. Same events always produce the same projection — no side effects, no I/O. Supports typed payloads from the v2 stream contract, seq-gap detection, attempt resets, and all 12 event types.

- **Watchdog D3 fix** (`Stores/ChatStore.swift`): `.messageSent` no longer resets the watchdog deadline. The relay merely accepting a job is not real progress — the watchdog now waits for actual content (text/tool/reasoning/terminal) before extending the timeout.

- **D7 fix — remove client-side retry** (`Stores/ChatStore.swift`): Removed `maxAutoRetries` and `stallRetryCounts`. The relay now owns retries via leases. The client runs a single streaming attempt; if the watchdog fires, it shows "Waiting for host..." instead of resubmitting the same `clientMessageID`.

- **D11 partial — activeStreams map** (`Stores/ChatStore.swift`): Replaced the single `streamingMessageID` mutable slot with `activeStreams: [UUID: UUID]` (jobId → placeholderId). The computed `streamingMessageID` property provides backward compat for UI.

- **Injectable watchdog timeout** (`Stores/ChatStore.swift`): `watchdogTimeout` is now `static var` so tests can set it to milliseconds.

## [1.4.3] - 2026-07-20

### Cursor-Aware SSE + JobStreamCoordinator

- **`Last-Event-ID` header** (`RelayAPIClient.swift`): `streamEvents()` now accepts an optional `lastEventID` parameter and sends it as the `Last-Event-ID` HTTP header for cursor-based SSE replay (fixes D2).

- **`JobStreamCoordinator` actor** (`Services/Live/JobStreamCoordinator.swift`): New resilient SSE consumer that replaces the one-shot `streamJobEvents`. Opens SSE with persisted cursor, decodes v2 `JobEventEnvelope` with v1 fallback, detects `seq` gaps and reconnects from last contiguous seq, bounded exponential backoff with jitter (capped at 60s), checks authoritative job status after EOF, and yields `RunResult` on completion (fixes D4 — never emits `.failed` for a live job).

- **Keyed by `jobId`** (`LiveHeraldClient.swift`): Streaming is now keyed by `jobId` via `JobStreamCoordinator` instead of the single `streamingMessageID` slot (fixes D11).

- **`JobStatusResponse` snapshot fields** (`LiveHeraldClient.swift`): Added `attempt` and `lastSeq` optional fields to `JobStatusResponse` for recovery and diagnostics. Added `getJobStatusSnapshot()` helper that maps to `JobStreamCoordinator.JobStatusSnapshot`.

## [1.4.2] - 2026-07-20

### Durable SSE Replay, `id:` Lines, `cancelled` Terminal, Snapshot Fields, Retention

- **DB-backed SSE replay** (`relay/app/main.py`): Rewrote `GET /v1/jobs/{job_id}/events` to replay events from the `job_events` table instead of the destructive in-memory `pop()`-based buffer (fixes D5). Supports `Last-Event-ID` header and `?after=<seq>` query parameter for cursor-based reconnection.

- **Real `id:` lines** (`relay/app/main.py`): Every SSE frame now includes `id: <seq>` so iOS clients can send `Last-Event-ID` on reconnect and resume without gaps (fixes D1).

- **`cancelled` terminal status** (`relay/app/main.py`, `relay/app/services.py`): SSE endpoint, `wait_for_job_completion`, `reply_state_for_job`, and `append_job_event` now recognize `cancelled` as a terminal status alongside `completed` and `failed` (fixes D10).

- **EventFanout integration** (`relay/app/main.py`): Wired `EventFanout` into the lifespan and `publish_job_event`. Each published event is now appended to the durable DB log and wakes EventFanout subscribers. Legacy in-memory queues preserved for backward compat.

- **Snapshot fields** (`relay/app/main.py`): `GET /v1/jobs/{job_id}` now returns `attempt` and `lastSeq` fields for recovery and diagnostics.

- **Retention cleanup** (`relay/app/services.py`): Added `cleanup_old_job_events()` to delete events for terminal jobs older than the retention window. Non-terminal job events are never expired.

## [1.4.1] - 2026-07-20

### Durable Event Log — job_events Table + Transactional Append

- **JobEvent model** (`relay/app/models.py`): Added `JobEvent` SQLAlchemy model with `job_id`, `seq`, `attempt`, `source_seq`, `type`, `payload_json`, and `created_at` columns. Added `attempt` column to `MessageJob` for attempt-scoped event tracking.

- **Idempotent DDL** (`relay/app/database.py`): Added `CREATE TABLE IF NOT EXISTS job_events` with unique indexes on `(job_id, seq)` and `(job_id, attempt, source_seq)`. Added `ALTER TABLE message_jobs ADD COLUMN IF NOT EXISTS attempt`. **Operator note**: run the DDL manually against field Postgres before deploying — the SQLite migration is automatic.

- **EventFanout** (`relay/app/streaming.py`): Extracted in-memory fan-out for SSE subscribers into a standalone class. Uses async lock-protected subscribe/unsubscribe with wake-signal delivery. Replaces the destructive `pop()`-based buffer (to be fully removed in PR 3).

- **Transactional append service** (`relay/app/services.py`): Added `append_job_event()` with dedup on `(job_id, attempt, source_seq)`, monotonic seq allocation via `COALESCE(MAX(seq), 0) + 1`, and terminal-job rejection. Added `get_job_events_after()` and `get_job_last_seq()` for SSE replay queries.

## [1.4.0] - 2026-07-20

### Stream Contract v2 — Golden Fixture Freeze

- **Contract specification** (`docs/STREAM_CONTRACT_V2.md`): Defined the v2 event envelope with `contractVersion`, `jobId`, `conversationId`, `attempt`, `seq`, `type`, `timestamp`, and `payload` fields. Documented 12 event types including terminal events (`run.completed`, `run.failed`, `run.cancelled`) and new v2 additions (`run.requeued`, `commentary`, `approval.required`).

- **Python Pydantic models** (`connector/src/herald_connector/stream_contract.py`): Added Pydantic v2 models for every v2 event type and a `JobEventEnvelope` discriminated union. Includes `TERMINAL_TYPES` constant for validation.

- **Swift Codable types** (`Herald/Models/JobEvent.swift`): Added `JobEventEnvelope` struct with `JobEventType` enum and per-event payload structs. All types are `Sendable` for Swift 6.2 strict concurrency. Uses custom `Codable` implementation for type-safe payload decoding.

- **Golden fixtures** (`connector/tests/fixtures/hermes/`, `HeraldTests/Fixtures/StreamContractV2/`): Created 8 canonical fixture files covering text-only, reasoning, multi-tool, commentary, approval, error, cancelled, and goal-continuation scenarios. Same JSON in both Python and Swift test locations.

- **Contract tests** (`connector/tests/test_stream_contract.py`, `HeraldTests/StreamContractV2Tests.swift`): Validates every fixture against the envelope models, asserts seq ordering, terminal event rules, contractVersion=2, and jobId/conversationId consistency.

## [1.3.3] - 2026-07-20

### Fixed - APNs Push Notifications + iPad Notification Routing

- **Bundle ID mismatch** (`relay/app/config.py`, `relay/app/apns.py`, `relay/.env.example`): Changed default APNs bundle ID from `net.fihonline.herald` to `net.fihonline.herald` to match the actual app bundle identifier. The wrong topic caused Apple to reject every push notification (`TopicDisallowed`/`BadDeviceToken`).

- **APNs environment default** (`relay/app/config.py`, `relay/.env.example`): Changed default APNs environment from `development` to `production` to match TestFlight builds. Development tokens sent to the production gateway caused `BadEnvironmentKeyInToken` silent failures.

- **iPad notification tap routing** (`Herald/Core/Router.swift`, `Herald/Stores/AppContainer.swift`, `Herald/AppEntry.swift`): Added `switchToTab(_:)` method to `TabRouter` that bridges `selectedTab` changes to `oniPadSectionSwitch` on iPad. Notification taps, deeplinks, and pairing removal now correctly switch the iPad sidebar section instead of silently setting `selectedTab` without updating the visible column.

- **Connector adapter logging** (`connector/src/herald_connector/client.py`): `runtime_adapter_for_state_async` now logs which adapter was chosen (API/streaming vs CLI) and, when the API health check fails, logs the failure reason. Streaming status is now diagnosable from connector logs instead of guessed.

- **Stop notification action** (`Herald/Services/Protocols/HeraldClientProtocol.swift`, `Herald/Services/Live/LiveHeraldClient.swift`, `Herald/Services/Mocks/MockHeraldClient.swift`, `Herald/Services/Support/ResilientHeraldClient.swift`, `Herald/Stores/AppContainer.swift`): Added `cancelJob(jobID:)` to `HeraldClientProtocol` and wired the notification Stop action to call `POST /v1/jobs/{id}/cancel` on the relay. The Stop button on active-job notifications now actually cancels the running job end-to-end.

## [1.3.2] - 2026-07-20

### Fixed - MCP Revival, Live Connector Delivery, and Responsive UI

- **MCP rename compatibility** (`connector/pyproject.toml`, `connector/src/herald_connector/`): restored the legacy `hermes-mobile-mcp` and `hermes-mobile` entrypoints as aliases to the Herald implementation, and retained compatibility imports for gateways and extensions created before the rename. Cached Hermes MCP commands no longer crash by importing the deleted `hermes_mobile_connector` package after reinstall.

- **Live MCP revival loop** (host deployment): restarted the six-day-old Hermes WebUI process that still generated the removed `mcp_stdio_watchdog.py --create-time` argument. Its in-memory caller now matches the installed watchdog, eliminating the five-minute `TaskGroup` reconnect cycle.

- **Persisted connector runtime** (`connector/src/herald_connector/herald_runner.py`): map persisted `ConnectorRuntimeConfig.hermes_*` fields into the Herald-named runtime adapter correctly. The connector now reconnects after service restarts instead of remaining active with a hidden attribute error.

- **Relay WebSocket lease crash** (`relay/app/main.py`): imported the lease clock and normalized SQLite timestamps before comparing them. Active connector jobs no longer lose their WebSocket to `NameError` or offset-naive/offset-aware datetime exceptions.

- **Awaited job heartbeats** (`connector/src/herald_connector/client.py`): async heartbeat senders are now awaited, so long-running jobs actually renew their relay lease instead of silently creating an un-awaited coroutine.

- **Lower live-delivery latency** (`relay/app/config.py`, `relay/.env.example`): reduced connector idle job polling from 1 second to 100 ms and connector reconnect delay from 3 seconds to 1 second.

- **Herald notification identity** (`relay/app/main.py`): completion notifications and inbox records now use the Herald product name.

- **iPhone landscape layout** (`Herald/Features/Sidebar/AdaptiveRootView.swift`): all iPhones now use `MainTabView` in both orientations. Removed `verticalSizeClass == .compact` check that was routing landscape iPhones into the iPad `NavigationSplitView` shell.

- **iPad inspector workspace** (`Herald/Features/Sidebar/AdaptiveRootView.swift`): replaced three-column `NavigationSplitView` with two-column split + optional trailing inspector that is genuinely inserted/removed from layout. Added drag-handle divider for resizing with width budgeting (chat minimum 420pt). Swipe gesture preserved for opening/closing the inspector.

- **Width-aware toolbar** (`Herald/Features/Chat/ChatScreen.swift`): replaced `DeviceClass.isPhone` toolbar check with `ViewThatFits` adaptive composition. Eliminates synthesized `...` overflow on narrow iPad columns.

- **Truthful inspector labels** (`Herald/Features/Sidebar/iPadRightPanelView.swift`): renamed "Logs" to "Activity", "Tools" to "Usage", terminal clearly labeled as preview. Log-level filter chips are now functional.

- **Canvas close/clear separation** (`Herald/Features/Canvas/CanvasView.swift`): X button now only dismisses; explicit trash button with confirmation dialog for artifact deletion.

- **Buffered tool-marker parser** (`connector/src/herald_connector/herald_api_executor.py`): tool markers now parsed from accumulated buffer instead of per-delta, handling split/combined SSE chunk boundaries correctly. Also handles `delta.tool_calls` to prevent silent tool-execution windows.

- **Drawer width responsiveness** (`Herald/Features/Sidebar/iPhoneSessionDrawer.swift`): drawer width now uses `GeometryReader` instead of static `UIScreen.main.bounds`.

### Changed - iPad Three-Panel Layout

- **Adaptive root mounted** (`Herald/Features/Onboarding/AppRootView.swift`): `AppRootView` now renders `AdaptiveRootView()` after onboarding instead of `MainTabView()`. `AdaptiveRootView` is responsible for choosing `MainTabView` on iPhone.

- **Three-column layout** (`Herald/Features/Sidebar/AdaptiveRootView.swift`): iPad now uses a proper three-column `NavigationSplitView` with sidebar, content, and detail columns. The right panel is no longer an overlay — it's a genuine detail column.

- **Router synchronization** (`Herald/Features/Sidebar/AdaptiveRootView.swift`): `oniPadSectionSwitch` binding is installed/removed with the adaptive view lifecycle. Router tab changes are synchronized with sidebar section selection.

- **Panel placeholder**: When the detail panel is closed, a placeholder view is shown instead of empty space.

- **Accessibility**: Added accessibility label to the detail panel toggle button.

## [1.2.6] - 2026-07-20

### Changed - iPhone Toolbar Cleanup

- **Separate phone/pad toolbars** (`Herald/Features/Chat/ChatScreen.swift`): Toolbar now uses conditional compositions via `DeviceClass.isPhone` instead of trying to fit all controls into one layout.

- **iPhone leading**: Hamburger/session drawer button only, with accessibility label.

- **iPhone principal**: New `compactStatusControl` showing connection dot + compact model name + context ring. Width-bounded (no `fixedSize`) to prevent system overflow ellipsis.

- **iPhone trailing**: Canvas only. Removed duplicate Settings gear (iPhone has Settings tab in bottom tab bar).

- **iPad**: Retains richer profile/model/timer presentation. Settings gear remains since iPad has no tab bar.

- **Accessibility**: Added accessibility labels to all icon-only toolbar controls.

## [1.2.5] - 2026-07-20

### Added - Lock-Screen Notification Actions

- **Notification categories** (`Herald/Services/Protocols/NotificationServiceProtocol.swift`, `Herald/Services/Live/LiveNotificationService.swift`): Registered `HERALD_MESSAGE_READY` (Read, Reply, Nudge) and `HERALD_JOB_ACTIVE` (Read, Stop, Nudge) categories with stable action identifiers.

- **Reply action** (`Herald/Stores/AppContainer.swift`): `UNTextInputNotificationAction` sends typed text to the notification's `conversationId` using a fresh `clientMessageId`. Works regardless of currently displayed conversation.

- **Nudge action** (`Herald/Stores/AppContainer.swift`): Sends fixed follow-up text "Continue, and give me a concise status update." to the correct conversation.

- **Stop action** (`Herald/Stores/AppContainer.swift`, `relay/app/main.py`, `connector/src/herald_connector/client.py`): Cancels `jobId` end to end via new `POST /v1/jobs/{job_id}/cancel` endpoint and `jobs.cancel` connector RPC. Idempotent for already-completed jobs.

- **Job action handling** (`Herald/AppEntry.swift`): Notification handler extracts reply text from `UNTextInputNotificationResponse` and delegates to `AppContainer`.

- **Relay cancel endpoint** (`relay/app/main.py`): New `POST /v1/jobs/{job_id}/cancel` verifies job ownership, dispatches connector RPC for running jobs, and publishes terminal `cancelled` SSE event.

- **Connector cancel RPC** (`connector/src/herald_connector/client.py`): New `jobs.cancel` RPC method cancels the running asyncio task, cleans up staged attachments, and returns `{jobId, status: "cancelled"}`.

## [1.2.4] - 2026-07-20

### Fixed - Notification Metadata + Crash-Safe Routing

- **Push broker metadata** (`relay/app/schemas.py`, `relay/app/main.py`): Push broker request now carries `conversationId`, `messageId`, `jobId`, and `category` fields through the signed payload. Both direct APNs and managed broker transport produce identical notification payloads.

- **Notification category** (`relay/app/main.py`): Message completion pushes now use `HERALD_MESSAGE_READY` category identifier for lock-screen action support.

- **Crash-safe notification routing** (`Herald/Stores/AppContainer.swift`): New `handleNotificationRoute` method processes notification taps through a single entry point. Pending routes are stored during cold launch and processed exactly once after initialization.

- **Direct load-by-ID** (`Herald/AppEntry.swift`): Notification handler now extracts primitive strings from `UNNotificationResponse` and delegates to `AppContainer`. The no-argument `loadConversation()` fallback is removed — notifications either load the exact conversation by ID or show a recoverable error.

- **Single-flight initialization** (`Herald/Stores/AppContainer.swift`): `initialize()` now guards against concurrent callers to prevent competing state writers during cold launch.

- **Push broker test updated** (`relay/tests/test_push_broker.py`): Test now asserts `category: "HERALD_MESSAGE_READY"` and metadata fields (`conversationId`, `messageId`, `jobId`) are forwarded to APNs.

## [1.2.3] - 2026-07-20

### Changed - Resumable Live Job Connection

- **Connector job heartbeats** (`connector/src/herald_connector/client.py`): Jobs now emit `job.started` immediately and `job.heartbeat` every 10 seconds with phase tracking (`starting`, `thinking`, `tool`, `writing`, `cli_waiting`). Active jobs survive across WebSocket reconnects.

- **Relay renewable lease** (`relay/app/main.py`, `relay/app/services.py`): Job lease is now renewed by `job.started`, `job.heartbeat`, and `job.progress` messages instead of using a fixed 180-second wall-clock deadline. Healthy long-running jobs are no longer killed by timeout.

- **SSE event IDs** (`relay/app/main.py`): Job events now carry monotonically increasing `eventId` fields for reconnection tracking.

- **Job status endpoint** (`relay/app/main.py`): New `GET /v1/jobs/{job_id}` returns authoritative job status for recovery after SSE gaps.

- **Grace period on disconnect** (`relay/app/main.py`): Connector WebSocket disconnect no longer immediately fails in-flight jobs. A `reconnecting` event is published and the job's lease governs recovery.

- **iOS resumable streaming** (`Herald/Services/Live/LiveHeraldClient.swift`): SSE EOF without `done` is now treated as a transport interruption, not success. The client checks job status via `GET /v1/jobs/{id}` and handles completed/failed/running states appropriately.

- **New streaming events** (`Herald/Models/StreamingUpdate.swift`): Added `.started(phase:)`, `.heartbeat(phase:)`, `.reconnecting`, and `.cancelled` cases for richer job lifecycle visibility.

- **SSE event ID parsing** (`Herald/Services/Support/RelayAPIClient.swift`, `Herald/Models/SSEEvent.swift`): SSE parser now extracts `id:` fields for reconnection tracking.

- **Polling safety net** (`Herald/Stores/ChatStore.swift`): Polling no longer marks messages as `.failed` after exhausting attempts. Server-authoritative job state is respected.

## [1.2.2] - 2026-07-20

### Fixed - Build 12 & 13 Reconciliation

- **Build 12** (`Services/Live/LiveHeraldClient.swift`): Fixed reasoning display — preserve reasoning content across metadata merge so chain-of-thought text survives conversation refresh.
- **Build 13** (`AppEntry.swift`): Fixed Swift 6 strict concurrency for `UNUserNotificationCenterDelegate` methods — notification delegate data crossings now use primitive `Sendable` types only.

## [1.2.1] - 2026-07-20

### Fixed - Streaming watchdog, scroll, iPad swipe, /new command, MCP

- **Bug B — False "tap to retry" during multi-tool/multiagent work** (connector + relay + app): The streaming pipeline went silent during tool execution and subagent fan-out because the connector only parsed `delta.content` and ignored `delta.tool_calls`. A 120s client watchdog misfired on that silence, showing "Herald didn't respond" while Hermes was still working.
  - `connector/src/herald_connector/herald_api_executor.py`: Now handles `delta.tool_calls` — emits `tool_activity` StreamEvent for each tool function name. Also emits `keepalive` events when the upstream SSE sends a chunk with no user-visible content (role-only deltas, empty content during subagent work).
  - `connector/src/herald_connector/client.py`: Forwards `keepalive` events to the relay as `job.progress` with `kind: "keepalive"`.
  - `Herald/Models/StreamingUpdate.swift`: New `.keepalive` case.
  - `Herald/Services/Live/LiveHeraldClient.swift`: Handles `"keepalive"` SSE events from the relay.
  - `Herald/Stores/ChatStore.swift`: `.keepalive` resets the watchdog timer via `progressContinuation`. `failStalledMessage` now preserves the placeholder ID so a late `.finished` can find and replace the error message with the actual response. Post-failure polling task refreshes the conversation after 15s to pick up server-side completion.

- **Bug A — Sent message not scrolled into view** (`ChatScreen.swift`): Removed the `streamingMessageID == nil` guard from the `pendingMessageSentAt` onChange handler. User messages now always scroll into view on send, even when streaming starts immediately.

- **Bug C — iPad swipe for logs panel** (`AdaptiveRootView.swift`): Added a `DragGesture` to the iPad detail content — swipe left opens the right panel (logs), swipe right closes it. Matches the existing iPhone drawer gesture pattern.

- **Bug D — `/new` starts new session without confirmation** (`ChatScreen.swift`, `SlashCommand.swift`): Split `/new` from `/clear`. `/new` now calls `performClear()` immediately without the destructive confirmation dialog. `/clear` retains the confirmation. Marked `/new` as `isDestructive: false`.

- **Bug F — MCP `hermes_mobile` stale command path** (`connector/src/herald_connector/client.py`): Added `register_native_mcp_server()` call on every connector connect, not just at enroll/setup. Self-heals stale `herald-mcp` paths in `~/.hermes/config.yaml` when the connector venv moves or is reinstalled.

### Notes

- Bug B fix requires all three components (connector + relay + app) deployed together. Ship connector changes before or with the app change so an older app safely ignores unknown `keepalive` events.
- The relay forwards arbitrary `kind` values through `publish_job_event` — no relay code change was needed for `keepalive`.

## [1.1.0] - 2026-07-19

### Added - Mimo TTS + Bug Fixes

- **Mimo TTS Integration** (`Services/Live/MimoTTSService.swift`): Full text-to-speech via Xiaomi MiMo v2.5 TTS API. Uses OpenAI-compatible chat completions format (`POST /v1/chat/completions` with model `mimo-v2.5-tts`). Returns base64-encoded WAV audio, played back via AVAudioPlayer.

- **TTSServiceProtocol** (`Services/Protocols/TTSServiceProtocol.swift`): Protocol for TTS services with `synthesize()`, `speak()`, `stop()`, and `isPlaying` state.

- **8 Premium Voices**: Mia (lively girl), Chloe (sweet dreamy), Milo (sunny boy), Dean (steady gentleman) for English. 冰糖, 茉莉, 苏打, 白桦 for Chinese.

- **Voice Settings** (Settings → Voice): Mimo API key field (stored in UserDefaults), voice picker dropdown, TTS on/off toggle, auto-speak toggle for Talk mode.

- **Read-Aloud Buttons**: Speaker icon on Hermes chat messages and Talk transcript bubbles to read any response aloud via Mimo TTS.

- **Auto-Speak in Talk**: When enabled, completed Herald responses in Talk mode are automatically spoken aloud via Mimo TTS.

- **iPhone Permissions Fix**: Permissions screen now uses NavigationLink inside the settings sheet instead of dismissing and pushing onto the chat NavigationStack. Fixes the issue where permissions appeared behind the settings sheet on iPhone.

### Changed

- TalkStore now has `ttsService`, `ttsSettingsProvider`, `speakText()`, `stopTTS()`, and `autoSpeakLatestHermesResponse()` for TTS integration.
- UserSettings gained `ttsEnabled`, `ttsVoice`, `ttsAutoSpeak` properties with backward-compatible Codable migration.
- AppContainer wires MimoTTSService into TalkStore at launch.
- Version badge updated to 1.1.0.
- README updated with Mimo TTS feature documentation.

## [1.0.0] - 2026-07-19

### Changed
- Rebrand from Hermes-iOS to HERALD
- New bundle ID: `net.fihonline.herald`
- New relay container: `herald-relay`
- New connector package: `herald-connector`
- Theme preset renamed: `.nous` → `.herald` with brand orange (#FF6B00) accent
- Version reset to 1.0.0 to mark the new identity

## [0.10.0] - 2026-07-17

### Added - Session Management + Right Panel + iPhone Drawer

- **Session Management API** (`relay/app/main.py`, `relay/app/services.py`): 9 new REST endpoints for full session lifecycle — list, search, create, delete, archive, toggle pin, rename, load conversation. Paginated with `limit`/`offset`.

- **Device-scoped sessions** (`relay/app/models.py`): Added `device_id`, `source`, `is_pinned`, `preview_text` columns to Conversation. Sessions created from an iPhone only appear on that iPhone; sessions from iPad only appear on that iPad. Hermes-host sessions (CLI, Telegram, etc.) with null `device_id` are visible across all devices.

- **iPhoneSessionDrawer** (`Features/Sidebar/iPhoneSessionDrawer.swift`): Slide-out session browser for iPhone. Drag-from-left-edge gesture with spring animation, hamburger button in chat toolbar. Shows pinned/recent sessions, context menu for pin/archive/delete, load-more pagination.

- **iPadRightPanelView** (`Features/Sidebar/iPadRightPanelView.swift`): Right-side inspector panel with three tabs — Logs (scrollable log feed with level filters), Terminal (console-style output), Tools (token usage). Toggle via `sidebar.right` button in sidebar header and detail toolbar.

- **Session browser (iPad sidebar)**: Full session list in sidebar with search, pinned section, recent section, platform sub-sections, swipe actions, context menu for rename/pin/archive/delete.

### Changed

- **Conversation scoping**: `get_or_create_current_conversation` and `archive_current_conversation` now accept optional `device_id`. Current conversation is device-scoped, preventing cross-device session leakage.
- **ChatScreen**: Accepts `$isSessionDrawerOpen` binding for iPhone drawer toggle. Hamburger button added to leading toolbar on iPhone.
- **MainTabView**: Wraps content in ZStack with `iPhoneSessionDrawer` overlay.
- **AdaptiveRootView**: iPad layout now uses HStack with NavigationSplitView + right panel. Right panel toggle in detail toolbar.
- **iPadSidebarView**: Header now has new-chat button + right panel toggle.

### Relay API Surface

| Method | Endpoint | Description |
|--------|----------|-------------|
| GET | `/v1/sessions` | List sessions (paginated) |
| GET | `/v1/sessions/search?q=` | Search by title |
| POST | `/v1/sessions` | Create new session |
| GET | `/v1/sessions/{id}` | Get session summary |
| GET | `/v1/sessions/{id}/conversation` | Load full conversation |
| DELETE | `/v1/sessions/{id}` | Delete session |
| POST | `/v1/sessions/{id}/archive` | Archive session |
| POST | `/v1/sessions/{id}/pin` | Toggle pin |
| PATCH | `/v1/sessions/{id}` | Rename session |

### Files Changed

| File | Status |
|------|--------|
| relay/app/models.py | Modified |
| relay/app/services.py | Modified |
| relay/app/main.py | Modified |
| Features/Sidebar/iPhoneSessionDrawer.swift | New |
| Features/Sidebar/iPadRightPanelView.swift | New |
| Features/Sidebar/AdaptiveRootView.swift | Modified |
| Features/Sidebar/iPadSidebarView.swift | Modified |
| ContentView.swift | Modified |
| Features/Chat/ChatScreen.swift | Modified |
| HermesMobile.xcodeproj/project.pbxproj | Modified |

### Added - iPad Layout Support

- **DeviceClass** (`Core/DeviceClass.swift`): enum detecting iPad vs iPhone at runtime via UIDevice.current.userInterfaceIdiom. Static current, isPad, isPhone properties (MainActor-isolated).

- **SidebarSection** (`Features/Sidebar/iPadSidebarView.swift`): enum with .chat, .inbox, .talk, .settings cases. Each has title and icon computed properties.

- **iPadSidebarView**: SwiftUI sidebar using List with .sidebar style. Shows section icons, titles, and an orange offline dot next to Chat when hostStore.connectionState != .online. Uses Design system tokens throughout.

- **AdaptiveRootView** (`Features/Sidebar/AdaptiveRootView.swift`): Root view that branches between NavigationSplitView (iPad, sidebar + detail) and MainTabView (iPhone, existing tab bar). Driven by DeviceClass.isPad.

- **TabRouter iPad extension** (`Core/Router.swift`): Added SidebarSection enum and oniPadSectionSwitch callback closure. When .settings is presented on iPad, calls the callback instead of presenting a sheet. All existing iPhone behavior unchanged.

### Changed

- **AppRootView**: Now renders AdaptiveRootView instead of MainTabView directly.
- **project.pbxproj**: New files registered in the main HermesMobile target Sources build phase.

### Architecture

iPad layout uses NavigationSplitView:

```
+--------------+----------------------------+
| Sidebar      | Detail                     |
| (list)       |                            |
|   Chat  <--  | ChatScreen / InboxScreen / |
|   Inbox      | TalkModeScreen /           |
|   Talk       | SettingsScreen             |
|   Settings   |                            |
+--------------+----------------------------+
```

iPhone layout unchanged: MainTabView with existing tab bar.

### Files Changed

| File | Status |
|------|--------|
| Core/DeviceClass.swift | New |
| Features/Sidebar/iPadSidebarView.swift | New |
| Features/Sidebar/AdaptiveRootView.swift | New |
| Core/Router.swift | Modified |
| Features/Onboarding/AppRootView.swift | Modified |
| HermesMobile.xcodeproj/project.pbxproj | Modified |
