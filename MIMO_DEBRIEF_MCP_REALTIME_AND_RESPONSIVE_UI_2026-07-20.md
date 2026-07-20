# Herald: MCP/realtime debrief and responsive UI debug for Mimo

**Date:** 2026-07-20  
**Canonical repository:** `[project-root]/Herald`  
**Current working version:** `1.2.8` / build `20`  
**Purpose:** handoff of the completed MCP/relay/connector recovery work plus a code-grounded, read-only diagnosis of the iPad and iPhone layout/action-surface defects shown in the supplied screenshots.

> Important: the responsive-layout and action-surface sections below are diagnosis and implementation guidance only. No Swift, Xcode project, or layout code was changed during this UI-debug pass.

---

## 1. Executive debrief

There are two separate bodies of work in this handoff.

1. **MCP and live-delivery recovery was implemented.** The repeated `hermes_mobile` MCP `TaskGroup` failures were not a single network problem. They were a rename-compatibility failure combined with a stale six-day-old Hermes WebUI process, a persisted connector-config field mismatch, relay lease exceptions, and an un-awaited connector heartbeat. Compatibility aliases, runtime mapping, lease handling, heartbeat awaiting, and lower poll/reconnect intervals are now present in the working tree. The stale host process was restarted so it stopped launching the watchdog with the removed `--create-time` argument.

2. **The current responsive UI has deterministic architecture defects.** The app deliberately routes every compact-height device—including every landscape iPhone—into the iPad `NavigationSplitView`. On iPad, “closing” the inspector changes its contents to an empty placeholder but leaves the third column mounted and consuming width. The right panel also contains several representations that are not truthful to their labels: its terminal is static demo copy, its Tools tab is token usage, its filter chips do not filter, and its Logs tab is only a small local iOS lifecycle buffer rather than Hermes logs.

The portrait iPhone screenshot is the visual baseline to preserve. The correct fix is not global font or spacing reduction. Mimo needs to repair root layout selection, panel ownership/visibility, width budgeting, and action-data semantics.

### Relay/connector cause and fix—in one block

This was an end-to-end failure chain, not merely an MCP client retry problem:

1. A stale Hermes WebUI process repeatedly launched the MCP watchdog with the removed `--create-time` argument. The MCP child exited during startup, and the manager surfaced only the outer `unhandled errors in a TaskGroup` message.
2. Pre-Herald gateway configurations still referenced `hermes-mobile-mcp`, legacy modules, and `Hermes*` symbols. The renamed package did not retain every compatibility path, so a reinstall could leave cached commands unable to start.
3. The connector's persisted configuration schema still uses `hermes_*` fields, but `ConnectorHeraldSettings.from_runtime_config` read nonexistent `herald_*` properties. After restart, the connector could appear installed/active while runtime construction failed before useful work began.
4. The connector heartbeat loop invoked an async WebSocket sender without awaiting it. Jobs therefore looked alive locally but did not reliably renew their relay lease.
5. The relay lease monitor could itself crash because `utcnow` was missing and SQLite lease timestamps could be offset-naive while the comparison clock was offset-aware. That disconnected the connector and created another reconnect/error cycle.
6. Even on a healthy path, one-second idle polling and a three-second reconnect delay added avoidable latency.

The applied fix was correspondingly cross-tier:

- restart the stale Hermes WebUI process so the live caller matches the installed watchdog;
- restore `hermes-mobile` / `hermes-mobile-mcp`, legacy module, and `Hermes*` symbol compatibility aliases;
- explicitly translate persisted `ConnectorRuntimeConfig.hermes_*` values into `ConnectorHeraldSettings.herald_*` values;
- await heartbeat senders and respect the configured heartbeat interval;
- import the relay lease clock and normalize stored timestamps before comparison;
- reduce idle job polling from 1.0 seconds to 0.1 seconds and reconnect delay from 3.0 seconds to 1.0 second.

Expected result: MCP startup no longer dies in the watchdog wrapper, the connector survives service restarts, active jobs renew their relay leases, relay lease checking no longer kills the WebSocket, and newly queued work reaches an idle connector with substantially less avoidable delay.

---

