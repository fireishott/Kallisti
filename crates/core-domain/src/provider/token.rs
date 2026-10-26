//! OAuth tokens: a record with an expiry, and a reference that names it.
//!
//! An API key and an OAuth token are not the same thing, and 1.0 keeps them
//! apart on purpose. A key is a value that lives in the environment behind a
//! [`super::secret::SecretRef`] and never changes. A token is a *record*: an
//! access token, a refresh token behind it, an expiry, and an origin that says
//! whether this daemon may refresh it at all.
//!
//! So `SecretRef` is untouched. The token path has its own types:
//!
//! | Type | What it is |
//! |---|---|
//! | [`TokenRef`] | `token:claude`, a name resolved through a [`TokenStore`] |
//! | [`TokenRecord`] | the grant itself, encrypted at rest by the store |
//!
//! Two rules are structural rather than documented:
//!
//! - [`TokenRecord`] has **no** `Serialize`, and its `Debug` is redacted. The
//!   only way a record's bytes leave this type is [`TokenRecord::to_store_json`],
//!   which the encrypting store calls and nothing else does. There is therefore
//!   no code path that can put a token in a settings row, a log line, an event
//!   payload, or an audit row by accident.
//! - the refresh decision lives in [`TokenRecord::usable`], in one place. A
//!   caller asks for a usable credential and gets either the access token or a
//!   typed [`TokenError`] that says what is wrong.

use serde::{Deserialize, Serialize};

/// Prefix that marks a reference as a token-store name, mirroring `env:` for
/// environment variables.
pub const TOKEN_REF_PREFIX: &str = "token:";

/// Where a stored grant came from, which decides whether it may be refreshed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenOrigin {
    /// Copied out of another program's database. That program still owns it and
    /// keeps it alive; this copy is a staging record and is **never** refreshed,
    /// because spending the refresh token would kill the owner's copy.
    Migrated,
    /// Produced by this daemon's own browser login. Nobody else holds it, so
    /// refreshing it is safe and is the normal path.
    Login,
}

impl TokenOrigin {
    /// Whether this origin may ever be refreshed.
    #[must_use]
    pub const fn is_refreshable(self) -> bool {
        matches!(self, Self::Login)
    }

    /// Stable lowercase name, for a status line.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Migrated => "migrated",
            Self::Login => "login",
        }
    }
}

/// Seconds before an expiry at which a refresh becomes worth doing.
///
/// Measured from the two live grants: Claude's token lives 8 hours and wants
/// four hours of headroom; Antigravity's lives one hour and wants five minutes.
/// An unknown provider gets no lead, which means "refresh only once expired".
#[must_use]
pub fn refresh_lead_secs(provider: &str) -> i64 {
    match provider {
        "claude" => 4 * 60 * 60,
        "antigravity" => 5 * 60,
        _ => 0,
    }
}

/// One stored grant.
///
/// No `Serialize`. See the module note: [`Self::to_store_json`] is the single
/// escape hatch, and it exists for the encrypting store.
#[derive(Clone, PartialEq, Eq)]
pub struct TokenRecord {
    /// Which provider this grant belongs to; also the file name in the store.
    pub provider: String,
    /// The bearer value a request carries. Secret.
    pub access_token: String,
    /// The value a refresh spends. Secret, and optional: a migrated read that
    /// only carried an access token still loads.
    pub refresh_token: Option<String>,
    /// Unix seconds. `None` means the source did not say.
    pub expires_at: Option<i64>,
    /// Scopes the grant was issued for, as granted.
    pub scopes: Vec<String>,
    /// Antigravity only: the cloud project the calls are billed to.
    pub project_id: Option<String>,
    /// `Bearer`, when the source said so.
    pub token_type: Option<String>,
    /// Unix seconds of the last successful refresh, when known.
    pub last_refresh: Option<i64>,
    /// Email or display name for the status line. Never a secret, always a label.
    pub account: Option<String>,
    /// Where it came from, which decides whether it may be refreshed.
    pub origin: TokenOrigin,
}

