//! Sensor and health data: the backend half of the app's "senses".
//!
//! HealthKit, CoreMotion and location data land here from a paired phone and are
//! read back by the iOS health/senses dashboard. This is a dedicated store in its
//! own SQLite file (`sensors.db` beside `eris.db`), not a set of tables in the
//! daemon database: the schema below is the legacy `sensor_store` schema verbatim,
//! because downstream queries and the iOS side are built against those exact
//! names. Keeping it in its own file means this module can never drift the main
//! daemon's migration list, and a sensor write can never contend with a turn.
//!
//! The file is opened with the same three pragmas as [`crate::db`]: WAL,
//! `busy_timeout`, and `synchronous=FULL`, so a commit that acks a sample
//! survives a power cut, not only a process crash.
//!
//! Storage behaviour mirrors the legacy store:
//!
//! - a health POST inserts one row per sample into `health_samples` (id is a
//!   generated UUID) and upserts `health_latest` per metric;
//! - a sample whose `(metric, unit, start_at)` already exists is a duplicate and
//!   is skipped entirely;
//! - a location POST upserts `location_current` (`id = 1`) and appends to
//!   `location_history`;
//! - a location point whose `recorded_at`, latitude and longitude all match the
//!   current point (within a tiny epsilon) is a duplicate and is skipped.
//!
//! `health_daily` is created for schema compatibility but is not aggregated in
//! this pass; nothing downstream has asked for it yet.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::db::BUSY_TIMEOUT;

/// Two points this close in degrees (about a centimetre) count as one location.
const LOCATION_NEAR_EPSILON: f64 = 1e-7;

