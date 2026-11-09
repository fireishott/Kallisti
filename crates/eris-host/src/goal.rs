//! The objective turns keep working toward, and when they should not carry it.
//!
//! A goal is one line of text in `settings` plus a round budget. Every turn from
//! the moment it is set is told what it is for and told not to stop asking
//! whether to continue, which is what "work through to completion" needs from a
//! single turn: the loop is the objective plus the budget, not a background
//! process the operator cannot see.
//!
//! That is also the failure this module exists to prevent. A greeting carries no
//! task, so with a goal set the only actionable instruction left in the prompt is
//! the objective, and the answer to "Sup" becomes a warehouse sweep. Build 149
//! shipped the goal without a gate and build 150 wired it to every turn; the
//! operator got a sixty-round status report per salutation.
//!
//! There are three deliberate pieces here:
//!
//! - [`block`] is the text a goal turn is given. It is one function so the device
//!   lane and the browser lane cannot drift into two different instructions.
//! - [`is_small_talk`] is the gate. It is closed-set and deterministic on purpose:
//!   no model call, no heuristic on message length alone, and no clock. The same
//!   message is a greeting on every turn, so a test can assert it.
//! - [`wants_long_job`] is a separate, budget-only signal. It never changes the
//!   prompt: it decides whether the round raise applies, so an ordinary question
//!   keeps the operator's `tools.max_rounds` and only an explicit long job is
//!   lifted toward `tools.goal_rounds_ceiling`.

/// The `settings` row a goal lives in. One active goal per daemon.
pub const SETTING: &str = "goal.objective";

/// Rounds a long-job turn may spend by default. Deep enough to finish a real job,
/// shallow enough that a runaway objective stops on its own.
///
/// This is the size of the raise, not the budget: an ordinary turn gets the
/// operator's `tools.max_rounds`, and only a turn that asks for a long job is
/// lifted to this, capped by `tools.goal_rounds_ceiling` (which defaults to this
/// value, so the ceiling changes nothing until an operator dials it down).
pub const ROUNDS: usize = 60;

/// The longest objective the settings form accepts.
///
/// The compiled system prompt is capped at 32 KiB and refuses an overage rather
/// than truncating, so a stray paste must be refused here where the operator can
/// still see the number.
pub const MAX_OBJECTIVE_CHARS: usize = 2000;

/// The stored objective, or `None` when there is none to carry.
///
/// An absent row and an empty row are the same answer: a goal somebody cleared is
/// a goal nobody set.
#[must_use]
pub fn stored(db: &crate::db::Db) -> Option<String> {
    let found: String = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT value FROM settings WHERE key = ?1",
                rusqlite::params![SETTING],
                |row| row.get(0),
            )
        })
        .ok()?;
    let objective = found.trim().to_owned();
    (!objective.is_empty()).then_some(objective)
}
/// The objective the turn being sent right now should carry, if any.
///
/// This is the whole gate: a stored goal, and a message that is not small talk.
/// Both lanes call this rather than reading the row themselves, so neither can
/// remember the goal and forget the exemption.
#[must_use]
pub fn for_turn(db: &crate::db::Db, text: &str) -> Option<String> {
    if is_small_talk(text) {
        return None;
    }
    stored(db)
}

/// The instruction appended to a goal turn's system prompt.
///
/// Worded as one paragraph of a prompt rather than as a config dump, because that
/// is what it is: the reason this turn exists. It is appended last, after persona,
/// context and the skill catalog, since a block the model reads first is the one
/// it follows.
#[must_use]
pub fn block(objective: &str) -> String {
    format!(
        "## Objective\n\n{objective}\n\nKeep working toward that objective with the tools \
         available and do not stop to ask whether to continue. When you run out of rounds or \
         the work is done, say what is finished and exactly what is left."
    )
}

/// `system` with the objective appended, or `system` unchanged for a turn that
/// carries no goal.
#[must_use]
pub fn with_objective(system: Option<String>, objective: Option<&str>) -> Option<String> {
    match (system, objective) {
        (Some(system), Some(objective)) if !system.trim().is_empty() => {
            Some(format!("{system}\n\n{}", block(objective)))
        }
        (_, Some(objective)) => Some(block(objective)),
        (system, None) => system,
    }
}

/// Rounds a turn gets: the operator's `budget` for an ordinary turn, or the goal
/// raise for a turn that asked for a long job.
///
/// The operator's `budget` is a ceiling for ordinary turns: nothing here raises
/// it. Only a long-job turn ([`wants_long_job`]) is lifted past it, and only as
/// far as `ceiling`, which is the operator's `tools.goal_rounds_ceiling` and
/// defaults to [`ROUNDS`]. A budget already above the raise is left where it is
/// rather than lowered, so an operator who typed a bigger number keeps it.
#[must_use]
pub fn rounds(budget: usize, raise: bool, ceiling: usize) -> usize {
    if raise {
        budget.max(ROUNDS.min(ceiling))
    } else {
        budget
    }
}

