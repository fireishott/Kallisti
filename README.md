# Kallisti

> **To the Most Beautiful.** The most beautiful way to meet your agent.

<p align="center">
  <img src="https://img.shields.io/badge/daemon-1.4.11-C8CCD2?style=flat-square&labelColor=0C0C10" alt="daemon 1.4.11"/>
  <img src="https://img.shields.io/badge/iOS-1.4.14-C8CCD2?style=flat-square&labelColor=0C0C10" alt="iOS 1.4.14"/>
  <img src="https://img.shields.io/badge/Rust-1.99-F74C00?style=flat-square&logo=rust&logoColor=white" alt="Rust 1.99"/>
  <img src="https://img.shields.io/badge/tests-1159-C8CCD2?style=flat-square&labelColor=0C0C10" alt="1159 tests"/>
  <img src="https://img.shields.io/badge/device_api-no_relay-C8CCD2?style=flat-square&labelColor=0C0C10" alt="no relay"/>
  <img src="https://img.shields.io/badge/license-MIT-C8CCD2?style=flat-square&labelColor=0C0C10" alt="MIT"/>
</p>

Kallisti is a complete package built from three parts that ship together.
**Kallisti for iOS** is the frontend - a native Swift 6 / SwiftUI app for iPhone
and iPad. **Kallisti-Eris** is the backend - a Rust daemon that holds the brain,
the tools and the memory, on hardware you own. **The Kallisti-Eris Web UI** is the
daemon's own dashboard, served by that same binary, for the times the phone is not
in your hand. The app talks to the daemon over its device API; there is no
third-party relay, and nothing leaves your infrastructure.

**Private TestFlight beta in progress.** MIT licensed: the source is open, and it
stays free to compile from source.

---

## The three parts

| Part | Where it lives | What it is |
|---|---|---|
| **Kallisti for iOS** | branch `restore/kallisti-identity-on-eris` | The front. The literal Kallisti 0.4.0 UI - rich chat, the full sidebar (Chat, Inbox, Talk, Notes, Settings), health and sensors - as a native Swift 6 / SwiftUI app for iPhone and iPad. |
| **Kallisti-Eris** | branch `main` | The back. The Rust daemon: turns, tools, memory, MCP, approvals, sensors, inbox, notes, push, backups and device pairing. |
| **Kallisti-Eris Web UI** | branch `main`, `crates/eris-host/src/web` | The same daemon's dashboard: chat, sessions, settings, schedules, skills, approvals, backups and the onboarding wizard. Server-rendered HTML with one inlined stylesheet and one script, so it works with no build step and no JavaScript bundle. |

Eris is the backend; the phone and the dashboard are its two frontends. They are
the same product: a setting changed in the browser is the setting the phone reads
on its next load, because both read the daemon's own table rather than a copy.
The daemon keeps the state, the credentials, the sandbox and the tools. Neither
frontend ever holds your model keys.

### Repository layout

One repository, two branches, because the app and the daemon are one product with
two toolchains:

| Branch | Holds |
|---|---|
| `main` | the Rust workspace (daemon + web UI), `install.sh`, the release workflow |
| `restore/kallisti-identity-on-eris` | `Herald.xcodeproj` and the Swift sources for the iOS app |
| `nightly`, `release/*`, `backup/*` | historical and retained lines; not current |

Releases are tagged on `main` (`v<daemon version>-build.<n>`). App builds are
tagged `ios-v<app version>-build.<n>` on the app branch, because an app artifact is
a TestFlight build rather than a downloadable binary.

## What the iOS app does

Native surfaces, all first-class rather than wrapped in a web view:

- **The 0.4.0 UI, on Eris.** The app is the Kallisti 0.4.0 frontend unchanged -
  `AppRootView` -> the full `AdaptiveRootView` sidebar. Its `HeraldClientProtocol`
  ships as `ErisHeraldClient`, so those screens run over Eris as they are.
- **Rich chat** - streaming over Eris `/v1/turns` and SSE, markdown,
  syntax-highlighted code, and the model's reasoning beside the answer.
- **Health and sensors** - HealthKit, CoreLocation and CoreMotion, uploaded to the
  daemon's `sensors.db` and shown in the Senses view.
- **Inbox** - the daemon's inbox items and actions.
- **Notes** - local-first on device today; server sync is the next pass.
- **Camera and photos**, **handwriting and OCR** on iPad, **widgets and Live
  Activities**, **push notifications**.
- **Device pairing with opaque tokens.** The 0.4.0 onboarding posts the daemon
  address and a pairing code to `/v1/pair`; the pair lands in the keychain and the
  client restores it into chat.

**In development:** Voice Mode (Talk answers a typed in-development), the Plugin
Engine, and a native watchOS app. Labelled as such, because a roadmap presented as
a feature is a lie with good typography.