/// What the on-disk (pre-encryption) JSON looks like. Private, so the field
/// names live in one place.
#[derive(Serialize, Deserialize)]
struct StoreForm {
    provider: String,
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at: Option<i64>,
    #[serde(default)]
    scopes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_refresh: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account: Option<String>,
    origin: TokenOrigin,
}

impl TokenRecord {
    /// Plaintext JSON for the store to encrypt.
    ///
    /// Deliberately not `Serialize` on the type itself: a derived `Serialize`
    /// would let any caller write a token into a settings row. This method is
    /// the one place the plaintext bytes exist, and its only caller is the
    /// store's encrypt step.
    ///
    /// # Errors
    ///
    /// [`TokenError::Malformed`] if the record cannot be represented.
    pub fn to_store_json(&self) -> Result<Vec<u8>, TokenError> {
        let form = StoreForm {
            provider: self.provider.clone(),
            access_token: self.access_token.clone(),
            refresh_token: self.refresh_token.clone(),
            expires_at: self.expires_at,
            scopes: self.scopes.clone(),
            project_id: self.project_id.clone(),
            token_type: self.token_type.clone(),
            last_refresh: self.last_refresh,
            account: self.account.clone(),
            origin: self.origin,
        };
        serde_json::to_vec(&form).map_err(|error| TokenError::Malformed {
            provider: self.provider.clone(),
            detail: error.to_string(),
        })
    }

    /// Read plaintext JSON back. Called by the store after it decrypts.
    ///
    /// # Errors
    ///
    /// [`TokenError::Malformed`] when the decrypted bytes are not a record. The
    /// detail is the serde message, which names fields, never values.
    pub fn from_store_json(provider: &str, bytes: &[u8]) -> Result<Self, TokenError> {
        let form: StoreForm =
            serde_json::from_slice(bytes).map_err(|error| TokenError::Malformed {
                provider: provider.to_owned(),
                detail: error.to_string(),
            })?;
        Ok(Self {
            provider: form.provider,
            access_token: form.access_token,
            refresh_token: form.refresh_token,
            expires_at: form.expires_at,
            scopes: form.scopes,
            project_id: form.project_id,
            token_type: form.token_type,
            last_refresh: form.last_refresh,
            account: form.account,
            origin: form.origin,
        })
    }

    /// Whether the access token is still good at `now`.
    #[must_use]
    pub fn is_fresh(&self, now: i64) -> bool {
        match self.expires_at {
            // A record with no expiry cannot be shown to be stale, so it is used
            // as-is. That is what a source that omitted `expiresAt` means.
            None => true,
            Some(expires_at) => expires_at > now,
        }
    }

    /// Whether the expiry is close enough that a refresh should happen first.
    #[must_use]
    pub fn is_inside_lead(&self, now: i64) -> bool {
        match self.expires_at {
            None => false,
            Some(expires_at) => expires_at - refresh_lead_secs(&self.provider) <= now,
        }
    }

    /// The access token, if this record may be used right now.
    ///
    /// This is the single place that decides, and it never performs I/O: it
    /// either says the token is usable, or says which of the reasons it is not.
    /// A caller that gets [`TokenError::NeedsRefresh`] and has
    /// `oauth.auto_refresh` on may refresh and retry; a caller that gets
    /// [`TokenError::Expired`] may not, ever.
    ///
    /// # Errors
    ///
    /// - [`TokenError::Expired`] for a [`TokenOrigin::Migrated`] record past its
    ///   expiry. Never refreshable.
    /// - [`TokenError::NeedsRefresh`] for a [`TokenOrigin::Login`] record inside
    ///   its refresh lead.
    pub fn usable(&self, now: i64) -> Result<&str, TokenError> {
        if self.is_fresh(now) && !self.is_inside_lead(now) {
            return Ok(self.access_token.as_str());
        }
        if !self.origin.is_refreshable() {
            return Err(TokenError::Expired {
                provider: self.provider.clone(),
                origin: self.origin,
            });
        }
        Err(TokenError::NeedsRefresh {
            provider: self.provider.clone(),
        })
    }
}

