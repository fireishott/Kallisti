//! Config: the secret half and the non-secret half.
//!
//! There are exactly two files, and they hold different kinds of thing.
//!
//! `~/.config/kallisti/eris.env` (mode `0600`, required):
//!
//! ```text
//! KALLISTI_BASE_URL=http://127.0.0.1:20128/v1
//! KALLISTI_MODEL=ds/deepseek-v4-flash
//! KALLISTI_API_KEY_REF=env:NINEROUTER_API_KEY
//! ```
//!
//! It holds a **reference** to the key, never the key. The loader refuses to
//! run if the file is group- or world-readable, so a probe or a server either
//! reads a file only the owner can read, or it refuses to start.
//!
//! `~/.config/kallisti/config.toml` (optional, no mode requirement, because it
//! holds nothing secret):
//!
//! ```toml
//! workspace_path = "~/kallisti-workspace"
//! log_level = "info"
//! ```
//!
//! and one optional `[admin]` table, which is how the UI is reached:
//!
//! ```toml
//! [admin]
//! listen = ["127.0.0.1:7340"]                   # every address is served at once
//! public_url = "https://kallisti.example.com"   # absolute links, cookie security
//! trusted_proxies = ["127.0.0.1/32", "::1/128"] # peers that may set forwarding headers
//! ```
//!
//! Every key is optional and none is a secret. An absent `listen` means exactly
//! `127.0.0.1:7340`; an absent `trusted_proxies` means loopback only; an absent
//! `public_url` means the operator reaches the daemon by address.
//!
//! The schema is deliberately not extensible by accident. A key that looks like
//! a secret is rejected by name *before* deserialisation, so the error says why:
//! a secret value belongs in the environment behind a reference, never in a file
//! that is world-readable. Unknown keys are refused by serde. Values are never
//! logged and never echoed, so a config error names the *key* and not the value.

use std::fmt;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use core_domain::SecretRef;
use serde::Deserialize;

use crate::admin::listen::{ListenError, parse_listen_list};
use crate::web::proxy::{AdminAccess, ProxyConfigError};

/// Required: the OpenAI-compatible base URL.
pub const KEY_BASE_URL: &str = "KALLISTI_BASE_URL";
/// Required: the model id to ask for.
pub const KEY_MODEL: &str = "KALLISTI_MODEL";
/// Required: where the key comes from, as `env:NAME`.
pub const KEY_API_KEY_REF: &str = "KALLISTI_API_KEY_REF";
/// Optional: output cap for one turn.
pub const KEY_MAX_OUTPUT_TOKENS: &str = "KALLISTI_MAX_OUTPUT_TOKENS";

/// Mode the secret file must have: owner read and write, nothing else.
pub const REQUIRED_MODE: u32 = 0o600;

/// Config keys that would put a secret in a world-readable file. Rejected on
/// sight. `key` covers `api_key` and `apikey`; `credential` covers the plural.
pub const FORBIDDEN_SECRET_KEYS: &[&str] = &[
    "key",
    "secret",
    "token",
    "password",
    "passphrase",
    "credential",
];

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
    #[error("HOME is not set, so the config paths cannot be resolved")]
    NoHome,
    /// The TOML file could not be read.
    #[error("cannot read config file {path}: {source}")]
    TomlRead {
        /// Path that failed.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// The TOML file did not parse, or carried a key outside the two-key schema.
    #[error("config file {path} is not valid: {source}")]
    TomlParse {
        /// Path that was read.
        path: PathBuf,
        /// Parse error.
        source: toml::de::Error,
    },
    /// `admin.listen` could not be used.
    #[error("config file {path} has an unusable admin.listen: {source}")]
    BadListen {
        /// Path that was read.
        path: PathBuf,
        /// Why the list was refused.
        source: ListenError,
    },
    /// `admin.public_url` or `admin.trusted_proxies` could not be used.
    #[error("config file {path} has an unusable admin setting: {source}")]
    BadAdminSetting {
        /// Path that was read.
        path: PathBuf,
        /// Why the setting was refused.
        source: ProxyConfigError,
    },
    /// A secret-looking key was present. Secrets do not live in this file.
    #[error(
        "config file {path} carries the key {key}, but config.toml must not hold a secret: keep the value in the environment and put only a reference in eris.env"
    )]
    SecretInToml {
        /// Path that was read.
        path: PathBuf,
        /// Key path that was rejected.
        key: String,
    },
    /// `log_level` was not one of the five tracing levels.
    #[error(
        "config file {path} has an unusable log_level; use one of trace, debug, info, warn, error"
    )]
    BadLogLevel {
        /// Path that was read.
        path: PathBuf,
        /// The value that was rejected. Not a secret, but reported by name only.
        value: String,
    },
}

