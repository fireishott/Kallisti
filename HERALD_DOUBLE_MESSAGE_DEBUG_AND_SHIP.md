# Herald "Double Message" Bug — Debug Report and Ship Guide (for Claude Code)

**Status:** Phase 1 (root cause) is IN PROGRESS, not closed. This document hands off exactly where the investigation stopped, with everything confirmed so far, the leading hypothesis, and the exact next steps to finish root-causing BEFORE writing a fix. Do not skip to a fix without completing the "Remaining investigation" section — that is how the last three Herald regressions got reintroduced.

**Context:** this is a continuation of the Build 116 stabilization work. Auth (dead chat), the wedged conversation, Talk (MiMo key), and "no reply" (dropped/unmirrored assistant replies) are all fixed and verified live. This document is about the remaining, distinct symptom: the user's own message appearing twice in the transcript ("Hey homie" rendered as two bubbles one minute apart).

---

## 0. TL;DR

- **Confirmed: this is NOT a server-side duplicate row.** The connector's delivery ledger has exactly ONE user message row, ONE clientMessageId, ONE conversation binding for the incident under investigation. The server received 3 `POST /v1/messages` calls in a 31-second window but only ever persisted one canonical row (dedup by `client_message_id` worked correctly).
- **Working hypothesis: this is a client-side (iOS) reconciliation bug** in `ChatStore.swift` — the optimistic local bubble and a later "refreshed from server" copy of the same message both end up in the rendered transcript instead of being merged into one.
- **Not yet confirmed:** the exact trigger. The merge function (`mergeConversationMetadata`) IS invoked on every `loadConversation`, and the optimistic row DOES set `clientMessageID` — so the naive "identity data is missing" theory (suggested by an old code comment) does not hold up on inspection. Something more specific is going on. See §3.
- **Environment reminder (still true):** production Hermes = `192.168.10.118`; the relay is sunset (Caddy on `.101` forwards straight to the connector facade on `:8010`); connector is an editable install (`~/Hermes-iOS/connector/src`, restart via `systemctl --user restart hermes-mobile-connector.service`); canonical repo is `/Users/you/Herald` on the MBP, currently at commit `2dc0c9c` on `build30/remediation`.

---

## 1. Confirmed ground truth (server side is clean)

Reproduced from the live incident (2026-08-03, ~08:15-08:16 local / 15:15-15:16 UTC), queried directly against the connector's delivery ledger:

```
conversation_messages row for the incident:
  conversation_id  = 706a6720-a430-4220-adff-54075e6bb7a5
  role             = user
  client_message_id = 110B3B8D-1A8D-4454-8F14-D6AF83A4C3B9
  content          = "Hey homie"
  created_at       = 2026-08-03T15:15:53Z
  sequence         = 1                      <- only ONE row, ever

message_requests row:
  client_message_id = 110B3B8D-1A8D-4454-8F14-D6AF83A4C3B9
  state              = terminal
  created_at         = 2026-08-03T15:15:53Z
  updated_at         = 2026-08-03T15:15:59Z   <- turn completed server-side in 6s

conversation_bindings:
  706a6720-...  ->  api-2206873503615fec     <- only ONE binding, no duplicate/competing UUID this time
```

But the raw connector access log shows **three** separate `POST /v1/messages → 200` calls from the same client (192.168.10.101:48570, i.e. one keep-alive connection through Caddy) within a 31-second window: `08:15:32`, `08:15:53`, `08:16:03`. Only the middle one matches the persisted row's timestamp exactly.

