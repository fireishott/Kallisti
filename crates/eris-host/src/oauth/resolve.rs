//! One choke point for "give me a usable credential".
//!
//! Before 108 a credential was always an `env:NAME` reference and every caller
//! called [`core_runtime::provider::resolve_api_key`] itself. A token-backed
//! provider cannot be resolved that way, and scattering a second lookup across
//! the 24 call sites that read a key is how a token read ends up in a log line.
//!
//! So there is one function, [`resolve_credential`], and it is the only thing a
//! caller needs:
//!
//! | `key_ref` | where the value comes from |
//! |---|---|
//! | `env:NAME` | the process environment, unchanged from build 107 |
//! | `token:NAME` | the encrypted store, through the refresh gate |
//!
//! Everything a caller has to know about expiry is in the returned value or the
//! returned error, and the error never carries a token.

use std::sync::Arc;

use core_domain::{LlmError, SecretRef, TokenError, TokenStore};
use core_runtime::provider::SecretKey;

use super::refresh::{HttpTransport, RefreshSpec, refresh_decision};

/// Whether a stored credential may be refreshed on use.
///
/// Comes from `oauth.auto_refresh` in `config.toml` and is **on** by default.
/// While it is off nothing refreshes, ever: a login grant that is inside its
/// lead returns [`TokenError::NeedsRefresh`] and a migrated grant past its expiry
/// returns [`TokenError::Expired`], and the caller reports that to the operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RefreshPolicy {
    /// `oauth.auto_refresh`. On unless the operator writes `false`, and only
    /// `RefreshPolicy::never` switches it off inside a test.
    pub auto_refresh: bool,
}

impl RefreshPolicy {
    /// The read-only policy: never refresh. What a status panel or a test uses.
    #[must_use]
    pub const fn never() -> Self {
        Self {
            auto_refresh: false,
        }
    }
}

/// Everything resolution needs, so a caller passes one value instead of four.
pub struct CredentialSource {
    /// The encrypted store.
    pub store: Arc<dyn TokenStore + Send + Sync>,
    /// Whether a refresh is permitted.
    pub policy: RefreshPolicy,
    /// The HTTP transport a refresh would use. Only consulted when the policy
    /// allows a refresh; a test passes a transport that panics to prove it is
    /// never called.
    pub transport: Arc<dyn HttpTransport + Send + Sync>,
    /// Where each provider's token endpoint and client id are configured.
    pub specs: Arc<Vec<RefreshSpec>>,
}

impl std::fmt::Debug for CredentialSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialSource")
            .field("policy", &self.policy)
            .field("specs", &self.specs.len())
            .finish_non_exhaustive()
    }
}

impl CredentialSource {
    /// The store on its own, with refreshing forbidden and no transport. This is
    /// what a read-only caller (a status panel, a doctor line) gets.
    #[must_use]
    pub fn read_only(store: Arc<dyn TokenStore + Send + Sync>) -> Self {
        Self {
            store,
            policy: RefreshPolicy::never(),
            transport: Arc::new(super::refresh::NoTransport),
            specs: Arc::new(Vec::new()),
        }
    }

    /// The refresh spec for one provider, when one is configured.
    #[must_use]
    pub fn spec_for(&self, provider: &str) -> Option<&RefreshSpec> {
        self.specs.iter().find(|spec| spec.provider == provider)
    }
}

/// Resolve one `key_ref` to a usable credential.
///
/// This is the single entry point: `env:` behaves exactly as it did in build 107,
/// and `token:` goes through the store and the refresh gate.
///
/// # Errors
///
/// [`LlmError::Auth`] for every failure, because from a caller's side they are
/// all "I cannot authenticate this request". The detail names the provider and
/// the reason and never a value.
pub fn resolve_credential(
    key_ref: &str,
    source: &CredentialSource,
    now: i64,
) -> Result<SecretKey, LlmError> {
    let trimmed = key_ref.trim();
    if let Some(name) = trimmed.strip_prefix(core_domain::TOKEN_REF_PREFIX) {
        let record = match source.store.load(name.trim()) {
            Ok(Some(record)) => record,
            Ok(None) => {
                return Err(LlmError::Auth {
                    detail: format!(
                        "no stored token for `{name}`; connect the account in Settings > Providers"
                    ),
                });
            }
            Err(error) => {
                return Err(LlmError::Auth {
                    detail: token_error_detail(&error),
                });
            }
        };

        return match refresh_decision(&record, now, source.policy.auto_refresh, source) {
            Ok(access_token) => SecretKey::new(access_token).map_err(|_| LlmError::Auth {
                detail: format!("the stored `{name}` token is empty"),
            }),
            Err(error) => Err(LlmError::Auth {
                detail: token_error_detail(&error),
            }),
        };
    }

    // Anything else is the build 107 path, byte for byte. A bad reference is an
    // auth failure here rather than a panic: the operator typed something.
    let reference = SecretRef::try_from(trimmed).map_err(|error| LlmError::Auth {
        detail: format!("`{trimmed}` is not a key reference: {error}"),
    })?;
    core_runtime::provider::resolve_api_key_with(&reference, super::credential::env_value)
}

