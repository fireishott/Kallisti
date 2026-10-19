//! Secret references.
//!
//! 1.0 accepts exactly one form: `env:NAME`, resolved from the process
//! environment at call time. No keychain, no file, no config value, no
//! `keyring:` scheme, no encrypted blob. A reference is a name, never a
//! materialised secret, so a [`SecretRef`] can be logged, stored in the config
//! file, and written into an event payload without leaking anything.

use serde::{Deserialize, Serialize};

/// Prefix that marks a secret reference as an environment variable name.
pub const API_KEY_REF_ENV_PREFIX: &str = "env:";

/// A parsed secret reference.
///
/// The wire form is the same string the config file carries: `env:NAME`. Only
/// that shape deserializes, so a config file that accidentally contains a
/// literal key fails to load instead of being quietly accepted.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum SecretRef {
    /// Read the value from this environment variable at call time.
    Env(String),
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

    /// The environment variable this reference names.
    #[must_use]
    pub fn env_name(&self) -> &str {
        match self {
            Self::Env(name) => name,
        }
    }

    /// The wire form, `env:NAME`.
    #[must_use]
    pub fn wire(&self) -> String {
        format!("{API_KEY_REF_ENV_PREFIX}{}", self.env_name())
    }
}

impl TryFrom<&str> for SecretRef {
    type Error = SecretRefError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let Some(name) = value.strip_prefix(API_KEY_REF_ENV_PREFIX) else {
            return Err(SecretRefError::UnsupportedScheme);
        };
        Self::env(name)
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
    /// The reference did not start with [`API_KEY_REF_ENV_PREFIX`].
    #[error("secret reference must start with `env:`")]
    UnsupportedScheme,
    /// The reference named no variable.
    #[error("secret reference `env:` names no variable")]
    MissingName,
    /// The name already carried the prefix, as in `env:env:FOO`.
    #[error("secret reference name must not repeat the `env:` prefix")]
    NestedPrefix,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_reference_resolves_to_a_variable_name() {
        let parsed = SecretRef::try_from("env:NINEROUTER_API_KEY").expect("reference should parse");
        assert_eq!(parsed, SecretRef::Env("NINEROUTER_API_KEY".to_owned()));
        assert_eq!(parsed.env_name(), "NINEROUTER_API_KEY");
        assert_eq!(parsed.wire(), "env:NINEROUTER_API_KEY");
    }

    #[test]
    fn a_bare_value_is_not_a_reference() {
        assert_eq!(
            SecretRef::try_from("sk-live-not-a-reference"),
            Err(SecretRefError::UnsupportedScheme)
        );
    }

    #[test]
    fn an_empty_name_is_rejected() {
        assert_eq!(
            SecretRef::try_from("env:"),
            Err(SecretRefError::MissingName)
        );
    }

    #[test]
    fn a_nested_prefix_is_rejected() {
        assert_eq!(
            SecretRef::try_from("env:env:FOO"),
            Err(SecretRefError::NestedPrefix)
        );
    }

    #[test]
    fn the_wire_form_is_the_only_accepted_serde_form() {
        let json = serde_json::to_string(&SecretRef::Env("FOO".to_owned()))
            .expect("a reference should serialize");
        assert_eq!(json, "\"env:FOO\"");
        let parsed: SecretRef =
            serde_json::from_str("\"env:FOO\"").expect("reference should parse");
        assert_eq!(parsed.env_name(), "FOO");
        let literal: Result<SecretRef, _> = serde_json::from_str("\"sk-literal\"");
        assert!(literal.is_err(), "a bare value must not deserialize");
    }
}
