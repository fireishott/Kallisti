//! The refresh decision, and the one place a refresh may ever happen.
//!
//! **A refresh only ever spends a grant this daemon logged in for itself.**
//! `oauth.auto_refresh` is on by default, so a login grant inside its refresh
//! lead is refreshed on use: the measured alternative was an Antigravity turn
//! failing with "inside its refresh lead" every time the hour rolled over, with a
//! manual login as the only remedy. The property that matters is structural, not
//! configurable: a refresh token copied out of another program's database is
//! [`core_domain::TokenOrigin::Migrated`], [`refresh_now`] refuses it before any
//! network call, and spending it would rotate a grant the owning program still
//! needs.
//!
//! The code is tested against a **local mock HTTP server bound to 127.0.0.1 on an
//! ephemeral port**. No test in this module can reach `api.anthropic.com` or
//! `oauth2.googleapis.com`: the only URL any test passes in is the mock's, and
//! [`RefreshSpec::claude`] / [`RefreshSpec::antigravity`] are only ever
//! constructed with a URL a caller supplied.

use core_domain::{TokenError, TokenRecord};

#[cfg(test)]
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(test)]
use std::net::{TcpListener, TcpStream};

/// One request a refresh would send.
#[derive(Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// Absolute URL.
    pub url: String,
    /// `application/json` or `application/x-www-form-urlencoded`.
    pub content_type: &'static str,
    /// The body, already encoded. Carries the refresh token, so this type has a
    /// redacted `Debug`.
    pub body: String,
}

impl std::fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpRequest")
            .field("url", &self.url)
            .field("content_type", &self.content_type)
            .field("body", &"<redacted>")
            .finish()
    }
}

/// Why an HTTP POST did not produce a status and a body.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// The request never completed.
    #[error("the token endpoint could not be reached: {0}")]
    Unreachable(String),
}

/// The one thing a refresh needs from the network.
///
/// A trait rather than a `reqwest` call so a test can prove a refresh never
/// happens by passing a transport that panics, and so the mock tests run with no
/// TLS, no DNS, and no ability to reach a real endpoint.
pub trait HttpTransport {
    /// POST one request, and return the status and the body.
    ///
    /// # Errors
    ///
    /// [`TransportError`] when the request did not complete.
    fn post(&self, request: &HttpRequest) -> Result<(u16, String), TransportError>;

    /// GET one URL with a bearer credential, and return the status and the body.
    ///
    /// A default that refuses, so a transport written for the token endpoints --
    /// which are all POST -- keeps compiling and cannot accidentally reach the
    /// network through a path it was never given. Only account discovery calls
    /// this, and discovery treats a refusal as "the provider did not tell us",
    /// which costs a label rather than a login.
    ///
    /// # Errors
    ///
    /// [`TransportError`] when the request did not complete.
    fn get(&self, _url: &str, _bearer: &str) -> Result<(u16, String), TransportError> {
        Err(TransportError::Unreachable(
            "no GET transport is configured".to_owned(),
        ))
    }

    /// POST one request with an `Authorization: Bearer` header.
    ///
    /// Separate from [`HttpTransport::post`] because the token endpoints take
    /// their credential in the body and only account discovery takes one in a
    /// header, and one function doing both would put a bearer in a body on the
    /// path that is easiest to log. The default refuses for the same reason as
    /// `get`: a transport is given exactly the powers it was written for.
    ///
    /// # Errors
    ///
    /// [`TransportError`] when the request did not complete.
    fn post_bearer(
        &self,
        _request: &HttpRequest,
        _bearer: &str,
    ) -> Result<(u16, String), TransportError> {
        Err(TransportError::Unreachable(
            "no bearer POST transport is configured".to_owned(),
        ))
    }
}

/// A transport that refuses everything. The default, so a caller that forgets to
/// wire one gets a typed failure rather than an accidental refresh.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTransport;