/// The parsed secret-half config. Carries a key *reference*, never a key.
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
        Ok(Paths::from_home()?.env_file)
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

        if let Some(mode) = file_mode(&metadata)
            && mode != REQUIRED_MODE
        {
            return Err(ConfigError::InsecurePermissions {
                path: path.clone(),
                actual: mode,
                expected: REQUIRED_MODE,
            });
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

/// Every path 1.0 touches under `$HOME`.
#[derive(Debug, Clone)]
pub struct Paths {
    /// `~/.config/kallisti`
    pub config_dir: PathBuf,
    /// `~/.config/kallisti/eris.env` - secret references, mode `0600`.
    pub env_file: PathBuf,
    /// `~/.config/kallisti/config.toml` - non-secret settings, optional.
    pub toml_file: PathBuf,
    /// `~/.local/share/kallisti`
    pub state_dir: PathBuf,
    /// `~/.local/share/kallisti/eris.db`
    pub db_file: PathBuf,
    /// `~/.config/kallisti/tokens` - encrypted token records, mode `0700`.
    pub token_dir: PathBuf,
    /// `~/.config/kallisti/keys/token.key` - the AEAD key, mode `0600`.
    pub token_key_file: PathBuf,
}

impl Paths {
    /// Resolve the defaults from `$HOME`.
    pub fn from_home() -> Result<Self, ConfigError> {
        let home = std::env::var("HOME").map_err(|_| ConfigError::NoHome)?;
        let home = PathBuf::from(home);
        Ok(Self {
            config_dir: home.join(".config/kallisti"),
            env_file: home.join(".config/kallisti/eris.env"),
            toml_file: home.join(".config/kallisti/config.toml"),
            state_dir: home.join(".local/share/kallisti"),
            db_file: home.join(".local/share/kallisti/eris.db"),
            token_dir: home.join(".config/kallisti/tokens"),
            token_key_file: home.join(".config/kallisti/keys/token.key"),
        })
    }

    /// The persona file, `persona.md` in the config directory.
    ///
    /// Derived rather than stored so a scratch root from [`Paths::for_root`]
    /// gets its own persona: no test can read the operator's real one, and an
    /// acceptance run cannot edit it.
    #[must_use]
    pub fn persona_file(&self) -> PathBuf {
        self.config_dir.join("persona.md")
    }

    /// The same four paths under an explicit root.
    ///
    /// This exists so a test or an acceptance run can exercise the first-run
    /// state machine without touching the real database: `--state-dir` points at
    /// a scratch root and every path moves under it, `config/`, `state/`, the
    /// database and `first-run.txt` included. Nothing about the real
    /// `~/.local/share/kallisti/eris.db` is read or written in that mode.
    ///
    /// The layout mirrors `$HOME` on purpose, so a scratch run follows the same
    /// relative shape the installed daemon uses.
    #[must_use]
    pub fn for_root(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let config_dir = root.join("config");
        let state_dir = root.join("state");
        Self {
            env_file: config_dir.join("eris.env"),
            toml_file: config_dir.join("config.toml"),
            db_file: state_dir.join("eris.db"),
            token_dir: config_dir.join("tokens"),
            token_key_file: config_dir.join("keys/token.key"),
            config_dir,
            state_dir,
        }
    }
}

/// Log level, as spelled in `config.toml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    /// `trace`
    Trace,
    /// `debug`
    Debug,
    /// `info`
    Info,
    /// `warn`
    Warn,
    /// `error`
    Error,
}

impl LogLevel {
    /// Default when `config.toml` does not say.
    pub const DEFAULT: Self = Self::Info;

    /// The `tracing` level this maps to.
    #[must_use]
    pub fn as_tracing(self) -> tracing::Level {
        match self {
            Self::Trace => tracing::Level::TRACE,
            Self::Debug => tracing::Level::DEBUG,
            Self::Info => tracing::Level::INFO,
            Self::Warn => tracing::Level::WARN,
            Self::Error => tracing::Level::ERROR,
        }
    }

