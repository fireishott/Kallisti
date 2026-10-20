//! Providers: more than one endpoint.
//!
//! Until now the daemon had exactly one provider, described by three loose
//! settings rows. That is enough for a chat box and not enough for a platform:
//! an auxiliary role that wants a vision model on a different endpoint has
//! nowhere to point.
//!
//! This module is the registry: a list of providers, each with a name, an
//! endpoint and a key *reference*, and one of them marked primary. The rows that
//! described the original provider are still read, so an existing install keeps
//! working: they seed a `default` entry on first use.
//!
//! Two rules carried over from the rest of the daemon. A key is a reference
//! (`env:NAME`), never a value, so nothing secret can be written here. And a
//! registry that does not parse is refused rather than repaired silently: an
//! operator who typed an endpoint wants to know it was not understood.

use serde::{Deserialize, Serialize};

/// The `settings` row that holds the registry, as JSON.
pub const SETTING_REGISTRY: &str = "providers.registry";

/// The `settings` row naming the primary provider id.
pub const SETTING_PRIMARY: &str = "providers.primary";

/// The id the legacy settings rows seed.
pub const DEFAULT_ID: &str = "default";

/// The most providers the registry may hold.
pub const MAX_PROVIDERS: usize = 16;

/// One configured endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provider {
    /// Stable id, used by auxiliary roles to point at this provider.
    pub id: String,
    /// What the operator reads.
    pub name: String,
    /// The OpenAI-compatible base URL.
    pub base_url: String,
    /// The `env:NAME` reference the key is read from.
    pub key_ref: String,
}

/// Why a registry could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryError {
    /// What the operator should fix.
    pub message: String,
}

impl RegistryError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Parse and validate a registry document.
///
/// # Errors
///
/// [`RegistryError`] naming the provider and the field that is wrong.
pub fn parse(raw: &str) -> Result<Vec<Provider>, RegistryError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let providers: Vec<Provider> = serde_json::from_str(trimmed).map_err(|error| {
        RegistryError::new(format!(
            "the provider list must be a JSON array of {{\"id\", \"name\", \"base_url\", \"key_ref\"}} objects: {error}"
        ))
    })?;
    validate(&providers)?;
    Ok(providers)
}

/// Check every entry, and the set as a whole.
///
/// # Errors
///
/// [`RegistryError`] for the first problem found.
pub fn validate(providers: &[Provider]) -> Result<(), RegistryError> {
    if providers.len() > MAX_PROVIDERS {
        return Err(RegistryError::new(format!(
            "{} providers is more than the {MAX_PROVIDERS} this build will store",
            providers.len()
        )));
    }
    let mut seen: Vec<&str> = Vec::new();
    for provider in providers {
        validate_one(provider)?;
        if seen.contains(&provider.id.as_str()) {
            return Err(RegistryError::new(format!(
                "two providers share the id {}",
                provider.id
            )));
        }
        seen.push(&provider.id);
    }
    Ok(())
}

/// Check one entry.
fn validate_one(provider: &Provider) -> Result<(), RegistryError> {
    if provider.id.trim().is_empty() {
        return Err(RegistryError::new("every provider needs an id"));
    }
    if !provider
        .id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(RegistryError::new(format!(
            "the id `{}` must be lowercase letters, digits, hyphen or underscore",
            provider.id
        )));
    }
    if provider.name.trim().is_empty() {
        return Err(RegistryError::new(format!("{} needs a name", provider.id)));
    }
    if !(provider.base_url.starts_with("http://") || provider.base_url.starts_with("https://")) {
        return Err(RegistryError::new(format!(
            "{} needs a base URL starting http:// or https://, not `{}`",
            provider.id, provider.base_url
        )));
    }
    if provider.base_url.contains(char::is_whitespace) {
        return Err(RegistryError::new(format!(
            "{} has a base URL with a space in it",
            provider.id
        )));
    }
    if !is_env_reference(&provider.key_ref) {
        return Err(RegistryError::new(format!(
            "{} needs a key reference like `env:NAME`, and never the key itself",
            provider.id
        )));
    }
    Ok(())
}

