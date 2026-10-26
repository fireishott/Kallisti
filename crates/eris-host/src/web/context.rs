//! Deciding what context a turn carries, and compressing what it cannot.
//!
//! A conversation outgrows a window. Before this module the only rule was "keep
//! the newest [`HISTORY_MESSAGES`](super::chat::HISTORY_MESSAGES) messages and
//! drop the rest", which is a message count and not a size: twenty-four short
//! exchanges leave a million-token window almost empty, and twenty-four long ones
//! with command output in them overflow a 128K window and the provider answers
//! with a context-length failure.
//!
//! Three things here, and one thing deliberately not here:
//!
//! - [`plan`] sizes the window against the budget the model actually declared.
//!   A window nobody could resolve leaves the old fixed window standing, because
//!   guessing a budget is how a turn gets cut for no reason.
//! - [`compress`] turns the rows that fell outside it into a summary, iteratively:
//!   a previous summary is updated with the new turns rather than replaced by a
//!   fresh reading of the whole tail, which is one aux call either way but keeps
//!   what earlier turns decided instead of re-deriving it from a truncation.
//! - [`estimate`] is a rough token count, used only to decide. Every number that
//!   is reported to the operator comes from a provider's own usage, never from
//!   this.
//!
//! - the cooldown in [`cooling_down`] and [`record_failure`] is what keeps a
//!   dead summary model from being paid for on every single turn. Without it a
//!   provider outage turns every turn into two failed calls and a marker row,
//!   forever, and the operator cannot tell that from the daemon being slow.
//!
//! What is not here is in-turn trimming. A turn that calls twelve tools and grows
//! its own prompt past the window is handled at the round boundary inside
//! [`super::chat::drive_tool_loop`], because that is the only place that can see
//! the messages growing and the only place that can elide a tool result without
//! an auxiliary call in the middle of a turn the operator is waiting on.

use rusqlite::{OptionalExtension, params};

use crate::db::{Db, DbError};

use super::chat::{HISTORY_MESSAGES, Message, Role, State};

/// The `settings` row that holds how full the window may get before a turn
/// compresses what it cannot carry.
pub const SETTING_SHARE: &str = "context.compress_at";

/// How full the window may get before compression, in percent.
///
/// 80 by default. Higher leaves more of the conversation verbatim and compresses
/// later, which is what most providers reward; lower compresses earlier and pays
/// the aux call sooner. Never above 95: the prompt is estimated and the estimate
/// is wrong in both directions, and a turn that fires only at 99% has no room for
/// the error.
pub const DEFAULT_SHARE: u8 = 80;

/// The fewest messages a plan will keep, whatever the arithmetic says.
///
/// Four turns is the smallest window that still reads as a conversation: the
/// question being answered, the answer before it, and enough either side to know
/// what the pronouns refer to. Below that the model is answering with no context
/// and the summary is doing all the work, which is not what a window is for.
pub const MIN_KEEP: usize = 4;

/// The most messages compressing one tail will read.
///
/// A conversation left alone for a month has a thousand turns outside the window
/// and summarising all of them is one enormous prompt that costs more than the
/// turn being protected. The newest [`COMPRESS_SLICE`] are read instead, and the
/// previous summary stands for everything older.
pub const COMPRESS_SLICE: usize = 40;

/// The shortest a compression failure backs off for, in seconds.
///
/// A minute is long enough that a provider hiccup does not cost two calls on the
/// next turn, and short enough that a recovered provider is used again while the
/// operator is still in the conversation.
pub const COOLDOWN_BASE_SECS: i64 = 60;

/// The longest a compression failure backs off for, in seconds.
///
/// Fifteen minutes. Past that the daemon is not failing, it is unconfigured, and
/// waiting longer only means the window quietly overflows instead.
pub const COOLDOWN_MAX_SECS: i64 = 900;

/// How many ineffective compressions in a row trip the breaker.
///
/// Two. One compaction that did not help is noise - the estimate was wrong, or
/// the provider answered with something useless. Two in a row means a summary is
/// not doing the work the window needs, and paying a third time is throwing
/// tokens at a problem summarising cannot solve.
pub const THRASH_LIMIT: u32 = 2;

/// How many characters of the operator's own words ride into a compaction
/// verbatim, across all of them.
///
/// A summary paraphrases, and a paraphrase of an instruction is how an agent
/// confidently does the thing it was told not to do six turns later: "use the
/// existing helper, do not add a new one" comes back as "added a helper". What
/// the assistant did survives being summarised well enough; what the operator
/// asked does not, so it is carried as it was typed.
pub const VERBATIM_USER_CHARS: usize = 24_000;

/// How much of one operator message may ride verbatim.
///
/// A single pasted log would otherwise be the whole allowance and there would be
/// no room for the instruction that came with it.
pub const VERBATIM_ONE_CHARS: usize = 4_000;

/// Roughly how many tokens a string costs, for decisions only.
///
/// Four characters per token is the rule of thumb every provider-agnostic tool
/// uses, and it is wrong in a known direction: code and JSON cost more, prose
/// costs less. It is used to pick a keep count and never to report anything, so
/// being approximately right is the whole requirement.
#[must_use]
pub fn estimate(text: &str) -> u64 {
    let chars = text.chars().count() as u64;
    chars.div_ceil(4)
}

