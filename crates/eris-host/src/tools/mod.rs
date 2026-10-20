//! The tool layer: what a turn may call, and the sandbox that decides whether it
//! may actually do it.
//!
//! Two rules shape this module.
//!
//! The first is that a path from a model is untrusted input. Every path is
//! resolved against the workspace, and a resolution that lands outside it is
//! refused by name rather than clamped quietly: a model that asked for
//! `/etc/shadow` should learn that it was refused, not receive a nearby file.
//!
//! The second is that a sandbox is a boundary enforced in code, not a sentence
//! in a settings page. `read-only` refuses every write at the tool, and
//! `workspace-write` confines writes to the workspace even when the caller asks
//! for somewhere else.

use std::path::{Component, Path, PathBuf};

pub use crate::onboarding::permissions::SandboxMode;
use core_runtime::provider::ToolSpec;

/// The largest file a turn may read in one call.
///
/// A model that pages through a 2 GB file is not reading it, it is burning a
/// context window. Bigger than this is refused by size, and the refusal says so
/// rather than silently truncating into a half-file the model will trust.
pub const MAX_READ_BYTES: u64 = 256 * 1024;

/// How long one command may run before `timeout` kills it.
pub const COMMAND_TIMEOUT_SECS: u64 = 30;

/// How much of a command's output is handed back to the model.
pub const MAX_COMMAND_OUTPUT_BYTES: usize = 64 * 1024;

/// How much of a command's output is streamed to a watching browser.
///
/// The same budget as the capture, so what the operator watches and what the
/// model reads are the same size. A command that prints past it keeps running;
/// the live view simply stops growing, and the final record states the omission.
pub const MAX_LIVE_OUTPUT_BYTES: usize = MAX_COMMAND_OUTPUT_BYTES;

/// Why a tool could not do the thing it was asked to do.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// The requested path resolves outside the workspace.
    #[error("path escapes the workspace: {requested}")]
    Escape {
        /// What the caller asked for, verbatim.
        requested: String,
    },
    /// The path was empty or only whitespace.
    #[error("a tool path cannot be empty")]
    EmptyPath,
    /// The filesystem refused, for a reason worth showing.
    #[error("{path}: {detail}")]
    Io {
        /// The path involved.
        path: String,
        /// The underlying message.
        detail: String,
    },
}

/// Resolve a model-supplied path against the workspace, refusing an escape.
///
/// Three layers, because each catches what the one before it misses:
///
/// 1. Lexical normalization, so `a/../../etc/passwd` cannot walk out with `..`.
/// 2. A lexical containment test against the workspace.
/// 3. A canonicalized containment test on the deepest existing ancestor, so a
///    symlink inside the workspace pointing outside it is still refused.
///
/// A path that does not exist yet is allowed - writing a new file is the point -
/// so layer 3 checks the deepest ancestor that does exist rather than the leaf.
pub fn resolve(workspace: &Path, requested: &str) -> Result<PathBuf, ToolError> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Err(ToolError::EmptyPath);
    }

    let canonical_workspace = workspace.canonicalize().map_err(|error| ToolError::Io {
        path: workspace.display().to_string(),
        detail: error.to_string(),
    })?;

    let asked = Path::new(requested);
    let joined = if asked.is_absolute() {
        asked.to_path_buf()
    } else {
        workspace.join(asked)
    };
    let normalized = normalize(&joined);

    let escape = || ToolError::Escape {
        requested: requested.to_owned(),
    };

    // Layer 2: the normalized path must sit under the workspace, either as given
    // or as canonicalized (they differ when the workspace itself is a symlink).
    if !normalized.starts_with(workspace) && !normalized.starts_with(&canonical_workspace) {
        return Err(escape());
    }

    // Layer 3: nothing that already exists on the way there may lead outside.
    let existing = deepest_existing(&normalized).ok_or_else(escape)?;
    let canonical_existing = existing.canonicalize().map_err(|error| ToolError::Io {
        path: existing.display().to_string(),
        detail: error.to_string(),
    })?;
    if !canonical_existing.starts_with(&canonical_workspace) {
        return Err(escape());
    }

    Ok(normalized)
}

