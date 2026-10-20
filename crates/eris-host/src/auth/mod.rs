//! Auth core: pairing codes, opaque tokens, scopes, and verifier-only storage.
//!
//! Everything here is design and storage. P6 registers no route and no handler;
//! P7 and P8 call these types. The rules are enforced in one place so a later
//! phase cannot soften them by accident:
//!
//! - A pairing code is 8 characters from an alphabet with no `0`, `O`, `1`, `I`
//!   or `l`, lives five minutes, and is single use. Only its Argon2id PHC
//!   verifier reaches SQLite.
//! - An access token is opaque: 32 bytes from the OS CSPRNG, handed to the
//!   caller as lowercase hex. The server keeps an Argon2id verifier and the
//!   device id, nothing else. There is no JWT, so there is nothing to decode.
//! - A refresh token has the same shape with a longer life, and rotation retires
//!   the presented row before the new pair is minted.
//! - A scope set is a type, not a free-form string. `admin` is never granted by
//!   default: it is minted either from a [`LoopbackMint`] witness, which the
//!   onboarding flow and `eris setup` hold after they checked the peer
//!   themselves, or from a pairing code whose operator ticked the administration
//!   box at mint time. The second one arrives as
//!   [`token::TokenOrigin::PairedAdmin`], so the two are never confused.
//! - Revocation is a database fact that is re-read on every verification. There
//!   is no cache, so a revoke is immediate and a rotated token is dead at once.
//!
//! Nothing in this module writes a secret to a log line. Audit rows carry ids,
//! never values.

use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use argon2::password_hash::phc::PasswordHash;
use argon2::{Argon2, password_hash::PasswordHasher, password_hash::PasswordVerifier};

pub mod pairing;
pub mod scope;
pub mod store;
pub mod token;

pub use pairing::{IssuedCode, PairingPolicy, PairingService};
pub use scope::{Scope, ScopeSet};
pub use store::AuthStore;
pub use token::{
    MintedPair, MintedToken, TokenKind, TokenOrigin, TokenPolicy, TokenService, VerifiedToken,
};

/// The witness that a mint request came from an onboarding step on the loopback
/// listener in this process.
///
/// The name of the constructor is the audit hook: P7 and `eris setup` call
/// [`LoopbackMint::assume_loopback_peer`] only after they have checked the
/// connection peer, so `grep -rn assume_loopback_peer` lists every place that can
/// mint `admin` this way. A request on the device listener never gets one; the
/// other way `admin` is minted is a pairing code the operator ticked the
/// administration box for, which is `token::TokenOrigin::PairedAdmin`.
#[derive(Clone, Copy, Debug)]
pub struct LoopbackMint(());

impl LoopbackMint {
    /// Claim that the caller has already proven the peer is loopback.
    #[must_use]
    pub const fn assume_loopback_peer() -> Self {
        Self(())
    }

    /// Add `admin` to a scope set.
    #[must_use]
    pub fn grant_admin(self, base: ScopeSet) -> ScopeSet {
        base.with(Scope::Admin)
    }
}

