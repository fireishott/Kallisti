# Herald Build 17 — Bug Remedy Document

> Generated 2026-07-26 from verified source inspection.
> Pass this document to Claude Code CLI for implementation.

---

## Bug 1: Stream stalled retrying (yellow banner)

**Symptom:** Yellow "Stream stalled — retrying…" banner appears during normal streaming.

**Root cause:** JobStreamCoordinator's SSE-level watchdog fires at **30 seconds** of silence (line 49). Large local models take 30–45 seconds for prefill on constrained hardware (acknowledged in ChatStore comment at line 55–58). The relay may not send heartbeats frequently enough during model loading, so the 30s watchdog expires and shows the banner even though the job is healthy.

**Files:**
- `[project-root]/Herald/Herald/Services/Live/JobStreamCoordinator.swift:49` — watchdog timeout constant
- `[project-root]/Herald/Herald/Stores/ChatStore.swift:60` — ChatStore watchdog (90s, separate concern)
- `[project-root]/Herald/Herald/Features/Chat/ChatScreen.swift:70` — banner display condition

**Fix:**
1. **Increase JobStreamCoordinator watchdog from 30s → 60s** (line 49):
   ```swift
   private static let watchdogTimeoutSeconds: TimeInterval = 60.0
   ```
2. **Verify relay sends heartbeats** — check `relay/app/` that the SSE transport emits heartbeat/comment lines at least every 15s during model prefill. The JobStreamCoordinator at line 110 resets its watchdog on ANY SSE data, including comment keepalives.

---

## Bug 2: Health permissions inconsistent across releases

**Symptom:** Health permission status flips between "Authorized", "Denied", and "Not Determined" across launches/builds.

**Root cause:** In `LiveHealthService.requestAuthorization()` (lines 94–108), if `HKHealthStore.requestAuthorization()` throws for ANY reason (including background-delivery config failure), the status is set to `.denied` AND the `healthAuthRequestedKey` flag is NOT persisted. On next launch, `refreshAuthorizationStatus()` (line 131) sees no flag → returns `.notDetermined`. This causes oscillation: `.denied` → `.notDetermined` → `.authorized` (if user re-grants) → `.denied` (if background delivery fails again).

**Files:**
- `[project-root]/Herald/Herald/Services/Live/LiveHealthService.swift:94–108` — authorization request + error handling
- `[project-root]/Herald/Herald/Services/Live/LiveHealthService.swift:110–148` — refresh flow
- `[project-root]/Herald/Herald/Features/Permissions/PermissionsScreen.swift:17–22` — always calls request flow for health

**Fix:**
1. **Persist the flag before background delivery** — set `healthAuthRequestedKey` immediately after `requestAuthorization` succeeds, BEFORE `configureBackgroundDeliveryIfNeeded()`:
   ```swift
   // LiveHealthService.swift ~line 98
   try await store.requestAuthorization(toShare: [], read: types)
   authorizationStatus = .authorized
   UserDefaults.standard.set(true, forKey: Self.healthAuthRequestedKey)  // Keep this FIRST
   // Background delivery failure should not revert authorization status
   await configureBackgroundDeliveryIfNeeded()  // Failure here is non-fatal
   ```
2. **Don't set `.denied` on non-denial errors** — catch only the specific denial case:
   ```swift
   } catch {
       // Only treat as denied if the user actually denied.
       // System errors (background delivery, etc.) should preserve the
       // previous state rather than flipping to denied.
       if previouslyRequested {
           authorizationStatus = .authorized  // preserve
       } else {
           authorizationStatus = .notDetermined
       }
   }
   ```

---

## Bug 3: Chat rename quit working

**Symptom:** Can't rename chat sessions.

**Root cause (iPhone):** The iPhone session drawer context menu has **no Rename option at all**. It only offers Pin/Unpin, Archive, Delete.

**Root cause (iPad, if applicable):** The rename alert is triggered by setting `renamingSession` state from a context menu button. On iOS 18, the context menu dismissal animation can race with the alert presentation, causing the alert to not appear.

**Files:**
- `[project-root]/Herald/Herald/Features/Sidebar/iPhoneSessionDrawer.swift:318–335` — context menu (NO rename)
- `[project-root]/Herald/Herald/Features/Sidebar/iPadSidebarView.swift:468–473` — rename context menu button
- `[project-root]/Herald/Herald/Features/Sidebar/iPadSidebarView.swift:104–116` — rename alert
- `[project-root]/Herald/Herald/Stores/SessionListStore.swift:283–299` — `renameSession` network call

