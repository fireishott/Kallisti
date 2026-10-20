# Locked amendments

Design constraints agreed before any code was written. They override the original
plan where they conflict, and they remain the record of why several things are
shaped the way they are. Some name features that a later version has since
shipped; the constraint still applies to how it was built.

Moved here from the README, which now describes the product rather than the plan.

1. **No backward-compat is not "full parity".**
   Cutover uses a signed capability matrix: `ship`, `retire`, or
   `defer`. 1.0 does not import legacy sessions. Export, retention,
   and rollback are a later ADR, not a silent assumption.

2. **Lite airplane-mode cannot call a cloud model.**
   Gate A: in-process fake Provider, no listener, no network.
   Gate B: real BYO provider, network on, Keychain key. Separate.

3. **"No secret on disk" is false.**
   Real rule: no plaintext credential, bearer, or pairing secret in
   source, config, or logs. Server stores salted verifiers. Client
   tokens live in the OS secret store. A stolen state dir must not
   yield a usable bearer. Design this before any auth code.

4. **ExternalComm and Financial never transmit in 1.0.**
   They produce a review artifact only. Approval binds a canonical
   action hash, capability, expiry, and preconditions. Replay of an
   old approval against a changed draft fails. "Every action is
   revocable" is dropped. 1.0 prohibits irreversible side effects.

5. **Lessons are not `{trigger, guidance}` strings.**
   A lesson cannot become Active from reflection text, a single
   acceptance, or an ignore. Ignore is unknown. Product-wide learning
   is out of 1.0. User-scoped candidates only, after a later gate.

6. **Cache blame is not a provider verdict.**
   A local block diff names the first changed block. It does not prove
   the provider missed, and it does not invent a dollar delta when
   usage or pricing is missing. Verdicts are structured:
   `confirmed_hit`, `confirmed_miss`, `prefix_changed`,
   `eligible_unobserved`, `below_floor`, `provider_unavailable`.

7. **Do not append one growing `.jsonl.zst`.**
   Sealed immutable segments plus a short uncompressed WAL. Recovery
   never rewrites a committed segment. Torn WAL tail is truncated at
   the last valid envelope and recorded as a new event.

8. **JSON Schema is not automatically three languages.**
   Spike both. Prefer Rust wire types with `serde(tag = "type")`,
   Typeshare for TS and Swift, Schemars for schema, and golden
   fixtures. Typify plus Draft 2020-12 is the fallback only if the
   spike proves unions round-trip. No quicktype.

9. **Feature flags are not a Lite security boundary.**
   Split crates: `core-domain`, `core-runtime`, `eris-host`,
   `ios-host`. CI checks the Lite dependency graph, not a comment.

10. **Third-party in-process plugins are out of 1.0.**
    First-party modules may be static. Everything else is a later
    sidecar with a capability RPC. Do not design the host yet.

11. **Minimum proactive proof, later, not now:**
    detect a local follow-up, write a draft, push a notification,
    require one-tap approval, write an immutable ledger row.
    No send, no pay, no delete, no shell.

12. **Prompt block order, pending ADR, default:**
    persona, immutable product policy, tool schemas, skill catalog,
    workspace, session-pinned lessons, volatile tail.
    Lessons do not sit in front of tools. Tool order is frozen.

13. **Device skills are Full-only, and the core never imports a
    device framework.** The Rust core takes a `HostCapabilities` value
    and a `DeviceContext` trait. No crate in `core-domain` or
    `core-runtime` may reference HealthKit, CoreLocation, CoreMotion,
    CoreBluetooth, CoreNFC, or EventKit. The iOS shell owns every
    permission prompt. Lite compiles with zero capabilities and is not
    a degraded build, it is the same core with no device host.

14. **No silent sensor upload.** Each device channel is a named
    capability the customer can see and switch off. A sample carries a
    purpose tag. Uploads go through a durable outbox with bounded
    retry, and a revoke purges the outbox. Health, location, and motion
    payloads are never written into the session event log in plaintext;
    the log stores a redacted reference plus channel and count.

