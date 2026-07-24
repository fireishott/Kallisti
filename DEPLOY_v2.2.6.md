# Herald v2.2.6 (build 63) — Deployment Instructions

**Date:** 2026-07-24  
**All 4 phases combined into a single release**  
**Target:** TestFlight (iOS) + Relay deploy (fih-ai-host)

---

## Pre-Flight Checklist

- [ ] All code changes committed (`git status` clean)
- [ ] Xcode project regenerated (`xcodegen generate`)
- [ ] Keychain unlocked on build Mac
- [ ] Relay changes reviewed and tested
- [ ] SSH access to fih-ai-host (herald-host.internal) confirmed

---

## Step 1: Commit All Changes

```bash
cd [project-root]/Herald
git add -A
git status  # Verify all changed files are staged
git commit -m "Release v2.2.6 (build 63): crash fix, /new routing, streaming, reasoning, push, settings, scroll, auto-compress, session isolation

Phase 1 (Critical):
- TalkAudioCapture: extract tap handler to nonisolated static (P0 crash fix)
- /new routes to createNewSession() instead of clearConversation()
- Haptic delayed 100ms after stream end to prevent render race

Phase 2 (Streaming/Push):
- Watchdog: 90s → 30s, flush interval: 33ms → 16ms
- Reasoning: extract <think> tags to message.reasoning before stripping
- Skip reloadConversationForStreaming when donePayload has message
- Inbox items created independently of push delivery (relay)

Phase 3 (UX):
- Settings gear: switchToTab(.settings) instead of sheet presentation
- Scroll debounce: Task-based 100ms coalescing replaces 500ms throttle

Phase 4 (Features):
- Auto-compress triggers /compress at 85% context
- Session-scoped conversation cache (currentSessionId)
- createNewSession() wires up session isolation automatically

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

## Step 2: Deploy Relay Updates to fih-ai-host

```bash
# Copy updated relay to the host
scp [project-root]/Herald/relay/app/main.py fihadmin@herald-host.internal:/opt/herald-relay/app/main.py

# SSH in and restart
ssh fihadmin@herald-host.internal 'sudo systemctl restart herald-relay'

# Verify it came back healthy
sleep 3
curl -s http://herald-host.internal:8766/v1/health | python3 -m json.tool
```

**Expected output:** `{"status": "ok", "data": {"status": "healthy", ...}}`

---

## Step 3: Build iOS App

On the build Mac (Build-Mac):

```bash
# 1. Unlock keychain (REQUIRED before every xcodebuild)
security unlock-keychain -p "$(security find-generic-password -w -s 'login-keychain')" ~/Library/Keychains/login.keychain-db 2>/dev/null || \
security unlock-keychain ~/Library/Keychains/login.keychain-db

# 2. Regenerate project (if not already done)
cd [project-root]/Herald
xcodegen generate --spec project.yml --project .

# 3. Archive build
xcodebuild -project Herald.xcodeproj \
  -scheme Herald \
  -configuration Release \
  -archivePath Herald.xcarchive \
  -destination 'generic/platform=iOS' \
  archive \
  CODE_SIGN_STYLE=Automatic \
  DEVELOPMENT_TEAM=58U7UPFS53

# 4. Verify archive exists
ls -la Herald.xcarchive/
```

---

## Step 4: Strip Entitlements + Export IPA

```bash
# Strip problematic entitlements for TestFlight distribution (paid team)
cp Herald.xcarchive/Products/Applications/Herald.app/Herald.entitlements \
   Herald.xcarchive/Products/Applications/Herald.app/Herald.entitlements.bak

/usr/libexec/PlistBuddy \
  -c "Delete :com.apple.developer.carplay-communications" \
  -c "Delete :aps-environment" \
  Herald.xcarchive/Products/Applications/Herald.app/Herald.entitlements 2>/dev/null

# Export IPA
xcodebuild -exportArchive \
  -archivePath Herald.xcarchive \
  -exportPath Herald-export \
  -exportOptionsPlist ExportOptions.plist \
  CODE_SIGN_STYLE=Automatic

# Verify IPA
ls -la Herald-export/Herald.ipa
```

---

## Step 5: Upload to TestFlight

```bash
xcrun altool --upload-app \
  -f Herald-export/Herald.ipa \
  -t ios \
  --apiKey "$APP_STORE_CONNECT_API_KEY_ID" \
  --apiIssuer "$APP_STORE_CONNECT_ISSUER_ID"