/// What a turn should carry, given the window its model declared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plan {
    /// How many of the newest settled messages the prompt may carry.
    pub keep: usize,
    /// The estimated tokens those messages cost.
    pub estimate: u64,
    /// The budget they were measured against, when the window is known.
    pub budget: Option<u64>,
    /// Whether the estimate went over the budget even after the keep was reduced
    /// to [`MIN_KEEP`]. The turn runs anyway: a truncated conversation is a better
    /// answer than a refusal, and the operator is told.
    pub over: bool,
}

/// Read the configured trigger.
///
/// A row that is not a number, or a number outside 10..=95, is treated as unset
/// rather than obeyed: a typo in a settings row must not stop every turn from
/// compressing, and a share of 2 would compress on the first exchange.
#[must_use]
pub fn share(stored: Option<&str>) -> u8 {
    stored
        .and_then(|value| value.trim().parse::<u8>().ok())
        .filter(|value| (10..=95).contains(value))
        .unwrap_or(DEFAULT_SHARE)
}

/// The settled messages a turn may carry, oldest first, and never the answer row
/// being written.
fn settled<'a>(transcript: &'a [Message], answering: &str) -> Vec<&'a Message> {
    transcript
        .iter()
        .filter(|message| message.id != answering)
        .filter(|message| message.state == State::Complete)
        .filter(|message| !message.body.trim().is_empty())
        .collect()
}

/// Size the window.
///
/// The count is picked by walking the settled messages newest first and stopping
/// at the first one that would take the estimate over the budget. That is a
/// greedy fit rather than an optimal one, which is right here: a conversation is
/// read in order, so the newest turns are the ones that matter and the oldest
/// kept turn is the one that gets dropped.
///
/// A `window` of `None` is a model nobody could resolve a context length for.
/// The old fixed window stands: a turn that invented a budget would cut a
/// conversation for no reason a reader could see.
#[must_use]
pub fn plan(transcript: &[Message], answering: &str, window: Option<u64>, share_pct: u8) -> Plan {
    let all = settled(transcript, answering);
    let Some(window) = window.filter(|window| *window > 0) else {
        let keep = all.len().min(HISTORY_MESSAGES);
        let estimate = all[all.len().saturating_sub(keep)..]
            .iter()
            .map(|message| estimate_of(message))
            .sum();
        return Plan {
            keep,
            estimate,
            budget: None,
            over: false,
        };
    };

    let budget = window * u64::from(share_pct) / 100;
    let mut estimate = 0u64;
    let mut keep = 0usize;
    for message in all.iter().rev() {
        if keep >= HISTORY_MESSAGES {
            break;
        }
        let cost = estimate_of(message);
        // The first message is always kept whatever it costs: a single pasted log
        // that is over budget on its own is the turn's whole subject, and dropping
        // it would answer a question nobody asked.
        if keep > 0 && estimate + cost > budget {
            break;
        }
        estimate += cost;
        keep += 1;
    }
    Plan {
        keep,
        estimate,
        budget: Some(budget),
        over: estimate > budget,
    }
}

/// What one message costs, body and tool records together.
///
/// Tool output is counted twice over: it is in the body the loop streamed and
/// again in the records the transcript keeps, and the records are what actually
/// gets sent back because the body is prose. Counting the larger of the two is
/// the conservative reading and costs nothing.
fn estimate_of(message: &Message) -> u64 {
    let body = estimate(&message.body);
    let tools: u64 = message
        .tools
        .iter()
        .map(|record| estimate(&record.arguments) + estimate(&record.output))
        .sum();
    body.max(tools)
}

/// The rows a plan leaves outside the window, oldest first.
#[must_use]
pub fn outside(transcript: &[Message], answering: &str, keep: usize) -> Vec<Message> {
    let all = settled(transcript, answering);
    let start = all.len().saturating_sub(keep);
    all[..start]
        .iter()
        .map(|message| (*message).clone())
        .collect()
}

/// How much of an elided tool result is kept, per end.
///
/// A result the window cannot afford is cut to its first and last characters,
/// not dropped: the beginning says what the command was looking at and the end is
/// where an error and a summary land. The middle is what a long output is made of.
pub const ELIDE_KEEP_CHARS: usize = 600;

