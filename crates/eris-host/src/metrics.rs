//! What the daemon spent, and how long it took.
//!
//! Every finished answer already records its model, its token counts, how many of
//! the prompt tokens were served from cache, how long the turn took and whether it
//! failed. Nothing aggregated them, so "why was that slow" and "what is this
//! costing" were answered by pasting SQL into a terminal.
//!
//! One question per window: every finished answer since a cutoff, grouped by
//! model. Migration 0027 puts an index on `(created_at, model)` so that question
//! is answered from the index rather than by scanning the transcript, which is the
//! table that grows fastest in this product.
//!
//! Medians and p95 are computed here rather than in SQL. SQLite has no percentile
//! function, and a percentile approximated by bucketing is a number that looks
//! authoritative and is not; the durations are already in hand from the one scan,
//! so sorting them is a few microseconds and the answer is exact.

use crate::db::{Db, DbError};

/// One window of history the panel shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// The last twenty-four hours.
    Day,
    /// The last seven days.
    Week,
    /// Everything that is still in the transcript.
    ///
    /// A conversation's own numbers are not a window: a chat that started last
    /// month is still the chat the operator is reading, and cutting it to 24h
    /// would report the tail of it as if it were the whole thing.
    All,
}

impl Window {
    /// The window's length in hours.
    #[must_use]
    pub fn hours(self) -> i64 {
        match self {
            Self::Day => 24,
            Self::Week => 24 * 7,
            // Long enough to reach before any row this daemon can hold.
            Self::All => 24 * 365 * 100,
        }
    }

    /// The label a person reads.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Day => "24 hours",
            Self::Week => "7 days",
            Self::All => "all time",
        }
    }

    /// Parse the query-string spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "24h" | "day" | "1d" => Some(Self::Day),
            "7d" | "week" => Some(Self::Week),
            "all" | "all-time" | "total" => Some(Self::All),
            _ => None,
        }
    }
}

/// Durations, calls, tokens in and out, cached tokens, failures.
type Bucket = (Vec<i64>, i64, i64, i64, i64, i64);

/// One model's numbers inside a window.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRow {
    /// The model id, or `unknown` when a row predates model capture.
    pub model: String,
    /// Answers completed or failed.
    pub calls: i64,
    /// Answers that ended failed.
    pub failed: i64,
    /// The median turn duration, when any row recorded one.
    pub median_ms: Option<i64>,
    /// The 95th percentile turn duration.
    pub p95_ms: Option<i64>,
    /// Prompt tokens billed.
    pub prompt_tokens: i64,
    /// Completion tokens billed.
    pub completion_tokens: i64,
    /// Prompt tokens served from cache.
    pub cached_tokens: i64,
}

impl ModelRow {
    /// The share of prompt tokens that came from cache, 0.0 to 1.0.
    #[must_use]
    pub fn cache_rate(&self) -> f64 {
        if self.prompt_tokens <= 0 {
            return 0.0;
        }
        (self.cached_tokens as f64 / self.prompt_tokens as f64).clamp(0.0, 1.0)
    }

    /// Whether this model had any failure in the window.
    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.failed > 0
    }
}

/// Everything one window holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Summary {
    /// One row per model, busiest first.
    pub rows: Vec<ModelRow>,
    /// Answers in the window, every model.
    pub calls: i64,
    /// Failures in the window.
    pub failed: i64,
    /// Prompt tokens, every model.
    pub prompt_tokens: i64,
    /// Completion tokens, every model.
    pub completion_tokens: i64,
    /// Cached prompt tokens, every model.
    pub cached_tokens: i64,
    /// Rows that recorded no duration, so the percentiles say what they skip.
    pub unmeasured: i64,
}

impl Summary {
    /// The whole window's cache hit rate.
    #[must_use]
    pub fn cache_rate(&self) -> f64 {
        if self.prompt_tokens <= 0 {
            return 0.0;
        }
        (self.cached_tokens as f64 / self.prompt_tokens as f64).clamp(0.0, 1.0)
    }

    /// Whether there is anything to draw.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.calls == 0
    }
}

/// Collect one window, in one query.
///
/// # Errors
///
/// A database failure.
pub fn summary(db: &Db, window: Window) -> Result<Summary, DbError> {
    summary_in(db, window, None)
}

