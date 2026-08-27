# Changelog

All notable Kallisti changes are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.2] - 2026-08-22

Current build release (build 135.10). Private beta build.

### Changed

- **Connect retry now recovers instead of wedging (135.10).** On device sleep
  or temporary network loss, an in-flight connection can be suspended by iOS
  with no completion delivered, which left the client stuck on
  "Connecting..." permanently until a force-quit. A watchdog now bounds the
  connection attempt and force-clears the stuck state so foreground and
  timer-triggered reconnects can actually start a fresh attempt. No more
  permanent "Connecting." screen.

### Fixed

- **Color picker no longer blanks while a note syncs (135.10).** Repeatedly
  forcing the drawing canvas into first-responder during note-editor
  re-renders (including each reasoning-stream delta) corrupted the open color
  and attribute popover into an empty gray void. The picker is now left alone
  unless it actually needs showing, so the popover stays populated while a
  note is being enriched.# Changelog

All notable Kallisti changes are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.1] - 2026-08-20

Current build release (build 132.0). Private beta build.

### Added

- **Handwriting OCR quality pass (132.0).** On-device recognition now renders
  the ink at 4x (was 2x), runs TWO Vision passes - the raw drawing plus an
  adaptive-threshold contrast-boosted version - and merges per line by
  confidence. Short/abbreviated tokens are no longer "corrected" into the
  wrong dictionary word. Handwriting that read as "Quen Movels" now reads
  "Qwen Models".
- **Note photo/scan attachments now reach enrichment (132.0).** When a note
  has attached photos or scans, they are sent alongside the drawing as inline
  images so the enrichment model sees the FULL source, not just the ink.
- **Smart title fallback (132.0).** When the model one-shot for a title comes
  back empty, the app now derives a client-side title from the note's content
  (first 3-6 meaningful words, sentence case). Untitled notes no longer stay
  "Untitled Note".
- **QuickLook attachments get a Done button (132.0).** PDFs and files opened
  from the Inbox or chat now have a visible close control - previously a
  full-screen QuickLook had no way back.

### Changed

- **Enrichment authority flip (132.0).** The drawing is now the source of
  truth. The on-device OCR text is treated as a noisy draft, used only to
  disambiguate letterforms - never as the final reading. The enrichment model
  is told explicitly to read the handwriting from the image and flag
  uncertain words instead of guessing.

### Fixed

- **Notifications off now actually stops pushes (132.0).** The connector's
  `push/deactivate` endpoint was a 501 stub - toggling Notifications off in
  Settings left the APNs token registered and "Response ready" pushes kept
  arriving. The endpoint now removes the device's token from the registry,
  and the native-gateway path calls it when the toggle is switched off.

## [0.3.1] - 2026-08-20

Current build release (build 131.23). Private beta build.

### Fixed

- **Session drawer only opens on the Chat tab (131.23).** The drawer carried
  an invisible 24pt left-edge catcher + drag gesture that was active on EVERY
  tab in rich mode, so a left-edge swipe on Settings / Cron / Inbox / Talk
  opened the session list where it does not belong. The drawer now renders
  only on the Chat tab.
- **"session busy" is now actionable (131.23).** When the server rejects a
  send because the conversation is mid-turn on another device, the app
  surfaces clear copy ("This conversation is active on another device") with
  a Start New Session action chip instead of a raw error + dead-end Retry.

### Fixed

- **Rich-chat left-edge swipe no longer fires back navigation (131.22).**
  In rich chat the session drawer owns the left edge on EVERY tab - it
  renders over the whole TabView - so a left-edge swipe must only open the
  drawer and never pop the nav stack or switch tabs underneath it. The
  131.18 guard only exempted the chat tab, so on other tabs a swipe opened
  the session list AND navigated back behind it. The back gesture now never
  runs in rich mode.

### Fixed

- **New-chat bleed (131.21).** A stream that outlived a New Chat / session
  switch kept writing its events into the CURRENT conversation because the
  consumer loop only guarded on attempt identity, not conversation identity. A
  mid-event, reconnecting, or relay-owned stream could deliver after the
  conversation swapped, bleeding the old turn's content and queued counts into
  the fresh chat. The stream now captures its conversation ID at start and
  drops every remaining event when the active conversation no longer matches.
- **Inbox showing "All Caught Up" with rows present.** The connector emitted
  inbox attachments in the chat-message schema (`type`/`filename`/`data`)
  while the app decodes inbox items with the `MessageAttachment` schema
  (`id`/`kind`/`fileName`/`mimeType`). One malformed attachment failed the
  entire items decode, so the inbox appeared empty. The connector now
  normalizes attachments to the app's schema (server-side fix; no app change
  needed).
- **Notification storm / phantom "Turn complete" pushes (connector).** The
  connector's native watch re-created its per-session terminal-dedupe state on
  every WebSocket reconnect, so a finished-but-idle session re-fired a
  "Response ready" push to every device on each reconnect cycle. The dedupe
  state now survives reconnects and only clears when a genuinely new turn
  starts (server-side fix; no app change needed).
- **WebSocket keepalive hardening (the reconnect flap).** Three-part fix so the
  socket survives silent reaping instead of flapping between Connected and
  Offline:
  - `GatewayClient` sends a heartbeat ping every 25 seconds on the live socket.
    A socket silently killed by NAT/cellular reaping or iOS suspension is
    detected within one interval instead of on the next user request.
  - Keepalive ping teardown now requires 3 CONSECUTIVE failed pings before
    killing the socket. A single slow pong on cellular no longer murders a
    healthy socket (the old 60s death pattern).
  - `reconnectIfNeeded()` liveness probe is 2-strike with a scheduled 3s retry,
    and the probe timeout widened 8s -> 15s. Brief NAT/carrier stalls no longer
    trigger the reconnect storm while genuinely dead sockets still recover.
- **Queue status bar button overlap (131.19).** View / Hold buttons no longer
  compress or overlap on narrow screens; status text truncates first.
- **Queue status bar hide-while-sending (131.19).** The bar hides the moment
  the last queued item is leased and actively sending - it is a WAITING
  indicator, not a send-status indicator.
