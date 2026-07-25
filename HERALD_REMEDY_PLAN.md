# Herald v2.3.0 — Remedy Plan

**Date:** 2026-07-25
**Current build:** v2.3.0 (build 71) — "The Realtime Edition"
**Issues found:** 6 (3 P0, 3 P1)

---

## P0-1: APNS Environment Is `development` — Push Notifications Never Delivered

### Root Cause

Three places hardcode `development` when TestFlight requires `production`:

| File | Line | What's Wrong |
|------|------|-------------|
| `project.yml` | 61 | `com.apple.developer.aps-environment: development` |
| `Herald/Stores/AppContainer.swift` | 800 | `let pushEnvironment = "development"` |
| `deploy/deploy.py` | 31 | `SET push_environment='development'` |
| `deploy/push-only.py` | 16 | `SET push_environment='development'` |
| `deploy/push-fix.py` | 16 | `SET push_environment='development'` |

The comment at `AppContainer.swift:797-799` is **factually wrong**:

> "Herald is TestFlight-only (not App Store). The aps-environment entitlement is hardcoded to 'development', so iOS always issues development tokens."

**TestFlight uses the production APNs environment.** Apple documents this clearly: TestFlight builds are signed with a Distribution provisioning profile, and iOS issues production push tokens for them. The only way to get a development APNs token is to build directly to a device from Xcode with a Development profile.

Since the entitlement says `development`, iOS issues a **development** push token. The relay (defaulting to `APNS_ENVIRONMENT=production`) sends the push to the **production** APNs server with a development token. APNs rejects it silently. No push notification is ever delivered.

### Remedy

**Three coordinated changes (all must be made together):**

#### 1. Fix the entitlement (`project.yml:61`)
```yaml
# Change:
com.apple.developer.aps-environment: development
# To:
com.apple.developer.aps-environment: production
```

#### 2. Fix the push environment string (`AppContainer.swift:797-800`)
```swift
// Change:
// Herald is TestFlight-only (not App Store). The aps-environment
// entitlement is hardcoded to "development", so iOS always issues
// development tokens. The relay must match: APNS_ENVIRONMENT=development.
let pushEnvironment = "development"

// To:
// TestFlight builds use the production APNs environment. The
// aps-environment entitlement must be "production" so iOS issues
// production tokens that the relay can deliver via APNs production.
let pushEnvironment = "production"
```

#### 3. Fix the deploy scripts (3 files)
In `deploy/deploy.py:31`, `deploy/push-only.py:16`, `deploy/push-fix.py:16`:
```sql
-- Change:
UPDATE push_registrations SET is_active=true, push_environment='development', updated_at=NOW();
-- To:
UPDATE push_registrations SET is_active=true, push_environment='production', updated_at=NOW();
```

#### 4. Clean up existing DB state (one-time on .118)
```bash
docker exec herald-postgres psql -U herald -d herald -c \
  "UPDATE push_registrations SET push_environment='production', updated_at=NOW();"
```

### Acceptance Criteria
- [ ] Build with `aps-environment: production` entitlement
- [ ] iOS issues production push token (hex starts with production prefix)
- [ ] Token registered in DB with `push_environment='production'`
- [ ] Push notification delivered to TestFlight device within 10s of job completion
- [ ] Notification Service Extension formats the notification with preview text

---

## P0-2: Control Center Widgets Reference Non-Existent Classes

### Root Cause

The `HeraldControls` target contains three Control Widgets that reference classes that were **never created**:

| File | Missing Class | Usage |
|------|--------------|-------|
| `GatewayStatusControl.swift` | `HeraldAppState` | `.shared.relayBaseURL`, `.shared.accessToken` |
| `GatewayStatusControl.swift` | `GatewayState` | `.shared.update(...)` |
| `RestartGatewayControl.swift` | `HeraldAppState` | `.shared.relayBaseURL`, `.shared.accessToken` |
| `ModelSwitchControl.swift` | `HeraldAppState` | `.shared.relayBaseURL`, `.shared.accessToken` |
| `ModelSwitchControl.swift` | `GatewayState` | `.shared.update(...)` |

These files **will not compile** — `HeraldAppState` and `GatewayState` do not exist anywhere in the codebase. The Control Center extension was shipped with forward references to classes that were documented in the v2.3 changelog but never implemented.

