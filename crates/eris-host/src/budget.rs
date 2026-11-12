//! The budget a turn can read before it spends.
//!
//! A turn that discovers the wall by hitting it has already paid for every step up
//! to the wall. The numbers already exist: `metrics` aggregates them per window and
//! per conversation, and the settings panel renders them. What was missing is a
//! way for the turn itself to ask, so a long job can plan two turns instead of
//! discovering at the end that it needed three.
//!
//! Three rules, each of which is a way this could be useless:
//!
//! - **The cap is a setting, not a constant.** `quota.soft_cap_tokens` is stored and
//!   validated like every other operator switch; a hardcoded ceiling would be right
//!   for one operator and wrong for the next.
//! - **A number that was not measured is absent, not zero.** A window with no
//!   duration rows reports no p95, because `0 ms` is a measurement and "nothing ran"
//!   is not.
//! - **A conversation filter narrows, never re-derives.** The same `summary_in` the
//!   settings panel calls answers here, so the tool cannot disagree with the page.

use crate::db::{Db, DbError};
use crate::metrics::{self, Window};

/// The setting that holds the soft cap.
pub const SETTING_SOFT_CAP: &str = "quota.soft_cap_tokens";

/// The default soft cap, in tokens.
pub const DEFAULT_SOFT_CAP: i64 = 200_000;

/// The smallest cap an operator may store.
pub const MIN_SOFT_CAP: i64 = 1_000;

/// The largest cap an operator may store.
pub const MAX_SOFT_CAP: i64 = 10_000_000;

/// What one turn is told about the budget.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// Assistant turns in the window.
    pub turns: i64,
    /// Prompt tokens billed in the window.
    pub prompt_tokens: i64,
    /// Completion tokens billed in the window.
    pub completion_tokens: i64,
    /// Both together.
    pub total_tokens: i64,
    /// The stored soft cap.
    pub soft_cap_tokens: i64,
    /// What is left of it, floored at zero.
    pub remaining_tokens: i64,
    /// True when the window has already spent the cap.
    pub cap_reached: bool,
    /// The 95th percentile turn duration, when any row recorded one.
    pub p95_ms: Option<i64>,
    /// The window the numbers cover, as a person reads it.
    pub window: &'static str,
    /// The conversation the numbers cover, when one was named.
    pub conversation: Option<String>,
}

impl Report {
    /// The one sentence a turn reads first.
    #[must_use]
    pub fn verdict(&self) -> &'static str {
        if self.cap_reached {
            "the soft cap is reached: stop and report instead of continuing a long job"
        } else if self.remaining_tokens * 4 < self.soft_cap_tokens {
            "less than a quarter of the soft cap is left: plan to finish soon"
        } else {
            "there is room under the soft cap"
        }
    }
}

/// Read the stored soft cap, falling back to the default.
///
/// A stored value outside the range is not repaired silently: it is reported as
/// the default here and refused at the form, so a hand-edited database reads as
/// "unset" rather than as a cap nobody chose.
#[must_use]
pub fn soft_cap(setting: Option<&str>) -> i64 {
    setting
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|value| (MIN_SOFT_CAP..=MAX_SOFT_CAP).contains(value))
        .unwrap_or(DEFAULT_SOFT_CAP)
}

/// Check a soft cap the operator typed.
///
/// # Errors
///
/// The value is outside the stored range, or is not a number.
pub fn validate_soft_cap(value: &str) -> Result<i64, String> {
    let parsed = value.trim().parse::<i64>().map_err(|_| {
        format!("the soft cap is a whole number of tokens, and '{value}' is not one")
    })?;
    if !(MIN_SOFT_CAP..=MAX_SOFT_CAP).contains(&parsed) {
        return Err(format!(
            "the soft cap is between {MIN_SOFT_CAP} and {MAX_SOFT_CAP} tokens and this one is \
             {parsed}"
        ));
    }
    Ok(parsed)
}