- **Queue manager Send Now + trash (131.20).** Queue rows get a Send Now
  action that force-submits that message immediately; the status bar gets a
  trash button that clears all queued messages for the current conversation in
  one tap.
- **Left-edge swipe-back outside rich chat (131.18).** Swipe from the left
  edge pops the tab navigation stack or switches tabs at root; skipped in rich
  chat mode where the session drawer owns the left edge.
- **Skills and Cron empty lists (131.18, connector).** `/v1/skills` now
  returns the real command catalog (248 skills) and `/v1/cron` returns real
  jobs from the cron store (30 jobs).
- **Reset Connection lands in onboarding (131.10).** Reset now wipes to the
  pairing flow instead of returning to chat.
- **Reset Connection clears cookies (131.11).** The session cookie survived a
  wipe and reconnected the app behind the onboarding screen; credentials,
  cookies, and cache are now purged together.
- **Connect respects deliberate disconnect (131.12).** Background triggers no
  longer reconnect a deliberately-disconnected app, killing the onboarding/chat
  flap.
- **installationID survives Reset Connection (131.13).** Reset minted a fresh
  installation ID, so the old pairing code (bound to the old ID) was rejected
  with 401. The device's permanent identity now survives reset; only
  credentials and pairing are wiped.

### Security

- Dashboard login no longer exposes the Kallisti pairing-code form. Pairing
  still works via the authenticated password-login path.

## [0.2.6] - 2026-08-18

Current build release (build 128.93). This is the private beta build being
distributed to TestFlight testers.

### Added

- Embedded TUI terminal mode: a real terminal emulator (SwiftTerm) wired to
  the connector's `/v1/terminal` PTY bridge, so the chat surface can run an
  actual Hermes TUI session with live stdout, reasoning status, and tool
  activity. Includes keyboard resize behavior, a dismiss button, and touch
  mode toggle (select by default, scroll option).
- Session resume for TUI: resumable Hermes sessions are listed on terminal
  open with a one-tap Resume / Start new / Cancel dialog. The bridge spawns
  `hermes --tui --resume <id>` instead of a fresh session, so returning to
  the terminal keeps the same conversation.
- TUI reconnect on tab return: returning to the chat tab restarts the
  terminal session instead of leaving a dead "terminal closed" screen.
- TUI touch scroll: explicit pan gesture drives terminal scrolling so swiping
  scrolls while long-press still selects text.
- Notes-as-session sync: each note syncs to Hermes as its own titled session,
  and each edit appends as a message to that session.
- Notes sync scheduling: manual or timed sync (2m, 5m, 15m, 30m, 60m, 3h, 6h,
  12h, 24h, off) with a live progress bar and per-note sync button.
- Real reasoning bubble during note sync: the live chat-style OCR thinking
  bubble (brain icon, timer, expandable text) surfaces the agent's actual
  reasoning instead of a static status line, with a watchdog so sync can
  never hang silently.
- Health background sync toggle in Permissions. HealthKit background delivery
  is programmatic (there is no iOS system switch), so the app now exposes an
  in-app toggle backed by the signed build's
  `com.apple.developer.healthkit.background-delivery` entitlement, with the
  resulting On/Off state reflected on the Health permission card.
- Gateway restart authentication: preflight and restart calls now interpolate
  the target correctly and prefer the native bearer token when present,
  falling back to the session cookie for pairing/basic-auth logins.
- Live terminal stdout: tool `output` streams through the native gateway path
  with SSE parity and awaiting-user teardown for server-turn watch.
- Config editor: YAML validation, Save & Restart Gateway, realtime restart
  overlay, line numbers, and YAML indent/outdent/comment tools.
- Unified iPad navigation: bottom tab bar on both iPhone and iPad; Skills and
  Cron moved under Settings > Agent Tools and wired to real connector
  endpoints (19 real skills, full cron CRUD).
- Clarify answers from within chat via the ClarifyCard instead of leaving the
  turn stuck.
- Authenticated connector endpoints for skills/cron management and config
  read/write, available to paired devices.

### Fixed

- TUI black screen on first mount: terminal output is buffered until the view
  mounts, and SwiftTerm size is pushed on appearance.
- TUI freeze after Settings round-trip: the terminal restarts on tab return
  instead of showing a dead "terminal closed" state.
- Send button dead touch: the queue gesture no longer swallows button taps
  (drag minimum distance raised to a real threshold; suppression only arms
  when the queue actually fires and always resets on gesture end).
- Blank live-thought expansion: replaced the nil mask path that blanked the
  whole reasoning view on expand, and fixed resize for processed (non
  streaming) bubbles.
- Tool results routing: `tool.complete` resultPreview now renders in the
  terminal view instead of falling back to raw JSON debug dumps
  (`String(describing:)` on NativeJSONValue replaced with real JSON).
- YAML editor gutter: line numbers stay in sync when scrolling.
- Notes: selectable previous-note rows on iPad, canvas resizes with the
  sidebar, sync dirty-filter covers pre-sync notes, wider ruled paper lines
  and margins, live OCR readout banner, sync error interpolation fixed.
- Composer enablement gates on live socket status instead of a stale host
  probe, so the keyboard comes up even when the host info probe is degraded.
- Notes sync only pushes real activity: blank, untouched, or legacy notes are
  skipped; the sync turn carries an explicit instruction prompt so the agent
  knows it's an automatic background sync.
- Push registration storms and cookie-aware context window (b94 fixes).
- Session-not-found after resume: the app re-points to the live resumed
  session ID.
- History decode handles both `text` and `content` wire shapes.

### Security

- Gateway restart, push registration, and live activity calls no longer leak
  stale stored tokens; every request refreshes the native bearer or rides the
  session cookie.

## [0.2.5] - 2026-08-13

### Added

- Live stdout streaming (`tool.output`) through the connector with
  subagent-ordering guard and attachment directive stripping.
- Auxiliary model task slots raised to the canonical count (7 -> 11,
  matching the gateway), with a searchable auxiliary model picker: filter by
  model name, provider ID, or provider display name, server default pinned.
- Real-time model filter search and reset credential purge on onboarding.
- Cookie-auth support for sensor uploads and Live Activity registration.
- Chat text color field in Appearance settings.

