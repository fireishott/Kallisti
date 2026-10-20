# Changelog

Versions follow the daemon's own `VERSION`, with a build number that moves on every
ship without changing the version. Entries describe behaviour that a user can
observe; commit history carries the detail.

## Unreleased

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
