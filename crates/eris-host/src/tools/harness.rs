//! Handing real code work to an external coding harness.
//!
//! The daemon's own tools edit files one call at a time. A coding harness is the
//! other shape of the same work: it is given one task, it runs on its own, and it
//! reports what it did. This module is the seam that lets a turn hand a task over
//! instead of pretending the daemon can do a multi-file change in twelve rounds.
//!
//! Three rules shape it.
//!
//! - **No shell.** The argv is built here and handed to the operating system as
//!   argv. The task is one argument, whatever it contains: spaces, quotes and
//!   semicolons arrive as text the harness reads, never as syntax a shell would
//!   run.
//! - **The operator chooses the harness.** `routing.harness` names one of the
//!   built-in commands, and `routing.harness.command` replaces the built-in
//!   entirely. An absent or unrecognised row is no harness at all, which is the
//!   shipped state.
//! - **A write is a write.** Delegating may change files, so it obeys the same
//!   gate `run_command` does: refused unless the sandbox is `full`.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use core_runtime::provider::ToolSpec;

use super::{
    MAX_COMMAND_OUTPUT_BYTES, Sandbox, SandboxMode, ToolResult, string_arg, truncate_bytes,
};
use crate::db::Db;
use crate::harness_runs::{self, RunStatus};

/// The tool's name, as the model calls it.
pub const TOOL: &str = "delegate_to_harness";

/// `settings` row: which harness a delegated task runs on.
pub const SETTING_HARNESS: &str = "routing.harness";

/// `settings` row: an explicit command line that replaces the built-in one.
pub const SETTING_COMMAND: &str = "routing.harness.command";

/// `settings` row: the model id passed through to the harness.
pub const SETTING_MODEL: &str = "routing.harness.model";

/// `settings` row: where a delegated task runs.
pub const SETTING_CWD: &str = "routing.harness.cwd";

/// `settings` row: how long one delegated task may run.
pub const SETTING_TIMEOUT_SECS: &str = "routing.harness.timeout_secs";

/// How long one delegated task may run when the row is absent or unusable.
pub const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// The shortest timeout an operator may configure.
pub const MIN_TIMEOUT_SECS: u64 = 30;

/// The longest timeout an operator may configure.
pub const MAX_TIMEOUT_SECS: u64 = 3600;

/// The most characters a task may carry, matching the schema's `maxLength`.
pub const MAX_TASK_CHARS: usize = 20_000;

/// One of the harnesses this build knows how to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    /// `dsh -p {task}`
    Dsh,
    /// `claude -p {task}`
    ClaudeCode,
    /// `antigravity run {task}`
    Antigravity,
}

impl Harness {
    /// Read the stored name, or nothing at all.
    ///
    /// `none` and every unrecognised spelling are the shipped state rather than
    /// an error: a settings row that names a harness this build does not have
    /// starts no process, and the tool refuses by name.
    #[must_use]
    pub fn parse(stored: Option<&str>) -> Option<Self> {
        match stored.map(str::trim) {
            Some("dsh") => Some(Self::Dsh),
            Some("claude-code") => Some(Self::ClaudeCode),
            Some("antigravity") => Some(Self::Antigravity),
            _ => None,
        }
    }

    /// The stored spelling, which is also what a result reports.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dsh => "dsh",
            Self::ClaudeCode => "claude-code",
            Self::Antigravity => "antigravity",
        }
    }

    /// The binary the built-in command starts.
    #[must_use]
    pub fn binary(self) -> &'static str {
        match self {
            Self::Dsh => "dsh",
            Self::ClaudeCode => "claude",
            Self::Antigravity => "antigravity",
        }
    }

    /// The words between the binary and the prompt in the built-in command.
    fn prefix(self) -> &'static [&'static str] {
        match self {
            Self::Dsh | Self::ClaudeCode => &[],
            Self::Antigravity => &["run"],
        }
    }

    /// The flag that introduces the prompt, when the harness takes one.
    fn prompt_flag(self) -> &'static [&'static str] {
        match self {
            Self::Dsh | Self::ClaudeCode => &["-p"],
            Self::Antigravity => &[],
        }
    }
}

/// What the operator configured for delegated tasks.
///
/// Built by [`config`] from the settings rows, or directly by a caller that
/// already knows what it wants - which is how a test starts a stub command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Which harness reports the result.
    pub harness: Harness,
    /// An explicit command line, when the operator supplied one.
    pub command: Option<String>,
    /// A model id passed through with `--model`.
    pub model: Option<String>,
    /// Where the task runs, when the operator pinned one.
    pub cwd: Option<PathBuf>,
    /// How long one task may run.
    pub timeout_secs: u64,
}

