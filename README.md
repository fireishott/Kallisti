# Kallisti

**A client host for your own assistant.** One daemon on hardware you own, one
interface wherever you are, and a model endpoint you choose. No account, no cloud
control plane, no telemetry sink.

Two ways to drive it: **Lite**, the web UI in any browser, shipping today. **Pro**,
the Kallisti iOS suite, currently a stub crate and the long-term play after beta.

`eris 1.4.0` · Rust · MIT

---

## The shape of it

Kallisti is a **client host**, not a chat wrapper. The daemon holds the state, the
credentials, the sandbox and the tools. Clients are replaceable views onto it.

| | **Lite** | **Pro** |
|---|---|---|
| **Interface** | Web UI, any browser | Native iOS suite |
| **Where it runs** | The host, loopback or tailnet | On-device, against the host |
| **Status** | **Shipping** in 1.4.0 | Crate stub; after beta |
| **Needs** | A browser | A paired device |

Lite is the whole product today and it is not a demo: streaming chat, tools, MCP,
approvals, memory and backups all work through it.

Pro is not "Lite in an app". Pro's point is that **the phone becomes a capability
provider to the daemon**: the assistant keeps its brain, its tools and its memory on
the host, and gains the senses and surfaces of the device it is paired with. The
`ios-host` crate is the seam - a UniFFI surface with no I/O of its own, so the core
never imports a device framework and every permission prompt belongs to the iOS
shell, not to a Rust dependency.

Capabilities the suite is built around: health and sensors, camera and photos,
handwriting and OCR on iPad, voice, push notifications, widgets and Live Activities,
and gateway control. Each one is a device-side capability the host can call, which
is why Pro is a suite rather than a view.

## What it does

- **Streaming chat** with the model's reasoning shown beside the answer, and a run
  log of every call the turn executed.
- **Tools with a boundary.** Read, write and list inside a workspace, run commands,
  fetch a URL, draw an image, and call any MCP server you configure.
- **MCP client.** Real stdio JSON-RPC: `initialize`, `tools/list`, `tools/call`,
  one owned child process per server, a deadline on every request, tools offered to
  the model as `server__tool`, children reaped on shutdown.
- **More than one provider.** A validated registry with a primary; every auxiliary
  role can point at any endpoint on it.
- **Twelve auxiliary roles.** Vision, image generation, web extraction, context
  compression, conversation titles, approval review, and more. A role left on
  `primary` costs nothing - pinning a model is what switches it on.
- **Memory that is yours.** Before answering, the turn recalls from gbrain (the
  default), a directory of markdown, or nothing at all.
- **An approval gate that is a boundary.** With policy `ask`, writes and MCP calls
  park until a person decides. A pinned approval role adds a one-line reading of the
  call. The reading never decides anything.
- **A run log with three modes.** Full, Summary, or Off - and off means the output is
  never streamed to the browser at all.
- **Timezones, personas, wallpapers.** Because a daily driver should not feel like a
  control panel.

## The security model is enforced, not asserted

- **Secrets are references.** Provider keys are stored as `env:NAME`; a literal key
  in that field is refused by name. Tokens, pairing codes and passphrases are stored
  only as salted verifiers. A stolen state directory yields no usable credential.
- **The sandbox decides.** `read-only` refuses every write at the tool.
  `workspace-write` confines writes to the workspace and refuses an escaping path by
  name, including a symlink that points outside it. `full` runs commands, and only a
  restart with `--allow-full` can select it.
- **Fetching is guarded.** `fetch_url` refuses loopback, link-local and private
  address literals before a socket is opened.
- **Backups cannot leak the key.** An archive is exactly three members - the
  database, the env file of references, the config. The setup passphrase is excluded
  unconditionally, and the workspace is opt-in.
- **A dropped connection is not a failed turn.** The daemon finishes the work and
  writes the answer; the page says so instead of quoting a browser error.

Known gap, stated where the guard lives: `fetch_url` does not resolve a public
hostname that points at a private address. That needs resolution-time checking.

## Architecture

Four crates, and the seams are the point:

| Crate | Holds |
|---|---|
| `core-domain` | ids, events, capabilities, provider types, errors. No I/O. |
| `core-runtime` | event log, prompt compilation, the provider seam, the turn machine. |
| `eris-host` | config, SQLite state, admin listener, tools, MCP, memory, onboarding. |
| `ios-host` | on-device surface stub. UniFFI export, zero I/O by design. |

The binary is thin. Everything testable lives in the library, so the acceptance
tests drive the same router the process serves - the tests are not a mock of the
product, they are the product.

```sh
cargo test --workspace        # 496 tests
cargo clippy --workspace --all-targets
cargo fmt --check
```

MCP is exercised against a shell fixture that speaks JSON-RPC, so the framing is
real rather than mocked.

## Run it

```sh
cargo build --release
./target/release/eris serve
```

First boot generates a six-word passphrase once, prints it, and writes it to
`~/.config/kallisti/first-run.txt` at mode 0600. Onboarding then walks through
provider, models, permissions, workspace and the admin listener. The dashboard is at
`http://127.0.0.1:7340`.

Two optional config files: `config.toml` for non-secret settings, `eris.env` for
secret **references** only. Everything else is
`~/.local/share/kallisti/eris.db` - SQLite in WAL mode, forward-only migrations.

## Roadmap

- **Beta: Lite alone.** Harden the daemon, the tool boundary and the approval gate.
- **After beta: Pro.** The iOS suite on the `ios-host` seam: pairing, a first-party
  chat surface, and the device capabilities a browser cannot reach - health and
  sensors, camera and photos, OCR, voice, notifications, widgets, Live Activities
  and gateway control. The long-term play is not a prettier client; it is the
  assistant gaining the phone's senses while its brain stays on hardware you own.

Six of the twelve auxiliary roles are not wired yet, and the reason differs. Five -
skills hub, triage specifier, kanban decomposer, profile describer, curator - have
no feature behind them at all. The sixth, an `mcp` reasoning pass, waits on a
feature rather than being an oversight. Every settings row states which case it is
instead of looking wired.

## Licence

MIT. See [`LICENSE`](LICENSE). Use it, change it, ship it, sell it, as long as the
notice travels with it and the software comes with no warranty.

Design constraints from before the first line of code live in
[`docs/amendments.md`](docs/amendments.md). Architecture decisions live in
[`docs/adr/`](docs/adr). Changes are in [`CHANGELOG.md`](CHANGELOG.md).
