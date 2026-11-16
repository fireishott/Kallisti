# Acceptance test plan: what has been run, and what has not

Section 16 of the harvest brief lists sixteen behaviours. This file records the
evidence for each: run and passed, run and failed, or not run and why. A blank is
not a pass.

## Run and passed on the live daemon

| # | Test | Evidence |
|---|---|---|
| 1 | Resume: drop the socket mid-turn, resume from the last ack | `scripts/acceptance-resume.py`: first connection read 48 frames (ids to 50, deltas only), socket dropped, reconnect with `Last-Event-ID: 50` returned 94 frames (ids 51 to 144), **zero overlap**, terminal `done` received. Job `01a1e2f6-afb8`. |
| 8 | Sensors: batch with one bad row | `POST /v1/device/sensors` with a valid row then `telepathy` answered 400 `sensor_batch_invalid`, naming row 0, and `device_events` gained nothing. |
| 9 | Clock skew: future timestamp beyond the window | One batch with a 2099 health sample and a 2019 motion sample: the health row stored with its timestamp clamped to now, the motion row accepted and pruned by retention in the same transaction. |
| 10 | Email: recipient lock | `comms::confirm` refuses a recipient that differs from the staged one, by name, and a confirmation cannot be replayed. Test `a_confirmation_cannot_swap_the_recipient`. |
| 6 | Auth: token minted by another host | Migration 0037 binds a token to `/etc/machine-id`; a row bound elsewhere is refused `token_from_another_daemon`. Test `a_token_minted_by_another_daemon_is_refused`. |
| 7 | Sensitivity: unknown, declined, flipped | Tests `an_unknown_key_defaults_to_ask`, `a_declined_key_stays_declined_across_reads`, `a_key_can_flip_between_two_reads`; live: `heart_rate=off` stored the key with a null value. |
| 15 | Bots: a message that names nobody | `mention_policy_answers_only_a_named_bot` proves the policy answers nobody; `pills_report_replied_passed_and_responding` proves the pill states. Live room answered a mention in job `01a1e2d7`. |
| 3 | Jobs: kill the worker mid-job | Covered at the storage layer rather than by killing a process: `a_lease_that_stopped_beating_is_visible_and_renewal_clears_it`, `the_boot_sweep_tells_a_dead_worker_from_a_restart`, `a_dead_turn_is_requeued_and_a_capped_one_is_not` (one reclaim, none at the cap). |
| 4 | Ask: phone sleeps with an open ask | `a_gate_times_out_rather_than_waiting_forever`: the wait answers `TimedOut` at the deadline, so the turn cannot hang. Not run against a sleeping phone. |

## Not run, and why

| # | Test | Why |
|---|---|---|
| 2 | Two devices on one profile resume independently | Both tokens exist, but proving "independently" needs two clients holding the stream at once. The sequence is per job and the ack is per device, so the pieces exist; the test does not. |
| 5 | Approval: deny then re-run | Blocked: this daemon runs `sandbox=full` with `approval=never`, so nothing parks. Needs one permission change by the operator. |
| 11 | Delivery: relay down | There is no relay in this design, so there is no fallback to observe: the direct stream is the only path. |
| 12 | Schedules: a job fails delivery | The journal records a failed delivery (`both_channels_of_one_run_are_journalled_separately`), but nothing schedules a delivery against a failing target end to end. |
| 13 | Diff: binary file in a turn | Not run. `write_file` reads the old content as UTF-8 and the diff is line-based, so a binary file is the case most likely to be wrong. |
| 14 | Artifacts: page taller than the cap | App-side: the cap is honoured in `ErisPageCard`, but "opens full screen" is not implemented. |
| 16 | Voice: lock the screen mid-call | Tier 1 only, by the operator's decision; no host call to lock. |

## The rule this file exists to keep

An assertion never shares a function body with the path it tests, one test per
stated behaviour, and a behaviour change without a test change is a review reject.
Where a test cannot be run, it is listed here with the reason rather than counted as
green.