### Fixed

- Marquee model pill: open pill scrolls, closed pill stays static; pretty
  model names in the hub; two-row hub names.
- Turn-complete dedup so a reply is not rendered twice.
- Upload request timeouts for large attachments (`file.attach` now sends
  `data_url`, not `content_base64`).
- Push registration storm killed; cookie-aware context window.
- Native watch no longer sends spurious "Turn complete" pushes: unwatch on
  stream completion.
- Messages ordered by monotonic id, not wall-clock timestamp.
- Launch-to-usable latency and connection stability; green-dot status
  restored through the native status funnel.
- iPad model pill text no longer collapses to zero width.

## [0.2.4] - 2026-08-12

### Added

- Keepalive armor and live tool rail visibility.
- Realtime model filter search field.
- Onboarding reset that also purges stored credentials.

### Fixed

- Fresh-install onboarding no longer loops; relay state resets cleanly.
- Inbox notification attachments render.
- Photo library: missing plist key added, save-to-photos crash fixed, video
  file upload and file attachment RPC fixed.
- Settings exclusivity crash and zoom/save auth paths.
- Launch onboarding flash eliminated; delivery status restored.
- Native connection status authority: status funnel updates the container
  handler, fixing latency readouts and the green dot.
- Connect probe timeout widened (5s -> 12s) for slower gateways.
- Onboarding permission requests include speech recognition.

## [0.2.3] - 2026-08-11

### Added

- Per-device inbox with installation-ID scoping, so push notifications land in the right device's inbox on multi-device setups.
- Inbox notification action window with dismiss and snooze actions from the notification detail sheet.
- Realtime connector latency readout in Settings, polled while the gateway is connected (not just while Settings is visible), with danger coloring above 300 ms.
- Searchable auxiliary model picker: filter by model name, provider ID, or provider display name, with the server default pinned at top.
- Software update check in Settings backed by the connector: truthful behind-count from git history, expandable changelog, and Update Now / Skip actions.
- Stall banner with realtime ticking snapshot, token counter, and a 60-second thinking-only timeout so a dead stream never looks like a live turn.
- Live Activity phase mirrors for stall and error states, with session-type tracking and foreground marking.
- Launch behavior matches desktop: opens the last active conversation, or a fresh ready-to-go session when there is none.
- Authenticated update-check endpoint on the connector, available to paired devices.

### Fixed

- Push registration in pairing mode: the app now rides the gateway session cookie for facade calls instead of demanding a bearer token that pairing-mode login no longer stores. This eliminated the persistent 401 loop on `POST /v1/push/register` and got real APNs tokens registered.
- Auxiliary model switching now uses `cli.exec` config-set through the native gateway, surfaces real errors instead of generic failures, and covers the full dynamic 7-task catalog.
- WebSocket churn on connect: an in-flight guard prevents minting multiple tickets and opening duplicate sockets when `connect()` is called concurrently.
- Session-not-found after resume: the app now re-points its session map to the live resumed session ID instead of continuing with the stale original.
- All-devices filter no longer includes cron/kanban/tool/subagent noise, so the session list shows real user sessions.
- Model switching no longer spins and reverts; the loading surface no longer flickers during mid-session reconnect; typed drafts survive reconnect and view recreation.
- Live Activity lock-screen status stays current instead of freezing on "Thinking"; the activity is cleaned up on expiry.
- Dark wallpaper brightened to a usable watermark; light wallpaper rebuilt at the correct scale with feathered edges.
- Stream settles on completion push, so a test notification no longer leaves the chat looking like it is still thinking.
- Chat image thumbnails render aspect-fit instead of squished.
- Fresh-install onboarding no longer loops; once connected, the app never tears down to the splash surface again.
- History decode handles both `text` and `content` wire shapes from the gateway, so older chats open correctly.
- Default keychain access group restored before the shared group, fixing credential lookup on reinstall.

### Security

- Removed internal LAN addresses, Apple Developer team identifiers from export options, a personal-domain privacy URL, and signed IPA artifacts from the public repository and its history.
- Signed application archives are no longer tracked; export configuration with team identifiers stays local.

## [0.2.2] - 2026-08-10

### Added

- In-flight checkpoint persistence on background-task expiry: an interrupted turn can be resumed instead of lost.
- Local notification when a background task expires, so the user knows the turn did not finish.
- Connection overlay extended to mid-session reconnects with debounce, so churn does not flash raw errors.
- Tool `start` / `complete` events wired through the native WebSocket path.
- Regression tests for checkpoint recovery, draft persistence, and connection overlay behavior.
- Push registration endpoint accepts native gateway bearer authentication, not just connector tokens.

### Fixed

- Draft text survives reconnect view recreation.
- Version display, reconnect UX, and status text corrected in Settings.
- Stall detection no longer mislabels healthy long turns as stalled.

## [0.2.1] - 2026-08-09

### Fixed

- WebSocket frames over 1 MB no longer kill the connection. Image-generation
  tool results routinely exceed 2-4 MB as base64, and the previous 1 MB
  receive cap silently dropped the terminal event, leaving the app stuck on
  "Thinking" until the stall watchdog fired and the turn was re-submitted
  (double billing). The client now accepts frames up to 64 MB.
- Long-running tool calls no longer trip the stall watchdog. The watchdog
  skips its check while a tool is in flight, so image generation and other
  multi-minute tool executions complete without a false "took too long" and
  without re-running the same job.
- Push registration, gateway logs, restart, and live activity registration
  now refresh the native gateway bearer before every request, eliminating
  401s caused by expired stored tokens.
- The loading surface no longer tears down mid-session on reconnect. A
  `hasConnectedOnce` flag keeps the chat UI mounted during recovery, so typed
  text and scroll position survive connection churn.
- Ping timeout raised to 45 seconds so a stalled gateway event loop during a
  heavy turn cannot kill a healthy socket.
- The model pill reloads from the gateway on connect, so it no longer shows a
  bare green dot until manually opened.
- iPad model pill layout fixed: the model name text now has layout priority
  and renders instead of being compressed to zero width.
- Live Activity lockscreen status now includes an elapsed-time heartbeat, so
  the lockscreen no longer freezes on "Thinking".
