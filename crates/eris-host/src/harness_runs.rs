//! A live registry of the coding-harness runs this daemon has started.
//!
//! A delegated task is the one piece of work this daemon hands to another
//! process and then does not look at again until it ends. Before this module the
//! only record of it was a tool result: a caller that wanted to know whether a
//! build was still going, how long the harness had been quiet, or what it had
//! printed so far had to be the turn that started it. Nothing else on the host
//! could see it at all.
//!
//! So this is the table behind `GET /v1/harness/runs`: one record per run, in
//! memory, bounded, with the last 8 KiB of each stream kept so a monitor can read
//! the tail without the daemon retaining a whole build log.
//!
//! Three rules shape it.
//!
//! - **Nothing here starts, stops or reads a process on its own.** The tool
//!   registers a run and feeds it the bytes it was already reading. The registry
//!   records; it does not duplicate the reader, and there is exactly one
//!   implementation of ending a harness process group, [`terminate_group`],
//!   shared by the tool's deadline, the watchdog and the kill route.
//! - **A stall is not a failure.** A harness that runs a long test suite prints
//!   nothing for minutes and is not broken. Silence only changes how the run
//!   reads, never whether it lives: the operator's timeout is what ends a run.
//! - **A quiet run recovers.** Output after a stall puts the run back to
//!   `running`, so a monitor never has to clear a flag by hand.
//!
//! The table is bounded at [`MAX_RUNS`], oldest evicted first. A daemon that ran
//! a thousand delegations overnight keeps the last fifty; the transcript of each
//! turn still holds the rest.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::broadcast;

use crate::auth::{format_timestamp, now};

/// How many runs the registry keeps. The oldest is evicted to make room.
pub const MAX_RUNS: usize = 50;

/// The most of one stream a run keeps: the tail, not the log.
pub const TAIL_BYTES: usize = 8 * 1024;

/// The most characters of the task a run keeps.
///
/// The task itself is already in the turn that delegated it; this is one line a
/// monitor can render in a list, not a second answer.
pub const TASK_CHARS: usize = 200;

/// How often the watchdog looks at a run.
pub const WATCHDOG_TICK: Duration = Duration::from_secs(5);

/// How long a run may produce nothing before it reads as stalled.
///
/// Ninety seconds: longer than any single command a harness runs quietly (a
/// compile, a migration) and shorter than an operator's patience with a screen
/// that says nothing.
pub const STALL_AFTER: Duration = Duration::from_secs(90);

/// How many events the channel buffers before a slow reader is told it lagged.
const EVENT_BUFFER: usize = 256;

/// Where a run is in its life.
///
/// `stalled` is a running run that has gone quiet, not an ended one: it carries
/// no exit code and the process is still there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// Registered, the process not started or not yet attached.
    Queued,
    /// The process is alive.
    Running,
    /// The process is alive and has produced nothing for longer than the
    /// registry's silence threshold.
    Stalled,
    /// Exited zero.
    Succeeded,
    /// Exited non-zero, or could not be started at all.
    Failed,
    /// Ran past its deadline and was ended.
    TimedOut,
    /// An operator asked for it to stop.
    Killed,
}

impl RunStatus {
    /// The wire spelling, which is also what the tool result uses.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Stalled => "stalled",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::Killed => "killed",
        }
    }

    /// Whether this is an ending rather than a state a run can leave.
    ///
    /// A terminal run keeps its status: the first ending recorded is the true
    /// one, so a kill that lands while a timeout is being written down cannot
    /// rewrite the other.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::TimedOut | Self::Killed
        )
    }
}