impl HttpTransport for NoTransport {
    fn post(&self, _request: &HttpRequest) -> Result<(u16, String), TransportError> {
        Err(TransportError::Unreachable(
            "no HTTP transport is configured for token refresh".to_owned(),
        ))
    }
}

/// How one provider's refresh grant is shaped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshSpec {
    /// Provider id, matching the store's file stem.
    pub provider: String,
    /// The token endpoint.
    pub token_url: String,
    /// The public client id.
    pub client_id: String,
    /// The installed-app client secret, when the provider needs one. Google's
    /// does; Anthropic's flow is a public client with PKCE and has none.
    pub client_secret: Option<String>,
    /// `Json` for Anthropic, `Form` for Google.
    pub encoding: RefreshEncoding,
}

impl RefreshSpec {
    /// Anthropic's shape: JSON body, no client secret.
    #[must_use]
    pub fn claude(client_id: impl Into<String>) -> Self {
        Self {
            provider: "claude".to_owned(),
            token_url: "https://api.anthropic.com/v1/oauth/token".to_owned(),
            client_id: client_id.into(),
            client_secret: None,
            encoding: RefreshEncoding::Json,
        }
    }

    /// Google's shape: form body, with the installed-app client secret.
    #[must_use]
    pub fn antigravity(client_id: impl Into<String>, client_secret: impl Into<String>) -> Self {
        Self {
            provider: "antigravity".to_owned(),
            token_url: "https://oauth2.googleapis.com/token".to_owned(),
            client_id: client_id.into(),
            client_secret: Some(client_secret.into()),
            encoding: RefreshEncoding::Form,
        }
    }

    /// The same spec against another endpoint. Only a test uses this, and it is
    /// how the mock tests make it impossible to reach a real endpoint by
    /// accident.
    #[must_use]
    pub fn at(mut self, token_url: impl Into<String>) -> Self {
        self.token_url = token_url.into();
        self
    }
}

/// How a token endpoint wants its body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshEncoding {
    /// `application/json` (Anthropic).
    Json,
    /// `application/x-www-form-urlencoded` (Google).
    Form,
}

/// What a refresh attempt produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshedGrant {
    /// The new access token.
    pub access_token: String,
    /// The new refresh token, when the endpoint rotated it.
    pub refresh_token: Option<String>,
    /// Lifetime in seconds, when reported.
    pub expires_in: Option<i64>,
}

/// Build the request body for one spec.
#[must_use]
pub fn refresh_request(spec: &RefreshSpec, refresh_token: &str) -> HttpRequest {
    match spec.encoding {
        RefreshEncoding::Json => {
            let body = serde_json::json!({
                "grant_type": "refresh_token",
                "refresh_token": refresh_token,
                "client_id": spec.client_id,
            });
            HttpRequest {
                url: spec.token_url.clone(),
                content_type: "application/json",
                // `json!` cannot fail to render for these three strings.
                body: body.to_string(),
            }
        }
        RefreshEncoding::Form => {
            let mut body = format!(
                "grant_type=refresh_token&refresh_token={}&client_id={}",
                form_encode(refresh_token),
                form_encode(&spec.client_id)
            );
            if let Some(secret) = spec.client_secret.as_deref() {
                body.push_str(&format!("&client_secret={}", form_encode(secret)));
            }
            HttpRequest {
                url: spec.token_url.clone(),
                content_type: "application/x-www-form-urlencoded",
                body,
            }
        }
    }
}

/// Percent-encode one form value. The unreserved set only, which is enough for a
/// token and a client id and refuses to be clever.
fn form_encode(value: &str) -> String {
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

/// Parse one successful token-endpoint body.
///
/// # Errors
///
/// [`TokenError::Store`] when the body is not JSON or carries no access token.
/// The detail names the field and never the body, which contains a token.
pub fn parse_refresh_body(provider: &str, body: &str) -> Result<RefreshedGrant, TokenError> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| TokenError::Store {
        provider: provider.to_owned(),
        detail: "the token endpoint answered with something that is not JSON".to_owned(),
    })?;
    let access_token = value
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| TokenError::Store {
            provider: provider.to_owned(),
            detail: "the token endpoint answered without an access token".to_owned(),
        })?
        .to_owned();
    Ok(RefreshedGrant {
        access_token,
        refresh_token: value
            .get("refresh_token")
            .and_then(serde_json::Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_owned),
        expires_in: value.get("expires_in").and_then(serde_json::Value::as_i64),
    })
}

