# UI Fixes, Job Retry Resilience, Session Auto-Titling, Live Logging Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix six device-reported UI bugs, add job auto-retry with real error surfacing, auto-derive session titles instead of a hardcoded "Hermes" default, and merge existing unmerged live-logging work onto master.

**Architecture:** All fixes are surgical — no new architecture. Each targets a precisely root-caused bug (see spec) using patterns already established in the surrounding code.

**Tech Stack:** Swift/SwiftUI (iOS), Python/FastAPI (relay)

## Global Constraints

- Rich Chat and configurable TTS are explicitly OUT OF SCOPE for this plan — deferred to separate brainstorms
- Reasoning/streaming display is confirmed NOT broken (verified live against ignyte host) — no task addresses it
- Session title derivation: plain truncation only, no LLM call, only fires once per conversation while title is still the default "Hermes"
- Job auto-retry: max 1 automatic retry per message, then requires manual tap
- Live logging is a client-side activity log (streaming lifecycle narration), not real host/agent log tailing — Terminal tab placeholder stays as-is

---

## Task 1: iOS — Keyboard Auto-Dismiss on Send

**Files:**
- Modify: `HermesMobile/Features/Chat/ChatScreen.swift`

**Interfaces:**
- Modifies: `sendMessage()` to clear `isComposerFocused`

- [ ] **Step 1: Locate sendMessage() and isComposerFocused**

Read `ChatScreen.swift` to confirm the exact current signature of `sendMessage()` and the declaration `@FocusState private var isComposerFocused: Bool`.

- [ ] **Step 2: Set isComposerFocused = false at the top of sendMessage()**

Add as the first line of the function body:

```swift
private func sendMessage() {
    isComposerFocused = false
    // ... existing body
}
```

If `sendMessage()` is `async` or has an early-return guard clause before any work happens, place the dismiss call before that guard so it always fires regardless of early exits.

- [ ] **Step 3: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Features/Chat/ChatScreen.swift
git commit -m "fix(ios): dismiss keyboard after sending a message"
```

---

## Task 2: iOS — Enter Key Sends Message

**Files:**
- Modify: `HermesMobile/Features/Chat/ChatInputBar.swift`

**Interfaces:**
- Modifies: the composer `TextField` to intercept Return via `.onKeyPress` instead of relying on `.onSubmit`

- [ ] **Step 1: Read the current composer implementation**

Read `ChatInputBar.swift` to find the exact `TextField(..., text: $text, axis: .vertical)` declaration, its modifiers (`.lineLimit`, `.submitLabel`, `.onSubmit`), and `handlePrimaryAction()`.

- [ ] **Step 2: Add .onKeyPress(.return) to intercept Enter**

Add an `.onKeyPress(.return)` modifier to the TextField that calls `handlePrimaryAction()` when Return is pressed without Shift held, and returns `.handled` to suppress the newline insert. Allow Shift+Return to fall through as `.ignored` so it still inserts a newline for multi-line composition:

```swift
.onKeyPress(.return) { press in
    if press.modifiers.contains(.shift) {
        return .ignored
    }
    if canSend {
        handlePrimaryAction()
    }
    return .handled
}
```

Keep the existing `.onSubmit { if canSend { handlePrimaryAction() } }` in place as a fallback for platforms/contexts where `.onKeyPress` doesn't fire (e.g., software keyboard "send" button on iOS still uses `.onSubmit`/`.submitLabel(.send)`).

- [ ] **Step 3: Verify canSend and handlePrimaryAction are accessible in this scope**

Confirm both are already in scope at the TextField call site (per the brief's existing code) — no new state needed.

- [ ] **Step 4: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Features/Chat/ChatInputBar.swift
git commit -m "fix(ios): make Enter key send message instead of inserting newline"
```

---

## Task 3: iOS — iPad Sidebar "Back to Chat" Navigation

**Files:**
- Modify: `HermesMobile/Features/Sidebar/iPadSidebarView.swift`

**Interfaces:**
- Modifies: `bottomSections`'s iteration list

- [ ] **Step 1: Read bottomSections**

Read `iPadSidebarView.swift`'s `bottomSections` computed property to find the exact `ForEach([SidebarSection.inbox, .talk, .settings], id: \.self)` line.

