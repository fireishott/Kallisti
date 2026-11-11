//! Background processes: start, poll, wait, log, kill.
//!
//! `run_command` caps a foreground command at thirty seconds and 64 KB, and that
//! cap is why 122 recorded calls on this host read `timed out`. A build, a
//! warehouse refresh or a test suite cannot finish inside it, so the work became
//! a `nohup` shell hack that the daemon could not see, poll or stop.
//!
//! This module is the handle that hack was standing in for. A command started with
//! `background: true` returns immediately with an id and a pid, writes its output
//! to a file under the workspace, and records a `jobs` row of kind `tool` so the
//! work is visible on the page and in the app rather than in a stray shell.
//!
//! Three behaviours are copied from a working agent's process tool because they
//! are what make background work cheap:
//!
//! - **notify-on-complete.** Two consecutive checks that find no new output arm a
//!   flag and say so, so the model stops spending one call per poll.
//! - **signals are explained**, not numbered. `exit -9` becomes a sentence about
//!   the OOM killer, which removes a whole class of confused diagnosis.
//! - **a refusal beats silence.** A foreground command is never auto-backgrounded
//!   because it looked slow; the flag is required and the description says so.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use crate::db::Db;
use crate::tools::ToolResult;

/// The tool name the model calls.
pub const TOOL: &str = "process";

/// How long `wait` blocks when the caller does not say.
pub const DEFAULT_WAIT_MS: u64 = 30_000;

/// The most `wait` will block, so a turn cannot be held for an hour.
pub const MAX_WAIT_MS: u64 = 600_000;

/// The most `log` returns, so a chatty build cannot flood a turn.
pub const MAX_LOG_BYTES: u64 = 200 * 1024;

/// The most bytes a `poll` returns from the tail.
pub const POLL_TAIL_BYTES: u64 = 4 * 1024;

/// How many handles one daemon keeps at a time.
///
/// Each live handle owns one monitor thread, so this is a thread bound as much as
/// a table bound. Sixteen concurrent background builds on one host is already more
/// than this operator runs.
pub const MAX_HANDLES: usize = 16;

/// How many quiet checks arm the notify flag.
const QUIET_CHECKS_TO_NOTIFY: u32 = 2;

/// How long a kill waits for a polite exit before insisting.
const KILL_GRACE: Duration = Duration::from_secs(5);

/// How often a poll, wait or monitor looks at the child.
const TICK: Duration = Duration::from_millis(100);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock means another thread panicked while holding it. The data is
    // a child handle and a couple of counters, so continuing with the last state is
    // better than refusing every later call.
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The tool spec, offered wherever `run_command` is.
#[must_use]
pub fn spec() -> core_runtime::provider::ToolSpec {
    core_runtime::provider::ToolSpec {
        name: TOOL.to_owned(),
        description: "Check on work started with run_command's background flag. \
                      Use `poll` to look, `wait` to block until it finishes, `log` \
                      for more of the output, `list` for everything this \
                      conversation started, and `kill` to stop one. Two quiet \
                      checks turn on notify-on-complete: then stop polling."
            .to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "poll", "wait", "log", "kill"],
                    "description": "What to do with the handle."
                },
                "process_id": {
                    "type": "string",
                    "description": "The id run_command returned when it started the work."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": format!("wait only: how long to block, 1 to {MAX_WAIT_MS}. Defaults to {DEFAULT_WAIT_MS}.")
                }
            },
            "required": ["action"]
        }),
    }
}

/// The live state of one child, behind one lock.
struct Inner {
    /// The child, owned here so `kill` can reach it.
    child: Child,
    /// Set once by whoever reaps it first, the monitor or a wait.
    finished: Option<ExitStatus>,
    /// The log's length at the previous check, for the quiet counter.
    last_seen: u64,
    /// Consecutive checks that found no new output.
    quiet: u32,
    /// Whether the model was told to stop checking.
    notify: bool,
}

