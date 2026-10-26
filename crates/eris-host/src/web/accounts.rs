//! The Accounts panel: the status of each login, and the two presses.
//!
//! This module is the surface for `crate::oauth`. Everything it does is one of
//! three things: read the store to say whether an account is connected, start a
//! login, or delete a sealed grant. It never renders a secret, because the types
//! it holds cannot print one and the only function that returns a bearer value is
//! the one the turn machine calls.
//!
//! The login is four moves: Connect, open the link in a new tab, approve, then
//! paste the address the browser lands on into the field and press Verify. A
//! browser on this host skips the paste, because the loopback listener catches
//! the redirect itself.
//!
//! Two rules worth stating, because both are easy to get wrong in a web handler:
//!
//! - **the connect press starts a login and reports a URL; it does not follow
//!   it.** Opening a browser is a thing a *person* does on the machine in front of
//!   them, and the daemon may well be headless. So the answer is a 303 back to
//!   Settings carrying the URL in the notice, and the page offers it as a link. An
//!   operator on a laptop sees the same page and clicks the same link.
//! - **a disconnect is local.** It deletes the record this daemon sealed. Neither
//!   provider has a revoke endpoint this build has proven, and guessing one is how
//!   a "disconnect" logs somebody out of something else.

use core_domain::TokenStore;

use super::templates::AccountView;
use crate::oauth::flow::Provider;
use crate::oauth::{Callbacks, Pending, Pkce, now_secs};

/// Build one row per account this build can log in to.
///
/// A store that cannot be read is a row that says so rather than a page that
/// fails: the panel exists to tell the operator what state each account is in,
/// and "the store would not open" is a state.
pub fn rows(
    callbacks: &Callbacks,
    offset_minutes: i64,
    registry: &[crate::providers::Provider],
) -> Vec<AccountView> {
    let now = now_secs();
    // One directory read for the whole panel, turned into an optional fault: a
    // store that cannot be listed is a problem with the daemon rather than with
    // one account, so every row states it instead of one row failing the page.
    let listing = callbacks.store.list().err().map(|error| error.to_string());
    Provider::ALL
        .into_iter()
        .map(|provider| {
            let mut row = AccountView {
                provider: provider.id().to_owned(),
                label: provider.label().to_owned(),
                base_url: provider.base_url().to_owned(),
                connected: false,
                account: None,
                expires_at: None,
                expired: false,
                origin: None,
                project: None,
                key_ref: format!("token:{}", provider.id()),
                in_registry: registry.iter().any(|entry| {
                    entry.key_ref.trim()
                        == format!("{}{}", core_domain::TOKEN_REF_PREFIX, provider.id())
                }),
                pending_url: callbacks
                    .pending
                    .pending_for(provider, now)
                    .map(|(url, _expires)| url),
                outcome: None,
                outcome_ok: false,
                listener_error: callbacks.listen_error(),
            };
            if let Some(error) = &listing {
                row.outcome = Some(format!("the token store could not be read: {error}"));
                return row;
            }
            match callbacks.store.load(provider.id()) {
                Ok(Some(record)) => {
                    row.connected = true;
                    row.account = record.account.clone();
                    row.origin = Some(record.origin.as_str().to_owned());
                    row.project = record.project_id.clone();
                    row.expired = record.expires_at.is_some_and(|at| at <= now);
                    row.expires_at = record.expires_at.map(|at| clock(at, offset_minutes));
                }
                Ok(None) => {}
                Err(error) => {
                    row.outcome = Some(format!("stored grant unreadable: {error}"));
                }
            }
            if let Some(outcome) = callbacks.last_for(provider) {
                row.outcome = Some(outcome.message.clone());
                row.outcome_ok = outcome.ok;
            }
            row
        })
        .collect()
}

/// Render a unix stamp as the operator's clock, `YYYY-MM-DD HH:MM`.
fn clock(unix: i64, offset_minutes: i64) -> String {
    let Ok(stamp) = time::OffsetDateTime::from_unix_timestamp(unix) else {
        return "an unrepresentable time".to_owned();
    };
    let shifted = stamp + time::Duration::minutes(offset_minutes);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        shifted.year(),
        u8::from(shifted.month()),
        shifted.day(),
        shifted.hour(),
        shifted.minute()
    )
}