impl Config {
    /// The timeout as a duration.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(self.timeout_secs)
    }
}

/// Read the harness the daemon would delegate to, or `None` when unconfigured.
///
/// Every read is tolerant on purpose: an unreadable row is no harness at all,
/// because a settings read that fails must not start a process on a guess.
#[must_use]
pub fn config(db: &Db) -> Option<Config> {
    let read = |key: &str| crate::web::store::settings_get(db, key).ok().flatten();
    let harness = Harness::parse(read(SETTING_HARNESS).as_deref())?;
    Some(Config {
        harness,
        command: nonempty(read(SETTING_COMMAND)),
        model: nonempty(read(SETTING_MODEL)),
        cwd: nonempty(read(SETTING_CWD)).map(PathBuf::from),
        timeout_secs: timeout_secs(read(SETTING_TIMEOUT_SECS).as_deref()),
    })
}

/// A stored value, trimmed, with the empty string read as absence.
fn nonempty(stored: Option<String>) -> Option<String> {
    stored
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// The timeout in seconds, read inside the range the form accepts.
///
/// An absent or out-of-range row is [`DEFAULT_TIMEOUT_SECS`], not the nearest
/// edge: a zero or a typo is not an instruction to give a coding harness a minute.
#[must_use]
pub fn timeout_secs(stored: Option<&str>) -> u64 {
    stored
        .map(str::trim)
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|secs| (MIN_TIMEOUT_SECS..=MAX_TIMEOUT_SECS).contains(secs))
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
}

/// The argv for one task, program first, task never interpreted.
///
/// The built-in commands are fixed: a binary, an optional `--model` pair, the
/// harness's own prompt flag, and the task as the last argument. A custom command
/// is split on whitespace once, at startup, and `{task}` is replaced inside a
/// token - so a task with spaces stays one argument. A custom command without
/// `{task}` gets the task appended, which is the only reading of a command line
/// that does not drop the work.
#[must_use]
pub fn argv(config: &Config, task: &str) -> Vec<String> {
    if let Some(command) = config
        .command
        .as_deref()
        .map(str::trim)
        .filter(|command| !command.is_empty())
    {
        let mut argv: Vec<String> = command.split_whitespace().map(str::to_owned).collect();
        let mut substituted = false;
        for argument in &mut argv {
            if argument.contains("{task}") {
                *argument = argument.replace("{task}", task);
                substituted = true;
            }
        }
        if !substituted {
            argv.push(task.to_owned());
        }
        return argv;
    }

    let mut argv = vec![config.harness.binary().to_owned()];
    argv.extend(
        config
            .harness
            .prefix()
            .iter()
            .map(|word| (*word).to_owned()),
    );
    if let Some(model) = config
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        argv.push("--model".to_owned());
        argv.push(model.to_owned());
    }
    argv.extend(
        config
            .harness
            .prompt_flag()
            .iter()
            .map(|word| (*word).to_owned()),
    );
    argv.push(task.to_owned());
    argv
}

/// The tool description the model reads.
#[must_use]
pub fn spec(config: &Config) -> ToolSpec {
    ToolSpec {
        name: TOOL.to_owned(),
        description: format!(
            "Delegate one coding task to the external `{}` harness and return its report. \
             The harness runs on its own with its own tools and may change files in the \
             working directory. When a coding task is delegated here, return the harness's \
             own report: do not re-implement the work yourself afterwards.",
            config.harness.as_str()
        ),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": MAX_TASK_CHARS,
                    "description": "The coding task, in full. It is passed to the harness as one \
                                    argument, never read by a shell."
                },
                "cwd": {
                    "type": ["string", "null"],
                    "description": "Working directory for this task. Defaults to the configured \
                                    directory, then to the daemon's current directory."
                }
            },
            "required": ["task"]
        }),
    }
}

/// The spec, when this turn may actually run one.
///
/// Two conditions, both necessary. A turn is offered the tool only where a
/// command may run at all - the same `full` gate `run_command` has - and only
/// when the operator configured a harness. A tool the daemon would refuse must
/// never reach the model's list.
#[must_use]
pub fn offered(sandbox: &Sandbox) -> Option<ToolSpec> {
    if sandbox.mode() != SandboxMode::Full {
        return None;
    }
    sandbox.harness().map(spec)
}