/// Collapse `.` and `..` without touching the filesystem.
///
/// `..` pops one component and stops at the root, which is what the kernel does
/// with a path it has not resolved yet.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// The deepest ancestor of `path` that exists, or `None` at the root of nothing.
fn deepest_existing(path: &Path) -> Option<PathBuf> {
    let mut current = path.to_path_buf();
    loop {
        if current.exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// `settings` row: whether a turn may call tools at all.
///
/// Default on. Off means the model is offered nothing, so it answers from the
/// prompt alone rather than spending a round trip discovering a refusal.
pub const SETTING_TOOLS_ENABLED: &str = "tools.enabled";

/// `settings` row: how many tool rounds one turn may take.
pub const SETTING_TOOL_ROUNDS: &str = "tools.max_rounds";

/// The default tool-round budget.
pub const DEFAULT_TOOL_ROUNDS: usize = 12;

/// The most rounds an operator may configure.
///
/// Operationally unbounded: the ceiling exists only to reject a number that is
/// not a number of rounds at all, so it is set far past any turn a human would
/// wait for rather than at a value that silently truncates real infra work. The
/// budget is per turn and the turn is cancellable, so the guard against a
/// runaway is the stop button and the bill, not this constant. The default of
/// [`DEFAULT_TOOL_ROUNDS`] is what keeps an ordinary question cheap.
pub const MAX_TOOL_ROUNDS_CEILING: u32 = 99_999;

/// Whether tools are enabled, from a stored value.
///
/// Anything but an explicit `false` leaves them on: a corrupt row must not
/// silently remove a capability the operator relies on.
#[must_use]
pub fn enabled(stored: Option<&str>) -> bool {
    !matches!(stored.map(str::trim), Some("false"))
}

/// The tool-round budget, clamped to something a turn can actually finish.
#[must_use]
pub fn max_rounds(stored: Option<&str>) -> usize {
    stored
        .map(str::trim)
        .and_then(|raw| raw.parse::<u32>().ok())
        .filter(|rounds| *rounds >= 1 && *rounds <= MAX_TOOL_ROUNDS_CEILING)
        .map_or(DEFAULT_TOOL_ROUNDS, |rounds| rounds as usize)
}

/// Where a turn may work, and how much it may do there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sandbox {
    workspace: PathBuf,
    mode: SandboxMode,
}

impl Sandbox {
    /// Build a sandbox over a workspace.
    #[must_use]
    pub fn new(workspace: PathBuf, mode: SandboxMode) -> Self {
        Self { workspace, mode }
    }

    /// The directory every path resolves against.
    #[must_use]
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// The configured mode.
    #[must_use]
    pub fn mode(&self) -> SandboxMode {
        self.mode
    }

    /// Whether a write is permitted at all.
    ///
    /// `Full` still writes only inside the workspace: this layer confines paths,
    /// and full-host reach is a different gate that 1.0 does not open.
    #[must_use]
    pub fn allows_write(&self) -> bool {
        matches!(self.mode, SandboxMode::WorkspaceWrite | SandboxMode::Full)
    }
}

/// What a call produced. Always something to hand back to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    /// JSON, so the model reads structure rather than prose.
    pub content: String,
    /// Whether this call failed. A failed call is reported, never hidden.
    pub is_error: bool,
}

impl ToolResult {
    /// A successful result carrying a JSON payload.
    pub fn ok(payload: serde_json::Value) -> Self {
        Self {
            content: payload.to_string(),
            is_error: false,
        }
    }

    fn error(message: impl Into<String>) -> Self {
        Self {
            content: serde_json::json!({ "error": message.into() }).to_string(),
            is_error: true,
        }
    }

    /// A result returned by an MCP server.
    ///
    /// Wrapped rather than passed through raw so the run log can tell which
    /// server produced it, and so a server that reports a failure still renders
    /// as a failure.
    #[must_use]
    pub fn from_mcp(tool: &str, outcome: &crate::mcp::protocol::CallOutcome) -> Self {
        Self {
            content: serde_json::json!({
                "tool": tool,
                "output": outcome.text,
            })
            .to_string(),
            is_error: outcome.is_error,
        }
    }

    /// A call the gate refused before it ran.
    ///
    /// Used when the call could not even be parked. Doing nothing is the safe
    /// answer; a policy that could not be recorded is not permission to proceed.
    #[must_use]
    pub fn refused(reason: &str) -> Self {
        Self {
            content: serde_json::json!({
                "status": "refused",
                "error": reason,
            })
            .to_string(),
            is_error: true,
        }
    }

    /// A call that was parked for approval instead of run.
    ///
    /// Not an error: nothing went wrong, and the model should tell the operator
    /// that the write is waiting rather than retry it or claim it succeeded.
    #[must_use]
    pub fn parked(approval_id: &str, tool: &str) -> Self {
        Self {
            content: serde_json::json!({
                "status": "pending_approval",
                "approval_id": approval_id,
                "tool": tool,
                "message": "This call is waiting for the operator to approve it. \
                            Nothing has run yet.",
            })
            .to_string(),
            is_error: false,
        }
    }
}

/// The tools offered to the model this build.
///
/// Four, deliberately. A file tool that works beats a catalogue of half-built
/// ones, and every name here is dispatchable by [`execute`] - a spec the model
/// can call but the daemon cannot run is the bug this module exists to prevent.
///
/// `dispatch_agent` is the one that needs more than a sandbox to run: it writes a
/// row and spawns a child turn, which means a database and a parent turn, neither
/// of which the sandbox holds. [`execute`] therefore answers it with an explicit
/// refusal rather than "unknown tool", and the turn itself intercepts it before
/// this layer, exactly like `generate_image`. It is listed here because "which
/// tools may this turn call" is the question this function answers, and the answer
/// in `Full` mode is: these.
#[must_use]
pub fn specs(mode: SandboxMode) -> Vec<ToolSpec> {
    specs_offering(mode, true)
}

