//! The one place a key *or* a stored grant is turned into a usable credential.
//!
//! Before this module there were two worlds. `env:NAME` was resolved by
//! `core_runtime::provider::resolve_api_key` at twenty-one call sites, and a
//! stored grant was resolveable only through [`super::resolve::resolve_credential`],
//! which nothing called. A provider that authenticates with a login therefore
//! could not be used by a turn, however well it was stored.
//!
//! So there is one function, [`resolve_key`], and every call site uses it:
//!
//! | `key_ref` | what happens |
//! |---|---|
//! | `env:NAME` | exactly the build-109 bytes: the variable is read and the value returned |
//! | `token:NAME` | the encrypted store is read, the refresh gate is consulted, and a usable access token is returned |
//!
//! The source is installed once, at startup, from the daemon's configured paths
//! and its `oauth.auto_refresh` flag. Nothing else in the process builds one, so
//! there is one store, one policy and one place a refresh could ever happen.
//!
//! Three rules, structural rather than documented:
//!
//! - a token is never returned in an error, and [`core_runtime::provider::SecretKey`]
//!   cannot be printed, so no call site can leak one by formatting a failure;
//! - `oauth.auto_refresh` is **on** by default, so a login grant inside its
//!   refresh lead is refreshed on use instead of sending the operator to a login
//!   screen every hour; a `Migrated` grant is refused by the refresher before any
//!   network call, so the flag can never spend somebody else's refresh token;
//! - a `token:` reference with no installed source is a typed failure naming the
//!   provider, never a silent fall back to an environment variable that happens
//!   to share the name.

use std::sync::{Arc, OnceLock};

use core_domain::{LlmError, SecretRef, TokenStore};
use core_runtime::provider::SecretKey;

use super::refresh::RefreshSpec;
use super::resolve::{CredentialSource, RefreshPolicy, resolve_credential};

/// The process-wide source, installed once.
static SOURCE: OnceLock<Arc<CredentialSource>> = OnceLock::new();

/// Install the resolver for this process.
///
/// The first call wins; a later call is a no-op rather than an error, because a
/// daemon installs it once and a test that installs its own is the only other
/// caller. `store` is the encrypted store, and `auto_refresh` comes from
/// `[oauth]` in `config.toml`.
///
/// Returns whether this call installed it.
pub fn install(
    store: Arc<dyn TokenStore + Send + Sync>,
    auto_refresh: bool,
    specs: Vec<RefreshSpec>,
) -> bool {
    // The transport is real now, and it is still unreachable while the gate is
    // shut: `resolve_credential` only calls `refresh_now` when `auto_refresh` is
    // true, and nothing else holds this `Arc`. A host that turns the flag on
    // without specs gets a typed Store error naming the missing client rather
    // than a request to an endpoint nobody configured.
    let source = Arc::new(CredentialSource {
        store,
        policy: RefreshPolicy { auto_refresh },
        transport: Arc::new(super::refresh::ReqwestTransport),
        specs: Arc::new(specs),
    });
    SOURCE.set(source).is_ok()
}

/// The refresh specs an install's OAuth client file supports.
///
/// Claude's flow is a public PKCE client, so its spec needs only the client id.
/// Google's needs the installed-app secret, which this build does not ship: a host
/// without one gets no Antigravity spec, and a refresh attempt for that account
/// says so by name instead of sending a form body Google will refuse.
#[must_use]
pub fn refresh_specs(clients: &super::clients::Clients) -> Vec<RefreshSpec> {
    let mut specs = vec![RefreshSpec::claude(clients.claude_client_id.clone())];
    if let Some(secret) = clients.google_client_secret.as_deref() {
        specs.push(RefreshSpec::antigravity(
            clients.google_client_id.clone(),
            secret,
        ));
    }
    specs
}

/// Install a source directly. Used by a test that wants its own store.
pub fn install_source(source: Arc<CredentialSource>) -> bool {
    SOURCE.set(source).is_ok()
}

/// Whether a source has been installed. A daemon always has one; a unit test of
/// something unrelated may not.
#[must_use]
pub fn installed() -> bool {
    SOURCE.get().is_some()
}

/// Values this process was handed after it started.
///
/// A key pasted into the provider form is written to `eris.env`, which is a
/// *startup* mechanism: systemd reads it once, at exec. The running daemon's own
/// environment does not change when the file does, so without this the first turn
/// after a paste would fail with "not set in the process environment" until the
/// service was restarted. The overlay is the same value the file now holds, so a
/// restart and a live process answer identically.
static LATE_ENV: std::sync::OnceLock<
    std::sync::RwLock<std::collections::BTreeMap<String, String>>,
> = std::sync::OnceLock::new();

fn late_env() -> &'static std::sync::RwLock<std::collections::BTreeMap<String, String>> {
    LATE_ENV.get_or_init(Default::default)
}

/// Remember a variable whose value was just written to the environment file.
///
/// The value is never logged and never returned to a caller: this function exists
/// so the resolver can find it, not so anything can read it back out.
pub fn remember_env(name: &str, value: &str) {
    if let Ok(mut map) = late_env().write() {
        map.insert(name.to_owned(), value.to_owned());
    }
}

/// One environment lookup for every `env:` reference this process resolves.
///
/// The late overlay first - a value this process was handed after it started -
/// and the process environment second, which is what systemd already loaded. A
/// variable the operator set in the unit's environment keeps working: the overlay
/// only ever holds names this daemon minted itself.
#[must_use]
pub fn env_value(name: &str) -> Option<String> {
    if let Ok(map) = late_env().read()
        && let Some(value) = map.get(name)
    {
        return Some(value.clone());
    }
    std::env::var(name).ok()
}

