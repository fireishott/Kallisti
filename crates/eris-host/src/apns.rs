//! APNs: real Apple Push Notification delivery, HTTP/2 with an ES256 JWT.
//!
//! `push.rs` stores device tokens; this module is the sender. Before it existed
//! the send route answered `503 apns_not_configured` and every phone that
//! registered believed it was reachable. Ten registrations were active and
//! delivering nothing, which is worse than refusing: the app shows "Registered"
//! and the operator waits for a notification nobody ever sent.
//!
//! ## The key is team-scoped, not app-scoped
//!
//! An APNs `.p8` authenticates as a *team*. The app it delivers to is chosen
//! per request by the `apns-topic` header, which is the bundle id. So the relay's
//! existing key `LH5GM8356P` for team `58U7UPFS53` can deliver to
//! `net.fihonline.kallisti`, because that app is signed by the same team. There
//! is no separate key to mint and no reason to shell out to Python.
//!
//! ## Environment comes from the registration, never from a guess
//!
//! APNs has two hosts and a token is valid against exactly one of them. The
//! environment is stored per registration and passed through. A mismatch comes
//! back as `BadDeviceToken`, which this module reports as
//! [`Outcome::TokenInvalid`] so the caller can retire the row rather than retry
//! it forever. That distinction is the reason the outcome is an enum and not a
//! bool.
//!
//! ## Configuration
//!
//! | variable | meaning |
//! |---|---|
//! | `APNS_KEY_PATH` | path to the `.p8` |
//! | `APNS_KEY_CONTENTS` | the key text itself, for container deploys |
//! | `APNS_KEY_ID` | the 10-character key id from Apple |
//! | `APNS_TEAM_ID` | the 10-character team id |
//! | `APNS_BUNDLE_ID` | used only when a registration has none |
//! | `APNS_ENVIRONMENT` | `development` or `production`, same rule |
//!
//! All unset means no sender, and the send route keeps its honest 503. Half-set
//! is an error at start-up rather than a 503 nobody reads, because a typo in one
//! variable must not look like a feature that was never asked for.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::EcdsaKeyPair;
use serde_json::json;

/// Where a development-environment token must be sent.
pub const DEVELOPMENT_HOST: &str = "https://api.development.push.apple.com";
/// Where a production-environment token must be sent.
pub const PRODUCTION_HOST: &str = "https://api.push.apple.com";

/// The bundle this build delivers to when a registration names none.
pub const DEFAULT_BUNDLE: &str = "net.fihonline.kallisti";

/// Apple accepts a provider token for an hour. This refreshes well inside that,
/// so a clock skew or a long turn cannot present an expired one.
const TOKEN_LIFETIME: Duration = Duration::from_secs(45 * 60);

/// A configuration failure. Always loud: a half-configured sender must not
/// degrade silently into the 503 it was meant to replace.
#[derive(Debug, thiserror::Error)]
pub enum ApnsConfigError {
    #[error("{0} is set but {1} is not; APNs needs both")]
    Incomplete(&'static str, &'static str),
    #[error("cannot read the APNs key at {path}: {source}")]
    KeyUnreadable {
        /// The path that could not be read. Never the key itself.
        path: PathBuf,
        /// Why the read failed.
        source: std::io::Error,
    },
    #[error("the APNs key is empty")]
    KeyEmpty,
    #[error("the APNs key was rejected: {0}")]
    KeyRejected(String),
    #[error("{name} must be `development` or `production`, not {value:?}")]
    BadEnvironment {
        /// The variable that was wrong.
        name: &'static str,
        /// What it said instead.
        value: String,
    },
}

/// What APNs said, collapsed to the four answers a caller can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// 200. Apple accepted it.
    Sent,
    /// 410, or a 400 that names a bad token. Retire the registration.
    TokenInvalid,
    /// A 4xx about us rather than the token: a bad key, a wrong topic, a
    /// malformed payload. Not retryable without a change.
    Rejected {
        /// The HTTP status, or 0 for a local signing failure.
        status: u16,
        /// Apple's `reason` string, when the body carried one.
        reason: String,
    },
    /// A network error, a timeout, 429 or 5xx. Try again later.
    Transient {
        /// What went wrong, for the log.
        detail: String,
    },
}

impl Outcome {
    /// Whether this result means the registration can be retired.
    #[must_use]
    pub fn retires_token(&self) -> bool {
        matches!(self, Outcome::TokenInvalid)
    }

