# Kallisti

A self-hosted assistant daemon. One long-running process that holds a conversation,
runs tools on the machine it lives on, and keeps what it knows in local files and a
local database.

No cloud control plane, no account, no telemetry sink. The only network calls are
the ones a turn makes to the model endpoint you configure.

**Version 1.4.0.** Design constraints from before the first line of code live in
[`docs/amendments.md`](docs/amendments.md). Architecture decisions live in
[`docs/adr/`](docs/adr).

## What it does

- **Conversation** with streaming replies, the model's reasoning shown beside the
  answer when the provider sends it, and a run log of everything the turn executed.
- **Tools** the model can call: read, write and list inside a workspace, run
  commands, fetch a URL, and draw an image.
- **MCP client** that speaks JSON-RPC over stdio to Model Context Protocol servers
  you configure, offers their tools to the model as `server__tool`, and takes them
  down with the daemon.
- **Providers**: more than one endpoint can be configured. One is primary; each
  auxiliary role may point at any of them.
- **Auxiliary roles**: twelve named jobs beside the main answer - vision, image
  generation, web extraction, context compression, conversation titles, approval
  review and others. A role left on `primary` does nothing and costs nothing;
  pinning a model is what switches it on.
- **Approval gate**: under policy `ask`, writes and MCP calls park in a queue until
  a person decides. A pinned approval role adds a one-line reading of the call, and
  that reading never decides anything.

## Architecture

Four crates. The seams are the point.

| Crate | Holds |
|---|---|
| `core-domain` | ids, events, capabilities, provider types, errors. No I/O. |
| `core-runtime` | event log, prompt compilation, the provider seam, the turn machine. |
| `eris-host` | config, SQLite state, the admin listener, tools, MCP, onboarding. |
| `ios-host` | on-device surface stub. One exported function. |

`eris-host` is where the daemon lives. The binary is thin; everything testable sits
in the library so the acceptance tests drive the same router the process serves.

## Security model

Enforced in code, not asserted in a settings page.

- **Secrets are references.** A provider's key is stored as `env:NAME`; a literal
  key value in that field is refused by name. Tokens, pairing codes and passphrases
  are stored only as salted verifiers, so a stolen state directory yields no usable
  credential.
- **The sandbox decides.** `read-only` refuses every write at the tool.
  `workspace-write` confines writes to the workspace and refuses an escaping path
  by name, including a symlink that points outside it.
- **The approval gate is a boundary.** Writes and MCP calls park under `ask`.
  Approving one rewrites that call's record in the transcript with what actually
  ran, so a decided row never still reads as pending.
- **Fetching is guarded.** `fetch_url` refuses loopback, link-local and private
  address literals before a socket is opened.

Known gap, stated rather than papered over: the fetch guard does not resolve a
public hostname that points at a private address. That needs resolution-time
checking, which is not implemented.

## Running it

```sh
cargo build --release
./target/release/eris serve
```

First boot generates a six-word passphrase once, prints it, and writes it to
`~/.config/kallisti/first-run.txt` at mode 0600. Onboarding then walks through
provider, models, permissions, workspace and the admin listener. The dashboard
lives at `http://127.0.0.1:7340` by default.

Configuration is two optional files:

- `config.toml` - non-secret settings: listeners, trusted proxies, persona.
- `eris.env` - secret **references** only, at mode 0600.

Everything else lives in `~/.local/share/kallisti/eris.db`, SQLite in WAL mode with
forward-only migrations.

## Testing

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --check
```

The suite covers the protocol seam, the turn loop, the sandbox, approvals, MCP
framing and the HTTP surface. The MCP client is exercised against a shell fixture
that speaks JSON-RPC, so its framing is real rather than mocked.

## Not in this build

Named so that no surface has to pretend:

- No iOS or macOS client ships. `ios-host` is a stub.
- Five auxiliary roles - skills hub, triage specifier, kanban decomposer, profile
  describer, curator - have no feature behind them yet. Their settings rows say so
  rather than looking wired.
- Command execution needs `full` sandbox mode, which only a restart with
  `--allow-full` can select.
- No telemetry, and no cloud sink exists to enable.

## Licence

No licence file is present yet. Treat this as all rights reserved until one is
added.
