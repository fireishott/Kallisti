# Mimo Continuation Orders — Herald v1.7.4 Deployment

**Date:** 2026-07-21
**Status:** All code fixes applied in repo. One compile fix uncommitted. Nothing pushed or deployed yet.

---

## What's Already Done (DO NOT REDO)

All six fixes from the original marching orders are committed in `f5e8da7`:
- F1: `NotesRepository.swift` date decoding — `.iso8601` added
- F2: `relay/app/notes.py` `_parse_body` — made async, callers `await` it
- F3: `relay/app/notes.py` — `create_note` and `create_run` use `JSONResponse`
- F4: `ChatStore.swift` — robust NSRegularExpression `<think>` stripping; `MarkdownContentView.swift` — `hasStreamedReasoning` guard
- F5: `connector/client.py`, `runtime_adapter.py`, `herald_api_executor.py` — `reasoning_effort` threaded through
- F6: `AppContainer.swift` — `DashboardLogService` instantiated; `iPadRightPanelView.swift` — Gateway tab added
- Version bumped to 1.7.4 / build 40 in both `project.yml` and `project.pbxproj`

---

## What Remains (execute in order)

### Step 1: Commit the compile fix

Mimo found a compile error: `NSRegularExpression.stringByReplacingMatches` uses `withTemplate:` not `replacement:`. The fix is already applied but unstaged.

```bash
cd ~/Herald
git add Herald/Stores/ChatStore.swift Herald.xcodeproj/project.pbxproj
git commit -m "fix: correct NSRegularExpression API (withTemplate not replacement)"
```

The `project.pbxproj` change is the version bump that xcodegen regenerated — safe to include.

### Step 2: Push to origin

```bash
git push origin master
```

This pushes both `f5e8da7` (all v1.7.4 fixes) and the compile fix commit.

### Step 3: Build and install iOS app on device

```bash
# Unlock keychain FIRST (required before every xcodebuild)
security unlock-keychain -p "$(security find-generic-password -s 'login' -w)" ~/Library/Keychains/login.keychain-db

# Regenerate Xcode project (project.yml was updated)
cd ~/Herald
xcodegen generate

# Build for device
xcodebuild -scheme Herald -configuration Debug \
  -destination 'platform=iOS,name=Curtis iPad' \
  build
```

Install via Xcode onto both iPhone and iPad. Verify before proceeding to server deployment:
- Notes tab shows existing notes (not "0 notes")
- Create a new note — appears in list
- Kill and relaunch app — notes persist
- Gateway tab visible in iPad right panel

### Step 4: Deploy relay to host

```bash
ssh fihadmin@herald-host.internal

# Pull latest code
cd ~/Hermes-iOS
git fetch origin
git pull origin master

# Verify the fixes are present
grep -c "async def _parse_body" relay/app/notes.py        # expect 1
grep -c "JSONResponse" relay/app/notes.py                  # expect 2+
grep -c "await _parse_body" relay/app/notes.py             # expect 3

# Copy updated relay to deploy directory
cp -r relay/app/* ~/deploy/hermes-relay/relay/app/
cp relay/pyproject.toml ~/deploy/hermes-relay/relay/

# Backup .env and rebuild
cd ~/deploy/hermes-relay
cp .env .env.backup.$(date +%s)
docker compose build relay
docker compose up -d relay

# Verify health
sleep 5
curl -s http://localhost:8010/health | jq .
docker logs hermes-relay-relay-1 --tail 20
```

Test relay notes API:
```bash
curl -s -X POST http://localhost:8010/v1/notes \
  -H "Authorization: Bearer $INTERNAL_API_KEY" \
  -H "Content-Type: application/json" \
  -d '{"title": "Test Note From Curl"}' | jq .
# Should return {"data": {"id": "...", "title": "Test Note From Curl", ...}}
# NOT [{"data": ...}, 201]
```

### Step 5: Deploy connector to host

```bash
# Still on fihadmin@herald-host.internal
cd ~/Hermes-iOS

# Verify connector fixes are present
grep -c "reasoning_effort" connector/src/herald_connector/client.py          # expect 1
grep -c "reasoning_effort" connector/src/herald_connector/runtime_adapter.py # expect 2
grep -c "reasoning_effort" connector/src/herald_connector/herald_api_executor.py  # expect 3+

# Restart connector
sudo systemctl restart hermes-mobile-connector.service
sudo systemctl status hermes-mobile-connector.service
journalctl -u hermes-mobile-connector.service -n 20 --no-pager
```

### Step 6: End-to-end verification

From the iPad app after all deployments:

| Test | Expected |
|------|----------|
| Open Notes → create note with title | Note appears, title persists across relaunch |
| Send a chat message | Single thinking bubble during streaming, single after completion |
| Toggle "Show Reasoning" off in Settings | Thinking bubble hides on existing messages |
| Set Reasoning Effort to "off", send message | Response has no thinking tokens |
| Set Reasoning Effort to "high", send message | Response shows thinking |
| iPad right panel → Gateway tab | Shows connection state to :9119 dashboard |

### Step 7: Tag the release

```bash
# Back on MBP
cd ~/Herald
git tag v1.7.4
git push origin v1.7.4
```

---

## Environment Reminder

| Component | Location | Port |
|-----------|----------|------|
| Relay (Docker) | `fihadmin@herald-host.internal`, deploy dir `/home/agent/deploy/hermes-relay` | 8010 |
| Postgres (Docker sidecar) | Same host, container `hermes-relay-postgres-1` | internal only |
| Connector (systemd) | Same host, service `hermes-mobile-connector.service`, source at `~/Hermes-iOS/connector/` | n/a |
| Hermes agent | Same host, `api_server` | 8642 |
| Dashboard | Same host, separate process | 9119 |
| Deploy dir is NOT a git repo | Code must be copied from `~/Hermes-iOS/relay/` after `git pull` | — |