/// The words that can appear in a message that is only a greeting.
///
/// Closed on purpose. A word that is not here makes the message work, which is
/// the safe direction: carrying the goal on a message that did not need it leaves
/// the answer long, and dropping it on a message that did need it leaves the work
/// undone. Only one of those is expensive.
const SMALL_TALK: &[&str] = &[
    // Greetings.
    "hi",
    "hiya",
    "hey",
    "hello",
    "yo",
    "sup",
    "up",
    "howdy",
    "greetings",
    "morning",
    "afternoon",
    "evening",
    "night",
    "gm",
    // Small talk and acknowledgements.
    "good",
    "great",
    "fine",
    "well",
    "how",
    "are",
    "is",
    "it",
    "going",
    "you",
    "there",
    "whats",
    "what",
    "again",
    "thanks",
    "thank",
    "ty",
    "thx",
    "ok",
    "okay",
    "k",
    "cool",
    "nice",
    "awesome",
    "perfect",
    "gotcha",
    "right",
    "yeah",
    "yep",
    "yes",
    "no",
    "nope",
    "alright",
    "lol",
    "haha",
    "ha",
    // Names a greeting is likely to carry, so "morning ignyte" still counts.
    "curt",
    "curtis",
    "ignyte",
    "eris",
    "kallisti",
];

/// The longest a message can be and still be a greeting, in words.
///
/// Four covers "good morning, how are you" and stops well short of a sentence
/// with an instruction in it.
const SMALL_TALK_WORDS: usize = 5;

/// Whether this message only greets.
///
/// Lowercased, stripped to letters and digits, split on anything that is not a
/// word character, then: at most [`SMALL_TALK_WORDS`] words, and every word in
/// [`SMALL_TALK`]. Empty text is not a greeting - there is nothing there to greet
/// with - which leaves the goal in place for the turn that has to answer for it.
///
/// A question mark does not disqualify a message: "how are you?" is still small
/// talk. A word outside the vocabulary does, so "what is the status" carries the
/// goal while "what's up" does not.
#[must_use]
pub fn is_small_talk(text: &str) -> bool {
    // Apostrophes are dropped rather than split on, so "what's up" is the three
    // words a person typed and not four: "what", "s", "up". The straight and the
    // typographic apostrophe are both common on a phone keyboard.
    let lowered = text.to_lowercase().replace(['\'', '\u{2019}'], "");
    let words: Vec<&str> = lowered
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    if words.is_empty() || words.len() > SMALL_TALK_WORDS {
        return false;
    }
    words.iter().all(|word| SMALL_TALK.contains(word))
}

/// The words that mark a message as an explicit request for a long job.
///
/// Closed on purpose, and the default is NO raise. A marker that is absent leaves
/// the turn on the operator's ordinary budget, which is the cheap direction: an
/// ordinary question answered with the budget the operator set is the outcome this
/// list exists to produce, and a long job refused the raise only costs a second
/// ask. Substring matching, so "planning", "researched" and "refactoring" all
/// count without a second spelling each.
const LONG_JOB_MARKERS: &[&str] = &[
    "plan",
    "planning",
    "research",
    "audit",
    "investigate",
    "compare",
    "design",
    "build",
    "migrate",
    "refactor",
    "set up",
    "setup",
    "step by step",
    "take your time",
    "go deep",
    "thorough",
    "in depth",
    "full breakdown",
    "write a plan",
    "figure out",
];