    /// The lowercase name used in config and in doctor output.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "trace" => Some(Self::Trace),
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" | "warning" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

/// The parsed non-secret half. Every key is optional.
#[derive(Debug, Clone, Default)]
pub struct TomlConfig {
    workspace_path: Option<PathBuf>,
    log_level: Option<LogLevel>,
    admin: AdminConfig,
    persona: PersonaTable,
    oauth: OAuthTable,
    source: Option<PathBuf>,
}

/// What deserialisation actually accepts. `deny_unknown_fields` is the wall that
/// refuses a key nobody documented.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TomlConfigRaw {
    workspace_path: Option<PathBuf>,
    log_level: Option<String>,
    admin: Option<AdminRaw>,
    persona: Option<PersonaRaw>,
    oauth: Option<OAuthRaw>,
}

/// The `[persona]` table, before validation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersonaRaw {
    enabled: Option<bool>,
    path: Option<PathBuf>,
    text: Option<String>,
}

/// The `[persona]` table: whether the assistant has an identity, and where it
/// lives.
///
/// Every key is optional. `enabled` defaults to `true` because a persona file
/// that exists is a decision the operator already made by writing it; `path`
/// defaults to `~/.config/kallisti/persona.md`; `text` is used only when that
/// file is absent, which is the case for a deployment that wants the text in
/// the config rather than a second file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonaTable {
    enabled: bool,
    path: Option<PathBuf>,
    text: Option<String>,
}

impl Default for PersonaTable {
    fn default() -> Self {
        Self {
            enabled: true,
            path: None,
            text: None,
        }
    }
}

impl PersonaTable {
    /// Whether the persona is on, when the dashboard has not said otherwise.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// The configured path, before tilde expansion.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The inline fallback text.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        self.text.as_deref()
    }
}

/// The `[oauth]` table, before validation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OAuthRaw {
    auto_refresh: Option<bool>,
}

/// The `[oauth]` table: whether a stored token may be refreshed on use.
///
/// **Defaults to false, in the parser and not only in the file on disk.** A
/// missing key means false. While it is false nothing refreshes, ever: a grant
/// that is inside its refresh lead is reported as needing a login, and a grant
/// copied out of another program is reported as expired. Build 108 never turns
/// it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OAuthTable {
    auto_refresh: bool,
}

impl Default for OAuthTable {
    fn default() -> Self {
        Self {
            auto_refresh: false,
        }
    }
}

impl OAuthTable {
    /// `oauth.auto_refresh`, false unless the operator wrote true.
    #[must_use]
    pub const fn auto_refresh(&self) -> bool {
        self.auto_refresh
    }
}

/// The `[admin]` table, before validation.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdminRaw {
    listen: Option<Vec<String>>,
    public_url: Option<String>,
    trusted_proxies: Option<Vec<String>>,
}

/// The `[admin]` table, validated: how the UI is reached and who may front it.
#[derive(Debug, Clone)]
pub struct AdminConfig {
    listen: Vec<SocketAddr>,
    access: AdminAccess,
}

impl Default for AdminConfig {
    fn default() -> Self {
        Self {
            listen: parse_listen_list(&[]).unwrap_or_else(|_| {
                vec![
                    "127.0.0.1:7340"
                        .parse()
                        .expect("the documented default is a literal ip:port"),
                ]
            }),
            access: AdminAccess::loopback_only(),
        }
    }
}

impl AdminConfig {
    /// Every address the daemon will bind, in order. Never empty.
    #[must_use]
    pub fn listen(&self) -> &[SocketAddr] {
        &self.listen
    }

    /// `admin.public_url`, when the operator configured one.
    #[must_use]
    pub fn public_url(&self) -> Option<&str> {
        self.access.public_url.as_deref()
    }

    /// The access model: trusted proxies and the public URL.
    #[must_use]
    pub fn access(&self) -> &AdminAccess {
        &self.access
    }
}

impl TomlConfig {
    /// Defaults with no file behind them.
    #[must_use]
    pub fn defaults() -> Self {
        Self::default()
    }

