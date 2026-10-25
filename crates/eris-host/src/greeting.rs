//! A generated line under the salutation on an empty conversation.
//!
//! The salutation itself is the time of day, from [`crate::web::templates::greeting_for`],
//! and it needs no clock or model: it is arithmetic on the hour. This module is
//! the *second* line, the one a model writes, and it is off until the operator
//! asks for it, because a page that calls a model on every render spends money
//! on a decoration.
//!
//! Three things are deliberate here:
//!
//! - **The prompt carries no context.** No name, no path, no hostname, no memory,
//!   no transcript. A line that pretends to know the reader is the one failure
//!   mode a greeting cannot afford, so there is nothing to know it from.
//! - **The line is cached.** The text and the second it was written are two rows
//!   in `settings`, and a render only asks for a new one when the stored line is
//!   older than the refresh window.
//! - **The call is bounded.** [`crate::web::chat::TurnConfig`] is the same seam
//!   the title and approval helpers use, and the caller runs it on a blocking
//!   thread with a deadline, so a page never waits on a model with no ceiling.
//!
//! The scrubber is not cosmetic. The rendered page is tested for a bang and for
//! an em dash, so a model that writes either does not get to put one there.

use core_domain::{Capabilities, ModelProfile, ProviderProfile};
use core_runtime::provider::{
    ChatMessage, ChatRequest, ChatRole, OpenAiCompatProvider, ProviderEvent,
};

use crate::db::{Db, DbError};
use crate::web::chat::TurnConfig;
use crate::web::store;

/// Whether the operator asked for a generated line at all.
///
/// Default off. Absent is off, an empty row is off, and anything that is not an
/// explicit yes is off: spending tokens is the thing being asked for here, so a
/// corrupt row must not be able to say yes.
pub const SETTING_GREETING_ENABLED: &str = "greeting.enabled";

/// The model the line is written with. [`INHERIT`] follows the chat model.
/// The line under the salutation, editable by the operator.
///
/// A constant in the template was the wrong home for it: it is the first sentence a
/// person reads on an empty page, and it should be theirs to change.
pub const SETTING_HINT: &str = "chat.hint";

/// What that line says before anyone changes it.
pub const DEFAULT_HINT: &str = "Ask anything. The reply streams in.";

/// The longest accepted hint.
pub const MAX_HINT_CHARS: usize = 140;

/// Read the stored hint, or the default when it is absent or blank.
#[must_use]
pub fn hint(stored: Option<&str>) -> String {
    match stored.map(str::trim) {
        Some(text) if !text.is_empty() => text.chars().take(MAX_HINT_CHARS).collect(),
        _ => DEFAULT_HINT.to_owned(),
    }
}

pub const SETTING_GREETING_MODEL: &str = "greeting.model";

/// How often the line is rewritten: one of [`Refresh`]'s three values.
pub const SETTING_GREETING_REFRESH: &str = "greeting.refresh";

/// The line last written. Absent until a line exists, and never a placeholder.
pub const SETTING_TEXT: &str = "greeting.text";

/// When that line was written, in seconds since the Unix epoch.
pub const SETTING_MADE_AT: &str = "greeting.made_at";

/// The value that means "whatever the chat model is".
///
/// The same sentinel the auxiliary roles use, so the two settings pages read the
/// same way to an operator who has seen either one.
pub const INHERIT: &str = "primary";

/// The longest line this build stores or renders, in characters.
///
/// Ninety is a line, not a paragraph: it has to sit under a 30px salutation
/// without becoming the loud thing on the page.
pub const LIMIT: usize = 90;

/// How often a stored line is allowed to age before it is rewritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresh {
    /// Every hour.
    Hour,
    /// Every day. The default.
    Day,
    /// Every week.
    Week,
}

impl Refresh {
    /// Every value the form may submit, in the order the select lists them.
    pub const ALL: [Self; 3] = [Self::Hour, Self::Day, Self::Week];

    /// What an unwritten row means.
    pub const DEFAULT: Self = Self::Day;