Additionally, `RelayAPIClient` is defined in `Herald/Services/Support/RelayAPIClient.swift` (main app target) and is **not accessible** from the `HeraldControls` extension target. The Controls target depends on `HeraldWidgets`, not the main app.

### Remedy

**Option A — Create the shared state classes (recommended):**

#### 1. Create `HeraldAppState` (in `HeraldWidgets/` so both targets can access it)
```swift
// HeraldWidgets/HeraldAppState.swift
import Foundation

/// Shared app state accessible from both the main app and Control Center extension.
/// Uses App Group UserDefaults for cross-process communication.
final class HeraldAppState: Sendable {
    static let shared = HeraldAppState()

    private let defaults = UserDefaults(suiteName: "group.net.fihonline.herald")!

    var relayBaseURL: String {
        defaults.string(forKey: "herald.relayBaseURL") ?? "https://herald-host.internal:8010/v1"
    }

    var accessToken: String? {
        defaults.string(forKey: "herald.accessToken")
    }

    func update(relayBaseURL: String, accessToken: String?) {
        defaults.set(relayBaseURL, forKey: "herald.relayBaseURL")
        defaults.set(accessToken, forKey: "herald.accessToken")
    }
}
```

#### 2. Create `GatewayState` (in `HeraldWidgets/`)
```swift
// HeraldWidgets/GatewayState.swift
import Foundation
import WidgetKit

/// Shared gateway status for Control Center widget display.
final class GatewayState: Sendable {
    static let shared = GatewayState()

    private let defaults = UserDefaults(suiteName: "group.net.fihonline.herald")!

    var isConnected: Bool { defaults.bool(forKey: "gw.connected") }
    var activeJobs: Int { defaults.integer(forKey: "gw.activeJobs") }
    var model: String? { defaults.string(forKey: "gw.model") }
    var version: String? { defaults.string(forKey: "gw.version") }

    func update(connected: Bool? = nil, activeJobs: Int? = nil, model: String? = nil, version: String? = nil) {
        if let v = connected { defaults.set(v, forKey: "gw.connected") }
        if let v = activeJobs { defaults.set(v, forKey: "gw.activeJobs") }
        if let v = model { defaults.set(v, forKey: "gw.model") }
        if let v = version { defaults.set(v, forKey: "gw.version") }
        WidgetCenter.shared.reloadAllTimelines()
    }
}
```

#### 3. Move `RelayAPIClient` to `HeraldWidgets/` target
The `Herald/Services/Support/RelayAPIClient.swift` needs to be added to the `HeraldWidgets` target membership (or a copy placed there) so the Controls extension can use it.

#### 4. Wire the main app to update shared state
In `AppContainer.swift`, after successful connection, update the shared state:
```swift
HeraldAppState.shared.update(
    relayBaseURL: settingsStore.settings.relayConfiguration.activeBaseURLString ?? "",
    accessToken: sessionStore.state.accessToken
)
```

Periodically (on a timer or after key events), update `GatewayState`:
```swift
GatewayState.shared.update(
    connected: hostStore.isHostOnline,
    activeJobs: chatStore.activeStreamCount,
    model: settingsStore.settings.model,
    version: Bundle.main.appVersion
)
```

#### 5. Add `RelayAPIClient.swift` to HeraldWidgets target sources in `project.yml`
```yaml
HeraldWidgets:
  sources:
    - path: HeraldWidgets
    - path: Herald/Services/Support/RelayAPIClient.swift  # Shared with Controls
```

### Acceptance Criteria
- [ ] `HeraldControls` target compiles without errors
- [ ] Control Center shows "Herald GW" widget option
- [ ] Tapping Gateway Status widget calls `/gw/status` and updates display
- [ ] Tapping Restart widget calls `/gw/restart` with target selection
- [ ] Tapping Model Switch widget calls `/gw/model/switch` with model selection

---

## P0-3: Live Activity Fails Because ContentState Mismatch Between App and Widget

### Root Cause

There are **two different versions** of `HeraldActivityAttributes.ContentState`:

| File | Target | Fields |
|------|--------|--------|
| `Herald/Models/HeraldActivityAttributes.swift` | Main app | 6 fields (status, toolName, elapsedSeconds, startDate, sessionType, emoji) |
| `HeraldWidgets/HeraldActivityAttributes.swift` | Widget ext | 11 fields (+ gatewayConnected, activeQueries, modelName, version, cpuPercent, memoryUsedGb, memoryTotalGb, uptimeHours, alertCount) |

The main app creates `Activity<HeraldActivityAttributes>` with the 6-field `ContentState`. When iOS delivers this to the widget extension, the widget's `HeraldActivityAttributes.ContentState` (11 fields) fails to decode because the encoded data doesn't contain the 5 telemetry keys. Swift's compiler-synthesized `Codable` does NOT use default values for missing keys — it throws a `DecodingError.keyNotFound`.

The `LiveActivityService` appears to work (no errors thrown during `Activity.request()`), but the widget silently fails to render.

Additionally, `LiveActivityService.swift:165-167` stores the push token in `UserDefaults` but **never sends it to the relay** — so even if the activity rendered, remote push updates wouldn't work.

### Remedy

#### 1. Unify the two `HeraldActivityAttributes` files
**Keep ONE file** in `HeraldWidgets/` (shared via target membership with the main app). Use the enhanced version with all 11 fields and proper `Codable` defaults:

```swift
// HeraldWidgets/HeraldActivityAttributes.swift (single source of truth)
import ActivityKit
import Foundation

struct HeraldActivityAttributes: ActivityAttributes, Sendable {
    struct ContentState: Codable, Hashable, Sendable {
        // Session state
        var status: String
        var toolName: String?
        var elapsedSeconds: Int
        var startDate: Date?
        var sessionType: String
        var emoji: String?

        // Gateway telemetry (Herald 2.3.0)
        var gatewayConnected: Bool = false
        var activeQueries: Int = 0
        var modelName: String? = nil
        var version: String? = nil
        var cpuPercent: Double = 0.0
        var memoryUsedGb: Double = 0.0
        var memoryTotalGb: Double = 0.0
        var uptimeHours: Double = 0.0
        var alertCount: Int = 0

        // Custom Codable so missing telemetry fields from older encoded
        // states don't cause decode failures.
        enum CodingKeys: String, CodingKey {
            case status, toolName, elapsedSeconds, startDate, sessionType, emoji
            case gatewayConnected, activeQueries, modelName, version
            case cpuPercent, memoryUsedGb, memoryTotalGb, uptimeHours, alertCount
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            status = try container.decode(String.self, forKey: .status)
            toolName = try container.decodeIfPresent(String.self, forKey: .toolName)
            elapsedSeconds = try container.decode(Int.self, forKey: .elapsedSeconds)
            startDate = try container.decodeIfPresent(Date.self, forKey: .startDate)
            sessionType = try container.decode(String.self, forKey: .sessionType)
            emoji = try container.decodeIfPresent(String.self, forKey: .emoji)
            // Telemetry fields — use decodeIfPresent so old states without them
            // (from before this fix) don't break decoding.
            gatewayConnected = try container.decodeIfPresent(Bool.self, forKey: .gatewayConnected) ?? false
            activeQueries = try container.decodeIfPresent(Int.self, forKey: .activeQueries) ?? 0
            modelName = try container.decodeIfPresent(String.self, forKey: .modelName)
            version = try container.decodeIfPresent(String.self, forKey: .version)
            cpuPercent = try container.decodeIfPresent(Double.self, forKey: .cpuPercent) ?? 0.0
            memoryUsedGb = try container.decodeIfPresent(Double.self, forKey: .memoryUsedGb) ?? 0.0
            memoryTotalGb = try container.decodeIfPresent(Double.self, forKey: .memoryTotalGb) ?? 0.0
            uptimeHours = try container.decodeIfPresent(Double.self, forKey: .uptimeHours) ?? 0.0
            alertCount = try container.decodeIfPresent(Int.self, forKey: .alertCount) ?? 0
        }

        // Standard init for app-side creation
        init(status: String, toolName: String? = nil, elapsedSeconds: Int = 0,
             startDate: Date? = nil, sessionType: String = "chat", emoji: String? = nil,
             gatewayConnected: Bool = false, activeQueries: Int = 0,
             modelName: String? = nil, version: String? = nil,
             cpuPercent: Double = 0.0, memoryUsedGb: Double = 0.0,
             memoryTotalGb: Double = 0.0, uptimeHours: Double = 0.0,
             alertCount: Int = 0) {
            self.status = status
            self.toolName = toolName
            self.elapsedSeconds = elapsedSeconds
            self.startDate = startDate
            self.sessionType = sessionType
            self.emoji = emoji
            self.gatewayConnected = gatewayConnected
            self.activeQueries = activeQueries
            self.modelName = modelName
            self.version = version
            self.cpuPercent = cpuPercent
            self.memoryUsedGb = memoryUsedGb
            self.memoryTotalGb = memoryTotalGb
            self.uptimeHours = uptimeHours
            self.alertCount = alertCount
        }
    }

    var agentName: String = "Herald"
}
```

