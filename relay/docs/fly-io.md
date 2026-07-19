# Fly.io deployment

This guide is the manual Fly.io path for the Hermes iOS relay.

> [!NOTE]
> The connector setup wizard can guide you through a Fly deployment. Use this document if you want to do the same steps manually or troubleshoot the wizard.

## Prerequisites

- `flyctl` installed
- `flyctl auth login` completed
- a Fly organization/account
- Python dependencies already installed in `relay/`

## 1. Create the Fly app

```bash
cd relay
flyctl apps create your-relay-app
```

Update [fly.toml](../fly.toml) before deploy:

- `app = "your-relay-app"`
- `PUBLIC_BASE_URL = "https://your-relay-app.fly.dev/v1"`

## 2. Create a persistent SQLite volume

The tracked `fly.toml` uses:

```toml
DATABASE_URL = "sqlite:////data/relay.db"

[mounts]
source = "relay_data"
destination = "/data"
```

Create the volume in the same region as the app:

```bash
flyctl volumes create relay_data --region iad --size 1 -a your-relay-app
```

This keeps the relay database out of the ephemeral container filesystem. Fly
Volumes are local to one machine and one region, so this path is intentionally
single-node. Add your own backup/restore process before depending on it for
long-lived production data.

## Optional: Create Managed Postgres

Use Managed Postgres instead of SQLite when you need multi-node relay workers,
managed database operations, or easier external admin access.

```bash
flyctl mpg create --name your-relay-db --region iad
```

List clusters and note the **cluster ID**:

```bash
flyctl mpg list
```

### Attach the database

Use the **cluster ID**, not the database name:

```bash
flyctl mpg attach <CLUSTER_ID> -a your-relay-app
```

This configures `DATABASE_URL` for the relay app and overrides the SQLite
setting in `fly.toml`.

## 3. Set secrets

At minimum:

```bash
flyctl secrets set INTERNAL_API_KEY=replace-with-a-real-secret -a your-relay-app
flyctl secrets set CONNECTOR_SETUP_SECRET=replace-with-a-bootstrap-secret -a your-relay-app
```

Optional APNs:

```bash
flyctl secrets set APNS_KEY_ID=XXXXXXXXXX -a your-relay-app
flyctl secrets set APNS_TEAM_ID=YYYYYYYYYY -a your-relay-app
flyctl secrets set APNS_BUNDLE_ID=io.hermesmobile.HermesMobile -a your-relay-app
flyctl secrets set APNS_ENVIRONMENT=development -a your-relay-app
```

If you prefer to inject the key contents instead of mounting a file:

```bash
flyctl secrets set APNS_KEY_CONTENTS="$(cat /path/to/AuthKey_XXXXXXXXXX.p8)" -a your-relay-app
```

## 4. Deploy

```bash
flyctl deploy -a your-relay-app
```

## 5. Verify

```bash
flyctl status -a your-relay-app
flyctl logs -a your-relay-app
curl https://your-relay-app.fly.dev/v1/health
```

The health endpoint should return a healthy relay response.

## Recommended production settings

- set `min_machines_running = 1` if you do not want cold starts
- keep `HERMES_ADAPTER=connector`
- use the tracked SQLite volume for the single-node managed beta path
- use PostgreSQL when you need multi-node relay workers or managed database operations
- set a strong `INTERNAL_API_KEY`

## After deploy

1. Point the connector at `https://your-relay-app.fly.dev/v1`
2. Run `hermes-mobile setup`
3. Run `hermes-mobile pair-phone`
4. Point the iOS app at the same relay URL

If the setup wizard already deployed the relay for you, you can still use this guide to verify or repair the deployment.
