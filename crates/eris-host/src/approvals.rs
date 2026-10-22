//! The approvals queue.
//!
//! A row is a write the model asked for and no one has answered yet. The queue
//! exists so `approval_policy = ask` is a boundary rather than a sentence: the
//! turn parks the call, tells the model it is waiting, and the filesystem is not
//! touched until a decision lands.
//!
//! A decision is one-way by construction. The update that settles a row requires
//! it to still be `pending`, so two operators racing cannot both approve and deny
//! the same call, and a second attempt reports that it changed nothing instead of
//! silently succeeding.

use crate::db::{Db, DbError};

/// What happened when an approved call was actually run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ran {
    /// The call was executed and its result written back into the transcript.
    Done {
        /// What the tool said, as the transcript keeps it.
        content: String,
        /// How many transcript rows were rewritten. Zero means the turn never
        /// stored the call, which is reported rather than hidden.
        rewritten: usize,
    },
    /// The call was not run, so the row stays pending and a person can look again.
    Refused(String),
    /// Somebody settled the row between the read and the write.
    AlreadySettled,
}

/// Approve a parked call and actually run it.
///
/// The queue is only a boundary if a decision has a consequence: an approval that
/// settles the row and runs nothing is a person saying yes to a form. Both the CLI
/// and a paired device call this, so there is one execution path and not two.
///
/// `actor` is who decided, recorded on the row's audit.
///
/// # Errors
///
/// A database failure reading the row, rewriting the transcript or settling it.
pub fn approve_and_run(
    db: &Db,
    paths: &crate::config::Paths,
    id: &str,
    actor: &str,
) -> Result<Ran, DbError> {
    let Some(row) = get(db, id)? else {
        return Ok(Ran::AlreadySettled);
    };
    let Ok(arguments) = serde_json::from_str::<serde_json::Value>(&row.arguments) else {
        return Ok(Ran::Refused(
            "the parked call has arguments that cannot be read back".to_owned(),
        ));
    };

    let engine = crate::onboarding::SetupEngine::new(db.clone(), paths.clone());
    let workspace = engine
        .workspace_path()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let mode = engine
        .permissions()
        .map(|permissions| permissions.sandbox_mode)
        .unwrap_or(crate::onboarding::permissions::SandboxMode::ReadOnly);
    let sandbox = crate::tools::Sandbox::new(workspace, mode);

    // A namespaced name is an MCP tool: approving it has to reach the server, not
    // the local file executor. `display` is what the transcript keeps - a server's
    // own words, not this run's JSON envelope, or a run log entry reads as soup.
    let outcome: (crate::tools::ToolResult, String) =
        if crate::mcp::protocol::split_qualified(&row.tool).is_some() {
            let servers = engine
                .setting(crate::mcp::SETTING_SERVERS)
                .ok()
                .flatten()
                .and_then(|raw| crate::mcp::parse(&raw).ok())
                .unwrap_or_default();
            let pool = crate::mcp::Pool::new(servers);
            match pool.call(&row.tool, &arguments) {
                Ok(outcome) => (
                    crate::tools::ToolResult::from_mcp(&row.tool, &outcome),
                    outcome.text,
                ),
                Err(error) => (
                    crate::tools::ToolResult::refused(&error.message),
                    error.message.clone(),
                ),
            }
        } else {
            let result = crate::tools::execute(&row.tool, &arguments, &sandbox);
            let text = result.content.clone();
            (result, text)
        };
    let (result, display) = outcome;

    if result.is_error {
        // A refusal leaves the row pending: nothing was written, so nobody has
        // actually said yes to anything yet.
        return Ok(Ran::Refused(result.content));
    }

    let rewritten =
        crate::web::chat::apply_approval_result(db, &row.id, &display, false).unwrap_or(0);
    if !decide(db, id, Decision::Approved, actor)? {
        return Ok(Ran::AlreadySettled);
    }
    Ok(Ran::Done {
        content: result.content,
        rewritten,
    })
}

/// What an operator decided about a parked call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Run it when the turn resumes.
    Approved,
    /// Never run it, and tell the model so.
    Denied,
}

impl Decision {
    /// The wire and database spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Denied => "denied",
        }
    }
}

/// One call waiting on a human.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    /// Stable id, quoted in the prompt and in the dashboard.
    pub id: String,
    /// The conversation it came from.
    pub conversation: String,
    /// The tool name.
    pub tool: String,
    /// The arguments, as JSON.
    pub arguments: String,
    /// When it was parked.
    pub created_at: String,
    /// What the approval role thought, when one is pinned. Advice only.
    pub advice: Option<String>,
}

