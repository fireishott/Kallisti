# Herald v2.2.5 — Research & Remedy Deployment Plan

**Date:** 2026-07-24  
**Current build:** v2.2.5 (build 62)  
**Crashes analyzed:** 3 xccrashpoints (v2.2.3–v2.2.4)  
**Reported bugs:** 12

---

## Crash Analysis Summary

### Crash 1: TalkAudioCapture — Actor Isolation Violation (v2.2.4, build 61)

- **Crash point:** `DLv6v_kVaI3SezmMxhpnMD` — `UIEventFetcher.m:1333`
- **Exception:** `EXC_BREAKPOINT (SIGTRAP)` on Thread 2
- **Root cause:** `AVAudioNode.installTap` callback fires on a **realtime audio thread**. The Swift 6 compiler infers `@MainActor` isolation on closures created within a `@MainActor` context. Even though the tap callback at `TalkAudioCapture.swift:90` only accesses the `TapAccumulator` (not `self`), the closure itself may be flagged as `@MainActor`-isolated in Swift 6 language mode, triggering `dispatch_assert_queue_fail` → `swift_task_checkIsolatedSwift`.
- **Fix status:** The `TapAccumulator` pattern (v2.2.1) mitigates but may not fully resolve the issue under Swift 6 complete concurrency checking. The crash appears 13 seconds after launch (audio init).
- **Severity:** P0 — crash on Talk mode start

### Crash 2: AttributeGraph `AG::vector<Trace>::size()` (v2.2.x)

- **Crash point:** `DX92OkzRhoXVlVzJ2c-Q3A` — SwiftUI AttributeGraph crash
- **Likely cause:** View state inconsistency — a `@Observable` property update triggers a view body recomputation while the AttributeGraph is in an invalid state. Often caused by state mutations during view rendering or from non-main-actor callbacks.
- **Severity:** P1 — intermittent SwiftUI crash

### Crash 3: caulk `semaphore::timed_wait` (v2.2.x)

- **Crash point:** `CbXJ3MievpXVcQpWyz5mj5` — Audio system semaphore timeout
- **Likely cause:** Audio engine resource exhaustion or deadlock. The caulk audio worker threads hang waiting on a semaphore. Often related to audio session configuration or rapid start/stop cycles.
- **Severity:** P1 — intermittent audio crash

---

## Bug Catalog & Remedies

### P0 — Crashing / Data Loss

#### B1. TalkAudioCapture Crash on Start

**Finding:** The `TapAccumulator` pattern in `TalkAudioCapture.swift:90-105` captures `accumulator` (a local `let`) and does not reference `self`. However, Swift 6's complete concurrency checking may still infer `@MainActor` isolation on the non-`@Sendable` escaping closure because it is created in a `@MainActor` context.

**Remedy:**
1. Mark the tap callback closure as explicitly `nonisolated` by extracting it to a file-level function or a `nonisolated` static method
2. Or: Use `withoutActuallyEscaping` / `UnsafeContinuation` to forward buffers without actor hopping
3. **Quick fix:** Add `@Sendable` to the closure type by wrapping in a `nonisolated` static:

```swift
// In TalkAudioCapture, add:
private nonisolated static func makeTapHandler(
    accumulator: TapAccumulator
) -> (AVAudioPCMBuffer, AVAudioTime) -> Void {
    return { buffer, _ in
        let channelData = buffer.floatChannelData?[0]
        let frames = buffer.frameLength
        let power: Float
        if let channelData, frames > 0 {
            var rms: Float = 0
            vDSP_measqv(channelData, 1, &rms, vDSP_Length(frames))
            power = 10 * log10(rms + 1e-10)
        } else {
            power = -160.0
        }
        accumulator.append(buffer, power: power)
    }
}

// Then in startRecording():
inputNode.installTap(onBus: 0, bufferSize: 1024, format: format,
    tap: Self.makeTapHandler(accumulator: accumulator))
```

