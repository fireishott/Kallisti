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
/// Bounded because each round is a provider call that bills, and a number typed
/// with a stray zero should be refused rather than obeyed.
pub const MAX_TOOL_ROUNDS_CEILING: u32 = 40;

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
    fn ok(payload: serde_json::Value) -> Self {
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
                "message": "This write is waiting for the operator to approve it. \
                            Nothing has been written yet.",
            })
            .to_string(),
            is_error: false,
        }
    }
}

/// The tools offered to the model this build.
///
/// Three, deliberately. A file tool that works beats a catalogue of half-built
/// ones, and every name here is dispatchable by [`execute`] - a spec the model
/// can call but the daemon cannot run is the bug this module exists to prevent.
#[must_use]
pub fn specs(mode: SandboxMode) -> Vec<ToolSpec> {
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
    }

    offered
}

/// Run one call. Never panics and never returns success for a failure.
pub fn execute(name: &str, arguments: &serde_json::Value, sandbox: &Sandbox) -> ToolResult {
    match name {
        "list_dir" => list_dir(arguments, sandbox),
        "read_file" => read_file(arguments, sandbox),
        "write_file" => write_file(arguments, sandbox),
        "run_command" => run_command(arguments, sandbox),
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

fn run_command(arguments: &serde_json::Value, sandbox: &Sandbox) -> ToolResult {
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
    let output = std::process::Command::new("timeout")
        .arg(format!("{COMMAND_TIMEOUT_SECS}s"))
        .arg("sh")
        .arg("-c")
        .arg(command)
        .current_dir(sandbox.workspace())
        .output();

    let output = match output {
        Ok(output) => output,
        Err(error) => return ToolResult::error(format!("cannot run the command: {error}")),
    };

    let stdout = truncate(&output.stdout);
    let stderr = truncate(&output.stderr);
    let exit_code = output.status.code();

    let body = serde_json::json!({
        "command": command,
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": stderr,
    });

    if output.status.success() {
        ToolResult::ok(body)
    } else {
        // A non-zero exit is a failure with evidence attached, never a success
        // the model has to notice on its own.
        ToolResult {
            content: body.to_string(),
            is_error: true,
        }
    }
}

/// Trim captured output to the budget, saying so when it did.
fn truncate(bytes: &[u8]) -> String {
    let limit = MAX_COMMAND_OUTPUT_BYTES.min(bytes.len());
    let text = String::from_utf8_lossy(&bytes[..limit]).into_owned();
    if bytes.len() > MAX_COMMAND_OUTPUT_BYTES {
        format!("{text}\n[truncated: {} bytes omitted]", bytes.len() - limit)
    } else {
        text
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
