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
