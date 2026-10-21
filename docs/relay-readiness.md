# Relay readiness

What a paired device can reach on `/v1` today, graded against what the daemon
actually does elsewhere. This is a description of the code as it stands, not a
plan. Every route in the first section exists. Every route in the third section
does not, and is written as a proposal with a method and a path so the shape is
arguable rather than vague.

Sources read: `crates/eris-host/src/device/mod.rs`,
`crates/eris-host/src/device/handlers.rs`,
`crates/eris-host/src/device/turns.rs`, `crates/eris-host/src/auth/scope.rs`,
the admin router in `crates/eris-host/src/web/routes.rs`,
`crates/eris-host/src/web/chat.rs`, `crates/eris-host/src/mcp/mod.rs`,
`crates/eris-host/src/approvals.rs`, `crates/eris-host/src/projects.rs`, and the
CLI in `crates/eris-host/src/main.rs`.

## 0. The surface as shipped (build 78)

This section supersedes the counts below, which were written when `/v1` was six
routes and a device could not administer anything. It is the authoritative list;
the grading in section 2 is kept because the reasoning about each gap is still the
right reasoning, but its totals are stale.

Twenty-two routes. Every one requires a bearer token unless it is the two whose
credential is in the body. The scope column is checked before the handler runs.

| Route | Method | Scope | Answers |
|---|---|---|---|
| `/v1/pair` | POST | pairing code in the body | 201 with tokens, device id and the granted scopes |
| `/v1/token/refresh` | POST | refresh token in the body | 200 with a new access token, scope set carried forward |
| `/v1/turns` | POST | `chat` | 202 with `job_id`, `session_id`, `state`, and `conversation_id` when one was named |
| `/v1/turns/{id}/events` | GET | `sessions` | `text/event-stream`: `delta`, `reasoning`, `tool`, `done`, `error`, `cancelled`, `subagent` |
| `/v1/turns/{id}/cancel` | POST | `chat` | 200 cancelled; a settled turn is 409 |
| `/v1/sessions/{id}` | GET | `sessions` | the session state and its turns |
| `/v1/conversations` | GET | `sessions` | newest first, `count`, `truncated`, per-row message count and project |
| `/v1/conversations` | POST | `sessions` | 201 with the new id and title |
| `/v1/conversations/{id}` | GET | `sessions` | the transcript with reasoning and the same tool records the browser stores |
| `/v1/conversations/{id}/clear` | POST | `sessions` | 200 with the number cleared |
| `/v1/projects` | GET | `sessions` | the project list |
| `/v1/projects` | POST | `sessions` | 201, or a duplicate-name refusal by code |
| `/v1/projects/assign` | POST | `sessions` | 200; an empty project unassigns |
| `/v1/settings` | GET | `admin` | model, sandbox, memory, approval, tools, rounds, timezone - no credential, key, path or base URL |
| `/v1/settings` | POST | `admin` | any subset of model, sandbox, memory, approval; all-or-nothing, audit per change |
| `/v1/approvals` | GET | `admin` | parked calls newest first, cap 50, `truncated` |
| `/v1/approvals/{id}/decide` | POST | `admin` | settles the row AND runs the call; refusal leaves it pending with 422 |
| `/v1/mcp` | GET | `sessions` | servers with `healthy` true, false or null, plus `health_known` |

Scope changes since the grading below was written:

- `admin` is no longer loopback-only. It is granted to a device only when the
  operator ticks the administration box while minting that device's pairing code.
  The decision travels on the code, so it is made once, by a person, and the
  devices page states which kind each device is.
- A device token without `admin` is refused with a sentence that says the device
  was paired without administration and how to change that, not a bare 403.

Still missing, and the honest list: the device does not create or revoke other
devices, cannot read the audit log, cannot start a backup, cannot change MCP
servers or the persona, and cannot dispatch a subagent (a subagent has no page to
report to and no parent answer row to count against). Those are in section 3.

## 1. The surface that existed when this was graded

Six routes. `/v1` only: anything else on the listener answers a typed 404, and a
known path with the wrong method answers a typed 405.

| Route | Method | Credential | Answers |
|---|---|---|---|
| `/v1/pair` | POST | a single-use pairing code in the body | 201, with `device_id`, `access_token`, `refresh_token`, `scopes`, `expires_at` |
| `/v1/token/refresh` | POST | a refresh token in the body | 200, with a new `access_token` and `expires_at` |
| `/v1/turns` | POST | bearer, scope `chat`, onboarding finished, listener accepting | 202, with `job_id`, `session_id`, `state`; honours an `Idempotency-Key` header |
| `/v1/turns/{id}/events` | GET | bearer, scope `sessions`, job owned by the caller's device | `text/event-stream`; `Last-Event-ID` resumes, a resume past the buffer is 409 `buffer_miss` with a `snapshot_url` |
| `/v1/turns/{id}/cancel` | POST | bearer, scope `chat`, job owned by the caller's device | 200 `cancelled`; a settled turn is 409 `turn_not_cancellable` |
| `/v1/sessions/{id}` | GET | bearer, scope `sessions`, session owned by the caller's device | the session's state and one row per turn: `job_id`, `state`, `text`, `created_at`, `finished_at` |

