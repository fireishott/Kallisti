I need you to help me set up Herald -- a self-hosted iOS AI companion -- using Tailscale for networking. Herald connects my iPhone to my local Hermes agent through a relay + connector architecture. Here's what we need to do:

## What we're building

iPhone (Herald app) → Relay (FastAPI on this machine) → Connector (WebSocket bridge) → Hermes agent (local LLM or OpenRouter)

The phone reaches the relay over my Tailscale tailnet. No public ports, no cloud relay.

## Step 1: Prerequisites

Check that I have these installed. If not, help me install them:
- Python 3.11+ (with uv or pip)
- Tailscale (installed and logged in -- run `tailscale status` to verify)
- Hermes agent (the `hermes` CLI -- check with `which hermes`)
- Git (to clone the Herald repo)

## Step 2: Clone and set up the relay

```
git clone https://github.com/fireishott/Herald.git
cd Herald/relay
cp .env.example .env
```

Edit `.env` with these values:
- PUBLIC_BASE_URL -- set to my Tailscale machine URL, e.g. `https://my-hostname.tailnet-name.ts.net:8000/v1` (get my hostname from `tailscale status`)
- DATABASE_URL -- `sqlite:///./herald.db` is fine for single-user
- INTERNAL_API_KEY -- generate a strong random value (`python3 -c "import secrets; print(secrets.token_hex(32))"`)
- HERALD_ADAPTER=connector

Then install and run the relay:
```
pip install -e ".[dev]"
uvicorn app.main:app --host 0.0.0.0 --port 8000
```

## Step 3: Expose via Tailscale Serve (recommended)

Instead of opening port 8000 directly, use Tailscale Serve for HTTPS:
```
tailscale serve --bg 8000
```
This gives me a `https://my-hostname.tailnet-name.ts.net` URL with a valid cert. Update PUBLIC_BASE_URL to match (append `/v1`).

## Step 4: Set up the connector

```
cd ../connector
pip install -e ".[dev]"
```

Set environment variables:
- HERMES_MOBILE_RELAY_URL -- same as PUBLIC_BASE_URL
- HERMES_COMMAND -- path to my `hermes` binary (from `which hermes`)

Then run the interactive wizard:
```
herald
```
This registers my machine with the relay and generates a phone pairing code + QR.

## Step 5: Pair the phone

1. Install Herald from TestFlight or build from source (Xcode, iOS 26+)
2. On the connection screen, choose "Self-Hosted (Tailscale)"
3. Enter my tailnet relay URL: `https://my-hostname.tailnet-name.ts.net/v1`
4. Enter the pairing code shown by the connector wizard
5. The app should connect and show the chat screen

## Step 6: Background service (optional)

Install the connector as a background service so it runs on boot:
```
herald install-service
```
On macOS this creates a LaunchAgent. On Linux, a systemd user unit.

## Important notes

- Both my iPhone and this machine must be on the same Tailscale tailnet
- No push notifications in Tailscale mode -- messages arrive when the app is foregrounded
- The Tailscale app must be active on the iPhone for connectivity
- If the relay is unreachable, Herald will prompt me to open Tailscale

Walk me through each step. Check what I already have installed before proceeding. Ask me to confirm each step before moving to the next.
