# Kallisti

> **To the Most Beautiful.** The most beautiful way to meet your agent.

Kallisti is a complete package, not a client and not a daemon. A native **iPhone and
iPad app**, a **relay** that connects it to your hardware, and **Kallisti-Eris**, the
daemon that holds the brain, the tools and the memory. Nothing leaves your
infrastructure, and there is no third party to leave it to.

**Private TestFlight beta in progress.** MIT licensed.

---

## The package

| Part | What it is |
|---|---|
| **Kallisti for iOS** | The senses. A native Swift 6 / SwiftUI app for iPhone, iPad and Apple Watch. |
| **Kallisti Relay** | The connector. Carries paired-device traffic between the app and your daemon. |
| **Kallisti-Eris** | The brain. A Rust daemon and a web UI, on hardware you own. |

The app extends the agent with what only a phone has. The daemon keeps the state, the
credentials, the sandbox and the tools. The relay joins them, and the phone never
holds your model keys.

## What the iOS app does

Native surfaces, all first-class rather than wrapped in a web view:

- **Rich chat** - streaming, markdown, syntax-highlighted code, and the model's
  reasoning beside the answer.
- **Health and sensors** - HealthKit, CoreLocation and CoreMotion, so the agent can
  answer with what the device already knows.
- **Camera and photos** - capture, attach, and have the image described or acted on.
- **Handwriting and OCR** - iPad, on-device text recognition.
- **Widgets and Live Activities** - a turn in progress, visible without opening the app.
- **Push notifications** - the agent reaching you, not just answering you.
- **Session management** - conversations across devices.
- **Gateway control** - start, stop and steer the backend from the phone.
- **Security and privacy** - device pairing with opaque tokens, salted verifiers, and
  no password on disk.

**In development:** Voice Mode, the Plugin Engine, and a native watchOS app. Labelled
as such on the site and in the app, because a roadmap presented as a feature is a lie
with good typography.

## What Kallisti-Eris does

One long-running process on your hardware, and a web UI in any browser when the phone
is not in your hand.

- **Streaming chat** with reasoning, usage accounting, and a run log of every call the
  turn executed - Full, Summary, or Off.
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
- **Backups that cannot leak the key.** Three members: the database, the env file of
  references, the config. The setup passphrase is excluded unconditionally.

## Relay ready

The daemon already speaks to paired devices over its own API, which is what the relay
carries. Today:

`/v1/pair` · `/v1/token/refresh` · `/v1/turns` · `/v1/turns/{id}/events` ·
`/v1/turns/{id}/cancel` · `/v1/sessions/{id}` · `/v1/conversations` ·
`/v1/approvals` · `/v1/settings` · `/v1/backups` · `/v1/devices` · `/v1/mcp` ·
`/v1/projects` · `/v1/persona` · `/v1/audit` · `/v1/version`

Pairing codes expire, device tokens are opaque and stored as salted verifiers, and a
stolen state directory yields no usable credential.

What a paired device gets today is narrower than what a browser gets, and it is
worth being exact about it: a device turn streams text only. The reasoning, the tool
loop and the run log that the browser shows are not forwarded - a device turn is a
bare provider call, and a device that sends `/stop` spends tokens doing it. The full
gap, capability by capability, is graded in
[`docs/relay-readiness.md`](docs/relay-readiness.md): five of thirty-six capabilities
exposed, and what each missing one would need.

Not yet exposed to a device: settings, approvals, tool configuration and memory
choice. Those are the control surfaces a relay needs before "full control" is a
truthful phrase, and they are the next thing to build.

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
cargo test --workspace        # 497 tests
cargo clippy --workspace --all-targets
cargo fmt --check
```

The binary is thin and everything testable is in the library, so the acceptance tests
drive the same router the process serves.

## Run the daemon

```sh
cargo build --release
./target/release/eris serve
```

First boot generates a six-word passphrase once, prints it, and writes it to
`~/.config/kallisti/first-run.txt` at mode 0600. The dashboard is at
`http://127.0.0.1:7340`; the device listener is loopback-only on `:7341`.

## Status

Shipped and running: Kallisti-Eris 1.4.0 and the web UI.
In private beta: the iOS app, TestFlight in progress.
In development: Voice Mode, Plugin Engine, watchOS.

## Licence

MIT. See [`LICENSE`](LICENSE). Design constraints from before the first line of code
live in [`docs/amendments.md`](docs/amendments.md), decisions in [`docs/adr/`](docs/adr),
changes in [`CHANGELOG.md`](CHANGELOG.md).