    /// Whether a later attempt could plausibly succeed unchanged.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Outcome::Transient { .. })
    }
}

/// The resolved sender: key, ids, and a cached provider token.
pub struct ApnsSender {
    key: EcdsaKeyPair,
    key_id: String,
    team_id: String,
    default_bundle: Option<String>,
    default_environment: String,
    client: reqwest::Client,
    token: Mutex<Option<(String, Instant)>>,
}

impl std::fmt::Debug for ApnsSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The key never appears, not even as a fingerprint: a debug line
        // becomes a log line eventually.
        f.debug_struct("ApnsSender")
            .field("key_id", &self.key_id)
            .field("team_id", &self.team_id)
            .field("default_bundle", &self.default_bundle)
            .field("default_environment", &self.default_environment)
            .finish_non_exhaustive()
    }
}

impl ApnsSender {
    /// Build a sender from a PEM key and explicit ids.
    pub fn new(
        key_pem: &str,
        key_id: &str,
        team_id: &str,
        default_bundle: Option<String>,
        default_environment: &str,
    ) -> Result<Self, ApnsConfigError> {
        if key_pem.trim().is_empty() {
            return Err(ApnsConfigError::KeyEmpty);
        }
        if !matches!(default_environment, "development" | "production") {
            return Err(ApnsConfigError::BadEnvironment {
                name: "APNS_ENVIRONMENT",
                value: default_environment.to_owned(),
            });
        }
        let der = der_from_pem(key_pem)?;
        let key = EcdsaKeyPair::from_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &der,
            &SystemRandom::new(),
        )
        .map_err(|error| ApnsConfigError::KeyRejected(error.to_string()))?;

