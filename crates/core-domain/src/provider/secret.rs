//! Secret references.
//!
//! A reference is a **name**, never a value. Two schemes exist:
//!
//! | Wire form | Where the value comes from |
//! |---|---|
//! | `env:NAME` | the process environment, at call time |
//! | `token:NAME` | the daemon's encrypted token store, by provider id |
//!
//! `env:` is the only scheme that existed before build 110 and it behaves
//! exactly as it did: same parse, same rejections, same resolution. The `token:`
//! scheme names a grant the daemon itself owns after a browser login. It can
//! only be resolved through `eris_host`'s credential choke point, because the
//! store and its key live a layer above this crate - `core_runtime` refuses a
//! token reference by name rather than pretending it can read one.
//!
//! What has not changed is the guarantee: a [`SecretRef`] can be logged, stored
//! in a config file, and written into an event payload without leaking anything,
//! because it carries a name.

use serde::{Deserialize, Serialize};

/// Prefix that marks a secret reference as an environment variable name.
pub const API_KEY_REF_ENV_PREFIX: &str = "env:";

/// Prefix that marks a secret reference as a token-store name. The same string
/// `core_domain::TOKEN_REF_PREFIX` carries; named here so a reference can be
/// parsed without reaching for the token module.
pub const API_KEY_REF_TOKEN_PREFIX: &str = "token:";

/// A parsed secret reference.
///
/// The wire form is the same string the config file carries: `env:NAME` or
/// `token:NAME`. Only those two shapes deserialize, so a config file that
/// accidentally contains a literal key fails to load instead of being quietly
/// accepted.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum SecretRef {
    /// Read the value from this environment variable at call time.
    Env(String),
    /// Read the value out of the token store under this name.
    ///
    /// The name is a provider id matching the file stem the store uses, so
    /// `token:claude` reads `claude.tok`. It is restricted to the character set
    /// the store accepts, so a reference can never become a path traversal.
    Token(String),
}

impl SecretRef {
    /// Build a reference to an environment variable.
    ///
    /// Rejects an empty or [`API_KEY_REF_ENV_PREFIX`]-prefixed name so a caller
    /// cannot build `env:env:FOO` by accident.
    pub fn env(name: impl Into<String>) -> Result<Self, SecretRefError> {
        let name = name.into();
        if name.is_empty() {
            return Err(SecretRefError::MissingName);
        }
        if name.starts_with(API_KEY_REF_ENV_PREFIX) {
            return Err(SecretRefError::NestedPrefix);
        }
        Ok(Self::Env(name))
    }

    /// Build a reference to a stored grant.
    ///
    /// Same rules as [`Self::env`], with the store's character set: lowercase
    /// letters, digits, hyphen and underscore. Anything else is refused here
    /// rather than becoming a file name.
    pub fn token(name: impl Into<String>) -> Result<Self, SecretRefError> {
        let name = name.into();
        if name.is_empty() {
            return Err(SecretRefError::MissingName);
        }
        if name.starts_with(API_KEY_REF_TOKEN_PREFIX) {
            return Err(SecretRefError::NestedPrefix);
        }
        if !name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            return Err(SecretRefError::BadTokenName);
        }
        Ok(Self::Token(name))
    }

    /// The environment variable this reference names, when it names one.
    ///
    /// `None` for a token reference: every caller that used to assume an
    /// environment variable has to say what it does with a grant, and the
    /// compiler asks the question instead of a runtime surprise.
    #[must_use]
    pub fn env_name(&self) -> Option<&str> {
        match self {
            Self::Env(name) => Some(name),
            Self::Token(_) => None,
        }
    }

    /// The token-store name this reference names, when it names one.
    #[must_use]
    pub fn token_name(&self) -> Option<&str> {
        match self {
            Self::Token(name) => Some(name),
            Self::Env(_) => None,
        }
    }

    /// Whether this reference reads the process environment.
    #[must_use]
    pub const fn is_env(&self) -> bool {
        matches!(self, Self::Env(_))
    }

    /// Whether this reference reads the token store.
    #[must_use]
    pub const fn is_token(&self) -> bool {
        matches!(self, Self::Token(_))
    }

    /// The wire form, `env:NAME` or `token:NAME`.
    #[must_use]
    pub fn wire(&self) -> String {
        match self {
            Self::Env(name) => format!("{API_KEY_REF_ENV_PREFIX}{name}"),
            Self::Token(name) => format!("{API_KEY_REF_TOKEN_PREFIX}{name}"),
        }
    }
}

