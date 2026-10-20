//! Slash commands, parsed and answered before a turn exists.
//!
//! A message that begins with `/` is not a prompt. It is a command, and the
//! daemon handles it: the send handler parses the text and dispatches it before
//! it builds a turn config, writes a question row, or touches a provider. That
//! ordering is the whole guarantee - a command costs no tokens because there is
//! no turn to spend them on, not because something remembers to skip one.
//!
//! This module holds the part that needs no database and no settings: the
//! grammar, the one-line description `/help` prints, the copy that names a
//! command as unsupported, and the parser for the one argument that has a closed
//! set of values. The dispatch that reads and writes the store lives in
//! `web::routes::chat_send`, where the send handler already owns those.
//!
//! Copy rule: an answer written here is rendered onto the chat page, so it
//! carries no exclamation mark and no em dash. A unit test below enforces that
//! rather than leaving it to the page sweep to notice.

/// One parsed command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// The command itself, without its slash and lowercased, so `/MEMORY` and
    /// `/memory` are the same command.
    pub name: String,
    /// Everything after the first run of whitespace, trimmed. Empty when the
    /// command carried no argument at all.
    pub argument: String,
}

/// Every command this daemon implements, with the line `/help` prints for it.
///
/// The order is the order `/help` lists them in: the one that explains the rest
/// first, then the conversation commands, then the two that change settings.
pub const SUPPORTED: &[(&str, &str)] = &[
    ("help", "list every command this daemon implements"),
    ("new", "start a new conversation and show it"),
    ("clear", "clear this conversation's transcript"),
    ("stop", "cancel a turn that is running in this conversation"),
    (
        "model",
        "set the stored default model, by id from the model list",
    ),
    ("memory", "set the memory backend to gbrain, vault or off"),
];

/// Names that come from other tools and are not this daemon's features.
///
/// They are listed rather than merely refused, because an operator who types
/// `/goal` expects a goal loop to exist and deserves to be told the truth in one
/// line instead of watching nothing happen.
pub const NOT_FEATURES: &[&str] = &["goal", "restart"];

/// What a command resolves to without touching the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Answered from the grammar alone: this is the text to show.
    Reply(String),
    /// Not a command this daemon implements. The name is carried so the refusal
    /// can name it.
    Unsupported(String),
}

/// Split a message into a command, when it is one.
///
/// Returns `Some` only when the trimmed text starts with `/` and the text after
/// it has a non-empty name. The name ends at the first whitespace and is
/// lowercased; everything after that whitespace is the argument, trimmed, so
/// `/model   x ` and `/model x` are the same command.
///
/// A message that does not start with a slash is a prompt and returns `None`, so
/// the caller keeps its existing path.
#[must_use]
pub fn parse(text: &str) -> Option<Command> {
    let after_slash = text.trim().strip_prefix('/')?;
    // A slash followed by nothing but space is punctuation, not a command.
    let after_slash = after_slash.trim_start();
    if after_slash.is_empty() {
        return None;
    }
    let (name, argument) = match after_slash.find(char::is_whitespace) {
        Some(index) => (&after_slash[..index], after_slash[index..].trim()),
        None => (after_slash, ""),
    };
    if name.is_empty() {
        return None;
    }
    Some(Command {
        name: name.to_lowercase(),
        argument: argument.to_owned(),
    })
}

/// Whether this name is one of the commands in [`SUPPORTED`].
#[must_use]
pub fn is_supported(name: &str) -> bool {
    SUPPORTED.iter().any(|(supported, _)| *supported == name)
}

/// The text `/help` shows.
///
/// The list is fenced so the columns survive the transcript renderer, which
/// collapses newlines in ordinary prose. Built from [`SUPPORTED`], so a command
/// that is added there cannot be missing here.
#[must_use]
pub fn help() -> String {
    let mut text = String::from(
        "Slash commands are handled by this daemon: a message that starts with a slash is \
         answered here and never sent to a model. This list is all of them.\n\n",
    );
    text.push_str("```\n");
    for (name, description) in SUPPORTED {
        text.push_str(&format!("/{name} - {description}\n"));
    }
    text.push_str("```\n\n");
    text.push_str(&not_features_line());
    text.push('.');
    text
}