    /// Read a stored or submitted value.
    ///
    /// A closed set, so an unknown or absent value is the default rather than an
    /// error: the only way to get here with something else is a hand-edited
    /// database, and a greeting is not worth refusing a save over.
    #[must_use]
    pub fn parse(stored: Option<&str>) -> Self {
        match stored.map(str::trim) {
            Some("hour") => Self::Hour,
            Some("week") => Self::Week,
            _ => Self::DEFAULT,
        }
    }

    /// The stored form, and the form value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::Day => "day",
            Self::Week => "week",
        }
    }

    /// What the select shows.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Hour => "Every hour",
            Self::Day => "Every day",
            Self::Week => "Every week",
        }
    }

    /// How old a line may be before it is rewritten, in seconds.
    #[must_use]
    pub fn seconds(self) -> i64 {
        match self {
            Self::Hour => 60 * 60,
            Self::Day => 24 * 60 * 60,
            Self::Week => 7 * 24 * 60 * 60,
        }
    }
}

/// Whether the operator has switched the generated line on.
///
/// Only an explicit yes counts. The default is off, so an absent row, an empty
/// row, and every unreadable value is off: the expensive direction is the one
/// that needs saying out loud.
#[must_use]
pub fn enabled(stored: Option<&str>) -> bool {
    let Some(value) = stored.map(str::trim) else {
        return false;
    };
    value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("on") || value == "1"
}

/// The model a line should be written with, given the chat model.
///
/// An empty row and the sentinel mean the same thing, exactly like
/// [`crate::aux::effective`]: follow the primary model.
#[must_use]
pub fn model_for(stored: Option<&str>, primary: &str) -> String {
    match stored.map(str::trim) {
        None | Some("") | Some(INHERIT) => primary.to_owned(),
        Some(model) => model.to_owned(),
    }
}