**Files:** `Herald/Services/Live/TalkAudioCapture.swift:70-105`  
**Verification:** Launch Talk mode on iOS 26.6, confirm no crash within 60s. Run with Thread Sanitizer enabled.

---

### P1 — Core Functionality Broken

#### B2. Streaming Not Working / Delayed Responses

**Finding:** Multiple failure points in the streaming pipeline:

1. **Non-connector adapters skip streaming entirely** — `LiveHeraldClient.sendStreaming()` line 199: if `response.replyState != "pending"`, yields `.finished()` directly. The relay forces pending only when `herald_adapter == "connector"`. CLI adapter returns complete responses synchronously.

2. **Delta coalescing adds latency** — 33ms flush interval per chunk. For long responses, this adds up to 2+ seconds of additional perceived delay.

3. **`reloadConversationForStreaming()` on completion** — An extra HTTP round-trip after the SSE stream already delivered the final message.

4. **90s watchdog + 30s grace period** — A total of 120s before surfacing "no response" to user.

**Remedy:**
1. Reduce watchdog from 90s to 30s for the first progress event (text/reasoning/tool), since the connector's own watchdog is 120s
2. Reduce delta flush interval from 33ms to 16ms (60fps cap instead of 30fps)
3. Remove `reloadConversationForStreaming()` call — the SSE stream's `.finished` event already carries the final message, usage, and context
4. For CLI adapter: implement subprocess line-buffered stdout reading so deltas stream as they arrive instead of blocking until exit

**Files:**
- `Herald/Stores/ChatStore.swift:39` (watchdog timeout)
- `Herald/Stores/ChatStore.swift:58` (flush interval)
- `Herald/Services/Live/LiveHeraldClient.swift:272-280` (reload on completion)
- `relay/app/herald_adapter.py` (CLI adapter — synchronous subprocess)

#### B3. Reasoning/Thinking Disappears

**Finding:** Two separate reasoning paths that conflict:

1. **Dedicated `reasoningDelta` events** — buffered and displayed in `ReasoningView`. Works when the connector sends discrete reasoning deltas.
2. **Inline `<think>` tags in content** — parsed during streaming by `ThinkingBlockView`, then **stripped by regex** on `.finished` (ChatStore.swift:380-384).

If the LLM outputs reasoning ONLY via inline `<think>` tags (DeepSeek, Qwen models), the reasoning is visible during streaming but **disappears** when streaming completes because the regex strips it. The stripped content is NOT copied to `message.reasoning`.

**Remedy:**
1. On `.finished`, before stripping `<think>` tags from content, extract the content and save it to `message.reasoning`
2. In `JobEventReducer`, when processing the terminal `done` event, capture `canonicalText` separately from the content-with-reasoning
3. Or: in the connector, parse `<think>` blocks and emit them as `reasoning_delta` events so they follow the dedicated channel

**Files:**
- `Herald/Stores/ChatStore.swift:380-384` (think tag stripping)
- `Herald/Features/Chat/Renderers/ThinkingBlockView.swift`
- `Herald/Features/Chat/ReasoningView.swift`
- `relay/app/main.py` (job event publishing)
- `connector/src/herald_connector/` (connector-side event construction)

#### B4. Push Notifications Not Delivered

**Finding:** Three separate failure modes:

1. **APNs h2 package missing** — Fixed in v2.2.4 (`httpx[http2]` installed in connector venv)
2. **APNs environment mismatch** — Fixed in v2.2.5 (removed `APNS_ENVIRONMENT=development` override)
3. **Self-hosted mode exclusion** — `RelayConnectionMode.reliesOnOfficialPushRelay` returns `false` for `.tailscale` and `.selfHostedRelay` modes. Push broker path is never taken. Direct APNs requires `APNS_KEY_PATH`, `APNS_KEY_ID`, `APNS_TEAM_ID` env vars on the relay.
4. **Foreground check** — `maybe_send_message_push()` skips devices with `app_state == "foreground"` within `stale_seconds`. If the check is too aggressive, backgrounded-but-recently-foreground devices won't get pushes.
5. **Connector-direct registration** — `registerWithConnector` calls `register_push_device` MCP tool on Hermes host, which may not exist as a standard tool.