        // APNs speaks HTTP/2 only and refuses the upgrade dance. Without prior
        // knowledge the request goes out as 1.1 and Apple answers with a 400
        // that reads like a payload problem rather than a transport one.
        let client = reqwest::Client::builder()
            .http2_prior_knowledge()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| ApnsConfigError::KeyRejected(error.to_string()))?;

        Ok(Self {
            key,
            key_id: key_id.to_owned(),
            team_id: team_id.to_owned(),
            default_bundle,
            default_environment: default_environment.to_owned(),
            client,
            token: Mutex::new(None),
        })
    }

    /// Read the configuration from the environment.
    ///
    /// `Ok(None)` means genuinely unconfigured, which leaves the 503 in place.
    /// `Err` means somebody tried to configure it and got it wrong.
    pub fn from_env() -> Result<Option<Self>, ApnsConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// The same, against an arbitrary lookup, so the rules are testable.
    pub fn from_lookup(
        lookup: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, ApnsConfigError> {
        let non_empty = |name: &str| {
            lookup(name)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };
        let key_id = non_empty("APNS_KEY_ID");
        let team_id = non_empty("APNS_TEAM_ID");
        let key_path = non_empty("APNS_KEY_PATH");
        let key_contents = non_empty("APNS_KEY_CONTENTS");

        if key_id.is_none() && team_id.is_none() && key_path.is_none() && key_contents.is_none() {
            return Ok(None);
        }
        let key_id = key_id.ok_or(ApnsConfigError::Incomplete(
            "APNS_KEY_PATH or APNS_KEY_CONTENTS",
            "APNS_KEY_ID",
        ))?;
        let team_id = team_id.ok_or(ApnsConfigError::Incomplete(
            "APNS_KEY_PATH or APNS_KEY_CONTENTS",
            "APNS_TEAM_ID",
        ))?;

        let key_pem = match (key_path, key_contents) {
            (_, Some(contents)) => contents,
            (Some(path), None) => {
                let path = PathBuf::from(path);
                std::fs::read_to_string(&path).map_err(|source| ApnsConfigError::KeyUnreadable {
                    path: path.clone(),
                    source,
                })?
            }
            (None, None) => {
                return Err(ApnsConfigError::Incomplete(
                    "APNS_KEY_ID and APNS_TEAM_ID",
                    "APNS_KEY_PATH or APNS_KEY_CONTENTS",
                ));
            }
        };

        let environment = non_empty("APNS_ENVIRONMENT").unwrap_or_else(|| "production".to_owned());
        let bundle = non_empty("APNS_BUNDLE_ID");

        Self::new(&key_pem, &key_id, &team_id, bundle, &environment).map(Some)
    }

    /// The bundle id APNs will see in `apns-topic`.
    #[must_use]
    pub fn bundle_for(&self, registered: Option<&str>) -> String {
        registered
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| self.default_bundle.clone())
            .unwrap_or_else(|| DEFAULT_BUNDLE.to_owned())
    }

    /// The host for a registration's environment. An unknown value falls back
    /// to the configured default rather than assuming production, because a
    /// production send against a development token is the one combination that
    /// cannot be retried.
    #[must_use]
    pub fn host_for(&self, environment: Option<&str>) -> &'static str {
        let environment = environment
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(&self.default_environment);
        if environment.eq_ignore_ascii_case("development") {
            DEVELOPMENT_HOST
        } else {
            PRODUCTION_HOST
        }
    }

    /// The other environment's host, for the one retry that matters.
    ///
    /// A token is valid against exactly one host, and which one is a property
    /// of the *entitlement*, not of the build's receipt. A development-signed
    /// build can carry `aps-environment: production` (this app's entitlements
    /// file says exactly that), and the app's own heuristic then reports the
    /// wrong one. Retrying once against the other host turns that into a
    /// delivered push instead of a retired registration.
    #[must_use]
    pub fn other_host(&self, host: &str) -> &'static str {
        if host == DEVELOPMENT_HOST {
            PRODUCTION_HOST
        } else {
            DEVELOPMENT_HOST
        }
    }

    /// Send against an explicit host, for the retry.
    pub async fn send_silent_to(
        &self,
        host: &str,
        device_token: &str,
        bundle: Option<&str>,
    ) -> Outcome {
        let payload = json!({ "aps": { "content-available": 1 } });
        self.send_to(host, device_token, bundle, "background", 5, &payload)
            .await
    }

    /// Send an alert against an explicit host, for the retry.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_alert_to(
        &self,
        host: &str,
        device_token: &str,
        bundle: Option<&str>,
        title: &str,
        body: &str,
        category: Option<&str>,
        user_info: Option<&serde_json::Value>,
    ) -> Outcome {
        let payload = alert_payload(title, body, category, user_info);
        self.send_to(host, device_token, bundle, "alert", 10, &payload)
            .await
    }

    /// The provider token, minted once and reused until it is nearly stale.
    fn provider_token(&self) -> Result<String, ApnsConfigError> {
        let mut cached = self.token.lock().unwrap_or_else(|error| error.into_inner());
        if let Some((token, at)) = cached.as_ref()
            && at.elapsed() < TOKEN_LIFETIME
        {
            return Ok(token.clone());
        }
        let token = self.mint_token()?;
        *cached = Some((token.clone(), Instant::now()));
        Ok(token)
    }

    /// Build and sign the ES256 JWT Apple wants as a bearer token.
    pub(crate) fn mint_token(&self) -> Result<String, ApnsConfigError> {
        let header = json!({ "alg": "ES256", "kid": self.key_id });
        let claims = json!({
            "iss": self.team_id,
            "iat": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_secs())
                .unwrap_or(0),
        });
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string().as_bytes()),
            URL_SAFE_NO_PAD.encode(claims.to_string().as_bytes())
        );
        let signature = self
            .key
            .sign(&SystemRandom::new(), signing_input.as_bytes())
            .map_err(|error| ApnsConfigError::KeyRejected(error.to_string()))?;
        Ok(format!(
            "{signing_input}.{}",
            URL_SAFE_NO_PAD.encode(signature.as_ref())
        ))
    }

    /// The public key, so a test can prove the signature verifies rather than
    /// merely existing. `EcdsaKeyPair` reaches it through the
    /// `KeyPair` trait, which is why that trait is imported rather than the
    /// method being inherent.
    #[must_use]
    pub fn public_key(&self) -> &[u8] {
        use ring::signature::KeyPair as _;
        self.key.public_key().as_ref()
    }

    /// A wake-up with no visible notification.
    pub async fn send_silent(
        &self,
        device_token: &str,
        bundle: Option<&str>,
        environment: Option<&str>,
    ) -> Outcome {
        let payload = json!({ "aps": { "content-available": 1 } });
        self.send(device_token, bundle, environment, "background", 5, &payload)
            .await
    }

    /// A visible notification, optionally grouped by a thread id and carrying
    /// extra keys the app reads.
    #[allow(clippy::too_many_arguments)]
    pub async fn send_alert(
        &self,
        device_token: &str,
        bundle: Option<&str>,
        environment: Option<&str>,
        title: &str,
        body: &str,
        category: Option<&str>,
        user_info: Option<&serde_json::Value>,
    ) -> Outcome {
        let payload = alert_payload(title, body, category, user_info);
        self.send(device_token, bundle, environment, "alert", 10, &payload)
            .await
    }

    async fn send(
        &self,
        device_token: &str,
        bundle: Option<&str>,
        environment: Option<&str>,
        push_type: &str,
        priority: u8,
        payload: &serde_json::Value,
    ) -> Outcome {
        let host = self.host_for(environment);
        self.send_to(host, device_token, bundle, push_type, priority, payload)
            .await
    }

    async fn send_to(
        &self,
        host: &str,
        device_token: &str,
        bundle: Option<&str>,
        push_type: &str,
        priority: u8,
        payload: &serde_json::Value,
    ) -> Outcome {
        let token = match self.provider_token() {
            Ok(token) => token,
            Err(error) => {
                return Outcome::Rejected {
                    status: 0,
                    reason: format!("cannot mint a provider token: {error}"),
                };
            }
        };
        let url = format!("{host}/3/device/{}", device_token.trim());
        let response = self
            .client
            .post(&url)
            .header("authorization", format!("bearer {token}"))
            .header("apns-topic", self.bundle_for(bundle))
            .header("apns-push-type", push_type)
            .header("apns-priority", priority.to_string())
            .json(payload)
            .send()
            .await;

        match response {
            Ok(response) => classify(response).await,
            Err(error) => Outcome::Transient {
                // The URL carries the device token, so it is never part of the
                // detail: a log line is not a place for a push token.
                detail: if error.is_timeout() {
                    "timed out talking to APNs".to_owned()
                } else if error.is_connect() {
                    "could not connect to APNs".to_owned()
                } else {
                    format!("APNs request failed: {}", error.without_url())
                },
            },
        }
    }
}

