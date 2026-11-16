//! One closed set of levels, one cached mapping, both directions.
//!
//! The phone owns the switch and the daemon owns the inference. A mapping change
//! arrives as an ordinary upload, so a key the operator declined once stays
//! declined without the daemon asking again - which is the whole point of
//! caching it rather than deciding per batch.

use rusqlite::params;
use serde_json::{Value, json};

use crate::db::Db;

/// The three levels, and the whole set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// The value may go into a prompt without asking.
    Always,
    /// The value is sent, and the phone asks once.
    Ask,
    /// The key is recorded; the value is not sent.
    Off,
}

impl Level {
    /// The spelling stored and sent.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Always => "always",
            Self::Ask => "ask",
            Self::Off => "off",
        }
    }

    /// Read a level, or `None` when the word is not one of the three.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "always" => Some(Self::Always),
            "ask" => Some(Self::Ask),
            "off" => Some(Self::Off),
            _ => None,
        }
    }

    /// Whether the value may leave the daemon.
    #[must_use]
    pub const fn sends_value(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// The level a key gets when the operator has not said otherwise.
///
/// Health and location are `always` because a question about them is the point of
/// the sensor; calendar and contacts are `ask` because they are about other
/// people; photos are `off` until an explicit send. Anything unknown is `ask`,
/// which is the safe default rather than the convenient one.
#[must_use]
pub fn default_for(key: &str) -> Level {
    match key.trim().to_ascii_lowercase().as_str() {
        "health" | "location" => Level::Always,
        "calendar" | "contacts" | "contact" => Level::Ask,
        "photos" | "photo" => Level::Off,
        _ => Level::Ask,
    }
}

/// The level for a key: the operator's answer when there is one, the default
/// otherwise.
///
/// # Errors
///
/// The database's own error.
pub fn level_for(db: &Db, key: &str) -> Result<Level, crate::db::DbError> {
    let stored: Option<String> = db.with_conn(|conn| {
        Ok(conn
            .query_row(
                "SELECT level FROM sensitivity_map WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .ok())
    })?;
    Ok(stored
        .as_deref()
        .and_then(Level::parse)
        .unwrap_or_else(|| default_for(key)))
}

/// Read one upload of mapping changes.
///
/// The phone owns the switch, so this is the whole vocabulary it can use: one
/// `mapping` or a short `mappings` array, each with a `key` and a `level`. An
/// unknown level is refused rather than stored, because a stored word nothing
/// recognises would read as `ask` for ever.
///
/// # Errors
///
/// The sentence a caller shows, naming the entry that failed.
pub fn parse_mappings(value: &Value) -> Result<Vec<(String, Level)>, String> {
    let entries: Vec<&Value> = match (value.get("mappings"), value.get("key")) {
        (Some(Value::Array(items)), _) => items.iter().collect(),
        (None, Some(_)) => vec![value],
        (Some(other), _) => {
            return Err(format!("`mappings` must be an array, not {other}"));
        }
        (None, None) => {
            return Err("the body carries neither a `key` nor a `mappings` array".to_owned());
        }
    };
    if entries.len() > 200 {
        return Err(format!("a mapping upload carries at most 200 entries, got {}", entries.len()));
    }
    let mut out = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let key = entry
            .get("key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .ok_or_else(|| format!("entry {index} names no `key`"))?;
        let level = entry
            .get("level")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("entry {index} names no `level`"))?;
        let level = Level::parse(level).ok_or_else(|| {
            format!("entry {index} has `{level}`, which is not a level: always, ask, off")
        })?;
        out.push((key.to_owned(), level));
    }
    Ok(out)
}

/// Record the operator's answer about a key.
///
/// # Errors
///
/// The database's own error.
pub fn record(db: &Db, key: &str, level: Level) -> Result<(), crate::db::DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO sensitivity_map (key, level) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET level = ?2, \
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
            params![key, level.as_str()],
        )?;
        Ok(())
    })
}

/// Strip the values a level does not allow, keeping the rows themselves.
///
/// The contract says at `off` the key is recorded and the value is not sent. That
/// has to happen before the write, not at read time: a value that reached the
/// table has already leaked into every backup of it.
pub fn apply_to_batch<F>(rows: &mut [crate::sensors::BatchRow], level_of: F)
where
    F: Fn(&str) -> Level,
{
    for row in rows.iter_mut() {
        let key = row.key.as_deref().unwrap_or(&row.kind).to_owned();
        if !level_of(&key).sends_value() {
            row.value = None;
        }
    }
}