/// A call the gate refused before anything ran.
fn refusal(code: &str, reason: String) -> ToolResult {
    ToolResult {
        content: serde_json::json!({
            "status": "refused",
            "code": code,
            "error": reason,
        })
        .to_string(),
        is_error: true,
    }
}

/// Run one delegated task under the configured harness.
pub fn run(arguments: &serde_json::Value, sandbox: &Sandbox) -> ToolResult {
    // The write gate first, exactly where `run_command` puts it: a confined turn
    // cannot reach the filesystem through a harness either.
    if sandbox.mode() != SandboxMode::Full {
        return ToolResult::error(format!(
            "delegate_to_harness is refused: the sandbox mode is {} and delegating needs full host",
            sandbox.mode().as_str()
        ));
    }

    let Some(config) = sandbox.harness() else {
        return refusal(
            "harness_not_configured",
            "no external coding harness is configured, so nothing was delegated; set \
             routing.harness to dsh, claude-code or antigravity first"
                .to_owned(),
        );
    };

    let task = match string_arg(arguments, "task") {
        Ok(task) => task,
        Err(error) => return error,
    };
    if task.trim().is_empty() {
        return ToolResult::error("a delegated task cannot be empty");
    }
    if task.chars().count() > MAX_TASK_CHARS {
        return ToolResult::error(format!(
            "the task is {} characters, over the {MAX_TASK_CHARS} character limit",
            task.chars().count()
        ));
    }

    let requested_cwd = arguments
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from);
    let cwd = requested_cwd.or_else(|| config.cwd.clone());
    if let Some(dir) = &cwd
        && !dir.is_dir()
    {
        return refusal(
            "harness_cwd_missing",
            format!(
                "the working directory for this task does not exist: {}",
                dir.display()
            ),
        );
    }

    let argv = argv(config, task);
    let Some(program) = argv.first().cloned() else {
        return refusal(
            "harness_spawn_failed",
            "the configured harness command is empty, so there was no binary to start".to_owned(),
        );
    };
    // The run is registered before its process exists, so a spawn failure is a
    // recorded ending rather than a silence on the monitor's screen. It is
    // `queued` until the pid is attached below.
    let run_id = harness_runs::start(
        config.harness.as_str(),
        task,
        sandbox.conversation_id().map(str::to_owned),
        sandbox.turn_id().map(str::to_owned),
        config.timeout_secs,
    );

    let mut command = std::process::Command::new(&program);
    command
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(dir) = &cwd {
        command.current_dir(dir);
    }
    // Its own group, so a timeout can end the task the harness started rather
    // than only the process this module spawned. A harness that forks a worker
    // and waits on it must not outlive the deadline in the background.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        // The binary name, never the environment: a spawn failure is about one
        // program, and the environment it would have inherited is not the
        // operator's business to read back out of a tool result.
        Err(error) => {
            harness_runs::finish(&run_id, RunStatus::Failed, None);
            return refusal(
                "harness_spawn_failed",
                format!("cannot start the harness binary {program}: {error}"),
            );
        }
    };

    let pid = child.id();
    harness_runs::attach_pid(&run_id, pid);
    // The same readers that fill the tool result also feed the registry: one
    // drain per stream, two consumers, so a monitor never costs the harness a
    // second read of its pipe.
    let stdout = stream(child.stdout.take(), run_id.clone(), Sink::Stdout);
    let stderr = stream(child.stderr.take(), run_id.clone(), Sink::Stderr);

    let timeout = config.timeout();
    let started = Instant::now();
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if started.elapsed() >= timeout {
                    timed_out = true;
                    harness_runs::finish(&run_id, RunStatus::TimedOut, None);
                    let _ = child.kill();
                    harness_runs::terminate_group(pid);
                    break child.wait().ok();
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(error) => {
                harness_runs::finish(&run_id, RunStatus::Failed, None);
                let _ = child.kill();
                harness_runs::terminate_group(pid);
                let _ = child.wait();
                return ToolResult::error(format!("cannot collect the harness process: {error}"));
            }
        }
    };

    // The buffers are shared with the reader threads rather than returned by
    // them, so a killed harness whose child still holds the pipe cannot turn a
    // timeout into a wait on that child. A short grace period collects the tail
    // the pipe had already buffered; past it the snapshot is what arrived.
    let stdout = stdout.finish();
    let stderr = stderr.finish();

    let exit_code = status.and_then(|status| status.code());
    // The run ended on its own. A run an operator killed, or one the watchdog
    // already ended, keeps the ending it was given: the first terminal status is
    // the true one.
    let ending = if timed_out {
        RunStatus::TimedOut
    } else if exit_code == Some(0) {
        RunStatus::Succeeded
    } else {
        RunStatus::Failed
    };
    harness_runs::finish(&run_id, ending, exit_code);
    let body = serde_json::json!({
        "harness": config.harness.as_str(),
        "exit_code": exit_code,
        "timed_out": timed_out,
        "stdout": truncate_bytes(&stdout.kept, stdout.omitted),
        "stderr": truncate_bytes(&stderr.kept, stderr.omitted),
    });
    let failed = timed_out || exit_code != Some(0);
    ToolResult {
        content: body.to_string(),
        is_error: failed,
    }
}