15. **Brand is Kallisti. Herald is a retired internal name.**
    Any user-visible string, notification body, error message, HTTP
    header name, Xcode target, module name, or logger subsystem that
    says Herald is a 1.0 defect. Fix it in the new tree. Do not rename
    the shipped pre-1.0 tree.

## Repository layout at Wave 0

| Crate | Role |
|---|---|
| `crates/core-domain` | Types only: ids, events, capability enum, `DeviceSnapshot`, `DeviceContext`. No I/O, no HTTP, no async runtime. |
| `crates/core-runtime` | Runtime seams: `log`, `prompt`, `provider`, `turn`. Skeleton modules in Wave 0. |
| `crates/eris-host` | The Full daemon. P5: config loader (`eris.env` + `config.toml`), SQLite state with embedded migrations, the loopback admin listener on `127.0.0.1:7340`, the typed error body, and `doctor`. |
| `crates/ios-host` | The Lite on-device surface. UniFFI 0.32.2 proc-macro surface: one exported function, `run_fake_turn`, backed by an in-process fake provider. Device half unproven; see `docs/adr/0003-lite-spike.md`. |

Split crates rather than feature flags, because a feature flag is not a Lite
security boundary (amendment 9). `ios-host` depends on `core-domain` and `uniffi`
and nothing else; that dependency graph is the boundary, and both a source-scan
test and a CI grep enforce it.

## Build

Rust 1.99.1, pinned by `rust-toolchain.toml`. The toolchain is installed with
`--no-modify-path`, so shell rc files are untouched and `cargo` is added to
`PATH` per shell:

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

## Running at P7

One binary, installed at `~/.local/opt/kallisti/bin/eris`:

```bash
eris                  # `serve` is the default subcommand
eris serve            # admin listener on 127.0.0.1:7340, loopback only
eris serve --allow-full          # permit `full` sandbox mode to be stored at all
eris serve --state-dir /tmp/p7   # scratch root: config/ and state/ under it
eris setup            # print onboarding progress and the wizard URL
eris setup --reset    # reopen the wizard; the admin passphrase is read from stdin
eris probe "hi"       # one real turn through the configured provider
eris doctor           # JSON report: --json for one machine-readable object
eris device           # placeholder, exits 2 (device listener is P8)
eris eval             # placeholder, exits 2
eris version
```

State and config live outside the repo:

| Path | Mode | Contents |
|---|---|---|
| `~/.config/kallisti/eris.env` | `0600` (enforced) | provider base URL, model, and a key **reference** (`env:NINEROUTER_API_KEY`) |
| `~/.config/kallisti/config.toml` | none | `workspace_path`, `log_level`. Never a secret, never a bind address. |
| `~/.config/kallisti/first-run.txt` | `0600` (enforced) | the six-word setup passphrase, written once |
| `~/.local/share/kallisti/eris.db` | dir `0700` | SQLite, WAL, migrations in `schema_migrations` |
| `~/kallisti-workspace` | `0755` | default sandbox workspace, created on demand |

The admin address is a `const` in `crates/eris-host/src/admin.rs`. It is not a
config key and not a flag: `config.toml` rejects any listen/bind key by name, and
`eris serve` refuses to start when `eris.env` is group- or world-readable. The
device port `7341` is not bound by this build.

### First run

On a database with no admin credential, `eris serve` generates a six-word
passphrase from the embedded EFF short diceware list, writes it to
`~/.config/kallisti/first-run.txt` (mode `0600`), and prints one block with the
SSH tunnel command and `http://127.0.0.1:7340/setup`. It is generated once and
never printed again after onboarding completes. `eris setup --reset` is the
recovery path and it requires the existing admin passphrase.

The claim stores that passphrase as the admin credential (Argon2id verifier
only), which is what a later `--reset` verifies.

### The wizard

Server-side state: one `settings` row per step with a `completed_at`, so a
refresh, a closed tab, or a restarted daemon never loses progress.

