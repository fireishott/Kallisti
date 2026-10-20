//! Subagents: work the model hands off, and the bar that says it is happening.
//!
//! One row in `subagents` is one handoff. A turn calls `dispatch_agent`, a row is
//! written, and a background task runs a child turn with that prompt. The parent
//! does not wait: the whole point is that two things happen at once, so the tool
//! returns an id and the result lands on the row.
//!
//! Four rules live here rather than in a route, because there is more than one
//! caller and a rule that exists in one handler is a rule the next one forgets.
//!
//! - **A child is a turn.** Same provider, same system prompt, same persona, its
//!   own budget and its own usage. The way to have a subagent behave like a turn
//!   is to run a turn, not to write a second, smaller inference path that drifts.
//! - **A child has no tools.** [`child_settings`] turns tool calling off, the
//!   child's sandbox is read-only, and its gate refuses every call. Three layers,
//!   because the interesting failure is a subagent that can run commands, and
//!   that failure is recursive: a child that could dispatch would be an unbounded
//!   fan-out of processes on the operator's box.
//! - **Work is capped.** [`MAX_RUNNING`] at once and [`MAX_PER_TURN`] per parent
//!   turn, both counted in the same transaction that writes the row, so two
//!   dispatches racing cannot both pass the check.
//! - **A settled row is settled.** Every terminal write is conditional on the row
//!   still being `queued` or `running`. A child whose watchdog already failed it
//!   writes nothing when it eventually returns, so a recorded outcome is never
//!   overwritten by a later one.
//!
//! The bar is server-rendered from [`bar`], which is the same rows: no script
//! reads them, so an operator with JavaScript blocked still sees every subagent
//! and its result.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use core_runtime::provider::{ChatMessage, ChatRole, ToolSpec};
use rusqlite::params;

use crate::db::{Db, DbError};
use crate::web::chat::{self, TurnConfig, TurnSettings};

/// The `settings` row that turns dispatching off.
///
/// Default on: the tool is offered only in `Full` sandbox mode, and `Full` is
/// already an explicit opt-in, so a second switch that starts off would be a
/// feature nobody finds.
pub const SETTING_AGENTS_ENABLED: &str = "agents.enabled";

/// The tool name, in one place because the spec, the dispatcher and the tests all
/// have to agree about it.
pub const TOOL: &str = "dispatch_agent";

/// How many subagents may be running at once, across the whole daemon.
///
/// Counted globally rather than per conversation: the limit exists to bound
/// provider spend and child processes on one host, and neither of those is
/// per-conversation.
pub const MAX_RUNNING: i64 = 4;

/// How many one parent turn may dispatch in total, finished or not.
///
/// A per-turn cap is what stops a single turn from queueing a hundred children
/// one after another, which the running cap alone would not: each dispatch
/// returns immediately, so a model would otherwise be free to loop.
pub const MAX_PER_TURN: i64 = 8;

/// How long a child may run before the watchdog settles its row.
///
/// Short on purpose. A subagent is a bounded piece of work; a job that needs
/// longer belongs in the turn that started it, where the operator can see it
/// streaming and stop it.
pub const DEADLINE: Duration = Duration::from_secs(120);

/// The longest label the bar shows, in characters.
pub const MAX_LABEL_CHARS: usize = 60;

/// The largest result kept on a row, in characters.
///
/// The bar renders it in a `<pre>`, so this is also what bounds the page. A child
/// that produced more than this has its text clipped here rather than stored in
/// full and rendered truncated: one number, so what is shown and what is kept
/// cannot disagree.
pub const MAX_RESULT_CHARS: usize = 4_000;

/// The largest prompt accepted, in characters.
pub const MAX_PROMPT_CHARS: usize = 8_000;

/// How long a finished row stays in the bar.
pub const RECENT_MINUTES: i64 = 30;

/// How many rows the bar lists.
pub const BAR_LIMIT: usize = 6;

/// The stored value that turns dispatching off.
#[must_use]
pub fn enabled(stored: Option<&str>) -> bool {
    !matches!(stored.map(str::trim), Some("false"))
}

/// Where a subagent is in its life. The database spells these, and the bar prints
/// them, so there is one set of words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Written, not started.
    Queued,
    /// Running now.
    Running,
    /// Produced a result.
    Succeeded,
    /// Ended without one.
    Failed,
    /// Stopped before it could finish.
    Cancelled,
}

impl Status {
    /// The stored and rendered spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse a stored value. Anything outside the CHECK constraint is a fact about
    /// a hand-edited database, and it is reported rather than guessed at.
    pub fn parse(value: &str) -> Result<Self, DbError> {
        match value {
            "queued" => Ok(Self::Queued),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            other => Err(DbError::UnexpectedSubagentStatus {
                value: other.to_owned(),
            }),
        }
    }

