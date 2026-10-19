//! Reading `~/.config/kallisti/eris.env`.
//!
//! 1.0 has exactly one config surface, and it is a small `KEY=VALUE` file that is
//! required to be mode `0600`. There is no TOML layer, no environment overlay,
//! no dotenv crate: a probe either reads a file only the owner can read, or it
//! refuses to run.
//!
//! The file holds a **reference** to the key, never the key:
//!
//! ```text
//! KALLISTI_BASE_URL=http://127.0.0.1:20128/v1
//! KALLISTI_MODEL=ds/deepseek-v4-flash
//! KALLISTI_API_KEY_REF=env:NINEROUTER_API_KEY
//! ```
//!
//! Values are never logged and never echoed, so a config error names the *key*
//! and not the value.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use core_domain::SecretRef;

/// Required: the OpenAI-compatible base URL.
pub const KEY_BASE_URL: &str = "KALLISTI_BASE_URL";
/// Required: the model id to ask for.
pub const KEY_MODEL: &str = "KALLISTI_MODEL";
/// Required: where the key comes from, as `env:NAME`.
pub const KEY_API_KEY_REF: &str = "KALLISTI_API_KEY_REF";
/// Optional: output cap for one turn.
pub const KEY_MAX_OUTPUT_TOKENS: &str = "KALLISTI_MAX_OUTPUT_TOKENS";

/// Mode the config file must have: owner read and write, nothing else.
pub const REQUIRED_MODE: u32 = 0o600;

/// Why the config could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file could not be read.
    #[error("cannot read config file {path}: {source}")]
    Read {
        /// Path that failed.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// The file is readable by someone other than its owner.
    #[error(
        "config file {path} has mode {actual:04o}, but {expected:04o} is required: run `chmod 0600 {path}`"
    )]
    InsecurePermissions {
        /// Path that failed the check.
        path: PathBuf,
        /// Mode found, masked to permission bits.
        actual: u32,
        /// Mode required.
        expected: u32,
    },
    /// A required key was absent.
    #[error("config file {path} is missing {key}")]
    MissingKey {
        /// Path that was read.
        path: PathBuf,
        /// Key that was absent.
        key: &'static str,
    },
    /// A line was not `KEY=VALUE`.
    #[error("config file {path} line {line} is not KEY=VALUE")]
    MalformedLine {
        /// Path that was read.
        path: PathBuf,
        /// 1-based line number.
        line: usize,
    },
    /// The key reference was not `env:NAME`.
    #[error("config file {path} has an unusable {key}: {source}")]
    BadSecretRef {
        /// Path that was read.
        path: PathBuf,
        /// Key that failed.
        key: &'static str,
        /// Why it failed.
        source: core_domain::SecretRefError,
    },
    /// A numeric field did not parse.
    #[error("config file {path} has a non-numeric {key}")]
    BadNumber {
        /// Path that was read.
        path: PathBuf,
        /// Key that failed.
        key: &'static str,
    },
    /// `$HOME` is not set, so the default path cannot be resolved.
    #[error("HOME is not set, so the config path cannot be resolved")]
    NoHome,
}

/// The parsed config. Carries a key *reference*, never a key.
#[derive(Clone, PartialEq, Eq)]
pub struct KallistiConfig {
    base_url: String,
    model: String,
    api_key_ref: SecretRef,
    max_output_tokens: Option<u32>,
    path: PathBuf,
}

impl fmt::Debug for KallistiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KallistiConfig")
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .field("api_key_ref", &self.api_key_ref)
            .field("max_output_tokens", &self.max_output_tokens)
            .field("path", &self.path)
            .finish()
    }
}

impl KallistiConfig {
    /// The default config path, `~/.config/kallisti/eris.env`.
    pub fn default_path() -> Result<PathBuf, ConfigError> {
        let home = std::env::var("HOME").map_err(|_| ConfigError::NoHome)?;
        Ok(Path::new(&home).join(".config/kallisti/eris.env"))
    }

    /// Load and validate a config file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref().to_path_buf();

        let metadata = fs::metadata(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        if !metadata.is_file() {
            return Err(ConfigError::Read {
                path: path.clone(),
                source: std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "config path is not a regular file",
                ),
            });
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = metadata.permissions().mode() & 0o777;
            if mode != REQUIRED_MODE {
                return Err(ConfigError::InsecurePermissions {
                    path: path.clone(),
                    actual: mode,
                    expected: REQUIRED_MODE,
                });
            }
        }