- Opening a previous session falls back to `session.resume` before creating a
  blank chat, restoring the correct transcript after gateway restarts.

### Added

- Tool-in-flight watchdog exemption with an absolute job deadline safety net.
- Native bearer token priority for all connector-facing HTTP calls.
- Speech recognition permission included in onboarding permission requests.

## [0.2.0-build.50] - 2026-08-09

### Added

- Full-screen branded connection overlay showing real-time connection stages during connect, reconnect, and relaunch with stored login.
- Manual Reset Connection action from the overlay (after a short delay) and from Settings, for tearing down stale transport and starting a fresh authenticated connection.
- Historical MEDIA references are resolved after conversation reload so images remain accessible after gateway restarts.
- Media path normalization: client converts absolute server paths to relative forms; connector resolves both current default-profile and legacy per-profile image roots.
- Focused regression tests for connection stage transitions, overlay visibility and error suppression, reset idempotency, and media path backward compatibility.

### Fixed

- Reconnect and relaunch states no longer flash the raw "Cannot connect to gateway" error while recovery is in progress.
- Stale connection state is properly torn down on manual reset without overlapping reconnect loops.
- Stored credentials and conversations are preserved through manual connection reset.

## [0.2.0-build.49] - 2026-08-09

### Added

- Native `MEDIA:` path conversion to authenticated inline-image Markdown.
- Restricted `/v1/native/media` connector endpoint for agent-generated images.
- Native image upload staging through `image.attach_bytes` before prompt submission.
- Regression coverage for inline media, image staging, fast acknowledgments, stalled streams, and pre-ack placeholder ownership.

### Fixed

- Follow-up messages no longer wait behind a stream that ended after acknowledgment without a terminal event.
- Pre-ack transcript polling no longer removes or duplicates the active thinking placeholder.
- Accepted outbox jobs settle through deadline-aware recovery and release the per-conversation FIFO lease.
- Native gateway responses arriving during WebSocket send registration are no longer dropped.
- Public media URLs use the configured HTTPS gateway host instead of an unreachable private connector port.
- Authenticated image requests attach credentials for `/v1/native/` routes without leaking them to arbitrary image hosts.
- Connector media downloads accept validated gateway cookie sessions, native bearer tokens, and persisted paired-device tokens.
- Connector restarts rehydrate paired-device tokens and preserve native-watch refresh credentials.
- Push registration and Live Activity cleanup no longer block chat startup or leave stale activity state.

### Security

- Media serving is limited to configured Hermes image/media roots.
- Unsupported file types, traversal attempts, missing files, and files larger than 10 MB are rejected.
- Gateway cookie and bearer validation is delegated to the gateway's authenticated identity endpoint.
- Public source and documentation were scrubbed of personal names, user-specific paths, private host addresses, device identifiers, captured production logs, and signing credentials.

## [0.2.0-build.41] - 2026-08-09

### Added

- One-time Kallisti pairing-code sign-in for native gateway mode.
- Direct native WebSocket transport for chat, session history, model selection, and profile selection.
- Gateway status, logs, restart operations, push notifications, and Live Activities.

### Fixed

- WebSocket send timeouts and response-registration races.
- Session identity and reconnect handling.
- Model and profile selection against the active session.
- Native basic-auth and pairing-cookie login flows.
- Gateway ticket refresh and session creation timeouts.

## [0.1.0] - 2026-08-03

### Added

- Initial Kallisti branding and iOS application structure.
- Connector, relay, widget, watch, intents, and notification-extension targets.

### Attribution

- Built from the earlier Herald mobile client foundation under the repository's MIT license history.
## [Nightly] - $(date '+%Y-%m-%d')

### Fixed (daily reconciliation)

- 2026-08-20 13:28 PT | a2eea3b4 | test entry - nightly pipeline setup
- 2026-08-20 14:13 PT | 76b703ed | 132.1: thought bubble reset on new note, ALL note attachments (photos/scans/files) included in enrichment prompt, OCR reading-order sort + per-stroke groundwork
- 2026-08-20 15:22 PT | ff4d6310 | 132.2: fix notes ruled lines stopping short on right side in portrait - bounds KVO self-heal in PencilCanvasRepresentable; dev-signed direct install on CDF iPad + CDF iPhone
- 2026-08-20 15:47 PT | a81d2b0f | 132.3: launch-surface flap fix (never re-show after connected), smart completion notifications (real reply text in push+inbox via session history), inbox select-all + bulk dismiss, in-app banner suppression
- 2026-08-20 15:58 PT | 43d4762a | 132.4: retire launch surface once connected + activeModel known - model refresh no longer holds cold start on Connected, <model>
- 2026-08-20 19:25 PT | a3f04936 | 133.0: context-aware note enrichment - model reads drawing/attachments as source of truth, enriches by note type (study/meeting/shopping/personal/doodle), web research + hyperlinks enabled, callback isolation maintained
- 2026-08-20 21:00 PT | bc64423f | Bug 1: enrichment 2x->4x drawing render; Bug 5: composer keyboard after Settings->Chat; driver scale + status resync. 133.1 installed both devices
- 2026-08-20 22:02 PT | 16b5b2f8 | 133.2: enrichment renders markdown (clickable hyperlinks + inline images when model emits them); writing pad sidebar resize (132.2 bounds observer) ships to devices; connector note.enrich prompt synced with hyperlink+inline-image rules

## [Nightly] - $(date '+%Y-%m-%d')

### Fixed (daily reconciliation)

- 2026-08-21 07:08 PT | e8463a52 | nightly 133.3 installed on iPad A16 + iPhone 15 Pro (dev-signed direct push)
- 2026-08-21 08:11 PT | e8463a52 | 133.4: notes ink auto-scales to fit column when sidebar present (was clipping at right edge)
- 2026-08-21 08:22 PT | 1cba468b | 1cba468b 133.4: autoscale v2 - contentSize=target/zoom coordinate math fix, paper spans content coords, left anchor (installed both devices)
- 2026-08-21 11:22 PT | e3b2943a | 133.5 UAT: phantom new-chat ghost fixes (probe outbox before requeue, cap server-turn watch 75s, label remote turns, stable new-chat id) installed on iPad A16 + iPhone 15 Pro (dev-signed direct push)
- 2026-08-21 11:37 PT | 7809a097 | 135.5 TESTFLIGHT RELEASE: promoted nightly->main (e0782af5), phantom new-chat ghost fixes, delivery UUID 5cab2423, VALID