    /// Whether this is an end state.
    #[must_use]
    pub fn settled(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// One dispatched subagent, as the table holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subagent {
    /// Opaque id, and what the tool returns.
    pub id: String,
    /// The conversation whose turn dispatched it.
    pub parent_conversation: String,
    /// That turn's answer row, when there was one.
    pub parent_turn: Option<String>,
    /// What the bar calls it.
    pub label: String,
    /// What it was asked to do.
    pub prompt: String,
    /// Which model ran it.
    pub model: String,
    /// Where it is.
    pub status: Status,
    /// What it produced, or the sentence explaining why it did not.
    pub result: Option<String>,
    /// The short code a failure carries.
    pub error_code: Option<String>,
    /// Prompt tokens the provider reported.
    pub prompt_tokens: Option<u64>,
    /// Completion tokens the provider reported.
    pub completion_tokens: Option<u64>,
    /// When the row was written.
    pub created_at: String,
    /// When it started running.
    pub started_at: Option<String>,
    /// When it stopped.
    pub finished_at: Option<String>,
    /// Seconds from start (or creation) to now, computed by the query.
    pub elapsed_secs: i64,
}

impl Subagent {
    /// The elapsed time as the bar prints it: seconds under a minute, then
    /// minutes and seconds. Never a unit nobody asked for.
    #[must_use]
    pub fn elapsed_label(&self) -> String {
        elapsed_label(self.elapsed_secs)
    }

    /// The result, clipped for the bar. `None` while there is nothing to show.
    #[must_use]
    pub fn result_preview(&self) -> Option<&str> {
        self.result
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
    }
}

/// Format a second count the way the bar reads it.
#[must_use]
pub fn elapsed_label(seconds: i64) -> String {
    let seconds = seconds.max(0);
    if seconds < 60 {
        return format!("{seconds}s");
    }
    format!("{}m {:02}s", seconds / 60, seconds % 60)
}

/// Why a dispatch was refused, in the two halves the model reads: a stable code
/// and a sentence that names the rule it hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// A short code the model and a test can match on.
    pub code: &'static str,
    /// The sentence, which names the cap rather than saying "denied".
    pub reason: String,
}

impl Refusal {
    fn new(code: &'static str, reason: impl Into<String>) -> Self {
        Self {
            code,
            reason: reason.into(),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.reason)
    }
}

/// One dispatch request, echoed into [`reserve`] so the signature stays readable.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// The conversation whose turn is dispatching.
    pub conversation: &'a str,
    /// That turn's answer row, when one exists.
    pub parent_turn: Option<&'a str>,
    /// What the bar will call it.
    pub label: &'a str,
    /// The task itself.
    pub prompt: &'a str,
    /// The model that will run it.
    pub model: &'a str,
}

/// The columns every read shares, so a new field cannot be added to one query and
/// forgotten in another.
const COLUMNS: &str = "id, parent_conversation, parent_turn, label, prompt, model, status, \
                       result, error_code, prompt_tokens, completion_tokens, created_at, \
                       started_at, finished_at";

/// A token count SQLite stored as an integer, or `None` when it stored nothing.
///
/// SQLite has no unsigned integer, so the column is signed and the conversion
/// happens on the way out rather than being assumed. A negative value cannot come
/// from a provider; if one is there, the row has been edited, and reporting "not
/// measured" is more honest than reporting a number.
fn tokens(value: Option<i64>) -> Option<u64> {
    value.and_then(|count| u64::try_from(count).ok())
}

/// A `u64` count going into a signed column.
fn stored_tokens(value: Option<u64>) -> Option<i64> {
    value.and_then(|count| i64::try_from(count).ok())
}

/// Row to [`Subagent`]. The elapsed column is last so the two queries that select
/// it agree.
fn row_to_subagent(row: &rusqlite::Row<'_>) -> rusqlite::Result<Subagent> {
    let status: String = row.get(6)?;
    Ok(Subagent {
        id: row.get(0)?,
        parent_conversation: row.get(1)?,
        parent_turn: row.get(2)?,
        label: row.get(3)?,
        prompt: row.get(4)?,
        model: row.get(5)?,
        status: Status::parse(&status).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::other(error.to_string())),
            )
        })?,
        result: row.get(7)?,
        error_code: row.get(8)?,
        prompt_tokens: tokens(row.get(9)?),
        completion_tokens: tokens(row.get(10)?),
        created_at: row.get(11)?,
        started_at: row.get(12)?,
        finished_at: row.get(13)?,
        elapsed_secs: row.get::<_, i64>(14)?,
    })
}

/// The elapsed-seconds expression, so every read measures time the same way.
///
/// A running row is measured to now, a settled one to its own finish, and a row
/// that never started is measured from creation: the bar should show how long the
/// operator has been waiting, not how long the child has been thinking.
const ELAPSED: &str = "CAST(ROUND((julianday(COALESCE(finished_at, \
                       strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))) \
                       - julianday(COALESCE(started_at, created_at))) * 86400.0) AS INTEGER)";

