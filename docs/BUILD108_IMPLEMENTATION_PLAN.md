# Build 108 Implementation Plan

## Overview

Build 108 makes the chat transcript a deterministic projection of one durable server ledger. A user message must remain visible, in the same position, with the same content, before, during, and after streaming, navigation, app relaunch, connector restart, snapshot refresh, and retry.

## Phase 1: Canonical Wire Contract (Workstream A)

### 1.1 Define the Ledger Schema

Create a new `conversation_messages` table in the delivery store:

```sql
CREATE TABLE IF NOT EXISTS conversation_messages (
    canonical_message_id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL REFERENCES conversation_bindings(app_conversation_id),
    sequence INTEGER NOT NULL,
    revision INTEGER NOT NULL,
    role TEXT NOT NULL CHECK(role IN ('user', 'assistant', 'system', 'tool', 'reasoning')),
    client_message_id TEXT NULL,
    job_id TEXT NULL,
    hermes_message_id TEXT NULL,
    content TEXT NOT NULL,
    display_content TEXT NOT NULL,
    model_input_content TEXT NULL,
    state TEXT NOT NULL CHECK(state IN ('pending', 'accepted', 'running', 'terminal', 'failed', 'cancelled')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(conversation_id, sequence),
    UNIQUE(conversation_id, client_message_id) WHERE client_message_id IS NOT NULL,
    UNIQUE(job_id, revision) WHERE job_id IS NOT NULL
);
```

### 1.2 Add Conversation Revision Tracking

Add a `revision` column to `conversation_bindings`:

```sql
ALTER TABLE conversation_bindings ADD COLUMN revision INTEGER NOT NULL DEFAULT 0;
```

### 1.3 Update Atomic Acceptance Contract

Modify the message acceptance flow to commit in one transaction:
1. Canonical conversation binding
2. Canonical user message with sequence
3. Request idempotency record
4. Job record
5. New conversation revision
6. Initial accepted event

### 1.4 Update Snapshot and Event Contracts

Ensure every snapshot and event carries:
- `canonicalConversationId`
- `conversationRevision`
- `canonicalMessageId`
- `sequence`
- `role`
- `clientMessageId` (user rows)
- `jobId` (assistant/tool/reasoning rows)
- `messageRevision`
- `state`
- `displayContent`

## Phase 2: Migration (Workstream B)

### 2.1 Backup Active Database

Create a backup of the active connector delivery database before migration.

### 2.2 Idempotent Schema Migration

Add migration logic to:
1. Create the `conversation_messages` table
2. Add `revision` column to `conversation_bindings`
3. Import existing user/assistant display rows
4. Strip system-context envelopes from imported user display content
5. Assign sequences using stable database row identity
6. Resolve aliases to binding that owns the Hermes session
7. Verify unique constraints

### 2.3 Migration Report

Generate report with counts for:
- Imported rows
- Deduplicated rows
- Alias-resolved rows
- Quarantined rows
- Failed rows

## Phase 3: iOS Reducer (Workstream C)

### 3.1 Create Actor-Isolated Reducer

Create `TranscriptReducer` actor responsible for all message state transitions:
- Optimistic send
- HTTP acknowledgement
- Stream events
- Terminal events
- Snapshots
- Retries
- Navigation
- Reconnects
- App restoration

### 3.2 Identity Matching Rules

1. Match user row by `clientMessageId` first
2. Match assistant row by canonical message ID, then `jobId` plus message revision
3. Never use equal content as identity

### 3.3 Row Lifecycle Rules

1. Never remove optimistic user row until connector acknowledges
2. Acknowledgement upgrades existing row, does not append
3. Snapshot with revision lower than reducer's revision is discarded
4. Snapshot for non-active canonical conversation is cached only
5. Stream event from prior conversation cannot change visible thread

### 3.4 Navigation Cancellation

Session switch cancels:
- Old loads
- Polling tasks
- Stream subscriptions

