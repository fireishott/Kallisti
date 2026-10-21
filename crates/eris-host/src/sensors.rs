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
            address: Some("Louisville, KY".to_owned()),
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
            .record_location(&location(38.25, -85.76, "2026-10-19T12:00:00.000Z"))
            .expect("first location");

        let snapshot = store.current_location().expect("current").expect("present");
        assert_eq!(snapshot.latitude, 38.25);
        assert_eq!(snapshot.longitude, -85.76);
        assert_eq!(snapshot.altitude, Some(150.0));
        assert_eq!(snapshot.accuracy, Some(5.0));
        assert_eq!(snapshot.address.as_deref(), Some("Louisville, KY"));
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
            .record_location(&location(38.25, -85.76, "2026-10-19T12:00:00.000Z"))
            .expect("first");
        store
            .record_location(&location(38.25, -85.76, "2026-10-19T12:00:00.000Z"))
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
            .record_location(&location(38.25, -85.76, "2026-10-19T12:00:00.000Z"))
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
            r#"{"latitude":38.25,"longitude":-85.76,"altitude":150.0,"accuracy":5.0,"address":"Louisville, KY","recorded_at":"2026-10-19T12:00:00.000Z"}"#
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
