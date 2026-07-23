I need you to help me set up Herald -- a self-hosted iOS AI companion -- using a remote relay for networking. Herald connects my iPhone to my local Hermes agent through a relay + connector architecture. Here's what we need to do:

## What we're building

iPhone (Herald app) → Relay (hosted on Fly.io / VPS / cloud) → Connector (on my local machine, dials out via WebSocket) → Hermes agent (local LLM or OpenRouter)

The relay lives on a public URL. My local connector dials out to it -- no inbound ports needed on my machine.

## Step 1: Prerequisites

Check that I have these installed. If not, help me install them:
- Python 3.11+ (with uv or pip)
- Hermes agent (the `hermes` CLI -- check with `which hermes`)
- Git (to clone the Herald repo)
- For Fly.io deploy: `flyctl` (check with `fly version`)

## Step 2: Clone the repo

```
git clone https://github.com/fireishott/Herald.git
cd Herald
```

## Step 3: Deploy the relay

### Option A: Fly.io (recommended for most users)

The connector wizard can deploy to Fly.io automatically:
```
cd connector
pip install -e ".[dev]"
herald
```
Choose "Deploy a new relay to Fly.io" when prompted. The wizard will:
- Create a Fly app
- Set up a Postgres database
- Configure secrets (INTERNAL_API_KEY, CONNECTOR_SETUP_SECRET)
- Deploy the relay from the repo's `relay/` directory
- Return the public relay URL

### Option B: Docker on a VPS

On my server:
```
cd relay
cp .env.example .env
```

Edit `.env`:
- PUBLIC_BASE_URL -- my server's public URL + `/v1`, e.g. `https://herald.example.com/v1`
- DATABASE_URL -- `postgresql+psycopg://postgres:postgres@postgres:5432/herald` (default for docker-compose)
- INTERNAL_API_KEY -- generate a strong random value (`python3 -c "import secrets; print(secrets.token_hex(32))"`)
- CONNECTOR_SETUP_SECRET -- generate another random value (connector must match)
- HERALD_ADAPTER=connector

Then:
```
docker compose up -d
```

This starts the relay (port 8000) and Postgres. Put it behind a reverse proxy (nginx/caddy) with HTTPS.

### Option C: Direct Python on a VPS

```
cd relay
pip install -e ".[dev]"
cp .env.example .env
# Edit .env as in Option B, but use sqlite for simplicity:
# DATABASE_URL=sqlite:///./herald.db
uvicorn app.main:app --host 0.0.0.0 --port 8000
```

## Step 4: Set up the connector (on my local machine)

```
cd connector
pip install -e ".[dev]"
```

Set environment variables:
- HERMES_MOBILE_RELAY_URL -- the public relay URL from Step 3
- HERMES_COMMAND -- path to my `hermes` binary (from `which hermes`)
- CONNECTOR_SETUP_SECRET -- must match the relay's value (if set)

Run the setup:
```
herald setup --relay-url https://my-relay.fly.dev/v1
```

Or use the interactive wizard:
```
herald
```
Choose "Use an existing relay URL" and enter the URL.

This registers my machine and generates a phone pairing code + QR.

## Step 5: Pair the phone

1. Install Herald from TestFlight or build from source (Xcode, iOS 26+)
2. On the connection screen, choose "Self-Hosted Relay"
3. Enter my relay URL: `https://my-relay.fly.dev/v1` (or whatever my public URL is)
4. Enter the pairing code shown by the connector
5. The app should connect and show the chat screen

## Step 6: Background service (optional)

Install the connector as a background service so it auto-starts:
```
herald install-service
```
On macOS this creates a LaunchAgent. On Linux, a systemd user unit.

## Important notes

- The connector dials OUT to the relay -- no firewall/port changes needed on my local machine
- The relay must be publicly reachable (HTTPS recommended)
- No official push notifications for self-hosted relays yet -- messages arrive when the app is foregrounded or refreshed
- Messages queue on the relay while the connector is offline and deliver when it reconnects
- For Fly.io: the free tier works fine for single-user; relay is lightweight

Walk me through each step. Check what I already have installed before proceeding. Ask me to confirm each step before moving to the next.