/// Write one row for a dispatch, refusing when a cap is already spent.
///
/// The count and the insert are one transaction on one connection, which is what
/// makes the cap meaningful: two dispatches landing at the same instant are
/// serialized, and the second one sees the first one's row.
pub fn reserve(db: &Db, request: &Request<'_>) -> Result<String, ReserveError> {
    let prompt = request.prompt.trim();
    if prompt.is_empty() {
        return Err(ReserveError::Refused(Refusal::new(
            "subagent_prompt_empty",
            "a subagent needs a prompt describing the task",
        )));
    }
    let prompt = clip(prompt, MAX_PROMPT_CHARS);
    let label = label_for(request.label, prompt.as_str());
    let id = uuid::Uuid::now_v7().to_string();

    let id_for_insert = id.clone();
    let refused = db.with_conn(|conn| {
        let transaction = conn.transaction()?;
        let running: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM subagents WHERE status IN ('queued', 'running')",
            [],
            |row| row.get(0),
        )?;
        if running >= MAX_RUNNING {
            return Ok(Some(Refusal::new(
                "subagents_at_capacity",
                format!(
                    "{running} subagents are already running and the cap is {MAX_RUNNING}, \
                     so nothing was dispatched. Wait for one to finish, or do this work in \
                     this turn instead."
                ),
            )));
        }
        let spent: i64 = match request.parent_turn {
            Some(turn) => transaction.query_row(
                "SELECT COUNT(*) FROM subagents WHERE parent_turn = ?1",
                params![turn],
                |row| row.get(0),
            )?,
            None => transaction.query_row(
                "SELECT COUNT(*) FROM subagents WHERE parent_conversation = ?1",
                params![request.conversation],
                |row| row.get(0),
            )?,
        };
        if spent >= MAX_PER_TURN {
            return Ok(Some(Refusal::new(
                "turn_dispatch_budget_spent",
                format!(
                    "this turn has already dispatched {spent} subagents and the budget is \
                     {MAX_PER_TURN}, so nothing was dispatched. Ask for the work in this turn \
                     instead, or start a new one."
                ),
            )));
        }
        transaction.execute(
            "INSERT INTO subagents
                 (id, parent_conversation, parent_turn, label, prompt, model, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'queued')",
            params![
                id_for_insert,
                request.conversation,
                request.parent_turn,
                label,
                prompt,
                request.model,
            ],
        )?;
        transaction.commit()?;
        Ok(None)
    })?;

    match refused {
        Some(refusal) => Err(ReserveError::Refused(refusal)),
        None => Ok(id),
    }
}

/// Why [`reserve`] produced no row.
#[derive(Debug, thiserror::Error)]
pub enum ReserveError {
    /// A cap or an input rule refused the dispatch.
    #[error("{0}")]
    Refused(Refusal),
    /// The database could not answer, which is a refusal too: a dispatch that
    /// cannot be recorded is not a dispatch that should run.
    #[error("could not record the subagent: {0}")]
    Unavailable(#[from] DbError),
}

impl ReserveError {
    /// The code, for a caller that only has room for one string.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Refused(refusal) => refusal.code,
            Self::Unavailable(_) => "subagent_unavailable",
        }
    }

    /// The sentence.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Refused(refusal) => refusal.reason.clone(),
            Self::Unavailable(error) => {
                format!("the subagent row could not be written ({error}), so nothing ran")
            }
        }
    }
}

/// Claim a queued row for its own execution.
///
/// False means somebody else already settled it: the watchdog timed it out, or a
/// stop cancelled it. The child then does not run at all, which is the honest
/// reading of a row that is no longer queued.
pub fn mark_running(db: &Db, id: &str) -> Result<bool, DbError> {
    let changed = db.with_conn(|conn| {
        conn.execute(
            "UPDATE subagents
                SET status = 'running',
                    started_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
              WHERE id = ?1 AND status = 'queued'",
            params![id],
        )
    })?;
    Ok(changed == 1)
}

/// What a finished child leaves behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The text, or the sentence explaining the failure.
    pub result: Option<String>,
    /// The short code a failure carries.
    pub error_code: Option<String>,
    /// Prompt tokens, when the provider reported any.
    pub prompt_tokens: Option<u64>,
    /// Completion tokens, when the provider reported any.
    pub completion_tokens: Option<u64>,
}

impl Outcome {
    /// A run that produced text.
    #[must_use]
    pub fn succeeded(text: &str, usage: &core_domain::Usage) -> Self {
        Self {
            result: Some(clip(text.trim(), MAX_RESULT_CHARS)),
            error_code: None,
            prompt_tokens: usage.input_tokens(),
            completion_tokens: usage.output_tokens(),
        }
    }

    /// A run that did not.
    #[must_use]
    pub fn failed(code: &str, message: &str) -> Self {
        Self {
            result: Some(clip(message.trim(), MAX_RESULT_CHARS)),
            error_code: Some(code.to_owned()),
            prompt_tokens: None,
            completion_tokens: None,
        }
    }
}

/// Settle a row, and report whether this call was the one that settled it.
///
/// The `WHERE` clause is the rule: a row that is already `succeeded`, `failed` or
/// `cancelled` is left exactly as the first writer left it.
pub fn finish(db: &Db, id: &str, status: Status, outcome: &Outcome) -> Result<bool, DbError> {
    let changed = db.with_conn(|conn| {
        conn.execute(
            "UPDATE subagents
                SET status = ?2,
                    result = ?3,
                    error_code = ?4,
                    prompt_tokens = ?5,
                    completion_tokens = ?6,
                    finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
              WHERE id = ?1 AND status IN ('queued', 'running')",
            params![
                id,
                status.as_str(),
                outcome.result,
                outcome.error_code,
                stored_tokens(outcome.prompt_tokens),
                stored_tokens(outcome.completion_tokens),
            ],
        )
    })?;
    Ok(changed == 1)
}