/// One delegated task, as this process remembers it.
///
/// The timing fields are [`Instant`]s rather than the wall clock: an operator
/// moving the host's clock must not make a run look like it elapsed in negative
/// time or has been silent since before it started. `started_at` and
/// `finished_at` are the wall-clock strings a client shows.
#[derive(Debug, Clone)]
pub struct HarnessRun {
    /// The run's id, and the `{id}` in its routes.
    pub id: String,
    /// The harness name the operator configured (`dsh`, `claude-code`, ...).
    pub harness: String,
    /// The first [`TASK_CHARS`] characters of the task, on one line.
    pub task: String,
    /// Where the run is in its life.
    pub status: RunStatus,
    /// When the run was registered, RFC 3339.
    pub started_at: String,
    /// When it ended, RFC 3339, or `None` while it is still going.
    pub finished_at: Option<String>,
    /// The exit code the process reported, when it reported one.
    pub exit_code: Option<i32>,
    /// The last [`TAIL_BYTES`] bytes of stdout, lossily decoded.
    pub stdout_tail: String,
    /// The last [`TAIL_BYTES`] bytes of stderr, lossily decoded.
    pub stderr_tail: String,
    /// The conversation that delegated this run, when the turn knew one.
    pub conversation_id: Option<String>,
    /// The turn that delegated this run, when the turn knew one.
    pub turn_id: Option<String>,
    /// How long this run was allowed to take.
    pub timeout_secs: u64,
    /// The process-group leader, once the process exists.
    ///
    /// Private on purpose: it is how this module ends a run, not a thing a route
    /// should name. A client that wants a run stopped asks for that.
    pub(crate) pid: Option<u32>,
    /// The monotonic instant the run was registered.
    started: Instant,
    /// The monotonic instant output last arrived.
    last_output: Instant,
    /// The monotonic instant the run ended, when it has.
    finished: Option<Instant>,
}

impl HarnessRun {
    /// How long ago this run last produced anything.
    ///
    /// Measured against the ending once the run has ended, so a finished record
    /// states the silence it ended with rather than growing a number nobody can
    /// act on.
    #[must_use]
    pub fn silent_ms(&self) -> u64 {
        let reference = self.finished.unwrap_or_else(Instant::now);
        millis(
            reference
                .saturating_duration_since(self.last_output)
                .as_millis(),
        )
    }

    /// How long the run has been going, frozen at its ending once it has ended.
    #[must_use]
    pub fn elapsed_ms(&self) -> u64 {
        let reference = self.finished.unwrap_or_else(Instant::now);
        millis(
            reference
                .saturating_duration_since(self.started)
                .as_millis(),
        )
    }

    /// The list item `GET /v1/harness/runs` answers, and the data of every SSE
    /// frame: one definition, so a monitor cannot read two shapes for one fact.
    #[must_use]
    pub fn list_item(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "harness": self.harness,
            "status": self.status.as_str(),
            "task": self.task,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "elapsed_ms": self.elapsed_ms(),
            "silent_ms": self.silent_ms(),
            "exit_code": self.exit_code,
            "stalled": self.status == RunStatus::Stalled,
        })
    }

    /// The list item plus the two tails, the deadline, and the turn the run
    /// belongs to, which is what `GET /v1/harness/runs/{id}` answers.
    ///
    /// The two ids are here and not in the list item because linking a run back
    /// to its chat is what a reader does after it has picked the run: the list is
    /// a table, the detail is the record.
    #[must_use]
    pub fn detail(&self) -> serde_json::Value {
        let mut item = self.list_item();
        if let Some(map) = item.as_object_mut() {
            map.insert(
                "stdout".to_owned(),
                serde_json::Value::String(self.stdout_tail.clone()),
            );
            map.insert(
                "stderr".to_owned(),
                serde_json::Value::String(self.stderr_tail.clone()),
            );
            map.insert("timeout_secs".to_owned(), self.timeout_secs.into());
            map.insert(
                "conversation_id".to_owned(),
                self.conversation_id
                    .clone()
                    .map_or(serde_json::Value::Null, serde_json::Value::String),
            );
            map.insert(
                "turn_id".to_owned(),
                self.turn_id
                    .clone()
                    .map_or(serde_json::Value::Null, serde_json::Value::String),
            );
        }
        item
    }
}

