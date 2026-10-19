//! Pairing codes: 8 characters, five minutes, single use, verifier only.
//!
//! A code is short because a human retypes it from one screen to another, so the
//! alphabet drops every glyph that is misread when handwritten or copied:
//! there is no `0`, `O`, `1`, `I` or `l`. Lowercase is not in the alphabet at
//! all, and input is uppercased before comparison, so case never decides a match.
//!
//! Generation is rejection-sampled from the OS CSPRNG: 31 does not divide 256, so
//! a plain modulo would bias the first eight characters. Bytes at or above
//! `256 - (256 % 31)` are discarded and the rest are reduced. That is the whole
//! reason the loop exists.
//!
//! Only the Argon2id PHC string is stored. A code is handed to the caller once,
//! on the screen it was printed to, and is unrecoverable from the database by
//! design. `Debug` is redacted.

use std::fmt;
use std::time::Duration;

use crate::auth::store::{AuthStore, StoredPairingCode};
use crate::auth::{
    AuthError, format_timestamp, hash_secret, now, parse_timestamp, random_bytes, verify_secret,
};
use crate::db::Db;
use time::OffsetDateTime;

/// Characters in a code.
pub const CODE_LENGTH: usize = 8;

/// The unambiguous alphabet: A-Z without `I`, `L`, `O`, plus 2-9. 31 symbols,
/// no lowercase, no glyph that reads as another.
pub const ALPHABET: &str = "ABCDEFGHJKMNPQRSTUVWXYZ23456789";

/// Five minutes.
pub const CODE_TTL: Duration = Duration::from_secs(5 * 60);

/// How long codes live. Zero makes an instantly expired code, for tests.
#[derive(Debug, Clone, Copy)]
pub struct PairingPolicy {
    /// Code lifetime.
    pub ttl: Duration,
}

impl Default for PairingPolicy {
    fn default() -> Self {
        Self { ttl: CODE_TTL }
    }
}

/// A code that was just issued. The value is readable exactly once; only the
/// verifier is in the database.
#[derive(Clone)]
pub struct IssuedCode {
    code: String,
    code_id: String,
    expires_at: OffsetDateTime,
}

impl IssuedCode {
    /// The value to show the operator. Lowercase hex? No - eight characters of
    /// [`ALPHABET`], and the only time it exists in memory here.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The row id, safe to log.
    #[must_use]
    pub fn code_id(&self) -> &str {
        &self.code_id
    }

    /// When it stops redeeming.
    #[must_use]
    pub fn expires_at(&self) -> OffsetDateTime {
        self.expires_at
    }
}

impl fmt::Debug for IssuedCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IssuedCode")
            .field("code", &"<redacted>")
            .field("code_id", &self.code_id)
            .field("expires_at", &format_timestamp(self.expires_at))
            .finish()
    }
}

/// A code that redeemed. The device binding is optional because `eris setup` can
/// mint a code before the phone exists.
#[derive(Debug, Clone)]
pub struct RedeemedCode {
    code_id: String,
    device_id: Option<String>,
}

impl RedeemedCode {
    /// The row id of the code that was consumed.
    #[must_use]
    pub fn code_id(&self) -> &str {
        &self.code_id
    }

    /// The device the code was minted for, when it was minted for one.
    #[must_use]
    pub fn device_id(&self) -> Option<&str> {
        self.device_id.as_deref()
    }
}

/// The pairing surface: issue one, redeem one.
pub struct PairingService {
    store: AuthStore,
    policy: PairingPolicy,
}

impl PairingService {
    /// The documented policy: five minutes.
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self::with_policy(db, PairingPolicy::default())
    }

    /// An explicit policy. Tests use a zero TTL.
    #[must_use]
    pub fn with_policy(db: Db, policy: PairingPolicy) -> Self {
        Self {
            store: AuthStore::new(db),
            policy,
        }
    }

    /// Issue one unbound code: `eris setup` mints it before a device exists.
    pub fn issue(&self) -> Result<IssuedCode, AuthError> {
        self.issue_bound(None)
    }

    /// Issue one code bound to a known device.
    pub fn issue_for_device(&self, device_id: &str) -> Result<IssuedCode, AuthError> {
        if self.store.device(device_id)?.is_none() {
            return Err(AuthError::UnknownDevice);
        }
        self.issue_bound(Some(device_id))
    }

    /// Redeem a code. Single use: the consume update is conditional on
    /// `consumed_at IS NULL`, so two racing redeems cannot both win.
    pub fn redeem(&self, presented: &str) -> Result<RedeemedCode, AuthError> {
        let normalized = normalize(presented)?;
        let mut matched: Option<StoredPairingCode> = None;
        for row in self.store.pairing_rows()? {
            if row.consumed_at.is_some() {
                continue;
            }
            if verify_secret(normalized.as_bytes(), &row.code_hash)? {
                matched = Some(row);
                break;
            }
        }
        let row = matched.ok_or(AuthError::PairingRejected)?;
        if now() >= parse_timestamp(&row.expires_at)? {
            return Err(AuthError::PairingExpired);
        }
        if !self.store.consume_pairing_code(&row.id, &now())? {
            return Err(AuthError::PairingRejected);
        }
        self.store
            .audit("loopback", "auth.pairing_redeemed", Some(&row.id))?;
        Ok(RedeemedCode {
            code_id: row.id,
            device_id: row.device_id,
        })
    }

    fn issue_bound(&self, device_id: Option<&str>) -> Result<IssuedCode, AuthError> {
        let code = generate_code()?;
        let code_hash = hash_secret(code.as_bytes())?;
        let expires_at = now() + self.policy.ttl;
        let code_id = self
            .store
            .insert_pairing_code(&code_hash, device_id, expires_at)?;
        self.store
            .audit("loopback", "auth.pairing_issued", Some(&code_id))?;
        Ok(IssuedCode {
            code,
            code_id,
            expires_at,
        })
    }
}

