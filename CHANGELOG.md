# Changelog

One repository, two products. The app and the daemon have always moved on separate
version numbers, so their entries are kept apart rather than interleaved.

## Kallisti for iOS

All notable Kallisti changes are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.9.0] - 2026-11-28

### Fixed

- **A new chat's first message no longer vanishes.** On a fresh chat the first
  message could disappear, and leaving and returning showed the chat with no
  activity at all. Two causes, both about a conversation id that was not real yet:
  the id was persisted before the daemon had created the conversation, and the
  reload path then returned an empty stub for an id it could not resolve, replacing
  the message with nothing. The id is now persisted only once the daemon has bound
  it, and a reload keeps the rows already on screen instead of blanking a live chat.

## [1.8.9] - 2026-11-28

### Fixed

- **The Claude Code sign-in now shows as signed in.** The sign-in had been working:
  the daemon held a sealed token. It still displayed "Not signed in" because the
  status probe asked the CLI, and the CLI only knows about its own credentials file,
  keychain, or `CLAUDE_CODE_OAUTH_TOKEN` — not the daemon's encrypted store. The
  probe now asks with the token in its environment, which is the same variable a
  delegated run gets, so the answer matches reality.
- **Auxiliary role changes are accepted.** Picking a model for Fallback (or any
  role) was refused with `aux_role_unknown: role does not accept 'Fallback'`: the
  daemon matches its canonical key `fallback` and the app was sending the row's
  display label. The app now sends the key and keeps the label for the legacy
  connector path, which keys off labels.

### Also in this run, daemon side

- The code is committed with a second, separate Return — the actual sign-in bug.
- Queued task prompts no longer appear in the chat as if you typed them.

## [1.8.8] - 2026-11-27

### Fixed

- **The Claude Code sign-in no longer hangs up before the daemon answers.**
  "The code could not be sent" was the app's network timeout, not a rejection: the
  daemon holds that request open while the CLI exchanges the code (up to 120
  seconds) and `URLSession` gave up at its 60-second default. The app's ceiling is
  now above the daemon's.
- Two silent returns in the same flow now say what happened instead of leaving the
  sheet spinning with no message: a sign-in with no session id, and a missing
  daemon address.

### Earlier in this run, daemon side

- The pasted token is now exported to the harness process as
  `CLAUDE_CODE_OAUTH_TOKEN`; it was stored and never used.
- The daemon asks `claude auth status` instead of guessing from a credentials file.
- The token is sealed in the encrypted store rather than sitting in the database.
- Abandoned `setup-token` processes are killed on prune, which was the broken pipe.
- A dead sign-in names itself instead of reporting an errno.

## [1.8.7] - 2026-11-26

### Fixed

- **Ten photos, full size, no compression.** The picker offered ten and the daemon
  accepted eight, so a ten-photo message came back `attachments_invalid: a message
  may carry at most 8 files` after the operator had already been told ten was fine.
  Both are ten now, pinned by a test that derives from the shared constant.
- **Photos are uploaded as the bytes the library holds.** The app downscaled every
  image to 768 points on its longest edge, encoded JPEG at quality 0.5, then stepped
  the quality down until the file fit in 350 KB: a 12-megapixel photo arrived as
  roughly 0.2 megapixels. Those limits guarded a 1 MB relay body carrying base64,
  which no longer exists. The picker path now re-encodes nothing at all, and a
  camera capture is one high-quality encode.
- The budgets moved with it: the aggregate ceiling 64 MB to 256 MB (ten
  full-resolution PNGs exceed 64 MB) and the vision ceiling 8 MB to 24 MB (a
  full-resolution PNG was being silently skipped, so the model was told a file
  existed and never shown it).
- An oversize file is refused with its size named rather than silently crushed.

## [1.8.6] - 2026-11-26

### Fixed

- **The tool and skill lines now appear on a live turn, not only after a reopen.**
  Three gaps between what the daemon sent and what the phone could draw: the
  device's live `done` receipt never carried the counts or the name lists, the
  learned figures were parsed from the totals and never copied onto the receipt,
  and the two name rows defaulted to off. All three fixed; the wire shape is
  asserted end to end in the daemon's own test.
- The learned sub-footer is session-scoped by design: the learning review runs
  after a turn ends, so a live frame cannot know what the conversation just
  taught. It appears on the next read, which is the first moment it is true.

## [1.8.5] - 2026-11-26

### Added

- **A sub-footer saying what the chat taught the skill library.** The per-answer
  rows say what a turn *used*; this says what the conversation *created*, drawn
  small beneath the receipt:

  ```
  🌱 3 skills learned · 1 skill extended
  ```

  Read from the learning ledger, which is the only durable link between a
  conversation and the knowledge it produced - the review runs after the turn
  ends, so a creation lands later than the turn that earned it. `create` and
  `append` are counted apart: one brought a skill into existence, the other added
  to one that already existed.

- It draws nothing on a chat that taught nothing. Twenty ordinary chats should
  not carry a "0 learned" strip for every one that produced knowledge. Its own
  switch sits under Settings > Turn footer, on by default.

## [1.8.4] - 2026-11-26

### Added

- **Optional rows naming the tools and the skills used.** Two switches under
  Settings > Turn footer, both off by default: one draws a line of every tool the
  turn ran with its count, the other a line of every skill it read. They are off
  by default because a busy turn is a long line - 34 tools is not something to
  draw under every reply.

  ```
  🔧 run_command×31 · search_sessions×2 · read_skill
  📚 dsh-image-vision-routing
  ```

- Ordered by count descending then name, so the busiest tool leads and two turns
  that used the same tools read identically. A skill's name is the key from the
  `read_skill` arguments, so the row names the skill as the model asked for it.

### Note

- A skill read whose arguments cannot be parsed still counts as a tool call, so
  the totals stay honest, but it adds no name to the skills row: naming a skill
  the daemon was never told would be a guess.

## [1.8.3] - 2026-11-26

### Fixed

- **The tool count in the footer was never a number.** The client decoded
  `tool_calls` from the receipt and no daemon surface ever sent it, so the switch
  existed and the field never drew. The daemon sends it now.

### Added

- **Skills are counted separately from tools.** A skill is read with the
  `read_skill` tool, so every skill use is also a tool call: the footer draws the
  total and the subset, `12 tools · 2 skills`, which is the shape the Hermes
  footer used. A conversation's own totals carry both, so a chat can say what it
  has done overall rather than only what the last answer did.
- A "Skills used" switch under Settings > Turn footer.
- "Tool calls" now defaults to on. It defaulted to off because it could never
  show anything.

## [1.8.2] - 2026-11-25

### Fixed

- **An attachment opens when you tap it.** Every image the daemon attaches was
  drawn by a view with no gesture on it, so it could be looked at and never
  opened: no viewer, no zoom, no share, no save to Photos. That covers the
  "Where is the headshot" case - the files were on the daemon the whole time and
  the thumbnail was not a button.
- **The round counter belongs to the chat you are looking at.** It was cleared
  only when a turn finished, so starting a new chat - or opening another one -
  while a turn was running carried that turn's `round 44/100` onto a thread that
  had run nothing.
- **Restarting the daemon cannot end a live turn.** The preflight answered from
  readiness alone, so a restart could be scheduled mid-turn; the boot sweep then
  settled that turn as `interrupted` and the reply was lost. It now refuses while
  a job is running, says which one, and the app asks before posting rather than
  after.
- **A healthy long tool no longer reads as a stall.** The fallback poll loop armed
  the stall banner on a timer without checking whether the job was alive, so a
  24-second `run_command` showed "Stream stalled - 10s elapsed". When the server
  confirms the turn is running, the waiting label stays and the banner is not
  armed. That path also no longer spins: confirming the job alive skipped the
  loop's own sleep and fired a probe per iteration.

### Changed

- **Tool activity reads newest-first.** Expanding a 57-tool turn put the call
  running now at the bottom, below every call that had already finished.

### Changed in the daemon

- `GET /v1/gw/restart/preflight` reports `canRestart: false` with a `blocker` while
  a turn is running, instead of reporting only setup readiness.

## [1.8.1] - 2026-11-25

### Fixed

- **The tick marks the provider you chose, not every provider that publishes that
  id.** `deepseek/deepseek-v4.1-flash` is sold by Command Code, by Open Uncensored
  and by native DeepSeek, and the picker lists one row per publisher so the
  account a turn spends is visible. The selection test compared only the model
  name, so choosing one drew a tick beside all three.
- The test now uses the daemon's own answer first. `GET /v1/models` stamps
  `is_default` on the row the pin belongs to, computed from `(provider, model)`
  identity, so at most one row in the catalogue can carry it - a client comparing
  strings could only ever disagree with the daemon that actually routes the turn.
- The same defect in the Settings model picker, from the other direction: it
  matched a bare id before the qualified one, which ticked every row sharing it.
  It now compares the exact value the row writes.
- A label correction: the selected row read "provider default", but the flag
  behind it is the pinned turn model, not the provider's own default id.

### Changed

- **"Set as default" is the first section of the model list**, not the last. On a
  catalogue this size it sat several screens down, so the switch that decides
  whether a pick survives a restart needed a scroll past the whole list.

## [1.8.0] - 2026-11-24

### Changed

- **A delegated harness task gets 30 minutes, not 10.** The default deadline was
  600 s, and the first real delegation - note editing across a 900-line FastAPI
  app - hit it with the work half done. The harness was killed mid-task, its
  report read `timed out`, and the turn finished the job by hand. Thirty minutes
  is still bounded, the run stays visible in the harness monitor the whole time,
  and the ceiling stays at an hour.
- **The tool description names the deadline.** A model that does not know the
  budget writes a task that cannot fit in it and gets a kill instead of a report.
  Saying "killed at N minutes" lets it split the work across calls instead.

### Fixed

- **A truncated harness report keeps its end, not its beginning.** The capture
  capped at 64 KiB and kept the FIRST bytes. For a single command that is right;
  for a harness it is backwards, because a run's conclusion - what broke, whether
  the tests passed, what it decided - is written last. The buffer now rolls,
  keeping the newest bytes and stating how many were dropped. A reader no longer
  gets the tree scan and loses the findings.

### Changed in the daemon

- `DEFAULT_TIMEOUT_SECS` 600 -> 1800. An explicit `routing.harness.timeout_secs`
  row still wins, and the 30 s..3600 s bounds are unchanged.

## [1.7.9] - 2026-11-24

### Fixed

- **Restart Daemon works against the daemon it is actually paired with.** The
  button had two paths and neither addressed Eris: in native mode it asked a
  `cli.exec` socket for `gateway restart`, a Hermes command with no transport
  here, and otherwise it posted to the relay's legacy `gw/restart`, which is
  mounted at the host root and absent under `/v1`. That is the "Relay request
  failed with status 405" the card showed. It now posts `POST /v1/gw/restart`,
  reports the operation the daemon answers with, and treats a dropped connection
  as the expected shape of a scheduled restart rather than a failure.

## [1.7.8] - 2026-11-23

### Added

- The answer footer carries the field set `hermes-signature` carried: the model,
  the provider, tool rounds, tool calls, wall time, tokens in and out, the combined
  count, the cached count and its share, the turn's dollar cost, the session's
  running cost, the conversation's turn counter, and the context reading as a
  share of the window the model declares. Twelve switches under Settings > Turn
  footer, one per field, and the master switch still hides all of them.
- A dollar cost appears only when the daemon has a price for the model. The
  price table is the `assistant.model_prices` settings row, and a model that runs
  on this host is priced at zero by rule. An unpriced model prints nothing rather
  than `free`.
- A session total that does not cover every answer is marked `+`, because a
  partial total presented as a complete one is the one number a footer must not
  get wrong.

### Fixed

- The footer vanished on live turns. The terminal frame carries the receipt and
  the client decoded the text and dropped the numbers, so a footer only appeared
  after the chat was reopened. It draws from the stream now. A transcript refresh
  that lands before the row is written also no longer erases a receipt it does not
  have.
- The context reading was recorded as NULL on every turn that came from the phone:
  the turn loop measured it and `Signal::Done` had no field to carry it. It crosses
  now, so the footer's context figure works on both surfaces.
- Settings > Turn footer no longer looks like it came from another app. It uses
  the same section shell, eyebrow, card, row height and switch as every other
  section instead of stock `Toggle`s in a bare `Section`.
- Signing Claude Code in from the phone sent an incomplete code. The CLI's code is
  `code#state` and the daemon cut the pasted text at `#`, so every attempt was a
  400 the app reported as "Claude refused that code". A callback URL is unwrapped
  into the same pair, and a code the CLI rejects locally now says so instead of
  blaming the server.
- A turn streaming fast no longer recompiles two regexes per fragment, and the live
  token hint no longer rewrites the whole transcript array per fragment. Both were
  on the main actor ahead of the coalescer.

## [1.7.7] - 2026-11-23

### Added

- A running turn says where it is. The daemon already announced its round before
  every tool pass and the client decoded the frame and dropped it, so a long turn
  showed a spinner and nothing else. It reaches the store now and reads as round
  3/100.
- A settled answer carries the daemon's accounting object - rounds, wall time,
  tokens in and out, cache share, context - drawn as one line under the bubble.
  Every field has its own switch under Settings > Turn footer, and a field the
  daemon never measured is left out rather than printed as a zero.
- Build number 243.

### Fixed

- The transcript could not be written back to the local cache. `Message` is
  `Codable` and `TurnAccounting` was decode-only, so re-encoding a row that
  carried accounting failed. It encodes now; the daemon stays the writer of
  record and the app never posts the object back.
- The model picker writes `provider/id` when two catalogues publish the same id,
  which is the only spelling the daemon accepts for an ambiguous id.
- A completion push refreshes the conversation on screen by its own id instead of
  the daemon's notion of which one is current.

## [1.7.6] - 2026-11-23

### Added

- Texts leave the phone for real. A confirmed `sms` or `imessage` is handed to a
  BlueBubbles server on the host and the row records what that server answered,
  so a send either has a guid or the server's own reason. `comms_status` reports
  the transport, the allowlist and the last five sends. `email` and `call` are
  still recorded only and say so.
- An `imessage` skill so a turn knows how to stage, confirm and check a text, and an
  `ios-senses` skill so a turn reads the phone's location, health and activity
  through the sensor tools with the sensitivity level already applied.
- Build number 242.

### Fixed

- The Claude Code sign-in popup could never reach the daemon. The paired base URL
  already ends in `/v1` and every route starts with `/v1`, so the two joined into
  `/v1/v1/...` and every call 404'd. One trailing segment is dropped now, and the
  paste-a-token field opens expanded instead of hiding behind a disclosure.
- A long turn ended in a transport alert. The client's own error type was missing
  from the transient-error check, so a timed-out request set the message instead
  of being retried quietly.
- A turn parked by the app going to the background needed repeated nudges to
  reattach. The single reattach attempt was spent per process; it is now per
  foreground.

## [1.7.5] - 2026-11-22

### Changed

- Signing Claude Code in is two steps in the app instead of a terminal command.
  The daemon opens the tool's own sign-in, the phone shows the link and takes the
  code back, and the daemon stores the credential it receives. A pasted token is
  kept behind an advanced control for a host that cannot run the flow.
- Build number 241.

## [1.7.4] - 2026-11-22

### Added

- A sign-in sheet for Claude Code, reachable from the Claude row whenever the
  credential is not verified. A token saved from the phone is checked before the
  host's environment, credential file or Keychain, because it is the one someone
  deliberately set.

### Changed

- Choosing a model closes the picker. A pick is the end of the question.

### Fixed

- The harness tools read ran on the blocking pool, where a test's fixture was
  invisible, so an empty fixture reported this host's own harnesses.

- Build number 240.

## [1.7.3] - 2026-11-22

### Added

- Each coding harness reports whether it is installed, its version, and, for
  Claude Code, the account it is signed in as. A missing tool offers Install,
  which runs as a bounded job and streams into the monitor; Antigravity is
  reported and installed the same way.
- A harness model the chosen tool does not accept is refused by name, and the
  card offers only that tool's models.

### Fixed

- The daemon searched a service's bare `PATH`, so `dsh` in `~/.local/bin` and a
  Homebrew binary read as missing on a host that had both.

- Build number 239.

## [1.7.2] - 2026-11-22

### Changed

- The routing model fields are pickers over the daemon's own catalogue rather
  than free text, so a model no provider publishes cannot be stored.
- The picker is searchable and grouped by the provider that publishes each id,
  with the provider named under the chosen model. The catalogue carries the
  provider per row, so the grouping is reported rather than guessed.

- Build number 238. Build 237 shipped the same release with a flat picker.

## [1.7.1] - 2026-11-22

### Added

- A harness monitor. Every delegated run is recorded with its status and the
  last of its output, a watchdog marks a run that has been silent for ninety
  seconds as stalled without killing it, and the deadline still ends it. The
  monitor reads it as a popover in the web UI and a sheet in the app, with Kill
  for an administrator.
- A tool call the provider cuts before its JSON closes is retried with a
  correction that says so and asks for smaller parts, twice at most, and the
  failure names the provider's finish reason instead of a bare protocol error.

### Fixed

- The routing card asked for `/v1/v1/routing`, because the paired base is stored
  with and without the `/v1` prefix. The daemon's 404 read as a missing route,
  which left automatic routing untouchable.