/// Settle every row a previous process left behind.
///
/// A `queued` or `running` row at boot belongs to a task that no longer exists.
/// Left alone it would spend the concurrency cap forever and sit in the bar
/// looking like work that is still happening.
pub fn fail_orphaned(db: &Db) -> Result<usize, DbError> {
    db.with_conn(|conn| {
        let changed = conn.execute(
            "UPDATE subagents
                SET status = 'failed',
                    error_code = 'interrupted',
                    result = 'the daemon stopped before this subagent finished',
                    finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
              WHERE status IN ('queued', 'running')",
            [],
        )?;
        Ok(changed)
    })
}

/// One row by id, for a caller that watched it be created.
pub fn get(db: &Db, id: &str) -> Result<Option<Subagent>, DbError> {
    let sql = format!(
        "SELECT {columns}, {elapsed} AS elapsed FROM subagents WHERE id = ?1",
        columns = COLUMNS,
        elapsed = ELAPSED,
    );
    let mut found: Option<Subagent> = None;
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&sql)?;
        let mut rows = statement.query(params![id])?;
        if let Some(row) = rows.next()? {
            found = Some(row_to_subagent(row)?);
        }
        Ok(())
    })?;
    Ok(found)
}

/// What the bar lists: anything still working, plus anything that finished
/// inside [`RECENT_MINUTES`]. Newest first, bounded.
///
/// A finished row ages out of the bar on its own. That is deliberate: the bar is a
/// live view, and a list that only grows is a list that stops being one.
pub fn bar(db: &Db, limit: usize) -> Result<Vec<Subagent>, DbError> {
    let sql = format!(
        "SELECT {columns}, {elapsed} AS elapsed
           FROM subagents
          WHERE status IN ('queued', 'running')
             OR finished_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?2)
          ORDER BY created_at DESC, id DESC
          LIMIT ?1",
        columns = COLUMNS,
        elapsed = ELAPSED,
    );
    let window = format!("-{RECENT_MINUTES} minutes");
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(params![limit as i64, window], row_to_subagent)?;
        rows.collect()
    })
}

/// How many rows are working right now, for the shapes of a status line.
pub fn running_count(db: &Db) -> Result<i64, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM subagents WHERE status IN ('queued', 'running')",
            [],
            |row| row.get(0),
        )
    })
}

// ---------------------------------------------------------------------------
// A child turn
// ---------------------------------------------------------------------------

/// What one child turn is asked for.
#[derive(Debug, Clone)]
pub struct RunRequest {
    /// Where the child sends its request.
    pub config: TurnConfig,
    /// The system prompt, which is the parent's: same persona, same assistant.
    pub system: Option<String>,
    /// The task.
    pub prompt: String,
    /// The parent's workspace. A child is confined to it and may not write to it,
    /// because it has no tools; see [`child_sandbox`].
    pub workspace: PathBuf,
}

/// What one child turn produced.
#[derive(Debug, Clone)]
pub struct RunOutput {
    /// The text.
    pub text: String,
    /// Usage the provider reported.
    pub usage: core_domain::Usage,
}

/// How a child turn is produced.
///
/// A seam rather than a direct call so a test can drive the whole lifecycle, and
/// the row writes around it, without a network. Production supplies
/// [`provider_runner`], which is the same `run_turn_with_key` the browser drives.
pub type Runner = dyn Fn(&RunRequest) -> Result<RunOutput, (String, String)> + Send + Sync;

/// Told about a status change, so a live page can recount the bar.
pub type Notify = Arc<dyn Fn(&Report<'_>) + Send + Sync>;

/// The three fields a live page needs. Nothing else crosses to the stream: a
/// result is read from the row, not pushed at a socket.
#[derive(Debug, Clone, Copy)]
pub struct Report<'a> {
    /// Which subagent changed.
    pub id: &'a str,
    /// What the bar calls it.
    pub label: &'a str,
    /// Where it is now.
    pub status: &'a str,
}

/// What a child turn is allowed to do, which is: no tools, and no dispatching.
///
/// One function, called by the runner and read by a test, so the claim "a
/// subagent has no tools" is a value rather than a comment.
#[must_use]
pub fn child_settings() -> TurnSettings {
    TurnSettings {
        tools: false,
        max_rounds: 1,
        agents: false,
    }
}

/// The sandbox a child runs in: the parent's workspace, read-only, always.
///
/// The mode is not inherited. A child has no tools, so this is never consulted for
/// anything a tool would do, and that is exactly why it must not be permissive: if
/// a tool ever did reach a child, the sandbox still refuses to write.
#[must_use]
pub fn child_sandbox(workspace: PathBuf) -> crate::tools::Sandbox {
    crate::tools::Sandbox::new(workspace, crate::tools::SandboxMode::ReadOnly)
}

/// The production runner: one tool-less turn through the same loop every other
/// turn uses.
fn run_child_turn(request: &RunRequest) -> Result<RunOutput, (String, String)> {
    let key = core_runtime::provider::resolve_api_key(&request.config.key_ref)
        .map_err(|error| (error.kind().to_owned(), error.to_string()))?;
    let sandbox = child_sandbox(request.workspace.clone());
    // Nothing a child asks for may run, including a call that a provider invented
    // despite being offered an empty tool list.
    let mut gate = |_call: &core_domain::ToolCall| chat::GateDecision::Refuse {
        reason: "a subagent has no tools".to_owned(),
    };
    let mut sink = |_part: chat::StreamPart<'_>| {};
    let mut poll = || chat::TurnSignal::Continue;
    let messages = vec![ChatMessage::text(ChatRole::User, request.prompt.clone())];
    let outcome = chat::run_turn_with_key(
        &request.config,
        &key,
        request.system.clone(),
        messages,
        &sandbox,
        &mut gate,
        None,
        None,
        None,
        None,
        child_settings(),
        &mut poll,
        &mut sink,
    )?;
    Ok(RunOutput {
        text: outcome.text,
        usage: outcome.usage,
    })
}

