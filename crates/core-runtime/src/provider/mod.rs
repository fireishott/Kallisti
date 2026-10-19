//! Provider seam. Skeleton only in Wave 0; the real trait and the
//! OpenAI-compatible adapter land in Wave 2 (P4).
//!
//! Two decisions are already locked and are recorded here so the adapter can
//! only implement them one way:
//!
//! - a key is referenced, never stored: `api_key_ref` is `env:NAME` and resolves
//!   from the process environment, which is the only form 1.0 accepts;
//! - usage that a provider cannot report is `provider_unavailable`, never zero
//!   (locked amendment 6).

/// Prefix that marks a secret reference as an environment variable name.
pub const API_KEY_REF_ENV_PREFIX: &str = "env:";

/// A parsed secret reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretRef<'a> {
    /// Read the value from this environment variable at call time.
    Env(&'a str),
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
}

impl<'a> TryFrom<&'a str> for SecretRef<'a> {
    type Error = SecretRefError;

    fn try_from(value: &'a str) -> Result<Self, Self::Error> {
        let Some(name) = value.strip_prefix(API_KEY_REF_ENV_PREFIX) else {
            return Err(SecretRefError::UnsupportedScheme);
        };
        if name.is_empty() {
            return Err(SecretRefError::MissingName);
        }
        Ok(Self::Env(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_reference_resolves_to_a_variable_name() {
        assert_eq!(
            SecretRef::try_from("env:NINEROUTER_API_KEY"),
            Ok(SecretRef::Env("NINEROUTER_API_KEY"))
        );
    }

    #[test]
    fn a_bare_value_is_not_a_reference() {
        assert_eq!(
            SecretRef::try_from("not-a-reference"),
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
}