## What Kallisti-Eris does

One long-running process on your hardware, and a web UI in any browser when the phone
is not in your hand.

- **Streaming chat** with reasoning, usage accounting, and a run log of every call the
  turn executed - Full, Summary, or Off. A turn that spends its whole tool budget still
  answers: the last round is offered no tools and asked for prose, and if it still says
  nothing the turn reports the rounds it spent, every tool that ran with its count and
  the last result, so the next message continues the job instead of restarting it.
- **Questions, not guesses.** A turn that needs a decision, or a fact no tool can
  supply, asks for it and ends - one question, naming the options it would accept. The
  reply lands in the same transcript, so the next turn continues from the answer rather
  than starting over.
- **Tools with a boundary.** Read, write and list inside a workspace, run commands,
  fetch a URL, draw an image, and call any MCP server you configure.
- **MCP client.** Real stdio JSON-RPC: handshake, `tools/list`, `tools/call`, one owned
  child per server, a deadline on every request, children reaped on shutdown.
- **Approval gate.** Under policy `ask`, writes and MCP calls park until a person
  decides. An approval model can read the call and say what it would do - and that
  reading never decides anything.
- **Memory you choose.** gbrain (the default), a directory of markdown, or nothing.
- **Bring your own model.** A validated provider registry with a primary, and
  thirteen auxiliary roles that can each point at a different endpoint and model,
  including a fallback role that retries a failed turn on another model.
- **The senses.** Health and location device routes (`/v1/device/sensor/*`) into
  `sensors.db`.
- **Inbox, notes and push.** Device routes for inbox items and actions, notes (CRUD,
  blobs, recognitions, enrichment runs), and push token registration.
- **Schedules.** A prompt on a cron: a name, an expression and the question to ask,
  fired by the daemon itself. Every run is a real conversation titled after the
  schedule, with its own run history, pruned to the last fifty. A bad expression is
  refused when it is saved, not when it fires.
- **Skills.** A library of procedures at `~/.config/kallisti/skills`, one directory
  per skill holding a `SKILL.md`. The prompt carries names and descriptions only;
  the model calls `read_skill` to pull one in when the task fits. Each skill has its
  own switch.
- **Backups that cannot leak the key.** Three members: the database, the env file of
  references, the config. The setup passphrase is excluded unconditionally.

## Device API

The app talks to the daemon's own device API - there is no relay in between. Today:

`/v1/pair` · `/v1/token/refresh` · `/v1/turns` · `/v1/turns/{id}/events` ·
`/v1/turns/{id}/cancel` · `/v1/sessions/{id}` · `/v1/conversations` ·
`/v1/conversations/{id}` · `/v1/conversations/{id}/clear` ·
`/v1/approvals` · `/v1/settings` · `/v1/settings/full-access` · `/v1/devices` ·
`/v1/mcp` · `/v1/projects` · `/v1/persona` · `/v1/audit` · `/v1/version` ·
`/v1/backups` · `/v1/backups/{name}/restore` · `/v1/device/sensor/*` ·
`/v1/device/health/latest` · `/v1/device/location` · `/v1/inbox` · `/v1/notes` ·
`/v1/note-runs` · `/v1/push/*` · `/v1/talk/*` (placeholder)

Control plane, one screen per group: `/v1/models` · `/v1/cron` · `/v1/cron/{id}` ·
`/v1/skills` · `/v1/skills/{key}` · `/v1/aux`. Gateway control: `/v1/gw/health` ·
`/v1/gw/restart/preflight` · `/v1/gw/restart` · `/v1/gw/update/check` - every one of
them behind the bearer and the `admin` scope, and the only unit any of them can name
is this daemon's own.

A turn is created once per message. `POST /v1/turns` reads an `Idempotency-Key`
header, so a retry after a dropped connection is handed the job it already created
rather than queueing the same question twice.

**The legacy-parity surface.** A shipped client build still calls the retired relay's
paths. Each one is either answered from state the daemon genuinely holds, or refused
by name - never faked:

`/v1/health` · `/v1/config` · `/v1/config/validate` · `/v1/gw/status` · `/v1/gw/logs` ·
`/v1/gw/logs/stream` · `/v1/auth/refresh` · `/v1/auth/revoke` · `/v1/device/register` ·
`/v1/device/app-state` · `/v1/hosts/current` · `/v1/connector/events` · `/v1/profiles` ·
`/v1/commands` · `/v1/canvas/processes/stream` · and `/v1/hosts/current/revoke`, which
answers `403 host_revoke_unsupported` because this host is the only one.

Pairing codes expire, device tokens are opaque and stored as salted verifiers, and a
stolen state directory yields no usable credential.