/// The runner production uses.
#[must_use]
pub fn provider_runner() -> Arc<Runner> {
    Arc::new(run_child_turn)
}

// ---------------------------------------------------------------------------
// The desk: one turn's dispatch surface
// ---------------------------------------------------------------------------

/// The dispatch surface handed to a turn that may dispatch.
///
/// Cheap to clone, because the background task owns one. Everything a child needs
/// is decided before it starts: the row is the request, and this is the context
/// around it.
#[derive(Clone)]
pub struct Desk {
    db: Db,
    conversation: String,
    parent_turn: Option<String>,
    config: TurnConfig,
    system: Option<String>,
    workspace: PathBuf,
    runner: Arc<Runner>,
    notify: Option<Notify>,
}

impl Desk {
    /// A desk for one turn.
    #[must_use]
    pub fn new(
        db: Db,
        config: TurnConfig,
        system: Option<String>,
        workspace: PathBuf,
        conversation: String,
        parent_turn: Option<String>,
    ) -> Self {
        Self {
            db,
            conversation,
            parent_turn,
            config,
            system,
            workspace,
            runner: provider_runner(),
            notify: None,
        }
    }

    /// Replace the runner. Used by the suites; production keeps the provider.
    #[must_use]
    pub fn with_runner(mut self, runner: Arc<Runner>) -> Self {
        self.runner = runner;
        self
    }

    /// Attach the stream the parent turn is writing to.
    #[must_use]
    pub fn with_notify(mut self, notify: Notify) -> Self {
        self.notify = Some(notify);
        self
    }

    /// The model a child will run on.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.config.model
    }

    /// The database, for a caller that wants to read the row it just created.
    #[must_use]
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Dispatch one subagent and return its id without waiting for it.
    ///
    /// The row is written here, before anything is spawned, so a caller that gets
    /// an id back can read it: the id is not a promise, it is a row.
    pub fn dispatch(&self, label: &str, prompt: &str) -> Result<String, ReserveError> {
        let label = label_for(label, prompt);
        let request = Request {
            conversation: &self.conversation,
            parent_turn: self.parent_turn.as_deref(),
            label: &label,
            prompt,
            model: &self.config.model,
        };
        let id = reserve(&self.db, &request)?;
        self.spawn(id.clone(), label, prompt.to_owned());
        Ok(id)
    }

    /// Start the background task for a row that already exists.
    ///
    /// With a runtime the child runs on a blocking thread and a watchdog settles
    /// the row if it outlives [`DEADLINE`]. Without one - a unit test that drives
    /// the lifecycle by hand - the work still runs, just with nobody watching the
    /// clock.
    pub fn spawn(&self, id: String, label: String, prompt: String) {
        let desk = self.clone();
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let watcher = self.clone();
                let watched_id = id.clone();
                let watched_label = label.clone();
                handle.spawn(async move {
                    let child = tokio::task::spawn_blocking(move || {
                        desk.execute(&id, &label, &prompt);
                    });
                    if tokio::time::timeout(DEADLINE, child).await.is_err() {
                        watcher.expire(&watched_id, &watched_label);
                    }
                });
            }
            Err(_) => {
                std::thread::spawn(move || {
                    desk.execute(&id, &label, &prompt);
                });
            }
        }
    }

    /// Run one row to its end and settle it.
    ///
    /// Returns whether this call owned the row. A row that was already settled by
    /// the watchdog or a stop is not run, and its recorded outcome stands.
    pub fn execute(&self, id: &str, label: &str, prompt: &str) -> bool {
        match mark_running(&self.db, id) {
            Ok(true) => {}
            // A row that cannot be claimed is a row somebody else owns. Not
            // running it is the whole point of the claim.
            _ => return false,
        }
        self.report(id, label, Status::Running);

        let request = RunRequest {
            config: self.config.clone(),
            system: self.system.clone(),
            prompt: prompt.to_owned(),
            workspace: self.workspace.clone(),
        };
        let (status, outcome) = match (self.runner)(&request) {
            Ok(output) if output.text.trim().is_empty() => (
                Status::Failed,
                Outcome::failed("subagent_empty", "the subagent returned no text"),
            ),
            Ok(output) => (
                Status::Succeeded,
                Outcome::succeeded(&output.text, &output.usage),
            ),
            Err((code, message)) => (Status::Failed, Outcome::failed(&code, &message)),
        };
        if finish(&self.db, id, status, &outcome).unwrap_or(false) {
            self.report(id, label, status);
        }
        true
    }

    /// Settle a row that outlived its deadline.
    ///
    /// The provider request already in flight is not aborted: a blocking call has
    /// no seam to interrupt. What ends is the record, and the child's own write
    /// then finds a settled row and does nothing.
    fn expire(&self, id: &str, label: &str) {
        let outcome = Outcome::failed(
            "subagent_timeout",
            &format!(
                "the subagent passed its {} second deadline, so it was recorded as failed",
                DEADLINE.as_secs()
            ),
        );
        if finish(&self.db, id, Status::Failed, &outcome).unwrap_or(false) {
            self.report(id, label, Status::Failed);
        }
    }

    /// Tell the parent's stream, when anybody is listening.
    fn report(&self, id: &str, label: &str, status: Status) {
        if let Some(notify) = &self.notify {
            notify(&Report {
                id,
                label,
                status: status.as_str(),
            });
        }
    }
}

