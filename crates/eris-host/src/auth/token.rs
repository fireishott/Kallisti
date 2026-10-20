//! Opaque access and refresh tokens, stored as Argon2id verifiers.
//!
//! A token is 32 bytes from the OS CSPRNG, handed to the caller once as lowercase
//! hex. There is no JWT and no signed envelope, so the value is not decodable by
//! anyone, including this process: the only way to learn what a token is for is
//! to match it against a stored verifier.
//!
//! Storage per token row is the Argon2id PHC string, the device id, the kind, the
//! scope set and three timestamps. The plaintext exists in caller memory from
//! mint until it is written to the client, and nowhere else. `Debug` is
//! hand-written to print `<redacted>` so a stray `dbg!` or a struct in a log line
//! cannot leak a bearer.
//!
//! Verification re-reads the rows every time. That is the point: a revoke or a
//! rotation is visible to the very next check, with no TTL cache to serve a
//! retired token.

use std::fmt;
use std::time::Duration;

use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::store::{AuthStore, NewToken};
use crate::auth::{
    AuthError, ScopeSet, format_timestamp, hash_secret, now, parse_timestamp, random_bytes,
    verify_secret,
};
use crate::db::Db;

/// Bytes of entropy in a token.
pub const TOKEN_BYTES: usize = 32;

/// Characters in a token's hex spelling.
pub const TOKEN_HEX_LEN: usize = TOKEN_BYTES * 2;

/// Access token lifetime: one hour.
pub const ACCESS_TTL: Duration = Duration::from_secs(60 * 60);

/// Refresh token lifetime: thirty days, per device.
pub const REFRESH_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// Which token this is. Stored, so a refresh token cannot be used as an access
/// token even if it leaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// Short-lived, presented on every request.
    Access,
    /// Long-lived, presented only to the rotation endpoint.
    Refresh,
}

impl TokenKind {
    /// The storage spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Access => "access",
            Self::Refresh => "refresh",
        }
    }

    /// Parse the storage spelling.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "access" => Some(Self::Access),
            "refresh" => Some(Self::Refresh),
            _ => None,
        }
    }
}

impl fmt::Display for TokenKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Who asked for the mint. `admin` requires [`TokenOrigin::Loopback`] or
/// [`TokenOrigin::PairedAdmin`], and both are claims a caller can only make at a
/// point where a person decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenOrigin {
    /// A paired device: scopes are the device defaults, never `admin`.
    Device,
    /// A paired device whose pairing code was minted with the administration box
    /// ticked. The operator made that decision once, at mint time, and the code
    /// carried it here; this origin is how the token says so.
    PairedAdmin,
    /// The loopback onboarding surface or `eris setup`.
    Loopback,
}

impl TokenOrigin {
    /// The audit actor for this origin. A paired device is a device whichever
    /// box was ticked, so both device origins log the same actor.
    #[must_use]
    pub const fn actor(self) -> &'static str {
        match self {
            Self::Device | Self::PairedAdmin => "device",
            Self::Loopback => "loopback",
        }
    }

    /// Whether this origin may mint a token that carries `admin`.
    #[must_use]
    pub const fn may_mint_admin(self) -> bool {
        matches!(self, Self::PairedAdmin | Self::Loopback)
    }
}

/// How long each kind lives. The default is the documented policy; tests set a
/// zero TTL to mint an already-expired token instead of sleeping.
#[derive(Debug, Clone, Copy)]
pub struct TokenPolicy {
    /// Access token lifetime.
    pub access_ttl: Duration,
    /// Refresh token lifetime.
    pub refresh_ttl: Duration,
}

impl Default for TokenPolicy {
    fn default() -> Self {
        Self {
            access_ttl: ACCESS_TTL,
            refresh_ttl: REFRESH_TTL,
        }
    }
}

impl TokenPolicy {
    /// A policy with one overridden access lifetime.
    #[must_use]
    pub const fn with_access_ttl(access_ttl: Duration) -> Self {
        Self {
            access_ttl,
            refresh_ttl: REFRESH_TTL,
        }
    }
}

/// A token that was just minted. The value is readable exactly once, by the
/// caller that has to hand it to the client.
#[derive(Clone)]
pub struct MintedToken {
    token: String,
    token_id: String,
    device_id: String,
    kind: TokenKind,
    expires_at: OffsetDateTime,
}

impl MintedToken {
    /// The bearer value, lowercase hex.
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The row id, which is safe to log and to audit.
    #[must_use]
    pub fn token_id(&self) -> &str {
        &self.token_id
    }