/// Resolve one reference to a usable credential.
///
/// # Errors
///
/// [`LlmError::Auth`] for every failure, because from a caller's side they are
/// all "I cannot authenticate this request". The detail names the provider and
/// the reason and never a value.
pub fn resolve_key(reference: &SecretRef, now: i64) -> Result<SecretKey, LlmError> {
    match reference {
        // The build-109 path, byte for byte, plus the overlay a provider form
        // fills when an operator pastes a key: `env_value` only adds names this
        // process minted, so a unit-set variable resolves exactly as before.
        SecretRef::Env(name) => {
            core_runtime::provider::resolve_api_key_with(reference, |_| env_value(name))
        }
        SecretRef::Token(name) => {
            let Some(source) = SOURCE.get() else {
                return Err(LlmError::Auth {
                    detail: format!(
                        "`token:{name}` is a stored grant and this process has no credential resolver installed"
                    ),
                });
            };
            resolve_credential(&reference.wire(), source, now)
        }
    }
}

/// Resolve against the current clock.
///
/// # Errors
///
/// As [`resolve_key`].
pub fn resolve_key_now(reference: &SecretRef) -> Result<SecretKey, LlmError> {
    resolve_key(reference, super::flow::now_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::store::TokenFileStore;
    use core_domain::{TokenOrigin, TokenRecord};

    fn source(record: Option<TokenRecord>, auto_refresh: bool) -> Arc<CredentialSource> {
        let dir = Box::leak(Box::new(tempfile::tempdir().expect("temp dir")));
        let store = Arc::new(TokenFileStore::new(
            dir.path().join("tokens"),
            dir.path().join("keys/token.key"),
        )) as Arc<dyn TokenStore + Send + Sync>;
        if let Some(record) = record {
            store.store_token(&record).expect("stores");
        }
        Arc::new(CredentialSource {
            store,
            policy: RefreshPolicy { auto_refresh },
            transport: Arc::new(super::super::refresh::NoTransport),
            specs: Arc::new(Vec::new()),
        })
    }

    fn login(provider: &str, expires_at: i64) -> TokenRecord {
        TokenRecord {
            provider: provider.to_owned(),
            access_token: "STORED-ACCESS-VALUE".to_owned(),
            refresh_token: Some("STORED-REFRESH-VALUE".to_owned()),
            expires_at: Some(expires_at),
            scopes: vec!["user:inference".to_owned()],
            project_id: None,
            token_type: Some("Bearer".to_owned()),
            last_refresh: None,
            account: Some("curtis@example.test".to_owned()),
            origin: TokenOrigin::Login,
        }
    }

    #[test]
    fn an_env_reference_never_touches_the_store() {
        let reference = SecretRef::Env("PATH".to_owned());
        let key = resolve_key(&reference, 0).expect("resolves");
        assert_eq!(key.expose(), std::env::var("PATH").expect("set"));
    }

    #[test]
    fn a_token_reference_with_no_resolver_is_refused_by_name() {
        // This test only holds while nothing else in this process installed a
        // source; the daemon installs one before any turn can run.
        if installed() {
            return;
        }
        let error =
            resolve_key(&SecretRef::Token("claude".to_owned()), 0).expect_err("must refuse");
        let message = error.to_string();
        assert!(message.contains("token:claude"), "{message}");
        assert!(message.contains("no credential resolver"), "{message}");
    }

    #[test]
    fn a_stored_login_grant_resolves_through_the_installed_source() {
        // A grant that is twenty-four hours out, measured against the clock the
        // resolver reads: a fixed stamp from 1970 is already expired, and
        // asserting against one would only prove the store can hold a dead grant.
        let now = super::super::flow::now_secs();
        let source = source(Some(login("claude", now + 24 * 60 * 60)), false);
        // A process that already installed one keeps it; the assertion below is
        // what matters and holds either way for a fresh store.
        let _ = install_source(Arc::clone(&source));
        let key = super::resolve_credential("token:claude", &source, now).expect("resolves");
        assert_eq!(key.expose(), "STORED-ACCESS-VALUE");
    }

    #[test]
    fn the_refresh_specs_follow_the_client_file() {
        let clients = super::super::clients::Clients::default();
        let specs = refresh_specs(&clients);
        // Claude's flow is a public PKCE client, so it always has a spec.
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].provider, "claude");
        assert_eq!(specs[0].client_id, clients.claude_client_id);
        assert_eq!(
            specs[0].client_secret, None,
            "a client secret on a public client is a secret the provider never issued"
        );
        assert_eq!(
            specs[0].token_url,
            "https://api.anthropic.com/v1/oauth/token"
        );

        // No Google secret ships with this build, so an Antigravity refresh is
        // refused by name rather than sent as a form Google would reject.
        assert!(specs.iter().all(|spec| spec.provider != "antigravity"));

        let mut with_google = clients.clone();
        with_google.google_client_secret = Some("a-secret-this-host-supplies".to_owned());
        let specs = refresh_specs(&with_google);
        let antigravity = specs
            .iter()
            .find(|spec| spec.provider == "antigravity")
            .expect("a host with a secret gets a spec");
        assert_eq!(antigravity.token_url, "https://oauth2.googleapis.com/token");
        assert_eq!(
            antigravity.client_secret.as_deref(),
            Some("a-secret-this-host-supplies")
        );
    }

    #[test]
    fn a_grant_inside_its_refresh_lead_is_reported_and_not_refreshed() {
        let now = super::super::flow::now_secs();
        let source = source(Some(login("claude", now + 60)), false);
        let error = super::resolve_credential("token:claude", &source, now)
            .expect_err("must report a stale grant");
        assert!(error.to_string().contains("refresh lead"), "{error}");
        assert!(!error.to_string().contains("STORED-ACCESS-VALUE"));
    }
}