/// The tool spec for `dispatch_agent`.
///
/// The description states the three things a model needs to decide whether to use
/// it: that it returns immediately, that the child has no tools, and that the caps
/// are real. A model that does not know a subagent is tool-less will hand it work
/// that needs a command.
#[must_use]
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL.to_owned(),
        description: format!(
            "Hand one self-contained task to a subagent and return immediately with its id. \
             The subagent is a separate model turn with this turn's system prompt and persona, \
             its own token budget and its own timeout, running in the background while this \
             turn carries on. It has no tools at all: it cannot read or write files, run \
             commands, browse, or dispatch another subagent, so give it a task that can be \
             done in one answer from the prompt alone. At most {MAX_RUNNING} run at once and \
             {MAX_PER_TURN} per turn; beyond that the call is refused and says why. The result \
             appears on the subagents control in the top bar."
        ),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "label": {
                    "type": "string",
                    "description": format!(
                        "A few words naming the task, for the top bar. At most \
                         {MAX_LABEL_CHARS} characters."
                    )
                },
                "prompt": {
                    "type": "string",
                    "description": "The whole task, written as if to the model itself. The \
                                    subagent cannot ask a follow-up question."
                }
            },
            "required": ["label", "prompt"]
        }),
    }
}

/// A label for a dispatch that did not send one: the prompt's first line, clipped.
#[must_use]
pub fn label_for(label: &str, prompt: &str) -> String {
    let given = label.trim();
    if !given.is_empty() {
        return clip(given.lines().next().unwrap_or(given), MAX_LABEL_CHARS);
    }
    let first = prompt.trim().lines().next().unwrap_or("subagent").trim();
    if first.is_empty() {
        return "subagent".to_owned();
    }
    clip(first, MAX_LABEL_CHARS)
}

