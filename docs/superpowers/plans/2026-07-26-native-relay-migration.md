# Native Relay Migration Plan

**Goal:** Eliminate the custom Herald relay (4145 lines of FastAPI) by merging it into the connector and adding the native Hermes gateway relay protocol.

## Current vs Target

```
CURRENT:
iOS App → HTTP/SSE → Caddy (:443)
                        ↓
                   Herald Relay (Docker, :8010) ← custom, 4145 lines
                        ↓ WebSocket (custom protocol)
                   Connector (systemd)
                        ↓ CLI/HTTP
                   Hermes Gateway (API server)

TARGET:
iOS App → HTTP/SSE → Caddy (:443)
                        ↓
                   Connector (systemd, :8010) ← now serves HTTP directly
                        ├─ HTTP/SSE facade (same API as old relay)
                        ├─ Native Relay WS (gateway/relay/ws_transport.py protocol)
                        └─ Hermes Gateway bridge (API server :8642)
```

## What Changes

### Phase 1: Merge relay into connector (this PR)
1. Move `relay/app/*` → `connector/src/herald_connector/http_relay/`
2. Connector's `__main__` starts the HTTP server on :8010
3. Add native relay WebSocket server alongside
4. iOS app: ZERO changes (same API, same port)

### Phase 2: Delete Docker relay
1. Stop `hermes-relay` Docker container
2. Update Caddy to point at connector's :8010 directly
3. Archive `relay/` directory

### Phase 3: Native relay protocol (future)
1. Gateway connects via `WebSocketRelayTransport` to connector's `/relay` WS
2. Gateway sends `hello` → connector replies with `descriptor`
3. Messages flow: inbound (iOS→gateway), outbound (gateway→iOS)
4. Multi-platform: Discord, Telegram, iOS all through same connector

## Endpoints to Preserve (iOS app API)

Core (must keep):
- POST /v1/messages (SSE streaming)
- GET /v1/models, POST /v1/model
- GET /v1/profiles, POST /v1/profile
- GET /v1/session, POST /v1/auth/refresh
- GET /v1/conversations/current, POST /v1/conversations/current/clear
- GET /v1/sessions (CRUD)
- GET /v1/health, /health
- GET /v1/commands, /v1/capabilities
- GET /v1/inbox, POST /v1/inbox/{id}/action
- POST /v1/push/register, /v1/push/deactivate
- POST /v1/device/register, /v1/device/sensor/*
- Pairing/setup endpoints
- Host management
- Talk/voice endpoints

Dashboard (keep if dashboard port moves):
- /gw/* endpoints
- /api/* endpoints
- WebSocket /ws/control, /v1/hosts/ws

## What the Native Relay Protocol Adds

The Hermes gateway's `gateway/relay/` provides:
- `RelayAdapter` — generic multi-platform adapter
- `WebSocketRelayTransport` — gateway→connector WS client
- `CapabilityDescriptor` — connector advertises capabilities
- Auth: HMAC upgrade tokens + delivery signatures
- Command manifest: slash commands declared by gateway

Once the connector speaks this protocol, ANY Hermes gateway can connect and serve ANY platform through it.
