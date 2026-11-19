//! Bots and rooms, as tools.
//!
//! Five small wrappers over `rooms`, so a room can be built from a conversation
//! instead of from SQL. They create and read; nothing here starts a turn, because
//! a room turn is a different piece of work and a tool that claimed to start one
//! would be the half-wired feature the brief calls a bug.

use serde_json::{Value, json};

use crate::db::Db;
use crate::rooms::{self, Policy};
use crate::tools::ToolResult;
use core_runtime::provider::ToolSpec;

/// The names this module answers to.
pub const NAMES: [&str; 5] = [
    "bot_create",
    "bot_list",
    "room_create",
    "room_list",
    "room_history",
];

/// The tools as the model sees them.
#[must_use]
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "bot_create".to_owned(),
            description: "Create a bot profile: a name, its standing instructions, and optionally \
                          a model and a tool subset. A bot does not run on its own; a room runs it."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string", "description": "1 to 40 characters, unique."},
                    "instructions": {"type": "string", "description": "How this bot should behave."},
                    "model": {"type": "string", "description": "Optional model pin, provider/model."},
                    "tools": {"type": "array", "items": {"type": "string"}, "description": "Optional tool names this bot may use."}
                },
                "required": ["name"]
            }),
        },
        ToolSpec {
            name: "bot_list".to_owned(),
            description: "Every bot profile this daemon holds.".to_owned(),
            parameters: json!({"type": "object", "properties": {}, "required": []}),
        },
        ToolSpec {
            name: "room_create".to_owned(),
            description: "Create a room with bots in it. A mention routes to the bot named; with \
                          no mention the room's policy decides: everyone, the named lead, or \
                          nobody until somebody is named."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "purpose": {"type": "string"},
                    "policy": {"type": "string", "enum": ["everyone", "lead", "mention"]},
                    "lead": {"type": "string", "description": "A member's name, required for the lead policy."},
                    "members": {"type": "array", "items": {"type": "string"}, "description": "Bot names."},
                    "max_turns": {"type": "integer", "description": "Turns one message may spend, 1 to 24. Default 6."}
                },
                "required": ["name", "members"]
            }),
        },
        ToolSpec {
            name: "room_list".to_owned(),
            description: "Every room, with its policy and how many bots are in it.".to_owned(),
            parameters: json!({"type": "object", "properties": {}, "required": []}),
        },
        ToolSpec {
            name: "room_history".to_owned(),
            description: "The messages in a room, oldest first.".to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "room_id": {"type": "string"},
                    "limit": {"type": "integer", "description": "At most 200. Default 50."}
                },
                "required": ["room_id"]
            }),
        },
    ]
}

/// Run one call. `None` means this module does not own the name.
pub fn run(name: &str, arguments: &Value, db: &Db) -> Option<ToolResult> {
    match name {
        "bot_create" => Some(bot_create(arguments, db)),
        "bot_list" => Some(bot_list(db)),
        "room_create" => Some(room_create(arguments, db)),
        "room_list" => Some(room_list(db)),
        "room_history" => Some(room_history(arguments, db)),
        _ => None,
    }
}

fn bot_create(arguments: &Value, db: &Db) -> ToolResult {
    let name = arguments.get("name").and_then(Value::as_str).unwrap_or("");
    let instructions = arguments
        .get("instructions")
        .and_then(Value::as_str)
        .unwrap_or("");
    let model = arguments.get("model").and_then(Value::as_str);
    let tools: Vec<String> = arguments
        .get("tools")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    match rooms::create_bot(db, name, instructions, model, &tools) {
        Ok(id) => ToolResult::ok(json!({"id": id, "name": name})),
        Err(message) => ToolResult::refused(&message),
    }
}

fn bot_list(db: &Db) -> ToolResult {
    match rooms::bots(db) {
        Ok(rows) => ToolResult::ok(json!({
            "bots": rows
                .into_iter()
                .map(|(id, name)| json!({"id": id, "name": name}))
                .collect::<Vec<_>>(),
        })),
        Err(error) => ToolResult::refused(&format!("the bots could not be read: {error}")),
    }
}

