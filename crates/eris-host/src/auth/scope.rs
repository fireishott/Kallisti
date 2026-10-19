//! Scopes: what a token is allowed to do, as a type rather than a string.
//!
//! There are four, and the set is closed. `admin` is the one that matters: it is
//! never granted by a pairing code, and [`crate::auth::LoopbackMint`] is the only
//! way to add it. A scope string that comes from anywhere else is a parse error,
//! not an unknown scope that is silently ignored.

use std::fmt;

use crate::auth::AuthError;

/// One permission. The wire and storage spelling is [`Scope::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum Scope {
    /// Use the provider.
    Chat,
    /// Read and write conversation state.
    Sessions,
    /// Read and write workspace files.
    Files,
    /// Onboarding and administration. Loopback only.
    Admin,
}

impl Scope {
    /// Every scope, in the canonical order storage uses.
    pub const ALL: [Self; 4] = [Self::Chat, Self::Sessions, Self::Files, Self::Admin];

    /// What a freshly paired device gets. Deliberately without `admin`.
    pub const DEVICE_DEFAULT: [Self; 3] = [Self::Chat, Self::Sessions, Self::Files];

    /// The storage and display spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Sessions => "sessions",
            Self::Files => "files",
            Self::Admin => "admin",
        }
    }

    /// Parse the canonical spelling. Anything else is `None`, never a default.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == text)
    }

    const fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A set of scopes, stored as one canonical space-separated string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScopeSet(u8);

impl ScopeSet {
    /// No scopes. A token with this set is refused at mint time.
    pub const EMPTY: Self = Self(0);

    /// `chat`, `sessions` and `files`: what a paired device is for.
    #[must_use]
    pub const fn device_default() -> Self {
        Self(Scope::Chat.bit() | Scope::Sessions.bit() | Scope::Files.bit())
    }

    /// This set plus one scope.
    #[must_use]
    pub const fn with(self, scope: Scope) -> Self {
        Self(self.0 | scope.bit())
    }

    /// This set minus one scope.
    #[must_use]
    pub const fn without(self, scope: Scope) -> Self {
        Self(self.0 & !scope.bit())
    }

    /// Whether the set carries the scope.
    #[must_use]
    pub const fn contains(self, scope: Scope) -> bool {
        self.0 & scope.bit() != 0
    }

    /// Whether the set is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The union of two sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Whether `admin` is in the set. The question every guard asks first.
    #[must_use]
    pub const fn has_admin(self) -> bool {
        self.contains(Scope::Admin)
    }

    /// The scopes present, in canonical order.
    pub fn iter(self) -> impl Iterator<Item = Scope> {
        Scope::ALL
            .into_iter()
            .filter(move |scope| self.contains(*scope))
    }

    /// Refuse the operation when the scope is missing.
    pub fn require(self, scope: Scope) -> Result<(), AuthError> {
        if self.contains(scope) {
            Ok(())
        } else {
            Err(AuthError::ScopeDenied {
                required: scope,
                granted: self,
            })
        }
    }

    /// The canonical storage spelling: one scope per word, `ALL` order.
    #[must_use]
    pub fn to_storage(self) -> String {
        self.iter().map(Scope::as_str).collect::<Vec<_>>().join(" ")
    }

    /// Read the canonical storage spelling. Unknown words and an empty set are
    /// corrupt rows, because this module never writes them.
    pub fn from_storage(text: &str) -> Result<Self, AuthError> {
        let mut set = Self::EMPTY;
        let mut found = false;
        for word in text.split_whitespace() {
            let scope = Scope::parse(word).ok_or(AuthError::CorruptRow {
                what: "auth_tokens.scope",
            })?;
            set = set.with(scope);
            found = true;
        }
        if !found {
            return Err(AuthError::CorruptRow {
                what: "auth_tokens.scope",
            });
        }
        Ok(set)
    }
}

impl fmt::Display for ScopeSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_storage())
    }
}

// Tests live at module level (not in a `mod tests`) so the reported name is
// exactly `auth::scope::<name>`.

#[cfg(test)]
#[test]
fn admin_scope_required() {
    use crate::auth::token::{TokenOrigin, TokenService};
    use crate::auth::{LoopbackMint, Scope, ScopeSet};
    use crate::db::Db;

    let device_scopes = ScopeSet::device_default();
    assert!(!device_scopes.contains(Scope::Admin));
    let denied = device_scopes
        .require(Scope::Admin)
        .expect_err("a device scope set must not grant admin");
    assert_eq!(denied.code(), "scope_denied");

    let service = TokenService::new(Db::open_in_memory().expect("in-memory database"));
    let device = service.register_device("phone", "ios").expect("device row");
    let refused = service
        .mint_access(
            &device,
            device_scopes.with(Scope::Admin),
            TokenOrigin::Device,
        )
        .expect_err("the device origin must not mint admin");
    assert_eq!(refused.code(), "admin_scope_requires_loopback");

    let admin_scopes = LoopbackMint::assume_loopback_peer().grant_admin(device_scopes);
    let minted = service
        .mint_access(&device, admin_scopes, TokenOrigin::Loopback)
        .expect("loopback mint");
    let verified = service
        .verify_access(minted.token())
        .expect("the admin token verifies");
    verified
        .scope()
        .require(Scope::Admin)
        .expect("admin is present");
}
