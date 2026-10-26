//! The browser login: authorize URL, loopback callback, code exchange.
//!
//! One flow, two providers, and the differences between them are three facts
//! rather than three implementations:
//!
//! | | Claude | Antigravity |
//! |---|---|---|
//! | authorize | `https://claude.ai/oauth/authorize` | `https://accounts.google.com/o/oauth2/v2/auth` |
//! | token | `api.anthropic.com/v1/oauth/token`, JSON body | `oauth2.googleapis.com/token`, form body |
//! | secret | none (public client, PKCE) | installed-app secret |
//!
//! Three rules the module exists to enforce:
//!
//! - **nothing leaves this process without an operator.** A flow is started by a
//!   request from the dashboard, the URL is handed to a browser, and the verifier
//!   waits on [`PendingFlows`] until the callback arrives or the flow expires.
//!   There is no unattended login path.
//! - **the callback is its own listener.** It is an unauthenticated GET from a
//!   browser, so it gets one route on one loopback port rather than a seat inside
//!   the admin surface, which has a session and a documented trust boundary.
//! - **a code is exchanged once.** The pending entry is removed before the
//!   exchange is attempted, so a replayed callback finds nothing rather than
//!   spending the code twice.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use core_domain::{LlmError, TokenOrigin, TokenRecord};

use super::clients::{CLAUDE_SCOPES, Clients, GOOGLE_SCOPES};
use super::pkce::Pkce;
use super::refresh::{HttpRequest, HttpTransport, TransportError};

/// Which account a flow logs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    /// Anthropic, the public PKCE client.
    Claude,
    /// Google, the installed-app client the Antigravity surface uses.
    Antigravity,
}

impl Provider {
    /// The provider id, which is also the token-store file stem and the name a
    /// registry entry's `token:NAME` reference uses.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Antigravity => "antigravity",
        }
    }

    /// The name a person reads.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Antigravity => "Google Antigravity",
        }
    }

    /// The endpoint the account's models are served from, which is what a
    /// registry entry needs beside the token reference.
    #[must_use]
    pub const fn base_url(self) -> &'static str {
        match self {
            Self::Claude => "https://api.anthropic.com/v1",
            Self::Antigravity => "https://daily-cloudcode-pa.googleapis.com",
        }
    }

    /// The request shape its endpoint speaks, as a registry entry spells it.
    ///
    /// A registry entry needs this beside the token reference, and taking it from
    /// here is what keeps the account and the entry from disagreeing.
    #[must_use]
    pub const fn dialect(self) -> &'static str {
        match self {
            Self::Claude => "messages",
            Self::Antigravity => "antigravity",
        }
    }

    /// Every provider this build can log in to, in the order the settings page
    /// shows them.
    pub const ALL: [Self; 2] = [Self::Claude, Self::Antigravity];

    /// Parse the provider id a route carried.
    ///
    /// # Errors
    ///
    /// [`LlmError::Protocol`] naming the value, so a typo in a form is reported
    /// rather than resolved to a default account.
    pub fn parse(raw: &str) -> Result<Self, LlmError> {
        match raw.trim() {
            "claude" => Ok(Self::Claude),
            "antigravity" => Ok(Self::Antigravity),
            other => Err(LlmError::Protocol(format!(
                "`{other}` is not an account this build can connect; the choices are `claude` and `antigravity`"
            ))),
        }
    }
}

/// Ninety-nine characters of authorize URL, built from measured constants.
///
/// `code=true` is what the same client sends, and it is what makes the flow hand
/// back a code a person can paste as well as one a listener can catch.
#[must_use]
pub fn authorize_url(provider: Provider, clients: &Clients, pkce: &Pkce, state: &str) -> String {
    let redirect = clients.redirect_uri();
    match provider {
        Provider::Claude => {
            let scopes = CLAUDE_SCOPES.join(" ");
            format!(
                "https://claude.ai/oauth/authorize?code=true&client_id={client}\
                 &response_type=code&redirect_uri={redirect}&scope={scope}\
                 &code_challenge={challenge}&code_challenge_method={method}&state={state}",
                client = clients.claude_client_id,
                redirect = percent(&redirect),
                scope = percent(&scopes),
                challenge = pkce.challenge(),
                method = pkce.method(),
                state = percent(state),
            )
        }
        Provider::Antigravity => {
            let scopes = GOOGLE_SCOPES.join(" ");
            format!(
                "https://accounts.google.com/o/oauth2/v2/auth?client_id={client}\
                 &response_type=code&redirect_uri={redirect}&scope={scope}\
                 &code_challenge={challenge}&code_challenge_method={method}&state={state}\
                 &access_type=offline&prompt=consent",
                client = clients.google_client_id,
                redirect = percent(&redirect),
                scope = percent(&scopes),
                challenge = pkce.challenge(),
                method = pkce.method(),
                state = percent(state),
            )
        }
    }
}