## 2. Screenshot evidence

| Screenshot | Pixels | Observed mode | Evidence |
|---|---:|---|---|
| `[project-root]/Downloads/Screenshot 2026-07-20 at 3.48.51 AM.png` | 1179 × 2556 | iPhone portrait | Good baseline: single chat surface, bottom tabs, hamburger, bounded model/status pill, composer, and Canvas button all fit. Preserve this hierarchy and density. |
| `[project-root]/Downloads/Screenshot 2026-07-20 at 3.48.33 AM.png` | 2556 × 1179 | iPhone landscape | The session/sidebar surface becomes the entire app. Header, search, filters, and toggle scale into a wide sparse canvas; chat, composer, and bottom tabs are absent. This is the iPad split-view branch collapsing to one column, not the phone drawer merely being too wide. |
| `[project-root]/Downloads/Screenshot 2026-07-20 at 3.41.54 AM.png` | 2360 × 1640 | iPad landscape | Three allocations remain visible even though the right inspector is “closed.” The right allocation is a large blank placeholder, the chat is squeezed, and there is no functional divider/resize affordance. The center toolbar has another system `…` overflow because a physically-iPad toolbar is being laid into a narrow split column. |

### Visual conclusions

- The iPhone portrait design is not the problem and should be regression-protected.
- The iPhone landscape failure is a root-container switch, not a local sidebar-frame bug.
- The iPad blank area is a live `NavigationSplitView` detail column, not unexplained padding.
- The iPad `…` is toolbar overflow caused by the available **column** width, even though the physical device is an iPad.
- A frame-only patch will leave the navigation semantics and false action data broken.

---

## 3. Work already completed: MCP, relay, connector, and realtime delivery

The following changes are currently present as uncommitted working-tree changes. Do not reimplement or overwrite them while doing the UI work.

### 3.1 Why `hermes_mobile` repeatedly failed MCP revival

The log sequence:

```text
MCP server 'hermes_mobile': attempting revival...
initial connection failed (attempt 1/3)... TaskGroup...
initial connection failed (attempt 2/3)... TaskGroup...
initial connection failed (attempt 3/3)... TaskGroup...
failed initial connection ... parking until a reconnect is requested
```

was the outer MCP manager reporting a child-process startup failure. The useful exception was hidden under Python's generic `ExceptionGroup` / `TaskGroup` summary.

The concrete causes found were:

- The product/package was renamed from Hermes Mobile to Herald, but cached Hermes MCP configurations could still invoke `hermes-mobile-mcp` or import old `Hermes*` compatibility symbols.
- A six-day-old Hermes WebUI process still held old Python code in memory and invoked `mcp_stdio_watchdog.py` with the deleted `--create-time` argument. Reinstalling files did not update that running caller.
- Persisted `ConnectorRuntimeConfig` still exposes `hermes_*` fields, but `ConnectorHeraldSettings.from_runtime_config` attempted to read nonexistent `herald_*` fields. A restart could therefore leave the service nominally active while its runtime construction failed.

### 3.2 Compatibility work added

`connector/pyproject.toml` now retains both canonical and legacy executables:

- `herald`
- `herald-mcp`
- `hermes-mobile`
- `hermes-mobile-mcp`

The connector code also retains compatibility symbols for pre-rename imports:

- `HermesMobileConnector = HeraldConnector`
- `HermesAPIExecutor = HeraldAPIExecutor`
- `HermesRuntimeAdapter = HeraldRuntimeAdapter`
- `HermesAPIRuntimeAdapter = HeraldAPIRuntimeAdapter`
- compatibility module files for old executor/runner import paths

This is intentional migration support. Do not remove it merely because the new product name is Herald; cached gateway/MCP configs can outlive a package update.

### 3.3 Persisted runtime mapping fixed

`connector/src/herald_connector/herald_runner.py` now maps the actual persisted fields:

```text
ConnectorRuntimeConfig.hermes_command       -> ConnectorHeraldSettings.herald_command
ConnectorRuntimeConfig.hermes_workdir       -> ConnectorHeraldSettings.herald_workdir
ConnectorRuntimeConfig.hermes_provider      -> ConnectorHeraldSettings.herald_provider
ConnectorRuntimeConfig.hermes_model         -> ConnectorHeraldSettings.herald_model
ConnectorRuntimeConfig.hermes_toolsets      -> ConnectorHeraldSettings.herald_toolsets
ConnectorRuntimeConfig.hermes_source        -> ConnectorHeraldSettings.herald_source
ConnectorRuntimeConfig.hermes_history_limit -> ConnectorHeraldSettings.herald_history_limit
```

This is a schema-boundary translation, not an invitation to rename persisted fields without migration support.

### 3.4 Relay WebSocket lease failures fixed

`relay/app/main.py` had two live failure paths around connector-job leases:

- `utcnow` was used without being imported.
- SQLite could return an offset-naive `lease_expires_at`, while the current clock was timezone-aware, causing an invalid datetime comparison.

The working tree imports `utcnow` and compares against `normalize_datetime(current_job.lease_expires_at)`. This prevents a healthy connector WebSocket from being closed by either `NameError` or naive/aware datetime exceptions while a job is active.

### 3.5 Heartbeats now actually renew leases

`HeraldConnector._start_job_heartbeat` previously called an async sender without awaiting it. That creates a coroutine object but does not send the heartbeat, so the relay can expire an active job despite the heartbeat loop appearing to run.

The implementation now:

- uses the configured `heartbeat_interval_seconds` rather than a second hard-coded `10.0`;
- captures the sender result;
- checks `inspect.isawaitable(...)`; and
- awaits async senders.

A targeted regression test exercises an async sender and verifies that `job.heartbeat` is actually delivered.

### 3.6 Realtime latency tuning applied

- Relay idle job polling default: **1.0 s → 0.1 s**.
- Connector reconnect delay default: **3.0 s → 1.0 s**.

This removes avoidable dispatch/recovery latency, but it does not make a polling architecture equivalent to push. Mimo should preserve the WebSocket-first path and treat polling as a safety net.

### 3.7 Naming and release bookkeeping

- Completion push and inbox titles now say **Herald**, not Hermes.
- `CHANGELOG.md`, `README.md`, `project.yml`, and the generated Xcode project are at **1.2.8 / build 20** in the working tree.
- The stale Hermes WebUI runtime was restarted so its in-memory MCP watchdog caller matches the installed code.

### 3.8 Validation status—do not overclaim

Targeted validation for the new runtime mapping and async heartbeat is green:

```text
connector/.venv/bin/pytest -q \
  tests/test_service_management.py \
  tests/test_streaming.py::test_job_heartbeat_awaits_async_sender

9 passed
```

The complete existing suites are **not** green and must not be described as green:

- Connector full suite: 10 failures. Five sensor-history tests use April timestamps and now fall outside current retention behavior; five streaming tests still expect the pre-`job.started` message sequence.
- Selected relay streaming/push suite: 2 failures. One expects two separate text deltas even though the relay now coalesces adjacent deltas; one APNs stub does not accept the newer `user_info` argument.

These stale/time-sensitive expectations are separate cleanup work, but Mimo should update them before claiming release-grade CI.

---

## 4. P0: iPhone landscape selects the wrong application shell

### Confirmed call chain

```text
AppRootView
  -> AdaptiveRootView
       -> useIPadLayout
            DeviceClass.isPad || verticalSizeClass == .compact
       -> iPadLayout
            three-column NavigationSplitView
```

Evidence:

- `Herald/Features/Onboarding/AppRootView.swift:12-20` mounts `AdaptiveRootView` after onboarding.
- `Herald/Features/Sidebar/AdaptiveRootView.swift:12-16` reads `verticalSizeClass` and returns true for `DeviceClass.isPad || verticalSizeClass == .compact`.
- `AdaptiveRootView.swift:18-25` chooses the iPad shell whenever that expression is true.
- `AdaptiveRootView.swift:31-62` constructs a three-column `NavigationSplitView`.
- The source comment at `AdaptiveRootView.swift:5` explicitly says “iPhone landscape: iPad layout scaled down.” The screenshot demonstrates that this product decision is wrong.

