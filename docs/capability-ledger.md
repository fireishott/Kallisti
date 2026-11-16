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
| 06 | `turn.approval` as a stream frame | partial | emitted when a call parks, payload tested; LIVE PROOF BLOCKED: this operator runs sandbox=full and approval=never, so nothing parks. Needs a turn under approval=ask to observe |
| 04 | Job lease, heartbeat, reclaim | partial | lease + heartbeat carried; `lease_expired_jobs` names the dead; the sweep settles a dead worker's turn as `lease_expired` and a restart's as `interrupted` (proved by `the_boot_sweep_tells_a_dead_worker_from_a_restart`). Reclaim is a requeue, not a resume: `requeue_reclaimed` puts the dead turn's request back as a task bounded by `chain_depth`, and `a_dead_turn_is_requeued_and_a_capped_one_is_not` proves one requeue and none at the cap |
| 04 | `job.cancel` at a step boundary | carried | `POST /v1/turns/{id}/cancel`; proved by `a_cancelled_turn_takes_no_further_steps` (a cancelled turn answers None to the next step, result or delta) |
| 07 | Per-device hashed tokens | carried | `auth_tokens` (Argon2id verifiers), `devices` |
| 07 | Token contrast test on `(host, profile)` | carried | migration 0037 records `auth_tokens.minted_for` = `/etc/machine-id` at mint; verify refuses a row bound to another instance with `token_from_another_daemon`; NULL rows predate the binding and keep working (tested) |
| 08 | Single-use pair code | carried | redemption marks `pairing_codes.consumed_at` inside the same statement that reads it, so two racing redeems cannot both win; `auth::pairing::code_single_use` |
| 08 | QR handoff | carried | `web/qr.rs` renders the payload as SVG with a quiet zone; the dashboard pairing page shows it (`routes.rs` Pairing::new + svg); 4 tests incl. a too-long payload returning none rather than a broken image |
| 07 | Rotation retires the old token once | carried | `TokenService::rotate` retires the presented refresh row before minting; `rotation_is_recorded` asserts exactly one row retired and an `auth.rotate` audit row |
| 07 | New token valid only after the first acked frame | absent | the minted pair is valid immediately; nothing ties it to an ack |
| 12 | FTS transcript search | carried | `chat_messages_fts`; `GET /v1/search`; `search_transcripts` tool offered to the model when the turn has a database (query, limit; replies with total and hits: conversation, title, snippet, ts, role) |
| 09 | Sensor storage + retention | carried | `sensors.db` with location/health projections; `device_events` table, the closed kind set and `parse_batch`/`record_batch` now exist (validate-then-write, one transaction, 3 tests). `POST /v1/device/sensors` is live: 3-row batch accepted with oldest/newest echoed, unknown kind refused 400 by row index, rows read back from `sensors.db` |
| 09 | Batch `/v1/device/sensors`, `/v1/device/events` | carried | `/v1/device/sensors` with the closed kind set, validate-then-write, one transaction; live exchange recorded above. `/v1/device/events` is not a separate route: every kind travels the same batch |
| 10 | Sensitivity closed set + inference | carried | three levels (always/ask/off), defaults health/location always, calendar/contacts ask, photos off, unknown ask; `sensitivity_map` cache (migration 0038); values stripped before the write; `POST /v1/device/sensitivity` takes one flip or an array. Live: updated 1 / updated 2 / `sometimes` refused 400 naming entry 0, rows read back from the table. 6 tests |
| 11 | `where_is_the_operator`, `health_today`, `recent_activity`, `query_device_db` | carried | `sensor_tools.rs`, offered only when this process opened the sensor store, each reading through the sensitivity mapping so a key at `off` returns its name and a null value. Live: the model called query_device_db itself and got rows carrying their level. 4 tests |
| 13 | Realtime envelope | partial | `POST /v1/rpc` with `{method, params}`, context from the bearer (admin = operator), one success/refusal shape. Live: ping, `models.list` 403 for a phone, unknown method 404 with the legal set, `turn.cancel` on a finished turn, `turn.state` answered succeeded, and `turn.create` created a turn that reached `done` through the shim. The phone route was re-proved after the extraction: a turn created on `POST /v1/turns` succeeded with its delta intact |
| 14 | Delivery journal, device fan-out | partial | `delivery_journal` (migration 0039) keys every attempt by `run:channel`; `scheduler::record_delivery` writes it, so both paths report to one place and a duplicate is logged. Live schema 39, table present. Channels (none/push/email/both) and the one-recipient email guard already existed. Open: the APNs wake over the relay, and the journal is not yet read by any dashboard view |
| 15 | `schedule_run` / `schedule_list` / `schedule_cancel` | carried | `scheduler_tools.rs`, offered when the turn has a database. Live: the model called schedule_run and schedule_list itself and a `harvest-proof` row (30 6 * * 1-5, push) appeared in `schedules`; removed after the proof. A one-shot `at` is refused with the reason. 3 tests |
| 16 | Comms tools with allowlist and confirm | absent | - |
| 17 | Bots, rooms, mentions, pills | partial | migration 0041 adds `bot_profiles`, `rooms`, `room_members`; `rooms.rs` holds the routing rule and the pills. A mention beats the policy, `mention` policy answers nobody unnamed, `lead` with a non-member lead answers nobody, and an empty `@` falls through to the policy. `create_bot` and `create_room` exist with three refusals (unknown bot, empty room, lead not a member). Migration 0042 gives each room a conversation, so its history is the existing message table rather than a second store, and `history()` reads it (tested). Five tools (`bot_create`, `bot_list`, `room_create`, `room_list`, `room_history`) wired; live: the model created a bot and a room, `harvest-room|mention|1` appeared in `rooms`, then removed. `POST /v1/rooms/{id}/say` writes the operator's message once and starts one turn per routed bot. Live: a mention room with one bot answered 202 with job 01a1e2d7, the job reached succeeded, and the room conversation holds the operator row, the bot's composed prompt and its answer. The turn limit refuses with room_turn_limit once max_turns is spent. Known cost: a bot's composed prompt is written as a user row, so the history shows who was asked what |
| 18 | Live voice tiers | partial | Tier 1 is the phone's own speech-to-text and speech and exists (Talk capture + TTS in the app). Tier 2 needs a host transcription and speech engine and this host has neither: no whisper, no piper, no `say`. Tier 3 additionally needs the provider decision the brief lists as the operator's. Recorded as blocked on a decision rather than guessed |
| 19 | Client shell (diffs, widgets, share sheet) | partial | chat, sessions, inbox, notes, sensors exist in the app |
| 20 | MCP and skills hooks | carried | `crates/eris-host/src/mcp`, skills library |
| 21 | Diagnostics ring buffer | partial | `diagnostics.rs`: bounded 200-entry buffer in-process, `record_turn` writes only a turn past the 30s watermark with its duration, `GET /v1/diagnostics` serves the bundle for an operator token (x-diagnostics-limit capped at 200). Live: a phone token is refused 403 scope_denied, which is the operator gate working. The turn writer feeds it from the done path |
| 22 | Rotation and hardening | partial | token rotation exists; pairing/tunnel guidance not written |
| 23 | Local Live experiment flag | absent | - |

## The smallest first deploy

`turn.start`, `resume.ack` and a `turn.end` status are the three additions that
change behaviour without changing a single existing frame's meaning, so the
shipped client keeps working while the contract is satisfied. Diff and page
artifacts follow in WP-05, behind the same additive rule.