#### 2. Delete the duplicate
Remove `Herald/Models/HeraldActivityAttributes.swift` — the main app target should include the widget's copy via target membership.

#### 3. Add `HeraldWidgets/HeraldActivityAttributes.swift` to main app target
In `project.yml`, ensure the main `Herald` target includes the shared file:
```yaml
Herald:
  sources:
    - path: Herald
      excludes:
        - "**/.DS_Store"
    - path: HeraldWidgets/HeraldActivityAttributes.swift  # Shared with widget
```

#### 4. Wire live activity push token to relay
In `LiveActivityService.swift:165-167`, after storing the token, register it:
```swift
private nonisolated func registerLiveActivityPushTokenSync(_ token: String) {
    UserDefaults.standard.set(token, forKey: "herald.liveActivity.pushToken")
    // Post notification so AppContainer can pick it up and send to relay
    Task { @MainActor in
        NotificationCenter.default.post(
            name: .heraldLiveActivityPushTokenUpdated,
            object: token
        )
    }
}
```

Then in `AppContainer.swift`, handle the notification to call the relay's push registration endpoint.

### Acceptance Criteria
- [ ] Only ONE `HeraldActivityAttributes.swift` exists in the project
- [ ] Live Activity appears on Lock Screen when Herald is thinking/responding
- [ ] Live Activity shows in Dynamic Island during active jobs
- [ ] Live Activity push token is sent to relay
- [ ] Old encoded states (without telemetry fields) decode without errors
- [ ] Telemetry fields populate when gateway data is available

---

## P1-1: Settings Has No Gateway Controls Except "Restart Connection"

### Root Cause

The Settings screen (`SettingsScreen.swift`) has a "Restart Connection" button that does NOT call the gateway control plane — it just reloads the conversation and refreshes host status. The relay exposes 9 gateway control endpoints (`/gw/restart`, `/gw/restart/connector`, `/gw/restart/hermes`, `/gw/model/switch`, `/gw/update`, `/gw/update/check`, `/gw/config/reload`, `/gw/telemetry`, `/gw/logs`) but **none are wired into the iOS UI**.

### Remedy

Add a **Gateway** section to `SettingsScreen.swift` between `relaySection` and `appearanceSection`:

```swift
// MARK: - Gateway

private var gatewaySection: some View {
    SettingsSectionView(title: "Gateway") {
        VStack(spacing: 0) {
            // Gateway status row (connected/offline, model name, active jobs)
            NavigationLink(value: Route.gatewayStatus) {
                HStack(spacing: Design.Spacing.sm) {
                    Image(systemName: "network")
                        .font(.system(size: 14))
                        .foregroundStyle(hostStore.isHostOnline ? Design.Colors.success : Design.Colors.warning)
                        .frame(width: 20, alignment: .center)
                    Text("Gateway Status")
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.foreground)
                    Spacer()
                    Text(hostStore.isHostOnline ? "Online" : "Offline")
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.secondaryForeground)
                    Image(systemName: "chevron.right")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
                .frame(minHeight: Design.Size.minTapTarget)
            }
            .buttonStyle(.plain)

            sectionDivider

            // Restart gateway button
            gatewayActionButton(
                label: "Restart Gateway",
                icon: "arrow.triangle.2.circlepath",
                color: .orange,
                target: "relay"
            )

            sectionDivider

            // Restart connector button
            gatewayActionButton(
                label: "Restart Connector",
                icon: "arrow.triangle.2.circlepath",
                color: .orange,
                target: "connector"
            )

            sectionDivider

            // Restart Hermes button
            gatewayActionButton(
                label: "Restart Hermes Agent",
                icon: "arrow.triangle.2.circlepath",
                color: .orange,
                target: "hermes"
            )

            sectionDivider

            // Model switch picker
            if !hostStore.availableModels.isEmpty {
                HStack(spacing: Design.Spacing.sm) {
                    Image(systemName: "brain.head.profile")
                        .font(.system(size: 14))
                        .foregroundStyle(.purple)
                        .frame(width: 20, alignment: .center)
                    Text("Model")
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.foreground)
                    Spacer()
                    Picker("Model", selection: $selectedModel) {
                        ForEach(hostStore.availableModels, id: \.self) { model in
                            Text(model).tag(model)
                        }
                    }
                    .pickerStyle(.menu)
                    .onChange(of: selectedModel) { _, model in
                        Task { await switchModel(to: model) }
                    }
                }
                .frame(minHeight: Design.Size.minTapTarget)

                sectionDivider
            }

            // View logs entry
            NavigationLink(value: Route.gatewayLogs) {
                HStack(spacing: Design.Spacing.sm) {
                    Image(systemName: "doc.text.magnifyingglass")
                        .font(.system(size: 14))
                        .foregroundStyle(.blue)
                        .frame(width: 20, alignment: .center)
                    Text("View Logs")
                        .font(Design.Typography.callout)
                        .foregroundStyle(Design.Colors.foreground)
                    Spacer()
                    Image(systemName: "chevron.right")
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(Design.Colors.secondaryForeground)
                }
                .frame(minHeight: Design.Size.minTapTarget)
            }
            .buttonStyle(.plain)
        }
    }
}
```

Then add it to the body:
```swift
connectionSection
relaySection
gatewaySection  // <-- NEW
if settingsStore.availableEnvironments.count > 1 {
    environmentSection
}
appearanceSection
```

### Files to create
- `Herald/Features/Settings/GatewayStatusScreen.swift` — telemetry dashboard
- `Herald/Features/Settings/GatewayLogsScreen.swift` — log viewer

### Acceptance Criteria
- [ ] Settings shows Gateway section with status, restart buttons, model picker, logs link
- [ ] Restart buttons call `/gw/restart` with correct target
- [ ] Model picker calls `/gw/model/switch` and shows confirmation
- [ ] Gateway status screen shows telemetry data from `/gw/telemetry`
- [ ] Logs screen shows tail from `/gw/logs` with optional streaming via SSE

---

## P1-2: No Live Gateway Information or Logging Tab

### Root Cause

The relay's `GatewayController` (`relay/app/gateway_control.py`) exposes rich telemetry and log endpoints, but no iOS UI consumes them. The v2.3 changelog documents these as features, but the iOS side was never built.

### Remedy

#### 1. Create `GatewayStatusScreen.swift`
A new view that polls `/gw/telemetry` and displays:
- Connection status (relay ↔ connector ↔ hermes)
- Active job count with job IDs
- Current model name
- Relay version and uptime
- System stats (CPU %, memory used/total GB)
- Alert count and recent alerts
- Auto-refresh every 5s (or SSE stream via `/gw/telemetry/stream`)

#### 2. Create `GatewayLogsScreen.swift`
A new view that:
- Fetches recent logs from `/gw/logs?lines=100&level=info`
- Optionally subscribes to `/gw/logs/stream` for live tail (SSE)
- Provides level filter (debug/info/warning/error)
- Search within logs
- Share/export button

#### 3. Add routes to `Router.swift`
```swift
enum Route: Hashable {
    // ... existing cases ...
    case gatewayStatus
    case gatewayLogs
}
```

#### 4. Add `HeraldHostStore` methods
```swift
func fetchTelemetry() async throws -> GatewayTelemetry { ... }
func fetchLogs(lines: Int, level: String) async throws -> [LogLine] { ... }
func streamLogs(level: String) -> AsyncStream<LogLine> { ... }
```

### Acceptance Criteria
- [ ] "Gateway Status" is accessible from Settings → Gateway
- [ ] Dashboard shows live telemetry (connected, jobs, model, stats, alerts)
- [ ] "View Logs" is accessible from Settings → Gateway
- [ ] Log viewer shows last N lines with level filtering
- [ ] Live log streaming works (optional, nice-to-have)

---

## P1-3: Streaming Reliability Improvements

