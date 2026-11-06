//! The background half of the learning loop.
//!
//! One task, three jobs, each bounded on purpose:
//!
//! 1. **Sweep.** Find finished answers the loop has not considered yet and queue
//!    at most a few of them, inside a rolling-hour budget. A turn that streamed
//!    nothing worth reading is not queued. The watermark means a restart reviews
//!    nothing twice.
//! 2. **Review one.** Claim the oldest queued run, build a bounded digest of the
//!    exchange, ask the model for one JSON object, and apply it through the
//!    policy in [`crate::learning`]. One run per tick: each review is a billed
//!    provider call, and a queue that drains itself at full speed is a queue that
//!    spends a day's budget in a minute.
//! 3. **Curate.** When the interval has elapsed, age the learned skills: active
//!    to stale after a month, stale to archived after a quarter, and back again
//!    when one is read. Only agent-written, unpinned skills move.
//!
//! The provider seam is a `Runner` so the whole lifecycle - queue, claim, digest,
//! apply, settle - is testable without a network, exactly like the note worker.

use std::path::PathBuf;
use std::sync::Arc;

use rusqlite::params;
use time::OffsetDateTime;

use crate::db::Db;
use crate::learning::{self, LearningError, Origin, Review, Shelf, State};
use crate::onboarding::SetupEngine;
use crate::web::chat;
use core_runtime::provider::{ChatMessage, ChatRole};

/// How often the worker looks for work.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(20);

/// How long a claimed review is owned before another tick may take it.
pub const LEASE_SECONDS: i64 = 300;

/// How many finished answers one sweep may queue. A burst of turns should be
/// reviewed over several ticks, not all at once.
pub const SWEEP_LIMIT: usize = 3;

/// The most a run's summary may carry.
pub const MAX_SUMMARY_CHARS: usize = 400;

/// What a review is asked to read.
#[derive(Debug, Clone)]
pub struct WorkRequest {
    /// The provider, key reference and model for the review.
    pub config: chat::TurnConfig,
    /// The system message.
    pub system: String,
    /// The digest and the learned-skill catalog.
    pub prompt: String,
    /// The workspace a review may read, if a future revision grants it tools.
    /// Today the review is tool-less, so this is carried for the seam's shape.
    pub workspace: PathBuf,
}

/// What one review produced.
#[derive(Debug, Clone)]
pub struct WorkOutput {
    /// The raw reply, which [`learning::parse_review`] turns into an action.
    pub text: String,
}

/// How a review is produced.
pub type Runner = dyn Fn(&WorkRequest) -> Result<WorkOutput, (String, String)> + Send + Sync;

/// One queued review, as claimed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claimed {
    /// Run id.
    pub id: String,
    /// The conversation the answer belongs to.
    pub conversation_id: String,
    /// The assistant row the review is about.
    pub answer_id: String,
    /// How many attempts this row has had, including this one.
    pub attempt: i64,
}

/// The worker: an engine to read settings and the provider from, a shelf to
/// write into, and a seam to run the model through.
pub struct LearningWorker {
    engine: SetupEngine,
    shelf: Option<Shelf>,
    runner: Arc<Runner>,
}

impl LearningWorker {
    /// Build a worker around an explicit runner and shelf. Tests use this.
    #[must_use]
    pub fn new(engine: SetupEngine, shelf: Option<Shelf>, runner: Arc<Runner>) -> Self {
        Self {
            engine,
            shelf,
            runner,
        }
    }

    /// The production worker: the library and history the daemon itself uses.
    #[must_use]
    pub fn production(engine: SetupEngine) -> Self {
        let shelf = Shelf::for_engine(&engine);
        if let Some(shelf) = &shelf {
            if let Err(error) = std::fs::create_dir_all(&shelf.dir) {
                tracing::warn!(%error, dir = %shelf.dir.display(), "learning: the library could not be created");
            }
        }
        Self::new(engine, shelf, Arc::new(production_runner))
    }