/// The legacy `sensor_store` schema, byte for byte. Do not rename a column here:
/// the iOS side and the downstream health queries read these names directly.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS location_current (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    latitude REAL NOT NULL, longitude REAL NOT NULL, altitude REAL,
    accuracy REAL, address TEXT, recorded_at TEXT NOT NULL, updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS location_history (
    id TEXT PRIMARY KEY, latitude REAL NOT NULL, longitude REAL NOT NULL,
    altitude REAL, accuracy REAL, address TEXT, recorded_at TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_location_history_time ON location_history(recorded_at DESC);
CREATE TABLE IF NOT EXISTS device_events (
    id          TEXT PRIMARY KEY, kind TEXT NOT NULL, key TEXT, value REAL, unit TEXT,
    sensitivity TEXT, recorded_at TEXT NOT NULL,
    received_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_device_events_kind ON device_events(kind, recorded_at DESC);
CREATE TABLE IF NOT EXISTS health_samples (
    id TEXT PRIMARY KEY, metric TEXT NOT NULL, value REAL NOT NULL, unit TEXT NOT NULL,
    start_at TEXT NOT NULL, end_at TEXT, source TEXT NOT NULL DEFAULT 'healthkit',
    created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
);
CREATE INDEX IF NOT EXISTS idx_health_samples_metric_time ON health_samples(metric, start_at DESC);
CREATE TABLE IF NOT EXISTS health_latest (
    metric TEXT PRIMARY KEY, value REAL NOT NULL, unit TEXT NOT NULL,
    recorded_at TEXT, start_at TEXT, end_at TEXT, updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS health_daily (
    metric TEXT NOT NULL, date TEXT NOT NULL, sum_value REAL, avg_value REAL,
    min_value REAL, max_value REAL, count INTEGER NOT NULL DEFAULT 0,
    unit TEXT NOT NULL, updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
    PRIMARY KEY (metric, date)
);
CREATE INDEX IF NOT EXISTS idx_health_daily_date ON health_daily(date DESC);
";

/// `POST /v1/device/sensor/location`. Field names are the legacy wire names, so
/// `recordedAt` stays camelCase even though the rest of the crate is snake_case.
#[derive(Debug, Deserialize)]
pub struct LocationUpload {
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: Option<f64>,
    pub accuracy: f64,
    pub address: Option<String>,
    #[serde(rename = "recordedAt")]
    pub recorded_at: String,
}

/// `POST /v1/device/sensor/health`.
#[derive(Debug, Deserialize)]
pub struct HealthUpload {
    pub samples: Vec<HealthSample>,
}

/// One health sample, with the legacy `startAt`/`endAt` spellings.
#[derive(Debug, Deserialize)]
pub struct HealthSample {
    pub metric: String,
    pub value: f64,
    pub unit: String,
    #[serde(rename = "startAt")]
    pub start_at: String,
    #[serde(rename = "endAt")]
    pub end_at: Option<String>,
}

/// The upload ack. `deliveryState == "delivered"` is the exact field and value
/// the iOS `DeliveryResult` decoder requires, so both are pinned here.
#[derive(Debug, Serialize)]
pub struct Delivered {
    #[serde(rename = "deliveryState")]
    pub delivery_state: &'static str,
}

impl Delivered {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            delivery_state: "delivered",
        }
    }
}

impl Default for Delivered {
    fn default() -> Self {
        Self::new()
    }
}

/// `GET /v1/device/health/latest`.
#[derive(Debug, Serialize)]
pub struct HealthLatest {
    pub metrics: Vec<HealthMetricRow>,
}

/// One metric row, ordered by metric. `recorded_at` is a non-null string on read
/// because the query coalesces `recorded_at` to `start_at` when it is absent.
#[derive(Debug, Serialize, PartialEq)]
pub struct HealthMetricRow {
    pub metric: String,
    pub value: f64,
    pub unit: String,
    pub recorded_at: String,
}

/// `GET /v1/device/location`. The nullable columns mirror the schema's nullable
/// `REAL`/`TEXT` columns exactly.
#[derive(Debug, Serialize, PartialEq)]
pub struct LocationSnapshot {
    pub latitude: f64,
    pub longitude: f64,
    pub altitude: Option<f64>,
    pub accuracy: Option<f64>,
    pub address: Option<String>,
    pub recorded_at: String,
}

/// A sensor-store failure. It is always turned into a 503 by the device layer,
/// because a sensor write that cannot reach the database must not look like a
/// successful delivery.
#[derive(Debug, thiserror::Error)]
pub enum SensorError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("sensor database connection is unusable: a previous holder panicked")]
    Locked,
    #[error("cannot prepare state directory {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("refusing to open the sensor store: journal_mode is {actual:?}, not wal")]
    NotWal { actual: String },
}

/// A handle to the sensor database. Cheap to clone; the connection is shared.
/// Every kind a phone may report, and the whole set: anything else is refused
/// rather than stored under a name nobody reads.
pub const KINDS: [&str; 10] = [
    "location",
    "health",
    "motion",
    "notification",
    "alarm",
    "focus",
    "shortcut",
    "calendar",
    "reminder",
    "contact",
];

/// How long each kind is kept, in days.
///
/// Location is the shortest because it is the most sensitive and the least
/// useful a season later; health is the longest because a year-over-year reading
/// is the point of keeping health at all. A kind not listed here is kept for
/// ninety days, which is the floor rather than an accident.
#[must_use]
pub fn retention_days(kind: &str) -> i64 {
    match kind {
        "location" => 90,
        "health" => 365,
        "motion" => 30,
        _ => 90,
    }
}

/// How far a sample's own clock may run ahead of the daemon's before the
/// timestamp is clamped. A phone with a wrong date must not write next year into
/// the table, and a five-minute allowance covers a lazy NTP sync.
pub const MAX_SKEW_SECS: i64 = 300;

/// One row of a sensor batch, after it has been checked.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchRow {
    /// One of [`KINDS`].
    pub kind: String,
    /// What the sample is about: a metric, a package, a person.
    pub key: Option<String>,
    /// The number, when the sample is a number.
    pub value: Option<f64>,
    /// Its unit, verbatim.
    pub unit: Option<String>,
    /// The phone's own sensitivity reading, verbatim.
    pub sensitivity: Option<String>,
    /// RFC 3339, when the sample was taken.
    pub recorded_at: String,
}

