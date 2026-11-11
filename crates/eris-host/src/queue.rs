//! Work one turn hands to the next turn in the same conversation.
//!
//! The engine gives a turn a finite tool budget, and a real job on this host
//! routinely needs more than one turn's worth. Before this module the only way to
//! continue was for the operator to type "continue": the wrap-up round had no
//! tools, so the model could report the remainder but not queue it. That is the
//! nudge loop this module exists to end.
//!
//! Two things are queued, and they are the same row:
//!
//! - A task the model asked for with `queue_task`. The model writes the prompt
//!   it would have asked next, and the turn that is running keeps working.
//! - A continuation the daemon adds when a turn ends capped and queued nothing.
//!   Its prompt carries the last answer so the next turn starts from where this
//!   one stopped instead of re-deriving it.
//!
//! A queued task is a `jobs` row of kind `task` with status `queued`, so it is
//! durable, visible beside every other turn, and stoppable by the same cancel
//! that stops a live one. It is drained by the worker that just finished, after
//! its own transcript row is settled, so two turns never write one conversation at
//! the same time.
//!
//! The chain is bounded on purpose. `chain_depth` counts how many turns downstream
//! of the operator's message this one is: an operator turn is zero, a queued task
//! is one more, and an automatic continuation stops at [`MAX_AUTO`]. A manual
//! queue stops at [`MAX_CHAIN`]. A job that keeps capping cannot spend a night of
//! tokens on its own.

use rusqlite::{OptionalExtension, params};

use crate::db::{Db, DbError};
use crate::tools::ToolResult;
use core_runtime::provider::ToolSpec;

/// The tool the model calls to hand work forward.
pub const TOOL: &str = "queue_task";

/// How many turns downstream of the operator a queued task may run.
///
/// Six is the size of a real multi-slice job and far short of a runaway: each
/// step is one turn, and every one of them is a row in `jobs` the operator can
/// see and stop.
pub const MAX_CHAIN: i64 = 6;

/// How many automatic continuations a capped turn may start.
///
/// Two: the turn that capped, plus one that finishes the work. A third would be
/// the daemon deciding to keep going long after the operator stopped reading.
pub const MAX_AUTO: i64 = 2;

/// The longest prompt a queued task may carry.
pub const MAX_PROMPT_CHARS: usize = 8_000;

/// How much of the previous answer a continuation prompt quotes.
pub const CONTINUATION_TAIL_CHARS: usize = 1_500;

/// Where queued work is written, and how deep this turn already is.
#[derive(Debug, Clone)]
pub struct Sink {
    /// The database the jobs live in.
    pub db: Db,
    /// The conversation the queued turn must write into.
    pub conversation: String,
    /// The device that asked for the turn this queue belongs to.
    pub device_id: String,
    /// How many turns downstream of the operator this turn is.
    pub depth: i64,
}

/// One queued task, read back for the worker that will run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Queued {
    /// The `jobs` row id, which is also the turn id the drain spawns.
    pub id: String,
    /// What that turn is for.
    pub prompt: String,
    /// How deep it is in the chain.
    pub depth: i64,
}

/// The tool spec for `queue_task`.
///
/// The description has to say three things the model cannot infer: that the task
/// runs after this turn rather than now, that the current turn should keep
/// working, and that a queued task is not a substitute for doing the work that
/// fits.
#[must_use]
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL.to_owned(),
        description: format!(
            "Queue one task to run as its own turn in this conversation, immediately after \
             this turn ends. Use it for work that will NOT fit in this turn's tool budget: \
             the task runs with a fresh budget and the same conversation, so it starts from \
             this transcript. Do not queue work you are about to do yourself, and do not \
             queue the same prompt twice. The task is durable (a jobs row), visible in the \
             daemon, and stoppable. At most {MAX_CHAIN} turns deep in one chain."
        ),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The whole task, written as if to yourself in the next \
                                    turn: what is left, what is already done, and how to \
                                    verify it. At most 8000 characters."
                },
                "label": {
                    "type": "string",
                    "description": "Optional few-word name, for the row."
                }
            },
            "required": ["prompt"]
        }),
    }
}