- [ ] **Step 2: Add .chat to the iteration list**

Change:

```swift
ForEach([SidebarSection.inbox, .talk, .settings], id: \.self) { section in
```

To:

```swift
ForEach([SidebarSection.chat, .inbox, .talk, .settings], id: \.self) { section in
```

- [ ] **Step 3: Verify SidebarSection.chat has title/icon already defined**

Confirm `SidebarSection.chat`'s `.title` and `.icon` computed properties already exist in the enum (per the earlier investigation, they do — "Chat" / "bubble.left.and.bubble.right").

- [ ] **Step 4: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Features/Sidebar/iPadSidebarView.swift
git commit -m "fix(ios): add Chat as a persistent iPad sidebar nav entry"
```

---

## Task 4: iOS — iPhone Sidebar Swipe-to-Open Gesture

**Files:**
- Modify: `HermesMobile/Features/Sidebar/iPhoneSessionDrawer.swift`

**Interfaces:**
- Modifies: the gesture-carrying container to be hit-testable when closed

- [ ] **Step 1: Read the current drawer structure**

Read `iPhoneSessionDrawer.swift` fully — the `ZStack`, the `HStack { drawer-box; Spacer() }`, the `.offset(...)`, and the `.gesture(DragGesture()...)` attachment point.

- [ ] **Step 2: Add .contentShape(Rectangle()) to the gesture-carrying HStack**

Add `.contentShape(Rectangle())` to the `HStack` that currently carries the `DragGesture`, so its full frame (including the transparent `Spacer()` region) participates in hit-testing:

```swift
HStack(spacing: 0) {
    // ... existing drawer box content
    Spacer()
}
.contentShape(Rectangle())
.offset(x: isOpen ? min(0, dragOffset) : -drawerWidth + dragOffset)
.gesture(
    DragGesture()
        // ... existing gesture body, unchanged
)
```

- [ ] **Step 3: Verify the HStack's frame covers the expected swipe zone**

Confirm the `HStack` (with `Spacer()`) spans the full screen width so the `.contentShape` covers a reasonable swipe-detection area, not just the drawer's own width. If it doesn't naturally span full width, add `.frame(maxWidth: .infinity)` before `.contentShape`.

- [ ] **Step 4: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Features/Sidebar/iPhoneSessionDrawer.swift
git commit -m "fix(ios): make iPhone session drawer swipe-to-open gesture hit-testable when closed"
```

---

## Task 5: iOS — New Session Button Navigation + Error Surfacing

**Files:**
- Modify: `HermesMobile/Features/Sidebar/iPadSidebarView.swift`
- Modify: `HermesMobile/Features/Sidebar/iPhoneSessionDrawer.swift`

**Interfaces:**
- Modifies: the "new chat" button action in `iPadSidebarView.swift` to navigate on success
- Adds: an error banner/alert in both files bound to `sessionStore.errorMessage`

- [ ] **Step 1: Read the current new-session button and error handling**

Read `iPadSidebarView.swift`'s `headerRow` for the "new chat" pencil button's `Task { await sessionStore.createNewSession() }` call, and confirm `SessionListStore.errorMessage`'s exact type/declaration in `SessionListStore.swift`.

- [ ] **Step 2: Navigate to .chat on successful session creation**

Change the button action to set `selectedSection = .chat` after the call:

```swift
Button {
    Task {
        await sessionStore.createNewSession()
        selectedSection = .chat
    }
} label: {
    Image(systemName: "square.and.pencil")
        // ... existing modifiers
}
```

(If `createNewSession()` can be checked for success/failure via `sessionStore.errorMessage` being nil afterward, only navigate on success — otherwise navigating unconditionally is acceptable since the error banner from Step 3 will surface any failure regardless of which section is showing.)

- [ ] **Step 3: Add error banner to iPadSidebarView**

Add a dismissible error banner near the top of the sidebar's `List` (or as an `.alert`), bound to `sessionStore.errorMessage`:

```swift
.alert(
    "Error",
    isPresented: Binding(
        get: { sessionStore.errorMessage != nil },
        set: { if !$0 { sessionStore.errorMessage = nil } }
    )
) {
    Button("OK", role: .cancel) {}
} message: {
    Text(sessionStore.errorMessage ?? "")
}
```