/// Check a batch before a single row of it is stored.
///
/// The contract is atomic: one bad row fails the batch. That is only true if the
/// check happens first, because a writer that inserts as it validates leaves the
/// good rows behind when the bad one arrives - and a phone that retries would
/// then double them.
///
/// # Errors
///
/// The sentence a caller shows, naming the index that failed.
pub fn parse_batch(value: &serde_json::Value) -> Result<Vec<BatchRow>, String> {
    let rows = value
        .get("rows")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "the body carries no `rows` array".to_owned())?;
    let mut out = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let kind = row
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("row {index} names no `kind`"))?;
        if !KINDS.contains(&kind) {
            return Err(format!(
                "row {index} names `{kind}`, which is not a sensor kind: {}",
                KINDS.join(", ")
            ));
        }
        let recorded_at = row
            .get("ts")
            .or_else(|| row.get("recorded_at"))
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| format!("row {index} carries no `ts`"))?;
        if time::OffsetDateTime::parse(recorded_at, &time::format_description::well_known::Rfc3339)
            .is_err()
        {
            return Err(format!("row {index} has a `ts` that is not RFC 3339"));
        }
        let value = match row.get("value") {
            None | Some(serde_json::Value::Null) => None,
            Some(number) => Some(
                number
                    .as_f64()
                    .ok_or_else(|| format!("row {index} has a `value` that is not a number"))?,
            ),
        };
        // A timestamp in the future is a clock that is wrong, not a sample that
        // happened. Clamped to now rather than refused: the reading is real even
        // when the phone's date is not.
        let now = time::OffsetDateTime::now_utc();
        let recorded_at = if let Ok(parsed) =
            time::OffsetDateTime::parse(recorded_at, &time::format_description::well_known::Rfc3339)
        {
            if (parsed - now).whole_seconds() > MAX_SKEW_SECS {
                now.format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_else(|_| recorded_at.to_owned())
            } else {
                recorded_at.to_owned()
            }
        } else {
            recorded_at.to_owned()
        };
        out.push(BatchRow {
            kind: kind.to_owned(),
            key: row.get("key").and_then(serde_json::Value::as_str).map(str::to_owned),
            value,
            unit: row.get("unit").and_then(serde_json::Value::as_str).map(str::to_owned),
            sensitivity: row
                .get("sensitivity")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            recorded_at,
        });
    }
    Ok(out)
}

#[derive(Clone)]
pub struct SensorStore {
    inner: Arc<Inner>,
}

struct Inner {
    conn: Mutex<Connection>,
}