    /// The device this token belongs to.
    #[must_use]
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// Which kind it is.
    #[must_use]
    pub fn kind(&self) -> TokenKind {
        self.kind
    }

    /// When it stops verifying.
    #[must_use]
    pub fn expires_at(&self) -> OffsetDateTime {
        self.expires_at
    }
}

impl fmt::Debug for MintedToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MintedToken")
            .field("token", &"<redacted>")
            .field("token_id", &self.token_id)
            .field("device_id", &self.device_id)
            .field("kind", &self.kind)
            .field("expires_at", &format_timestamp(self.expires_at))
            .finish()
    }
}

/// An access and refresh pair minted together. This is what a fresh pair or a
/// rotation hands back.
#[derive(Debug)]
pub struct MintedPair {
    /// The short-lived token.
    pub access: MintedToken,
    /// The long-lived token.
    pub refresh: MintedToken,
}

/// A token that matched a stored verifier and passed every status check.
#[derive(Debug, Clone)]
pub struct VerifiedToken {
    token_id: String,
    device_id: String,
    kind: TokenKind,
    scope: ScopeSet,
    expires_at: OffsetDateTime,
}

impl VerifiedToken {
    /// The row id.
    #[must_use]
    pub fn token_id(&self) -> &str {
        &self.token_id
    }

    /// The device the token belongs to.
    #[must_use]
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// The kind that matched.
    #[must_use]
    pub fn kind(&self) -> TokenKind {
        self.kind
    }

    /// What it is allowed to do.
    #[must_use]
    pub fn scope(&self) -> ScopeSet {
        self.scope
    }

    /// When it expires.
    #[must_use]
    pub fn expires_at(&self) -> OffsetDateTime {
        self.expires_at
    }
}

/// The token surface: mint, verify, rotate, revoke.
pub struct TokenService {
    store: AuthStore,
    policy: TokenPolicy,
}

