---
name: kallisti-ios-senses
description: Read the phone's senses - location, health, activity - through the four sensor tools, with the sensitivity level applied. Use when a question is about where he is, how he slept, what his body is doing, or what the phone can see.
version: 1.0.0
author: Kallisti
license: MIT
platforms: [linux, macos]
metadata:
  kallisti:
    tags: [ios, sensors, location, health, activity, context]
    related_skills: [kallisti-ios, apple-reminders]
---

# The phone's senses

The iPhone uploads samples to this daemon. These four tools read them back with the
sensitivity level already applied, which is the whole reason to prefer them over
going at the database yourself: a key the operator left at `off` comes back keyed
with a null value, so you can say "he records this and will not share it" instead
of asking again or inventing one.

## The four tools

| tool | answers | when |
|---|---|---|
| `where_is_the_operator` | the last fix, with accuracy and address | "am I near a store", "how far out is he" |
| `health_today` | today's rollups, one row per metric | "how's my step count", "did I sleep" |
| `recent_activity` | the newest coarse activities, newest first | "was he driving", "is he up" |
| `query_device_db` | one kind's stored rows, bounded, newest first | any specific kind below |

`query_device_db` takes `kind`, an optional `limit` (max 100) and an optional
`since` as an ISO 8601 bound. The kinds are a closed set:

`location`, `health`, `motion`, `notification`, `alarm`, `focus`, `shortcut`,
`calendar`, `reminder`, `contact`

Anything outside that list is refused and the refusal repeats the set. Do not guess
a kind.

## Reading the answer

- Every reply carries `level`. A `null` `value` with a level of `off` is a fact:
  report it as "recorded, not shared", never as "no data".
- `level` of `ask` means it is being sent on the operator's standing answer. Do not
  re-ask on every turn.
- An empty list or a null `place` is genuinely nothing reported yet, usually a fresh
  install or a phone that has been asleep. Say that, and carry on with what else
  you know rather than stalling.
- `recorded_at` is UTC. Convert before saying a time out loud.

## What each one is good for

**Where.** Coarse is fine. "You're about fifteen minutes from the office" beats six
decimal places. If `accuracy` is large the fix is loose; say so rather than
pretending to precision the phone did not have.

**Health.** Steps, distance, heart rate and sleep are the ones that show up. One
metric per question reads better than a table. `health_today` is today only - for a
week of something, use `query_device_db` with a `since`.

**Activity.** Newest first, and only the coarse kind. It tells you whether he is
moving, driving or still; it does not tell you what he is doing.

**The rest.** Notifications, alarms and focus say what the phone is doing. Calendar,
reminder and contact rows arrive as keyed values rather than a parsed event, so
read the key and the value together before drawing a conclusion.

## Pitfalls

- **Do not read `sensors.db` directly when a tool will do it.** The tools apply the
  level mapping; raw SQL does not, and you will leak an `off` value.
- **`motion` is the activity kind**, not `activity`. `recent_activity` is a shortcut
  over `motion` and is the one to call for movement questions.
- **Nothing here is live.** A reading is as fresh as its timestamp. Do not claim
  current position from a fix an hour old.
- **Retention is short by design** - location ninety days, health a year. Asking for
  a fix from last year is a wasted round.
- **The phone owns the switch.** Widening a level means the operator answering the
  prompt on the phone; you cannot widen it from here.