/// The same numbers, for one conversation or for all of them.
///
/// A conversation filter is not a different calculation: it is the same rows,
/// narrowed. The phone's chat footer asks for one chat, and it must get the same
/// percentile the settings panel would have shown, or the two surfaces disagree
/// about a number that is supposed to be a measurement.
pub fn summary_in(db: &Db, window: Window, conversation: Option<&str>) -> Result<Summary, DbError> {
    const SQL: &str = "
        SELECT COALESCE(model, 'unknown') AS model,
               duration_ms,
               COALESCE(prompt_tokens, 0),
               COALESCE(completion_tokens, 0),
               COALESCE(cached_tokens, 0),
               CASE WHEN state = 'failed' THEN 1 ELSE 0 END
          FROM chat_messages
         WHERE role = 'assistant'
           AND created_at >= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1)
           AND (?2 IS NULL OR conversation_id = ?2)
         ORDER BY created_at DESC";
    let cutoff = format!("-{} hours", window.hours());
    let rows: Vec<(String, Option<i64>, i64, i64, i64, i64)> = db.with_conn(|conn| {
        let mut statement = conn.prepare(SQL)?;
        let mapped = statement.query_map(rusqlite::params![cutoff, conversation], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
            ))
        })?;
        mapped.collect()
    })?;

    // One pass into per-model buckets, then a sort per model for the percentiles.
    let mut buckets: std::collections::BTreeMap<String, Bucket> = std::collections::BTreeMap::new();
    let mut summary = Summary::default();
    for (model, duration, prompt, completion, cached, failed) in rows {
        let bucket = buckets.entry(model).or_default();
        if let Some(duration) = duration {
            bucket.0.push(duration);
        } else {
            summary.unmeasured += 1;
        }
        bucket.1 += 1;
        bucket.2 += prompt;
        bucket.3 += completion;
        bucket.4 += cached;
        bucket.5 += failed;
        summary.calls += 1;
        summary.prompt_tokens += prompt;
        summary.completion_tokens += completion;
        summary.cached_tokens += cached;
    }
    for (model, (mut durations, calls, prompt, completion, cached, failed)) in buckets {
        durations.sort_unstable();
        summary.rows.push(ModelRow {
            model,
            calls,
            failed,
            median_ms: percentile(&durations, 50.0),
            p95_ms: percentile(&durations, 95.0),
            prompt_tokens: prompt,
            completion_tokens: completion,
            cached_tokens: cached,
        });
        summary.failed += failed;
    }
    // Busiest first, then by name so two models with equal traffic do not swap
    // places between two page loads.
    summary
        .rows
        .sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.model.cmp(&b.model)));
    Ok(summary)
}

