//! The wire: JSON-RPC 2.0 over newline-delimited JSON.
//!
//! MCP speaks JSON-RPC. Over stdio the framing is one JSON object per line, so
//! this module is deliberately small and synchronous: encode a request, decode a
//! reply, and turn a server's own vocabulary into the shapes Kallisti already
//! understands.
//!
//! Nothing here spawns or decides. That is [`super::client`], so a protocol bug
//! can be found without a process, a pipe, or a timeout.

use serde_json::{Value, json};

/// The protocol revision this client speaks.
///
/// Pinned rather than negotiated upward: a client that claims the newest version
/// it has heard of can hand a server a shape it does not implement. The server
/// answers with the version it will use, and that answer is recorded.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// One request to encode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The JSON-RPC id. Unique per client, monotonic.
    pub id: u64,
    /// The method name.
    pub method: String,
    /// Its parameters, or `Value::Null` for none.
    pub params: Value,
}

impl Request {
    /// The line to write to the child's stdin.
    #[must_use]
    pub fn encode(&self) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": self.id,
            "method": self.method,
            "params": self.params,
        })
        .to_string()
    }
}

/// A notification: a request with no id and no reply.
#[must_use]
pub fn notification(method: &str) -> String {
    json!({ "jsonrpc": "2.0", "method": method }).to_string()
}

/// The `initialize` parameters, with the client's own identity.
#[must_use]
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {},
        "clientInfo": { "name": "kallisti", "version": crate::admin::VERSION },
    })
}

/// What a reply line turned into.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// A result for the request that was waiting.
    Result(Value),
    /// A JSON-RPC error object.
    Error {
        /// The numeric code.
        code: i64,
        /// The server's message.
        message: String,
    },
    /// A request or notification from the server, which this client does not
    /// answer. Kept distinct so it is skipped rather than mistaken for a reply.
    Other,
}

/// Decode one line, expecting the reply to `id`.
///
/// A line whose `id` is absent is `Other`, not an error: servers are allowed to
/// send logs and notifications between replies, and a client that treated those
/// as failures would fall over on a chatty server.
#[must_use]
pub fn decode(line: &str, id: u64) -> Reply {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return Reply::Other;
    };
    let Some(reply_id) = value.get("id").and_then(Value::as_u64) else {
        return Reply::Other;
    };
    if reply_id != id {
        return Reply::Other;
    }
    if let Some(error) = value.get("error") {
        return Reply::Error {
            code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the server sent an error with no message")
                .to_owned(),
        };
    }
    match value.get("result") {
        Some(result) => Reply::Result(result.clone()),
        None => Reply::Other,
    }
}

/// A tool as a server advertises it.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerTool {
    /// The name the server knows.
    pub name: String,
    /// What it does, when the server says.
    pub description: String,
    /// The JSON Schema for its arguments.
    pub input_schema: Value,
}

/// Read the tool list out of a `tools/list` result.
///
/// A tool with no name is dropped rather than offered under an empty label: a
/// name is the only way to call it back, so a nameless tool is unusable.
#[must_use]
pub fn tools_from(result: &Value) -> Vec<ServerTool> {
    let Some(items) = result.get("tools").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|tool| {
            let name = tool.get("name")?.as_str()?.trim().to_owned();
            if name.is_empty() {
                return None;
            }
            Some(ServerTool {
                name,
                description: tool
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                input_schema: tool
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({ "type": "object" })),
            })
        })
        .collect()
}

/// What a `tools/call` result means to us.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallOutcome {
    /// The text the server produced, concatenated in order.
    pub text: String,
    /// True when the server marked the call as failed.
    pub is_error: bool,
}

/// Read a `tools/call` result.
///
/// Content is an array of typed blocks. Text blocks are concatenated; anything
/// else is named by type rather than dropped silently, because "the server
/// returned an image" is information and an empty string is not.
#[must_use]
pub fn outcome_from(result: &Value) -> CallOutcome {
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut parts: Vec<String> = Vec::new();
    if let Some(items) = result.get("content").and_then(Value::as_array) {
        for item in items {
            let kind = item
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            if kind == "text"
                && let Some(text) = item.get("text").and_then(Value::as_str)
            {
                parts.push(text.to_owned());
                continue;
            }
            parts.push(format!("[{kind} content omitted]"));
        }
    }
    if parts.is_empty()
        && let Some(text) = result.get("content").and_then(Value::as_str)
    {
        parts.push(text.to_owned());
    }
    CallOutcome {
        text: parts.join("\n"),
        is_error,
    }
}