/// The same list, with the operator's dispatch switch applied.
///
/// `agents` off means the dispatching tool is not offered, for the same reason
/// `run_command` is not offered outside `Full`: a tool the daemon will not run
/// must never reach the model's list.
#[must_use]
pub fn specs_offering(mode: SandboxMode, agents: bool) -> Vec<ToolSpec> {
    let mut offered = vec![
        ToolSpec {
            name: "list_dir".to_owned(),
            description: "List the entries in a directory inside the workspace.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory relative to the workspace. Defaults to the workspace root."
                    }
                },
                "required": []
            }),
        },
        ToolSpec {
            name: "read_file".to_owned(),
            description: "Read a UTF-8 text file inside the workspace.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path relative to the workspace."
                    }
                },
                "required": ["path"]
            }),
        },
        ToolSpec {
            name: "write_file".to_owned(),
            description: "Create or replace a file inside the workspace.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "File path relative to the workspace."
                    },
                    "content": {
                        "type": "string",
                        "description": "The full new contents of the file."
                    }
                },
                "required": ["path", "content"]
            }),
        },
        ToolSpec {
            name: "fetch_url".to_owned(),
            description: "Fetch a web page and return its readable text.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "An http or https URL. Private and loopback addresses are refused."
                    }
                },
                "required": ["url"]
            }),
        },
    ];

    // Command execution is offered only where the sandbox will honour it. A tool
    // the daemon refuses must never reach the model's list, or every turn spends
    // a round trip discovering a refusal the prompt could have prevented.
    if mode == SandboxMode::Full {
        offered.push(ToolSpec {
            name: "run_command".to_owned(),
            description: "Run a shell command on this host and return its output. \
                          Runs in the workspace with the daemon's own privileges."
                .to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command line, run with sh -c."
                    }
                },
                "required": ["command"]
            }),
        });
        // Dispatching is a Full-mode capability for the same reason command
        // execution is: it spends the host's resources on the model's say-so. Off
        // by setting, and off outside Full always, because a confined turn that
        // could spawn children could reach past its own confinement through them.
        if agents {
            offered.push(crate::subagents::spec());
        }
    }

    offered
}

/// Run one call. Never panics and never returns success for a failure.
pub fn execute(name: &str, arguments: &serde_json::Value, sandbox: &Sandbox) -> ToolResult {
    execute_streaming(name, arguments, sandbox, &mut |_chunk| {})
}

/// Run one call, handing output to `sink` while it is still being produced.
///
/// Only `run_command` produces anything before it returns; every other tool is a
/// single filesystem or network read and reports once. The sink exists because a
/// command that takes twenty seconds should look like a command that takes twenty
/// seconds, not like a turn that is thinking hard about nothing.
pub fn execute_streaming(
    name: &str,
    arguments: &serde_json::Value,
    sandbox: &Sandbox,
    sink: &mut dyn FnMut(&str),
) -> ToolResult {
    match name {
        "list_dir" => list_dir(arguments, sandbox),
        "read_file" => read_file(arguments, sandbox),
        "write_file" => write_file(arguments, sandbox),
        "run_command" => run_command(arguments, sandbox, sink),
        "fetch_url" => fetch_url(arguments, sandbox),
        // Named rather than unknown: this layer cannot dispatch, because a
        // dispatch needs a database and a parent turn. The turn that offered the
        // spec intercepts the call before it reaches here, so this is the answer a
        // direct caller gets, and it says why instead of pretending the tool does
        // not exist.
        "dispatch_agent" => ToolResult::refused(
            "dispatching needs the turn that offered it, so this call did nothing; \
             it runs inside a conversation turn and not in the sandbox layer",
        ),
        other => ToolResult::error(format!("unknown tool: {other}")),
    }
}

/// Pull a required string argument.
fn string_arg<'a>(arguments: &'a serde_json::Value, key: &str) -> Result<&'a str, ToolResult> {
    arguments
        .get(key)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ToolResult::error(format!("missing required string argument: {key}")))
}

fn list_dir(arguments: &serde_json::Value, sandbox: &Sandbox) -> ToolResult {
    let requested = arguments
        .get("path")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(".");

    let path = match resolve(sandbox.workspace(), requested) {
        Ok(path) => path,
        Err(error) => return ToolResult::error(error.to_string()),
    };

    let entries = match std::fs::read_dir(&path) {
        Ok(entries) => entries,
        Err(error) => return ToolResult::error(format!("{}: {error}", path.display())),
    };

    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
        names.push(if is_dir { format!("{name}/") } else { name });
    }
    names.sort();

    ToolResult::ok(serde_json::json!({
        "path": requested,
        "entries": names,
    }))
}