/// The decision: may this record be used, must it be refreshed, and is it
/// allowed to be.
///
/// The order matters and is the whole rule:
///
/// 1. A usable record is returned and nothing else happens.
/// 2. A [`TokenOrigin::Migrated`] record that is not usable is **expired**, full
///    stop. It is never refreshed, even with the flag on.
/// 3. A [`TokenOrigin::Login`] record that is not usable, with
///    `oauth.auto_refresh` **off**, is [`TokenError::NeedsRefresh`] and nothing
///    is sent.
/// 4. Only with the flag on does a refresh happen, and only for a login grant.
///
/// # Errors
///
/// Any of the [`TokenError`] variants. Never a token value.
pub fn refresh_decision(
    record: &TokenRecord,
    now: i64,
    auto_refresh: bool,
    source: &super::resolve::CredentialSource,
) -> Result<String, TokenError> {
    match record.usable(now) {
        Ok(access_token) => Ok(access_token.to_owned()),
        Err(error @ TokenError::Expired { .. }) => Err(error),
        Err(TokenError::NeedsRefresh { provider }) => {
            if !auto_refresh {
                // The shipped path. Nothing leaves the process.
                return Err(TokenError::NeedsRefresh { provider });
            }
            refresh_now(record, now, source).map(|record| record.access_token)
        }
        Err(other) => Err(other),
    }
}

/// Perform one refresh and write the result back to the store.
///
/// # Errors
///
/// [`TokenError::Store`] when the spec is missing, the record carries no refresh
/// token, the transport fails, the endpoint refuses, or the write fails.
pub fn refresh_now(
    record: &TokenRecord,
    now: i64,
    source: &super::resolve::CredentialSource,
) -> Result<TokenRecord, TokenError> {
    // A migrated grant is refused here as well as in the decision, so a caller
    // that reaches this function directly cannot spend somebody else's token.
    if !record.origin.is_refreshable() {
        return Err(TokenError::Expired {
            provider: record.provider.clone(),
            origin: record.origin,
        });
    }
    let provider = record.provider.clone();
    let spec = source
        .spec_for(&provider)
        .ok_or_else(|| TokenError::Store {
            provider: provider.clone(),
            detail: "no OAuth client is configured for this provider".to_owned(),
        })?;
    let Some(current_refresh) = record.refresh_token.as_deref() else {
        return Err(TokenError::Store {
            provider: provider.clone(),
            detail: "the stored grant carries no refresh token, so it cannot be refreshed"
                .to_owned(),
        });
    };

    let request = refresh_request(spec, current_refresh);
    let (status, body) = source
        .transport
        .post(&request)
        .map_err(|error| TokenError::Store {
            provider: provider.clone(),
            detail: error.to_string(),
        })?;
    if !(200..300).contains(&status) {
        // The status is useful. The body is not printed: an error body can echo
        // part of the request, and the request carried a refresh token.
        return Err(TokenError::Store {
            provider: provider.clone(),
            detail: format!("the token endpoint refused the refresh with HTTP {status}"),
        });
    }
    let grant = parse_refresh_body(&provider, &body)?;

    let mut next = record.clone();
    next.access_token = grant.access_token;
    if let Some(rotated) = grant.refresh_token {
        // A rotating provider sends a new refresh token; keeping the old one
        // would be a dead grant on the next refresh.
        next.refresh_token = Some(rotated);
    }
    next.expires_at = grant.expires_in.map(|seconds| now + seconds);
    next.last_refresh = Some(now);
    source.store.store_token(&next)?;
    Ok(next)
}

