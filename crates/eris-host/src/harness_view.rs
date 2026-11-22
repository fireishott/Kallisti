//! The harness runs the top bar reports.
//!
//! A row here is one run of an external coding harness. The list route is the
//! daemon's, and the page needs a copy before it can draw anything: the bar is
//! server-rendered, so a control that waited for a fetch would not exist on the
//! first paint and the row would shift when it arrived.
//!
//! [`rows`] is the one place that knows where the snapshot comes from. It is
//! deliberately empty on this branch: the process that records runs writes its
//! registry separately, and the page must report nothing rather than invent a
//! row before that registry exists. When it lands, only this function changes.

/// One harness run, as the list route reports it.
///
/// The fields are the wire contract's, not a second shape: a page, a test and
/// the eventual snapshot all read the same names, so a run cannot be missing a
/// field on one surface and present on another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The run's opaque id, also the route segment.
    pub id: String,
    /// The harness that ran it, as the operator named it.
    pub harness: String,
    /// `running`, `succeeded`, `failed`, `timed_out`, `killed` or `stalled`.
    pub status: String,
    /// The task the run was given.
    pub task: String,
    /// When the run started, as the daemon stamped it.
    pub started_at: String,
    /// When the run finished, or `None` while it still runs.
    pub finished_at: Option<String>,
    /// How long it has run, in milliseconds.
    pub elapsed_ms: i64,
    /// How long since it last said anything, in milliseconds.
    pub silent_ms: i64,
    /// The process exit code, when there is one.
    pub exit_code: Option<i32>,
    /// True when the watchdog has stopped trusting the run to be alive.
    pub stalled: bool,
}

// Pages read the registry through here. Under test it answers empty unless the
// test asked for runs: the registry is process-wide and several suites run in
// one process, so a run seeded in one test would otherwise appear in every
// later page render and turn page-shape assertions into ordering luck.
#[cfg(test)]
thread_local! {
    static SHOW_RUNS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub fn show_runs_for_test() {
    SHOW_RUNS.with(|cell| cell.set(true));
}

/// The runs a page should draw, newest first, from the daemon's registry.
#[must_use]
pub fn rows() -> Vec<Row> {
    #[cfg(test)]
    if !SHOW_RUNS.with(std::cell::Cell::get) {
        return Vec::new();
    }
    crate::harness_runs::snapshot()
        .into_iter()
        .map(|run| {
            let item = run.list_item();
            Row {
                id: run.id.clone(),
                harness: run.harness.clone(),
                status: run.status.as_str().to_owned(),
                task: run.task.clone(),
                started_at: run.started_at.clone(),
                finished_at: run.finished_at.clone(),
                elapsed_ms: item["elapsed_ms"].as_i64().unwrap_or(0),
                silent_ms: item["silent_ms"].as_i64().unwrap_or(0),
                exit_code: run.exit_code,
                stalled: run.status == crate::harness_runs::RunStatus::Stalled,
            }
        })
        .collect()
}

/// The status spelling the bar draws, and therefore also its class name.
///
/// The contract fixes the six values. Anything else is a database or daemon
/// that disagrees with it, and "unknown" is the honest thing to draw: a hostile
/// or malformed status never becomes a class name in the page.
#[must_use]
pub fn status(value: &str) -> &'static str {
    match value {
        "running" => "running",
        "succeeded" => "succeeded",
        "failed" => "failed",
        "timed_out" => "timed_out",
        "killed" => "killed",
        "stalled" => "stalled",
        _ => "unknown",
    }
}

/// Whether a run is still worth watching.
///
/// `stalled` is a live state, not a settled one: the watchdog has stopped
/// trusting the run, but the process may still be there, and the operator's
/// next act is to look at it rather than to read a result.
#[must_use]
pub fn live(row: &Row) -> bool {
    row.stalled || row.status == "running" || row.status == "stalled"
}

/// How many runs are in the `running` state.
///
/// The summary badge counts work in flight, not history: a bar that said four
/// because four runs finished this morning would be a log, and the list under
/// it is the log.
#[must_use]
pub fn running_count(rows: &[Row]) -> usize {
    rows.iter().filter(|row| row.status == "running").count()
}

/// The watchdog's answer, in words a person reads without decoding a field.
///
/// A stalled run says `stalled` rather than a silence count, because the count
/// is what the watchdog already decided about. A live run says how long it has
/// been quiet. A settled run has no watchdog state at all.
#[must_use]
pub fn watchdog(row: &Row) -> String {
    if row.stalled || row.status == "stalled" {
        return "stalled".to_owned();
    }
    if row.status == "running" {
        return format!("silent {}", elapsed_label(row.silent_ms));
    }
    "settled".to_owned()
}

/// Format a millisecond count the way the bar reads it.
///
/// Seconds under a minute, then minutes and padded seconds, matching the
/// subagents control so two durations in one row do not use two grammars. A
/// negative count is a clock that moved, and it reads as zero.
#[must_use]
pub fn elapsed_label(milliseconds: i64) -> String {
    let seconds = milliseconds.max(0) / 1000;
    if seconds < 60 {
        return format!("{seconds}s");
    }
    format!("{}m {:02}s", seconds / 60, seconds % 60)
}

/// The task, flattened to the one line the row has room for.
///
/// Whitespace becomes one space, because a task written as a paragraph would
/// otherwise stretch the popover or disappear behind its own newlines. The cut
/// is counted in characters, not bytes, and the ellipsis says text was cut
/// rather than that the task ended there.
#[must_use]
pub fn task_preview(task: &str) -> String {
    const MAX: usize = 80;
    let flat = task.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX {
        return flat;
    }
    let mut cut: String = flat.chars().take(MAX).collect();
    cut.push_str("...");
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(status: &str) -> Row {
        Row {
            id: "run-1".to_owned(),
            harness: "dsh".to_owned(),
            status: status.to_owned(),
            task: "fix the build".to_owned(),
            started_at: "2026-10-19T12:00:00Z".to_owned(),
            finished_at: None,
            elapsed_ms: 65_000,
            silent_ms: 130_000,
            exit_code: None,
            stalled: false,
        }
    }

    #[test]
    fn a_live_run_reads_its_silence_and_a_stalled_run_says_so() {
        assert_eq!(watchdog(&row("running")), "silent 2m 10s");
        let mut stalled = row("running");
        stalled.stalled = true;
        assert_eq!(watchdog(&stalled), "stalled");
        assert_eq!(watchdog(&row("succeeded")), "settled");
    }

    #[test]
    fn elapsed_reads_as_seconds_then_minutes() {
        assert_eq!(elapsed_label(0), "0s");
        assert_eq!(elapsed_label(59_999), "59s");
        assert_eq!(elapsed_label(60_000), "1m 00s");
        assert_eq!(elapsed_label(-5), "0s");
        assert_eq!(elapsed_label(4_925_000), "82m 05s");
    }

    #[test]
    fn the_preview_is_one_line_and_says_when_it_was_cut() {
        assert_eq!(task_preview("first\nsecond\tthird"), "first second third");
        let long = task_preview(&"word ".repeat(40));
        assert_eq!(long.chars().count(), 83, "{long}");
        assert!(long.ends_with("..."), "{long}");
        assert_eq!(task_preview("  "), "");
    }

    #[test]
    fn only_the_contract_statuses_become_class_names() {
        for known in [
            "running",
            "succeeded",
            "failed",
            "timed_out",
            "killed",
            "stalled",
        ] {
            assert_eq!(status(known), known);
        }
        assert_eq!(status("running\" onclick=\"x"), "unknown");
        assert_eq!(status("queued"), "unknown");
    }
}