    /// One tick: sweep, review at most one queued run, curate when due.
    ///
    /// `Ok(true)` means a review was executed. A tick that only swept or curated
    /// is `Ok(false)`, which is the ordinary case.
    ///
    /// # Errors
    ///
    /// [`LearningError`] when the database or the shelf refused. A failure that
    /// belongs to one run is written onto that run instead.
    pub fn run_once(&self) -> Result<bool, LearningError> {
        let db = self.engine.db();
        if !learning::enabled(&db) {
            return Ok(false);
        }
        let Some(shelf) = self.shelf.as_ref() else {
            tracing::warn!("learning: no library directory resolves, so the loop is idle");
            return Ok(false);
        };
        let swept = self.sweep(&db)?;
        if swept > 0 {
            tracing::info!(queued = swept, "learning: queued reviews");
        }
        let reviewed = match self.claim_next(&db)? {
            Some(claimed) => {
                self.execute(&db, shelf, &claimed)?;
                true
            }
            None => false,
        };
        if curator_due(&db) {
            match run_curator(&db, shelf) {
                Ok(report) => tracing::info!(
                    checked = report.checked,
                    archived = report.archived,
                    stale = report.marked_stale,
                    "learning: curator pass complete"
                ),
                Err(error) => tracing::warn!(%error, "learning: curator pass failed"),
            }
        }
        Ok(reviewed)
    }

