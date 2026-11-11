//! The cadence gate: which interval intents are due, and which are held back.
//!
//! This module answers one question and stops. It does not insert a fire row and
//! it does not start a turn. The caller does both, in the same transaction as the
//! turn it starts, because a fire recorded without its turn claims work that never
//! happened and a turn started without its fire is invisible to the next tick.
//!
//! The two gates are ordered, and they mean different things:
//!
//! - **Cooldown owns cadence.** Any fire resets the clock, suppressed ones
//!   included, because a suppressed fire is still the moment the intent was
//!   evaluated and the answer was "not yet".
//! - **Budget owns volume.** Only fires that actually started a turn are counted,
//!   so an intent the cooldown keeps refusing does not spend the window it is
//!   being refused in. Without that split, an intent polled once a minute spends
//!   its whole daily budget on the ticks that refused it and never fires at all.
//!
//! Manual intents never reach this crate's decision at all: [`crate::mem::interval_intents`]
//! excludes them, and a fixture here pins that so the exclusion cannot drift.

use time::Duration;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::db::{Db, DbError};
use crate::learning::parse_timestamp;
use crate::mem;

/// The reason a fire was held back by the cooldown.
pub const COOLDOWN: &str = "cooldown";

/// The reason a fire was held back by the budget.
pub const BUDGET: &str = "budget";

/// The reason a fire was held back because the caller's clock would not parse.
pub const CLOCK: &str = "clock";

/// One intent this tick is willing to fire, or is holding back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Due {
    /// Which intent.
    pub intent_id: String,
    /// The prompt body a fire turns into a turn.
    pub body: String,
    /// Whether the gates held it back.
    pub suppressed: bool,
    /// Why it was held back: `cooldown`, `budget` or `clock`. `None` when due.
    pub reason: Option<String>,
}

/// Every interval intent, in creation order, marked due or suppressed.
///
/// The caller writes the fire row for both outcomes. A suppressed row is what
/// makes the next tick wait instead of asking again, which is why this function
/// refusing to write it would leave the cadence gate with nothing to read.
///
/// # Errors
///
/// A database failure.
pub fn due(db: &Db, now: &str) -> Result<Vec<Due>, DbError> {
    let intents = mem::interval_intents(db)?;

    // The caller's stamp is what every bound is derived from. Without it neither
    // gate can be evaluated, and answering "due" would bypass both at once, so
    // an unreadable stamp suppresses instead of firing.
    let Some(current) = parse_timestamp(now) else {
        return Ok(intents
            .into_iter()
            .map(|intent| Due {
                intent_id: intent.id,
                body: intent.body,
                suppressed: true,
                reason: Some(CLOCK.to_owned()),
            })
            .collect());
    };

    let mut out = Vec::with_capacity(intents.len());
    for intent in intents {
        if let Some(reason) = gate(db, &intent, current)? {
            out.push(Due {
                intent_id: intent.id,
                body: intent.body,
                suppressed: true,
                reason: Some(reason.to_owned()),
            });
            continue;
        }
        out.push(Due {
            intent_id: intent.id,
            body: intent.body,
            suppressed: false,
            reason: None,
        });
    }
    Ok(out)
}

/// Which gate holds this intent back, if any.
fn gate(
    db: &Db,
    intent: &mem::Intent,
    current: OffsetDateTime,
) -> Result<Option<&'static str>, DbError> {
    if let Some(stamp) = mem::last_fire_at(db, &intent.id)? {
        // A stamp that will not parse is read as "never fired" rather than as an
        // error. A hand-edited row must not be able to stop every intent in the
        // table, and the worst case here is one early fire rather than silence.
        if let Some(last) = parse_timestamp(&stamp)
            && current - last < Duration::seconds(intent.cooldown_secs)
        {
            return Ok(Some(COOLDOWN));
        }
    }

    let since = stamp(current - Duration::seconds(intent.window_secs));
    if mem::unsuppressed_fires_since(db, &intent.id, &since)? >= intent.budget_per_window {
        return Ok(Some(BUDGET));
    }

    Ok(None)
}

