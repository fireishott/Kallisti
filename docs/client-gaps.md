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