/// Cut tool results down until a prompt fits the budget, in place.
///
/// This is the in-turn answer to a prompt that grew past its window: a turn that
/// calls twelve tools accumulates twelve outputs, and the check at the start of
/// the turn cannot see any of them. Unlike [`compress`] this makes no provider
/// call, because it runs inside a turn the operator is waiting on and a summary
/// request in the middle of that is a second silence they did not ask for.
///
/// The newest result is elided last: it is the one the next round is about.
/// Returns how many were cut, so the caller can say so rather than silently
/// shipping a prompt that no longer contains what a tool found.
#[must_use]
pub fn elide(messages: &mut [core_runtime::provider::ChatMessage], budget: u64) -> usize {
    let total = |messages: &[core_runtime::provider::ChatMessage]| -> u64 {
        messages
            .iter()
            .map(|message| estimate(&message.content))
            .sum()
    };
    if total(messages) <= budget {
        return 0;
    }
    // Tracked as we go rather than recounted: a recount inside the loop would
    // need the whole slice while one element is borrowed mutably.
    let mut running = total(messages);
    // The newest tool result is what the next round is about, so it is the last
    // one cut. Everything older goes first even though it is a bigger saving per
    // call, because a round that cannot read what the previous tool found is
    // answering a question it can no longer see.
    let newest_tool = messages
        .iter()
        .rposition(|message| message.role == core_runtime::provider::ChatRole::Tool);
    let mut cut = 0usize;
    for (index, message) in messages.iter_mut().enumerate() {
        if running <= budget {
            break;
        }
        if Some(index) == newest_tool {
            continue;
        }
        if message.role != core_runtime::provider::ChatRole::Tool {
            continue;
        }
        if let Some(saved) = elide_one(message) {
            running = running.saturating_sub(saved);
            cut += 1;
        }
    }
    if running > budget
        && let Some(index) = newest_tool
        && elide_one(&mut messages[index]).is_some()
    {
        cut += 1;
    }
    cut
}

/// Cut one message to its ends. Returns the tokens it saved, or `None` when the
/// message is already short enough that cutting it would only remove context
/// without buying room.
fn elide_one(message: &mut core_runtime::provider::ChatMessage) -> Option<u64> {
    let chars = message.content.chars().count();
    if chars <= ELIDE_KEEP_CHARS * 2 {
        return None;
    }
    let head: String = message.content.chars().take(ELIDE_KEEP_CHARS).collect();
    let tail: String = message
        .content
        .chars()
        .skip(chars - ELIDE_KEEP_CHARS)
        .collect();
    let omitted = chars - ELIDE_KEEP_CHARS * 2;
    let rebuilt = format!(
        "{head}\n\n[{omitted} characters of this output were elided to fit the \
         context window]\n\n{tail}"
    );
    let before = estimate(&message.content);
    let after = estimate(&rebuilt);
    message.content = rebuilt;
    Some(before.saturating_sub(after))
}

/// A summary as the conversation row keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// What the earlier turns came to.
    pub text: String,
    /// How many settled rows it already covers, oldest first.
    pub rows: usize,
}

/// Read the summary a conversation is already carrying.
///
/// # Errors
///
/// A database failure reading the row. A conversation that has never been
/// compressed is `Ok(None)`, which is not an error.
pub fn stored(db: &Db, conversation: &str) -> Result<Option<Summary>, DbError> {
    db.with_conn(|conn| {
        let row = conn
            .query_row(
                "SELECT summary, summary_rows FROM chat_conversations WHERE id = ?1",
                params![conversation],
                |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;
        Ok(row.and_then(|(text, rows)| {
            let text = text?;
            if text.trim().is_empty() {
                return None;
            }
            Some(Summary {
                text,
                rows: usize::try_from(rows).unwrap_or(0),
            })
        }))
    })
}

/// Persist a summary against the conversation it describes.
///
/// # Errors
///
/// A database failure writing the row.
pub fn store(db: &Db, conversation: &str, summary: &Summary) -> Result<(), DbError> {
    let rows = i64::try_from(summary.rows).unwrap_or(i64::MAX);
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_conversations
                SET summary = ?2, summary_rows = ?3
              WHERE id = ?1",
            params![conversation, summary.text, rows],
        )?;
        Ok(())
    })
}

/// Forget a summary, so the next turn rebuilds it.
///
/// Called when the transcript shrank under it - a conversation's rows deleted or
/// reset - because a summary that covers rows which no longer exist is a claim
/// about a conversation nobody can read.
///
/// # Errors
///
/// A database failure writing the row.
pub fn clear(db: &Db, conversation: &str) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_conversations SET summary = NULL, summary_rows = 0 WHERE id = ?1",
            params![conversation],
        )?;
        Ok(())
    })
}

/// The `settings` row that holds when the breaker next allows a compaction,
/// as an RFC 3339 stamp.
pub const SETTING_COOLDOWN_UNTIL: &str = "context.compress_cooldown_until";

/// The `settings` row that holds how many failures in a row the breaker has seen.
pub const SETTING_FAILURES: &str = "context.compress_failures";

/// The `settings` row that holds how many compactions in a row did not help.
pub const SETTING_THRASH: &str = "context.compress_thrash";

/// Why compression is (or is not) allowed right now.
///
/// Returned rather than a bare bool so the reason reaches the audit log and the
/// operator can tell "the summary model is refusing" from "the summary is not
/// helping". Those want opposite fixes and a boolean cannot say which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// Go ahead.
    Clear,
    /// A failure put the breaker in cooldown. Carries the operator-facing reason.
    Cooling(String),
    /// Too many compactions in a row that did not free anything.
    Thrashing,
}

