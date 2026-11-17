//! The three schedule tools, so the operator can set a recurring job without
//! opening the dashboard.
//!
//! `schedule_run` takes a cron expression rather than an `at` time. A one-shot
//! time is a different lifecycle from a recurring one - it has to switch itself
//! off after firing, and nothing in this daemon does that yet - so asking for one
//! is refused with the reason instead of quietly creating a schedule that fires
//! every year at that minute.

use serde_json::{Value, json};

use crate::db::Db;
use crate::scheduler;
use crate::tools::ToolResult;
use core_runtime::provider::ToolSpec;

/// The names this module answers to.
pub const NAMES: [&str; 3] = ["schedule_run", "schedule_list", "schedule_cancel"];

/// The tools as the model sees them.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "schedule_run".to_owned(),
            description: "Create a recurring task. It runs the prompt on the cron schedule and \
                          delivers the answer to the places asked for."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "A short name, unique enough to recognise."},
                    "cron": {"type": "string", "description": "Five-field cron, for example `30 6 * * 1-5` for 6:30 on weekdays."},
                    "prompt": {"type": "string", "description": "What to ask when it fires."},
                    "deliver": {
                        "type": "array",
                        "items": {"type": "string", "enum": ["push", "email"]},
                        "description": "Where the answer goes. One channel per call; omit for the operator's default."
                    }
                },
                "required": ["name", "cron", "prompt"]
            }),
        },
        ToolSpec {
            name: "schedule_list".to_owned(),
            description: "Every schedule this daemon holds, with when it next fires.".to_owned(),
            parameters: json!({"type": "object", "properties": {}, "required": []}),
        },
        ToolSpec {
            name: "schedule_cancel".to_owned(),
            description: "Delete one schedule by id. Returns the name it removed."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "The schedule id from schedule_list."}
                },
                "required": ["id"]
            }),
        },
    ]
}

/// Run one call. `None` means this module does not own the name.
pub fn run(name: &str, arguments: &Value, db: &Db) -> Option<ToolResult> {
    match name {
        "schedule_run" => Some(schedule_run(arguments, db)),
        "schedule_list" => Some(schedule_list(db)),
        "schedule_cancel" => Some(schedule_cancel(arguments, db)),
        _ => None,
    }
}

fn schedule_run(arguments: &Value, db: &Db) -> ToolResult {
    if arguments.get("at").is_some() {
        return ToolResult::refused(
            "one-shot `at` times are not supported: this daemon has no lifecycle that turns a \
             schedule off after it fires. Give a `cron` expression instead.",
        );
    }
    let name = arguments.get("name").and_then(Value::as_str).unwrap_or("").trim();
    let cron = arguments.get("cron").and_then(Value::as_str).unwrap_or("").trim();
    let prompt = arguments.get("prompt").and_then(Value::as_str).unwrap_or("").trim();
    if name.is_empty() || cron.is_empty() || prompt.is_empty() {
        return ToolResult::refused("schedule_run needs a `name`, a `cron` and a `prompt`");
    }
    let offset = time::UtcOffset::UTC;
    match scheduler::create(db, name, cron, prompt, offset) {
        Ok(id) => {
            let delivery = arguments
                .get("deliver")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .filter(|joined| !joined.is_empty());
            if let Some(channels) = delivery {
                let channel = if channels.contains("push") && channels.contains("email") {
                    "both"
                } else if channels.contains("email") {
                    "email"
                } else {
                    "push"
                };
                if let Err(error) = scheduler::set_deliver(db, &id, channel) {
                    return ToolResult::refused(&format!(
                        "the schedule was created but its delivery could not be set: {error}"
                    ));
                }
            }
            ToolResult::ok(json!({"id": id, "name": name, "cron": cron}))
        }
        Err(error) => ToolResult::refused(&format!("the schedule was not created: {error}")),
    }
}

fn schedule_list(db: &Db) -> ToolResult {
    match scheduler::list(db) {
        Ok(rows) => ToolResult::ok(json!({
            "schedules": rows.iter().map(|row| json!({
                "id": row.id,
                "name": row.name,
                "cron": row.cron,
                "deliver": row.deliver,
                "enabled": row.enabled,
                "next_fire_at": row.next_fire_at,
            })).collect::<Vec<_>>(),
        })),
        Err(error) => ToolResult::refused(&format!("the schedules could not be read: {error}")),
    }
}

fn schedule_cancel(arguments: &Value, db: &Db) -> ToolResult {
    let Some(id) = arguments.get("id").and_then(Value::as_str) else {
        return ToolResult::refused("schedule_cancel needs an `id`");
    };
    match scheduler::get(db, id) {
        Ok(Some(row)) => match scheduler::delete(db, id) {
            Ok(()) => ToolResult::ok(json!({"removed": row.name, "id": id})),
            Err(error) => ToolResult::refused(&format!("the schedule was not removed: {error}")),
        },
        Ok(None) => ToolResult::refused("no schedule with that id exists"),
        Err(error) => ToolResult::refused(&format!("the schedule could not be read: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{run, specs};
    use crate::db::Db;

    /// Three tools, and the run tool refuses an `at` time by name.
    #[test]
    fn a_one_shot_time_is_refused_with_the_reason() {
        let db = Db::open_in_memory().expect("database");
        let refused = run(
            "schedule_run",
            &serde_json::json!({"name": "x", "at": "2026-12-01T08:00:00Z", "prompt": "hi"}),
            &db,
        )
        .expect("owned");
        assert!(refused.is_error);
        assert!(refused.content.contains("cron"), "{}", refused.content);
    }

    /// A created schedule is listed with its delivery channel, and cancelled by id.
    #[test]
    fn a_schedule_is_created_listed_and_cancelled() {
        let db = Db::open_in_memory().expect("database");
        let created = run(
            "schedule_run",
            &serde_json::json!({
                "name": "morning",
                "cron": "30 6 * * 1-5",
                "prompt": "What matters today?",
                "deliver": ["push"]
            }),
            &db,
        )
        .expect("owned");
        assert!(!created.is_error, "{}", created.content);
        let listed = run("schedule_list", &serde_json::json!({}), &db).expect("owned");
        assert!(listed.content.contains("morning"), "{}", listed.content);
        let rows = crate::scheduler::list(&db).expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].deliver, "push");
        let cancelled = run(
            "schedule_cancel",
            &serde_json::json!({"id": rows[0].id}),
            &db,
        )
        .expect("owned");
        assert!(!cancelled.is_error, "{}", cancelled.content);
        assert!(crate::scheduler::list(&db).expect("list").is_empty());
    }

    #[test]
    fn the_specs_are_the_three_tools() {
        assert_eq!(specs().len(), 3);
    }
}