/// Generate one code from the OS CSPRNG with rejection sampling, so every symbol
/// is equally likely.
pub fn generate_code() -> Result<String, AuthError> {
    let symbols = ALPHABET.len();
    let limit = 256 - (256 % symbols);
    let mut code = String::with_capacity(CODE_LENGTH);
    while code.len() < CODE_LENGTH {
        let chunk = random_bytes::<16>()?;
        for byte in chunk {
            let value = usize::from(byte);
            if value < limit && code.len() < CODE_LENGTH {
                let index = value % symbols;
                // The alphabet is ASCII, so this cannot split a character.
                code.push(char::from(ALPHABET.as_bytes()[index]));
            }
        }
    }
    Ok(code)
}

/// Uppercase, drop spaces and dashes, then insist on the alphabet. Anything else
/// is a malformed code and never a guess worth hashing.
fn normalize(presented: &str) -> Result<String, AuthError> {
    let cleaned = presented
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '-')
        .map(|character| character.to_ascii_uppercase())
        .collect::<String>();
    if cleaned.len() != CODE_LENGTH {
        return Err(AuthError::PairingMalformed);
    }
    let known = ALPHABET.as_bytes();
    if !cleaned.bytes().all(|byte| known.contains(&byte)) {
        return Err(AuthError::PairingMalformed);
    }
    Ok(cleaned)
}

// Tests live at module level (not in a `mod tests`) so the reported name is
// exactly `auth::pairing::<name>`.

#[cfg(test)]
#[test]
fn code_single_use() {
    use crate::db::Db;

    use super::PairingService;

    let pairing = PairingService::new(Db::open_in_memory().expect("in-memory database"));
    let issued = pairing.issue().expect("issue a code");
    assert_eq!(issued.code().len(), 8);

    let redeemed = pairing
        .redeem(issued.code())
        .expect("the first redeem works");
    assert_eq!(redeemed.code_id(), issued.code_id());

    let refused = pairing
        .redeem(issued.code())
        .expect_err("a pairing code is single use");
    assert_eq!(refused.code(), "pairing_invalid");

    let rendered = format!("{issued:?}");
    assert!(
        !rendered.contains(issued.code()),
        "Debug must redact the code: {rendered}"
    );
}

#[cfg(test)]
#[test]
fn code_expires_after_ttl() {
    use std::time::Duration;

    use crate::db::Db;

    use super::{PairingPolicy, PairingService};

    let pairing = PairingService::with_policy(
        Db::open_in_memory().expect("db"),
        PairingPolicy {
            ttl: Duration::ZERO,
        },
    );
    let issued = pairing.issue().expect("issue a code");

    let refused = pairing
        .redeem(issued.code())
        .expect_err("an expired code must not redeem");
    assert_eq!(refused.code(), "pairing_expired");
}

#[cfg(test)]
#[test]
fn pairing_codes_look_uniform() {
    // 512 codes is 4096 characters: each of the 31 symbols expects about 132,
    // so the bounds below are more than five standard deviations wide and the
    // test does not flake on a correct generator.
    const SAMPLES: usize = 512;
    let mut counts = vec![0usize; ALPHABET.len()];
    for _ in 0..SAMPLES {
        let code = generate_code().expect("the OS random source works");
        assert_eq!(code.len(), CODE_LENGTH);
        for byte in code.bytes() {
            let index = ALPHABET
                .bytes()
                .position(|candidate| candidate == byte)
                .expect("every character is in the alphabet");
            counts[index] += 1;
        }
    }

    let expected = SAMPLES as f64 * CODE_LENGTH as f64 / ALPHABET.len() as f64;
    for (index, count) in counts.iter().enumerate() {
        let character = ALPHABET.as_bytes()[index] as char;
        let ratio = *count as f64 / expected;
        assert!(
            (0.5..1.6).contains(&ratio),
            "{character} appeared {count} times, expected about {expected:.0}; \
             a CSPRNG with correct rejection sampling does not do that"
        );
    }
    for ambiguous in ['0', 'O', '1', 'I', 'l'] {
        assert!(
            !ALPHABET.contains(ambiguous),
            "{ambiguous} is ambiguous in a hand-copied code"
        );
    }
}

#[cfg(test)]
#[test]
fn pairing_malformed_is_rejected() {
    use crate::db::Db;

    use super::PairingService;

    let pairing = PairingService::new(Db::open_in_memory().expect("db"));
    for bad in ["", "ABC", "ABCDEFGHI", "ABCDEFG0", "abcdefgl", "!@#$%^&*"] {
        let refused = pairing.redeem(bad).expect_err("malformed input is refused");
        assert_eq!(refused.code(), "pairing_malformed", "input {bad:?}");
    }
}
