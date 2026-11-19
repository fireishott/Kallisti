//! Clause streaming and barge-in.
//!
//! A spoken answer is not one file. It is a run of clauses sent one at a time so
//! the first words are audible while the rest is still being written, and a
//! person can interrupt at any clause boundary and be heard immediately.
//!
//! Nothing here knows about a transport. [`split`] turns text into speakable
//! clauses, [`SpeechQueue`] hands them out one at a time and records how far the
//! voice had got when it was interrupted, so the host can tell the model what the
//! person actually heard before they cut in.

/// A clause ends here. Semicolons and colons count because a spoken list is
/// unlistenable when every item waits for the full stop.
const BREAK: [char; 5] = ['.', '!', '?', ';', ':'];

/// Short fragments read better merged into the clause that follows.
const MIN_CHARS: usize = 24;

/// A clause longer than this is cut on a space rather than spoken in one breath.
const HARD_CAP: usize = 240;

/// Speakable clauses, in order, each small enough to start fast.
///
/// Whitespace is normalised first, so a caller may pass a paragraph with newlines
/// in it. An empty or whitespace-only input yields no clauses.
#[must_use]
pub fn split(text: &str) -> Vec<String> {
    let clean: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if clean.is_empty() {
        return Vec::new();
    }

    let mut out: Vec<String> = Vec::new();
    let mut buffer = String::new();
    for piece in pieces(&clean) {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        buffer = format!("{buffer} {piece}").trim().to_owned();
        let long_enough = buffer.chars().count() >= MIN_CHARS;
        let closes = piece.ends_with(['.', '?', '!']);
        if long_enough || closes {
            out.push(std::mem::take(&mut buffer));
        }
    }
    if !buffer.is_empty() {
        out.push(buffer);
    }

    let mut sized: Vec<String> = Vec::new();
    for clause in out {
        let mut rest: Vec<char> = clause.chars().collect();
        while rest.len() > HARD_CAP {
            let window = &rest[..HARD_CAP];
            let cut = window
                .iter()
                .rposition(|c| c.is_whitespace())
                .filter(|&at| at > 0)
                .unwrap_or(HARD_CAP);
            let piece: String = rest[..cut].iter().collect();
            let piece = piece.trim().to_owned();
            if !piece.is_empty() {
                sized.push(piece);
            }
            rest = rest[cut..].to_vec();
            while rest.first().is_some_and(|c| c.is_whitespace()) {
                rest.remove(0);
            }
        }
        let tail: String = rest.iter().collect();
        let tail = tail.trim().to_owned();
        if !tail.is_empty() {
            sized.push(tail);
        }
    }
    sized
}

/// The raw pieces a clause break leaves behind, punctuation kept.
fn pieces(clean: &str) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let mut start = 0usize;
    let mut chars = clean.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let Some(&(next_at, next)) = chars.peek() else {
            break;
        };
        let boundary = if BREAK.contains(&c) {
            next.is_whitespace()
        } else if c == ',' {
            // A comma only ends a clause when a capital follows: "wait, then" is
            // one thought, "Wait, Then" is two.
            next.is_whitespace()
                && clean[next_at..]
                    .trim_start()
                    .chars()
                    .next()
                    .is_some_and(char::is_uppercase)
        } else {
            false
        };
        if boundary {
            out.push(&clean[start..=at]);
            start = at + c.len_utf8();
        }
    }
    if start < clean.len() {
        out.push(&clean[start..]);
    }
    out
}

/// Clauses waiting to be spoken, with the interruption cursor.
#[derive(Debug, Clone, Default)]
pub struct SpeechQueue {
    clauses: Vec<String>,
    spoken: usize,
    interrupted: bool,
}

impl SpeechQueue {
    /// A queue holding the clauses of `text`.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        Self {
            clauses: split(text),
            spoken: 0,
            interrupted: false,
        }
    }

    /// The clauses, in speaking order.
    #[must_use]
    pub fn clauses(&self) -> &[String] {
        &self.clauses
    }

    /// The next clause, or `None` when the answer is finished or was cut off.
    ///
    /// Named `next` because the contract callers use that name; this is not an
    /// iterator and does not implement [`Iterator`].
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<String> {
        if self.interrupted || self.spoken >= self.clauses.len() {
            return None;
        }
        let clause = self.clauses[self.spoken].clone();
        self.spoken += 1;
        Some(clause)
    }

    /// Stop speaking. Everything already handed out counts as heard.
    pub fn barge_in(&mut self) {
        self.interrupted = true;
    }

    /// What the person actually listened to, for the model's context.
    #[must_use]
    pub fn heard(&self) -> String {
        let end = self.spoken.min(self.clauses.len());
        self.clauses[..end].join(" ").trim().to_owned()
    }

    /// What the interruption cut off.
    #[must_use]
    pub fn unheard(&self) -> String {
        let start = self.spoken.min(self.clauses.len());
        self.clauses[start..].join(" ").trim().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_three_clause_answer_stays_three_clauses() {
        let text = "The first clause is long enough to stand alone. \
                    The second clause is also long enough. \
                    And the third one closes it out.";
        let clauses = split(text);
        assert_eq!(clauses.len(), 3, "{clauses:?}");
        assert!(clauses[0].ends_with("alone."));
        assert!(clauses[2].ends_with("out."));
    }

    #[test]
    fn a_short_fragment_merges_into_the_next_clause() {
        let clauses = split("Well, Then go");
        assert_eq!(clauses, vec!["Well, Then go".to_owned()], "{clauses:?}");
    }

    #[test]
    fn a_semicolon_and_a_colon_end_a_clause() {
        let text = "One thing is true and it is worth saying out loud; \
                    another follows right after that point: \
                    and here is the rest of it.";
        let clauses = split(text);
        assert_eq!(clauses.len(), 3, "{clauses:?}");
        assert!(clauses[0].ends_with(';'), "{clauses:?}");
        assert!(clauses[1].ends_with(':'), "{clauses:?}");
    }

    #[test]
    fn barge_in_before_the_first_clause_yields_nothing() {
        let mut queue = SpeechQueue::from_text("Something long enough to be one clause.");
        queue.barge_in();
        assert_eq!(queue.next(), None);
        assert_eq!(queue.heard(), "");
        assert!(!queue.unheard().is_empty());
    }

    #[test]
    fn heard_plus_unheard_reconstructs_the_input() {
        let text = "One clause that is long enough. A second clause follows it. And a third.";
        let mut queue = SpeechQueue::from_text(text);
        assert!(queue.next().is_some());
        queue.barge_in();
        let joined = format!("{}{}", queue.heard(), queue.unheard());
        let strip =
            |value: &str| -> String { value.chars().filter(|c| !c.is_whitespace()).collect() };
        assert_eq!(strip(&joined), strip(text));
    }

    #[test]
    fn a_clause_past_the_cap_is_cut_on_a_space() {
        let long = "word ".repeat(60) + "end.";
        let clauses = split(&long);
        assert!(clauses.len() > 1, "{clauses:?}");
        for clause in &clauses {
            assert!(clause.chars().count() <= HARD_CAP, "{clause}");
        }
        let strip =
            |value: &str| -> String { value.chars().filter(|c| !c.is_whitespace()).collect() };
        assert_eq!(
            strip(&clauses.concat()),
            strip(&long),
            "cutting must not drop characters"
        );
    }

    #[test]
    fn an_empty_answer_has_no_clauses() {
        assert!(split("   \n ").is_empty());
        assert_eq!(SpeechQueue::from_text("").next(), None);
    }
}