impl Health {
    /// Whether a compaction may run.
    #[must_use]
    pub fn allowed(&self) -> bool {
        matches!(self, Self::Clear)
    }

    /// The audit detail, for the row this decision writes.
    #[must_use]
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Clear => "clear",
            Self::Cooling(_) => "cooling_down",
            Self::Thrashing => "thrashing",
        }
    }
}

/// Read the breaker.
///
/// Failures are counted, and the cooldown is exponential in them: one failure
/// backs off for [`COOLDOWN_BASE_SECS`], two for twice that, and so on up to
/// [`COOLDOWN_MAX_SECS`]. A cooldown that has run out is not a cooldown, so an
/// expired stamp reads as clear without needing a write.
///
/// A settings row that will not parse is treated as clear. The alternative is a
/// typo silently disabling compression for the rest of the deployment's life,
/// and compression failing to run is the worse failure of the two.
#[must_use]
pub fn health(db: &Db, now: time::OffsetDateTime) -> Health {
    let read = |key: &str| -> Option<String> { super::store::settings_get(db, key).ok().flatten() };
    let thrash: u32 = read(SETTING_THRASH)
        .and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(0);
    if thrash >= THRASH_LIMIT {
        return Health::Thrashing;
    }
    let Some(until) = read(SETTING_COOLDOWN_UNTIL) else {
        return Health::Clear;
    };
    let Ok(until) =
        time::OffsetDateTime::parse(until.trim(), &time::format_description::well_known::Rfc3339)
    else {
        return Health::Clear;
    };
    if until <= now {
        return Health::Clear;
    }
    let wait = (until - now).whole_seconds().max(0);
    Health::Cooling(format!(
        "the summary model failed recently; compression resumes in {wait}s"
    ))
}

/// Count one failure and set the cooldown it earns.
///
/// Returns the seconds of cooldown, so the caller can say how long in one audit
/// row instead of making the operator read two.
pub fn record_failure(db: &Db, now: time::OffsetDateTime) -> i64 {
    let failures: u32 = super::store::settings_get(db, SETTING_FAILURES)
        .ok()
        .flatten()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(0)
        .saturating_add(1);
    // Exponential, capped, and computed in i64 with the cap applied before any
    // multiply that could overflow on a truly broken counter.
    let exponent = failures.saturating_sub(1).min(8);
    let seconds = COOLDOWN_BASE_SECS
        .saturating_mul(1_i64 << exponent.min(4))
        .clamp(COOLDOWN_BASE_SECS, COOLDOWN_MAX_SECS);
    let until = now + time::Duration::seconds(seconds);
    let stamp = until
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    let _ = super::store::settings_put(db, SETTING_FAILURES, &failures.to_string());
    let _ = super::store::settings_put(db, SETTING_COOLDOWN_UNTIL, &stamp);
    seconds
}

/// A compaction ran and worked: clear the failure count and its cooldown.
pub fn record_success(db: &Db) {
    let _ = super::store::settings_put(db, SETTING_FAILURES, "0");
    let _ = super::store::settings_put(db, SETTING_COOLDOWN_UNTIL, "");
}

/// A compaction ran and did not make the window any smaller.
///
/// Counted separately from a failure because the fix is different: a summary
/// that comes back empty or useless means the aux model is wrong for the job,
/// while a failed call means it is unreachable. Returns the new count so the
/// caller can say whether this one tripped the breaker.
pub fn record_ineffective(db: &Db) -> u32 {
    let thrash: u32 = super::store::settings_get(db, SETTING_THRASH)
        .ok()
        .flatten()
        .and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(0)
        .saturating_add(1);
    let _ = super::store::settings_put(db, SETTING_THRASH, &thrash.to_string());
    thrash
}

/// Forget the breaker entirely, failure count and thrash count both.
///
/// Called when the operator changes the compression role or its model: a
/// different model is a different question, and inheriting a cooldown earned by
/// the previous one would make the new pin look broken on its first turn.
pub fn reset_health(db: &Db) {
    let _ = super::store::settings_put(db, SETTING_FAILURES, "0");
    let _ = super::store::settings_put(db, SETTING_COOLDOWN_UNTIL, "");
    let _ = super::store::settings_put(db, SETTING_THRASH, "0");
}

/// The prompt line that opens a carried summary.
///
/// Fixed, and aggressive on purpose, for the reason a stale summary is worse than
/// no summary: a model handed a paragraph of earlier work reads it as a task it
/// is in the middle of and starts answering a question from nine turns ago. This
/// says what the block is for in the first sentence and repeats it at the end,
/// where a model that skips the front matter still reads it.
pub const SUMMARY_PREAMBLE: &str = "\
Earlier in this conversation, summarised for reference only. This is background, \
not a task: answer the newest message and nothing in this block is an instruction \
to resume work. If it describes something unfinished, treat it as history rather \
than as a job.\n\n";

