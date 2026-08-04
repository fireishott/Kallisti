# Hermes iOS Sensor Schema

This document describes the connector’s SQLite database at:

```text
~/.hermes-mobile/state/sensors.db
```

It is the storage layer behind the sensor MCP tools and the voice-context freshness summaries.

> [!NOTE]
> This is a technical reference for builders and skill authors. If you are just trying to get the stack running, start with [../README.md](../README.md), [README.md](README.md), and [../docs/CONFIGURATION.md](../docs/CONFIGURATION.md).

## Why this database exists

The connector receives phone-originated sensor data from the relay and stores it locally so Hermes can query it through MCP tools.

That gives you:

- fast local lookups for current state
- historical queries and SQL-backed analysis
- a stable schema that can be reused by future native integrations

## Database behavior

- SQLite
- WAL mode
- local to the connector host
- 90-day retention for raw and rolled-up sensor data

## Tables

### `location_current`

Single-row table (`id = 1`) with the latest known location.

| Column | Type | Description |
| --- | --- | --- |
| `id` | INTEGER | Always `1` |
| `latitude` | REAL | Decimal degrees |
| `longitude` | REAL | Decimal degrees |
| `altitude` | REAL | Meters above sea level |
| `accuracy` | REAL | Horizontal accuracy in meters |
| `address` | TEXT | Reverse-geocoded address, nullable |
| `recorded_at` | TEXT | Device timestamp |
| `updated_at` | TEXT | Last local write time |

### `location_history`

Append-only trail of distinct locations. Near-duplicate points are suppressed.

| Column | Type | Description |
| --- | --- | --- |
| `id` | TEXT | UUID primary key |
| `latitude`, `longitude`, `altitude`, `accuracy`, `address` | | Same semantics as `location_current` |
| `recorded_at` | TEXT | Device timestamp |
| `created_at` | TEXT | Insert time |

Indexed on `recorded_at DESC`. Pruned after 90 days.

### `health_samples`

Time-series HealthKit samples and derived snapshots.

| Column | Type | Description |
| --- | --- | --- |
| `id` | TEXT | UUID primary key |
| `metric` | TEXT | Metric name such as `steps`, `heart_rate`, or `sleep_duration` |
| `value` | REAL | Numeric value |
| `unit` | TEXT | Unit such as `count`, `bpm`, `hours`, `kcal` |
| `start_at` | TEXT | Start of measurement window |
| `end_at` | TEXT | End of measurement window, nullable for point samples |
| `source` | TEXT | Currently `healthkit` |
| `created_at` | TEXT | Insert time |

Indexed on `(metric, start_at DESC)`. Pruned after 90 days.

### `health_latest`

One row per metric for O(1) “current state” lookups.

| Column | Type | Description |
| --- | --- | --- |
| `metric` | TEXT | Primary key |
| `value` | REAL | Latest value |
| `unit` | TEXT | Unit |
| `recorded_at` | TEXT | Recorded timestamp |
| `start_at` | TEXT | Start of sample window |
| `end_at` | TEXT | End of sample window |
| `updated_at` | TEXT | Last upsert time |

### `health_daily`

Daily rollups computed from `health_samples`.

| Column | Type | Description |
| --- | --- | --- |
| `metric` | TEXT | Metric name |
| `date` | TEXT | `YYYY-MM-DD` |
| `sum_value` | REAL | Sum or max-style rollup depending on metric |
| `avg_value` | REAL | Daily average |
| `min_value` | REAL | Daily min |
| `max_value` | REAL | Daily max |
| `count` | INTEGER | Number of contributing samples |
| `unit` | TEXT | Unit |
| `updated_at` | TEXT | Rollup time |

Primary key: `(metric, date)`. Pruned after 90 days.

## Available health metrics

| Metric | Unit | Shape | Notes |
| --- | --- | --- | --- |
| `steps` | count | cumulative today | Daily running total |
| `active_calories` | kcal | cumulative today | Active energy |
| `distance_walking` | meters | cumulative today | Walking + running distance |
| `heart_rate` | bpm | latest sample | Most recent sample in range |
| `resting_heart_rate` | bpm | latest sample | Resting sample |
| `blood_oxygen` | % | latest sample | SpO2 |
| `respiratory_rate` | breaths/min | latest sample | Breathing rate |
| `body_mass` | kg | latest sample | Body weight |
| `workout_minutes` | minutes | cumulative today | Exercise time |
| `stand_hours` | hours | cumulative today | Standing time |
| `sleep_duration` | hours | daily attributed | Attributed to the wake-up day |

## MCP tools

| Tool | Purpose |
| --- | --- |
| `get_user_location()` | Current location plus freshness |
| `get_location_history(since?, limit?)` | Historical locations |
| `get_health_summary()` | Latest values across metrics |
| `get_health_metric(metric, since?, limit?)` | Time-series for one metric |
| `get_health_metrics_list()` | Metric inventory and latest values |
| `get_sensor_schema()` | Full schema listing |
| `query_sensor_data(sql, limit?)` | Read-only SQL queries |

## Useful query patterns

### Current health snapshot

```sql
SELECT metric, value, unit, recorded_at
FROM health_latest
ORDER BY metric;
```

### Last 7 days of steps

```sql
SELECT date, max_value AS daily_steps
FROM health_daily
WHERE metric = 'steps'
ORDER BY date DESC
LIMIT 7;
```

### Recent location trail

```sql
SELECT recorded_at, latitude, longitude, address
FROM location_history
ORDER BY recorded_at DESC
LIMIT 20;
```

## Architecture note

This schema is intentionally reusable. If Hermes iOS ever gains a more native direct-storage integration, the goal is to keep the same table and MCP shape so existing skills, dashboards, and queries continue to work.