    /// Queue every finished answer newer than the watermark, up to the sweep and
    /// budget ceilings.
    fn sweep(&self, db: &Db) -> Result<usize, LearningError> {
        if !learning::review_enabled(db) {
            return Ok(0);
        }
        let watermark = match crate::web::store::settings_get(db, learning::SETTING_WATERMARK)? {
            Some(raw) => raw,
            None => {
                // First observation seeds the mark and defers one pass: a fresh
                // install must not review every answer in its history.
                crate::web::store::settings_put(db, learning::SETTING_WATERMARK, &learning::now())?;
                return Ok(0);
            }
        };
        let floor = learning::min_chars(db);
        let budget = learning::max_per_hour(db);
        let since = (OffsetDateTime::now_utc() - time::Duration::hours(1))
            .format(&time::format_description::well_known::Rfc3339)
            .map(|text| text.replace("+00:00", "Z"))
            .unwrap_or_else(|_| learning::now());
        let queued = db.with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM learning_runs WHERE created_at > ?1",
                params![since],
                |row| row.get::<_, i64>(0),
            )
        })?;
        if queued >= budget {
            return Ok(0);
        }
        let candidates: Vec<(String, String, String, Option<String>, String)> = db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, conversation_id, body, tools, created_at FROM chat_messages \
                  WHERE role = 'assistant' AND state = 'complete' AND created_at > ?1 \
                  ORDER BY created_at ASC LIMIT ?2",
            )?;
            let mapped = statement.query_map(params![watermark, SWEEP_LIMIT as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?;
            mapped.collect::<Result<Vec<_>, _>>()
        })?;
        let mut added = 0usize;
        let mut newest = watermark.clone();
        for (answer_id, conversation_id, body, tools, created_at) in candidates {
            newest = created_at.clone();
            let has_tools = tools
                .as_deref()
                .is_some_and(|raw| !raw.trim().is_empty() && raw.trim() != "[]");
            if body.chars().count() < usize::try_from(floor).unwrap_or(0) && !has_tools {
                continue;
            }
            if queued + added as i64 >= budget {
                break;
            }
            let id = uuid::Uuid::now_v7().to_string();
            let inserted = db.with_conn(|conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO learning_runs (id, conversation_id, answer_id) \
                     VALUES (?1, ?2, ?3)",
                    params![id, conversation_id, answer_id],
                )
            })?;
            if inserted > 0 {
                added += 1;
            }
        }
        if newest != watermark {
            crate::web::store::settings_put(db, learning::SETTING_WATERMARK, &newest)?;
        }
        Ok(added)
    }

    /// Claim the oldest queued (or lease-expired) review.
    fn claim_next(&self, db: &Db) -> Result<Option<Claimed>, LearningError> {
        let now = learning::now();
        let expired = (OffsetDateTime::now_utc() - time::Duration::seconds(LEASE_SECONDS))
            .format(&time::format_description::well_known::Rfc3339)
            .map(|text| text.replace("+00:00", "Z"))
            .unwrap_or_else(|_| now.clone());
        let candidate: Option<(String, String, String, i64)> = db.with_conn(|conn| {
            conn.query_row(
                "SELECT id, conversation_id, answer_id, attempts FROM learning_runs \
                  WHERE status = 'queued' \
                     OR (status = 'running' AND lease_expires_at IS NOT NULL AND lease_expires_at < ?1) \
                  ORDER BY created_at ASC LIMIT 1",
                params![expired],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
        })?;
        let Some((id, conversation_id, answer_id, attempts)) = candidate else {
            return Ok(None);
        };
        let lease = (OffsetDateTime::now_utc() + time::Duration::seconds(LEASE_SECONDS))
            .format(&time::format_description::well_known::Rfc3339)
            .map(|text| text.replace("+00:00", "Z"))
            .unwrap_or_else(|_| now.clone());
        let changed = db.with_conn(|conn| {
            conn.execute(
                "UPDATE learning_runs SET status = 'running', attempts = attempts + 1, \
                        started_at = ?2, lease_expires_at = ?3 \
                  WHERE id = ?1 AND (status = 'queued' OR (status = 'running' AND lease_expires_at < ?4))",
                params![id, now, lease, expired],
            )
        })?;
        if changed == 0 {
            return Ok(None);
        }
        Ok(Some(Claimed {
            id,
            conversation_id,
            answer_id,
            attempt: attempts + 1,
        }))
    }

    /// Execute one claimed review.
    fn execute(
        &self,
        db: &Db,
        shelf: &Shelf,
        claimed: &Claimed,
    ) -> Result<(), LearningError> {
        let messages = match chat::load(db, &claimed.conversation_id) {
            Ok(messages) => messages,
            Err(error) => return self.fail(db, claimed, "conversation_unreadable", &error.to_string()),
        };
        let Some(digest) = learning::digest(&messages, &claimed.answer_id) else {
            return self.settle(
                db,
                claimed,
                "skipped",
                None,
                None,
                Some("the answer is no longer in the transcript"),
                None,
            );
        };
        let Some(config) = self.review_config(db) else {
            return self.fail(
                db,
                claimed,
                "provider_not_configured",
                "no provider is stored, so a review cannot run",
            );
        };
        let learned = learning::rows(db)?;
        let prompt = learning::user_prompt(&digest, &learned);
        let request = WorkRequest {
            config: config.clone(),
            system: learning::SYSTEM.to_owned(),
            prompt,
            workspace: self
                .engine
                .workspace_path()
                .unwrap_or_else(|_| PathBuf::from(".")),
        };
        let output = match (self.runner)(&request) {
            Ok(output) => output,
            Err((code, message)) => return self.fail(db, claimed, &code, &message),
        };
        let review: Review = match learning::parse_review(&output.text) {
            Ok(review) => review,
            Err(error) => return self.fail(db, claimed, "invalid_review", &error.to_string()),
        };
        let applied = match learning::apply(db, shelf, &review, Some(&claimed.id)) {
            Ok(applied) => applied,
            Err(LearningError::Refused(reason)) => {
                return self.settle(db, claimed, "skipped", None, None, Some(&reason), None)
            }
            Err(other) => return Err(other),
        };
        let status = if applied.action == "none" { "skipped" } else { "done" };
        self.settle(
            db,
            claimed,
            status,
            Some(applied.action),
            applied.key.as_deref(),
            Some(&applied.note),
            Some(&config.model),
        )
    }

    /// The config a review runs on: the `learning` role when pinned, else the
    /// primary model.
    fn review_config(&self, db: &Db) -> Option<chat::TurnConfig> {
        let _ = db;
        chat::aux_pinned_config(&self.engine, "learning")
            .ok()
            .flatten()
            .or_else(|| chat::turn_config(&self.engine).ok().flatten())
    }

    fn fail(
        &self,
        db: &Db,
        claimed: &Claimed,
        code: &str,
        message: &str,
    ) -> Result<(), LearningError> {
        self.settle(db, claimed, "failed", None, None, Some(message), None)
            .and_then(|()| {
                db.with_conn(|conn| {
                    conn.execute(
                        "UPDATE learning_runs SET error_code = ?2 WHERE id = ?1",
                        params![claimed.id, code],
                    )?;
                    Ok(())
                })
                .map_err(LearningError::Db)
            })
    }

    /// Settle one run. `summary` is truncated to the column's honest size.
    fn settle(
        &self,
        db: &Db,
        claimed: &Claimed,
        status: &str,
        action: Option<&str>,
        key: Option<&str>,
        summary: Option<&str>,
        model: Option<&str>,
    ) -> Result<(), LearningError> {
        let summary = summary.map(|text| {
            let count = text.chars().count();
            if count <= MAX_SUMMARY_CHARS {
                text.to_owned()
            } else {
                let cut: String = text.chars().take(MAX_SUMMARY_CHARS).collect();
                format!("{cut}...")
            }
        });
        let result = serde_json::json!({
            "status": status,
            "action": action,
            "skill": key,
            "summary": summary,
        });
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE learning_runs SET status = ?2, finished_at = ?3, lease_expires_at = NULL, \
                        action = ?4, skill_key = ?5, summary = ?6, model = COALESCE(?7, model), \
                        result_json = ?8 \
                  WHERE id = ?1",
                params![
                    claimed.id,
                    status,
                    learning::now(),
                    action,
                    key,
                    summary,
                    model,
                    result.to_string()
                ],
            )?;
            Ok(())
        })?;
        tracing::info!(
            run = %claimed.id,
            attempt = claimed.attempt,
            status,
            action = action.unwrap_or("-"),
            skill = key.unwrap_or("-"),
            "learning: review settled"
        );
        Ok(())
    }
}

