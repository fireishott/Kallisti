//! The note-enrichment worker: what actually executes a `POST /v1/notes/{id}/runs`.
//!
//! `notes.rs` records a run and owns the event log, the lease columns and the
//! enriched-revision table, but it deliberately never calls a model. This module
//! is the other half: it claims a queued run, builds the prompt from the note's
//! own content, runs exactly one tool-less turn through the same
//! `run_turn_with_key` every other turn uses, writes the event log as it goes and
//! finishes by storing an enriched revision and settling the run.
//!
//! # The contract the client reads
//!
//! One run, oldest first. Claiming is a conditional update against the lease, so
//! two ticks cannot both take the same run and a daemon that died mid-run leaves
//! a row that the next tick re-claims after the lease expires.
//!
//! Events, in the order they are written, each with a monotonic `seq`:
//!
//! - `started`   - `{"attempt": n}` - written before the model is called.
//! - `completed` - `{"revisionId": "…", "title": "…"}` - the enriched revision
//!   exists and the run is `completed`.
//! - `failed`    - `{"message": "…"}` - the run is `failed` and the reason is in
//!   both the event and the run row's `error_text`.
//! - `cancelled` - `{}` - the client cancelled while the model was running, so
//!   the result is discarded rather than stored.
//!
//! # What it deliberately does not do
//!
//! A tool-less turn. The app's own enrichment prompt asks for `vision_analyze`
//! and web search, and this worker has neither: it reads the note's recognized
//! text, which is the OCR hint the device already uploaded. That is an honest
//! limit rather than a silent one, and it is why a run's `result` records the
//! source it actually read.

use std::sync::Arc;

use crate::notes::{ClaimedRun, NotesError, NotesStore};
use crate::onboarding::SetupEngine;

/// How long a claimed run is owned before another tick may take it.
pub const LEASE_SECONDS: i64 = 300;

/// How often the worker looks for a queued run.
pub const TICK: std::time::Duration = std::time::Duration::from_secs(15);

/// The bound on how much recognized text is handed to the model, so a runaway
/// OCR dump cannot turn one note into an unbounded provider bill.
pub const MAX_SOURCE_CHARS: usize = 12_000;

/// How many characters of live text make one `delta` event.
///
/// A bound on events, not on the reply: a long enrichment writes one event per
/// this many characters rather than one per provider fragment, so a note that
/// streams five hundred fragments leaves tens of rows and not hundreds.
pub const DELTA_CHARS: usize = 200;

/// What one enrichment turn is asked to do.
#[derive(Debug, Clone)]
pub struct WorkRequest {
    /// The provider, the key reference and the model.
    pub config: crate::web::chat::TurnConfig,
    /// The system message, when one is configured.
    pub system: Option<String>,
    /// The note's own content and directives.
    pub prompt: String,
}

/// What one enrichment turn produced.
#[derive(Debug, Clone)]
pub struct WorkOutput {
    /// The enrichment markdown.
    pub text: String,
}

/// How an enrichment turn is produced.
///
/// A seam rather than a direct call, for the same reason
/// [`crate::subagents::Runner`] is one: a test can drive the whole lifecycle -
/// claim, events, revision, settle - without a network. The `&mut dyn FnMut(&str)`
/// is the text as it arrives, which the worker throttles into `delta` events so a
/// phone polling the run can show the enrichment being written rather than a
/// spinner and then a wall of markdown.
pub type Runner = dyn Fn(&WorkRequest, &mut dyn FnMut(&str)) -> Result<WorkOutput, (String, String)>
    + Send
    + Sync;

/// The worker: a store to claim from, an engine to read the provider, and a way
/// to run a turn.
pub struct NotesWorker {
    store: NotesStore,
    engine: SetupEngine,
    runner: Arc<Runner>,
}

impl NotesWorker {
    /// Build a worker around an explicit runner. Tests use this.
    #[must_use]
    pub fn new(store: NotesStore, engine: SetupEngine, runner: Arc<Runner>) -> Self {
        Self {
            store,
            engine,
            runner,
        }
    }

