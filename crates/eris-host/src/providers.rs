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
    /// Which request shape this endpoint speaks, when it is not the default.
    ///
    /// `None` means the OpenAI shape, which is every entry that existed before
    /// this field did. An entry names `messages` for a gateway whose Claude
    /// models only answer in the Anthropic shape; the model's own declared
    /// endpoint decides per turn when the catalogue publishes one, and this is
    /// the fallback for a catalogue that publishes nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dialect: Option<String>,
    /// Whether this endpoint separates its chain of thought from the answer
    /// when asked to.
    ///
    /// `None` means "do not ask", which is what every entry that existed before
    /// this field did. An entry sets `true` for a deployment that inlines its
    /// reasoning into the answer unless the request says otherwise - MiniMax
    /// does this by default - so the daemon asks for the split and the thinking
    /// lands in the reasoning channel instead of the reply. It is off by default
    /// because a gateway that does not know the field may refuse the request,
    /// and one compatibility flag is not worth a 400 on somebody else's server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_split: Option<bool>,
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
    if let Some(dialect) = provider.dialect.as_deref() {
        let known = ["chat_completions", "messages"];
        if !known.contains(&dialect.trim()) {
            return Err(RegistryError::new(format!(
                "{} names the dialect `{}`; this build speaks {}",
                provider.id,
                dialect.trim(),
                known.join(" and ")
            )));
        }
    }
    Ok(())
}

/// The dialect one registry entry declares, as the provider vocabulary spells
/// it. An unknown or absent value is the OpenAI shape, which is what every
/// deployment used before the field existed.
#[must_use]
pub fn dialect_of(provider: &Provider) -> core_domain::ApiDialect {
    provider
        .dialect
        .as_deref()
        .map(core_domain::ApiDialect::parse)
        .unwrap_or(core_domain::ApiDialect::ChatCompletions)
}

/// Whether one registry entry asks its endpoint to split reasoning from the
/// answer. An absent value asks for nothing, which is what every entry written
/// before the field existed did.
#[must_use]
pub fn reasoning_split_of(provider: &Provider) -> bool {
    provider.reasoning_split.unwrap_or(false)
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
        dialect: None,
        reasoning_split: None,
    }]
}

/// The provider id a stored `env:NAME` reference belongs to, by key reference.
///
/// The legacy `provider.api_key_ref` row names one provider's key and nothing
/// else about it, so a key reference is the one thing that identifies which
/// registry entry the legacy probe was taken against. Two providers sharing one
/// reference is ambiguous and resolves to the first entry, which is the same
/// rule [`resolve`] applies to a primary: a registry with entries is more useful
/// than no answer.
#[must_use]
pub fn legacy_provider_id(providers: &[Provider], key_ref: &str) -> String {
    let wire = key_ref.trim();
    providers
        .iter()
        .find(|provider| provider.key_ref.trim() == wire)
        .map(|provider| provider.id.clone())
        .unwrap_or_else(|| DEFAULT_ID.to_owned())
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
            dialect: None,
            reasoning_split: None,
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
    fn an_entry_written_before_the_dialect_existed_still_parses() {
        let raw = r#"[{"id":"default","name":"Default",
                       "base_url":"http://127.0.0.1:20128/v1",
                       "key_ref":"env:NINEROUTER_API_KEY"}]"#;
        let parsed = parse(raw).expect("parses");
        assert_eq!(parsed[0].dialect, None);
        assert_eq!(
            dialect_of(&parsed[0]),
            core_domain::ApiDialect::ChatCompletions
        );

        let named = parse(
            r#"[{"id":"cc","name":"Command Code",
                 "base_url":"https://api.commandcode.ai/provider/v1",
                 "key_ref":"env:COMMANDCODE_API_KEY","dialect":"messages"}]"#,
        )
        .expect("parses");
        assert_eq!(dialect_of(&named[0]), core_domain::ApiDialect::Messages);
    }

    #[test]
    fn an_entry_can_ask_for_a_reasoning_split_and_an_old_one_cannot() {
        // The row written before the field existed asks for nothing, so the
        // request bytes an old install sends are unchanged.
        let old = parse(
            r#"[{"id":"default","name":"Default",
                 "base_url":"http://127.0.0.1:20128/v1",
                 "key_ref":"env:NINEROUTER_API_KEY"}]"#,
        )
        .expect("parses");
        assert_eq!(old[0].reasoning_split, None);
        assert!(!reasoning_split_of(&old[0]));

        let asked = parse(
            r#"[{"id":"minimax","name":"MiniMax",
                 "base_url":"https://api.minimax.io/v1",
                 "key_ref":"env:MINIMAX_API_KEY","reasoning_split":true}]"#,
        )
        .expect("parses");
        assert!(reasoning_split_of(&asked[0]));

        // Explicitly false is the same wire behaviour as absent, and the
        // document round trips so the settings editor shows what was stored.
        let declined = parse(
            r#"[{"id":"beta","name":"Beta","base_url":"http://b/v1",
                 "key_ref":"env:B","reasoning_split":false}]"#,
        )
        .expect("parses");
        assert!(!reasoning_split_of(&declined[0]));
        assert_eq!(parse(&to_json(&asked)).expect("round trip"), asked);
    }

    #[test]
    fn a_dialect_this_build_cannot_speak_is_refused_by_name() {
        let mut provider = one("alpha");
        provider.dialect = Some("responses".to_owned());
        let error = validate(&[provider]).unwrap_err();
        assert!(error.message.contains("alpha"), "{}", error.message);
        assert!(
            error.message.contains("chat_completions"),
            "{}",
            error.message
        );
    }

    #[test]
    fn the_legacy_reference_identifies_the_provider_it_belongs_to() {
        let registry = vec![one("alpha"), one("beta")];
        // Both seeded entries carry the same reference, so the first wins: the
        // rule is stated rather than left to whichever order a loop happened to
        // take.
        assert_eq!(
            legacy_provider_id(&registry, "env:NINEROUTER_API_KEY"),
            "alpha"
        );

        let mut beta = one("beta");
        beta.key_ref = "env:BETA_KEY".to_owned();
        let registry = vec![one("alpha"), beta];
        assert_eq!(legacy_provider_id(&registry, "env:BETA_KEY"), "beta");
        assert_eq!(
            legacy_provider_id(&registry, "env:NOTHING"),
            DEFAULT_ID,
            "an install whose registry never matched still has a default to seed"
        );
    }

    #[test]
    fn the_legacy_rows_seed_one_default_provider() {
        let seeded = seed("http://127.0.0.1:20128/v1", "env:NINEROUTER_API_KEY");
        assert_eq!(seeded.len(), 1);
        assert_eq!(seeded[0].id, DEFAULT_ID);
        assert!(validate(&seeded).is_ok(), "the seed must be valid itself");
    }
}