Four scopes exist and the set is closed: `chat`, `sessions`, `files`, `admin`. A
paired device is minted `chat sessions files` and never `admin`. The pairing
response states the set, so a client can read what it got.

The stream carries four frame kinds and no more: `delta` (one fragment of answer
text), `done` (the assembled text and the usage), `error` (a code and a message),
`cancelled`. An idle stream sends a `keep-alive` comment every 15 seconds.

## 2. Capability grade

`exposed` means a paired device can do it over `/v1` today. `partial` means the
daemon has the fact but `/v1` exposes only a fraction of it. `missing` means
`/v1` has no route for it at all. The daemon column uses the same three words
about the browser and the CLI.

| Capability | Daemon (browser or CLI) | `/v1` |
|---|---|---|
| Send a text prompt and get a turn | yes, the composer | exposed, `POST /v1/turns` |
| Stream the answer as it arrives | yes, `delta` frames over the chat response stream | exposed, `GET /v1/turns/{id}/events` |
| Stream the model's reasoning | yes, `reasoning` frames, gated by the `show_reasoning` setting | missing |
| Run tools during a turn | yes, the tool loop with a round budget | missing, the device worker calls the provider once and never enters the loop |
| See tool calls and their output | yes, `tool` and `tool_output` frames, then a stored row per call | missing |
| Read the run log afterwards | yes, per answer in the transcript and per job on `/dashboard/sessions/{id}` | partial, a device reads its own stream and no tool rows are written for its turns |
| Choose the model for one turn | yes, the composer posts `model` | missing |
| Choose the sandbox mode for one turn | yes, the composer posts `mode` | missing |
| Stop a running turn | yes, the stop button and `POST /dashboard/jobs/{id}/cancel` | exposed, `POST /v1/turns/{id}/cancel` |
| Start a new conversation | yes, `POST /dashboard/chat/new` | missing |
| Clear a conversation | yes, `POST /dashboard/chat/clear` | missing |
| Attach files to a turn | yes, multipart to `/dashboard/chat/send`, queued chips, `GET /dashboard/chat/file/{id}` | missing |
| Read conversation history | yes, the rail, `/dashboard/chat/sessions.json`, `/dashboard/sessions` | missing, one session by id only |
| Slash commands (`/help` `/new` `/clear` `/stop` `/model` `/memory`) | yes, parsed in `chat_send` before a turn exists | missing, a device that sends `/stop` spends tokens |
| List the approvals queue | partial, a count on `/dashboard` and rows from `eris approvals list` | missing |
| Decide an approval | yes, `eris approvals approve` and `eris approvals deny` | missing |
| Read settings | yes, `/dashboard/settings` | missing |
| Write the provider and the model | yes, `POST /dashboard/settings/provider` | missing |
| Write the sandbox mode and the approval policy | yes, `POST /dashboard/settings/permissions` | missing |
| Write tools on and off, and the round count | yes, `POST /dashboard/settings/tools` | missing |
| Write the aux role table | yes, `POST /dashboard/settings/aux` | missing |
| Write the reasoning and tool-log display choice | yes, `POST /dashboard/settings/reasoning` | missing |
| Write the persona | yes, `POST /dashboard/settings/persona` | missing |
| Write the memory backend and its vault path | yes, `POST /dashboard/settings/memory` | missing |
| Write the timezone | yes, `POST /dashboard/settings/timezone` | missing |
| Read and write the MCP server list | yes, `POST /dashboard/settings/mcp` | missing |
| Read MCP health | partial, the last start or call failure per server on the settings page, a failing count on `/dashboard` | missing |
| Write the provider registry and its primary | yes, `POST /dashboard/settings/providers` | missing |
| List and create projects, assign a conversation | yes, `POST /dashboard/projects` and `POST /dashboard/projects/assign` | missing |
| Redeem a pairing code | yes | exposed, `POST /v1/pair` |
| Refresh an access token | yes | exposed, `POST /v1/token/refresh` |
| List paired devices | yes, `GET /dashboard/devices` | missing |
| Mint a pairing code | yes, `POST /dashboard/devices` | missing |
| Revoke a device | yes, `POST /dashboard/device/revoke` | missing |
| List, create and restore backups | yes, `/dashboard/backups`, `POST /dashboard/backups/create`, `POST /dashboard/backups/restore`, plus `eris backup` and `eris restore` | missing |
| Read the audit log | yes, `GET /dashboard/activity` | missing |