/// The report for one window, for one conversation or for all of them.
///
/// # Errors
///
/// The database could not answer.
pub fn report(
    db: &Db,
    conversation: Option<&str>,
    cap: i64,
    window: Window,
) -> Result<Report, DbError> {
    let summary = metrics::summary_in(db, window, conversation)?;
    let total_tokens = summary.prompt_tokens + summary.completion_tokens;
    // The window's own p95, which is the slowest of the per-model rows rather than
    // a second calculation: the percentage a person reads is the same one the
    // settings panel shows.
    let p95_ms = summary
        .rows
        .iter()
        .filter_map(|row| row.p95_ms)
        .max()
        .filter(|value| *value > 0);
    Ok(Report {
        turns: summary.calls,
        prompt_tokens: summary.prompt_tokens,
        completion_tokens: summary.completion_tokens,
        total_tokens,
        soft_cap_tokens: cap,
        remaining_tokens: (cap - total_tokens).max(0),
        cap_reached: total_tokens >= cap,
        p95_ms,
        window: window.label(),
        conversation: conversation.map(str::to_owned),
    })
}

/// The tool's JSON body, which is what a turn actually reads.
#[must_use]
pub fn to_json(report: &Report) -> serde_json::Value {
    serde_json::json!({
        "turns": report.turns,
        "prompt_tokens": report.prompt_tokens,
        "completion_tokens": report.completion_tokens,
        "total_tokens": report.total_tokens,
        "soft_cap_tokens": report.soft_cap_tokens,
        "remaining_tokens": report.remaining_tokens,
        "cap_reached": report.cap_reached,
        "p95_ms": report.p95_ms,
        "window": report.window,
        "conversation": report.conversation,
        "verdict": report.verdict(),
    })
}

/// The tool a turn calls to read its own budget.
///
/// It needs the database, which the sandbox does not hold, so the turn intercepts
/// it before dispatch and `tools::execute` answers a named refusal - the same shape
/// `queue_task` and `dispatch_agent` use. The description says the two things a
/// model cannot infer: that the numbers are for the window rather than this turn,
/// and what to do when the cap is reached.
pub const TOOL: &str = "budget";

/// The tool spec.
#[must_use]
pub fn spec() -> core_runtime::provider::ToolSpec {
    core_runtime::provider::ToolSpec {
        name: TOOL.to_owned(),
        description: "Read the token budget for this conversation or for the whole daemon: what \
                      the window has spent, what the soft cap is, how much is left, and the p95 \
                      turn duration. Call it before starting a long job. When `cap_reached` is \
                      true, stop and report instead of continuing: a spent cap answers with an \
                      error that costs a turn to discover."
            .to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "conversation": {
                    "type": "string",
                    "description": "The chat to report on. Omit for every conversation in the \
                                    window."
                },
                "window": {
                    "type": "string",
                    "enum": ["24h", "7d", "all"],
                    "description": "How far back to count. Defaults to `all`, because a chat \
                                    that started days ago still has a budget."
                }
            },
            "required": []
        }),
    }
}

/// Answer one `budget` call.
///
/// A refusal never returns an empty object with a 200: the numbers are in the body
/// or the reason is, and a caller that cannot tell those apart cannot act.
pub fn execute(
    db: &Db,
    setting: Option<&str>,
    arguments: &serde_json::Value,
) -> crate::tools::ToolResult {
    let conversation = arguments
        .get("conversation")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let window = arguments
        .get("window")
        .and_then(serde_json::Value::as_str)
        .and_then(Window::parse)
        .unwrap_or(Window::All);
    let cap = soft_cap(setting);
    match report(db, conversation, cap, window) {
        Ok(report) => crate::tools::ToolResult::ok(to_json(&report)),
        Err(error) => crate::tools::ToolResult::error(format!(
            "the budget could not be read ({error}), so this call reported nothing rather than \
             reporting zero"
        )),
    }
}

