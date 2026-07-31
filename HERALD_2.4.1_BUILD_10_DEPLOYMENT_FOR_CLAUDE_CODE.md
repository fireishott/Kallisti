# Herald 2.4.1 (10) — Claude Code / Superpowers deployment runbook

## Objective

Deploy the iOS and connector fixes as one compatible release. The release has
three non-negotiable invariants:

1. A device always sends to and reloads its explicitly selected conversation
   UUID; it never uses a connector-global “current” conversation.
2. A pending-message acknowledgement never replaces the complete local
   transcript. Streamed text, reasoning, and tool activity remain one turn
   until an explicit history read reconciles it.
3. A draft app UUID and the Hermes-derived UUID for that same session produce
   one session-list row, not two.

## Claude Code instructions

Paste this task into Claude Code after activating the Superpowers plugin:

```text
Use Superpowers for this release. Work in /Users/you/Herald.
Do not change scope, secrets, production data, signing settings, or service
units. First read HERALD_2.4.1_BUILD_10_DEPLOYMENT_FOR_CLAUDE_CODE.md in full.
Perform every preflight and test gate in order. Stop before any deployment if a
gate fails. Preserve exact command output, commit SHA, image/service revision,
and device-test markers in the release report. Never print tokens, passwords,
APNs tokens, or database content.
```

## 1. Local preflight and test gate

From the repository root:

```sh
git status --short
git diff --check
git log -1 --oneline
connector/.venv/bin/python -m pytest connector/tests/test_session_resolution.py connector/tests/test_streaming.py -q
xcodegen generate
xcodebuild test -project Herald.xcodeproj -scheme Herald \
  -destination 'platform=iOS Simulator,name=iPhone 16 Pro' \
  -only-testing:HeraldTests/B40ConversationMergeTests
xcodebuild -project Herald.xcodeproj -scheme Herald \
  -configuration Release -destination 'generic/platform=iOS' build
```

Expected results: clean diff check, all targeted connector tests pass, targeted
iOS tests pass, and the release build succeeds. Confirm the generated project
contains `MARKETING_VERSION = 2.4.1` and `CURRENT_PROJECT_VERSION = 10` before
archiving. Do not use an archive whose Info.plist says build 9 or lower.

## 2. Commit and rollback point

Create one intentional commit after the gate passes. Record its SHA. On the
connector host, first record the active revision, systemd user unit, environment
file path, and service status. Preserve any existing dirty host checkout as a
patch before replacing it. Do not use `sudo systemctl`: this connector runs as
the `fihadmin` user service.

```sh
git status --short
git add Herald connector project.yml Herald.xcodeproj HERALD_2.4.1_BUILD_10_DEPLOYMENT_FOR_CLAUDE_CODE.md
git commit -m 'fix: preserve conversation identity and stream ordering'
git rev-parse HEAD
```

## 3. Connector deployment on fih-ai-host (.118)

Use the approved SSH identity and replace `RELEASE_SHA` only with the committed
SHA from step 2.

```sh
ssh fihadmin@192.168.10.118 '
  set -eu
  systemctl --user status hermes-mobile-connector.service --no-pager
  systemctl --user show hermes-mobile-connector.service -p FragmentPath -p EnvironmentFiles
  readlink -f /proc/$(systemctl --user show -p MainPID --value hermes-mobile-connector.service)/cwd
  journalctl --user -u hermes-mobile-connector.service -n 100 --no-pager
'
```

Verify that the discovered checkout is `/home/agent/Hermes-iOS/connector`.
If it is not, stop and update this runbook with the real path; do not deploy to
an inferred directory.

On the host, in the confirmed checkout:

```sh
set -eu
cd /home/agent/Hermes-iOS/connector
git status --short
git rev-parse HEAD
git fetch --all --tags --prune
git checkout RELEASE_SHA
.venv/bin/python -m pytest tests/test_session_resolution.py tests/test_streaming.py -q
systemctl --user restart hermes-mobile-connector.service
systemctl --user is-active hermes-mobile-connector.service
systemctl --user status hermes-mobile-connector.service --no-pager
journalctl --user -u hermes-mobile-connector.service -n 150 --no-pager
```

Roll back only if the service fails health/test gates: check out the recorded
previous SHA and restart the same user service. Do not alter the Hermes state
database or delete `session_meta.json`; this release must preserve existing
session mappings.

## 4. Connector smoke checks

From an approved authenticated client, verify these endpoints return valid JSON
without exposing credentials:

```text
GET /v1/models                 → activeModel.name is present when config has a model
GET /v1/sessions?limit=50      → no duplicate row for a known draft/canonical pair
GET /v1/sessions/{UUID}/conversation → returned conversation.id equals requested UUID
POST /v1/messages              → acknowledgement conversation.id equals requested UUID
GET /v1/jobs/{job}/events      → monotonic SSE ids; one terminal done event
```

For a single marker turn, capture only: marker, iPhone/iPad, selected
conversation UUID, client-message UUID, job UUID, first `reasoning_delta`,
first `text_delta`, terminal event, and final conversation UUID. Do not log
message content beyond the harmless marker.

## 5. Archive and install iOS build 2.4.1 (10)

Use the team’s established signing/export configuration. Archive only after the
connector is healthy:

```sh
xcodebuild archive -project Herald.xcodeproj -scheme Herald \
  -configuration Release -destination 'generic/platform=iOS' \
  -archivePath build/Herald-2.4.1-10.xcarchive
xcodebuild -showBuildSettings -project Herald.xcodeproj -scheme Herald | \
  grep -E 'MARKETING_VERSION|CURRENT_PROJECT_VERSION'
```

Install the same signed archive on one iPhone and one iPad. Do not test an iPad
with build 10 against an iPhone still on an earlier connector contract.

## 6. Physical-device acceptance matrix

Use fresh, unique markers. A pass requires every assertion below.

| Scenario | Required assertion |
|---|---|
| iPad launch | The toolbar model pill shows the configured model name, or visibly says `Model unavailable`; it never renders as an unlabeled toggle/icon. |
| Same-chat reasoning | Send one tool/reasoning-heavy marker turn. Thought process, tool rows, and final response remain inside one assistant turn in emitted stream order. |
| Duplicate guard | Background/foreground and force one SSE reconnect during a response. The final answer appears once, and there is one session-list row. |
| iPhone → iPad | Start a new chat on iPhone, then open the iPad. The iPad remains in its own selected chat until the user explicitly opens the iPhone chat. |
| iPad → iPhone | Repeat in the other direction. A new chat must send its first POST with that device’s selected `conversationId`, never an omitted/global id. |
| Same session on both devices | Explicitly open one shared session on both devices and send sequential turns. Both see one canonical history with no duplicated assistant message. |

If any response appears above its prompt, in another chat, twice, or without the
model label, stop rollout. Export the IDs/timestamps listed in step 4 and roll
back the connector or app to the recorded prior artifact as appropriate.

## 7. Release report

Report: source SHA, connector SHA, connector service status, iOS archive path,
version/build, test outputs, six-device-matrix result, and any rollback. State
explicitly whether the app used the direct connector facade or the production
relay during the physical test; the two paths must not be conflated.
