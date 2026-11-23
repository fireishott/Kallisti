# Kallisti

<div align="center">

**A self-hosted AI companion for iPhone and iPad.**
Yours. On your hardware. Under your rules.

[Private beta on TestFlight](#private-beta) · [Install the daemon](#install-the-daemon) · [Pair your phone](#pair-your-phone) · [Guides](#guides)

</div>

---

## What it is

Kallisti is a native iOS app and a Rust daemon. The app talks straight to the daemon on
a machine you own. There is no vendor backend in the middle, no usage meter, and no
account to create.

Your conversations, files, memory and credentials stay on hardware you control. The
daemon can run on a Mac mini in a closet, a VPS, or a box under the desk. The phone
reaches it over Tailscale, your LAN, or your own TLS endpoint.

## The app

- **Streaming answers, built for long work.** Text and reasoning arrive as they are
  produced. A live tool panel shows what the daemon is running, with the step it is on
  and what the command printed. Leave the chat and come back: the stream reattaches to
  the turn that is still running.
- **Thinking that tells the truth.** A turn that is slow says so, with the round it is
  on and the time it has spent, instead of an empty bubble.
- **Files, not just text.** Attach photos and PDFs. Uploads show per-file progress and
  a result, and land on the daemon with the turn that names them. Open them again from
  the transcript.
- **PDFs you can actually use.** Tap a PDF in a chat to open it, fill its form fields,
  and share or save the filled document from the same screen.
- **Texts from your own number.** Stage a message, see the words and the
  recipient, confirm it, and it goes out of your own Messages app through a
  BlueBubbles server on the host. The send carries the server's message id or the
  server's reason for refusing it, so nothing is ever called delivered twice for
  free.
- **The phone's senses, read properly.** Location, health and activity come back
  through tools that carry the sensitivity level, so a key left at `off` reads as
  recorded-but-withheld instead of missing.
- **Approvals.** A turn that wants to write a file or touch the system can park and
  ask. You approve or refuse from the phone, with the detail that matters.
- **Cron jobs.** See every schedule on the host, what it will run, when it fires next,
  and whether the last run worked. Thirty jobs or none, it is the daemon's own list.
- **Skills.** Browse the daemon's skill library, search it, and install a skill from
  the phone.
- **Notes.** Each note can carry its own chat session, so a thought and the work around
  it stay together. Sync is opt-in and iPad-first.
- **Continuity across devices.** Start on the iPad, continue on the iPhone. The chat
  binds to the daemon's conversation, not to the device that created it.
- **Notifications that know where you are.** Push arrives for the chats you are not
  looking at. The chat on screen stays quiet.
- **The daemon's own console.** Health, latency, logs, settings and the update check,
  read from the daemon rather than guessed. Edit the daemon's configuration from the
  phone, and install an update from the phone, with a confirmation before it restarts.
- **Model choice.** Point the daemon at the model you want, including local and
  OpenAI-compatible providers, and change it per conversation.
- **Routing that names itself.** Coding, heavy and general turns each go to the
  model you pick for them, and a finished message says which route it took.
- **A coding tool where one belongs.** Hand code work to dsh, Claude Code or
  Antigravity, watch it work in a live monitor, and install or sign it in from
  the phone when it is missing.
- **Everywhere else.** Widgets, Control Center controls, Live Activities, and a watchOS
  companion.

## Routing and coding harnesses

Two questions decide where a turn runs: what kind of work it is, and whether a
coding tool should do it.

- **Classification is explicit.** A message is coding when it carries a code
  block, a source-file extension or a verb like implement, refactor or debug;
  heavy when it is long or asks for research, a plan or a design; general
  otherwise. The reason is shown, not guessed at, and `POST
  /v1/routing/preview` answers what a message would do without running it.
- **A model per intent.** Coding, heavy and general each have a model. An empty
  field inherits the daemon's own, and the pickers offer the daemon's catalogue
  grouped by the provider that publishes each id, so the choice names where it
  runs.
- **A coding tool does the code work.** With a harness configured, a coding turn
  is offered `delegate_to_harness`: the task goes to `dsh`, Claude Code or
  Antigravity as one argument, the tool works with its own tools in the
  configured directory, and its report comes back. The tool's model is one of the
  ones it actually ships (Sonnet, Opus, Haiku for Claude Code), never an id from
  the chat catalogue.
- **Installed, or installable.** The daemon reports whether each tool is present,
  its version, and, for Claude Code, the account it is signed in as. A missing
  tool can be installed from the phone, and a signing-in flow opens the tool's
  own authorization page and takes the code back, so the daemon needs no browser.
- **Watched while it works.** Each run is recorded with its status and the tail
  of its output. A run silent for ninety seconds shows as stalled without being
  killed, the deadline still ends it, and the monitor in the web UI and in the
  app shows the live tail with a Kill for an administrator.

## Privacy by design

- **No vendor backend.** The app has no server of ours to sign into. There is nothing
  to subscribe to and no third party in the request path.
- **Your host holds the data.** Transcripts, attachments, memory and settings live in
  the daemon's own SQLite database on your machine.
- **Opaque device tokens.** Pairing exchanges a one-time code for an access and refresh
  pair. Tokens are stored as salted verifiers, never in the clear, and live in the iOS
  keychain.
- **Scoped access.** A phone paired without the administration box can chat and nothing
  else. Reading settings, logs, cron jobs or skills needs a code minted with it.
- **Writes are gated.** Sandbox modes run from read-only to full, and approvals stop a
  write before it happens.
- **No telemetry to us.** The app reports to your daemon and nowhere else.

## Private beta

Kallisti for iOS is in **private beta on TestFlight**. Invites are sent by link, one
test cohort at a time.

Want in? Open an issue titled `TestFlight` with your Apple ID email and the platform
you will run it on. You will get a TestFlight invitation and the daemon notes to pair
against.

What the beta means in practice: builds ship most nights, the daemon and the app move
together, and things you find go straight into the next build. Expect the app to change
under you, and expect it to get better every time.

Requirements: iOS 18 or later, and a host you can install a small Rust service on.

## Install the daemon

One line on a Linux host with systemd:

```sh
curl -fsSL https://raw.githubusercontent.com/fireishott/Kallisti/main/install.sh | sh
```

It picks the right architecture, installs a prebuilt binary when the release has one,
falls back to building from source with a pinned toolchain when it does not, writes a
`systemd --user` unit, and starts it. No root, nothing outside `$HOME`.

Or build and install from a checkout:

```sh
git clone https://github.com/fireishott/Kallisti
cd Kallisti
cargo build --release
scripts/deploy.sh            # waits for idle so a running turn is not killed
```

The daemon listens on two ports:

| Port | Surface |
|---|---|
| `:7340` | administration: dashboard, settings, logs, updates |
| `:7341` | the device API: turns, streams, attachments, cron, skills |

Then open `http://127.0.0.1:7340` on the host to finish setup. The dashboard is where
you mint pairing codes. For the same steps from the terminal, see
[DAEMON.md](DAEMON.md).

## Pair your phone

1. **Mint a pairing code** on the host's dashboard at `:7340`, under Devices. Tick
   **administration** if the phone should read settings, logs, cron jobs, skills, and
   manage updates. Without it the phone can chat and nothing more.
2. **Enter the address** of the daemon in the app, plus the code. The address is
   whatever reaches the host: a Tailscale name, `http://192.168.1.x:7341`, or your own
   TLS endpoint.
3. **Send a message.** The first turn creates the conversation, and the transcript is
   stored on the daemon from that point on.

## Build the app

```sh
xcodegen generate
open Kallisti.xcodeproj
```

`project.yml` is the source of truth; the `.xcodeproj` is generated, so add a source
file and regenerate. Set your own team and bundle identifiers in `project.yml` for a
local build. A TestFlight build needs an App Store Connect key, which is what
`scripts/release-<build>.sh` and `scripts/tf-<build>-oneshot.sh` drive.

## Verify it

```sh
cargo test --workspace

xcodebuild test -project Kallisti.xcodeproj -scheme Kallisti \
  -destination "platform=iOS Simulator,name=iPhone 17 Pro" \
  -only-testing:KallistiTests
```

## Architecture

```
iPhone / iPad  ──HTTPS + SSE──▶  Kallisti-Eris daemon  ──▶  model provider
                                   :7341 device API            (yours, local or hosted)
                                   :7340 administration
                                   SQLite: transcripts, memory, cron, skills
```

- **Turns** are `POST /v1/turns`, answered by a frame stream at
  `GET /v1/turns/{id}/events` that resumes from the last event id the client holds.
- **Files** are `POST /v1/attachments` with the bytes and a filename; the turn names
  the ids it carries.
- **Everything else** the phone can do is a route on the same device API: conversations,
  approvals, cron, skills, notes, memory, settings, diagnostics.
- The wire is documented in [docs/wire-contracts.md](docs/wire-contracts.md) and
  [docs/STREAM_CONTRACT_V2.md](docs/STREAM_CONTRACT_V2.md).

## Guides

- [DAEMON.md](DAEMON.md) - installing, pairing, operating, updating and troubleshooting
  the daemon.
- [docs/wire-contracts.md](docs/wire-contracts.md) - every device route and its shape.
- [docs/STREAM_CONTRACT_V2.md](docs/STREAM_CONTRACT_V2.md) - the turn frame stream,
  including resume.
- [CONTRIBUTING.md](CONTRIBUTING.md) - layout, build, tests and house style.
- [CHANGELOG.md](CHANGELOG.md) - what changed, app and daemon, per build.

## Support

Open an issue. For anything that happens in chat, the daemon's own record answers it
first: `GET /v1/diagnostics` holds the recent events, and `GET /v1/gw/health` says
whether the daemon thinks it is well.

## License

MIT - see [LICENSE](LICENSE).