impl SensorStore {
    /// Open (creating if needed), configure, and initialise the sensor database.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SensorError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| SensorError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let conn = Connection::open(path)?;
        configure(&conn)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            inner: Arc::new(Inner {
                conn: Mutex::new(conn),
            }),
        })
    }

    /// An in-memory database for tests. WAL is not available to `:memory:`, so the
    /// pragmas that still apply are set and the WAL check is skipped.
    pub fn open_in_memory() -> Result<Self, SensorError> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(&format!(
            "PRAGMA foreign_keys = ON; PRAGMA busy_timeout = {};",
            BUSY_TIMEOUT.as_millis()
        ))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            inner: Arc::new(Inner {
                conn: Mutex::new(conn),
            }),
        })
    }

    /// Store one location point: upsert `location_current` and append to
    /// `location_history`, unless it is a duplicate of the current point.
    pub fn record_location(&self, upload: &LocationUpload) -> Result<(), SensorError> {
        let mut conn = self.lock()?;
        let transaction = conn.transaction()?;

        let current = optional(transaction.query_row(
            "SELECT latitude, longitude, recorded_at FROM location_current WHERE id = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, f64>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        ))?;

        if let Some((latitude, longitude, recorded_at)) = current
            && recorded_at == upload.recorded_at
            && (latitude - upload.latitude).abs() < LOCATION_NEAR_EPSILON
            && (longitude - upload.longitude).abs() < LOCATION_NEAR_EPSILON
        {
            transaction.commit()?;
            return Ok(());
        }

        transaction.execute(
            "INSERT INTO location_current
                 (id, latitude, longitude, altitude, accuracy, address, recorded_at, updated_at)
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
             ON CONFLICT(id) DO UPDATE SET
                 latitude = excluded.latitude,
                 longitude = excluded.longitude,
                 altitude = excluded.altitude,
                 accuracy = excluded.accuracy,
                 address = excluded.address,
                 recorded_at = excluded.recorded_at,
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
            rusqlite::params![
                upload.latitude,
                upload.longitude,
                upload.altitude,
                upload.accuracy,
                upload.address,
                upload.recorded_at,
            ],
        )?;

        let id = uuid::Uuid::now_v7().to_string();
        transaction.execute(
            "INSERT INTO location_history
                 (id, latitude, longitude, altitude, accuracy, address, recorded_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                id,
                upload.latitude,
                upload.longitude,
                upload.altitude,
                upload.accuracy,
                upload.address,
                upload.recorded_at,
            ],
        )?;

        transaction.commit()?;
        Ok(())
    }

    /// Store health samples: insert into `health_samples` and upsert
    /// `health_latest`, skipping samples that are already present.
    pub fn record_health(&self, samples: &[HealthSample]) -> Result<(), SensorError> {
        let mut conn = self.lock()?;
        let transaction = conn.transaction()?;

        for sample in samples {
            let duplicate: bool = transaction.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM health_samples
                     WHERE metric = ?1 AND unit = ?2 AND start_at = ?3
                 )",
                rusqlite::params![sample.metric, sample.unit, sample.start_at],
                |row| row.get(0),
            )?;
            if duplicate {
                continue;
            }

            let id = uuid::Uuid::now_v7().to_string();
            transaction.execute(
                "INSERT INTO health_samples (id, metric, value, unit, start_at, end_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    id,
                    sample.metric,
                    sample.value,
                    sample.unit,
                    sample.start_at,
                    sample.end_at,
                ],
            )?;

            transaction.execute(
                "INSERT INTO health_latest
                     (metric, value, unit, recorded_at, start_at, end_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, strftime('%Y-%m-%dT%H:%M:%fZ','now'))
                 ON CONFLICT(metric) DO UPDATE SET
                     value = excluded.value,
                     unit = excluded.unit,
                     recorded_at = excluded.recorded_at,
                     start_at = excluded.start_at,
                     end_at = excluded.end_at,
                     updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
                rusqlite::params![
                    sample.metric,
                    sample.value,
                    sample.unit,
                    sample.start_at,
                    sample.start_at,
                    sample.end_at,
                ],
            )?;
        }

        transaction.commit()?;
        Ok(())
    }

    /// The latest reading per metric, ordered by metric.
    pub fn latest_health(&self) -> Result<Vec<HealthMetricRow>, SensorError> {
        let conn = self.lock()?;
        let mut statement = conn.prepare(
            "SELECT metric, value, unit, COALESCE(recorded_at, start_at, '')
             FROM health_latest ORDER BY metric",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(HealthMetricRow {
                metric: row.get(0)?,
                value: row.get(1)?,
                unit: row.get(2)?,
                recorded_at: row.get(3)?,
            })
        })?;
        let mut metrics = Vec::new();
        for row in rows {
            metrics.push(row?);
        }
        Ok(metrics)
    }

    /// The current location, if one has been recorded.
    pub fn current_location(&self) -> Result<Option<LocationSnapshot>, SensorError> {
        let conn = self.lock()?;
        let snapshot = optional(conn.query_row(
            "SELECT latitude, longitude, altitude, accuracy, address, recorded_at
             FROM location_current WHERE id = 1",
            [],
            |row| {
                Ok(LocationSnapshot {
                    latitude: row.get(0)?,
                    longitude: row.get(1)?,
                    altitude: row.get(2)?,
                    accuracy: row.get(3)?,
                    address: row.get(4)?,
                    recorded_at: row.get(5)?,
                })
            },
        ))?;
        Ok(snapshot)
    }

    /// Store a whole batch, or none of it.
    ///
    /// One transaction: the contract says a batch is atomic, and a phone that
    /// retried a half-written batch would double the rows that did land. Every
    /// row is checked before this is called, so nothing fails halfway from a bad
    /// value; what is left for the transaction is a database failure, which
    /// rolls the lot back.
    ///
    /// # Errors
    ///
    /// The database's own error, with the batch rolled back.
    pub fn record_batch(&self, rows: &[BatchRow]) -> Result<usize, SensorError> {
        if rows.is_empty() {
            return Ok(0);
        }
        let mut conn = self.lock()?;
        let transaction = conn.transaction()?;
        for row in rows {
            transaction.execute(
                "INSERT INTO device_events \
                    (id, kind, key, value, unit, sensitivity, recorded_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![
                    uuid::Uuid::now_v7().to_string(),
                    row.kind,
                    row.key,
                    row.value,
                    row.unit,
                    row.sensitivity,
                    row.recorded_at,
                ],
            )?;
        }
        // Retention, per kind, in the same transaction: a table that only grows
        // is a table that eventually cannot be read on a phone.
        for kind in KINDS {
            transaction.execute(
                "DELETE FROM device_events \
                  WHERE kind = ?1 \
                    AND recorded_at < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?2)",
                rusqlite::params![kind, format!("-{} days", retention_days(kind))],
            )?;
        }
        transaction.commit()?;
        Ok(rows.len())
    }

    /// How many rows of one kind are stored. Used by the retention test and by
    /// anything that needs to know the table is not growing without bound.
    ///
    /// # Errors
    ///
    /// The database's own error.
    pub fn count_events(&self, kind: &str) -> Result<i64, SensorError> {
        let conn = self.lock()?;
        Ok(conn.query_row(
            "SELECT count(*) FROM device_events WHERE kind = ?1",
            rusqlite::params![kind],
            |row| row.get(0),
        )?)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, SensorError> {
        self.inner.conn.lock().map_err(|_| SensorError::Locked)
    }
}