/// The refusal `tools::execute` answers when a caller reaches the sandbox layer.
#[must_use]
pub fn sandbox_refusal() -> crate::tools::ToolResult {
    crate::tools::ToolResult::refused(
        "the budget needs the database, so this call did nothing; it runs inside a conversation \
         turn and not in the sandbox layer",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    fn db() -> Db {
        Db::open_in_memory().expect("in-memory db")
    }

    /// One assistant row with the token counts a turn would bill.
    fn seed(db: &Db, conversation: &str, prompt: i64, completion: i64, duration: Option<i64>) {
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO chat_messages (id, conversation_id, role, body, state, model, \
                 prompt_tokens, completion_tokens, duration_ms, created_at) \
                 VALUES (?1, ?2, 'assistant', 'an answer', 'complete', 'a-model', ?3, ?4, ?5, \
                 strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
                rusqlite::params![
                    uuid::Uuid::now_v7().to_string(),
                    conversation,
                    prompt,
                    completion,
                    duration
                ],
            )?;
            Ok(())
        })
        .expect("a row");
    }

    #[test]
    fn a_report_sums_the_window_and_names_what_is_left() {
        let db = db();
        seed(&db, "conv-a", 1_000, 500, Some(1_200));
        seed(&db, "conv-a", 2_000, 250, Some(3_000));
        let report = report(&db, None, 10_000, Window::All).expect("a report");
        assert_eq!(report.turns, 2);
        assert_eq!(report.prompt_tokens, 3_000);
        assert_eq!(report.completion_tokens, 750);
        assert_eq!(report.total_tokens, 3_750);
        assert_eq!(report.remaining_tokens, 6_250);
        assert!(!report.cap_reached);
        assert_eq!(report.p95_ms, Some(3_000), "the slowest model p95");
        assert_eq!(report.verdict(), "there is room under the soft cap");
    }

    #[test]
    fn a_conversation_filter_narrows_to_that_chat() {
        let db = db();
        seed(&db, "conv-a", 1_000, 500, None);
        seed(&db, "conv-b", 9_000, 900, None);
        let narrow = report(&db, Some("conv-a"), 10_000, Window::All).expect("a report");
        assert_eq!(narrow.turns, 1);
        assert_eq!(narrow.total_tokens, 1_500);
        assert_eq!(narrow.conversation.as_deref(), Some("conv-a"));
        let wide = report(&db, None, 10_000, Window::All).expect("a report");
        assert_eq!(wide.turns, 2);
    }

    #[test]
    fn a_spent_cap_is_reached_and_the_verdict_says_stop() {
        let db = db();
        seed(&db, "conv-a", 9_000, 2_000, None);
        let report = report(&db, None, 10_000, Window::All).expect("a report");
        assert!(report.cap_reached);
        assert_eq!(report.remaining_tokens, 0, "never a negative remainder");
        assert!(
            report.verdict().contains("stop and report"),
            "{}",
            report.verdict()
        );
    }

    #[test]
    fn a_nearly_spent_cap_warns_before_it_is_reached() {
        let db = db();
        seed(&db, "conv-a", 8_000, 0, None);
        let report = report(&db, None, 10_000, Window::All).expect("a report");
        assert!(!report.cap_reached);
        assert!(report.verdict().contains("quarter"), "{}", report.verdict());
    }

    #[test]
    fn an_unmeasured_window_reports_no_percentile_rather_than_zero() {
        let db = db();
        seed(&db, "conv-a", 10, 10, None);
        let report = report(&db, None, 10_000, Window::All).expect("a report");
        assert_eq!(report.p95_ms, None, "no duration rows is not 0 ms");
    }

    #[test]
    fn the_cap_is_a_validated_setting() {
        assert_eq!(soft_cap(None), DEFAULT_SOFT_CAP);
        assert_eq!(soft_cap(Some("5000")), 5_000);
        // Out of range reads as unset rather than as a cap nobody chose.
        assert_eq!(soft_cap(Some("1")), DEFAULT_SOFT_CAP);
        assert_eq!(soft_cap(Some("nonsense")), DEFAULT_SOFT_CAP);
        assert_eq!(validate_soft_cap("5000"), Ok(5_000));
        assert!(validate_soft_cap("1").is_err());
        assert!(validate_soft_cap("99999999").is_err());
        assert!(validate_soft_cap("abc").is_err());
    }

    #[test]
    fn the_json_body_carries_every_number_a_turn_needs() {
        let db = db();
        seed(&db, "conv-a", 1_000, 500, Some(900));
        let report = report(&db, None, 10_000, Window::All).expect("a report");
        let body = to_json(&report);
        for key in [
            "turns",
            "prompt_tokens",
            "completion_tokens",
            "total_tokens",
            "soft_cap_tokens",
            "remaining_tokens",
            "cap_reached",
            "p95_ms",
            "window",
            "verdict",
        ] {
            assert!(body.get(key).is_some(), "{key} is missing from {body}");
        }
        assert_eq!(body["total_tokens"], 1_500);
    }
}