/// Normalise what the form submitted: the sentinel is stored, never a model name.
#[must_use]
pub fn store_model(submitted: &str) -> String {
    let trimmed = submitted.trim();
    if trimmed.is_empty() {
        INHERIT.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// One cached line: the words, and the second they were written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cached {
    /// The line, already scrubbed when it was stored.
    pub text: String,
    /// When it was written, in epoch seconds. `0` when the row is unreadable,
    /// which makes it due immediately rather than eternal.
    pub made_at: i64,
}

/// The stored line, or `None` when there is not one yet.
#[must_use]
pub fn stored(db: &Db) -> Option<Cached> {
    let text = store::settings_get(db, SETTING_TEXT).ok()??;
    let text = text.trim().to_owned();
    if text.is_empty() {
        return None;
    }
    let made_at = store::settings_get(db, SETTING_MADE_AT)
        .ok()
        .flatten()
        .and_then(|raw| raw.trim().parse::<i64>().ok())
        .unwrap_or(0);
    Some(Cached { text, made_at })
}

/// Forget the cached line so the next render writes a new one.
///
/// Called when a conversation is started. The line belongs to the empty page, and
/// a new conversation is a new empty page, so it should not open with the words
/// that were written for the last one.
pub fn forget(db: &Db) -> Result<(), DbError> {
    store::settings_put(db, SETTING_TEXT, "")?;
    store::settings_put(db, SETTING_MADE_AT, "0")
}

/// Whether a line written at `made_at` is old enough to rewrite at `now`.
///
/// A stamp from the future (a clock that moved backwards) is not due, so a host
/// whose clock jumped forward once does not rewrite on every render forever.
#[must_use]
pub fn is_due(made_at: i64, refresh: Refresh, now: i64) -> bool {
    now.saturating_sub(made_at) >= refresh.seconds()
}

/// Keep a line, with the second it was written.
pub fn remember(db: &Db, text: &str, now: i64) -> Result<(), DbError> {
    store::settings_put(db, SETTING_TEXT, text)?;
    store::settings_put(db, SETTING_MADE_AT, &now.to_string())
}

/// What the model is told. Not one word about the operator.
///
/// The instruction is a constant rather than a formatted string on purpose:
/// there is no field in it that could carry a name, a path, a hostname, a
/// memory, or a transcript, so the isolation is structural rather than a rule
/// somebody has to remember.
const SYSTEM: &str = "You write one quiet line of ambient text that sits under a time-of-day \
salutation on an empty chat page. Reply with exactly one line of plain prose and nothing else. \
It must be factual and generic, true for any reader: a short observation about the hour, the \
light, the weather, or the quiet of a room. Do not greet the reader, do not ask a question, do \
not address anyone, do not mention work, plans, or the day someone is having. Under 90 \
characters. No markdown, no quotation marks, no list, and no explanation.";

/// The whole user turn: the instruction, and no context of any kind.
const INSTRUCTION: &str = "Write the line now.";

/// Ask a model for one short line.
///
/// Blocking, exactly like [`crate::web::chat::suggest_title`] and
/// `advise_approval`: `stream_chat` takes a `&mut dyn FnMut` sink, which is not
/// `Send`, so the runtime is built here and the caller runs this whole function
/// on a blocking thread.
#[must_use]
pub fn generate(config: &TurnConfig) -> Option<String> {
    let profile =
        ProviderProfile::new("greeting", config.base_url.clone(), config.key_ref.clone()).ok()?;
    let provider = OpenAiCompatProvider::new(
        profile,
        ModelProfile::new(config.model.clone()),
        Capabilities::openai_compatible(),
    )
    .ok()?;
    let key = core_runtime::provider::resolve_api_key(&config.key_ref).ok()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;

    let request = ChatRequest {
        system: Some(SYSTEM.to_owned()),
        messages: vec![ChatMessage::text(ChatRole::User, INSTRUCTION)],
        // No cap, for the reason `suggest_title` states: a thinking model spends
        // a short cap on reasoning and then writes nothing at all.
        ..ChatRequest::default()
    };

    let mut sink = |_event: ProviderEvent| {};
    let completion = runtime
        .block_on(provider.stream_chat(&request, &key, &mut sink))
        .ok()?;
    let line = scrub(&completion.text);
    (!line.is_empty()).then_some(line)
}

/// Ask for a line and keep it. `None` leaves whatever was stored alone.
#[must_use]
pub fn regenerate(db: &Db, config: &TurnConfig, now: i64) -> Option<String> {
    let line = generate(config)?;
    remember(db, &line, now).ok()?;
    Some(line)
}

/// Make a model's answer usable as a line of page copy.
///
/// Four things happen, in this order, and each one exists because a model did it
/// in testing or would plausibly do it:
///
/// - the first non-empty line only, because "one line" is an instruction and not
///   a guarantee;
/// - every `!` dropped and every em dash turned into a hyphen, because the page
///   is tested for both and a decoration is not worth an exception;
/// - whitespace collapsed, wrapping quotes stripped, a trailing stop or bang
///   stripped, and the whole thing bounded to [`LIMIT`] characters;
/// - nothing else. The caller cannot inject markup: the template escapes it.
#[must_use]
pub fn scrub(raw: &str) -> String {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let cleaned: String = line
        .chars()
        .filter(|ch| *ch != '!')
        .map(|ch| if ch == '\u{2014}' { '-' } else { ch })
        .collect();
    let collapsed: String = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = trim_wrapping(&collapsed);
    let bounded: String = trimmed.chars().take(LIMIT).collect();
    trim_wrapping(bounded.trim_end())
}

/// Strip wrapping quotes and a trailing stop, once per layer of wrapping.
///
/// Looped because the two can nest in either order: `"A line".` needs the stop
/// and then the quote, and `"A line."` needs the quote and then the stop.
fn trim_wrapping(text: &str) -> String {
    let mut current = text.trim();
    loop {
        let without_stop = current
            .trim_end_matches(['.', '!', '?', ':', ','])
            .trim_end();
        let without_quotes = without_stop
            .trim_matches(|ch| {
                ch == '"' || ch == '\'' || ch == '`' || ch == '\u{201c}' || ch == '\u{201d}'
            })
            .trim();
        if without_quotes.len() == current.len() {
            return without_quotes.to_owned();
        }
        current = without_quotes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("an in-memory database")
    }

    #[test]
    fn an_unwritten_greeting_is_off() {
        // The default has to be the cheap one: absent, empty, and unreadable are
        // all off, and only an explicit yes switches it on.
        assert!(!enabled(None));
        assert!(!enabled(Some("")));
        assert!(!enabled(Some("  ")));
        assert!(!enabled(Some("false")));
        assert!(!enabled(Some("FALSE")));
        assert!(!enabled(Some("0")));
        assert!(!enabled(Some("no")));
        assert!(!enabled(Some("off")));
        assert!(enabled(Some("true")));
        assert!(enabled(Some("TRUE")));
        assert!(enabled(Some("on")));
        assert!(enabled(Some("1")));
        assert!(enabled(Some(" true ")));
    }

    #[test]
    fn every_refresh_value_parses_and_round_trips() {
        assert_eq!(Refresh::parse(Some("hour")), Refresh::Hour);
        assert_eq!(Refresh::parse(Some("day")), Refresh::Day);
        assert_eq!(Refresh::parse(Some("week")), Refresh::Week);
        for refresh in Refresh::ALL {
            assert_eq!(Refresh::parse(Some(refresh.as_str())), refresh);
        }
        // Absent and unknown are the default, deliberately.
        assert_eq!(Refresh::parse(None), Refresh::Day);
        assert_eq!(Refresh::parse(Some("")), Refresh::Day);
        assert_eq!(Refresh::parse(Some("  day  ")), Refresh::Day);
        assert_eq!(Refresh::parse(Some("fortnight")), Refresh::Day);
        assert_eq!(Refresh::DEFAULT, Refresh::Day);
    }

    #[test]
    fn the_refresh_windows_are_an_hour_a_day_and_a_week() {
        assert_eq!(Refresh::Hour.seconds(), 3_600);
        assert_eq!(Refresh::Day.seconds(), 86_400);
        assert_eq!(Refresh::Week.seconds(), 604_800);
        assert_eq!(Refresh::Hour.label(), "Every hour");
        assert_eq!(Refresh::Day.label(), "Every day");
        assert_eq!(Refresh::Week.label(), "Every week");
    }

    #[test]
    fn a_line_is_due_once_the_window_has_passed() {
        let now = 1_800_000_000;
        assert!(!is_due(now, Refresh::Hour, now));
        assert!(!is_due(now - 3_599, Refresh::Hour, now));
        assert!(is_due(now - 3_600, Refresh::Hour, now));
        assert!(!is_due(now - 86_399, Refresh::Day, now));
        assert!(is_due(now - 86_400, Refresh::Day, now));
        assert!(is_due(now - 604_800, Refresh::Week, now));
        // A stamp from the future is not due, so a clock that jumped does not
        // cause a rewrite on every render for the rest of the window.
        assert!(!is_due(now + 600, Refresh::Hour, now));
    }

    #[test]
    fn the_model_row_follows_primary_until_it_is_pinned() {
        assert_eq!(model_for(None, "ds/one"), "ds/one");
        assert_eq!(model_for(Some(""), "ds/one"), "ds/one");
        assert_eq!(model_for(Some(INHERIT), "ds/one"), "ds/one");
        assert_eq!(model_for(Some(" ag/two "), "ds/one"), "ag/two");
        assert_eq!(store_model(""), INHERIT);
        assert_eq!(store_model("  "), INHERIT);
        assert_eq!(store_model(INHERIT), INHERIT);
        assert_eq!(store_model(" ag/two "), "ag/two");
    }

    #[test]
    fn a_line_round_trips_through_the_settings_rows() {
        let db = db();
        assert_eq!(stored(&db), None);
        remember(&db, "Light through the window", 1_800_000_000).expect("write");
        let cached = stored(&db).expect("a stored line");
        assert_eq!(cached.text, "Light through the window");
        assert_eq!(cached.made_at, 1_800_000_000);
        // An empty row is not a line, and a line with no stamp is due at once.
        store::settings_put(&db, SETTING_TEXT, "  ").expect("blank");
        assert_eq!(stored(&db), None);
        store::settings_put(&db, SETTING_TEXT, "Still here").expect("text");
        store::settings_put(&db, SETTING_MADE_AT, "not-a-number").expect("stamp");
        assert_eq!(stored(&db).map(|cached| cached.made_at), Some(0));
        assert!(is_due(0, Refresh::Week, 1_800_000_000));
    }

    #[test]
    fn the_line_under_the_salutation_is_the_operators_to_write() {
        assert_eq!(hint(None), DEFAULT_HINT, "absent reads as the default");
        assert_eq!(hint(Some("   ")), DEFAULT_HINT, "blank restores it too");
        assert_eq!(hint(Some("  Say it plainly  ")), "Say it plainly");
        let long = "x".repeat(MAX_HINT_CHARS + 40);
        assert_eq!(hint(Some(&long)).chars().count(), MAX_HINT_CHARS);
    }

    #[test]
    fn the_scrubber_takes_the_first_line_and_collapses_it() {
        assert_eq!(
            scrub("Light through the window.\nA second line."),
            "Light through the window"
        );
        assert_eq!(scrub("  Rain   on the   glass  "), "Rain on the glass");
        assert_eq!(scrub("\n\n   \nSteady rain\n"), "Steady rain");
        assert_eq!(scrub(""), "");
        assert_eq!(scrub("   \n  "), "");
    }

    #[test]
    fn the_scrubber_strips_quotes_and_a_trailing_stop() {
        assert_eq!(scrub("Steady rain."), "Steady rain");
        assert_eq!(scrub("Steady rain!"), "Steady rain");
        assert_eq!(scrub("Steady rain"), "Steady rain");
        assert_eq!(scrub("\"Steady rain.\""), "Steady rain");
        assert_eq!(scrub("\"Steady rain\"."), "Steady rain");
        assert_eq!(scrub("'Light fading'"), "Light fading");
        assert_eq!(scrub("`Light fading`"), "Light fading");
        assert_eq!(scrub("\u{201c}Light fading\u{201d}."), "Light fading");
    }

    #[test]
    fn no_line_ever_carries_a_bang_or_an_em_dash() {
        // The page is tested for both, so the line cannot be the one that slips
        // one through. A bang in the middle goes too, not just a trailing one.
        assert_eq!(scrub("Quiet out there!"), "Quiet out there");
        assert_eq!(scrub("Quiet! Really quiet."), "Quiet Really quiet");
        assert_eq!(scrub("Quiet \u{2014} really quiet"), "Quiet - really quiet");
        assert_eq!(
            scrub("Steady rain\u{2014}light fading"),
            "Steady rain-light fading"
        );
        for sample in ["Yes!", "A \u{2014} B", "ok!"] {
            let line = scrub(sample);
            assert!(!line.contains('!'), "{line}");
            assert!(!line.contains('\u{2014}'), "{line}");
        }
    }

    #[test]
    fn the_scrubber_bounds_the_line_at_ninety_characters() {
        let long = "a".repeat(200);
        assert_eq!(scrub(&long).chars().count(), LIMIT);
        let words = "word ".repeat(40);
        let line = scrub(&words);
        assert!(line.chars().count() <= LIMIT, "{line}");
        assert!(!line.ends_with(' '), "{line}");
        // A line that already fits is untouched.
        let short = "Light through the window";
        assert_eq!(scrub(short), short);
    }

    #[test]
    fn the_instruction_carries_no_operator_context() {
        // The isolation is structural: there is no field in the prompt that could
        // hold a name, a path, a hostname, a memory, or a transcript.
        for prompt in [SYSTEM, INSTRUCTION] {
            assert!(!prompt.contains('\u{2014}'), "{prompt}");
            assert!(!prompt.contains('!'), "{prompt}");
        }
        assert!(SYSTEM.contains("Under 90 characters"));
        assert!(SYSTEM.contains("Do not greet the reader"));
    }
}