/// Percent-encode a query value. The unreserved set only, which is what a client
/// id, a redirect URI, a scope list and a state value all consist of plus their
/// separators.
fn percent(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// What a token endpoint answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// The bearer value.
    pub access_token: String,
    /// The value a refresh spends, when the endpoint rotated or issued one.
    pub refresh_token: Option<String>,
    /// Lifetime in seconds, when reported.
    pub expires_in: Option<i64>,
    /// Scopes the grant was issued for, as granted.
    pub scope: Option<String>,
}

/// Parse a successful token-endpoint body.
///
/// # Errors
///
/// [`LlmError::Auth`] when the body is not JSON or carries no access token. The
/// detail names the field and never the body, which contains the grant.
pub fn parse_grant(body: &str) -> Result<Grant, LlmError> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| LlmError::Auth {
        detail: "the token endpoint answered with something that is not JSON".to_owned(),
    })?;
    let access_token = value
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| LlmError::Auth {
            detail: "the token endpoint answered without an access token".to_owned(),
        })?
        .to_owned();
    Ok(Grant {
        access_token,
        refresh_token: value
            .get("refresh_token")
            .and_then(serde_json::Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_owned),
        expires_in: value.get("expires_in").and_then(serde_json::Value::as_i64),
        scope: value
            .get("scope")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    })
}

/// The request one code exchange sends.
///
/// JSON for Anthropic, because that is the encoding its own client uses and the
/// endpoint validates the code before the encoding, so a shape error and a bad
/// code would otherwise be indistinguishable. Form for Google, which is what its
/// token endpoint documents.
#[must_use]
pub fn exchange_request(
    provider: Provider,
    clients: &Clients,
    code: &str,
    verifier: &str,
    state: &str,
) -> HttpRequest {
    let redirect = clients.redirect_uri();
    match provider {
        Provider::Claude => {
            let body = serde_json::json!({
                "grant_type": "authorization_code",
                "code": code,
                "client_id": clients.claude_client_id,
                "redirect_uri": redirect,
                "code_verifier": verifier,
                // Anthropic's token endpoint answers 400 "Invalid request
                // format" when `state` is absent; its own client always sends it.
                "state": state,
            });
            HttpRequest {
                url: "https://api.anthropic.com/v1/oauth/token".to_owned(),
                content_type: "application/json",
                body: body.to_string(),
            }
        }
        Provider::Antigravity => {
            let mut body = format!(
                "grant_type=authorization_code&code={}&client_id={}&redirect_uri={}&code_verifier={}",
                percent(code),
                percent(&clients.google_client_id),
                percent(&redirect),
                percent(verifier),
            );
            if let Some(secret) = clients.google_client_secret.as_deref() {
                body.push_str(&format!("&client_secret={}", percent(secret)));
            }
            HttpRequest {
                url: "https://oauth2.googleapis.com/token".to_owned(),
                content_type: "application/x-www-form-urlencoded",
                body,
            }
        }
    }
}