impl std::fmt::Debug for TokenRecord {
    /// Redacted. Both token values are replaced, so `{:?}` on a containing
    /// struct, a `tracing::debug!` of the record, and a panic message are all
    /// safe. The non-secret fields stay readable, because a diagnostic that says
    /// nothing is a diagnostic nobody uses.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenRecord")
            .field("provider", &self.provider)
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &if self.refresh_token.is_some() {
                    "<redacted>"
                } else {
                    "<none>"
                },
            )
            .field("expires_at", &self.expires_at)
            .field("scopes", &self.scopes)
            .field("project_id", &self.project_id)
            .field("token_type", &self.token_type)
            .field("last_refresh", &self.last_refresh)
            .field("account", &self.account)
            .field("origin", &self.origin)
            .finish()
    }
}

/// A parsed `token:NAME` reference.
///
/// The provider id travels; the token does not. This is the token-side twin of
/// [`super::secret::SecretRef`], deliberately a separate type so the `env:` path
/// every existing deployment uses is untouched by the token work.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TokenRef(String);

impl TokenRef {
    /// Build a reference to a stored grant.
    ///
    /// # Errors
    ///
    /// [`TokenError::BadReference`] for an empty name, a nested prefix, or a name
    /// that is not lowercase letters, digits, hyphen or underscore.
    pub fn new(provider: impl Into<String>) -> Result<Self, TokenError> {
        let provider = provider.into();
        if provider.is_empty() {
            return Err(TokenError::BadReference {
                detail: "empty token reference".to_owned(),
            });
        }
        if provider.starts_with(TOKEN_REF_PREFIX) {
            return Err(TokenError::BadReference {
                detail: "token reference name must not repeat the `token:` prefix".to_owned(),
            });
        }
        if !provider
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        {
            return Err(TokenError::BadReference {
                detail: format!(
                    "token reference `{provider}` must be lowercase letters, digits, hyphen or underscore"
                ),
            });
        }
        Ok(Self(provider))
    }

    /// The provider id this reference names.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.0
    }

    /// The wire form, `token:NAME`.
    #[must_use]
    pub fn wire(&self) -> String {
        format!("{TOKEN_REF_PREFIX}{}", self.0)
    }
}

impl TryFrom<&str> for TokenRef {
    type Error = TokenError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let Some(name) = value.trim().strip_prefix(TOKEN_REF_PREFIX) else {
            return Err(TokenError::BadReference {
                detail: "token reference must start with `token:`".to_owned(),
            });
        };
        Self::new(name)
    }
}

impl TryFrom<String> for TokenRef {
    type Error = TokenError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_from(value.as_str())
    }
}

impl From<TokenRef> for String {
    fn from(value: TokenRef) -> Self {
        value.wire()
    }
}

/// The store seam: read, write, delete, list. No I/O in `core-domain`, so the
/// trait is sync, object-safe, and implemented by `eris-host` over encrypted
/// files.
pub trait TokenStore {
    /// The record for one provider, or `None` when there is none.
    ///
    /// # Errors
    ///
    /// [`TokenError::Store`] for an unreadable, undecryptable, or malformed file.
    fn load(&self, provider: &str) -> Result<Option<TokenRecord>, TokenError>;

    /// Write a record, creating the directory and file with owner-only modes.
    ///
    /// # Errors
    ///
    /// [`TokenError::Store`] when the bytes cannot be written or encrypted.
    fn store_token(&self, record: &TokenRecord) -> Result<(), TokenError>;

    /// Delete a record. Deleting one that is not there is not an error.
    ///
    /// # Errors
    ///
    /// [`TokenError::Store`] when the file exists and cannot be removed.
    fn delete(&self, provider: &str) -> Result<(), TokenError>;

