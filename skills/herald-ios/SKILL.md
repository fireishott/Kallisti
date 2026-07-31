---
name: herald-ios
description: Work with the Herald iOS app — deliver images and files inline in chat, and read real-time iPhone sensor data (location, health, activity).
version: 2.0.0
author: Herald
license: MIT
platforms: [macos, linux]
metadata:
  hermes:
    tags: [ios, herald, mobile, media, attachments, location, health, sensors, context]
    related_skills: [hermes-mobile-chat, find-nearby]
---

# Herald iOS

Herald is the iOS client. This skill covers two things: **sending images and files back
to the user so they render inline in chat**, and **reading live sensor data** from the
user's iPhone.

Formerly `hermes-ios`. The app, the connector, and the transport are all Herald now;
"Hermes" refers only to the agent runtime.

## Delivering images and files inline — READ THIS FIRST

**Never paste a remote URL and call it delivered.** A `https://…` link renders as dead
blue text in Herald chat. It is not an attachment. If you generated an image, downloaded
a file, produced a chart, or rendered a PDF, the user expects to *see* it in the bubble.

### The contract

Herald inlines a file when your response contains a `MEDIA:` tag pointing at a **local
file that exists on this host**:

```
MEDIA: /absolute/path/to/file.png
```

The connector reads that file, base64-encodes it, attaches it to the message, and strips
the tag from the visible text before the user ever sees it. Enforced by
`_extract_media_from_response` in `connector/src/herald_connector/client.py`.

### Rules

1. **Absolute path, or `~/`-prefixed.** Relative paths are ignored silently.
2. **The file must already exist when you emit the tag.** Write it to disk first, then
   reference it. Emitting the tag for a path you are about to create delivers nothing.
3. **One tag per file.** Multiple tags in one response are all honoured.
4. **10 MB hard ceiling per file.** Larger files are skipped without an error. Downscale
   images or link to a LAN path instead, and say so.
5. **Put the tag on its own line**, separated by a blank line from prose.
6. **Do not describe the tag.** Never write "here is the MEDIA tag" — the tag is stripped
   and your sentence will read as a non sequitur.

### Supported types

| Category | Extensions |
|---|---|
| Images | `.png` `.jpg` `.jpeg` `.gif` `.webp` |
| Video | `.mp4` `.mov` `.avi` `.mkv` `.webm` |
| Audio | `.ogg` `.opus` `.mp3` `.wav` `.m4a` |

Anything else is delivered as a generic file attachment. Extension drives the MIME type —
a PNG named `.dat` will not be recognised as an image.

### Staging directory

Stage anything you generate or download here:

```
~/.hermes-mobile/attachment_staging/
```

That is the real path — `state.py` resolves the connector state dir to `~/.hermes-mobile`.
It is **not** under `~/.hermes/profiles/<profile>/`. Create the directory if it is missing.

### Pattern: an image from a remote generator

Any image tool that hands back a URL (xAI/Grok `files-cdn.x.ai`, OpenAI, Replicate,
a scraped asset) needs the same three steps. The URL alone is never the deliverable.

```bash
mkdir -p ~/.hermes-mobile/attachment_staging
OUT=~/.hermes-mobile/attachment_staging/$(date +%s)-doggo.png
curl -sSL --fail --max-time 60 -o "$OUT" "<the URL the tool returned>"
test -s "$OUT" && file "$OUT"
```

Then reply:

```
There you go — corporate doggo with the CEO energy.

MEDIA: ~/.hermes-mobile/attachment_staging/1753929600-doggo.png
```

Verify before you emit the tag: `curl` can write a 404 HTML body to the path and exit 0
without `--fail`. Check `file "$OUT"` reports actual image data, and that the size is
non-zero. If the download failed, say so plainly — do not fall back to pasting the URL.

### Pattern: a file you generated

```
Report's ready — 3 regressions, all in the merge path.

MEDIA: ~/.hermes-mobile/attachment_staging/regression-report.pdf
```

### Pattern: several files at once

```
Both charts:

MEDIA: ~/.hermes-mobile/attachment_staging/latency.png
MEDIA: ~/.hermes-mobile/attachment_staging/throughput.png
```

### Housekeeping

Staged files are cheap but not free. Prune anything older than a day when you notice it:

```bash
find ~/.hermes-mobile/attachment_staging -type f -mtime +1 -delete 2>/dev/null
```

### Receiving attachments

Users attach photos and files from the chat composer. Images arrive as vision context;
files are staged to disk and referenced by path. Acknowledge what you actually received —
name the file — rather than assuming.

