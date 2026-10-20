//! MCP: the servers Kallisti runs, and the client that speaks to them.
//!
//! [`protocol`] is the wire. [`client`] owns one child process per configured
//! server and answers requests with a deadline. This file holds the stored list
//! and the pool the daemon keeps.
//!
//! This module used to say it had no client, and that was true then. It is not
//! true now: a server is started on demand, its tools are offered to a turn as
//! `server__tool`, and every call passes the approval gate. The stored list is no
//! longer configuration waiting for a feature - it is the feature's input.

pub mod client;
pub mod protocol;

use serde::{Deserialize, Serialize};

/// The `settings` row that holds the server list, as JSON.
pub const SETTING_SERVERS: &str = "mcp.servers";

/// The sentence the settings page states above the editor.
pub const STATUS: &str = "A server is started on demand, its tools are offered to a turn as \
     `server__tool`, and every call passes through the approval gate exactly like a \
     file write: an MCP server is code you installed, not a trusted helper.";

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

impl McpError {
    /// A transport, deadline or server failure, already named.
    #[must_use]
    pub fn client(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
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

/// The running servers, one child each, started on demand.
///
/// A server is started the first time a turn could use it, not at boot: a daemon
/// that spawns processes for a feature nobody asked for is a daemon that fails
/// health checks for no reason. Nothing is thrown away on a failed start either -
/// the error is kept, so the settings page can say which server is broken instead
/// of quietly offering no tools.
#[derive(Debug, Default)]
pub struct Pool {
    servers: Vec<Server>,
    inner: std::sync::Mutex<PoolState>,
}

#[derive(Debug, Default)]
struct PoolState {
    clients: std::collections::HashMap<String, client::Client>,
    /// The last failure per server, cleared on a successful start.
    errors: std::collections::HashMap<String, String>,
}

impl Pool {
    /// A pool over the configured servers.
    #[must_use]
    pub fn new(servers: Vec<Server>) -> Self {
        Self {
            servers,
            inner: std::sync::Mutex::new(PoolState::default()),
        }
    }

    /// The configured names, in order.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.servers
            .iter()
            .map(|server| server.name.clone())
            .collect()
    }

    /// The last start failure per server, for the settings page.
    #[must_use]
    pub fn errors(&self) -> Vec<(String, String)> {
        self.inner
            .lock()
            .map(|state| {
                let mut rows: Vec<(String, String)> = state
                    .errors
                    .iter()
                    .map(|(name, error)| (name.clone(), error.clone()))
                    .collect();
                rows.sort();
                rows
            })
            .unwrap_or_default()
    }

    /// Every tool from every server that started, namespaced `server__tool`.
    ///
    /// A server that cannot start contributes nothing and records why. A lock
    /// poisoned by a panic contributes nothing too: a turn must not die because a
    /// previous request did.
    #[must_use]
    pub fn specs(&self) -> Vec<core_runtime::provider::ToolSpec> {
        let mut out = Vec::new();
        let Ok(mut state) = self.inner.lock() else {
            return out;
        };
        for server in &self.servers {
            if let Err(error) = ensure_started(&mut state, server) {
                state.errors.insert(server.name.clone(), error.message);
                continue;
            }
            let Some(client) = state.clients.get_mut(&server.name) else {
                continue;
            };
            match client.tools() {
                Ok(tools) => {
                    state.errors.remove(&server.name);
                    for tool in tools {
                        out.push(core_runtime::provider::ToolSpec {
                            name: protocol::qualified(&server.name, &tool.name),
                            description: if tool.description.trim().is_empty() {
                                format!("{} from the {} MCP server", tool.name, server.name)
                            } else {
                                format!("{} (MCP: {})", tool.description, server.name)
                            },
                            parameters: tool.input_schema,
                        });
                    }
                }
                Err(error) => {
                    state.errors.insert(server.name.clone(), error.message);
                }
            }
        }
        out
    }

    /// Call a namespaced tool.
    ///
    /// # Errors
    ///
    /// [`McpError`] when the name does not belong to a configured server, the
    /// server cannot start, or the call fails.
    pub fn call(
        &self,
        qualified: &str,
        arguments: &serde_json::Value,
    ) -> Result<protocol::CallOutcome, McpError> {
        let Some((server_name, tool)) = protocol::split_qualified(qualified) else {
            return Err(McpError::client(format!(
                "`{qualified}` is not a namespaced MCP tool"
            )));
        };
        let Some(server) = self.servers.iter().find(|s| s.name == server_name) else {
            return Err(McpError::client(format!(
                "no MCP server named `{server_name}`"
            )));
        };
        let mut state = self
            .inner
            .lock()
            .map_err(|_| McpError::client("the MCP pool is unavailable"))?;
        ensure_started(&mut state, server)?;
        let client = state
            .clients
            .get_mut(&server_name)
            .ok_or_else(|| McpError::client(format!("{server_name}: not running")))?;
        match client.call(&tool, arguments.clone()) {
            Ok(outcome) => {
                state.errors.remove(&server_name);
                Ok(outcome)
            }
            Err(error) => {
                state.errors.insert(server_name, error.message.clone());
                Err(error)
            }
        }
    }
}

/// Start a server when it is not running, restarting a dead one.
fn ensure_started(state: &mut PoolState, server: &Server) -> Result<(), McpError> {
    if let Some(client) = state.clients.get_mut(&server.name)
        && client.is_alive()
    {
        return Ok(());
    }
    state.clients.remove(&server.name);
    let client = client::Client::start(server)?;
    state.clients.insert(server.name.clone(), client);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in server used by this module's tests and the client's.
    ///
    /// A shell script rather than a second binary so the fixture is readable in
    /// the test. It echoes each request's own id back, so the client has to match
    /// ids the way it would against a real server.
    pub(super) fn fixture_server(dir: &std::path::Path, name: &str) -> Server {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(format!("{name}-mcp.sh"));
        let script = r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1.2.3"}}}\n' "$id" ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","method":"notifications/message","params":{"level":"info"}}\n'
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"echo","description":"Echo text","inputSchema":{"type":"object"}}]}}\n' "$id" ;;
    *'"method":"tools/call"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"pong"}]}}\n' "$id" ;;
    *'"method":"fail/call"'*)
      printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32601,"message":"no such method"}}\n' "$id" ;;
  esac