fn read_file(arguments: &serde_json::Value, sandbox: &Sandbox) -> ToolResult {
    let requested = match string_arg(arguments, "path") {
        Ok(path) => path,
        Err(error) => return error,
    };

    let path = match resolve(sandbox.workspace(), requested) {
        Ok(path) => path,
        Err(error) => return ToolResult::error(error.to_string()),
    };

    let metadata = match std::fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) => return ToolResult::error(format!("{}: {error}", path.display())),
    };
    if metadata.is_dir() {
        return ToolResult::error(format!("{requested} is a directory, not a file"));
    }
    if metadata.len() > MAX_READ_BYTES {
        return ToolResult::error(format!(
            "{requested} is {} bytes, over the {MAX_READ_BYTES} byte read limit",
            metadata.len()
        ));
    }

    match std::fs::read_to_string(&path) {
        Ok(content) => ToolResult::ok(serde_json::json!({
            "path": requested,
            "bytes": metadata.len(),
            "content": content,
        })),
        Err(error) => ToolResult::error(format!("{requested}: {error}")),
    }
}

fn write_file(arguments: &serde_json::Value, sandbox: &Sandbox) -> ToolResult {
    if !sandbox.allows_write() {
        return ToolResult::error(format!(
            "write_file is refused: the sandbox mode is {} and writing needs workspace-write",
            sandbox.mode().as_str()
        ));
    }

    let requested = match string_arg(arguments, "path") {
        Ok(path) => path,
        Err(error) => return error,
    };
    let content = match string_arg(arguments, "content") {
        Ok(content) => content,
        Err(error) => return error,
    };

    let path = match resolve(sandbox.workspace(), requested) {
        Ok(path) => path,
        Err(error) => return ToolResult::error(error.to_string()),
    };

    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        return ToolResult::error(format!("{}: {error}", parent.display()));
    }

    match std::fs::write(&path, content) {
        Ok(()) => ToolResult::ok(serde_json::json!({
            "path": requested,
            "bytes": content.len(),
        })),
        Err(error) => ToolResult::error(format!("{requested}: {error}")),
    }
}

/// The most of a page that is fetched or handed back.
pub const MAX_FETCH_BYTES: usize = 512 * 1024;

/// How long a fetch may take.
pub const FETCH_TIMEOUT_SECS: u64 = 15;

/// The most redirects a fetch will follow.
pub const MAX_FETCH_REDIRECTS: usize = 5;

/// Whether a host may be fetched at all.
///
/// A model can be talked into fetching anything, so the guard is here rather than
/// in the prompt: loopback, link-local and private ranges are refused by name.
/// This does not stop a public hostname that resolves to a private address - a
/// full answer needs resolution-time checking - and that limitation is stated
/// rather than papered over.
pub fn fetch_allowed(url: &str) -> Result<(), String> {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .ok_or_else(|| format!("`{url}` is not an http or https URL"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_creds, host)| host);
    let host = host.split(':').next().unwrap_or("").to_ascii_lowercase();
    if host.is_empty() {
        return Err("the URL has no host".to_owned());
    }
    let forbidden = host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host == "[::1]"
        || host.starts_with("127.")
        || host.starts_with("10.")
        || host.starts_with("192.168.")
        || host.starts_with("169.254.")
        || (host.starts_with("172.") && {
            let second = host
                .split('.')
                .nth(1)
                .and_then(|part| part.parse::<u8>().ok())
                .unwrap_or(0);
            (16..=31).contains(&second)
        });
    if forbidden {
        return Err(format!(
            "`{host}` is a private or loopback address, and fetching it is refused"
        ));
    }
    Ok(())
}

/// Turn an HTML document into readable text.
///
/// Not a parser: script and style bodies are dropped, tags are removed, a few
/// entities are decoded and whitespace is collapsed. Enough to read an article,
/// and deliberately not enough to pretend it understands a page.
#[must_use]
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let lower = after.to_ascii_lowercase();

        // A script or style body is not content: skip to its closing tag in one
        // move. Falling through to the generic tag branch would drop only the
        // opening tag and leave the code behind, which is worse than nothing.
        let mut skipped = false;
        for tag in ["script", "style"] {
            if lower.starts_with(&format!("<{tag}")) {
                if let Some(end) = lower.find(&format!("</{tag}>")) {
                    rest = &after[end + tag.len() + 3..];
                    out.push(' ');
                    skipped = true;
                }
                break;
            }
        }
        if skipped {
            continue;
        }

        match after.find('>') {
            Some(end) => rest = &after[end + 1..],
            None => break,
        }
    }
    out.push_str(rest);

    let decoded = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn fetch_url(arguments: &serde_json::Value, _sandbox: &Sandbox) -> ToolResult {
    let url = match string_arg(arguments, "url") {
        Ok(url) => url.trim().to_owned(),
        Err(error) => return error,
    };
    if let Err(refusal) = fetch_allowed(&url) {
        return ToolResult::refused(&refusal);
    }

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return ToolResult::error(format!("cannot start a fetch: {error}")),
    };

    let response = runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(FETCH_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::limited(MAX_FETCH_REDIRECTS))
            .build()?;
        client
            .get(&url)
            .header(reqwest::header::USER_AGENT, "kallisti-mcp/1.4")
            .send()
            .await
    });

    let response = match response {
        Ok(response) => response,
        Err(error) => return ToolResult::error(format!("cannot fetch {url}: {error}")),
    };
    if !response.status().is_success() {
        return ToolResult::error(format!("{url} answered {}", response.status()));
    }
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown")
        .to_owned();

    let body = match runtime.block_on(response.text()) {
        Ok(body) => body,
        Err(error) => return ToolResult::error(format!("cannot read {url}: {error}")),
    };
    let truncated = body.len() > MAX_FETCH_BYTES;
    let body = if truncated {
        body.chars().take(MAX_FETCH_BYTES).collect::<String>()
    } else {
        body
    };

    let text = if content_type.contains("html") {
        html_to_text(&body)
    } else {
        body.split_whitespace().collect::<Vec<_>>().join(" ")
    };

    ToolResult::ok(serde_json::json!({
        "url": url,
        "status": status,
        "content_type": content_type,
        "truncated": truncated,
        "text": text,
    }))
}