    /// The production runner: one tool-less turn through the ordinary loop.
    #[must_use]
    pub fn production(store: NotesStore, engine: SetupEngine) -> Self {
        Self::new(store, engine, Arc::new(production_runner))
    }

    /// Claim and execute at most one run.
    ///
    /// `Ok(false)` means there was nothing queued, which is the ordinary case and
    /// not a failure. Every error it returns has already been written to the run
    /// as a `failed` event, so a caller that only logs is still leaving a trail.
    pub fn run_once(&self) -> Result<bool, NotesError> {
        let Some(claimed) = self.store.claim_next(LEASE_SECONDS)? else {
            return Ok(false);
        };
        self.execute(&claimed)?;
        Ok(true)
    }

    /// Execute one already-claimed run.
    fn execute(&self, claimed: &ClaimedRun) -> Result<(), NotesError> {
        self.store.emit_event(
            &claimed.run_id,
            claimed.attempt,
            "started",
            serde_json::json!({ "attempt": claimed.attempt }),
        )?;

        let request = match self.build_request(claimed) {
            Ok(request) => request,
            Err(error) => return self.settle_failure(claimed, &error.0, &error.1),
        };

        // The live text, throttled into `delta` events. A fragment that would
        // carry less than one event's worth is held until it is worth a row, and
        // whatever is still held when the turn ends is dropped rather than
        // written twice: the `completed` event and the run's own result carry the
        // whole text.
        let mut pending = String::new();
        let mut emitted = 0i64;
        let run_id = claimed.run_id.clone();
        let attempt = claimed.attempt;
        let mut sink = |delta: &str| {
            pending.push_str(delta);
            // A fragment can arrive in any size. Take one event's worth at a time
            // rather than whatever happens to be buffered, so one provider that
            // hands over a whole paragraph in a single chunk still produces
            // events of a bounded size.
            while pending.chars().count() >= DELTA_CHARS {
                let chunk: String = pending.chars().take(DELTA_CHARS).collect();
                pending = pending.chars().skip(DELTA_CHARS).collect();
                match self.store.emit_event(
                    &run_id,
                    attempt,
                    "delta",
                    serde_json::json!({ "text": chunk, "seq": emitted }),
                ) {
                    Ok(_) => emitted += 1,
                    // A delta that cannot be written is not a reason to fail the
                    // run: the reply is still coming and the completed event
                    // carries it.
                    Err(error) => {
                        tracing::warn!(%error, run = %run_id, "notes: a delta event did not land");
                    }
                }
            }
        };

        let outcome = match (self.runner)(&request, &mut sink) {
            Ok(outcome) => outcome,
            Err((code, message)) => return self.settle_failure(claimed, &code, &message),
        };

        // A client may have cancelled while the model was running. The result is
        // then discarded rather than stored: a cancelled run that quietly wrote
        // an enriched revision would be a run the operator cancelled and got
        // anyway.
        match self.store.run_status(&claimed.run_id)? {
            Some(status) if status == "cancelled" => {
                self.store.emit_event(
                    &claimed.run_id,
                    claimed.attempt,
                    "cancelled",
                    serde_json::json!({}),
                )?;
                return Ok(());
            }
            Some(status) if status != "running" => return Ok(()),
            _ => {}
        }

        let text = outcome.text.trim().to_owned();
        if text.is_empty() {
            return self.settle_failure(
                claimed,
                "empty_enrichment",
                "the model returned nothing to store",
            );
        }

        let note = self.store.get(&claimed.device_id, &claimed.note_id)?;
        let title = note
            .as_ref()
            .map(|note| note.title.clone())
            .unwrap_or_default();
        let revision_id = self.store.store_enrichment(
            &claimed.note_id,
            &claimed.run_id,
            claimed.source_drawing_revision,
            claimed.source_text_revision,
            &enrichment_title(&title),
            &text,
            serde_json::json!({
                "sections": [{ "kind": "summary", "title": "Enrichment", "markdown": text }],
                "source": "recognized_text",
            }),
        )?;
        self.store.emit_event(
            &claimed.run_id,
            claimed.attempt,
            "completed",
            serde_json::json!({
                "revisionId": revision_id,
                "title": enrichment_title(&title),
            }),
        )?;
        self.store.finish_run(
            &claimed.run_id,
            serde_json::json!({
                "revisionId": revision_id,
                "title": enrichment_title(&title),
                "markdown": text,
            }),
        )?;
        tracing::info!(
            run = %claimed.run_id,
            note = %claimed.note_id,
            attempt = claimed.attempt,
            "notes: enrichment completed"
        );
        Ok(())
    }