done
"#;
        // Write beside the target and rename, so a parallel exec never sees a
        // file that is still open for writing: that is ETXTBSY, and it made this
        // fixture flaky under a loaded test run.
        let staging = dir.join(format!("{name}-mcp.sh.tmp"));
        std::fs::write(&staging, script).expect("write fixture");
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        std::fs::rename(&staging, &path).expect("activate fixture");
        Server {
            name: name.to_owned(),
            command: path.display().to_string(),
            args: Vec::new(),
        }
    }

    #[test]
    fn an_empty_document_is_an_empty_list() {
        assert_eq!(parse("").expect("empty is fine"), Vec::new());
        assert_eq!(parse("   ").expect("blank is fine"), Vec::new());
    }

    #[cfg(test)]
    mod pool_tests {
        use super::fixture_server;

        #[test]
        fn the_pool_namespaces_every_tool_and_routes_the_call_back() {
            let dir = tempfile::tempdir().expect("temp");
            let server = fixture_server(dir.path(), "fake");
            let pool = super::super::Pool::new(vec![server]);

            let specs = pool.specs();
            assert_eq!(specs.len(), 1);
            assert_eq!(specs[0].name, "fake__echo");
            assert!(
                specs[0].description.contains("MCP"),
                "the description says where it came from: {}",
                specs[0].description
            );

            let outcome = pool
                .call("fake__echo", &serde_json::json!({"text": "hi"}))
                .expect("call");
            assert_eq!(outcome.text, "pong");
            assert!(
                pool.errors().is_empty(),
                "a healthy server reports no error"
            );
        }

        #[test]
        fn a_server_that_cannot_start_reports_instead_of_offering_nothing_silently() {
            let pool = super::super::Pool::new(vec![super::super::Server {
                name: "ghost".to_owned(),
                command: "/nonexistent/mcp-server".to_owned(),
                args: Vec::new(),
            }]);

            assert!(pool.specs().is_empty(), "no tools from a dead server");
            let errors = pool.errors();
            assert_eq!(errors.len(), 1);
            assert_eq!(errors[0].0, "ghost");
            assert!(
                errors[0].1.contains("cannot start"),
                "the reason is kept: {}",
                errors[0].1
            );
        }

        #[test]
        fn an_unqualified_name_is_refused_rather_than_sent_somewhere() {
            let pool = super::super::Pool::new(Vec::new());
            let error = pool
                .call("read_file", &serde_json::json!({}))
                .expect_err("not an MCP name");
            assert!(error.message.contains("namespaced"), "{}", error.message);
        }
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