/// Whether a maintenance pass is due.
///
/// The first observation seeds the clock and defers one interval, so a fresh
/// install never prunes a library it has not looked at yet.
#[must_use]
pub fn curator_due(db: &Db) -> bool {
    if !learning::curator_enabled(db) {
        return false;
    }
    let Ok(stored) = crate::web::store::settings_get(db, learning::SETTING_LAST_CURATOR) else {
        return false;
    };
    let Some(raw) = stored else {
        let _ = crate::web::store::settings_put(db, learning::SETTING_LAST_CURATOR, &learning::now());
        return false;
    };
    let Some(last) = learning::parse_timestamp(&raw) else {
        let _ = crate::web::store::settings_put(db, learning::SETTING_LAST_CURATOR, &learning::now());
        return false;
    };
    OffsetDateTime::now_utc() - last >= time::Duration::hours(learning::interval_hours(db))
}

/// What one maintenance pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CuratorOutcome {
    /// Learned, unpinned skills that were examined.
    pub checked: i64,
    /// Skills marked stale.
    pub marked_stale: i64,
    /// Skills archived.
    pub archived: i64,
    /// Skills brought back from stale.
    pub reactivated: i64,
}

/// One maintenance pass: reconcile the ledger, then age the learned skills.
///
/// # Errors
///
/// [`LearningError`] when the ledger or the shelf refused. A skill whose directory
/// cannot be moved is logged and left alone rather than aborting the pass.
pub fn run_curator(db: &Db, shelf: &Shelf) -> Result<CuratorOutcome, LearningError> {
    let started = learning::now();
    let _ = learning::sync_library(db, &shelf.dir);
    let stale = time::Duration::days(learning::stale_days(db));
    let archive = time::Duration::days(learning::archive_days(db));
    let now = OffsetDateTime::now_utc();
    let mut outcome = CuratorOutcome::default();
    for row in learning::rows(db)? {
        if row.created_by != Origin::Agent || row.pinned {
            continue;
        }
        let anchor = learning::parse_timestamp(
            row.last_activity_at.as_deref().unwrap_or(&row.created_at),
        );
        let Some(anchor) = anchor else {
            continue;
        };
        let age = now - anchor;
        outcome.checked += 1;
        let on_disk = shelf.dir.join(&row.key).is_dir();
        if age >= archive {
            if row.state != State::Archived {
                if on_disk {
                    match learning::archive(shelf, &row.key) {
                        Ok(path) => {
                            tracing::info!(skill = %row.key, path = %path.display(), "learning: archived a stale skill");
                        }
                        Err(error) => {
                            tracing::warn!(%error, skill = %row.key, "learning: archive failed; the skill is left in place");
                            continue;
                        }
                    }
                }
                learning::mark_archived(db, &row.key)?;
                outcome.archived += 1;
            }
        } else if age >= stale && row.state == State::Active {
            learning::set_state(db, &row.key, State::Stale)?;
            outcome.marked_stale += 1;
        } else if age < stale && row.state == State::Stale {
            learning::set_state(db, &row.key, State::Active)?;
            outcome.reactivated += 1;
        }
    }
    let id = uuid::Uuid::now_v7().to_string();
    let report = serde_json::json!({
        "checked": outcome.checked,
        "marked_stale": outcome.marked_stale,
        "archived": outcome.archived,
        "reactivated": outcome.reactivated,
    });
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO curator_runs (id, started_at, finished_at, checked, marked_stale, archived, reactivated, report_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                id,
                started,
                learning::now(),
                outcome.checked,
                outcome.marked_stale,
                outcome.archived,
                outcome.reactivated,
                report.to_string()
            ],
        )?;
        Ok(())
    })?;
    crate::web::store::settings_put(db, learning::SETTING_LAST_CURATOR, &learning::now())?;
    Ok(outcome)
}