A device turn streams more than its answer. The stream carries `delta`, `reasoning`,
`round` (which round is starting, and whether it may still call tools),
`tool_start` (the call about to run), `tool_output` (what a running command prints,
live and never journalled), `tool` (the finished call with its whole output), `asked`
(a question the turn is parked on), and the three terminal kinds. A client that knows
only `delta` still works; the vocabulary is advertised on `/v1/version` so a client
can learn it rather than pin a build number.

A transcript read is bounded by bytes as well as by message count: the newest
messages inside 512 KB, each tool output cut to an 8 KB preview that says how much it
left out, and `omitted` / `elided_outputs` on the wire. A conversation that ran a
hundred shell commands is not allowed to make a phone time out.

A few `HeraldClientProtocol` members are still typed-stubbed pending their device
routes (session delete/archive/pin/rename, title generation, job status, voice
transcript), and attachments are text-only. Cancel and interrupt are no longer on
that list: Stop reaches `/v1/turns/{id}/cancel`. That remainder is the current
backlog.

## Security model, enforced rather than asserted

- **Secrets are references.** Provider keys are stored as `env:NAME`; a literal key in
  that field is refused by name.
- **The sandbox decides.** `read-only` refuses every write at the tool.
  `workspace-write` confines writes to the workspace and refuses an escaping path by
  name, including a symlink. `full` runs commands, and only a restart with
  `--allow-full` can select it.
- **Fetching is guarded.** Private and loopback address literals are refused before a
  socket opens.

Known gap, written where the guard lives: `fetch_url` does not resolve a public
hostname that points at a private address.

## Architecture

| Crate | Holds |
|---|---|
| `core-domain` | ids, events, capabilities, provider types, errors. No I/O. |
| `core-runtime` | event log, prompt compilation, the provider seam, the turn machine. |
| `eris-host` | config, SQLite state, admin listener, tools, MCP, memory, onboarding, device API. |
| `ios-host` | on-device surface stub: a UniFFI export with no I/O of its own, so the core never imports a device framework and every permission prompt belongs to the Swift shell. |

```sh
cargo test --workspace        # 1159 tests across the unit and integration suites, 0 failures
cargo clippy --workspace --all-targets
cargo fmt --check
```

The binary is thin and everything testable is in the library, so the acceptance tests
drive the same router the process serves.

## Install (one line)

Fresh Linux box, no root, no compiler:

```sh
curl -fsSL https://raw.githubusercontent.com/fireishott/Kallisti/main/install.sh | sh
```

That is the whole install. What it does:

- Installs a **prebuilt binary** from the newest release, verifying the published
  `.sha256`. A release carries whatever architectures were built for it: this one
  carries `linux/x86_64`.
- Falls back to **building from source** on the pinned toolchain when there is no
  asset for the machine, which is how an `aarch64` box installs today. It installs
  `rustup` itself if Rust is absent, and tells you the one `apt` line if the C
  toolchain is missing.
- Puts the binary at `~/.local/opt/kallisti/bin/eris`, symlinked to
  `~/.local/bin/eris`.
- Creates `~/.config/kallisti` (`eris.env` and `keys.env`, both mode 0600),
  `~/.local/share/kallisti` and `~/kallisti-workspace`.
- Writes and starts a systemd **user** unit, `kallisti-eris.service`, and tells you
  if `Linger` is off.

Then two edits and you are in:

```sh
$EDITOR ~/.config/kallisti/eris.env    # base URL and model
$EDITOR ~/.config/kallisti/keys.env    # the key the reference names
```

Open `http://127.0.0.1:7340`, claim the wizard with the six-word passphrase the
daemon printed on first start (`~/.config/kallisti/first-run.txt`).

| Flag | What it changes |
|---|---|
| `--from-source` | Skip the prebuilt binary and build |
| `--version <tag>` | Install a specific release instead of the newest |
| `--prefix <dir>` | Where the binary lives (default `~/.local/opt/kallisti`) |
| `--bin-dir <dir>` | Where the symlink lives (default `~/.local/bin`) |
| `--no-service` | Do not write or start the systemd unit |
| `--dry-run` | Print the plan, change nothing |

Read it before you pipe it to `sh` - it is 311 lines and nothing in it needs root:

```sh
curl -fsSL https://raw.githubusercontent.com/fireishott/Kallisti/main/install.sh | less
```

## Build from source

```sh
git clone https://github.com/fireishott/Kallisti.git && cd Kallisti
cargo build --release --locked
./target/release/eris serve
```

The toolchain is pinned in `rust-toolchain.toml` (1.99.1); plain `cargo` picks it
up if `rustup` is installed. First boot generates the six-word passphrase once,
prints it, and writes it to `~/.config/kallisti/first-run.txt` at mode 0600. The
dashboard is at `http://127.0.0.1:7340`; the device listener is loopback-only on
`:7341` and off until the dashboard switch or `--device-listen` turns it on.