/// Spend one authorization code and turn it into a stored grant.
///
/// The returned record is [`TokenOrigin::Login`]: this daemon performed the login
/// and owns the grant, so it is the one record that may be refreshed once
/// `oauth.auto_refresh` is on.
///
/// `account` is the label the status line shows (an email or a display name), and
/// `project` is the cloud project an Antigravity request is billed to.
///
/// # Errors
///
/// [`LlmError::Auth`] for a refusal, a transport failure, or a body with no
/// grant. The detail never carries a token: a refusal body can echo part of the
/// request, and the request carried the code and the verifier.
#[allow(clippy::too_many_arguments)]
pub fn exchange(
    provider: Provider,
    clients: &Clients,
    transport: &dyn HttpTransport,
    code: &str,
    verifier: &str,
    state: &str,
    now: i64,
    account: Option<String>,
    project: Option<String>,
) -> Result<TokenRecord, LlmError> {
    let request = exchange_request(provider, clients, code, verifier, state);
    let (status, body) = transport.post(&request).map_err(|error| LlmError::Auth {
        detail: match error {
            TransportError::Unreachable(detail) => format!(
                "the {} token endpoint could not be reached: {detail}",
                provider.label()
            ),
        },
    })?;
    if !(200..300).contains(&status) {
        return Err(LlmError::Auth {
            detail: format!(
                "the {} login was refused by the token endpoint with HTTP {status}; \
                 start the login again from Settings",
                provider.label()
            ),
        });
    }
    let grant = parse_grant(&body)?;
    Ok(TokenRecord {
        provider: provider.id().to_owned(),
        access_token: grant.access_token,
        refresh_token: grant.refresh_token,
        expires_at: grant.expires_in.map(|seconds| now + seconds),
        scopes: grant
            .scope
            .as_deref()
            .map(|scope| {
                scope
                    .split([' ', ','])
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        project_id: project,
        token_type: Some("Bearer".to_owned()),
        last_refresh: Some(now),
        account,
        origin: TokenOrigin::Login,
    })
}

/// A login that has been started and is waiting for its callback.
#[derive(Debug)]
pub struct Pending {
    /// Which account.
    pub provider: Provider,
    /// The proof key. Redacted `Debug`, and dropped when the flow ends.
    pub pkce: Pkce,
    /// The anti-forgery token the callback must echo.
    pub state: String,
    /// The authorize URL the operator was handed, so the page can offer it again.
    pub authorize_url: String,
    /// When this flow stops being accepted, in unix seconds.
    pub expires_at: i64,
}

/// How long a started login stays valid.
///
/// Ten minutes: long enough to find a browser and sign in, short enough that a
/// forgotten flow is not a token waiting to be spent.
pub const FLOW_TTL_SECS: i64 = 600;

/// The logins this process has started and not yet finished.
///
/// In process on purpose. A pending login is a verifier and a state value, and
/// neither belongs in a database: a restart ends the logins, exactly as it ends
/// the turns and the questions they were waiting on.
#[derive(Debug, Default)]
pub struct PendingFlows {
    by_state: Mutex<HashMap<String, Pending>>,
}

impl PendingFlows {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget any flow that has expired by `now`, and return how many were
    /// dropped. Called before an insert so an abandoned flow cannot accumulate.
    fn sweep(&self, now: i64) {
        if let Ok(mut flows) = self.by_state.lock() {
            flows.retain(|_, pending| pending.expires_at > now);
        }
    }

    /// Record a started flow. One flow per provider: starting a second login for
    /// the same account replaces the first, because keeping both would mean two
    /// verifiers for one account and a callback that could match either.
    pub fn start(&self, pending: Pending, now: i64) {
        self.sweep(now);
        if let Ok(mut flows) = self.by_state.lock() {
            flows.retain(|_, existing| existing.provider != pending.provider);
            flows.insert(pending.state.clone(), pending);
        }
    }

    /// Take the flow a callback names, removing it. `None` when the state is
    /// unknown, was already spent, or has expired.
    ///
    /// Taking rather than reading is the whole point: a code is exchanged once,
    /// so a replayed callback finds nothing.
    pub fn take(&self, state: &str, now: i64) -> Option<Pending> {
        let mut flows = self.by_state.lock().ok()?;
        flows.retain(|_, pending| pending.expires_at > now);
        flows.remove(state)
    }

    /// The flow currently pending for one provider, for the status panel.
    #[must_use]
    pub fn pending_for(&self, provider: Provider, now: i64) -> Option<(String, i64)> {
        let flows = self.by_state.lock().ok()?;
        flows
            .values()
            .find(|pending| pending.provider == provider && pending.expires_at > now)
            .map(|pending| (pending.authorize_url.clone(), pending.expires_at))
    }

    /// The state value of the flow pending for one provider, so a pasted address
    /// that carries no state can still be matched to the login that is waiting.
    #[must_use]
    pub fn state_for(&self, provider: Provider, now: i64) -> Option<String> {
        let flows = self.by_state.lock().ok()?;
        flows
            .values()
            .find(|pending| pending.provider == provider && pending.expires_at > now)
            .map(|pending| pending.state.clone())
    }

    /// How many flows are pending, expired ones excluded.
    #[must_use]
    pub fn len(&self, now: i64) -> usize {
        self.by_state
            .lock()
            .map(|flows| {
                flows
                    .values()
                    .filter(|pending| pending.expires_at > now)
                    .count()
            })
            .unwrap_or(0)
    }

    /// Whether no flow is pending.
    #[must_use]
    pub fn is_empty(&self, now: i64) -> bool {
        self.len(now) == 0
    }
}

/// Unix seconds now, saturating at zero rather than panicking on a clock before
/// the epoch.
#[must_use]
pub fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// The query pairs of a request target, as `(key, value)` with no decoding of
/// `+` (a code never contains one, and a state value is base64url).
#[must_use]
pub fn query_pairs(target: &str) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    let Some((_, query)) = target.split_once('?') else {
        return pairs;
    };
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        pairs.push((decode(key), decode(value)));
    }
    pairs
}

