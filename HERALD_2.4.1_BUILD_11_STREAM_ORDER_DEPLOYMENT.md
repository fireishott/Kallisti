# Herald 2.4.1 (11) — stream-order deployment instructions

## Scope

This build fixes two presentation defects visible in the July 30 device
captures:

1. A completed response could be inserted above its own optimistic user prompt.
2. The thought-process bubble could jump above/below the response while tokens
   arrived.

The cause is local, not model behavior: `ChatStore` preserved local messages in
order but did not make each inserted local row available as the next anchor; the
answer therefore anchored to an older server row. The chat list also changed a
streaming row's SwiftUI ID on every delta, recreating the view at 60 fps.

## Task for Claude Code with Superpowers

```text
Use Superpowers. Work only in /Users/you/Herald. Read this file in
full. Preserve the existing 2.4.1(10) fixes in the working tree; do not reset,
stash, or replace unrelated changes. Verify the anchor-set and stable-row-ID
fixes, run every test gate below, then produce a signed 2.4.1 (11) archive only
if all gates pass. Do not deploy the connector for this iOS-only layout fix
unless the recorded source SHA also contains connector changes from build 10.
Never print credentials, tokens, device identifiers, or conversation content.
```

## Required source review

Confirm all three conditions before building:

- `ChatStore.mergeConversationMetadata` updates its anchor set after every
  local-only insertion. The expected transcript is historical rows → optimistic
  user prompt → streamed assistant reply.
- `ChatScreen.messageList` keys `MessageBubble` only by `message.id`; it must
  not use `streamingCompositeID`, content length, reasoning length, timestamp,
  or array index as the row identity.
- `B40ConversationMergeTests.streamedReplyStaysBelowItsOptimisticPrompt` is
  present and asserts exactly that order.

## Local gates

```sh
cd /Users/you/Herald
git status --short
git diff --check
xcodegen generate
xcodebuild test -project Herald.xcodeproj -scheme Herald \
  -destination 'platform=iOS Simulator,name=iPhone 16 Pro' \
  -only-testing:HeraldTests/B40ConversationMergeTests
xcodebuild -project Herald.xcodeproj -scheme Herald \
  -configuration Release -destination 'generic/platform=iOS' build
xcodebuild -showBuildSettings -project Herald.xcodeproj -scheme Herald | \
  grep -E 'MARKETING_VERSION|CURRENT_PROJECT_VERSION'
```

Expected build settings are `MARKETING_VERSION = 2.4.1` and
`CURRENT_PROJECT_VERSION = 11`. A passing simulator test is necessary but does
not prove scrolling correctness.

## Archive and install

Use the established team signing/export configuration:

```sh
xcodebuild archive -project Herald.xcodeproj -scheme Herald \
  -configuration Release -destination 'generic/platform=iOS' \
  -archivePath build/Herald-2.4.1-11.xcarchive
```

Install the archive on the same iPhone used for the screenshots. Preserve the
build-10 archive so rollback is one install operation.

## Physical-device acceptance

Use a new harmless marker and a response expected to emit reasoning and at
least one tool event. Capture a screen recording from Send through completion.

- The optimistic user bubble appears at the bottom immediately after Send.
- The active thinking/placeholder row remains directly below that bubble.
- During reasoning, tools, text deltas, and terminal reconciliation, neither
  row moves above the prompt or to the top of the transcript.
- On completion, the same assistant row changes from active thought process to
  its collapsed thought summary plus final response. It is not recreated above
  the user prompt.
- Background/foreground once mid-stream and allow one SSE reconnect. There is
  one prompt, one assistant response, and no visual bounce.
- Open an older chat, then return to the tested chat. Order remains unchanged
  after the history refresh.

Failure condition: any answer or thought bubble appears above its own user
prompt, jumps position during a delta, or duplicates. Stop rollout and attach
the screen recording plus only the conversation UUID, job UUID, and timestamps.

## Release record

Record source SHA, archive path/hash, version/build, simulator result, device
model/iOS version, marker timestamps, reconnect result, and rollback archive.