Every normal landscape iPhone has a compact vertical size class. Rotation therefore destroys the good `MainTabView` phone hierarchy and substitutes the iPad split hierarchy. At the available landscape-phone width/height, SwiftUI collapses the split view to a single column; the screenshot happens to show the session sidebar.

### Secondary defects caused by that branch

- The bottom Chat/Inbox/Talk/Settings tab bar disappears because `MainTabView` is no longer mounted.
- Chat and composer can become inaccessible behind split-view column navigation.
- If the chat column is reached, `ChatScreen` still sees a physical phone and installs the iPhone hamburger at `ChatScreen.swift:156-180`.
- That hamburger is wired to `.constant(false)` by `AdaptiveRootView.swift:69`, so toggling it cannot mutate state. It is a visible dead control.
- The landscape phone can combine phone toolbar content with an iPad navigation shell, an internally inconsistent layout mode.

### Required correction

For the next implementation, **all iPhones should retain the phone shell in both orientations** unless a separately designed and tested landscape-phone shell exists.

The safe policy is:

```text
physical iPhone -> MainTabView in portrait and landscape
physical iPad   -> adaptive tablet container based on current window geometry
```

Do not use `verticalSizeClass == .compact` as a synonym for iPad layout. Size classes describe available space, not product/device role.

After that correction, make the phone drawer width responsive to the current container. `iPhoneSessionDrawer.swift:11` captures a width from `UIScreen.main.bounds`; a `GeometryReader` or container-relative width will behave more reliably through rotation, Split View, Display Zoom, and future windowing.

### Acceptance criteria

- Rotating the phone never replaces `MainTabView` with `NavigationSplitView`.
- Chat, composer, hamburger, and bottom tabs remain immediately reachable in both orientations.
- Landscape density is compact and intentional; it does not inflate the session browser to the full wide canvas shown in the screenshot.
- The portrait screenshot remains visually unchanged except for explicitly approved action-surface corrections.
- The phone hamburger always controls a mutable drawer binding.

---

## 5. P0: “closed” iPad inspector still consumes an entire column

### Confirmed root cause

`isRightPanelOpen` does not control split-view visibility. It only controls which view is placed inside the detail column.

At `AdaptiveRootView.swift:31-62`, the three structural columns always exist:

```text
sidebar = iPadSidebarView
content = contentColumn (Chat/Inbox/Talk/Settings)
detail  = iPadRightPanelView OR full-size placeholder
```

When `isRightPanelOpen == false`, lines 49-60 create a full-size view containing “Open the detail panel.” The detail column is therefore not closed. It is replaced with a blank detail page that continues participating in `NavigationSplitView` allocation.

Both visible toggles—`iPadSidebarView.swift:137-149` and `AdaptiveRootView.swift:89-102`—only flip the Boolean. The right-panel X button at `iPadRightPanelView.swift:55-63` also only sets the Boolean false. All three controls produce the same result: the inspector contents disappear, but the reserved detail column remains.

That exactly matches the iPad screenshot.

### Why the current three-column semantic model is wrong

SwiftUI's three-column `NavigationSplitView` models:

```text
sidebar -> supplementary content -> primary detail
```

Herald is using it as:

```text
session navigation -> primary chat -> optional utility inspector
```

Those are different semantics. The primary chat is placed in the split view's `content` role while an optional utility is placed in the primary `detail` role. During automatic collapse, SwiftUI can prioritize the inspector/placeholder as the detail destination. That is the opposite of Herald's product priority.

`NavigationSplitViewVisibility.doubleColumn` alone is not a complete repair: in a three-column split it generally means content + detail, which would hide the session sidebar and retain the inspector role. Mimo must fix the ownership model, not just add a visibility binding and hope the desired two columns remain.

### Recommended container architecture

Use a two-part tablet workspace:

1. A normal **two-column** `NavigationSplitView` owns session navigation + primary content.
2. An optional trailing inspector sits beside that split view in a width-aware container and is removed from layout when closed.

The trailing inspector must be a sibling that causes main content to reflow, not an overlay. A custom divider/drag gesture or a UIKit split controller bridge may be used for resizing. The required state is conceptually:

```text
sidebar visibility       -> owned by navigation split state
inspector presented      -> explicit scene-local Boolean
inspector width          -> explicit scene-local CGFloat, clamped to current window
selected inspector tab   -> explicit scene-local RightPanelTab
```

If Mimo keeps a three-column `NavigationSplitView`, it must demonstrate through tests that closed state removes the inspector allocation, sidebar + chat remain, compact collapse always prioritizes chat, and resizing is possible. The current mapping does none of those things.

### Width budget

Do not hard-code all three widths independently. Compute a budget from current window geometry and protect the main content first. Reasonable initial targets for testing—not immutable design tokens—are:

- session sidebar: 280–360 pt;
- inspector: 300–480 pt;
- primary chat: never below roughly 420 pt on an iPad workspace intended to show adjacent panels;
- below the combined minimum: automatically close/collapse inspector before squeezing chat.

The open/closed and user-resized width should survive ordinary selection changes and rotation for the scene. Do not persist a width that is invalid for the next window size; clamp it on every geometry change.

### Acceptance criteria

- “Close inspector” removes the inspector and its width; no placeholder column remains.
- Reopening the inspector restores its last valid width in the current scene.
- A visible divider/handle resizes the inspector with sensible minimum and maximum bounds.
- Main chat does not fall below the approved readable minimum merely to retain an empty utility panel.
- Narrow Stage Manager/Split View widths collapse the inspector first.
- Sidebar collapse and inspector close are separate, understandable actions.
- VoiceOver exposes clear “Open inspector,” “Close inspector,” and “Resize inspector” semantics.

---

## 6. P1: iPad sizing and toolbar overflow are driven by column width, not device class

### Confirmed problems

1. `iPadRightPanelView.swift:27-32` applies an internal fixed width of `300` points.
2. Its caller adds `.frame(minWidth: 280, idealWidth: 320)` at `AdaptiveRootView.swift:43-48`.
3. The split view has no explicit window-width budget or main-content minimum.
4. `ChatScreen.swift:156-163` chooses toolbar composition from `DeviceClass.isPhone`, not the width actually available to the chat column.
5. The iPad toolbar packs profile, model, and timer on the leading side (`ChatScreen.swift:201-207`) plus Canvas and Settings trailing (`ChatScreen.swift:209-227`). `AdaptiveRootView` then contributes another trailing inspector toggle (`AdaptiveRootView.swift:69-80`).

On a full iPad, the physical-device check selects the rich iPad toolbar even when the center chat column is phone-width. SwiftUI responds by synthesizing the `…` overflow shown in the iPad screenshot.

### Required correction

- Make toolbar composition responsive to the **chat column's available width**, not just `UIDevice.userInterfaceIdiom`.
- Define wide, medium, and compact toolbar compositions, with the portrait iPhone arrangement as the proven compact baseline.
- Keep one owner for each action. Do not install Canvas or inspector controls independently in nested toolbars if that creates duplicates or unpredictable overflow grouping.
- Remove the panel's internal fixed-width fight. The parent workspace should own inspector width; the panel contents should use `maxWidth: .infinity` within that allocation.
- Do not “solve” this by shrinking all typography. The screenshot is caused by allocation and action density.

### Acceptance criteria

- No synthesized `…` appears in any supported iPad window width.
- Required actions remain reachable and have stable VoiceOver labels.
- The center chat keeps readable line length and does not become a narrow strip while an inactive utility column consumes space.
- Rotating or resizing does not duplicate toolbar actions or reset current selection.

---

## 7. P1: “action panel” accuracy audit

The repository has three surfaces a user could reasonably call the action panel. All three were audited so Mimo does not fix the wrong one.

### 7.1 iPad right inspector: labels overpromise the data

#### Logs is not a Hermes log viewer

`ChatStore.logEntries` is a local in-memory array (`ChatStore.swift:10-12`) capped at 500 entries by `appendLog` (`ChatStore.swift:615-620`). Repository search finds only a handful of writes in `ChatStore`:

- streaming started;
- message accepted;
- job started;
- heartbeat;
- reconnecting;
- cancelled.