impl TryFrom<&str> for SecretRef {
    type Error = SecretRefError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if let Some(name) = value.strip_prefix(API_KEY_REF_ENV_PREFIX) {
            return Self::env(name);
        }
        if let Some(name) = value.strip_prefix(API_KEY_REF_TOKEN_PREFIX) {
            return Self::token(name);
        }
        Err(SecretRefError::UnsupportedScheme)
    }
}

impl TryFrom<String> for SecretRef {
    type Error = SecretRefError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl From<SecretRef> for String {
    fn from(value: SecretRef) -> Self {
        value.wire()
    }
}

/// Why a reference could not be understood.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SecretRefError {
    /// The reference did not start with [`API_KEY_REF_ENV_PREFIX`] or
    /// [`API_KEY_REF_TOKEN_PREFIX`].
    #[error("secret reference must start with `env:` or `token:`")]
    UnsupportedScheme,
    /// The reference named no variable.
    #[error("secret reference `env:` names no variable")]
    MissingName,
    /// The name already carried a prefix, as in `env:env:FOO`.
    #[error("secret reference name must not repeat its own prefix")]
    NestedPrefix,
    /// A `token:` name carried something the store will not accept as a
    /// provider id.
    #[error("token reference name must be lowercase letters, digits, hyphen or underscore")]
    BadTokenName,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_reference_resolves_to_a_variable_name() {
        let parsed = SecretRef::try_from("env:NINEROUTER_API_KEY").expect("reference should parse");
        assert_eq!(parsed, SecretRef::Env("NINEROUTER_API_KEY".to_owned()));
        assert_eq!(parsed.env_name(), Some("NINEROUTER_API_KEY"));
        assert_eq!(parsed.token_name(), None);
        assert!(parsed.is_env());
        assert_eq!(parsed.wire(), "env:NINEROUTER_API_KEY");
    }

    #[test]
    fn a_token_reference_names_a_stored_grant_and_carries_no_value() {
        let parsed = SecretRef::try_from("token:claude").expect("reference should parse");
        assert_eq!(parsed, SecretRef::Token("claude".to_owned()));
        assert_eq!(parsed.token_name(), Some("claude"));
        assert_eq!(parsed.env_name(), None);
        assert!(parsed.is_token());
        assert_eq!(parsed.wire(), "token:claude");
        assert_eq!(format!("{parsed:?}"), "Token(\"claude\")");
    }

    #[test]
    fn the_env_path_is_unchanged_by_the_token_scheme() {
        assert_eq!(
            SecretRef::try_from("sk-live-not-a-reference"),
            Err(SecretRefError::UnsupportedScheme)
        );
        assert_eq!(
            SecretRef::try_from("env:"),
            Err(SecretRefError::MissingName)
        );
        assert_eq!(
            SecretRef::try_from("env:env:FOO"),
            Err(SecretRefError::NestedPrefix)
        );
        assert_eq!(SecretRef::env("env:FOO"), Err(SecretRefError::NestedPrefix));
    }

    #[test]
    fn a_token_name_outside_the_store_character_set_is_refused() {
        assert_eq!(
            SecretRef::try_from("token:"),
            Err(SecretRefError::MissingName)
        );
        assert_eq!(
            SecretRef::try_from("token:token:claude"),
            Err(SecretRefError::NestedPrefix)
        );
        assert_eq!(
            SecretRef::try_from("token:claude/../../etc/passwd"),
            Err(SecretRefError::BadTokenName)
        );
        assert_eq!(
            SecretRef::try_from("token:Claude"),
            Err(SecretRefError::BadTokenName)
        );
        assert!(SecretRef::try_from("token:antigravity_1").is_ok());
    }

    #[test]
    fn the_wire_form_is_the_only_accepted_serde_form() {
        let json = serde_json::to_string(&SecretRef::Env("FOO".to_owned()))
            .expect("a reference should serialize");
        assert_eq!(json, "\"env:FOO\"");
        let parsed: SecretRef =
            serde_json::from_str("\"env:FOO\"").expect("reference should parse");
        assert_eq!(parsed.env_name(), Some("FOO"));

        let json = serde_json::to_string(&SecretRef::Token("claude".to_owned()))
            .expect("a reference should serialize");
        assert_eq!(json, "\"token:claude\"");

        let literal: Result<SecretRef, _> = serde_json::from_str("\"sk-literal\"");
        assert!(literal.is_err(), "a bare value must not deserialize");
    }
}
