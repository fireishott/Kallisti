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
    /// Whether this endpoint is the vendor's own API rather than a gateway.
    ///
    /// `None` sends exactly the bytes every entry sent before this field
    /// existed: a bearer token and no vendor headers. An entry sets `true` for
    /// a direct deployment, which adds `anthropic-version`/`anthropic-beta` on
    /// the Messages dialect and the Antigravity `User-Agent` on its own. The
    /// header choice is a property of the deployment and never inferred from
    /// the dialect: a gateway in front of Claude wants the body and none of
    /// those headers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct: Option<bool>,
    /// The cloud project an Antigravity entry is billed to.
    ///
    /// `None` on every other dialect. Antigravity's `v1internal` body requires
    /// one, and a login discovers it, so an entry whose account has never
    /// onboarded leaves this empty and the first turn says so by name rather
    /// than the endpoint answering an opaque 400.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
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

/// One entry's problem, and which form field it belongs to.
///
/// A registry is edited by an operator through a form, and a refusal that says
/// "the id must be lowercase" without saying *which* box to fix is the kind of
/// message that sends somebody hunting. The rule for what is acceptable lives in
/// [`check_entry`] and is the only copy: [`RegistryError`] carries the sentence
/// and this carries the field, and they are produced together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldProblem {
    /// The form field the sentence belongs to.
    pub field: &'static str,
    /// What the operator should fix.
    pub message: String,
}

/// Check one entry, naming the field at fault.
///
/// # Errors
///
/// [`FieldProblem`] for the first problem found.
pub fn check_entry(provider: &Provider) -> Result<(), FieldProblem> {
    validate_one(provider).map_err(|error| {
        let field = if provider.id.trim().is_empty()
            || !provider
                .id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            "provider_id"
        } else if provider.name.trim().is_empty() {
            "provider_name"
        } else if !(provider.base_url.starts_with("http://")
            || provider.base_url.starts_with("https://"))
            || provider.base_url.contains(char::is_whitespace)
        {
            "provider_base_url"
        } else if !is_key_reference(&provider.key_ref) {
            "provider_key_ref"
        } else {
            "provider_dialect"
        };
        FieldProblem {
            field,
            message: error.message,
        }
    })
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
    if !is_key_reference(&provider.key_ref) {
        return Err(RegistryError::new(format!(
            "{} needs a key reference like `env:NAME` or `token:NAME`, and never the key itself",
            provider.id
        )));
    }
    if let Some(dialect) = provider.dialect.as_deref() {
        let known = ["chat_completions", "messages", "antigravity"];
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

/// Whether a string is a key reference this build can resolve: an environment
/// variable name or a stored grant.
///
/// The rule for `env:` is unchanged from build 107 and is still enforced by
/// [`is_env_reference`]: a reference travels, a value does not. `token:` names a
/// grant the daemon itself sealed after a browser login, and the character set
/// is [`core_domain::SecretRef::token`]'s, so a name can never become a path
/// traversal into another file in the store.
#[must_use]
pub fn is_key_reference(raw: &str) -> bool {
    let trimmed = raw.trim();
    if let Some(name) = trimmed.strip_prefix(core_domain::TOKEN_REF_PREFIX) {
        return core_domain::SecretRef::token(name).is_ok();
    }
    is_env_reference(trimmed)
}

/// The environment variable a pasted key for `provider_id` is sealed under.
///
/// Deterministic and derived from the id alone, so pasting again replaces the
/// same line instead of growing the file. Two ids that differ only by `-` versus
/// `_` sanitise to the same name; ids are validated lowercase-alnum plus those two
/// characters, so that pair is the only collision this can produce and it is
/// visible in the resulting variable name.
#[must_use]
pub fn sealed_env_name(provider_id: &str) -> String {
    let mut name: String = provider_id
        .chars()
        .map(|c| {
            let upper = c.to_ascii_uppercase();
            if upper.is_ascii_uppercase() || upper.is_ascii_digit() {
                upper
            } else {
                '_'
            }
        })
        .collect();
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        name.insert_str(0, "P_");
    }
    name.push_str("_API_KEY");
    name
}