| Route | What it does |
|---|---|
| `GET /` | 302 to `/setup` or `/dashboard`, by state |
| `GET /setup` | the wizard, at the step the rows say |
| `POST /setup/claim` | verify the setup passphrase, mint the admin session cookie |
| `POST /setup/provider` | probe `GET {base_url}/models`, store the endpoint and the key reference |
| `POST /setup/models` | pick the default model from the probe's list |
| `POST /setup/permissions` | sandbox mode, approval policy, autopilot ceiling, telemetry |
| `POST /setup/workspace` | workspace path; created `0755` when absent |
| `POST /setup/device` | optional: mint one pairing code |
| `POST /setup/finish` | lock the wizard, write the audit rows, state becomes `ready` |
| `GET /setup/state` | JSON progress (session required) |
| `GET /dashboard` | health, provider, devices, kill switches (`409 not_ready` before setup finishes) |
| `POST /dashboard/provider/rotate` | re-read the key reference from the environment and re-probe |
| `POST /dashboard/device/revoke` | revoke a device and its tokens |
| `POST /dashboard/mode` | the kill switches |

Form field names, for a `curl` walk:

```
claim        passphrase
provider     base_url, api_key_ref          (the reference, never a value)
models       model
permissions  sandbox_mode, approval_policy, autopilot_ceiling, telemetry
workspace    workspace_path
device       device_name                    (optional)
mode         sandbox_mode, approval_policy, autopilot_ceiling, telemetry,
             device_listener_enabled
```

Every POST answers `{"ok":true,...}` or `{"ok":false,"field","code","message"}`,
and a failed POST is never a redirect. A request whose `Accept` header names
`text/html` gets the same validation as a re-rendered page plus a 303 on success,
so the browser flow and the `curl` flow share one code path.

```bash
curl -s localhost:7340/healthz
# {"status":"ok","version":"1.0.0","state":"setup","db":"ok"}

curl -si localhost:7340/dashboard | head -1
# HTTP/1.1 409 Conflict   (code: not_ready)

curl -c jar -b jar -d 'passphrase=word word word word word word' localhost:7340/setup/claim
```

Five claim attempts per ten minutes, then `429 rate_limited`. Any request
carrying `X-Forwarded-For`, `Forwarded` or a `Tailscale-*` header is
`403 not_loopback`, so a future Serve mapping cannot expose the wizard.

### Permissions

| Setting | Default | Options | Rule |
|---|---|---|---|
| Sandbox mode | `read-only` | `read-only`, `workspace-write` | `full` needs `eris serve --allow-full` at start |
| Approval policy | `ask` | `ask`, `never` | `never` needs a paired device and a finished setup |
| Autopilot ceiling | `L1 draft` | `L0 notify`, `L1 draft` | `L2` and `L3` do not exist in 1.0 |
| External communication | `draft only` | locked | 1.0 never transmits in your voice |
| Financial | `disabled` | locked | no financial action exists in 1.0 |
| Workspace | `~/kallisti-workspace` | any directory | refused if it overlaps `~/.dsh`, `~/.config`, `~/.ssh`, `~/vault` |
| Network | `loopback only` | locked | the device listener stays off until enabled on the dashboard |
| Telemetry | `off` | `off`, `local only` | never a cloud sink |

Every row carries a one-line plain-English consequence. The palette is closed and
listed in `docs/adr/0004-palette.md`, which is also the purple denylist
reference: no purple appears in the rendered HTML, and a test enforces it.

## Status

Wave 0 skeleton plus P1 through P7. What runs today: the provider seam returns a
real completion through 9router, the session event log is green, the auth core
stores verifiers and nothing reversible, and the daemon serves the onboarding
wizard and the dashboard on `127.0.0.1:7340` from a migrated SQLite database.
Not built yet: the device listener on `7341` (P8) and the
`kallisti-eris.service` unit (P9). The wave-by-wave plan is
`~/vault/projects/kallisti-1.0-deployment-plan.md`.
