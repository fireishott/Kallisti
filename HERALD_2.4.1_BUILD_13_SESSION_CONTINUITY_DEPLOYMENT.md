# Herald 2.4.1 (13) — Hermes session-continuity deployment

## Production finding

The live connector log on `fih-ai-host` proved the failure at 18:25 PDT:

1. Herald posted a message and opened its job SSE stream.
2. The app-facing conversation UUID resolved to an existing Hermes `run_…`
   session in the connector sidecar.
3. The connector started `/v1/runs` with the session only in
   `X-Hermes-Session-Id`.
4. Hermes ignored that header on the runs endpoint, created a new `run_…`
   session, and therefore had no earlier context.

The connector now sends the raw resolved Hermes ID as the documented JSON field
`session_id` in the `/v1/runs` request. The existing header remains harmless
compatibility metadata, but is not relied on for runs continuity.

## Claude Code / Superpowers task

```text
Use Superpowers and work in /Users/you/Herald. Read this file first.
Preserve all current build-12 changes; do not reset, stash, or delete the
working tree. Verify that HeraldAPIExecutor._runs_request_payload includes
session_id only when one was supplied, then execute all local, connector, and
physical-device gates below. Do not print credentials, tokens, full message
contents, or database rows. Stop rollout if session continuity is not proven by
IDs and timestamps.
```

## Local gate

```sh
cd /Users/you/Herald
git diff --check
connector/.venv/bin/python -m pytest \
  connector/tests/test_runs_streaming.py \
  connector/tests/test_session_resolution.py -q
xcodegen generate
xcodebuild test -project Herald.xcodeproj -scheme Herald \
  -destination 'platform=iOS Simulator,name=iPhone 16 Pro' \
  -only-testing:HeraldTests/B40ConversationMergeTests
xcodebuild -showBuildSettings -project Herald.xcodeproj -scheme Herald | \
  grep -E 'MARKETING_VERSION|CURRENT_PROJECT_VERSION'
```

Expected version/build: `2.4.1 (13)`. Do not deploy an iOS build 13 with a
connector that lacks the `/v1/runs` `session_id` payload fix.

## Connector preflight and deployment

Use the approved SSH identity. The connector is a `systemctl --user` service
on `fihadmin`; do not use `sudo systemctl`.

```sh
ssh fihadmin@192.168.10.118 '
  set -eu
  systemctl --user status hermes-mobile-connector.service --no-pager
  cd /home/agent/Hermes-iOS/connector
  git status --short
  git rev-parse HEAD
  git diff -- src/herald_connector/herald_api_executor.py
'
```

The host contains historical backup files and may be dirty. Preserve its
pre-deploy patch and record its SHA before applying the reviewed release commit.
Never delete `session_meta.json` or modify Hermes `state.db` to “clean up” the
duplicates; both contain the evidence/mappings needed for continuity.

After checking out the reviewed release SHA:

```sh
cd /home/agent/Hermes-iOS/connector
.venv/bin/python -m pytest tests/test_runs_streaming.py tests/test_session_resolution.py -q
systemctl --user restart hermes-mobile-connector.service
systemctl --user is-active hermes-mobile-connector.service
journalctl --user -u hermes-mobile-connector.service -n 100 --no-pager
```

## Mandatory live proof

Use one harmless unique marker in an existing Herald chat that already has a
known app UUID and Hermes session mapping. Record only identifiers/timestamps.

1. Before Send, record selected `conversationId` and its sidecar
   `_hermes_id`.
2. On the connector, record the job ID and the `/v1/runs` request payload shape
   (presence of `session_id`, not its value).
3. Confirm Hermes reports the same raw session ID in the run status/terminal
   event; it must not create a different `run_…` session.
4. Reload `GET /v1/sessions/{conversationId}/conversation`; it must include
   the old turns and the new user/assistant turn, in order.
5. Refresh the sidebar. There must be one row for the conversation—not a
   second row with the same opening text.

Fail the release if connector logs contain either of these for the marker:

- `Runtime reported session None`;
- `message was written to run_…` where that ID differs from the selected
  conversation's mapped Hermes ID.

## Device acceptance

Install the signed 2.4.1 (13) archive on iPhone, then test iPad with the same
connector revision. In one existing thread, ask a follow-up that requires a
specific earlier turn. Hermes must reference that prior turn. Repeat after
background/foreground and after explicitly reopening the thread from the
sidebar. Verify no new duplicate session appears.

## Rollback

If continuity proof fails, restore the recorded connector SHA and restart only
`hermes-mobile-connector.service` as `fihadmin`. Reinstall the previous signed
iOS archive if build 13 was installed. Do not delete duplicate session rows or
sidecar mappings during rollback.
