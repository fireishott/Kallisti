# Kallisti for iOS

<p align="center">
  <img src="docs/assets/banner-dark.png" alt="Kallisti - To the Most Beautiful." width="100%"/>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/version-1.4.14-C8CCD2?style=flat-square&labelColor=0C0C10" alt="version"/>
  <img src="https://img.shields.io/badge/iOS-18+-C8CCD2?style=flat-square&labelColor=0C0C10" alt="iOS 18+"/>
  <img src="https://img.shields.io/badge/Swift-6.2-F05138?style=flat-square&logo=swift&logoColor=white" alt="Swift 6.2"/>
  <img src="https://img.shields.io/badge/backend-Eris-C8CCD2?style=flat-square&labelColor=0C0C10" alt="Eris"/>
  <img src="https://img.shields.io/badge/license-MIT-C8CCD2?style=flat-square&labelColor=0C0C10" alt="MIT"/>
</p>

Kallisti is the iPhone and iPad frontend for **Kallisti-Eris**, the self-hosted
daemon that holds the brain, the tools and the memory. Eris is the backend; Kallisti
is the frontend. The app is the literal Kallisti 0.4.0 UI, and it talks to the daemon
over its own device API - there is no third-party relay, and nothing leaves your
infrastructure.

## The three parts

| Part | Where it lives | What it is |
|---|---|---|
| **Kallisti for iOS** | this branch | The front. The literal Kallisti 0.4.0 UI - `AppRootView` -> the full `AdaptiveRootView` sidebar (Chat, Inbox, Talk, Notes, Settings), health and sensors - as native Swift 6 / SwiftUI. |
| **Kallisti-Eris** | `main` | The back. A Rust daemon on your hardware: turns, tools, memory, MCP, approvals, sensors, inbox, notes, push, backups and device pairing. |
| **Kallisti-Eris Web UI** | `main`, `crates/eris-host/src/web` | The same daemon's dashboard: chat, sessions, settings, schedules, skills, approvals, backups and the onboarding wizard. Server-rendered HTML, one inlined stylesheet, one script. |

The app and the dashboard are two frontends onto one product, so they read the same
daemon state. Settings is the clearest case: the phone renders the daemon's own
settings table - all six of its tabs - rather than keeping a copy, which is why a
setting changed in the browser is the setting the phone shows, and a setting added
to the daemon appears on the phone with no app release.

## How it connects

`HeraldClientProtocol` is implemented by `ErisHeraldClient`, so the 0.4.0 screens run
over Eris unchanged. A fresh install pairs through the 0.4.0 onboarding:
`ErisPairingService` posts the daemon address + pairing code to `/v1/pair`, the pair
lands in the keychain, and the client restores it into chat.

**Backed over Eris:** pairing, chat + SSE streaming with the model's reasoning, the
round it is in, the command it is running and that command's stdout decoded while the
turn runs, Stop that cancels the turn on the daemon rather than only on the screen,
conversations and their transcripts, session create/list/search, Inbox, Senses
(health/location), Settings (the daemon's whole table: models, assistant, memory,
devices, automation, system, plus the full-host switch and backups), the control plane
(models, schedules, skills, auxiliary roles), gateway control, and notes enrichment
through a real run.

**Still typed-stubbed** pending their device routes: session delete/archive/pin/rename,
title generation, job status, voice transcript. Attachments are text-only on
`/v1/turns`. Talk is in development.

A turn is created once per message: the client sends an idempotency key derived from
the message's own id, so a retry after a dropped connection resumes the turn it already
started instead of asking the daemon twice.

## Status

Private beta. TestFlight **1.4.14 (109)** is the current app build, on daemon
**1.4.11 build 123**.

The app and the daemon are separate version lines: the daemon's build number moves
when the daemon ships and the app's when the app does, so a problem report should name
both. `GET /v1/version` is the daemon's own answer, and the app shows its own build in
Settings.

## Features (0.4.0 UI)

- **Rich chat** - streaming, markdown, syntax-highlighted code, and the model's reasoning streamed into the thought card while the turn runs rather than after it
- **Health and sensors** - HealthKit, CoreLocation, CoreMotion into Eris `sensors.db`
- **Inbox** - Eris inbox items and actions
- **Notes** - enrichment runs on the daemon; the app opens a run and streams its progress
- **Camera and photos**, **handwriting and OCR** (iPad), **widgets and Live Activities**, **push notifications**
- **Device pairing with opaque tokens** - the code is never persisted

## Requirements

- iOS 18 or newer
- A running **Kallisti-Eris** daemon (see the `main` branch)
- Xcode 26 or newer and an Apple Developer account only if you build from source

## Quick start

### TestFlight

Install Kallisti from TestFlight, open it, and enter your Eris daemon address plus a
minted pairing code at the onboarding pairing step. No Apple Developer account needed.

### Build from source

```bash
git clone https://github.com/fireishott/Kallisti.git
cd Kallisti
open Herald.xcodeproj
```

Select the `Kallisti` scheme, choose your Apple Developer team, and build to a
simulator or registered device.

## Security and privacy

- Credentials are stored in the iOS Keychain.
- Pairing codes are single-use and never persisted.
- Sensor synchronization is optional and self-hosted.

See [SECURITY.md](SECURITY.md). Build steps, signing and the one config file\nare in [docs/BUILDING.md](docs/BUILDING.md).

## Release history

TestFlight for private beta testers; GitHub releases carry notes. Full history in
[CHANGELOG.md](CHANGELOG.md).

## License

MIT. See [LICENSE](LICENSE).

Kallisti is open source and stays that way, on both sides. Everything this project
ships is built from the source in this repository, and anyone can clone it and build
the app and the daemon themselves - that path needs only Xcode and a Rust toolchain,
and it keeps working. The branches here are the source of truth, not a mirror of it.

## Development team

- **Curtis Freeman** ([@fireishott](https://github.com/fireishott)) - Creator and lead developer.
- **Mark Davis** ([@doc-holliday-1](https://github.com/doc-holliday-1)) - iPhone and iPad development.