/// Run one `queue_task` call.
///
/// Refusals are tool results rather than errors: the model reads a sentence it can
/// act on and keeps going, which is the same contract every other tool follows.
#[must_use]
pub fn execute(sink: &Sink, arguments: &serde_json::Value) -> ToolResult {
    let prompt = arguments
        .get("prompt")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim();
    if prompt.is_empty() {
        return ToolResult::refused("a queued task needs a prompt: what the next turn is for");
    }
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        return ToolResult::refused(&format!(
            "that prompt is longer than {MAX_PROMPT_CHARS} characters; split it into two tasks \
             or make it shorter"
        ));
    }
    let depth = sink.depth + 1;
    if depth > MAX_CHAIN {
        return ToolResult::refused(&format!(
            "this chain is already {MAX_CHAIN} turns deep, which is the limit; finish what is \
             left in this turn or report what still needs a human"
        ));
    }
    // A queued task that already exists is not queued twice: a model retrying a
    // call would otherwise run the same job on a new budget.
    match already_queued(&sink.db, &sink.conversation, prompt) {
        Ok(true) => {
            return ToolResult::ok(serde_json::json!({
                "status": "already_queued",
                "note": "An identical task is already waiting on this conversation. It was \
                         not added twice.",
            }));
        }
        Ok(false) => {}
        Err(error) => {
            return ToolResult::refused(&format!("could not read the queue: {error}"));
        }
    }
    match enqueue(&sink.db, &sink.conversation, &sink.device_id, prompt, depth) {
        Ok(id) => ToolResult::ok(serde_json::json!({
            "status": "queued",
            "task_id": id,
            "depth": depth,
            "note": "It runs as its own turn in this conversation after this one ends. Keep \
                     working on this turn; do not do the queued task now.",
        })),
        Err(error) => ToolResult::refused(&format!("could not queue the task: {error}")),
    }
}

/// Write one queued task and return its job id.
pub fn enqueue(
    db: &Db,
    conversation: &str,
    device_id: &str,
    prompt: &str,
    depth: i64,
) -> Result<String, DbError> {
    let id = uuid::Uuid::now_v7().to_string();
    let device = (!device_id.is_empty()).then_some(device_id);
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO jobs
                 (id, kind, status, device_id, conversation_id, chain_depth, request_text)
             VALUES (?1, 'task', 'queued', ?2, ?3, ?4, ?5)",
            params![id, device, conversation, depth, prompt],
        )?;
        Ok(())
    })?;
    Ok(id)
}

/// The oldest queued task for a conversation, if any.
pub fn next(db: &Db, conversation: &str) -> Result<Option<Queued>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT id, request_text, chain_depth
               FROM jobs
              WHERE kind = 'task' AND status = 'queued' AND conversation_id = ?1
              ORDER BY created_at, id
              LIMIT 1",
            [conversation],
            |row| {
                Ok(Queued {
                    id: row.get(0)?,
                    prompt: row.get(1)?,
                    depth: row.get::<_, i64>(2)?,
                })
            },
        )
        .optional()
    })
}

/// How many tasks are waiting on one conversation.
pub fn pending(db: &Db, conversation: &str) -> Result<i64, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM jobs
              WHERE kind = 'task' AND status = 'queued' AND conversation_id = ?1",
            [conversation],
            |row| row.get(0),
        )
    })
}

/// Whether an identical prompt is already waiting.
fn already_queued(db: &Db, conversation: &str, prompt: &str) -> Result<bool, DbError> {
    db.with_conn(|conn| {
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM jobs
                  WHERE kind = 'task' AND status = 'queued' AND conversation_id = ?1
                    AND request_text = ?2
                  LIMIT 1",
                params![conversation, prompt],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    })
}

