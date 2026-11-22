//! Model routing: sending a turn to the model that suits it.
//!
//! A turn used to run on one model - the conversation's own, or the daemon's
//! default - no matter what the operator asked it to do. That is the right answer
//! for a chat about the weather and the wrong one for a debugging session, and it
//! is the wrong one for a long research request, which is a different shape of
//! work again. This module names the difference and lets the operator pin a model
//! per kind of work.
//!
//! The decision is local and cheap. [`classify`] reads the message and nothing
//! else: no provider call, no model, no network. A routing layer that asked a
//! model which model to use would pay for the answer twice and fail when the
//! endpoint is the thing that is broken. The heuristics are therefore deliberately
//! small and named in the reason they return, so an operator can read why a turn
//! went where it went instead of guessing.
//!
//! Three intents exist:
//!
//! - [`Intent::Coding`] - code work: a fenced block, a source-file extension, or
//!   a verb that asks for it (implement, refactor, debug, ...).
//! - [`Intent::Heavy`] - long or deep work: a message past the length floor, or
//!   one that asks to research, plan, audit, benchmark and the like.
//! - [`Intent::General`] - everything else.
//!
//! Coding wins over heavy, because "implement this and explain the trade-offs" is
//! code work with a paragraph attached, not a research request. Heavy only fires
//! when no coding signal does, which is what makes a long code request stay on
//! the coding model.
//!
//! A coding turn may also name a *harness*: an external coding tool the work can
//! be handed to. This module only carries the name. Nothing here shells out, and
//! a turn that names a harness still runs its usual loop on its usual model.
//!
//! Everything is read from `settings`, never written: the write side is the
//! `/v1/routing` route, which validates a harness before it stores one.

use crate::db::Db;
use crate::web::store::settings_get;

/// The `settings` row arming automatic routing.
///
/// `"true"` and `"false"`, with `"true"` for an unset row: the feature ships on so
/// an operator gets the model-per-intent behaviour without hunting for a switch,
/// and one row turns it off again.
pub const SETTING_AUTO: &str = "routing.auto";

/// The `settings` row holding the coding model.
pub const SETTING_MODEL_CODING: &str = "routing.model.coding";

/// The `settings` row holding the heavy model.
pub const SETTING_MODEL_HEAVY: &str = "routing.model.heavy";

/// The `settings` row holding the general model.
pub const SETTING_MODEL_GENERAL: &str = "routing.model.general";

/// The `settings` row holding the harness a coding turn names.
pub const SETTING_HARNESS: &str = "routing.harness";

/// The harness value that means "no external tool".
pub const HARNESS_NONE: &str = "none";

/// The harnesses this build names, in the order a picker lists them.
///
/// A closed set on purpose: a stored name no client knows is a name nothing can
/// act on, so the write route refuses anything outside this list by name.
pub const HARNESSES: [&str; 4] = [HARNESS_NONE, "dsh", "claude-code", "antigravity"];

/// How long a message has to be before it counts as heavy work.
///
/// Characters, not bytes: a request written in a script that costs three bytes
/// per character is not longer work than the same request in ASCII. Six hundred
/// is roughly a paragraph of specification, which is past the point where the
/// cheap model starts losing the thread.
pub const HEAVY_CHARS: usize = 600;

/// What a turn is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    /// Neither code nor deep work: an ordinary question.
    General,
    /// Code work, from a file extension to a request to refactor.
    Coding,
    /// Long or deep work: research, planning, an audit.
    Heavy,
}

impl Intent {
    /// The wire and storage spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Coding => "coding",
            Self::Heavy => "heavy",
        }
    }

    /// Parse the spelling this module stores and reports.
    ///
    /// Trimmed and case-insensitive, because the value arrives from a query
    /// string as often as from a column, where `"Coding"` is a spelling and not a
    /// different intent.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "general" => Some(Self::General),
            "coding" => Some(Self::Coding),
            "heavy" => Some(Self::Heavy),
            _ => None,
        }
    }

    /// The `settings` row that holds this intent's model.
    #[must_use]
    pub const fn setting_key(self) -> &'static str {
        match self {
            Self::General => SETTING_MODEL_GENERAL,
            Self::Coding => SETTING_MODEL_CODING,
            Self::Heavy => SETTING_MODEL_HEAVY,
        }
    }
}