    /// Load `config.toml`. A missing file is not an error; it means defaults.
    ///
    /// There is deliberately no permission check here: this file must never
    /// hold a secret, and the key-name scan refuses one if it appears.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        match fs::read_to_string(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::defaults()),
            Err(source) => Err(ConfigError::TomlRead {
                path: path.to_path_buf(),
                source,
            }),
            Ok(text) => Self::parse(&text, path),
        }
    }

    /// Parse `config.toml` text.
    pub fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        // Parse to a generic value first so a rejected key can be named before
        // serde turns it into "unknown field".
        let value: toml::Value = toml::from_str(text).map_err(|source| ConfigError::TomlParse {
            path: path.to_path_buf(),
            source,
        })?;
        reject_forbidden_keys(&value, "", path)?;

        let raw: TomlConfigRaw = toml::from_str(text).map_err(|source| ConfigError::TomlParse {
            path: path.to_path_buf(),
            source,
        })?;

        let log_level = match raw.log_level.as_deref() {
            None => None,
            Some(name) => Some(
                LogLevel::parse(name).ok_or_else(|| ConfigError::BadLogLevel {
                    path: path.to_path_buf(),
                    value: name.to_owned(),
                })?,
            ),
        };

        let admin = match raw.admin {
            None => AdminConfig::default(),
            Some(raw_admin) => {
                let listen = parse_listen_list(raw_admin.listen.as_deref().unwrap_or_default())
                    .map_err(|source| ConfigError::BadListen {
                        path: path.to_path_buf(),
                        source,
                    })?;
                let access = AdminAccess::parse(
                    raw_admin.trusted_proxies.as_deref().unwrap_or_default(),
                    raw_admin.public_url.as_deref(),
                )
                .map_err(|source| ConfigError::BadAdminSetting {
                    path: path.to_path_buf(),
                    source,
                })?;
                AdminConfig { listen, access }
            }
        };

        Ok(Self {
            workspace_path: raw.workspace_path,
            log_level,
            admin,
            persona: match raw.persona {
                None => PersonaTable::default(),
                Some(persona) => PersonaTable {
                    enabled: persona.enabled.unwrap_or(true),
                    path: persona.path,
                    text: persona.text,
                },
            },
            oauth: match raw.oauth {
                None => OAuthTable::default(),
                Some(oauth) => OAuthTable {
                    auto_refresh: oauth.auto_refresh.unwrap_or(false),
                },
            },
            source: Some(path.to_path_buf()),
        })
    }

    /// Sandbox workspace for turns. Defaults to `~/kallisti-workspace`.
    #[must_use]
    pub fn workspace_path(&self) -> PathBuf {
        match &self.workspace_path {
            Some(path) => expand_tilde(path),
            None => expand_tilde(Path::new("~/kallisti-workspace")),
        }
    }

    /// Log level, defaulting to `info`.
    #[must_use]
    pub fn log_level(&self) -> LogLevel {
        self.log_level.unwrap_or(LogLevel::DEFAULT)
    }

    /// The `[admin]` table, validated.
    #[must_use]
    pub fn admin(&self) -> &AdminConfig {
        &self.admin
    }

    /// The `[persona]` table. Absent means the documented defaults.
    #[must_use]
    pub fn persona(&self) -> &PersonaTable {
        &self.persona
    }

    /// The `[oauth]` table. Absent means never refresh.
    #[must_use]
    pub fn oauth(&self) -> OAuthTable {
        self.oauth
    }

    /// The file this was read from, when there was one.
    #[must_use]
    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// Whether an explicit `workspace_path` was configured.
    #[must_use]
    pub fn workspace_path_is_default(&self) -> bool {
        self.workspace_path.is_none()
    }
}

/// Both halves, plus the paths they came from.
#[derive(Debug, Clone)]
pub struct LoadedConfig {
    eris: KallistiConfig,
    toml: TomlConfig,
    paths: Paths,
}

impl LoadedConfig {
    /// Load the secret half (strictly) and the non-secret half (optionally).
    ///
    /// This is the loader `serve` uses, so an `eris.env` that anyone else can
    /// read stops the daemon from starting at all.
    pub fn load(paths: Paths) -> Result<Self, ConfigError> {
        let eris = KallistiConfig::load(&paths.env_file)?;
        let toml = TomlConfig::load(&paths.toml_file)?;
        Ok(Self { eris, toml, paths })
    }

