# Pairing Props

**One-shot setup prompts you paste into any AI assistant to get Herald running.**

Herald connects your iPhone to your self-hosted Hermes agent through a three-tier architecture:

```
iPhone (Herald app) → Relay (FastAPI) → Connector (WebSocket bridge) → Hermes agent
```

Getting all three tiers configured correctly involves environment variables, networking, service registration, and phone pairing. These prompts encode every step so your AI assistant can walk you through the whole thing interactively.

---

## Two methods, one result

| | Tailscale | Remote Relay |
|---|---|---|
| **Networking** | Tailscale tailnet (private mesh VPN) | Public URL (Fly.io, VPS, or any host) |
| **Relay location** | Runs on your local machine | Hosted in the cloud or on a public server |
| **Inbound ports** | None (Tailscale handles it) | None on your machine (connector dials out) |
| **Phone requirements** | Tailscale app active on iPhone | Any internet connection |
| **Push notifications** | Not available (foreground only) | Not yet official (foreground + queue) |
| **Best for** | Home lab, single-network, privacy-first | Mobile use, travel, always-on access |

---

## How to use

1. Pick the method that fits your setup
2. Open the corresponding prompt file:
   - [`tailscale.md`](tailscale.md) - Tailscale / LAN method
   - [`remote-relay.md`](remote-relay.md) - Remote relay / public URL method
3. Copy the entire contents
4. Paste it into a conversation with your Hermes agent, Claude, ChatGPT, or any capable AI assistant
5. Follow the guided walkthrough

The prompts check prerequisites, walk through each step in order, and ask you to confirm before proceeding. They're designed to work with any AI assistant that can run shell commands or guide you through terminal steps.

---

## Prerequisites (both methods)

- **Python 3.11+** with pip or uv
- **Hermes agent** CLI installed (`hermes` binary)
- **Git** for cloning the Herald repo
- **Xcode 26+** if building the iOS app from source (or TestFlight access)

### Tailscale-specific
- Tailscale installed and authenticated on both your Mac/Linux host and your iPhone

### Remote relay-specific
- A place to host the relay: [Fly.io](https://fly.io) account (free tier works), a VPS, or any server with a public IP
- `flyctl` CLI if using the Fly.io deploy path

---

## What the prompts configure

Both prompts walk through the same logical steps:

1. **Prerequisite check** - verify installed tools
2. **Relay deployment** - start the FastAPI relay (locally or hosted)
3. **Networking** - expose the relay to the phone (Tailscale Serve or public URL)
4. **Connector setup** - register the local machine, configure Hermes integration
5. **Phone pairing** - generate a code, enter it in the Herald app
6. **Background service** - optional LaunchAgent/systemd for auto-start

---

## Architecture reference

```
┌──────────────┐     HTTPS/SSE      ┌──────────────┐    WebSocket     ┌──────────────┐
│              │ ──────────────────> │              │ ───────────────> │              │
│  Herald iOS  │                    │    Relay     │                  │  Connector   │
│              │ <────────────────── │  (FastAPI)   │ <─────────────── │              │
│              │     SSE stream      │              │    NDJSON        │              │
└──────────────┘                    └──────────────┘                  └──────┬───────┘
                                                                            │
                                                                     CLI / API call
                                                                            │
                                                                     ┌──────▼───────┐
                                                                     │   Hermes     │
                                                                     │   Agent      │
                                                                     │ (LLM / MCP)  │
                                                                     └──────────────┘
```

- **Relay** handles auth, device registration, pairing, session lifecycle, and message queueing
- **Connector** bridges the relay to your local Hermes installation over a persistent WebSocket
- **Hermes agent** runs your LLM (Ollama, OpenRouter, etc.) and MCP tools locally

---

## Troubleshooting

**Relay health check fails**
```bash
curl https://your-relay-url/v1/health
```
Should return 200. If not, check the relay is running and the URL is correct.

**Connector can't reach relay**
- Tailscale: run `tailscale status` and verify both machines are online
- Remote: check the relay URL is publicly reachable and HTTPS is working

**Pairing code expired**
Codes are short-lived (default 10 minutes). Generate a new one:
```bash
herald pair-phone
```

**Phone can't connect after pairing**
- Tailscale: make sure the Tailscale app is connected on your iPhone
- Remote: check the relay URL entered in Herald matches your deployment
- Both: verify the connector is running (`herald run` or the background service)

---

## Related docs

- [Configuration reference](../CONFIGURATION.md)
- [Connection modes](../CONNECTION_MODES.md)
- [Building from source](../BUILDING.md)
- [Production architecture](../PRODUCTION_ARCHITECTURE.md)