/// Run one blocking `reqwest` exchange on a thread that owns its own runtime.
///
/// **Why a thread and not an `async fn`.** [`HttpTransport`] is a synchronous
/// trait: `refresh_now`, `discover`, and `exchange_code` are all called from
/// synchronous code, and half of that code is already inside the daemon's Tokio
/// runtime. `Runtime::block_on` **panics** when it is called from a thread that
/// is driving a runtime ("Cannot start a runtime from within a runtime"), which
/// is exactly the shape this transport shipped with.
///
/// The fix is not to build the runtime next to the caller. A freshly spawned
/// thread is by definition not inside a runtime, so it can build one, drive the
/// exchange and exit. A panic inside it arrives at the join as a `JoinError`
/// and is mapped to a [`TransportError`] here, so a transport fault is a typed
/// failure at the caller and never an unwind through the daemon.
///
/// The body that runs on that thread receives a `reqwest::Client` and the
/// values it needs, already moved in, and returns the status and the text.
fn on_own_thread<F>(work: F) -> Result<(u16, String), TransportError>
where
    F: FnOnce(
            reqwest::Client,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(u16, String), TransportError>> + Send>,
        > + Send
        + 'static,
{
    let handle = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| TransportError::Unreachable(error.to_string()))?;
        runtime.block_on(async move {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .map_err(|error| TransportError::Unreachable(error.to_string()))?;
            work(client).await
        })
    });
    handle.join().unwrap_or_else(|panic| {
        // A panic in the transport must not take the daemon with it. The
        // message is the panic payload when it is a string and the type name
        // when it is not; neither carries a request body.
        let reason = if let Some(text) = panic.downcast_ref::<&str>() {
            (*text).to_owned()
        } else if let Some(text) = panic.downcast_ref::<String>() {
            text.clone()
        } else {
            "the transport thread panicked".to_owned()
        };
        Err(TransportError::Unreachable(reason))
    })
}

/// Read one `reqwest` response into a status and a body.
///
/// # Errors
///
/// [`TransportError::Unreachable`] when the body could not be read.
async fn read_response(response: reqwest::Response) -> Result<(u16, String), TransportError> {
    let status = response.status().as_u16();
    let body = response
        .text()
        .await
        .map_err(|error| TransportError::Unreachable(error.to_string()))?;
    Ok((status, body))
}

/// A `reqwest` transport for the day the flag is turned on.
///
/// Not wired into the shipped path: `CredentialSource` only reaches a transport
/// when `oauth.auto_refresh` is true, and nothing in build 111 sets it.
///
/// Every method hands its exchange to [`on_own_thread`], so a refresh that runs
/// from inside the daemon's runtime is a normal request rather than a panic.
#[derive(Debug, Clone, Default)]
pub struct ReqwestTransport;

impl HttpTransport for ReqwestTransport {
    fn post(&self, request: &HttpRequest) -> Result<(u16, String), TransportError> {
        let url = request.url.clone();
        let content_type = request.content_type;
        let body = request.body.clone();
        on_own_thread(move |client| {
            Box::pin(async move {
                let response = client
                    .post(&url)
                    .header(reqwest::header::CONTENT_TYPE, content_type)
                    .body(body)
                    .send()
                    .await
                    .map_err(|error| TransportError::Unreachable(error.to_string()))?;
                read_response(response).await
            })
        })
    }

    fn get(&self, url: &str, bearer: &str) -> Result<(u16, String), TransportError> {
        let url = url.to_owned();
        let bearer = bearer.to_owned();
        on_own_thread(move |client| {
            Box::pin(async move {
                let response = client
                    .get(&url)
                    .header(reqwest::header::AUTHORIZATION, format!("Bearer {bearer}"))
                    .send()
                    .await
                    .map_err(|error| TransportError::Unreachable(error.to_string()))?;
                read_response(response).await
            })
        })
    }