/// One started process.
pub struct Handle {
    id: String,
    pid: u32,
    command: String,
    started_at: String,
    log: PathBuf,
    inner: Mutex<Inner>,
}

impl Handle {
    fn status(&self) -> Option<ExitStatus> {
        lock(&self.inner).finished
    }

    fn length(&self) -> u64 {
        std::fs::metadata(&self.log)
            .map(|meta| meta.len())
            .unwrap_or(0)
    }

    /// One check.
    ///
    /// `observed` is whether the *model* looked: the monitor thread checks every
    /// hundred milliseconds so the jobs row is correct on its own, and those
    /// internal checks must not count toward the two-quiet-checks rule. Counting
    /// them armed notify-on-complete before a caller had asked once, which is a
    /// promise the tool had no way to keep.
    fn check(&self, observed: bool) -> (Option<ExitStatus>, bool) {
        let length = self.length();
        let mut inner = lock(&self.inner);
        if inner.finished.is_none()
            && let Ok(Some(status)) = inner.child.try_wait()
        {
            inner.finished = Some(status);
        }
        if inner.finished.is_some() {
            inner.quiet = 0;
            inner.notify = false;
            return (inner.finished, false);
        }
        if length == inner.last_seen {
            if observed {
                inner.quiet = inner.quiet.saturating_add(1);
                if inner.quiet >= QUIET_CHECKS_TO_NOTIFY {
                    inner.notify = true;
                }
            }
        } else {
            inner.quiet = 0;
            inner.last_seen = length;
        }
        (inner.finished, inner.notify)
    }

    /// The tail of the log, at most `limit` bytes.
    fn tail(&self, limit: u64) -> String {
        let Ok(mut file) = std::fs::File::open(&self.log) else {
            return String::new();
        };
        let length = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        let skip = length.saturating_sub(limit);
        if skip > 0 && file.seek(SeekFrom::Start(skip)).is_err() {
            return String::new();
        }
        let mut buffer = Vec::new();
        if file.read_to_end(&mut buffer).is_err() {
            return String::new();
        }
        String::from_utf8_lossy(&buffer).into_owned()
    }
}

/// The handle table: every process this daemon is running, by id.
///
/// Process-wide rather than a field on [`Desk`], and that is deliberate. A turn
/// builds its own sandbox, so a per-turn table would mean a `poll` in the next
/// turn could not see what the previous turn started, and the device routes could
/// not see either. There is one daemon and one operator, so there is one table.
static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, Arc<Handle>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Where background work is started, and the database that records it.
pub struct Desk {
    db: Db,
    dir: PathBuf,
}

impl Desk {
    /// Build a desk whose logs live under the workspace's own directory.
    #[must_use]
    pub fn new(db: Db, workspace: &Path) -> Self {
        Self {
            db,
            dir: workspace.join(".processes"),
        }
    }