/// Whether a string is an `env:NAME` reference.
///
/// The same rule the onboarding step applies: a reference travels, a value does
/// not, and a value in this field is refused rather than quietly used as a name.
#[must_use]
pub fn is_env_reference(raw: &str) -> bool {
    let Some(name) = raw.trim().strip_prefix("env:") else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// The provider a turn should use.
///
/// A named primary wins. A primary id that is not in the list falls back to the
/// first entry, because a registry with providers and no match is still more
/// useful than no provider at all - and the settings page states which one is in
/// force, so the fallback is visible rather than mysterious.
#[must_use]
pub fn resolve<'a>(providers: &'a [Provider], primary: &str) -> Option<&'a Provider> {
    providers
        .iter()
        .find(|provider| provider.id == primary)
        .or_else(|| providers.first())
}

/// Build the seeded registry from the three legacy settings rows.
#[must_use]
pub fn seed(base_url: &str, key_ref: &str) -> Vec<Provider> {
    vec![Provider {
        id: DEFAULT_ID.to_owned(),
        name: "Default".to_owned(),
        base_url: base_url.to_owned(),
        key_ref: key_ref.to_owned(),
    }]
}

/// Render a registry for the editor.
#[must_use]
pub fn to_json(providers: &[Provider]) -> String {
    if providers.is_empty() {
        return "[]".to_owned();
    }
    serde_json::to_string_pretty(providers).unwrap_or_else(|_| "[]".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(id: &str) -> Provider {
        Provider {
            id: id.to_owned(),
            name: "Local".to_owned(),
            base_url: "http://127.0.0.1:20128/v1".to_owned(),
            key_ref: "env:NINEROUTER_API_KEY".to_owned(),
        }
    }

    #[test]
    fn a_valid_registry_round_trips() {
        let registry = vec![one("alpha"), one("beta")];
        let parsed = parse(&to_json(&registry)).expect("round trip");
        assert_eq!(parsed, registry);
    }

    #[test]
    fn an_empty_document_is_an_empty_registry() {
        assert_eq!(parse("").expect("empty"), Vec::new());
        assert_eq!(parse("  ").expect("blank"), Vec::new());
    }

    #[test]
    fn a_key_value_is_refused_and_a_reference_is_allowed() {
        let mut provider = one("alpha");
        provider.key_ref = "sk-live-abcd1234".to_owned();
        let error = validate(&[provider]).unwrap_err();
        assert!(error.message.contains("env:NAME"), "{}", error.message);

        assert!(is_env_reference("env:NINEROUTER_API_KEY"));
        assert!(is_env_reference("env:A1"));
        assert!(!is_env_reference("NINEROUTER_API_KEY"));
        assert!(!is_env_reference("env:"));
        assert!(!is_env_reference("env:a_b"));
    }

    #[test]
    fn a_bad_endpoint_or_id_is_refused_by_name() {
        let mut provider = one("alpha");
        provider.base_url = "localhost:20128".to_owned();
        let error = validate(&[provider]).unwrap_err();
        assert!(error.message.contains("alpha"), "{}", error.message);

        let mut provider = one("Alpha One");
        provider.id = "Alpha One".to_owned();
        let error = validate(&[provider]).unwrap_err();
        assert!(error.message.contains("lowercase"), "{}", error.message);
    }

    #[test]
    fn duplicate_ids_are_refused_rather_than_merged() {
        let error = validate(&[one("alpha"), one("alpha")]).unwrap_err();
        assert!(error.message.contains("alpha"), "{}", error.message);
    }

    #[test]
    fn the_primary_is_honoured_and_an_unknown_one_falls_back_to_the_first() {
        let registry = vec![one("alpha"), one("beta")];
        assert_eq!(
            resolve(&registry, "beta").map(|p| p.id.as_str()),
            Some("beta")
        );
        assert_eq!(
            resolve(&registry, "nope").map(|p| p.id.as_str()),
            Some("alpha"),
            "an unknown primary falls back rather than disabling the daemon"
        );
        assert!(resolve(&[], "alpha").is_none());
    }

    #[test]
    fn the_legacy_rows_seed_one_default_provider() {
        let seeded = seed("http://127.0.0.1:20128/v1", "env:NINEROUTER_API_KEY");
        assert_eq!(seeded.len(), 1);
        assert_eq!(seeded[0].id, DEFAULT_ID);
        assert!(validate(&seeded).is_ok(), "the seed must be valid itself");
    }
}