    /// Every provider id with a stored record, sorted.
    ///
    /// # Errors
    ///
    /// [`TokenError::Store`] when the directory cannot be read.
    fn list(&self) -> Result<Vec<String>, TokenError>;
}

/// What went wrong with a token.
///
/// Every message names the provider and never a value, so an error is safe to
/// log, safe to render in the Accounts panel, and safe to put in an audit row.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    /// A migrated grant has gone stale. It is never refreshed, because the
    /// program it was copied from still owns it.
    #[error(
        "the {provider} token is expired; a re-login is needed, because a copied grant is never refreshed"
    )]
    Expired {
        /// Provider id.
        provider: String,
        /// Where the grant came from.
        origin: TokenOrigin,
    },
    /// A grant this daemon owns is close enough to expiry that a refresh should
    /// happen before the request. Only reachable with `oauth.auto_refresh` on.
    #[error("the {provider} token is inside its refresh lead and must be refreshed before use")]
    NeedsRefresh {
        /// Provider id.
        provider: String,
    },
    /// No record for that provider.
    #[error("no {provider} token is stored")]
    Missing {
        /// Provider id.
        provider: String,
    },
    /// The record could not be read, decrypted, or understood.
    #[error("the {provider} token could not be read: {detail}")]
    Store {
        /// Provider id, or the file stem when the id is not known.
        provider: String,
        /// What failed, with no secret in it.
        detail: String,
    },
    /// The decrypted bytes were not a record.
    #[error("the {provider} token record is malformed: {detail}")]
    Malformed {
        /// Provider id.
        provider: String,
        /// The serde message, which names fields and not values.
        detail: String,
    },
    /// A `token:` reference did not parse.
    #[error("bad token reference: {detail}")]
    BadReference {
        /// Why it did not parse.
        detail: String,
    },
}

impl TokenError {
    /// Stable wire name, for a status panel and for a log line.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Expired { .. } => "expired",
            Self::NeedsRefresh { .. } => "needs_refresh",
            Self::Missing { .. } => "missing",
            Self::Store { .. } => "store",
            Self::Malformed { .. } => "malformed",
            Self::BadReference { .. } => "bad_reference",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(origin: TokenOrigin, expires_at: Option<i64>) -> TokenRecord {
        TokenRecord {
            provider: "claude".to_owned(),
            access_token: "sk-ant-oat01-SECRET-ACCESS-VALUE".to_owned(),
            refresh_token: Some("sk-ant-ort01-SECRET-REFRESH-VALUE".to_owned()),
            expires_at,
            scopes: vec!["user:inference".to_owned()],
            project_id: None,
            token_type: Some("Bearer".to_owned()),
            last_refresh: Some(1_700_000_000),
            account: Some("operator@example.com".to_owned()),
            origin,
        }
    }

    #[test]
    fn a_token_reference_is_a_name_and_never_a_value() {
        let reference = TokenRef::try_from("token:claude").expect("parses");
        assert_eq!(reference.provider(), "claude");
        assert_eq!(reference.wire(), "token:claude");

        assert!(TokenRef::try_from("sk-ant-oat01-SECRET").is_err());
        assert!(TokenRef::try_from("token:").is_err());
        assert!(TokenRef::try_from("token:token:claude").is_err());
        assert!(TokenRef::try_from("token:Claude").is_err());

        let json = serde_json::to_string(&reference).expect("serialises");
        assert_eq!(json, "\"token:claude\"");
        assert_eq!(
            serde_json::from_str::<TokenRef>("\"token:claude\"").expect("parses"),
            reference
        );
        assert!(serde_json::from_str::<TokenRef>("\"sk-ant-oat01-SECRET\"").is_err());
    }

