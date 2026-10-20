//! MCP servers: the configuration, not the client.
//!
//! This build has no MCP client. It cannot spawn a server, speak JSON-RPC to it,
//! or offer its tools to a model. What it can do is keep the list of servers the
//! operator wants, validated, so the configuration is not re-typed the day a
//! client lands - and say plainly, in the place the operator edits it, that
//! nothing runs it yet.
//!
//! Storing configuration ahead of the feature is only honest when the surface
//! admits it. That is what [`STATUS`] exists for.

use serde::{Deserialize, Serialize};

/// The `settings` row that holds the server list, as JSON.
pub const SETTING_SERVERS: &str = "mcp.servers";

/// The sentence the settings page states above the editor.
pub const STATUS: &str = "Configured only. This build has no MCP client, so no server is started and no tool \
     from one is offered to a model. The list is kept so the day a client lands it has \
     something to read.";

/// The most servers the list may hold.
pub const MAX_SERVERS: usize = 32;

/// One configured server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Server {
    /// A name the operator will recognize.
    pub name: String,
    /// The executable to run.
    pub command: String,
    /// Its arguments, in order.
    #[serde(default)]
    pub args: Vec<String>,
}

/// Why a submitted list was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpError {
    /// What the operator should fix.
    pub message: String,
}

/// Parse and validate the stored or submitted list.
///
/// An empty document is an empty list, which is the default. Everything else
/// must be a JSON array of objects with a name and a command; a duplicate name is
/// refused rather than silently merged.
///
/// # Errors
///
/// [`McpError`] naming the problem.
pub fn parse(raw: &str) -> Result<Vec<Server>, McpError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let servers: Vec<Server> = serde_json::from_str(trimmed).map_err(|error| McpError {
        message: format!(
            "the server list must be a JSON array of {{\"name\", \"command\", \"args\"}} objects: {error}"
        ),
    })?;
    if servers.len() > MAX_SERVERS {
        return Err(McpError {
            message: format!(
                "{} servers is more than the {MAX_SERVERS} this build will store",
                servers.len()
            ),
        });
    }
    let mut seen: Vec<&str> = Vec::new();
    for server in &servers {
        if server.name.trim().is_empty() {
            return Err(McpError {
                message: "every server needs a name".to_owned(),
            });
        }
        if server.command.trim().is_empty() {
            return Err(McpError {
                message: format!("{} has no command", server.name),
            });
        }
        if seen.contains(&server.name.as_str()) {
            return Err(McpError {
                message: format!("two servers are called {}", server.name),
            });
        }
        seen.push(&server.name);
    }
    Ok(servers)
}

/// Render a list for the editor.
#[must_use]
pub fn to_json(servers: &[Server]) -> String {
    if servers.is_empty() {
        return "[]".to_owned();
    }
    serde_json::to_string_pretty(servers).unwrap_or_else(|_| "[]".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_document_is_an_empty_list() {
        assert_eq!(parse("").expect("empty is fine"), Vec::new());
        assert_eq!(parse("   ").expect("blank is fine"), Vec::new());
    }

    #[test]
    fn a_valid_list_round_trips() {
        let raw = r#"[{"name":"vault","command":"mcp-vault","args":["--root","/tmp"]}]"#;
        let servers = parse(raw).expect("valid");
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "vault");
        assert_eq!(servers[0].args, vec!["--root", "/tmp"]);

        let again = parse(&to_json(&servers)).expect("round trip");
        assert_eq!(again, servers);
    }

    #[test]
    fn a_missing_name_or_command_is_refused_by_name() {
        let no_name = parse(r#"[{"name":"  ","command":"x"}]"#).unwrap_err();
        assert!(no_name.message.contains("name"), "{}", no_name.message);

        let no_command = parse(r#"[{"name":"vault","command":""}]"#).unwrap_err();
        assert!(
            no_command.message.contains("vault"),
            "{}",
            no_command.message
        );
    }

    #[test]
    fn duplicate_names_are_refused_rather_than_merged() {
        let error =
            parse(r#"[{"name":"a","command":"x"},{"name":"a","command":"y"}]"#).unwrap_err();
        assert!(error.message.contains('a'), "{}", error.message);
    }

    #[test]
    fn junk_is_refused_with_a_message_that_says_what_it_wanted() {
        let error = parse("not json").unwrap_err();
        assert!(error.message.contains("JSON array"), "{}", error.message);
        let error = parse(r#"{"name":"one"}"#).unwrap_err();
        assert!(error.message.contains("JSON array"), "{}", error.message);
    }
}