        let text = fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        Self::parse(&text, &path)
    }

    /// Parse config text that has already been read and validated.
    ///
    /// The format is a subset of shell assignment: blank lines, `#` comments,
    /// `KEY=VALUE`, and an optional `export ` prefix so a hand-written file that
    /// someone sourced once still loads. Quotes around a value are stripped.
    pub fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        let mut base_url = None;
        let mut model = None;
        let mut api_key_ref = None;
        let mut max_output_tokens = None;

        for (index, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
            let Some((key, value)) = line.split_once('=') else {
                return Err(ConfigError::MalformedLine {
                    path: path.to_path_buf(),
                    line: index + 1,
                });
            };
            let key = key.trim();
            let value = unquote(value.trim());

            match key {
                KEY_BASE_URL => base_url = Some(value.to_owned()),
                KEY_MODEL => model = Some(value.to_owned()),
                KEY_API_KEY_REF => {
                    let parsed =
                        SecretRef::try_from(value).map_err(|source| ConfigError::BadSecretRef {
                            path: path.to_path_buf(),
                            key: KEY_API_KEY_REF,
                            source,
                        })?;
                    api_key_ref = Some(parsed);
                }
                KEY_MAX_OUTPUT_TOKENS => {
                    let parsed = value.parse::<u32>().map_err(|_| ConfigError::BadNumber {
                        path: path.to_path_buf(),
                        key: KEY_MAX_OUTPUT_TOKENS,
                    })?;
                    max_output_tokens = Some(parsed);
                }
                // Unknown keys are ignored so a later phase can add one without
                // breaking an older binary. No value is ever echoed.
                _ => {}
            }
        }

        let require = |value: Option<String>, key: &'static str| {
            value.ok_or_else(|| ConfigError::MissingKey {
                path: path.to_path_buf(),
                key,
            })
        };

        Ok(Self {
            base_url: require(base_url, KEY_BASE_URL)?,
            model: require(model, KEY_MODEL)?,
            api_key_ref: api_key_ref.ok_or_else(|| ConfigError::MissingKey {
                path: path.to_path_buf(),
                key: KEY_API_KEY_REF,
            })?,
            max_output_tokens,
            path: path.to_path_buf(),
        })
    }

    /// The OpenAI-compatible base URL.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The configured model id.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Where the key comes from. A reference, not a value.
    #[must_use]
    pub fn api_key_ref(&self) -> &SecretRef {
        &self.api_key_ref
    }

    /// Output cap for one turn, when configured.
    #[must_use]
    pub fn max_output_tokens(&self) -> Option<u32> {
        self.max_output_tokens
    }

    /// The file this was read from.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Strip one layer of matching single or double quotes.
fn unquote(value: &str) -> &str {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' || first == b'\'') && first == last {
            return &value[1..value.len() - 1];
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# Kallisti 1.0 config
KALLISTI_BASE_URL=http://127.0.0.1:20128/v1
KALLISTI_MODEL=ds/deepseek-v4-flash
KALLISTI_API_KEY_REF=env:NINEROUTER_API_KEY
";

    fn sample_path() -> PathBuf {
        PathBuf::from("/tmp/eris.env")
    }

    #[test]
    fn the_documented_file_parses() {
        let config = KallistiConfig::parse(SAMPLE, &sample_path()).expect("config should parse");
        assert_eq!(config.base_url(), "http://127.0.0.1:20128/v1");
        assert_eq!(config.model(), "ds/deepseek-v4-flash");
        assert_eq!(
            config.api_key_ref().env_name(),
            "NINEROUTER_API_KEY",
            "the file carries a reference, and only a reference"
        );
        assert_eq!(config.max_output_tokens(), None);
    }

    #[test]
    fn debug_renders_the_reference_and_never_a_resolved_key() {
        let config = KallistiConfig::parse(SAMPLE, &sample_path()).expect("config should parse");
        let rendered = format!("{config:?}");
        // The variable name is not a secret; it is the whole point of a
        // reference. What must never appear is a materialised key.
        assert!(rendered.contains("NINEROUTER_API_KEY"), "{rendered}");
        assert!(!rendered.contains("SecretKey"), "{rendered}");
        assert!(!rendered.contains("Bearer"), "{rendered}");
    }

    #[test]
    fn comments_blanks_quotes_and_export_are_tolerated() {
        let text = "\
export KALLISTI_BASE_URL=\"http://127.0.0.1:11434/v1\"

# a comment
KALLISTI_MODEL='llama3'
KALLISTI_API_KEY_REF=env:OLLAMA_KEY
KALLISTI_MAX_OUTPUT_TOKENS=64
KALLISTI_UNKNOWN_KEY=ignored
";
        let config = KallistiConfig::parse(text, &sample_path()).expect("config should parse");
        assert_eq!(config.base_url(), "http://127.0.0.1:11434/v1");
        assert_eq!(config.model(), "llama3");
        assert_eq!(config.max_output_tokens(), Some(64));
    }

    #[test]
    fn a_missing_required_key_names_the_key() {
        let text = "KALLISTI_BASE_URL=http://127.0.0.1:20128/v1\nKALLISTI_MODEL=m\n";
        let error = KallistiConfig::parse(text, &sample_path()).expect_err("must fail");
        assert!(error.to_string().contains(KEY_API_KEY_REF), "{error}");
    }

    #[test]
    fn a_literal_key_in_the_config_is_rejected_as_a_reference() {
        // The failure mode this catches: someone pastes the key value itself.
        let text = "\
KALLISTI_BASE_URL=http://127.0.0.1:20128/v1
KALLISTI_MODEL=m
KALLISTI_API_KEY_REF=sk-not-a-reference
";
        let error = KallistiConfig::parse(text, &sample_path()).expect_err("must fail");
        assert!(
            matches!(error, ConfigError::BadSecretRef { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn a_line_without_an_equals_sign_is_rejected() {
        let error =
            KallistiConfig::parse("KALLISTI_BASE_URL\n", &sample_path()).expect_err("must fail");
        assert!(matches!(error, ConfigError::MalformedLine { line: 1, .. }));
    }

    #[test]
    fn a_mode_other_than_0600_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("eris.env");
        std::fs::write(&path, SAMPLE).expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        let error = KallistiConfig::load(&path).expect_err("0644 must be refused");
        match error {
            ConfigError::InsecurePermissions {
                actual, expected, ..
            } => {
                assert_eq!(actual, 0o644);
                assert_eq!(expected, 0o600);
            }
            other => panic!("expected InsecurePermissions, got {other:?}"),
        }

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        let config = KallistiConfig::load(&path).expect("0600 must load");
        assert_eq!(config.model(), "ds/deepseek-v4-flash");
    }
}