    /// Start one command and return its handle immediately.
    ///
    /// # Errors
    ///
    /// A sentence for the model when the directory, the log or the spawn fails,
    /// or when the desk is already full.
    pub fn start(self: &Arc<Self>, command: &str, cwd: &Path) -> Result<serde_json::Value, String> {
        let live = lock(&REGISTRY).len();
        if live >= MAX_HANDLES {
            return Err(format!(
                "{live} background processes are already running, which is the limit; \
                 stop one with `process` action `kill` before starting another"
            ));
        }
        std::fs::create_dir_all(&self.dir)
            .map_err(|error| format!("cannot make the process directory: {error}"))?;
        let id = format!("p{}", uuid::Uuid::now_v7().simple());
        let log = self.dir.join(format!("{id}.log"));
        let file = std::fs::File::create(&log)
            .map_err(|error| format!("cannot open the process log: {error}"))?;
        // `exec 2>&1` merges the two streams in the child so the log reads in the
        // order a terminal would have shown. Only stdout is handed to the file:
        // stderr is redirected onto that same descriptor by the shell rather than
        // opened a second time, which is what keeps the lines in order.
        let child = Command::new("sh")
            .arg("-c")
            .arg(format!("exec 2>&1\n{command}"))
            .current_dir(cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::from(file))
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("cannot start the command: {error}"))?;
        let pid = child.id();
        let started_at = crate::learning::now();
        self.record_start(&id, command, &started_at)?;
        let handle = Arc::new(Handle {
            id: id.clone(),
            pid,
            command: command.to_owned(),
            started_at: started_at.clone(),
            log,
            inner: Mutex::new(Inner {
                child,
                finished: None,
                last_seen: 0,
                quiet: 0,
                notify: false,
            }),
        });
        lock(&REGISTRY).insert(id.clone(), Arc::clone(&handle));
        self.monitor(Arc::clone(&handle));
        Ok(serde_json::json!({
            "process_id": id,
            "pid": pid,
            "started_at": started_at,
            "status": "running",
            "note": "The work runs in the background with no cap. Check it with \
                     `process` action `poll`, or `wait` to block. Do not start it again.",
        }))
    }

    /// Watch one handle to completion on its own thread, so the `jobs` row is
    /// correct even if nothing ever polls it.
    fn monitor(self: &Arc<Self>, handle: Arc<Handle>) {
        let db = self.db.clone();
        std::thread::spawn(move || {
            loop {
                let Some(status) = handle.check(false).0 else {
                    std::thread::sleep(TICK);
                    continue;
                };
                settle_row(&db, &handle.id, status);
                return;
            }
        });
    }

    fn record_start(&self, id: &str, command: &str, started_at: &str) -> Result<(), String> {
        // `kind = 'tool'` is what separates operator-visible background work from
        // the chat jobs the rest of the daemon queues. There is no CHECK constraint
        // on `kind`, so this needs no migration.
        self.db
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO jobs (id, kind, status, created_at, started_at, request_text) \
                     VALUES (?1, 'tool', 'running', ?2, ?2, ?3)",
                    rusqlite::params![id, started_at, command],
                )?;
                Ok(())
            })
            .map_err(|error| format!("cannot record the process: {error}"))?;
        Ok(())
    }

    /// Answer one `process` call.
    #[must_use]
    pub fn call(&self, arguments: &serde_json::Value) -> ToolResult {
        let action = arguments
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim();
        let id = arguments
            .get("process_id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim();
        match action {
            "list" => self.list(),
            "poll" => self.with(id, |handle| self.report(handle, POLL_TAIL_BYTES)),
            "log" => self.with(id, |handle| self.report(handle, MAX_LOG_BYTES)),
            "wait" => {
                let timeout = arguments
                    .get("timeout_ms")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(DEFAULT_WAIT_MS)
                    .clamp(1, MAX_WAIT_MS);
                self.with(id, |handle| self.block(handle, timeout))
            }
            "kill" => self.with(id, |handle| self.kill(handle)),
            other => ToolResult::refused(&format!(
                "`{other}` is not an action this tool has; it has list, poll, wait, log \
                 and kill, so nothing was done"
            )),
        }
    }

    /// Resolve an id to a handle, or say plainly that it is not one of ours.
    fn with(&self, id: &str, run: impl FnOnce(&Arc<Handle>) -> ToolResult) -> ToolResult {
        if id.is_empty() {
            return ToolResult::error("this action needs the `process_id` the start returned");
        }
        match lock(&REGISTRY).get(id).cloned() {
            Some(handle) => run(&handle),
            None => ToolResult::error(format!(
                "no process `{id}` belongs to this daemon; use action `list` to see the ones that do"
            )),
        }
    }

    fn list(&self) -> ToolResult {
        ToolResult::ok(self.snapshot())
    }

    /// Every handle, newest first, for the tool, the route and the page.
    #[must_use]
    pub fn snapshot(&self) -> serde_json::Value {
        let handles: Vec<Arc<Handle>> = lock(&REGISTRY).values().cloned().collect();
        let mut rows: Vec<serde_json::Value> = handles
            .iter()
            .map(|handle| info(handle, handle.check(true).0, handle.inner_notify()))
            .collect();
        // A hash map has no order, and a list that reorders itself between two
        // polls is a list the operator cannot read.
        rows.sort_by(|a, b| b["started_at"].as_str().cmp(&a["started_at"].as_str()));
        let running = rows.iter().filter(|row| row["status"] == "running").count();
        serde_json::json!({
            "processes": rows,
            "running": running,
            "total": rows.len(),
        })
    }

    /// Stop one by id, for the route that has no arguments object.
    #[must_use]
    pub fn kill_by_id(&self, id: &str) -> serde_json::Value {
        let result = self.with(id, |handle| self.kill(handle));
        serde_json::from_str(&result.content)
            .unwrap_or_else(|_| serde_json::json!({"error": result.content}))
    }

    fn report(&self, handle: &Arc<Handle>, tail_bytes: u64) -> ToolResult {
        let (status, notify) = handle.check(true);
        if let Some(status) = status {
            // Whoever observes the end writes it down, so the row is correct even
            // when the monitor thread is descheduled behind a busy turn.
            settle_row(&self.db, &handle.id, status);
        }
        let mut body = info(handle, status, notify);
        body["output"] = serde_json::Value::String(handle.tail(tail_bytes));
        if notify {
            body["note"] = serde_json::Value::String(
                "Nothing new has arrived twice in a row. notify_on_complete is on: stop \
                 checking and the daemon will report the finish. Do not poll again."
                    .to_owned(),
            );
        }
        ToolResult::ok(body)
    }

    fn block(&self, handle: &Arc<Handle>, timeout_ms: u64) -> ToolResult {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        loop {
            let (status, notify) = handle.check(true);
            if let Some(status) = status {
                settle_row(&self.db, &handle.id, status);
                return ToolResult::ok(finished_body(handle, status, handle.tail(POLL_TAIL_BYTES)));
            }
            if Instant::now() >= deadline {
                let mut body = info(handle, None, notify);
                body["status"] = serde_json::Value::String("running".to_owned());
                body["timed_out"] = serde_json::Value::Bool(true);
                body["output"] = serde_json::Value::String(handle.tail(POLL_TAIL_BYTES));
                body["note"] = serde_json::Value::String(format!(
                    "Still running after {}s. Call `wait` again, or check `log` for progress.",
                    timeout_ms / 1000
                ));
                return ToolResult::ok(body);
            }
            std::thread::sleep(TICK);
        }
    }

    fn kill(&self, handle: &Arc<Handle>) -> ToolResult {
        if let Some(status) = handle.status() {
            return ToolResult::ok(finished_body(handle, status, String::new()));
        }
        // Ask first. `sh -c "kill -TERM <pid>"` uses the shell's own builtin, so
        // this needs no signal crate and no unsafe block; the shell that runs the
        // command was replaced by it with `exec`, so the pid is the command's.
        let _ = Command::new("sh")
            .arg("-c")
            .arg(format!("kill -TERM {} 2>/dev/null", handle.pid))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let deadline = Instant::now() + KILL_GRACE;
        loop {
            if let Some(status) = handle.check(true).0 {
                settle_row(&self.db, &handle.id, status);
                let mut body = finished_body(handle, status, String::new());
                body["killed"] = serde_json::Value::Bool(true);
                body["note"] = serde_json::Value::String(
                    "SIGTERM stopped it: the process ended within the five-second grace."
                        .to_owned(),
                );
                return ToolResult::ok(body);
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(TICK);
        }
        // Then insist. This is the direct child; a grandchild that escaped the
        // group is not tracked, and the result says what was done rather than
        // claiming the whole tree is gone.
        let killed = {
            let mut inner = lock(&handle.inner);
            inner.child.kill().is_ok()
        };
        let status = self.await_exit(handle);
        let mut body = match status {
            Some(status) => finished_body(handle, status, String::new()),
            None => info(handle, None, false),
        };
        body["killed"] = serde_json::Value::Bool(true);
        body["note"] = serde_json::Value::String(if killed {
            "SIGTERM was ignored for five seconds, so the process was killed with SIGKILL."
                .to_owned()
        } else {
            "The process could not be signalled; it may already be gone.".to_owned()
        });
        if let Some(status) = status {
            settle_row(&self.db, &handle.id, status);
        }
        ToolResult::ok(body)
    }

    fn await_exit(&self, handle: &Arc<Handle>) -> Option<ExitStatus> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(status) = handle.check(true).0 {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(TICK);
        }
    }
}

