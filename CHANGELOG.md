# Changelog

All notable Kallisti changes are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.1] - 2026-08-20

Current build release (build 131.22). Private beta build.

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