impl TokenService {
    /// The default policy.
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self::with_policy(db, TokenPolicy::default())
    }

    /// An explicit policy. Tests use this with a zero TTL.
    #[must_use]
    pub fn with_policy(db: Db, policy: TokenPolicy) -> Self {
        Self {
            store: AuthStore::new(db),
            policy,
        }
    }

    /// The store underneath, for a caller that needs the audit trail or the
    /// device rows.
    #[must_use]
    pub fn store(&self) -> &AuthStore {
        &self.store
    }

    /// Insert a device and return its id. A pairing flow calls this once per
    /// device, then mints tokens for it.
    pub fn register_device(&self, name: &str, platform: &str) -> Result<String, AuthError> {
        self.store.register_device(name, platform)
    }

    /// Mint one short-lived access token.
    pub fn mint_access(
        &self,
        device_id: &str,
        scope: ScopeSet,
        origin: TokenOrigin,
    ) -> Result<MintedToken, AuthError> {
        self.mint(device_id, TokenKind::Access, scope, origin)
    }

    /// Mint one long-lived refresh token.
    pub fn mint_refresh(
        &self,
        device_id: &str,
        scope: ScopeSet,
        origin: TokenOrigin,
    ) -> Result<MintedToken, AuthError> {
        self.mint(device_id, TokenKind::Refresh, scope, origin)
    }

    /// Mint an access and refresh pair. A pairing flow calls this once.
    pub fn mint_pair(
        &self,
        device_id: &str,
        scope: ScopeSet,
        origin: TokenOrigin,
    ) -> Result<MintedPair, AuthError> {
        let access = self.mint(device_id, TokenKind::Access, scope, origin)?;
        let refresh = self.mint(device_id, TokenKind::Refresh, scope, origin)?;
        self.store
            .audit(origin.actor(), "auth.tokens_minted", Some(device_id))?;
        Ok(MintedPair { access, refresh })
    }

    /// Verify an access token. Every check reads the row again, so a revoke or a
    /// rotation between two calls is visible to the second call.
    pub fn verify_access(&self, presented: &str) -> Result<VerifiedToken, AuthError> {
        self.verify(presented, TokenKind::Access)
    }

    /// Verify a refresh token.
    pub fn verify_refresh(&self, presented: &str) -> Result<VerifiedToken, AuthError> {
        self.verify(presented, TokenKind::Refresh)
    }

    /// Retire the presented refresh token and mint a new pair for the same
    /// device and the same scope set.
    ///
    /// The old row is rotated first: if the rotate update loses a race, the mint
    /// never happens and the caller gets a revoked token error rather than a
    /// second live pair.
    pub fn rotate(&self, presented_refresh: &str) -> Result<MintedPair, AuthError> {
        let verified = self.verify_refresh(presented_refresh)?;
        if !self.store.mark_token_rotated(&verified.token_id, &now())? {
            return Err(AuthError::TokenRevoked);
        }
        let origin = if verified.scope.has_admin() {
            TokenOrigin::Loopback
        } else {
            TokenOrigin::Device
        };
        let access = self.mint(
            &verified.device_id,
            TokenKind::Access,
            verified.scope,
            origin,
        )?;
        let refresh = self.mint(
            &verified.device_id,
            TokenKind::Refresh,
            verified.scope,
            origin,
        )?;
        self.store
            .audit(origin.actor(), "auth.rotate", Some(&verified.device_id))?;
        Ok(MintedPair { access, refresh })
    }

    /// Revoke every live token of one device and the device itself. Returns how
    /// many tokens were retired. The effect is immediate: the next verification
    /// of any of them fails.
    pub fn revoke_device(&self, device_id: &str) -> Result<usize, AuthError> {
        self.store.revoke_device(device_id)
    }

    /// Revoke one token by row id.
    pub fn revoke_token(&self, token_id: &str) -> Result<(), AuthError> {
        if !self.store.mark_token_revoked(token_id, &now())? {
            return Err(AuthError::TokenRejected);
        }
        self.store
            .audit("loopback", "auth.revoke_token", Some(token_id))
    }

    /// Mint one token and write its verifier.
    fn mint(
        &self,
        device_id: &str,
        kind: TokenKind,
        scope: ScopeSet,
        origin: TokenOrigin,
    ) -> Result<MintedToken, AuthError> {
        if scope.is_empty() {
            return Err(AuthError::EmptyScope);
        }
        if scope.has_admin() && !origin.may_mint_admin() {
            return Err(AuthError::AdminScopeRequiresLoopback);
        }
        match self.store.device(device_id)? {
            None => return Err(AuthError::UnknownDevice),
            Some(device) if device.revoked_at.is_some() => return Err(AuthError::DeviceRevoked),
            Some(_) => {}
        }

        let bytes = random_bytes::<TOKEN_BYTES>()?;
        let token_hash = hash_secret(&bytes)?;
        let ttl = match kind {
            TokenKind::Access => self.policy.access_ttl,
            TokenKind::Refresh => self.policy.refresh_ttl,
        };
        let expires_at = now() + ttl;
        let token_id = Uuid::now_v7().to_string();
        self.store.insert_token(&NewToken {
            id: token_id.clone(),
            device_id: device_id.to_owned(),
            kind,
            token_hash,
            scope,
            expires_at,
        })?;

        Ok(MintedToken {
            token: hex::encode(bytes),
            token_id,
            device_id: device_id.to_owned(),
            kind,
            expires_at,
        })
    }

    /// Match the presented value against every stored verifier and then apply
    /// the status checks, in an order that keeps a retired token retired.
    fn verify(&self, presented: &str, expected: TokenKind) -> Result<VerifiedToken, AuthError> {
        let bytes = decode_token(presented)?;
        for row in self.store.token_rows()? {
            if !verify_secret(&bytes, &row.token_hash)? {
                continue;
            }
            if row.revoked_at.is_some() || row.rotated_at.is_some() {
                return Err(AuthError::TokenRevoked);
            }
            let expires_at = parse_timestamp(&row.expires_at)?;
            if now() >= expires_at {
                return Err(AuthError::TokenExpired);
            }
            if let Some(device) = self.store.device(&row.device_id)?
                && device.revoked_at.is_some()
            {
                return Err(AuthError::DeviceRevoked);
            }
            let kind = TokenKind::parse(&row.kind).ok_or(AuthError::CorruptRow {
                what: "auth_tokens.kind",
            })?;
            if kind != expected {
                return Err(AuthError::TokenWrongKind { expected });
            }
            let scope = ScopeSet::from_storage(&row.scope)?;
            return Ok(VerifiedToken {
                token_id: row.id,
                device_id: row.device_id,
                kind,
                scope,
                expires_at,
            });
        }
        Err(AuthError::TokenRejected)
    }
}

/// The presented value, checked for shape before a single hash is compared.
fn decode_token(presented: &str) -> Result<[u8; TOKEN_BYTES], AuthError> {
    let trimmed = presented.trim();
    if trimmed.len() != TOKEN_HEX_LEN || !trimmed.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(AuthError::TokenMalformed);
    }
    let decoded = hex::decode(trimmed).map_err(|_| AuthError::TokenMalformed)?;
    decoded.try_into().map_err(|_| AuthError::TokenMalformed)
}