It does not stream the Hermes log shown in the user's original MCP report. It also does not append each `.toolActivity`, text failure, relay request, MCP exception, or host process log. Calling the tab “Logs” without a source label is inaccurate.

The level row at `iPadRightPanelView.swift:90-113` is not a filter. It renders `Text` pills for every `LogLevel` but owns no selection state and performs no filtering. It is decorative.

#### Terminal is a static mock

`iPadRightPanelView.swift:131-170` hard-codes:

- `$ hermes agent --version`;
- `Hermes Agent v2.1.0 — Nous Research`;
- `$ tail -f ~/.herald/logs/agent.log`;
- a synthesized “Connected to relay” string; and
- “Terminal integration coming soon.”

No command is executed and no terminal output is received. This must not look like live, authoritative terminal state.

#### Tools is token usage, not tool activity

`iPadRightPanelView.swift:172-195` labels the tab `TOOL ACTIVITY` but reads only `conversation.latestUsage` and displays prompt/completion/total tokens. It never reads `Message.toolActivities`. The tab is analytically useful but semantically mislabeled.

#### Truthful interim options

Until real sources are wired:

- rename Logs to **App Activity** or **Connection Events** and display its source;
- make log-level controls functional or remove the filter styling;
- label Terminal **Preview / Coming Soon** and visually disable it, or remove the tab;
- rename Tools to **Usage** if it continues to show token totals;
- add a real Tool Activity view by aggregating the active/current message's `toolActivities` if that is the desired product.

Do not fabricate authoritative-looking output.

### 7.2 iPhone top-right overlapping-rectangles action

In the portrait screenshot, the top-right `rectangle.on.rectangle` control is **Canvas**, not an inspector/action switcher.

Call chain:

```text
ChatScreen iPhone trailing toolbar
  -> showCanvas = true
  -> CanvasView sheet with medium/large detents
```

Evidence: `ChatScreen.swift:184-196` and `ChatScreen.swift:132-135`.

Accuracy/usability problems:

- The button is always enabled even when `canvasStore.activeArtifact == nil`; color alone indicates artifact presence.
- With no artifact it opens an empty view that tells the user to long-press a code message.
- The icon visually resembles a panel/window switcher and has no visible label, so “action panel” is a reasonable interpretation.
- `CanvasView.swift:31-38` makes the X action call `store.clear()` and then dismiss. Closing and deleting the active artifact are conflated. Drag-dismiss closes without clearing, so two close paths have different data semantics.
- On iPad, `ChatScreen` still has its own Canvas sheet while `iPadRightPanelView` also has a Canvas tab. The two presentation paths are not coordinated by shared navigation state.

Required product decision:

- If this is an artifact Canvas, make that explicit through icon/label/help and disabled/empty-state behavior.
- Separate **close** from **clear/delete** everywhere.
- On iPad, opening Canvas from a message should select/open the inspector's Canvas tab rather than presenting an unrelated sheet, unless the product intentionally supports both and labels them distinctly.
- On iPhone, keep a sheet/full-screen presentation; do not expose the tablet inspector shell.

### 7.3 Inline tool/action rail: structured events can be misclassified

The intended pipeline is sound in outline:

```text
Hermes API SSE content
  -> connector StreamEvent(tool_activity)
  -> WebSocket job.progress(kind=tool_activity)
  -> relay SSE event: tool_activity
  -> LiveHeraldClient StreamingUpdate.toolActivity
  -> ChatStore Message.toolActivities
  -> ToolActivityRail
```

But the connector classifier at `connector/src/herald_connector/herald_api_executor.py:15-18` and `:288-302` only recognizes a tool marker if a **single SSE content delta** exactly matches:

```regex
^\n`([^\n]+)`\n$
```

SSE token/chunk boundaries are not semantic boundaries. A marker can be split across deltas, combined with surrounding prose, or emitted without the exact leading/trailing newline. Those cases fall through as `text_delta` and are rendered as ordinary assistant prose.

The existing regex test only verifies complete one-chunk markers (`connector/tests/test_streaming.py:82-91`). It does not cover split, adjacent, or combined marker chunks.