/// One pair from a query, by key.
#[must_use]
pub fn query_value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

/// Percent-decode a query component. `+` is a space, which is what a form
/// encoding means by it.
fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = &value[index + 1..index + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What an operator pasted back after approving a login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pasted {
    /// The authorization code, when one was found.
    pub code: Option<String>,
    /// The state value, when the paste carried one.
    pub state: Option<String>,
    /// The provider's own error code, when the paste was a refusal.
    pub error: Option<String>,
}

/// Read what the operator pasted: the whole address the browser landed on
/// (`http://localhost:7345/callback?code=...&state=...`, even when the page
/// itself would not load), a `code#state` pair as the provider's own page shows
/// it, or a bare code.
///
/// Forgiving about surrounding whitespace and quotes, and never about meaning: a
/// paste with nothing code-shaped in it yields no code rather than a guess.
#[must_use]
pub fn parse_paste(raw: &str) -> Pasted {
    let text = raw
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '<' || c == '>')
        .trim();
    if text.contains('?') {
        // An address. Anything after a `#` is a fragment, not part of the query.
        let query = text.split('#').next().unwrap_or(text);
        let pairs = query_pairs(query);
        let get = |key: &str| {
            query_value(&pairs, key)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        return Pasted {
            code: get("code"),
            state: get("state"),
            error: get("error"),
        };
    }
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return Pasted {
            code: None,
            state: None,
            error: None,
        };
    }
    match text.split_once('#') {
        Some((code, state)) => Pasted {
            code: Some(code.to_owned()).filter(|code| !code.is_empty()),
            state: Some(state.to_owned()).filter(|state| !state.is_empty()),
            error: None,
        },
        None => Pasted {
            code: Some(text.to_owned()),
            state: None,
            error: None,
        },
    }
}

/// How long the callback listener waits for a browser before giving up.
pub const CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