**Fix (iPhone):**
1. **Add Rename button to iPhone context menu** in `iPhoneSessionDrawer.swift` (~line 318):
   ```swift
   .contextMenu {
       Button {
           renameText = session.title
           renamingSession = session
       } label: {
           Label("Rename", systemImage: "pencil")
       }
       // ... existing Pin, Archive, Delete buttons
   }
   ```
2. **Add the rename alert** to the iPhone drawer (same pattern as iPad):
   ```swift
   @State private var renamingSession: SessionSummary?
   @State private var renameText = ""
   // ...
   .alert("Rename Session", isPresented: .init(
       get: { renamingSession != nil },
       set: { if !$0 { renamingSession = nil } }
   )) {
       TextField("Title", text: $renameText)
       Button("Rename") {
           if let session = renamingSession {
               Task { await sessionStore.renameSession(session, newTitle: renameText) }
           }
           renamingSession = nil
       }
       Button("Cancel", role: .cancel) { renamingSession = nil }
   }
   ```

**Fix (iPad, defensive):**
1. **Add a short delay** before presenting the alert after context menu dismissal:
   ```swift
   Button {
       renameText = session.title
       Task {
           try? await Task.sleep(for: .milliseconds(300))
           renamingSession = session
       }
   } label: {
       Label("Rename", systemImage: "pencil")
   }
   ```

---

## Bug 4: Model context display is fabricated

**Symptom:** Context window/usage shown in UI doesn't match reality.

**Root cause:** The old `estimateContextIfMissing()` was REMOVED (ChatStore line 1548–1549), which is good — context % now only comes from the relay (line 515–516). However, `resolvedContextWindow()` (lines 1146–1165) still fabricates a context window via substring matching on the model name. Many models don't contain "128k" or "qwen" in their name string, so the 131,072 default is wrong for small-context models (4K, 8K, 32K).

**Files:**
- `[project-root]/Herald/Herald/Stores/ChatStore.swift:1146–1165` — `resolvedContextWindow` with substring guessing
- `[project-root]/Herald/Herald/Stores/ChatStore.swift:143–149` — stale context cleared on load (already fixed)
- `[project-root]/Herald/Herald/Stores/ChatStore.swift:513–516` — context % set from relay SSE metadata

**Fix:**
1. **Return nil instead of guessing** when the relay hasn't provided a context window:
   ```swift
   func resolvedContextWindow(fallbackModelName: String?) -> Int? {
       // Prefer relay-provided context window from SSE metadata
       if let ctx = contextWindow, ctx > 0 { return ctx }
       // ModelStore may have the context window from the catalog
       // (if loaded from config.yaml providers)
       return nil  // Don't fabricate
   }
   ```
2. **Show "Unknown" instead of fabricated %** in the ContextBar/UI when `contextWindow` is nil.
3. **Ensure the relay includes context info** in the SSE `done` event (JobStreamCoordinator lines 362–365 already extracts `context.window` and `context.used` from the done event data).

---

## Bug 5: Herald Hub useless (profiles and models)

**Symptom:** Herald Hub (HeraldSelectorSheet) shows empty model/profile lists or no feedback.

**Root cause:** The Hub (now `HeraldSelectorSheet`) loads profiles and models on appear via `.task {}`. If the relay is slow or the API calls fail:
- `errorMessage` is set on the stores but the sheet only shows it in a dismissible banner — easy to miss
- Empty states show "No Models" / "No Profiles" with no indication of WHY
- No pull-to-refresh — stuck after failed load
- No loading indicator in the empty state

**Files:**
- `[project-root]/Herald/Herald/Features/Chat/HeraldSelectorSheet.swift` — Hub UI (full file)
- `[project-root]/Herald/Herald/Stores/ModelStore.swift:75–102` — model loading
- `[project-root]/Herald/Herald/Stores/ProfileStore.swift:54–91` — profile loading