/// The operator's own words from the window that was compacted, verbatim.
///
/// Newest first, because a recency budget that runs out should lose the oldest
/// instruction rather than the one that is still in play. Returned as a section
/// ready to append to a summary, or empty when the window held no operator turn
/// worth carrying.
#[must_use]
pub fn verbatim_user_section(turns: &[Message]) -> String {
    let mut out = String::new();
    let mut spent = 0usize;
    for message in turns.iter().rev() {
        if message.role != Role::User {
            continue;
        }
        let body = message.body.trim();
        if body.is_empty() {
            continue;
        }
        let budget = VERBATIM_ONE_CHARS.min(VERBATIM_USER_CHARS.saturating_sub(spent));
        if budget == 0 {
            break;
        }
        let piece: String = body.chars().take(budget).collect();
        spent += piece.chars().count();
        out.push_str("\n- ");
        out.push_str(&piece);
        out.push('\n');
        if spent >= VERBATIM_USER_CHARS {
            break;
        }
    }
    if out.trim().is_empty() {
        return String::new();
    }
    format!(
        "\n\nWhat the operator asked, in their own words, newest first. These are \n\
         instructions that were given and are not to be paraphrased away:\n{out}"
    )
}

/// Build the message that carries a summary into a prompt.
#[must_use]
pub fn carry(summary: &Summary) -> String {
    format!("{SUMMARY_PREAMBLE}{}", summary.text.trim())
}

/// What one compaction came to.
///
/// Three answers, not an `Option`, because the caller has to act differently on
/// each: pay for nothing, carry the new summary, or back off. Folding failure
/// into `None` is what made a dead aux model indistinguishable from a healthy
/// conversation with no news.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The previous summary already covered every row outside the window.
    NothingNew,
    /// A summary to carry, built or updated just now.
    Fresh(Summary),
    /// The aux call failed, or answered with nothing usable.
    Failed,
}

