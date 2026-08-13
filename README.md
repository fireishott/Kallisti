# Kallisti

<p align="center">
  <img src="docs/assets/rebrand/github/readme-banner-1600x480.png" alt="Kallisti - To the Most Beautiful." width="100%"/>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/version-0.2.3-C8CCD2?style=flat-square&labelColor=0C0C10" alt="version"/>
  <img src="https://img.shields.io/badge/iOS-18+-C8CCD2?style=flat-square&labelColor=0C0C10" alt="iOS 18+"/>
  <img src="https://img.shields.io/badge/Swift-6.2-F05138?style=flat-square&logo=swift&logoColor=white" alt="Swift 6.2"/>
  <img src="https://img.shields.io/badge/license-MIT-C8CCD2?style=flat-square&labelColor=0C0C10" alt="MIT"/>
</p>

Kallisti is a self-hosted iPhone and iPad client for [Hermes Agent](https://github.com/NousResearch/hermes-agent). It connects to a Hermes gateway over a native WebSocket for chat, sessions, models, and profiles, and uses the optional Kallisti connector for mobile services: push notifications, Live Activities, authenticated media, and optional sensor synchronization.

There is no hosted vendor backend. You bring your own Hermes gateway, connector, and TLS endpoint. Conversations, credentials, and media stay on infrastructure you control. The app is available from the App Store, and the source is open for anyone who wants to build it themselves.

## Highlights

- Native WebSocket chat with durable session and outbox recovery
- Streaming Markdown, code blocks, tool activity, and reasoning status
- One-time pairing-code sign-in with native gateway mode
- Authenticated inline rendering for agent-generated images
- Push notifications with per-device routing and an in-app inbox
- Live Activities on the lockscreen with elapsed-time heartbeat
- Widgets, watch, and notification-service extensions
- Voice mode with configurable ASR/TTS providers and Apple speech fallback
- Optional HealthKit, CoreLocation, and CoreMotion synchronization
- Handwriting notes with Apple Pencil and OCR / AI enrichment
- Gateway status, logs, restart, software update checks, and truthful connection stages
- Connection latency monitoring and searchable auxiliary model switching

## Features

### Chat

- Streaming Markdown chat with syntax-highlighted code blocks
- Real-time tool activity and reasoning status
- Session history, model selection, and profile selection
- Durable outbox with deadline-aware recovery
- Draft text persists across reconnect and view recreation
- One live thinking placeholder per active turn
- Opens to your last active conversation, or a fresh session on first launch

### Media

- Agent responses with local `MEDIA:` paths render inline as authenticated images
- Native image uploads stage through the gateway before prompt submission
- Historical images stay accessible after gateway restarts
- Media serving is restricted to configured Hermes media roots
- Aspect-fit thumbnails for consistent chat layout

### Mobile

- Push notifications for background turns, scoped per device via installation ID
- Live Activities on the lockscreen with elapsed-time heartbeat
- In-app notification inbox with dismiss and snooze actions
- Widgets, watch, and notification-service extensions
- Voice mode with configurable ASR/TTS and Apple speech fallback
- Optional HealthKit, CoreLocation, and CoreMotion sync
- Handwriting notes with Apple Pencil, OCR, and AI enrichment

### Gateway control

- Connection status with real, truthful stages
- Manual reset connection
- Gateway logs, restart, and software update checks
- Realtime connector latency readout in Settings

## Architecture

```text
Kallisti iOS
  -> Hermes gateway WebSocket (chat, sessions, models, profiles)
  -> Kallisti connector (push, sensors, authenticated media)
  -> optional public reverse proxy for remote access
```

The gateway WebSocket carries chat and session traffic. The connector adds mobile services: APNs push registration, Live Activities, sensor synchronization, and authenticated media delivery. For remote access, operators place a TLS reverse proxy (for example Caddy) in front of the gateway and connector.

Connection modes are documented in [docs/CONNECTION_MODES.md](docs/CONNECTION_MODES.md), production architecture in [docs/PRODUCTION_ARCHITECTURE.md](docs/PRODUCTION_ARCHITECTURE.md), and the threat model in [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md).

## Requirements

- iOS 18 or newer
- A running [Hermes Agent](https://github.com/NousResearch/hermes-agent) gateway
- Python 3.11 or newer for the optional connector
- Xcode 16 or newer and an Apple Developer account only if you build from source

## Quick start

### Install from the App Store

Download Kallisti from the App Store and pair it with your Hermes gateway. No Apple Developer account or Xcode setup is needed for the App Store path.

If you want push notifications, HealthKit synchronization, Live Activities, or authenticated media delivery, run the optional connector on your Hermes host:

```bash
cd connector
python3 -m venv .venv
. .venv/bin/activate
pip install -e .
kallisti run
```

Configure secrets and environment-specific URLs outside the repository. Do not commit credentials, private keys, device identifiers, internal hostnames, or production logs. The connector serves the HTTP facade for push registration, Live Activities, and authenticated media on the port you configure.

### Build from source

Kallisti is open source under MIT and free to build. For tinkerers who want their own signed builds:

```bash
git clone https://github.com/fireishott/Kallisti.git
cd Kallisti
open Herald.xcodeproj
```

Select the `Kallisti` scheme, choose your Apple Developer team, configure your gateway URL, and build to a simulator or registered device. Detailed build notes are in [docs/BUILDING.md](docs/BUILDING.md) and configuration options in [docs/CONFIGURATION.md](docs/CONFIGURATION.md).

### Sign in on device

1. Launch Kallisti and choose the native gateway mode.
2. Generate a one-time pairing code from your Hermes host.
3. Enter the code in the app. The code is never persisted on the device.

## Push notifications

Push delivery uses APNs through the connector. The app registers an APNs token keyed by its installation ID, so multi-device setups route notifications to the right device. Live Activity registration uses a separate token kind.

See [docs/PUSH_RELAY.md](docs/PUSH_RELAY.md) for the relay and push broker architecture, and [docs/IOS_CAPABILITIES.md](docs/IOS_CAPABILITIES.md) for capability setup (background modes, push entitlements, Live Activities).

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
- Pairing codes are single-use and never persisted.
- Sensor synchronization is optional and self-hosted.
- Public bug reports must not include tokens, private URLs, device IDs, personal data, or raw production logs.

See [SECURITY.md](SECURITY.md) for supported versions and responsible disclosure guidance, and [docs/THREAT_MODEL.md](docs/THREAT_MODEL.md) for the full threat model.

## Release history

The app is distributed through the App Store. GitHub releases carry source notes for those building from source, and the full history is in [CHANGELOG.md](CHANGELOG.md).

## License

MIT. See [LICENSE](LICENSE).

## Acknowledgments

Kallisti is built to work with [Hermes Agent](https://github.com/NousResearch/hermes-agent) and incorporates earlier work from the Herald mobile client under the repository's MIT license history.