### Root Cause

The streaming pipeline is structurally sound but has several failure modes that make it feel broken to users:

1. **120s watchdog**: If SSE silently drops (proxy timeout, network blip), the user sees a blank placeholder for 2 minutes before any feedback.
2. **`replyState != "pending"` fallback**: If the relay returns a non-pending response (e.g., CLI adapter mode), streaming is skipped entirely — the full response arrives at once, but with no incremental display.
3. **No streaming health indicators**: Users can't tell if streaming is working, stuck, or reconnecting.
4. **Backoff starts at 1s, doubles to 60s**: During extended outages, reconnect takes a full minute between attempts.

### Remedy

#### 1. Reduce watchdog timeout from 120s → 30s
`JobStreamCoordinator.swift:48`:
```swift
// Change:
private static let watchdogTimeoutSeconds: TimeInterval = 120.0
// To:
private static let watchdogTimeoutSeconds: TimeInterval = 30.0
```

#### 2. Surface streaming status in UI
In `ChatStore`, expose the current streaming phase:
```swift
enum StreamingPhase {
    case idle
    case sending          // POST /messages
    case waitingForJob    // Job accepted, waiting for first event
    case streaming        // Receiving deltas
    case reconnecting     // Transport dropped, retrying
    case stalled          // Watchdog about to fire
}
```

Show this in the chat UI (subtle indicator near the input bar or on the placeholder message).

#### 3. Cap backoff at 15s (not 60s)
`JobStreamCoordinator.swift:42`:
```swift
// Change:
private static let maxBackoff: TimeInterval = 60.0
// To:
private static let maxBackoff: TimeInterval = 15.0
```

#### 4. Add streaming diagnostics to the settings Gateway section
Show: SSE connection state, last event timestamp, reconnect count, current backoff.

### Acceptance Criteria
- [ ] Watchdog fires at 30s instead of 120s
- [ ] Streaming phase indicator visible in chat UI
- [ ] Max reconnect backoff is 15s
- [ ] Streaming health visible in Gateway Status screen

---

## Deployment Order

### Phase 1 — The Must-Fix (build 72, ASAP)
| # | Issue | Changes | Risk |
|---|-------|---------|------|
| P0-1 | APNS environment | `project.yml:61`, `AppContainer.swift:800`, 3 deploy scripts, DB UPDATE | Low — isolated string changes |
| P0-2 | Control Center crash | Create `HeraldAppState`, `GatewayState` in Widgets target, move `RelayAPIClient` | Medium — new files + target membership |
| P0-3 | Live Activity mismatch | Unify `HeraldActivityAttributes`, custom Codable, delete duplicate | Medium — touches ActivityAttributes decoding |

### Phase 2 — The Should-Fix (build 73)
| # | Issue | Changes | Risk |
|---|-------|---------|------|
| P1-1 | Gateway settings | Add gatewaySection to SettingsScreen, gateway action buttons, model picker | Low — additive UI |
| P1-2 | GW info/logs tab | Create GatewayStatusScreen + GatewayLogsScreen, add routes, host store methods | Medium — new screens |
| P1-3 | Streaming reliability | Reduce watchdog, cap backoff, add streaming phase indicator | Low — config changes + UI indicator |

### Deploy Notes

1. **iOS build**: Bump `MARKETING_VERSION` to 2.3.1 + `CURRENT_PROJECT_VERSION` to 72 in `project.yml`, regenerate Xcode project (`xcodegen generate`), unlock keychain, strip entitlements per [[herald-testflight-procedure]]
2. **Relay**: After code changes, rsync `relay/` to `fihadmin@herald-host.internal:/home/agent/deploy/hermes-relay/`, `docker compose build --no-cache relay && docker compose up -d relay`
3. **DB cleanup**: Run the UPDATE on herald-postgres to fix push_environment on existing registrations
4. **App Group**: Verify `group.net.fihonline.herald` is enabled in the provisioning profile (already in entitlements at `project.yml:63-64`)

## Related

- [[herald-deploy-topology]] — prod relay on fih-ai-host .118
- [[herald-testflight-procedure]] — full build/upload pipeline
- [[hermes-ios-app]] — relay chain, connector, Hermes agent
- [[herald-marching-orders-process]] — how to format Mimo work specs from this plan