/// Everything the auth core can refuse.
///
/// Messages never carry a secret, a verifier, or a presented value. The `code`
/// strings are the stable identifiers a later HTTP layer maps onto one `401`
/// body, so a client cannot use the variant to probe which guess was close.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// SQLite failed.
    #[error("database error: {0}")]
    Db(#[from] crate::db::DbError),
    /// Argon2 refused to hash.
    #[error("argon2 could not hash the value")]
    Hash,
    /// The presented token is not 32 bytes of hex.
    #[error("the token is not 32 bytes of hex")]
    TokenMalformed,
    /// No stored token matched.
    #[error("no token matches")]
    TokenRejected,
    /// The matching token is past its expiry.
    #[error("the token has expired")]
    TokenExpired,
    /// The matching token was revoked or rotated.
    #[error("the token was revoked")]
    TokenRevoked,
    /// The matching token is of the other kind.
    #[error("expected a {} token", expected.as_str())]
    TokenWrongKind {
        /// What the caller asked for.
        expected: TokenKind,
    },
    /// The presented pairing code is not 8 characters of the alphabet.
    #[error("the pairing code is not 8 characters from the pairing alphabet")]
    PairingMalformed,
    /// No live pairing code matched.
    #[error("no pairing code matches")]
    PairingRejected,
    /// The matching pairing code is past its expiry.
    #[error("the pairing code has expired")]
    PairingExpired,
    /// `admin` was requested by an origin that may not mint it: neither a
    /// loopback mint nor a pairing code the operator asked administration for.
    #[error("admin scope needs a loopback mint or a pairing code minted for it")]
    AdminScopeRequiresLoopback,
    /// The scope set does not carry the scope the operation needs.
    #[error("scope {required} is not granted")]
    ScopeDenied {
        /// The scope the caller needed.
        required: Scope,
        /// What the token actually carries.
        granted: ScopeSet,
    },
    /// A token was about to be minted with no scopes at all.
    #[error("a token must carry at least one scope")]
    EmptyScope,
    /// The admin passphrase is shorter than the floor.
    #[error("the admin passphrase must be at least {minimum} characters")]
    WeakAdminPassword {
        /// The enforced floor.
        minimum: usize,
    },
    /// No admin credential has been stored yet.
    #[error("no admin credential is stored")]
    NoAdminCredential,
    /// The admin passphrase did not verify.
    #[error("the admin passphrase was rejected")]
    AdminPasswordRejected,
    /// The device id is not in `devices`.
    #[error("unknown device")]
    UnknownDevice,
    /// The device id is in `devices` and has a revocation timestamp.
    #[error("the device is revoked")]
    DeviceRevoked,
    /// A device needs a name that is not blank.
    #[error("a device name must not be blank")]
    EmptyDeviceName,
    /// A stored row does not hold what this module wrote.
    #[error("a stored auth row is corrupt: {what}")]
    CorruptRow {
        /// Which row, as a stable label and never a value.
        what: &'static str,
    },
    /// The OS random source failed.
    #[error("the OS random source failed")]
    Csprng,
    /// A stored timestamp is not RFC 3339.
    #[error("a stored timestamp could not be parsed")]
    Timestamp(#[from] time::error::Parse),
}

impl AuthError {
    /// The stable machine-readable code for this failure.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Db(_) => "db_error",
            Self::Hash => "hash_failed",
            Self::TokenMalformed => "token_malformed",
            Self::TokenRejected => "token_invalid",
            Self::TokenExpired => "token_expired",
            Self::TokenRevoked => "token_revoked",
            Self::TokenWrongKind { .. } => "token_wrong_kind",
            Self::PairingMalformed => "pairing_malformed",
            Self::PairingRejected => "pairing_invalid",
            Self::PairingExpired => "pairing_expired",
            Self::AdminScopeRequiresLoopback => "admin_scope_requires_loopback",
            Self::ScopeDenied { .. } => "scope_denied",
            Self::EmptyScope => "empty_scope",
            Self::WeakAdminPassword { .. } => "weak_admin_password",
            Self::NoAdminCredential => "no_admin_credential",
            Self::AdminPasswordRejected => "admin_password_rejected",
            Self::UnknownDevice => "unknown_device",
            Self::DeviceRevoked => "device_revoked",
            Self::EmptyDeviceName => "empty_device_name",
            Self::CorruptRow { .. } => "corrupt_row",
            Self::Csprng => "csprng_unavailable",
            Self::Timestamp(_) => "corrupt_timestamp",
        }
    }
}

/// Now, as UTC. One helper so a later phase cannot mix clocks.
#[must_use]
pub fn now() -> OffsetDateTime {
    OffsetDateTime::now_utc()
}

/// Format a timestamp the way every auth row stores it.
#[must_use]
pub fn format_timestamp(at: OffsetDateTime) -> String {
    at.format(&Rfc3339)
        .expect("RFC 3339 formatting cannot fail for a valid timestamp")
}

/// Parse a stored timestamp.
pub fn parse_timestamp(text: &str) -> Result<OffsetDateTime, AuthError> {
    OffsetDateTime::parse(text, &Rfc3339).map_err(AuthError::Timestamp)
}

/// `bytes` from the OS CSPRNG. The only random source in this crate.
pub(crate) fn random_bytes<const N: usize>() -> Result<[u8; N], AuthError> {
    let mut buffer = [0u8; N];
    getrandom::fill(&mut buffer).map_err(|_| AuthError::Csprng)?;
    Ok(buffer)
}

/// Argon2id with the crate defaults: memory 19456 KiB, 3 passes over 2
/// iterations, one lane. That is the documented default, not a weakened one,
/// and it is fast enough for a loopback daemon (tens of milliseconds).
pub(crate) fn hash_secret(secret: &[u8]) -> Result<String, AuthError> {
    Argon2::default()
        .hash_password(secret)
        .map(|hash| hash.to_string())
        .map_err(|_| AuthError::Hash)
}

/// `true` when the stored verifier matches, `false` when it does not. A row that
/// does not parse is a corruption, not a wrong guess, so it is an error.
pub(crate) fn verify_secret(secret: &[u8], stored: &str) -> Result<bool, AuthError> {
    let parsed = PasswordHash::new(stored).map_err(|_| AuthError::CorruptRow {
        what: "a stored verifier",
    })?;
    Ok(Argon2::default().verify_password(secret, &parsed).is_ok())
}