## [Nightly] - $(date '+%Y-%m-%d')

### Fixed (daily reconciliation)

- 2026-08-21 07:08 PT | e8463a52 | nightly 133.3 installed on iPad A16 + iPhone 15 Pro (dev-signed direct push)
- 2026-08-21 08:11 PT | e8463a52 | 133.4: notes ink auto-scales to fit column when sidebar present (was clipping at right edge)
- 2026-08-21 08:22 PT | 1cba468b | 1cba468b 133.4: autoscale v2 - contentSize=target/zoom coordinate math fix, paper spans content coords, left anchor (installed both devices)
- 2026-08-21 11:22 PT | e3b2943a | 133.5 UAT: phantom new-chat ghost fixes (probe outbox before requeue, cap server-turn watch 75s, label remote turns, stable new-chat id) installed on iPad A16 + iPhone 15 Pro (dev-signed direct push)
- 2026-08-21 11:37 PT | 7809a097 | 135.5 TESTFLIGHT RELEASE: promoted nightly->main (e0782af5), phantom new-chat ghost fixes, delivery UUID 5cab2423, VALID
- 2026-08-22 11:19 PT | 233bd523 | 135.10: fix connect() zombie-hang watchdog (device sleep silently suspends URLRequest, isConnecting latched forever -> permanent Connecting screen until force-quit); fix PencilCanvasRepresentable updateUIView churn corrupting the open color/attribute popover during reasoning-stream re-renders (blank gray void bug)
- 2026-08-22 12:16 PT | d52ca286 | 135.11: break checkpoint write/re-render feedback loop (re-entrancy latch + throttle + no redundant state writes) + memory-warning/willTerminate safety save
- 2026-08-22 13:01 PT | 2d38d3e3 | 135.12: memory-leak diagnostics + OCR churn reduction - in-app phys_footprint logger (30s + memory-warning), cap recognition render at 2000px longest side (was unbounded 4x full-canvas), skip OCR when drawing content unchanged
- 2026-08-22 13:15 PT | 46885045 | 135.13: FIX instant-close - removed the 135.11 didReceiveMemoryWarning observer that called persistDrawing+OCR render at the moment iOS demanded free memory (guaranteed Jetsam kill); willTerminate now writes blob directly without OCR; observers removed on disappear
- 2026-08-22 13:38 PT | 6039888f | 135.14: FIX checkpoint ballooning - pruneOldCheckpoints was never called, so every snapshot (5min auto + backgrounding + restore + manual) copied FULL drawing + ALL attachment blobs (multi-MB screen recordings) into a new bundle that accumulated unbounded = disk-write storm + memory pressure. Now prunes to latest 10 after every snapshot.
- 2026-08-22 13:53 PT | 94f4b7f5 | 135.15: FIX chats flying off screen - userScrollTimer was declared but never scheduled, so any accidental drag left isUserScrolling=true forever (streaming piled up off-screen, Jump-to-Latest arrow blocked). Drag end now schedules a 2.5s grace timer that releases scroll ownership so auto-follow resumes and the arrow always works.
- 2026-08-22 14:08 PT | 13005c05 | 135.16: FIX Queue sheet row buttons dead - .contentShape(Rectangle()) + .onTapGesture on the row container swallowed every tap so Send/Edit/Delete buttons inside never fired. Switched to simultaneousGesture so the row actions receive taps.
- 2026-08-22 14:36 PT | 7c661569 | 135.17: stop checkpoint from melting long-note devices (metadata-only snapshots - no attachment blob copies) + live background-task viewer in Canvas Live tab (SSE process stream + Stop button)
- 2026-08-22 14:49 PT | 62a3a93d | 135.18: REMOVE entire checkpoint system from notes (auto loop, backgrounding snapshots, manual UI, restore, Settings picker) - was causing instant-close on opening saved notes and device melt
- 2026-08-22 15:10 PT | 7ec0a399 | 135.19: FIX camera dead buttons - Use Photo/Retake left fullScreenCover presented (showCamera never reset + system picker never dismissed); app froze until force-quit
- 2026-08-22 15:26 PT | ec4141aa | 135.20: FIX notes instant-close - revisions.json embedded FULL drawing blob per revision (100MB+); note-open loaded/decoded the whole file -> 3GB Jetsam kill. Now metadata-only + migrate-on-load + cap 30 revisions
- 2026-08-22 15:28 PT | ba890482 | 135.21: FIX custom color picker (PKToolPicker attribute palette) vanishing - updateUIView forced becomeFirstResponder/setVisible on every parent re-render while the palette popover was open; now never touches responder/visibility when picker is visible
- 2026-08-22 15:33 PT | 0759c546 | 135.22: FIX notes instant-close for the bloated legacy notes - pre-flight migration shrinks oversized revisions.json (huge embedded drawingData blobs) BEFORE decoding so note-open never loads 100-300MB into memory

## [Nightly] - $(date '+%Y-%m-%d')

### Fixed (daily reconciliation)