**Fix:**
1. **Show errors in the empty state** instead of just "No Models":
   ```swift
   // In modelList/profileList, check errorMessage before showing empty
   if modelStore.models.isEmpty {
       if let error = modelStore.errorMessage {
           ContentUnavailableView("Couldn't Load Models",
               systemImage: "wifi.slash",
               description: Text(error))
       } else if modelStore.isLoading {
           ProgressView("Loading models…")
       } else {
           ContentUnavailableView("No Models", systemImage: "cpu")
       }
   }
   ```
2. **Add `.refreshable {}`** to the List/ScrollView for pull-to-refresh.
3. **Show loading state** while `isLoading` is true.

---

## Bug 6: Inbox has messages but Open button does nothing

**Symptom:** Inbox items show an "Open" button. Tapping it does nothing.

**Root cause:** `InboxItemRow`'s `onPrimaryAction` calls `inboxStore.performPrimaryAction(for: item)` which submits an action to the relay (`POST /inbox/action` with `actionID: "approve"`) — it does NOT navigate to a conversation. The `onOpenDetails` closure in `InboxScreen` is explicitly a no-op (line 37: `// Inbox detail navigation deprecated — no-op`). There is no navigation logic anywhere in the inbox flow.

Additionally, `InboxItem.payload` is a `[String: String]?` dictionary that could contain a `conversationId` key, but it's never read for navigation purposes.

**Files:**
- `[project-root]/Herald/Herald/Features/Inbox/InboxItemRow.swift:72–81` — Open button calls `onPrimaryAction`
- `[project-root]/Herald/Herald/Features/Inbox/InboxScreen.swift:30–32` — primary action → `performPrimaryAction(for:)`
- `[project-root]/Herald/Herald/Stores/InboxStore.swift:59–62` — `performPrimaryAction` → `submitAction` with "approve"
- `[project-root]/Herald/Herald/Features/Inbox/InboxItemDetailSheet.swift` — detail sheet (exists but not wired up)

**Fix:**
1. **Add navigation logic to InboxStore or InboxScreen:**
   ```swift
   // In InboxStore
   func openConversation(for item: InboxItem) {
       guard let convIdString = item.payload?["conversationId"],
             let convId = UUID(uuidString: convIdString) else { return }
       // Navigate via TabRouter or AppContainer
   }
   ```
2. **Wire up InboxItemRow's `onOpenDetails`** to actually navigate (unlike the current no-op at InboxScreen line 37).
3. **For actionable items (approvals), keep the current action-submit behavior** but add a secondary "Open Chat" button.
4. **For message-type items, make primary action = navigate** to the conversation instead of submitting "approve".

---

## Bug 7: Restart Hermes Agent — "error: not found"

**Symptom:** Tapping "Restart Hermes Agent" in Settings shows "Error: not found".

**Root cause:** The `RelayAPIClient` constructs URLs as `{baseURL}/{path}` where `baseURL` is the relay URL (e.g., `https://herald-host.internal:8010/v1`) and `path` is stripped of leading/trailing slashes (RelayAPIClient line 268). So `path: "gw/restart"` becomes `https://herald-host.internal:8010/v1/gw/restart`.

The relay likely registers its gateway blueprint at `/gw` (not `/v1/gw`). The correct URL should be `https://herald-host.internal:8010/gw/restart`.

Additionally, the `RestartResponse` struct (SettingsScreen lines 494–501) has a nested `Data` struct. If `RelayAPIClient.post` auto-unwraps a JSON `data` envelope, the nested decode will fail because the relay returns `{"restarting": true, "target": "hermes"}` directly (no outer `data` key).

**Files:**
- `[project-root]/Herald/Herald/Features/Settings/SettingsScreen.swift:474–523` — `restartGateway` function
- `[project-root]/Herald/Herald/Features/Settings/SettingsScreen.swift:493–501` — `RestartResponse` struct
- `[project-root]/Herald/Herald/Services/Support/RelayAPIClient.swift:262–273` — URL construction (line 268 strips slashes, line 271 concatenates)
- `[project-root]/Herald/HeraldControls/RestartGatewayControl.swift:46–56` — Control Widget restart (same nested struct pattern)
- `[project-root]/Herald/relay/app/gateway_control.py` — relay gateway blueprint prefix

**Fix (two options):**