/// The refusal for a name this daemon does not implement.
///
/// It names the command back, says plainly that nothing was sent to a model, and
/// names the two commands operators most often arrive with as somebody else's
/// feature rather than pretending they work.
#[must_use]
pub fn unsupported(name: &str) -> String {
    format!(
        "/{name} is not supported: this daemon does not implement it, and nothing was sent to \
         a model. {}: there is no goal loop here and no restart command either. /help lists the \
         commands this daemon does implement.",
        not_features_line()
    )
}

/// The one clause the help text and the refusal share, so the two cannot drift.
fn not_features_line() -> String {
    let listed = NOT_FEATURES
        .iter()
        .map(|name| format!("/{name}"))
        .collect::<Vec<_>>()
        .join(" and ");
    format!("{listed} are not Kallisti features")
}

/// Check `/memory`'s argument against the three backends.
///
/// The refusal names the argument that was typed and the three that exist, and
/// says that nothing changed, because a settings command that refuses must not
/// look like one that applied.
pub fn memory_backend(argument: &str) -> Result<crate::memory::Backend, String> {
    match argument.trim().to_ascii_lowercase().as_str() {
        "gbrain" => Ok(crate::memory::Backend::Gbrain),
        "vault" => Ok(crate::memory::Backend::Vault),
        "off" => Ok(crate::memory::Backend::Off),
        "" => Err(
            "That needs a backend: /memory <gbrain|vault|off>. The stored backend was not \
             changed."
                .to_owned(),
        ),
        other => Err(format!(
            "{other} is not a memory backend: the three are gbrain, vault and off. The stored \
             backend was not changed."
        )),
    }
}

