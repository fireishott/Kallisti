# Changelog

Versions follow the daemon's own `VERSION`, with a build number that moves on every
ship without changing the version. Entries describe behaviour that a user can
observe; commit history carries the detail.

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