    #[test]
    fn debug_never_prints_either_token() {
        let record = record(TokenOrigin::Login, Some(1_800_000_000));
        let rendered = format!("{record:?}");
        assert!(!rendered.contains("SECRET-ACCESS-VALUE"), "{rendered}");
        assert!(!rendered.contains("SECRET-REFRESH-VALUE"), "{rendered}");
        assert!(rendered.contains("<redacted>"));
        assert!(rendered.contains("claude"), "the label is still useful");

        let holder = vec![record];
        let rendered = format!("{holder:?}");
        assert!(!rendered.contains("SECRET-ACCESS-VALUE"), "{rendered}");
    }

    #[test]
    fn the_store_json_round_trips_and_carries_the_origin() {
        let record = record(TokenOrigin::Migrated, Some(1_800_000_000));
        let bytes = record.to_store_json().expect("serialises");
        let back = TokenRecord::from_store_json("claude", &bytes).expect("parses");
        assert_eq!(back, record);
        assert_eq!(back.origin, TokenOrigin::Migrated);

        let mut thin = record;
        thin.refresh_token = None;
        thin.expires_at = None;
        thin.project_id = Some("proj-123".to_owned());
        let bytes = thin.to_store_json().expect("serialises");
        let back = TokenRecord::from_store_json("claude", &bytes).expect("parses");
        assert_eq!(back, thin);
    }

    #[test]
    fn malformed_bytes_are_a_typed_error_rather_than_a_panic() {
        let error =
            TokenRecord::from_store_json("claude", b"not json at all").expect_err("must fail");
        assert!(matches!(error, TokenError::Malformed { .. }));
        assert_eq!(error.kind(), "malformed");
        assert!(error.to_string().contains("claude"));
    }

    #[test]
    fn a_migrated_token_past_its_expiry_is_expired_and_never_refreshable() {
        let record = record(TokenOrigin::Migrated, Some(1_000));
        let error = record.usable(2_000).expect_err("stale migrated grant");
        assert!(matches!(
            error,
            TokenError::Expired {
                origin: TokenOrigin::Migrated,
                ..
            }
        ));
        assert_eq!(error.kind(), "expired");
        let message = error.to_string();
        assert!(message.contains("re-login"), "{message}");
        assert!(!message.contains("SECRET"), "{message}");
    }

    #[test]
    fn a_migrated_token_outside_its_lead_is_usable_while_it_is_fresh() {
        let record = record(TokenOrigin::Migrated, Some(10_000));
        assert_eq!(
            record.usable(10_000 - 5 * 3600).expect("usable"),
            "sk-ant-oat01-SECRET-ACCESS-VALUE"
        );
    }

    #[test]
    fn a_login_grant_inside_its_lead_asks_for_a_refresh() {
        let record = record(TokenOrigin::Login, Some(10_000));
        let error = record
            .usable(10_000 - 60)
            .expect_err("inside the four-hour lead");
        assert!(matches!(error, TokenError::NeedsRefresh { .. }));
        assert_eq!(error.kind(), "needs_refresh");

        assert!(record.usable(10_000 - 5 * 3600).is_ok());
        assert!(matches!(
            record.usable(20_000),
            Err(TokenError::NeedsRefresh { .. })
        ));
    }

    #[test]
    fn the_refresh_leads_are_the_measured_ones() {
        assert_eq!(refresh_lead_secs("claude"), 14_400);
        assert_eq!(refresh_lead_secs("antigravity"), 300);
        assert_eq!(refresh_lead_secs("something-else"), 0);
    }

    #[test]
    fn only_a_login_grant_is_refreshable() {
        assert!(!TokenOrigin::Migrated.is_refreshable());
        assert!(TokenOrigin::Login.is_refreshable());
        assert_eq!(TokenOrigin::Migrated.as_str(), "migrated");
        assert_eq!(TokenOrigin::Login.as_str(), "login");
    }

    #[test]
    fn a_record_with_no_expiry_is_used_as_is() {
        let record = record(TokenOrigin::Migrated, None);
        assert!(record.is_fresh(1_000_000));
        assert!(!record.is_inside_lead(1_000_000));
        assert!(record.usable(1_000_000).is_ok());
    }
}