/// What a caller may render for a value at this level.
///
/// At `off` the key is still known - a schedule can say "health" without saying
/// what the reading was - and the value is not.
#[must_use]
pub fn visible(level: Level, key: &str, value: Value) -> Value {
    if level.sends_value() {
        json!({"key": key, "level": level.as_str(), "value": value})
    } else {
        json!({"key": key, "level": level.as_str(), "value": Value::Null})
    }
}

#[cfg(test)]
mod tests {
    use super::{Level, default_for, level_for, record, visible};
    use crate::db::Db;

    /// An unknown key is `ask`: the safe default, not the convenient one.
    #[test]
    fn an_unknown_key_defaults_to_ask() {
        let db = Db::open_in_memory().expect("database");
        assert_eq!(level_for(&db, "who-knows").expect("level"), Level::Ask);
        assert_eq!(default_for("health"), Level::Always);
        assert_eq!(default_for("photos"), Level::Off);
    }

    /// A declined key stays declined without being recorded twice, and the
    /// second write of the same level is not an error.
    #[test]
    fn a_declined_key_stays_declined_across_reads() {
        let db = Db::open_in_memory().expect("database");
        record(&db, "heart_rate", Level::Off).expect("decline");
        assert_eq!(level_for(&db, "heart_rate").expect("level"), Level::Off);
        record(&db, "heart_rate", Level::Off).expect("decline again");
        assert_eq!(level_for(&db, "heart_rate").expect("level"), Level::Off);
    }

    /// A key that flips mid-turn reads through immediately: the next call sees
    /// the new answer, which is what "both directions" means.
    #[test]
    fn a_key_can_flip_between_two_reads() {
        let db = Db::open_in_memory().expect("database");
        record(&db, "steps", Level::Always).expect("allow");
        assert_eq!(level_for(&db, "steps").expect("level"), Level::Always);
        record(&db, "steps", Level::Off).expect("revoke");
        assert_eq!(level_for(&db, "steps").expect("level"), Level::Off);
    }

    /// The upload the phone sends is read strictly: a bad level is refused by
    /// index, and either shape is accepted.
    #[test]
    fn a_mapping_upload_is_read_in_both_shapes() {
        let single = serde_json::json!({"key": "heart_rate", "level": "off"});
        assert_eq!(
            super::parse_mappings(&single).expect("single"),
            vec![("heart_rate".to_owned(), Level::Off)]
        );
        let many = serde_json::json!({"mappings": [
            {"key": "steps", "level": "always"},
            {"key": "contacts", "level": "ask"},
        ]});
        assert_eq!(super::parse_mappings(&many).expect("many").len(), 2);

        let bad = serde_json::json!({"mappings": [{"key": "steps", "level": "sometimes"}]});
        let refused = super::parse_mappings(&bad).expect_err("a bad level is refused");
        assert!(refused.contains("entry 0"), "{refused}");
        assert!(refused.contains("always, ask, off"), "{refused}");
    }

    /// A batch is rewritten before it is stored, so the value never reaches the
    /// table at all.
    #[test]
    fn an_off_key_loses_its_value_before_the_write() {
        let mut rows = vec![
            crate::sensors::BatchRow {
                kind: "health".to_owned(),
                key: Some("heart_rate".to_owned()),
                value: Some(61.0),
                unit: None,
                sensitivity: None,
                recorded_at: "2026-11-16T03:00:00Z".to_owned(),
            },
            crate::sensors::BatchRow {
                kind: "health".to_owned(),
                key: Some("steps".to_owned()),
                value: Some(4210.0),
                unit: None,
                sensitivity: None,
                recorded_at: "2026-11-16T03:00:00Z".to_owned(),
            },
        ];
        super::apply_to_batch(&mut rows, |key| {
            if key == "heart_rate" {
                Level::Off
            } else {
                Level::Always
            }
        });
        assert_eq!(rows[0].value, None, "the declined key keeps its name only");
        assert_eq!(rows[1].value, Some(4210.0), "the allowed key is untouched");
    }

    /// At `off` the key survives and the value does not.
    #[test]
    fn an_off_key_renders_its_name_and_not_its_value() {
        let shown = visible(Level::Off, "steps", serde_json::json!(4210));
        assert_eq!(shown["key"], "steps");
        assert!(shown["value"].is_null());
        let open = visible(Level::Always, "steps", serde_json::json!(4210));
        assert_eq!(open["value"], 4210);
    }
}