    /// Build the turn from the note's own content.
    fn build_request(&self, claimed: &ClaimedRun) -> Result<WorkRequest, (String, String)> {
        let config = crate::web::chat::turn_config(&self.engine)
            .map_err(|error| ("provider_unavailable".to_owned(), error.to_string()))?
            .ok_or_else(|| {
                (
                    "provider_not_configured".to_owned(),
                    "no provider is stored: finish onboarding and pick an endpoint and model"
                        .to_owned(),
                )
            })?;

        let note = self
            .store
            .get(&claimed.device_id, &claimed.note_id)
            .map_err(|error| ("notes_unreadable".to_owned(), error.to_string()))?;
        let title = note
            .as_ref()
            .map(|note| note.title.clone())
            .unwrap_or_default();
        let recognized = self
            .store
            .latest_recognition_text(&claimed.note_id)
            .map_err(|error| ("notes_unreadable".to_owned(), error.to_string()))?
            .unwrap_or_default();

        Ok(WorkRequest {
            config,
            system: Some(SYSTEM.to_owned()),
            prompt: build_prompt(&title, &recognized, &claimed.requested_directives),
        })
    }

    /// Record why a run failed, on the run and in its event log.
    fn settle_failure(
        &self,
        claimed: &ClaimedRun,
        code: &str,
        message: &str,
    ) -> Result<(), NotesError> {
        self.store.emit_event(
            &claimed.run_id,
            claimed.attempt,
            "failed",
            serde_json::json!({ "code": code, "message": message }),
        )?;
        self.store.fail_run(&claimed.run_id, message)?;
        tracing::warn!(
            run = %claimed.run_id,
            note = %claimed.note_id,
            code,
            "notes: enrichment failed"
        );
        Ok(())
    }
}

/// The identity of an enrichment turn. Short on purpose: the note's own content
/// carries the instruction, and a long system message would crowd it.
pub const SYSTEM: &str = "You write the enrichment for one note in a note-taking app. \
Read the note's recognized text and answer or summarise it. Return markdown only, \
with no preamble and no closing offer to help. If the note asks a question, answer \
it. If the recognized text is too garbled to use, say so in one line rather than \
inventing content.";

/// The user message: the note, bounded, with any directives the client sent.
#[must_use]
pub fn build_prompt(title: &str, recognized: &str, directives: &serde_json::Value) -> String {
    let mut out = String::new();
    out.push_str("Note title: ");
    out.push_str(if title.trim().is_empty() {
        "(untitled)"
    } else {
        title.trim()
    });
    out.push('\n');

    let recognized = recognized.trim();
    out.push_str("\nRecognized text from the note:\n");
    if recognized.is_empty() {
        out.push_str("(the note carries no recognized text yet)\n");
    } else {
        out.push_str(&truncate(recognized, MAX_SOURCE_CHARS));
        out.push('\n');
    }

    let names = directive_names(directives);
    if !names.is_empty() {
        out.push_str("\nRequested directives:\n");
        for name in names {
            out.push_str("- ");
            out.push_str(&name);
            out.push('\n');
        }
    }
    out
}

/// The directive names a client asked for, in the order it sent them.
///
/// The legacy contract is a list of strings, and the app has sent both strings
/// and `{"name": "…"}` objects, so both shapes are read rather than one being
/// declared correct.
#[must_use]
pub fn directive_names(directives: &serde_json::Value) -> Vec<String> {
    let Some(items) = directives.as_array() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| match item {
            serde_json::Value::String(text) => Some(text.trim().to_owned()),
            serde_json::Value::Object(object) => object
                .get("name")
                .and_then(|value| value.as_str())
                .map(str::trim)
                .map(str::to_owned),
            _ => None,
        })
        .filter(|name| !name.is_empty())
        .collect()
}