impl Handle {
    fn inner_notify(&self) -> bool {
        lock(&self.inner).notify
    }
}

/// What one handle looks like on the wire.
fn info(handle: &Handle, status: Option<ExitStatus>, notify: bool) -> serde_json::Value {
    let mut body = serde_json::json!({
        "process_id": handle.id,
        "pid": handle.pid,
        "command": handle.command,
        "started_at": handle.started_at,
        "bytes": handle.length(),
        "notify_on_complete": notify,
    });
    match status {
        Some(status) => {
            body["status"] = serde_json::Value::String(if status.success() {
                "succeeded".to_owned()
            } else {
                "failed".to_owned()
            });
            body["exit_code"] = serde_json::json!(status.code());
            body["signal"] = serde_json::json!(signal_of(status));
            body["signal_note"] = serde_json::json!(explain(status));
        }
        None => {
            body["status"] = serde_json::Value::String("running".to_owned());
        }
    }
    body
}

/// The finished shape, with the explanation the model needs to stop guessing.
fn finished_body(handle: &Handle, status: ExitStatus, output: String) -> serde_json::Value {
    let mut body = info(handle, Some(status), false);
    body["output"] = serde_json::Value::String(output);
    body
}

/// The signal that ended a child, when one did.
#[must_use]
pub fn signal_of(status: ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

/// What a signal death means, in a sentence rather than a number.
///
/// `exit -9` and `exit 137` are the same fact, and neither tells a model why the
/// work stopped. Naming the signal, and the likely cause, removes a whole class of
/// confused diagnosis where a killed command is read as a bug in the command.
#[must_use]
pub fn explain_signal(signal: i32) -> Option<&'static str> {
    let text = match signal {
        9 => {
            "terminated by signal 9: SIGKILL - often the kernel OOM killer on memory \
             exhaustion, or an explicit kill -9"
        }
        15 => {
            "terminated by signal 15: SIGTERM - asked to stop, usually by the operator or a supervisor"
        }
        11 => "terminated by signal 11: SIGSEGV - the program dereferenced a bad pointer",
        6 => {
            "terminated by signal 6: SIGABRT - the program aborted itself, often a failed assertion"
        }
        13 => "terminated by signal 13: SIGPIPE - it wrote to a pipe whose reader had gone",
        25 => "terminated by signal 25: SIGXFSZ - it wrote more than the file-size limit allows",
        24 => "terminated by signal 24: SIGXCPU - it used more CPU time than the limit allows",
        2 => "terminated by signal 2: SIGINT - interrupted",
        _ => return None,
    };
    Some(text)
}