**Conclusion so far:** the app POSTed the send more than once (retries — expected/idempotent behavior, and this was BEFORE the "no reply" fix landed, so the app had every reason to think the send hadn't gone through). The server correctly deduped every retry down to one row. The two visible "Hey homie" bubbles are therefore a **client-side rendering artifact**, not a second real message.

Reproduction query (safe, read-only) if you need to re-check a fresh incident:

```bash
ssh fihadmin@192.168.10.118 '
export HERMES_MOBILE_CONNECTOR_HOME=/home/agent/.hermes/profiles/ignyte/home/.hermes-mobile
DB=$HERMES_MOBILE_CONNECTOR_HOME/delivery.sqlite3
/home/agent/Hermes-iOS/connector/.venv/bin/python -c "
import sqlite3
c = sqlite3.connect(\"'"\$DB"'\"); c.row_factory = sqlite3.Row
for r in c.execute(\"SELECT conversation_id,role,client_message_id,content,created_at,sequence FROM conversation_messages WHERE content LIKE %CONTENT_FRAGMENT% ORDER BY created_at\"):
    print(dict(r))
"'
```

(Replace `%CONTENT_FRAGMENT%` with a LIKE pattern for the duplicated text, quoting carefully.)

---

## 2. Leading hypothesis: `ChatStore.swift` reconciliation

### 2.1 The merge pipeline that should prevent this

`Herald/Stores/ChatStore.swift`:

- `sendMessage()` → `enqueueMessage()` (creates the optimistic row, appends it to `conversation?.messages`, `Herald/Stores/ChatStore.swift:401-407`) → `submitNextEligible()`.
- The optimistic row **does** set its identity correctly:
  ```swift
  let clientID = clientMessageID ?? UUID()
  let optimistic = Message(
      id: clientID,
      clientMessageID: clientID,
      sender: .user,
      content: displayContent,
      status: .sending,
      ...
  )
  ```
- `submitNextEligible()` calls `ensureConversation`, then `POST /v1/messages`, then (`ChatStore.swift:662`) `_ = try? await heraldClient.loadConversation(id: targetID)`.
- `loadConversation(id:)` (`ChatStore.swift:310-330`) always routes through the merge function:
  ```swift
  let refreshed = try await heraldClient.loadConversation(id: id)
  conversation = mergeConversationMetadata(from: conversation, into: refreshed)
  ```
- `mergeConversationMetadata` (`ChatStore.swift:2703` onward) has a documented reconciliation hierarchy for a local optimistic row against the server's refreshed copy: **(1)** canonical id, **(2)** `clientMessageID + sender`, **(3)** `jobID + sender`, **(4)** content fingerprint (`"\(sender)|\(normalizedContent)"`, `ChatStore.swift:3159`).

On paper, this should collapse the local optimistic "Hey homie" onto the server's canonical row the moment `loadConversation` runs, via path (2) — both sides have `clientMessageID` populated (confirmed: the ledger row has `client_message_id = 110B3B8D...`, and `_canonical_message_to_relay` — `http_facade.py:3697+` — passes `row.get("clientMessageId")` straight through on every read). So the naive version of the theory ("the connector doesn't return clientMessageId for user rows, so identity-matching fails and the fingerprint fallback saves it — except when it doesn't") does **not** hold up against the *current* code. That comment (`ChatStore.swift:2947-2966`, citing "`session_store._message_to_dict` hardcodes `clientMessageId: None`") describes a historical/legacy read path (`ctx.session_conversation` RPC → `upstream_conv`) whose `messages` array is explicitly discarded in the current handler (`http_facade.py:3806+`, Build 108 Phase 3A v2 correction) in favor of the canonical ledger. That specific gap looks closed.

**So the bug is more specific than "identity is missing."** Something is producing a second, independently-rendered copy of the message that never goes through `mergeConversationMetadata` at all, OR goes through it but fails to match for a reason not yet identified.

### 2.2 What is NOT yet checked (this is the gap)

1. **Is there a second render path that bypasses `loadConversation`/`mergeConversationMetadata` entirely?** E.g. a streaming/SSE handler that appends a message directly to `conversation?.messages` on its own, independent of the merge pipeline. Search `ChatStore.swift` for every other place that does `conversation?.messages.append(` or `conversation?.messages =` besides the optimistic-append at line 488 and the merge assignment at line 320 — if a job-event / SSE listener does its own append of a "confirmed" user row (e.g. echoing back what it thinks the user sent, sourced from a job event rather than the canonical ledger), that would explain two renders that never got a chance to dedupe against each other.
2. **Does the retry path (`retryMessage`, or whatever re-fires `submitNextEligible` for an outbox item stuck in `.retryableFailure`/backoff) create a *new* optimistic row, or does it reuse the existing pending item's row?** If retry logic calls `enqueueMessage` again (which unconditionally builds a new `optimistic` Message and appends it — `ChatStore.swift:401-488`) instead of re-submitting the *same* still-pending `ChatOutboxRecord`, you'd get a second LOCAL optimistic bubble with a **different** `clientMessageID` than the first — which the server would then (correctly) treat as a second, distinct message... except we confirmed server-side there is only one `client_message_id`. So if this is the mechanism, the second call must be failing before it reaches the network (e.g. throttled/deduped by the outbox itself before the POST), while still leaving a second local bubble behind. Check whether the outbox's enqueue path can produce a visible optimistic row that never gets torn down even when the underlying send is a no-op.
3. **Does `mergeConversationMetadata`'s claim logic have an ordering bug** where the *second* `loadConversation` call (there can be more than one in flight — see the `conversationGeneration` guard at `ChatStore.swift:316`, which exists specifically because "navigating to a new chat while a poll response is in-flight could replace the just-loaded conversation with stale data") merges against a **stale** `conversation` snapshot (captured before the first merge completed), effectively re-adding a local row that a concurrent merge had already resolved? The generation counter guards against a *different* conversation's stale response, not against two overlapping refreshes for the *same* conversation racing each other.
4. Confirm whether `POST /v1/messages`'s response body on a **duplicate** hit (`"duplicate": true` — see `delivery_store.create_user_message_atomically`, `delivery_store.py:1421-1465`) is (a) actually propagated by `send_message`'s HTTP response in `http_facade.py`, and (b) checked by the iOS client at all. If the client ignores the duplicate flag and treats every 200 response as "message accepted, render confirmation," a naive per-POST-response render (separate from the merge pipeline) would exactly reproduce this bug across retries — tie this back to point 1.

### 2.3 Concrete next commands (read-only until root cause is nailed)

```bash
# 1. Every place ChatStore.swift mutates conversation.messages directly
#    (not via mergeConversationMetadata) — the prime suspect for a second render path.
grep -n "\.messages\.append\|\.messages\[.*\] =\|\.messages = " Herald/Stores/ChatStore.swift

# 2. Full retry path: does it call enqueueMessage again, or resubmit the existing record?
grep -n "func retryMessage\|func .*[Rr]etry" Herald/Stores/ChatStore.swift
# then read that function in full.

# 3. Confirm the send_message HTTP response shape re: "duplicate"
grep -n "duplicate" connector/src/herald_connector/http_facade.py connector/src/herald_connector/delivery_store.py

# 4. Confirm the iOS decoder for the send response even has a place to notice "duplicate"
grep -rn "duplicate" Herald/Services/Live/LiveHeraldClient.swift Herald/Models/*.swift

# 5. Any SSE/job-event handler that renders a user-role message directly (bypassing the merge)?
grep -n "sender: .user" Herald/Stores/ChatStore.swift Herald/Services/Live/*.swift
```

Read the results of (1) and (2) FIRST — one of those two almost certainly contains the actual defect. Do not write a fix until you can point at the exact line that produces the second bubble and explain why the merge pipeline didn't catch it.

---

## 3. Why this wasn't hot-patched live

Per the debugging discipline used for the last three fixes in this project (auth rehydration, the conversation wedge, "no reply"): **no fix without a demonstrated root cause**, and this investigation was interrupted before that bar was met. The server-side ground truth (§1) rules out a whole category of fixes (anything touching `delivery_store.py`'s dedup-by-`client_message_id`, which is already correct) — don't waste time there. The remaining defect is almost certainly Swift-side, which also means, unlike the last three fixes, **this one likely requires a new iOS build**, not just a connector restart.

---

## 4. Fix (fill in once §2.3 is resolved)

This section is intentionally a template — do not implement blind. Once the exact code path is identified:

1. Write the smallest possible change that stops the second render (likely one of: (a) route the offending append through `mergeConversationMetadata` instead of a direct append, (b) make retry reuse the existing outbox record/optimistic row instead of creating a new one, (c) fix a race in the generation-guarded refresh).
2. Add or extend the existing test coverage — this project already has merge/reconciliation tests (see `HeraldTests/` for anything matching "duplicate", "reconcil", "merge", "SessionIsolation", "TranscriptReducer" — note `TranscriptReducer.swift` and its tests were deleted in commit `3a4070b` as dead code; make sure any new test doesn't resurrect that abandoned path).
3. Verify locally: build for simulator, drive a send through the merge pipeline with a synthetic slow/retried response, and confirm exactly one bubble renders.
4. Bump `CURRENT_PROJECT_VERSION` to `117` (see §5) — `MARKETING_VERSION` stays `2.4.3` per this project's convention (bump build number only; see commit history — `2.4.0` was the one deliberate exception).

---

## 5. Ship guide (iOS build 117 → TestFlight)

This mirrors the Build 116 pipeline exactly (already proven this session). Reuse it as-is once the fix from §4 is committed.

### 5.1 Current known-good state (as of this doc)

- Repo: `/Users/you/Herald`, branch `build30/remediation`, HEAD `2dc0c9c`.
- `project.yml`: `CURRENT_PROJECT_VERSION: "116"` (bump to `"117"` for this fix — all target blocks, `sed`/`Edit` every occurrence).
- Connector: `0.9.4`, already deployed live on `.118` with the `text_delta` and assistant-ledger-materialization fixes (commits `5782830`, `2dc0c9c`) — **no connector changes expected for this bug** unless §2.3 surprises us with a server-side contributor.
- Signing: `iPhone Distribution: C Freeman (58U7UPFS53)`, ASC upload key `32NT26772F` / issuer `69a6de93-5191-47e3-e053-5b8c7c11a4d1`, app id `6792659019` ("Herald Companion"), external testers group `15f612a1-b8f4-450a-8891-7447e932fd5a` ("Herald External Testers").
- Login keychain must be unlocked before archiving (`security unlock-keychain ~/Library/Keychains/login.keychain-db`) — ask the user to run this; do not attempt to type or receive a password directly.
- `scripts/ship-b116.sh` and `ExportOptions-b116.plist` already exist in the repo from the prior release — copy/rename to `-b117` or parameterize the version, whichever is faster; the export destination must be `export` (not `upload`) so `xcodebuild -exportArchive` doesn't double-upload alongside the `altool` step.

### 5.2 Steps

```bash
cd /Users/you/Herald

# 1. Bump build number everywhere CURRENT_PROJECT_VERSION appears
#    (verify with: grep -n CURRENT_PROJECT_VERSION project.yml)
#    then regenerate the Xcode project:
/Users/you/bin/xcodegen generate

# 2. Compile gate BEFORE archiving (catches project.yml drift / stray files early —
#    this caught a real "Multiple commands produce Info.plist" break on Build 116):
xcodebuild -project Herald.xcodeproj -scheme Herald \
  -destination 'generic/platform=iOS Simulator' -configuration Release \
  CODE_SIGNING_ALLOWED=NO build
# Expect: ** BUILD SUCCEEDED **

# 3. Commit the fix + version bump
git add -A   # review `git status` first; do not add connector/dist/*.whl or other stray build artifacts
git commit -m "fix(b117): <one-line description of the double-message fix>"

# 4. Archive -> export -> upload (ask the user to unlock the keychain first)
#    Adapt scripts/ship-b116.sh -> ship-b117.sh (bump ARCH/EXPORT paths), or run inline:
xcodebuild archive \
  -project Herald.xcodeproj -scheme Herald -configuration Release \
  -archivePath build/Herald-117.xcarchive -destination 'generic/platform=iOS' \
  -allowProvisioningUpdates DEVELOPMENT_TEAM=58U7UPFS53

xcodebuild -exportArchive -archivePath build/Herald-117.xcarchive \
  -exportPath build/Herald-117-export \
  -exportOptionsPlist ExportOptions-b116.plist -allowProvisioningUpdates
  # (that file's destination=export works regardless of the number in its name;
  #  fine to reuse, or copy to ExportOptions-b117.plist for clarity)

xcrun altool --upload-app -f build/Herald-117-export/Herald.ipa -t ios \
  --apiKey 32NT26772F --apiIssuer 69a6de93-5191-47e3-e053-5b8c7c11a4d1
```

### 5.3 Link to external testers (after ASC finishes processing — poll, don't guess)

```python
# Same pattern as the Build 116 linker. Poll /v1/builds?filter[app]=6792659019&filter[version]=117
# until attributes.processingState == "VALID", then:
# POST /v1/betaGroups/15f612a1-b8f4-450a-8891-7447e932fd5a/relationships/builds
# {"data": [{"type": "builds", "id": "<build-id>"}]}
```

(The exact JWT/ASC-auth boilerplate used for Build 116 is disposable — regenerate it inline with `PyJWT` + the `.p8` key at `~/.appstoreconnect/private_keys/AuthKey_32NT26772F.p8` rather than hunting for a saved script.)

### 5.4 Verification (do not claim success without this)

1. On a real device (or ask the user), send a message that previously triggered the double-bubble (a fresh, never-before-sent phrase). Confirm exactly one bubble appears and stays.
2. Force the retry path deliberately if possible (e.g. toggle airplane mode for a few seconds mid-send) and confirm still exactly one bubble, not two.
3. Re-run the server-side ground-truth query from §1 against the new send — confirm still exactly one canonical row (this should already hold; it's a regression check, not the primary fix target).
4. Only then report the bug fixed.

---

## 6. Related, still-open items (not in scope for this fix)

- Cosmetic: the producer's `started`/`finish`/`done` event names are still unmapped in `stream_contract.EVENT_TYPE_TO_MODEL` and get dropped with a logged exception. Harmless today because the terminal `run.completed`/`run.failed` events are constructed and published separately (this is how the "no reply" fix's verification succeeded), but worth mapping properly at some point to stop the log spam and exception churn.
- The `_bind_conversation_early` / `_persist_delivery_bindings` pattern (`http_facade.py:650-745`) deliberately attempts to bind BOTH the real `conversationId` and a deterministic alias (`_app_uuid(hermes_sid)`) to the same Hermes session on every turn, and relies on `DuplicateConflictError` being silently absorbed for the alias. This produces a `delivery: binding conflict` WARNING log on effectively every turn (confirmed still happening during this session's own test traffic). It has not been shown to cause a visible defect (the conflict is caught and logged, not surfaced), but it is unnecessary work and log noise on every single message, and it's adjacent enough to the duplicate-UUID family of bugs ([[herald-duplicate-sessions-uuid-pair]] in project memory) that it deserves a closer look independently of the double-message bug in this document.