- 2026-08-21 07:08 PT | e8463a52 | nightly 133.3 installed on iPad A16 + iPhone 15 Pro (dev-signed direct push)
- 2026-08-21 08:11 PT | e8463a52 | 133.4: notes ink auto-scales to fit column when sidebar present (was clipping at right edge)
- 2026-08-21 08:22 PT | 1cba468b | 1cba468b 133.4: autoscale v2 - contentSize=target/zoom coordinate math fix, paper spans content coords, left anchor (installed both devices)
- 2026-08-21 11:22 PT | e3b2943a | 133.5 UAT: phantom new-chat ghost fixes (probe outbox before requeue, cap server-turn watch 75s, label remote turns, stable new-chat id) installed on iPad A16 + iPhone 15 Pro (dev-signed direct push)
- 2026-08-21 11:37 PT | 7809a097 | 135.5 TESTFLIGHT RELEASE: promoted nightly->main (e0782af5), phantom new-chat ghost fixes, delivery UUID 5cab2423, VALID
- 2026-08-22 11:19 PT | 233bd523 | 135.10: fix connect() zombie-hang watchdog (device sleep silently suspends URLRequest, isConnecting latched forever -> permanent Connecting screen until force-quit); fix PencilCanvasRepresentable updateUIView churn corrupting the open color/attribute popover during reasoning-stream re-renders (blank gray void bug)
- 2026-08-22 12:16 PT | d52ca286 | 135.11: break checkpoint write/re-render feedback loop (re-entrancy latch + throttle + no redundant state writes) + memory-warning/willTerminate safety save
- 2026-08-22 13:01 PT | 2d38d3e3 | 135.12: memory-leak diagnostics + OCR churn reduction - in-app phys_footprint logger (30s + memory-warning), cap recognition render at 2000px longest side (was unbounded 4x full-canvas), skip OCR when drawing content unchanged
- 2026-08-22 13:15 PT | 46885045 | 135.13: FIX instant-close - removed the 135.11 didReceiveMemoryWarning observer that called persistDrawing+OCR render at the moment iOS demanded free memory (guaranteed Jetsam kill); willTerminate now writes blob directly without OCR; observers removed on disappear
- 2026-08-22 13:38 PT | 6039888f | 135.14: FIX checkpoint ballooning - pruneOldCheckpoints was never called, so every snapshot (5min auto + backgrounding + restore + manual) copied FULL drawing + ALL attachment blobs (multi-MB screen recordings) into a new bundle that accumulated unbounded = disk-write storm + memory pressure. Now prunes to latest 10 after every snapshot.
- 2026-08-22 13:53 PT | 94f4b7f5 | 135.15: FIX chats flying off screen - userScrollTimer was declared but never scheduled, so any accidental drag left isUserScrolling=true forever (streaming piled up off-screen, Jump-to-Latest arrow blocked). Drag end now schedules a 2.5s grace timer that releases scroll ownership so auto-follow resumes and the arrow always works.
- 2026-08-22 14:08 PT | 13005c05 | 135.16: FIX Queue sheet row buttons dead - .contentShape(Rectangle()) + .onTapGesture on the row container swallowed every tap so Send/Edit/Delete buttons inside never fired. Switched to simultaneousGesture so the row actions receive taps.
- 2026-08-22 14:36 PT | 7c661569 | 135.17: stop checkpoint from melting long-note devices (metadata-only snapshots - no attachment blob copies) + live background-task viewer in Canvas Live tab (SSE process stream + Stop button)
- 2026-08-22 14:49 PT | 62a3a93d | 135.18: REMOVE entire checkpoint system from notes (auto loop, backgrounding snapshots, manual UI, restore, Settings picker) - was causing instant-close on opening saved notes and device melt
- 2026-08-22 15:10 PT | 7ec0a399 | 135.19: FIX camera dead buttons - Use Photo/Retake left fullScreenCover presented (showCamera never reset + system picker never dismissed); app froze until force-quit
- 2026-08-22 15:26 PT | ec4141aa | 135.20: FIX notes instant-close - revisions.json embedded FULL drawing blob per revision (100MB+); note-open loaded/decoded the whole file -> 3GB Jetsam kill. Now metadata-only + migrate-on-load + cap 30 revisions
- 2026-08-22 15:28 PT | ba890482 | 135.21: FIX custom color picker (PKToolPicker attribute palette) vanishing - updateUIView forced becomeFirstResponder/setVisible on every parent re-render while the palette popover was open; now never touches responder/visibility when picker is visible
- 2026-08-22 15:33 PT | 0759c546 | 135.22: FIX notes instant-close for the bloated legacy notes - pre-flight migration shrinks oversized revisions.json (huge embedded drawingData blobs) BEFORE decoding so note-open never loads 100-300MB into memory

## [Nightly] - $(date '+%Y-%m-%d')

### Fixed (daily reconciliation)

