//! The six model-facing steward tools.
//!
//! They are offered only while `steward.enabled` is on and a database is in hand,
//! and dispatched from the turn that offered them, the way `budget` and `queue_task`
//! are. A refusal is a tool result the model can read and act on, not an error.
//! Duty mutations are compare-and-set: a stale revision comes back naming the
//! current revision, and the model re-reads the duty before it retries.

use core_runtime::provider::ToolSpec;

use crate::db::Db;
use crate::steward::{self, DutyError, DutyPatch, Phase};
use crate::tools::ToolResult;

/// Seed duties from a written responsibility.
pub const CHARTER: &str = "steward_charter";
/// Read every duty with its revision and next wake.
pub const DUTY_LIST: &str = "steward_duty_list";
/// Compare-and-set change to one duty.
pub const DUTY_UPDATE: &str = "steward_duty_update";
/// Close one duty. Terminal.
pub const DUTY_CLOSE: &str = "steward_duty_close";
/// Raise one decision for a person.
pub const CALL: &str = "steward_call";
/// Append one field note.
pub const NOTE: &str = "steward_note";

/// Every steward tool name, in the order they are offered.
pub const NAMES: [&str; 6] = [CHARTER, DUTY_LIST, DUTY_UPDATE, DUTY_CLOSE, CALL, NOTE];

/// Whether a tool name belongs to the steward.
#[must_use]
pub fn is_steward_tool(name: &str) -> bool {
    NAMES.contains(&name)
}

const CAS: &str = "Duty mutations are compare-and-set: pass the revision you read. A stale-revision \
    error means the duty changed since you read it, so re-read it with steward_duty_list and retry \
    at the revision it reports.";

/// The six specs, offered together.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: CHARTER.to_owned(),
            description: format!(
                "Seed duties from a written responsibility: one duty per non-empty line of `text`. \
                 A line that already names an open duty is skipped. Returns what it created. {CAS}"
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string", "description": "The responsibility, one duty per line."}
                },
                "required": ["text"]
            }),
        },
        ToolSpec {
            name: DUTY_LIST.to_owned(),
            description: format!(
                "Read the steward's duties with their phase, revision and next wake, optionally \
                 narrowed to one phase. {CAS}"
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "phase": {"type": "string", "enum": ["watching", "working", "waiting", "needs_you", "paused", "closed"]}
                },
                "required": []
            }),
        },
        ToolSpec {
            name: DUTY_UPDATE.to_owned(),
            description: format!(
                "Change one duty: phase, statement, cadence, next wake, or a blocker on a paused \
                 duty. Pass `revision`, the revision you read. A duty in needs_you cannot be moved \
                 by you: a person answers the call first. {CAS}"
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "duty_id": {"type": "string"},
                    "revision": {"type": "integer", "minimum": 1},
                    "phase": {"type": "string", "enum": ["watching", "working", "waiting", "paused"]},
                    "statement": {"type": "string"},
                    "every_seconds": {"type": "integer", "minimum": 30, "maximum": 86400},
                    "blocker_code": {"type": "string"},
                    "blocker_detail": {"type": "string"}
                },
                "required": ["duty_id", "revision"]
            }),
        },
        ToolSpec {
            name: DUTY_CLOSE.to_owned(),
            description: format!(
                "Close one duty with its outcome in one line. Closing is terminal: a closed duty \
                 takes no further write and no wake. {CAS}"
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "duty_id": {"type": "string"},
                    "revision": {"type": "integer", "minimum": 1},
                    "outcome": {"type": "string"}
                },
                "required": ["duty_id", "revision", "outcome"]
            }),
        },
        ToolSpec {
            name: CALL.to_owned(),
            description: "Raise one decision only a person can make. The duty moves to needs_you \
                          and stops waking; the question reaches the person on the next watch pass. \
                          One open call per duty."
                .to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "duty_id": {"type": "string"},
                    "revision": {"type": "integer", "minimum": 1},
                    "question": {"type": "string"},
                    "choices": {"type": "array", "items": {"type": "string"}, "maxItems": 5},
                    "urgency": {"type": "string", "enum": ["normal", "high"]}
                },
                "required": ["duty_id", "revision", "question"]
            }),
        },
        ToolSpec {
            name: NOTE.to_owned(),
            description: "Append one field note: your scratch memory for a duty, or for the steward \
                          as a whole when no duty id is given. Notes are append-only; the most \
                          recent ones are quoted back to you on each watch round."
                .to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "text": {"type": "string"},
                    "duty_id": {"type": "string"},
                    "tags": {"type": "array", "items": {"type": "string"}, "maxItems": 8}
                },
                "required": ["text"]
            }),
        },
    ]
}