/// The exact percentile of a sorted slice, or `None` when it holds nothing.
///
/// Nearest-rank, which is what a latency percentile means: the value at or below
/// which that share of the samples fall. No interpolation, because an interpolated
/// p95 invents a duration no turn ever had.
#[must_use]
pub fn percentile(sorted: &[i64], percent: f64) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (percent / 100.0 * sorted.len() as f64).ceil() as usize;
    let index = rank.saturating_sub(1).min(sorted.len() - 1);
    sorted.get(index).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("in-memory db")
    }

    fn answer(
        db: &Db,
        id: &str,
        model: &str,
        duration: Option<i64>,
        prompt: i64,
        cached: i64,
        failed: bool,
    ) {
        answer_in(db, "c", id, model, duration, prompt, cached, failed);
    }

    #[allow(clippy::too_many_arguments)]
    fn answer_in(
        db: &Db,
        conversation: &str,
        id: &str,
        model: &str,
        duration: Option<i64>,
        prompt: i64,
        cached: i64,
        failed: bool,
    ) {
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO chat_conversations (id, title) VALUES (?1, 't') ON CONFLICT(id) DO NOTHING",
                [conversation],
            )?;
            conn.execute(
                "INSERT INTO chat_messages (id, conversation_id, role, body, model, duration_ms,
                                            prompt_tokens, completion_tokens, cached_tokens, state, error_code)
                 VALUES (?1, ?8, 'assistant', 'x', ?2, ?3, ?4, 5, ?5, ?6, ?7)",
                rusqlite::params![
                    id,
                    model,
                    duration,
                    prompt,
                    cached,
                    if failed { "failed" } else { "complete" },
                    if failed { Some("provider") } else { None },
                    conversation
                ],
            )
        })
        .expect("seed");
    }

    #[test]
    fn a_conversation_sees_only_its_own_numbers() {
        let db = db();
        answer_in(&db, "c1", "a1", "model-a", Some(100), 100, 50, false);
        answer_in(&db, "c1", "a2", "model-a", Some(300), 100, 50, false);
        answer_in(&db, "c2", "b1", "model-b", Some(900), 1000, 0, false);

        let one = summary_in(&db, Window::All, Some("c1")).expect("c1");
        assert_eq!(one.calls, 2, "one conversation is its own two answers");
        assert_eq!(one.rows.len(), 1);
        assert_eq!(one.rows[0].model, "model-a");
        assert_eq!(one.rows[0].p95_ms, Some(300));
        assert_eq!(one.prompt_tokens, 200);

        let every = summary_in(&db, Window::All, None).expect("all");
        assert_eq!(every.calls, 3, "no filter is every conversation");

        let missing = summary_in(&db, Window::All, Some("nope")).expect("missing");
        assert_eq!(missing.calls, 0, "an unknown chat is empty, not an error");
        assert_eq!(Window::parse("all"), Some(Window::All));
    }

    #[test]
    fn percentiles_are_nearest_rank_and_none_when_empty() {
        assert_eq!(percentile(&[], 50.0), None);
        assert_eq!(percentile(&[7], 95.0), Some(7));
        assert_eq!(
            percentile(&[1, 2, 3, 4], 50.0),
            Some(2),
            "n=4 p50 is the 2nd"
        );
        assert_eq!(percentile(&[1, 2, 3, 4], 95.0), Some(4));
        let hundred: Vec<i64> = (1..=100).collect();
        assert_eq!(percentile(&hundred, 50.0), Some(50));
        assert_eq!(percentile(&hundred, 95.0), Some(95));
    }

    #[test]
    fn a_window_groups_by_model_and_sums_what_it_saw() {
        let db = db();
        for (index, duration) in [100, 200, 300, 400].into_iter().enumerate() {
            answer(
                &db,
                &format!("a{index}"),
                "model-a",
                Some(duration),
                100,
                50,
                false,
            );
        }
        answer(&db, "b1", "model-b", Some(900), 10, 0, true);

        let day = summary(&db, Window::Day).expect("summary");
        assert_eq!(day.calls, 5);
        assert_eq!(day.failed, 1);
        assert_eq!(day.rows.len(), 2);
        // Busiest first.
        assert_eq!(day.rows[0].model, "model-a");
        assert_eq!(day.rows[0].calls, 4);
        assert_eq!(day.rows[0].median_ms, Some(200));
        assert_eq!(day.rows[0].p95_ms, Some(400));
        assert_eq!(day.rows[0].prompt_tokens, 400);
        assert_eq!(day.rows[0].cached_tokens, 200);
        assert!((day.rows[0].cache_rate() - 0.5).abs() < f64::EPSILON);
        assert!(!day.rows[0].has_failures());
        assert!(day.rows[1].has_failures());
        // 200 cached of 410 prompt tokens across both models, not 200 of 400:
        // the second model's prompt counts toward the denominator.
        assert!(
            (day.cache_rate() - 200.0 / 410.0).abs() < 0.001,
            "{}",
            day.cache_rate()
        );
    }

    #[test]
    fn rows_without_a_duration_are_counted_and_never_faked() {
        let db = db();
        answer(&db, "a1", "model-a", Some(120), 10, 0, false);
        answer(&db, "a2", "model-a", None, 10, 0, false);
        let day = summary(&db, Window::Day).expect("summary");
        assert_eq!(
            day.unmeasured, 1,
            "the panel says what it could not measure"
        );
        assert_eq!(day.rows[0].calls, 2);
        assert_eq!(
            day.rows[0].median_ms,
            Some(120),
            "the median is over measured rows"
        );
    }

    #[test]
    fn an_empty_window_is_empty_rather_than_a_division_by_zero() {
        let db = db();
        let day = summary(&db, Window::Day).expect("summary");
        assert!(day.is_empty());
        assert_eq!(day.calls, 0);
        assert_eq!(day.cache_rate(), 0.0);
        assert!(day.rows.is_empty());
    }

    #[test]
    fn the_user_rows_of_a_turn_are_not_counted_as_calls() {
        let db = db();
        answer(&db, "a1", "model-a", Some(10), 10, 0, false);
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO chat_messages (id, conversation_id, role, body)
                 VALUES ('u1', 'c', 'user', 'a question')",
                [],
            )
        })
        .expect("seed");
        let day = summary(&db, Window::Day).expect("summary");
        assert_eq!(day.calls, 1, "only answers cost tokens and time");
    }

    #[test]
    fn the_window_spellings_are_the_ones_a_client_sends() {
        assert_eq!(Window::parse("24h"), Some(Window::Day));
        assert_eq!(Window::parse("7d"), Some(Window::Week));
        assert_eq!(Window::parse("week"), Some(Window::Week));
        assert_eq!(Window::parse("month"), None);
        assert_eq!(Window::Day.label(), "24 hours");
        assert_eq!(Window::Week.hours(), 168);
    }
}