/// One routing decision, and why it was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// What the message is for.
    pub intent: Intent,
    /// The model this turn should run on, or `None` when there is no model at
    /// all - a daemon that has not finished onboarding.
    pub model: Option<String>,
    /// The external coding tool a coding turn names, when one is configured.
    pub harness: Option<String>,
    /// The one clause that decided the intent, for the operator and the log.
    pub reason: String,
}

impl Route {
    /// The route as the `turn.start` frame and the preview route report it.
    ///
    /// One shape in one place: the frame a phone reads and the answer a client
    /// previews cannot drift apart if they are built here.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "intent": self.intent.as_str(),
            "model": self.model,
            "harness": self.harness,
            "reason": self.reason,
        })
    }
}

/// The file extensions that make a message code work.
///
/// Ordered so a longer extension that contains a shorter one is matched first:
/// `.tsx` before `.ts`, `.json` before `.js`, `.css` before `.cs`. The operator
/// reads which one matched, so the wrong one is not merely cosmetic.
const CODE_EXTENSIONS: [&str; 21] = [
    ".rs", ".swift", ".py", ".tsx", ".ts", ".json", ".js", ".go", ".java", ".css", ".cs", ".kt",
    ".rb", ".php", ".sql", ".sh", ".yaml", ".yml", ".toml", ".html", ".md",
];

/// The verbs and phrases that ask for code work, with the clause the reason
/// reports.
///
/// Single words are matched at a word boundary; multi-word entries against the
/// message with its whitespace collapsed, so a phrase broken across a newline is
/// still the phrase.
const CODE_SIGNALS: [(&str, &str); 18] = [
    // Phrases first, and longer stems before the words they start with: the
    // first match writes the reason, and "fix the build" must not be reported
    // as "asks to build".
    ("fix the build", "asks to fix the build"),
    ("unit test", "asks to write a unit test"),
    ("add a test", "asks to add a test"),
    ("stack trace", "mentions a stack trace"),
    ("compiler", "mentions a compiler"),
    ("implement", "asks to implement"),
    ("refactor", "asks to refactor"),
    ("compile", "asks to compile"),
    ("build", "asks to build"),
    ("migrate", "asks to migrate"),
    ("patch", "asks to patch"),
    ("debug", "asks to debug"),
    ("linter", "mentions a linter"),
    ("ci", "mentions ci"),
    ("endpoint", "mentions an endpoint"),
    ("schema", "mentions a schema"),
    ("migration", "mentions a migration"),
    ("dependency", "mentions a dependency"),
];

/// The words and phrases that ask for deep work, with the clause the reason
/// reports.
///
/// Checked after every coding signal, so a long request that names a source file
/// stays code work. The American spelling rides beside the British one because
/// the message is the operator's own words and either spelling is the same ask.
const HEAVY_SIGNALS: [(&str, &str); 11] = [
    ("research", "asks to research"),
    ("analyse in depth", "asks to analyse in depth"),
    ("analyze in depth", "asks to analyze in depth"),
    ("compare options", "asks to compare options"),
    ("plan", "asks to plan"),
    ("architect", "asks to architect"),
    ("design doc", "asks for a design doc"),
    ("investigate", "asks to investigate"),
    ("audit", "asks to audit"),
    ("benchmark", "asks to benchmark"),
    ("report", "asks to report"),
];

/// What a message is for, and the clause that decided it.
///
/// Pure: no database, no provider call, no clock. The reason is written for a
/// person reading a log line or a preview, so it names the signal rather than
/// restating the intent.
#[must_use]
pub fn classify(message: &str) -> (Intent, String) {
    if message.contains("```") || message.contains("~~~") {
        return (Intent::Coding, "fenced code block".to_owned());
    }
    let lowered = message.to_ascii_lowercase();
    if let Some(extension) = CODE_EXTENSIONS
        .iter()
        .find(|extension| lowered.contains(*extension))
    {
        return (Intent::Coding, format!("file extension {extension}"));
    }
    // Whitespace is collapsed once, so both the word and the phrase matches see
    // the same text: a phrase split across a wrap is still one phrase.
    let collapsed = lowered.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some((_, reason)) = CODE_SIGNALS
        .iter()
        .find(|(needle, _)| contains_signal(&collapsed, needle))
    {
        return (Intent::Coding, (*reason).to_owned());
    }
    if let Some((_, reason)) = HEAVY_SIGNALS
        .iter()
        .find(|(needle, _)| contains_signal(&collapsed, needle))
    {
        return (Intent::Heavy, (*reason).to_owned());
    }
    let characters = message.trim().chars().count();
    if characters >= HEAVY_CHARS {
        return (
            Intent::Heavy,
            format!("long message ({characters} characters)"),
        );
    }
    (Intent::General, "no coding or heavy signal".to_owned())
}

