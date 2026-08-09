# Production Architecture

This is the end-to-end topology Hermes iOS is designed around. It preserves a hard split between the client, the relay, and the Hermes agent runtime, regardless of which connection mode the user picks.

## High-level topology

```
┌──────────────┐        HTTP / SSE         ┌────────────────┐      WebSocket     ┌───────────────────┐
│   iOS app    │ ───────────────────────▶  │  Hermes relay  │ ───────────────▶   │   Connector       │
│ (SwiftUI)    │                           │   (FastAPI)    │                    │ (Python CLI/MCP)  │
└──────┬───────┘                           └──────┬─────────┘                    └──────┬────────────┘
       │                                          │                                     │
       │                                          │                                     │
       │  (App Attest + registration)             │                                     │
       ▼                                          │                                     ▼
┌──────────────┐        APNs HTTP/2        ┌──────┴─────────┐                    ┌──────────────────┐
│  Push broker │ ◀──── send grant ──────── │  Hermes relay  │                    │   Hermes runtime │
│  (Hermes)    │                           │                │                    │ (local on host)  │
└──────────────┘                           └────────────────┘                    └──────────────────┘
                                                                                         │
                                                                                         ▼
                                                                                 ┌──────────────┐
                                                                                 │ Sensor SQLite│
                                                                                 │  (local)     │
                                                                                 └──────────────┘

 Realtime audio / camera: iOS ──── WebRTC / direct ───▶ OpenAI Realtime (not via relay)
```

## Trust boundaries

| Component | Trusted with | Never sees |
| --- | --- | --- |
| iOS app | User's content, Keychain session keys, opaque push grants | APNs `.p8`, other users' data, connector credentials |
| Relay | Session keys, job queue, conversation history, opaque push grants | APNs `.p8` (in managed builds; broker holds it), raw OpenAI keys, local sensor data |
| Connector | Hermes runtime I/O, local sensor SQLite, OpenAI keys, access tokens to the relay | Other users' sessions, APNs secrets |
| Push broker | APNs `.p8`, attestation records, raw device tokens | Conversation content, user messages, agent state |
| Hermes runtime | Everything the user ran it against locally | Anything cross-user |

The split matters because:

1. **Relay compromise does not leak APNs credentials.** The relay only ever holds `relayHandle` + `sendGrant` pairs (Phase 4 push broker design).
2. **Relay compromise does not expose OpenAI keys.** Realtime audio/vision streams go directly from iOS to OpenAI; the relay is not in that path.
3. **Connector owns execution.** The relay cannot arbitrarily run code on the user's Mac — it can only enqueue jobs for the connector WebSocket.
4. **User content stays behind the relay.** Sensor SQLite, project files, and local state are all connector-local.

## Why not OpenClaw-style direct-gateway?

OpenClaw's iOS app talks directly to a gateway WebSocket. Hermes does not copy that:

- A relay/connector split keeps the iOS app stateless and the connector durable — better for mobile networks that churn connections constantly.
- The relay can queue jobs and host pushes; a direct gateway on the user's Mac cannot reliably do that.
- The product boundary (managed / Tailscale / self-hosted) becomes uniform — every mode is "iOS → relay → connector," only the relay's location changes.

What we do borrow from OpenClaw:

- QR-first setup with manual fallback.
- Honest about private-gateway foreground/reconnect behavior.
- Official-build push relay instead of sharing APNs credentials.

## Storage

### Relay (managed beta path)

- **SQLite** on a Fly Volume at `/data/relay.db` for the single-node managed beta.
- WAL mode + `busy_timeout` + foreign keys enforced (see [relay/app/database.py](../relay/app/database.py)).
- Fly Volumes are single-region, one-machine-attached, not auto-replicated. Upgrade to LiteFS or Postgres before scaling past one node.

### Connector

- Sensor SQLite lives on the user's machine; never transits the relay.
- `~/.hermes-mobile` is the default state directory.

### iOS

- `UserDefaults` for non-sensitive preferences.
- Keychain for session keys and App Attest key handles.
- Shared App Group for widget snapshots.

## Realtime audio / vision

Voice Mode uses OpenAI Realtime. The ephemeral token is minted on the connector (which holds the OpenAI key), then sent through the relay to the iOS app. iOS establishes the WebRTC connection directly to OpenAI — **the relay is not on the audio/video path**. This keeps latency low and keeps the relay out of the content path for live sessions.

## Modes in this topology

All three connection modes preserve the diagram above. Only two boxes move:

- **Managed Relay:** Hermes operates the relay box. Push broker is reachable.
- **Tailscale:** User runs the relay on their own Mac, reachable only on the tailnet. Push broker is unreachable from the user's relay (by design).
- **Self-Hosted Relay URL:** User runs the relay publicly. Same as Tailscale for push; same as managed for reachability.

See [CONNECTION_MODES.md](CONNECTION_MODES.md) for the UX implications of each mode.
