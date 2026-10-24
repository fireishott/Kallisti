//! The assistant's identity: one text file that becomes the system prompt.
//!
//! Three deliberate properties, because this is the one file whose content
//! changes what every turn says:
//!
//! - **It is read per turn, not per boot.** An edit takes effect on the next
//!   turn with no restart, which is the whole point of keeping it a file.
//! - **It is capped.** A file over [`MAX_PERSONA_BYTES`] is refused with the
//!   numbers rather than truncated, because half a persona is a persona the
//!   operator did not write.
//! - **It is never fatal to be absent.** A missing file with no inline text
//!   means "no system block": the turn proceeds. Only an unreadable or oversized
//!   file stops a turn, and both are configuration mistakes with a clear message.
//!
//! The file is not a secret and is written 0644. It is never logged: the audit
//! row and the debug line carry a length, never the text.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use core_runtime::prompt::MAX_SYSTEM_BYTES;

use crate::config::{ConfigError, Paths, expand_tilde};

/// The hard cap on persona text, in bytes. The same number the compiler uses,
/// so a persona that fits here cannot fail to compile on size.
pub const MAX_PERSONA_BYTES: usize = MAX_SYSTEM_BYTES;

/// The `settings` row that holds the assistant's display name.
///
/// The identity file says who the assistant is; this row says what to call it.
/// They are separate because a name is a label ("Message Kallisti"), not a claim
/// the model has to be told about on every turn.
pub const SETTING_ASSISTANT_NAME: &str = "assistant.name";

/// The name the dashboard uses before the operator sets one.
pub const DEFAULT_ASSISTANT_NAME: &str = "Kallisti";

/// The `settings` row that remembers the operator's on/off choice.
///
/// The `[persona] enabled` key in `config.toml` is the default; a row here is
/// the operator's later decision from the dashboard, and it wins.
pub const SETTING_PERSONA_ENABLED: &str = "assistant.persona_enabled";

/// The documented default path, `~/.config/kallisti/persona.md`.
pub const DEFAULT_PERSONA_PATH: &str = "~/.config/kallisti/persona.md";

/// The environment-context file, kept beside the persona.
///
/// `persona.md` answers "who are you"; `context.md` answers "what is this box".
/// They are separate files so the dashboard's identity form cannot clobber the
/// environment facts when it saves.
pub const CONTEXT_FILE_NAME: &str = "context.md";

/// Why a persona could not be used.
#[derive(Debug, thiserror::Error)]
pub enum PersonaError {
    /// The text is bigger than the cap.
    #[error(
        "the persona file {path} is {actual} bytes; the limit is {limit} bytes. \
         Shorten it, or disable the persona on the settings page"
    )]
    TooLarge {
        /// The file that was read.
        path: PathBuf,
        /// Its size.
        actual: usize,
        /// [`MAX_PERSONA_BYTES`].
        limit: usize,
    },
    /// The file exists but cannot be read.
    #[error("cannot read the persona file {path}: {source}")]
    Read {
        /// The file that was read.
        path: PathBuf,
        /// The underlying failure.
        source: io::Error,
    },
    /// Writing the file failed, or the scratch state behind it did.
    #[error("cannot write the persona file {path}: {source}")]
    Write {
        /// The file that was written.
        path: PathBuf,
        /// The underlying failure.
        source: io::Error,
    },
}

impl PersonaError {
    /// The stable machine-readable code the dashboard answers with.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TooLarge { .. } => "persona_too_large",
            Self::Read { .. } => "persona_unreadable",
            Self::Write { .. } => "persona_write_failed",
        }
    }
}

/// Where the persona lives and whether it is on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonaConfig {
    path: PathBuf,
    enabled_by_default: bool,
    inline_text: Option<String>,
}

