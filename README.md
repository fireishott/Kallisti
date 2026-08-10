# Kallisti

<p align="center">
  <img src="docs/assets/kallisti/banner-dark.png" alt="Kallisti - To the Most Beautiful." width="100%"/>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/version-0.2.1-C8CCD2?style=flat-square&labelColor=0C0C10" alt="version"/>
  <img src="https://img.shields.io/badge/iOS-18+-C8CCD2?style=flat-square&labelColor=0C0C10" alt="iOS 18+"/>
  <img src="https://img.shields.io/badge/Swift-6.2-F05138?style=flat-square&logo=swift&logoColor=white" alt="Swift 6.2"/>
  <img src="https://img.shields.io/badge/license-MIT-C8CCD2?style=flat-square&labelColor=0C0C10" alt="MIT"/>
</p>

Kallisti is a self-hosted iPhone and iPad client for [Hermes Agent](https://github.com/NousResearch/hermes-agent). It connects directly to a Hermes gateway over a native WebSocket for chat and sessions, and uses the optional Kallisti connector for mobile services: push notifications, Live Activities, sensors, and authenticated media delivery.

There is no hosted vendor backend. You bring your own Hermes gateway, connector, TLS endpoint, and Apple signing configuration. Your conversations, credentials, and media stay on infrastructure you control.

## Highlights

- Native WebSocket chat with durable session and outbox recovery
- Streaming Markdown, code blocks, tool activity, and reasoning status
- Authenticated inline rendering for agent-generated images
- Push notifications, Live Activities, widgets, and watch support
- Voice mode with configurable ASR/TTS providers and Apple speech fallback
- Optional HealthKit, CoreLocation, and CoreMotion synchronization
- Gateway status, logs, restart operations, and update checks
- Keychain-backed credentials and fully self-hosted transport

## Release 0.2.1 highlights

- Root-cause fix for long image-generation turns: the WebSocket client now accepts frames up to 64 MB, so large tool results and inline images stream to completion instead of dropping the connection mid-turn
- Tool-in-flight watchdog exemption: long-running tool calls (image generation, analysis) no longer trigger false stall detection or duplicate re-submission
- Push, logs, and restart operations refresh the native gateway bearer before every request
- Stable connection lifecycle: no more mid-session loading-surface teardown, typed text survives reconnect
- Model pill reloads on connect; iPad layout fixed
- Live Activity heartbeat keeps lockscreen status current
- Session resume fallback restores previous sessions after gateway restarts

See [CHANGELOG.md](CHANGELOG.md) for the complete history.

## Features

### Chat

- Streaming Markdown chat with syntax-highlighted code blocks
- Real-time tool activity and reasoning status
- Session history, model selection, and profile selection
- One live thinking placeholder per active turn
- Durable outbox with deadline-aware recovery

### Media

- Agent responses containing local `MEDIA:` paths render inline as authenticated images
- Native image uploads stage through the gateway before prompt submission
- Historical images stay accessible after gateway restarts
- Media serving is restricted to configured Hermes media roots

### Mobile

- Push notifications for background turns
- Live Activities on the lockscreen with elapsed-time heartbeat
- Widgets and watch support
- Voice mode with configurable ASR/TTS and Apple speech fallback
- Optional HealthKit, CoreLocation, and CoreMotion sync

### Gateway control

- Connection status with real, truthful stages
- Manual reset connection
- Gateway logs, restart, and update checks

## Architecture

```text
Kallisti iOS
  -> Hermes gateway WebSocket (chat, sessions, models, profiles)
  -> Kallisti connector (push, sensors, authenticated media)
  -> optional public reverse proxy for remote access
```

The gateway WebSocket carries chat and session traffic. The connector adds mobile services: APNs push registration, Live Activities, sensor synchronization, and authenticated media delivery. For remote access, operators place a TLS reverse proxy (for example Caddy) in front of the gateway.

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
