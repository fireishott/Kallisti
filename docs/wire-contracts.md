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

### 2.1 What WP-06 still needs

`turn.ask` exists as the `asked` frame and it does block the turn, but three
contract requirements are not met:

1. **Field names.** The frame carries `{request, answer}` rather than the flat
   `ask_id`, `question`, `choices`, `allow_text`, `deadline`.
2. **Deadline.** Nothing expires an unanswered question; a turn waits forever.
3. **One open ask per turn.** Two `clarify` calls in one turn park two
   questions, and a client that renders one card has no rule for the second.

`turn.approval` does not exist as a frame at all: approvals are read from
`GET /v1/approvals` and settled by `POST /v1/approvals/{id}/decide`, so a parked
command is invisible to a stream that is already open. That is the next piece of
WP-06 and it is why the package is `partial`.

### 2.2 Rotation: the ack gate, and why it is not enforced

The contract says a rotated token is valid only after the first acked frame. This
daemon does not enforce that, and the reason is a failure mode rather than an
omission.

Today a refresh retires the presented row before minting a new pair, so the old
token cannot be replayed and the new one is live immediately. Making the new one
live only on an ack adds a state to every bearer: *minted but not yet usable*.
The phone would have to present a token it cannot yet use, receive a frame,
acknowledge it, and only then be authenticated - or wait for a grace period. If
the ack is lost (app backgrounded, connection dropped at the wrong moment, a
phone that crashes mid-rotate), the phone has a token it cannot use and no way to
get another without pairing again.

The threat the gate would close is narrow: an attacker who observes the rotation
response in the same instant, on the same connection, and races the phone to
acknowledge first. The controls that do close real threats are already in place -
retire-before-mint, single-use pairing codes, and the machine-id contrast test
that refuses a token minted by another install.

So the rule is recorded as a deliberate deviation, with its alternative named. If
the operator wants it anyway, the shape that does not lock the phone out is a
short grace period (30 seconds) before the token becomes usable, not an
indefinite wait for an ack.

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

### 4.1 Reclaim, and why it is not wired yet

A reclaim is a second worker taking a turn whose first worker stopped renewing
its lease. It is the one line of WP-04 this tree does not carry, and the reason
is a design fact rather than an omission.

Turns here are request-driven: a turn runs on the worker that created it, and
when that worker dies there is nothing that picks work back up. Reclaiming the
row without that picker would move a job from `running` to `queued` and leave it
there, which is a worse lie than the lease expiry it replaces.

The manager loop that closes this has to do three things in one place:

1. claim from `queued` (one statement, the existing `claim`),
2. renew while it runs (the existing `renew`),
3. reclaim a lease-expired job only when `attempts` is under the cap, journaled
   as a reclaim with the attempt count it carries.

The executor it needs already has everything required to re-run a stored turn:
`jobs.request_text`, `conversation_id`, `device_id` and `chain_depth` are on the
row. The loop is not written, so the ledger says `partial` and this section says
so out loud rather than leaving the reader to discover it.

## 5. Auth

- One token per device, minted at pair, stored in the phone keychain and hashed in
  the daemon registry.
- Every inbound write carries it; reads on the same connection inherit it.
- The token is bound to `(host, profile)`. A token presented to a different pair is
  refused and forces a re-register.
- A reconnect may present the previous token once for rotation; the new token is
  valid only after the first acked frame.