/// The three pragmas that are not optional. Mirrors [`crate::db::configure`].
fn configure(conn: &Connection) -> Result<(), SensorError> {
    let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(SensorError::NotWal { actual: mode });
    }
    conn.execute_batch(&format!(
        "PRAGMA foreign_keys = ON; PRAGMA busy_timeout = {}; PRAGMA synchronous = FULL;",
        BUSY_TIMEOUT.as_millis()
    ))?;
    Ok(())
}

/// `Option<T>` for a query that may match no row, without a crate for it.
fn optional<T>(result: Result<T, rusqlite::Error>) -> Result<Option<T>, rusqlite::Error> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> SensorStore {
        SensorStore::open_in_memory().expect("in-memory sensor store opens")
    }

    fn location(latitude: f64, longitude: f64, recorded_at: &str) -> LocationUpload {
        LocationUpload {
            latitude,
            longitude,
            altitude: Some(150.0),
            accuracy: 5.0,
            address: Some("Springfield, MO".to_owned()),
            recorded_at: recorded_at.to_owned(),
        }
    }

    fn sample(metric: &str, value: f64, start_at: &str) -> HealthSample {
        HealthSample {
            metric: metric.to_owned(),
            value,
            unit: "count".to_owned(),
            start_at: start_at.to_owned(),
            end_at: None,
        }
    }

    fn count(store: &SensorStore, sql: &str) -> i64 {
        let conn = store.inner.conn.lock().expect("lock");
        conn.query_row(sql, [], |row| row.get(0)).expect("count")
    }

    #[test]
    fn location_post_upserts_current_and_appends_history() {
        let store = store();
        store
            .record_location(&location(37.2090, -93.2923, "2026-10-19T12:00:00.000Z"))
            .expect("first location");

        let snapshot = store.current_location().expect("current").expect("present");
        assert_eq!(snapshot.latitude, 37.2090);
        assert_eq!(snapshot.longitude, -93.2923);
        assert_eq!(snapshot.altitude, Some(150.0));
        assert_eq!(snapshot.accuracy, Some(5.0));
        assert_eq!(snapshot.address.as_deref(), Some("Springfield, MO"));
        assert_eq!(snapshot.recorded_at, "2026-10-19T12:00:00.000Z");
        assert_eq!(count(&store, "SELECT COUNT(*) FROM location_history"), 1);

        // A second, distinct point replaces current and grows history.
        store
            .record_location(&location(38.26, -85.77, "2026-10-19T12:01:00.000Z"))
            .expect("second location");
        assert_eq!(count(&store, "SELECT COUNT(*) FROM location_history"), 2);
        let snapshot = store.current_location().expect("current").expect("present");
        assert_eq!(snapshot.latitude, 38.26);
    }

    #[test]
    fn location_duplicate_is_suppressed() {
        let store = store();
        store
            .record_location(&location(37.2090, -93.2923, "2026-10-19T12:00:00.000Z"))
            .expect("first");
        store
            .record_location(&location(37.2090, -93.2923, "2026-10-19T12:00:00.000Z"))
            .expect("duplicate");
        assert_eq!(count(&store, "SELECT COUNT(*) FROM location_history"), 1);
    }

    #[test]
    fn health_post_stores_samples_and_latest() {
        let store = store();
        let samples = vec![
            sample("step_count", 512.0, "2026-10-19T12:00:00.000Z"),
            sample("heart_rate", 72.0, "2026-10-19T12:00:00.000Z"),
        ];
        store.record_health(&samples).expect("health samples");

        assert_eq!(count(&store, "SELECT COUNT(*) FROM health_samples"), 2);

        let latest = store.latest_health().expect("latest");
        assert_eq!(latest.len(), 2);
        // Ordered by metric.
        assert_eq!(latest[0].metric, "heart_rate");
        assert_eq!(latest[0].value, 72.0);
        assert_eq!(latest[1].metric, "step_count");
        assert_eq!(latest[1].value, 512.0);
        // recorded_at falls back to the sample start.
        assert_eq!(latest[1].recorded_at, "2026-10-19T12:00:00.000Z");
    }

    #[test]
    fn health_duplicate_is_deduped_and_latest_upserted() {
        let store = store();
        store
            .record_health(&[sample("step_count", 512.0, "2026-10-19T12:00:00.000Z")])
            .expect("first");

        // Same metric+unit+start_at: duplicate, no new sample row.
        store
            .record_health(&[sample("step_count", 512.0, "2026-10-19T12:00:00.000Z")])
            .expect("duplicate");
        assert_eq!(count(&store, "SELECT COUNT(*) FROM health_samples"), 1);

        // A new start_at is a new sample and upserts the same metric's latest.
        store
            .record_health(&[sample("step_count", 600.0, "2026-10-19T13:00:00.000Z")])
            .expect("newer");
        assert_eq!(count(&store, "SELECT COUNT(*) FROM health_samples"), 2);
        let latest = store.latest_health().expect("latest");
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].value, 600.0);
        assert_eq!(latest[0].recorded_at, "2026-10-19T13:00:00.000Z");
    }

    #[test]
    fn read_shapes_serialize_to_the_contract() {
        let store = store();
        store
            .record_health(&[sample("step_count", 512.0, "2026-10-19T12:00:00.000Z")])
            .expect("health");
        store
            .record_location(&location(37.2090, -93.2923, "2026-10-19T12:00:00.000Z"))
            .expect("location");

        let health = serde_json::to_string(&HealthLatest {
            metrics: store.latest_health().expect("latest"),
        })
        .expect("health json");
        assert_eq!(
            health,
            r#"{"metrics":[{"metric":"step_count","value":512.0,"unit":"count","recorded_at":"2026-10-19T12:00:00.000Z"}]}"#
        );

        let location =
            serde_json::to_string(&store.current_location().expect("current").expect("present"))
                .expect("location json");
        assert_eq!(
            location,
            r#"{"latitude":37.209,"longitude":-93.2923,"altitude":150.0,"accuracy":5.0,"address":"Springfield, MO","recorded_at":"2026-10-19T12:00:00.000Z"}"#
        );

        let delivered = serde_json::to_string(&Delivered::new()).expect("delivered json");
        assert_eq!(delivered, r#"{"deliveryState":"delivered"}"#);
    }

    #[test]
    fn no_location_is_none_not_a_row() {
        let store = store();
        assert!(store.current_location().expect("current").is_none());
    }
}