**Remedy:**
1. Verify APNs key is configured on the production relay: check `APNS_KEY_PATH`, `APNS_KEY_ID`, `APNS_TEAM_ID` env vars
2. Reduce `stale_seconds` from default to 30s so backgrounded devices get pushes sooner
3. Fall back to local notification when APNs push fails (already partially done in ChatStore line 421-440, but only for `.background` state)
4. Add connector-direct push registration as a fallback when direct APNs is unavailable
5. Add push delivery status to the iOS debug/status panel so users can verify

**Files:**
- `relay/app/main.py:462-543` (push sending logic)
- `Herald/Services/Support/PushRegistrationCoordinator.swift:130-168` (connector-direct registration)
- `Herald/Models/UserSettings.swift:131-133` (reliesOnOfficialPushRelay)

#### B5. `/new` Routes to Random Chats

**Finding:** `/new` calls `ChatStore.clearConversation()` which hits `POST /conversations/current/clear` — this **clears the current conversation in-place** rather than creating a **new session**. The relay's "current conversation" is a single mutable pointer per device. Race conditions:

1. `clearConversation()` sets `currentConversation = nil` on line 336 of `LiveHeraldClient`, then makes the API call
2. While `currentConversation` is nil, any concurrent operation (polling, reconnect, foreground handler) calls `loadConversationIfNeeded()`, which may load a stale cached conversation or a different conversation from the relay
3. After `clearConversation()` returns, the `needsServerRefresh` flag forces a reload, but the reload targets `GET /conversations/current`, which may have shifted

**Remedy:**
1. Replace `clearConversation()` call in `/new` handler with `SessionListStore.createNewSession()` which properly calls `POST /sessions` and switches to the new session
2. Update the context warning banner's "New" button to use the same path
3. Remove `POST /conversations/current/clear` from the relay API — force session-based semantics everywhere
4. Make `currentConversation` atomic: set the new conversation BEFORE clearing the old one (never leave it nil mid-flight)

```swift
// ChatScreen.swift:1007 — change:
case "new", "reset":
    Task { await performClear() }  // OLD: clears in-place
// to:
case "new", "reset":
    Task { await createNewSessionAndSwitch() }  // NEW: creates fresh session

private func createNewSessionAndSwitch() async {
    guard let sessionListStore = sessionListStore else { return }
    await sessionListStore.createNewSession()
}
```

**Files:**
- `Herald/Features/Chat/ChatScreen.swift:1007, 1083-1094` (slash command handler + performClear)
- `Herald/Stores/ChatStore.swift:612-632` (clearConversation)
- `Herald/Services/Live/LiveHeraldClient.swift:332-347` (API call)
- `Herald/Stores/SessionListStore.swift:181-189` (createNewSession — already implemented!)

#### B6. Thinking → Haptic → No Visible Response

**Finding:** Race condition in streaming completion sequence:
1. `.finished` arrives → placeholder replaced with resolved message
2. `activeStreams.removeValue(forKey:)` → `streamingMessageID` transitions to nil
3. `ChatScreen.onChange(of: streamingMessageID)` fires → `HapticEngine.responseReceived()` plays
4. The message replacement at step 1 races with the `@Observable` notification that triggers the view redraw
5. If the haptic fires before the view redraw completes, the user feels the haptic but sees no change

**Remedy:**
1. Delay haptic by 100ms after streaming end to ensure view has redrawn
2. Or: trigger haptic from the message count observer (`onChange(of: messages.count)`) only when the LAST message is `.delivered` and not from user
3. Or: add a `didCompleteStreaming` flag that ChatScreen observes instead of `streamingMessageID`