**Option A (iOS-side):** Add a `RelayAPIClient` initializer/method that uses the raw host URL without `/v1`:
```swift
// In RelayAPIClient, add:
func postToGateway<Body: Encodable, T: Decodable>(
    path: String, body: Body, accessToken: String? = nil
) async throws -> T {
    // Use baseURL without /v1 for gateway routes
    let rawBase = baseURLProvider()
        .trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        .replacingOccurrences(of: "/v1", with: "")
    // ... construct URL with rawBase
}
```
Then in `restartGateway`, use `postToGateway` instead of `post`.

**Option B (relay-side, recommended):** Register the gateway blueprint at `/v1/gw` in the relay:
```python
# In relay/app/__init__.py or relay/app/main.py
app.register_blueprint(gateway_bp, url_prefix='/v1/gw')
```

**Also fix:** Flatten `RestartResponse` (SettingsScreen lines 494–501 and RestartGatewayControl lines 46–53):
```swift
struct RestartResponse: Decodable {
    let restarting: Bool
    let target: String
    let message: String?
}
```

---

## Bug 8: View Logs still doesn't work

**Symptom:** "View Logs" navigates to a blank/stuck screen.

**Root cause:** `DashboardLogService.connectAndStream()` (line 129) uses `for try await line in bytes.lines` — the `URLSession.AsyncBytes.lines` API is known to hang indefinitely when the server doesn't flush after every byte. Most SSE servers buffer output, so the async iterator waits forever for the next line that never comes because the buffer isn't full.

Additionally, the `baseURLProvider` for DashboardLogService must point to the **dashboard** port (9119), not the relay port (8010). If this is misconfigured, the connection fails silently.

**Files:**
- `[project-root]/Herald/Herald/Services/Support/DashboardLogService.swift:98–156` — `connectAndStream` with `bytes.lines`
- `[project-root]/Herald/Herald/Services/Support/DashboardLogService.swift:44–49` — `baseURLProvider` init
- `[project-root]/Herald/Herald/Features/Sidebar/LiveLogView.swift` — log viewer UI
- `[project-root]/Herald/Herald/Features/Settings/GatewayLogsScreen.swift` — gateway logs screen

**Fix:**
1. **Replace `bytes.lines` with a URLSessionDataDelegate** approach in `DashboardLogService`:
   ```swift
   private func connectAndStream() async throws {
       let baseURL = baseURLProvider()
       guard let url = URL(string: "\(baseURL)/logs/stream") else {
           throw URLError(.badURL)
       }
       
       let delegate = StreamingDataDelegate()
       let session = URLSession(configuration: .default, delegate: delegate, delegateQueue: nil)
       var request = URLRequest(url: url)
       request.setValue("text/event-stream", forHTTPHeaderField: "Accept")
       
       let (bytes, _) = try await session.bytes(for: request)
       // Use delegate-based buffering instead of bytes.lines
       // ... process via delegate callbacks
   }
   ```
   The `RelayAPIClient` already has `StreamingDataDelegate` and `sseLines(from:)` implemented — reuse those.

2. **Verify dashboard URL construction** — ensure `baseURLProvider()` returns the dashboard URL (port 9119) with basic auth credentials, not the relay URL.

---

## Bug 9: Check for Update / Update Agent do nothing

**Symptom:** Tapping "Check for Updates" or "Update Agent" gives no feedback — no success, no error, nothing.

**Root cause:** Both functions make API calls but:
1. The response is decoded then **discarded** (`let _: UpdateStatus = try await...`)
2. The catch blocks are **empty** (`} catch { // Silently fail }`)
3. No `@State` properties are updated on success or failure
4. This means **zero UI feedback** — the user can't tell if anything happened

Additionally, the same `/v1/gw/...` URL prefix issue from Bug 7 may mean the requests hit the wrong endpoint and fail silently.

**Files:**
- `[project-root]/Herald/Herald/Features/Settings/SettingsScreen.swift:392–413` — `checkForUpdates`
- `[project-root]/Herald/Herald/Features/Settings/SettingsScreen.swift:415–436` — `updateAgent`

**Fix:**
1. **Add `@State` properties for feedback:**
   ```swift
   @State private var updateCheckResult: String?
   @State private var updateAgentResult: String?
   @State private var isCheckingForUpdate = false
   @State private var isUpdatingAgent = false
   ```

