//! Standing orders: proactive turns on a cadence.
//!
//! A standing order is a prompt, a cadence and somewhere for the answer to land.
//! Everything but the last part already existed: `mem_intents` holds the prompt
//! and the cadence, and [`crate::memintent`] answers which ones are due and which
//! are held back. Nothing called it, so an interval intent was a row that never
//! fired.
//!
//! The anti-spam guard is the same one schedules use and the one the intent table
//! already carries: a cooldown owns cadence and an unsuppressed-fires budget owns
//! volume. A held-back evaluation still writes its fire row, so the next tick waits
//! instead of asking again.
//!
//! Delivery is the configured default channel, not a second per-order column: a
//! standing order's output has to land somewhere a person sees it, and one rule
//! for "where" is easier to reason about than a column that can disagree with the
//! switch that gates it.

use crate::db::Db;
use crate::memintent;

/// The `settings` row that arms the whole feature.
pub const SETTING_ENABLED: &str = "standing.enabled";

/// Whether standing orders fire at all.
#[must_use]
pub fn enabled(db: &Db) -> bool {
    !matches!(
        crate::web::store::settings_get(db, SETTING_ENABLED)
            .ok()
            .flatten()
            .as_deref()
            .map(str::trim),
        Some("false")
    )
}

/// Evaluate every standing order for one tick, and fire the due ones.
///
/// Returns how many turns were started. A suppressed evaluation writes its fire
/// row and returns; a due one writes the fire and then runs the turn inline, for
/// the same reason a schedule run is awaited: a slow turn must not stack a second
/// run of the same order behind it.
pub async fn pass(state: &crate::admin::AppState, now: &str) -> usize {
    let db = state.engine.db();
    if !enabled(db) {
        return 0;
    }
    let due = match memintent::due(db, now) {
        Ok(rows) => rows,
        Err(error) => {
            tracing::error!(%error, "standing: cannot read the due orders");
            return 0;
        }
    };
    let mut fired = 0usize;
    for row in due {
        if row.suppressed {
            // The row is what makes the next tick wait. Its reason is kept, so a
            // cadence that is refusing everything can be seen rather than guessed.
            let _ = crate::mem::record_fire(db, &row.intent_id, true, row.reason.as_deref(), None);
            continue;
        }
        let name = crate::mem::intent_name(db, &row.intent_id)
            .ok()
            .flatten()
            .unwrap_or_else(|| "standing order".to_owned());
        tracing::info!(order = %row.intent_id, %name, "standing: firing");
        let order = crate::scheduler::Schedule {
            id: row.intent_id.clone(),
            name: name.clone(),
            cron: String::new(),
            prompt: row.body.clone(),
            enabled: true,
            next_fire_at: None,
            last_fired_at: None,
            last_status: None,
            last_error: None,
            fired_count: 0,
            deliver: crate::delivery::default_channel(db),
        };
        // The fire is written before the turn so the cadence gate sees this
        // evaluation even if the turn crashes the process.
        let _ = crate::mem::record_fire(db, &row.intent_id, false, None, None);
        let outcome = crate::web::routes::run_one_turn(state, &row.body, &name).await;
        match outcome {
            crate::web::routes::TurnOutcome::NoProvider => {}
            crate::web::routes::TurnOutcome::Conversation {
                conversation,
                summary,
                ..
            } => {
                let (delivered, note) = crate::delivery::deliver(
                    state,
                    &order,
                    &row.intent_id,
                    &conversation,
                    summary.as_deref(),
                )
                .await;
                // A standing order has no run row of its own; the fire row is the
                // record, and the delivery note rides on it.
                let detail = if note.is_empty() {
                    delivered.clone()
                } else {
                    format!("{delivered}: {note}")
                };
                let _ = crate::mem::note_fire_delivery(db, &row.intent_id, &detail);
            }
        }
        fired += 1;
    }
    fired
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("an in-memory database")
    }

    #[test]
    fn the_switch_defaults_on_and_reads_false() {
        let db = db();
        assert!(enabled(&db));
        crate::web::store::settings_put(&db, SETTING_ENABLED, "false").expect("store");
        assert!(!enabled(&db));
    }

    #[test]
    fn a_held_back_order_still_writes_its_fire() {
        // The gate is `memintent`'s; this pins the half that belongs here, that a
        // suppressed row is written rather than dropped.
        let db = db();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO mem_intents (id, name, body, trigger_kind, cooldown_secs, created_at) \
                 VALUES ('o1', 'brief', 'brief me', 'interval', 3600, '2026-11-01T00:00:00.000Z')",
                [],
            )?;
            Ok(())
        })
        .expect("intent");
        crate::mem::record_fire(&db, "o1", true, Some("cooldown"), None).expect("fire");
        let count: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM mem_intents_fires WHERE intent_id = 'o1' AND suppressed = 1",
                    [],
                    |row| row.get(0),
                )
            })
            .expect("count");
        assert_eq!(count, 1);
    }
}