/// Format a stamp the way [`crate::mem::now`] formats one.
///
/// SQLite compares these columns as text, so a window bound has to carry the
/// same spelling as the rows it is compared against or it sorts against the
/// wrong set.
fn stamp(at: OffsetDateTime) -> String {
    at.format(&Rfc3339)
        .map(|text| text.replace("+00:00", "Z"))
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("in-memory database")
    }

    /// The tests drive every bound from one fixed stamp, so the assertions are
    /// about the gate and never about how long the test took to run.
    const NOW: &str = "2026-01-01T12:00:00Z";

    fn at(seconds_ago: i64) -> String {
        let current = parse_timestamp(NOW).expect("fixed stamp");
        stamp(current - Duration::seconds(seconds_ago))
    }

    /// Intents have no writer in `mem`, so a fixture that needs one writes the
    /// row it wants rather than growing the table module for test-shaped code.
    fn intent(db: &Db, name: &str, kind: &str, cooldown: i64, budget: i64, window: i64) -> String {
        let id = format!("intent-{name}");
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO mem_intents
                     (id, name, body, trigger_kind, cooldown_secs, budget_per_window,
                      window_secs, enabled, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8)",
                rusqlite::params![
                    id,
                    name,
                    format!("look at {name}"),
                    kind,
                    cooldown,
                    budget,
                    window,
                    at(86_400),
                ],
            )?;
            Ok(())
        })
        .expect("insert intent");
        id
    }

    fn fire(db: &Db, intent_id: &str, seconds_ago: i64, suppressed: bool) {
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO mem_intents_fires (id, intent_id, fired_at, suppressed, reason, turn_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, NULL)",
                rusqlite::params![
                    format!("fire-{intent_id}-{seconds_ago}"),
                    intent_id,
                    at(seconds_ago),
                    i64::from(suppressed),
                    if suppressed { Some(COOLDOWN) } else { None },
                ],
            )?;
            Ok(())
        })
        .expect("insert fire");
    }

    /// The gate is open once the cooldown has run out, and closed while it has
    /// not. The fire that closed it does not have to be an old one: any row in
    /// the table is enough, which is the whole point of counting suppressed ones.
    #[test]
    fn a_cooled_down_intent_is_due() {
        let db = db();
        let intent_id = intent(&db, "tidy", "interval", 300, 3, 86_400);

        let due_now = due(&db, NOW).expect("due");
        assert_eq!(due_now.len(), 1, "{due_now:?}");
        assert!(!due_now[0].suppressed);
        assert_eq!(due_now[0].reason, None);
        assert_eq!(due_now[0].body, "look at tidy");

        // One minute into a five minute cooldown: held back, and the reason says
        // which gate did it.
        fire(&db, &intent_id, 60, false);
        let held = due(&db, NOW).expect("due");
        assert!(held[0].suppressed, "{held:?}");
        assert_eq!(held[0].reason.as_deref(), Some(COOLDOWN));

        // Ten minutes on, the same row is spent and the intent is due again.
        let later = at(-600);
        let due_now = due(&db, &later).expect("due");
        assert!(!due_now[0].suppressed, "{due_now:?}");
        assert_eq!(due_now[0].reason, None);
    }

    /// A fire the cooldown refused delays the next attempt and nothing else. If
    /// it also counted against the budget, an intent polled more often than its
    /// window allows would be permanently suppressed by its own refusals.
    #[test]
    fn a_suppressed_fire_delays_but_does_not_burn_budget() {
        let db = db();
        // A budget of one, so a single counted fire is enough to close the gate
        // and any result other than "due" proves the suppressed row counted.
        let intent_id = intent(&db, "tidy", "interval", 300, 1, 86_400);
        fire(&db, &intent_id, 3_600, true);

        let due_now = due(&db, NOW).expect("due");
        assert_eq!(due_now.len(), 1, "{due_now:?}");
        assert!(
            !due_now[0].suppressed,
            "the suppressed fire must not have spent the window: {due_now:?}"
        );
        assert_eq!(due_now[0].reason, None);

        // The other half of the split: an unsuppressed fire does count, so the
        // budget gate is real rather than unreachable.
        fire(&db, &intent_id, 3_000, false);
        let held = due(&db, NOW).expect("due");
        assert_eq!(held[0].reason.as_deref(), Some(BUDGET), "{held:?}");
    }

    /// An intent that fires only when the operator asks is not a timer
    /// candidate. The exclusion lives in `mem::interval_intents`, and this pins
    /// it so a later filter change is a failing test rather than a turn nobody
    /// asked for.
    #[test]
    fn a_manual_intent_never_fires_from_due() {
        let db = db();
        let manual = intent(&db, "on-request", "manual", 300, 3, 86_400);

        let candidates = mem::interval_intents(&db).expect("candidates");
        assert!(
            candidates.is_empty(),
            "a manual intent must not be a timer candidate: {candidates:?}"
        );

        let due_now = due(&db, NOW).expect("due");
        assert!(
            due_now.is_empty(),
            "due() returned a manual intent: {due_now:?}"
        );
        assert!(
            !due_now.iter().any(|row| row.intent_id == manual),
            "due() returned the manual intent"
        );
    }
}