/// The production runner: one tool-less turn through the ordinary loop, with the
/// review's system message and no chance to act on anything it reads.
fn production_runner(request: &WorkRequest) -> Result<WorkOutput, (String, String)> {
    let key = crate::oauth::credential::resolve_key_now(&request.config.key_ref)
        .map_err(|error| (error.kind().to_owned(), error.to_string()))?;
    let sandbox = crate::tools::Sandbox::new(
        request.workspace.clone(),
        crate::onboarding::permissions::SandboxMode::ReadOnly,
    );
    // A review reads a digest that is already in its prompt. A tool call would be
    // the model deciding to go looking on its own, which is an action with no
    // operator watching it; there is no surface to refuse from, so every call is
    // refused here.
    let mut gate = |_call: &core_domain::ToolCall| chat::GateDecision::Refuse {
        reason: "a learning review has no tools; answer from the digest".to_owned(),
    };
    let mut sink = |_part: chat::StreamPart<'_>| {};
    let mut poll = || chat::TurnSignal::Continue;
    let messages = vec![ChatMessage::text(ChatRole::User, request.prompt.clone())];
    let outcome = chat::run_turn_with_key(
        &request.config,
        &key,
        Some(request.system.clone()),
        messages,
        &sandbox,
        &mut gate,
        None,
        None,
        None,
        None,
        None,
        None,
        chat::TurnSettings {
            tools: false,
            max_rounds: 1,
            agents: false,
            context_budget: None,
        },
        None,
        &mut poll,
        &mut sink,
    )?;
    Ok(WorkOutput { text: outcome.text })
}