fn text_arg<'a>(arguments: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    arguments.get(key).and_then(serde_json::Value::as_str)
}

fn int_arg(arguments: &serde_json::Value, key: &str) -> Option<i64> {
    arguments.get(key).and_then(serde_json::Value::as_i64)
}

fn string_list(arguments: &serde_json::Value, key: &str) -> Vec<String> {
    arguments
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

fn refusal(error: &DutyError) -> ToolResult {
    match error {
        DutyError::StaleRevision { current } => ToolResult::refused(&format!(
            "stale revision: the duty is at revision {current}. Re-read it with steward_duty_list, \
             then retry at revision {current}."
        )),
        other => ToolResult::refused(&other.to_string()),
    }
}

/// Run one steward tool call.
#[must_use]
pub fn execute(db: &Db, name: &str, arguments: &serde_json::Value) -> ToolResult {
    if !steward::enabled(db) {
        return ToolResult::refused("the steward is switched off in settings, so nothing was changed");
    }
    match name {
        CHARTER => {
            let text = text_arg(arguments, "text").unwrap_or_default();
            match steward::seed_from_text(db, text) {
                Ok(seeded) => ToolResult::ok(serde_json::json!({
                    "created": seeded.created.iter().map(steward::Duty::json).collect::<Vec<_>>(),
                    "skipped": seeded.skipped,
                    "refused": seeded.refused,
                })),
                Err(error) => refusal(&error),
            }
        }
        DUTY_LIST => {
            let phase = match text_arg(arguments, "phase") {
                None => None,
                Some(raw) => match raw.parse::<Phase>() {
                    Ok(phase) => Some(phase),
                    Err(error) => return refusal(&error),
                },
            };
            match steward::list_duties(db, phase) {
                Ok(duties) => ToolResult::ok(serde_json::json!({
                    "count": duties.len(),
                    "duties": duties.iter().map(steward::Duty::json).collect::<Vec<_>>(),
                })),
                Err(error) => refusal(&error),
            }
        }
        DUTY_UPDATE => {
            let Some(id) = text_arg(arguments, "duty_id") else {
                return ToolResult::refused("steward_duty_update needs duty_id");
            };
            let Some(revision) = int_arg(arguments, "revision") else {
                return ToolResult::refused("steward_duty_update needs the revision you read");
            };
            let phase = match text_arg(arguments, "phase") {
                None => None,
                Some(raw) => match raw.parse::<Phase>() {
                    Ok(phase) => Some(phase),
                    Err(error) => return refusal(&error),
                },
            };
            let blocker = match (text_arg(arguments, "blocker_code"), text_arg(arguments, "blocker_detail")) {
                (None, None) => None,
                (code, detail) => Some(Some(steward::Blocker {
                    code: code.unwrap_or_default().to_owned(),
                    detail: detail.unwrap_or_default().to_owned(),
                })),
            };
            let patch = DutyPatch {
                statement: text_arg(arguments, "statement").map(str::to_owned),
                phase,
                every_seconds: int_arg(arguments, "every_seconds").map(Some),
                blocker,
                ..DutyPatch::default()
            };
            match steward::update_duty(db, id, revision, &patch) {
                Ok(duty) => ToolResult::ok(serde_json::json!({"duty": duty.json()})),
                Err(error) => refusal(&error),
            }
        }
        DUTY_CLOSE => {
            let Some(id) = text_arg(arguments, "duty_id") else {
                return ToolResult::refused("steward_duty_close needs duty_id");
            };
            let Some(revision) = int_arg(arguments, "revision") else {
                return ToolResult::refused("steward_duty_close needs the revision you read");
            };
            match steward::close_duty(db, id, revision, text_arg(arguments, "outcome").unwrap_or_default()) {
                Ok(duty) => ToolResult::ok(serde_json::json!({"duty": duty.json()})),
                Err(error) => refusal(&error),
            }
        }
        CALL => {
            let Some(id) = text_arg(arguments, "duty_id") else {
                return ToolResult::refused("steward_call needs duty_id");
            };
            let Some(revision) = int_arg(arguments, "revision") else {
                return ToolResult::refused("steward_call needs the revision you read");
            };
            let urgency = text_arg(arguments, "urgency").unwrap_or("normal");
            match steward::raise_call(
                db,
                id,
                revision,
                text_arg(arguments, "question").unwrap_or_default(),
                &string_list(arguments, "choices"),
                urgency,
            ) {
                Ok(duty) => ToolResult::ok(serde_json::json!({
                    "duty": duty.json(),
                    "delivery": "the question reaches the person on the next watch pass",
                })),
                Err(error) => refusal(&error),
            }
        }
        NOTE => {
            let tags = string_list(arguments, "tags");
            match steward::append_note(
                db,
                text_arg(arguments, "duty_id"),
                text_arg(arguments, "text").unwrap_or_default(),
                &tags,
            ) {
                Ok(note) => ToolResult::ok(serde_json::json!({"note": note.json()})),
                Err(error) => refusal(&error),
            }
        }
        other => ToolResult::refused(&format!("`{other}` is not a steward tool")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("an in-memory database")
    }

    #[test]
    fn six_tools_with_the_exact_names_and_an_object_schema_each() {
        let specs = specs();
        let names: Vec<&str> = specs.iter().map(|spec| spec.name.as_str()).collect();
        assert_eq!(names, NAMES.to_vec());
        for spec in &specs {
            assert_eq!(spec.parameters["type"], "object", "{}", spec.name);
            assert!(spec.description.len() > 20, "{}", spec.name);
        }
        let cas: Vec<&ToolSpec> = specs
            .iter()
            .filter(|spec| matches!(spec.name.as_str(), DUTY_UPDATE | DUTY_CLOSE))
            .collect();
        assert_eq!(cas.len(), 2);
        assert!(cas.iter().all(|spec| spec.description.contains("compare-and-set")));
        assert!(cas.iter().all(|spec| spec.description.contains("re-read")));
    }

    #[test]
    fn a_stale_update_is_refused_and_names_the_current_revision() {
        let db = db();
        let duty = steward::create_duty(&db, "watch the queue", None).expect("create");
        let first = execute(&db, DUTY_UPDATE, &serde_json::json!({"duty_id": duty.id, "revision": 1, "phase": "working"}));
        assert!(!first.is_error, "{}", first.content);
        let stale = execute(&db, DUTY_UPDATE, &serde_json::json!({"duty_id": duty.id, "revision": 1, "phase": "paused"}));
        assert!(stale.content.contains("\"status\":\"refused\""), "{}", stale.content);
        assert!(stale.content.contains("revision 2"), "{}", stale.content);
        assert!(stale.content.contains("Re-read"), "{}", stale.content);
    }

    #[test]
    fn a_model_cannot_clear_an_open_call_or_close_into_needs_you() {
        let db = db();
        let duty = steward::create_duty(&db, "keep the lights on", None).expect("create");
        let call = execute(
            &db,
            CALL,
            &serde_json::json!({"duty_id": duty.id, "revision": 1, "question": "Pay the bill?", "choices": ["yes", "no"]}),
        );
        assert!(!call.is_error, "{}", call.content);
        let clear = execute(&db, DUTY_UPDATE, &serde_json::json!({"duty_id": duty.id, "revision": 2, "phase": "working"}));
        assert!(clear.content.contains("\"status\":\"refused\""), "{}", clear.content);
        let direct = execute(&db, DUTY_UPDATE, &serde_json::json!({"duty_id": duty.id, "revision": 2, "phase": "needs_you"}));
        assert!(direct.content.contains("only set through a call"), "{}", direct.content);
    }

    #[test]
    fn the_charter_tool_seeds_one_duty_per_line_once() {
        let db = db();
        let first = execute(&db, CHARTER, &serde_json::json!({"text": "Watch backups\n\nChase vendor\n"}));
        assert!(first.content.contains("Watch backups"), "{}", first.content);
        let again = execute(&db, CHARTER, &serde_json::json!({"text": "Watch backups\nChase vendor"}));
        let value: serde_json::Value = serde_json::from_str(&again.content).expect("json");
        assert_eq!(value["created"].as_array().map_or(0, Vec::len), 0, "a second seed adds nothing");
        assert_eq!(steward::list_duties(&db, None).expect("list").len(), 2);
    }

    #[test]
    fn the_tools_refuse_while_the_steward_is_off() {
        let db = db();
        crate::web::store::settings_put(&db, steward::SETTING_ENABLED, "false").expect("store");
        let result = execute(&db, DUTY_LIST, &serde_json::json!({}));
        assert!(result.content.contains("switched off"), "{}", result.content);
    }
}