impl PersonaConfig {
    /// A persona at one path, enabled, with no inline fallback.
    #[must_use]
    pub fn at_path(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            enabled_by_default: true,
            inline_text: None,
        }
    }

    /// A persona with every part spelled out.
    #[must_use]
    pub fn new(
        path: impl Into<PathBuf>,
        enabled_by_default: bool,
        inline_text: Option<String>,
    ) -> Self {
        Self {
            path: path.into(),
            enabled_by_default,
            inline_text,
        }
    }

    /// The `[persona]` table applied to the paths a run resolved.
    ///
    /// `path` is tilde-expanded, so `~/.config/kallisti/persona.md` written by
    /// hand in `config.toml` points at the same file the default does.
    #[must_use]
    pub fn from_toml(table: &crate::config::PersonaTable, paths: &Paths) -> Self {
        let path = table
            .path()
            .map(expand_tilde)
            .unwrap_or_else(|| paths.persona_file());
        Self::new(path, table.enabled(), table.text().map(str::to_owned))
    }

    /// The file this persona reads and writes.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What `[persona] enabled` says, when the dashboard has not overridden it.
    #[must_use]
    pub const fn enabled_by_default(&self) -> bool {
        self.enabled_by_default
    }

    /// The inline text, used only when the file is absent.
    #[must_use]
    pub fn inline_text(&self) -> Option<&str> {
        self.inline_text.as_deref()
    }

    /// Whether the persona is on, given the stored dashboard row.
    ///
    /// The row wins when it parses; anything else falls back to the config
    /// default, so a corrupt row cannot silently switch a persona on.
    #[must_use]
    pub fn effective_enabled(&self, stored: Option<&str>) -> bool {
        match stored.map(str::trim) {
            Some("true") => true,
            Some("false") => false,
            _ => self.enabled_by_default,
        }
    }

    /// The text the settings page shows: the file when it exists, else the
    /// inline fallback, else nothing.
    ///
    /// This ignores the on/off flag on purpose: a disabled persona still has
    /// text, and showing an empty box would invite the operator to retype it.
    ///
    /// # Errors
    ///
    /// [`PersonaError::Read`] when the file exists but cannot be read, and
    /// [`PersonaError::TooLarge`] when it is over the cap.
    pub fn read_text(&self) -> Result<String, PersonaError> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.inline_text.clone().unwrap_or_default()
            }
            Err(source) => {
                return Err(PersonaError::Read {
                    path: self.path.clone(),
                    source,
                });
            }
        };
        if text.len() > MAX_PERSONA_BYTES {
            return Err(PersonaError::TooLarge {
                path: self.path.clone(),
                actual: text.len(),
                limit: MAX_PERSONA_BYTES,
            });
        }
        Ok(text)
    }

    /// The system block for one turn, or `None` when there is nothing to send.
    ///
    /// Two files make up the block, and they answer different questions:
    /// `persona.md` says who the assistant is, and `context.md` beside it says
    /// what this host is - which projects exist, where they live, what they do.
    /// Identity is switchable; environment facts are not, because a turn that
    /// forgets the box is on cannot answer a question about the box.
    ///
    /// `None` is the ordinary case: no persona configured, the persona switched
    /// off, and no context file. A turn never fails because a file is missing.
    ///
    /// # Errors
    ///
    /// A file that exists and cannot be read, or one over the cap. Both are
    /// operator mistakes, and both refuse the turn rather than send a prompt the
    /// operator did not ask for.
    pub fn load(&self, stored_enabled: Option<&str>) -> Result<Option<String>, PersonaError> {
        let identity = if self.effective_enabled(stored_enabled) {
            let text = self.read_text()?;
            if text.trim().is_empty() {
                None
            } else {
                Some(text)
            }
        } else {
            None
        };
        let context = self.read_context()?;

        Ok(match (identity, context) {
            (None, None) => None,
            (Some(identity), None) => Some(identity),
            (None, Some(context)) => Some(context),
            (Some(identity), Some(context)) => Some(format!("{identity}\n\n{context}")),
        })
    }

    /// The capability note every turn carries, whether or not a persona exists.
    ///
    /// This is the prompt-side half of the transcript's file rendering: the page
    /// can draw a workspace file, but only the model can decide that showing one
    /// is the answer. It is deliberately short and path-only, and it forbids the
    /// two things the renderer will not accept (base64 and an absolute path).
    pub const CAPABILITIES: &'static str = "\
Files and pictures you produce can be shown in the transcript.\n\
- A picture: ![caption](relative/path.png) on its own line.\n\
- Any other file (PDF, PPTX, DOCX, XLSX, CSV, text): [name](relative/path.pdf).\n\
The path is relative to the workspace and must be one a tool result named; do not \
invent a path and do not paste base64. Use at most a few per answer, each next to \
the sentence that explains it.";

    /// How a turn asks instead of guessing.
    ///
    /// This is the 1.0 form of the old `clarify` card. There is no callback and no
    /// mid-turn wait: the transcript is the resume path. A question ends the turn,
    /// the operator answers in the same conversation, and the next turn continues
    /// from it. Saying that in the prompt is what makes an ask a normal move
    /// rather than a failure, which is why a turn used to guess or stall instead.
    pub const ASKING: &'static str = "\
Do the work; do not stall on needless questions. Ask only when the answer depends on \
a decision only the operator can make, or on a fact no tool can supply and you would \
otherwise have to guess a value. When you must ask: one question, at the very end of \
your answer, naming the options you would accept. Never guess a credential, and never \
invent a number or a fact you could have read or asked for.";

    /// `text` with the capability notes appended, or the notes alone.
    #[must_use]
    pub fn with_capabilities(text: Option<String>) -> Option<String> {
        let notes = format!("{}\n\n{}", Self::CAPABILITIES, Self::ASKING);
        Some(match text {
            Some(text) if !text.trim().is_empty() => format!("{text}\n\n{notes}"),
            _ => notes,
        })
    }

    /// Where the environment context lives: `context.md` beside the persona.
    #[must_use]
    pub fn context_path(&self) -> PathBuf {
        self.path.with_file_name(CONTEXT_FILE_NAME)
    }

    /// The environment facts for this turn, or `None` when there are none.
    ///
    /// Absent is normal and silent. Present but unreadable or oversized is an
    /// operator mistake and is reported, never truncated: half a set of facts
    /// about the host is worse than none, because the model cannot tell which
    /// half it is missing.
    fn read_context(&self) -> Result<Option<String>, PersonaError> {
        let path = self.context_path();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(PersonaError::Read { path, source }),
        };
        if text.len() > MAX_PERSONA_BYTES {
            return Err(PersonaError::TooLarge {
                path,
                actual: text.len(),
                limit: MAX_PERSONA_BYTES,
            });
        }
        if text.trim().is_empty() {
            return Ok(None);
        }
        Ok(Some(text))
    }

    /// Write the persona text at mode 0644.
    ///
    /// The parent directory is created when it is missing, which is the
    /// onboarding case: `~/.config/kallisti` exists by then, a scratch root's
    /// `config/` may not.
    ///
    /// # Errors
    ///
    /// [`PersonaError::Write`] when the directory or the file cannot be written.
    /// The cap is enforced by the caller, which has the status code for it.
    pub fn write(&self, text: &str) -> Result<(), PersonaError> {
        if let Some(parent) = self.path.parent()
            && let Err(source) = fs::create_dir_all(parent)
        {
            return Err(PersonaError::Write {
                path: self.path.clone(),
                source,
            });
        }
        if let Err(source) = fs::write(&self.path, text) {
            return Err(PersonaError::Write {
                path: self.path.clone(),
                source,
            });
        }
        set_world_readable(&self.path)
    }
}

