# Herald — Project Brief (onboarding for Codex)

> Read this end-to-end before touching code. It is the fastest path from cold-start to
> productive. Companion doc: `MIMO_MARCHING_ORDERS.md` (the current defect/feature backlog).

---

## 1. What Herald is

Herald is a **native iOS/iPadOS companion app for a self-hosted [Hermes Agent](https://github.com/NousResearch/hermes-agent) runtime**. It is *not* the AI — it is the phone/tablet front-end for **your** Hermes agent running on your own machine. It gives that agent a polished mobile surface: streaming chat, voice mode, health/location/motion sensors, CarPlay, widgets/Live Activities, and session management — without user data leaving the user's infrastructure.

The product was previously "Hermes-iOS"; it was **rebranded to HERALD** at v1.0.0 (brand orange `#FF6B00`). You will still see "Hermes" everywhere it refers to the *agent runtime* (correct) vs. "Herald" for *this app* (also correct). Don't "fix" that distinction.

Repo root: `[project-root]/Herald` (git repo; default branch has the build-N commit history).

---

## 2. Architecture — three tiers, hard trust split

```
iOS app (SwiftUI)  ──HTTP/SSE──▶  Relay (FastAPI)  ──WebSocket──▶  Connector (Python)  ──▶  Hermes runtime (local)
      │                                  │                                                        │
   App Attest + push grants         SQLite job queue / sessions                          Sensor SQLite (local)
      │                                  │
      └──────── APNs via Push Broker ◀───┘         Voice/vision: iOS ──WebRTC──▶ OpenAI Realtime (NOT via relay)
```

- **iOS app** — stateless client. Holds user content, Keychain session keys, opaque push grants. Never sees APNs `.p8`, OpenAI keys, or other users' data.
- **Relay** (`relay/`) — FastAPI + SQLite. Job queue, session/conversation history, command/model/profile catalog proxy, push registration. Talks to the connector over a WebSocket (`/v1/hosts/ws`). **The relay cannot run code on the host** — it can only enqueue jobs.
- **Connector** (`connector/`) — Python CLI + MCP client that runs on the user's machine next to Hermes. Owns execution, sensor SQLite, OpenAI keys. Connects *out* to the relay via WebSocket and services RPC calls.
- **Push broker** — holds APNs credentials; relay only holds opaque `relayHandle`+`sendGrant` pairs. Compromising the relay leaks neither APNs nor OpenAI secrets.
- **Realtime audio/vision** — ephemeral OpenAI token minted on the connector, passed through relay to iOS; iOS connects **directly** to OpenAI over WebRTC. Relay is not on the media path.

**Why relay/connector instead of a direct gateway WS (OpenClaw-style):** keeps the phone stateless + connector durable across mobile network churn; relay can queue jobs and host pushes; and all three *connection modes* reduce to "iOS → relay → connector," only the relay's location moves.

**Connection modes** (see `docs/CONNECTION_MODES.md`): **Managed Relay** (Hermes-operated, push works), **Tailscale** (relay on user's Mac, tailnet-only, no push), **Self-Hosted URL** (public relay). The iOS `SettingsStore.settings.relayConfiguration.connectionMode` drives mode-aware UX (e.g. the "unreachable" banner/deep-links in `ChatScreen`).

Full detail: `docs/PRODUCTION_ARCHITECTURE.md`, `docs/THREAT_MODEL.md`, `docs/PUSH_RELAY.md`.

---

## 3. Repo layout

```
Herald/
├── project.yml            # XcodeGen source of truth — the .xcodeproj is GENERATED, edit THIS
├── Herald/                # iOS app target (Swift 6.2)
│   ├── AppEntry.swift, ContentView.swift
│   ├── Core/              # Design system, Theme/ThemeManager, MarkdownParser, Router, Haptics, DeviceClass
│   ├── Components/        # Reusable views (GlassCircleButton, StatusIndicator…)
│   ├── Features/          # Screen modules: Chat, Talk (voice), Canvas, Inbox, Cron, Skills,
│   │                      #   Settings, Sidebar (iPad), Onboarding, Permissions, Capture, CarPlay
│   ├── Models/            # ~50 value types: Message, Conversation, SSEEvent, StreamingUpdate,
│   │                      #   SlashCommand, UserSettings, *Status, HeraldActivityAttributes…
│   ├── Stores/            # @Observable @MainActor state: AppContainer (DI root) + per-domain stores
│   └── Services/          # Live/ (real impls) · Mocks/ · Protocols/ · Support/ (Keychain, RelayAPIClient,
│                          #   Resilient* wrappers, PushBroker, AppAttest…)
├── HeraldWidgets/         # WidgetKit extension: Live Activities + Home Screen widgets (shares App Group)
├── HeraldTests/, HeraldUITests/
├── relay/                 # FastAPI relay (relay/app/main.py is the endpoint surface)
├── connector/             # Python connector (connector/src/herald_connector/…)
├── skills/                # Herald-specific agent skills
├── docs/                  # Architecture, threat model, connection modes, superpowers/{plans,specs}
└── *.md                   # README, CHANGELOG, ROADMAP-APPSTORE, MONETIZATION, SECURITY, MAINTAINER_NOTES
```

`docs/superpowers/{plans,specs}/` hold the design docs for each feature wave (model switching, profiles/sessions/skills/cron, themes/wallpaper, rich chat, rebrand, ui-fixes-and-resilience). **Read the relevant spec before extending a feature** — they capture intent the code doesn't.

---

## 4. iOS app conventions (the ones that matter)

- **Swift 6.2, `SWIFT_STRICT_CONCURRENCY: complete`.** Everything UI-facing is `@MainActor`. Stores are `@MainActor @Observable final class`. New async/delegate/callback code MUST be concurrency-clean — build 13 spent a whole commit fixing notification-delegate concurrency. Prefer structured concurrency; don't sprinkle `@unchecked Sendable`.
- **Dependency injection via `AppContainer`** (`Stores/AppContainer.swift`). It constructs every store + service at launch and injects them into the SwiftUI environment. To add a store/service, wire it here.
- **Live / Mock / Resilient service pattern.** Each capability has a `Protocol` (`Services/Protocols/`), a `Live*` impl (`Services/Live/`), a `Mock*` impl (`Services/Mocks/`, used in tests/previews/unpaired fallback), and often a `Resilient*` wrapper (`Services/Support/`) that falls back to the mock when unpaired or the primary throws. Follow this shape for anything new that hits the network.
- **`RelayAPIClient`** (`Services/Support/RelayAPIClient.swift`) is the typed HTTP client to the relay; it takes an access-token provider. Stores like `ProfileStore`/`ModelStore` are constructed with it.
- **Streaming chat** (`Stores/ChatStore.swift`) is the heart of the app. Outgoing message → optimistic user bubble + empty streaming placeholder → `heraldClient.sendStreaming(message:attachments:clientMessageID:)` yields an `AsyncStream<StreamingUpdate>` (`.messageSent`/`.textDelta`/`.reasoningDelta`/`.toolActivity`/`.finished`/`.failed`). A **~30s watchdog** guards each attempt and auto-retries stalls. **`clientMessageID` is the idempotency key** — carry it through any retry work (see marching orders; the backend does not yet dedupe on it, which is a live bug).
- **Design system** in `Core/Design.swift` + `Core/Theme.swift` + `ThemeManager`. Use `Design.Colors/Spacing/Typography/Motion/CornerRadius` — don't hardcode colors/margins. Theme accent is brand orange.
- **Device adaptivity:** `Core/DeviceClass.swift` (`isPhone`/`isPad`) + `Features/Sidebar/AdaptiveRootView` (iPhone drawer vs. iPad split view). Toolbar width is tight on iPhone — overflowing toolbar items make iOS synthesize a `⋯` menu (a known bug, see marching orders).
- **Slash commands** (`Models/SlashCommand.swift`): **local** commands run in-app (`new`, `clear`, `undo`, `retry`, `save`, `title`, `history`); **gateway** commands are sent as chat text to the Hermes agent. The full catalog is fetched at runtime via `GET /v1/commands` and merged with the built-in fallback. Dispatch logic lives in `Features/Chat/ChatScreen.swift` (`handleSlashCommand`, `dispatchTypedSlashCommand`) and `ChatInputBar.swift`.
- **Widgets/Live Activities** share data through the App Group `group.net.fihonline.herald` via `SharedWidgetDataStore`. `HeraldActivityAttributes` (in both `Herald/Models/` and `HeraldWidgets/` — keep them in sync) defines the Dynamic Island `ContentState`.

---

## 5. Relay (`relay/app/`)

FastAPI, SQLite (WAL) at `/data/relay.db` on Fly for managed; `relay/app/database.py`. Key modules: `main.py` (all HTTP + the `/v1/hosts/ws` connector WebSocket), `pairing.py`, `push_broker.py`, `apns.py`, `herald_adapter.py`, `security.py`, `app_attest*.py`, `rate_limit.py`, `schemas.py`, `models.py`.

Endpoint surface (`relay/app/main.py`) — the ones you'll touch most:
- Chat/session: `POST /v1/messages`, `GET /v1/jobs/{id}/events` (SSE), `GET/POST /v1/conversations/current`, `GET/POST/DELETE /v1/sessions*`.
- Catalogs (proxied to connector RPC): `GET /v1/commands`, `GET /v1/models` + `POST /v1/model`, `GET /v1/profiles`, `GET /v1/skills`, `GET/POST/DELETE /v1/cron`, `GET /v1/memories`, `GET /v1/tools`.
- Pairing/registration: `POST /v1/pairing/redeem`, `/v1/hosts/enrollment-codes`, `/v1/device/register`, `/v1/auth/refresh|revoke`.
- Push: `/v1/push/register|deactivate|send`, `/v1/push-broker/*`, `/v1/device/app-state`.
- Voice: `/v1/talk/*`.

> **No migrations.** The relay has no migration framework — schema changes require **manual `ALTER`s**. If you change a table, include the exact ALTER statements in your PR and note them for the operator. Tests live in `relay/tests/`.

---

## 6. Connector (`connector/src/herald_connector/`)

Python CLI (`cli.py`) + long-running WebSocket client (`client.py`). Runs on the host beside Hermes; connects out to the relay.

- **RPC dispatch table** is in `client.py` (`if method == …`): `talk.*`, `commands.catalog`, `models.list`, `model.set`, `profiles.list`, `skills.list`, `cron.*`, `memories.list`, `tools.list`. **To add a native capability (e.g. a profile *switch*), add a method here + a relay endpoint + an iOS caller** — that trio is the standard pattern.
- **Profiles:** `_rpc_profiles_list` enumerates sibling dirs under `HERMES_HOME`'s parent; the *active* profile is the basename of `HERMES_HOME`. Switching profiles today is a CLI/gateway restart (`hermes profile use <name>`), not an RPC (this gap is the top item in the marching orders).
- **Service manager** (`service_management.py`): installs/starts/stops/**restarts** the connector as a background service (macOS LaunchAgent / WSL). `build_service_manager()` picks the platform impl.
- **Runtime bridge:** `herald_runner.py`, `herald_api_executor.py`, `runtime_adapter.py` create/dispatch jobs against the Hermes runtime. `mcp_server.py` + `mcp_registration.py` expose/register MCP tools. Sensor data → `sensor_store.py` (local SQLite, never transits relay).
- CLI subcommands: `setup`, `pair-phone`, `run`, `status`, `validate-mcp`, `reset`, `service {install,start,stop,restart,status,logs,uninstall}`. Tests in `connector/tests/`.

---

## 7. Build & release pipeline

- **Project is generated by XcodeGen from `project.yml`.** Edit `project.yml`, not the `.xcodeproj` (it's regenerated). Run `xcodegen generate` after changing it.
- Targets: `Herald` (app) + `HeraldWidgets` (extension) + test bundles. Package dep: `WebRTC` (stasel, 130.0.0).
- Deployment target iOS 18 in `project.yml`, but README/marketing say "iOS 26+" — treat **iOS 26** as the real minimum for new APIs (Live Activities frequent updates, latest SwiftUI). Confirm before using an API newer than 18.
- **Versioning lives in multiple places — keep them in lockstep** (`hermes-ios-build-pipeline` note): `MARKETING_VERSION` + `CURRENT_PROJECT_VERSION` in `project.yml` (both the `Herald` and `HeraldWidgets` targets), the README version badge, `CHANGELOG.md`, and any in-app version label. ⚠️ **They are currently drifted:** `project.yml` says `MARKETING_VERSION 1.2.1 / build 11`, README badge says `1.2.1`, but git history is on "build 13." Reconcile these as part of your first change.
- **MBP build gotcha:** unlock the login keychain before *every* `xcodebuild`, and strip entitlements appropriately for the paid signing team (details in the `hermes-ios-build-pipeline` note / `docs/BUILDING.md` + `MAINTAINER_NOTES.md`).
- **Documentation discipline (hard rule):** every shippable change bumps the version everywhere, adds a dated `## [x.y.z]` entry to `CHANGELOG.md` with `### Added/Changed/Fixed` naming the files touched, and updates README/`docs/` as needed. **One logical change = one commit = one changelog entry.** The current top CHANGELOG line ("build 13: fix streaming, reasoning, scroll, timer, images, video, tool persistence, notifications, error surfacing") is the anti-pattern to avoid — do not batch unrelated fixes.

---

## 8. Known-issue backlog (start here for the current work)

The active defect/feature list — each with exact file/line anchors and a concrete fix — is in **`MIMO_MARCHING_ORDERS.md`**. Summary:

| Pri | Item | Root cause (grounded) |
|----|------|------------------------|
| P0 | Profile selector doesn't switch | No `profiles.use` RPC; app sends read-only `/profile` as chat text. Needs connector RPC + relay endpoint + iOS caller. Switch **restarts the gateway** (drops session) — this is accepted. |
| P0 | Retry runs the turn twice ("already did that") | 30s watchdog re-sends slow-but-alive jobs; backend doesn't dedupe on `clientMessageID`. Fix = end-to-end idempotency. |
| P1 | Dynamic Island brain + completion notifications | Live Activity only starts on first *tool call*; no local notification on response complete. |
| P1 | Pinch-to-zoom chat text + persist size | Only image attachments have magnification; no text font-scale exists. |
| P1 | `⋯` toolbar overflow keeps appearing | Leading toolbar chips overflow narrow nav bar → SwiftUI synthesizes `⋯`. Condense chips. |
| P2 | `/new` takes several tries / cycles sessions | `/new` gated behind confirmation dialog + possible catalog-collision with `/resume` suggestions. |

---

## 9. How to generate marching orders for Mimo

"Marching orders" = a code-grounded, prioritized work spec handed to **Mimo** (the coding
model wired into Herald chat — `mimo-v2.5`) or any coding agent. `MIMO_MARCHING_ORDERS.md`
is the canonical example. Regenerate/extend it whenever the user reports a batch of
bugs/features. **Always produce them the same way and always include the environment block
below** so the agent can act without guessing.

### 9.1 Method (repeatable)

1. **Assess read-only first. Change no code** while drafting orders.
2. **Locate before you reason.** Find the real files for each reported symptom
   (`find`/`grep` the Swift/relay/connector trees). Never describe a fix from memory.
3. **Ground every claim in `file:line`.** Each item cites the exact source that causes the
   behavior. If you can't point at the code, say "repro needed" and give the call chain to
   instrument — don't invent a root cause.
4. **Prioritize P0→P2.** P0 = broken core flow / data-correctness (e.g. double-execution);
   P1 = high-value UX; P2 = polish/edge.
5. **One concrete fix path per item**, spanning the right tiers. For a new capability, spell
   out the **connector RPC + relay endpoint + iOS caller** trio explicitly (that is the
   house pattern — §6).
6. **Give acceptance criteria** per item (observable pass/fail on device).
7. **Restate the cross-cutting rules** at the top of the doc: bump version everywhere, one
   change = one commit = one dated CHANGELOG entry, update README/docs, relay has **no
   migrations → ship manual ALTERs**, keep Swift 6 strict-concurrency clean.
8. **Flag hard constraints** that change the fix (e.g. profile switch restarts the gateway;
   host OOM freezes mean "accepted but slow" ≠ failure).
9. **Resolve genuine forks with the user** (e.g. "which `⋯`?", "is a session-dropping
   restart acceptable?") before finalizing — don't guess on decisions only they can make.
10. **Sequence the work** at the end: independent PRs, each self-contained with its own
    version bump + changelog entry.

### 9.2 Environment block (paste into every marching-orders doc)

| Thing | Value |
|------|-------|
| **App repo** | `[project-root]/Herald` (git; branch history is "build N" commits) |
| **iOS app** | `Herald/` target, Swift 6.2, `SWIFT_STRICT_CONCURRENCY: complete`, iOS 26 target for new APIs |
| **Widgets** | `HeraldWidgets/` extension, App Group `group.net.fihonline.herald` |
| **Relay** | `relay/` FastAPI + SQLite; **no migration framework → manual `ALTER`s**; reached in the field at host `herald-host.internal:8010` |
| **Connector** | `connector/src/herald_connector/`; Python WS client; RPC table in `client.py`; runs beside Hermes on the host |
| **Hermes host** | `fih-ai-host` @ `herald-host.internal`; **hard-freezes multiple times/day (OOM history)** → expect slow first-token; watchdog must not retry accepted-but-slow jobs |
| **Coding model** | Mimo `mimo-v2.5` (in-app model selector); this is who the orders address |
| **Build machine** | MacBook Pro. **Unlock login keychain before *every* `xcodebuild`**; bump version in `project.yml` (both targets) + README badge + CHANGELOG; strip entitlements for the paid signing team |
| **Project gen** | XcodeGen — edit `project.yml`, run `xcodegen generate`; the `.xcodeproj` is generated |
| **Bundle / team** | `net.fihonline.herald` · `DEVELOPMENT_TEAM 58U7UPFS53` (`project.yml` is authoritative) |
| **Secrets** | Never hardcode. iOS → Keychain; relay/connector → env. Known host-side security debt is out of scope for Herald changes |
| **Version state** | ⚠️ drifted: `project.yml` `1.2.1 / build 11`, README `1.2.1`, git on "build 13" — reconcile in first change |

### 9.3 Prompt template (to regenerate on demand)

> "Assess the Herald codebase read-only (no code changes) and produce marching orders for
> Mimo covering: `<list the reported symptoms>`. For each: cite `file:line`, give the root
> cause, one concrete fix path (connector RPC + relay endpoint + iOS caller where it's a new
> capability), and acceptance criteria. Prioritize P0→P2. Open with the cross-cutting rules
> (version/changelog/docs every change; relay has no migrations → manual ALTERs; Swift 6
> strict concurrency) and include the environment block from `HERALD_PROJECT_BRIEF.md §9.2`.
> Ask me about any decision only I can make before finalizing."

---

## 10. Fast-orientation reading order

1. This brief + `docs/PRODUCTION_ARCHITECTURE.md`.
2. `Stores/AppContainer.swift` — how everything is wired.
3. `Stores/ChatStore.swift` — the streaming/retry/live-activity core.
4. `Features/Chat/ChatScreen.swift` + `ChatInputBar.swift` — the main UI + slash dispatch.
5. `relay/app/main.py` — the endpoint contract.
6. `connector/src/herald_connector/client.py` — the RPC contract.
7. `MIMO_MARCHING_ORDERS.md` — what to build next.

---

## 11. Operational context (don't hardcode secrets)

- The Hermes runtime this app targets runs on a self-hosted host (see the operator's infra notes). The host has a history of memory-pressure hard-freezes — expect occasionally slow first-token times, which is *why* the retry watchdog must not treat "accepted but slow" as failure.
- There is a **known security debt** outside this repo (a plaintext admin password across Hermes *skill* files; an unsandboxed api_server exposure the operator has accepted). These are **Hermes-host** concerns, not the Herald app — do not import those patterns, never hardcode credentials in Herald, keep secrets in Keychain (iOS) / env (relay/connector).
- Bundle/team: `net.fihonline.herald`, App Group `group.net.fihonline.herald`, `DEVELOPMENT_TEAM 58U7UPFS53`. (Note: CHANGELOG 1.0.0 references an older `net.fihonline.herald` id — `project.yml` is authoritative.)

---

**TL;DR for Codex:** Herald is a Swift 6.2 strict-concurrency iOS app that is the mobile face of a self-hosted Hermes agent, reached through a FastAPI relay and a Python connector. Add capabilities as a **connector RPC + relay endpoint + iOS store/caller** trio. Respect the Live/Mock/Resilient service pattern and the design system. Bump versions + changelog + docs on every change, one change per commit. Relay has no migrations — ship manual ALTERs. Current work is in `MIMO_MARCHING_ORDERS.md`.