/// The explanation for a finished status, when the status needs explaining.
#[must_use]
pub fn explain(status: ExitStatus) -> Option<String> {
    if let Some(signal) = signal_of(status) {
        return Some(match explain_signal(signal) {
            Some(text) => text.to_owned(),
            None => format!("terminated by signal {signal}"),
        });
    }
    // A shell reports a signalled child as 128 + the signal, so 137 means the same
    // thing as a signal 9 death and is explained the same way.
    let code = status.code()?;
    if let Some(signal) = (code > 128).then(|| code - 128)
        && let Some(text) = explain_signal(signal)
    {
        return Some(format!("exit {code}: {text}"));
    }
    None
}

/// Write the terminal state of one handle into its `jobs` row.
fn settle_row(db: &Db, id: &str, status: ExitStatus) {
    let code = status.code();
    let note = explain(status);
    let (state, error_code, error_message) = if status.success() {
        ("succeeded", None, None)
    } else if let Some(signal) = signal_of(status) {
        ("failed", Some(format!("signal_{signal}")), note)
    } else {
        ("failed", Some(format!("exit_{}", code.unwrap_or(-1))), note)
    };
    let _ = db.with_conn(|conn| {
        conn.execute(
            "UPDATE jobs SET status = ?2, finished_at = ?3, error_code = ?4, error_message = ?5 \
              WHERE id = ?1",
            rusqlite::params![id, state, crate::learning::now(), error_code, error_message],
        )?;
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desk() -> (tempfile::TempDir, Arc<Desk>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open_in_memory().expect("a database");
        let desk = Arc::new(Desk::new(db, dir.path()));
        (dir, desk)
    }

    fn parse(result: &ToolResult) -> serde_json::Value {
        serde_json::from_str(&result.content).expect("a JSON body")
    }

    fn id_of(body: &serde_json::Value) -> String {
        body["process_id"]
            .as_str()
            .expect("a process id")
            .to_owned()
    }

    fn job_row(desk: &Desk, id: &str) -> (String, Option<String>) {
        desk.db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT status, error_code FROM jobs WHERE id = ?1",
                    rusqlite::params![id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
            })
            .expect("the job row")
    }

    #[test]
    fn a_background_command_returns_at_once_and_settles_into_its_job_row() {
        let (dir, desk) = desk();
        let started = Instant::now();
        let body = desk
            .start("sleep 0.3; echo done", dir.path())
            .expect("it started");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the handle comes back before the work ends"
        );
        assert_eq!(body["status"], "running");
        let id = id_of(&body);
        assert!(body["pid"].as_u64().unwrap_or(0) > 0);

        let waited = desk.call(&serde_json::json!({
            "action": "wait",
            "process_id": id,
            "timeout_ms": 20_000
        }));
        assert!(!waited.is_error, "{}", waited.content);
        let payload = parse(&waited);
        assert_eq!(payload["status"], "succeeded");
        assert_eq!(payload["exit_code"], 0);
        assert!(
            payload["output"]
                .as_str()
                .unwrap_or_default()
                .contains("done"),
            "the log is the output: {}",
            payload["output"]
        );

        let (status, code) = job_row(&desk, &id);
        assert_eq!(status, "succeeded", "the jobs row records the work");
        assert_eq!(code, None);
    }

    #[test]
    fn a_failing_command_is_a_failure_with_its_exit_code() {
        let (dir, desk) = desk();
        let body = desk.start("exit 3", dir.path()).expect("it started");
        let id = id_of(&body);
        let waited = desk.call(&serde_json::json!({
            "action": "wait",
            "process_id": id,
            "timeout_ms": 20_000
        }));
        let payload = parse(&waited);
        assert_eq!(payload["status"], "failed");
        assert_eq!(payload["exit_code"], 3);
        let (status, code) = job_row(&desk, &id);
        assert_eq!(status, "failed");
        assert_eq!(code.as_deref(), Some("exit_3"));
    }

    #[test]
    fn two_quiet_polls_arm_notify_on_complete() {
        let (dir, desk) = desk();
        // A command that prints nothing, so every check is a quiet one.
        let body = desk.start("sleep 30", dir.path()).expect("it started");
        let id = id_of(&body);

        let first = parse(&desk.call(&serde_json::json!({
            "action": "poll",
            "process_id": id
        })));
        assert_eq!(first["status"], "running");
        assert_eq!(first["notify_on_complete"], false);

        let second = parse(&desk.call(&serde_json::json!({
            "action": "poll",
            "process_id": id
        })));
        assert_eq!(second["notify_on_complete"], true);
        assert!(
            second["note"].as_str().unwrap_or_default().contains("stop"),
            "the answer tells the model to stop checking: {}",
            second["note"]
        );

        let _ = desk.call(&serde_json::json!({"action": "kill", "process_id": id}));
    }

    #[test]
    fn kill_stops_a_long_command_and_says_what_happened() {
        let (dir, desk) = desk();
        let body = desk.start("sleep 30", dir.path()).expect("it started");
        let id = id_of(&body);
        let killed = desk.call(&serde_json::json!({"action": "kill", "process_id": id}));
        assert!(!killed.is_error, "{}", killed.content);
        let payload = parse(&killed);
        assert_eq!(payload["killed"], true);
        assert_ne!(payload["status"], "running");
        let (status, _) = job_row(&desk, &id);
        assert_ne!(status, "running", "the row follows the process");
    }

    #[test]
    fn an_unknown_id_is_refused_by_name_and_the_list_has_a_shape() {
        let (_dir, desk) = desk();
        let listed = parse(&desk.call(&serde_json::json!({"action": "list"})));
        // Not "empty": the handle table is process-wide, so a sibling test in this
        // binary may be running work at the same moment.
        assert!(
            listed["processes"].is_array(),
            "the list has rows: {listed}"
        );
        assert!(listed["total"].as_u64().is_some(), "{listed}");
        let missing = desk.call(&serde_json::json!({
            "action": "poll",
            "process_id": "pnope"
        }));
        assert!(missing.is_error);
        assert!(missing.content.contains("pnope"), "{}", missing.content);
        let nonsense = desk.call(&serde_json::json!({"action": "delete"}));
        assert!(nonsense.is_error);
        assert!(nonsense.content.contains("nothing was done"));
    }

    #[test]
    fn a_wait_that_times_out_says_so_instead_of_failing() {
        let (dir, desk) = desk();
        let body = desk.start("sleep 30", dir.path()).expect("it started");
        let id = id_of(&body);
        let waited = desk.call(&serde_json::json!({
            "action": "wait",
            "process_id": id,
            "timeout_ms": 200
        }));
        assert!(!waited.is_error, "a timeout is not an error");
        let payload = parse(&waited);
        assert_eq!(payload["timed_out"], true);
        assert_eq!(payload["status"], "running");
        let _ = desk.call(&serde_json::json!({"action": "kill", "process_id": id}));
    }

    #[test]
    fn signals_are_explained_in_words() {
        let killed = explain_signal(9).expect("SIGKILL has a sentence");
        assert!(killed.contains("SIGKILL"), "{killed}");
        assert!(
            killed.contains("OOM"),
            "the likely cause is named: {killed}"
        );
        assert!(explain_signal(15).unwrap_or_default().contains("SIGTERM"));
        assert!(explain_signal(11).unwrap_or_default().contains("SIGSEGV"));
        assert_eq!(explain_signal(64), None, "an unnamed signal stays unnamed");
    }

    #[test]
    fn a_shell_reported_signal_death_is_explained_from_its_code() {
        use std::os::unix::process::ExitStatusExt;
        // A shell reports a signalled child as 128 + the signal.
        let status = ExitStatus::from_raw(9);
        assert!(explain(status).unwrap_or_default().contains("SIGKILL"));
        let code = ExitStatus::from_raw(137 << 8);
        assert!(
            explain(code).unwrap_or_default().contains("137"),
            "the 128+signal form is explained too"
        );
    }

    #[test]
    fn background_without_a_desk_is_refused_rather_than_silently_foregrounded() {
        let dir = tempfile::tempdir().expect("temp dir");
        let sandbox =
            crate::tools::Sandbox::new(dir.path().to_path_buf(), crate::tools::SandboxMode::Full);
        let result = crate::tools::execute(
            "run_command",
            &serde_json::json!({"command": "echo hi", "background": true}),
            &sandbox,
        );
        assert!(result.is_error, "{}", result.content);
        assert!(
            result.content.contains("process desk"),
            "{}",
            result.content
        );
    }
}
