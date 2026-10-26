//! PKCE: the proof key both providers' browser logins are built on.
//!
//! A public client cannot keep a secret, so the grant is bound to a value only
//! this process knows: a fresh random `verifier`, sent to the token endpoint with
//! the exchange, and an `S256` challenge of it sent in the authorize URL. An
//! attacker who sees the redirect cannot exchange the code without the verifier.
//!
//! Two rules, both structural:
//!
//! - the verifier is generated from the OS random source and never reused. One
//!   login, one verifier; the pending flow owns it and drops it when it ends.
//! - the verifier never leaves this process. It is not stored, not logged, and
//!   its `Debug` is redacted, so a panic message cannot leak it.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// Bytes of entropy in one verifier, before encoding.
///
/// RFC 7636 requires 32; more costs nothing and the encoded length still lands
/// inside the 43..128 character range the spec allows.
const VERIFIER_BYTES: usize = 32;

/// One login's proof key.
#[derive(Clone, PartialEq, Eq)]
pub struct Pkce {
    verifier: String,
}

impl std::fmt::Debug for Pkce {
    /// Redacted, so a `{:?}` on a containing struct cannot print the verifier.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("verifier", &"<redacted>")
            .finish()
    }
}

impl Pkce {
    /// A fresh verifier from the OS random source.
    ///
    /// # Errors
    ///
    /// The OS message when the random source cannot be read. That is a fault of
    /// the host rather than of the caller, and it is reported rather than
    /// papered over with a weaker source: a guessable verifier is worse than no
    /// login.
    pub fn generate() -> Result<Self, String> {
        let mut bytes = [0u8; VERIFIER_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| format!("no OS randomness: {error}"))?;
        Ok(Self {
            verifier: URL_SAFE_NO_PAD.encode(bytes),
        })
    }

    /// The verifier, for the one place that has to put it in a token request.
    #[must_use]
    pub fn verifier(&self) -> &str {
        &self.verifier
    }

    /// The `S256` challenge: base64url, unpadded, of the SHA-256 of the verifier.
    #[must_use]
    pub fn challenge(&self) -> String {
        let digest = Sha256::digest(self.verifier.as_bytes());
        URL_SAFE_NO_PAD.encode(digest)
    }

    /// The method name the authorize URL carries beside the challenge.
    #[must_use]
    pub const fn method(&self) -> &'static str {
        "S256"
    }
}

/// A fresh `state` value: the anti-forgery token echoed back in the redirect.
///
/// # Errors
///
/// The OS message when the random source cannot be read.
pub fn state() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| format!("no OS randomness: {error}"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_challenge_is_the_unpadded_base64url_of_the_sha256_of_the_verifier() {
        // The worked example from RFC 7636 appendix B, so the encoding is pinned
        // to the spec rather than to whatever this implementation happens to do.
        let pkce = Pkce {
            verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".to_owned(),
        };
        assert_eq!(
            pkce.challenge(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(pkce.method(), "S256");
    }

    #[test]
    fn a_generated_verifier_is_long_enough_and_url_safe() {
        let pkce = Pkce::generate().expect("randomness");
        let verifier = pkce.verifier();
        assert!(
            (43..=128).contains(&verifier.len()),
            "RFC 7636 bounds: {} chars",
            verifier.len()
        );
        assert!(
            verifier
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "url-safe alphabet only: {verifier}"
        );
        // No padding, which is what both providers' examples show.
        assert!(!verifier.contains('='));
    }

    #[test]
    fn two_logins_never_share_a_verifier() {
        let first = Pkce::generate().expect("randomness");
        let second = Pkce::generate().expect("randomness");
        assert_ne!(first.verifier(), second.verifier());
        assert_ne!(first.challenge(), second.challenge());
        assert_ne!(state().expect("randomness"), state().expect("randomness"));
    }

    #[test]
    fn a_verifier_cannot_be_printed_by_accident() {
        let pkce = Pkce::generate().expect("randomness");
        let printed = format!("{pkce:?}");
        assert!(printed.contains("<redacted>"), "{printed}");
        assert!(!printed.contains(pkce.verifier()), "{printed}");
    }
}
