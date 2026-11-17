# Kallisti

Kallisti is a self-hosted AI companion for iPhone, iPad and Mac, plus the Rust daemon
that answers it. There is no vendor backend: the app talks to your own host over the
device API, and conversations, credentials and files stay on hardware you control.

## What is in here

- **The app** - `Kallisti/`, `Kallisti.xcodeproj`, `KallistiControls/`, `KallistiWatch/`,
  `KallistiWidgets/`. Swift 6, SwiftUI, iOS. Chats with a paired daemon over
  `/v1/turns` and a server-sent event stream, with attachments, approvals, cron jobs,
  skills, notes and settings.
- **The daemon** - `crates/`, `contract/`, `install.sh`. Rust, service
  `kallisti-eris`, administration listener on `:7340` and device listener on `:7341`.
  It runs the model turns, the tool loop, the scheduler, the skill library, memory and
  the transcript.

The daemon's install, operating and architecture notes are in [DAEMON.md](DAEMON.md).

## Pairing

1. Install the daemon on a host you control - see [DAEMON.md](DAEMON.md).
2. Mint a pairing code on the host. Tick **administration** if the phone should read
   cron jobs, skills, settings and logs; a code minted without it pairs a phone that can
   chat and nothing else.
3. Open the app, enter the host address and the code.

The address is whatever reaches your host: a Tailscale name, a LAN name, or your own
TLS endpoint. The app holds a device token in the keychain and never a password.

## What the phone can do

- Chat with streaming answers, live tool activity and reattachable streams.
- Attach files and images; they upload to the daemon and appear in the transcript.
- Approvals: a turn that wants to write a file can park and ask.
- Notes, cron jobs, skills, memory and the daemon's own settings.
- Push notifications, with the banner silenced for the chat on screen.
- Latency and gateway health, read from the daemon rather than guessed.

## Building the app

```sh
xcodegen generate
open Kallisti.xcodeproj
```

`project.yml` is the source of truth for the project; the `.xcodeproj` is generated.
Adding a source file means running `xcodegen generate` again.

## Testing

```sh
# daemon
cargo test --workspace

# app
xcodebuild test -project Kallisti.xcodeproj -scheme Kallisti \
  -destination "platform=iOS Simulator,name=iPhone 17 Pro" \
  -only-testing:KallistiTests
```

## Layout

```
Kallisti/                the iOS app
Kallisti.xcodeproj         generated project
KallistiControls/        Control Center and widget intents
KallistiWatch/           watchOS companion
KallistiWidgets/         widgets
KallistiNotificationService/  notification service extension
KallistiSupport/         shared host-side support code
crates/                  the Eris daemon (eris-host, core runtime, tools, memory)
contract/                the wire contract
docs/                    daemon and client documentation
scripts/                 build, release and deploy scripts
```

## License

MIT - see [LICENSE](LICENSE).