/// What a connect press produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    /// The login exists and the operator has a URL to open.
    Ready { url: String, port: Option<u16> },
    /// The login could not be started, with the reason and no secret in it.
    Refused(String),
}

/// Start a login for one provider and hand back the URL a browser should open.
///
/// The callback listener is started here rather than at boot: a host that never
/// connects an account should not hold a port open, and a taken port is a failure
/// the operator should meet at the press rather than in a log.
pub async fn start(callbacks: &std::sync::Arc<Callbacks>, provider: Provider) -> Started {
    // The listener is the shortcut for a browser on this host, not a requirement:
    // the pasted address finishes a login from any device. A taken port is kept
    // as a note beside the row (`Callbacks::listen_error`) and does not stop it.
    let port = callbacks.ensure_listening().await.ok();
    let pkce = match Pkce::generate() {
        Ok(pkce) => pkce,
        Err(error) => return Started::Refused(error),
    };
    let state = match crate::oauth::pkce::state() {
        Ok(state) => state,
        Err(error) => return Started::Refused(error),
    };
    let url = crate::oauth::authorize_url(provider, &callbacks.clients, &pkce, &state);
    let now = now_secs();
    callbacks.pending.start(
        Pending {
            provider,
            pkce,
            state,
            authorize_url: url.clone(),
            expires_at: now + crate::oauth::flow::FLOW_TTL_SECS,
        },
        now,
    );
    callbacks.clear();
    Started::Ready { url, port }
}

/// What a disconnect press produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Deleted {
    /// The record is gone. A record that was never there is this outcome too:
    /// the operator asked for it gone, and it is gone.
    Removed,
    /// The store refused, with the reason and no secret in it.
    Refused(String),
}

/// Delete one sealed grant. Local only, by design.
pub fn disconnect(store: &dyn TokenStore, provider: Provider) -> Deleted {
    match store.delete(provider.id()) {
        Ok(()) => Deleted::Removed,
        Err(error) => Deleted::Refused(error.to_string()),
    }
}

/// Whether the registry already names this account's `token:` reference, which
/// is what decides whether the row offers Add to providers or says it is wired.
///
/// A registry that will not parse is reported as not wired: the operator needs
/// the button more, not less, and pressing it is how they find out why.
#[must_use]
pub fn in_registry(engine: &crate::onboarding::SetupEngine, provider: Provider) -> bool {
    let Ok(registry) = crate::web::chat::provider_registry(engine) else {
        return false;
    };
    let reference = format!("{}{}", core_domain::TOKEN_REF_PREFIX, provider.id());
    registry
        .iter()
        .any(|entry| entry.key_ref.trim() == reference)
}

/// Put one connected account into the provider registry.
///
/// A login is not a provider until it is in this list: the pickers, the auxiliary
/// roles and the reply-bubble dropdown all read catalogues keyed by registry id,
/// so an account that is connected and not registered serves nothing. This is the
/// one function that adds it, called on every successful verify and by the Add to
/// providers press.
///
/// # Errors
///
/// A sentence naming what the registry refused or could not be written.
pub fn register(
    engine: &crate::onboarding::SetupEngine,
    provider: Provider,
    project: Option<String>,
) -> Result<crate::providers::Upserted, String> {
    let mut registry =
        crate::web::chat::provider_registry(engine).map_err(|error| error.to_string())?;
    let entry = crate::providers::oauth_entry(
        provider.id(),
        provider.label(),
        provider.base_url(),
        provider.dialect(),
        project,
    );
    let outcome = crate::providers::upsert(&mut registry, entry);
    if outcome == crate::providers::Upserted::Unchanged {
        return Ok(outcome);
    }
    crate::providers::validate(&registry).map_err(|error| error.message)?;
    crate::web::store::settings_put(
        engine.db(),
        crate::providers::SETTING_REGISTRY,
        &crate::providers::to_json(&registry),
    )
    .map_err(|error| error.to_string())?;
    Ok(outcome)
}

/// The cloud project a stored grant discovered, when it discovered one.
#[must_use]
pub fn project_of(callbacks: &Callbacks, provider: Provider) -> Option<String> {
    callbacks
        .store
        .load(provider.id())
        .ok()
        .flatten()
        .and_then(|record| record.project_id)
}