/// A title for the enriched revision: the note's own, marked as derived.
#[must_use]
pub fn enrichment_title(note_title: &str) -> String {
    let trimmed = note_title.trim();
    if trimmed.is_empty() {
        "Enrichment".to_owned()
    } else {
        format!("{trimmed} (enriched)")
    }
}

/// Cut a string to `limit` characters on a character boundary.
fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push_str("\n[truncated]");
    out
}

/// The production runner: one tool-less turn through `run_turn_with_key`.
///
/// No tools and one round, the same settings [`crate::subagents::child_settings`]
/// uses. An enrichment that could call tools would be a second place the operator's
/// sandbox is decided, and the worker has no approval surface to park a call on.
fn production_runner(
    request: &WorkRequest,
    on_text: &mut dyn FnMut(&str),
) -> Result<WorkOutput, (String, String)> {
    use core_runtime::provider::{ChatMessage, ChatRole};

    // Through the daemon's one credential choke point, like every other role: a
    // provider that authenticates with a stored grant is usable here for the same
    // reason it is usable in a turn, and an `env:` reference resolves exactly as
    // it did before.
    let key = crate::oauth::credential::resolve_key_now(&request.config.key_ref)
        .map_err(|error| (error.kind().to_owned(), error.to_string()))?;
    let sandbox = crate::tools::Sandbox::new(
        std::path::PathBuf::from("."),
        crate::onboarding::permissions::SandboxMode::ReadOnly,
    );
    let mut gate = |_call: &core_domain::ToolCall| crate::web::chat::GateDecision::Refuse {
        reason: "an enrichment run has no tools".to_owned(),
    };
    let mut sink = |part: crate::web::chat::StreamPart<'_>| {
        if let crate::web::chat::StreamPart::Text(delta) = part {
            on_text(delta);
        }
    };
    let mut poll = || crate::web::chat::TurnSignal::Continue;
    let messages = vec![ChatMessage::text(ChatRole::User, request.prompt.clone())];
    let outcome = crate::web::chat::run_turn_with_key(
        &request.config,
        &key,
        request.system.clone(),
        messages,
        &sandbox,
        &mut gate,
        None, // MCP servers
        None, // the web-extract role
        None, // the image role
        None, // the dispatch desk
        None, // no reply row to attach a file to
        None, // no skill library
        crate::web::chat::TurnSettings {
            tools: false,
            max_rounds: 1,
            agents: false,
            // One note, one prompt, no history: nothing here can grow past a
            // window, so there is no budget to enforce.
            context_budget: None,
        },
        // A note enrichment runs with no tools and cannot ask anybody anything:
        // it is background work with no operator watching the result.
        None,
        &mut poll,
        &mut sink,
    )?;
    Ok(WorkOutput { text: outcome.text })
}

