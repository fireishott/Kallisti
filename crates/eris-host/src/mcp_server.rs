//! Eris as an MCP server, so another agent can ask it things.
//!
//! The protocol is JSON-RPC over one HTTP endpoint: `initialize`, `tools/list`,
//! `tools/call`, and `notifications/initialized` which has no reply. Nothing is
//! exposed that a caller could not already do with a token: the tools here are
//! reads, and the one that searches the transcript calls the same function the
//! in-process tool does.
//!
//! This is deliberately not a general RPC: an MCP client is another agent on this
//! host, and giving it `run_command` over a protocol designed for tool discovery
//! would be a shell with a friendly handshake.

use serde_json::{Value, json};

use crate::db::Db;

/// The protocol version this server speaks.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// The tools this server offers. Read-only by construction.
pub const TOOLS: [&str; 2] = ["search_transcripts", "daemon_status"];

/// One JSON-RPC message in, one out. `None` means the message needs no reply -
/// a notification, or an id we do not answer.
#[must_use]
pub fn handle(db: &Db, body: &Value) -> Option<Value> {
    let id = body.get("id").cloned();
    let method = body.get("method").and_then(Value::as_str).unwrap_or_default();
    // A notification has no id and never gets a response, per JSON-RPC.
    let id = id?;
    let reply = |result: Value| {
        json!({"jsonrpc": "2.0", "id": id, "result": result})
    };
    let error = |code: i64, message: String| {
        json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
    };
    match method {
        "initialize" => Some(reply(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "eris", "version": env!("CARGO_PKG_VERSION")},
        }))),
        "tools/list" => Some(reply(json!({
            "tools": [
                {
                    "name": "search_transcripts",
                    "description": "Search every past conversation for the words given.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "query": {"type": "string"},
                            "limit": {"type": "integer"}
                        },
                        "required": ["query"]
                    }
                },
                {
                    "name": "daemon_status",
                    "description": "The daemon's version and whether its database is readable.",
                    "inputSchema": {"type": "object", "properties": {}, "required": []}
                }
            ]
        }))),
        "tools/call" => {
            let params = body.get("params").cloned().unwrap_or(Value::Null);
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
            let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
            match name {
                "search_transcripts" => {
                    let result = crate::search::transcript_tool(db, &arguments);
                    Some(reply(call_result(&result.content, result.is_error)))
                }
                "daemon_status" => Some(reply(call_result(
                    &json!({"version": env!("CARGO_PKG_VERSION"), "db": "ok"}).to_string(),
                    false,
                ))),
                other => Some(error(-32602, format!("no tool named `{other}`"))),
            }
        }
        other => Some(error(-32601, format!("no method `{other}`"))),
    }
}

/// The MCP result shape for one call.
fn call_result(text: &str, is_error: bool) -> Value {
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": is_error,
    })
}

#[cfg(test)]
mod tests {
    use super::{PROTOCOL_VERSION, TOOLS, handle};
    use crate::db::Db;

    fn db() -> Db {
        Db::open_in_memory().expect("database")
    }

    /// initialize answers with the protocol version and a tools capability, and
    /// a notification gets no reply at all.
    #[test]
    fn initialize_answers_and_a_notification_does_not() {
        let body = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"});
        let reply = handle(&db(), &body).expect("a reply");
        assert_eq!(reply["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert!(reply["result"]["capabilities"]["tools"].is_object());

        let note = serde_json::json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        assert!(handle(&db(), &note).is_none(), "a notification is not answered");
    }

    /// Every advertised tool is callable, and an unknown one is a JSON-RPC error
    /// rather than a silent empty result.
    #[test]
    fn the_tools_are_listed_and_unknown_ones_are_refused() {
        let list = handle(
            &db(),
            &serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
        )
        .expect("a reply");
        let names: Vec<String> = list["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .map(|tool| tool["name"].as_str().unwrap_or_default().to_owned())
            .collect();
        assert_eq!(names.len(), TOOLS.len());
        assert!(names.contains(&"search_transcripts".to_owned()));

        let call = handle(
            &db(),
            &serde_json::json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": {"name": "run_command", "arguments": {}}
            }),
        )
        .expect("a reply");
        assert_eq!(call["error"]["code"], -32602, "an unknown tool is refused");
    }

    /// A real call answers in the MCP content shape.
    #[test]
    fn a_status_call_returns_content() {
        let call = handle(
            &db(),
            &serde_json::json!({
                "jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": {"name": "daemon_status"}
            }),
        )
        .expect("a reply");
        assert_eq!(call["result"]["content"][0]["type"], "text");
        assert!(call["result"]["content"][0]["text"].as_str().unwrap_or_default().contains("version"));
        assert_eq!(call["result"]["isError"], false);
    }
}
