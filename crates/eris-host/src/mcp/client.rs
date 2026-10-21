//! One child process per configured server.
//!
//! The client is synchronous on purpose. A turn already runs on a blocking
//! thread, and a blocking read with a deadline is far easier to reason about than
//! an async child whose stdout is a stream of frames: when the deadline passes,
//! the answer is "no reply" and the caller decides.
//!
//! Three rules shape it.
//!
//! 1. **The child is owned.** It is killed on drop and on a failed handshake, so
//!    a server that misbehaves cannot outlive the daemon that started it.
//! 2. **Every request has a deadline.** A server that hangs is a failed tool
//!    call, never a hung turn.
//! 3. **A line that is not our reply is skipped.** Servers log, send progress and
//!    notify; a client that treated those as answers would be answering noise.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::thread;
use std::time::Duration;

use serde_json::Value;

use super::McpError;
use super::Server;
use super::protocol::{self, CallOutcome, Reply, Request, ServerTool};

/// How long a single request may take before it is a failure.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// How long the handshake may take. A cold `npx` server can be slow, but not
/// unbounded, and a turn should not wait on a package manager.
pub const START_TIMEOUT: Duration = Duration::from_secs(25);

/// A live connection to one server.
pub struct Client {
    /// The configured name, used in messages and in tool namespacing.
    pub name: String,
    /// The protocol revision the server said it would use.
    pub protocol_version: String,
    /// The server's self-reported name and version, when it sent them.
    pub server_info: String,
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("name", &self.name)
            .field("protocol_version", &self.protocol_version)
            .field("server_info", &self.server_info)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// Spawn a server and complete the handshake.
    ///
    /// # Errors
    ///
    /// [`McpError`] when the process cannot start, exits during the handshake, or
    /// answers with something that is not an initialize result.
    pub fn start(server: &Server) -> Result<Self, McpError> {
        let mut child = Command::new(&server.command)
            .args(&server.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // stderr is inherited on purpose: a server's own complaints belong in
            // the journal, where the operator will look, not in a pipe nobody
            // reads that would eventually block it.
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|error| {
                McpError::client(format!(
                    "{name}: cannot start `{command}`: {error}",
                    name = server.name,
                    command = server.command
                ))
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::client(format!("{}: stdin was not available", server.name)))?;
        let stdout = child.stdout.take().ok_or_else(|| {
            McpError::client(format!("{}: stdout was not available", server.name))
        })?;

        // A dedicated reader: `recv_timeout` is what gives every request a
        // deadline, and a blocking `read_line` on the request path cannot.
        let (tx, rx) = channel();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) if !line.trim().is_empty() => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
        });

        let mut client = Self {
            name: server.name.clone(),
            protocol_version: String::new(),
            server_info: String::new(),
            child,
            stdin,
            lines: rx,
            next_id: 1,
        };
        client.handshake()?;
        Ok(client)
    }

    /// `initialize`, the initialized notification, and nothing else.
    ///
    /// MCP requires the notification before any other request, so it is sent here
    /// rather than left to each caller to remember.
    fn handshake(&mut self) -> Result<(), McpError> {
        let result =
            self.request_with_deadline("initialize", protocol::initialize_params(), START_TIMEOUT)?;
        self.protocol_version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(protocol::PROTOCOL_VERSION)
            .to_owned();
        self.server_info = result
            .get("serverInfo")
            .map(|info| {
                let name = info
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let version = info.get("version").and_then(Value::as_str).unwrap_or("?");
                format!("{name} {version}")
            })
            .unwrap_or_else(|| "unnamed server".to_owned());

        let note = protocol::notification("notifications/initialized");
        self.write_line(&note)?;
        Ok(())
    }

    /// The tools this server advertises.
    ///
    /// # Errors
    ///
    /// [`McpError`] on a transport failure, a deadline, or a server error.
    pub fn tools(&mut self) -> Result<Vec<ServerTool>, McpError> {
        let result = self.request("tools/list", serde_json::json!({}))?;
        Ok(protocol::tools_from(&result))
    }

    /// Call one tool by its server-side name.
    ///
    /// # Errors
    ///
    /// [`McpError`] on a transport failure, a deadline, or a server error. A tool
    /// that reports a failure *result* is not an error here: it is a
    /// [`CallOutcome`] with `is_error`, because the server answered.
    pub fn call(&mut self, tool: &str, arguments: Value) -> Result<CallOutcome, McpError> {
        let result = self.request(
            "tools/call",
            serde_json::json!({ "name": tool, "arguments": arguments }),
        )?;
        Ok(protocol::outcome_from(&result))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        self.request_with_deadline(method, params, REQUEST_TIMEOUT)
    }

    /// Send one request and wait for its reply, skipping anything else.
    fn request_with_deadline(
        &mut self,
        method: &str,
        params: Value,
        deadline: Duration,
    ) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = Request {
            id,
            method: method.to_owned(),
            params,
        };
        self.write_line(&request.encode())?;

        loop {
            match self.lines.recv_timeout(deadline) {
                Ok(line) => match protocol::decode(&line, id) {
                    Reply::Result(value) => return Ok(value),
                    Reply::Error { code, message } => {
                        return Err(McpError::client(format!(
                            "{}: {method} failed ({code}): {message}",
                            self.name
                        )));
                    }
                    Reply::Other => continue,
                },
                Err(RecvTimeoutError::Timeout) => {
                    return Err(McpError::client(format!(
                        "{}: {method} did not answer within {}s",
                        self.name,
                        deadline.as_secs()
                    )));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // The reader thread ended: the child closed stdout or died.
                    let status = self
                        .child
                        .try_wait()
                        .ok()
                        .flatten()
                        .map(|status| status.to_string());
                    return Err(McpError::client(format!(
                        "{}: {method} lost the server ({})",
                        self.name,
                        status.unwrap_or_else(|| "stdout closed".to_owned())
                    )));
                }
            }
        }
    }

    fn write_line(&mut self, line: &str) -> Result<(), McpError> {
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.write_all(b"\n"))
            .and_then(|()| self.stdin.flush())
            .map_err(|error| {
                McpError::client(format!(
                    "{}: cannot write to the server: {error}",
                    self.name
                ))
            })
    }

    /// Whether the child is still running.
    #[must_use]
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        // Owned, so the daemon never leaves a server behind. `kill` then `wait`
        // reaps it: a zombie child is a process table entry that outlives the
        // thing that made it.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(dir: &std::path::Path) -> Server {
        super::super::tests::fixture_server(dir, "fake")
    }

    #[test]
    fn a_client_handshakes_lists_tools_and_calls_one() {
        let _serial = crate::mcp::serial::hold();
        let dir = tempfile::tempdir().expect("temp");
        let server = fixture(dir.path());

        let mut client = Client::start(&server).expect("handshake");
        assert_eq!(client.protocol_version, "2024-11-05");
        assert_eq!(client.server_info, "fake 1.2.3");

        let tools = client.tools().expect("tools");
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");

        let outcome = client
            .call("echo", serde_json::json!({"text": "hi"}))
            .expect("call");
        assert_eq!(outcome.text, "pong");
        assert!(!outcome.is_error);
    }

    #[test]
    fn a_notification_between_replies_is_skipped_rather_than_mistaken_for_one() {
        let _serial = crate::mcp::serial::hold();
        let dir = tempfile::tempdir().expect("temp");
        let server = fixture(dir.path());
        // The fixture sends a notification before the tools/list reply; reaching
        // the answer at all proves the skip works.
        let mut client = Client::start(&server).expect("handshake");
        assert!(client.tools().is_ok());
    }

    #[test]
    fn a_server_error_is_reported_with_its_code() {
        let _serial = crate::mcp::serial::hold();
        let dir = tempfile::tempdir().expect("temp");
        let server = fixture(dir.path());
        let mut client = Client::start(&server).expect("handshake");

        let error = client
            .request("fail/call", serde_json::json!({}))
            .expect_err("an error reply is an error");
        assert!(
            error.message.contains("no such method"),
            "{}",
            error.message
        );
        assert!(error.message.contains("-32601"), "{}", error.message);
    }

    #[test]
    fn a_missing_executable_is_named_rather_than_silently_skipped() {
        let _serial = crate::mcp::serial::hold();
        let server = Server {
            name: "ghost".to_owned(),
            command: "/nonexistent/mcp-server".to_owned(),
            args: Vec::new(),
        };
        let error = Client::start(&server).expect_err("cannot start");
        assert!(error.message.contains("ghost"), "{}", error.message);
        assert!(error.message.contains("cannot start"), "{}", error.message);
    }

    #[test]
    fn dropping_the_client_kills_the_child() {
        let _serial = crate::mcp::serial::hold();
        let dir = tempfile::tempdir().expect("temp");
        let server = fixture(dir.path());
        let pid = {
            let mut client = Client::start(&server).expect("handshake");
            assert!(client.is_alive());
            client.child.id()
        };
        // The scope ended, so the client dropped and killed it. Checking that the
        // pid is absent is not enough: pids are reused, and a neighbour process
        // inheriting the number makes this test flaky rather than honest. The
        // identity is checked instead - the process must no longer be running
        // *our fixture script*.
        let proc = std::path::PathBuf::from(format!("/proc/{pid}/cmdline"));
        let mut gone = false;
        for _ in 0..40 {
            match std::fs::read(&proc) {
                Err(_) => {
                    gone = true;
                    break;
                }
                Ok(bytes) => {
                    let cmdline = String::from_utf8_lossy(&bytes);
                    if !cmdline.contains("fake-mcp.sh") {
                        // A different process now owns the number.
                        gone = true;
                        break;
                    }
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert!(gone, "the child outlived the client: pid {pid}");
    }
}
