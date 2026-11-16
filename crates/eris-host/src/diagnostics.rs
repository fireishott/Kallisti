//! A local ring buffer of what the daemon has been doing lately.
//!
//! Local first: the buffer lives in this process, is bounded, and never leaves the
//! host on its own. A surface asks for it when the operator asks, which is the
//! "one-tap bundle" in the brief - there is no remote upload path, because a
//! diagnostic stream that reports home is a different product.
//!
//! What goes in is deliberately small: a kind, a sentence, and when. Timings
//! matter more than prose, so a caller records a duration when it has one.

use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};

/// How many entries are kept. Old entries fall off silently; the buffer is for
/// "what just happened", not for history.
pub const CAPACITY: usize = 200;

/// A turn that took longer than this is worth a note, which is the watermark the
/// brief asks for.
pub const SLOW_TURN_MS: i64 = 30_000;

/// One recorded event.
#[derive(Debug, Clone)]
pub struct Entry {
    /// A short kind, for example `turn.slow` or `boot`.
    pub kind: String,
    /// A sentence a person can read.
    pub detail: String,
    /// When it happened, RFC 3339.
    pub at: String,
    /// A duration in milliseconds, when the event is about one.
    pub duration_ms: Option<i64>,
}

fn buffer() -> &'static Mutex<VecDeque<Entry>> {
    static BUFFER: OnceLock<Mutex<VecDeque<Entry>>> = OnceLock::new();
    BUFFER.get_or_init(|| Mutex::new(VecDeque::with_capacity(CAPACITY)))
}

/// Record one event, dropping the oldest when the buffer is full.
pub fn record(kind: &str, detail: &str, duration_ms: Option<i64>) {
    let entry = Entry {
        kind: kind.to_owned(),
        detail: detail.to_owned(),
        at: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default(),
        duration_ms,
    };
    if let Ok(mut buffer) = buffer().lock() {
        if buffer.len() == CAPACITY {
            buffer.pop_front();
        }
        buffer.push_back(entry);
    }
}

/// Record a turn's duration, and note it by name when it crossed the watermark.
pub fn record_turn(duration_ms: i64, turn_id: &str) {
    if duration_ms >= SLOW_TURN_MS {
        record(
            "turn.slow",
            &format!("turn {turn_id} took {duration_ms}ms, over the {SLOW_TURN_MS}ms watermark"),
            Some(duration_ms),
        );
    }
}

/// The buffer, oldest first.
#[must_use]
pub fn snapshot(limit: usize) -> Vec<Entry> {
    let Ok(buffer) = buffer().lock() else {
        return Vec::new();
    };
    let take = limit.min(buffer.len());
    buffer
        .iter()
        .skip(buffer.len() - take)
        .cloned()
        .collect()
}

/// The bundle a surface fetches in one tap: the entries, the field names, and the
/// host facts that make them readable without a second call.
#[must_use]
pub fn bundle(limit: usize) -> Value {
    let entries: Vec<Value> = snapshot(limit)
        .into_iter()
        .map(|entry| {
            json!({
                "kind": entry.kind,
                "detail": entry.detail,
                "at": entry.at,
                "duration_ms": entry.duration_ms,
            })
        })
        .collect();
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "capacity": CAPACITY,
        "slow_turn_ms": SLOW_TURN_MS,
        "entries": entries,
    })
}

#[cfg(test)]
mod tests {
    use super::{CAPACITY, bundle, record, snapshot};

    /// The buffer is bounded and keeps the newest: an operator asking for
    /// diagnostics wants what just happened, not the first two hundred things.
    #[test]
    fn the_buffer_is_bounded_and_keeps_the_newest() {
        for index in 0..(CAPACITY + 5) {
            record("test.probe", &format!("entry {index}"), None);
        }
        let entries = snapshot(CAPACITY + 50);
        assert_eq!(entries.len(), CAPACITY, "the buffer never exceeds its capacity");
        assert!(entries.last().expect("last").detail.ends_with("204"), "newest kept");
    }

    /// A slow turn is recorded by name with its duration; a fast one is not
    /// recorded at all.
    #[test]
    fn only_a_slow_turn_is_written_down() {
        super::record_turn(super::SLOW_TURN_MS + 1, "turn-slow");
        super::record_turn(10, "turn-fast");
        let entries = snapshot(50);
        let slow: Vec<_> = entries.iter().filter(|entry| entry.kind == "turn.slow").collect();
        assert!(slow.iter().any(|entry| entry.detail.contains("turn-slow")));
        assert!(!slow.iter().any(|entry| entry.detail.contains("turn-fast")));
    }

    /// The bundle states its own limits, so a reader knows what it is looking at.
    #[test]
    fn the_bundle_carries_the_capacity_and_the_watermark() {
        record("test.bundle", "one", Some(5));
        let bundle = bundle(10);
        assert_eq!(bundle["capacity"], CAPACITY);
        assert_eq!(bundle["slow_turn_ms"], super::SLOW_TURN_MS);
        assert!(!bundle["version"].as_str().unwrap_or_default().is_empty());
    }
}
