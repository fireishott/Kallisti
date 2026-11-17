# 0008 - Standing agents, webhook triggers, and a budget a turn can read

Status: accepted (1.7.1, build 186).

Three additions, each answering a question the daemon could not answer before. They
ship together because they share one property: each is a surface a turn reads or a
prompt supplies, and none of them adds a second runner.

## 1. A roster row is a saved brief, not a mode

A dispatch used to be a prompt: the operator re-explained the same specialist on
every call, and the child inherited whatever the parent happened to have.

A `roster` row stores a name, a role, a standing brief, a tool allowance, an optional
model role and an optional directory scope. `dispatch_agent` gains an `agent` field;
when it is named, the child carries the brief and the task, and the tool list is the
parent's minus dispatching itself.

Decisions inside that:

- **A name is canonical.** Lowercased, `[a-z0-9-]`, 1..32 characters, unique under
  `COLLATE NOCASE`. A rename is a new row, because the name is what a prompt says.
- **Bounds refuse; nothing truncates.** A silently shortened brief is a brief the
  operator did not write. That is the same rule the note files follow.
- **`read_only` is two refusals, not one.** The writing tools are absent from the
  child's list *and* the sandbox is read-only, so an invented call still fails.
- **A scope is prefix-checked after canonicalisation.** `..`, an absolute path and a
  symlink are all judged by where they resolve, and a scope outside the workspace
  refuses the dispatch rather than confining the child somewhere new.
- **`model_role` is stored, not resolved.** Storing one column now and wiring it
  later is cheaper than a review cycle; resolving it in this build would have meant
  touching the aux-role resolver, which is its own change.

## 2. A trigger is a saved prompt with a token, and a fire is a row before it is a turn

Nothing could start a turn because something happened; only a clock could.

`POST /hooks/{token}` accepts one request and starts one durable turn. The decisions:

- **The token is stored hashed.** `hex(sha256(token))` plus a twelve-character prefix.
  The plaintext appears in the response that minted it and nowhere else: not in a log,
  not in an audit row, not in a page after that render. A database leak is then a
  leak of verifiers, not credentials.
- **Every refusal is the same 404.** Unknown, revoked and disabled are indistinguishable
  on the wire. A distinguishable refusal is an oracle.
- **Constant-time comparison at the end.** The hash is the index key; the fold
  re-checks the row the index returned, so a collision cannot resolve to whichever row
  the planner read first.
- **A fire is written before the turn starts.** A process that dies mid-turn leaves a
  `queued` row. A fire that existed only in a variable is a request nobody can account
  for.
- **The rate limit counts every fire, refusals included.** A caller over the ceiling
  that keeps calling must not be able to reset its own counter by failing.
- **Admin listener only, POST only, 8 KiB body.** The token is the only credential on
  this route, and the phone's listener must not carry it.
- **One runner, extended.** The fire calls the same `run_one_turn` the schedule ticker
  calls. A second runner loop is how a job gets two owners, and it was refused here
  deliberately rather than deferred.

`eris trigger list` reports; it does not mint. A token minted from a shell is a token
in a terminal history, which is the one thing the storage rule exists to prevent.

## 3. The budget is a report, never a refusal

The numbers already existed: `metrics::summary_in` aggregates them and the settings
panel renders them. A turn could not ask.

`budget` is a tool in every sandbox mode, because reading a number changes nothing.
It answers from the same function the panel calls, so the figure a turn acts on is the
figure an operator can check.

- **The cap is the stored setting**, `quota.soft_cap_tokens`, validated 1,000 to
  10,000,000. A hand-edited value outside the range reads as unset rather than as a
  ceiling nobody chose.
- **Nothing is cut off at the cap.** The tool reports `cap_reached` and a verdict that
  says stop. The daemon does not truncate a running turn on a number it was told to
  report.
- **An unmeasured window reports no p95**, not zero. `0 ms` is a measurement; "nothing
  ran" is not one.

## Deliberately left out

Spoken replies (no provider client path exists here), per-agent model routing
(stored, resolved later), a second runner loop for triggers (refused), group or
multi-agent rooms (the roster plus dispatch covers one operator's fan-out), mobile
roster editing (the device surface gets a read-only route), conversation compaction
(it collides with the budget package), and cost dashboards beyond the report (a chart
is not a decision).