Totals at the time of grading: 5 of 36 capabilities exposed, 1 partial, 30 missing.
See section 0 for what is exposed now.

Two of the missing rows have a `partial` daemon column, because the daemon holds
the fact and only shows a piece of it: the approvals queue is a count, and MCP
health is a failure list. The `/v1` verdict for both is still `missing`.

## 3. What each missing one would be

Every line is a proposal. None of it is implemented. "Scope" names a scope that
would have to be checked on the route, and "audit" names the row that would have
to be written with the device id as the actor, where every admin write today
records the actor as `loopback`.

- **Reasoning on the stream.** No new endpoint. The existing stream gains a
  `reasoning` frame and the worker stops discarding `ProviderEvent::Reasoning`.
  Scope: `chat`, already checked on `/v1/turns`. Audit: none, it is read traffic.
  Gated by the stored `show_reasoning` setting so a device can be told no.
- **Tool calls on the stream.** No new endpoint. The stream gains `tool` and
  `tool_output` frames. Scope: `chat`. Audit: none. This is the larger half of the
  gap, because the device worker would also have to run the tool loop, and then
  answer the approval gate, or a parked write waits forever.
- **Settings read.** `GET /v1/settings`. Scope: a fifth scope, `settings`; the set
  is closed at four and `admin` is never granted to a device. Audit: none.
- **Provider and model write.** `PATCH /v1/settings/provider`. Scope: `settings`.
  Audit: `settings.provider`. The same reference rule the dashboard enforces has
  to hold: a literal key is refused, `env:NAME` is the only accepted form.
- **Sandbox mode and approval policy.** `PATCH /v1/settings/permissions`. Scope:
  `settings`. Audit: `settings.permissions`. Must refuse `full` unless the process
  started with `--allow-full`, the same check the dashboard makes.
- **Per-turn mode and model.** No new endpoint. `POST /v1/turns` gains `mode` and
  `model`, validated exactly as `chat_send` validates them. Audit: `chat.mode`
  when a turn asks for something other than the stored default.
- **Tools, aux roles, reasoning, persona.** `PATCH /v1/settings/tools`,
  `PATCH /v1/settings/aux`, `PATCH /v1/settings/reasoning`,
  `PATCH /v1/settings/persona`. Scope: `settings`. Audit: `settings.tools`,
  `settings.aux`, `settings.reasoning`, `settings.persona`. The aux table is the
  largest body here: one row per role, each with an endpoint and a model.
- **Memory backend.** `PATCH /v1/settings/memory`. Scope: `settings`. Audit:
  `settings.memory`. The vault path is a filesystem path and needs the same
  validation the dashboard applies.
- **Timezone.** `PATCH /v1/settings/timezone`. Scope: `settings`. Audit:
  `settings.timezone`. The closed set is `host`, `UTC`, or an offset.
- **MCP list and health.** `GET /v1/mcp` and `PUT /v1/mcp` for the list, plus
  `POST /v1/mcp/{name}/probe`, which starts that child and returns its tool list
  or its failure. Scope: `settings` for the write, `sessions` for the read.
  Audit: `settings.mcp`, `mcp.probe`.
- **Projects.** `GET /v1/projects`, `POST /v1/projects`, and
  `POST /v1/projects/{id}/assign`. Scope: `sessions`, because a project is
  conversation state. Audit: `projects.created`, `projects.assigned`.
- **Slash commands.** No endpoint. Either `POST /v1/turns` parses them the way
  `chat_send` does, or the client does and the daemon stays a chat backend. Today
  a device that sends `/help` is billed for it.
- **Attachments.** `POST /v1/turns/{id}/attachments` as multipart, and
  `GET /v1/files/{id}` to read one back. Scope: `files`, the one scope every
  device already holds with nothing behind it. Audit: `turn.attachment`.
- **Conversation list, new, clear.** `GET /v1/sessions`, `POST /v1/sessions`,
  `POST /v1/sessions/{id}/clear`. Scope: `sessions`. Audit: `session.created`,
  `session.cleared`.
- **Devices.** `GET /v1/devices`, `POST /v1/devices/pairing-codes`,
  `POST /v1/devices/{id}/revoke`. Scope: `admin` today, which a device never
  holds, so this needs a scope decision before it is an endpoint at all. Audit:
  `device.pairing_code_issued`, `device.revoked`.
- **Approvals.** `GET /v1/approvals` to list the queue, `POST /v1/approvals/{id}`
  with `{"decision":"approved"}` or `{"decision":"denied"}` to settle one. Scope:
  `chat`. Audit: `approval.decided`. There is no such audit row today: the CLI
  decides and prints, and the row records `decided_by` but nothing else.