/// What a command resolves to before anything is read or written.
///
/// `Some` means the answer is complete already: `/help`, and every name outside
/// [`SUPPORTED`]. `None` means the command is implemented and the caller has to
/// reach the store to answer it.
#[must_use]
pub fn immediate(command: &Command) -> Option<Outcome> {
    if command.name == "help" {
        return Some(Outcome::Reply(help()));
    }
    if is_supported(&command.name) {
        return None;
    }
    Some(Outcome::Unsupported(command.name.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A command with no argument: the name is the whole text.
    #[test]
    fn a_bare_command_parses_with_no_argument() {
        let command = parse("/help").expect("a command");
        assert_eq!(command.name, "help");
        assert_eq!(command.argument, "");
    }

    /// The argument is everything after the first whitespace, trimmed, so a
    /// sloppy number of spaces is the same command as a tidy one.
    #[test]
    fn the_argument_is_everything_after_the_first_whitespace() {
        assert_eq!(
            parse("/model x"),
            Some(Command {
                name: "model".to_owned(),
                argument: "x".to_owned()
            })
        );
        assert_eq!(
            parse("/model   two words "),
            Some(Command {
                name: "model".to_owned(),
                argument: "two words".to_owned()
            })
        );
        assert_eq!(
            parse("/help me"),
            Some(Command {
                name: "help".to_owned(),
                argument: "me".to_owned()
            })
        );
    }

    /// The name is lowercased and the surrounding whitespace is not part of it.
    #[test]
    fn the_name_is_lowercased_and_the_text_is_trimmed() {
        assert_eq!(
            parse(" /MEMORY off "),
            Some(Command {
                name: "memory".to_owned(),
                argument: "off".to_owned()
            })
        );
        assert_eq!(
            parse("\n/model\tid\n"),
            Some(Command {
                name: "model".to_owned(),
                argument: "id".to_owned()
            })
        );
    }

    /// A slash with nothing after it is punctuation, not a command.
    #[test]
    fn a_slash_with_no_name_is_not_a_command() {
        assert_eq!(parse("/"), None);
        assert_eq!(parse("/  "), None);
        assert_eq!(parse("   /   "), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("   "), None);
    }

    /// Ordinary text is a prompt and keeps the old path.
    #[test]
    fn text_that_does_not_start_with_a_slash_is_a_prompt() {
        assert_eq!(parse("hello"), None);
        assert_eq!(parse("what is 1/2"), None);
        assert_eq!(parse("help"), None);
    }

    /// A name outside the list is unknown, and the refusal names it.
    #[test]
    fn an_unknown_name_is_unknown_and_the_refusal_names_it() {
        let command = parse("/goal get fit").expect("a command");
        assert!(!is_supported(&command.name));
        match immediate(&command) {
            Some(Outcome::Unsupported(name)) => {
                assert_eq!(name, "goal");
                let text = unsupported(&name);
                assert!(text.contains("/goal"), "the refusal names it: {text}");
                assert!(
                    text.contains("not supported"),
                    "the refusal says so: {text}"
                );
                assert!(
                    text.contains("nothing was sent to a model"),
                    "the refusal says no model was asked: {text}"
                );
            }
            other => panic!("an unknown command must be refused: {other:?}"),
        }
    }

    /// Every implemented command needs the store, and /help does not.
    #[test]
    fn help_is_answered_here_and_every_other_command_needs_the_store() {
        let help = parse("/help").expect("a command");
        match immediate(&help) {
            Some(Outcome::Reply(text)) => {
                assert!(text.contains("Slash commands"), "help explains itself");
            }
            other => panic!("/help must be answered here: {other:?}"),
        }
        for (name, _) in SUPPORTED {
            if *name == "help" {
                continue;
            }
            let command = parse(&format!("/{name}")).expect("a command");
            assert_eq!(
                immediate(&command),
                None,
                "/{name} has to reach the store to answer"
            );
        }
    }

    /// `/goal` and `/restart` are named as somebody else's features, in both the
    /// help text and the refusal, because that is the honest answer.
    #[test]
    fn the_copy_names_goal_and_restart_as_not_kallisti_features() {
        let text = help();
        for name in NOT_FEATURES {
            assert!(
                text.contains(&format!("/{name}")),
                "/{name} is named: {text}"
            );
        }
        assert!(
            text.contains("not Kallisti features"),
            "help says whose they are: {text}"
        );
        let refusal = unsupported("goal");
        for name in NOT_FEATURES {
            assert!(
                refusal.contains(&format!("/{name}")),
                "/{name} is named in the refusal: {refusal}"
            );
        }
        assert!(refusal.contains("no goal loop"), "the loop is denied");
        assert!(
            refusal.contains("no restart command"),
            "the restart is denied"
        );
    }

    /// The list is built from [`SUPPORTED`], so nothing can be implemented and
    /// left out of it.
    #[test]
    fn help_lists_every_supported_command() {
        let text = help();
        for (name, description) in SUPPORTED {
            assert!(
                text.contains(&format!("/{name}")),
                "/{name} is listed: {text}"
            );
            assert!(
                text.contains(description),
                "the line for /{name} carries its description: {text}"
            );
        }
        assert_eq!(
            SUPPORTED.len(),
            6,
            "six commands ship, and the list is the contract"
        );
        // A description is one line of prose, because /help prints one row each.
        for (name, description) in SUPPORTED {
            assert!(!description.trim().is_empty(), "/{name} has a description");
            assert!(
                !description.contains('\n'),
                "the description for /{name} is one line"
            );
        }
    }

    /// `/memory` takes a closed set, and refuses anything else by name.
    #[test]
    fn a_memory_backend_is_one_of_three() {
        for (argument, expected) in [
            ("gbrain", crate::memory::Backend::Gbrain),
            ("vault", crate::memory::Backend::Vault),
            ("off", crate::memory::Backend::Off),
        ] {
            assert_eq!(memory_backend(argument), Ok(expected));
        }
        // The comparison is case-insensitive and tolerates surrounding space.
        assert_eq!(memory_backend("  OFF "), Ok(crate::memory::Backend::Off));
        let refused = memory_backend("postgres").expect_err("refused");
        assert!(
            refused.contains("postgres") && refused.contains("gbrain"),
            "the refusal names the argument and the three that exist: {refused}"
        );
        assert!(
            refused.contains("not changed"),
            "a refusal says nothing changed: {refused}"
        );
        assert!(memory_backend("").is_err(), "an empty argument is refused");
    }

    /// Everything written here lands on the chat page, so it obeys the page copy
    /// rule: no exclamation mark, no em dash.
    #[test]
    fn no_command_copy_carries_an_exclamation_or_an_em_dash() {
        let mut texts = vec![help(), unsupported("goal"), unsupported("restart")];
        texts.push(memory_backend("postgres").expect_err("refused"));
        texts.push(memory_backend("").expect_err("refused"));
        for text in texts {
            assert!(!text.contains('!'), "no exclamation mark: {text}");
            assert!(!text.contains('\u{2014}'), "no em dash: {text}");
            assert!(!text.to_lowercase().contains("purple"), "no purple: {text}");
        }
    }
}
