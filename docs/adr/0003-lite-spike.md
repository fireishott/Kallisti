# ADR 0003 - Lite spike: UniFFI 0.32.2 proc-macro surface, fake provider, no network

- Status: **host half implemented and green; device half UNPROVEN**
- Date: 2026-10-19
- Wave: 2 (P3), host half only
- Related: locked amendments 2 (no listener, no network), 9 (the boundary is a
  dependency graph), 13 (Lite is the same core with no device host)
- Supersedes: nothing

## Context

P3 has to prove one thing before the Lite build earns any further work: a turn
can execute end to end **in process**, across the FFI boundary shape, with no
listener, no socket, and no HTTP client anywhere in the crate. If that cannot be
shown, the "Lite is just the same core" claim is marketing.

It is a host-target spike. Nothing here runs on an iPhone yet.

## Decision

### One exported function

`crates/ios-host` exposes exactly one function across the FFI boundary:

```rust
#[uniffi::export]
pub fn run_fake_turn(input: String) -> TurnResult
```

`TurnResult` is a UniFFI `Record` (`#[derive(uniffi::Record)]`) with a single
field, `text: String`. It is plain data: cloneable, comparable, no handle, no
callback, no lifetime. The Swift name is `runFakeTurn(input:)`.

`lite_capabilities()` remains a plain Rust function and is **not** exported, so
the boundary carries one function and only one.

### The provider is a string formatter

`crates/ios-host/src/fake_provider.rs` returns `"fixture-ok: <input>"`. There is
no address, no port, no URL, no DNS call, no runtime, and no client crate. That
is the point of the spike.

The real provider seam is P4 in `core-runtime`. `ios-host` must not be able to
reach it: `eris-host` and `core-runtime` are absent from the dependency graph and
`crates/ios-host/tests/no_network.rs` fails if either name appears in the
manifest.

### Dependency line

```toml
uniffi = { workspace = true }     # resolves to workspace `uniffi = "0.32.2"`
```

No feature flags are enabled, `default-features` is left alone, and the version
lives in the workspace table pinned in P1. **No `build.rs` is needed.** No UDL
file, no `generate_scaffolding`, no `uniffi_build`, no `bindgen`.

### The 0.32.2 API detail that cost a compile

The proc-macro surface is not quite one attribute. uniffi 0.32.2 requires a
single crate-root call:

```rust
uniffi::setup_scaffolding!();
```

Without it the macros fail with `cannot find type UniFfiTag in the crate root`,
because `setup_scaffolding!()` is what emits the `UniFfiTag` parameter type, the
`ffi_<crate>_uniffi_contract_version` symbol, and the RustBuffer helpers. It is
a proc macro, so this still needs no build script and no codegen step. `#[uniffi::export]`
does generate the per-function scaffolding itself (`uniffi_ios_host_fn_func_run_fake_turn`).

### crate-type

```toml
[lib]
crate-type = ["staticlib", "cdylib", "rlib"]
```

`staticlib` is what `xcodebuild -create-xcframework` consumes on iOS, `cdylib` is
what library-mode binding generation reads, and `rlib` keeps `cargo test` and any
future workspace dependent working. All three build on this host today.

## What P3 actually proved (host target, on fih-ai-host)

- `cargo test -p ios-host`: 9 tests pass. `run_fake_turn("x").text == "fixture-ok: x"`.
- `cargo test --workspace`: 38 tests pass (P2 left 30).
- `cargo clippy --all-targets -- -D warnings`: clean, zero warnings.
- `cargo fmt --check`: clean.
- The crate builds `libios_host.a`, `libios_host.so`, `libios_host.rlib`.
- `nm -D` on the cdylib shows exactly one exported user function,
  `uniffi_ios_host_fn_func_run_fake_turn`, plus its checksum and record metadata.
- `grep -rniE 'reqwest|tokio|TcpListener' crates/ios-host/src/` is empty, and the
  same check runs in CI.
- Guard: `GUARD PASS: 29 checks unchanged`.

These are host facts. None of them is a device fact.

## The device gate is UNPROVEN

**The device half of the Lite gate has not run.** It is `unproven` until the
following executes on the MBP (M3 Max, `192.168.68.27`) with the radio off and
the app returns `fixture-ok: x` on screen. No claim in this repo, in the plan, or
in any report may say the device gate passed before that happens.

The commands below are the intended sequence, written down so the MBP run is a
transcription rather than a design exercise. Every one of them is `VERIFY:`
because none has been executed.

