# Contributing to Kallisti

Kallisti is a self-hosted AI companion for iPhone, iPad and Mac, and the Rust daemon
that answers it. There is no vendor backend: the app speaks to the daemon's device API
on the machine you run.

## Layout

- **App:** `Kallisti/` (Swift 6, SwiftUI), `KallistiControls/`, `KallistiWatch/`,
  `KallistiWidgets/`, `KallistiNotificationService/`, `KallistiSupport/`.
- **Daemon:** `crates/` (Rust), `contract/` (the wire contract), `install.sh`.
- **Docs:** `docs/` for both halves; the daemon's own notes are in `DAEMON.md`.

## Building

```sh
# daemon
cargo build --release
cargo test --workspace

# app - project.yml is the source of truth
xcodegen generate
open Kallisti.xcodeproj
```

Adding a Swift file means running `xcodegen generate` again; the `.xcodeproj` is
generated, not edited.

## Testing before a pull request

```sh
cargo test --workspace
xcodebuild test -project Kallisti.xcodeproj -scheme Kallisti \
  -destination "platform=iOS Simulator,name=iPhone 17 Pro" \
  -only-testing:KallistiTests
```

A daemon change ships with a test that fails without it. The suite is the contract: it
ran green on the commit you are building on.

## Style

- Rust: `cargo fmt`, `cargo clippy --workspace --all-targets`.
- Swift: the project builds with no warnings that name a file, and the app's design
  tokens live in `Design`.
- Comments explain *why* a thing is the way it is, especially where the obvious
  alternative is wrong. A comment that restates the code is noise.

## Commits

- One change per commit, with a message that says what was wrong and why the fix is
  right. The changelog is assembled from these.
- Bump the daemon's `BUILD` when a daemon change ships; the app's
  `CURRENT_PROJECT_VERSION` moves with every TestFlight build.

## Reporting a bug

Include the daemon build (`eris version`), the app build (TestFlight number), and what
you saw. `GET /v1/diagnostics` on the device API holds the daemon's recent events, and
`GET /v1/gw/health` says whether the daemon thinks it is well.