- Switching chats abandoned the running turn: navigation was treated as an
  interrupt, cancelling the record and asking a transport that cannot cancel to
  stop a host job that kept running. A switch is a detach now, and returning
  reattaches.

- Build number 236.

## [1.7.0] - 2026-11-21

### Added

- Smart routing. A turn is classified as coding, heavy or general from named
  signals, sent to the model configured for that intent when one is set, and a
  coding turn carries the configured coding harness. The finished message says
  how it was routed.
- Coding harnesses. A `delegate_to_harness` tool hands code work to the tool
  chosen in Settings, with the task as one argument and bounded time and output,
  and returns that tool's own report. The prompt says to use it for code work
  rather than doing the work in parallel.
- A Routing card in Settings: automatic routing, a coding model, a heavy model, a
  general model, and the harness.

- Build number 235.

## [1.6.2] - 2026-11-19

### Added

- The event stream can be resumed. The client keeps the `id:` of the last frame
  it saw and `GET /v1/turns/{id}/events` accepts it as `Last-Event-ID`, which the
  daemon answers by replaying exactly what was missed and then tailing. This is
  the mechanism a mid-turn reattach needs; the chat consumer that uses it is not
  in this build.

### Changed

- Build number 228, version 1.6.2.

### Fixed

- The phone asked to be paired again every few hours. The pre-pairing refresh
  lane read the live refresh token, so it rotated the same token the transport
  was rotating; the loser was answered `token_invalid` and the app read that as
  "pair me again". Both lanes go through the transport that owns the pair now,
  and the daemon answers a token retired within sixty seconds, so a retry whose
  answer was lost in flight cannot end a pairing either.
- The toolbar refresh did nothing while a turn was live: the catch-up refused to
  run while `isStreaming` was true, and a stream frozen by a background trip
  stays true forever.
- Backgrounding a turn dropped its live feed. The stream-update consumer moved
  into one method so the live send and a reattach drive the same code, and a
  turn that was live is reattached on return.
- A turn now survives a force-quit: the outbox already persisted the accepted
  job, so a relaunch asks the daemon whether it is still running and reattaches.
- The subagent count opened the Canvas sheet, which answered "No artifact open".
  It opens the subagents, with rows that expand and follow the poll.

## [1.6.1] - 2026-11-19

### Added

- A watchOS app that ships inside the phone app: the conversation in one glance,
  one tap to talk, the Action Button and Siri through `StartConversationIntent`,
  and a complication on all four accessory families that opens straight into the
  conversation through `kallisti://talk`.
- Answers arrive in the ear clause by clause, so speech starts before the
  paragraph is finished. A tap while the wrist is talking cuts the clause off and
  hands back the mic, and the state is a named one rather than a spinner.
- The wrist never transcribes. It records 16 kHz mono, caps the capture at 45
  seconds, and the host turns it into text, so the clip stays on the machine.

### Changed

- A turn records where it came from, so a watch turn is visible as one instead of
  being guessed from the client's shape.
- The watch pairing is a token pair with one refresh and a `reauth` on the second
  refusal, kept in the shared keychain group.
- The watch test target runs without a host app; the suite had never started
  because it named an executable that does not exist.

### Fixed

- The notification preview stayed one character over its cap when it clipped,
  because the ellipsis was appended after the clamp.
- The watch app called itself KallistiWatch on the home screen, said "Herald" in
  its notifications and its unpaired screen, and wore a placeholder flame. It is
  Kallisti, with the phone app's icon, in build 221.
- Talk sent every answer to the Mimo cloud client and named a Mimo voice in the
  call. The engine is chosen per answer now: the daemon's own `POST /v1/speak`
  first, which uses whatever provider the daemon is configured for, then Mimo
  when its key is the only thing that can speak. Settings -> Voice carries the
  choice.
- The microphone was Mimo-only. It now sends the clip to the daemon's
  `POST /v1/transcribe`, which forwards it to the transcription model the daemon
  is configured with. Settings -> Voice carries a Microphone choice beside the
  speaker: Automatic, This daemon, Mimo cloud.
- A paired phone hands its wrist the daemon origin and token pair over
  WatchConnectivity, so the watch opens on the conversation instead of a
  heartbeat waiting to be told what to do.
- Talk no longer refuses to start without a Mimo key. The key is required only
  when the Mimo engine is selected; the daemon speaks and hears on its own.
- The wrist keeps its pairing in an ungrouped keychain item when its profile
  does not grant the shared access group, which is what a distribution profile
  can do. Before this, every turn answered `no_transport` on a real watch.
- The watch app had no microphone usage description, so the first tap on the
  orb terminated it. It now carries one, and says what it is for.
- Pairing lasted longer. The app read its access token from the live Eris pair
  and its refresh token from the older pre-Eris key, so a refresh rotated one
  family while the app kept presenting the other and the daemon asked for a new
  pairing. Both halves come from the same pair now, and two refreshes arriving
  together are single-flighted instead of racing to retire each other.
- The wrist pairs as its own device, through `POST /v1/watch/pair`. Handing it a
  copy of the phone's tokens made the two clients rotate one family, so the
  phone's next refresh failed and it asked to be paired again.
- The toolbar refresh did nothing while a turn was live. The catch-up returned
  early whenever `isStreaming` was true, and a stream frozen by a background
  trip stays true forever, so the one control a person reaches for was disabled
  by the very condition it exists to resolve. A silent stream is now treated as
  dead after twenty seconds, the stale phases are reconciled, and the transcript
  catch-up runs.
- Talk asked the daemon's Mimo readiness route even when the daemon's own speech
  was selected, so the screen still demanded a Mimo key. That route is only
  consulted when the Mimo engine is chosen.

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

- Media serving is limited to the daemon's configured media roots.
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

- Built on the earlier mobile client foundation under the repository's MIT license history.

## Eris daemon

Versions follow the daemon's own `VERSION`, with a build number that moves on every
ship without changing the version. Entries describe behaviour that a user can
observe; commit history carries the detail.

## 1.11.0

- **Local models accept every tool.** A tool schema that declares a bare `true` or
  `false` where a schema belongs (the evidence field of a `oneOf` branch was one)
  made llama.cpp refuse the whole request with a 400 before it generated a word.
  The chat-completions request now rewrites those positions to `{}` and
  `{"not": {}}`, which mean the same thing, and leaves boolean keywords such as
  `additionalProperties` and `required` alone. The stored tool definitions are not
  changed.

## 1.10.1

- **A turn is never charged for a round it does not run.** The redundancy guard
  counted a round and then ran it, so "two repeats and stop" cost three rounds of
  the same read and put one more identical result in the prompt. The round that
  trips the limit is called off before its calls execute: the stop sentence goes
  in, the calls are dropped, and the next pass finds the tools gone and the
  answer due.
- **A turn is bounded by time, not by running out of rounds mid-job.** Every
  recent answer wrapped up on round 29 of 30 and said so. The loop no longer
  takes tools away two rounds early, and the wall clock is a setting - half an
  hour by default instead of eight minutes. `build 217`.
- **The dsh delegation starts.** The built-in command was `dsh --model M -p task`,
  which the launcher refuses twice over: a profile is mandatory and it takes no
  `-p`. Every delegated coding task died at the spawn. It is
  `dsh --profile headless <task>` now.
- **Stopping the unit is instant.** Shutdown waits on the whole cgroup, and a
  delegated harness with its own children kept it open, so the unit sat in
  final-sigterm until the timeout and the next start raced a half-dead process for
  the ports. Shutdown kills every live harness run's group and every open sign-in
  child before the runtime drains. `build 215`.
- **The Claude sign-in accepts what a person pastes.** The prompt reads the
  terminal in raw mode, where Enter is CR, and the daemon wrote a newline, so the
  CLI echoed it and waited for a key that never came. A pasted callback URL is
  unwrapped to its code, the screen is read until the prompt is up before the code
  is sent, the tool's own rejection comes back as the reason, and a refused code
  says whether there was no prompt or a refused code. Sign-in no longer demands
  administration, and a harness run forwards its stdout to the turn as it arrives,
  so a ten-minute delegate is not a frozen row.
- **Antigravity is an agent CLI, not a chat binary.** `antigravity` takes `-p`,
  which is what the harness wrapper now passes, and one installer runs at a time
  because the vendor's staging path is fixed and two is a checksum failure that
  reads like a corrupt download.

## 1.10.0

- **A turn is routed to the model that suits it.** `routing.rs` classifies a
  message as coding, heavy or general from named signals, picks the model stored
  for that intent or the provider's own, and carries the coding harness on the
  route. `turn.start` announces it, `POST /v1/routing/preview` answers what
  would happen, and `PUT /v1/routing` stores the choices. `build 206`.
- **Code work can be delegated to a coding harness.** `delegate_to_harness` runs
  the configured tool with the task as one argument, never shell-split, bounded
  by a deadline and an output cap, and returns that tool's own report. It is
  offered only when a harness is configured and the sandbox may write. The
  prompt says it is the tool for code work. `build 206`.
- **Harness runs are recorded and watched.** A registry keeps status, tails and
  the turn for each run, a watchdog marks ninety seconds of silence as stalled
  and enforces the deadline, and four routes read it with one SSE stream for
  changes. `build 207`.
- **The daemon knows which coding tools exist.** Detection reports installed,
  version, path, whether it can be installed here, and, for Claude Code, the
  credential's source and account without surfacing the token. A missing tool can
  be installed as a bounded run. A harness model the chosen tool does not accept
  is refused by name. `build 212`.
- **A coding harness can be signed in from the phone.** A token pasted in the app
  is stored and checked before the host's environment; the tool's own sign-in can
  also be opened and finished with the code it shows. `build 214`.
- **A second speech engine, off by default.** `speech.engine` selects the
  original audio endpoint or a chat-audio engine that synthesizes through the
  audio modality and transcribes through the input-audio one. `build 203`.
- **The speech model is settable and named.** `GET /v1/speech` reports the engine,
  the endpoint and model rows, and the model the engine will actually use;
  `PUT /v1/speech/chat` stores or clears them. `build 204`.
- **The event stream can be resumed.** `GET /v1/turns/{id}/events` honours
  `Last-Event-ID`, replaying exactly what was missed and then tailing. `build 202`.
- **Retired refresh tokens answer for a minute.** A token retired within the
  grace window still verifies, so a second lane or a lost answer cannot log a
  paired device out. `build 205`.
- **A truncated tool call retries instead of failing the turn.** A stream that
  ended mid-arguments failed the round with a protocol error, and the only way
  forward was the phone's Retry, which replayed the same truncated turn. One
  bounded retry of the round now happens first.
- **A call with no provider id gets one.** A Gemini-shaped stream names a call by
  its function name and carries no id; replaying that transcript into a dialect
  that requires `tool_use.id` failed the whole next round with `Field required`.
  The assembler mints `call_<index>` when none arrived, and a provider-issued id
  is kept untouched. `build 201`.
- **A turn knows where it came from.** `jobs` gained `source` (migration 0044),
  `turn.start` carries it, and the set is closed to `phone`, `watch`, `browser`
  and `cli`, with anything else landing on `phone`. A watch turn was previously
  indistinguishable from a phone turn once it reached the daemon. `build 198`.
- **The daemon speaks as it writes.** With `?audio=1` the event stream carries a
  `speak.clause` frame per clause, each with one WAV, emitted as the answer is
  produced rather than after `done`. Without the parameter the stream is
  unchanged. Speech resolves its engine from `tts.base_url` first, so a keyless
  local engine no longer needs a chat provider to exist.
- **A watch can pair and read state.** `POST /v1/watch/pair` mints a token pair
  for a wrist and reports the origin it can actually dial; `GET /v1/watch/state`
  reports whether a turn is active and how much is unread. The readiness frame
  gained `transport` so a phone knows which credential it is holding.
- **Transcription is a route.** `POST /v1/transcribe` accepts a 16 kHz mono clip
  and answers text, which is how the watch hears without a speech framework.

## 1.9.3

- **Subagents can use tools.** A `dispatch_agent` child was a single tool-less
  inference: useful for drafting text, useless for "go fix this." It now gets
  the same tool list its parent has, eight rounds to use it, and the parent's
  workspace reach. The recursion guard is structural: `dispatch_agent` is never
  offered to a child, and a child that invents the call is refused by the tool
  layer. The child deadline moves from 2 minutes to 10 so real work can finish.
- **Tool output reaches the phone's Live tab.** The daemon has always streamed
  `tool_start`, `tool` and `tool_output` frames; the app decoded only the step
  label, so the Live sheet showed a tool name and a timer with no stdout. Those
  frames now become the tool row's start, preview and live terminal output.
- **A finished turn waits for its children.** When the model stops while
  subagents it dispatched are still queued or running, the daemon holds the
  turn, polls them, and then hands their settled results back to the model in
  the same turn so it can finish the task. The wait is bounded by the child
  count (60 seconds each, 10 minutes maximum); children still working at the
  deadline are named in the continuation. The phone's subagent poll keeps the
  stall watchdog alive while the stream is quiet. The subagents route now
  also reports `checkin_secs` for the parked parent, so the phone can show the
  countdown. `build 197`.

## 1.9.2

- **Phone chats get a real name, not their first sentence.** The title role
  defaults to `primary`, and that used to mean no title call at all, so a chat
  started from the app kept the truncated first question forever while the
  browser's got a model-written name. A role left on primary now generates the
  title with the turn's own provider. A title is requested only while the stored
  one is still a placeholder or a daemon truncation, so a named conversation is
  not renamed on every turn and is never renamed after the operator renames it.
  `build 194`.
- **A long job is not interrupted for lack of rounds when it has no `/goal`.**
  The larger budget was gated on both a stored goal and a long-job message, so
  "fix all of it" without `/goal` got the ordinary budget and ended in a status
  report. The long-job signal alone now raises the budget to the operator's
  ceiling, and the turn carries one instruction: finish and verify the work;
  do not hand back a plan or a "still open" list unless the budget is spent or a
  decision is genuinely needed; check an old note against the live system before
  repeating it. `build 194`.

## 1.9.1

- **An Antigravity turn no longer dies on its own tool schemas.** Google's
  function-declaration `Schema` is a proto with one `type` string per node and a
  fixed field list. An MCP server's `inputSchema` is full JSON Schema, so a
  nullable property's `type: ["string", "null"]` arrived as a repeated field
  (`Proto field is not repeating, cannot start list`) and a reusable definition's
  `$defs` was an unknown name. The schema is now reduced to what the proto can
  hold: local `$ref`s are resolved and inlined, `$defs`/`definitions` are dropped,
  a type union becomes its one non-null type plus `nullable: true`, `anyOf`/`oneOf`
  take their first non-null branch, `allOf` branches merge, and property names are
  preserved verbatim. `required`, `properties`, `enum` and the numeric bounds are
  untouched. `build 192`.

- **The Antigravity client version is current.** The surface began answering
  `antigravity/ide/2.1.1` with "This version of Antigravity is no longer
  supported", after the schema error above was cleared. The header now names the
  published IDE release, 2.7.0. `build 193`.

- **A turn can name its model, and its conversation.** `POST /v1/turns` accepts
  `model` and `title`. The model is checked against that provider's own probe and
  recorded on the conversation, so the note sync can run enrichment on a model of
  its own without moving the daemon default or the chat picker; a model no probe
  published is refused by name rather than answered on the default in silence. The
  title names a background turn's conversation from the caller's own words instead
  of a header in its instruction block. A pending model pin from the phone's picker
  is cleared when a turn names a model explicitly, because the pin belongs to
  whichever chat was open, not to a turn that already said what it wants. A
  titled background turn clears it too, so a note sync cannot spend the model
  the operator picked for the chat they are in.

## 1.9.0

- **The phone can see the files a turn made, and send its own.** A turn's attachments
  are stated on every message the device API returns - id, name, size, media type -
  and `GET /v1/attachments/{id}` serves the bytes with the paired bearer, so a phone
  never needs a dashboard session for a file its own daemon holds. An upload posts the
  raw bytes with `x-kallisti-filename` and `x-kallisti-conversation` and is answered
  with an id a turn may name. A turn that carries files no longer needs words: the
  operator who drops two screenshots and presses send has said everything with the
  picture, and the old `text_required` refusal threw the uploaded rows away for a
  caption nobody wrote. `build 189`.

- **Every turn lands on a transcript.** `POST /v1/turns` wrote the question into
  the conversation and then created the job with `None` for the conversation, so every
  device turn in this database carried a null `jobs.conversation_id` even when the
  phone had named one - nothing related a turn to its chat. The job records it now. A
  turn that names no conversation at all used to be a "stream and store nothing" mode,
  which from the phone is indistinguishable from a broken bind: the reply appears and
  then exists nowhere. The daemon opens a conversation for it instead. `build 191`.

- **An update is a newer build, not any leftover.** `/v1/gw/update/check` answered
  `update_available: true, can_install: true` and named `1.8.0 build 187` on a host
  running `1.9.0 build 188`: two stale `eris.new-*` files from earlier deploys were
  parsed and offered purely because they existed, so the phone's Install button would
  have installed a downgrade. A staged build now has to be newer than the running one
  to count, and the card names only a newer staged build. `build 190`.

- **A transcript can be read backwards.** `GET /v1/conversations/{id}` reads from the
  newest end instead of the oldest, and an `x-before` cursor names the oldest message
  the caller already holds. The cursor is the row id, not the timestamp, because two
  messages in the same second are common and a timestamp cursor skips or repeats
  them. The reply carries `has_more`, so a client can stop asking at the true
  beginning. `build 189`.