/// The name a tool is offered under: `server__tool`.
///
/// Double underscore because a single one is common inside tool names, and
/// because the split has to be unambiguous when the call comes back.
#[must_use]
pub fn qualified(server: &str, tool: &str) -> String {
    format!("{server}__{tool}")
}

/// Split a qualified name back into (server, tool).
///
/// Splits on the first `__`, so a tool whose own name contains `__` survives.
#[must_use]
pub fn split_qualified(name: &str) -> Option<(String, String)> {
    let (server, tool) = name.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_owned(), tool.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_encodes_to_one_line_of_json_rpc() {
        let line = Request {
            id: 7,
            method: "tools/list".to_owned(),
            params: json!({}),
        }
        .encode();
        let value: Value = serde_json::from_str(&line).expect("json");
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 7);
        assert_eq!(value["method"], "tools/list");
        assert!(!line.contains('\n'), "one object, one line");
    }

    #[test]
    fn a_reply_is_matched_by_id_and_anything_else_is_skipped() {
        assert_eq!(
            decode(r#"{"jsonrpc":"2.0","id":3,"result":{"ok":true}}"#, 3),
            Reply::Result(json!({"ok": true}))
        );
        assert_eq!(
            decode(r#"{"jsonrpc":"2.0","id":4,"result":{}}"#, 3),
            Reply::Other,
            "a reply to another request is not ours"
        );
        assert_eq!(
            decode(r#"{"jsonrpc":"2.0","method":"notifications/progress"}"#, 3),
            Reply::Other,
            "a notification between replies is not a failure"
        );
        assert_eq!(decode("not json at all", 3), Reply::Other);
    }

    #[test]
    fn a_server_error_carries_its_code_and_message() {
        let reply = decode(
            r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no such method"}}"#,
            1,
        );
        assert_eq!(
            reply,
            Reply::Error {
                code: -32601,
                message: "no such method".to_owned()
            }
        );
    }

    #[test]
    fn a_tool_list_keeps_names_and_drops_the_nameless() {
        let result = json!({
            "tools": [
                {"name": "search", "description": "Find things", "inputSchema": {"type": "object"}},
                {"description": "no name at all"},
                {"name": "   "},
                {"name": "fetch"}
            ]
        });
        let tools = tools_from(&result);
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "search");
        assert_eq!(tools[0].description, "Find things");
        assert_eq!(
            tools[1].input_schema,
            json!({"type": "object"}),
            "a server that omits the schema still gets a usable one"
        );
    }

    #[test]
    fn a_call_result_concatenates_text_and_names_what_it_cannot_show() {
        let result = json!({
            "content": [
                {"type": "text", "text": "first"},
                {"type": "image", "data": "..."},
                {"type": "text", "text": "second"}
            ]
        });
        let outcome = outcome_from(&result);
        assert!(outcome.text.contains("first"));
        assert!(outcome.text.contains("second"));
        assert!(
            outcome.text.contains("[image content omitted]"),
            "unshown content is named: {}",
            outcome.text
        );
        assert!(!outcome.is_error);
    }

    #[test]
    fn a_server_marked_failure_stays_a_failure() {
        let outcome = outcome_from(&json!({
            "content": [{"type": "text", "text": "it broke"}],
            "isError": true
        }));
        assert!(outcome.is_error);
        assert_eq!(outcome.text, "it broke");
    }

    #[test]
    fn a_qualified_name_round_trips_even_when_the_tool_has_underscores() {
        let name = qualified("vault", "read__note");
        assert_eq!(name, "vault__read__note");
        assert_eq!(
            split_qualified(&name),
            Some(("vault".to_owned(), "read__note".to_owned()))
        );
        assert_eq!(split_qualified("nodashes"), None);
    }
}
