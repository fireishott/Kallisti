# Capability ledger

One row per contract feature. Status is about the code in this tree, checked on
2026-11-16 against schema 35 and daemon 1.8.0 build 187.

`carried` means the behaviour exists under another name and satisfies the contract
once the wire vocabulary is mapped. `partial` means part of it exists. `absent`
means nothing in the tree does it. A row only moves when a test proves the move.

| WP | Feature | Status | Where it lives |
|---|---|---|---|
| 02 | Sequential frames per turn | carried | `job_events(seq)` unique per job; `device/turns.rs` writer |
| 02 | Ring buffer per turn | carried | `DEFAULT_SSE_BUFFER`, `DEFAULT_REASONING_BUFFER` |
| 02 | Resume from a position | carried | `Last-Event-ID` -> `resume_from` in `device/handlers.rs` |
| 02 | `buffer_miss` with snapshot fallback | carried | 409 + snapshot route, same handler |
| 02 | `resume.ack` / release of retained frames | carried | `POST /v1/turns/{id}/ack`; `jobs.acked_seq` (migration 0036); `TurnStore::acknowledge` |
| 03 | `turn.delta` | carried | frame `delta` |
| 03 | `turn.start` | carried | frame `turn.start` (model, profile) emitted before the provider call |
| 03 | `turn.step` / `turn.step.result` | carried | emitted beside `tool_start`/`tool`: step_type, step_id, label, started_at, status, summary, duration_ms. Proven live: job 01a1e281 wrote turn.start(1), tool_start(3), turn.step(4), tool(5), turn.step.result(6), done(9) |
| 03 | `turn.end` with status | carried | frame `done` carries `status: ok` beside text + usage |
| 05 | Diff artifact | partial | `crate::diff::file` builds the contract shape (path, added, removed, hunks with old/new starts and context/add/remove lines, 3 tests) and `write_file` attaches it to its result; the `turn.artifact.diff` frame is emitted beside the step result when a `write_file` result carries a diff |
| 05 | Page artifact | carried | an HTML `write_file` result carries `page` (html + height_cap 2000); the writer emits `turn.artifact.page`; tested by an_html_write_publishes_a_page_and_anything_else_does_not |
| 06 | `turn.ask` blocking the turn | partial | frame `asked` carries `{request, answer}` and the turn blocks; `turn.ask` now rides beside it: one frame per question with `ask_id`, `choices` as labels, `multi`, `allow_text`, and `deadline` null. The wait already has a deadline (`clarify::DEFAULT_TIMEOUT`, one hour) and the frame now states it; the one-open-ask rule is enforced at park time (`waiting_for`), refused with a tool result the model can act on; the refusal branch itself has no test yet |
| 06 | `turn.approval` as a stream frame | carried | emitted when a call parks (approval_id, scope, command, reason, created_at, deadline) on the device stream and the browser SSE; the route is unchanged |
| 04 | Job lease, heartbeat, reclaim | partial | lease + heartbeat carried; `lease_expired_jobs` names the dead; the sweep settles a dead worker's turn as `lease_expired` and a restart's as `interrupted` (proved by `the_boot_sweep_tells_a_dead_worker_from_a_restart`). Reclaim is a requeue, not a resume: `requeue_reclaimed` puts the dead turn's request back as a task bounded by `chain_depth`, and `a_dead_turn_is_requeued_and_a_capped_one_is_not` proves one requeue and none at the cap |
| 04 | `job.cancel` at a step boundary | carried | `POST /v1/turns/{id}/cancel`; proved by `a_cancelled_turn_takes_no_further_steps` (a cancelled turn answers None to the next step, result or delta) |
| 07 | Per-device hashed tokens | carried | `auth_tokens` (Argon2id verifiers), `devices` |
| 07 | Token contrast test on `(host, profile)` | partial | scopes are checked; host binding is not recorded |
| 08 | Single-use pair code, QR handoff | carried | `pairing_codes.consumed_at`, dashboard mint |
| 12 | FTS transcript search | carried | `chat_messages_fts`; `GET /v1/search` |
| 09 | Sensor storage + retention | partial | `sensors.db`; `POST /v1/device/sensor/{location,health}` |
| 09 | Batch `/v1/device/sensors`, `/v1/device/events` | absent | per-sample routes only |
| 10 | Sensitivity closed set + inference | absent | - |
| 11 | `device_location`, `device_health`, `device_activity`, `query_device_db` | absent | tool registry holds `fetch_url`, `list_dir`, `read_file`, `run_command`, `tool_index`, `write_file` |
| 14 | Delivery journal, device fan-out | partial | APNs registrations + `push.db`; email path not wired to a schedule |
| 15 | `schedule_run` / `schedule_list` / `schedule_cancel` | partial | `schedules`, `schedule_runs` tables and routes; no tool wrapper |
| 16 | Comms tools with allowlist and confirm | absent | - |
| 17 | Bots, rooms, mentions, pills | absent | - |
| 18 | Live voice tiers | partial | Talk capture + TTS on the phone; no tier 2 or 3 on the host |
| 19 | Client shell (diffs, widgets, share sheet) | partial | chat, sessions, inbox, notes, sensors exist in the app |
| 20 | MCP and skills hooks | carried | `crates/eris-host/src/mcp`, skills library |
| 21 | Diagnostics ring buffer | absent | - |
| 22 | Rotation and hardening | partial | token rotation exists; pairing/tunnel guidance not written |
| 23 | Local Live experiment flag | absent | - |

## The smallest first deploy

`turn.start`, `resume.ack` and a `turn.end` status are the three additions that
change behaviour without changing a single existing frame's meaning, so the
shipped client keeps working while the contract is satisfied. Diff and page
artifacts follow in WP-05, behind the same additive rule.