2. **Update state on response:**
   ```swift
   private func checkForUpdates() async {
       // ... setup ...
       isCheckingForUpdate = true
       updateCheckResult = nil
       defer { isCheckingForUpdate = false }
       
       do {
           let status: UpdateStatus = try await client.post(
               path: "gw/update/check", body: EmptyRequest(), accessToken: token
           )
           updateCheckResult = status.message ?? status.status ?? "Update check complete"
       } catch {
           updateCheckResult = "Check failed: \(error.localizedDescription)"
       }
   }
   ```

3. **Show the result in the UI** — add a Text view next to the button showing `updateCheckResult` when non-nil, with auto-dismiss after 5 seconds.

4. **Fix the URL path** (same as Bug 7) — use `/gw/update/check` not `/v1/gw/update/check`.

---

## Bug 10: Push says "Not Registered"

**Symptom:** Settings shows "Not Registered" for push notifications even after granting permission.

**Root cause:** The push status line (SettingsScreen lines 603–609) uses:
```swift
value: UIApplication.shared.isRegisteredForRemoteNotifications
    ? "Registered" : "Not Registered"
```
This is a **one-time snapshot** evaluated when the view body renders. `UIApplication.shared.isRegisteredForRemoteNotifications` returns the registration state AT THAT MOMENT — it does not update when registration completes asynchronously. Since APNs registration is async (requires network round-trip to Apple), the view almost always captures the pre-registration state.

The `AppSessionStore` likely has a proper dynamic `pushTokenRegistered` state that IS updated on registration completion. The Settings UI isn't using it.

**Files:**
- `[project-root]/Herald/Herald/Features/Settings/SettingsScreen.swift:603–609` — push status row
- `[project-root]/Herald/Herald/Stores/AppContainer.swift` — push registration coordinator (has dynamic state)
- `[project-root]/Herald/Herald/Services/Support/PushRegistrationCoordinator.swift` — registration flow

**Fix:**
1. **Bind to the dynamic state from AppSessionStore:**
   ```swift
   // Replace line 608-609:
   value: sessionStore.state.pushTokenRegistered
       ? "Registered" : "Not Registered"
   ```
   (Verify the exact property name — check `AppSessionState.swift` for the push token field.)

2. **Ensure `AppSessionStore.state` is `@Observable`** so the Settings view re-renders when `pushTokenRegistered` changes.

---

## Summary Table

| # | Bug | Severity | File(s) to Edit |
|---|-----|----------|-----------------|
| 1 | Stream stalled banner too aggressive | Medium | `JobStreamCoordinator.swift:49` |
| 2 | Health permissions inconsistent | High | `LiveHealthService.swift:94–108` |
| 3 | Chat rename missing/broken | High | `iPhoneSessionDrawer.swift:318`, `iPadSidebarView.swift:468` |
| 4 | Model context fabricated | Medium | `ChatStore.swift:1146–1165` |
| 5 | Hub empty with no feedback | Medium | `HeraldSelectorSheet.swift` |
| 6 | Inbox Open does nothing | High | `InboxStore.swift:59`, `InboxScreen.swift:30–37` |
| 7 | Restart Hermes "not found" | High | `SettingsScreen.swift:474–523`, relay `gateway_control.py` |
| 8 | View Logs hangs | High | `DashboardLogService.swift:98–156` |
| 9 | Update check/apply no feedback | Medium | `SettingsScreen.swift:392–436` |
| 10 | Push "Not Registered" stale | Medium | `SettingsScreen.swift:608–609` |

---

## Implementation Order (recommended)

1. **Bug 7 (Restart Hermes)** — quick relay-side fix (move `/gw` → `/v1/gw`) + flatten RestartResponse struct
2. **Bug 8 (View Logs)** — replace `bytes.lines` with delegate-based streaming
3. **Bug 3 (Chat rename)** — add Rename to iPhone context menu + delay on iPad
4. **Bug 6 (Inbox Open)** — wire up navigation from inbox items to conversations
5. **Bug 2 (Health permissions)** — fix error handling in authorization flow
6. **Bug 1 (Stream banner)** — increase watchdog timeout
7. **Bug 9 (Update feedback)** — add result state and UI feedback
8. **Bug 10 (Push status)** — bind to dynamic state
9. **Bug 4 (Context display)** — remove context window guessing
10. **Bug 5 (Hub UX)** — add error states and pull-to-refresh
