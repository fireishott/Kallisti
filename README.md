# Kallisti 1.0

> **It grows.** The assistant at month six is measurably better for *this* customer than on day one, and that claim is provable instead of asserted.
>
> **It has hands.** It initiates work proactively, and it earns the right to act without asking by being accurate, not by being configured.

## Locked amendments

These override the original plan where they conflict.

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
| `crates/eris-host` | The Full daemon. Wave 0 stub binary that prints a version line. |
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

## Status

Wave 0 is a skeleton, not a working assistant. There is no daemon, no listener,
no provider call, no database, and no generated bindings yet. The wave-by-wave
plan is `~/vault/projects/kallisti-1.0-deployment-plan.md`.