/// Run one command, streaming what it writes while it writes it.
///
/// One pipe, not two. The shell merges its stderr into its stdout before the
/// command starts (`exec 2>&1`), so what arrives is the order the command wrote
/// it in, which is the whole point of a terminal view. Two pipes would need two
/// readers and produce two streams whose relative order the daemon cannot know.
///
/// A reader thread drains the pipe into a channel and the caller's own thread
/// hands chunks to the sink, so the sink never has to be `Send`. Nothing here
/// blocks on the child while the pipe is full: the reader runs from the moment
/// the process starts until it closes its output.
fn run_command(
    arguments: &serde_json::Value,
    sandbox: &Sandbox,
    sink: &mut dyn FnMut(&str),
) -> ToolResult {
    if sandbox.mode() != SandboxMode::Full {
        return ToolResult::error(format!(
            "run_command is refused: the sandbox mode is {} and running commands needs full host",
            sandbox.mode().as_str()
        ));
    }

    let command = match string_arg(arguments, "command") {
        Ok(command) => command,
        Err(error) => return error,
    };
    if command.trim().is_empty() {
        return ToolResult::error("a command cannot be empty");
    }

    // `timeout` rather than a Rust timer: the child is killed by the kernel and
    // exit 124 says so, which is a failure the model can see instead of a hang
    // that looks like the daemon dying.
    let mut child = match std::process::Command::new("timeout")
        .arg(format!("{COMMAND_TIMEOUT_SECS}s"))
        .arg("sh")
        .arg("-c")
        .arg(format!("exec 2>&1\n{command}"))
        .current_dir(sandbox.workspace())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return ToolResult::error(format!("cannot run the command: {error}")),
    };

    let pipe = child.stdout.take();
    let (sender, receiver) = std::sync::mpsc::channel::<String>();
    let reader = std::thread::spawn(move || read_pipe(pipe, &sender));

    let mut captured: Vec<u8> = Vec::new();
    let mut streamed = 0usize;
    while let Ok(chunk) = receiver.recv() {
        captured.extend_from_slice(chunk.as_bytes());
        // The live view is bounded too: a command that prints all day must not
        // turn into all-day traffic to one browser.
        if streamed < MAX_LIVE_OUTPUT_BYTES {
            streamed += chunk.len();
            sink(&chunk);
        }
    }
    // Kept, not merely counted: the head of the output is the part worth
    // reading, and the omission is stated rather than silently dropped.
    let omitted = captured.len().saturating_sub(MAX_COMMAND_OUTPUT_BYTES);
    captured.truncate(MAX_COMMAND_OUTPUT_BYTES);
    // The reader ends at end of file, which the kernel delivers when the last
    // writer closes: the process is finished, so waiting cannot block.
    let _ = reader.join();
    let status = child.wait();

    let text = truncate_bytes(&captured, omitted);
    let exit_code = status
        .as_ref()
        .ok()
        .and_then(std::process::ExitStatus::code);
    // The key is `stdout` even though the pipe was merged, because that is the
    // name the model has been reading and the name the transcript renderer
    // knows. `stderr` is empty rather than absent, so a shape with two streams
    // still has two, and `streams` states plainly that the two were interleaved
    // to keep the order a terminal would have shown.
    let body = serde_json::json!({
        "command": command,
        "exit_code": exit_code,
        "streams": "merged",
        "stdout": text,
        "stderr": "",
    });

    match status {
        // A command that could not be reaped is a failure with evidence
        // attached, never a success the model has to notice on its own.
        Err(error) => ToolResult::error(format!("cannot collect the command: {error}")),
        Ok(status) if status.success() => ToolResult::ok(body),
        Ok(_) => ToolResult {
            content: body.to_string(),
            is_error: true,
        },
    }
}