Adjust based on `SessionListStore.errorMessage`'s actual declared type (may need `private(set)` mutation via a dedicated `clearError()` method if it's not directly settable from the view — check `SessionListStore.swift` first).

- [ ] **Step 4: Add the same error banner to iPhoneSessionDrawer**

Apply the identical `.alert` pattern to `iPhoneSessionDrawer.swift`.

- [ ] **Step 5: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Features/Sidebar/iPadSidebarView.swift HermesMobile/Features/Sidebar/iPhoneSessionDrawer.swift
git commit -m "fix(ios): navigate to chat on new session creation, surface session errors"
```

---

## Task 6: iOS — Profile Selector Chip Visibility

**Files:**
- Modify: `HermesMobile/Features/Chat/ChatScreen.swift`

**Interfaces:**
- Modifies: `profileChip`'s visibility condition and label

- [ ] **Step 1: Read the current profileChip implementation**

Read `ChatScreen.swift`'s `profileChip` computed property (around lines 199-226 per the investigation) to see its exact current gate and label logic.

- [ ] **Step 2: Change the visibility gate**

Change the condition from:

```swift
if profileStore.activeProfile != nil {
```

To:

```swift
if !profileStore.profiles.isEmpty {
```

- [ ] **Step 3: Handle the no-active-profile label case**

Inside the chip's label, when `profileStore.activeProfile` is nil but `profiles` is non-empty, show a neutral placeholder instead of crashing/showing blank text:

```swift
Text(profileStore.activeProfile?.name ?? "Select Profile")
```

Adjust to match whatever the existing label expression looks like — the key change is falling back to a neutral string instead of assuming `activeProfile` is always present once the chip is shown.

- [ ] **Step 4: Verify ProfileSelectorSheet handles nil activeProfile gracefully**

Read `ProfileSelectorSheet.swift` to confirm it doesn't assume `activeProfileName` is non-nil anywhere that would crash when no profile is currently active (it shouldn't, since profile selection is the whole point of the sheet, but verify).

- [ ] **Step 5: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Features/Chat/ChatScreen.swift
git commit -m "fix(ios): show profile selector chip whenever profiles exist, not just when one is active"
```

---

## Task 7: Relay — Session Auto-Titling from First Message

**Files:**
- Modify: `relay/app/services.py`

**Interfaces:**
- Modifies: the message-append path to derive a title on first message

- [ ] **Step 1: Locate the message-append function**

Read `relay/app/services.py` to find the function that appends a user message to a conversation (likely `append_message` or similar, called from the `/v1/messages` POST handler in `main.py`).

- [ ] **Step 2: Locate the Conversation model's title default**

Confirm in `relay/app/models.py` that `Conversation.title` defaults to `"Hermes"` (per the investigation).

- [ ] **Step 3: Add title derivation logic**

In the message-append function, after appending the message but before returning, check if the conversation's title is still the default:

```python
def derive_title_from_message(text: str, max_length: int = 40) -> str:
    """Derive a short conversation title by truncating the first message."""
    cleaned = " ".join(text.split())  # collapse whitespace/newlines
    if len(cleaned) <= max_length:
        return cleaned
    return cleaned[:max_length].rstrip() + "…"
```

```python
# Inside append_message (or wherever the first user message lands):
if conversation.title == "Hermes" and role == "user":
    conversation.title = derive_title_from_message(text)
    db.add(conversation)
```

Adjust to match the actual function signature and variable names found in Step 1 — this is illustrative, not verbatim. Place the check so it only fires for the FIRST user message (title still equals the literal default "Hermes"), not on every message.

- [ ] **Step 4: Write a test**

Add a test in `relay/tests/` (find the existing test file pattern for conversation/message creation) verifying:
- A new conversation's first user message triggers a title update
- The derived title is a truncated version of the message text
- A conversation with a manually-set (non-default) title does NOT get overwritten by a later message
- A message longer than 40 characters gets truncated with an ellipsis

- [ ] **Step 5: Run the test**

```bash
cd ~/Hermes-iOS/relay && .venv/bin/python -m pytest tests/ -q
```

Expected: new test passes, all existing tests still pass (47+ previously passing).

- [ ] **Step 6: Commit**

```bash
cd ~/Hermes-iOS
git add relay/app/services.py relay/tests/
git commit -m "feat(relay): derive session title from first message instead of hardcoded default"
```

---

## Task 8: Relay — Log Stale Unclaimed Jobs

**Files:**
- Modify: `relay/app/services.py` or `relay/app/main.py` (wherever job claiming/lease logic lives)

**Interfaces:**
- Adds: a warning log when a job sits queued past its lease expiry without being claimed

- [ ] **Step 1: Locate the job claim/lease logic**

Read `relay/app/services.py` for `claim_next_message_job` and any related `MessageJob` status/lease-checking code (per the earlier session's exploration, `message_jobs` table has `status`, `lease_expires_at`, `claimed_at`).

- [ ] **Step 2: Add a staleness check**

Find or add a place where queued jobs are periodically checked (this may need a lightweight check inside the existing claim-attempt path rather than a new background task, to keep scope small) — when a job's `created_at` (or `lease_expires_at`) has passed without a `claimed_at` being set, log a warning:

```python
logger.warning(
    "MessageJob %s has been queued since %s without being claimed (host may be offline or WebSocket dropped)",
    job.id, job.created_at,
)
```

If there's no existing periodic sweep, the minimal-scope version is to add this check to whatever endpoint/path already reads job status (e.g., when the SSE endpoint `/v1/jobs/{job_id}/events` is polled and finds the job still queued past a threshold) rather than introducing a new scheduled task — keep this task narrowly scoped to logging, not a new subsystem.

- [ ] **Step 3: Verify relay imports and tests pass**

```bash
cd ~/Hermes-iOS/relay && .venv/bin/python -c "from app.main import create_app; app = create_app(); print('OK')"
.venv/bin/python -m pytest tests/ -q
```

- [ ] **Step 4: Commit**

```bash
cd ~/Hermes-iOS
git add relay/app/services.py relay/app/main.py
git commit -m "feat(relay): log warning when a message job goes stale without being claimed"
```

---

## Task 9: iOS — Job Auto-Retry with Error Surfacing

**Files:**
- Modify: `HermesMobile/Stores/ChatStore.swift`

**Interfaces:**
- Modifies: `sendMessage()`'s streaming/watchdog logic
- Adds: retry-count tracking per message, real error text on final failure

- [ ] **Step 1: Read the current sendMessage() streaming flow**

Read `ChatStore.swift`'s `sendMessage()` in full — the `AsyncStream<StreamingUpdate>` consumption loop, the `.messageSent`/`.textDelta`/`.reasoningDelta`/`.toolActivity`/`.finished`/`.failed` cases, and the existing polling-fallback logic mentioned in earlier context (30 attempts x 2s = 60s max).

- [ ] **Step 2: Add a watchdog timeout racing the stream**

Wrap the streaming consumption in a `withTaskGroup` or use `Task.sleep` + a cancellation race so that if no progress event (`.textDelta`, `.reasoningDelta`, `.toolActivity`, or `.finished`) arrives within 30 seconds of job submission, the watchdog fires:

```swift
// Pseudocode — adapt to the actual streaming loop structure found in Step 1
let watchdogTask = Task {
    try? await Task.sleep(for: .seconds(30))
    return true  // watchdog fired
}
// Race watchdog against the first meaningful stream event; cancel watchdog once any progress arrives
```

- [ ] **Step 3: Add retry-count tracking**

Add a property to track retry attempts per pending message (e.g., a `[String: Int]` keyed by `clientMessageID` on `ChatStore`, or a field on the `Message` model if one already tracks retry state — check `Message.swift` first for an existing `retryCount` or similar before adding a new one).

- [ ] **Step 4: Implement the auto-retry**

When the watchdog fires and `retryCount < 1` for this message: increment the retry count, re-invoke the send path with the same message content (don't create a duplicate user-facing message bubble — reuse the existing pending one), and restart the watchdog.

When the watchdog fires and `retryCount >= 1` (already retried once): mark the message `.failed` with a real error string (e.g., "Hermes didn't respond — tap to retry") instead of the current bare Retry icon, and stop auto-retrying (require manual tap from here).

- [ ] **Step 5: Verify existing manual-retry tap still works**

Confirm whatever UI currently renders the "Retry" icon/tap-to-retry (likely in `MessageBubble.swift`) still functions for the manual-retry-after-auto-retry-exhausted case, and now displays the improved error text rather than just an icon.

- [ ] **Step 6: Commit**

```bash
cd ~/Hermes-iOS
git add HermesMobile/Stores/ChatStore.swift
git commit -m "feat(ios): auto-retry stalled jobs once, surface real error text on repeated failure"
```

---

## Task 10: iOS — Cherry-Pick Live Logging from feat/ipad-layout

**Files:**
- Modify: `HermesMobile/Stores/ChatStore.swift`
- Modify: `HermesMobile/Features/Sidebar/iPadRightPanelView.swift`

**Interfaces:**
- Adds: `logEntries`/`appendLog(level:_:)` on `ChatStore` (cherry-picked)
- Modifies: `iPadRightPanelView`'s Logs tab to read from `chatStore.logEntries`

- [ ] **Step 1: Identify the exact commits and check mergeability**

```bash
cd ~/Hermes-iOS
git log --oneline feat/ipad-layout -- HermesMobile/Stores/ChatStore.swift HermesMobile/Features/Sidebar/iPadRightPanelView.swift | grep -E 'f20df34|8ae88eb'
git show f20df34 -- HermesMobile/Stores/ChatStore.swift HermesMobile/Features/Sidebar/iPadRightPanelView.swift
git show 8ae88eb -- HermesMobile/Stores/ChatStore.swift HermesMobile/Features/Sidebar/iPadRightPanelView.swift
```

Read both diffs in full before attempting the cherry-pick, since master has diverged significantly (session management, theming, model switching all landed since this branch was cut).

- [ ] **Step 2: Attempt the cherry-pick**

```bash
cd ~/Hermes-iOS
git cherry-pick f20df34
```

If conflicts arise (expected, given the divergence — especially in `ChatStore.swift`'s streaming loop which Task 9 also modifies), resolve them manually:
- Keep master's current `ChatStore.swift` structure (session management, model/profile switching, the Task 9 retry logic) as the base
- Add ONLY the `logEntries: [(timestamp: Date, level: String, message: String)]` property and `appendLog(level:_:)` method from the cherry-picked commit
- Add the `appendLog(...)` calls at the equivalent points in the CURRENT (post-Task-9) streaming loop — "Streaming started" when the stream begins, "Message accepted — job {id}" when the job ID is received — even if the exact line numbers don't match the original commit

- [ ] **Step 3: Cherry-pick or manually apply the second commit**

```bash
git cherry-pick 8ae88eb
```

Same conflict-resolution approach — this commit likely touches `iPadRightPanelView.swift` to rewire the Logs tab from the local `@State` stub to `chatStore.logEntries`. Apply that rewiring against the CURRENT `iPadRightPanelView.swift` (which already has the `@MainActor` fix on `LogLevel.color` from the themes round — do not revert that).

- [ ] **Step 4: Verify LogLevel.color's @MainActor annotation survived**

```bash
grep -n "@MainActor" HermesMobile/Features/Sidebar/iPadRightPanelView.swift
```

Confirm the `@MainActor` annotation directly above `var color: Color` (on the `LogLevel` enum) is still present after the cherry-pick/merge — this was added during the themes round to fix a real compile error when `Design.Colors` became adaptive, and losing it would reintroduce that build failure.

- [ ] **Step 5: Device-target build verification**

Build on the MBP with the standard entitlements-strip + keychain-unlock pattern:

```bash
# On MBP (herald-dev@build-host.internal):
cp HermesMobile/HermesMobile.entitlements HermesMobile/HermesMobile.entitlements.bak
echo '<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict></dict></plist>' > HermesMobile/HermesMobile.entitlements
security unlock-keychain -p '<password>' ~/Library/Keychains/login.keychain-db
security set-key-partition-list -S apple-tool:,apple: -s -k '<password>' ~/Library/Keychains/login.keychain-db
xcodebuild -project HermesMobile.xcodeproj -scheme HermesMobile -configuration Debug -destination 'generic/platform=iOS' build -allowProvisioningUpdates DEVELOPMENT_TEAM=58U7UPFS53
cp HermesMobile/HermesMobile.entitlements.bak HermesMobile/HermesMobile.entitlements
rm HermesMobile/HermesMobile.entitlements.bak
```

Expected: `** BUILD SUCCEEDED **`

- [ ] **Step 6: Commit**

If cherry-pick applied cleanly:
```bash
cd ~/Hermes-iOS
git status  # confirm cherry-pick commits are in place
```

If manually resolved (more likely given divergence):
```bash
cd ~/Hermes-iOS
git add HermesMobile/Stores/ChatStore.swift HermesMobile/Features/Sidebar/iPadRightPanelView.swift
git commit -m "feat(ios): wire live logging into iPad right panel (ported from feat/ipad-layout)"
```

---

## Task 11: Deploy and Verify

- [ ] **Step 1: Push to GitHub**

```bash
cd ~/Hermes-iOS && git push origin master
```

- [ ] **Step 2: Sync and rebuild relay**

```bash
ssh fihadmin@herald-host.internal "cd /home/agent/hermes-ios-work && git pull origin master"
ssh fihadmin@herald-host.internal "cp /home/agent/hermes-ios-work/relay/app/services.py /home/agent/hermes-ios-work/relay/app/main.py /home/agent/Hermes-iOS/relay/app/"
ssh fihadmin@herald-host.internal "cd /home/agent/deploy/hermes-relay && docker compose down && docker compose up -d --build"
```

Verify: `curl -s http://localhost:8010/v1/health` from the ignyte host returns `{"data":{"status":"ok"}...}`.

- [ ] **Step 3: Verify session titling live**

Create a new session through the app (or via `POST /v1/messages` directly) and confirm the title updates from "Hermes" to a truncated version of the first message.

- [ ] **Step 4: Sync and restart connector (only if any connector files changed — check first)**

This plan doesn't touch the connector, but confirm:
```bash
ssh fihadmin@herald-host.internal "systemctl --user status hermes-mobile-connector.service --no-pager | head -8"
```
Expected: still `active (running)` from the prior session — no restart needed unless it has stopped.

- [ ] **Step 5: Build and install iOS app on both devices**

Follow the standard build/export/install pattern (entitlements-strip, archive, export, `xcrun devicectl device install app` for both iPhone `8BE18B66-1D74-5495-82FA-F8A74B505947` and iPad `A1AB3152-5CA0-5E28-9431-92BF4AC3312C`), restoring entitlements after.

- [ ] **Step 6: Generate fresh pairing codes if needed**

If either device needs re-pairing (shouldn't, since this isn't a fresh install):
```bash
ssh fihadmin@herald-host.internal "CREDS=\$(cat /home/agent/.hermes/profiles/ignyte/home/.hermes-mobile/state.json | python3 -c 'import json,sys; print(json.load(sys.stdin)[\"connector_credential\"])') && curl -s http://localhost:8010/v1/connector/phone-pairing-codes -X POST -H \"Authorization: Bearer \$CREDS\""
```

- [ ] **Step 7: Manual verification checklist on-device**

- [ ] Send a message, confirm keyboard dismisses automatically
- [ ] Press Return in the composer with text typed, confirm it sends (not a newline)
- [ ] On iPad: navigate to Inbox/Talk/Settings, confirm a "Chat" row is visible and clickable to return
- [ ] On iPhone: with drawer closed, swipe from the left edge, confirm the drawer opens
- [ ] Tap "new session" from a non-Chat section, confirm it navigates to Chat and shows the new session
- [ ] Confirm the Profile chip is now visible in the chat toolbar (even without a host-side default profile configured)
- [ ] Create a new conversation, send a first message, confirm the sidebar shows a derived title (not "Hermes") after a refresh
- [ ] On iPad, open the right panel's Logs tab, send a message, confirm log entries appear ("Streaming started", "Message accepted — job ...")

- [ ] **Step 8: Final commit**

```bash
cd ~/Hermes-iOS
git commit --allow-empty -m "chore: verify UI fixes, job retry resilience, session titling, live logging end-to-end"
```