## Sensor data

Real-time data from the user's iPhone via the Herald Mobile MCP server.

### When to use

- **Location** — "where am I?", "how far am I from…?"
- **Health / fitness** — "how many steps today?", "how did I sleep?", "what's my heart rate?"
- **Activity** — "am I walking?", "what am I doing?"
- **Location history** — "where have I been today?"
- **Health trends** — "steps this week", "sleep over the last 7 days"
- **Context adaptation** — shorter answers while walking, health nudges if sedentary

Do not use it for general knowledge questions, or anything that gains nothing from
location/health context.

### Tools

| Tool | Purpose | When |
|---|---|---|
| `get_user_location` | Current location with address | "Where am I?", nearby queries |
| `get_location_history` | Recent location trail | "Where have I been?" |
| `get_health_summary` | All latest health metrics | "How's my health?" |
| `get_health_metric` | Time-series for one metric | "Steps this week" |
| `get_health_metrics_list` | Available metrics + latest values | Discovering what exists |
| `get_user_activity` | Current physical activity | "Am I walking?" |
| `get_sensor_schema` | Database table structure | Before custom queries |
| `query_sensor_data` | Read-only SQL against the sensor DB | Correlations, trends |

### Patterns

```
# Current location
→ get_user_location

# Today's trail — compute the ISO timestamp, never hardcode a date
→ get_location_history  since=<today at 00:00:00Z>

# Recent locations
→ query_sensor_data  sql="SELECT address, recorded_at FROM location_history
                          WHERE recorded_at > datetime('now', '-6 hours')
                          ORDER BY recorded_at DESC"

# Quick health check
→ get_health_summary

# One metric with history
→ get_health_metric  metric="steps"

# Cross-metric correlation
→ query_sensor_data  sql="SELECT date(start_at) AS day, metric, SUM(value) AS total
                          FROM health_samples
                          WHERE metric IN ('steps','active_calories')
                            AND start_at > datetime('now','-7 days')
                          GROUP BY day, metric ORDER BY day"

# What's the user doing right now?
→ get_user_activity   # stationary | walking | running | automotive | cycling | unknown
```

### Health metrics

| Metric | Unit | Description |
|---|---|---|
| `steps` | count | Daily step count |
| `active_calories` | kcal | Calories burned from activity |
| `distance_walking` | meters | Walking + running distance |
| `heart_rate` | bpm | Most recent heart rate |
| `resting_heart_rate` | bpm | Resting heart rate (daily) |
| `blood_oxygen` | % | SpO2 percentage |
| `respiratory_rate` | breaths/min | Breathing rate |
| `body_mass` | kg | Body weight |
| `workout_minutes` | minutes | Active workout time |
| `stand_hours` | hours | Hours with standing activity |
| `sleep_duration` | hours | Total sleep (attributed to wake-up day) |

### Freshness

Every response carries `recordedAt`, `updatedAt`, `isFresh`, `ageSeconds`.

- Location older than 10 minutes — say it may not be current.
- Health metrics older than 1 hour — note the age.
- Activity older than 15 minutes — note it may have changed.

### Schema

`query_sensor_data` runs read-only SQL against:

| Table | Contents |
|---|---|
| `location_current` | Single row, latest location + address |
| `location_history` | Time-series of location updates |
| `health_samples` | Raw health samples with timestamps |
| `health_latest` | Most recent value per metric |
| `health_daily` | Daily aggregates |

Call `get_sensor_schema` for exact columns before writing a query.

### Context-aware responses

- **Walking / running** — brief, voice-friendly.
- **Driving** — audio-only, no links or code.
- **Stationary at home** — full detail, full formatting.
- **Late night + poor sleep** — consider suggesting rest.
- **Step milestone** — acknowledge it.

## Pitfalls

- **A URL is not an attachment.** The single most common failure. Download, stage, tag.
- **Tag before write.** Emitting `MEDIA:` for a file that does not exist yet delivers
  nothing, silently, and the tag is stripped — so the user sees a confident message with
  no image.
- **Wrong staging root.** `~/.hermes-mobile/attachment_staging/`, not
  `~/.hermes/profiles/<profile>/…`.
- **Silent size skip.** Over 10 MB is dropped with no error. Check size before tagging.
- **No sensor data yet.** Fresh install means empty tables. Say so; do not guess.
- **Stale location.** A two-hour-old fix is not "where they are now."
- **Missing health metrics.** Some need an Apple Watch. Absence is not an error.
- **SQL safety.** `query_sensor_data` is a read-only connection; writes are blocked at the
  database level.