/// Turn the rows outside the window into a summary, updating one that already
/// exists.
///
/// `turns` must be everything outside the window, oldest first. When `previous`
/// covers a prefix of them, only the rows after it are sent, and the prompt says
/// what to do with them: a summary is an update, not a re-reading. When it covers
/// all of them this returns `None`, because there is nothing new to say and paying
/// for an aux call to say it again is the bug this parameter exists to fix.
///
/// [`Outcome::Failed`] is the best-effort failure case, kept distinct from
/// [`Outcome::NothingNew`] because the two want opposite handling: a provider
/// that is down must leave the turn exactly as it would have been and start a
/// cooldown, while having nothing new to say is the healthy steady state and
/// must not.
pub fn compress(
    config: &super::chat::TurnConfig,
    turns: &[Message],
    previous: Option<&Summary>,
) -> Outcome {
    let covered = previous.map(|summary| summary.rows).unwrap_or(0);
    if turns.is_empty() || covered >= turns.len() {
        return Outcome::NothingNew;
    }
    // Only the newest slice of what is new. A thousand-row tail is not a bigger
    // prompt than a provider can take, but it is a bigger one than is worth
    // paying for: the previous summary stands for whatever the slice leaves out.
    let fresh = &turns[covered..];
    let slice = &fresh[fresh.len().saturating_sub(COMPRESS_SLICE)..];

    let mut lines = String::new();
    for message in slice {
        let who = match message.role {
            Role::User => "operator",
            Role::Assistant => "assistant",
        };
        let body: String = message.body.chars().take(1200).collect();
        lines.push_str(&format!("{who}: {body}\n"));
        // A tool call is how the work was done and often the only record of what
        // it found: a summary that dropped every command output would say "ran
        // some commands" and leave out the answer.
        for record in message.tools.iter().take(6) {
            let output: String = record.output.chars().take(400).collect();
            let verdict = if record.is_error { "failed" } else { "ran" };
            lines.push_str(&format!("  {verdict} {}: {output}\n", record.name));
        }
    }

    let (system, user) = match previous {
        Some(previous) => (
            "You maintain the running summary of a long conversation. You are given the \
             summary so far and the newest turns. Reply with an updated summary that keeps \
             every decision, name, path, number and open question from both, under 250 \
             words. Reply with the summary only, no preamble, no headings, no thinking."
                .to_owned(),
            format!(
                "Summary so far:\n{}\n\nNewest turns:\n{}",
                previous.text.trim(),
                lines
            ),
        ),
        None => (
            "You summarise the earlier part of a conversation so it can be carried as \
             context. Keep every decision, name, path, number and open question, and say \
             what was settled and what was still open. Reply with the summary only, under \
             250 words, no preamble, no thinking."
                .to_owned(),
            lines,
        ),
    };

    let Some(text) = super::chat::aux_completion(config, &system, &user) else {
        return Outcome::Failed;
    };
    let text = text.trim().to_owned();
    // An empty summary is a failure, not a success: carrying nothing forward
    // would silently drop the very turns this call existed to preserve.
    if text.is_empty() {
        return Outcome::Failed;
    }
    // The summary says what happened; the operator's own words say what was
    // asked. Carried together so a later turn cannot read a paraphrase of an
    // instruction and act on that instead.
    let verbatim = verbatim_user_section(turns);
    Outcome::Fresh(Summary {
        text: format!("{text}{verbatim}"),
        rows: turns.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, role: Role, body: &str) -> Message {
        Message {
            id: id.to_owned(),
            role,
            body: body.to_owned(),
            state: State::Complete,
            error_code: None,
            created_at: "2026-01-01T00:00:00Z".to_owned(),
            model: None,
            prompt_tokens: None,
            completion_tokens: None,
            cached_tokens: None,
            context_tokens: None,
            context_window: None,
            duration_ms: None,
            reasoning: None,
            tools: Vec::new(),
            client_message_id: None,
            job_id: None,
            rounds: None,
            capped: None,
            quota: None,
        }
    }

    fn transcript(pairs: usize, body_chars: usize) -> Vec<Message> {
        let body = "x".repeat(body_chars);
        let mut out = Vec::new();
        for index in 0..pairs {
            out.push(message(&format!("q{index}"), Role::User, &body));
            out.push(message(&format!("a{index}"), Role::Assistant, &body));
        }
        out
    }

    #[test]
    fn a_window_nobody_could_resolve_keeps_the_old_fixed_count() {
        let rows = transcript(20, 10);
        let plan = plan(&rows, "", None, DEFAULT_SHARE);
        assert_eq!(plan.keep, HISTORY_MESSAGES);
        assert_eq!(plan.budget, None);
        assert!(!plan.over, "no window means no verdict to give");
    }

    #[test]
    fn a_small_window_keeps_fewer_messages_than_the_old_rule() {
        // 40 messages of 4000 characters is 1000 estimated tokens each. A 12 000
        // token window at 80% holds 9600, so nine of them.
        let rows = transcript(20, 4000);
        let plan = plan(&rows, "", Some(12_000), DEFAULT_SHARE);
        assert_eq!(plan.budget, Some(9_600));
        assert!(
            plan.keep < HISTORY_MESSAGES,
            "the window, not the count, is what decided: {}",
            plan.keep
        );
        assert!(
            plan.estimate <= 9_600,
            "kept {}: {}",
            plan.keep,
            plan.estimate
        );
    }

    #[test]
    fn a_large_window_keeps_exactly_the_old_count() {
        let rows = transcript(40, 4000);
        let plan = plan(&rows, "", Some(1_000_000), DEFAULT_SHARE);
        assert_eq!(plan.keep, HISTORY_MESSAGES);
        assert!(!plan.over);
    }

    #[test]
    fn one_message_over_budget_on_its_own_is_still_kept() {
        // The newest message is the one that overflowed, which is the case the
        // rule exists for: a single pasted log is the turn's whole subject.
        let rows = vec![
            message("a", Role::Assistant, "short"),
            message("b", Role::User, &"y".repeat(400_000)),
        ];
        let plan = plan(&rows, "", Some(100_000), DEFAULT_SHARE);
        assert_eq!(plan.keep, 1, "the newest message is the turn's subject");
        assert!(plan.over, "and the turn is told the window is over budget");
    }

    #[test]
    fn the_answer_being_written_is_never_part_of_the_window() {
        let rows = vec![
            message("q", Role::User, "question"),
            message("a", Role::Assistant, "partial answer"),
        ];
        let plan = plan(&rows, "a", Some(100_000), DEFAULT_SHARE);
        assert_eq!(plan.keep, 1);
        let outside = outside(&rows, "a", 1);
        assert!(outside.is_empty(), "the answer is not history either");
    }

    #[test]
    fn what_the_window_leaves_out_is_what_gets_summarised() {
        let rows = transcript(20, 10);
        let outside = outside(&rows, "", 6);
        assert_eq!(outside.len(), rows.len() - 6);
        assert_eq!(outside[0].id, "q0");
        assert_eq!(
            outside.last().map(|message| message.id.as_str()),
            Some("a16"),
            "the newest dropped row is the one before the window"
        );
    }

    #[test]
    fn a_share_outside_the_sane_range_is_ignored() {
        assert_eq!(share(None), DEFAULT_SHARE);
        assert_eq!(share(Some(" 55 ")), 55);
        assert_eq!(share(Some("2")), DEFAULT_SHARE, "too eager to compress");
        assert_eq!(share(Some("99")), DEFAULT_SHARE, "no room for the error");
        assert_eq!(share(Some("lots")), DEFAULT_SHARE);
    }

    #[test]
    fn a_summary_already_covering_the_tail_is_not_paid_for_again() {
        let rows = outside(&transcript(20, 10), "", 6);
        let previous = Summary {
            text: "earlier".to_owned(),
            rows: rows.len(),
        };
        let config = super::super::chat::TurnConfig {
            base_url: "http://127.0.0.1:1/v1".to_owned(),
            key_ref: core_runtime::provider::SecretRef::Env("KALLISTI_TEST_KEY".to_owned()),
            model: "m/one".to_owned(),
            dialect: core_domain::ApiDialect::ChatCompletions,
            reasoning_split: false,
            direct: false,
            project: None,
        };
        // Nothing listens on that port, so a call would return None anyway; what
        // is asserted is that the covered comparison decides it, before any
        // request is built.
        assert_eq!(
            compress(&config, &rows, Some(&previous)),
            Outcome::NothingNew
        );
    }

    #[test]
    fn a_fresh_breaker_allows_compression() {
        let db = Db::open_in_memory().expect("in-memory database");
        let now = time::OffsetDateTime::now_utc();
        let state = health(&db, now);
        assert!(state.allowed());
        assert_eq!(state.reason(), "clear");
    }

    #[test]
    fn one_failure_cools_down_and_the_cooldown_runs_out() {
        let db = Db::open_in_memory().expect("in-memory database");
        let now = time::OffsetDateTime::now_utc();
        let waited = record_failure(&db, now);
        assert_eq!(waited, COOLDOWN_BASE_SECS);

        // Still inside the window: held, and the reason says how long is left.
        let held = health(&db, now + time::Duration::seconds(1));
        assert!(!held.allowed());
        assert_eq!(held.reason(), "cooling_down");

        // Past it: clear again, without anything having to write the row back.
        assert!(
            health(&db, now + time::Duration::seconds(waited + 1)).allowed(),
            "an expired cooldown is not a cooldown"
        );
    }

    #[test]
    fn repeated_failures_back_off_further_but_never_forever() {
        let db = Db::open_in_memory().expect("in-memory database");
        let now = time::OffsetDateTime::now_utc();
        assert_eq!(record_failure(&db, now), 60);
        assert_eq!(record_failure(&db, now), 120);
        assert_eq!(record_failure(&db, now), 240);
        assert_eq!(record_failure(&db, now), 480);
        // The cap, and it stays there however many more arrive.
        assert_eq!(record_failure(&db, now), COOLDOWN_MAX_SECS);
        assert_eq!(record_failure(&db, now), COOLDOWN_MAX_SECS);
    }

    #[test]
    fn a_success_clears_the_cooldown_it_earned() {
        let db = Db::open_in_memory().expect("in-memory database");
        let now = time::OffsetDateTime::now_utc();
        record_failure(&db, now);
        assert!(!health(&db, now).allowed());
        record_success(&db);
        assert!(
            health(&db, now).allowed(),
            "a provider that answered is not a provider that is down"
        );
    }

    #[test]
    fn compactions_that_do_not_help_trip_the_breaker() {
        let db = Db::open_in_memory().expect("in-memory database");
        let now = time::OffsetDateTime::now_utc();
        assert_eq!(record_ineffective(&db), 1);
        assert!(
            health(&db, now).allowed(),
            "one useless summary is noise, not a breaker"
        );
        assert_eq!(record_ineffective(&db), THRASH_LIMIT);
        let state = health(&db, now);
        assert!(!state.allowed());
        assert_eq!(state.reason(), "thrashing");
        // A cooldown does not clear a thrash count; only a reset does.
        record_success(&db);
        assert!(!health(&db, now).allowed());
        reset_health(&db);
        assert!(health(&db, now).allowed());
    }

    #[test]
    fn a_breaker_row_that_will_not_parse_is_ignored() {
        let db = Db::open_in_memory().expect("in-memory database");
        let now = time::OffsetDateTime::now_utc();
        super::super::store::settings_put(&db, SETTING_COOLDOWN_UNTIL, "not a stamp")
            .expect("written");
        super::super::store::settings_put(&db, SETTING_THRASH, "lots").expect("written");
        assert!(
            health(&db, now).allowed(),
            "a typo must not disable compression for good"
        );
    }

    #[test]
    fn a_summary_round_trips_through_the_conversation_row() {
        let db = Db::open_in_memory().expect("in-memory database");
        let conversation = super::super::chat::create_conversation(&db).expect("conversation");
        assert_eq!(stored(&db, &conversation).expect("readable"), None);

        let summary = Summary {
            text: "we settled the build".to_owned(),
            rows: 12,
        };
        store(&db, &conversation, &summary).expect("written");
        assert_eq!(
            stored(&db, &conversation).expect("readable"),
            Some(summary.clone())
        );
        // A second summary replaces the first rather than stacking.
        store(
            &db,
            &conversation,
            &Summary {
                text: "and then shipped it".to_owned(),
                rows: 20,
            },
        )
        .expect("written");
        let read = stored(&db, &conversation)
            .expect("readable")
            .expect("a summary");
        assert_eq!(read.rows, 20);
        assert!(read.text.contains("shipped"), "{}", read.text);

        clear(&db, &conversation).expect("cleared");
        assert_eq!(stored(&db, &conversation).expect("readable"), None);
    }

    fn tool_message(body: &str) -> core_runtime::provider::ChatMessage {
        let mut message =
            core_runtime::provider::ChatMessage::text(core_runtime::provider::ChatRole::Tool, body);
        message.tool_call_id = Some("call_1".to_owned());
        message
    }

    #[test]
    fn a_prompt_that_already_fits_is_left_alone() {
        let mut messages = vec![tool_message(&"y".repeat(10_000))];
        assert_eq!(elide(&mut messages, 10_000), 0);
        assert_eq!(
            messages[0].content.chars().count(),
            10_000,
            "nothing was cut"
        );
    }

    #[test]
    fn an_oversized_tool_result_keeps_its_head_and_its_tail() {
        let mut body = String::new();
        body.push_str("COMMAND: cargo test\n");
        body.push_str(&"m".repeat(60_000));
        body.push_str("\nERROR: the gate is red");
        let mut messages = vec![tool_message(&body)];
        let cut = elide(&mut messages, 1_000);
        assert_eq!(cut, 1);
        let kept = &messages[0].content;
        assert!(kept.starts_with("COMMAND: cargo test"), "the head survived");
        assert!(kept.len() < body.len(), "the middle is gone");
        assert!(kept.contains("characters of this output were elided"));
        assert!(
            kept.ends_with("ERROR: the gate is red"),
            "the tail survived"
        );
        assert!(estimate(kept) < estimate(&body), "the prompt got smaller");
    }

    #[test]
    fn the_newest_tool_result_is_the_last_one_cut() {
        // Two results, each big enough to be cuttable on its own. Enough budget
        // that cutting either one alone does the job, so the order is visible.
        let mut messages = vec![
            tool_message(&"a".repeat(40_000)),
            core_runtime::provider::ChatMessage::text(
                core_runtime::provider::ChatRole::Assistant,
                "looking",
            ),
            tool_message(&format!("NEWEST{}", "z".repeat(40_000))),
        ];
        // 20 000 estimated tokens of tool output; cutting the older result alone
        // brings it to about 10 300, so 11 000 of budget is met by that cut and
        // the newest one should never be touched.
        let cut = elide(&mut messages, 11_000);
        assert_eq!(cut, 1, "one cut was enough");
        assert!(
            messages[0]
                .content
                .contains("characters of this output were elided"),
            "the older result was cut first"
        );
        assert!(
            messages[2].content.starts_with("NEWEST"),
            "the newest result still has its whole body: {}",
            messages[2].content.chars().take(40).collect::<String>()
        );
    }

    #[test]
    fn a_user_message_is_never_cut_for_room() {
        let mut messages = vec![
            core_runtime::provider::ChatMessage::text(
                core_runtime::provider::ChatRole::User,
                "u".repeat(80_000),
            ),
            tool_message(&"t".repeat(80_000)),
        ];
        let cut = elide(&mut messages, 100);
        assert_eq!(cut, 1, "only the tool result was cut");
        assert_eq!(
            messages[0].content.chars().count(),
            80_000,
            "the operator's words are not a budget line"
        );
    }

    #[test]
    fn nothing_to_cut_leaves_an_over_budget_prompt_intact() {
        // Every message is already under the cut threshold, so there is no room
        // to buy. The count stays zero rather than lying about a saving.
        let mut messages = vec![tool_message(&"s".repeat(1_000))];
        assert_eq!(elide(&mut messages, 1), 0);
    }

    #[test]
    fn the_operator_carries_into_the_summary_in_their_own_words() {
        let turns = vec![
            message("q1", Role::User, "use the existing helper"),
            message("a1", Role::Assistant, "adding a new one"),
            message("q2", Role::User, "no, the existing one"),
        ];
        let section = verbatim_user_section(&turns);
        assert!(section.contains("use the existing helper"));
        assert!(section.contains("no, the existing one"));
        assert!(
            !section.contains("adding a new one"),
            "the assistant's prose is summarised, not carried: {section}"
        );
        assert!(section.contains("not to be paraphrased away"));
        // Newest first.
        let newest = section.find("no, the existing one").expect("newest");
        let older = section.find("use the existing helper").expect("older");
        assert!(newest < older, "newest first: {section}");
    }

    #[test]
    fn a_window_of_only_model_turns_carries_no_operator_section() {
        let turns = vec![message("a1", Role::Assistant, "I did the thing")];
        assert_eq!(verbatim_user_section(&turns), "", "nothing to carry");
    }

    #[test]
    fn the_operator_section_stops_at_its_budget() {
        let body = "q".repeat(VERBATIM_ONE_CHARS * 3);
        let turns = vec![
            message("q1", Role::User, &body),
            message("q2", Role::User, &body),
            message("q3", Role::User, &body),
            message("q4", Role::User, &body),
            message("q5", Role::User, &body),
            message("q6", Role::User, &body),
            message("q7", Role::User, &body),
        ];
        let section = verbatim_user_section(&turns);
        let carried = section.matches('q').count();
        assert!(
            carried <= VERBATIM_USER_CHARS,
            "carried {carried}, budget {}",
            VERBATIM_USER_CHARS
        );
        assert!(
            carried >= VERBATIM_ONE_CHARS,
            "the newest one is carried in full: {carried}"
        );
    }

    #[test]
    fn the_carried_summary_says_it_is_reference_and_not_a_task() {
        let carried = carry(&Summary {
            text: "we were mid-way through the build".to_owned(),
            rows: 4,
        });
        assert!(carried.starts_with(SUMMARY_PREAMBLE.trim_end()));
        assert!(carried.contains("reference only"));
        assert!(carried.contains("not a task"));
        assert!(carried.ends_with("we were mid-way through the build"));
    }
}
