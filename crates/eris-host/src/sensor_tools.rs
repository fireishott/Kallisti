//! The four tools that let the model ask about the phone's senses.
//!
//! Each one reads through the sensitivity mapping instead of around it: a key at
//! `off` comes back with its name and a null value, which is the difference
//! between "the operator records this and will not share it" and "there is no
//! such reading". A tool that hid the key entirely would make the model ask
//! again; a tool that ignored the level would leak the value WP-10 exists to
//! withhold.

use serde_json::{Value, json};

use crate::db::Db;
use crate::sensitivity::{self, Level};
use crate::sensors::{KINDS, SensorStore};
use crate::tools::ToolResult;
use core_runtime::provider::ToolSpec;

/// The name of every tool in this module.
pub const NAMES: [&str; 4] = [
    "where_is_the_operator",
    "health_today",
    "recent_activity",
    "query_device_db",
];

/// The tools as the model sees them, offered only when a sensor store exists.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "where_is_the_operator".to_owned(),
            description: "Where the operator's phone last reported itself and when. Returns the \
                          coordinates, accuracy and address, or a null place when location is \
                          switched off at the sensitivity level."
                .to_owned(),
            parameters: json!({"type": "object", "properties": {}, "required": []}),
        },
        ToolSpec {
            name: "health_today".to_owned(),
            description: "Today's health rollups from the phone, one row per metric with its \
                          value and unit. A metric switched off at the sensitivity level comes \
                          back with a null value."
                .to_owned(),
            parameters: json!({"type": "object", "properties": {}, "required": []}),
        },
        ToolSpec {
            name: "recent_activity".to_owned(),
            description: "The most recent coarse activities the phone reported: walking, \
                          running, driving, cycling or stationary, newest first."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "limit": {"type": "integer", "description": "How many, at most 50. Default 10."}
                },
                "required": []
            }),
        },
        ToolSpec {
            name: "query_device_db".to_owned(),
            description: "Read stored sensor readings of one kind. The kinds are the closed set \
                          the phone may report; anything else is refused rather than guessed."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "description": "One sensor kind, for example health or motion."},
                    "limit": {"type": "integer", "description": "How many rows, at most 100. Default 20."},
                    "since": {"type": "string", "description": "Optional ISO 8601 lower bound on the reading's own timestamp."}
                },
                "required": ["kind"]
            }),
        },
    ]
}

/// Run one call. `None` means this module does not own the name.
pub fn run(
    name: &str,
    arguments: &Value,
    sensors: &SensorStore,
    db: &Db,
) -> Option<ToolResult> {
    match name {
        "where_is_the_operator" => Some(where_is(sensors, db)),
        "health_today" => Some(health_today(sensors, db)),
        "recent_activity" => Some(recent_activity(arguments, sensors, db)),
        "query_device_db" => Some(query_device(arguments, sensors, db)),
        _ => None,
    }
}

/// The last known place, or why there is not one.
fn where_is(sensors: &SensorStore, db: &Db) -> ToolResult {
    let level = sensitivity::level_for(db, "location").unwrap_or(Level::Ask);
    match sensors.current_location() {
        Ok(Some(place)) => {
            let shown = if level.sends_value() {
                json!({
                    "latitude": place.latitude,
                    "longitude": place.longitude,
                    "altitude": place.altitude,
                    "accuracy": place.accuracy,
                    "address": place.address,
                    "recorded_at": place.recorded_at,
                })
            } else {
                Value::Null
            };
            let _ = (level.sends_value(), &place);
            ToolResult::ok(json!({
                "level": level.as_str(),
                "place": shown,
            }))
        }
        Ok(None) => ToolResult::ok(json!({
            "level": level.as_str(),
            "place": Value::Null,
            "note": "the phone has not reported a location",
        })),
        Err(error) => ToolResult::refused(&format!("the location store could not be read: {error}")),
    }
}

/// Today's rollups, with each metric's own value gated by its level.
fn health_today(sensors: &SensorStore, db: &Db) -> ToolResult {
    let rows = match sensors.latest_health() {
        Ok(rows) => rows,
        Err(error) => {
            return ToolResult::refused(&format!("the health store could not be read: {error}"));
        }
    };
    let metrics: Vec<Value> = rows
        .iter()
        .map(|row| {
            let level = sensitivity::level_for(db, &row.metric).unwrap_or(Level::Ask);
            sensitivity::visible(level, &row.metric, json!(row.value))
        })
        .collect();
    ToolResult::ok(json!({"metrics": metrics}))
}