/// The prompt an automatic continuation carries.
///
/// It names the reason the last turn stopped, quotes what that turn reported, and
/// says plainly not to redo finished work. A continuation that re-explores costs
/// the same budget the cap just spent.
#[must_use]
pub fn continuation_prompt(previous: &str) -> String {
    let tail = tail_chars(previous, CONTINUATION_TAIL_CHARS);
    let mut out = String::from(
        "Continue this job. The previous turn ended at its tool-round cap, not because the \
         work was done.\n\nLast turn reported:\n",
    );
    out.push_str(&tail);
    out.push_str(
        "\n\nStart from that point: do not redo finished work, do not re-run commands whose \
         result is already quoted, and do not ask whether to continue. Finish the remainder, \
         verify it, and report what is proven. If the remainder will not fit in this turn's \
         budget either, queue the rest with queue_task before you answer.",
    );
    out
}

/// The last `limit` characters of `text`, cut on a character boundary.
fn tail_chars(text: &str, limit: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= limit {
        return trimmed.to_owned();
    }
    let skip = trimmed.chars().count() - limit;
    trimmed.chars().skip(skip).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Db, String) {
        let db = Db::open_in_memory().expect("in-memory database");
        let conversation = crate::web::chat::create_conversation(&db).expect("conversation");
        (db, conversation)
    }

    #[test]
    fn a_queued_task_round_trips_with_its_depth() {
        let (db, conversation) = fixture();
        let id = enqueue(&db, &conversation, "", "finish the parser", 1).expect("queued");
        let queued = next(&db, &conversation).expect("read").expect("one row");
        assert_eq!(queued.id, id);
        assert_eq!(queued.prompt, "finish the parser");
        assert_eq!(queued.depth, 1);
        assert_eq!(pending(&db, &conversation).expect("count"), 1);
    }

    #[test]
    fn a_finished_task_leaves_the_queue() {
        let (db, conversation) = fixture();
        let id = enqueue(&db, &conversation, "", "one", 1).expect("queued");
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE jobs SET status = 'running' WHERE id = ?1",
                params![id],
            )?;
            Ok(())
        })
        .expect("claim");
        assert!(next(&db, &conversation).expect("read").is_none());
        assert_eq!(pending(&db, &conversation).expect("count"), 0);
    }

    #[test]
    fn a_chain_stops_at_the_limit() {
        let (db, conversation) = fixture();
        let sink = Sink {
            db,
            conversation,
            device_id: String::new(),
            depth: MAX_CHAIN,
        };
        let result = execute(&sink, &serde_json::json!({ "prompt": "keep going" }));
        assert!(result.is_error, "the refusal is a tool result, not a write");
        assert!(result.content.contains("limit"));
    }

    #[test]
    fn an_identical_prompt_is_not_queued_twice() {
        let (db, conversation) = fixture();
        let sink = Sink {
            db,
            conversation,
            device_id: String::new(),
            depth: 0,
        };
        let first = execute(&sink, &serde_json::json!({ "prompt": "same task" }));
        let second = execute(&sink, &serde_json::json!({ "prompt": "same task" }));
        assert!(!first.is_error);
        assert!(!second.is_error);
        assert!(second.content.contains("already_queued"));
        assert_eq!(pending(&sink.db, &sink.conversation).expect("count"), 1);
    }

    #[test]
    fn a_continuation_quotes_the_tail_and_forbids_a_restart() {
        let long = "x".repeat(CONTINUATION_TAIL_CHARS + 400);
        let prompt = continuation_prompt(&long);
        assert!(prompt.contains("not because the work was done"));
        assert!(prompt.contains("do not redo finished work"));
        assert!(prompt.contains("queue_task"));
        let quoted = prompt
            .split("Last turn reported:\n")
            .nth(1)
            .expect("the tail is quoted");
        assert!(quoted.chars().count() >= CONTINUATION_TAIL_CHARS);
    }
}
