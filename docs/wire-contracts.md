# Kallisti wire contracts

Frozen 2026-11-16. This file is the contract both ends build against. A change here
is a version bump and a migration of both sides, not an edit.

Names are Kallisti's own: `turn.*`, `ask`, `device_job`, `diff`. No borrowed
project names anywhere in this tree.

## 1. Stream, daemon to client

Every frame carries `turn_id` and `seq`. `seq` is monotonic per turn, starts at 1,
and never repeats or skips inside one turn. A client drops any frame at or below
its high-water mark for that turn.

| Frame | Payload | Notes |
|---|---|---|
| `turn.start` | `turn_id`, `seq`, `ts`, `model`, `profile` | First frame of a turn. Nothing precedes it. |
| `turn.step` | `turn_id`, `seq`, `step_type`, `label`, `started_at` | `step_type` in `thinking`, `tool_call`, `delegate`, `plan`, `note`. |
| `turn.step.result` | `turn_id`, `seq`, `step_id`, `status`, `summary`, `duration_ms` | `status` in `ok`, `error`, `timeout`, `cancelled`. |
| `turn.delta` | `turn_id`, `seq`, `text` | Markdown chunks, in order. |
| `turn.artifact.diff` | `turn_id`, `seq`, `files[]` | Per file `path`, `added`, `removed`, `hunks[]`; per hunk `old_start`, `old_lines`, `new_start`, `new_lines`, `lines[]`, each with `kind` in `context`, `add`, `remove`. |
| `turn.artifact.page` | `turn_id`, `seq`, `html`, `height_cap` | Rendered page; the phone caps height and opens full screen on tap. |
| `turn.ask` | `turn_id`, `seq`, `ask_id`, `question`, `choices[]`, `allow_text`, `deadline` | Blocks the turn until answered or the deadline passes. |
| `turn.approval` | `turn_id`, `seq`, `approval_id`, `command`, `scope`, `cwd`, `reason`, `deadline` | Pre-execution gate. The command has not run. |
| `turn.end` | `turn_id`, `seq`, `status`, `token_usage` | `status` in `ok`, `stopped`, `error`. Terminal; nothing follows it. |

Rules:

- `seq` is assigned by the writer at append time, not by the reader.
- Unknown frame types are ignored, never an error.
- A reconnect replays strictly after the last acknowledged `seq`.

## 2. Stream, client to daemon

| Frame | Payload |
|---|---|
| `hello` | `device_id`, `token`, `resume_from_seq`, `app_version`, `locale` |
| `resume.ack` | `device_id`, `turn_id`, `acked_seq` |
| `turn.steer` | `turn_id`, `text` |
| `turn.queue` | `turn_id`, `text` |
| `turn.stop` | `turn_id`, `reason` |
| `ask.answer` | `ask_id`, `choice_id`, `text` |
| `approval.answer` | `approval_id`, `decision` in `allow`, `allow_always`, `deny`, `note` |
| `job.cancel` | `job_id` |

`resume.ack` is what lets the daemon release retained frames. A client that never
acks keeps its frames until the turn is evicted, which is correct and expensive.

## 3. Device upload

| Route | Payload | Notes |
|---|---|---|
| `POST /v1/device/hello` | `device_id`, `token`, `platform`, `app_version` | Contrast-tested against `(host, profile)`. |
| `POST /v1/device/sensors` | batch of `kind`, `key`, `value`, `unit`, `sensitivity`, `ts` | Atomic: one bad row fails the batch. |
| `POST /v1/device/events` | batch of device events | Same atomicity. |

`kind` is a closed set: `location`, `health`, `motion`, `notification`, `alarm`,
`focus`, `shortcut`, `calendar`, `reminder`, `contact`.

## 4. Job state machine

`pending -> claimed -> running -> done`, plus `failed` and `expired`.

- A claim is a lease with a heartbeat, never a boolean flag.
- A lease past its heartbeat is reclaimable, and a re-claim is journaled as a
  reclaim, not a new job.
- `job.cancel` is honoured at the next step boundary.
- The job holds the turn open: the turn ends only on `done`, `failed`,
  `cancelled` or `expired`.

## 5. Auth

- One token per device, minted at pair, stored in the phone keychain and hashed in
  the daemon registry.
- Every inbound write carries it; reads on the same connection inherit it.
- The token is bound to `(host, profile)`. A token presented to a different pair is
  refused and forces a re-register.
- A reconnect may present the previous token once for rotation; the new token is
  valid only after the first acked frame.