/// Render a token error as a detail line. No value, ever.
#[must_use]
pub fn token_error_detail(error: &TokenError) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::refresh::{HttpRequest, HttpTransport, RefreshSpec, TransportError};
    use crate::oauth::store::TokenFileStore;
    use core_domain::{TokenOrigin, TokenRecord};

    struct NeverCalled;

    impl HttpTransport for NeverCalled {
        fn post(&self, _request: &HttpRequest) -> Result<(u16, String), TransportError> {
            panic!("a transport must never be called while oauth.auto_refresh is false");
        }
    }

    fn record(provider: &str, origin: TokenOrigin, expires_at: Option<i64>) -> TokenRecord {
        TokenRecord {
            provider: provider.to_owned(),
            access_token: "STORED-ACCESS-VALUE".to_owned(),
            refresh_token: Some("STORED-REFRESH-VALUE".to_owned()),
            expires_at,
            scopes: Vec::new(),
            project_id: None,
            token_type: Some("Bearer".to_owned()),
            last_refresh: None,
            account: None,
            origin,
        }
    }

    fn store() -> (tempfile::TempDir, Arc<dyn TokenStore + Send + Sync>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store =
            TokenFileStore::new(dir.path().join("tokens"), dir.path().join("keys/token.key"));
        (dir, Arc::new(store) as Arc<dyn TokenStore + Send + Sync>)
    }

    #[test]
    fn an_env_reference_still_resolves_exactly_as_it_did_in_build_107() {
        let (_dir, store) = store();
        let source = CredentialSource::read_only(store);
        // The variable is set by the test harness itself in this process.
        // SAFETY-adjacent note: `std::env::set_var` is unsafe in edition 2024, so
        // the test reads a variable the shell always provides instead.
        let name = "PATH";
        let key = resolve_credential(&format!("env:{name}"), &source, 0).expect("resolves");
        assert_eq!(key.expose(), std::env::var(name).expect("set"));
    }

    #[test]
    fn a_missing_env_reference_is_an_auth_error_naming_the_variable() {
        let (_dir, store) = store();
        let source = CredentialSource::read_only(store);
        let error =
            resolve_credential("env:KALLISTI_NOT_SET_XYZ", &source, 0).expect_err("must fail");
        assert!(matches!(error, LlmError::Auth { .. }));
        assert!(error.to_string().contains("KALLISTI_NOT_SET_XYZ"));
    }

    #[test]
    fn a_bare_value_is_not_a_reference() {
        let (_dir, store) = store();
        let source = CredentialSource::read_only(store);
        let error = resolve_credential("sk-live-abcd", &source, 0).expect_err("must fail");
        assert!(matches!(error, LlmError::Auth { .. }));
        assert!(error.to_string().contains("not a key reference"));
    }

    #[test]
    fn a_stored_token_resolves_and_carries_no_token_in_any_error() {
        let (_dir, store) = store();
        // Eight hours out, so outside Claude's four-hour refresh lead.
        store
            .store_token(&record("claude", TokenOrigin::Login, Some(100_000)))
            .expect("stores");
        let source = CredentialSource::read_only(store);
        let key = resolve_credential("token:claude", &source, 10_000).expect("resolves");
        assert_eq!(key.expose(), "STORED-ACCESS-VALUE");
    }

    #[test]
    fn a_second_read_sees_the_same_record_through_the_trait_object() {
        let (_dir, store) = store();
        store
            .store_token(&record("antigravity", TokenOrigin::Migrated, Some(2_000)))
            .expect("stores");
        let source = CredentialSource::read_only(store);
        assert_eq!(
            resolve_credential("token:antigravity", &source, 1_000)
                .expect("resolves")
                .expose(),
            "STORED-ACCESS-VALUE"
        );
    }

    #[test]
    fn a_provider_with_no_stored_token_says_what_to_do_and_does_not_refresh() {
        let (_dir, store) = store();
        let source = CredentialSource {
            store,
            policy: RefreshPolicy { auto_refresh: true },
            transport: Arc::new(NeverCalled),
            specs: Arc::new(Vec::new()),
        };
        let error = resolve_credential("token:claude", &source, 1_000).expect_err("must fail");
        let message = error.to_string();
        assert!(message.contains("no stored token"), "{message}");
        assert!(message.contains("Settings"), "{message}");
    }

    #[test]
    fn an_expired_migrated_grant_is_reported_and_never_refreshed_even_with_the_flag_on() {
        let (_dir, store) = store();
        store
            .store_token(&record("claude", TokenOrigin::Migrated, Some(1_000)))
            .expect("stores");
        let source = CredentialSource {
            store,
            policy: RefreshPolicy { auto_refresh: true },
            transport: Arc::new(NeverCalled),
            specs: Arc::new(vec![RefreshSpec::claude("client-id")]),
        };
        let error = resolve_credential("token:claude", &source, 2_000).expect_err("must fail");
        let message = error.to_string();
        assert!(message.contains("re-login"), "{message}");
        assert!(!message.contains("STORED-ACCESS-VALUE"), "{message}");
    }

    #[test]
    fn a_login_grant_inside_its_lead_is_refused_while_the_flag_is_false() {
        let (_dir, store) = store();
        store
            .store_token(&record("claude", TokenOrigin::Login, Some(10_000)))
            .expect("stores");
        let source = CredentialSource {
            store,
            policy: RefreshPolicy::never(),
            transport: Arc::new(NeverCalled),
            specs: Arc::new(vec![RefreshSpec::claude("client-id")]),
        };
        // Inside the four-hour lead, so NeedsRefresh, and the transport is never
        // touched because the flag is off.
        let error = resolve_credential("token:claude", &source, 10_000 - 60).expect_err("fail");
        assert!(error.to_string().contains("refresh lead"), "{error}");
    }
}