/// The page a browser sees when a login finishes.
///
/// Hand-written rather than rendered from the dashboard's stylesheet: this
/// listener serves one route, and pulling the whole shell in would make the
/// callback depend on a session it does not have.
#[must_use]
pub fn callback_page(ok: bool, message: &str) -> String {
    let (title, tone) = if ok {
        ("Account connected", "#2f7d55")
    } else {
        ("Login did not finish", "#a33b3b")
    };
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>{title}</title><style>\
         :root{{color-scheme:dark}}body{{margin:0;min-height:100vh;display:grid;\
         place-items:center;background:#101113;color:#e8e6e1;\
         font:16px/1.5 system-ui,-apple-system,Segoe UI,sans-serif}}\
         main{{max-width:34rem;padding:2rem;border:1px solid #2a2c31;border-radius:14px;\
         background:#16181b}}h1{{margin:0 0 .75rem;font-size:1.25rem;color:{tone}}}\
         p{{margin:0 0 .5rem}}code{{background:#0e0f11;padding:.1rem .35rem;border-radius:4px}}\
         </style></head><body><main><h1>{title}</h1><p>{message}</p>\
         <p>You can close this tab and return to <code>Kallisti settings</code>.</p>\
         </main></body></html>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn clients() -> Clients {
        Clients {
            google_client_secret: Some("GOCSPX-test".to_owned()),
            callback_port: 7345,
            ..Clients::default()
        }
    }

    #[test]
    fn a_claude_authorize_url_carries_the_challenge_the_state_and_the_scopes() {
        let clients = clients();
        let pkce = Pkce::generate().expect("randomness");
        let url = authorize_url(Provider::Claude, &clients, &pkce, "state-1");
        assert!(
            url.starts_with("https://claude.ai/oauth/authorize?"),
            "{url}"
        );
        assert!(url.contains("code=true"), "{url}");
        assert!(url.contains(&format!("client_id={}", clients.claude_client_id)));
        assert!(url.contains(&format!("code_challenge={}", pkce.challenge())));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=state-1"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A7345%2Fcallback"));
        assert!(url.contains("user%3Ainference"), "{url}");
        // The verifier itself is never in the URL: only its challenge is.
        assert!(!url.contains(pkce.verifier()), "{url}");
    }

    #[test]
    fn a_google_authorize_url_asks_for_offline_access_so_a_refresh_token_is_issued() {
        let clients = clients();
        let pkce = Pkce::generate().expect("randomness");
        let url = authorize_url(Provider::Antigravity, &clients, &pkce, "state-2");
        assert!(
            url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"),
            "{url}"
        );
        assert!(url.contains(&format!("client_id={}", clients.google_client_id)));
        assert!(url.contains("access_type=offline"), "{url}");
        assert!(url.contains("prompt=consent"), "{url}");
        assert!(url.contains("state=state-2"));
        assert!(url.contains("cloud-platform"), "{url}");
        assert!(
            url.contains(&format!("code_challenge={}", pkce.challenge())),
            "{url}"
        );
        assert!(
            !url.contains(pkce.verifier()),
            "the verifier is never in the URL"
        );
    }

    #[test]
    fn an_exchange_sends_json_to_anthropic_and_a_form_to_google() {
        let clients = clients();
        let claude = exchange_request(
            Provider::Claude,
            &clients,
            "the-code",
            "the-verifier",
            "the-state",
        );
        assert_eq!(claude.url, "https://api.anthropic.com/v1/oauth/token");
        assert_eq!(claude.content_type, "application/json");
        let body: serde_json::Value = serde_json::from_str(&claude.body).expect("json");
        assert_eq!(body["grant_type"], "authorization_code");
        assert_eq!(body["code"], "the-code");
        assert_eq!(body["code_verifier"], "the-verifier");
        assert_eq!(
            body["state"], "the-state",
            "Anthropic rejects an exchange with no state"
        );
        assert_eq!(body["redirect_uri"], "http://localhost:7345/callback");

        let google = exchange_request(
            Provider::Antigravity,
            &clients,
            "the-code",
            "the-verifier",
            "the-state",
        );
        assert_eq!(google.url, "https://oauth2.googleapis.com/token");
        assert_eq!(google.content_type, "application/x-www-form-urlencoded");
        assert!(google.body.contains("grant_type=authorization_code"));
        assert!(google.body.contains("code_verifier=the-verifier"));
        assert!(google.body.contains("client_secret=GOCSPX-test"));
    }

    #[test]
    fn a_grant_body_parses_and_a_body_without_a_token_does_not() {
        let grant = parse_grant(
            r#"{"access_token":"at","refresh_token":"rt","expires_in":3600,"scope":"a b"}"#,
        )
        .expect("parses");
        assert_eq!(grant.access_token, "at");
        assert_eq!(grant.refresh_token.as_deref(), Some("rt"));
        assert_eq!(grant.expires_in, Some(3600));

        let error = parse_grant(r#"{"error":"invalid_grant"}"#).expect_err("no token");
        assert!(matches!(error, LlmError::Auth { .. }));
        assert!(!error.to_string().contains("invalid_grant"), "no body echo");
    }

    #[test]
    fn a_query_is_read_by_key_and_percent_decoded() {
        let pairs = query_pairs("/callback?code=ab%2Fcd&state=xyz&empty=");
        assert_eq!(query_value(&pairs, "code"), Some("ab/cd"));
        assert_eq!(query_value(&pairs, "state"), Some("xyz"));
        assert_eq!(query_value(&pairs, "empty"), Some(""));
        assert_eq!(query_value(&pairs, "missing"), None);
        assert!(query_pairs("/callback").is_empty());
    }

    #[test]
    fn a_pasted_address_yields_its_code_and_state_even_when_the_page_never_loaded() {
        let pasted = parse_paste("  localhost:7345/callback?code=JKNS5GcX&state=1QBfZCBA9w-eoH0\n");
        assert_eq!(pasted.code.as_deref(), Some("JKNS5GcX"));
        assert_eq!(pasted.state.as_deref(), Some("1QBfZCBA9w-eoH0"));
        let full = parse_paste("\"http://localhost:7345/callback?code=a%2Fb&state=s#frag\"");
        assert_eq!(full.code.as_deref(), Some("a/b"));
        assert_eq!(full.state.as_deref(), Some("s"));
    }

    #[test]
    fn a_code_hash_state_pair_and_a_bare_code_are_both_accepted() {
        let pair = parse_paste("thecode#thestate");
        assert_eq!(pair.code.as_deref(), Some("thecode"));
        assert_eq!(pair.state.as_deref(), Some("thestate"));
        let bare = parse_paste("thecode");
        assert_eq!(bare.code.as_deref(), Some("thecode"));
        assert_eq!(bare.state, None);
    }

    #[test]
    fn a_paste_with_no_code_in_it_yields_none_and_a_provider_error_is_kept() {
        assert_eq!(parse_paste("").code, None);
        assert_eq!(parse_paste("this is not a code").code, None);
        let refused = parse_paste("http://localhost:7345/callback?error=access_denied&state=s");
        assert_eq!(refused.code, None);
        assert_eq!(refused.error.as_deref(), Some("access_denied"));
    }

    #[test]
    fn a_flow_is_taken_once_so_a_replayed_callback_finds_nothing() {
        let flows = PendingFlows::new();
        let pkce = Pkce::generate().expect("randomness");
        flows.start(
            Pending {
                provider: Provider::Claude,
                pkce,
                state: "s1".to_owned(),
                authorize_url: "https://example.test/authorize".to_owned(),
                expires_at: 1_000,
            },
            900,
        );
        assert_eq!(flows.len(900), 1);
        assert!(flows.take("s1", 900).is_some());
        assert!(flows.take("s1", 900).is_none(), "a code is spent once");
        assert!(flows.is_empty(900));
    }

    #[test]
    fn an_expired_flow_is_refused_and_swept() {
        let flows = PendingFlows::new();
        flows.start(
            Pending {
                provider: Provider::Antigravity,
                pkce: Pkce::generate().expect("randomness"),
                state: "s2".to_owned(),
                authorize_url: "https://example.test/authorize".to_owned(),
                expires_at: 100,
            },
            0,
        );
        assert!(flows.take("s2", 101).is_none(), "past its expiry");
        assert!(flows.is_empty(0));
    }

    #[test]
    fn starting_a_second_login_for_the_same_account_replaces_the_first() {
        let flows = PendingFlows::new();
        flows.start(
            Pending {
                provider: Provider::Claude,
                pkce: Pkce::generate().expect("randomness"),
                state: "old".to_owned(),
                authorize_url: "https://example.test/old".to_owned(),
                expires_at: 1_000,
            },
            0,
        );
        flows.start(
            Pending {
                provider: Provider::Claude,
                pkce: Pkce::generate().expect("randomness"),
                state: "new".to_owned(),
                authorize_url: "https://example.test/new".to_owned(),
                expires_at: 1_000,
            },
            0,
        );
        assert_eq!(flows.len(0), 1);
        assert!(flows.take("old", 0).is_none());
        assert!(flows.take("new", 0).is_some());
    }

    #[test]
    fn the_two_accounts_do_not_displace_each_other() {
        let flows = PendingFlows::new();
        for (provider, state) in [(Provider::Claude, "c"), (Provider::Antigravity, "a")] {
            flows.start(
                Pending {
                    provider,
                    pkce: Pkce::generate().expect("randomness"),
                    state: state.to_owned(),
                    authorize_url: "https://example.test/authorize".to_owned(),
                    expires_at: 1_000,
                },
                0,
            );
        }
        assert_eq!(flows.len(0), 2);
        assert!(flows.pending_for(Provider::Claude, 0).is_some());
        assert!(flows.pending_for(Provider::Antigravity, 0).is_some());
    }
}