<details>
<summary>Install <code>gh</code> (GitHub CLI) - for tags, releases and PRs</summary>

### Install `gh` (GitHub CLI)

Releases, tags and PRs are one command with `gh`. Linux, no root, no keyring, one
line:

```sh
mkdir -p ~/.local/bin && v=$(curl -sI https://github.com/cli/cli/releases/latest | tr -d '\r' | sed -n 's#^[Ll]ocation: .*/tag/v##p') && curl -fsSL "https://github.com/cli/cli/releases/download/v$v/gh_${v}_linux_amd64.tar.gz" | tar xz -C ~/.local --strip-components=1 && ~/.local/bin/gh --version
```

On an arm64 box change `linux_amd64` to `linux_arm64`; that is the only difference.

macOS, Homebrew keeps it updated: `brew install gh`.
Windows: `winget install --id GitHub.cli`.
Debian or Ubuntu with root, from GitHub's own repo (not `universe`):

```sh
(type -p wget >/dev/null || (sudo apt update && sudo apt install wget -y)) \
  && sudo mkdir -p -m 755 /etc/apt/keyrings \
  && out=$(mktemp) && wget -nv -O$out https://cli.github.com/packages/githubcli-archive-keyring.gpg \
  && cat $out | sudo tee /etc/apt/keyrings/githubcli-archive-keyring.gpg > /dev/null \
  && sudo chmod go+r /etc/apt/keyrings/githubcli-archive-keyring.gpg \
  && echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" | sudo tee /etc/apt/sources.list.d/github-cli.list > /dev/null \
  && sudo apt update && sudo apt install gh -y
```

If `~/.local/bin` is not on your `PATH`, either add it or drop the binary
system-wide: `sudo install -m 755 ~/.local/bin/gh /usr/local/bin/gh`.

Then authenticate. Interactively is one command, and it works over SSH without a
browser (it prints a one-time code to paste into any browser you do have):

```sh
gh auth login
```

Non-interactively, with a token that carries `repo` scope - the form a script or
an agent needs:

```sh
gh auth login --with-token <<< "$GITHUB_TOKEN"
```

Check it with `gh auth status`; it should name the account and the scopes.

Two traps worth knowing. Do not install the distro's own `gh` from `universe` -
the 2.45/2.46 builds are broken against current GitHub APIs, which is why the apt
block points at GitHub's own repo. And on a headless box,
`gh auth login --with-token` can block on a keyring that is not there; if it hangs,
add `--insecure-storage`, or set `GH_TOKEN` and skip the login step entirely.

</details>

## Status

Shipped and running: **Kallisti-Eris 1.4.11, build 123**, with the cron scheduler and
the skills library switched on. The web client is server-rendered HTML with an inlined
stylesheet and one script, so a transcript renders on a browser with no scripting and a
turn that is already running can be reattached to.

The last eight builds are mostly the transcript. A conversation is a fixed box the size
of the viewport with the transcript as its only scroller, so the composer cannot end up
in the middle of a document. A message is a flex item that cannot shrink, so a folded
answer is always the 320px `--fold` block it says it is, it scrolls to its own last line,
and the fade that says "more below" stops saying it once there is no more. A reply opened
in the whole window sits above the composer and the rail, and its controls travel with it.
A tool call survives a gateway that re-sends the whole `arguments` object instead of only
the new fragment, which used to fail the turn at the parser before any tool ran.

In private beta: the Kallisti iOS app on the Eris daemon, TestFlight **1.4.14 (109)**.
The app and the daemon are separate version lines - the daemon's build number moves
when the daemon ships, the app's when the app does - so a report of a problem should
name both, and `GET /v1/version` is where the daemon's answer comes from.

What the phone does with the daemon, as of this pair: chat with reasoning and a live
run log (the round, the command running, its stdout as it arrives), conversations that
load and stay loadable, Stop that stops the turn on the daemon rather than only on the
screen, and Settings that renders the daemon's own settings table - all six of its
tabs - with the full-host switch and the backups screen behind their guards.

In development: Voice Mode, Plugin Engine, watchOS.

## Licence

MIT. See [`LICENSE`](LICENSE).

Kallisti is open source and stays that way. Every build this project ships is built
from the source in this repository, and anyone can clone it and compile the daemon
themselves - that path is documented above, it needs only a Rust toolchain, and it
will keep working. The `main` branch is the binary's source of truth, not a mirror
of it. Design constraints from before the first line of code live in
[`docs/amendments.md`](docs/amendments.md), decisions in [`docs/adr/`](docs/adr),
changes in [`CHANGELOG.md`](CHANGELOG.md), and the app in its own branch.

Contributions and forks are welcome. If you build something with it, the licence
already says everything you need.