/// Mode 0644: readable, so a backup or an operator's editor can read it, and not
/// executable. It is not a secret and is deliberately not treated as one.
fn set_world_readable(path: &Path) -> Result<(), PersonaError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(source) = fs::set_permissions(path, fs::Permissions::from_mode(0o644)) {
            return Err(PersonaError::Write {
                path: path.to_path_buf(),
                source,
            });
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// The character count the dashboard shows and the audit row records: the text
/// as the model would receive it, counted in chars, not bytes.
#[must_use]
pub fn character_count(text: &str) -> usize {
    text.chars().count()
}

/// Turn a config error into a persona refusal, for the settings page.
#[must_use]
pub fn config_error_code(error: &ConfigError) -> &'static str {
    match error {
        ConfigError::SecretInToml { .. } => "config_secret_rejected",
        _ => "config_error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_with_no_inline_text_is_not_an_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let config = PersonaConfig::at_path(dir.path().join("persona.md"));
        assert_eq!(config.load(None).expect("missing is fine"), None);
    }

    /// A turn with no persona at all still learns that it may ask. Asking is a
    /// capability, not something an operator has to write into their persona for
    /// it to be allowed, and the old clarify card is why that matters.
    #[test]
    fn every_turn_is_told_that_it_may_ask_and_how_the_answer_arrives() {
        let notes = PersonaConfig::with_capabilities(None).expect("notes");
        assert!(
            notes.contains("do not stall on needless questions"),
            "{notes}"
        );
        assert!(notes.contains("When you must ask"), "{notes}");
        assert!(
            notes.contains("naming the options you would accept"),
            "{notes}"
        );
        assert!(
            notes.contains("![caption]"),
            "the file note is kept: {notes}"
        );

        let with_persona =
            PersonaConfig::with_capabilities(Some("You are a daemon.".to_owned())).expect("notes");
        assert!(
            with_persona.starts_with("You are a daemon."),
            "{with_persona}"
        );
        assert!(
            with_persona.contains("at the very end of your answer"),
            "{with_persona}"
        );

        // An empty persona is the same case as no persona: notes only, no blank
        // paragraph where the operator's text would have been.
        let blank = PersonaConfig::with_capabilities(Some("   ".to_owned())).expect("notes");
        assert!(!blank.starts_with('\n'), "{blank}");
        assert!(blank.contains("When you must ask"), "{blank}");
    }

    #[test]
    fn the_context_file_is_appended_after_the_persona() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("persona.md");
        fs::write(&path, "you are Kallisti").expect("write persona");
        fs::write(dir.path().join("context.md"), "the warehouse is here").expect("write context");

        let combined = PersonaConfig::at_path(path).load(None).expect("loads");

        let combined = combined.expect("a system block");
        assert!(combined.contains("you are Kallisti"), "{combined}");
        assert!(combined.contains("the warehouse is here"), "{combined}");
        assert!(
            combined.find("you are Kallisti") < combined.find("the warehouse is here"),
            "identity first, environment second: {combined}"
        );
    }

    #[test]
    fn context_alone_still_reaches_the_turn_when_identity_is_off() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(dir.path().join("context.md"), "the warehouse is here").expect("write context");

        let config = PersonaConfig::new(dir.path().join("persona.md"), false, None);
        let block = config.load(None).expect("loads");

        let block = block.expect("environment facts are not identity, so they still ride");
        assert!(block.contains("the warehouse is here"), "{block}");
    }

    #[test]
    fn a_missing_context_file_leaves_the_persona_alone() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("persona.md");
        fs::write(&path, "you are Kallisti").expect("write persona");

        let block = PersonaConfig::at_path(path).load(None).expect("loads");

        assert_eq!(block, Some("you are Kallisti".to_owned()));
    }

    #[test]
    fn the_file_wins_over_the_inline_text() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("persona.md");
        let config = PersonaConfig::new(path.clone(), true, Some("inline".to_owned()));
        fs::write(&path, "from the file").expect("write");
        assert_eq!(
            config.load(None).expect("loads"),
            Some("from the file".to_owned())
        );
    }

    #[test]
    fn the_inline_text_is_used_only_when_the_file_is_absent() {
        let dir = tempfile::tempdir().expect("temp dir");
        let config = PersonaConfig::new(
            dir.path().join("persona.md"),
            true,
            Some("inline".to_owned()),
        );
        assert_eq!(config.load(None).expect("loads"), Some("inline".to_owned()));
    }

    #[test]
    fn a_disabled_persona_is_not_read_at_all() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("persona.md");
        // Deliberately unreadable content: a disabled persona must not touch it.
        fs::write(&path, "x".repeat(MAX_PERSONA_BYTES + 10)).expect("write");
        let config = PersonaConfig::at_path(path);
        assert_eq!(config.load(Some("false")).expect("disabled is fine"), None);
        assert!(!config.effective_enabled(Some("false")));
    }

    #[test]
    fn an_oversized_file_is_refused_with_both_numbers() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("persona.md");
        fs::write(&path, "x".repeat(MAX_PERSONA_BYTES + 10)).expect("write");
        let config = PersonaConfig::at_path(path);
        let error = config.load(None).expect_err("over the cap");
        assert_eq!(error.code(), "persona_too_large");
        let rendered = error.to_string();
        assert!(rendered.contains(&(MAX_PERSONA_BYTES + 10).to_string()));
        assert!(rendered.contains(&MAX_PERSONA_BYTES.to_string()));
    }

    #[test]
    fn the_stored_row_wins_over_the_config_default() {
        let config = PersonaConfig::at_path("/tmp/persona.md");
        assert!(
            config.effective_enabled(None),
            "the documented default is on"
        );
        assert!(!config.effective_enabled(Some("false")));
        assert!(config.effective_enabled(Some("true")));
        // A row nobody can parse is the default, not "on" by accident.
        assert!(config.effective_enabled(Some("maybe")));
        let off = PersonaConfig::new("/tmp/persona.md", false, None);
        assert!(!off.effective_enabled(None));
        assert!(!off.effective_enabled(Some("nonsense")));
    }

    #[test]
    fn a_write_creates_the_directory_and_lands_at_0644() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("nested/config/persona.md");
        PersonaConfig::at_path(&path)
            .write("hello")
            .expect("write creates the parent");
        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
        assert_eq!(fs::read_to_string(&path).expect("read"), "hello");
    }
}