#[cfg(test)]
mod batch_tests {
    use super::{parse_batch, BatchRow, SensorStore};

    fn row(kind: &str, ts: &str) -> serde_json::Value {
        serde_json::json!({"kind": kind, "key": "steps", "value": 42.0, "ts": ts})
    }

    /// One bad row fails the whole batch before anything is written.
    #[test]
    fn a_batch_is_checked_before_a_single_row_is_stored() {
        let body = serde_json::json!({"rows": [
            row("health", "2026-11-16T03:00:00Z"),
            row("nonsense", "2026-11-16T03:00:00Z"),
        ]});
        let refused = parse_batch(&body).expect_err("an unknown kind is refused");
        assert!(refused.contains("row 1"), "{refused}");
        assert!(refused.contains("nonsense"), "{refused}");
    }

    #[test]
    fn a_row_without_a_usable_timestamp_is_refused_by_index() {
        let body = serde_json::json!({"rows": [row("health", "yesterday")]});
        let refused = parse_batch(&body).expect_err("a bad ts is refused");
        assert!(refused.contains("RFC 3339"), "{refused}");
    }

    /// A phone whose clock is a year fast must not write next year into the
    /// table; the reading is kept, the timestamp is not believed.
    #[test]
    fn a_future_timestamp_is_clamped_rather_than_trusted() {
        let body = serde_json::json!({"rows": [
            {"kind": "health", "key": "steps", "value": 1.0, "ts": "2099-01-01T00:00:00Z"},
        ]});
        let rows = parse_batch(&body).expect("a future sample still parses");
        let stamped = time::OffsetDateTime::parse(
            &rows[0].recorded_at,
            &time::format_description::well_known::Rfc3339,
        )
        .expect("still RFC 3339");
        let now = time::OffsetDateTime::now_utc();
        assert!(
            stamped <= now + time::Duration::seconds(super::MAX_SKEW_SECS),
            "clamped to now, got {}",
            rows[0].recorded_at
        );
    }