```swift
// ChatScreen.swift:124-138 — change:
.onChange(of: chatStore.streamingMessageID) { old, new in
    if old != nil && new == nil {
        isUserScrolling = false
        // Delay scroll + haptic to ensure message replacement has rendered
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(100))
            if let lastID = chatStore.conversation?.messages.last?.id {
                withAnimation(Design.Motion.standard) {
                    scrollProxy?.scrollTo(lastID, anchor: .bottom)
                }
            }
            if settingsStore.settings.hapticFeedbackEnabled {
                HapticEngine.responseReceived()
            }
        }
    }
}
```

**Files:**
- `Herald/Features/Chat/ChatScreen.swift:124-138`
- `Herald/Stores/ChatStore.swift:356-440` (finish streaming handler)

---

### P2 — Quality of Life

#### B7. Settings "Left-Right Bounce"

**Finding:** The `TabView` uses spring animations (`dampingFraction: 0.75`), which is underdamped enough to create visible overshoot. When the user is on the Settings tab (last tab) and tries to swipe further right, the TabView spring-bounces because there's no next tab. Additionally, the `iPhoneSessionDrawer` edge-swipe gesture (24pt drag-catcher strip) interferes with the TabView's own swipe gesture recognizer.

Also, Settings is accessible both as a tab AND as a sheet (from the gear icon in ChatScreen). The sheet presentation with `.presentationDetents([.large])` only has one detent, so swiping down bounces back to large.

**Remedy:**
1. Add `.animation(.linear(duration: 0), value: router.selectedTab)` to suppress spring bounce on the TabView — or use `.interactiveSpring(dampingFraction: 1.0)` to eliminate overshoot
2. Remove the sheet presentation for Settings — since it's already a tab, the gear icon should switch to the settings tab via `router.switchToTab(.settings)` instead of `router.presentSheet(.settings)`
3. Or: keep the sheet but add `.medium` detent so users can swipe down without bounce

```swift
// ContentView.swift:48 — change sheet presentation:
.sheet(item: $router.activeSheet) { destination in
    sheetDestination(destination)
}
// And in ChatScreen.swift:257-258 — change gear icon action:
GlassCircleButton(icon: "gearshape", accessibilityLabel: "Open settings") {
    router.switchToTab(.settings)  // Was: router.presentSheet(.settings)
}
```

**Files:**
- `Herald/Features/Chat/ChatScreen.swift:257-259` (gear button)
- `Herald/ContentView.swift:38-45, 48-51, 87-91` (TabView + sheet)
- `Herald/Core/Design.swift:130-136` (spring animation constants)
- `Herald/Features/Sidebar/iPhoneSessionDrawer.swift:55-61` (edge drag strip)

#### B8. Chats Fly Off Screen on New Replies

**Finding:** `scrollToBottom()` fires from multiple `onChange` handlers simultaneously during streaming. The throttle (`scrollThrottleInterval: 0.5`) shares a single `lastAutoScrollTime` across all triggers. When `streamingContentLength` changes at 30fps and `streamingMessageID` transitions to nil, both fire `scrollToBottom()` in the same run loop.

**Remedy:**
1. Replace shared throttle with per-source throttles or use a single `Task` debounce
2. Use `scrollPosition(id:)` (iOS 18+) instead of `ScrollViewProxy.scrollTo()` for more predictable behavior
3. Add a `scrollAnchor` state that coalesces multiple scroll requests into one per frame

```swift
// Debounce all auto-scroll requests into one per ~100ms
private func scheduleAutoScroll() {
    autoScrollTask?.cancel()
    autoScrollTask = Task { @MainActor in
        try? await Task.sleep(for: .milliseconds(100))
        guard !Task.isCancelled, !isUserScrolling else { return }
        if let lastID = conversation?.messages.last?.id {
            withAnimation(.linear(duration: 0.15)) {
                scrollProxy?.scrollTo(lastID, anchor: .bottom)
            }
        }
    }
}
```

**Files:** `Herald/Features/Chat/ChatScreen.swift:100-138, 1189-1204`

#### B9. Delayed Responses (Multiple Sources)