Every post-await mutation rechecks generation and canonical conversation ID.

### 3.5 Stabilize Projection

1. `loadConversation(id:)` captures generation and requested canonical ID before await
2. Guard both afterward
3. Reconcile rows into reducer's keyed state
4. Render stable `sequence` projection
5. Cache writes occur after reducer commit

## Phase 4: Retry and Duplicate Semantics (Workstream D)

### 4.1 Duplicate ClientMessageId Handling

- If existing request accepted/running: return current job and canonical rows
- If terminal: return canonical terminal snapshot, mark outbox reconciled
- If retryable failure: retry with explicit attempt number, preserve user sequence
- If permanent failure: keep user row visible, offer edited resend

### 4.2 Regenerate Semantics

- Separate user action targeting assistant response
- Creates new response revision after same user row
- Never inferred from empty placeholder or duplicate terminal response

### 4.3 Premature Regenerate Fix

Remove premature regenerate UI path caused by settling live empty placeholder. Stream ownership and server state, not empty text, decide whether response failed.

## Phase 5: System Context as Invisible Metadata (Workstream E)

### 5.1 Wire Contract Change

- iOS sends `displayText` separately from structured client context
- Connector constructs model input server-side
- `displayText` is only content persisted to user-role transcript ledger
- No string-prefix cleanup in Swift

### 5.2 Regression Test

Add test that sends text beginning with `[System context` as literal user content; it must remain literal.

## Phase 6: Talk Configuration (Workstream F)

### 6.1 Remove iOS Key Management

- Remove MiMo API-key field from iOS Settings
- Remove **Update API Key** from Talk Mode
- Remove device-Keychain MiMo credential injection
- Migrate/delete obsolete Keychain entry during app upgrade

### 6.2 Hermes Host Configuration Discovery

1. Discover active Hermes provider using Hermes config/environment loading
2. Load API key and base URL from same server-side source
3. Prefer shared provider adapter or Hermes-supported API

### 6.3 Talk Readiness Response

Return typed readiness fields without secrets:
```json
{
  "configured": true,
  "credentialSource": "hermesHostEnvironment",
  "provider": "xiaomi",
  "baseURLHost": "token-plan-sgp.xiaomimimo.com",
  "asrModel": "mimo-v2.5-asr",
  "ttsModel": null,
  "lastProbeAt": "2026-08-01T18:00:00Z",
  "probeKind": "realAudioCanary",
  "status": "ready"
}
```

### 6.4 Audio Canary

Add fixture-audio canary that:
- Exercises same authenticated ASR route used by iOS
- Asserts non-empty transcript
- Cache with explicit TTL
- Provide manual refresh

### 6.5 Security

- Never log key in app logs
- Use non-reversible fingerprint or last four characters in host logs
- Move secrets to mode-0600 `EnvironmentFile`

## Phase 7: Hermes Restart (Workstream G)

### 7.1 Restart Authorization

- Use paired bearer token for client authentication
- Separate server-side policy for privileged restart authorization
- Do not ask phone to possess host sudo token

### 7.2 Restart Execution

1. Keep existing confirmation dialog
2. State which component will restart
3. State active runs may be interrupted
4. Execute only allowlisted service action
5. Return typed staged result: accepted, stopping, starting, health-checking, ready/failed

### 7.3 Progress Streaming

Stream progress to Settings and disable repeated taps while operation active.

### 7.4 Health Verification

Success requires Hermes API to answer real authenticated health request after restart.

### 7.5 Singleton Verification

Record service PID before/after and ensure connector remains singleton.

## Phase 8: Gateway Logs (Workstream H)

### 8.1 Log Source Identification

Use Hermes logging implementation as primary reference:
- Gateway logs
- Agent logs
- Error logs
- GUI logs
- Desktop logs
- MCP logs

### 8.2 Endpoint Requirements

