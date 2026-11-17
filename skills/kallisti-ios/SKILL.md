---
name: kallisti-ios
description: Work with the Kallisti iOS app - deliver files inline with attach_file, and read iPhone sensor data (location, health, activity) from the daemon sensors.db.
version: 3.1.0
author: Kallisti
license: MIT
platforms: [linux, macos]
metadata:
  kallisti:
    tags: [ios, mobile, attachments, location, health, sensors, context]
    related_skills: [apple-notes, apple-reminders]
---

# Kallisti iOS

The iOS client is Kallisti-iOS (Swift 6 / SwiftUI, iPhone and iPad). It talks to the
Eris daemon's device API directly: no relay, no connector. This skill covers the two
things that matter from the daemon side: **getting a produced file into the chat bubble**,
and **reading the sensor samples the phone has uploaded**.

This file replaces the connector-era version (2.0.0). Two of its mechanisms no longer
exist: the `MEDIA:` tag and the eight `get_user_*` / `query_sensor_data` tools. Both were
retired when the daemon absorbed the connector. Use `attach_file` and `sqlite3` instead.

## Delivering a file inline

**Never paste a remote URL and call it delivered.** A link is not an attachment. The user
expects to see the image or open the PDF in the bubble.

Use the `attach_file` tool. It copies a workspace file into the conversation's uploads,
records the attachment row, and binds it to the answer row this turn is writing.

### Rules

1. **The file must exist before you call the tool.** Tag before write delivers nothing.
2. **The path must resolve inside the workspace.** `attach_file` cannot reach
   `~/.ssh/id_ed25519` or any absolute path outside the workspace root. Write there first.
3. **Caps are the transcript's, not the tool's.** A reply cannot carry more files or more
   bytes than an operator question may, so a large batch will be refused.
4. **Verify, then report.** The tool result names the file it bound. If it errored, say so
   plainly instead of writing "attached" in prose.

### Supported types

Images render inline (png, jpg, gif, webp). PDF, DOCX, XLSX, PPTX, CSV, TXT render as a
downloadable card. Anything else is better linked from a note than attached.

### Pattern: a file you generated

```
write_file path=reports/weekly.pdf content=<generated>
attach_file path=reports/weekly.pdf
```

Then one line of prose about what it contains. Not a second copy of the contents.

## Sensor data

The phone uploads samples to the daemon; the daemon stores them. There are **no agent
tools** for reading them. Read the database directly, read-only.

Database: `~/.local/share/kallisti/sensors.db` (WAL).

```
sqlite3 'file:$HOME/.local/share/kallisti/sensors.db?mode=ro'
```

### Tables

| table | holds |
|---|---|
| `location_current` | latest fix, one row |
| `location_history` | one row per fix |
| `health_samples` | every uploaded HealthKit sample |
| `health_latest` | latest value per metric |
| `health_daily` | created for schema compatibility, never aggregated |

`health_samples` and `health_latest` carry `metric`, `value`, `unit`, `start_at`,
`end_at`, `source`. `location_current` and `location_history` carry `latitude`,
`longitude`, `altitude`, `horizontal_accuracy`, `speed`, `course`, `timestamp`.

### Patterns

```sql
-- current location
select latitude, longitude, horizontal_accuracy, timestamp
from location_current order by timestamp desc limit 1;

-- today's trail (compute the ISO timestamp, never hardcode a date)
select latitude, longitude, timestamp from location_history
where timestamp >= strftime('%Y-%m-%dT00:00:00Z','now')
order by timestamp;

-- every metric with its latest value
select metric, value, unit, start_at from health_latest order by metric;

-- one metric, 7 days
select value, unit, start_at from health_samples
where metric = 'steps' and start_at >= strftime('%Y-%m-%dT00:00:00Z','now','-7 days')
order by start_at;
```

### Freshness

A sample is only as good as its timestamp. Compare `start_at` to now before answering.

- location older than 10 minutes: say when the last fix was, do not present it as "now"
- health older than 1 hour: report it as a last reading
- activity older than 15 minutes: do not describe it as what they are doing

### Activity codes

`user_activity` stores `value` as `activity_code` with `unit = activity_code`. Map it
before speaking: 0 stationary, 1 walking, 2 running, 3 automotive, 4 cycling, 5 unknown.
Anything else: say the code, do not invent a meaning.

### Missing metrics

Some metrics require an Apple Watch. Absent is not broken. Report what exists and name
what is not being collected, once, without a lecture.

## Pitfalls

- **A URL is not an attachment.** Download or generate, then attach.
- **The old staging root is gone.** The previous install's staging root under the home
  directory (`attachment_staging/` inside its own hidden state folder) is a leftover
  tree. Do not write there for delivery; `attach_file` owns the uploads path.
- **The sensor tools in version 2.0.0 do not exist.** `get_user_location`,
  `get_location_history`, `get_health_summary`, `get_health_metric`,
  `get_health_metrics_list`, `get_user_activity`, `get_sensor_schema`, `query_sensor_data`
  are not registered in this daemon. Calling them wastes a round.
- **Read-only, and keep it that way.** Always open with `?mode=ro`. Never write to
  `sensors.db`; the phone owns it.
- **Empty tables after a fresh install.** Say the data is not there yet.
- **Do not aggregate `health_daily`.** It is empty by design. Use `health_samples`.