/// Park a call and return its id.
pub fn queue(
    db: &Db,
    conversation: &str,
    tool: &str,
    arguments: &serde_json::Value,
    advice: Option<&str>,
) -> Result<String, DbError> {
    let id = uuid::Uuid::now_v7().to_string();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO approvals (id, conversation, tool, arguments, advice)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, conversation, tool, arguments.to_string(), advice],
        )
    })?;
    Ok(id)
}

/// Everything still waiting, oldest first.
pub fn pending(db: &Db) -> Result<Vec<PendingApproval>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, conversation, tool, arguments, created_at, advice
               FROM approvals
              WHERE status = 'pending'
              ORDER BY created_at, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(PendingApproval {
                id: row.get(0)?,
                conversation: row.get(1)?,
                tool: row.get(2)?,
                arguments: row.get(3)?,
                created_at: row.get(4)?,
                advice: row.get(5)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })
}

/// One row by id, whatever its status.
pub fn get(db: &Db, id: &str) -> Result<Option<PendingApproval>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, conversation, tool, arguments, created_at, advice
               FROM approvals
              WHERE id = ?1",
        )?;
        let mut rows = statement.query_map([id], |row| {
            Ok(PendingApproval {
                id: row.get(0)?,
                conversation: row.get(1)?,
                tool: row.get(2)?,
                arguments: row.get(3)?,
                created_at: row.get(4)?,
                advice: row.get(5)?,
            })
        })?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    })
}

/// Settle one parked call.
///
/// Returns `false` when the row was not pending any more - already decided, or
/// never there - and changed nothing. A caller that treats `false` as success
/// would be reporting an approval that did not happen.
pub fn decide(db: &Db, id: &str, decision: Decision, actor: &str) -> Result<bool, DbError> {
    let changed = db.with_conn(|conn| {
        conn.execute(
            "UPDATE approvals
                SET status = ?1,
                    decided_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now'),
                    decided_by = ?2
              WHERE id = ?3 AND status = 'pending'",
            rusqlite::params![decision.as_str(), actor, id],
        )
    })?;
    Ok(changed == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("in-memory database")
    }

    #[test]
    fn a_queued_call_is_pending_with_its_tool_and_arguments_intact() {
        let db = db();
        let arguments = serde_json::json!({"path": "notes/a.txt", "content": "hi"});

        let id = queue(&db, "conv-1", "write_file", &arguments, None).expect("queued");
        let waiting = pending(&db).expect("pending");

        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].id, id);
        assert_eq!(waiting[0].conversation, "conv-1");
        assert_eq!(waiting[0].tool, "write_file");
        let parsed: serde_json::Value =
            serde_json::from_str(&waiting[0].arguments).expect("arguments are JSON");
        assert_eq!(parsed, arguments);
    }

    #[test]
    fn deciding_settles_the_row_and_a_second_decision_changes_nothing() {
        let db = db();
        let id = queue(&db, "conv-1", "write_file", &serde_json::json!({}), None).expect("queued");

        assert!(decide(&db, &id, Decision::Approved, "operator").expect("decided"));
        assert!(
            !decide(&db, &id, Decision::Denied, "operator").expect("a second attempt"),
            "a settled row must not be decided twice"
        );
        assert!(
            pending(&db).expect("pending").is_empty(),
            "a decided call leaves the queue"
        );
    }

    #[test]
    fn advice_is_kept_with_the_row_and_never_decides_it() {
        let db = db();
        let id = queue(
            &db,
            "conv-1",
            "write_file",
            &serde_json::json!({"path": "a.txt"}),
            Some("CAREFUL - overwrites a file that already exists"),
        )
        .expect("queued");

        let waiting = pending(&db).expect("pending");
        assert_eq!(
            waiting[0].advice.as_deref(),
            Some("CAREFUL - overwrites a file that already exists")
        );
        assert_eq!(
            pending(&db).expect("pending").len(),
            1,
            "advice alone must not settle the row"
        );
        assert!(decide(&db, &id, Decision::Approved, "operator").expect("decided"));
    }

    #[test]
    fn a_decision_records_who_made_it() {
        let db = db();
        let id = queue(&db, "conv-1", "write_file", &serde_json::json!({}), None).expect("queued");
        decide(&db, &id, Decision::Denied, "operator").expect("decided");

        let (status, decided_by): (String, String) = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT status, decided_by FROM approvals WHERE id = ?1",
                    [&id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
            })
            .expect("row");

        assert_eq!(status, "denied");
        assert_eq!(decided_by, "operator");
    }
}