/// Decode a PEM `PRIVATE KEY` block to the DER ring wants.
fn der_from_pem(key_pem: &str) -> Result<Vec<u8>, ApnsConfigError> {
    let body: String = key_pem
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("-----") && !line.is_empty())
        .collect();
    if body.is_empty() {
        return Err(ApnsConfigError::KeyRejected(
            "the key has no PEM body: is it really a .p8 file?".to_owned(),
        ));
    }
    STANDARD.decode(body.as_bytes()).map_err(|error| {
        ApnsConfigError::KeyRejected(format!("the PEM body is not base64: {error}"))
    })
}

/// The `aps` dictionary for a visible notification, plus the app's own keys.
fn alert_payload(
    title: &str,
    body: &str,
    category: Option<&str>,
    user_info: Option<&serde_json::Value>,
) -> serde_json::Value {
    let mut aps = serde_json::Map::new();
    aps.insert("alert".to_owned(), json!({ "title": title, "body": body }));
    aps.insert("sound".to_owned(), json!("default"));
    if let Some(category) = category {
        aps.insert("category".to_owned(), json!(category));
    }
    // Grouping by conversation is what stops ten notifications from ten
    // replies reading as ten unrelated things.
    if let Some(id) = user_info
        .and_then(|info| info.get("conversationId"))
        .and_then(serde_json::Value::as_str)
    {
        aps.insert("thread-id".to_owned(), json!(id));
    }
    let mut payload = serde_json::Map::new();
    payload.insert("aps".to_owned(), serde_json::Value::Object(aps));
    if let Some(info) = user_info.and_then(serde_json::Value::as_object) {
        for (key, value) in info {
            // `aps` belongs to the notification, never to the app payload.
            if key != "aps" {
                payload.insert(key.clone(), value.clone());
            }
        }
    }
    serde_json::Value::Object(payload)
}

/// Turn an APNs response into an [`Outcome`]. Separate so it can be tested
/// against a status and a body without a network.
pub(crate) async fn classify(response: reqwest::Response) -> Outcome {
    let status = response.status().as_u16();
    if status == 200 {
        return Outcome::Sent;
    }
    let body = response.text().await.unwrap_or_default();
    let reason = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| body.chars().take(120).collect());

    match status {
        410 => Outcome::TokenInvalid,
        // A wrong environment, a token from another team, or a topic that does
        // not match the token all arrive as one of these. All three mean the
        // registration cannot work as recorded.
        400 if matches!(
            reason.as_str(),
            "BadDeviceToken" | "DeviceTokenNotForTopic" | "Unregistered"
        ) =>
        {
            Outcome::TokenInvalid
        }
        429 => Outcome::Transient {
            detail: format!("APNs rate limited the request ({reason})"),
        },
        500..=599 => Outcome::Transient {
            detail: format!("APNs answered {status} ({reason})"),
        },
        _ => Outcome::Rejected { status, reason },
    }
}