### 1. Toolchain and targets on the MBP

```bash
# VERIFY: rustup on the MBP has 1.99.1 and the iOS targets are installed.
rustup toolchain install 1.99.1 --profile default
rustup target add --toolchain 1.99.1 aarch64-apple-ios aarch64-apple-ios-sim

# VERIFY: a fresh clone of the repo at the P3 commit.
git clone <repo> ~/src/kallisti && cd ~/src/kallisti
```

### 2. Device and simulator static libraries

```bash
# VERIFY: produces target/aarch64-apple-ios/release/libios_host.a
cargo build --release --target aarch64-apple-ios -p ios-host

# VERIFY: produces target/aarch64-apple-ios-sim/release/libios_host.a
cargo build --release --target aarch64-apple-ios-sim -p ios-host
```

### 3. Swift bindings

The repo has no `uniffi-bindgen` bin target yet, so this step needs one of two
paths first. Neither has been tried.

```bash
# VERIFY: either add a bin target gated on the `cli` feature, or install the
# matching generator for the pinned version. Then:
# VERIFY: emits KallistiLite.swift plus the C header into bindings/swift/
cargo run --bin uniffi-bindgen -- generate \
  --library target/aarch64-apple-ios/release/libios_host.a \
  --language swift \
  --out-dir bindings/swift
```

VERIFY: the header emitted here is what `-headers` points at in step 4. If the
generator cannot read a `.a`, the fallback is to generate from the host `.dylib`
of the same commit and confirm the Swift names match (`runFakeTurn(input:)`,
`TurnResult.text`).

### 4. XCFramework

```bash
# VERIFY: builds target/KallistiLite.xcframework with ios-arm64 and
# ios-arm64-simulator slices.
xcodebuild -create-xcframework \
  -library target/aarch64-apple-ios/release/libios_host.a \
  -headers bindings/swift \
  -library target/aarch64-apple-ios-sim/release/libios_host.a \
  -headers bindings/swift \
  -output target/KallistiLite.xcframework
```

### 5. Build the spike app for a device

```bash
# VERIFY: KallistiLiteSpike.xcodeproj does not exist yet; it must link
# KallistiLite.xcframework and call runFakeTurn(input: "x").
# VERIFY: exit 0 and a Release-iphoneos .app product.
xcodebuild \
  -project KallistiLiteSpike.xcodeproj \
  -scheme KallistiLiteSpike \
  -configuration Release \
  -sdk iphoneos \
  -destination 'generic/platform=iOS' \
  -derivedDataPath build \
  DEVELOPMENT_TEAM=<TEAM_ID> \
  CODE_SIGN_STYLE=Automatic \
  build
```

### 6. Install and launch with the radio off

```bash
# VERIFY: <UDID> from `xcrun devicectl list devices`.
xcrun devicectl device install app \
  --device <UDID> \
  build/Build/Products/Release-iphoneos/KallistiLiteSpike.app

# VERIFY: app launches and stays running.
xcrun devicectl device process launch --device <UDID> com.fih.kallisti.litespike
```

VERIFY: **Airplane Mode is on before launch** (Control Center, or Settings >
Airplane Mode), and Wi-Fi and Bluetooth are off in addition. The device must be
airgapped, not merely off the LAN.

### 7. The gate itself

VERIFY: the app displays exactly `fixture-ok: x` with the radio off.

If it displays anything that required a network, that is a Gate A failure, not a
flaky test. If the app fails to launch or crashes at the FFI boundary, Gate A is
still unproven and this ADR stays at `unproven`.

## Consequences

- UniFFI is now a real dependency of a workspace member. It pulls
  `uniffi_core`, `uniffi_macros`, `uniffi_pipeline`, and (via default features)
  `cargo_metadata` and friends into the build graph. That is acceptable for a
  crate that will ship as a static library; it would not be acceptable in
  `core-domain`.
- `unsafe_code = "forbid"` at workspace level does not fight the generated
  scaffolding, because the macro expansion carries its own attributes and spans.
  Verified by `clippy -D warnings` passing.
- Adding a second exported function is a boundary change and needs its own
  review. The CI grep and the source-scan test both stay in place to keep the
  crate I/O-free.
- The device gate is the next real milestone, not a formality. It is unproven.

## Not in this ADR

No iOS app, no Xcode project, no signing identity, no device run, no bindings
generator bin, no network permission, and no claim about the device gate.