- **Backups.** `GET /v1/backups`, `POST /v1/backups`, `POST /v1/backups/restore`.
  Scope: `admin`, because a restore replaces state. Audit: `backup.created`,
  `backup.restored`. A restore is the one operation here that should probably stay
  off the relay surface on purpose.
- **Audit log.** `GET /v1/activity`. Scope: `admin`. Audit: none.

## 4. What a client can do today

- Pair with a single-use code and receive an access and refresh pair.
- Refresh the access token. The refresh token is verified and left alone rather
  than rotated, so a second refresh keeps working.
- Queue one text turn and get its `job_id` and `session_id` back before the
  provider is called.
- Replay and tail that turn's stream, resuming from the last event id it saw.
- Learn that it lost frames: a resume past the buffer is a 409 with the URL of
  the snapshot to read instead.
- Cancel a turn, and be told by code when there is nothing left to cancel.
- Read one session snapshot, by id, when it knows the id.
- Rely on the gates: onboarding finished, the operator switch on, a bearer that
  verifies, and a job or session that belongs to its own device. A foreign id is a
  404, never a 403 that confirms the id exists.

## 5. What it cannot do

- Use the agent. No tool loop, no tool rows, no reasoning, on any device turn.
- Choose anything per turn: not the model, not the sandbox mode, not the tools.
- Send a file, or read one back.
- See its own history: there is no list route, so an app that loses its session id
  after a restart cannot find the session it was in, and the snapshot URL in a
  `buffer_miss` points at an id the client already had.
- Use slash commands.
- Read or write any setting, including the ones that decide what a turn does.
- Answer an approval, or even see the queue as a list.
- Manage devices, mint codes, or revoke itself.
- Take, list or restore a backup, or read the audit log.

The one-sentence grade: a paired device is a chat client, not a remote control.
The relay carries text in and text out, with honest failure codes around it.

## 6. Corrections to claims elsewhere

- `README.md`, under "Relay ready", says "Turns stream events, so a phone sees the
  same run log a browser does." That is false. A browser gets `reasoning`, `tool`
  and `tool_output` frames; a device gets `delta`, `done`, `error` and
  `cancelled`, and its worker never runs a tool, so there is no run log to stream.
- `SettingsView` carries the doc comment "The sentence stating that no MCP client
  exists in this build", and `mcp/mod.rs` still has a paragraph opening "This
  build has no MCP client". Both are stale: `mcp::client` spawns one owned child
  per server, hands its tools to a turn as `server__tool`, and `mcp::STATUS` says
  so. The code is right and the comments are old.

Neither correction needs an endpoint, and neither is made here: this file is the
grade, and the two stale comments are noted for whoever touches them next.

## 7. Asymmetries found, not fixed

Two things look uneven. Neither is fixed, because each fix would decide a
security question that the code does not currently answer in a single way.

- An admin-scoped bearer is accepted on every `/v1` route, because `authenticate`
  only checks that the token verifies and never asks whether it carries `admin`.
  An admin-scoped refresh token is refused at `/v1/token/refresh` by name. So the
  same token class is refused on one route and accepted on five. An admin token
  can only be minted over loopback, so no paired device can reach this, and
  refusing it would change what a hand-minted loopback token can do.
- Every device is granted `files`, and no `/v1` route checks it. Nothing is
  exposed by it today. Removing it would change the pairing response and the
  stored spelling of every existing token, for no present gain.

No route is registered and unreachable, and no scope check is missing from a
route that needs one: `chat` guards the two writes (`POST /v1/turns`,
`POST /v1/turns/{id}/cancel`), `sessions` guards the two reads
(`GET /v1/turns/{id}/events`, `GET /v1/sessions/{id}`), and the two unauthenticated
routes are the two that cannot carry a bearer yet.

## 8. How this was checked

The `eris-host` acceptance suite already pins the behaviour this grade relies on:
`pair_redeems_a_code_and_refresh_mints_an_access_token`,
`a_turn_returns_202_then_streams_frames_with_monotonic_ids`,
`an_idempotency_key_replays_the_job_without_a_second_call`,
`last_event_id_resumes_with_only_the_missing_frames`,
`a_resume_past_the_retention_window_is_a_409_buffer_miss`,
`cancel_stops_a_running_turn_and_a_second_cancel_is_409`,
`the_admin_plane_is_404_on_the_device_listener`,
`the_not_ready_gate_and_the_missing_bearer_are_both_typed`, and
`a_foreign_job_is_404_and_the_port_is_loopback_only`.

The claim that a device turn carries no tools rests on
`crates/eris-host/src/device/turns.rs`, where the sink forwards `ProviderEvent::Text`
and nothing else, and on `FrameKind` having four variants. The claim that the
browser does carry them rests on the `reasoning`, `tool` and `tool_output` frames
that `crates/eris-host/src/web/templates.rs` handles.