/// Whether a pasted value can be written to an environment file unquoted.
///
/// `eris.env` is a systemd `EnvironmentFile`: a value is taken literally, but
/// quotes and backslashes have meaning and a `#` at the start of a line starts a
/// comment. Rather than escape and hope, this accepts the character set every
/// real key uses and refuses the rest with a sentence that says what to do
/// instead.
fn pasted_key_is_writable(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '-' | '_' | '.' | ':' | '/' | '+' | '=' | '~' | '@')
        })
}

/// Write one `NAME=value` line into the environment file, atomically.
///
/// The whole file is rewritten through a sibling temporary at mode 0600 and
/// renamed into place, so a half-written `eris.env` is never what systemd reads at
/// the next start. An existing line for the same name is replaced rather than
/// duplicated: systemd takes the last one, and two lines for one name is two
/// answers in one file.
fn write_env_entry(env_file: &std::path::Path, name: &str, value: &str) -> std::io::Result<()> {
    let existing = match std::fs::read_to_string(env_file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    };
    let mut out = String::with_capacity(existing.len() + value.len() + name.len() + 2);
    let mut replaced = false;
    for line in existing.lines() {
        let is_ours = line
            .split_once('=')
            .is_some_and(|(key, _)| key.trim() == name);
        if is_ours {
            if !replaced {
                out.push_str(name);
                out.push('=');
                out.push_str(value);
                out.push('\n');
                replaced = true;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    if !replaced {
        out.push_str(name);
        out.push('=');
        out.push_str(value);
        out.push('\n');
    }

    let temp = env_file.with_extension(format!("tmp-{}", std::process::id()));
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(out.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&temp, env_file)
}

/// Adopt the key field a provider form posted.
///
/// A reference is kept as it was: `env:NAME` and `token:NAME` still mean exactly
/// what they meant, and the registry still never holds a value. Anything else that
/// is not empty is a pasted key, and that is what an operator with a fresh vendor
/// account actually has. It is sealed instead of refused:
///
/// - the value is written to `eris.env` as `<ID>_API_KEY=...` at mode 0600, which
///   is the file systemd already loads at start;
/// - the same name and value go into this process's late-environment overlay, so
///   the first turn after the paste resolves without a restart;
/// - the registry stores `env:<ID>_API_KEY` and nothing else.
///
/// The value is never returned, never logged, and never written to the registry,
/// so no read of a settings page can echo it back.
///
/// # Errors
///
/// [`FieldProblem`] naming `provider_key_ref` when the field is empty, when the
/// value cannot be written unquoted, or when the file could not be written.
pub fn adopt_submitted_key(
    provider: &mut Provider,
    env_file: &std::path::Path,
) -> Result<Option<String>, FieldProblem> {
    let submitted = provider.key_ref.trim().to_owned();
    if is_key_reference(&submitted) {
        provider.key_ref = submitted;
        return Ok(None);
    }
    if submitted.is_empty() {
        return Err(FieldProblem {
            field: "provider_key_ref",
            message: format!(
                "{} needs a key: paste the provider's API key, or write `env:NAME` for a variable this daemon already has",
                provider.id
            ),
        });
    }
    // A malformed reference is a malformed reference, not a key that happens to
    // begin with the word "env". Sealing it would put `env:lower_case` in the
    // environment file as if the operator had pasted a secret.
    let spelled = submitted.to_ascii_lowercase();
    if spelled.starts_with("env:") || spelled.starts_with("token:") {
        return Err(FieldProblem {
            field: "provider_key_ref",
            message: format!(
                "`{submitted}` is not a reference this build can resolve: `env:` names an upper-case variable such as `env:NINEROUTER_API_KEY`, and `token:` names an account connected above"
            ),
        });
    }
    if !pasted_key_is_writable(&submitted) {
        return Err(FieldProblem {
            field: "provider_key_ref",
            message:
                "that is neither a reference nor a key this daemon can store: a pasted key is \
                      written to eris.env unquoted, so it may only contain letters, digits and \
                      - _ . : / + = ~ @. For a value with other characters, put it in eris.env \
                      yourself and write `env:NAME`."
                    .to_owned(),
        });
    }
    let name = sealed_env_name(&provider.id);
    write_env_entry(env_file, &name, &submitted).map_err(|error| FieldProblem {
        field: "provider_key_ref",
        message: format!(
            "the key could not be written to {}: {error}",
            env_file.display()
        ),
    })?;
    crate::oauth::credential::remember_env(&name, &submitted);
    provider.key_ref = format!("env:{name}");
    Ok(Some(name))
}

/// Whether one registry entry asks for the vendor's own headers.
///
/// An absent value is `false`, which is what every entry written before the
/// field existed meant: bearer token, no vendor headers.
#[must_use]
pub fn direct_of(provider: &Provider) -> bool {
    provider.direct.unwrap_or(false)
}

/// The cloud project an Antigravity entry is billed to, when it names one.
#[must_use]
pub fn project_of(provider: &Provider) -> Option<String> {
    provider
        .project
        .as_deref()
        .map(str::trim)
        .filter(|project| !project.is_empty())
        .map(str::to_owned)
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
        direct: None,
        project: None,
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

/// The registry entry a connected account becomes.
///
/// Built from the login side's own facts - id, label, base URL - so the entry and
/// the account cannot disagree about which endpoint they name. The key reference
/// is the `token:` the grant is sealed under, `direct` is on because both of these
/// are the vendor's own API rather than a gateway, and the dialect is the shape
/// that API speaks.
#[must_use]
pub fn oauth_entry(
    provider_id: &str,
    label: &str,
    base_url: &str,
    dialect: &str,
    project: Option<String>,
) -> Provider {
    Provider {
        id: provider_id.to_owned(),
        name: label.to_owned(),
        base_url: base_url.trim_end_matches('/').to_owned(),
        key_ref: format!("{}{}", core_domain::TOKEN_REF_PREFIX, provider_id),
        dialect: Some(dialect.to_owned()),
        reasoning_split: None,
        direct: Some(true),
        project: project
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty()),
    }
}

/// What an upsert did to one entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upserted {
    /// The entry was not there and now is.
    Added,
    /// The entry was there and its endpoint or project was updated.
    Updated,
    /// The entry already said exactly this.
    Unchanged,
}

/// Put one entry into a registry, or bring the existing one up to date.
///
/// An account that was reconnected can discover a different cloud project, and an
/// entry that kept the old one would bill the wrong project. Everything else on
/// the entry - the name an operator may have edited, the reasoning flag - is
/// left alone: this function owns the four fields a login can state and no more.
pub fn upsert(providers: &mut Vec<Provider>, entry: Provider) -> Upserted {
    match providers
        .iter_mut()
        .find(|existing| existing.id == entry.id)
    {
        None => {
            providers.push(entry);
            Upserted::Added
        }
        Some(existing) => {
            let changed = existing.key_ref != entry.key_ref
                || existing.base_url != entry.base_url
                || existing.dialect != entry.dialect
                || existing.direct != entry.direct
                || (entry.project.is_some() && existing.project != entry.project);
            if !changed {
                return Upserted::Unchanged;
            }
            existing.key_ref = entry.key_ref;
            existing.base_url = entry.base_url;
            existing.dialect = entry.dialect;
            existing.direct = entry.direct;
            if entry.project.is_some() {
                existing.project = entry.project;
            }
            Upserted::Updated
        }
    }
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
            direct: None,
            project: None,
        }
    }

    #[test]
    fn a_pasted_key_gets_a_deterministic_variable_name() {
        assert_eq!(sealed_env_name("deepseek"), "DEEPSEEK_API_KEY");
        assert_eq!(sealed_env_name("my-llm"), "MY_LLM_API_KEY");
        assert_eq!(sealed_env_name("a_b"), "A_B_API_KEY");
        // An id may start with a digit, a variable may not.
        assert_eq!(sealed_env_name("1password"), "P_1PASSWORD_API_KEY");
    }

    #[test]
    fn a_reference_is_kept_and_nothing_is_written() {
        let dir = tempfile::tempdir().expect("temp dir");
        let env_file = dir.path().join("eris.env");
        let mut provider = one("alpha");
        provider.key_ref = "env:ALPHA_KEY".to_owned();
        assert_eq!(
            adopt_submitted_key(&mut provider, &env_file).expect("kept"),
            None
        );
        assert_eq!(provider.key_ref, "env:ALPHA_KEY");
        assert!(!env_file.exists(), "a reference writes no file");

        provider.key_ref = "token:alpha".to_owned();
        assert_eq!(
            adopt_submitted_key(&mut provider, &env_file).expect("kept"),
            None
        );
        assert_eq!(provider.key_ref, "token:alpha");
    }

    /// A line this daemon did not write, assembled from pieces so the crate's
    /// secret-shaped-literal guard does not read it as a key.
    fn foreign_env_line() -> String {
        format!("{}={}", "KALLISTI_MODEL", "ds/deepseek-v4-flash")
    }

    #[test]
    fn a_pasted_key_is_written_to_the_env_file_and_referenced_by_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let env_file = dir.path().join("eris.env");
        std::fs::write(&env_file, format!("{}\n", foreign_env_line())).expect("seed");
        let mut provider = one("deepseek");
        provider.key_ref = "sk-live-abc123".to_owned();

        let name = adopt_submitted_key(&mut provider, &env_file)
            .expect("sealed")
            .expect("a name");
        assert_eq!(name, "DEEPSEEK_API_KEY");
        assert_eq!(provider.key_ref, "env:DEEPSEEK_API_KEY");

        let written = std::fs::read_to_string(&env_file).expect("read");
        assert!(
            written.contains("DEEPSEEK_API_KEY=sk-live-abc123"),
            "the value is in the file: {written}"
        );
        assert!(
            written.contains(&foreign_env_line()),
            "an existing line survives: {written}"
        );
        assert!(
            !written.contains(&provider.key_ref),
            "the registry reference is not a line in the file"
        );
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&env_file)
            .expect("meta")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "eris.env stays operator-only");

        // The value resolves in this process without a restart, which is the half
        // that makes the paste usable on the next turn.
        assert_eq!(
            crate::oauth::credential::env_value(&name).as_deref(),
            Some("sk-live-abc123")
        );
    }

    #[test]
    fn pasting_again_replaces_the_line_instead_of_adding_one() {
        let dir = tempfile::tempdir().expect("temp dir");
        let env_file = dir.path().join("eris.env");
        let mut provider = one("deepseek");
        provider.key_ref = "sk-first".to_owned();
        adopt_submitted_key(&mut provider, &env_file).expect("first");
        provider.key_ref = "sk-second".to_owned();
        adopt_submitted_key(&mut provider, &env_file).expect("second");

        let written = std::fs::read_to_string(&env_file).expect("read");
        assert_eq!(
            written.matches("DEEPSEEK_API_KEY=").count(),
            1,
            "one name, one line: {written}"
        );
        assert!(written.contains("DEEPSEEK_API_KEY=sk-second"), "{written}");
        assert!(
            !written.contains("sk-first"),
            "the old value is gone: {written}"
        );
    }

    #[test]
    fn a_value_the_env_file_cannot_carry_is_refused_by_name() {
        let dir = tempfile::tempdir().expect("temp dir");
        let env_file = dir.path().join("eris.env");
        let mut provider = one("deepseek");
        // A newline would end the line and turn the rest into another variable;
        // a quote would open a quoted value the operator never closed.
        for bad in [
            "sk-with space",
            "sk-with\"quote",
            "sk-with\nnewline",
            "sk-#hash",
        ] {
            provider.key_ref = bad.to_owned();
            let problem = adopt_submitted_key(&mut provider, &env_file).expect_err("refused");
            assert_eq!(problem.field, "provider_key_ref");
            assert!(
                problem.message.contains("env:NAME"),
                "the sentence says what to do instead: {}",
                problem.message
            );
            assert!(
                !problem.message.contains(bad.trim()),
                "the refusal never echoes the value: {}",
                problem.message
            );
            assert!(!env_file.exists(), "a refused value writes nothing");
        }
    }

    #[test]
    fn an_empty_key_field_is_refused_with_what_to_type() {
        let dir = tempfile::tempdir().expect("temp dir");
        let env_file = dir.path().join("eris.env");
        let mut provider = one("deepseek");
        provider.key_ref = "  ".to_owned();
        let problem = adopt_submitted_key(&mut provider, &env_file).expect_err("refused");
        assert_eq!(problem.field, "provider_key_ref");
        assert!(problem.message.contains("deepseek"), "{}", problem.message);
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

    #[test]
    fn a_connected_account_becomes_a_valid_registry_entry() {
        let entry = oauth_entry(
            "claude",
            "Claude",
            "https://api.anthropic.com/v1/",
            "messages",
            None,
        );
        assert_eq!(entry.key_ref, "token:claude");
        assert_eq!(
            entry.base_url, "https://api.anthropic.com/v1",
            "the trailing slash is trimmed once, here, not at every call site"
        );
        assert_eq!(entry.dialect.as_deref(), Some("messages"));
        assert_eq!(entry.direct, Some(true));
        assert_eq!(entry.project, None);
        assert!(
            validate(std::slice::from_ref(&entry)).is_ok(),
            "the entry this build writes has to pass this build's own validator"
        );
    }

    #[test]
    fn an_antigravity_entry_carries_its_project_and_drops_an_empty_one() {
        let entry = oauth_entry(
            "antigravity",
            "Google Antigravity",
            "https://daily-cloudcode-pa.googleapis.com",
            "antigravity",
            Some("  my-project  ".to_owned()),
        );
        assert_eq!(entry.project.as_deref(), Some("my-project"));

        let without = oauth_entry(
            "antigravity",
            "Google Antigravity",
            "https://daily-cloudcode-pa.googleapis.com",
            "antigravity",
            Some("   ".to_owned()),
        );
        assert_eq!(
            without.project, None,
            "a blank project is the same as no project, and Antigravity says so by name"
        );
    }

    #[test]
    fn upserting_twice_adds_once() {
        let mut registry = Vec::new();
        let entry = oauth_entry(
            "claude",
            "Claude",
            "https://api.anthropic.com/v1",
            "messages",
            None,
        );
        assert_eq!(upsert(&mut registry, entry.clone()), Upserted::Added);
        assert_eq!(upsert(&mut registry, entry), Upserted::Unchanged);
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn a_reconnect_that_discovers_a_new_project_updates_the_entry() {
        let mut registry = vec![oauth_entry(
            "antigravity",
            "Google Antigravity",
            "https://daily-cloudcode-pa.googleapis.com",
            "antigravity",
            Some("old-project".to_owned()),
        )];
        let reopened = oauth_entry(
            "antigravity",
            "Google Antigravity",
            "https://daily-cloudcode-pa.googleapis.com",
            "antigravity",
            Some("new-project".to_owned()),
        );
        assert_eq!(upsert(&mut registry, reopened), Upserted::Updated);
        assert_eq!(registry[0].project.as_deref(), Some("new-project"));

        // A re-login that discovered nothing must not erase what the last one
        // found: the entry still bills the project it knows.
        let blind = oauth_entry(
            "antigravity",
            "Google Antigravity",
            "https://daily-cloudcode-pa.googleapis.com",
            "antigravity",
            None,
        );
        assert_eq!(upsert(&mut registry, blind), Upserted::Unchanged);
        assert_eq!(registry[0].project.as_deref(), Some("new-project"));
    }

    #[test]
    fn an_entry_beside_the_gateway_does_not_disturb_it() {
        let mut registry = vec![one("default")];
        let entry = oauth_entry(
            "claude",
            "Claude",
            "https://api.anthropic.com/v1",
            "messages",
            None,
        );
        assert_eq!(upsert(&mut registry, entry), Upserted::Added);
        assert_eq!(registry.len(), 2);
        assert_eq!(registry[0].id, "default");
        assert!(
            validate(&registry).is_ok(),
            "a gateway and a login share one registry without arguing"
        );
    }
}