**Finding:** Latency accumulates from:
- 33ms delta coalescing
- DB round-trips per event (EventFanout polling)
- `reloadConversationForStreaming()` HTTP round-trip
- 90s watchdog before error surfacing
- Connector idle polling interval before picking up queued jobs

**Remedy:**
1. Reduce delta flush interval to 16ms (already recommended in B2)
2. The relay's SSE fanout wakes on every DB write — ensure DB writes use WAL mode and are synchronous
3. Remove `reloadConversationForStreaming()` (already recommended in B2)
4. Reduce connector idle poll interval to 1s
5. Reduce watchdog to 30s for first progress (already recommended in B2)

**Files:**
- `Herald/Stores/ChatStore.swift:39, 58`
- `Herald/Services/Live/LiveHeraldClient.swift:272-280`
- `relay/app/main.py` (EventFanout)
- `connector/src/herald_connector/`

---

### P3 — Missing Features

#### B10. Action Center (Inbox) Empty

**Finding:** Inbox items are created as a side effect of push notification delivery in `maybe_send_message_push()` (main.py:531-543). Since pushes don't reliably fire (B4), inbox items are never created. There's no separate mechanism to populate inbox items independent of push delivery.

**Remedy:**
1. Create inbox items in the WebSocket handler directly when a `job.result` arrives, **regardless** of push delivery status
2. Or: add a background job in the relay that creates inbox items for completed jobs every N seconds
3. On iOS: load inbox on ChatScreen appear (not just app launch)

```python
# main.py — in the WebSocket job.result handler (~line 3021):
# Always create inbox item, regardless of push state
await create_inbox_item_for_job(job_id, conversation_id, message_preview)
# Then attempt push delivery (best-effort)
await maybe_send_message_push(...)
```

**Files:**
- `relay/app/main.py:531-543` (inbox item creation)
- `relay/app/main.py:2980-3030` (WebSocket job.result handler)
- `Herald/Stores/InboxStore.swift`
- `Herald/Services/Live/LiveInboxService.swift`

#### B11. Sessions Don't Auto-Compress

**Finding:** No automatic compression. The "Compress" button in the context warning banner sends `/compress` as a chat message to the AI agent. The agent is expected to summarize and continue. There's no client-side context management at all.

**Remedy:**
1. Implement client-side auto-compression: when `contextPercent > 0.85`, automatically invoke the compress flow without user intervention
2. The flow should:
   a. Send a `/compress` system directive (not as a user-visible message)
   b. Wait for the compressed summary response
   c. Replace the conversation context with the compressed version
   d. Notify the user with a non-intrusive banner: "Context compressed (85% → 35%)"
3. Or: implement client-side summarization that builds a summary from the last N messages and inserts it as context

```swift
// In ChatStore:
func maybeAutoCompress() async {
    guard let contextInfo = lastContextInfo,
          let contextWindow = contextInfo.window,
          let contextUsed = contextInfo.used,
          contextWindow > 0,
          Double(contextUsed) / Double(contextWindow) > 0.85
    else { return }
    // Send /compress as system directive, not user message
    await sendCompressDirective()
}
```

**Files:**
- `Herald/Features/Chat/ChatScreen.swift:829-863, 1072-1081` (context banner + performCompress)
- `Herald/Stores/ChatStore.swift` (add auto-compress trigger)

#### B12. Each Chat Not a Different Session

**Finding:** The `ChatStore` holds a single `conversation`. The `LiveHeraldClient` has a single `currentConversation`. The conversation cache is a single entry in `UserDefaults`. There is no concept of session isolation — switching sessions reassigns the single `conversation` pointer.

`SessionListStore.createNewSession()` already exists and properly calls `POST /sessions` to create a new session with its own UUID. The fix for B5 (`/new` routing) should use this path.

**Additional remedy:**
1. Make `persistence.loadConversationCache()` scoped to the current session ID (not a single global key)
2. When session list loads, pre-warm the cache for the most recent session
3. When switching sessions, persist the old session's cache before loading the new one