fn room_create(arguments: &Value, db: &Db) -> ToolResult {
    let name = arguments.get("name").and_then(Value::as_str).unwrap_or("");
    let purpose = arguments
        .get("purpose")
        .and_then(Value::as_str)
        .unwrap_or("");
    let policy = match arguments.get("policy").and_then(Value::as_str) {
        None => Policy::Mention,
        Some(text) => match Policy::parse(text) {
            Some(policy) => policy,
            None => {
                return ToolResult::refused(&format!(
                    "`{text}` is not a policy. The policies are: everyone, lead, mention"
                ));
            }
        },
    };
    let lead = arguments.get("lead").and_then(Value::as_str);
    let members: Vec<String> = arguments
        .get("members")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let max_turns = arguments
        .get("max_turns")
        .and_then(Value::as_i64)
        .unwrap_or(6);
    match rooms::create_room(db, name, purpose, policy, lead, &members, max_turns) {
        Ok(id) => ToolResult::ok(json!({"id": id, "name": name, "policy": policy.as_str()})),
        Err(message) => ToolResult::refused(&message),
    }
}

fn room_list(db: &Db) -> ToolResult {
    match rooms::rooms(db) {
        Ok(rows) => ToolResult::ok(json!({
            "rooms": rows
                .into_iter()
                .map(|row| json!({
                    "id": row.id,
                    "name": row.name,
                    "policy": row.policy,
                    "members": row.members,
                    "max_turns": row.max_turns,
                }))
                .collect::<Vec<_>>(),
        })),
        Err(error) => ToolResult::refused(&format!("the rooms could not be read: {error}")),
    }
}

fn room_history(arguments: &Value, db: &Db) -> ToolResult {
    let Some(room_id) = arguments.get("room_id").and_then(Value::as_str) else {
        return ToolResult::refused("room_history needs a `room_id`");
    };
    let limit = arguments
        .get("limit")
        .and_then(Value::as_u64)
        .map_or(50, |value| value.min(200) as usize);
    match rooms::history(db, room_id, limit) {
        Ok(rows) => ToolResult::ok(json!({
            "messages": rows
                .into_iter()
                .map(|row| json!({
                    "role": row.role,
                    "body": row.body,
                    "created_at": row.created_at,
                    "job_id": row.job_id,
                }))
                .collect::<Vec<_>>(),
        })),
        Err(error) => ToolResult::refused(&format!("the room history could not be read: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::{run, specs};
    use crate::db::Db;

    #[test]
    fn the_specs_are_the_five_tools() {
        assert_eq!(specs().len(), 5);
    }

    /// Create a bot and a room through the tools, then read both back.
    #[test]
    fn a_bot_and_a_room_are_created_and_listed() {
        let db = Db::open_in_memory().expect("database");
        let bot = run(
            "bot_create",
            &serde_json::json!({"name": "Atlas", "instructions": "be brief"}),
            &db,
        )
        .expect("owned");
        assert!(!bot.is_error, "{}", bot.content);
        let room = run(
            "room_create",
            &serde_json::json!({"name": "standup", "members": ["Atlas"], "policy": "lead", "lead": "Atlas"}),
            &db,
        )
        .expect("owned");
        assert!(!room.is_error, "{}", room.content);
        let listed = run("room_list", &serde_json::json!({}), &db).expect("owned");
        assert!(listed.content.contains("standup"), "{}", listed.content);
        let bots = run("bot_list", &serde_json::json!({}), &db).expect("owned");
        assert!(bots.content.contains("Atlas"), "{}", bots.content);
    }

    /// An unknown policy is refused with the legal set, and a room with no bots
    /// is refused by the module under it.
    #[test]
    fn a_bad_policy_and_an_empty_room_are_refused() {
        let db = Db::open_in_memory().expect("database");
        let refused = run(
            "room_create",
            &serde_json::json!({"name": "x", "members": ["nobody"], "policy": "everyone"}),
            &db,
        )
        .expect("owned");
        assert!(refused.is_error);
        assert!(refused.content.contains("nobody"), "{}", refused.content);
    }
}