- 2026-08-21 07:08 PT | e8463a52 | nightly 133.3 installed on iPad A16 + iPhone 15 Pro (dev-signed direct push)
- 2026-08-21 08:11 PT | e8463a52 | 133.4: notes ink auto-scales to fit column when sidebar present (was clipping at right edge)
- 2026-08-21 08:22 PT | 1cba468b | 1cba468b 133.4: autoscale v2 - contentSize=target/zoom coordinate math fix, paper spans content coords, left anchor (installed both devices)
- 2026-08-21 11:22 PT | e3b2943a | 133.5 UAT: phantom new-chat ghost fixes (probe outbox before requeue, cap server-turn watch 75s, label remote turns, stable new-chat id) installed on iPad A16 + iPhone 15 Pro (dev-signed direct push)
- 2026-08-21 11:37 PT | 7809a097 | 135.5 TESTFLIGHT RELEASE: promoted nightly->main (e0782af5), phantom new-chat ghost fixes, delivery UUID 5cab2423, VALID
- 2026-08-22 11:19 PT | 233bd523 | 135.10: fix connect() zombie-hang watchdog (device sleep silently suspends URLRequest, isConnecting latched forever -> permanent Connecting screen until force-quit); fix PencilCanvasRepresentable updateUIView churn corrupting the open color/attribute popover during reasoning-stream re-renders (blank gray void bug)
- 2026-08-22 12:16 PT | d52ca286 | 135.11: break checkpoint write/re-render feedback loop (re-entrancy latch + throttle + no redundant state writes) + memory-warning/willTerminate safety save
- 2026-08-22 13:01 PT | 2d38d3e3 | 135.12: memory-leak diagnostics + OCR churn reduction - in-app phys_footprint logger (30s + memory-warning), cap recognition render at 2000px longest side (was unbounded 4x full-canvas), skip OCR when drawing content unchanged
- 2026-08-22 13:15 PT | 46885045 | 135.13: FIX instant-close - removed the 135.11 didReceiveMemoryWarning observer that called persistDrawing+OCR render at the moment iOS demanded free memory (guaranteed Jetsam kill); willTerminate now writes blob directly without OCR; observers removed on disappear
- 2026-08-22 13:38 PT | 6039888f | 135.14: FIX checkpoint ballooning - pruneOldCheckpoints was never called, so every snapshot (5min auto + backgrounding + restore + manual) copied FULL drawing + ALL attachment blobs (multi-MB screen recordings) into a new bundle that accumulated unbounded = disk-write storm + memory pressure. Now prunes to latest 10 after every snapshot.
- 2026-08-22 13:53 PT | 94f4b7f5 | 135.15: FIX chats flying off screen - userScrollTimer was declared but never scheduled, so any accidental drag left isUserScrolling=true forever (streaming piled up off-screen, Jump-to-Latest arrow blocked). Drag end now schedules a 2.5s grace timer that releases scroll ownership so auto-follow resumes and the arrow always works.
- 2026-08-22 14:08 PT | 13005c05 | 135.16: FIX Queue sheet row buttons dead - .contentShape(Rectangle()) + .onTapGesture on the row container swallowed every tap so Send/Edit/Delete buttons inside never fired. Switched to simultaneousGesture so the row actions receive taps.
- 2026-08-22 14:36 PT | 7c661569 | 135.17: stop checkpoint from melting long-note devices (metadata-only snapshots - no attachment blob copies) + live background-task viewer in Canvas Live tab (SSE process stream + Stop button)
- 2026-08-22 14:49 PT | 62a3a93d | 135.18: REMOVE entire checkpoint system from notes (auto loop, backgrounding snapshots, manual UI, restore, Settings picker) - was causing instant-close on opening saved notes and device melt
- 2026-08-22 15:10 PT | 7ec0a399 | 135.19: FIX camera dead buttons - Use Photo/Retake left fullScreenCover presented (showCamera never reset + system picker never dismissed); app froze until force-quit
- 2026-08-22 15:26 PT | ec4141aa | 135.20: FIX notes instant-close - revisions.json embedded FULL drawing blob per revision (100MB+); note-open loaded/decoded the whole file -> 3GB Jetsam kill. Now metadata-only + migrate-on-load + cap 30 revisions
- 2026-08-22 15:28 PT | ba890482 | 135.21: FIX custom color picker (PKToolPicker attribute palette) vanishing - updateUIView forced becomeFirstResponder/setVisible on every parent re-render while the palette popover was open; now never touches responder/visibility when picker is visible
- 2026-08-22 15:33 PT | 0759c546 | 135.22: FIX notes instant-close for the bloated legacy notes - pre-flight migration shrinks oversized revisions.json (huge embedded drawingData blobs) BEFORE decoding so note-open never loads 100-300MB into memory

## [Nightly] - $(date '+%Y-%m-%d')

### Fixed (daily reconciliation)

- 2026-08-21 07:08 PT | e8463a52 | nightly 133.3 installed on iPad A16 + iPhone 15 Pro (dev-signed direct push)
- 2026-08-21 08:11 PT | e8463a52 | 133.4: notes ink auto-scales to fit column when sidebar present (was clipping at right edge)
- 2026-08-21 08:22 PT | 1cba468b | 1cba468b 133.4: autoscale v2 - contentSize=target/zoom coordinate math fix, paper spans content coords, left anchor (installed both devices)
- 2026-08-21 11:22 PT | e3b2943a | 133.5 UAT: phantom new-chat ghost fixes (probe outbox before requeue, cap server-turn watch 75s, label remote turns, stable new-chat id) installed on iPad A16 + iPhone 15 Pro (dev-signed direct push)
- 2026-08-21 11:37 PT | 7809a097 | 135.5 TESTFLIGHT RELEASE: promoted nightly->main (e0782af5), phantom new-chat ghost fixes, delivery UUID 5cab2423, VALID
- 2026-08-22 11:19 PT | 233bd523 | 135.10: fix connect() zombie-hang watchdog (device sleep silently suspends URLRequest, isConnecting latched forever -> permanent Connecting screen until force-quit); fix PencilCanvasRepresentable updateUIView churn corrupting the open color/attribute popover during reasoning-stream re-renders (blank gray void bug)
- 2026-08-22 12:16 PT | d52ca286 | 135.11: break checkpoint write/re-render feedback loop (re-entrancy latch + throttle + no redundant state writes) + memory-warning/willTerminate safety save
- 2026-08-22 13:01 PT | 2d38d3e3 | 135.12: memory-leak diagnostics + OCR churn reduction - in-app phys_footprint logger (30s + memory-warning), cap recognition render at 2000px longest side (was unbounded 4x full-canvas), skip OCR when drawing content unchanged
- 2026-08-22 13:15 PT | 46885045 | 135.13: FIX instant-close - removed the 135.11 didReceiveMemoryWarning observer that called persistDrawing+OCR render at the moment iOS demanded free memory (guaranteed Jetsam kill); willTerminate now writes blob directly without OCR; observers removed on disappear
- 2026-08-22 13:38 PT | 6039888f | 135.14: FIX checkpoint ballooning - pruneOldCheckpoints was never called, so every snapshot (5min auto + backgrounding + restore + manual) copied FULL drawing + ALL attachment blobs (multi-MB screen recordings) into a new bundle that accumulated unbounded = disk-write storm + memory pressure. Now prunes to latest 10 after every snapshot.
- 2026-08-22 13:53 PT | 94f4b7f5 | 135.15: FIX chats flying off screen - userScrollTimer was declared but never scheduled, so any accidental drag left isUserScrolling=true forever (streaming piled up off-screen, Jump-to-Latest arrow blocked). Drag end now schedules a 2.5s grace timer that releases scroll ownership so auto-follow resumes and the arrow always works.
- 2026-08-22 14:08 PT | 13005c05 | 135.16: FIX Queue sheet row buttons dead - .contentShape(Rectangle()) + .onTapGesture on the row container swallowed every tap so Send/Edit/Delete buttons inside never fired. Switched to simultaneousGesture so the row actions receive taps.
- 2026-08-22 14:36 PT | 7c661569 | 135.17: stop checkpoint from melting long-note devices (metadata-only snapshots - no attachment blob copies) + live background-task viewer in Canvas Live tab (SSE process stream + Stop button)
- 2026-08-22 14:49 PT | 62a3a93d | 135.18: REMOVE entire checkpoint system from notes (auto loop, backgrounding snapshots, manual UI, restore, Settings picker) - was causing instant-close on opening saved notes and device melt
- 2026-08-22 15:10 PT | 7ec0a399 | 135.19: FIX camera dead buttons - Use Photo/Retake left fullScreenCover presented (showCamera never reset + system picker never dismissed); app froze until force-quit
- 2026-08-22 15:26 PT | ec4141aa | 135.20: FIX notes instant-close - revisions.json embedded FULL drawing blob per revision (100MB+); note-open loaded/decoded the whole file -> 3GB Jetsam kill. Now metadata-only + migrate-on-load + cap 30 revisions
- 2026-08-22 15:28 PT | ba890482 | 135.21: FIX custom color picker (PKToolPicker attribute palette) vanishing - updateUIView forced becomeFirstResponder/setVisible on every parent re-render while the palette popover was open; now never touches responder/visibility when picker is visible
- 2026-08-22 15:33 PT | 0759c546 | 135.22: FIX notes instant-close for the bloated legacy notes - pre-flight migration shrinks oversized revisions.json (huge embedded drawingData blobs) BEFORE decoding so note-open never loads 100-300MB into memory