/// One stream, as much as the cap allowed plus how much it dropped.
#[derive(Debug, Default, Clone)]
struct Captured {
    kept: Vec<u8>,
    omitted: usize,
}

/// A stream being drained by its own thread, readable without joining it.
struct Stream {
    snapshot: std::sync::Arc<std::sync::Mutex<Captured>>,
    finished: std::sync::mpsc::Receiver<()>,
}

impl Stream {
    /// The snapshot after a short grace period for the tail of the pipe.
    ///
    /// Five hundred milliseconds is not a budget for the harness; the timeout is.
    /// It is how long a local pipe may take to deliver bytes the kernel already
    /// holds after the process that wrote them is gone.
    fn finish(self) -> Captured {
        let _ = self
            .finished
            .recv_timeout(std::time::Duration::from_millis(500));
        self.snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

/// Which stream one reader is draining.
///
/// The reader does two things with the same bytes and neither is a second read:
/// it fills the tool's capped buffer, and it feeds the run's tail.
#[derive(Debug, Clone, Copy)]
enum Sink {
    Stdout,
    Stderr,
}

impl Sink {
    /// Hand one chunk to the registry.
    fn feed(self, run_id: &str, chunk: &[u8]) {
        match self {
            Self::Stdout => harness_runs::append_stdout(run_id, chunk),
            Self::Stderr => harness_runs::append_stderr(run_id, chunk),
        }
    }
}

/// Start draining one stream to its cap.
///
/// The pipe keeps being read after the cap, because a harness that prints past
/// it must not be stopped by a full pipe: the omission is a fact about the
/// report, not a reason to hang the task.
fn stream(pipe: Option<impl std::io::Read + Send + 'static>, run_id: String, sink: Sink) -> Stream {
    let snapshot = std::sync::Arc::new(std::sync::Mutex::new(Captured::default()));
    let (sender, finished) = std::sync::mpsc::channel();
    let shared = std::sync::Arc::clone(&snapshot);
    std::thread::spawn(move || {
        let Some(mut pipe) = pipe else {
            let _ = sender.send(());
            return;
        };
        let mut buffer = [0u8; 8192];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    sink.feed(&run_id, &buffer[..count]);
                    let mut captured = shared
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let room = MAX_COMMAND_OUTPUT_BYTES.saturating_sub(captured.kept.len());
                    let taken = room.min(count);
                    captured.kept.extend_from_slice(&buffer[..taken]);
                    captured.omitted += count - taken;
                }
                Err(_) => break,
            }
        }
        let _ = sender.send(());
    });
    Stream { snapshot, finished }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    /// A stub harness that records what it was given, then exits as told.
    fn stub(root: &std::path::Path, name: &str, body: &str) -> PathBuf {
        let path = root.join(name);
        let mut file = std::fs::File::create(&path).expect("stub");
        file.write_all(body.as_bytes()).expect("write stub");
        drop(file);
        let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("chmod");
        path
    }

    /// A config that starts `command` as the harness.
    fn stub_config(command: &std::path::Path, harness: Harness) -> Config {
        Config {
            harness,
            command: Some(command.display().to_string()),
            model: None,
            cwd: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        }
    }

    fn full_sandbox(root: &std::path::Path) -> Sandbox {
        Sandbox::new(root.to_path_buf(), SandboxMode::Full)
    }

    fn payload_of(result: &ToolResult) -> serde_json::Value {
        serde_json::from_str(&result.content).expect("a json payload")
    }

    /// The schema is the contract: one required task, one optional directory, and
    /// the documented size limit.
    #[test]
    fn the_tool_schema_is_shaped_as_documented() {
        let config = Config {
            harness: Harness::Dsh,
            command: None,
            model: None,
            cwd: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        };
        let spec = spec(&config);
        assert_eq!(spec.name, "delegate_to_harness");
        assert!(
            spec.description
                .contains("do not re-implement the work yourself"),
            "the description says what to do with the report: {}",
            spec.description
        );
        let parameters = &spec.parameters;
        assert_eq!(parameters["type"], "object");
        assert_eq!(parameters["required"], serde_json::json!(["task"]));
        let task = &parameters["properties"]["task"];
        assert_eq!(task["type"], "string");
        assert_eq!(task["minLength"], 1);
        assert_eq!(task["maxLength"], 20_000);
        let cwd = &parameters["properties"]["cwd"];
        assert_eq!(cwd["type"], serde_json::json!(["string", "null"]));
        assert!(
            parameters["properties"].get("cwd").is_some(),
            "cwd is a property"
        );
    }

    /// A task is one argument. Spaces, quotes and a semicolon arrive as text, and
    /// nothing here ever gives them to a shell.
    #[test]
    fn a_task_with_spaces_quotes_and_a_semicolon_is_one_argument() {
        let task = r#"fix the parser; rm -rf /tmp/never && echo "quoted"  two  spaces"#;
        let config = Config {
            harness: Harness::Dsh,
            command: Some("/usr/local/bin/runner --note {task}".to_owned()),
            model: None,
            cwd: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        };
        let argv = argv(&config, task);
        assert_eq!(
            argv,
            vec![
                "/usr/local/bin/runner".to_owned(),
                "--note".to_owned(),
                task.to_owned()
            ],
            "the task is exactly one argv entry"
        );
        assert_eq!(argv.iter().filter(|argument| *argument == task).count(), 1);
    }

    /// A custom command without a placeholder still gets the task, appended once.
    #[test]
    fn a_custom_command_without_a_placeholder_appends_the_task() {
        let config = Config {
            harness: Harness::Dsh,
            command: Some("my-harness --json".to_owned()),
            model: None,
            cwd: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        };
        assert_eq!(
            argv(&config, "one task"),
            vec!["my-harness", "--json", "one task"]
        );
    }

    /// The built-in commands, with the model pair before the prompt.
    #[test]
    fn the_built_in_commands_carry_the_task_and_the_model_in_order() {
        let mut config = Config {
            harness: Harness::Dsh,
            command: None,
            model: Some("a-model".to_owned()),
            cwd: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        };
        assert_eq!(
            argv(&config, "do it"),
            vec!["dsh", "--model", "a-model", "-p", "do it"]
        );

        config.harness = Harness::ClaudeCode;
        assert_eq!(
            argv(&config, "do it"),
            vec!["claude", "--model", "a-model", "-p", "do it"]
        );

        config.harness = Harness::Antigravity;
        assert_eq!(
            argv(&config, "do it"),
            vec!["antigravity", "run", "--model", "a-model", "do it"]
        );

        config.model = None;
        assert_eq!(argv(&config, "do it"), vec!["antigravity", "run", "do it"]);
    }

    /// A stub proves the parts that only a real process can: stdout, stderr and
    /// the exit code, with the task arriving whole.
    #[test]
    fn a_stub_harness_reports_stdout_stderr_and_its_exit_code() {
        let dir = tempfile::tempdir().expect("temp dir");
        let command = stub(
            dir.path(),
            "stub.sh",
            "#!/bin/sh\n\
             printf 'argc=%s\\n' \"$#\"\n\
             printf 'task=%s\\n' \"$1\"\n\
             printf 'a warning\\n' >&2\n\
             exit 7\n",
        );
        let config = stub_config(&command, Harness::Dsh);
        let sandbox = full_sandbox(dir.path()).with_harness(Some(config));

        let task = "change the file; echo \"not run\"";
        let result = super::super::execute(TOOL, &serde_json::json!({"task": task}), &sandbox);

        assert!(result.is_error, "a nonzero exit is a failure: {result:?}");
        let payload = payload_of(&result);
        assert_eq!(payload["harness"], "dsh");
        assert_eq!(payload["exit_code"], 7);
        assert_eq!(payload["timed_out"], false);
        assert_eq!(
            payload["stdout"],
            format!("argc=1\ntask={task}\n"),
            "the task reached the harness as one argument: {payload}"
        );
        assert_eq!(payload["stderr"], "a warning\n");
    }

    /// A harness that never finishes is killed at the configured timeout and the
    /// record says it timed out.
    #[test]
    fn a_hung_harness_is_killed_and_reported_as_timed_out() {
        let dir = tempfile::tempdir().expect("temp dir");
        let command = stub(dir.path(), "sleepy.sh", "#!/bin/sh\nsleep 30\n");
        let mut config = stub_config(&command, Harness::Dsh);
        config.timeout_secs = 1;
        let sandbox = full_sandbox(dir.path()).with_harness(Some(config));

        let started = Instant::now();
        let result = super::super::execute(TOOL, &serde_json::json!({"task": "wait"}), &sandbox);
        let elapsed = started.elapsed();

        assert!(result.is_error, "a timeout is a failure: {result:?}");
        let payload = payload_of(&result);
        assert_eq!(payload["timed_out"], true);
        assert!(
            payload["exit_code"].is_null(),
            "a killed process has no exit code: {payload}"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "the timeout ended the task rather than waiting for it: {elapsed:?}"
        );
    }

    /// A configured binary that is not on the host is a named refusal, not a
    /// generic spawn error, and the name is the only host fact it states.
    #[test]
    fn a_missing_binary_is_refused_by_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let missing = dir.path().join("definitely-not-here").join("runner");
        let config = stub_config(&missing, Harness::Dsh);
        let sandbox = full_sandbox(dir.path()).with_harness(Some(config));

        let result = super::super::execute(TOOL, &serde_json::json!({"task": "x"}), &sandbox);

        assert!(result.is_error, "{result:?}");
        let payload = payload_of(&result);
        assert_eq!(payload["code"], "harness_spawn_failed");
        assert!(
            payload["error"]
                .as_str()
                .is_some_and(|error| error.contains("runner")),
            "the refusal names the binary: {payload}"
        );
    }

    /// The same write gate `run_command` has: a confined sandbox refuses before
    /// the harness is spawned, so the stub cannot have run.
    #[test]
    fn a_confined_sandbox_refuses_and_the_harness_never_runs() {
        let dir = tempfile::tempdir().expect("temp dir");
        let marker = dir.path().join("ran.txt");
        let command = stub(
            dir.path(),
            "writes.sh",
            &format!("#!/bin/sh\nprintf 'ran\\n' > '{}'\n", marker.display()),
        );
        let config = stub_config(&command, Harness::Dsh);
        let sandbox = Sandbox::new(dir.path().to_path_buf(), SandboxMode::WorkspaceWrite)
            .with_harness(Some(config));

        let result = super::super::execute(TOOL, &serde_json::json!({"task": "x"}), &sandbox);

        assert!(result.is_error, "{result:?}");
        assert!(
            result.content.to_lowercase().contains("full"),
            "the refusal names the mode it needs: {}",
            result.content
        );
        assert!(
            !marker.exists(),
            "a refused delegation must not start the harness"
        );
    }

    /// Nothing configured means nothing offered, and a direct call says why.
    #[test]
    fn an_unconfigured_harness_is_not_offered_and_refuses_by_code() {
        let dir = tempfile::tempdir().expect("temp dir");
        let sandbox = full_sandbox(dir.path());
        assert!(offered(&sandbox).is_none(), "no harness, no tool spec");

        let result = super::super::execute(TOOL, &serde_json::json!({"task": "x"}), &sandbox);
        assert!(result.is_error, "{result:?}");
        assert_eq!(payload_of(&result)["code"], "harness_not_configured");
    }

    /// A configured harness is offered in `full` mode and nowhere else.
    #[test]
    fn the_tool_is_offered_only_when_configured_and_unconfined() {
        let dir = tempfile::tempdir().expect("temp dir");
        let command = stub(dir.path(), "stub.sh", "#!/bin/sh\nexit 0\n");
        let config = stub_config(&command, Harness::Dsh);

        let full = full_sandbox(dir.path()).with_harness(Some(config.clone()));
        assert_eq!(offered(&full).map(|spec| spec.name), Some(TOOL.to_owned()));

        let confined = Sandbox::new(dir.path().to_path_buf(), SandboxMode::WorkspaceWrite)
            .with_harness(Some(config));
        assert!(
            offered(&confined).is_none(),
            "a confined turn is not offered a write tool"
        );
    }

    /// A directory that does not exist is refused before the spawn, by code.
    #[test]
    fn a_missing_working_directory_is_refused_by_code() {
        let dir = tempfile::tempdir().expect("temp dir");
        let command = stub(dir.path(), "stub.sh", "#!/bin/sh\nexit 0\n");
        let mut config = stub_config(&command, Harness::Dsh);
        config.cwd = Some(dir.path().join("absent"));
        let sandbox = full_sandbox(dir.path()).with_harness(Some(config));

        let result = super::super::execute(TOOL, &serde_json::json!({"task": "x"}), &sandbox);
        assert!(result.is_error, "{result:?}");
        assert_eq!(payload_of(&result)["code"], "harness_cwd_missing");
    }

    /// The per-call directory wins over the configured one.
    #[test]
    fn a_per_call_directory_overrides_the_configured_one() {
        let dir = tempfile::tempdir().expect("temp dir");
        let configured = dir.path().join("configured");
        let per_call = dir.path().join("per-call");
        std::fs::create_dir_all(&configured).expect("configured");
        std::fs::create_dir_all(&per_call).expect("per-call");
        let command = stub(dir.path(), "pwd.sh", "#!/bin/sh\npwd\n");
        let mut config = stub_config(&command, Harness::Dsh);
        config.cwd = Some(configured);
        let sandbox = full_sandbox(dir.path()).with_harness(Some(config));

        let result = super::super::execute(
            TOOL,
            &serde_json::json!({"task": "x", "cwd": per_call}),
            &sandbox,
        );

        assert!(!result.is_error, "{result:?}");
        let payload = payload_of(&result);
        assert_eq!(
            payload["stdout"].as_str().map(str::trim),
            Some(per_call.to_string_lossy().as_ref()),
            "the call's directory is the one the harness ran in: {payload}"
        );
    }

    /// The settings rows resolve to a config, and the timeout row is bounded.
    #[test]
    fn the_settings_rows_resolve_and_the_timeout_is_bounded() {
        let db = Db::open_in_memory().expect("in-memory db");
        assert!(super::config(&db).is_none(), "absent is no harness");

        crate::web::store::settings_put(&db, SETTING_HARNESS, "claude-code").expect("harness");
        crate::web::store::settings_put(&db, SETTING_MODEL, "  a-model  ").expect("model");
        crate::web::store::settings_put(&db, SETTING_COMMAND, "   ").expect("command");
        crate::web::store::settings_put(&db, SETTING_TIMEOUT_SECS, "120").expect("timeout");
        let resolved = super::config(&db).expect("configured");
        assert_eq!(resolved.harness, Harness::ClaudeCode);
        assert_eq!(resolved.model.as_deref(), Some("a-model"));
        assert_eq!(resolved.command, None, "an empty command is the built-in");
        assert_eq!(resolved.timeout_secs, 120);

        crate::web::store::settings_put(&db, SETTING_HARNESS, "none").expect("none");
        assert!(super::config(&db).is_none(), "none starts nothing");

        crate::web::store::settings_put(&db, SETTING_HARNESS, "not-a-harness").expect("bogus");
        assert!(
            super::config(&db).is_none(),
            "an unknown name is no harness"
        );

        assert_eq!(timeout_secs(None), DEFAULT_TIMEOUT_SECS);
        assert_eq!(timeout_secs(Some("29")), DEFAULT_TIMEOUT_SECS);
        assert_eq!(timeout_secs(Some("3601")), DEFAULT_TIMEOUT_SECS);
        assert_eq!(timeout_secs(Some("junk")), DEFAULT_TIMEOUT_SECS);
        assert_eq!(timeout_secs(Some("30")), 30);
        assert_eq!(timeout_secs(Some(" 3600 ")), 3600);
    }

    /// The registry sees the run the tool started, fed by the same reader that
    /// fills the tool result, and the ending is recorded with the exit code.
    #[test]
    fn a_delegated_run_is_recorded_in_the_live_registry() {
        let dir = tempfile::tempdir().expect("temp dir");
        let command = stub(
            dir.path(),
            "recorded.sh",
            "#!/bin/sh\nprintf 'started\\n'\nprintf 'a warning\\n' >&2\nexit 3\n",
        );
        let config = stub_config(&command, Harness::Dsh);
        let sandbox = full_sandbox(dir.path()).with_harness(Some(config));
        let marker = "registry-record-marker";
        let task = format!("record this run {marker}");

        let result = super::super::execute(TOOL, &serde_json::json!({"task": task}), &sandbox);
        assert!(result.is_error, "{result:?}");

        let run = crate::harness_runs::snapshot()
            .into_iter()
            .find(|run| run.task.contains(marker))
            .expect("the run is in the registry");
        assert_eq!(run.status, RunStatus::Failed);
        assert_eq!(run.exit_code, Some(3));
        assert_eq!(run.harness, "dsh");
        assert_eq!(run.timeout_secs, DEFAULT_TIMEOUT_SECS);
        assert!(
            run.stdout_tail.contains("started"),
            "stdout reached the registry: {:?}",
            run.stdout_tail
        );
        assert!(
            run.stderr_tail.contains("a warning"),
            "stderr reached the registry: {:?}",
            run.stderr_tail
        );
        assert!(run.finished_at.is_some(), "the ending is recorded");
        assert!(run.conversation_id.is_none(), "this sandbox named no turn");
    }

    /// A deadline ends the run in the registry as well as in the result.
    #[test]
    fn a_timed_out_run_is_recorded_as_timed_out() {
        let dir = tempfile::tempdir().expect("temp dir");
        let command = stub(dir.path(), "slow.sh", "#!/bin/sh\nsleep 30\n");
        let mut config = stub_config(&command, Harness::Dsh);
        config.timeout_secs = 1;
        let sandbox = full_sandbox(dir.path()).with_harness(Some(config));
        let marker = "registry-timeout-marker";

        let result = super::super::execute(
            TOOL,
            &serde_json::json!({"task": format!("wait forever {marker}")}),
            &sandbox,
        );
        assert!(result.is_error, "{result:?}");

        let run = crate::harness_runs::snapshot()
            .into_iter()
            .find(|run| run.task.contains(marker))
            .expect("the run is in the registry");
        assert_eq!(run.status, RunStatus::TimedOut);
        assert_eq!(run.exit_code, None);
        assert_eq!(run.timeout_secs, 1);
    }

    /// A sandbox that names its turn puts the ids on the record.
    #[test]
    fn a_run_carries_the_turn_that_delegated_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let command = stub(dir.path(), "quick.sh", "#!/bin/sh\nexit 0\n");
        let config = stub_config(&command, Harness::Dsh);
        let sandbox = full_sandbox(dir.path())
            .with_harness(Some(config))
            .with_turn_context(Some("conversation-9".to_owned()), Some("turn-9".to_owned()));
        let marker = "registry-turn-marker";

        let result = super::super::execute(
            TOOL,
            &serde_json::json!({"task": format!("a named turn {marker}")}),
            &sandbox,
        );
        assert!(!result.is_error, "{result:?}");

        let run = crate::harness_runs::snapshot()
            .into_iter()
            .find(|run| run.task.contains(marker))
            .expect("the run is in the registry");
        assert_eq!(run.status, RunStatus::Succeeded);
        assert_eq!(run.conversation_id.as_deref(), Some("conversation-9"));
        assert_eq!(run.turn_id.as_deref(), Some("turn-9"));
    }

    /// Output past the cap is kept up to the cap, and the payload states how much
    /// was dropped.
    #[test]
    fn output_past_the_cap_is_truncated_and_says_so() {
        let dir = tempfile::tempdir().expect("temp dir");
        // One byte over the cap, written without a shell builtin that could
        // itself be affected by buffering.
        let over = MAX_COMMAND_OUTPUT_BYTES + 17;
        let command = stub(
            dir.path(),
            "loud.sh",
            &format!("#!/bin/sh\nhead -c {over} /dev/zero | tr '\\0' 'x'\n"),
        );
        let config = stub_config(&command, Harness::Dsh);
        let sandbox = full_sandbox(dir.path()).with_harness(Some(config));

        let result = super::super::execute(TOOL, &serde_json::json!({"task": "x"}), &sandbox);

        assert!(!result.is_error, "{result:?}");
        let payload = payload_of(&result);
        let stdout = payload["stdout"].as_str().expect("stdout");
        let head = "x".repeat(MAX_COMMAND_OUTPUT_BYTES);
        assert!(
            stdout.starts_with(head.as_str()),
            "the head of the output is kept"
        );
        assert!(
            stdout.contains("[truncated: 17 bytes omitted]"),
            "the omission is stated in the payload"
        );
    }
}