/// Whether `haystack` carries `needle` as a word, as the stem of one, or as the
/// phrase the needle spells.
///
/// A prefix counts for single words of four characters or more, so `implementing`
/// is the same ask as `implement`. `ci` is shorter than that on purpose: as a
/// prefix it would fire on every word beginning with those two letters, which is
/// most of a paragraph. A multi-word needle is matched against the collapsed text
/// directly, because "fix the build" is one ask and not three words that happen
/// to be near each other.
fn contains_signal(haystack: &str, needle: &str) -> bool {
    if needle.contains(' ') {
        return haystack.contains(needle);
    }
    let stemmed = needle.chars().count() >= 4;
    haystack
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .any(|word| word == needle || (stemmed && word.starts_with(needle)))
}

/// Whether automatic routing is switched on.
///
/// An unset row and an unreadable one are on: the feature is the default, and a
/// settings read that failed is not an operator asking for it to stop. Only an
/// explicit `false` (or one of the spellings a form may send) turns it off.
#[must_use]
pub fn auto_enabled(db: &Db) -> bool {
    match settings_get(db, SETTING_AUTO) {
        Ok(Some(raw)) => !matches!(
            raw.trim().to_ascii_lowercase().as_str(),
            "false" | "0" | "off" | "no"
        ),
        _ => true,
    }
}

/// One intent's stored model, trimmed, or the empty string when it inherits.
///
/// The empty string is the wire form of "inherit the daemon's model", which is
/// what the settings route reports and what [`route`] reads.
#[must_use]
pub fn model_setting(db: &Db, intent: Intent) -> String {
    settings_get(db, intent.setting_key())
        .ok()
        .flatten()
        .map(|raw| raw.trim().to_owned())
        .unwrap_or_default()
}

/// Whether a value names one of the harnesses this build knows.
#[must_use]
pub fn valid_harness(value: &str) -> bool {
    HARNESSES.contains(&value.trim())
}

/// The configured harness, or [`HARNESS_NONE`].
///
/// A value outside the closed set reads as none: it cannot be stored through the
/// route, and an install that somehow holds one is better described as "no
/// harness" than as a name nothing can act on.
#[must_use]
pub fn harness_setting(db: &Db) -> String {
    settings_get(db, SETTING_HARNESS)
        .ok()
        .flatten()
        .map(|raw| raw.trim().to_owned())
        .filter(|value| valid_harness(value))
        .unwrap_or_else(|| HARNESS_NONE.to_owned())
}