## [Nightly] - $(date '+%Y-%m-%d')

### Fixed (daily reconciliation)

- 2026-08-21 07:08 PT | e8463a52 | nightly 133.3 installed on iPad A16 + iPhone 15 Pro (dev-signed direct push)
- 2026-08-21 08:11 PT | e8463a52 | 133.4: notes ink auto-scales to fit column when sidebar present (was clipping at right edge)
- 2026-08-21 08:22 PT | 1cba468b | 1cba468b 133.4: autoscale v2 - contentSize=target/zoom coordinate math fix, paper spans content coords, left anchor (installed both devices)
- 2026-08-21 11:22 PT | e3b2943a | 133.5 UAT: phantom new-chat ghost fixes (probe outbox before requeue, cap server-turn watch 75s, label remote turns, stable new-chat id) installed on iPad A16 + iPhone 15 Pro (dev-signed direct push)
- 2026-08-21 11:37 PT | 7809a097 | 135.5 TESTFLIGHT RELEASE: promoted nightly->main (e0782af5), phantom new-chat ghost fixes, delivery UUID 5cab2423, VALID
- 2026-08-22 11:19 PT | 233bd523 | 135.10: fix connect() zombie-hang watchdog (device sleep silently suspends URLRequest, isConnecting latched forever -> permanent Connecting screen until force-quit); fix PencilCanvasRepresentable updateUIView churn corrupting the open color/attribute popover during reasoning-stream re-renders (blank gray void bug)
- 2026-08-22 12:16 PT | d52ca286 | 135.11: break checkpoint write/re-render feedback loop (re-entrancy latch + throttle + no redundant state writes) + memory-warning/willTerminate safety save
- 2026-08-22 13:01 PT | 2d38d3e3 | 135.12: memory-leak diagnostics + OCR churn reduction - in-app phys_footprint logger (30s + memory-warning), cap recognition render at 2000px longest side (was unbounded 4x full-canvas), skip OCR when drawing content unchanged
- 2026-08-22 13:15 PT | 46885045 | 135.13: FIX instant-close - removed the 135.11 didReceiveMemoryWarning observer that called persistDrawing+OCR render at the moment iOS demanded free memory (guaranteed Jetsam kill); willTerminate now writes blob directly without OCR; observers removed on disappear
- 2026-08-22 13:38 PT | 6039888f | 135.14: FIX checkpoint ballooning - pruneOldCheckpoints was never called, so every snapshot (5min auto + backgrounding + restore + manual) copied FULL drawing + ALL attachment blobs (multi-MB screen recordings) into a new bundle that accumulated unbounded = disk-write storm + memory pressure. Now prunes to latest 10 after every snapshot.
- 2026-08-22 13:53 PT | 94f4b7f5 | 135.15: FIX chats flying off screen - userScrollTimer was declared but never scheduled, so any accidental drag left isUserScrolling=true forever (streaming piled up off-screen, Jump-to-Latest arrow blocked). Drag end now schedules a 2.5s grace timer that releases scroll ownership so auto-follow resumes and the arrow always works.
- 2026-08-22 14:08 PT | 13005c05 | 135.16: FIX Queue sheet row buttons dead - .contentShape(Rectangle()) + .onTapGesture on the row container swallowed every tap so Send/Edit/Delete buttons inside never fired. Switched to simultaneousGesture so the row actions receive taps.
- 2026-08-22 14:36 PT | 7c661569 | 135.17: stop checkpoint from melting long-note devices (metadata-only snapshots - no attachment blob copies) + live background-task viewer in Canvas Live tab (SSE process stream + Stop button)
- 2026-08-22 14:49 PT | 62a3a93d | 135.18: REMOVE entire checkpoint system from notes (auto loop, backgrounding snapshots, manual UI, restore, Settings picker) - was causing instant-close on opening saved notes and device melt
- 2026-08-22 15:10 PT | 7ec0a399 | 135.19: FIX camera dead buttons - Use Photo/Retake left fullScreenCover presented (showCamera never reset + system picker never dismissed); app froze until force-quit
- 2026-08-22 15:26 PT | ec4141aa | 135.20: FIX notes instant-close - revisions.json embedded FULL drawing blob per revision (100MB+); note-open loaded/decoded the whole file -> 3GB Jetsam kill. Now metadata-only + migrate-on-load + cap 30 revisions
- 2026-08-22 15:28 PT | ba890482 | 135.21: FIX custom color picker (PKToolPicker attribute palette) vanishing - updateUIView forced becomeFirstResponder/setVisible on every parent re-render while the palette popover was open; now never touches responder/visibility when picker is visible
- 2026-08-22 15:33 PT | 0759c546 | 135.22: FIX notes instant-close for the bloated legacy notes - pre-flight migration shrinks oversized revisions.json (huge embedded drawingData blobs) BEFORE decoding so note-open never loads 100-300MB into memory