```

Or use Transporter app if `altool` is deprecated:
```bash
xcrun iTMSTransporter -m upload -f Herald-export/Herald.ipa
```

---

## Step 6: End-to-End Verification

After installing the TestFlight build on an iPhone (iOS 26.6):

### P0 — Crash Verification
- [ ] Open Herald → Tap Talk tab → **No crash within 60 seconds**
- [ ] Start voice session, let it run for 30s, stop → **No crash**
- [ ] Monitor Xcode crash organizer for any new `DLv6v_kVaI3SezmMxhpnMD` occurrences

### P1 — Streaming & Response
- [ ] Send a message in Chat → **Response streams word-by-word** (not all at once)
- [ ] Send a message that produces reasoning (complex question) → **Reasoning panel appears and stays visible after completion**
- [ ] Wait for response → **Response arrives within reasonable time** (not 90s+ of "Waiting for host...")
- [ ] With the app backgrounded, wait for a response → **Local notification fires with message preview**

### P1 — /new Routing
- [ ] Type `/new` → **New empty chat appears**
- [ ] Send a message in the new chat → **Response stays in THIS chat**
- [ ] Switch to another session from sidebar → **Correct conversation history loads**
- [ ] Hit "New" button in context warning banner → **New session created, not old one cleared**

### P1 — Thinking/Haptic
- [ ] Wait for a streaming response to complete → **Haptic fires AND response is visible simultaneously**
- [ ] No "Thinking disappears → haptic → blank screen" sequence

### P2 — Settings
- [ ] Tap gear icon in chat toolbar → **Switches to Settings tab** (not sheet)
- [ ] Settings tab → **No left-right bounce when swiping**
- [ ] Swipe between tabs → **Smooth transitions, no overshoot at edges**

### P2 — Scroll
- [ ] Send a message that produces a long streaming response → **Chat scrolls smoothly, doesn't fly off screen**
- [ ] Scroll up during streaming → **Auto-scroll defers to user**
- [ ] Send another message → **Auto-scroll resumes**

### P3 — Action Center
- [ ] Receive a response → **Inbox tab shows the new item**
- [ ] Inbox item → **Tapping navigates to correct conversation**

### P3 — Auto-Compress
- [ ] Send many messages until context ring shows >85% → **Auto-compress fires** (banner appears briefly)
- [ ] Context ring drops after compression → **Continues working normally**
- [ ] Send more messages → **Does NOT compress again** (once per conversation)

### P3 — Session Isolation
- [ ] Chat in session A for a while → **Switch to session B → B's history loads**
- [ ] Switch back to session A → **A's history is preserved** (not B's or empty)

---

## Step 7: Rollback Plan

If v2.2.6 introduces a regression:

### iOS Rollback
```bash
cd [project-root]/Herald
git checkout v2.2.5  # or the last known-good tag
# Rebuild and upload via Steps 3-5 above
```

### Relay Rollback
```bash
ssh fihadmin@herald-host.internal 'cd /opt/herald-relay && git checkout v2.2.5 && sudo systemctl restart herald-relay'
```

---

## Files Changed

### iOS (Herald/)
| File | Change |
|------|--------|
| `project.yml` | Version 2.2.5→2.3.0, build 62→63 |
| `Herald/Resources/Info.plist` | Version bumps |
| `Services/Live/TalkAudioCapture.swift` | Nonisolated static tap handler (P0 crash fix) |
| `Features/Chat/ChatScreen.swift` | /new→createNewSession, haptic delay, scroll debounce, gear→switchToTab |
| `Stores/ChatStore.swift` | Watchdog 90→30s, flush 33→16ms, reasoning extraction, auto-compress |
| `Services/Live/LiveHeraldClient.swift` | Skip reload when donePayload has message |
| `Services/Support/UserDefaultsAppPersistenceStore.swift` | Session-scoped conversation cache |
| `Services/Protocols/AppPersistenceStoreProtocol.swift` | Added currentSessionId |
| `Stores/SessionListStore.swift` | Sets currentSessionId on switch/create |
| `CHANGELOG.md` | v2.2.6 entries |

### Relay (relay/app/)
| File | Change |
|------|--------|
| `main.py` | Inbox creation independent of push delivery (2 sites) |