/// The newest activities, newest first.
fn recent_activity(arguments: &Value, sensors: &SensorStore, db: &Db) -> ToolResult {
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(10, |value| value.min(50) as usize);
    let level = sensitivity::level_for(db, "motion").unwrap_or(Level::Ask);
    match sensors.recent_events(&["motion"], limit) {
        Ok(rows) => {
            let events: Vec<Value> = rows
                .into_iter()
                .map(|row| {
                    sensitivity::visible(
                        level,
                        row.key.as_deref().unwrap_or("motion"),
                        json!({"key": row.key, "recorded_at": row.recorded_at}),
                    )
                })
                .collect();
            ToolResult::ok(json!({"level": level.as_str(), "activities": events}))
        }
        Err(error) => {
            ToolResult::refused(&format!("the sensor store could not be read: {error}"))
        }
    }
}

/// One kind, bounded, newest first.
fn query_device(arguments: &Value, sensors: &SensorStore, db: &Db) -> ToolResult {
    let Some(kind) = arguments.get("kind").and_then(Value::as_str) else {
        return ToolResult::refused("query_device_db needs a `kind`");
    };
    if !KINDS.contains(&kind) {
        return ToolResult::refused(&format!(
            "`{kind}` is not a sensor kind. The kinds are: {}",
            KINDS.join(", ")
        ));
    }
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(20, |value| value.min(100) as usize);
    let since = arguments.get("since").and_then(Value::as_str);
    match sensors.events_of(kind, since, limit) {
        Ok(rows) => {
            let events: Vec<Value> = rows
                .into_iter()
                .map(|row| {
                    let key = row.key.clone().unwrap_or_else(|| kind.to_owned());
                    let level = sensitivity::level_for(db, &key).unwrap_or(Level::Ask);
                    let value = if level.sends_value() {
                        json!(row.value)
                    } else {
                        Value::Null
                    };
                    json!({
                        "key": row.key,
                        "value": value,
                        "unit": row.unit,
                        "level": level.as_str(),
                        "recorded_at": row.recorded_at,
                    })
                })
                .collect();
            ToolResult::ok(json!({"kind": kind, "events": events}))
        }
        Err(error) => {
            ToolResult::refused(&format!("the sensor store could not be read: {error}"))
        }
    }
}


#[cfg(test)]
mod tests {
    use super::{run, specs};
    use crate::db::Db;
    use crate::sensors::SensorStore;

    /// Four tools, each with the arguments the model has to supply.
    #[test]
    fn the_specs_are_the_four_tools() {
        let specs = specs();
        assert_eq!(specs.len(), 4);
        let names: Vec<&str> = specs.iter().map(|spec| spec.name.as_str()).collect();
        assert!(names.contains(&"where_is_the_operator"));
        assert!(names.contains(&"query_device_db"));
    }

    /// A name this module does not own is not silently answered.
    #[test]
    fn a_foreign_name_is_not_claimed() {
        let sensors = SensorStore::open_in_memory().expect("store");
        let db = Db::open_in_memory().expect("db");
        assert!(run("write_file", &serde_json::json!({}), &sensors, &db).is_none());
    }

    /// An unknown kind is refused by name and the refusal lists the closed set.
    #[test]
    fn an_unknown_kind_is_refused_with_the_closed_set() {
        let sensors = SensorStore::open_in_memory().expect("store");
        let db = Db::open_in_memory().expect("db");
        let result = run(
            "query_device_db",
            &serde_json::json!({"kind": "telepathy"}),
            &sensors,
            &db,
        )
        .expect("owned");
        assert!(result.is_error);
        assert!(result.content.contains("telepathy"), "{}", result.content);
        assert!(result.content.contains("motion"), "{}", result.content);
    }

    /// An empty store answers empty rather than failing: "nothing recorded" and
    /// "could not read" are different facts.
    #[test]
    fn an_empty_store_answers_empty() {
        let sensors = SensorStore::open_in_memory().expect("store");
        let db = Db::open_in_memory().expect("db");
        let result = run("health_today", &serde_json::json!({}), &sensors, &db).expect("owned");
        assert!(!result.is_error);
        assert!(result.content.contains("metrics"), "{}", result.content);
    }
}