// Tests live at module level (not in a `mod tests`) so the reported name is
// exactly `auth::token::<name>`.

#[cfg(test)]
#[test]
fn mint_then_verify_ok() {
    use crate::auth::{Scope, ScopeSet};
    use crate::db::Db;

    use super::{TokenKind, TokenOrigin, TokenService};

    let service = TokenService::new(Db::open_in_memory().expect("in-memory database"));
    let device = service.register_device("phone", "ios").expect("device");
    let minted = service
        .mint_access(&device, ScopeSet::device_default(), TokenOrigin::Device)
        .expect("mint an access token");

    assert_eq!(minted.token().len(), 64, "32 random bytes as lowercase hex");
    assert!(
        minted.token().bytes().all(|byte| byte.is_ascii_hexdigit()),
        "the bearer is hex"
    );
    assert_eq!(minted.kind(), TokenKind::Access);

    let verified = service
        .verify_access(minted.token())
        .expect("a fresh token verifies");
    assert_eq!(verified.device_id(), device);
    assert_eq!(verified.kind(), TokenKind::Access);
    assert_eq!(verified.token_id(), minted.token_id());
    assert!(verified.scope().contains(Scope::Chat));

    let rendered = format!("{minted:?}");
    assert!(
        !rendered.contains(minted.token()),
        "Debug must redact the token value: {rendered}"
    );
}

#[cfg(test)]
#[test]
fn expired_is_rejected() {
    use crate::auth::ScopeSet;
    use crate::db::Db;

    use super::{TokenOrigin, TokenPolicy, TokenService};

    let policy = TokenPolicy {
        access_ttl: Duration::ZERO,
        ..TokenPolicy::default()
    };
    let service = TokenService::with_policy(Db::open_in_memory().expect("db"), policy);
    let device = service.register_device("phone", "ios").expect("device");
    let minted = service
        .mint_access(&device, ScopeSet::device_default(), TokenOrigin::Device)
        .expect("mint");

    let refused = service
        .verify_access(minted.token())
        .expect_err("an expired access token must not verify");
    assert_eq!(refused.code(), "token_expired");
}

#[cfg(test)]
#[test]
fn revoked_is_rejected_immediately() {
    use crate::auth::ScopeSet;
    use crate::db::Db;

    use super::{TokenOrigin, TokenService};

    let service = TokenService::new(Db::open_in_memory().expect("db"));
    let phone = service.register_device("phone", "ios").expect("device");
    let laptop = service.register_device("laptop", "linux").expect("device");
    let phone_pair = service
        .mint_pair(&phone, ScopeSet::device_default(), TokenOrigin::Device)
        .expect("pair");
    let laptop_pair = service
        .mint_pair(&laptop, ScopeSet::device_default(), TokenOrigin::Device)
        .expect("pair");

    assert_eq!(
        service.revoke_device(&phone).expect("revoke"),
        2,
        "both live tokens of that device are revoked"
    );

    // No sleep and no grace period: the very next verification is a reject.
    assert_eq!(
        service
            .verify_access(phone_pair.access.token())
            .expect_err("access token revoked")
            .code(),
        "token_revoked"
    );
    assert_eq!(
        service
            .verify_refresh(phone_pair.refresh.token())
            .expect_err("refresh token revoked")
            .code(),
        "token_revoked"
    );
    assert_eq!(
        service
            .verify_access(phone_pair.access.token())
            .expect_err("still revoked, no cache")
            .code(),
        "token_revoked"
    );

    // Another device is untouched by that revoke.
    service
        .verify_access(laptop_pair.access.token())
        .expect("the other device still works");

    // Rotation retires the presented refresh token immediately.
    let rotated = service.rotate(laptop_pair.refresh.token()).expect("rotate");
    assert_eq!(
        service
            .verify_refresh(laptop_pair.refresh.token())
            .expect_err("the old refresh token is retired")
            .code(),
        "token_revoked"
    );
    service
        .verify_access(rotated.access.token())
        .expect("the new access token works");
    service
        .verify_refresh(rotated.refresh.token())
        .expect("the new refresh token works");

    // A revoked device cannot mint its way out with a fresh token.
    assert_eq!(
        service
            .mint_access(&phone, ScopeSet::device_default(), TokenOrigin::Device)
            .expect_err("a revoked device cannot mint")
            .code(),
        "device_revoked"
    );
}
