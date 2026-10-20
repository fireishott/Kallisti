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
/// They are separate because a name is a label ("Message Ignyte"), not a claim
/// the model has to be told about on every turn.
pub const SETTING_ASSISTANT_NAME: &str = "assistant.name";

/// The name the dashboard uses before the operator sets one.
pub const DEFAULT_ASSISTANT_NAME: &str = "Ignyte";

/// The `settings` row that remembers the operator's on/off choice.
///
/// The `[persona] enabled` key in `config.toml` is the default; a row here is
/// the operator's later decision from the dashboard, and it wins.
pub const SETTING_PERSONA_ENABLED: &str = "assistant.persona_enabled";

/// The documented default path, `~/.config/kallisti/persona.md`.
pub const DEFAULT_PERSONA_PATH: &str = "~/.config/kallisti/persona.md";

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
    /// `None` is the ordinary case: no persona configured, the persona switched
    /// off, or an empty file. A turn never fails because the file is missing.
    ///
    /// # Errors
    ///
    /// A file that exists and cannot be read, or one over the cap. Both are
    /// operator mistakes, and both refuse the turn rather than send a prompt the
    /// operator did not ask for.
    pub fn load(&self, stored_enabled: Option<&str>) -> Result<Option<String>, PersonaError> {
        if !self.effective_enabled(stored_enabled) {
            return Ok(None);
        }
        let text = self.read_text()?;
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
