//! OAuth: the encrypted token store, the refresh gate, and the account copy.
//!
//! Build 108 makes Kallisti talk to Anthropic and Google directly instead of
//! through a gateway. That needs two things this module owns:
//!
//! | Item | What it is |
//! |---|---|
//! | [`store::TokenFileStore`] | the encrypted record store, one file per provider |
//! | [`resolve::resolve_credential`] | one choke point for `env:NAME` and `token:NAME` |
//! | [`refresh`] | the refresh decision and the one place a refresh may happen |
//! | [`migrate`] | a read-only copy of an account out of another program's database |
//!
//! The rule the whole module is built around: **no token is refreshed in build
//! 108.** `oauth.auto_refresh` defaults to false in the parser, and while it is
//! false nothing leaves the process. A grant copied out of another program is
//! [`core_domain::TokenOrigin::Migrated`] and is never refreshed even when the
//! flag is on, because spending its refresh token would break the program that
//! still owns it.
//!
//! A token is never rendered. [`core_domain::TokenRecord`] has a redacted `Debug`
//! and no `Serialize`, errors name the provider and the reason, and the one place
//! plaintext exists is the store's encrypt step.

pub mod migrate;
pub mod refresh;
pub mod resolve;
pub mod store;

pub use migrate::{MigrationReport, migrate, read_source_rows};
pub use refresh::{
    HttpRequest, HttpTransport, NoTransport, RefreshEncoding, RefreshSpec, RefreshedGrant,
    TransportError, parse_refresh_body, refresh_decision, refresh_now, refresh_request,
};
pub use resolve::{CredentialSource, RefreshPolicy, resolve_credential, token_error_detail};
pub use store::TokenFileStore;

/// The store the daemon uses: the encrypted files under `~/.config/kallisti`.
///
/// # Errors
///
/// [`core_domain::TokenError::Store`] when `$HOME` is unset.
pub fn default_store(
    paths: &crate::config::Paths,
) -> Result<std::sync::Arc<dyn core_domain::TokenStore + Send + Sync>, core_domain::TokenError>
{
    let store = TokenFileStore::new(paths.token_dir.clone(), paths.token_key_file.clone());
    Ok(std::sync::Arc::new(store))
}

#[cfg(test)]
mod tests {
    use core_domain::TokenStore;

    use super::*;
    use crate::config::Paths;

    #[test]
    fn the_default_store_is_the_configured_directory_and_key() {
        let paths = Paths::for_root("/tmp/example-root");
        let store = default_store(&paths).expect("builds");
        let file = TokenFileStore::new(paths.token_dir.clone(), paths.token_key_file.clone());
        assert_eq!(store.list().expect("lists"), file.list().expect("lists"));
        assert!(paths.token_dir.ends_with("config/tokens"));
        assert!(paths.token_key_file.ends_with("config/keys/token.key"));
    }

    #[test]
    fn a_scratch_root_gets_its_own_store_and_never_the_operators() {
        let left = Paths::for_root("/tmp/left");
        let right = Paths::for_root("/tmp/right");
        assert_ne!(left.token_dir, right.token_dir);
        assert_ne!(left.token_key_file, right.token_key_file);
    }
}