The portrait screenshot shows repeated narration such as “Let me dig…” and “Let me pull…”. The screenshot alone cannot prove whether those phrases were model-authored prose or misclassified action markers. The code does prove that arbitrary chunking can make structured action presentation inaccurate.

Required correction:

- Parse tool markers from an accumulated streaming buffer/state machine, not one delta at a time.
- Preserve ordinary text before and after a marker.
- Emit exactly one structured event per complete marker.
- Flush incomplete non-marker text safely at stream completion.
- Test one-token-at-a-time splits, marker + prose in one chunk, multiple markers in one chunk, malformed backticks, Unicode emoji, and final incomplete marker.
- Define whether final assistant content should retain or remove tool-progress marker text, then enforce the same rule in connector accumulation and final relay message storage.

### 7.4 Tool timeline state also loses truth after completion/reload

- When a new tool activity arrives, `ChatStore.swift:239-250` marks earlier activities inactive and appends the latest as active.
- On `.finished`, it copies the activities into the resolved message (`ChatStore.swift:256-265`) but does not explicitly mark the final activity inactive.
- `LiveHeraldClient.mapMessage` reconstructs persisted relay messages without tool activities (`LiveHeraldClient.swift:357-387`). `ChatStore` has local merge logic to preserve transient artifacts, but the relay conversation is not an authoritative durable tool timeline.

Mimo must decide whether tool history is transient session UI or durable conversation data. If durable, add structured persistence end to end. If transient, label it as live/local and do not imply complete historical accuracy.

---

## 8. What not to do

- Do not change global typography or spacing to mask the split allocation bug.
- Do not retain `verticalSizeClass == .compact` as the phone-landscape switch.
- Do not “close” the inspector by rendering `EmptyView` or a placeholder inside an allocated detail column.
- Do not use the three-column visibility enum without verifying which semantic columns survive collapse.
- Do not add another ellipsis menu; prevent system overflow through width-aware composition.
- Do not call token usage “Tool Activity.”
- Do not show hard-coded terminal copy as live output.
- Do not make Canvas X both close and destructive clear.
- Do not remove Hermes-named connector/MCP compatibility aliases during unrelated UI cleanup.
- Do not bump/revert version numbers blindly: the working tree is already at `1.2.8` / build `20` for the MCP/realtime patch.

---

## 9. Required implementation sequence for Mimo

Keep these as separate logical changes and commits.

### Change A: restore a single phone shell in both orientations

Files to begin with:

- `Herald/Features/Sidebar/AdaptiveRootView.swift`
- `Herald/ContentView.swift`
- `Herald/Features/Sidebar/iPhoneSessionDrawer.swift`
- `Herald/Features/Chat/ChatScreen.swift`

Deliver:

- iPhone always uses `MainTabView`;
- landscape preserves chat/tabs/composer;
- drawer width uses current container geometry;
- no dead `.constant(false)` phone drawer binding;
- portrait remains visually stable.

### Change B: replace the false third-column close with a real inspector workspace

Files to begin with:

- `Herald/Features/Sidebar/AdaptiveRootView.swift`
- `Herald/Features/Sidebar/iPadSidebarView.swift`
- `Herald/Features/Sidebar/iPadRightPanelView.swift`

Deliver:

- primary navigation semantics remain sidebar + main content;
- inspector is genuinely inserted/removed;
- resize affordance and clamped width;
- main-content minimum and auto-collapse policy;
- one source of truth for open state/tab/width;
- no placeholder allocation when closed.

### Change C: make toolbar actions column-width aware

Files to begin with:

- `Herald/Features/Chat/ChatScreen.swift`
- `Herald/Features/Sidebar/AdaptiveRootView.swift`

Deliver:

- wide/medium/compact toolbar arrangements based on actual content width;
- no synthesized `…` on iPad or iPhone;
- no duplicate Canvas/Settings/inspector actions;
- stable accessibility labels.

### Change D: make action/inspector content truthful

Files to begin with:

- `Herald/Features/Sidebar/iPadRightPanelView.swift`
- `Herald/Stores/ChatStore.swift`
- `Herald/Features/Canvas/CanvasView.swift`
- `Herald/Features/Chat/ChatScreen.swift`