/// Claim and execute queued runs until the daemon stops.
///
/// Runs on the daemon's runtime and does the work on a blocking task, so a slow
/// provider never holds the listener. One run per tick on purpose: enrichment is
/// a billed provider call, and a queue that drains itself at full speed is a
/// queue that can spend a day's budget in a minute.
pub async fn run(state: crate::admin::AppState) {
    let Some(store) = state.device.notes().cloned() else {
        tracing::warn!("notes: no store, so the enrichment worker is not started");
        return;
    };
    let worker = Arc::new(NotesWorker::production(store, state.engine.clone()));
    let mut interval = tokio::time::interval(TICK);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let worker = worker.clone();
        match tokio::task::spawn_blocking(move || worker.run_once()).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::error!(%error, "notes: the enrichment worker errored"),
            Err(join) => tracing::error!(%join, "notes: the enrichment task did not finish"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::NotesStore;

    /// A scratch engine, built the way every other fixture in this crate builds
    /// one: a temp root, an in-memory database, and nothing from `$HOME`.
    fn engine() -> (tempfile::TempDir, SetupEngine) {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().to_path_buf();
        let db = crate::db::Db::open(root.join("state/eris.db")).expect("db opens");
        let paths = crate::config::Paths::for_root(root.join("root"));
        let engine = SetupEngine::new(db, paths);
        // A stored provider, because the worker reads the real `turn_config`: a
        // run against an unconfigured daemon must fail with
        // `provider_not_configured`, and `a_fresh_daemon_fails_the_run_with_a_
        // reason` is the test that proves it.
        engine
            .set_provider(
                "http://127.0.0.1:20128/v1",
                "env:TEST_PROVIDER_KEY",
                &["test/model".to_owned()],
            )
            .expect("a provider");
        engine.set_model("test/model").expect("a model");
        (dir, engine)
    }

    /// A store with one note, its recognized text, and one queued run.
    fn prepared() -> (NotesStore, String, String) {
        let store = NotesStore::open_in_memory().expect("a store");
        let note = store
            .create("device-a", "Grocery list", None, false)
            .expect("a note");
        let recognition = store
            .create_recognition(
                "device-a",
                &note.id,
                note.current_drawing_revision,
                "apple-ocr",
                Some("1"),
                None,
                "milk eggs coffee 6 x 6 = ?",
                None,
            )
            .expect("a recognition");
        assert!(
            matches!(recognition, crate::notes::RecognitionOutcome::Ok(_)),
            "the note existed, so the recognition landed"
        );
        let run = store
            .create_run(
                "device-a",
                &note.id,
                "client-run-1",
                None,
                None,
                serde_json::json!(["summarise"]),
            )
            .expect("a run");
        let run_id = match run {
            crate::notes::RunCreateOutcome::Created(view) => view.id,
            other => panic!("expected a created run, got something else: {other:?}"),
        };
        (store, note.id, run_id)
    }

    fn stub(text: &'static str) -> Arc<Runner> {
        Arc::new(move |_request, on_text| {
            on_text(text);
            Ok(WorkOutput {
                text: text.to_owned(),
            })
        })
    }

    #[test]
    fn a_queued_run_is_claimed_and_completed() {
        let (store, _note_id, run_id) = prepared();
        let worker = NotesWorker::new(store.clone(), engine().1, stub("# answer\n\n36"));
        assert!(worker.run_once().expect("a run"), "one run was queued");
        let run = store
            .get_run("device-a", &run_id)
            .expect("a read")
            .expect("the run");
        assert_eq!(run.status, "completed", "{run:?}");
        assert!(run.completed_at.is_some());
        assert_eq!(run.attempt, 1);
        assert!(run.lease_expires_at.is_none(), "the lease was released");
        let result = run.result.expect("a result");
        assert_eq!(result["markdown"], "# answer\n\n36");
        assert!(result["revisionId"].is_string());
    }

    #[test]
    fn the_event_log_reads_started_then_completed_with_increasing_seq() {
        let (store, _note_id, run_id) = prepared();
        let worker = NotesWorker::new(store.clone(), engine().1, stub("# answer"));
        worker.run_once().expect("a run");
        let events = store
            .run_events("device-a", &run_id, None, 50)
            .expect("a read")
            .expect("the run belongs to the device");
        let types: Vec<&str> = events.iter().map(|event| event.type_.as_str()).collect();
        assert_eq!(types, vec!["started", "completed"], "{events:?}");
        assert!(events[0].seq < events[1].seq);
        assert_eq!(events[0].payload["attempt"], 1);
        assert!(events[1].payload["revisionId"].is_string());
    }

    #[test]
    fn the_enriched_revision_exists_and_carries_the_source() {
        let (store, note_id, run_id) = prepared();
        let worker = NotesWorker::new(store.clone(), engine().1, stub("# answer\n\n36"));
        worker.run_once().expect("a run");
        let revision = store
            .latest_enrichment(&note_id)
            .expect("a read")
            .expect("a revision");
        assert_eq!(revision.run_id, run_id);
        assert_eq!(revision.markdown, "# answer\n\n36");
        assert_eq!(revision.title, "Grocery list (enriched)");
        assert_eq!(revision.structured_sections["source"], "recognized_text");
    }

    #[test]
    fn an_empty_answer_fails_the_run_rather_than_storing_a_blank_revision() {
        let (store, _note_id, run_id) = prepared();
        let worker = NotesWorker::new(store.clone(), engine().1, stub("   \n  "));
        worker.run_once().expect("a run");
        let run = store
            .get_run("device-a", &run_id)
            .expect("a read")
            .expect("the run");
        assert_eq!(run.status, "failed");
        assert!(
            run.result.is_none(),
            "a failed run carries no result: {run:?}"
        );
        let events = store
            .run_events("device-a", &run_id, None, 50)
            .expect("a read")
            .expect("the run");
        let last = events.last().expect("an event");
        assert_eq!(last.type_, "failed");
        assert!(last.payload["message"].is_string());
    }

    #[test]
    fn a_runner_failure_is_written_to_the_run_and_the_log() {
        let (store, _note_id, run_id) = prepared();
        let runner: Arc<Runner> = Arc::new(|_request, _on_text| {
            Err(("provider_unreachable".to_owned(), "no route".to_owned()))
        });
        let worker = NotesWorker::new(store.clone(), engine().1, runner);
        worker.run_once().expect("a run");
        let run = store
            .get_run("device-a", &run_id)
            .expect("a read")
            .expect("the run");
        assert_eq!(run.status, "failed");
        assert_eq!(run.error_text.as_deref(), Some("no route"));
    }

    #[test]
    fn a_cancelled_run_is_not_overwritten_by_a_late_result() {
        let (store, _note_id, run_id) = prepared();
        // The cancel lands while the model is running: the runner cancels the
        // run, then returns, exactly as a slow provider would.
        let cancel_target = (store.clone(), run_id.clone());
        let runner: Arc<Runner> = Arc::new(move |_request, _on_text| {
            cancel_target
                .0
                .cancel_run("device-a", &cancel_target.1)
                .expect("a cancel");
            Ok(WorkOutput {
                text: "# too late".to_owned(),
            })
        });
        let worker = NotesWorker::new(store.clone(), engine().1, runner);
        worker.run_once().expect("a run");
        let run = store
            .get_run("device-a", &run_id)
            .expect("a read")
            .expect("the run");
        assert_eq!(run.status, "cancelled");
        assert!(run.result.is_none(), "the result was discarded: {run:?}");
        let events = store
            .run_events("device-a", &run_id, None, 50)
            .expect("a read")
            .expect("the run");
        assert_eq!(events.last().expect("an event").type_, "cancelled");
    }

    #[test]
    fn a_second_tick_finds_nothing_because_the_lease_was_released() {
        let (store, _note_id, _run_id) = prepared();
        let worker = NotesWorker::new(store.clone(), engine().1, stub("# answer"));
        assert!(worker.run_once().expect("a run"));
        assert!(!worker.run_once().expect("a read"), "nothing is left");
    }

    #[test]
    fn an_expired_lease_is_reclaimed() {
        let (store, _note_id, run_id) = prepared();
        // Claim it, then age the lease past it, which is what a daemon that died
        // mid-run leaves behind. A settled run is not reclaimable, so this is the
        // running-and-expired case.
        let claimed = store
            .claim_next(LEASE_SECONDS)
            .expect("a claim")
            .expect("a run");
        assert_eq!(claimed.run_id, run_id);
        store.expire_lease_for_test(&run_id).expect("an age");
        let again = store.claim_next(LEASE_SECONDS).expect("a claim");
        let again = again.expect("the expired lease is reclaimable");
        assert_eq!(again.attempt, 2, "the attempt count advanced");
    }

    #[test]
    fn a_daemon_with_no_provider_fails_the_run_with_the_reason() {
        let (store, _note_id, run_id) = prepared();
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path().to_path_buf();
        let db = crate::db::Db::open(root.join("state/eris.db")).expect("db opens");
        let bare = SetupEngine::new(db, crate::config::Paths::for_root(root.join("root")));
        let worker = NotesWorker::new(store.clone(), bare, stub("# never runs"));
        worker.run_once().expect("a run");
        let run = store
            .get_run("device-a", &run_id)
            .expect("a read")
            .expect("the run");
        assert_eq!(run.status, "failed");
        assert!(
            run.error_text
                .as_deref()
                .unwrap_or_default()
                .contains("no provider is stored"),
            "the reason is the operator's, not a stack trace: {run:?}"
        );
    }

    #[test]
    fn a_long_reply_is_streamed_as_throttled_deltas() {
        let (store, _note_id, run_id) = prepared();
        // Two and a half events' worth of text.
        let long: &'static str = Box::leak("abcdefghij".repeat(50).into_boxed_str());
        let worker = NotesWorker::new(store.clone(), engine().1, stub(long));
        worker.run_once().expect("a run");
        let events = store
            .run_events("device-a", &run_id, None, 500)
            .expect("a read")
            .expect("the run");
        let deltas: Vec<&crate::notes::NoteRunEventView> = events
            .iter()
            .filter(|event| event.type_ == "delta")
            .collect();
        assert_eq!(deltas.len(), 2, "500 chars at {DELTA_CHARS} per event");
        for (index, event) in deltas.iter().enumerate() {
            assert_eq!(event.payload["seq"], index as i64);
            let chunk = event.payload["text"].as_str().expect("a text chunk");
            assert_eq!(chunk.chars().count(), DELTA_CHARS);
        }
        // The deltas are written before the terminal event, and the sequence
        // numbers are strictly increasing across the whole log.
        assert_eq!(events.last().expect("an event").type_, "completed");
        let seqs: Vec<i64> = events.iter().map(|event| event.seq).collect();
        let mut sorted = seqs.clone();
        sorted.sort_unstable();
        assert_eq!(seqs, sorted, "the log reads in order");
        assert_eq!(
            seqs.iter().collect::<std::collections::HashSet<_>>().len(),
            seqs.len(),
            "no two events share a sequence number"
        );
    }

    #[test]
    fn a_short_reply_writes_no_delta_events() {
        let (store, _note_id, run_id) = prepared();
        let worker = NotesWorker::new(store.clone(), engine().1, stub("short"));
        worker.run_once().expect("a run");
        let events = store
            .run_events("device-a", &run_id, None, 500)
            .expect("a read")
            .expect("the run");
        assert!(
            events.iter().all(|event| event.type_ != "delta"),
            "below the threshold nothing is streamed: {events:?}"
        );
    }

    #[test]
    fn the_prompt_carries_the_title_the_text_and_the_directives() {
        let prompt = build_prompt(
            "Grocery list",
            "milk eggs",
            &serde_json::json!(["summarise", { "name": "answer_questions" }]),
        );
        assert!(prompt.contains("Note title: Grocery list"));
        assert!(prompt.contains("milk eggs"));
        assert!(prompt.contains("- summarise"));
        assert!(prompt.contains("- answer_questions"));
    }

    #[test]
    fn an_unrecognised_note_says_so_rather_than_sending_an_empty_prompt() {
        let prompt = build_prompt("", "   ", &serde_json::json!([]));
        assert!(prompt.contains("Note title: (untitled)"));
        assert!(prompt.contains("no recognized text yet"));
    }

    #[test]
    fn a_long_ocr_dump_is_truncated() {
        let dumped = "x".repeat(MAX_SOURCE_CHARS + 500);
        let prompt = build_prompt("t", &dumped, &serde_json::json!([]));
        assert!(prompt.contains("[truncated]"));
        assert!(prompt.len() < MAX_SOURCE_CHARS + 400);
    }

    #[test]
    fn an_enrichment_title_marks_the_derivation() {
        assert_eq!(enrichment_title("  Notes  "), "Notes (enriched)");
        assert_eq!(enrichment_title(""), "Enrichment");
    }
}
