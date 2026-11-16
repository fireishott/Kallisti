# What the phone can see, and what it currently ignores

Written after reading the shipped client's frame decoder on the MBP
(`~/kallisti-180-forward`, worktree at 1.6.0). This is the WP-19 gap list, stated
from the code rather than from the feature list.

## The decoder

`Herald/Eris/ErisTypes.swift`, `ErisFrame.decode(event:data:)`, switches on five
event names and returns `nil` for everything else:

```
case "delta", "reasoning", "done", "error", "cancelled"
```

`nil` means the frame is **skipped silently**. So every contract frame this
programme added is currently invisible to the phone:

| Frame | Daemon emits it | Phone decodes it |
|---|---|---|
| `turn.start` | since WP-03 | no |
| `turn.step` | WP-03, proven live | **no** |
| `turn.step.result` | WP-03, proven live | **no** |
| `turn.artifact.diff` | WP-05, proven live | **no** |
| `turn.artifact.page` | WP-05, proven live | **no** |
| `turn.ask` | WP-06 | **no** |
| `turn.approval` | WP-06 | **no** |

The older kinds (`delta`, `reasoning`, `done`, `error`, `cancelled`) are decoded
and are what the current UI renders.

## What this explains

- **The thinking bubble.** The daemon writes `turn.step` frames, and the phone
  never asks for them. The reasoning fragments do arrive (`reasoning` is decoded),
  so a turn with a model that emits reasoning shows text; a turn whose model does
  not has an empty Thought process row even though the daemon recorded six steps.
- **Diffs and pages.** WP-05 was proved on the wire (`turn.artifact.diff`,
  `turn.artifact.page` in `job_events`) and no surface draws them, because the
  client drops the frames before any view sees them.

## The work, in order

1. Decode `turn.step` and `turn.step.result` into an `ErisFrame.step` case, yield
   them from `ErisHeraldClient`, and render a step row in the thought process
   section. This is the smallest change that makes the bubble true.
2. Decode `turn.artifact.diff` and render the file summary the frame already
   carries (path, added, removed), with hunks behind a tap.
3. Decode `turn.artifact.page` and render the HTML in a capped WebView.
4. Decode `turn.ask` and `turn.approval` and give them cards with a deadline.

Each is additive: an unknown event still decodes to `nil`, so an old app against a
new daemon keeps working.

## The client is not in the client's git

`Herald/Eris/` is **untracked** in `~/kallisti-180-forward`. `git ls-files Herald/Eris`
returns nothing, and it is not ignored - the whole directory simply sits outside
version control while every build from 163 to 167 shipped from it.

That means the app's entire Eris integration - the client, the frame decoder, the
types, the pairing and control surfaces - exists only on one machine's disk. A
clone of that repository would not build the app that is on TestFlight.

**Fixed on 2026-11-16.** `git add Herald/Eris` and one commit, `33a309a0`: fourteen
files, 3696 lines - the client, the frame vocabulary including the `turn.step`
decode, the pairing surfaces, the token store. `git ls-files Herald/Eris` now
answers fourteen.

What is still uncommitted in that tree is someone else's in-flight work - the
project file, ChatScreen, the Notes screens - and it was left alone on purpose.

## Build 168

Uploaded 2026-11-16 00:50 (delivery 817b1054), 1.6.0 build 168, carrying the
`turn.step` decode: a step now lands in the Thought process row as an activity
line. Scripts `tf-168-oneshot.sh` / `release-168.sh` and the build number were
bumped in `project.yml` and in `Herald.xcodeproj/project.pbxproj` (xcodegen is not
installed on that Mac, so the pbxproj is the file that counts).

What to check on the phone: a turn whose model emits no reasoning should still
show step lines in the Thought process row. If it stays empty, the decode is not
the last link and the next place to look is the view that consumes
`toolActivity`.

## After build 168: the artifact decoders

`cdddc210` on the MBP decodes `turn.artifact.diff` and `turn.artifact.page`, which
WP-05 proved on the wire and no surface drew. Each lands as one run-log line: a
diff names its files with `+added -removed`, a page names its size and height cap.
Simulator build succeeded.

Deliberately not uploaded as build 169 yet: the frames carry hunks and html, and
shipping two TestFlight builds to watch a line of text is churn. The upload waits
for the rendering piece - hunks behind a tap, the page in a capped WebView - so
one build shows both.

## Build 169

Uploaded 2026-11-16 00:58 (delivery fd317422), 1.6.0 build 169. Carries, in one
install:

- `turn.step` in the Thought process row (also in 168),
- `turn.artifact.diff` rendered by the app's existing diff card, converted from the
  daemon hunks into the `CodeDiff` patch shape,
- `turn.artifact.page` rendered in a capped, scrollable `WKWebView` that loads only
  the html it is given.

What to check on the phone, in order:

1. A turn with tool calls shows step lines even when the model emits no reasoning.
2. A turn that writes a file shows a diff card with the file and its counts; tapping
   expands the hunks.
3. A turn that writes an .html file shows the page card, capped and scrollable, and
   the page cannot navigate anywhere.

If a card is missing, the decode is not the last link: check the store case for the
frame and the bubble branch that draws it.

## Approvals: the phone has no surface at all