/// Drain one pipe into the channel, emitting text only on character boundaries.
///
/// A read lands wherever the kernel says it does, which can be the middle of a
/// multi-byte character. Decoding each read on its own would put replacement
/// characters in the middle of an operator's output, so the incomplete tail is
/// carried into the next read.
fn read_pipe(pipe: Option<std::process::ChildStdout>, sender: &std::sync::mpsc::Sender<String>) {
    use std::io::Read as _;

    let Some(mut pipe) = pipe else { return };
    let mut buffer = [0u8; 8192];
    let mut carry: Vec<u8> = Vec::new();
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                carry.extend_from_slice(&buffer[..count]);
                let valid = match std::str::from_utf8(&carry) {
                    Ok(_) => carry.len(),
                    Err(error) => error.valid_up_to(),
                };
                if valid == 0 {
                    continue;
                }
                let text = String::from_utf8_lossy(&carry[..valid]).into_owned();
                carry.drain(..valid);
                if sender.send(text).is_err() {
                    return;
                }
            }
            Err(_) => break,
        }
    }
    if !carry.is_empty() {
        let _ = sender.send(String::from_utf8_lossy(&carry).into_owned());
    }
}

/// The captured bytes as text, with the omission stated when there was one.
fn truncate_bytes(bytes: &[u8], omitted: usize) -> String {
    let text = String::from_utf8_lossy(bytes).into_owned();
    if omitted == 0 {
        text
    } else {
        format!("{text}\n[truncated: {omitted} bytes omitted]")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A scratch workspace that owns its own directory.
    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("kallisti-tools-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&root);
            fs::create_dir_all(&root).expect("scratch workspace");
            Self { root }
        }

        fn workspace(&self) -> &Path {
            &self.root
        }

        fn file(&self, relative: &str, content: &str) -> PathBuf {
            let path = self.root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("parent");
            }
            fs::write(&path, content).expect("write");
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn a_relative_path_inside_the_workspace_resolves() {
        let scratch = Scratch::new("inside");
        scratch.file("notes/todo.txt", "hello");

        let resolved = super::resolve(scratch.workspace(), "notes/todo.txt").expect("resolves");
        assert_eq!(resolved, scratch.workspace().join("notes/todo.txt"));
    }

    #[test]
    fn a_parent_traversal_out_of_the_workspace_is_refused() {
        let scratch = Scratch::new("traversal");

        let refused = super::resolve(scratch.workspace(), "../../etc/passwd");
        assert!(
            matches!(refused, Err(super::ToolError::Escape { .. })),
            "expected a refusal, got {refused:?}"
        );
    }

    #[test]
    fn an_absolute_path_outside_the_workspace_is_refused() {
        let scratch = Scratch::new("absolute");

        let refused = super::resolve(scratch.workspace(), "/etc/passwd");
        assert!(
            matches!(refused, Err(super::ToolError::Escape { .. })),
            "expected a refusal, got {refused:?}"
        );
    }

    #[test]
    fn a_symlink_pointing_out_of_the_workspace_is_refused() {
        let scratch = Scratch::new("symlink");
        let outside = scratch
            .workspace()
            .parent()
            .expect("parent")
            .join("kallisti-outside.txt");
        fs::write(&outside, "secret").expect("outside file");
        std::os::unix::fs::symlink(&outside, scratch.workspace().join("link")).expect("symlink");

        let refused = super::resolve(scratch.workspace(), "link");
        assert!(
            matches!(refused, Err(super::ToolError::Escape { .. })),
            "expected a refusal, got {refused:?}"
        );
        let _ = fs::remove_file(&outside);
    }

    #[test]
    fn a_path_that_does_not_exist_yet_resolves_inside_the_workspace() {
        let scratch = Scratch::new("newfile");

        let resolved =
            super::resolve(scratch.workspace(), "drafts/new.md").expect("a new file resolves");
        assert_eq!(resolved, scratch.workspace().join("drafts/new.md"));
    }

    fn sandbox(scratch: &Scratch, mode: super::SandboxMode) -> super::Sandbox {
        super::Sandbox::new(scratch.workspace().to_path_buf(), mode)
    }

    #[test]
    fn a_write_is_refused_in_read_only_mode_and_nothing_is_created() {
        let scratch = Scratch::new("readonly");
        let sandbox = sandbox(&scratch, super::SandboxMode::ReadOnly);

        let result = super::execute(
            "write_file",
            &serde_json::json!({"path": "x.txt", "content": "hi"}),
            &sandbox,
        );

        assert!(result.is_error, "{result:?}");
        assert!(
            !scratch.workspace().join("x.txt").exists(),
            "a refused write must not create the file"
        );
    }

    #[test]
    fn a_write_lands_in_workspace_write_mode() {
        let scratch = Scratch::new("write");
        let sandbox = sandbox(&scratch, super::SandboxMode::WorkspaceWrite);

        let result = super::execute(
            "write_file",
            &serde_json::json!({"path": "notes/x.txt", "content": "hi"}),
            &sandbox,
        );

        assert!(!result.is_error, "{result:?}");
        let written = fs::read_to_string(scratch.workspace().join("notes/x.txt")).expect("written");
        assert_eq!(written, "hi");
    }

    #[test]
    fn a_write_outside_the_workspace_is_refused_even_in_workspace_write_mode() {
        let scratch = Scratch::new("writeescape");
        let sandbox = sandbox(&scratch, super::SandboxMode::WorkspaceWrite);

        let result = super::execute(
            "write_file",
            &serde_json::json!({"path": "../escaped.txt", "content": "hi"}),
            &sandbox,
        );

        assert!(result.is_error, "{result:?}");
        assert!(
            !scratch
                .workspace()
                .parent()
                .expect("parent")
                .join("escaped.txt")
                .exists(),
            "nothing may be written outside the workspace"
        );
    }

    #[test]
    fn read_file_returns_the_content() {
        let scratch = Scratch::new("read");
        scratch.file("a.txt", "hello there");
        let sandbox = sandbox(&scratch, super::SandboxMode::ReadOnly);

        let result = super::execute("read_file", &serde_json::json!({"path": "a.txt"}), &sandbox);

        assert!(!result.is_error, "{result:?}");
        assert!(
            result.content.contains("hello there"),
            "the content must come back: {}",
            result.content
        );
    }

    #[test]
    fn list_dir_names_the_entries() {
        let scratch = Scratch::new("list");
        scratch.file("notes/one.txt", "1");
        scratch.file("notes/two.txt", "2");
        let sandbox = sandbox(&scratch, super::SandboxMode::ReadOnly);

        let result = super::execute("list_dir", &serde_json::json!({"path": "notes"}), &sandbox);

        assert!(!result.is_error, "{result:?}");
        assert!(result.content.contains("one.txt"), "{}", result.content);
        assert!(result.content.contains("two.txt"), "{}", result.content);
    }

    #[test]
    fn an_unknown_tool_is_an_error_rather_than_a_silent_success() {
        let scratch = Scratch::new("unknown");
        let sandbox = sandbox(&scratch, super::SandboxMode::ReadOnly);

        let result = super::execute("delete_everything", &serde_json::json!({}), &sandbox);

        assert!(result.is_error, "{result:?}");
        assert!(
            result.content.contains("delete_everything"),
            "the refusal names what was asked for: {}",
            result.content
        );
    }

    #[test]
    fn every_offered_spec_is_executable_by_name() {
        let scratch = Scratch::new("specs");
        scratch.file("probe.txt", "x");
        let sandbox = sandbox(&scratch, super::SandboxMode::WorkspaceWrite);

        for spec in super::specs(super::SandboxMode::WorkspaceWrite) {
            let result = super::execute(&spec.name, &serde_json::json!({}), &sandbox);
            assert!(
                !result.content.contains("unknown tool"),
                "a spec offered to the model must be dispatchable, but {} was not: {}",
                spec.name,
                result.content
            );
        }

        // The same claim in the widest mode, where dispatching is offered. That
        // tool needs a database and a parent turn, so this layer refuses it by
        // name rather than by pretending it does not exist: the turn that offered
        // the spec answers it before it reaches here.
        let wide = super::Sandbox::new(scratch.workspace().to_path_buf(), super::SandboxMode::Full);
        for spec in super::specs(super::SandboxMode::Full) {
            let result = super::execute(&spec.name, &serde_json::json!({}), &wide);
            assert!(
                !result.content.contains("unknown tool"),
                "{} is named but the layer calls it unknown: {}",
                spec.name,
                result.content
            );
        }
    }

    /// Dispatching is a Full-mode capability, and its absence elsewhere is the
    /// mode gate rather than a settings value.
    #[test]
    fn dispatching_is_offered_in_full_mode_and_nowhere_else() {
        let names = |mode: super::SandboxMode| -> Vec<String> {
            super::specs(mode)
                .into_iter()
                .map(|spec| spec.name)
                .collect()
        };
        assert!(
            names(super::SandboxMode::Full).contains(&crate::subagents::TOOL.to_owned()),
            "a Full turn may dispatch"
        );
        for mode in [
            super::SandboxMode::ReadOnly,
            super::SandboxMode::WorkspaceWrite,
        ] {
            assert!(
                !names(mode).contains(&crate::subagents::TOOL.to_owned()),
                "{mode:?} is offered no dispatching"
            );
        }
        // And the operator's switch removes it from the one list it appears in.
        let off: Vec<String> = super::specs_offering(super::SandboxMode::Full, false)
            .into_iter()
            .map(|spec| spec.name)
            .collect();
        assert!(!off.contains(&crate::subagents::TOOL.to_owned()), "{off:?}");
    }

    #[test]
    fn a_fetch_reaches_the_public_web_and_refuses_the_local_one() {
        assert!(super::fetch_allowed("https://example.com/page").is_ok());
        assert!(super::fetch_allowed("http://example.com:8080/x?y=1").is_ok());

        for blocked in [
            "http://localhost:7340/healthz",
            "http://127.0.0.1/",
            "http://10.1.2.3/",
            "http://192.168.1.10/",
            "http://169.254.169.254/latest/meta-data",
            "http://172.16.0.1/",
            "http://printer.local/",
        ] {
            assert!(
                super::fetch_allowed(blocked).is_err(),
                "{blocked} should be refused"
            );
        }
        assert!(
            super::fetch_allowed("ftp://example.com/x").is_err(),
            "only http and https"
        );
        assert!(
            super::fetch_allowed("http://172.32.0.1/").is_ok(),
            "172.32 is public, only 172.16-31 is private"
        );
    }

    #[test]
    fn a_page_becomes_readable_text_without_its_scripts() {
        let html = "<html><head><style>body{color:red}</style>\
                    <script>var secret = 1;</script></head>\
                    <body><h1>Title</h1><p>First &amp; second</p>\
                    <p>third</p></body></html>";
        let text = super::html_to_text(html);
        assert!(text.contains("Title"), "{text}");
        assert!(text.contains("First & second"), "{text}");
        assert!(text.contains("third"), "{text}");
        assert!(!text.contains("var secret"), "scripts are dropped: {text}");
        assert!(!text.contains("color:red"), "styles are dropped: {text}");
        assert!(!text.contains('<'), "no tags survive: {text}");
    }

    #[test]
    fn a_fetch_of_the_local_admin_port_is_refused_before_it_runs() {
        let scratch = Scratch::new("fetchguard");
        let sandbox = sandbox(&scratch, super::SandboxMode::ReadOnly);
        let result = super::execute(
            "fetch_url",
            &serde_json::json!({"url": "http://127.0.0.1:7340/healthz"}),
            &sandbox,
        );
        assert!(result.is_error, "{result:?}");
        assert!(
            result.content.contains("refused"),
            "the refusal says so: {}",
            result.content
        );
    }

    #[test]
    fn an_mcp_result_keeps_its_server_and_its_failure_flag() {
        let ok = super::ToolResult::from_mcp(
            "echo__ping",
            &crate::mcp::protocol::CallOutcome {
                text: "pong".to_owned(),
                is_error: false,
            },
        );
        assert!(!ok.is_error);
        assert!(ok.content.contains("echo__ping"), "{}", ok.content);
        assert!(ok.content.contains("pong"), "{}", ok.content);

        let failed = super::ToolResult::from_mcp(
            "vault__read",
            &crate::mcp::protocol::CallOutcome {
                text: "permission denied".to_owned(),
                is_error: true,
            },
        );
        assert!(
            failed.is_error,
            "a server-marked failure must not read as success"
        );
    }

    #[test]
    fn run_command_is_refused_outside_full_host_mode() {
        let scratch = Scratch::new("execgate");
        let sandbox = sandbox(&scratch, super::SandboxMode::WorkspaceWrite);

        let result = super::execute(
            "run_command",
            &serde_json::json!({"command": "echo hi"}),
            &sandbox,
        );

        assert!(result.is_error, "{result:?}");
        assert!(
            result.content.to_lowercase().contains("full"),
            "the refusal names the mode it needs: {}",
            result.content
        );
    }

    #[test]
    fn run_command_runs_in_the_workspace_and_captures_output() {
        let scratch = Scratch::new("exec");
        let sandbox = sandbox(&scratch, super::SandboxMode::Full);

        let result = super::execute(
            "run_command",
            &serde_json::json!({"command": "echo hello && pwd"}),
            &sandbox,
        );

        assert!(!result.is_error, "{result:?}");
        assert!(result.content.contains("hello"), "{}", result.content);
        assert!(
            result
                .content
                .contains(scratch.workspace().to_string_lossy().as_ref()),
            "the command runs in the workspace: {}",
            result.content
        );
    }

    #[test]
    fn run_command_reports_a_nonzero_exit_instead_of_claiming_success() {
        let scratch = Scratch::new("execfail");
        let sandbox = sandbox(&scratch, super::SandboxMode::Full);

        let result = super::execute(
            "run_command",
            &serde_json::json!({"command": "exit 3"}),
            &sandbox,
        );

        assert!(
            result.is_error,
            "a failed command is not a success: {result:?}"
        );
        assert!(result.content.contains('3'), "{}", result.content);
    }

    #[test]
    fn fetching_is_offered_even_when_the_sandbox_is_narrow() {
        // Reading a public page is a network read, not a filesystem write, so a
        // read-only workspace does not remove it.
        let offered: Vec<String> = super::specs(super::SandboxMode::ReadOnly)
            .into_iter()
            .map(|spec| spec.name)
            .collect();
        assert!(
            offered.contains(&"fetch_url".to_owned()),
            "a read-only turn may still read the web: {offered:?}"
        );
    }

    #[test]
    fn command_execution_is_offered_only_in_full_host_mode() {
        let full: Vec<String> = super::specs(super::SandboxMode::Full)
            .into_iter()
            .map(|spec| spec.name)
            .collect();
        let confined: Vec<String> = super::specs(super::SandboxMode::WorkspaceWrite)
            .into_iter()
            .map(|spec| spec.name)
            .collect();

        assert!(full.contains(&"run_command".to_owned()));
        assert!(
            !confined.contains(&"run_command".to_owned()),
            "a tool the sandbox refuses must not be offered: {confined:?}"
        );
    }
}