    /// The provider half, read from `eris.env`.
    #[must_use]
    pub fn eris(&self) -> &KallistiConfig {
        &self.eris
    }

    /// The non-secret half, read from `config.toml`.
    #[must_use]
    pub fn toml(&self) -> &TomlConfig {
        &self.toml
    }

    /// The paths these were loaded from.
    #[must_use]
    pub fn paths(&self) -> &Paths {
        &self.paths
    }
}

/// Reject secret-looking keys anywhere in the TOML tree.
fn reject_forbidden_keys(
    value: &toml::Value,
    prefix: &str,
    path: &Path,
) -> Result<(), ConfigError> {
    let Some(table) = value.as_table() else {
        return Ok(());
    };
    for (key, child) in table {
        let lowered = key.to_ascii_lowercase();
        let full = if prefix.is_empty() {
            lowered.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if FORBIDDEN_SECRET_KEYS
            .iter()
            .any(|needle| lowered.contains(needle))
        {
            return Err(ConfigError::SecretInToml {
                path: path.to_path_buf(),
                key: full,
            });
        }
        reject_forbidden_keys(child, &full, path)?;
    }
    Ok(())
}

/// Permission bits of a file, on unix. `None` elsewhere.
fn file_mode(metadata: &fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(metadata.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

/// Expand a leading `~` against `$HOME`. Anything else is returned unchanged.
pub(crate) fn expand_tilde(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    let Some(rest) = text.strip_prefix('~') else {
        return path.to_path_buf();
    };
    let Ok(home) = std::env::var("HOME") else {
        return path.to_path_buf();
    };
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    if rest.is_empty() {
        PathBuf::from(home)
    } else {
        Path::new(&home).join(rest)
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

    fn toml_path() -> PathBuf {
        PathBuf::from("/tmp/config.toml")
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

    #[test]
    fn a_group_readable_mode_is_refused_by_the_full_loader() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let env_file = dir.path().join("eris.env");
        std::fs::write(&env_file, SAMPLE).expect("write");
        std::fs::set_permissions(&env_file, std::fs::Permissions::from_mode(0o640)).expect("chmod");
        let paths = Paths {
            config_dir: dir.path().to_path_buf(),
            env_file,
            toml_file: dir.path().join("config.toml"),
            state_dir: dir.path().join("state"),
            db_file: dir.path().join("state/eris.db"),
            token_dir: dir.path().join("config/tokens"),
            token_key_file: dir.path().join("config/keys/token.key"),
        };

        let error = LoadedConfig::load(paths).expect_err("0640 must be refused");
        assert!(
            matches!(
                error,
                ConfigError::InsecurePermissions { actual: 0o640, .. }
            ),
            "{error:?}"
        );
    }

    #[test]
    fn the_full_loader_reads_both_halves() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("temp dir");
        let env_file = dir.path().join("eris.env");
        let toml_file = dir.path().join("config.toml");
        std::fs::write(&env_file, SAMPLE).expect("write");
        std::fs::set_permissions(&env_file, std::fs::Permissions::from_mode(0o600)).expect("chmod");
        std::fs::write(&toml_file, "log_level = \"debug\"\n").expect("write");

        let loaded = LoadedConfig::load(Paths {
            config_dir: dir.path().to_path_buf(),
            env_file,
            toml_file,
            state_dir: dir.path().join("state"),
            db_file: dir.path().join("state/eris.db"),
            token_dir: dir.path().join("config/tokens"),
            token_key_file: dir.path().join("config/keys/token.key"),
        })
        .expect("both halves should load");

        assert_eq!(loaded.eris().model(), "ds/deepseek-v4-flash");
        assert_eq!(loaded.toml().log_level(), LogLevel::Debug);
        assert!(
            loaded
                .toml()
                .workspace_path()
                .ends_with("kallisti-workspace")
        );
    }

    #[test]
    fn a_missing_config_toml_means_defaults() {
        let dir = tempfile::tempdir().expect("temp dir");
        let toml = TomlConfig::load(dir.path().join("absent.toml")).expect("missing is fine");
        assert_eq!(toml.log_level(), LogLevel::Info);
        assert_eq!(toml.source(), None);
        assert!(toml.workspace_path_is_default());
        assert!(toml.workspace_path().ends_with("kallisti-workspace"));
    }

    #[test]
    fn the_two_documented_keys_parse() {
        let toml = TomlConfig::parse(
            "workspace_path = \"/tmp/ws\"\nlog_level = \"warn\"\n",
            &toml_path(),
        )
        .expect("documented schema");
        assert_eq!(toml.workspace_path(), PathBuf::from("/tmp/ws"));
        assert_eq!(toml.log_level(), LogLevel::Warn);
        assert!(!toml.workspace_path_is_default());
    }

    #[test]
    fn config_toml_accepts_and_validates_the_admin_table() {
        // The admin address is config now, so `[admin] listen` parses.
        let toml = TomlConfig::parse(
            "[admin]\nlisten = [\"127.0.0.1:7340\", \"0.0.0.0:7340\"]\n",
            &toml_path(),
        )
        .expect("the admin listen list is a documented key");
        assert_eq!(
            toml.admin().listen(),
            &[
                "127.0.0.1:7340"
                    .parse::<std::net::SocketAddr>()
                    .expect("literal"),
                "0.0.0.0:7340"
                    .parse::<std::net::SocketAddr>()
                    .expect("literal"),
            ]
        );

        // An absent table is the default, and the default is still loopback.
        let bare = TomlConfig::parse("log_level = \"info\"\n", &toml_path()).expect("no admin");
        assert_eq!(
            bare.admin().listen(),
            &["127.0.0.1:7340"
                .parse::<std::net::SocketAddr>()
                .expect("literal")]
        );
        assert!(bare.admin().public_url().is_none());

        // public_url and trusted_proxies are validated here, so a typo is a
        // startup error and not a silent surprise later.
        let full = TomlConfig::parse(
            "[admin]\npublic_url = \"https://kallisti.example.com\"\n\
             trusted_proxies = [\"127.0.0.1/32\", \"192.168.1.0/24\"]\n",
            &toml_path(),
        )
        .expect("a documented admin table");
        assert_eq!(
            full.admin().public_url(),
            Some("https://kallisti.example.com")
        );
        assert!(
            full.admin()
                .access()
                .trusted
                .contains("192.168.1.20".parse().expect("ip"))
        );

        let bad_listen =
            TomlConfig::parse("[admin]\nlisten = [\"localhost:7340\"]\n", &toml_path())
                .expect_err("a hostname bind is refused");
        assert!(
            matches!(bad_listen, ConfigError::BadListen { .. }),
            "{bad_listen:?}"
        );

        let bad_proxy = TomlConfig::parse(
            "[admin]\ntrusted_proxies = [\"10.0.0.0/33\"]\n",
            &toml_path(),
        )
        .expect_err("a bad CIDR is refused");
        assert!(
            matches!(bad_proxy, ConfigError::BadAdminSetting { .. }),
            "{bad_proxy:?}"
        );

        // A stray key nobody documented is still refused by serde.
        for text in [
            "bind_addr = \"0.0.0.0:7340\"\n",
            "[admin]\nhost = \"0.0.0.0\"\n",
        ] {
            let error = TomlConfig::parse(text, &toml_path()).expect_err("must be refused");
            assert!(
                matches!(error, ConfigError::TomlParse { .. }),
                "{text:?} gave {error:?}"
            );
        }
    }

    #[test]
    fn config_toml_refuses_a_secret_value() {
        for text in [
            "api_key = \"anything-at-all\"\n",
            "NINEROUTER_API_KEY = \"anything-at-all\"\n",
            "[provider]\ntoken = \"anything-at-all\"\n",
        ] {
            let error = TomlConfig::parse(text, &toml_path()).expect_err("must be refused");
            assert!(
                matches!(error, ConfigError::SecretInToml { .. }),
                "{text:?} gave {error:?}"
            );
        }
    }

    #[test]
    fn config_toml_refuses_an_unknown_key() {
        let error = TomlConfig::parse("colour_scheme = \"dark\"\n", &toml_path())
            .expect_err("unknown keys are refused");
        assert!(matches!(error, ConfigError::TomlParse { .. }), "{error:?}");
    }

    #[test]
    fn config_toml_refuses_an_unknown_log_level() {
        let error = TomlConfig::parse("log_level = \"chatty\"\n", &toml_path())
            .expect_err("must be refused");
        assert!(
            matches!(error, ConfigError::BadLogLevel { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn paths_default_under_home() {
        let paths = Paths::from_home().expect("HOME is set in the test environment");
        assert!(paths.env_file.ends_with(".config/kallisti/eris.env"));
        assert!(paths.toml_file.ends_with(".config/kallisti/config.toml"));
        assert!(paths.db_file.ends_with(".local/share/kallisti/eris.db"));
        assert!(
            paths
                .persona_file()
                .ends_with(".config/kallisti/persona.md")
        );
    }

    #[test]
    fn config_toml_accepts_the_persona_table_and_defaults_it_on() {
        // Absent means on at the documented path with no inline text.
        let bare = TomlConfig::parse("log_level = \"info\"\n", &toml_path()).expect("no persona");
        assert!(bare.persona().enabled());
        assert_eq!(bare.persona().path(), None);
        assert_eq!(bare.persona().text(), None);

        // Spelled out, every key lands.
        let text = "[persona]\nenabled = false\npath = \"/tmp/id.md\"\n";
        let toml = TomlConfig::parse(text, &toml_path()).expect("persona table");
        assert!(!toml.persona().enabled());
        assert_eq!(toml.persona().path(), Some(Path::new("/tmp/id.md")));
        assert_eq!(toml.persona().text(), None);

        // The inline fallback is accepted, and is not mistaken for a secret.
        let inline = TomlConfig::parse("[persona]\ntext = \"be brief\"\n", &toml_path())
            .expect("inline text");
        assert_eq!(inline.persona().text(), Some("be brief"));
        assert!(inline.persona().enabled());
    }

    #[test]
    fn the_persona_table_refuses_a_key_nobody_documented() {
        let error = TomlConfig::parse("[persona]\nmood = \"sunny\"\n", &toml_path())
            .expect_err("unknown persona keys are refused");
        assert!(matches!(error, ConfigError::TomlParse { .. }), "{error:?}");
    }

    #[test]
    fn auto_refresh_is_false_unless_the_operator_writes_true() {
        // The parser's default, not the file's: a missing table, a missing key,
        // and an empty document all mean no refresh.
        assert!(!TomlConfig::defaults().oauth().auto_refresh());
        assert!(
            !TomlConfig::parse("", &toml_path())
                .expect("empty document")
                .oauth()
                .auto_refresh()
        );
        assert!(
            !TomlConfig::parse("log_level = \"debug\"\n", &toml_path())
                .expect("no oauth table")
                .oauth()
                .auto_refresh()
        );
        assert!(
            !TomlConfig::parse("[oauth]\n", &toml_path())
                .expect("empty oauth table")
                .oauth()
                .auto_refresh()
        );

        // Only the explicit value turns it on, and build 108 writes no such file.
        assert!(
            TomlConfig::parse("[oauth]\nauto_refresh = true\n", &toml_path())
                .expect("explicit")
                .oauth()
                .auto_refresh()
        );
        assert!(
            !TomlConfig::parse("[oauth]\nauto_refresh = false\n", &toml_path())
                .expect("explicit false")
                .oauth()
                .auto_refresh()
        );
    }

    #[test]
    fn the_oauth_table_refuses_a_key_nobody_documented() {
        let error = TomlConfig::parse("[oauth]\nrefresh_everything = true\n", &toml_path())
            .expect_err("an unknown oauth key is refused");
        assert!(matches!(error, ConfigError::TomlParse { .. }), "{error:?}");
    }

    #[test]
    fn a_secret_in_the_oauth_table_is_refused_by_name() {
        let error = TomlConfig::parse("[oauth]\nrefresh_token = \"abc\"\n", &toml_path())
            .expect_err("a secret belongs nowhere near this file");
        assert!(matches!(error, ConfigError::SecretInToml { .. }), "{error:?}");
        assert!(error.to_string().contains("oauth.refresh_token"), "{error}");
    }

    #[test]
    fn the_token_paths_are_under_the_config_directory_and_nowhere_else() {
        let paths = Paths::for_root("/tmp/example");
        assert_eq!(paths.token_dir, PathBuf::from("/tmp/example/config/tokens"));
        assert_eq!(
            paths.token_key_file,
            PathBuf::from("/tmp/example/config/keys/token.key")
        );
    }
}