- History endpoint returns cursor and bounded records
- Stream endpoint resumes from cursor and emits heartbeats
- Source field identifies log type
- Log-level filtering is server-side
- File rotation and truncation handled
- No arbitrary path parameter
- Secrets redacted before data leaves host

### 8.3 Physical Canary

1. Start one chat run with unique correlation token
2. Observe it in Hermes gateway stream
3. Prove connector-only log line does not appear when source is gateway

## Phase 9: Update Check (Workstream I)

### 9.1 Hermes CLI Integration

Use Hermes CLI/update mechanism supported by installed Hermes version.

### 9.2 Response Fields

Return:
- Installed version
- Latest available version
- Channel/source
- Checked timestamp
- Update availability
- Typed error

### 9.3 Changelog

- Cache only successful results with short explicit TTL
- If update available: View Changelog, Later, Update
- Changelog from same trusted source, sanitized for display

### 9.4 Update Execution

- Report staged progress
- Recheck version and health afterward
- Failed network check is `unknown/error`, never `up to date`

## Phase 10: Structured Correlation (Workstream J)

### 10.1 Logging Fields

Every layer logs:
- `requestId`
- `accountId` or privacy-safe account fingerprint
- `ownerDeviceId` or privacy-safe device fingerprint
- `clientMessageId`
- `canonicalConversationId`
- `hermesSessionId`
- `jobId`
- `canonicalMessageId`
- `sequence`
- `conversationRevision`
- `messageRevision`
- `attempt`
- `event`
- `source`

### 10.2 Diagnostic Endpoint

Add endpoint that returns privacy-safe lifecycle for one `clientMessageId` across:
- Acceptance
- Hermes submission
- Events
- Terminalization
- Snapshot
- Client acknowledgement

### 10.3 iOS Security

iOS app never logs:
- Credentials
- Authorization headers
- Attachment bytes
- Raw model prompts
- MiMo secrets

## Implementation Order

1. **Phase 1**: Canonical Wire Contract (foundation)
2. **Phase 5**: System Context (independent, can parallel)
3. **Phase 2**: Migration (depends on Phase 1)
4. **Phase 4**: Retry/Duplicate Semantics (depends on Phase 1)
5. **Phase 3**: iOS Reducer (depends on Phase 1, 2, 4)
6. **Phase 6**: Talk Configuration (independent)
7. **Phase 7**: Hermes Restart (independent)
8. **Phase 8**: Gateway Logs (independent)
9. **Phase 9**: Update Check (independent)
10. **Phase 10**: Structured Correlation (贯穿 all phases)

## Testing Strategy

### Unit Tests (First)

Write failing tests for each phase before implementation:
- Connector contract tests (12 tests)
- iOS reducer tests (15 tests)
- Talk tests (6 tests)
- Operations tests (7 tests)

### Integration Tests

- Canonical conversation creation
- First message send with sequence capture
- Snapshot fetching during streaming
- Follow-up send without reconnect
- Retry of first client ID
- Connector restart and transcript projection
- Hermes restart with health verification
- Talk audio canary
- Gateway log correlation
- Update check

### Production Canary Sequence

1. Health, version, singleton, port ownership, database migration
2. Create new canonical conversation
3. Send unique first message
4. Fetch five snapshots during streaming
5. Send follow-up without reconnecting
6. Retry first client ID
7. Restart connector and refetch
8. Exercise confirmed Hermes restart
9. Run real Talk audio canary
10. Run gateway-log correlation canary
11. Run update/check changelog canary

## Definition of Done

Build 108 is done only when:
- Canonical ledger is sole transcript authority
- iOS renders that ledger deterministically
- Retries are idempotent
- System context is invisible
- Two consecutive turns work without reopening app
- Talk uses Hermes-host MiMo configuration with no client key UI
- Restart/log/update operations are real
- All production entitlements are preserved
- Connector is deployed
- Build 108 is uploaded to TestFlight
- Physical-device gates pass with evidence