/// The provider a press named, or the refusal a page shows beside the row.
///
/// Read from the form rather than the path so the panel's two forms can share one
/// route, and so a hand-written post cannot name an account this build does not
/// have.
pub fn provider_from(raw: &str) -> Result<Provider, String> {
    Provider::parse(raw).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::store::TokenFileStore;
    use core_domain::{TokenOrigin, TokenRecord};

    fn callbacks(dir: &tempfile::TempDir) -> std::sync::Arc<Callbacks> {
        let store = std::sync::Arc::new(TokenFileStore::new(
            dir.path().join("tokens"),
            dir.path().join("keys/token.key"),
        ));
        std::sync::Arc::new(Callbacks::new(
            std::sync::Arc::new(crate::oauth::PendingFlows::new()),
            std::sync::Arc::new(crate::oauth::Clients::default()),
            store,
        ))
    }

    fn record(provider: &str) -> TokenRecord {
        TokenRecord {
            provider: provider.to_owned(),
            access_token: "STORED-ACCESS-VALUE".to_owned(),
            refresh_token: Some("STORED-REFRESH-VALUE".to_owned()),
            expires_at: Some(now_secs() + 3_600),
            scopes: Vec::new(),
            project_id: Some("aicode-test".to_owned()),
            token_type: Some("Bearer".to_owned()),
            last_refresh: None,
            account: Some("curtis@example.test".to_owned()),
            origin: TokenOrigin::Login,
        }
    }

    #[test]
    fn an_empty_store_states_every_account_as_not_connected() {
        let dir = tempfile::tempdir().expect("temp dir");
        let rows = rows(&callbacks(&dir), 0, &[]);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|row| !row.connected));
        assert_eq!(rows[0].key_ref, "token:claude");
        assert_eq!(rows[1].key_ref, "token:antigravity");
        assert!(
            rows.iter().all(|row| !row.to_string_has_a_token()),
            "the panel must never carry a token"
        );
    }

    #[test]
    fn a_stored_grant_is_reported_with_its_account_and_never_its_token() {
        let dir = tempfile::tempdir().expect("temp dir");
        let callbacks = callbacks(&dir);
        callbacks
            .store
            .store_token(&record("claude"))
            .expect("stores");
        let rows = rows(&callbacks, 0, &[]);
        let claude = &rows[0];
        assert!(claude.connected);
        assert_eq!(claude.account.as_deref(), Some("curtis@example.test"));
        assert_eq!(claude.origin.as_deref(), Some("login"));
        assert!(!claude.expired);
        assert!(!claude.to_string_has_a_token());
    }

    #[test]
    fn a_disconnect_removes_the_record_and_a_second_one_is_still_removed() {
        let dir = tempfile::tempdir().expect("temp dir");
        let callbacks = callbacks(&dir);
        callbacks
            .store
            .store_token(&record("antigravity"))
            .expect("stores");
        assert_eq!(
            disconnect(callbacks.store.as_ref(), Provider::Antigravity),
            Deleted::Removed
        );
        assert!(
            callbacks
                .store
                .load("antigravity")
                .expect("readable")
                .is_none()
        );
        assert_eq!(
            disconnect(callbacks.store.as_ref(), Provider::Antigravity),
            Deleted::Removed,
            "asking for it gone twice is gone twice"
        );
    }

    #[test]
    fn a_press_for_an_account_this_build_does_not_have_is_refused_by_name() {
        let refusal = provider_from("openai").expect_err("must refuse");
        assert!(refusal.contains("claude"), "{refusal}");
        assert_eq!(provider_from("claude").expect("ok"), Provider::Claude);
    }

    /// A row renders as text in the page; this is the assertion that the text has
    /// no secret in it. Written as an extension so every test above can use it.
    trait NoToken {
        fn to_string_has_a_token(&self) -> bool;
    }

    impl NoToken for AccountView {
        fn to_string_has_a_token(&self) -> bool {
            let rendered = format!("{self:?}");
            rendered.contains("STORED-ACCESS-VALUE") || rendered.contains("STORED-REFRESH-VALUE")
        }
    }
}