/// Whether this message is an explicit request for a long job.
///
/// This is the second, separate signal for the round raise. It says nothing about
/// whether the turn carries the goal block: [`for_turn`] and [`is_small_talk`]
/// still decide that alone, so the prompt content is exactly what it was before
/// this gate existed. What changes is the budget: an ordinary question keeps
/// `tools.max_rounds`, and only a message that names a long job is lifted toward
/// `tools.goal_rounds_ceiling`.
///
/// Case-insensitive and closed-set, so the same message is the same answer on
/// every turn and a test can assert it. Empty text asks for nothing.
#[must_use]
pub fn wants_long_job(message: &str) -> bool {
    let lowered = message.to_lowercase();
    LONG_JOB_MARKERS
        .iter()
        .any(|marker| lowered.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two messages that started this: a greeting must not carry the goal.
    #[test]
    fn the_reported_greetings_are_small_talk() {
        assert!(is_small_talk("Sup"));
        assert!(is_small_talk("Good morning"));
        assert!(is_small_talk("Good morning, Curtis"));
        assert!(is_small_talk("morning"));
        assert!(is_small_talk("what's up"));
        assert!(is_small_talk("How are you?"));
        assert!(is_small_talk("hey there"));
        assert!(is_small_talk("thanks"));
        assert!(is_small_talk("ok cool"));
    }

    /// Punctuation and case are not the signal; the words are.
    #[test]
    fn shape_does_not_change_the_answer() {
        assert_eq!(is_small_talk("SUP!!!"), is_small_talk("sup"));
        assert_eq!(
            is_small_talk("  good   morning  "),
            is_small_talk("good morning")
        );
        assert!(is_small_talk("Good-morning"));
    }

    /// Anything with a task in it keeps the goal, which is the expensive half to
    /// get wrong.
    #[test]
    fn a_message_with_work_in_it_is_not_small_talk() {
        assert!(!is_small_talk(
            "Good morning. Let me get into the warehouse."
        ));
        assert!(!is_small_talk("what is the status"));
        assert!(!is_small_talk("Sup, did the sweep finish?"));
        assert!(!is_small_talk("Continue on"));
        assert!(!is_small_talk("status"));
        assert!(!is_small_talk("scan the warehouse"));
        assert!(!is_small_talk("good morning, check the tinfoil matchups"));
        assert!(!is_small_talk("/goal stop"));
    }

    /// An empty message is not a greeting: there is nothing to greet with, and a
    /// turn that has to answer for it still has the work to do.
    #[test]
    fn empty_is_not_a_greeting() {
        assert!(!is_small_talk(""));
        assert!(!is_small_talk("   "));
        assert!(!is_small_talk("!!!"));
    }

    /// A long run of greetings is a paragraph, not a salutation.
    #[test]
    fn length_alone_disqualifies() {
        assert!(!is_small_talk("good morning good morning good morning"));
        assert!(is_small_talk("good morning how are you"));
    }

    /// The block is one text for both lanes, and it names the objective.
    #[test]
    fn the_block_carries_the_objective_and_the_instruction() {
        let block = block("inspect the warehouse");
        assert!(block.contains("## Objective"));
        assert!(block.contains("inspect the warehouse"));
        assert!(block.contains("do not stop to ask whether to continue"));
    }

    /// A turn that already has a system prompt keeps it, ahead of the objective.
    #[test]
    fn the_block_is_appended_to_what_is_already_there() {
        let system = with_objective(Some("You are Ignyte.".to_owned()), Some("inspect"));
        let system = system.expect("a prompt");
        assert!(system.starts_with("You are Ignyte."));
        assert!(system.contains("## Objective"));
    }

    /// No goal means the prompt is untouched, byte for byte.
    #[test]
    fn no_goal_changes_nothing() {
        assert_eq!(
            with_objective(Some("You are Ignyte.".to_owned()), None).as_deref(),
            Some("You are Ignyte.")
        );
        assert_eq!(with_objective(None, None), None);
    }

    /// A goal with no persona behind it still reaches the turn rather than being
    /// dropped for having nothing to append to.
    #[test]
    fn a_goal_alone_is_still_a_prompt() {
        let system = with_objective(None, Some("inspect")).expect("a prompt");
        assert!(system.starts_with("## Objective"));
    }

    /// An empty system prompt is not a prefix worth keeping, so the objective is
    /// the whole thing rather than a blank line and a heading.
    #[test]
    fn a_blank_prompt_does_not_leave_leading_whitespace() {
        let system = with_objective(Some("   \n ".to_owned()), Some("inspect")).expect("a prompt");
        assert!(system.starts_with("## Objective"), "{system}");
    }

    /// The budget is raised only for a long-job turn, and only as far as the
    /// operator's ceiling allows.
    #[test]
    fn the_budget_only_moves_up_for_a_long_job() {
        // No raise: the operator's budget is the ceiling for an ordinary turn,
        // whatever it is and whatever the goal ceiling says.
        assert_eq!(rounds(12, false, ROUNDS), 12);
        assert_eq!(rounds(25, false, 120), 25);

        // A raise lands on the default goal budget.
        assert_eq!(rounds(12, true, ROUNDS), ROUNDS);
        assert_eq!(rounds(12, true, 120), ROUNDS);

        // Dialled down, the raise stops at the ceiling instead.
        assert_eq!(rounds(1, true, 25), 25);
        assert_eq!(rounds(1, true, 1), 1);

        // A budget already above the ceiling is not lowered by the raise, and a
        // raise never takes rounds away from anyone.
        assert_eq!(rounds(12, true, 1), 12);
        assert_eq!(rounds(80, true, 30), 80);
        assert_eq!(rounds(ROUNDS + 5, true, ROUNDS), ROUNDS + 5);
    }

    /// A message that names a long job is the only thing that lifts the budget.
    #[test]
    fn a_planning_request_wants_a_long_job_and_a_question_does_not() {
        assert!(wants_long_job("plan the theater night"));
        assert!(wants_long_job("Plan the theater night"));
        assert!(wants_long_job("PLAN the theater night"));

        assert!(!wants_long_job("what time does the garage close"));
        assert!(!wants_long_job("good morning"));
        assert!(!wants_long_job("Sup"));
        assert!(!wants_long_job(""));
    }

    /// The list is closed and substring matched, so the spellings a person types
    /// land on the same answer without a second table entry.
    #[test]
    fn the_long_job_markers_cover_the_requested_words() {
        for message in [
            "do a full audit of the warehouse",
            "research the venue options",
            "compare these two providers",
            "design the schema",
            "build me a plan",
            "migrate the database",
            "refactor the collector",
            "set up the tunnel",
            "walk me through it step by step",
            "take your time",
            "go deep on this",
            "be thorough",
            "an in depth read",
            "give me a full breakdown",
            "write a plan",
            "figure out why it fails",
        ] {
            assert!(wants_long_job(message), "{message} is a long job");
        }
    }
}