/// Cut a string to `max` characters, saying so when it was cut.
///
/// Characters rather than bytes, so a multi-byte prompt is not split mid-codepoint
/// and a truncated label stays readable.
#[must_use]
pub fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(max).collect();
    out.push_str(" ...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("in-memory database")
    }

    fn request<'a>(
        conversation: &'a str,
        turn: Option<&'a str>,
        label: &'a str,
        prompt: &'a str,
    ) -> Request<'a> {
        Request {
            conversation,
            parent_turn: turn,
            label,
            prompt,
            model: "stub/model",
        }
    }

    /// A desk whose child turn is a closure, so the whole lifecycle runs with no
    /// network and no provider.
    fn desk(db: &Db, runner: Arc<Runner>) -> Desk {
        let config = TurnConfig {
            base_url: "http://127.0.0.1:1/v1".to_owned(),
            key_ref: core_runtime::provider::SecretRef::try_from("env:KALLISTI_TEST_KEY")
                .expect("a valid reference"),
            model: "stub/model".to_owned(),
        };
        Desk::new(
            db.clone(),
            config,
            Some("you are eris".to_owned()),
            std::env::temp_dir(),
            "main".to_owned(),
            Some("answer-row".to_owned()),
        )
        .with_runner(runner)
    }

    fn answered(text: &str) -> RunOutput {
        RunOutput {
            text: text.to_owned(),
            usage: core_domain::Usage::reconcile(
                core_domain::CacheReadReporting::NotReported,
                Some(11),
                None,
                Some(5),
            ),
        }
    }

    fn quiet_runner(text: &'static str) -> Arc<Runner> {
        Arc::new(move |_request: &RunRequest| Ok(answered(text)))
    }

    /// The migration, the version, and the columns the feature actually reads.
    #[test]
    fn migration_fourteen_creates_the_subagent_row() {
        let db = db();
        // The pin follows the newest migration, which is 0015. This test owns the
        // "the binary and the file agree on the version" claim for the whole
        // database, so it is the one place that number is written down twice.
        assert_eq!(
            crate::db::SCHEMA_VERSION,
            15,
            "the binary declares the version it migrated to"
        );
        assert_eq!(db.schema_version().expect("version"), 15);
        assert_eq!(db.applied_migration_count().expect("count"), 15);
        assert!(
            crate::db::TABLES.contains(&"subagents"),
            "the table is in the documented list"
        );
        let columns = db.column_names("subagents").expect("subagents");
        for expected in [
            "id",
            "parent_conversation",
            "parent_turn",
            "label",
            "prompt",
            "status",
            "result",
            "error_code",
            "model",
            "prompt_tokens",
            "completion_tokens",
            "created_at",
            "started_at",
            "finished_at",
        ] {
            assert!(
                columns.iter().any(|column| column == expected),
                "subagents is missing {expected}: {columns:?}"
            );
        }
        assert_eq!(db.row_count("subagents").expect("rows"), 0);
    }

    /// The status column is a closed set, so a settled row cannot claim a name the
    /// bar does not know.
    #[test]
    fn the_status_column_refuses_a_value_outside_the_set() {
        let db = db();
        let id = reserve(&db, &request("main", Some("t1"), "one", "do a thing")).expect("queued");
        let refused = db.with_conn(|conn| {
            conn.execute(
                "UPDATE subagents SET status = 'paused' WHERE id = ?1",
                params![id],
            )
        });
        assert!(
            refused.is_err(),
            "an unknown status is a constraint failure"
        );
    }

    #[test]
    fn a_dispatch_over_the_running_cap_is_refused_by_name() {
        let db = db();
        for index in 0..MAX_RUNNING {
            let turn = format!("turn-{index}");
            let id = reserve(&db, &request("main", Some(&turn), "one", "work")).expect("queued");
            assert!(mark_running(&db, &id).expect("claimed"));
        }
        assert_eq!(running_count(&db).expect("count"), MAX_RUNNING);

        let refused = reserve(&db, &request("main", Some("turn-late"), "one", "work"))
            .expect_err("the cap refuses the fifth");
        assert_eq!(refused.code(), "subagents_at_capacity");
        assert!(
            refused.reason().contains(&MAX_RUNNING.to_string()),
            "the refusal names the cap: {}",
            refused.reason()
        );
        assert_eq!(
            db.row_count("subagents").expect("rows"),
            MAX_RUNNING,
            "a refused dispatch writes no row"
        );
    }

    #[test]
    fn a_dispatch_over_the_per_turn_budget_is_refused_by_name() {
        let db = db();
        // Finished rows, so the running cap is not what stops the ninth.
        for _ in 0..MAX_PER_TURN {
            let id = reserve(&db, &request("main", Some("turn-a"), "one", "work")).expect("queued");
            let outcome = Outcome::failed("stub", "nothing to do");
            assert!(finish(&db, &id, Status::Failed, &outcome).expect("settled"));
        }
        assert_eq!(running_count(&db).expect("running"), 0);

        let refused = reserve(&db, &request("main", Some("turn-a"), "one", "work"))
            .expect_err("the budget refuses the ninth");
        assert_eq!(refused.code(), "turn_dispatch_budget_spent");
        assert!(
            refused.reason().contains(&MAX_PER_TURN.to_string()),
            "the refusal names the budget: {}",
            refused.reason()
        );

        // A different turn still has its own budget, which is what makes the cap
        // per turn rather than per conversation.
        assert!(
            reserve(&db, &request("main", Some("turn-b"), "one", "work")).is_ok(),
            "another turn dispatches normally"
        );
    }

    /// The lifecycle the brief asks for: queued, then running, then succeeded,
    /// with the stub proving the row was `running` while the child was executing.
    #[test]
    fn a_row_runs_from_queued_through_running_to_succeeded() {
        let db = db();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observer = Arc::clone(&seen);
        let watched = db.clone();
        let runner: Arc<Runner> = Arc::new(move |run: &RunRequest| {
            let rows = bar(&watched, 10).expect("rows");
            observer
                .lock()
                .expect("observer")
                .push(rows.first().map(|row| row.status));
            assert_eq!(run.prompt, "summarise the incident");
            assert_eq!(run.system.as_deref(), Some("you are eris"));
            Ok(answered("the incident was a stale lease"))
        });

        let id = reserve(
            &db,
            &request(
                "main",
                Some("answer-row"),
                "summary",
                "summarise the incident",
            ),
        )
        .expect("queued");
        let queued = get(&db, &id).expect("read").expect("one row");
        assert_eq!(queued.status, Status::Queued);
        assert_eq!(queued.model, "stub/model");
        assert_eq!(queued.parent_conversation, "main");
        assert_eq!(queued.parent_turn.as_deref(), Some("answer-row"));
        assert!(queued.finished_at.is_none());

        let desk = desk(&db, runner);
        assert!(desk.execute(&id, "summary", "summarise the incident"));

        assert_eq!(
            *seen.lock().expect("observer"),
            vec![Some(Status::Running)],
            "the row is running while the child executes"
        );
        let done = get(&db, &id).expect("read").expect("one row");
        assert_eq!(done.status, Status::Succeeded);
        assert_eq!(
            done.result.as_deref(),
            Some("the incident was a stale lease")
        );
        assert_eq!(done.error_code, None);
        assert_eq!(done.prompt_tokens, Some(11));
        assert_eq!(done.completion_tokens, Some(5));
        assert!(done.started_at.is_some(), "the start is stamped");
        assert!(done.finished_at.is_some(), "the finish is stamped");
    }

    #[test]
    fn a_failed_child_records_its_code_and_still_carries_the_reason() {
        let db = db();
        let runner: Arc<Runner> = Arc::new(|_run: &RunRequest| {
            Err(("auth".to_owned(), "the key was refused".to_owned()))
        });
        let desk = desk(&db, runner);
        let id = reserve(&db, &request("main", Some("t"), "one", "work")).expect("queued");
        assert!(desk.execute(&id, "one", "work"));

        let row = get(&db, &id).expect("read").expect("one row");
        assert_eq!(row.status, Status::Failed);
        assert_eq!(row.error_code.as_deref(), Some("auth"));
        assert_eq!(row.result.as_deref(), Some("the key was refused"));
    }

    /// A settled row is settled: a child that returns after its watchdog already
    /// failed it writes nothing.
    #[test]
    fn a_late_write_does_not_overwrite_a_settled_row() {
        let db = db();
        let desk = desk(&db, quiet_runner("late"));
        let id = reserve(&db, &request("main", Some("t"), "one", "work")).expect("queued");
        let expired = Outcome::failed("subagent_timeout", "the deadline passed");
        assert!(finish(&db, &id, Status::Failed, &expired).expect("settled"));

        assert!(
            !desk.execute(&id, "one", "work"),
            "a claimed-and-settled row is not run"
        );
        let row = get(&db, &id).expect("read").expect("one row");
        assert_eq!(row.status, Status::Failed);
        assert_eq!(row.error_code.as_deref(), Some("subagent_timeout"));
    }

    /// A dispatch writes its row before spawning, and the id is a row rather than
    /// a promise.
    #[test]
    fn a_dispatch_returns_a_row_it_already_wrote() {
        let db = db();
        let desk = desk(&db, quiet_runner("done"));
        let id = desk
            .dispatch("nightly report", "summarise yesterday")
            .expect("dispatched");
        let row = get(&db, &id).expect("read").expect("the row exists");
        assert_eq!(row.label, "nightly report");
        assert_eq!(row.prompt, "summarise yesterday");
        assert_eq!(row.parent_turn.as_deref(), Some("answer-row"));
        assert_eq!(desk.model(), "stub/model");
    }

    /// No label, no problem: the prompt's first line names the row.
    #[test]
    fn a_label_falls_back_to_the_first_line_of_the_prompt() {
        assert_eq!(
            label_for("", "summarise the report\nand more"),
            "summarise the report"
        );
        assert_eq!(label_for("   ", "   "), "subagent");
        let long = "x".repeat(MAX_LABEL_CHARS + 20);
        let clipped = label_for("", &long);
        assert!(clipped.ends_with(" ..."), "{clipped}");
        assert_eq!(clipped.chars().count(), MAX_LABEL_CHARS + 4);
    }

    /// The whole point of the caps: an interrupted daemon must not leave the
    /// concurrency budget spent forever.
    #[test]
    fn boot_settles_what_a_previous_process_left_running() {
        let db = db();
        let id = reserve(&db, &request("main", Some("t"), "one", "work")).expect("queued");
        assert!(mark_running(&db, &id).expect("claimed"));
        assert_eq!(fail_orphaned(&db).expect("swept"), 1);
        let row = get(&db, &id).expect("read").expect("one row");
        assert_eq!(row.status, Status::Failed);
        assert_eq!(row.error_code.as_deref(), Some("interrupted"));
        assert_eq!(running_count(&db).expect("running"), 0);
    }

    /// A subagent gets no tools and cannot dispatch another one. All three layers
    /// are asserted, because each one alone is a single point of failure.
    #[test]
    fn a_child_turn_is_offered_no_tools_and_cannot_dispatch() {
        let settings = child_settings();
        assert!(!settings.tools, "a subagent is never offered a tool");
        assert!(
            !settings.agents,
            "a subagent cannot dispatch another subagent"
        );

        // The child's sandbox is read-only whatever the parent's mode is, so the
        // tool list for its own mode carries neither commands nor dispatching.
        let workspace = std::env::temp_dir();
        for parent in [
            crate::tools::SandboxMode::Full,
            crate::tools::SandboxMode::WorkspaceWrite,
        ] {
            let child = child_sandbox(workspace.clone());
            let names: Vec<String> = crate::tools::specs_offering(child.mode(), settings.agents)
                .into_iter()
                .map(|spec| spec.name)
                .collect();
            assert!(
                !names.contains(&TOOL.to_owned()),
                "a {parent:?} turn's child is never offered dispatching: {names:?}"
            );
            assert!(
                !names.contains(&"run_command".to_owned()),
                "a child is never offered a command: {names:?}"
            );
        }

        // And the spec the parent sees says so, so a model cannot hand a
        // tool-less child work that needs a command.
        let spec = spec();
        assert_eq!(spec.name, TOOL);
        assert!(
            spec.description.contains("no tools"),
            "the spec states that a subagent has no tools: {}",
            spec.description
        );
        assert!(
            spec.description.contains("cannot read or write files"),
            "the limit is named, not implied: {}",
            spec.description
        );
        assert!(
            spec.description.contains("dispatch another subagent"),
            "no recursion is stated: {}",
            spec.description
        );
        let required = spec
            .parameters
            .get("required")
            .and_then(serde_json::Value::as_array)
            .expect("required parameters");
        assert_eq!(required.len(), 2, "label and prompt: {}", spec.parameters);
    }

    #[test]
    fn a_stored_false_is_the_only_way_dispatching_is_off() {
        assert!(enabled(None), "default on");
        assert!(enabled(Some("true")));
        assert!(enabled(Some("")));
        assert!(!enabled(Some("false")));
        assert!(!enabled(Some(" false ")), "a padded value still reads");
    }

    #[test]
    fn elapsed_reads_as_seconds_then_minutes() {
        assert_eq!(elapsed_label(0), "0s");
        assert_eq!(elapsed_label(59), "59s");
        assert_eq!(elapsed_label(60), "1m 00s");
        assert_eq!(elapsed_label(-5), "0s");
        assert_eq!(elapsed_label(4925), "82m 05s");
    }
}