```swift
// UserDefaultsAppPersistenceStore — change:
func loadConversationCache() -> Conversation? {
    // Was: single key "conversation_cache"
    // Now: scoped to active session ID
    guard let sessionId = activeSessionId else { return nil }
    return loadConversationCache(for: sessionId)
}
```

**Files:**
- `Herald/Stores/ChatStore.swift:119-132` (loadConversationIfNeeded)
- `Herald/Services/Support/UserDefaultsAppPersistenceStore.swift` (cache key)
- `Herald/Stores/SessionListStore.swift:181-189` (createNewSession — reuse this!)

---

## Deployment Plan

### Phase 1 — Critical Fixes (build 63, target: 2026-07-25)

| # | Bug | Fix | Risk |
|---|-----|-----|------|
| P0-B1 | TalkAudioCapture crash | Extract tap handler to `nonisolated static` method | Low — isolated change |
| P1-B5 | `/new` random chats | Route `/new` → `createNewSession()` instead of `clearConversation()` | Medium — touches navigation |
| P1-B6 | Thinking/haptic race | Add 100ms delay before haptic after stream end | Low — timing only |

### Phase 2 — Streaming & Push (build 64, target: 2026-07-26)

| # | Bug | Fix | Risk |
|---|-----|-----|------|
| P1-B2 | Streaming not working | Reduce watchdog to 30s, flush to 16ms, remove reloadConversationForStreaming | Medium — streaming pipeline |
| P1-B3 | Reasoning disappears | Extract `<think>` content to `message.reasoning` before stripping | Low — string parsing |
| P1-B4 | Push not delivered | Config audit + stale_seconds reduction + inbox creation decoupling | Medium — requires relay deploy |

### Phase 3 — UX & Polish (build 65, target: 2026-07-27)

| # | Bug | Fix | Risk |
|---|-----|-----|------|
| P2-B7 | Settings bounce | Gear icon → switchToTab(.settings) instead of sheet | Low — one-line route change |
| P2-B8 | Chats fly off screen | Replace shared throttle with single Task debounce | Medium — scroll behavior |
| P2-B9 | Delayed responses | Cumulative improvements from B2 + connector poll interval | Low — config changes |

### Phase 4 — Feature Completion (build 66, target: 2026-07-28)

| # | Bug | Fix | Risk |
|---|-----|-----|------|
| P3-B10 | Action center empty | Create inbox items on job.result independently of push | Medium — new relay logic |
| P3-B11 | Auto-compress | Trigger `/compress` at 85% context automatically | Medium — new automated behavior |
| P3-B12 | Session isolation | Scope conversation cache per session ID | High — changes persistence layer |

### Deployment Notes

1. **All relay changes** require: `systemctl restart herald-relay` on the production host
2. **All connector changes** require: restart the connector process on the Hermes host
3. **iOS builds** require: bump version in `project.yml`, keychain unlock, entitlement strip, TestFlight upload (per standard pipeline)
4. **Verify after each phase:** monitor Xcode crash reports for regression on the fixed crash points

---

## Related Crash Points (for monitoring)

| Crash Point ID | Type | Version | Status |
|---|---|---|---|
| `DLv6v_kVaI3SezmMxhpnMD` | UIKitCore: UIEventFetcher threadMain | v2.2.4 | Fix in P0-B1 |
| `DX92OkzRhoXVlVzJ2c-Q3A` | AttributeGraph: vector size | v2.2.x | Monitor after P3 |
| `CbXJ3MievpXVcQpWyz5mj5` | caulk: semaphore timed_wait | v2.2.x | Monitor after P3 |

---

## Research Sources

- Herald iOS source: `[project-root]/Herald/Herald/`
- Herald relay: `[project-root]/Herald/relay/app/`
- Herald connector: `[project-root]/Herald/connector/src/herald_connector/`
- Crash logs: `~/Library/Developer/Xcode/Products/net.fihonline.herald/Crashes/`
- Changelog: `[project-root]/Herald/CHANGELOG.md`
- Git log shows 20+ commits across v2.0.0–v2.2.5, all in the last 3 days