Read on 2026-11-16 while building the decide card. `ErisApproval`,
`ErisApprovalPage`, `ErisApprovalDecision`, `ErisClient.approvals()` and
`ErisClient.decideApproval` all exist in `Herald/Eris`. Nothing calls any of them:
`grep -rn decideApproval Herald | grep -v ErisClient.swift` returns nothing, and
`MessageBubble` reaches only `TalkStore` and `SettingsStore`, not the Eris client.

So the approval card is not a card. It is a small feature:

1. A store that lists approvals (`clientForPairedDaemon().approvals()`), each
   refreshed when the daemon says one parked - `turn.approval` decodes to that
   event already, as of round 64.
2. A decide call that reports the outcome rather than assuming success:
   `turn_not_cancellable`, `approval_not_found` and a refused decision all have to
   read as themselves.
3. A surface: a sheet from the chat, or a card in the bubble, either way something
   that can be dismissed without deciding.

The transport exists (`ErisDaemonStore.clientForPairedDaemon()`), so this is
plumbing and a view, not a protocol.

Until it exists, `turn.approval` shows as a run-log line naming the tool and its
arguments. That is honest: it says what is waiting, and it does not offer a button
that cannot work.

## Build 170

Uploaded 2026-11-16 01:05, 1.6.0 build 170. Carries everything in 169 plus:

- `turn.ask` rendered by the clarify card the app already had,
- an approvals sheet reachable from the chat toolbar (checkmark.shield), which
  lists what the daemon is holding, decides with Allow or Deny, and keeps a row on
  screen with the daemon reason when a decision is refused.

What to check on the phone:

1. A turn that asks a question shows the clarify card with its choices.
2. The shield button opens Approvals; with nothing parked it says so.
3. A parked command shows its tool, arguments and advice; Allow runs it, Deny
   does not, and a refusal leaves the row visible with the reason.

With this build the app can do every interaction the daemon contract defines. What
is still missing is not a frame: it is the browser install path (WP-20), the local
engine flag (WP-23), and the WP-06 daemon-side proof that waits on a permission
change.

## Build 170 bug report: transcript loses history, repeats, and reorders

Reported by the operator on 2026-11-16 while using build 170. Five symptoms, and
the code says why.

### 1. Messages gone when you leave the chat and come back

`ErisHeraldClient.conversationIDByLocalID` is a **plain in-memory dictionary**.
The mapping from a local chat id to an Eris conversation id lives only in RAM, so
after a relaunch - or whenever the client object is recreated - the map is empty,
`loadConversation(id:)` hits its `guard let conversationID ... else` branch and
returns `Conversation(title: "New Chat")` with no messages at all.

The daemon has the history the whole time (`GET /v1/conversations/{id}`). The app
forgets which conversation to ask for.

### 2. The first reply disappears

`sendMessage` reads `conversationIDByLocalID[conversationID]` and passes whatever it
finds to `createTurn(text:conversation:)`. When the mapping is missing the turn goes
out with **no conversation**, so the daemon writes a job and an answer with no
transcript row: the reply arrives on the stream and exists nowhere afterwards. That
is a disappearing reply, and it is the same root cause as 1.

`ensureConversation(id:)` exists and does the right thing, but nothing guarantees it
ran before the first send.

### 3. The same message appears three times

The same local message is rendered repeatedly, which is what an optimistic insert
plus a stream echo plus a history merge looks like when no row carries a stable id
the three views can be matched on. The daemon already returns `id`, `job_id` and
`client_message_id`; the transcript merge has to key on them.

### 4. Messages out of order

Order is whatever the merge produced rather than `created_at` with a stable
tiebreak. Two messages in the same second need `id` as the second key.

### 5. No way to load older messages by scrolling up

The daemon's `GET /v1/conversations/{id}` returns a bounded window and **takes no
cursor**, so the app cannot ask for what came before. That needs a `before` query
parameter (and the reply should say whether more exists), then the app asks for the
next page when the transcript reaches the top.

### The order to fix them

1. Persist the conversation map (UserDefaults, keyed by local id, with the reverse
   map rebuilt on load). This alone fixes 1 and 2.
2. `sendMessage` awaits `ensureConversation(id:)` and fails loudly if it cannot, so
   a turn never goes out without a conversation.
3. Merge on `id`/`job_id`/`client_message_id`, sort by `(created_at, id)`.
4. Daemon: `before` cursor on the conversation route; app: page on scroll to top.

## Build 171: the transcript fixes, part one

Uploaded 2026-11-16 01:23. What changed, and what it addresses:

| Symptom | Fix | Where |
|---|---|---|
| Messages gone when you come back | the local-to-Eris conversation map is persisted in UserDefaults and rebuilt on launch | `ErisHeraldClient` |
| The first reply disappears | `sendMessage` binds a conversation before creating the turn and refuses to send without one | `ErisHeraldClient` |
| No way to load older messages | `GET /v1/conversations/{id}` now reads from the newest end and accepts `x-before: <message id>`, answering `has_more` | daemon, `chat::load_page` |

Still open, and next:

- **the same message rendered three times** - the transcript merge needs to key on
  `id` / `job_id` / `client_message_id`;
- **order** - sort by `(created_at, id)`;
- **the app does not use the cursor yet**: the daemon can page, the phone still asks
  for one page only.

Verified on the daemon: the newest page of a 26-message conversation came back as
26 rows, and `x-before` the oldest of those returned the 4 older messages.