/// Run the loop until the daemon stops.
///
/// One tick at a time on a blocking task, so a slow provider never holds the
/// listeners and a review that takes a minute is simply the next tick's
/// predecessor.
pub async fn run(state: crate::admin::AppState) {
    // Claim what is already on disk before the first tick. A skill the operator
    // wrote is recorded as theirs once, at startup, so the first review never
    // guesses and the page can count the library it inherited.
    if let Some(shelf) = Shelf::for_engine(&state.engine) {
        match learning::sync_library(&state.engine.db(), &shelf.dir) {
            Ok(rows) => tracing::info!(rows, library = %shelf.dir.display(), "learning: ledger seeded"),
            Err(error) => tracing::warn!(%error, "learning: the ledger could not be seeded"),
        }
    }
    let worker = Arc::new(LearningWorker::production(state.engine.clone()));
    let mut interval = tokio::time::interval(TICK);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let worker = worker.clone();
        match tokio::task::spawn_blocking(move || worker.run_once()).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::error!(%error, "learning: the worker errored"),
            Err(join) => tracing::error!(%join, "learning: the worker task did not finish"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learning::Switches;
    use crate::web::chat::Role;

    fn engine() -> (tempfile::TempDir, SetupEngine) {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().to_path_buf();
        let db = crate::db::Db::open(root.join("state/eris.db")).expect("db opens");
        let paths = crate::config::Paths::for_root(root.join("root"));
        let engine = SetupEngine::new(db, paths);
        engine
            .set_provider(
                "http://127.0.0.1:20128/v1",
                "env:TEST_PROVIDER_KEY",
                &["test/model".to_owned()],
            )
            .expect("a provider");
        engine.set_model("test/model").expect("a model");
        std::fs::create_dir_all(root.join("skills")).expect("library");
        (dir, engine)
    }

    fn shelf(root: &std::path::Path) -> Shelf {
        Shelf::new(root.join("skills"), root.join("history"))
    }

    fn worker(engine: SetupEngine, shelf: Shelf, reply: &'static str) -> LearningWorker {
        let runner: Arc<Runner> = Arc::new(move |_request: &WorkRequest| {
            Ok(WorkOutput {
                text: reply.to_owned(),
            })
        });
        LearningWorker::new(engine, Some(shelf), runner)
    }

    fn answer(db: &Db) -> (String, String) {
        let conversation = chat::create_conversation(db).expect("conversation");
        chat::append(db, &conversation, Role::User, "why did the deploy fail?", chat::State::Complete)
            .expect("question");
        let answer = chat::append_answer(db, &conversation, "test/model", None).expect("answer");
        chat::settle(
            db,
            &answer,
            "The guard baseline had drifted, so the deploy script stopped before touching the host. \
             The operator ran the snapshot first, then the check, then the deploy, and only then did the \
             turn settle cleanly. That order is the lesson: snapshot, check, deploy, and never edit the \
             baseline to make a drifted host pass.",
            None,
            &[],
            None,
            chat::Accounting::default(),
        )
        .expect("settle");
        (conversation, answer)
    }

    #[test]
    fn a_first_sweep_seeds_the_watermark_and_queues_nothing() {
        let (_dir, engine) = engine();
        let db = engine.db();
        answer(&db);
        let worker = worker(engine.clone(), shelf(_dir.path()), "{}");
        assert_eq!(worker.sweep(&db).expect("sweep"), 0);
        let queued: i64 = db
            .with_conn(|conn| conn.query_row("SELECT COUNT(*) FROM learning_runs", [], |row| row.get(0)))
            .unwrap();
        assert_eq!(queued, 0, "the first tick only seeds the watermark");
    }

    #[test]
    fn a_later_sweep_queues_a_real_answer_once() {
        let (_dir, engine) = engine();
        let db = engine.db();
        let (conversation, answer_id) = answer(&db);
        crate::web::store::settings_put(
            &db,
            learning::SETTING_WATERMARK,
            "2000-01-01T00:00:00Z",
        )
        .expect("seed the mark");
        let worker = worker(engine.clone(), shelf(_dir.path()), "{}");
        assert_eq!(worker.sweep(&db).expect("sweep"), 1);
        assert_eq!(worker.sweep(&db).expect("second sweep"), 0);
        let claimed = worker.claim_next(&db).expect("claim").expect("a run");
        assert_eq!(claimed.conversation_id, conversation);
        assert_eq!(claimed.answer_id, answer_id);
    }

    #[test]
    fn a_short_answer_with_no_tools_is_not_queued() {
        let (_dir, engine) = engine();
        let db = engine.db();
        let conversation = chat::create_conversation(&db).unwrap();
        chat::append(&db, &conversation, Role::User, "hi", chat::State::Complete).unwrap();
        let answer = chat::append_answer(&db, &conversation, "test/model", None).unwrap();
        chat::settle(&db, &answer, "hello", None, &[], None, chat::Accounting::default()).unwrap();
        crate::web::store::settings_put(&db, learning::SETTING_WATERMARK, "2000-01-01T00:00:00Z")
            .unwrap();
        let worker = worker(engine.clone(), shelf(_dir.path()), "{}");
        assert_eq!(worker.sweep(&db).unwrap(), 0);
    }

    #[test]
    fn a_review_that_creates_a_skill_settles_done_and_the_skill_is_readable() {
        let (_dir, engine) = engine();
        let db = engine.db();
        answer(&db);
        crate::web::store::settings_put(&db, learning::SETTING_WATERMARK, "2000-01-01T00:00:00Z")
            .unwrap();
        let reply = r###"{"action":"create","key":"deploy-guard","name":"Deploy Guard","description":"Ship the daemon only behind the guard.","body":"## When to Use\nBefore any deploy.\n\n## Procedure\n- Run the guard snapshot first.\n- Then run the deploy script.\n","reason":"the operator corrected the deploy order"}"###;
        let worker = worker(engine.clone(), shelf(_dir.path()), reply);
        assert!(worker.run_once().unwrap());
        let row = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT status, action, skill_key FROM learning_runs",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
                )
            })
            .unwrap();
        assert_eq!(row, ("done".to_owned(), "create".to_owned(), "deploy-guard".to_owned()));
        let body = crate::skills::read(&db, Some(&_dir.path().join("skills").display().to_string()), "deploy-guard")
            .expect("the learned skill reads");
        assert!(body.contains("guard snapshot"), "{body}");
        let usage = learning::usage(&db, "deploy-guard").unwrap().unwrap();
        assert_eq!(usage.created_by, Origin::Agent);
    }

    #[test]
    fn an_unparseable_reply_fails_the_run_with_a_named_code() {
        let (_dir, engine) = engine();
        let db = engine.db();
        answer(&db);
        crate::web::store::settings_put(&db, learning::SETTING_WATERMARK, "2000-01-01T00:00:00Z")
            .unwrap();
        let worker = worker(engine.clone(), shelf(_dir.path()), "I learned a lot today.");
        assert!(worker.run_once().unwrap());
        let (status, code): (String, String) = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT status, COALESCE(error_code, '') FROM learning_runs",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
            })
            .unwrap();
        assert_eq!(status, "failed");
        assert_eq!(code, "invalid_review");
    }

    #[test]
    fn a_none_review_settles_skipped_and_writes_no_skill() {
        let (_dir, engine) = engine();
        let db = engine.db();
        answer(&db);
        crate::web::store::settings_put(&db, learning::SETTING_WATERMARK, "2000-01-01T00:00:00Z")
            .unwrap();
        let worker = worker(engine.clone(), shelf(_dir.path()), r#"{"action":"none","reason":"small talk"}"#);
        assert!(worker.run_once().unwrap());
        let status: String = db
            .with_conn(|conn| conn.query_row("SELECT status FROM learning_runs", [], |row| row.get(0)))
            .unwrap();
        assert_eq!(status, "skipped");
        assert!(!_dir.path().join("skills/deploy-host").exists());
    }

    #[test]
    fn the_loop_switched_off_does_nothing() {
        let (_dir, engine) = engine();
        let db = engine.db();
        answer(&db);
        crate::web::store::settings_put(
            &db,
            learning::SETTING_ENABLED,
            "false",
        )
        .unwrap();
        crate::web::store::settings_put(&db, learning::SETTING_WATERMARK, "2000-01-01T00:00:00Z")
            .unwrap();
        let worker = worker(engine.clone(), shelf(_dir.path()), "{}");
        assert!(!worker.run_once().unwrap());
        let queued: i64 = db
            .with_conn(|conn| conn.query_row("SELECT COUNT(*) FROM learning_runs", [], |row| row.get(0)))
            .unwrap();
        assert_eq!(queued, 0);
    }

    #[test]
    fn the_curator_ages_an_old_learned_skill_stale_then_archived() {
        let (_dir, engine) = engine();
        let db = engine.db();
        let shelf = shelf(_dir.path());
        // A learned skill created 40 days ago and never used.
        std::fs::create_dir_all(shelf.dir.join("old-one")).unwrap();
        std::fs::write(
            shelf.dir.join("old-one").join("SKILL.md"),
            "---\nname: Old One\ndescription: Old.\n---\n\n## Procedure\n\nold\n",
        )
        .unwrap();
        learning::record_created(&db, "old-one", Some("Old."), None).unwrap();
        let old = (OffsetDateTime::now_utc() - time::Duration::days(40))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
            .replace("+00:00", "Z");
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE skill_usage SET created_at = ?2, last_activity_at = ?2 WHERE key = ?1",
                params!["old-one", old],
            )
        })
        .unwrap();
        let outcome = run_curator(&db, &shelf).unwrap();
        assert_eq!(outcome.marked_stale, 1);
        assert_eq!(learning::usage(&db, "old-one").unwrap().unwrap().state, State::Stale);
        assert!(shelf.dir.join("old-one").exists(), "stale keeps the directory");

        // Another 60 days and the same pass archives it.
        let older = (OffsetDateTime::now_utc() - time::Duration::days(100))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
            .replace("+00:00", "Z");
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE skill_usage SET last_activity_at = ?2 WHERE key = ?1",
                params!["old-one", older],
            )
        })
        .unwrap();
        let outcome = run_curator(&db, &shelf).unwrap();
        assert_eq!(outcome.archived, 1);
        assert_eq!(learning::usage(&db, "old-one").unwrap().unwrap().state, State::Archived);
        assert!(!shelf.dir.join("old-one").exists());
        assert!(shelf.dir.join(learning::ARCHIVE_DIR).exists());
    }

    #[test]
    fn the_curator_never_touches_an_operator_skill_or_a_pinned_one() {
        let (_dir, engine) = engine();
        let db = engine.db();
        let shelf = shelf(_dir.path());
        std::fs::create_dir_all(shelf.dir.join("mine")).unwrap();
        std::fs::write(
            shelf.dir.join("mine").join("SKILL.md"),
            "---\nname: Mine\ndescription: Mine.\n---\n\nbody\n",
        )
        .unwrap();
        learning::sync_library(&db, &shelf.dir).unwrap();
        // A separate learned skill, created through the writer so its row is the
        // loop's, then pinned: the second gate the pass must respect.
        learning::create_skill(
            &shelf,
            "pinned",
            "Pinned",
            "Mine.",
            "## When to Use\nnever\n",
        )
        .unwrap();
        learning::record_created(&db, "pinned", Some("Mine."), None).unwrap();
        learning::set_pinned(&db, "pinned", true).unwrap();
        let old = (OffsetDateTime::now_utc() - time::Duration::days(400))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
            .replace("+00:00", "Z");
        db.with_conn(|conn| {
            conn.execute("UPDATE skill_usage SET created_at = ?1, last_activity_at = ?1", params![old])
        })
        .unwrap();
        let outcome = run_curator(&db, &shelf).unwrap();
        assert_eq!(outcome.checked, 0, "a pinned row is skipped before it is counted");
        assert_eq!(outcome.archived, 0);
        assert!(shelf.dir.join("mine").exists());
        assert!(shelf.dir.join("pinned").exists());
    }

    #[test]
    fn the_curator_clock_defers_its_first_run() {
        let (_dir, engine) = engine();
        let db = engine.db();
        assert!(!curator_due(&db), "a fresh install seeds the clock and waits");
        assert!(crate::web::store::settings_get(&db, learning::SETTING_LAST_CURATOR)
            .unwrap()
            .is_some());
        let switches = Switches::read(&db);
        assert!(switches.curator);
    }
}
