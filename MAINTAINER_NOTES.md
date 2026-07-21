# Herald — Maintainer Notes

This file is for maintainers who want a short internal snapshot of the current implementation. It is **not** the recommended onboarding guide for public users.

Start here instead:

- [README.md](README.md)
- [connector/README.md](connector/README.md)
- [relay/README.md](relay/README.md)
- [docs/CONFIGURATION.md](docs/CONFIGURATION.md)

## Architecture

```text
iOS App ──HTTP/SSE──▶ Relay ──WebSocket──▶ Connector ──▶ Hermes Agent ──▶ Ignyte ──▶ DeepSeek
```

| Component | Location | Language | Runtime |
|-----------|----------|----------|---------|
| iOS app | `Herald/` | Swift | iPhone/iPad |
| Relay | `relay/` | Python (FastAPI) | Docker / Fly.io |
| Connector | `connector/` | Python | pm2 / systemd on host |
| Hermes Agent | external | — | Ignyte/DeepSeek backend |

## How to deploy

### Relay (Docker)

```bash
cd deploy/relay
cp .env.example .env        # edit with real values
docker compose up -d        # builds image, starts relay + Postgres
curl http://localhost:8010/v1/health
```

Port mapping: host `8010` → container `8000`.  Change in `deploy/relay/docker-compose.yml` if needed.

### Relay (Fly.io)

```bash
cd relay
fly deploy
```

Config lives in `relay/fly.toml`.  Set secrets via `fly secrets set KEY=value`.

### Connector

The connector runs on the host machine (macOS/Linux).  It connects outbound to the relay via WebSocket.

```bash
cd connector
uv sync
uv run python -m connector
```

For persistent operation, use pm2 or a systemd unit.  The deploy script (`deploy/deploy.sh`) attempts `pm2 restart` automatically.

### iOS app

Build and archive via Xcode.  No server-side steps required — the app connects to whatever relay URL is configured in its settings.

To point the app at a different relay, change `PUBLIC_BASE_URL` in the relay's `.env` and re-pair the device.

## Schema changes (Postgres)

There is **no migration framework** (no Alembic, no Flyway).  The relay uses SQLAlchemy `create_all()` which creates missing tables on startup but does **not** alter existing columns or add new ones.

When you add a column or table to `relay/app/models.py`:

1. **New table**: auto-created on next relay restart.  No manual step.
2. **New column on existing table**: you must write the ALTER TABLE by hand.
   ```bash
   # Connect to the Postgres instance
   docker compose -f deploy/relay/docker-compose.yml exec postgres \
     psql -U postgres -d herald

   # Then run your ALTER
   ALTER TABLE messages ADD COLUMN new_field TEXT;
   ```
3. **Rename/drop column**: manual ALTER + code change in same deploy.  Coordinate carefully.

Always test schema changes against a local `docker compose` instance first.

## Environment variables

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `RELAY_ENVIRONMENT` | no | `development` | `development` or `production` |
| `PUBLIC_BASE_URL` | yes | `http://127.0.0.1:8000/v1` | Relay's public URL (iOS connects here) |
| `DATABASE_URL` | yes | `sqlite:///./relay.db` | Postgres connection string |
| `INTERNAL_API_KEY` | yes | `replace-me` | Auth for internal API calls |
| `CONNECTOR_SETUP_SECRET` | no | — | Required secret for connector enrollment |
| `HERALD_ADAPTER` | no | `mock` | `connector` in production, `mock` for dev |
| `APNS_KEY_PATH` | no | — | Path to APNs `.p8` key (local dev) |
| `APNS_KEY_CONTENTS` | no | — | APNs key contents (Docker/production) |
| `APNS_KEY_ID` | no | — | APNs key ID from Apple |
| `APNS_TEAM_ID` | no | — | Apple Developer Team ID |
| `APNS_BUNDLE_ID` | no | `net.fihonline.herald` | iOS app bundle ID |
| `APNS_ENVIRONMENT` | no | `production` | `production` or `development` |

See `relay/.env.example` for the full list with defaults.

## Known gotchas

### Host hard-freezes

The macOS host running the connector can freeze entirely (kernel panic, thermal shutdown).  When this happens:
- The relay sees the WebSocket drop and marks the host offline after `CONNECTOR_HEARTBEAT_TIMEOUT_SECONDS` (default 30s).
- iOS gets a 409 on the next message attempt and shows "host offline."
- **Recovery**: reboot the host, connector auto-reconnects if pm2/systemd is configured.

### Keychain unlock

The connector may need access to keychain-stored credentials (API keys, TLS certs).  After a reboot:
- macOS may lock the keychain until the user logs in and unlocks it.
- If the connector starts before keychain unlock, it will fail to read credentials.
- **Fix**: set the keychain to auto-unlock on login, or delay connector start until after login.

### Entitlement stripping

When sideloading or distributing outside the App Store, iOS can strip entitlements:
- `com.apple.developer.applesignin` — lost on re-sign
- `com.apple.developer.networking.wifi-info` — needed for local network discovery
- Push notification entitlement — required for APNs

If push notifications stop working after a re-sign, verify the entitlements in the built `.ipa` match the provisioning profile.

### SQLite → Postgres migration

Older deployments used SQLite (`relay.db`).  The docker-compose setup uses Postgres.  To migrate:
1. Dump SQLite: `sqlite3 relay.db .dump > dump.sql`
2. Clean the dump for Postgres compatibility (remove SQLite pragmas, fix quoting)
3. Load into Postgres: `psql -U postgres -d herald < dump.sql`
4. Update `DATABASE_URL` in `.env`

There is no automated migration script for this.

## Current focus

- self-hosted-first setup
- public-safe defaults
- native iPhone UX for chat, voice, widgets, and sensor-aware context

## What is broadly working

- streaming chat and attachment delivery
- voice mode with Realtime bootstrap and Herald delegation
- dynamic slash-command catalog from Hermes surfaces
- sensor pipeline (location, health, motion) through connector SQLite + MCP tools
- widgets, Live Activities, inline image rendering, and model/context UI
- APNs registration and relay-side delivery when configured

## What still requires physical-device validation

- APNs end-to-end on real Apple credentials
- background location behavior
- CarPlay entitlement path
- background audio continuity under real interruptions

## Test posture

- connector suite: passing
- relay suite: passing
- iOS build: passing
- targeted iOS tests: useful, but simulator launch flakes still happen intermittently

Treat this file as a maintainer note, not product documentation.