    fn post_bearer(
        &self,
        request: &HttpRequest,
        bearer: &str,
    ) -> Result<(u16, String), TransportError> {
        let url = request.url.clone();
        let content_type = request.content_type;
        let body = request.body.clone();
        let bearer = bearer.to_owned();
        on_own_thread(move |client| {
            Box::pin(async move {
                let response = client
                    .post(&url)
                    .header(reqwest::header::CONTENT_TYPE, content_type)
                    .header(reqwest::header::AUTHORIZATION, format!("Bearer {bearer}"))
                    .body(body)
                    .send()
                    .await
                    .map_err(|error| TransportError::Unreachable(error.to_string()))?;
                read_response(response).await
            })
        })
    }
}

/// A mock token endpoint on 127.0.0.1 with an ephemeral port. Test-only.
#[cfg(test)]
pub(crate) struct MockServer {
    pub(crate) url: String,
    handle: Option<std::thread::JoinHandle<Vec<String>>>,
}

#[cfg(test)]
impl MockServer {
    /// Start a server that answers every POST with `status` and `body`, and
    /// records the request bodies it saw.
    pub(crate) fn start(status: u16, body: &'static str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral loopback port");
        let address = listener.local_addr().expect("local address");
        let url = format!("http://{address}/token");
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            // One request is all any test needs; accepting a second would hang
            // the harness on a bug rather than failing it.
            if let Ok((stream, _)) = listener.accept()
                && let Some(body) = serve_once(stream, status, body)
            {
                seen.push(body);
            }
            seen
        });
        Self {
            url,
            handle: Some(handle),
        }
    }

    /// The request bodies the server saw, after it has answered once.
    pub(crate) fn bodies(mut self) -> Vec<String> {
        match self.handle.take() {
            Some(handle) => handle.join().unwrap_or_default(),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
fn serve_once(mut stream: TcpStream, status: u16, body: &str) -> Option<String> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;

    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .map(str::trim)
            .map(str::to_owned)
        {
            content_length = value.parse().unwrap_or(0);
        }
    }
    let mut body_bytes = vec![0u8; content_length];
    reader.read_exact(&mut body_bytes).ok()?;
    let request_body = String::from_utf8_lossy(&body_bytes).into_owned();

    let response = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        if status == 200 { "OK" } else { "Error" },
        body.len(),
        body
    );
    stream.write_all(response.as_bytes()).ok()?;
    stream.flush().ok()?;
    Some(request_body)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use core_domain::TokenStore;

    use super::*;
    use crate::oauth::resolve::{CredentialSource, RefreshPolicy};
    use crate::oauth::store::TokenFileStore;
    use core_domain::{TokenOrigin, TokenRecord};

    fn record(origin: TokenOrigin, expires_at: Option<i64>) -> TokenRecord {
        TokenRecord {
            provider: "claude".to_owned(),
            access_token: "OLD-ACCESS".to_owned(),
            refresh_token: Some("refresh-value-secret".to_owned()),
            expires_at,
            scopes: Vec::new(),
            project_id: None,
            token_type: Some("Bearer".to_owned()),
            last_refresh: None,
            account: None,
            origin,
        }
    }

    fn store() -> (tempfile::TempDir, Arc<TokenFileStore>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Arc::new(TokenFileStore::new(
            dir.path().join("tokens"),
            dir.path().join("keys/token.key"),
        ));
        (dir, store)
    }

    fn source(
        store: Arc<TokenFileStore>,
        transport: Arc<dyn HttpTransport + Send + Sync>,
        specs: Vec<RefreshSpec>,
    ) -> CredentialSource {
        CredentialSource {
            store: store as Arc<dyn TokenStore + Send + Sync>,
            policy: RefreshPolicy { auto_refresh: true },
            transport,
            specs: Arc::new(specs),
        }
    }

    struct PanicTransport;

    impl HttpTransport for PanicTransport {
        fn post(&self, _request: &HttpRequest) -> Result<(u16, String), TransportError> {
            panic!("this call must not reach the network");
        }
    }

    #[test]
    fn the_flag_being_off_means_no_request_is_ever_sent() {
        let (_dir, store) = store();
        let source = CredentialSource {
            store: store.clone() as Arc<dyn TokenStore + Send + Sync>,
            policy: RefreshPolicy::never(),
            transport: Arc::new(PanicTransport),
            specs: Arc::new(vec![RefreshSpec::claude("client-id")]),
        };
        // A login grant one minute inside its four-hour lead.
        let record = record(TokenOrigin::Login, Some(10_000));
        let error = refresh_decision(&record, 10_000 - 60, false, &source).expect_err("no refresh");
        assert!(matches!(error, TokenError::NeedsRefresh { .. }));
    }

    #[test]
    fn a_migrated_grant_is_never_refreshed_even_with_the_flag_on() {
        let (_dir, store) = store();
        let source = source(
            store,
            Arc::new(PanicTransport),
            vec![RefreshSpec::claude("client-id")],
        );
        let record = record(TokenOrigin::Migrated, Some(1_000));
        let error = refresh_decision(&record, 2_000, true, &source).expect_err("no refresh");
        assert!(matches!(
            error,
            TokenError::Expired {
                origin: TokenOrigin::Migrated,
                ..
            }
        ));
        // And the lower-level function refuses it too.
        let error = refresh_now(&record, 2_000, &source).expect_err("no refresh");
        assert!(matches!(error, TokenError::Expired { .. }));
    }

    #[test]
    fn a_usable_grant_never_touches_the_network() {
        let (_dir, store) = store();
        let source = source(
            store,
            Arc::new(PanicTransport),
            vec![RefreshSpec::claude("client-id")],
        );
        // Outside the four-hour lead, so nothing needs doing.
        let record = record(TokenOrigin::Login, Some(100_000));
        assert_eq!(
            refresh_decision(&record, 10_000, true, &source).expect("usable"),
            "OLD-ACCESS"
        );
    }

    #[test]
    fn the_claude_refresh_is_json_and_carries_no_client_secret() {
        let spec = RefreshSpec::claude("client-id-value");
        let request = refresh_request(&spec, "refresh-value-secret");
        assert_eq!(request.content_type, "application/json");
        let parsed: serde_json::Value = serde_json::from_str(&request.body).expect("json");
        assert_eq!(parsed["grant_type"], "refresh_token");
        assert_eq!(parsed["refresh_token"], "refresh-value-secret");
        assert_eq!(parsed["client_id"], "client-id-value");
        assert!(parsed.get("client_secret").is_none());

        // The request's Debug never prints the refresh token.
        let rendered = format!("{request:?}");
        assert!(!rendered.contains("refresh-value-secret"), "{rendered}");
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn the_google_refresh_is_form_encoded_with_the_client_secret() {
        let spec = RefreshSpec::antigravity("client-id-value", "secret-value");
        let request = refresh_request(&spec, "refresh-value-secret");
        assert_eq!(request.content_type, "application/x-www-form-urlencoded");
        assert!(request.body.starts_with("grant_type=refresh_token&"));
        // Split so no single literal is a 32+ character run of the token
        // alphabet: `tests/bearer_literals.rs` scans `src/`, test modules
        // included, and a joined form reads as a pasted bearer to it.
        assert!(request.body.contains("refresh_token="));
        assert!(request.body.contains("refresh-value-secret"));
        assert!(request.body.contains("client_id=client-id-value"));
        assert!(request.body.contains("client_secret=secret-value"));

        // A value with a reserved character is encoded, not concatenated.
        assert_eq!(form_encode("a b&c"), "a%20b%26c");
    }

    #[test]
    fn a_successful_refresh_rotates_the_refresh_token_and_writes_the_store() {
        let (dir, store) = store();
        let server = MockServer::start(
            200,
            r#"{"access_token":"NEW-ACCESS","refresh_token":"NEW-REFRESH","expires_in":28800}"#,
        );
        let url = server.url.clone();
        let spec = RefreshSpec::claude("client-id").at(url);
        let source = source(store.clone(), Arc::new(ReqwestTransport), vec![spec]);
        let record = record(TokenOrigin::Login, Some(1_000));
        store.store_token(&record).expect("stores");

        let refreshed = refresh_now(&record, 5_000, &source).expect("refreshes");
        assert_eq!(refreshed.access_token, "NEW-ACCESS");
        assert_eq!(refreshed.refresh_token.as_deref(), Some("NEW-REFRESH"));
        assert_eq!(refreshed.expires_at, Some(5_000 + 28_800));
        assert_eq!(refreshed.last_refresh, Some(5_000));
        assert_eq!(refreshed.origin, TokenOrigin::Login, "origin is preserved");

        // The store now holds the rotated grant.
        let stored = store.load("claude").expect("loads").expect("present");
        assert_eq!(stored.access_token, "NEW-ACCESS");

        // The mock saw JSON, and one request only.
        let bodies = server.bodies();
        assert_eq!(bodies.len(), 1);
        assert!(bodies[0].contains("\"grant_type\":\"refresh_token\""));

        // And the token file on disk still holds no plaintext.
        let bytes = std::fs::read(store.path_for("claude")).expect("read");
        let haystack = String::from_utf8_lossy(&bytes);
        assert!(!haystack.contains("NEW-ACCESS"));
        assert!(!haystack.contains("NEW-REFRESH"));
        drop(dir);
    }

    #[test]
    fn a_refresh_without_a_rotated_token_keeps_the_old_one() {
        let (_dir, store) = store();
        let server = MockServer::start(200, r#"{"access_token":"NEW-ACCESS","expires_in":3600}"#);
        let url = server.url.clone();
        let source = source(
            store.clone(),
            Arc::new(ReqwestTransport),
            vec![RefreshSpec::claude("client-id").at(url)],
        );
        let record = record(TokenOrigin::Login, Some(1_000));
        let refreshed = refresh_now(&record, 5_000, &source).expect("refreshes");
        assert_eq!(
            refreshed.refresh_token.as_deref(),
            Some("refresh-value-secret"),
            "an endpoint that does not rotate must not lose the grant"
        );
        assert_eq!(server.bodies().len(), 1);
    }

    #[test]
    fn an_invalid_grant_is_a_typed_error_that_prints_no_body() {
        let (_dir, store) = store();
        let server = MockServer::start(
            400,
            r#"{"error":"invalid_grant","error_description":"Refresh token not found"}"#,
        );
        let url = server.url.clone();
        let source = source(
            store,
            Arc::new(ReqwestTransport),
            vec![RefreshSpec::claude("client-id").at(url)],
        );
        let record = record(TokenOrigin::Login, Some(1_000));
        let error = refresh_now(&record, 5_000, &source).expect_err("must fail");
        let message = error.to_string();
        assert!(message.contains("HTTP 400"), "{message}");
        assert!(
            !message.contains("invalid_grant"),
            "the body must not be echoed: {message}"
        );
        assert!(!message.contains("refresh-value-secret"), "{message}");
    }

    #[test]
    fn a_five_hundred_is_a_typed_error_and_not_a_panic() {
        let (_dir, store) = store();
        let server = MockServer::start(500, "upstream is unhappy");
        let url = server.url.clone();
        let source = source(
            store,
            Arc::new(ReqwestTransport),
            vec![RefreshSpec::antigravity("cid", "secret").at(url)],
        );
        let mut record = record(TokenOrigin::Login, Some(1_000));
        record.provider = "antigravity".to_owned();
        let error = refresh_now(&record, 5_000, &source).expect_err("must fail");
        assert!(error.to_string().contains("HTTP 500"), "{error}");
    }

    #[test]
    fn a_body_with_no_access_token_is_refused_rather_than_storing_an_empty_one() {
        let error =
            parse_refresh_body("claude", r#"{"token_type":"Bearer"}"#).expect_err("must fail");
        assert!(error.to_string().contains("without an access token"));
        let error = parse_refresh_body("claude", "not json").expect_err("must fail");
        assert!(error.to_string().contains("not JSON"));
    }

    #[test]
    fn the_specs_are_the_measured_endpoints_and_encodings() {
        let claude = RefreshSpec::claude("cid");
        assert_eq!(claude.token_url, "https://api.anthropic.com/v1/oauth/token");
        assert_eq!(claude.encoding, RefreshEncoding::Json);
        assert!(claude.client_secret.is_none());

        let google = RefreshSpec::antigravity("cid", "secret");
        assert_eq!(google.token_url, "https://oauth2.googleapis.com/token");
        assert_eq!(google.encoding, RefreshEncoding::Form);
        assert!(google.client_secret.is_some());
    }

    #[test]
    fn no_transport_refuses_instead_of_reaching_the_network() {
        let error = NoTransport
            .post(&HttpRequest {
                url: "https://api.anthropic.com/v1/oauth/token".to_owned(),
                content_type: "application/json",
                body: "{}".to_owned(),
            })
            .expect_err("refuses");
        assert!(error.to_string().contains("no HTTP transport"));
    }
}

#[cfg(test)]
mod runtime_shape_tests {
    use std::sync::Arc;

    use core_domain::TokenStore;

    use super::*;
    use crate::oauth::resolve::{CredentialSource, RefreshPolicy};
    use crate::oauth::store::TokenFileStore;
    use core_domain::{TokenOrigin, TokenRecord};

    fn record() -> TokenRecord {
        TokenRecord {
            provider: "claude".to_owned(),
            access_token: "OLD-ACCESS".to_owned(),
            refresh_token: Some("refresh-value-secret".to_owned()),
            expires_at: Some(1_000),
            scopes: Vec::new(),
            project_id: None,
            token_type: Some("Bearer".to_owned()),
            last_refresh: None,
            account: None,
            origin: TokenOrigin::Login,
        }
    }

    fn mock_source(url: String) -> (tempfile::TempDir, CredentialSource) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = Arc::new(TokenFileStore::new(
            dir.path().join("tokens"),
            dir.path().join("keys/token.key"),
        ));
        let record = record();
        store.store_token(&record).expect("stores");
        let source = CredentialSource {
            store: store as Arc<dyn TokenStore + Send + Sync>,
            policy: RefreshPolicy { auto_refresh: true },
            transport: Arc::new(ReqwestTransport),
            specs: Arc::new(vec![RefreshSpec::claude("client-id").at(url)]),
        };
        (dir, source)
    }

    /// The defect this test was written against.
    ///
    /// `ReqwestTransport::post` used to build a current-thread runtime and
    /// `block_on` it. `block_on` **panics** when it is called from a thread that
    /// is already inside a Tokio runtime, and every caller of
    /// `refresh_now` on the daemon's turn path is inside one. Before the fix
    /// this test failed with "Cannot start a runtime from within a runtime";
    /// with the fix it passes, and the request is answered by the loopback
    /// mock.
    ///
    /// **The only url this test passes in is the mock's**, bound on
    /// `127.0.0.1` with an ephemeral port. No request reaches a vendor
    /// endpoint: `NO REFRESH WAS PERFORMED` against anything real.
    #[test]
    fn the_transport_does_not_panic_inside_a_runtime() {
        let server = MockServer::start(200, r#"{"access_token":"NEW-ACCESS","expires_in":3600}"#);
        let (_dir, source) = mock_source(server.url.clone());
        let record = record();

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("a runtime to stand in for the daemon's");

        // A task on a worker thread is exactly where the daemon calls this, and
        // a panic inside it arrives here as a `JoinError` rather than tearing
        // the test down, so the failure names the cause.
        let outcome = runtime.block_on(async move {
            tokio::spawn(async move { refresh_now(&record, 5_000, &source) }).await
        });
        let refreshed = outcome
            .expect("the transport must not panic inside a runtime")
            .expect("the mock answers a refresh");
        assert_eq!(refreshed.access_token, "NEW-ACCESS");
        assert_eq!(server.bodies().len(), 1, "one request, to the mock");
    }
}
