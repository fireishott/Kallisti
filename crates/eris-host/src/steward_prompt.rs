//! The `<watch_round>` prompt a steward resume turn starts with.
//!
//! The watch loop renders this for each due duty. The text is fixed apart from
//! the duty's own fields and its field notes, which are bounded by count (the
//! `steward.note_window` setting, applied by the caller) and by
//! [`NOTE_BYTE_CAP`] here.

use crate::steward::{Duty, FieldNote};

/// The most bytes the notes block may hold.
pub const NOTE_BYTE_CAP: usize = 4096;

/// What the notes block says when a duty has no field notes yet.
pub const NO_NOTES: &str = "(no field notes yet)";

/// Render one `<watch_round>` prompt.
///
/// `notes` is newest first, as [`crate::steward::list_notes`] returns it. The
/// block keeps that order, stopping once the byte cap is reached.
#[must_use]
pub fn watch_round(duty: &Duty, round: i64, max_rounds: u32, notes: &[FieldNote]) -> String {
    format!(
        "<watch_round>\n\
         Duty: {id} - {statement} (phase: {phase}, round {round}/{max_rounds})\n\
         Field notes (most recent, bounded):\n\
         {notes}\n\
         Resume the duty. Re-read durable state and the workspace instead of trusting earlier narration. If nothing changed and nothing is due, say so and end the round. If the next step is a human decision, raise one call and set the duty to needs_you. Arm the next wake before you finish.\n\
         </watch_round>",
        id = duty.id,
        statement = duty.statement,
        phase = duty.phase,
        notes = render_notes(notes),
    )
}

/// The notes block: one `- <at> <text>` line per note, newest first, within
/// [`NOTE_BYTE_CAP`] bytes. A note that does not fit is cut at a character
/// boundary and the block ends there.
#[must_use]
pub fn render_notes(notes: &[FieldNote]) -> String {
    let mut block = String::new();
    for note in notes {
        let line = format!("- {} {}", note.at, note.text);
        let separator = usize::from(!block.is_empty());
        let room = NOTE_BYTE_CAP.saturating_sub(block.len() + separator);
        if room == 0 {
            break;
        }
        if separator == 1 {
            block.push('\n');
        }
        if line.len() <= room {
            block.push_str(&line);
        } else {
            block.push_str(cut_at_char(&line, room));
            break;
        }
    }
    if block.is_empty() {
        NO_NOTES.to_owned()
    } else {
        block
    }
}

/// The longest prefix of `text` no longer than `budget` bytes that ends on a
/// character boundary.
fn cut_at_char(text: &str, budget: usize) -> &str {
    if text.len() <= budget {
        return text;
    }
    let mut end = budget;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::steward::Phase;

    fn duty() -> Duty {
        Duty {
            id: "duty:watch-backup-1".to_owned(),
            revision: 4,
            created_at: "2026-10-01T00:00:00Z".to_owned(),
            updated_at: "2026-10-01T00:00:00Z".to_owned(),
            statement: "Watch the backup job".to_owned(),
            phase: Phase::Watching,
            every_seconds: Some(90),
            next_wake_at: None,
            blocker: None,
            closed_outcome: None,
            source_conversation: None,
            watch_rounds: 2,
        }
    }

    fn note(at: &str, text: &str) -> FieldNote {
        FieldNote {
            id: format!("note:{at}"),
            duty_id: Some("duty:watch-backup-1".to_owned()),
            at: at.to_owned(),
            text: text.to_owned(),
            tags: Vec::new(),
        }
    }

    #[test]
    fn the_round_prompt_matches_the_template_exactly() {
        let notes = [note("2026-10-02T10:00:00Z", "Backup ran clean.")];
        let text = watch_round(&duty(), 3, 24, &notes);
        let expected = "<watch_round>\n\
Duty: duty:watch-backup-1 - Watch the backup job (phase: watching, round 3/24)\n\
Field notes (most recent, bounded):\n\
- 2026-10-02T10:00:00Z Backup ran clean.\n\
Resume the duty. Re-read durable state and the workspace instead of trusting earlier narration. If nothing changed and nothing is due, say so and end the round. If the next step is a human decision, raise one call and set the duty to needs_you. Arm the next wake before you finish.\n\
</watch_round>";
        assert_eq!(text, expected);
    }

    #[test]
    fn no_notes_says_so() {
        let text = watch_round(&duty(), 1, 24, &[]);
        assert!(
            text.contains(&format!("\n{NO_NOTES}\nResume the duty.")),
            "{text}"
        );
    }

    #[test]
    fn notes_are_capped_at_4096_bytes_newest_first() {
        let big = "x".repeat(1000);
        let notes: Vec<FieldNote> = (0..10).map(|i| note(&format!("t{i}"), &big)).collect();
        let block = render_notes(&notes);
        assert!(block.len() <= NOTE_BYTE_CAP, "{}", block.len());
        assert!(block.starts_with("- t0 "), "newest first: {}", &block[..12]);
        assert!(block.len() > NOTE_BYTE_CAP - 1100, "fills the cap");
    }

    #[test]
    fn a_cut_note_stays_on_a_character_boundary() {
        let text = "é".repeat(3000);
        let block = render_notes(&[note("t", &text)]);
        assert!(block.len() <= NOTE_BYTE_CAP);
        assert!(block.is_char_boundary(block.len()));
    }
}