Deliver:

- accurate labels and functional filters;
- explicit mock/coming-soon state or removal of fake terminal;
- Tools backed by tool events, or renamed Usage;
- close and clear split into separate actions;
- one coordinated Canvas route per device class.

### Change E: harden structured tool-event parsing

Files to begin with:

- `connector/src/herald_connector/herald_api_executor.py`
- `connector/tests/test_streaming.py`
- potentially relay/iOS models only if durable tool history is selected.

Deliver:

- buffered marker parser independent of network chunk boundaries;
- adversarial chunk-boundary tests;
- ordinary prose preserved;
- final stored response consistent with live rendering.

### Change F: repair stale tests before release claim

- Update connector streaming tests for the intentional `job.started` event.
- Make sensor retention tests independent of the wall clock.
- Update relay delta expectations for intentional coalescing.
- Update APNs test doubles for `user_info`.
- Then run connector, relay, iOS unit, and iOS UI suites.

---

## 10. Device and window test matrix

At minimum, run each relevant state below with inspector closed and open where applicable.

| Device/window | Orientation/state | Must verify |
|---|---|---|
| Small supported iPhone | Portrait | Baseline hierarchy, no toolbar overflow, composer usable with keyboard. |
| Small supported iPhone | Landscape | Same phone shell, tabs/chat/composer reachable, drawer opens/closes, no split sidebar takeover. |
| Large iPhone | Portrait + landscape | Bounded status control and Canvas semantics; no dead space or giant session-browser scaling. |
| iPad 11-inch | Portrait + landscape | Sidebar/main usable; inspector inserts/removes; resize bounds; no placeholder column. |
| iPad 13-inch | Landscape full screen | Three working surfaces when inspector is open; chat receives a sane width; no `…`. |
| iPad Stage Manager | Multiple widths | Inspector collapses first; chat remains primary; stored width clamps safely. |
| iPad Split View | Narrow widths | No forced three-way squeeze; correct navigation collapse and restoration. |
| Any supported device | Accessibility text sizes | Toolbar actions remain reachable; labels do not become ambiguous. |

Required automated coverage:

- launch/rotate phone and assert bottom tabs + composer remain;
- open/dismiss drawer in both orientations;
- open/close inspector and assert the inspector element is removed, not merely hidden;
- drag inspector to minimum and maximum bounds;
- resize/rotate and verify selection/open state coherence;
- assert no overflow `…` accessibility element at target widths;
- assert Canvas close preserves artifact and explicit Clear removes it;
- assert panel filter changes visible log rows;
- assert split/combined tool markers become structured activity exactly once.

---

## 11. Final acceptance gate

Do not call this complete until all of the following are true:

- MCP legacy entrypoints still launch the Herald MCP server.
- Persisted connector runtime config survives restart.
- Active job heartbeats reach the relay and renew the lease.
- Normal message dispatch no longer pays the old one-second idle poll delay.
- iPhone portrait retains the supplied “sexy” baseline.
- iPhone landscape retains the phone shell and never shows the full-screen iPad sidebar.
- Closing the iPad inspector returns its width to chat.
- The iPad inspector can be resized and reopened predictably.
- No iPad or iPhone toolbar synthesizes a `…` overflow control at supported target sizes.
- Inspector/action labels describe their real data sources.
- Canvas close is non-destructive; Clear/Delete is explicit.
- Tool/action markers render accurately across arbitrary SSE chunk boundaries.
- Full connector and relay suites are green after stale expectations are repaired.
- Relevant iOS unit/UI tests pass on the matrix above.
- Version, build, changelog, README, and generated Xcode project are reconciled once per shippable change, without trampling the existing `1.2.8` / build `20` work.

---

## 12. Working-tree warning

The repository was already dirty before this UI diagnosis. The MCP/realtime patch and earlier Mimo handoff documents are uncommitted. Preserve them. Before implementation:

```bash
git status --short
git diff --stat
```

Do not reset, clean, regenerate, or bulk-format the repository without first isolating the intended files. `project.yml` is the XcodeGen source of truth; if a later implementation changes project settings, regenerate the Xcode project and review the generated diff.
