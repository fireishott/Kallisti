# Kallisti

<p align="center">
  <img src="docs/assets/kallisti/banner-dark.png" alt="Kallisti - To the Most Beautiful." width="100%"/>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/version-0.2.0-C8CCD2?style=flat-square&labelColor=0C0C10" alt="version"/>
  <img src="https://img.shields.io/badge/iOS-18+-C8CCD2?style=flat-square&labelColor=0C0C10" alt="iOS 18+"/>
  <img src="https://img.shields.io/badge/Swift-6.2-F05138?style=flat-square&logo=swift&logoColor=white" alt="Swift 6.2"/>
  <img src="https://img.shields.io/badge/license-MIT-C8CCD2?style=flat-square&labelColor=0C0C10" alt="MIT"/>
</p>

Kallisti is a self-hosted iPhone and iPad client for [Hermes Agent](https://github.com/NousResearch/hermes-agent). It connects directly to a Hermes gateway for chat and uses the optional Kallisti connector for mobile services such as push notifications, sensors, and authenticated media delivery.

## Build 50 highlights

- Full-screen branded connection overlay with real-time truthful stages during reconnect and relaunch
- Manual Reset Connection action from the overlay and Settings (idempotent, non-destructive)
- Historical images remain accessible after gateway restart through normalized media path resolution
- Connector resolves both current default-profile and legacy per-profile media roots
- Regression coverage for connection stages, overlay visibility, reset idempotency, and media path normalization

## Build 49 highlights

- Direct native WebSocket chat with durable session and outbox recovery
- Fast follow-up delivery without waiting on stale stream leases
- One live thinking placeholder per active turn
- Authenticated inline rendering for agent-generated `MEDIA:` images
- Image uploads through the native `image.attach_bytes` RPC
- Cookie, native bearer, and paired-device authentication for mobile media
- Push registration and Live Activity lifecycle fixes

## Features

- Streaming Markdown chat, code blocks, tool activity, and reasoning status
- Session history, model selection, and profile selection
- Voice mode with configurable ASR/TTS providers and Apple speech fallback
- Optional HealthKit, CoreLocation, and CoreMotion synchronization
- Push notifications, widgets, Live Activities, and watch support
- Gateway status, logs, restart operations, and update checks
- Keychain-backed credentials and authenticated self-hosted transport

## Architecture

```text
Kallisti iOS
  -> Hermes gateway WebSocket for chat and sessions
  -> Kallisti connector for push, sensors, and authenticated media
  -> optional public reverse proxy for remote access
```

Kallisti does not require a hosted vendor backend. Operators provide their own Hermes gateway, connector, TLS endpoint, and Apple signing configuration.

## Requirements

- iOS 18 or newer
- Xcode 16 or newer
- A running [Hermes Agent](https://github.com/NousResearch/hermes-agent) gateway
- Python 3.11 or newer for the optional connector
- An Apple Developer account for device builds and push capabilities

## Build the app

```bash
git clone https://github.com/fireishott/Kallisti.git
cd Kallisti
open Herald.xcodeproj
```

Select the `Kallisti` scheme, choose your Apple Developer team, configure your gateway URL, and build to a simulator or registered device.

## Run the connector

```bash
cd connector
python3 -m venv .venv
. .venv/bin/activate
pip install -e .
kallisti run
```

Configure secrets and environment-specific URLs outside the repository. Do not commit credentials, private keys, device identifiers, internal hostnames, or production logs.

## Native media delivery

Agent responses can include local `MEDIA:` paths. In native mode, Kallisti converts supported paths to the authenticated `/v1/native/media` route. The connector:

- validates gateway cookie sessions, native bearer tokens, or persisted paired-device tokens;
- serves only supported image files under configured Hermes media roots;
- rejects traversal, unsupported types, missing files, and files over 10 MB;
- returns private cache headers and never exposes arbitrary filesystem paths.

## Testing

```bash
# iOS
xcodebuild test \
  -project Herald.xcodeproj \
  -scheme Kallisti \
  -destination 'platform=iOS Simulator,name=iPhone 16 Pro'

# Connector
cd connector
pytest
```

Use a simulator destination installed on your Xcode host.

## Security and privacy

- Credentials are stored in the iOS Keychain.
- Gateway passwords are not embedded in the app or repository.
- Media and control endpoints require authentication.
- Sensor synchronization is optional and self-hosted.
- Public bug reports must not include tokens, private URLs, device IDs, personal data, or raw production logs.

See [SECURITY.md](SECURITY.md) for responsible disclosure guidance.

## Release history

See [CHANGELOG.md](CHANGELOG.md). GitHub releases contain source notes only; signed IPAs are distributed through authorized Apple channels or built by operators with their own signing identity.

## License

MIT. See [LICENSE](LICENSE).

## Acknowledgments

Kallisti is built to work with [Hermes Agent](https://github.com/NousResearch/hermes-agent) and incorporates earlier work from the Herald mobile client under the repository's MIT license history.