- **Conversations that never received a message are pruned.** Every client that
  creates a chat before sending, and every launch that could not restore the previous
  one, left an empty conversation behind; this daemon had 336 conversations, ten of
  them empty, and a session list that was mostly "New chat". Creation now removes
  rows with no messages older than an hour, and touches nothing else. `build 189`.

- **A turn with no conversation is logged.** A turn without a conversation streams but
  is never stored, which is a legitimate mode and also exactly what a client with a
  broken bind does by accident. The daemon now says which happened, with the device
  id, so a report of "the reply never appears" can be answered from evidence.

## 1.7.1

- **A turn can hand work to a named specialist.** A roster row is a name, a role and
  a standing brief, and `dispatch_agent` gains an `agent` field: the child turn
  carries that brief instead of the operator re-explaining it, and the prompt lists
  the names it may dispatch to. A read-only row is offered no writing tools and runs
  in a read-only sandbox, so a write is refused twice rather than once. A row that
  names a scope directory is canonicalised and prefix-checked at dispatch, and a
  scope that resolves outside the workspace refuses the dispatch instead of confining
  the child somewhere new. The roster has its own page, its own settings card, a
  read-only `GET /v1/roster` on the phone surface, and `eris roster --json`.
  `build 186`.

- **One POST starts one durable turn.** A trigger is a saved prompt with a minted
  token; `POST /hooks/<token>` answers 202 and starts the turn through the same path
  a schedule uses, in a conversation named after the trigger. The token is shown once,
  when it is minted, and stored only as a SHA-256 hash plus a twelve-character prefix,
  so a leaked database is a leak of verifiers. Unknown, revoked and disabled tokens
  all answer one indistinguishable 404, the comparison is constant-time at the last
  step, the body is capped at 8 KiB with the ceiling named, and a fire over the hourly
  ceiling answers 429 and leaves a `refused` row. The route is on the admin listener
  only and is POST only. Saving an existing trigger rotates its token in the same
  write, because "rotate" and "save" were always the same edit. `build 186`.

- **A turn can read its own budget before it spends it.** A `budget` tool answers the
  window's turn count, prompt and completion tokens, the stored soft cap, what is left,
  and the window's p95 turn duration, and it says plainly when the cap is reached:
  stop and report rather than continue. It reads through the same function the settings
  panel and the phone footer read, so the figure a turn acts on is the figure an
  operator can check. The cap is the operator's, `quota.soft_cap_tokens`, validated
  between 1,000 and 10,000,000 tokens, and nothing is cut off at it: the tool reports,
  it does not refuse. `eris budget --json` prints the same report. `build 186`.

## 1.6.0

- **A turn can hand its remainder to the next turn.** `queue_task` writes a
  durable `jobs` row of kind `task` with the prompt, the conversation and how deep
  the chain already is; the worker that just finished drains it as its own turn in
  the same conversation, after its own transcript row is settled. The prompt now
  states the tool budget before round one, so a job that needs two turns is planned
  as two instead of discovered at the wall. `build 185`.

- **A capped turn continues itself, once.** When a turn ends because the tool
  budget ran out and it queued nothing, the daemon starts one continuation with a
  prompt that quotes the last answer and forbids redoing finished work. Bounded at
  two automatic continuations and six queued turns per chain, so a job that keeps
  capping stops on a number instead of on an operator noticing. `build 185`.

- **The turn lease is renewed while the turn works.** `deploy.sh` reads
  `lease_expires_at` to decide whether a restart would kill someone's turn; the
  claim stamped sixty seconds and nothing moved it, so a ten-minute turn looked
  idle and a deploy killed it ("the daemon stopped before this turn finished").
  The worker now renews every twenty seconds on the frames it is already
  streaming. `build 185`.

- **Phone turns record what the budget did, and how long it took.** The terminal
  frame carries the cap, the round count and the wall time, and the settled row
  stores all three, so a capped turn is visible in the transcript instead of
  arriving with the flag unset, and a chat's p95 is a measurement rather than
  "not measured". `build 185`.

- **`/v1/metrics` answers for one conversation.** `GET /v1/metrics?conversation=`
  narrows the same rows, and `window=all` reads a chat over its whole life instead
  of the last day. The phone's chat footer asks for the chat in front of the
  operator; the daemon-wide panel keeps its window. `build 185`.

## 1.5.0

- **The update check answers the legacy envelope again, beside the flat keys.**
  The phone this build ships with reads `data.components[...]` with the legacy
  component key and its nine keys; the dashboard, the card and the newer client
  read the flat set. One document now carries both, so neither client has to be
  re-shipped for the other's sake and a caller that only knows one shape still
  gets an answer.
  `updateAvailable` is true when a build is staged **or** a newer release is
  published, and a staged filename that does not parse is not an offer. `build 183`.

- **`POST /v1/gw/update/apply` installs a staged binary.** A build placed beside
  the running one as `eris.new-<version>-<build>` is now the preferred install: the
  daemon writes a helper script into its own state directory and starts it as a
  transient unit, and the script backs up, installs, restarts, and restores the
  backup if anything fails. The published-release path - download, checksum, swap -
  stays as the fallback for a host with nothing staged. Both answer the same
  `{ok, data:{state, alreadyRunning, pid, output}}` shape the phone decodes, and
  both go through one entry point so the card and the phone cannot disagree.
  `build 183`.

- **The Software card installs from the dashboard.** The System tab's card is
  titled Software, states the running identity from the daemon's own version line,
  lists what is staged by name, and carries one button - **Install staged update** -
  that is disabled with a plain sentence when nothing is staged. It posts to its own
  route, records the write as `settings.update` in the audit trail, and redirects
  with a notice on the card that produced it. The check and the release manifest
  stay on separate buttons, so pressing Install can never be mistaken for pressing
  Check. `build 183`.

- **Update progress answers both key spellings.** The phone decodes `exitCode`,
  `startedAt` and `finishedAt`; this daemon's earlier row decodes `exit_code`,
  `started_at` and `finished_at`. Both keys now carry the same value, the output is
  capped at 200 lines of 500 bytes each, and the route reads the staged helper's
  `progress.json` first with `state.json` as the fallback. A missing document is
  still `200 idle`, never a `404`. `build 183`.

## 1.5.0

- **The update button does something.** `GET /v1/gw/update/check` now asks the
  release server what is newest, caches the answer for six hours, and reports the
  version, build, publish date, notes, download URL and checksum for the asset
  this host can actually install. Before this the check could only list binaries
  staged beside the running one, so on a clean host "update available" was
  unreachable by construction. A host with no route out answers from the cache and
  says so instead of showing a failure. `build 182`.

- **An install is requested, supervised and reported, never performed by the
  daemon.** `POST /v1/gw/update/apply` writes a request and starts a separate
  helper unit; `GET /v1/gw/update/progress` reports the helper's state and its last
  200 lines and never answers 404, because a client polls it in a loop and a 404
  reads as a dead socket. The daemon runs with its home directory read-only and
  could not replace the binary it started from even if it tried, which is the
  correct trust boundary and is not widened here. `build 182`.

- **A release swap that fails puts the old binary back.** The helper verifies the
  published checksum before it touches anything, refuses a release with no
  checksum at all, backs up the running binary, and restores it if the new one does
  not answer `/healthz` within thirty seconds. A failed update that leaves the
  daemon down would be worse than no update button. `build 182`.

- **The System tab has an Update card.** It states what is installed, what is
  newest, the release notes, a Check now button and an Install button that is
  disabled with the reason when this host cannot perform the install - a missing
  helper unit, a daemon started outside systemd, an install already running, or a
  release with no checksum. It sits directly above the Daemon card because the two
  answer the same question. `build 182`.

- **A pasted API key finishes onboarding step 2.** The step accepted `env:NAME` and
  nothing else, refusing a key value by name, so an operator holding a fresh vendor
  key could not get past it: the file a value would be sealed into, `eris.env`, was
  only writable from the provider registry form. A pasted key is now sealed into
  `eris.env` at mode 0600, put into the environment overlay so the probe that
  follows resolves it immediately, and the settings row keeps
  `env:KALLISTI_PRIMARY_API_KEY` and never the value. A malformed `env:` or
  `token:` reference is still refused as a reference. `build 182`.

- **`install.sh` writes the update helper and its unit.** The daemon cannot write
  `~/.local/opt`, so the swap belongs to a second unit with no sandbox of its own.
  Without it the Update card is honest but useless, so the installer writes the
  helper from the release archive or the checkout and the unit that runs it, and a
  re-run upgrades both. `build 182`.

## 1.4.15

- **The iOS skill ships with the daemon.** A fresh install used to start with an
  empty skill library, so the one procedure this phone needs existed only on the
  operator's own host. `skills/kallisti-ios/SKILL.md` is compiled into the daemon
  and written into the owned library on first run, and `install.sh` seeds the same
  file for a source checkout. The skill is `kallisti-ios` v3.1.0, it carries no
  app name, and the old skill key is gone. `build 181`.

- **A wrapped turn answers in short bullets instead of a wall of prose.** The
  three closing instructions (the wrap-up, the spent notice and the last-resort
  ask) ordered "one plain paragraph of prose, with no headings and no section
  list". That is the wall of text the operator kept rejecting: on every long
  tool turn the closing round is the only answer he reads, and the instruction
  overrode the persona's bullet rule at exactly that moment. They now ask for at
  most six short lines, one fact per line, and keep the bans on headings, next
  steps and permission asks. `build 180`.

- **The last round before the answer still has tools.** The lookahead used to take
  the tools away two rounds early, so a turn that read for thirteen rounds could
  describe the job it never did, which is how "provide me a pdf plan" came back as
  "no PDF, the budget ran out". The first reserved round is now the last action
  round and keeps its tools, with an instruction to produce the deliverable; only
  the final round is tool-free and writes the answer. A provider fault during the
  answer phase no longer fails a turn that already did work: it falls through to
  the forced-answer round and the recorded summary. `build 179`.

- **A wrapped turn reports the work instead of handing it back.** The three
  closing instructions a turn gets when its tool budget runs out asked for "the
  exact next step so the work can be picked up from this message", and the
  fallback notice told the operator to say "Continue". That is the defect in
  miniature: the turn ends with a plan for the next one, so the job is never
  finished in the turn that had the budget. All three notices now ask for one
  paragraph of what was done and what it proves, and the transcript, not the
  notice, is what a later turn resumes from. `build 178`.

- **One retry now also covers a 429 or a 5xx.** The single retry covered only a
  connection the peer dropped before the answer started. A rate limit or a
  gateway fault failed the whole turn even though nothing of the answer had been
  sent, discarding every tool call the turn had already run. The same one retry
  now also re-sends once after a 429 or a 5xx, 750ms later, and a retry that
  fails is still reported with the provider's `Retry-After`. `build 178`.

- **A skill can be rewritten and deleted.** The library was read-only: the daemon
  listed a skill and served its body, the dashboard had switches and a directory, and
  the phone could only browse. So a procedure that had gone wrong could not be fixed,
  and one that should not exist could not be taken out. `PUT /v1/skills/{key}` now
  replaces the whole `SKILL.md` through a temporary file in the skill's own directory
  and a rename, and refuses a key that is not a slug, a blank body, a body over 64 KiB,
  and a symlinked directory or file, since a rename through a link writes the link's
  target. `DELETE /v1/skills/{key}` deletes that one skill for good: its own directory
  and its file, with the library and every other skill untouched, and it drops the
  ledger row for a learned skill, which is what stops the curator and the learning
  loop writing it back. An archive that could be undone was shipped in `build 175` and
  replaced in `build 176`; there is no undo now, which is what a delete means. Both
  verbs are `admin`-only on the device API, and `/dashboard/skills` is the same two
  calls behind the session cookie with a per-skill editor, a per-skill delete that
  asks twice, and no JavaScript. `GET /v1/skills` now says whether each skill is the
  operator's or learned, whether it is active, stale or archived, whether it is
  pinned, and whether it can be rewritten from here. `build 176`.

- **A question no longer gets a long job's budget.** A turn "carried a goal" unless it
  was small talk, and a goal turn was raised to sixty rounds, so the operator's
  `tool_rounds` setting acted as a floor rather than a ceiling: four of nine turns in
  one evening ended at round 59, 80 to 103 tool calls, fifteen to twenty-five minutes
  each. The raise now needs the message to name a long job, and it is bounded by a new
  `tools.goal_rounds_ceiling` setting, editable from the dashboard and the phone.
  An ordinary question keeps `tool_rounds`. `build 175`.

- **A model id with a slash in it is not a provider name.** CommandCode serves
  `deepseek/deepseek-v4.1-flash`, where the head is the publisher; this daemon also
  has a provider called `deepseek`, pointing at api.deepseek.com. A conversation
  that recorded that id was read as `provider/model` and sent to DeepSeek's own API,
  which answered `400: the supported API model names are deepseek-flash,
  deepseek-v4-pro`. The catalogues now decide: a head is a provider only when that
  provider publishes the tail, and otherwise the whole string is one model on
  whichever provider publishes it. Both lanes read a recorded model through that
  rule. `build 173`.

- **Two chats, two models.** A chat kept the model it started on only until
  another chat picked one. The phone's picker writes the daemon default, because
  that is the only row it has ever written, and every turn read only that row: so
  starting a second chat on another provider moved the first chat's next turn onto
  that provider, and the transcript showed a conversation answering on a model
  nobody chose for it. A turn now resolves its model from the conversation it is
  writing into, the picker's write is taken once by the next turn as a pin, and the
  conversation reports the model it is on. `build 171`.

- **A conversation's model survives a reload.** The browser composer already
  restored what a conversation last ran on; a send that left the picker alone still
  fell back to the global default, so a page reopened after another tab changed the
  default quoted the wrong model. The fallback is the conversation's own record
  now. `build 171`.