/// One state change, as the SSE route fans it out.
///
/// The payload is the list item itself rather than a second, thinner event
/// shape: a monitor that renders the list and a monitor that renders the stream
/// read one document.
#[derive(Debug, Clone)]
pub struct RunEvent {
    /// The run this change is about.
    pub id: String,
    /// The run's list item at the moment of the change.
    pub item: serde_json::Value,
}

/// What one kill request did.
#[derive(Debug, Clone)]
pub enum KillOutcome {
    /// No run carries that id.
    Unknown,
    /// The run had already ended, so nothing was signalled.
    Finished(HarnessRun),
    /// The process group was signalled and the run is recorded as killed.
    Requested(HarnessRun),
}

/// What a watchdog pass decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// Keep watching.
    Continue,
    /// Stop: the run ended, or it is no longer in the table.
    Stop,
}

/// The process-wide registry, and the channel its state changes are published on.
///
/// Cloned by handle, not by value: the watchdog thread and every route see the
/// same table and the same channel.
#[derive(Debug, Clone)]
pub struct Registry {
    inner: Arc<Inner>,
}

/// The state one [`Registry`] shares with its watchdog and its readers.
#[derive(Debug)]
struct Inner {
    runs: Mutex<VecDeque<HarnessRun>>,
    events: broadcast::Sender<RunEvent>,
    /// How often the watchdog looks at a run.
    tick: Duration,
    /// How long a run may be silent before the watchdog calls it stalled.
    stall_after_ms: u64,
}

impl Registry {
    /// A registry with the shipped watchdog timings.
    #[must_use]
    pub fn new() -> Self {
        Self::with_timing(WATCHDOG_TICK, STALL_AFTER)
    }

    /// A registry with the watchdog timings a caller chooses.
    ///
    /// The timings are per registry rather than process-wide so a test can
    /// shorten them without changing what a real daemon does, and without one
    /// test's threshold leaking into another's.
    #[must_use]
    pub fn with_timing(tick: Duration, stall_after: Duration) -> Self {
        let (events, _) = broadcast::channel(EVENT_BUFFER);
        Self {
            inner: Arc::new(Inner {
                runs: Mutex::new(VecDeque::new()),
                events,
                tick,
                stall_after_ms: millis(stall_after.as_millis()),
            }),
        }
    }

    /// The channel every state change is published on.
    #[must_use]
    pub fn events(&self) -> broadcast::Sender<RunEvent> {
        self.inner.events.clone()
    }