    /// Retention is per kind and runs in the same transaction as the write.
    #[test]
    fn a_stale_row_is_pruned_when_a_batch_is_written() {
        let store = SensorStore::open_in_memory().expect("sensor store");
        let old = BatchRow {
            kind: "motion".to_owned(),
            key: None,
            value: None,
            unit: None,
            sensitivity: None,
            recorded_at: "2019-01-01T00:00:00Z".to_owned(),
        };
        assert_eq!(store.record_batch(&[old.clone()]).expect("seed"), 1);
        let fresh = BatchRow {
            recorded_at: time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default(),
            ..old
        };
        store.record_batch(&[fresh]).expect("write");
        let kept = store.count_events("motion").expect("count");
        assert_eq!(kept, 1, "the 2019 motion row is outside its 30-day window");
    }

    #[test]
    fn a_good_batch_parses_and_stores_in_one_go() {
        let body = serde_json::json!({"rows": [
            row("health", "2026-11-16T03:00:00Z"),
            row("motion", "2026-11-16T03:01:00Z"),
        ]});
        let rows: Vec<BatchRow> = parse_batch(&body).expect("a valid batch");
        assert_eq!(rows.len(), 2);
        let store = SensorStore::open_in_memory().expect("sensor store");
        assert_eq!(store.record_batch(&rows).expect("record"), 2);
        assert_eq!(store.record_batch(&[]).expect("empty"), 0);
    }
}
