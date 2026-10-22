# Kallisti

> **To the Most Beautiful.** The most beautiful way to meet your agent.

Kallisti is a complete package built from two halves. **Kallisti for iOS** is the
frontend - a native Swift 6 / SwiftUI app for iPhone and iPad. **Kallisti-Eris** is
the backend - a Rust daemon that holds the brain, the tools and the memory, on
hardware you own. The app talks to the daemon over its device API; there is no
third-party relay, and nothing leaves your infrastructure.

**Private TestFlight beta in progress.** MIT licensed.

---

## The two halves

| Half | What it is |
|---|---|
| **Kallisti for iOS** | The front. The literal Kallisti 0.4.0 UI - rich chat, the full sidebar (Chat, Inbox, Talk, Notes, Settings), health and sensors - as a native Swift 6 / SwiftUI app for iPhone and iPad. |
| **Kallisti-Eris** | The back. A Rust daemon and a web UI on your hardware: turns, tools, memory, MCP, approvals, sensors, inbox, notes, push and device pairing. |

Eris is the backend; Kallisti is the frontend. The daemon keeps the state, the
credentials, the sandbox and the tools. The phone never holds your model keys.

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
`/v1/approvals` · `/v1/settings` · `/v1/devices` · `/v1/mcp` · `/v1/projects` ·
`/v1/persona` · `/v1/audit` · `/v1/version` · `/v1/device/sensor/*` ·
`/v1/device/health/latest` · `/v1/device/location` · `/v1/inbox` · `/v1/notes` ·
`/v1/note-runs` · `/v1/push/*` · `/v1/talk/*` (placeholder)

Pairing codes expire, device tokens are opaque and stored as salted verifiers, and a
stolen state directory yields no usable credential.

A device turn streams text and reasoning. A few `HeraldClientProtocol` members are
still typed-stubbed pending their device routes (session delete/archive/pin/rename,
title generation, job status, cancel/interrupt, voice transcript, note sync), and
attachments are text-only. That list is the current backlog.

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
cargo test --workspace        # 515 tests
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

- Installs a **prebuilt binary** for `linux/x86_64` or `linux/aarch64` from the
  newest release, verifying the published `.sha256`.
- Falls back to **building from source** on the pinned toolchain when there is no
  asset for the machine. It installs `rustup` itself if Rust is absent, and tells
  you the one `apt` line if the C toolchain is missing.
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

## Install `gh` (GitHub CLI)

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

## Status

Shipped and running: Kallisti-Eris 1.4.2 and the web UI, with the cron scheduler and
the skills library switched on.
In private beta: the iOS app (Kallisti 0.4.0 UI on Eris), TestFlight build 89.
In development: Voice Mode, Plugin Engine, watchOS.

## Licence

MIT. See [`LICENSE`](LICENSE). Design constraints from before the first line of code
live in [`docs/amendments.md`](docs/amendments.md), decisions in [`docs/adr/`](docs/adr),
changes in [`CHANGELOG.md`](CHANGELOG.md).