/// A generated P-256 key in the PEM envelope Apple ships.
///
/// Shared with the device and notify tests. Test-only, so nothing in the
/// daemon can sign with a key that was never configured.
#[cfg(test)]
pub fn generated_test_key_pem() -> String {
    let rng = SystemRandom::new();
    let document =
        EcdsaKeyPair::generate_pkcs8(&ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .expect("ring can generate a P-256 key");
    let encoded = STANDARD.encode(document.as_ref());
    let mut pem = String::from("-----BEGIN PRIVATE KEY-----\n");
    for chunk in encoded.as_bytes().chunks(64) {
        pem.push_str(&String::from_utf8_lossy(chunk));
        pem.push('\n');
    }
    pem.push_str("-----END PRIVATE KEY-----\n");
    pem
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey};

    /// Nothing here is a real APNs key: it is generated per test. Shared with
    /// the notify tests, which need a sender and must never touch a real key.
    fn test_key_pem() -> String {
        generated_test_key_pem()
    }

    fn sender() -> ApnsSender {
        ApnsSender::new(
            &test_key_pem(),
            "LH5GM8356P",
            "58U7UPFS53",
            Some("net.fihonline.herald".to_owned()),
            "development",
        )
        .expect("a generated key builds a sender")
    }

    #[test]
    fn a_minted_token_is_a_verifiable_es256_jwt() {
        let sender = sender();
        let token = sender.mint_token().expect("a token is minted");
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3, "a JWT has three parts");

        let header: serde_json::Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(parts[0])
                .expect("header is base64url"),
        )
        .expect("header is JSON");
        assert_eq!(header["alg"], "ES256");
        assert_eq!(header["kid"], "LH5GM8356P");

        let claims: serde_json::Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD
                .decode(parts[1])
                .expect("claims are base64url"),
        )
        .expect("claims are JSON");
        assert_eq!(claims["iss"], "58U7UPFS53");
        assert!(
            claims["iat"].as_u64().unwrap_or(0) > 1_700_000_000,
            "iat is a real unix time, got {}",
            claims["iat"]
        );

        // The signature must verify against the sender's own public key. A
        // token that merely base64-decodes is a token Apple will reject.
        let signature = URL_SAFE_NO_PAD
            .decode(parts[2])
            .expect("signature is base64url");
        let input = format!("{}.{}", parts[0], parts[1]);
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, sender.public_key())
            .verify(input.as_bytes(), &signature)
            .expect("the signature verifies against the public key");
    }

    #[test]
    fn a_signature_is_64_bytes_and_not_der() {
        // APNs wants the raw R||S form that JWS specifies. A DER signature is
        // variable length and Apple rejects it, which is a maddening bug to
        // find in production, so the fixed length is asserted here.
        let sender = sender();
        let token = sender.mint_token().expect("a token is minted");
        let parts: Vec<&str> = token.split('.').collect();
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).expect("signature decodes");
        assert_eq!(signature.len(), 64);
    }

    #[test]
    fn the_token_is_cached_within_its_lifetime() {
        let sender = sender();
        let first = sender.provider_token().expect("first");
        let second = sender.provider_token().expect("second");
        assert_eq!(first, second, "a cached token is reused");
    }

    #[test]
    fn development_and_production_go_to_different_hosts() {
        let sender = sender();
        assert_eq!(sender.host_for(Some("development")), DEVELOPMENT_HOST);
        assert_eq!(sender.host_for(Some("production")), PRODUCTION_HOST);
        assert_eq!(sender.host_for(Some(" Development ")), DEVELOPMENT_HOST);
        // An absent or empty value falls back to the configured default, which
        // is development here.
        assert_eq!(sender.host_for(None), DEVELOPMENT_HOST);
        assert_eq!(sender.host_for(Some("")), DEVELOPMENT_HOST);
        assert_eq!(sender.host_for(Some("staging")), PRODUCTION_HOST);
    }

    #[test]
    fn a_registration_bundle_wins_over_the_default() {
        let sender = sender();
        assert_eq!(
            sender.bundle_for(Some("net.fihonline.kallisti")),
            "net.fihonline.kallisti"
        );
        assert_eq!(sender.bundle_for(None), "net.fihonline.herald");
        assert_eq!(sender.bundle_for(Some("   ")), "net.fihonline.herald");
    }

    #[test]
    fn a_sender_with_no_bundle_falls_back_to_this_app() {
        let sender =
            ApnsSender::new(&test_key_pem(), "K", "T", None, "production").expect("builds");
        assert_eq!(sender.bundle_for(None), DEFAULT_BUNDLE);
    }

    #[test]
    fn an_unconfigured_host_reports_none_rather_than_failing() {
        let config = ApnsSender::from_lookup(|_| None).expect("unconfigured is not an error");
        assert!(config.is_none());
    }

    #[test]
    fn blank_values_count_as_unset() {
        let config = ApnsSender::from_lookup(|_| Some("   ".to_owned()))
            .expect("whitespace is not a configuration");
        assert!(config.is_none());
    }

    #[test]
    fn a_half_configured_host_is_an_error_naming_the_missing_key() {
        let error = ApnsSender::from_lookup(|name| {
            (name == "APNS_KEY_ID").then(|| "LH5GM8356P".to_owned())
        })
        .expect_err("a key id alone is not a configuration");
        let message = error.to_string();
        assert!(
            message.contains("APNS_TEAM_ID"),
            "the error names what is missing, got {message}"
        );
    }

    #[test]
    fn ids_with_no_key_at_all_say_so() {
        let error = ApnsSender::from_lookup(|name| match name {
            "APNS_KEY_ID" => Some("LH5GM8356P".to_owned()),
            "APNS_TEAM_ID" => Some("58U7UPFS53".to_owned()),
            _ => None,
        })
        .expect_err("no key is not a configuration");
        let message = error.to_string();
        assert!(
            message.contains("APNS_KEY_PATH or APNS_KEY_CONTENTS"),
            "got {message}"
        );
    }

    #[test]
    fn inline_key_contents_are_accepted() {
        let key = test_key_pem();
        let sender = ApnsSender::from_lookup(|name| match name {
            "APNS_KEY_ID" => Some("LH5GM8356P".to_owned()),
            "APNS_TEAM_ID" => Some("58U7UPFS53".to_owned()),
            "APNS_KEY_CONTENTS" => Some(key.clone()),
            "APNS_ENVIRONMENT" => Some("production".to_owned()),
            _ => None,
        })
        .expect("an inline key configures")
        .expect("a sender exists");
        assert_eq!(sender.host_for(None), PRODUCTION_HOST);
        assert_eq!(sender.bundle_for(None), DEFAULT_BUNDLE);
    }

    #[test]
    fn an_unreadable_key_path_names_the_path_and_not_the_key() {
        let error = ApnsSender::from_lookup(|name| match name {
            "APNS_KEY_ID" => Some("LH5GM8356P".to_owned()),
            "APNS_TEAM_ID" => Some("58U7UPFS53".to_owned()),
            "APNS_KEY_PATH" => Some("/nonexistent/AuthKey_TEST.p8".to_owned()),
            _ => None,
        })
        .expect_err("a missing file is an error");
        let message = error.to_string();
        assert!(message.contains("/nonexistent/AuthKey_TEST.p8"));
        assert!(message.contains("cannot read"));
    }

    #[test]
    fn a_bad_environment_is_refused_by_name() {
        let error = ApnsSender::from_lookup(|name| match name {
            "APNS_KEY_ID" => Some("LH5GM8356P".to_owned()),
            "APNS_TEAM_ID" => Some("58U7UPFS53".to_owned()),
            "APNS_KEY_CONTENTS" => Some(test_key_pem()),
            "APNS_ENVIRONMENT" => Some("sandbox".to_owned()),
            _ => None,
        })
        .expect_err("sandbox is not an environment APNs has");
        assert!(error.to_string().contains("sandbox"));
    }

    #[test]
    fn a_non_key_document_is_rejected_rather_than_accepted() {
        let error = ApnsSender::new(
            "-----BEGIN PRIVATE KEY-----\nbm90IGEga2V5\n-----END PRIVATE KEY-----",
            "LH5GM8356P",
            "58U7UPFS53",
            None,
            "development",
        )
        .expect_err("garbage is not a key");
        assert!(matches!(error, ApnsConfigError::KeyRejected(_)));
    }

    #[test]
    fn a_pem_with_no_body_is_refused() {
        let error = ApnsSender::new(
            "-----BEGIN PRIVATE KEY-----\n-----END PRIVATE KEY-----",
            "K",
            "T",
            None,
            "development",
        )
        .expect_err("an empty PEM body is not a key");
        assert!(error.to_string().contains("no PEM body"));
    }

    #[test]
    fn an_empty_key_is_refused() {
        let error =
            ApnsSender::new("   ", "K", "T", None, "development").expect_err("blank is not a key");
        assert!(matches!(error, ApnsConfigError::KeyEmpty));
    }

    #[test]
    fn the_debug_line_never_contains_the_key() {
        let sender = sender();
        let rendered = format!("{sender:?}");
        assert!(rendered.contains("LH5GM8356P"));
        assert!(!rendered.contains("PRIVATE KEY"));
        assert!(!rendered.contains("MIG"));
    }

    /// A live round trip to Apple with a deliberately bogus device token.
    ///
    /// This is the only check that proves the whole chain at once: the PEM
    /// decodes, the JWT signs, HTTP/2 connects, the topic is accepted, and Apple
    /// answers in its own words. A bogus token must come back as
    /// `token_invalid`, which is Apple saying "I understood you, that token is
    /// not mine". A `Rejected` with `InvalidProviderToken` means the key is
    /// wrong, and a `Transient` means the network or the h2 handshake failed.
    ///
    /// Ignored by default because it needs the real key and the network.
    /// Run with:
    /// `APNS_KEY_PATH=... cargo test -p eris-host --lib apns::tests::live -- --ignored`
    #[tokio::test]
    #[ignore = "needs a real APNs key and network access"]
    async fn live_apns_accepts_our_signature_and_rejects_a_bogus_token() {
        let sender = ApnsSender::from_env()
            .expect("the environment parses")
            .expect("APNS_KEY_PATH and ids are set for this test");
        let bogus = "a".repeat(64);
        let outcome = sender
            .send_silent(&bogus, Some("net.fihonline.kallisti"), Some("development"))
            .await;
        match &outcome {
            Outcome::TokenInvalid => {}
            other => panic!(
                "Apple should call a bogus token invalid, meaning it read our \
                 signature and topic. It answered {other:?} instead."
            ),
        }
    }

    #[test]
    fn the_other_host_is_the_other_one() {
        let sender = sender();
        assert_eq!(sender.other_host(DEVELOPMENT_HOST), PRODUCTION_HOST);
        assert_eq!(sender.other_host(PRODUCTION_HOST), DEVELOPMENT_HOST);
        // Anything unrecognised is treated as production already, so the retry
        // goes to development rather than back to the same place.
        assert_eq!(sender.other_host("nonsense"), DEVELOPMENT_HOST);
    }

    #[test]
    fn an_alert_payload_carries_the_thread_and_the_app_keys_but_not_a_second_aps() {
        let info = serde_json::json!({
            "conversationId": "conv-1",
            "kind": "clarify",
            "aps": { "badge": 9 },
        });
        let payload = alert_payload("Title", "Body", Some("CLARIFY"), Some(&info));
        assert_eq!(payload["aps"]["alert"]["title"], "Title");
        assert_eq!(payload["aps"]["alert"]["body"], "Body");
        assert_eq!(payload["aps"]["category"], "CLARIFY");
        assert_eq!(payload["aps"]["thread-id"], "conv-1");
        assert_eq!(payload["aps"]["sound"], "default");
        assert_eq!(payload["kind"], "clarify");
        // The caller cannot smuggle a second `aps` in and win: the top-level
        // one is the one Apple reads, and it must be ours.
        assert_eq!(payload["aps"]["badge"], serde_json::Value::Null);
    }

    #[test]
    fn outcomes_say_what_the_caller_should_do() {
        assert!(!Outcome::Sent.retires_token());
        assert!(Outcome::TokenInvalid.retires_token());
        assert!(!Outcome::TokenInvalid.is_retryable());
        assert!(
            Outcome::Transient {
                detail: String::new()
            }
            .is_retryable()
        );
        assert!(
            !Outcome::Rejected {
                status: 403,
                reason: "InvalidProviderToken".to_owned()
            }
            .is_retryable()
        );
        assert!(
            !Outcome::Rejected {
                status: 403,
                reason: "InvalidProviderToken".to_owned()
            }
            .retires_token()
        );
    }
}
