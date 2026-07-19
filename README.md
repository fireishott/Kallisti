<!-- HERALD -->
<p align="center">
  <img src="docs/assets/brand-mark.svg" alt="HERALD" height="80"/>
  <br/>
  <sub>Self-hosted AI companion for iPhone and iPad</sub>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/version-1.0.0-FF6B00" alt="version"/>
  <img src="https://img.shields.io/badge/license-MIT-F5F0E8" alt="license"/>
  <img src="https://img.shields.io/badge/platform-iOS%2026+-0A0A0A?labelColor=0A0A0A&color=FF6B00" alt="platform"/>
  <img src="https://img.shields.io/badge/self--hosted-yes-FF3D00" alt="self-hosted"/>
  <img src="https://img.shields.io/badge/relay-active-FF6B00" alt="relay"/>
</p>

<p align="center">
  <img src="docs/assets/app-icon.svg" alt="HERALD icon" width="96" style="border-radius:20px"/>
  &nbsp;&nbsp;&nbsp;
  <span>HERALD is a native iOS companion for self-hosted AI runtimes. It adds voice mode, sensors, CarPlay, session management, and a relay so your AI moves between your phone, tablet, and desktop without becoming a hosted service.</span>
</p>

---

<p align="center">
  <img src="docs/screenshots/iphone-chat.png" alt="iPhone chat" width="30%" style="border-radius:12px;border:1px solid #1A1D23"/>
  &nbsp;
  <img src="docs/screenshots/ipad-sidebar.png" alt="iPad sidebar" width="30%" style="border-radius:12px;border:1px solid #1A1D23"/>
  &nbsp;
  <img src="docs/screenshots/voice-mode.png" alt="Voice mode" width="30%" style="border-radius:12px;border:1px solid #1A1D23"/>
</p>

---

## Features

| Feature | Description |
|---------|-------------|
| **Streaming Chat** | Real-time streaming with markdown, code blocks, inline diffs, and attachments |
| **Voice Mode** | OpenAI Realtime voice with live camera context and tool delegation |
| **iPad Native** | Full NavigationSplitView layout with session browser sidebar |
| **Session Management** | Pin, archive, rename, search. Device-scoped sessions. |
| **Model Switching** | Switch models on the fly via direct RPC |
| **Sensors** | Health, location, motion data piped to your AI in real-time |
| **CarPlay** | Hands-free AI from your dashboard |
| **Themes** | 6 built-in presets with custom wallpaper support |
| **Cron Jobs** | Schedule recurring AI tasks from your phone |
| **Skills Browser** | Browse and manage installed agent skills |

---

## Architecture

<p align="center">
  <img src="docs/assets/architecture.svg" alt="HERALD architecture" width="100%"/>
</p>

---

## Quick Start

1. **Deploy the relay**
   ```bash
   cd relay
   docker compose up -d
   ```

2. **Install the connector**
   ```bash
   pip install herald-connector
   herald start
   ```

3. **Install HERALD on your iPhone**
   - Build from source (see [Building from Source](#building-from-source)) or download the latest release
   - Open the app, scan the pairing QR code
   - Start chatting with your AI

---

## Building from Source

**Prerequisites:** Xcode 26+, macOS 26+, Apple Developer account

```bash
git clone https://github.com/fireishott/Herald.git
cd Herald
xcodegen generate
open Herald.xcodeproj
```

See [docs/BUILDING.md](docs/BUILDING.md) for signing, entitlements, and device install instructions.

---

## Relay Configuration

| Variable | Default | Description |
|----------|---------|-------------|
| `RELAY_ENVIRONMENT` | `development` | `production` or `development` |
| `PUBLIC_BASE_URL` | `http://localhost:8000/v1` | Public relay URL |
| `APNS_KEY_ID` | — | APNs key ID for push notifications |
| `APNS_TEAM_ID` | — | Apple Developer team ID |
| `APNS_BUNDLE_ID` | `net.fihonline.herald` | App bundle ID |

See [docs/CONFIGURATION.md](docs/CONFIGURATION.md) for the full reference.

---

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

---

## Acknowledgements

Built on the foundation of [Hermes-iOS](https://github.com/dylan-buck/Hermes-iOS) by [Dylan Buck](https://github.com/dylan-buck) and the [Nous Research](https://nousresearch.com/) community. Original work licensed under MIT.

---

## License

[MIT](LICENSE)