/// Choose the model and the harness for one message.
///
/// `provider_model` is the model this turn already resolved - the conversation's
/// own, the daemon's pin, or the picker's choice - and it is what the turn keeps
/// when the operator has pinned nothing for the intent. That order matters: this
/// layer is a refinement over the existing decision, not a replacement for it, so
/// a daemon with no routing rows behaves exactly as it did before.
///
/// With [`SETTING_AUTO`] off the answer is general, the provider's model and no
/// harness, so the one row is enough to return a daemon to its old behaviour
/// without clearing the models it stored.
#[must_use]
pub fn route(db: &Db, message: &str, provider_model: Option<&str>) -> Route {
    let provider_model = provider_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_owned);
    if !auto_enabled(db) {
        return Route {
            intent: Intent::General,
            model: provider_model,
            harness: None,
            reason: "automatic routing is off".to_owned(),
        };
    }
    let (intent, reason) = classify(message);
    let model = Some(model_setting(db, intent))
        .filter(|model| !model.is_empty())
        .or(provider_model);
    // The harness rides only on code work: handing a research question to a
    // coding tool is a way to get a worse answer, not a faster one.
    let harness = (intent == Intent::Coding)
        .then(|| harness_setting(db))
        .filter(|harness| harness != HARNESS_NONE);
    Route {
        intent,
        model,
        harness,
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch database and the directory that owns it, so the settings rows a
    /// test writes are thrown away with it.
    fn scratch() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = crate::config::Paths::for_root(dir.path());
        let db = Db::open(&paths.db_file).expect("db");
        (dir, db)
    }

    fn store(db: &Db, key: &str, value: &str) {
        crate::web::store::settings_put(db, key, value).expect("store");
    }

    #[test]
    fn an_intent_round_trips_through_its_spelling() {
        for intent in [Intent::General, Intent::Coding, Intent::Heavy] {
            assert_eq!(Intent::parse(intent.as_str()), Some(intent));
        }
        assert_eq!(Intent::parse(" Coding "), Some(Intent::Coding));
        assert_eq!(Intent::parse("nonsense"), None);
        assert_eq!(Intent::Coding.setting_key(), SETTING_MODEL_CODING);
    }

    #[test]
    fn a_fenced_code_block_is_coding() {
        let (intent, reason) = classify("why does this not compile?\n```rust\nfn main() {}\n```");
        assert_eq!(intent, Intent::Coding, "{reason}");
        assert_eq!(reason, "fenced code block");
        // The tilde fence is the same block.
        assert_eq!(classify("a\n~~~\nselect 1;\n~~~").0, Intent::Coding);
    }

    #[test]
    fn every_source_extension_is_coding() {
        for extension in CODE_EXTENSIONS {
            let (intent, reason) = classify(&format!("please read main{extension} and tell me"));
            assert_eq!(intent, Intent::Coding, "{extension}: {reason}");
            assert!(
                reason.contains(extension),
                "{extension} was not named: {reason}"
            );
        }
    }

    #[test]
    fn every_coding_verb_is_coding() {
        for (needle, clause) in CODE_SIGNALS {
            let (intent, reason) = classify(&format!("could you {needle} this for me"));
            assert_eq!(intent, Intent::Coding, "{needle}: {reason}");
            assert_eq!(reason, clause, "{needle} named the wrong signal");
        }
    }

    #[test]
    fn every_heavy_ask_is_heavy() {
        for (needle, clause) in HEAVY_SIGNALS {
            let (intent, reason) = classify(&format!("please {needle} the launch for me"));
            assert_eq!(intent, Intent::Heavy, "{needle}: {reason}");
            assert_eq!(reason, clause, "{needle} named the wrong signal");
        }
    }

    #[test]
    fn a_long_message_is_heavy_and_a_short_one_is_not() {
        let long = "alpha ".repeat(120);
        assert!(long.trim().chars().count() >= HEAVY_CHARS);
        let (intent, reason) = classify(&long);
        assert_eq!(intent, Intent::Heavy);
        assert!(reason.contains("characters"), "{reason}");

        // One character short of the floor stays general.
        let short = "a".repeat(HEAVY_CHARS - 1);
        assert_eq!(classify(&short).0, Intent::General);
    }

    #[test]
    fn a_plain_message_is_general() {
        let (intent, reason) = classify("how are you today?");
        assert_eq!(intent, Intent::General);
        assert_eq!(reason, "no coding or heavy signal");
    }

    #[test]
    fn code_work_stays_coding_even_when_the_message_is_long_or_asks_to_plan() {
        // A long specification that also asks to implement is code work: the
        // coding signal decides, and the length does not overrule it.
        let message = format!(
            "plan this carefully. implement the parser. {}",
            "x ".repeat(400)
        );
        assert_eq!(classify(&message).0, Intent::Coding);
    }

    #[test]
    fn the_harness_rides_only_on_code_work() {
        let (_dir, db) = scratch();
        store(&db, SETTING_HARNESS, "dsh");

        let coding = route(&db, "implement a parser", Some("primary"));
        assert_eq!(coding.intent, Intent::Coding);
        assert_eq!(coding.harness.as_deref(), Some("dsh"));

        for (message, intent) in [
            ("please research the market", Intent::Heavy),
            ("how are you today?", Intent::General),
        ] {
            let route = route(&db, message, Some("primary"));
            assert_eq!(route.intent, intent);
            assert_eq!(route.harness, None, "{message} should not carry a harness");
        }
    }

    #[test]
    fn no_harness_configured_means_no_harness_named() {
        let (_dir, db) = scratch();
        assert_eq!(harness_setting(&db), HARNESS_NONE);
        assert_eq!(route(&db, "implement it", Some("primary")).harness, None);

        store(&db, SETTING_HARNESS, "none");
        assert_eq!(route(&db, "implement it", Some("primary")).harness, None);
    }

    #[test]
    fn a_harness_outside_the_closed_set_reads_as_none() {
        let (_dir, db) = scratch();
        store(&db, SETTING_HARNESS, "some-other-tool");
        assert_eq!(harness_setting(&db), HARNESS_NONE);
        assert_eq!(route(&db, "implement it", Some("primary")).harness, None);
        assert!(!valid_harness("some-other-tool"));
        for harness in HARNESSES {
            assert!(valid_harness(harness), "{harness} must be accepted");
        }
    }

    #[test]
    fn a_stored_model_beats_the_provider_model_which_beats_nothing() {
        let (_dir, db) = scratch();

        // Nothing pinned: the model the turn already resolved is the answer.
        let inherited = route(&db, "implement a parser", Some("primary"));
        assert_eq!(inherited.model.as_deref(), Some("primary"));

        // Pinned for the intent: the route moves the turn.
        store(&db, SETTING_MODEL_CODING, "coder");
        assert_eq!(
            route(&db, "implement a parser", Some("primary"))
                .model
                .as_deref(),
            Some("coder")
        );
        // A different intent keeps its own answer.
        assert_eq!(
            route(&db, "how are you", Some("primary")).model.as_deref(),
            Some("primary")
        );

        // An empty row means inherit, so the provider model comes back.
        store(&db, SETTING_MODEL_CODING, "  ");
        assert_eq!(
            route(&db, "implement a parser", Some("primary"))
                .model
                .as_deref(),
            Some("primary")
        );

        // No provider model and no pin: there is no model to name.
        assert_eq!(route(&db, "how are you", None).model, None);
        assert_eq!(route(&db, "how are you", Some("  ")).model, None);
    }

    #[test]
    fn every_intent_reads_its_own_model_row() {
        let (_dir, db) = scratch();
        store(&db, SETTING_MODEL_CODING, "coder");
        store(&db, SETTING_MODEL_HEAVY, "thinker");
        store(&db, SETTING_MODEL_GENERAL, "talker");

        assert_eq!(model_setting(&db, Intent::Coding), "coder");
        assert_eq!(model_setting(&db, Intent::Heavy), "thinker");
        assert_eq!(model_setting(&db, Intent::General), "talker");

        assert_eq!(
            route(&db, "please research the market", Some("primary"))
                .model
                .as_deref(),
            Some("thinker")
        );
        assert_eq!(
            route(&db, "how are you", Some("primary")).model.as_deref(),
            Some("talker")
        );
    }

    #[test]
    fn automatic_routing_off_answers_with_the_provider_model_and_no_harness() {
        let (_dir, db) = scratch();
        store(&db, SETTING_AUTO, "false");
        // Rows that would all fire if the switch were on.
        store(&db, SETTING_MODEL_CODING, "coder");
        store(&db, SETTING_HARNESS, "claude-code");
        assert!(!auto_enabled(&db));

        let route = route(&db, "implement a parser", Some("primary"));
        assert_eq!(route.intent, Intent::General);
        assert_eq!(route.model.as_deref(), Some("primary"));
        assert_eq!(route.harness, None);
        assert_eq!(route.reason, "automatic routing is off");

        // Routing is on unless a row says otherwise.
        store(&db, SETTING_AUTO, "true");
        assert!(auto_enabled(&db));
    }

    #[test]
    fn the_route_reports_every_documented_key() {
        let route = Route {
            intent: Intent::Coding,
            model: Some("coder".to_owned()),
            harness: Some("dsh".to_owned()),
            reason: "asks to implement".to_owned(),
        };
        assert_eq!(
            route.json(),
            serde_json::json!({
                "intent": "coding",
                "model": "coder",
                "harness": "dsh",
                "reason": "asks to implement",
            })
        );
        // A route with no model and no harness serialises as null, not as a
        // missing key: a client reads the same four fields every time.
        let empty = Route {
            intent: Intent::General,
            model: None,
            harness: None,
            reason: "no coding or heavy signal".to_owned(),
        };
        assert_eq!(empty.json()["model"], serde_json::Value::Null);
        assert_eq!(empty.json()["harness"], serde_json::Value::Null);
    }
}