- **A deleted chat stays deleted.** The phone's delete called `DELETE
  /v1/sessions/{id}`, which this daemon only answered with `GET` for a device
  session, so the client dropped its own row and then failed the sync: "Removed
  locally - sync failed". `/v1/sessions/{id}` and `/v1/conversations/{id}` both
  delete a conversation and its transcript now, with an audit row, and a second
  press is a `404 conversation_not_found` rather than a silent success. `build 170`.

- **A greeting is answered in words.** The goal gate stopped the objective riding a
  hello, and the next greeting still came back as four tool calls, a container list,
  a services check and a warehouse data-quality sweep on the model's own initiative.
  Every turn now carries one more instruction: answer the message at the size it
  asks for, and a greeting gets words rather than a briefing. `build 170`.

- **A greeting is not a work order.** A goal used to ride every turn, and a message
  with no task in it left the objective as the only actionable instruction in the
  prompt: "Sup" came back as a warehouse sweep with sixty rounds of budget behind
  it. A turn whose text is nothing but a greeting or an acknowledgement no longer
  carries the objective and keeps the operator's own round budget. Anything with a
  word of work in it still carries the goal, which is the direction that costs
  nothing to get wrong. `build 169`.

- **The objective has a page, not just a command.** It was reachable only by typing
  `/goal` at a phone, which is how a goal came to be set for nine hours and
  forgotten. Settings now has an Objective card on the Assistant tab that shows what
  a turn would carry, saves it, and clears it, and the write is audited by length
  rather than by text. `build 169`.

- **`/goal` works in the browser.** `/help` has listed it since build 149 while the
  browser answered that the daemon does not implement it. It now sets, reports and
  clears the same row the phone writes. The difference is what happens next: a
  command in the browser is answered before a turn exists, so the objective is
  stored and the next message carries it, rather than starting the work
  immediately the way `/goal` on the phone does. `build 169`.

- **The browser lane carries the goal at all.** Until now only the device lane read
  it, so a goal set from the dashboard would have done nothing. Both lanes read it
  through one module, so the instruction text, the round budget and the greeting
  gate cannot drift apart. `build 169`.

- **The daemon learns from a finished turn.** After an answer settles, a background
  worker queues the exchange, reviews it once with a tool-less model call, and
  writes a class-level procedure into `~/.config/kallisti/skills` when the exchange
  taught something durable. It creates and appends, and only to skills the loop
  wrote itself: a skill the operator wrote is never edited, and a pinned one is
  never touched. A review that finds nothing settles as `skipped` with its reason
  rather than writing a hollow skill. `build 168`.

- **Reads, pins and age are a ledger, and the library has a page.** Every
  `read_skill` bumps a row; the new Learning page pins, unpins, restores an
  archived skill, runs a maintenance pass, and lists the last reviews. A weekly
  pass ages learned skills active -> stale -> archived, with `.archive` recovery,
  and the page states the windows. The loop is on by default and switching it off
  changes nothing on disk. `build 168`.

- **The phone carries the skill catalog too.** A device turn now gets the same
  names-and-descriptions catalog and the same `read_skill` tool as the dashboard,
  so a procedure learned from one surface is reachable from the other. `build 168`.

- **The free-read switch is settable.** `POST /v1/settings` now accepts
  `notes.enrichment.free_read` (`true`/`false` and the usual spellings), reports it in
  `changed`, and refuses anything else by name. Before this the notes worker honoured a
  key that nothing could write: the only way to change it was SQL, which is not a
  setting. `build 164`.

- **Only research mode reads on its own.** An enrichment in auto, follow-up, reminder,
  summary, story or sketch mode runs from the note alone; a jot that mentions a person
  must not quietly pull their history out of the vault and paste it into an enrichment
  nobody asked to be researched. Research reads because that is its job. Operators who
  want every mode to read set `notes.enrichment.free_read` to true. A refused read says
  which switch would allow it. `build 163`.

- **Enrichment may read.** The worker runs with `read_file` and `list_dir` allowed and
  everything else refused - no write, no command, no fetch, because those are actions
  and this worker has no approval surface to park one on. The sandbox root is the
  daemon's own workspace, so a note about a meeting can be enriched from what the vault
  and the brain already know, and rounds rise from one to six so it can look something
  up and still answer. `build 162`.

- **Note enrichment reads the ink when there is any.** The worker used to work from the
  recognised text alone, and on-device Vision returns confident nonsense for cursive,
  so the only honest answer was "too garbled". It now looks for the newest stored
  drawing revision, hands that image to the vision role, and enriches from the
  transcription when one comes back. No blob, an unreadable file or a provider that
  cannot see all fall back to the recognised text exactly as before. `build 159`.

- **Push registration stopped being refused twelve times a minute.** The app sends
  `bundleID`; the legacy request the daemon decodes calls it `bundleId`, so the field
  arrived empty, the route answered `400 pushEnvironment and bundleId are required`,
  and the client retried. Every field now accepts its snake_case and its other-caps
  spelling. Found only after the refusal log started naming its route. `build 157`.

- **The daemon logs when a phone attaches to a turn stream.** One line per attach, with
  the job and the resume point. A stream that attaches and dies is a transport problem;
  one that never attaches, or is refused, is the client's. That distinction has been
  guessed at three times now and this makes it readable. `build 155`.

- **A refresh rotates, and hands back the replacement.** Both refresh spellings share
  one token store, and only the legacy one rotated: a refresh there retired the token
  the newer lane held, so the phone could not refresh again until it was paired.
  `/v1/token/refresh` now rotates too, returns `refresh_token` in the response, and the
  retired token is refused. The app already prefers a refresh token from the response
  over the one it sent, and cannot present a superseded one: its refresh is
  single-flight and reads storage at call time. `build 154`.

- **Reverted: the legacy refresh rotates again.** Build 152 stopped `/v1/auth/refresh`
  from rotating so the two refresh spellings could not retire each other's token. The
  legacy client did not accept the same token coming back, treated the response as a
  failure, and revoked the device: an iPad pairing died three minutes after that build
  went in, and the app went back to "no host connected". Rotation is restored. The
  retired-token problem is real and is the next thing to solve, but not this way.
  `build 153`.

- **A dropped connection costs a reconnect, not a re-pair.** The daemon serves two
  refresh spellings from one token store: `/v1/token/refresh` verified and minted an
  access token, while `/v1/auth/refresh` rotated - retiring the presented refresh
  token and minting a new pair. Whenever the older lane refreshed, it silently killed
  the token the newer lane was holding, and that lane could never recover without
  pairing again. Both routes now verify and mint; neither rotates. `build 152`.

- **A goal keeps your words and sends the instruction only to the model.** Build 150
  rewrote the turn text, so the transcript showed machine instructions in the
  operator's mouth: the row read "Goal set: ... Start on it now" instead of the
  `/goal ...` they typed. The row now keeps what was typed and the objective rides
  only on the prompt. `build 151`.

- **`/goal <objective>` starts working instead of filing a note.** It stores the
  objective and then runs the turn that begins it, with the objective in the prompt
  and the sixty-round budget a long job needs. A goal that waits for the operator to
  type again is a note with extra steps, which is what it was. `build 150`.

- **`/goal` exists.** `/goal <objective>` stores the objective, `/goal status` reports
  it and `/goal stop` clears it. Every turn from then on carries the objective in its
  system prompt and is told to keep working until it is done or out of rounds, then
  say what is left, with the round budget raised to sixty so a real job can finish.
  The loop is the objective plus the budget: no background process, nothing spending
  tokens that the operator cannot see. `/restart` stays absent on purpose.
  `build 149`.

- **`GET /v1/slash-commands` publishes the command table** so a client cannot drift
  from what the daemon implements: every command with the line `/help` prints for
  it. Not `/v1/commands`, which the legacy control surface already owns - two
  handlers on one path panic at router build time, which is how the collision was
  found. `build 148`.

- **`/new`, `/clear` and `/model` answer on the phone too.** The device lane used to
  refuse four of the six commands because it resolves no conversation before a
  command is answered. `/new` now starts a conversation and names its id, `/clear`
  takes that id and clears that transcript, and `/model` reports the pin instead of
  being silent about it. Acting on the pin still belongs to `POST /v1/settings`,
  behind the admin scope. `build 146`.

- **A slow model no longer looks like a page that stopped.** The chat surface says
  which round is running and then went quiet until text arrived, so a two-minute
  think on a large context read as a finished page that later dumped a wall of text.
  The status line now ticks with elapsed seconds and stops the moment a delta or a
  tool receipt lands. `build 145`.

- **The chat page says how much of the conversation came from the provider's
  cache, and how much of the turn that just ran did.** The chip beside the model
  reads `cache N% session · M% turn`: the conversation total from every answer's
  usage, and the newest answer on its own, which is the figure that matters when a
  long chat gets expensive. `build 144`.
- **\_Old entry:** A chip beside the model and the context window shows the cached share of
  the prompt summed over the conversation, from the usage every answer already
  records. `build 143`.

- **A turn from the phone carries the conversation it is continuing.** The device
  lane assembled every turn as a single user message: no transcript, no window, no
  summary. The browser lane has always sent the settled exchange; the phone never
  did, so a model in a long chat answered the current message with nothing behind it
  and read as if it had moved to a new session and got slow. The turn now starts from
  the same 24-message window the browser uses, and appends the prompt only when it
  says something the question row does not (a described image, or a turn with no row
  at all). `build 142`.

- **Gemini thinking models can call tools again.** A 2.5 model returns a
  `thoughtSignature` beside every `functionCall` and refuses the next round with
  `400 Function call is missing a thought_signature in functionCall parts` if the
  replay omits it. The adapter rebuilt calls from name and arguments only, so any
  antigravity turn died on its second tool call. The signature is now kept from the
  stream, carried on the call, and echoed back beside it. `build 141`.

- **A phone that reconnects can attach to a running turn again.** A request with no
  `Last-Event-ID` was treated as a resume from zero, so once a long turn pruned its
  early frames the daemon answered `409 buffer_miss` to every reconnect. A client
  with no resume support can never name a position that is still retained, so the
  stream went quiet and the app fell back to polling: the turn ran, the phone showed
  nothing. A connection that names no position now watches from the current tail of
  a live turn, and still replays a finished one in full. An explicit stale resume
  point is still a `409`, and the snapshot route is still the recovery path.
  `build 140`.

- **A file the turn attaches shows up while it streams, not after a reload.** The
  transcript draws attachment chips from the rows it loads, and a bubble that
  arrived over the stream loads none: `attach_file` wrote the row, the answer came
  back, and the file was invisible until the page was reloaded. The turn stream now
  carries an `attachment` frame the moment the file lands - `id`, `name`, `size`,
  `media_type` - journalled like every other frame so a reconnect replays it.
  `/v1/version` advertises eleven frame kinds now. The dashboard draws the chip
  inline as a download, and the phone appends it to the streaming message (build
  114). `build 139`.

- **A file the model made can ride on the reply it made it in.** There was no tool
  for this, so a turn that produced a PDF could only write "attached: report.pdf" in
  prose - a claim the daemon had no way to make true, and a transcript with no
  attachment on it. `attach_file` names a workspace file, copies it into the
  conversation's uploads, records the attachment row and binds it to the answer row
  the turn is writing, so both surfaces render it with the code they already had for
  the operator's own files. The rules are the upload path's: the source has to
  resolve inside the workspace, and one reply carries no more files or bytes than one
  message may. Offered whenever a turn has a reply row, on either lane. `build 138`.

- **A whole tool call is recovered even when the next serialisation is cut short.**
  The recovery scan parsed every candidate object from its opening brace to the end
  of the buffer, so a complete call followed by the beginning of a second one read
  `trailing characters at line 1 column 553` and was refused - the 597-byte
  `clarify` failure, which killed the turn before the question card could appear.
  Each candidate is now parsed to its own closing brace, newest first, so the whole
  call is recovered and a cut-short neighbour is ignored. `build 137`.

- **The phone can read the subagents it dispatched.** `GET /v1/subagents` and
  `GET /v1/subagents/{id}`, behind the `sessions` scope. Before this the phone could
  hand work off and then had no way to read it: the tool's own answer ("dispatched,
  id ...") was the last the operator heard unless he was looking at the dashboard.
  The list is the dashboard bar's view - anything still working plus anything that
  finished recently, newest first, bounded at forty with a `truncated` flag - so the
  two surfaces cannot disagree about what is live. The single-row read is the
  durable half: a child that finished while the app was closed is fetched by the id
  the tool returned, and a row the bar has aged out is not lost. Field names are the
  daemon's own, so `status` is `queued`, `running`, `succeeded`, `failed` or
  `cancelled`. `build 136`.

- **The phone can hand work to a subagent.** The device turn hardcoded
  `agents: false`, so `dispatch_agent` was never offered and a child never ran -
  the comment said the surface had no bar to report one to and no answer row to
  count one against, and the second half has not been true since a phone turn
  started writing a question and an answer row. Dispatch is now gated exactly as
  the browser gates it: the operator's switch, the Full sandbox, and an answer row
  to count children against. The desk is built in the turn's own worker, so a
  child runs with the parent's model, system prompt, workspace and resolved key.
  End to end over the real listener: a phone turn that calls the tool writes the
  row, the child answers, and the row settles with its text. `build 135`.

- **Only an object can be a tool call.** A fragment that parsed as a JSON array
  counted as a whole re-serialised call, so an array - a JSON body inside a
  `curl`, say - could replace the real arguments. The turn then ended with
  "tool-call arguments must be a JSON object", holding the JSON it meant to send.
  Arguments are an object in both dialects, so completeness now means an object
  and nothing else, and a value that is not an object is recovered from the
  buffer when one is still in there. When nothing object-shaped survives, the
  refusal names what arrived (`an array`, `a string`, `a number`) instead of
  leaving the shape to be guessed. `build 134`.

- **A whole tool call is not thrown away by the fragment that follows it.** The
  argument assembler kept one buffer, and when that buffer already held a complete
  JSON object it replaced the buffer with the next fragment. Replacement is only
  right when that fragment begins a value, which is what a restarted
  serialisation does; anything else is a continuation of the call that is already
  whole. On 2026-10-28 a `run_command` hit exactly this: sixteen calls ran, the
  buffer held the whole object, the next fragment was seven bytes from the middle
  of the command, and the turn died with `expected value at line 1 column 1`
  holding nothing but those seven bytes. The accumulator now remembers the last
  whole value it held and recovers it when the buffer no longer parses, and it
  forgets that value the moment a new serialisation starts, so a restart that is
  cut short still refuses instead of running a stale spelling. `build 133`.

- **The picture is described by the turn, not by the request that queues it.**
  Build 131 moved the vision call off the async worker and onto a blocking thread,
  which fixed the panic and introduced a worse failure: the handler now waited for
  a provider call before answering, that call takes about seven seconds, and the
  phone's own request timed out first. The question and answer rows were written,
  no job was created, and the app read `transport failed: the request timed out`
  while the daemon had nothing to run. The request now writes the rows and queues
  the turn immediately, with the operator's words and the file list, and the
  worker describes the picture on its own blocking thread before it calls the
  model. The `202` is fast again and the description still happens. `build 132`.

- **The picture guard that refused the one thread it was meant to allow.** Build
  130 added a `Handle::try_current` check in front of the two blocking roles, on
  the theory that it would catch a caller on an async worker. It does not: a
  closure handed to `spawn_blocking` still sees the runtime's handle, so the
  check refused the description on exactly the thread the fix had moved it to.
  Vision and web-extract went silently unavailable on every turn that ran in a
  blocking task. The check is gone; `spawn_blocking` is the contract, and the
  property it depends on - a blocking thread can build a runtime of its own - is
  now a test rather than a comment. `build 131`.

- **A picture from the phone no longer kills the turn that carried it.** Every
  turn sent from the app with an image attached died before it was queued: the
  handler asked the pinned vision role to describe the picture on the async
  worker, and that role builds a runtime of its own, so the request panicked with
  "cannot start a runtime from within a runtime". The phone read `502: no
  message`, the dashboard said "this turn stopped without finishing", and the
  question row sat in the transcript with no job behind it. The description now
  runs on a blocking thread, the way the browser's lane already did it, and both
  blocking roles refuse with a logged warning instead of panicking if a future
  caller makes the same mistake - a turn without a description is worth more than
  a request that dies. `build 130`.

- **The argument assembler keeps the spaces it was given, and says which failure
  it hit.** Two faults from one turn on the phone. First, a fragment that was
  pure whitespace was dropped unless it came after a finished call - but this
  provider streams tokens, so indentation, blank lines and the space between two
  words arrive as fragments of their own. A 4 KB command lost 474 bytes of
  whitespace on the way in, which is why a written `getmail.py` came out at one
  space of indent with `defaults to993` in its docstring and would not compile;
  the model then spent its turn rewriting it. A whitespace-only fragment is now
  content unless the buffer is already a whole value. Second, a gateway that
  materialises `arguments` as an object instead of a string had that frame
  silently ignored, so the opening brace never reached the buffer and every later
  fragment was a continuation of nothing - the `expected value at line 1 column
  1` refusal, on calls the model had written correctly. The object is accepted as
  the whole call, the same way the Anthropic block path already treated a
  materialised `input`, and a refusal now says when the opening brace never
  arrived. A turn that fails before it streams anything also writes the failure
  into its transcript row, so the dashboard shows why instead of an empty bubble.
  `build 129`.

- **A file can ride with a turn from the phone.** The device listener had no
  attachment path at all, so the app dropped every picture and log it was handed
  and said so in a comment; the browser's composer had carried files for months.
  Three routes and one claim later, both lanes are the same feature: `POST
  /v1/attachments` takes the raw bytes with a filename header and the
  conversation they belong to and answers with an id, `POST /v1/turns` names
  those ids in an `attachments` field and the turn claims them for the question
  row it writes, and `GET /v1/attachments/{id}` serves the bytes back for
  history. The rules are the dashboard's own, not a second set: the row decides
  what may be read, the path has to resolve inside `workspace/uploads`, the read
  is bounded again on the way out, and the type comes from the bytes rather than
  the filename. What the model gets is a prompt that names each file and, for an
  image, a description from the pinned vision role - because the model serving
  the turn may be one that cannot see. The transcript row keeps the operator's
  words alone. `build 128`.

## 1.4.14

- **A question the model asks now survives the stream that carries it.** Every
  `clarify` call from the phone died with `TOOL CALL 'CLARIFY' ARGUMENTS NEVER
  COMPLETED INTO VALID JSON`, reported at 535 bytes as `EXPECTED VALUE AT LINE 1
  COLUMN 62`, so no question card was ever drawn and the turn failed before it could wait for
  an answer - on the phone, on the web, anywhere the tool was offered. The cause
  was the argument assembler, not the model: a fragment whose bytes were already a
  prefix of the buffer was read as "the sender repeated itself" and thrown away.
  A token-level stream sends a nested object's opener as its own fragment, so `{"`
  arriving inside `"questions": [` was dropped because the buffer also begins with
  `{"`, and the buffer was left reading `[{"question"...`. Replay detection is now
  positional and nothing else: a fragment that begins with everything buffered
  contributes only its suffix, an exact re-send contributes nothing at all, and a
  fragment is never judged by its content. Whitespace after a finished call no
  longer replaces it either. A real truncation is still refused with the same
  message, and the captured 9router stream that failed every question now
  assembles into the card it was meant to draw. `build 127`.

## 1.4.13

- **A key can be pasted, and it is stored where a key belongs.** The provider form
  only took a reference, so an operator holding a fresh key had to know to write it
  into `eris.env` first and get the variable name right; pasting the key itself was
  refused with "needs a key reference, and never the key itself". The field now
  takes either. A pasted key is written to `eris.env` as `<ID>_API_KEY=...` at mode
  0600 (the file systemd already loads at start), the running process is handed the
  same name and value so the next turn resolves without a restart, and the registry
  stores `env:<ID>_API_KEY` and nothing else. The value is never returned, never
  logged and never written to a settings row, so no page can echo it back - the
  provider list shows the reference. A reference is still kept exactly as written,
  a malformed one (`env:lower_case`) is still refused by the field that is wrong,
  and a value the environment file cannot carry (quotes, whitespace, a leading `#`)
  is refused with the instruction to use `env:NAME` instead. `build 126`.

## 1.4.12

- **A Google login stops expiring an hour after it is made.** `oauth.auto_refresh`
  shipped off, so an Antigravity grant that reached its five-minute refresh lead
  was refused with "inside its refresh lead" and the only remedy was to log in
  again by hand - which on this daemon happened roughly hourly. Refresh is on by
  default now. What the old default protected is still protected one layer lower:
  a grant copied out of another program (`TokenOrigin::Migrated`) is refused by
  the refresher before any network call, so the flag can never spend a refresh
  token this daemon does not own. Write `[oauth] auto_refresh = false` to get the
  old behaviour back. `build 125`.
- **A model the phone picked is a model the daemon accepts.** `GET /v1/models`
  publishes each row as `{id, provider}`, so a picker sends back the bare `id` it
  drew - and `POST /v1/models` split a bare id against the primary, which refused
  `gemini-3-flash` on a daemon whose primary is `commandcode` even though
  `antigravity` published it all along. A bare id now resolves to the catalogue
  that actually publishes it, the pin is stored `provider/model` so every later
  turn is served by that provider, and a refusal no longer pastes a 93-model
  catalogue into a phone's error banner. `build 125`.
- **A tool call survives a gateway that restarts its serialisation.** The
  assembler replaced the buffer when a fragment was a whole object, but a
  re-serialisation that arrives in pieces (one finished snapshot, then the first
  half of the new spelling) was appended onto the finished object and died as
  `expected value at line 1 column 14`. A buffer that is already valid JSON can
  never be extended into valid JSON, so the newest fragment replaces it. `build
  125`.

## 1.4.11

- **A tool-heavy conversation opens instead of timing out.** The device
  transcript endpoint bounded how many messages it returned and not how big they
  were, and one conversation on this daemon held 41 messages and 9.2 MB of JSON
  because every shell command's whole stdout is a stored tool record. It now also
  bounds the bytes: the newest messages are kept inside a 512 KB read, each tool
  output is cut to an 8 KB preview that says how much it left out, and the answer
  reports both `omitted` and `elided_outputs` so a surface can say what it is not
  showing. `build 119`.
- **The phone can see a command running, and what it is printing.** The device
  turn stream dropped three frames the browser has had since it grew a run log:
  the round that is starting, the call that is about to run, and the bytes that
  call writes while it writes them. It carries `round`, `tool_start` and
  `tool_output` now, so a running turn reads as work in progress rather than as
  the word "streaming", and a two-minute command is not two minutes of silence.
  Output chunks are published live and never journalled: a reconnect reads the
  finished `tool` frame, which already carries everything the turn stored.
  `build 119`.

- **The full-host switch is reachable from a paired phone.**
  `POST /v1/settings/full-access` exists on the device API behind the `admin`
  scope, with the dashboard's own three rules: enabling needs the word `full`
  typed, disabling needs nothing, and turning it off demotes a `full` sandbox to
  the default so the stored tuple cannot name a mode the switch that allowed it
  no longer permits. Before this, `sandbox=full` was a value a phone could pick
  from the settings record and never set. `GET /v1/settings` reports
  `full_access`. `build 121`.
- **The phone can see where the daemon is reachable.** `GET /v1/settings` now
  carries `listen` (the addresses the device listener binds) and `relay` (the
  origin the pairing QR carries), read exactly the way the dashboard reads them
  including the shipped default. They are reported rather than writable: a bind
  cannot be moved under a live phone, and a phone that could rewrite its own
  daemon's bind is one typo away from losing the daemon it is talking to.
  `build 122`.
- **Every probed provider is in the model list.** `GET /v1/models` reported the
  primary provider's catalogue alone, which was right while the daemon had one
  provider and became a lie once the registry could hold six: a phone's picker
  showed one provider's models and called the other five absent. Each provider
  whose catalogue a probe published now contributes its own rows, attributed to
  itself, and a provider nobody probed contributes nothing - the same distinction
  the `probed` flag draws for the whole answer. `build 123`.

- **A `clarify` call survives a sender that re-serialises it.** Three turns here
  died with `tool call clarify arguments never completed into valid JSON`, twice
  failing at byte 14 of a 692-byte buffer - a parse error at the START of an
  argument buffer, which a truncation cannot produce. The assembler appended
  fragments unless one extended what it already held, so a gateway that re-emits
  the whole object with a field reordered (or an escape spelled differently) glued
  two objects together. A fragment that is a complete JSON value on its own is now
  taken as a whole object and replaces the buffer, and a buffer that still holds
  concatenated objects is parsed from its newest complete one instead of failing
  the turn. A genuine truncation has no complete object to recover and is refused
  with the same message it always had. `build 124`.

## 1.4.10

- **A turn that is nearly out of budget answers instead of giving up.** The
  engine used to run tools until the last one was gone and then force an answer
  out of a model that had been cut off mid-thought. It now sees the end coming
  two rounds early, stops offering tools at that point so no call is spent that
  cannot finish, and asks for what it has found so far. The footer says
  `wrapped up` when that happened, and still says `capped` when the answer
  genuinely had to be forced. `build 111`.
- **A question the turn is waiting on takes the reply's place.** The clarify
  card used to sit above the run log, where a long turn buried it. It now
  stands where the answer is going to be, the answer's own bubble waits out of
  sight behind it, and settling the question brings the answer back in the same
  spot. A question that was already answered stays in the run log as a record
  rather than a second card. `build 111`.
- **A reply that is still being written does not show an empty box.** The
  bubble does not exist until there is a word to put in it; the caret lives on
  the status line, which says `thinking` from the first frame. Both the live
  path and a reloaded page behave the same way. `build 111`.
- **The run log says what it means.** `Turn 3` is which answer this is in the
  conversation, and `7 turns` is how much work it did. The word `round` is gone
  from every visible string; the database column is still called `rounds` and
  nothing had to be migrated. `build 111`.
- **A long answer flows in the thread instead of scrolling in its own box, and
  can be opened full.** The answer bubble used to stop growing at seventy
  percent of the window and scroll from there, which put a second scrollbar
  inside a page that already scrolls and made a long answer hard to read. The
  bubble now has no height ceiling: the thread carries the scroll, the way it
  does for every other block. A collapsed answer folds to `--fold`, the one
  token the thinking bubble's body also reads, so the two blocks are the same
  size. The expand control in the bubble's top right corner, and Escape, still
  open the reply in the whole window; a wide table or a long terminal line still
  scrolls sideways inside the answer rather than dragging the chat page with it.
  `build 111`.
- **The settings page has six tabs.** Models, Assistant, Memory, Devices,
  Automation and System, each its own set of forms, and every tab is a link:
  the page still works with no script at all, and a bookmark to one tab lands
  on that tab. A save returns you to the tab that holds the card you saved, and
  a refused value reopens the tab that holds the field it refused, so the
  message is never on a page you are not looking at. `build 111`.
- **A provider is added from a form.** The registry is a table you can read,
  with add, edit and delete, and a refusal names the box to fix. The primary
  cannot be deleted, because that would silently move which endpoint every turn
  is sent to; the refusal says so and says what to do instead. The JSON editor
  is still there, folded away, for the times a form is not enough. `build 111`.
- **The clock is twelve hours or twenty-four.** One setting, and every stamp on
  every surface follows it: the transcript, the conversation list and the
  sessions page. `build 111`.
- **A reply is announced to every device, or to the ones you pick.** The scope
  is a setting with three answers: the device that asked, every paired device,
  or a list you choose. There is a master switch above it, and off means
  nothing is sent whatever the scope says. A device that has been unpaired
  stops being announced to, even if its registration never expired, and a scope
  that reaches nobody says so rather than going quiet. `build 111`.
- **The conversation list can be sorted.** Recently used, newest first, by
  title, or grouped by day. The control sits next to the word Conversations at
  the top of the list rather than in the settings page, because that is where
  you are looking when you want it changed. `build 111`.
- **The menus are rows, the way iOS draws one.** The sort control and a
  conversation's project control were a select box and a separate Move button,
  which is two affordances for one choice. Both are now a single panel of rows:
  a check against the option in force, related rows grouped, the destructive row
  last and below a rule, title case, no ellipsis, and the press is the change.
  It is still a `details` element, so every one of them works with no script.
  `build 111`.
- **The phone and the browser render settings from one record.** The daemon
  serves a capability record naming every setting it accepts, which section it
  belongs to, what shape it is and what values it allows, so neither client
  keeps its own copy of the list. The device settings route also gained the
  clock format and the push scope alongside the seven fields it already had.
  `build 111`.
- **A tool call survives a gateway that repeats itself.** The argument buffer
  appended every fragment the provider sent, on the documented assumption that a
  fragment is only the new bytes. A gateway in front of 9router re-sent the whole
  `arguments` object on a later delta instead, which made the buffer two objects
  in a row and failed the turn at the parser with `trailing characters` or
  `EOF while parsing a list` before any tool ran. The buffer now compares each
  fragment with what it already holds, the same way the reasoning text does: a
  fragment that begins with the buffer contributes only its new suffix, a
  fragment already inside it contributes nothing, and a real truncation is still
  refused. `build 112`.
- **A reply opened in the whole window is above the page, and its way out stays
  in reach.** The window was `position: fixed` inside the transcript, and the
  transcript is a stacking context, so `fixed` bought the viewport but not the
  top layer: the composer printed over the bottom of the answer, and below 900
  points the rail drawer and its scrim covered the whole thing. The answer could
  not be read and the control that puts it back was behind the page. The
  transcript rises while a window is open, and the window's own row of controls
  now sticks to its top edge, so `Restore` is reachable however far down the
  answer you have read. `build 113`.
- **A conversation scrolls inside itself, not with the page.** The shell stated
  a height, but a page is allowed to grow past it: one long turn made the
  document taller than the window, and from then on the composer was not on the
  bottom edge, it was somewhere in the middle of the document with the answer
  running behind it. Scrolling to the end of a reply moved the composer up over
  the words instead of moving the words. The page that holds a conversation no
  longer scrolls at all; only the transcript does. The transcript also refuses to
  travel sideways, which it was silently allowed to do because stating
  `overflow-y` makes the other axis scrollable too, and a run log line that
  filled every fact about a turn could be wide enough to push the left edge of
  the conversation out of view. That line wraps now. `build 114`.
- **The conversation is the window, so there is nothing left to pan.** Locking
  the body was not enough: iOS Safari still pans the document, and a height on a
  static box is only as good as the ancestor chain that has to keep it. When the
  chain resolved to `auto` the transcript stopped being a scroller, the whole
  conversation became one long document, and the start of a reply could not be
  reached by scrolling because nothing inside the conversation scrolled. The
  conversation is now a fixed box the size of the visual viewport, the root is
  locked as well as the body, and the transcript is the only thing that moves.
  `build 115`.
- **A collapsed answer is 320 pixels tall, not the sliver it was rendered as.**
  A message carried `min-height: 0`, which is how a bounded scroller inside it
  gets its ceilings to bite, and is also how a grid item loses the floor its own
  content should give it. The transcript could then hand a message less height
  than its parts needed, and the flex column inside shrank what it could: the
  reasoning body held its 320px cap and the answer below it, also capped at 320,
  came out 45px tall with two thousand pixels of text clipped behind the fade.
  There was no reply to scroll to, because nothing was laying one out. Measured
  on a real 98-turn transcript the message wanted 893 and was given 432. A
  message is now as tall as what it holds, and the ceilings inside it are the
  ones stated on the scrollers themselves. `build 116`.
- **A folded answer can be read to its last line.** The fold was clipped shut:
  the fade at its bottom edge said there was more, and the only way to reach it
  was the whole-window view. A collapsed answer now scrolls inside its own 320px
  ceiling and chains out to the transcript when it reaches its end rather than
  trapping the gesture. The fade tracks the scroll position instead of the state,
  so it stops claiming there is more once the box is at its end, and a fold short
  enough to fit never wears it at all. `build 117`.
- **The transcript stacks its messages, and a message is as tall as what it
  holds.** The column was a one-track grid. That is a good way to say "never
  wider than the column" and a bad way to say "stack these": the grid gave the
  engine a height to decide per message, the message's floor came from its own
  content, and the two facts made a cycle. Measured on a real conversation,
  Chrome settled on 8px rows with every message on top of the next and Safari
  left the answer inside squeezed back to a sliver, which is the mess of
  overlapping cards and cut-off replies that survived three builds. The
  transcript is a flex column now, a message cannot shrink, and tall replies,
  tables and terminal output still scroll inside their own boxes. `build 118`.

## 1.4.9

- **A Claude account is connected from the Settings page.** Connect starts
  Anthropic's own browser login and the page hands back the link instead of
  opening it, because the daemon may well be headless and opening a browser is
  a thing a person does on the machine in front of them. Approve the login, then
  paste the address the browser lands on and press Verify: that works from a
  phone as easily as from a laptop, and a browser on the daemon's own host
  finishes by itself because the callback listener catches the redirect. The
  grant is sealed into the encrypted store, and the row states the account, when
  the grant stops working, and whether it was logged in here or copied from
  another program. `build 110`.
- **A Google Antigravity account is connected the same way, and finds its own
  cloud project.** Antigravity bills a turn to a cloud project, so a login that
  sealed a grant without one had a token this daemon could not spend. The login
  now asks the account for its project and every request carries it. An account
  that has never had a project is told so by name rather than meeting an opaque
  refusal mid-turn, and `[google] project` in `oauth-clients.toml` is the manual
  answer. Nothing provisions a project on the operator's behalf. `build 110`.
- **A connected account becomes a provider in one press.** A login is not a
  provider until it is in the registry: the model pickers, the auxiliary roles
  and the reply bubble all read catalogues keyed by a registry id, so a
  connected account that was never added served nothing. The press adds the
  entry with the account's own `token:` reference and reads its model catalogue
  at the same time, so a provider that answers with no models says so at the
  press rather than in the first turn. Reconnecting can discover a different
  cloud project and the entry follows it. `build 110`.
- **The daemon talks to Anthropic and Antigravity in their own shapes.** A
  registry entry can now declare that its endpoint is the vendor's own API
  rather than a gateway in front of one. That adds `anthropic-version` and the
  beta set a browser login's grant needs on the Messages dialect, and the
  Antigravity `User-Agent` on its own, and the Antigravity body is Gemini's
  `contents`/`parts` with `functionCall` and `functionResponse` blocks instead
  of `tool_calls`. A gateway carrying the same model still gets a bearer token
  and no vendor headers, because a header it does not expect is a header that
  can be refused. `build 110`.
- **A stored grant is a credential a turn can use.** Until now a `token:`
  reference was storable and not resolveable: twenty-one call sites read an
  environment variable and nothing else, so a provider that authenticates with
  a login could not serve a turn however well it was sealed. One function now
  answers both kinds of reference and every call site uses it, including the
  greeting, the auxiliary roles, the probe and the phone's turns. Refreshing
  stays off: a grant past its refresh lead is reported as needing a reconnect
  rather than having its refresh token spent. `build 110`.
- **A refresh can no longer take the daemon down.** The refresh transport built
  a runtime inside code that already runs inside one, and that combination
  panics rather than fails. It now runs its one exchange on a thread that owns
  its own runtime and maps a panic to a typed transport error, so a transport
  fault reaches the operator as a failure instead of unwinding the process. A
  test calls it from inside a runtime against a loopback mock and proves it. No
  token was refreshed to write that test, and auto-refresh is still off.
  `build 110`.
- **A reply can take the whole window.** The fold control answers "how much of
  this answer do I want in the column", and the operator kept asking a different
  question: show me this one thing and nothing else. `Maximize` sits beside the
  fold cycle in the reply's own header and puts that reply in the viewport - the
  whole width, the whole height, scrolling inside itself, above the rail and the
  composer. It is a toggle that reads `Restore` when it is open, not a fourth
  state in the cycle, and Escape closes it. `dvh` means a phone's URL bar does
  not cut off the last line. The reply is still in the document behind the window,
  so find in page still finds it. `build 109`.
- **A refused send gives the message back.** The words were the operator's and
  the daemon threw them away: an oversized attachment was refused after the send,
  the refusal was printed into a reply bubble that claimed a turn had happened,
  and the typed message stayed on a page that was never written to the database.
  A reload showed it had never existed. A refused send now removes the reply
  bubble and the optimistic user line, puts the text back in the composer with
  the cursor in it, and states the daemon's sentence in the status line. The
  attachment queue is untouched, because it is only emptied once the daemon has
  accepted the body. `build 109`.
- **A refused send writes nothing.** The reader wrote each file as its part
  arrived and refused the part that broke a cap at the end, so the files before
  it were already on disk and already in `chat_attachments` with no message to
  belong to. Twenty such rows existed on this host. Every part is now held until
  the whole body has passed, and only then written; a refusal leaves no file, no
  row and no audit line. The refusal names the file and the size a person reads
  rather than a byte count. Unclaimed rows an earlier build left are swept by age
  at start-up, files included. `build 109`.
- **The composer refuses what the daemon would refuse.** The single-file and
  whole-message caps existed only on the server, so the browser could add a file
  the send would reject. The caps ride on the shell as data, from the same
  constants the reader enforces, and a file over either is refused when it is
  picked, by name and by size, while the operator still has the picker open.
  `build 109`.
- **A bubble is sized by the column, with no floor under it.** The width rule was
  `clamp(22rem, 78vw, 52rem)`, and on a 393 CSS px phone `22rem` is 352px, which
  beat the `78vw` it was meant to sit between: every bubble was 90% of the screen
  and the phone breakpoint's own width restated a rule that could not take
  effect. The floor is gone, the readable measure is a ceiling, and the phone
  block releases the desktop ceilings as well as setting its own share. No
  `clamp` remains in the transcript rules. `build 109`.
- **A minimized reply quotes the answer, not the code block in it.** The
  one-line preview skipped fence markers but not what was inside them, so a reply
  that opened with a command showed the shell command as its summary. The whole
  fence is skipped now. `build 109`.
- **Four tests that were red are green, and the version assertions cannot rot.**
  `1.4.8` was hardcoded in three test files while `Cargo.toml` said `1.4.9`, so
  the suite that pins the wire format failed on a released version. The status
  body is built from the version the crate reports, and the remaining assertions
  compare against the manifest rather than a second copy of it. The Settings test
  matched the bare fragment `sk-` anywhere in the page, which the inlined
  stylesheet's `mask-image` satisfies; it now looks for a rendered value. `build
  109`.

- **A reply has three sizes, and the transcript remembers which.** Every settled
  answer can sit at full height, fold to a clipped preview with a fade, or
  shrink to a single line of its own opening words beside the name. One control
  in the reply's own header cycles the three, and its word is the press rather
  than the current state, so there is one thing to read instead of three
  buttons that can disagree. The choice survives a reload and a live stream
  renders the same way, because the size is an attribute on the reply and the
  stylesheet does the clipping: a folded answer is still in the page, so find
  in page finds it and a browser with no script still reads it. The default is
  Expanded, which is what every earlier build did, and it is set in Settings
  under the reasoning card. Three things are never folded automatically: a
  reply still arriving, a reply that failed, and a reply holding a question you
  have to answer. `build 108`.
- **A provider token is sealed on disk, and it says when it expires.** The two
  accounts already on this host were moved out of the settings row into one
  encrypted file each that the daemon owns, with the provider's own id bound
  into the ciphertext so a copied file cannot authenticate as a different
  account, and the old row was read and never written. A token now carries its
  expiry, a token that cannot say when it expires is not treated as fresh, and
  the refresh decision is one gate instead of a rule inside each provider.
  Refreshing is off by default and nothing was refreshed in this build. The
  providers still read their key the way they did before; pointing them at the
  store is the next ship. `build 108`.
- **Seven conversations were renamed.** Their titles were a chain of thought a
  model wrote before the naming path learned to scrub one, so the transcript
  showed a paragraph of reasoning where a name belongs. The rows are rewritten
  to the conversation's own first question, which is what the daemon wrote
  before a model was asked for a better name. The scrubber had no test for the
  shape actually stored, a truncated thought ending in an ellipsis, and now it
  does. `build 108`.
- **A greeting is spoken in the provider's own dialect.** The welcome turn was
  built without the profile's dialect, so a provider that speaks a different
  wire format answered it wrong. `build 108`.

## 1.4.8

- **A finished reply reaches the phone that asked for it.** Delivery existed and
  nothing called it: a turn settled, the transcript was written, and the phone
  learned about the answer on its next launch or not at all. A turn that ends
  `succeeded` now announces itself through APNs to every active registration of
  the device that owns it, with `HERALD_MESSAGE_READY` so the phone settles a
  streaming bubble that died and offers Reply from the banner, and with
  `conversationId` and `jobId` so a tap lands on the turn. Only a device turn is
  announced, only a successful one, and a push that does not land is a log line
  rather than a failed turn: the reply is already written. `build 107`.
- **APNs is a real sender, and it says which key is wrong.** The relay's key
  `LH5GM8356P` has never been accepted by Apple: it answers
  `403 InvalidProviderToken` under every topic, and so do `32NT26772F` and
  `UQWH2GWTLU`. `M375486F3F`, team `58U7UPFS53`, is the only one on this host
  that signs a token Apple accepts, and it is the team the app is signed under.
  An ES256 provider token is minted with `ring`, cached for forty-five minutes,
  and sent over HTTP/2 with the topic per registration. `build 107`.
- **A misconfigured host says so instead of claiming there is no feature.** All
  APNs variables unset still answers `503 apns_not_configured`, which is honest.
  Half-set, an unreadable key, or an environment that is not `development` or
  `production` is reported at start-up and carried on the service, so the send
  route answers `apns_misconfigured` with the reason. That distinction is the
  reason ten phones spent a week reporting `Registered` and delivering nothing.
  `build 107`.
- **The environment a token belongs to is learned, not assumed.** The app's
  entitlements say `production` and it reports `development` anyway, because it
  guesses from an App Store receipt a device-installed build does not have. A send
  that Apple rejects for an unknown token is retried once against the other host,
  and when that delivers, the proved environment is written back to `push.db` so
  every later send goes straight there. `build 107`.
- **One phone, one registration.** Every reinstall minted a new device id and
  recorded the same APNs token again, so one phone held ten active rows. A
  registration now retires the same token under any older device id, and a token
  Apple has genuinely retired is marked inactive instead of being retried on every
  send. `build 107`.
- **Compression stops retrying a summary model that is down.** A failed summary
  was indistinguishable from a conversation that needed none, so every turn paid
  for two failed calls and wrote a "no summary" audit row. Failures now count,
  with an exponential cooldown from a minute to fifteen, a thrash breaker, and an
  audit row that names the reason or says `held: cooling_down`. `build 107`.

## 1.4.7

- **The thought is not the answer, and it is not the title.** MiniMax inlines its
  chain of thought into `delta.content` unless the endpoint is asked to split it,
  and the naming role was neither asking nor expecting it: conversations were
  renamed `<think>We need answer only title max six words...`, and on several turns
  the rename wrote zero rows because a suggestion that was a tag matched none of
  the accepted candidates. A provider profile can now declare `reasoning_split`, so
  the adapter asks for the split where the deployment supports it rather than
  everyone being sent a field some endpoints refuse. Independently of that, a
  suggested title is scrubbed of think blocks before it is considered - an
  unclosed tag swallows the rest of the text, because half a thought is not a
  name - and a suggestion that is empty afterwards is discarded so the truncation
  of the question stands instead of a title made of markup. `build 106`.
- **A title is named after the question, not the answer.** The naming prompt
  carried six hundred characters of the answer, so a long reply was named after
  whatever it happened to end on. The question now carries the subject and the
  answer is trimmed to a quarter of that and labelled as context only. `build 106`.
- **A reply that was forced says it was forced.** The tool budget was spent, the
  loop wrote a final answer, and the footer showed `100 rounds` under a reply that
  looked exactly like a finished one. The cap was set in memory and sent once as a
  live notice, then written nowhere, so a reload forgot it and the notice was
  cleared a second after it appeared. A settled reply now records whether the
  budget ended it (`chat_messages.capped`, schema 22) and the page says so beside
  the round count, both live and after a reload. A reply that finished on its own,
  and one nobody measured, both stay unset: not capped and not measured are the
  same state, and neither is a confident zero. `build 106`.
- **The run log counts replies, and the session line counts questions.** The pill
  under a reply read `turn 6` while the footer read `100 rounds` and the session
  line read `7 turns`, three counters sharing two words. The pill is the reply's
  ordinal and now says `reply`; the session line counts the operator's own messages
  and now says `questions`. `build 106`.
- **A conversation compresses itself when the window fills.** The only rule was
  "keep the newest twenty-four messages", which is a count and not a size: short
  exchanges left a large window almost empty, and long ones with command output in
  them overflowed a small one and came back as a context-length failure. A turn now
  sizes its prompt against the context length its model declares, at a configurable
  share of it (`context.compress_at`, default 80), and the rows that fall outside
  are summarised by the configured compression model rather than dropped. The
  summary is updated with the new turns instead of being re-read from scratch, and
  it is persisted against the conversation (`chat_conversations.summary`, schema
  21), so a summary already covering the tail is not paid for again. A summary that
  covers rows the transcript no longer has is cleared. `build 106`.
- **What the operator asked survives compression word for word.** A summary
  paraphrases, and paraphrasing "use the existing helper" is how an agent
  confidently writes a new one six turns later. The operator's own messages from
  the compacted region ride into the summary verbatim, newest first, under a
  heading that says they are instructions already given. `build 106`.
- **A turn that outgrows its window mid-flight trims output, not history.** The
  check at the start of a turn cannot see the twelve tool results that turn is
  about to accumulate. Before each round trip the prompt is measured again and, if
  it no longer fits, oversized tool results are cut to their first and last
  characters with a line saying how much was removed. The operator's messages are
  never cut, and the newest result is cut last because that is what the next round
  is about. The cut is announced rather than silent, so a truncated result is not
  mistaken for a wrong one. No auxiliary call is made mid-turn: a summary request
  in the middle of a turn the operator is waiting on is a second silence they did
  not ask for. `build 106`.
- **The agent can ask a question and wait for the answer.** Asking was a prompt
  instruction to end the turn with one question, and the answer arrived as the next
  message. There is now a `clarify` tool: the model offers up to four labelled
  choices or a free-text field, the turn parks on a condition variable, and it
  resumes with the answer as the tool result when the operator replies. The wait is
  sliced so Stop is still noticed while a question is on screen, times out after an
  hour, and a timeout tells the model to proceed on its own judgement rather than
  ask again. Answering an empty question by name is refused, and so is an answer
  when nothing is waiting. `build 106`.
- **The phone can answer a question too.** A device turn refused `clarify` outright,
  because `/v1` had no route that resumes a parked turn: the only thing a phone
  could do with a question was wait out the hour. A device turn now offers the tool,
  the question goes out as an `asked` frame on the job stream and is written to the
  job log, `GET /v1/turns/{id}/asked` reads back a question that was missed, and
  `POST /v1/turns/{id}/answer` settles it so the parked thread resumes with the
  answer as the tool result. Both routes land on the same registry the dashboard
  uses, so either surface can answer a question the other one asked, and a question
  nobody owns is reported as missing rather than confirmed to a stranger. A cancel
  now reaches a parked turn as well, so a question nobody answers ends with the
  turn instead of outliving it. `build 106`.
- **A reply with no thinking bubble uses the width it has.** A reply was capped at
  the same column whether or not a thought bubble sat inside it, so a quiet answer
  left a third of the window empty. A reply with no visible thinking is now wider;
  one that shows its reasoning keeps the narrower column. `build 106`.

## 1.4.6

- **A conversation keeps the model it was on when you come back to it.** The rail
  restored nothing, so resuming a session rendered the global default and the next
  turn could land on a model the conversation had never run on - a fresh prompt
  cache asked to re-read a history it had not seen, which is both slower and the
  usual way a reply drifts. A conversation now records the model it runs on
  (`chat_conversations.model`, schema 20) when the first turn is written, and the
  composer restores it ahead of the configured default. A conversation with nothing
  recorded - one that predates the column, or one that never sent a turn - still
  opens on the default, which is what every conversation did before. `build 105`.
- **A new conversation opens on the configured default, not on whatever sorts
  first.** The stored pin is an unqualified model id, while the picker's options are
  `provider/model`, so the selected option never matched, no option carried
  `selected`, and the browser fell back to the first entry in the list. The default
  is now resolved with `chat::model_value`, the same qualified value the settings
  page writes, and a pin the catalogue does not list is inserted as its own option
  rather than dropped. `build 105`.
- **The phone and the browser reach the same endpoint for one pin.** A device turn
  read the three legacy provider rows and only stripped the provider prefix off the
  model, while the browser resolved the registry entry the primary names. On an
  install whose primary is not the legacy endpoint that is one stored value talking
  to two endpoints, which is a 404 from one surface and a 400 from the other. Device
  turns now resolve through the registry by the same rule, and the context-window
  meter reports the window of the provider actually serving the turn. `build 105`.
- **A generated title lands on every turn, and on the phone.** The rename only
  accepted the placeholder or the truncation of the *current* question, so from the
  second turn on a suggested title matched neither, zero rows were written, and the
  name the model produced was thrown away silently. The first question's truncation
  is now a third candidate, so a later turn can replace a name the daemon wrote
  itself while a name the operator or a previous model chose is still left alone.
  Device turns now ask the naming role too, so a conversation started from the phone
  stops being called by its first six words. `build 105`.
- **A reply says what it is doing while it is doing it.** The line under a live
  reply was a control hint, a round readout and a tool receipt fighting over one
  span, and none of them answered "what is happening now" - the daemon could not
  even say it, because the only tool frame was emitted *after* the call returned
  while its output was already streaming. A turn now emits a start frame carrying a
  phrase in the operator's own words - "Writing a file notes/todo.md", "Fetching a
  page", "Reading a skill pdf", "Running a command" - and the bubble shows it until
  the call finishes. An unfamiliar tool falls back to its own name rather than an
  invented label, and the line is behind the same switch as the run log. `build 105`.
- **The run log counts turns as well as calls.** A reply is one turn however many
  calls it ran, and the tool budget decides how many that can be; the log showed only
  the second number, so a 68-call answer and the turn it belonged to were read as the
  same fact. The summary now carries `turn N` before the call count, in the streamed
  log and in a reloaded one, and a reply's ordinal is read off its position in the
  transcript so a page that attaches mid-turn cannot disagree with a reload. `build 105`.
- **A paired phone stops expiring while it is in use.** Token lifetime was a clock
  that only ran down: an hour for an access token, thirty days for a refresh token,
  and a device that went quiet for longer than that had to be paired again from the
  Hub. Verification now slides the window. The first time a token is presented with
  less than half its lifetime left, the row is pushed back to a full lifetime, so a
  device that is used at all keeps working indefinitely, while a device that goes
  away still lapses on the old schedule. A retired row is refused before the renewal
  is ever considered, so revocation cannot be aged into a resurrection, and a young
  token is left alone rather than rewritten on every request. `TokenPolicy` carries
  the switch, default on. `build 104`.
- **MiniMax is a provider in its own right.** `api.minimax.io` speaks Chat
  Completions, so it is a registry row rather than an adapter: an OpenAI-compatible
  base URL, an `env:` key reference, and the existing probe populates its catalogue.
  Its streamed deltas carry `reasoning_content`, which the reader already accepts, so
  the thinking bubble fills the same as on any other provider. `build 104`.
- **The composer sits on the bottom of the window, at every window size.** The
  previous fix removed the dead band but left the column with a floor, so on a
  laptop viewport the column grew past the screen with its own content and the
  composer went under the fold - measured at 137px out of reach at 1280x800 and
  193px at 1024x768, while a tall window lined up by luck. The chat column now
  states its height against the window, the transcript scrolls inside it, and the
  only space under the composer is a 14px gutter; the phone breakpoint keeps the pin
  and zeroes the gutter because its composer already carries the safe-area inset.
  Returned as `height: 100vh; height: 100dvh` in place of the floor. `build 104`.
- **A Messages-dialect turn asks the provider to cache its own prompt.** The preamble,
  the tool schemas and the newest message now carry an `ephemeral` cache breakpoint on
  the Messages dialect, which is the only dialect that defines one. The persona and the
  working rules change about once a week and were being re-sent at full prompt price on
  every turn of every conversation; a breakpoint on the static head turns that into a
  cache read. The Chat Completions body is byte-identical to before, and the usage
  accounting already reported cache reads both ways, so a hit now shows up honestly
  instead of never being asked for. `build 103`.
- **Picking a model in the Hub is not an outage any more.** A picker that sends a
  provider-qualified pin - `commandcode/deepseek/deepseek-v4.1-flash` - was checked
  against a catalogue keyed by bare model ids, so a correct pin could never match; the
  device `set_model` path then re-checked the same value against the legacy
  `provider.models` row, and a validation refusal was reported as `503 db_unavailable`,
  which is why a working model looked like a down database. The value is now split with
  `chat::split_model_value` and checked against that provider's own catalogue, an
  unprobed provider takes the value at face value, and a refusal is a `400
  model_invalid` with the reason attached. The `/model` slash command splits the same
  way. `build 103`.
- **No dead band under a reply.** The conversation column is the page: the stylesheet
  gave the chat shell a height guessed from the chrome above it and left the prose
  rule's 72px bottom padding underneath, so a band of nothing sat below the composer.
  The shell now takes the room the column has left, the tail padding is gone on desktop
  and phone, and the phone breakpoint states its own viewport height instead of
  subtracting a constant. `build 103`.
- **An older client stops reading a 404 as a failure to read.** The retired relay had
  many hosts and a connector process to watch; Eris is one daemon on one box. So a
  shipped client that still asks for `/v1/health`, `/v1/config`, `/v1/gw/status`,
  `/v1/hosts/current`, `/v1/profiles` or `/v1/commands` was answered by a typed 404,
  which the phone renders as "the data couldn't be read" - a phone that can pair
  looking like a phone that cannot be configured. Sixteen legacy paths are declared;
  fifteen answer from something the daemon really holds - the `app_state` row,
  `config.toml`, the unit's own journal, `/etc/machine-id`, the real skill library -
  and the sixteenth, `/v1/hosts/current/revoke`, answers `403 host_revoke_unsupported`
  on purpose, because there is no second host to revoke. Nothing here invents a
  multi-host world to keep dead client code happy. `build 101`.
- **A message sent twice is one turn.** A phone that retries a `POST /v1/turns` after a
  dropped connection used to queue a second job and answer the same question twice.
  Turn creation now reads an `Idempotency-Key` header, and a `(device, key)` pair that
  already owns a job is handed that job back instead of minting another one. The key
  is derived from the message's own `client_message_id`, so a retry of the same message
  is idempotent while a genuinely new message is not. `build 101`.
- **Both transcript rows of a turn carry the turn's id.** The operator's question row
  and the assistant's answer row are stamped with the job id at the moment the job
  exists, rather than only when the turn settles. A page that refetches while the
  provider is still warming up now sees the two rows grouped as one turn, and a row is
  never left carrying half a turn. A turn whose rows cannot be stamped still answers -
  an unstamped turn is a turn whose rows are not grouped, not a turn that did not
  happen. `build 101`.
- **The phone's control screens are wired to Eris.** Models, cron, skills and the
  aux-model picker now have device routes on the daemon and Swift ports in the app,
  so the screens that still pointed at the retired relay stop 404ing.
- **Notes enrichment actually runs.** The daemon gains a worker that claims a
  queued enrichment run, streams deltas, and stores the enriched revision, and the
  app opens and polls a real run instead of reporting "not backed by Eris yet."
- **Gateway control is the daemon itself.** Health, restart preflight, restart and
  update-check routes exist on the device listener, and the app prefers Eris for
  them. `build 99`.

## 1.4.5

- **A reply on a phone keeps a share of the column.** The stylesheet sized a bubble
  by the window on a desktop and then handed it the whole column below 900px, so a
  reply ran edge to edge on a phone and read as a page rather than a message. The
  phone rule now caps a bubble at 88% of the column, which leaves the gutter that
  makes a turn read as a message while keeping both sides of a conversation in one
  column. `build 98`.

## 1.4.4

- **A page that lost the turn keeps watching it.** A tablet that suspended the
  browser mid-answer, or a reload, used to leave the transcript stuck on an empty
  "Streaming" bubble with no way back in: the only reader of a turn's stream was
  the request that had started it, and that request was gone. Every turn now
  publishes its frames to the conversation, `GET /dashboard/chat/live` streams
  them to any page that asks, and a reloaded page attaches on load (and again when
  the tab becomes visible) instead of showing dead air. The daemon also writes the
  partial answer, the run log and the round count into the row about once a second,
  so a reload shows real progress before it even attaches, and a row still marked
  streaming but with no turn running it is failed honestly as `interrupted` rather
  than left hanging for ever. The transcript lands on its newest message, and the
  status line says which round of how many is running. `build 96`.
- **A full sandbox does not ask before it writes.** `approval_policy = ask` parked
  every `write_file` even when the operator had already chosen a full sandbox, so a
  turn that wrote a file stopped dead on an approval nobody was answering - 29 rows
  piled up on this host. A full sandbox is the operator's own decision that writes
  are allowed there, so the gate leaves it alone; a confined sandbox still parks or
  refuses under `ask`. The rows the old behaviour left behind were expired. The
  device turn's gate matches: it refused a write under `ask`, and now a full sandbox
  runs it. `build 96`.
- **A phone-started turn shows its work in the dashboard.** A turn started from the
  device API wrote an empty transcript row until it finished. It now flushes the
  partial answer, run log and round count into the row the same way a web turn does,
  so a page watching it - or a phone re-attaching after a `buffer_miss` - sees
  progress instead of an empty "Streaming" bubble. `build 96`.
- **A dropped stream resumes where it stopped, and says so when it cannot.** The
  frames of a running turn now carry an SSE `id:`, so a browser that reconnects
  by itself tells the daemon the last frame it saw and is handed the frames it
  missed instead of silently resuming at whatever came next. A page that fell
  further behind than the last 256 frames is told to re-read the row rather than
  patched with half of what it lost. The daemon holds those frames only while the
  turn runs, so a restart still fails an interrupted turn honestly. `build 96`.
- **The transcript follows its own turn again.** The follow was bailing whenever
  the composer held focus, and the composer is focused on load and after every
  send, so on a desktop the page never scrolled itself. It now stands aside only
  for a draft with text in it, which is the one case the guard was ever for.
  `build 96`.
- **The thinking bubble follows its own stream.** Only a turn started in this tab
  scrolled the bubble; a page that re-attached, or reloaded, appended to a bubble
  parked on its first line. Both paths scroll it now. `build 96`.
- **Opening a conversation lands, and stays landed.** The transcript swept to the
  bottom twice and then stopped, so an answer that re-measured when its web font
  arrived ended a scrollbar short of the newest line. It lands again once the
  fonts are ready and once when a hidden tab comes back, and never after the
  operator has scrolled away. `build 96`.

## 1.4.2

- **The device list can be emptied the way the operator asked for it.** Thirty-four
  rows had piled up on this host, twenty-one of them revoked probes named after
  whatever the test was called, and the page could only revoke one row at a time
  while keeping every row for ever. The table now carries a tick per row and three
  verbs: revoke the ticked rows, delete the ticked rows, or delete every row that is
  already revoked. Revoking still keeps the row, which is the record that a device
  was cut off. Deleting removes the row and, by the schema's own cascade, every token
  it owned, and there is no undo - so an active row only goes when it was ticked and
  the deleting verb was chosen, and "delete all revoked" can never sweep up a pair
  that is still in use. The single-row Revoke button is unchanged and still posts to
  its own route, now as a submit button rather than a nested form. `build 95`.
- **A connection closed mid-turn no longer costs the answer.** A long turn on
  2026-10-22 got twenty-two rounds into a conversation, then died with "connection
  closed before message completed" and kept the half-sentence it had written. The
  request had been written into a socket the gateway had already dropped, so the
  gateway logged nothing at all and only the client ever saw the failure. Two
  changes. The HTTP client no longer offers a pooled connection that has been idle
  longer than a gateway's own keep-alive (9router advertises five seconds; the pool
  default was ninety), and a request that dies before a single response byte is put
  on a fresh connection once, which cannot duplicate anything because nothing had
  streamed. A repeated failure still reports normally. `build 94`.

- **Asking is a normal move again.** The old stack had a clarify card: a turn could
  stop mid-flight, ask, and resume on the answer. 1.0 had no such path, so a turn
  facing a real decision either guessed or stalled, and "does clarify work" was a
  fair question with the honest answer "no". It does now, without the card: every
  turn is told it may ask, that one question belongs at the end of the answer with
  the options it would accept, and that the operator's reply arrives in the same
  transcript so the next turn continues from it. The transcript is the resume path,
  the same principle the capped-turn fix rests on, and nothing is parked mid-turn,
  so there is no callback to lose. `build 93`.
- **A turn that spends its tool budget answers instead of shrugging.** The
  tool-free last round was already there, but the model was never told its tools
  were gone, so it did the natural thing and wrote the call it wanted to make.
  That markup arrived as the reply, was lifted out of the text as a tool call that
  could not run, and left an empty answer behind - which is how a long turn ended
  on "Stopped after 75 tool rounds without a final answer" with nothing to act on.
  Three changes close it. The tool-free round now opens with a notice that the
  budget is spent and prose is what is wanted. Recovered calls are only adopted
  while tools are actually on offer, so markup on the closing round is not spent
  on a call that cannot run. And if that round still yields no prose, the turn
  asks once more, plainly, then falls back to a summary of the work itself: the
  rounds spent, every tool that ran with its count, how many came back as errors,
  and the last result. "Nothing above was undone. Continue and I will pick up from
  here with a fresh budget." The transcript is the resume point, so the next turn
  knows where the last one got to instead of restarting the job. `build 92`.
- **Schedules.** A conversation can be put on a cron. A run is a real turn on the
  same path the composer uses, titled after the schedule, so it appears in the
  transcript like anything else. The expression is parsed when it is saved and
  `next_fire_at` is stored, so a bad one is refused at the form rather than
  discovered at three in the morning; the claim is a conditional update, so two
  ticks cannot double-fire the same run. A settings group carries the global
  switch, the create form, a toggle per schedule and its eight most recent runs,
  pruned to fifty. Anything left `running` by a dead process is settled at boot
  the way chat, subagents and device turns already were. `build 92`.
- **Skills.** The operator's own procedures are offered to a turn by name and one
  line of description; the body is never injected, so a library of eighty-nine
  costs the prompt a catalog rather than a library. A turn follows one by calling
  `read_skill` with its key, validated as a slug and capped at 64 KiB per file and
  24 KiB per read. The library is a directory on the settings page, one toggle per
  skill, rescanned on save. `build 92`.
- **The footer's context number is the window, not the sum.** `context_used` used
  to come from the first row that carried a window, and a failed round trip
  carries a window but no token count, so one failure blanked out a number the
  transcript already had. Used and window are now resolved independently, newest
  row first, and the footer says "unknown" only when the whole transcript is
  unknown. `build 92`.
- **A reload control, for windows with no chrome.** An installed homepage app has
  no browser reload button at all, so a daemon restart or a dropped stream left the
  operator with nothing to click. The composer row now carries a quiet **Reload**
  that fetches the conversation from the daemon and lands at the bottom of it. It
  is revealed by the script and turns visibly stale when the page has a reason to
  be reloaded, so a control that cannot act is never shown to a reader who has no
  script. `build 92`.
- **A paste is a message up to 128 KiB.** The ceiling was 16 KiB, which a stack
  trace or a log excerpt exceeds. The refusal names the limit in the units a person
  would use, and attachments remain the route for anything larger. `build 92`.

## 1.4.1

- **A fresh Linux box is one line.** `curl -fsSL
  https://raw.githubusercontent.com/fireishott/Kallisti/main/install.sh | sh`
  installs the daemon, the config, the workspace and a systemd user unit, with no
  root, no compiler and no manual step. The script prefers a prebuilt release
  binary and verifies its `.sha256`; it falls back to a source build on the pinned
  toolchain, installing `rustup` itself when Rust is absent. Already present, so
  nobody has to find them: `--from-source`, `--version`, `--prefix`,
  `--no-service` and `--dry-run`. A new `release` workflow builds the
  `linux-x86_64` and `linux-aarch64` tarballs the installer downloads, so the
  fast path exists on the first tagged push rather than only for whoever has a
  Rust toolchain. No daemon behaviour changed and no build number moved.
- **The `gh` install line is the line, not a puzzle.** The README's GitHub CLI
  section now leads with a no-root one-liner with the architecture written out,
  says the single token to change on arm64, and keeps the apt route and the two
  traps (the broken `universe` builds, the headless keyring) below it.
- **Resuming a turn that died always ends.** A turn interrupted by a stop, a
  crash or a killed process used to be a stream with no ending: the phone
  reconnected, replayed the frames that existed, then tailed a channel nobody
  published on, so the transcript sat in "thinking" forever. Two changes close
  it. At boot every `queued` or `running` device turn from the previous process
  is settled through the same path a live failure takes, so the status change
  and its terminal `error` frame are one transaction. And a stream opened on a
  turn whose status is already terminal synthesises the ending its status
  promises, for that response only - a read path still never writes. `build 91`.
- **A refused device request is in the journal.** Every 4xx from the device
  listener now logs its status and code, and an unknown path logs the method and
  the path exactly as it arrived. `/v1/v1/pair` used to be a silent 404 in the
  journal while the daemon was telling the client the answer in the body; now
  the doubled prefix is a line the operator can grep for. No message, token or
  pairing code is logged, and the response body is byte-for-byte unchanged.
  `build 91`.
- **Stopping the daemon stops the daemon.** `runtime.shutdown_timeout` bounds the
  wait for blocking work at five seconds. Dropping the multi-thread runtime used
  to block until every `spawn_blocking` task finished, and a turn runs its whole
  provider loop on one of those, so a stop during a turn held the process open
  past systemd's 90-second `TimeoutStopSec` and got SIGKILLed. The listeners
  still drain first and the interruption is settled at the next boot. `build 91`.
- **Two backups from the same second list in the order they were written.**
  A name sort put `kallisti-<stamp>-2.bk` after `kallisti-<stamp>.bk`, because
  `-` sorts before `.`, so the newest archive read as the older one. The stamp
  and its collision number are compared separately now. `build 91`.
- **A file is a message.** An empty composer with an attachment used to be a
  refusal by name, so sending a screenshot meant typing filler first. The box is
  now accepted when something is attached, and the row the transcript gets is
  named after the files - "Attached IMG_0082.png", or "Attached 2 files: a.log,
  b.json" - which is also the line the conversation is named after. Send reads as
  waiting until there is something to send, and a queued file is enough. `build 88`.
- **The run log follows the work in front of you.** The tool-log setting
  (`off`, `summary`, `full`) only ever reached the server render, so a page opened
  on `summary` streamed one thing and reloaded as another; the live builder now
  reads the choice off the shell and builds that shape. Only the running call is
  on screen by default - the count in the summary still says how many ran - and a
  new **Show all** toggle brings the rest back and is remembered per browser.
  The panel no longer forces itself open on every fragment, which was what made a
  dozen commands a wall of windows. `build 88`.
- **Typing mid-turn does not fight the scroll.** The transcript is followed at
  most once per frame instead of once per token, and not at all while the
  composer has focus: following the bottom is what dropped the iOS keyboard out
  from under a half-typed message. `build 88`.
- **The conversation gets the window.** The transcript used to sit in an 880px
  column inside the 1180px prose wrap, so on a wide screen a band of empty width
  ran down each side of it that nothing could be typed into. The chat page now
  lets its wrap fill the window, and the shell keeps a 1180px cap so a very wide
  display does not run the composer off the screen. `build 88`.
- **A bubble is sized by the screen, not by its text.** Two things were wrong at
  once. The transcript was a grid with no column declared, so its implicit `auto`
  track sized to the widest child: one long paragraph, code block or table made
  the whole column wider than the window, which pushed a right-aligned operator
  bubble off the right edge and left the answer scrolling sideways. The track is
  now one bounded column (`minmax(0, 1fr)`), and a bubble takes a fixed share of
  the window (`width: min(100%, clamp(22rem, 78vw, 52rem))`) so every message sits
  in the same column - a short answer no longer hugs its text while the next one
  spans the screen. A bubble and its contents are capped at `100%`, and the 520px
  table floor that used to apply to every table on a phone now applies only to a
  settings card, so a table in a reply cannot widen the conversation either.
  `build 90`. Supersedes the `max-width`-only measure in `build 89`, which capped
  how wide a bubble may *look* without stopping its content from widening the row.

- **The run log holds its shape.** Command output used to wrap, so every time
  the window or the panel changed width the log re-laid itself out under the
  reader - the lines moved while they were being read. It does not reflow now:
  a line is the width the command printed, `white-space: pre`, and the window
  scrolls left to right so the end of a long line is a drag away instead of a
  rewrap. The window keeps its own width (`max-width: 100%`) so the conversation
  around it does not resize either, and `overflow: auto` keeps the vertical scroll
  it already had. Reading the log is a manual act, so nothing about it is
  automatic. `build 90`.

- **A rate or usage limit is no longer a silent failure.** 9router reports a
  locked account as a 503 whose body carries the upstream `[429]`; Eris now reads
  that as a rate limit, takes the wait from `retry-after` or the body (`reset after
  30m`, `resets at ...`), and writes "Rate or usage limit on X - resets in 5m" into
  the answer row, so a reload shows it instead of an empty failed bubble. An
  attached image that no vision model read is now said out loud and told to the
  model rather than vanishing. `build 87`.
- **The answer footer reports rounds and cache share,** beside tokens and latency,
  the same way live and after a reload. A new **Settings -> Quota** toggle (off by
  default) adds the gateway's limit state under each answer - ok, locked or
  limited, time to reset, requests today - read from 9router's own database,
  read-only. `build 87`.

Two halves, back together: **Kallisti 0.4.0 is the iOS frontend, Eris is the
backend.** Versions align at 1.4.1 (daemon build 86, iOS app build 89).

- **The Kallisti 0.4.0 UI is the app, on Eris.** The iOS frontend is the literal
  0.4.0 UI restored unchanged - `AppEntry` -> `AppRootView` -> the full
  `AdaptiveRootView` sidebar (Chat, Inbox, Talk, Notes, Settings), device-permission
  onboarding, the Senses view and the Kallisti theme. No reduced shell, no new
  screens: `HeraldClientProtocol` is implemented by `ErisHeraldClient`, so the 0.4.0
  screens run over Eris as they are. A fresh install pairs to Eris through
  `ErisPairingService` (the onboarding's address + code posts to `/v1/pair`, the pair
  lands in the keychain, and the client restores it into chat). `iOS 1.4.1 build 89`.
- **Eris is a full backend.** Inbox (list and actions), Notes (full CRUD, blobs,
  recognitions, and the run lifecycle) and Push (register, deactivate, send) gain
  real routes and their own stores, so the frontend's Inbox and Notes tabs are
  backed rather than pending. Talk remains in development and answers a typed
  not-implemented rather than a 404. `build 86`.
- **The daemon has senses.** Health and location gain device routes
  (`POST /v1/device/sensor/health`, `POST /v1/device/sensor/location`, plus
  `GET /v1/device/health/latest` and `GET /v1/device/location`), stored in
  `sensors.db` on the legacy schema, so the app reads live health and location
  data instead of a placeholder. `build 86`.
- **The web UI works on a phone.** The chat surface and the setup wizard both got
  a responsive pass: an off-canvas sidebar drawer below the mobile breakpoint,
  `safe-area-inset-bottom` on the composer, `100dvh` sizing, and a scrim so text
  stays readable over the wallpaper. The setup page no longer prints the internal
  listener telemetry into the user-facing footer. `build 86`.
- **A photo no longer looks like a dropped connection.** The chat POST inherited
  Axum's 2 MiB body ceiling, so an iPhone screenshot over that size was refused
  while the multipart parser was still reading it, and the page said "lost the
  connection". The route now accepts the same 64 MiB the attachment rules
  already state, and a refusal states the daemon's own sentence instead of the
  connection message. `build 85`.
- **The live turn draws pictures too.** A sent picture is a thumbnail in the
  bubble the moment it is sent, not only after a reload, and a file reference in
  the answer is rendered when the stream finishes. The live run log also starts
  closed, matching the server-rendered one. `build 85`.
- **The assistant can show what it made.** A workspace path written as
  `![caption](generated/x.png)` is drawn as a thumbnail in the bubble, and
  `[name](docs/brief.pdf)` is a chip with its type badge and a download. The new
  `GET /dashboard/chat/workspace/{path}` route resolves through the tools'
  three-layer confinement, decides the type from the bytes, and refuses anything
  outside the workspace. `generate_image` now returns a ready display line, and
  every turn carries a short note saying how to write one, so a picture the model
  drew arrives as a picture rather than a path. `build 84`.
- **A picked picture is the picture.** The composer's attachment queue shows a
  thumbnail for an image before it is sent, through an object URL the browser
  owns, so a mis-picked screenshot is one glance away instead of one send away.
  `build 84`.
- **The run log starts closed.** Full mode used to render one open panel with
  every call inside it; it is now one line saying how many calls ran, and the
  existing summary and off modes are unchanged. Nothing expands itself, so a
  turn that ran a dozen commands cannot push the answer off the screen. `build 84`.
- **Streaming is a setting.** Settings > Responses gains a switch. On is the live
  transcript; off hands the turn to the plain form and returns the finished page
  in one response. The stored choice wins over an `Accept: text/event-stream`
  header, so a page that says do not stream cannot be talked into streaming.
  `build 84`.
- **A pairing code a phone can actually reach.** Settings > Device listener gains
  a pairing relay URL. The pairing QR, the wizard's device step and the JSON mint
  all carry that origin instead of a loopback bind, and the wizard now draws the
  QR the devices page already drew. The app appends `/v1` itself, so the field
  takes a scheme and host only; for a phone on the tailnet that is the Tailscale
  Serve name, `https://host.tailnet.ts.net`. `build 84`.
- **Keep me signed in.** The sign-in form gains a checkbox that mints a thirty-day
  admin session instead of the twelve-hour one, so a trusted device or a browser on
  the LAN/tailnet is asked for the passphrase once a month. The default stays short.
  A sign-out control in the sidebar revokes the session server-side and returns to
  the form, which is also the only way back to the checkbox. `build 83`.
- **A phone can back itself up.** A paired device lists, takes and restores a backup
  over the device API (`/v1/backups`, `/v1/backups/{name}/restore`), all admin-scoped,
  so a backup is no longer a host-side chore. `build 81`.
- **Fallback model.** Settings > auxiliary models grows a Fallback role. Leave it on
  `primary` and nothing changes; pin it to another model and a turn that fails retries
  once on that model, with the answer badged as the model that produced it and every
  retry audited. `build 82`.
- **A dead stream is now a failure, not a freeze.** A provider that stops sending
  bytes for 60 seconds fails the turn instead of leaving the conversation stuck on
  "streaming" forever, so the fallback (or a retry) has something to act on. `build 82`.
- **Approving runs the call.** A decision used to settle the row and change nothing
  else, so an approved write waited for a CLI run on the host while the phone said
  "approved". The execution path now lives in the library and both surfaces call it:
  the CLI and `POST /v1/approvals/{id}/decide` execute the call, rewrite the
  transcript entry with the real result, and settle the row. A tool that refuses
  leaves the row pending and answers 422 with the reason, because nothing was
  written and nobody has said yes to anything yet.
- **A device can administer, if the operator says so.** Minting a pairing code grows
  one unchecked box: tick it and the redeemed token carries the admin scope, leave it
  and the device chats and files conversations and nothing more. The decision travels
  with the code (migration 0015), so it is made once by a person rather than inferred,
  and the devices table reads "can administer" or "chat only". `GET /v1/settings`
  returns the operator-facing settings with no credential, key, path or base URL, and
  `POST /v1/settings` accepts any subset of model, sandbox, memory and approval with
  all-or-nothing validation and an audit row per change. A token without the scope is
  refused with a sentence that says the device was paired without administration and
  how to fix it, rather than "forbidden".
- **Subagents.** A turn in Full mode can call `dispatch_agent` and get a real child
  turn back: its own provider call, the parent's persona, memory and summary, its own
  token accounting, its result stored on a row. Four run at once daemon-wide and eight
  per parent turn, and both refusals name the cap. A child is offered no tools at all
  and cannot dispatch another - three independent layers say so, because a child that
  can run commands is a host process spawned on a model's say-so. The top bar grows a
  server-rendered dropdown listing each child with its status, model, elapsed time and
  result, and the parent's stream gets a `subagent` frame as each one changes state.
- **The run log expands all, collapses all, and remembers which.** The controls sit
  under the log summary rather than inside it, since a focusable button inside a
  summary fights the disclosure and rewrites its accessible name. The preference lives
  in local storage, not in a settings row, so there is no fourth mode to explain.
- **Steering.** A second message during a turn queues for that turn and is picked up at
  the next boundary instead of starting a competing one; Send reads "Steer" while
  streaming. Stop still stops at a boundary rather than aborting the socket, and both
  the code and the copy say so.
- **Attachments show themselves.** Images render as thumbnails in the bubble linking to
  the full file, everything else as a chip with a type badge, name and size. The
  serving route takes its type from the bytes, never the name, keeps the read bounded
  and confines the path to the uploads root.
- **A second message steers the running turn.** Sending while an answer streams no
  longer starts a second turn: the message is queued for the turn that already owns
  the conversation, and that turn appends it and answers it as a continuation at its
  next boundary, before the next provider round trip and between the tool calls of a
  round. One turn, one bill, and the answer the operator just redirected is the one
  that lands. The composer stays usable while streaming and its send control reads
  `Steer`, with the queued count stated on the page a plain form post lands on.
  `/stop` and the Stop button now also ask the running turn to stop, so it ends at
  its next boundary instead of finishing the round and settling an answer the
  operator already declined. The provider request already in flight is not aborted
  mid-stream: that seam is one blocking call, and the copy does not pretend
  otherwise.
- **Conversations over `/v1`.** A paired device can list its conversations, start
  one, read a transcript and clear it, so a client shows history instead of firing
  one-shot turns into the void. `GET /v1/conversations` is newest first and capped
  at a hundred, with `count` and `truncated`; `POST /v1/conversations` starts an
  empty one and answers 201; `GET /v1/conversations/{id}` returns the messages
  oldest first, capped at two hundred with `truncated`, and carries the same
  reasoning and tool records the browser stores; `POST /v1/conversations/{id}/clear`
  runs the dashboard's own clear statement. `POST /v1/turns` takes an optional
  `conversation` field: naming one writes the question and the answer into that
  conversation and the response reports `conversation_id`, and an id the daemon does
  not hold is a typed 404 `conversation_not_found` with no turn created. Every route
  needs a bearer carrying the `sessions` scope, and both writes are audited with the
  device as the actor.
- **A generated greeting line, off by default.** With it on, an empty conversation
  shows one short line under the salutation, written by a model of the operator's
  choosing (or the chat model) and rewritten on an hourly, daily or weekly window.
  The prompt carries no name, path, hostname, memory or transcript - there is no
  field that could carry one - and the line is scrubbed of bangs, em dashes, quotes
  and length overruns. Cached in settings, so a render only asks a model when the
  line is due, and a slow or failing provider falls back to the stored line rather
  than hanging the page.
- **The line under the salutation is editable.** It was a constant in a template;
  it is now a setting, blank restores the default, and the composer row is unchanged.
- **The composer row is one row.** The small-screen rule gave the send control a
  100% basis, so it could never share the line and was forced onto its own full-width
  row - the detached block that read as half off. Send, stop, both selects and the
  attach control now share one height and radius, the composer is a `minmax(0, 1fr)`
  grid so a long model id shrinks instead of blowing the box out, and Send keeps a
  platinum border and focus ring while losing the inverted block.
- **A generated greeting line.** The empty chat page can carry one short line under
  its salutation, written by a model. It is off by default and stays off until
  Settings > Greeting switches it on, because every rewrite is a billed provider
  call. The line is cached in the settings table with the second it was written, and
  it is only regenerated when it is older than the chosen window (hour, day or
  week), so a render serves the stored line instead of calling a model. The prompt
  carries no name, path, hostname, memory or transcript, and the answer is scrubbed
  to one line of at most 90 characters with no bang, no quote and no em dash. With
  the switch off, or with no line stored yet, the page renders exactly what it
  rendered before.
- **Projects.** Sessions can be grouped under named projects from the rail: a compact
  "New project" control sits under "New chat", each session row carries a menu that
  files it, and the rail paints project headings with ungrouped conversations below.
  No JavaScript: the menus are disclosures, so it works on an iPad and with a script
  blocker. A fresh rail says "No projects yet" rather than showing an empty control.
- **Slash commands.** A message starting with `/` is answered by the daemon and never
  sent to a model, so it costs no tokens. `/help`, `/new`, `/model <id>`,
  `/memory <gbrain|vault|off>`, `/clear` and `/stop` all work. Anything else is
  refused by name, and the refusal says plainly that `/goal` and `/restart` are not
  Kallisti features rather than pretending. `/stop` marks the running answer
  cancelled and the settle path refuses to write over it; the in-flight provider
  request is not aborted mid-stream.
- **A security page.** Rotating the sign-in credential gets its own page at
  `/dashboard/security`, because the settings page is tested to contain no password
  input at all - nothing stored there is a secret value, only references. Rotation
  requires the current credential even with a live session, refuses anything under
  eight characters, stores an argon2id verifier, and deletes every admin session so
  old cookies do not survive the change. The page states the recovery truth: on a
  self-hosted daemon there is no recovery e-mail, and the `admin_credential` row is
  the only way back.
- **The device listener can bind where a phone can reach it.** `/v1` bound
  `127.0.0.1:7341` as a literal, so pairing was only ever provable from the host.
  Settings > Device listener now takes a bind list, validated by the same parser the
  admin listener uses, and every address binds at once. Loopback stays the default.
  The device API is `/v1` only and every call needs a paired token, so opening an
  address does not open the dashboard. Applied at the next start, and the page says
  so rather than pretending it is instant.
- **Scannable pairing.** Minting a code renders a QR on the Devices page whose
  payload is the shape the legacy client already parses - compact JSON,
  `{"code":"...","relay":"http://host:7341"}`. `relay` is the daemon's own bind,
  preferring a non-loopback address, because a QR carrying `127.0.0.1` is a code
  that cannot work on a phone. Drawn as inline SVG with a quiet zone; no image
  encoder, no binary on the page.
- **Calls that arrive as text are re-issued as calls.** Some provider routes fail to
  lift a model's tool calls into the structured array and pass its own markup
  through as content. That markup is now parsed back into real calls, which take the
  normal path - same approval gate, same executor, same run-log records - and the
  visible answer keeps the prose and loses the tags.
- **Memory backends.** Before a turn answers, the daemon recalls from the operator's
  own memory and adds what it finds to the prompt, marked as possibly stale context
  rather than as instructions. Settings > Memory chooses the backend: **gbrain**
  (default, the page store on this host), **vault** (a directory of markdown) or
  **off** (prompt only, costs nothing).
  Recall is best effort: a brain that is down leaves the turn exactly as it was, and
  the reason is recorded as a `memory.recall` audit row instead of being swallowed.
  The gbrain binary and its `bun` interpreter are resolved explicitly, because a
  systemd user unit's PATH carries neither.
- **MIT.** The repository now carries a `LICENSE` with the MIT terms, every crate
  declares `license = "MIT"`, and the README states it. The earlier README reserved
  all rights because no licence file existed; this replaces that.

## 1.4.0

The platform release: configurable routing, an MCP client, and roles that actually
run.

- **MCP client.** JSON-RPC over newline-delimited stdio. `initialize` handshake,
  `tools/list`, `tools/call`, one owned child process per configured server,
  started on demand, killed and reaped on shutdown. Every request has a deadline,
  so a hanging server is a failed tool call and never a hung turn. Tools are offered
  as `server__tool`; a call is dispatched to the pool that owns the server. A server
  that cannot start is named on the settings page instead of silently offering
  nothing.
- **Approval review.** A pinned approval role writes one line about a parked call -
  `SAFE`, `CAREFUL` or `RISKY`, then why. Advice only; the row still waits for a
  person, and a test asserts that advice alone cannot settle it.
- **Image generation.** `generate_image` is offered only when the image role is
  pinned, posts to the provider's images endpoint, and writes the result into
  `generated/` inside the workspace. The file extension comes from the returned
  magic bytes, because the endpoint returns JPEG whatever it is asked for.
- **Web fetching.** New `fetch_url` tool: http and https only, 15s timeout, five
  redirects, 512 KiB cap, HTML reduced to readable text with scripts and styles
  dropped whole. Loopback, link-local and private address literals are refused
  before a socket opens.
- **Approvals rewrite the transcript.** Approving a parked call replaces that call's
  record with what actually ran, keeping the approval id, and leaves every other
  call in the turn alone. Previously the row kept saying `pending_approval` forever.
- **Thinking bubble is open by default** with a control that lifts it out of the
  column.
- **Run log as a window, with three modes.** Settings > Tools offers Full, Summary
  and Off.
  *Full* renders one closed one-line entry per call - tool and arguments on a single
  truncated line - and opening it shows the output in a short scroll box that stays
  bounded when expanded. The log body scrolls, so twenty calls cost one window
  rather than the conversation.
  *Summary* collapses the whole turn into a single collapsed strip: call count plus
  the last call, truncated, opening to the full list.
  *Off* means neither rendered nor streamed, so command output does not leave the
  daemon. The original on/off boolean is still read and maps to full/off.
- **Tool output is drawn as a window**, the way the iOS client shows it: traffic
  lights from the palette, a `kallisti - stdout` title, the character count, then
  the text in a bounded scroll body. The thinking block is a rounded card on its
  own surface. Streaming builds the same markup as the server, so a live turn and a
  reloaded transcript are the same page.

## 1.3.0

Configurable routing, and the first role that proves it.

- **Provider registry.** Providers are a validated list - `{id, name, base_url,
  key_ref}` - with one marked primary. Key references stay `env:NAME`; a literal key
  value is refused by name. The three original provider settings seed a `default`
  entry, so an existing install keeps answering with no action, and the turn
  resolves its endpoint through the registry.
- **Roles choose a provider as well as a model.** Each of the twelve auxiliary roles
  stores both. An id that has left the registry falls back to the primary rather
  than breaking the role; an id that was never in it is refused by field.
- **Title generation wired.** A settled turn can ask its resolved role to name the
  conversation, replacing the truncated first message. Every outcome is recorded as
  an audit row, including "no title suggested".
- **Compression wired.** Turns that fall out of the history window are summarised
  into the prompt instead of being dropped silently.

## 1.2.0

The turn becomes observable.

- **Run log.** Every executed tool call streams as its own event and is stored
  beside the answer, so a turn that ran a command renders as a turn that ran a
  command: name, arguments, output, failures marked.
- **Thinking bubble** renders open by default with an expand control; the toggle to
  disable it lives in Settings, and off means the reasoning is neither rendered nor
  streamed.
- **Tool-round cap** now ends with one tool-free attempt, so a turn that called tools
  to the limit returns an answer instead of a status line.
- **Host context file.** `context.md` beside the persona is appended to the system
  prompt, so a turn knows what the machine is without the identity form being able
  to clobber it.

## 1.1.0

Configuration surfaces for capabilities that were arriving.

- **Tools settings**: tool calling on or off, and a round budget bounded at 40
  because every round is a billed provider call.
- **Auxiliary model roles**: twelve named jobs with a stored model each, defaulting
  to the primary. Rows state whether anything reads them yet.
- **MCP server list**: validated JSON, stored, and at that point explicitly
  configuration only - the page said so until a client existed.

## 1.0.9

- Chat attachments: text files ride in the prompt, binaries are named and located.
- Full host access became a switch the operator can flip, still requiring
  `--allow-full` at process start to select `full` sandbox mode.

## 1.0.8

- Attachments and the access level surfaced in the composer.

## 1.0.7

- Subtle wallpaper: the artwork sinks into the page instead of sitting on it.

## 1.0.6

- Wallpaper restored byte for byte after a theme experiment was rejected.

## 1.0.5

- Wallpaper becomes the whole desk rather than a framed picture.

## 1.0.4

- Session rail: a conversation list, a cog, and deletion confined to one row.

## 1.0.3

- Application shell: navigation, footer, titles, the dashboard System card, editable
  settings and an activity page.

## 1.0.2

- Chat metrics: per-answer usage, cache share, context fill and speed, each shown
  only when the provider actually reported the number.

## 1.0.1

- Conversation surface, model picker, usage accounting, wallpaper.
- Session list, sessions and devices pages, job cancellation.
- Themed UI in obsidian and platinum.
- Persona: an identity carried on every turn.

## 1.0.0

First working daemon.

- Onboarding state machine with a six-word passphrase, a claim rate limiter, and
  a permission matrix whose defaults are the narrow ones.
- SQLite state in WAL mode with forward-only migrations, salted verifiers only, and
  an audit trail.
- Admin listener on loopback, typed error bodies, and `/healthz`.
- Pairing codes and opaque device tokens; a device listener on a second loopback
  port.
- Backup and restore: `VACUUM INTO` snapshot, verified archive, atomic restore, and
  a passphrase that is excluded from every archive unconditionally.
- OpenAI-compatible provider adapter with streaming SSE and per-index tool-call
  buffering.
- Lite spike: a UniFFI-exported function running in-process with a fake provider, no
  network.