    /// A receiver that hears every state change from now on.
    ///
    /// Live from the moment it is made: the registry does not replay, so a
    /// monitor that needs the current state reads the list first and then
    /// subscribes.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<RunEvent> {
        self.inner.events.subscribe()
    }

    /// Register one run, spawn its watchdog, and return its id.
    ///
    /// The run exists before its process does, so a spawn failure is a recorded
    /// ending rather than a silence. It is `queued` until [`Registry::attach_pid`]
    /// names the process.
    pub fn start(
        &self,
        harness: &str,
        task: &str,
        conversation_id: Option<String>,
        turn_id: Option<String>,
        timeout_secs: u64,
    ) -> String {
        let id = uuid::Uuid::now_v7().to_string();
        let at = Instant::now();
        let run = HarnessRun {
            id: id.clone(),
            harness: harness.to_owned(),
            task: summary(task),
            status: RunStatus::Queued,
            started_at: format_timestamp(now()),
            finished_at: None,
            exit_code: None,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            conversation_id,
            turn_id,
            timeout_secs,
            pid: None,
            started: at,
            last_output: at,
            finished: None,
        };
        {
            let mut runs = lock(&self.inner.runs);
            // The bound is the table's, not the run's: the oldest record goes,
            // ended or not, so a wedged run cannot push out every later one.
            while runs.len() >= MAX_RUNS {
                runs.pop_front();
            }
            runs.push_back(run.clone());
        }
        self.publish(&run);
        self.spawn_watchdog(&id);
        id
    }

    /// Attach the process a queued run started, moving it to `running`.
    ///
    /// Two endings are handled here. A run that already ended keeps the status it
    /// ended with, because a process id arriving after a kill does not make the
    /// run live again. But the process it names is real: a run asked to stop
    /// before its process existed must not leave that process running, so the
    /// signal goes out now.
    pub fn attach_pid(&self, id: &str, pid: u32) {
        let (running, ended) = {
            let mut runs = lock(&self.inner.runs);
            let Some(run) = runs.iter_mut().find(|run| run.id == id) else {
                return;
            };
            run.pid = Some(pid);
            if run.status.is_terminal() {
                (None, true)
            } else {
                run.status = RunStatus::Running;
                run.last_output = Instant::now();
                (Some(run.clone()), false)
            }
        };
        if ended {
            terminate_group(pid);
            return;
        }
        if let Some(run) = running {
            self.publish(&run);
        }
    }

    /// Add one stdout chunk to a run's tail.
    pub fn append_stdout(&self, id: &str, chunk: &[u8]) {
        self.append(id, chunk, Stream::Stdout);
    }

    /// Add one stderr chunk to a run's tail.
    pub fn append_stderr(&self, id: &str, chunk: &[u8]) {
        self.append(id, chunk, Stream::Stderr);
    }

    /// Record the ending of one run.
    ///
    /// The first ending wins: a watchdog that marked a run `timed_out` keeps that
    /// status when the tool that spawned it reports a plain non-zero exit for the
    /// kill it performed.
    pub fn finish(&self, id: &str, status: RunStatus, exit_code: Option<i32>) {
        let run = {
            let mut runs = lock(&self.inner.runs);
            let Some(run) = runs.iter_mut().find(|run| run.id == id) else {
                return;
            };
            if run.status.is_terminal() {
                return;
            }
            run.status = status;
            if exit_code.is_some() {
                run.exit_code = exit_code;
            }
            run.finished = Some(Instant::now());
            run.finished_at = Some(format_timestamp(now()));
            run.clone()
        };
        self.publish(&run);
    }

    /// Ask one run to stop, the one way this daemon ends a harness process.
    ///
    /// The record is marked `killed` before the signal goes out, so a monitor
    /// reads the intent even if the signal lands after the process already
    /// exited on its own.
    pub fn kill_requested(&self, id: &str) -> KillOutcome {
        let (run, pid) = {
            let mut runs = lock(&self.inner.runs);
            let Some(run) = runs.iter_mut().find(|run| run.id == id) else {
                return KillOutcome::Unknown;
            };
            if run.status.is_terminal() {
                return KillOutcome::Finished(run.clone());
            }
            run.status = RunStatus::Killed;
            run.finished = Some(Instant::now());
            run.finished_at = Some(format_timestamp(now()));
            (run.clone(), run.pid)
        };
        self.publish(&run);
        if let Some(pid) = pid {
            terminate_group(pid);
        }
        KillOutcome::Requested(run)
    }

    /// Every run this registry remembers, newest first.
    #[must_use]
    pub fn snapshot(&self) -> Vec<HarnessRun> {
        let runs = lock(&self.inner.runs);
        runs.iter().rev().cloned().collect()
    }

    /// One run by id, when it is still in the table.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<HarnessRun> {
        let runs = lock(&self.inner.runs);
        runs.iter().find(|run| run.id == id).cloned()
    }

    /// Add a chunk to a tail, and recover a stalled run that is producing again.
    ///
    /// A chunk that arrives after the ending still lands in the tail: the bytes
    /// a killed harness had already written are the most useful part of a timeout
    /// report, and the status is what says the run is over.
    fn append(&self, id: &str, chunk: &[u8], stream: Stream) {
        let recovered = {
            let mut runs = lock(&self.inner.runs);
            let Some(run) = runs.iter_mut().find(|run| run.id == id) else {
                return;
            };
            let text = String::from_utf8_lossy(chunk);
            match stream {
                Stream::Stdout => push_tail(&mut run.stdout_tail, &text),
                Stream::Stderr => push_tail(&mut run.stderr_tail, &text),
            }
            run.last_output = Instant::now();
            if run.status == RunStatus::Stalled {
                run.status = RunStatus::Running;
                Some(run.clone())
            } else {
                None
            }
        };
        if let Some(run) = recovered {
            self.publish(&run);
        }
    }

    /// One watchdog pass over one run.
    ///
    /// The deadline is checked before the silence, so a run that is both late
    /// and quiet ends as `timed_out` rather than lingering as `stalled`.
    fn step(&self, id: &str) -> Step {
        let at = Instant::now();
        let change = {
            let mut runs = lock(&self.inner.runs);
            let Some(run) = runs.iter_mut().find(|run| run.id == id) else {
                return Step::Stop;
            };
            if run.status.is_terminal() {
                return Step::Stop;
            }
            if at.saturating_duration_since(run.started) >= Duration::from_secs(run.timeout_secs) {
                run.status = RunStatus::TimedOut;
                run.finished = Some(at);
                run.finished_at = Some(format_timestamp(now()));
                Some((run.clone(), run.pid))
            } else if run.status == RunStatus::Running
                && run.silent_ms() > self.inner.stall_after_ms
            {
                run.status = RunStatus::Stalled;
                Some((run.clone(), None))
            } else {
                None
            }
        };
        let Some((run, pid)) = change else {
            return Step::Continue;
        };
        self.publish(&run);
        if let Some(pid) = pid {
            terminate_group(pid);
        }
        if run.status == RunStatus::TimedOut {
            Step::Stop
        } else {
            Step::Continue
        }
    }

    /// Start the watchdog for one run.
    fn spawn_watchdog(&self, id: &str) {
        let registry = self.clone();
        let id = id.to_owned();
        let tick = self.inner.tick;
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(tick);
                if registry.step(&id) == Step::Stop {
                    return;
                }
            }
        });
    }

    /// Publish one state change. No subscriber is not an error.
    fn publish(&self, run: &HarnessRun) {
        let _ = self.inner.events.send(RunEvent {
            id: run.id.clone(),
            item: run.list_item(),
        });
    }
}

impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}

/// Which stream a chunk belongs to.
#[derive(Debug, Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

/// End one harness process and everything it started.
///
/// This is the only place a harness process group is killed. The tool's deadline,
/// the watchdog's deadline and `POST /v1/harness/runs/{id}/kill` all call it, so
/// there is one signal and one spelling of it rather than three that can drift.
///
/// The signal goes to `kill`, not to a shell. A host without that command leaves
/// the process where it is, which is the same partial ending the tool had before
/// this module existed.
/// Kill every live run's process group.
///
/// Bounding the runtime is not enough at shutdown: systemd waits for the whole
/// cgroup, and a delegated harness that spawned its own children held the unit
/// in `final-sigterm` until `TimeoutStopSec` expired and it was SIGKILLed. The
/// next start then raced a half-dead process for the listener ports. A run that
/// is already terminal is left alone, because its group is gone.
pub fn terminate_live() {
    for run in snapshot() {
        if run.status.is_terminal() {
            continue;
        }
        if let Some(pid) = run.pid {
            terminate_group(pid);
        }
    }
}

pub fn terminate_group(pid: u32) {
    // Pid zero is not a harness: to `kill` it means this process's own group,
    // which is the one group a monitor must never signal. No child has ever had
    // it, so refusing it costs nothing.
    if pid == 0 {
        return;
    }
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .arg("-KILL")
            .arg("--")
            .arg(format!("-{pid}"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

// ---------------------------------------------------------------------------
// The process-wide registry
// ---------------------------------------------------------------------------

/// The one registry this daemon has.
#[must_use]
pub fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(Registry::new)
}

/// Register one run in the process-wide registry.
pub fn start(
    harness: &str,
    task: &str,
    conversation_id: Option<String>,
    turn_id: Option<String>,
    timeout_secs: u64,
) -> String {
    registry().start(harness, task, conversation_id, turn_id, timeout_secs)
}

/// Attach the process a run started.
pub fn attach_pid(id: &str, pid: u32) {
    registry().attach_pid(id, pid);
}

/// Feed one stdout chunk to a run.
pub fn append_stdout(id: &str, chunk: &[u8]) {
    registry().append_stdout(id, chunk);
}

/// Feed one stderr chunk to a run.
pub fn append_stderr(id: &str, chunk: &[u8]) {
    registry().append_stderr(id, chunk);
}

/// Record the ending of one run.
pub fn finish(id: &str, status: RunStatus, exit_code: Option<i32>) {
    registry().finish(id, status, exit_code);
}

/// Ask one run in the process-wide registry to stop.
#[must_use]
pub fn kill_requested(id: &str) -> KillOutcome {
    registry().kill_requested(id)
}

/// Every run the process knows, newest first.
#[must_use]
pub fn snapshot() -> Vec<HarnessRun> {
    registry().snapshot()
}

/// One run by id.
#[must_use]
pub fn get(id: &str) -> Option<HarnessRun> {
    registry().get(id)
}

/// The channel the SSE route subscribes to.
#[must_use]
pub fn events_sender() -> broadcast::Sender<RunEvent> {
    registry().events()
}

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// A duration in milliseconds, saturating rather than wrapping.
fn millis(value: u128) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// Take the registry's lock, surviving another thread's panic.
///
/// The data behind it is a table of finished and running records. Continuing with
/// the last state is better than refusing every later request.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The task as a list renders it: one line, at most [`TASK_CHARS`] characters.
fn summary(task: &str) -> String {
    let one_line = task.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.chars().take(TASK_CHARS).collect()
}

/// Append to a tail, keeping only the last [`TAIL_BYTES`].
///
/// The cut is moved forward to a character boundary, so trimming a chunk that
/// ended mid-character drops bytes rather than producing invalid text.
fn push_tail(tail: &mut String, chunk: &str) {
    tail.push_str(chunk);
    if tail.len() <= TAIL_BYTES {
        return;
    }
    let mut cut = tail.len() - TAIL_BYTES;
    while !tail.is_char_boundary(cut) {
        cut += 1;
    }
    tail.drain(..cut);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process id no test can signal: `kill` refuses it, and nothing a unit
    /// test does should ever reach a real group.
    const NO_SUCH_PID: u32 = 999_999;

    /// A registry whose watchdog looks every ten milliseconds and calls a run
    /// stalled after one, so the transitions are testable without a long sleep.
    fn quick() -> Registry {
        Registry::with_timing(Duration::from_millis(10), Duration::from_millis(1))
    }

    /// Wait for a condition a watchdog produces, or fail with the reason.
    fn wait_for(mut check: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if check() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("the watchdog did not reach the expected state within five seconds");
    }

    /// A run starts queued, keeps the first 200 characters of its task on one
    /// line, and carries the turn that delegated it.
    #[test]
    fn a_started_run_is_queued_and_carries_its_task_on_one_line() {
        let registry = Registry::new();
        let task = format!("first line\nsecond   line\t{}", "x".repeat(400));
        let id = registry.start(
            "dsh",
            &task,
            Some("conversation-1".to_owned()),
            Some("turn-1".to_owned()),
            600,
        );

        let run = registry.get(&id).expect("the run is registered");
        assert_eq!(run.status, RunStatus::Queued);
        assert_eq!(run.harness, "dsh");
        assert_eq!(run.task.chars().count(), TASK_CHARS);
        assert!(
            !run.task.contains('\n'),
            "the summary is one line: {}",
            run.task
        );
        assert!(run.task.starts_with("first line second line x"));
        assert_eq!(run.conversation_id.as_deref(), Some("conversation-1"));
        assert_eq!(run.turn_id.as_deref(), Some("turn-1"));
        assert_eq!(run.timeout_secs, 600);
        assert_eq!(run.exit_code, None);
        assert!(run.finished_at.is_none());
        assert!(!run.started_at.is_empty());
        registry.finish(&id, RunStatus::Succeeded, Some(0));
    }

    /// Output lands in the right tail, the tail is bounded at 8 KiB, and the
    /// tail is the last bytes rather than the first.
    #[test]
    fn output_is_kept_as_a_bounded_tail() {
        let registry = Registry::new();
        let id = registry.start("dsh", "build", None, None, 600);
        registry.attach_pid(&id, NO_SUCH_PID);

        registry.append_stdout(&id, b"hello ");
        registry.append_stderr(&id, b"a warning\n");
        let run = registry.get(&id).expect("registered");
        assert_eq!(run.stdout_tail, "hello ");
        assert_eq!(run.stderr_tail, "a warning\n");

        let loud = "x".repeat(TAIL_BYTES + 17);
        registry.append_stdout(&id, loud.as_bytes());
        let run = registry.get(&id).expect("registered");
        assert_eq!(run.stdout_tail.len(), TAIL_BYTES);
        assert!(
            run.stdout_tail.ends_with(&"x".repeat(17)),
            "the newest bytes are the ones kept"
        );
        assert!(
            !run.stdout_tail.contains("hello"),
            "the oldest bytes are the ones dropped"
        );
        registry.finish(&id, RunStatus::Succeeded, Some(0));
    }

    /// A run that is both quiet and late ends as timed out, and a finished run
    /// keeps the ending it was given.
    #[test]
    fn the_watchdog_times_out_a_run_that_passes_its_deadline() {
        let registry = Registry::with_timing(Duration::from_millis(5), STALL_AFTER);
        let id = registry.start("dsh", "hang", None, None, 0);

        wait_for(|| {
            registry
                .get(&id)
                .is_some_and(|run| run.status == RunStatus::TimedOut)
        });
        let run = registry.get(&id).expect("registered");
        assert!(run.finished_at.is_some(), "a timeout is an ending");
        assert_eq!(run.exit_code, None);
        // The first ending wins: a later report cannot rewrite it.
        registry.finish(&id, RunStatus::Failed, Some(9));
        assert_eq!(
            registry.get(&id).expect("registered").status,
            RunStatus::TimedOut
        );
    }

    /// Silence past the threshold reads as stalled, the run is still alive, and
    /// the next output puts it back to running.
    #[test]
    fn a_silent_run_stalls_and_recovers_when_output_arrives() {
        let registry = quick();
        let id = registry.start("dsh", "test suite", None, None, 600);
        registry.attach_pid(&id, NO_SUCH_PID);

        wait_for(|| {
            registry
                .get(&id)
                .is_some_and(|run| run.status == RunStatus::Stalled)
        });
        let stalled = registry.get(&id).expect("registered");
        assert!(stalled.finished_at.is_none(), "a stall is not an ending");
        assert!(stalled.silent_ms() > 0);
        assert_eq!(stalled.list_item()["stalled"], serde_json::json!(true));

        // A stalled run is never killed by the watchdog: it is still there and
        // still running.
        registry.append_stdout(&id, b"a later line\n");
        let recovered = registry.get(&id).expect("registered");
        assert_eq!(recovered.status, RunStatus::Running);
        assert_eq!(recovered.list_item()["stalled"], serde_json::json!(false));
        registry.finish(&id, RunStatus::Succeeded, Some(0));
    }

    /// The table is bounded: the fifty-first run evicts the first, and the
    /// evicted one is gone from both reads.
    #[test]
    fn the_registry_evicts_the_oldest_run_at_fifty() {
        let registry = Registry::new();
        let mut ids = Vec::new();
        for index in 0..MAX_RUNS {
            let id = registry.start("dsh", &format!("task {index}"), None, None, 600);
            registry.finish(&id, RunStatus::Succeeded, Some(0));
            ids.push(id);
        }
        assert_eq!(registry.snapshot().len(), MAX_RUNS);

        let newest = registry.start("dsh", "the fifty-first", None, None, 600);
        let runs = registry.snapshot();
        assert_eq!(runs.len(), MAX_RUNS);
        assert_eq!(runs.first().expect("newest first").id, newest);
        assert!(registry.get(&ids[0]).is_none(), "the oldest was evicted");
        assert!(
            registry.get(&ids[1]).is_some(),
            "only the oldest was evicted"
        );
        registry.finish(&newest, RunStatus::Succeeded, Some(0));
    }

    /// The snapshot is newest first, which is the order the list route promises.
    #[test]
    fn the_snapshot_is_newest_first() {
        let registry = Registry::new();
        let first = registry.start("dsh", "one", None, None, 600);
        let second = registry.start("dsh", "two", None, None, 600);
        let runs = registry.snapshot();
        assert_eq!(runs[0].id, second);
        assert_eq!(runs[1].id, first);
        registry.finish(&first, RunStatus::Succeeded, Some(0));
        registry.finish(&second, RunStatus::Succeeded, Some(0));
    }

    /// Every state change is published, with the list item as its payload.
    #[test]
    fn every_state_change_is_published_on_the_channel() {
        let registry = quick();
        let mut events = registry.subscribe();
        let id = registry.start("dsh", "publish", None, None, 600);
        registry.attach_pid(&id, NO_SUCH_PID);

        let mut seen = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match events.try_recv() {
                Ok(event) => {
                    assert!(
                        event.id == id,
                        "the event names the run it is about: {event:?}"
                    );
                    seen.push(event.item["status"].as_str().unwrap_or_default().to_owned());
                    if seen.last().is_some_and(|status| status == "stalled") {
                        break;
                    }
                }
                Err(broadcast::error::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("the channel closed: {error}"),
            }
        }
        assert_eq!(seen.first().map(String::as_str), Some("queued"));
        assert!(
            seen.iter().any(|status| status == "running"),
            "the attach is published: {seen:?}"
        );
        assert!(seen.iter().any(|status| status == "stalled"), "{seen:?}");

        registry.append_stdout(&id, b"more\n");
        registry.finish(&id, RunStatus::Succeeded, Some(0));
        let terminal = loop {
            match events.try_recv() {
                Ok(event) => {
                    if event.item["status"] == serde_json::json!("succeeded") {
                        break event;
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        };
        assert_eq!(terminal.id, id);
        assert_eq!(terminal.item["stalled"], serde_json::json!(false));
    }

    /// Killing a live run records the intent, and killing a finished one says so
    /// without signalling anything.
    #[test]
    fn a_kill_marks_a_live_run_and_refuses_a_finished_one() {
        let registry = Registry::new();
        let id = registry.start("dsh", "kill me", None, None, 600);
        match registry.kill_requested(&id) {
            KillOutcome::Requested(run) => {
                assert_eq!(run.status, RunStatus::Killed);
                assert!(run.finished_at.is_some());
            }
            other => panic!("a live run is killable: {other:?}"),
        }
        match registry.kill_requested(&id) {
            KillOutcome::Finished(run) => assert_eq!(run.status, RunStatus::Killed),
            other => panic!("a dead run is not killed twice: {other:?}"),
        }
        match registry.kill_requested("no-such-run") {
            KillOutcome::Unknown => {}
            other => panic!("an unknown run is unknown: {other:?}"),
        }
    }

    /// The two JSON shapes the routes answer, built in one place.
    #[test]
    fn the_list_item_and_the_detail_carry_the_documented_fields() {
        let registry = Registry::new();
        let id = registry.start("dsh", "shape", None, None, 300);
        registry.attach_pid(&id, NO_SUCH_PID);
        registry.append_stdout(&id, b"out\n");
        registry.append_stderr(&id, b"err\n");
        let run = registry.get(&id).expect("registered");

        let item = run.list_item();
        for key in [
            "id",
            "harness",
            "status",
            "task",
            "started_at",
            "finished_at",
            "elapsed_ms",
            "silent_ms",
            "exit_code",
            "stalled",
        ] {
            assert!(item.get(key).is_some(), "the list item carries {key}");
        }
        let detail = run.detail();
        assert_eq!(detail["stdout"], serde_json::json!("out\n"));
        assert_eq!(detail["stderr"], serde_json::json!("err\n"));
        assert_eq!(detail["timeout_secs"], serde_json::json!(300));
        registry.finish(&id, RunStatus::Succeeded, Some(0));
    }
}
