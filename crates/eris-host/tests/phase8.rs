//! P8 acceptance: the device surface contract over real HTTP.
//!
//! Every test here talks to a real `axum::serve` on an ephemeral loopback port
//! over a real TCP socket, so the coverage is the listener and not just the
//! router: request ids, header parsing, SSE framing, and the 404 the admin paths
//! earn.
//!
//! The provider is a local stub that speaks the OpenAI-compatible SSE dialect the
//! seam already parses. That is what makes the turn path testable offline and
//! deterministic; the real 9router turn is proved separately, by hand, in the
//! phase self-proof.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use eris_host::auth::{AuthStore, PairingService, Scope, ScopeSet, TokenOrigin, TokenService};
use eris_host::config::Paths;
use eris_host::db::{Db, SetupState};
use eris_host::device::turns::{DEFAULT_SSE_BUFFER, TurnStore};
use eris_host::device::{DEVICE_ADDR, DeviceService};
use eris_host::onboarding::SetupEngine;

/// The environment variable name the tests point the provider at. No such
/// variable exists: the service is built with an injected lookup, because edition
/// 2024 makes mutating the process environment `unsafe` and this workspace
/// forbids unsafe.
const TEST_KEY_REF: &str = "env:KALLISTI_P8_TEST_KEY";

/// The key the injected lookup returns.
const TEST_KEY: &str = "p8-test-key";

/// The body type a JSON request sends.
const JSON: &str = "application/json";

/// The body type a form request sends.
const FORM: &str = "application/x-www-form-urlencoded";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// One running device listener plus the state behind it.
struct Harness {
    _dir: tempfile::TempDir,
    addr: SocketAddr,
    engine: SetupEngine,
    /// Kept so a future test can assert on the service directly; the listener
    /// already holds its own clone.
    #[allow(dead_code)]
    service: DeviceService,
    server: tokio::task::JoinHandle<()>,
}

impl Harness {
    /// A listener in `ready` state, pointed at `provider_url`, with `buffer`
    /// retained events per job.
    async fn start(provider_url: &str, buffer: usize) -> Self {
        Self::spawn(provider_url, buffer, None).await
    }

    /// A listener whose service was handed an MCP pool, the way the daemon hands
    /// one to the device surface it serves.
    async fn start_with_mcp(
        provider_url: &str,
        buffer: usize,
        pool: Arc<eris_host::mcp::Pool>,
    ) -> Self {
        Self::spawn(provider_url, buffer, Some(pool)).await
    }

    /// Start one listener, optionally over an MCP pool.
    async fn spawn(
        provider_url: &str,
        buffer: usize,
        mcp: Option<Arc<eris_host::mcp::Pool>>,
    ) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open(dir.path().join("state/eris.db")).expect("db opens");
        let engine = SetupEngine::new(db.clone(), Paths::for_root(dir.path().join("root")));
        engine
            .set_provider(provider_url, TEST_KEY_REF, &["p8-model".to_owned()])
            .expect("provider settings");
        engine.set_model("p8-model").expect("model setting");
        db.set_setup_state(SetupState::Ready).expect("ready");

        let service = DeviceService::with_key_lookup(
            engine.clone(),
            buffer,
            Arc::new(|name: &str| (name == "KALLISTI_P8_TEST_KEY").then(|| TEST_KEY.to_owned())),
        );
        let service = match mcp {
            Some(pool) => service.with_mcp(pool),
            None => service,
        };
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral loopback listener");
        let addr = listener.local_addr().expect("a local address");
        let serving = service.clone();
        let server = tokio::spawn(async move {
            let _ = eris_host::device::serve_on(listener, serving).await;
        });

        Self {
            _dir: dir,
            addr,
            engine,
            service,
            server,
        }
    }

    /// A listener that has not finished onboarding.
    async fn start_not_ready() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open(dir.path().join("state/eris.db")).expect("db opens");
        let engine = SetupEngine::new(db, Paths::for_root(dir.path().join("root")));
        let service = DeviceService::with_key_lookup(
            engine.clone(),
            DEFAULT_SSE_BUFFER,
            Arc::new(|name: &str| (name == "KALLISTI_P8_TEST_KEY").then(|| TEST_KEY.to_owned())),
        );
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("an ephemeral loopback listener");
        let addr = listener.local_addr().expect("a local address");
        let serving = service.clone();
        let server = tokio::spawn(async move {
            let _ = eris_host::device::serve_on(listener, serving).await;
        });
        Self {
            _dir: dir,
            addr,
            engine,
            service,
            server,
        }
    }

    /// Register a device and mint a pairing code bound to it.
    fn device_with_code(&self, name: &str) -> (String, String) {
        let db = self.engine.db().clone();
        let device_id = AuthStore::new(db.clone())
            .register_device(name, "ios")
            .expect("device row");
        let issued = PairingService::new(db)
            .issue_for_device(&device_id)
            .expect("pairing code");
        (device_id, issued.code().to_owned())
    }

    /// Register a device and mint a pairing code the operator ticked the
    /// administration box for.
    fn device_with_admin_code(&self, name: &str) -> (String, String) {
        let db = self.engine.db().clone();
        let device_id = AuthStore::new(db.clone())
            .register_device(name, "ios")
            .expect("device row");
        let issued = PairingService::new(db)
            .issue_for_device_asking(&device_id, true)
            .expect("pairing code");
        (device_id, issued.code().to_owned())
    }

    /// A device and a live token that carries `admin`, without going through
    /// `/v1/pair`: the same scope set a ticked code redeems into.
    fn device_with_admin_token(&self, name: &str) -> (String, String) {
        let db = self.engine.db().clone();
        let device_id = AuthStore::new(db.clone())
            .register_device(name, "ios")
            .expect("device row");
        let token = TokenService::new(db)
            .mint_access(
                &device_id,
                ScopeSet::device_default().with(Scope::Admin),
                TokenOrigin::PairedAdmin,
            )
            .expect("admin access token");
        (device_id, token.token().to_owned())
    }

    /// A device and a live access token, without going through `/v1/pair`.
    fn device_with_token(&self, name: &str) -> (String, String) {
        let db = self.engine.db().clone();
        let device_id = AuthStore::new(db.clone())
            .register_device(name, "ios")
            .expect("device row");
        let token = TokenService::new(db)
            .mint_access(&device_id, ScopeSet::device_default(), TokenOrigin::Device)
            .expect("access token");
        (device_id, token.token().to_owned())
    }

    async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: Option<&str>,
    ) -> HttpResponse {
        http(self.addr, method, path, headers, body, JSON).await
    }

    /// A form-encoded body, which is what a `curl --data-urlencode` run sends.
    async fn request_form(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: &str,
    ) -> HttpResponse {
        http(self.addr, method, path, headers, Some(body), FORM).await
    }

    fn stop(&self) {
        self.server.abort();
    }
}

/// The decoded HTTP status, headers and body of one request.
struct HttpResponse {
    status: u16,
    headers: String,
    body: String,
}

impl HttpResponse {
    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or_else(|error| {
            panic!(
                "body is not json ({error}): status={} body={}",
                self.status, self.body
            )
        })
    }
}

/// One request over a real socket, with `Connection: close`, so the body ends
/// when the socket does.
async fn http(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    body: Option<&str>,
    content_type: &str,
) -> HttpResponse {
    let mut stream = TcpStream::connect(addr)
        .await
        .expect("connect to the listener");
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\n");
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    if let Some(body) = body {
        request.push_str(&format!("content-length: {}\r\n", body.len()));
        request.push_str(&format!("content-type: {content_type}\r\n"));
    }
    request.push_str("\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write the request");
    if let Some(body) = body {
        stream
            .write_all(body.as_bytes())
            .await
            .expect("write the body");
    }

    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), stream.read_to_end(&mut raw))
        .await
        .expect("the response arrives")
        .expect("read the response");
    let text = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("a status line");
    let body = if head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        dechunk(body)
    } else {
        body.to_owned()
    };
    HttpResponse {
        status,
        headers: head.to_owned(),
        body,
    }
}

// ---------------------------------------------------------------------------
// SSE client
// ---------------------------------------------------------------------------

/// One parsed SSE frame.
#[derive(Debug, Clone)]
struct Frame {
    id: i64,
    event: String,
    data: serde_json::Value,
}

/// A live SSE connection: reads incrementally, decodes chunked framing when the
/// server uses it, and hands back complete frames.
struct Sse {
    stream: TcpStream,
    chunked: bool,
    raw: Vec<u8>,
    body: Vec<u8>,
    closed: bool,
}

impl Sse {
    async fn open(addr: SocketAddr, path: &str, token: &str, last_event_id: Option<i64>) -> Self {
        let mut stream = TcpStream::connect(addr).await.expect("connect for sse");
        let mut request = format!(
            "GET {path} HTTP/1.1\r\nhost: 127.0.0.1\r\naccept: text/event-stream\r\nauthorization: Bearer {token}\r\n"
        );
        if let Some(id) = last_event_id {
            request.push_str(&format!("last-event-id: {id}\r\n"));
        }
        request.push_str("\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("write the sse request");

        // Read until the header block is complete.
        let mut head = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut byte))
                .await
                .expect("sse headers arrive")
                .expect("read sse headers");
            assert!(read == 1, "connection closed before the sse headers");
            head.push(byte[0]);
            if head.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&head).to_ascii_lowercase();
        assert!(
            head.starts_with("http/1.1 200"),
            "the stream must open with 200, got: {head}"
        );
        assert!(
            head.contains("text/event-stream"),
            "the stream must be event-stream: {head}"
        );
        let chunked = head.contains("transfer-encoding: chunked");
        Self {
            stream,
            chunked,
            raw: Vec::new(),
            body: Vec::new(),
            closed: false,
        }
    }

    /// The next frame, or `None` on timeout or a closed stream.
    async fn next_frame(&mut self, timeout: Duration) -> Option<Frame> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Some(frame) = self.take_frame() {
                return Some(frame);
            }
            if self.closed {
                return None;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let mut buffer = [0u8; 8192];
            match tokio::time::timeout(remaining, self.stream.read(&mut buffer)).await {
                Ok(Ok(0)) => self.closed = true,
                Ok(Ok(read)) => {
                    self.raw.extend_from_slice(&buffer[..read]);
                    if self.chunked {
                        self.decode_chunks();
                    } else {
                        self.body.extend_from_slice(&self.raw);
                        self.raw.clear();
                    }
                }
                _ => return None,
            }
        }
    }

    /// Every frame until the stream ends or the timeout expires.
    async fn frames_until_end(&mut self, timeout: Duration) -> Vec<Frame> {
        let mut frames = Vec::new();
        while let Some(frame) = self.next_frame(timeout).await {
            frames.push(frame);
        }
        frames
    }

    fn decode_chunks(&mut self) {
        loop {
            let Some(position) = find(&self.raw, b"\r\n") else {
                return;
            };
            let size_line = String::from_utf8_lossy(&self.raw[..position]).into_owned();
            let size = usize::from_str_radix(size_line.split(';').next().unwrap_or("0").trim(), 16)
                .unwrap_or(0);
            let start = position + 2;
            if size == 0 {
                self.closed = true;
                return;
            }
            if self.raw.len() < start + size + 2 {
                return;
            }
            self.body.extend_from_slice(&self.raw[start..start + size]);
            self.raw.drain(..start + size + 2);
        }
    }

    fn take_frame(&mut self) -> Option<Frame> {
        let position = find(&self.body, b"\n\n")?;
        let block = String::from_utf8_lossy(&self.body[..position]).into_owned();
        self.body.drain(..position + 2);

        let mut id = None;
        let mut event = None;
        let mut data = String::new();
        let mut has_data = false;
        for line in block.lines() {
            if line.starts_with(':') {
                continue;
            }
            let (name, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.trim_start();
            match name {
                "id" => id = value.trim().parse::<i64>().ok(),
                "event" => event = Some(value.trim().to_owned()),
                "data" => {
                    if has_data {
                        data.push('\n');
                    }
                    data.push_str(value);
                    has_data = true;
                }
                _ => {}
            }
        }
        if !has_data {
            // A keep-alive comment carries no frame.
            return self.take_frame();
        }
        Some(Frame {
            id: id.expect("every frame carries an id"),
            event: event.expect("every frame carries an event"),
            data: serde_json::from_str(&data).unwrap_or(serde_json::Value::String(data)),
        })
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// A chunked body, decoded once it is complete (non-streaming responses only).
fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size_line, remainder)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        if remainder.len() < size {
            out.push_str(remainder);
            break;
        }
        out.push_str(&remainder[..size]);
        rest = remainder[size..]
            .strip_prefix("\r\n")
            .unwrap_or(&remainder[size..]);
    }
    out
}

// ---------------------------------------------------------------------------
// The stub provider
// ---------------------------------------------------------------------------

/// A provider stub that streams `deltas` one frame at a time.
struct StubProvider {
    url: String,
    hits: Arc<AtomicUsize>,
    _task: tokio::task::JoinHandle<()>,
}

impl StubProvider {
    /// Start a stub. `gap` is the delay between deltas, so a test can hold a turn
    /// open long enough to cancel or disconnect mid-stream.
    async fn start(deltas: &[&'static str], gap: Duration) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a stub provider listener");
        let addr = listener.local_addr().expect("a stub provider address");
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        let deltas: Vec<&'static str> = deltas.to_vec();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                counter.fetch_add(1, Ordering::SeqCst);
                let deltas = deltas.clone();
                tokio::spawn(async move {
                    // Drain the request head; the body is not interesting.
                    let mut scratch = [0u8; 4096];
                    let _ = socket.read(&mut scratch).await;
                    let response = String::from(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n",
                    );
                    if socket.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                    for delta in deltas {
                        let frame = format!(
                            "data: {}\n\n",
                            serde_json::json!({
                                "choices": [{ "index": 0, "delta": { "content": delta } }]
                            })
                        );
                        if socket.write_all(frame.as_bytes()).await.is_err() {
                            return;
                        }
                        if !gap.is_zero() {
                            tokio::time::sleep(gap).await;
                        }
                    }
                    let last = format!(
                        "data: {}\n\ndata: [DONE]\n\n",
                        serde_json::json!({
                            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                            "usage": { "prompt_tokens": 9, "completion_tokens": 2 }
                        })
                    );
                    let _ = socket.write_all(last.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self {
            url: format!("http://{addr}/v1"),
            hits,
            _task: task,
        }
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------
// The six routes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pair_redeems_a_code_and_refresh_mints_an_access_token() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (device_id, code) = harness.device_with_code("iPhone");

    let response = harness
        .request(
            "POST",
            "/v1/pair",
            &[],
            Some(&format!(r#"{{"code":"{code}","device_name":"iPhone"}}"#)),
        )
        .await;
    assert_eq!(response.status, 201, "{}", response.body);
    let body = response.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["device_id"], device_id);
    let access = body["access_token"]
        .as_str()
        .expect("an access token")
        .to_owned();
    assert_eq!(access.len(), 64, "32 bytes as hex");
    assert!(
        access.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "the access token is hex"
    );
    let refresh = body["refresh_token"]
        .as_str()
        .expect("a refresh token")
        .to_owned();
    assert_ne!(refresh, access);
    let scopes = body["scopes"].as_array().expect("a scope list");
    assert!(scopes.contains(&serde_json::json!("chat")));
    assert!(!scopes.contains(&serde_json::json!("admin")));
    assert!(body["expires_at"].as_str().is_some_and(|at| !at.is_empty()));

    // The code is single use.
    let reused = harness
        .request(
            "POST",
            "/v1/pair",
            &[],
            Some(&format!(r#"{{"code":"{code}","device_name":"iPhone"}}"#)),
        )
        .await;
    assert_eq!(reused.status, 401, "{}", reused.body);
    assert_eq!(reused.json()["code"], "pairing_code_invalid");

    // A code that never existed is the same 401 with the same code.
    let bogus = harness
        .request(
            "POST",
            "/v1/pair",
            &[],
            Some(r#"{"code":"ZZZZZZZZ","device_name":"iPhone"}"#),
        )
        .await;
    assert_eq!(bogus.status, 401, "{}", bogus.body);
    assert_eq!(bogus.json()["code"], "pairing_code_invalid");
    assert_eq!(bogus.json()["ok"], false);

    // Refresh: the stored refresh token buys one fresh access token.
    let refreshed = harness
        .request(
            "POST",
            "/v1/token/refresh",
            &[],
            Some(&format!(r#"{{"refresh_token":"{refresh}"}}"#)),
        )
        .await;
    assert_eq!(refreshed.status, 200, "{}", refreshed.body);
    let refreshed_body = refreshed.json();
    assert_eq!(refreshed_body["ok"], true);
    let fresh = refreshed_body["access_token"]
        .as_str()
        .expect("a fresh access token");
    assert_ne!(fresh, access, "a refresh mints a new token");

    // The same refresh token still works a second time: rotation would retire
    // the caller's only way to refresh, and the documented body has no field to
    // hand back a replacement.
    let again = harness
        .request(
            "POST",
            "/v1/token/refresh",
            &[],
            Some(&format!(r#"{{"refresh_token":"{refresh}"}}"#)),
        )
        .await;
    assert_eq!(again.status, 200, "{}", again.body);

    // An access token is not a refresh token.
    let wrong_kind = harness
        .request(
            "POST",
            "/v1/token/refresh",
            &[],
            Some(&format!(r#"{{"refresh_token":"{access}"}}"#)),
        )
        .await;
    assert_eq!(wrong_kind.status, 401, "{}", wrong_kind.body);
    assert_eq!(wrong_kind.json()["code"], "token_invalid");

    harness.stop();
}

#[tokio::test]
async fn a_turn_returns_202_then_streams_frames_with_monotonic_ids() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];

    let response = harness
        .request(
            "POST",
            "/v1/turns",
            &auth,
            Some(r#"{"text":"reply with the single word: ok"}"#),
        )
        .await;
    assert_eq!(response.status, 202, "{}", response.body);
    let body = response.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["state"], "queued");
    let job_id = body["job_id"].as_str().expect("a job id").to_owned();
    let session_id = body["session_id"]
        .as_str()
        .expect("a session id")
        .to_owned();

    let mut stream = Sse::open(
        harness.addr,
        &format!("/v1/turns/{job_id}/events"),
        &token,
        None,
    )
    .await;
    let frames = stream.frames_until_end(Duration::from_secs(10)).await;
    assert!(!frames.is_empty(), "the stream must carry frames");
    let ids: Vec<i64> = frames.iter().map(|frame| frame.id).collect();
    assert_eq!(
        ids,
        (1..=ids.len() as i64).collect::<Vec<_>>(),
        "ids are 1..n"
    );
    let last = frames.last().expect("a last frame");
    assert_eq!(last.event, "done", "the last frame is the terminal one");
    assert_eq!(last.data["text"], "ok");
    assert!(
        last.data["usage"].is_object(),
        "the done frame carries usage: {}",
        last.data
    );
    let deltas: String = frames
        .iter()
        .filter(|frame| frame.event == "delta")
        .filter_map(|frame| frame.data["text"].as_str())
        .collect();
    assert_eq!(deltas, "ok", "the deltas assemble into the reply");

    // The snapshot route reports the same turn.
    let snapshot = harness
        .request("GET", &format!("/v1/sessions/{session_id}"), &auth, None)
        .await;
    assert_eq!(snapshot.status, 200, "{}", snapshot.body);
    let snapshot = snapshot.json();
    assert_eq!(snapshot["ok"], true);
    assert_eq!(snapshot["session_id"], session_id.as_str());
    let turns = snapshot["turns"].as_array().expect("a turn list");
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["job_id"], job_id.as_str());
    assert_eq!(turns[0]["state"], "succeeded");
    assert_eq!(turns[0]["text"], "ok");
    assert!(turns[0]["created_at"].as_str().is_some());
    assert!(turns[0]["finished_at"].as_str().is_some());

    harness.stop();
}

#[tokio::test]
async fn an_idempotency_key_replays_the_job_without_a_second_call() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (device_id, token) = harness.device_with_token("iPhone");
    let auth = [
        ("authorization", format!("Bearer {token}")),
        ("idempotency-key", "turn-42".to_owned()),
    ];

    let first = harness
        .request("POST", "/v1/turns", &auth, Some(r#"{"text":"ok"}"#))
        .await;
    assert_eq!(first.status, 202, "{}", first.body);
    let job_id = first.json()["job_id"].as_str().expect("job id").to_owned();

    let second = harness
        .request("POST", "/v1/turns", &auth, Some(r#"{"text":"ok"}"#))
        .await;
    assert_eq!(second.status, 202, "{}", second.body);
    assert_eq!(
        second.json()["job_id"],
        job_id.as_str(),
        "same key, same job"
    );

    let rows = harness
        .engine
        .db()
        .with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM jobs WHERE device_id = ?1",
                [&device_id],
                |row| row.get::<_, i64>(0),
            )
        })
        .expect("job count");
    assert_eq!(rows, 1, "no second turn row");

    // The provider was called once, and only after the worker claimed the job.
    let mut stream = Sse::open(
        harness.addr,
        &format!("/v1/turns/{job_id}/events"),
        &token,
        None,
    )
    .await;
    let _ = stream.frames_until_end(Duration::from_secs(10)).await;
    assert_eq!(provider.hits(), 1, "no second provider call");

    harness.stop();
}

#[tokio::test]
async fn last_event_id_resumes_with_only_the_missing_frames() {
    let provider =
        StubProvider::start(&["a", "b", "c", "d", "e", "f"], Duration::from_millis(120)).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];

    let response = harness
        .request("POST", "/v1/turns", &auth, Some(r#"{"text":"ok"}"#))
        .await;
    assert_eq!(response.status, 202, "{}", response.body);
    let job_id = response.json()["job_id"]
        .as_str()
        .expect("job id")
        .to_owned();
    let path = format!("/v1/turns/{job_id}/events");

    // Read one frame, then drop the connection mid-turn.
    let mut first = Sse::open(harness.addr, &path, &token, None).await;
    let head = first
        .next_frame(Duration::from_secs(5))
        .await
        .expect("the first frame arrives");
    assert_eq!(head.id, 1);
    assert_eq!(head.event, "delta");
    drop(first);

    // Let a few more frames land while nobody is listening.
    tokio::time::sleep(Duration::from_millis(450)).await;

    // Reconnect strictly after frame 1 and read the rest, including the terminal.
    let mut resumed = Sse::open(harness.addr, &path, &token, Some(1)).await;
    let frames = resumed.frames_until_end(Duration::from_secs(10)).await;
    assert!(!frames.is_empty(), "the resume must deliver frames");
    assert!(
        frames.iter().all(|frame| frame.id > 1),
        "a resume replays strictly after the id it was given: {:?}",
        frames.iter().map(|frame| frame.id).collect::<Vec<_>>()
    );
    let ids: Vec<i64> = frames.iter().map(|frame| frame.id).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "frames arrive in id order");
    let last = frames.last().expect("a terminal frame");
    assert_eq!(last.event, "done");

    // The replayed deltas plus the first frame are the whole reply.
    let mut text = head.data["text"].as_str().unwrap_or("").to_owned();
    for frame in &frames {
        if frame.event == "delta" {
            text.push_str(frame.data["text"].as_str().unwrap_or(""));
        }
    }
    assert_eq!(text, "abcdef", "no delta was lost across the reconnect");

    harness.stop();
}

#[tokio::test]
async fn a_resume_past_the_retention_window_is_a_409_buffer_miss() {
    let provider =
        StubProvider::start(&["a", "b", "c", "d", "e", "f", "g", "h"], Duration::ZERO).await;
    // Two retained events per job is enough to prove the window.
    let harness = Harness::start(&provider.url, 2).await;
    let (_, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];

    let response = harness
        .request("POST", "/v1/turns", &auth, Some(r#"{"text":"ok"}"#))
        .await;
    assert_eq!(response.status, 202, "{}", response.body);
    let body = response.json();
    let job_id = body["job_id"].as_str().expect("job id").to_owned();
    let session_id = body["session_id"].as_str().expect("session id").to_owned();
    let path = format!("/v1/turns/{job_id}/events");

    // Drain the stream so the turn is certainly finished and pruned. A live
    // subscriber sees every frame as it happens; retention is about what can be
    // replayed afterwards, which is what the two requests below prove.
    let mut stream = Sse::open(harness.addr, &path, &token, None).await;
    let live = stream.frames_until_end(Duration::from_secs(10)).await;
    assert_eq!(live.last().expect("a terminal frame").event, "done");
    assert_eq!(live.len(), 9, "eight deltas and a done frame");

    // Asking for everything from the start is now impossible, and the answer says
    // so rather than quietly streaming a suffix.
    let missed = harness
        .request(
            "GET",
            &path,
            &[
                ("authorization", format!("Bearer {token}")),
                ("last-event-id", "1".to_owned()),
            ],
            None,
        )
        .await;
    assert_eq!(missed.status, 409, "{}", missed.body);
    let missed_body = missed.json();
    assert_eq!(missed_body["ok"], false);
    assert_eq!(missed_body["code"], "buffer_miss");
    assert_eq!(
        missed_body["snapshot_url"],
        format!("/v1/sessions/{session_id}").as_str()
    );

    // The snapshot route is the documented recovery path.
    let snapshot = harness
        .request("GET", &format!("/v1/sessions/{session_id}"), &auth, None)
        .await;
    assert_eq!(snapshot.status, 200, "{}", snapshot.body);
    let snapshot = snapshot.json();
    assert_eq!(snapshot["turns"][0]["text"], "abcdefgh");

    harness.stop();
}

#[tokio::test]
async fn cancel_stops_a_running_turn_and_a_second_cancel_is_409() {
    let provider = StubProvider::start(
        &["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"],
        Duration::from_millis(150),
    )
    .await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];

    let response = harness
        .request("POST", "/v1/turns", &auth, Some(r#"{"text":"ok"}"#))
        .await;
    assert_eq!(response.status, 202, "{}", response.body);
    let job_id = response.json()["job_id"]
        .as_str()
        .expect("job id")
        .to_owned();

    // Hold the job open just long enough for the worker to be running.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let cancelled = harness
        .request("POST", &format!("/v1/turns/{job_id}/cancel"), &auth, None)
        .await;
    assert_eq!(cancelled.status, 200, "{}", cancelled.body);
    assert_eq!(cancelled.json()["state"], "cancelled");

    // The stream now ends with the cancellation, not with a done.
    let mut stream = Sse::open(
        harness.addr,
        &format!("/v1/turns/{job_id}/events"),
        &token,
        None,
    )
    .await;
    let frames = stream.frames_until_end(Duration::from_secs(10)).await;
    let last = frames.last().expect("the cancelled frame");
    assert_eq!(last.event, "cancelled");
    assert_eq!(last.data["reason"], "cancel");

    // A second cancel is a refusal, not a second cancelled frame.
    let again = harness
        .request("POST", &format!("/v1/turns/{job_id}/cancel"), &auth, None)
        .await;
    assert_eq!(again.status, 409, "{}", again.body);
    assert_eq!(again.json()["code"], "turn_not_cancellable");

    harness.stop();
}

#[tokio::test]
async fn the_admin_plane_is_404_on_the_device_listener() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_device, _token) = harness.device_with_token("iPhone");

    for path in [
        "/setup",
        "/setup/state",
        "/dashboard",
        "/healthz",
        "/static/app.css",
        "/",
        "/v2/turns",
    ] {
        let response = harness.request("GET", path, &[], None).await;
        assert_eq!(response.status, 404, "{path} must not exist here");
        assert!(
            response
                .headers
                .to_ascii_lowercase()
                .contains("x-request-id"),
            "{path} carries a request id too: {}",
            response.headers
        );
        let body = response.json();
        assert_eq!(body["ok"], false, "{path}");
        assert_eq!(body["code"], "not_found", "{path}");
    }

    // A POST to an admin path is a 404 too, not a 405: the route does not exist.
    let posted = harness
        .request("POST", "/setup/claim", &[], Some(r#"{"passphrase":"x"}"#))
        .await;
    assert_eq!(posted.status, 404, "{}", posted.body);
    assert_eq!(posted.json()["code"], "not_found");

    // A device route with a bearer still requires the bearer on the session
    // routes: the surface is authenticated even where the path exists.
    let anonymous = harness
        .request("GET", "/v1/sessions/whatever", &[], None)
        .await;
    assert_eq!(anonymous.status, 401, "{}", anonymous.body);
    assert_eq!(anonymous.json()["code"], "token_invalid");

    harness.stop();
}

#[tokio::test]
async fn the_not_ready_gate_and_the_missing_bearer_are_both_typed() {
    let harness = Harness::start_not_ready().await;
    let (_, token) = harness.device_with_token("iPhone");

    let not_ready = harness
        .request(
            "POST",
            "/v1/turns",
            &[("authorization", format!("Bearer {token}"))],
            Some(r#"{"text":"ok"}"#),
        )
        .await;
    assert_eq!(not_ready.status, 409, "{}", not_ready.body);
    assert_eq!(not_ready.json()["code"], "not_ready");

    let no_token = harness
        .request("POST", "/v1/turns", &[], Some(r#"{"text":"ok"}"#))
        .await;
    assert_eq!(no_token.status, 401, "{}", no_token.body);
    assert_eq!(no_token.json()["code"], "token_invalid");

    let bad_token = harness
        .request(
            "POST",
            "/v1/turns",
            &[("authorization", "Bearer not-a-token".to_owned())],
            Some(r#"{"text":"ok"}"#),
        )
        .await;
    assert_eq!(bad_token.status, 401, "{}", bad_token.body);
    assert_eq!(bad_token.json()["code"], "token_invalid");

    harness.stop();
}

#[tokio::test]
async fn a_foreign_job_is_404_and_the_port_is_loopback_only() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, mine) = harness.device_with_token("iPhone");
    let (_, theirs) = harness.device_with_token("iPad");

    let response = harness
        .request(
            "POST",
            "/v1/turns",
            &[("authorization", format!("Bearer {theirs}"))],
            Some(r#"{"text":"ok"}"#),
        )
        .await;
    assert_eq!(response.status, 202, "{}", response.body);
    let job_id = response.json()["job_id"]
        .as_str()
        .expect("job id")
        .to_owned();

    // The other device's job is not readable, cancellable, or resumable.
    for (method, path) in [
        ("GET", format!("/v1/turns/{job_id}/events")),
        ("POST", format!("/v1/turns/{job_id}/cancel")),
    ] {
        let foreign = harness
            .request(
                method,
                &path,
                &[("authorization", format!("Bearer {mine}"))],
                None,
            )
            .await;
        assert_eq!(foreign.status, 404, "{method} {path}: {}", foreign.body);
        assert_eq!(foreign.json()["code"], "job_not_found");
    }

    // Address and retention are literals, not settings.
    assert_eq!(DEVICE_ADDR, "127.0.0.1:7341");
    assert_eq!(TurnStore::buffer_from_env(), DEFAULT_SSE_BUFFER);
    assert!(harness.addr.ip().is_loopback());

    harness.stop();
}

// ---------------------------------------------------------------------------
// A provider stub that scripts whole rounds
// ---------------------------------------------------------------------------

/// A provider stub that answers each request with the next scripted round.
///
/// `StubProvider` above streams one fixed list of text deltas; this one exists
/// because a reasoning frame, a tool call and a second round are all *sequences of
/// different frames*, and a test that proved them would otherwise have to hard
/// code a provider dialect twice. Each round is the SSE payloads to send, in
/// order; the last round repeats if the turn asks again.
struct ScriptedProvider {
    url: String,
    hits: Arc<AtomicUsize>,
    _task: tokio::task::JoinHandle<()>,
}

impl ScriptedProvider {
    async fn start(rounds: Vec<Vec<serde_json::Value>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a stub provider listener");
        let addr = listener.local_addr().expect("a stub provider address");
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let served = counter.fetch_add(1, Ordering::SeqCst);
                let frames = rounds
                    .get(served)
                    .or_else(|| rounds.last())
                    .cloned()
                    .unwrap_or_default();
                tokio::spawn(async move {
                    let mut scratch = [0u8; 8192];
                    let _ = socket.read(&mut scratch).await;
                    let head = String::from(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n",
                    );
                    if socket.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    for frame in frames {
                        let body = format!("data: {frame}\n\n");
                        if socket.write_all(body.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                    let _ = socket.write_all(b"data: [DONE]\n\n").await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self {
            url: format!("http://{addr}/v1"),
            hits,
            _task: task,
        }
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

/// One chunk carrying a text delta.
fn text_chunk(text: &str) -> serde_json::Value {
    serde_json::json!({
        "choices": [{ "index": 0, "delta": { "content": text } }]
    })
}

/// One chunk carrying a reasoning fragment.
fn reasoning_chunk(text: &str) -> serde_json::Value {
    serde_json::json!({
        "choices": [{ "index": 0, "delta": { "reasoning_content": text } }]
    })
}

/// The chunk that ends a round, with the usage every round reports.
fn closing_chunk(reason: &str) -> serde_json::Value {
    serde_json::json!({
        "choices": [{ "index": 0, "delta": {}, "finish_reason": reason }],
        "usage": { "prompt_tokens": 9, "completion_tokens": 2 }
    })
}

// ---------------------------------------------------------------------------
// What a device turn carries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_turn_that_reasons_streams_reasoning_frames() {
    let provider = ScriptedProvider::start(vec![vec![
        reasoning_chunk("weighing it"),
        reasoning_chunk(" then answering"),
        text_chunk("ok"),
        closing_chunk("stop"),
    ]])
    .await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];

    let response = harness
        .request(
            "POST",
            "/v1/turns",
            &auth,
            Some(r#"{"text":"think first"}"#),
        )
        .await;
    assert_eq!(response.status, 202, "{}", response.body);
    let job_id = response.json()["job_id"]
        .as_str()
        .expect("a job id")
        .to_owned();

    let mut stream = Sse::open(
        harness.addr,
        &format!("/v1/turns/{job_id}/events"),
        &token,
        None,
    )
    .await;
    let frames = stream.frames_until_end(Duration::from_secs(10)).await;

    let reasoning: String = frames
        .iter()
        .filter(|frame| frame.event == "reasoning")
        .filter_map(|frame| frame.data["text"].as_str())
        .collect();
    assert_eq!(
        reasoning, "weighing it then answering",
        "every reasoning fragment is its own frame, in order: {frames:?}"
    );
    assert!(
        frames.iter().any(|frame| frame.event == "reasoning"),
        "the stream names the frame `reasoning`, the name the browser uses"
    );
    // The reply is still the reply: reasoning is never mixed into the text.
    let deltas: String = frames
        .iter()
        .filter(|frame| frame.event == "delta")
        .filter_map(|frame| frame.data["text"].as_str())
        .collect();
    assert_eq!(deltas, "ok");
    let last = frames.last().expect("a terminal frame");
    assert_eq!(last.event, "done");
    assert_eq!(last.data["text"], "ok");

    // A reasoning frame is not terminal, and the id sequence has no gaps.
    let ids: Vec<i64> = frames.iter().map(|frame| frame.id).collect();
    assert_eq!(ids, (1..=ids.len() as i64).collect::<Vec<_>>());
    assert!(
        frames
            .iter()
            .take(frames.len() - 1)
            .all(|frame| frame.event != "done"),
        "nothing terminates the stream early: {frames:?}"
    );

    harness.stop();
}

#[tokio::test]
async fn a_turn_that_calls_a_tool_streams_the_frame_the_browser_sends() {
    // Round one asks for a tool; round two answers with what it found. A
    // `list_dir` call is not a write, so no approval gate stands in its way.
    let provider = ScriptedProvider::start(vec![
        vec![
            serde_json::json!({
                "choices": [{
                    "index": 0,
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "id": "call_1",
                            "function": { "name": "list_dir", "arguments": "{\"path\":\".\"}" }
                        }]
                    }
                }]
            }),
            closing_chunk("tool_calls"),
        ],
        vec![text_chunk("one directory"), closing_chunk("stop")],
    ])
    .await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    // The turn works in a real directory, so the call actually runs.
    harness
        .engine
        .set_setting(
            eris_host::onboarding::SETTING_WORKSPACE,
            &harness._dir.path().display().to_string(),
        )
        .expect("a workspace setting");
    let (_, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];

    let response = harness
        .request(
            "POST",
            "/v1/turns",
            &auth,
            Some(r#"{"text":"what is here"}"#),
        )
        .await;
    assert_eq!(response.status, 202, "{}", response.body);
    let job_id = response.json()["job_id"]
        .as_str()
        .expect("a job id")
        .to_owned();

    let mut stream = Sse::open(
        harness.addr,
        &format!("/v1/turns/{job_id}/events"),
        &token,
        None,
    )
    .await;
    let frames = stream.frames_until_end(Duration::from_secs(20)).await;
    assert_eq!(provider.hits(), 2, "a tool round is a second provider call");

    let tool = frames
        .iter()
        .find(|frame| frame.event == "tool")
        .unwrap_or_else(|| panic!("a tool frame is in the stream: {frames:?}"));
    // Exactly the four fields the browser sends, and no invented fifth one.
    let data = tool.data.as_object().expect("the tool frame is an object");
    let mut keys: Vec<&str> = data.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["arguments", "is_error", "name", "output"],
        "the frame carries the browser's four fields: {data:?}"
    );
    assert_eq!(data["name"], "list_dir");
    assert_eq!(data["arguments"], r#"{"path":"."}"#);
    assert_eq!(data["is_error"], false, "the call ran: {data:?}");
    let output = data["output"].as_str().expect("an output string");
    assert!(
        output.contains("entries"),
        "the output is the tool's own result: {output}"
    );

    // The tool frame precedes the answer it produced, and the answer still lands.
    let tool_at = frames
        .iter()
        .position(|frame| frame.event == "tool")
        .expect("a tool position");
    let last = frames.last().expect("a terminal frame");
    assert_eq!(last.event, "done");
    assert_eq!(last.data["text"], "one directory");
    assert!(
        frames
            .iter()
            .skip(tool_at)
            .any(|frame| frame.event == "delta"),
        "the second round's text streams after the call: {frames:?}"
    );

    harness.stop();
}

// ---------------------------------------------------------------------------
// Slash commands over /v1
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_help_command_answers_without_a_provider_or_a_billed_token() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    // Every `provider.*` row goes, mirroring the browser's own command test: a
    // command is not a message to a model, so a daemon with no endpoint answers.
    harness
        .engine
        .db()
        .with_conn(|conn| {
            conn.execute("DELETE FROM settings WHERE key LIKE 'provider.%'", [])?;
            Ok(())
        })
        .expect("the provider rows are deleted");
    let (device_id, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];

    let response = harness
        .request("POST", "/v1/turns", &auth, Some(r#"{"text":"/help"}"#))
        .await;
    assert_eq!(response.status, 202, "{}", response.body);
    let body = response.json();
    assert_eq!(body["ok"], true);
    assert_eq!(
        body["state"], "succeeded",
        "a command settles before the 202 is answered"
    );
    let job_id = body["job_id"].as_str().expect("a job id").to_owned();

    let mut stream = Sse::open(
        harness.addr,
        &format!("/v1/turns/{job_id}/events"),
        &token,
        None,
    )
    .await;
    let frames = stream.frames_until_end(Duration::from_secs(10)).await;
    let last = frames.last().expect("a terminal frame");
    assert_eq!(last.event, "done", "the answer is a normal turn result");
    let text = last.data["text"].as_str().expect("the answer text");
    assert!(
        text.contains("/help"),
        "the answer is the help text: {text}"
    );
    assert!(
        text.contains("/stop"),
        "it lists the commands this daemon implements: {text}"
    );
    assert!(
        frames.iter().all(|frame| frame.event == "done"),
        "one frame, and it is the answer: {frames:?}"
    );

    // Nothing was billed and no model was asked: the usage numbers are absent
    // rather than a confident zero, which is the same distinction the browser's
    // null token columns make.
    let usage = &last.data["usage"];
    assert!(
        usage["input"]["tokens"].is_null() && usage["output"]["tokens"].is_null(),
        "no token count was reported: {usage}"
    );
    assert_eq!(
        usage["input"]["state"], "provider_unavailable",
        "unreported, not zero: {usage}"
    );
    assert_eq!(provider.hits(), 0, "no provider call");

    // The device transcript is never written: this turn has no conversation row
    // and no question row, which is the by-construction proof it cost nothing.
    let (messages, user_rows) = harness
        .engine
        .db()
        .with_conn(|conn| {
            Ok((
                conn.query_row("SELECT COUNT(*) FROM chat_messages", [], |row| {
                    row.get::<_, i64>(0)
                })?,
                conn.query_row(
                    "SELECT COUNT(*) FROM chat_messages WHERE role = 'user'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?,
            ))
        })
        .expect("the transcript counts");
    assert_eq!(messages, 0, "a command writes no transcript row");
    assert_eq!(user_rows, 0, "and no user row");

    // The command belongs to the device that asked, and the snapshot reports it.
    let row = harness
        .engine
        .db()
        .with_conn(|conn| {
            conn.query_row(
                "SELECT device_id, request_text FROM jobs WHERE id = ?1",
                [&job_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
        })
        .expect("the job row");
    assert_eq!(row.0, device_id);
    assert_eq!(row.1, "/help");

    harness.stop();
}

#[tokio::test]
async fn an_unknown_command_is_refused_by_name_and_costs_nothing() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (device_id, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];

    for name in ["goal", "restart", "doesnotexist"] {
        let response = harness
            .request(
                "POST",
                "/v1/turns",
                &auth,
                Some(&format!(r#"{{"text":"/{name}"}}"#)),
            )
            .await;
        assert_eq!(response.status, 400, "{}", response.body);
        let body = response.json();
        assert_eq!(body["ok"], false);
        assert_eq!(body["code"], "command_unsupported");
        let message = body["message"].as_str().expect("a message");
        assert!(
            message.contains(&format!("/{name}")),
            "the refusal names it: {message}"
        );
        assert!(
            message.contains("not supported"),
            "the refusal says so: {message}"
        );
        assert!(
            message.contains("nothing was sent to a model"),
            "the refusal says no model was asked: {message}"
        );
    }

    // A refused command queues no turn at all.
    let jobs = harness
        .engine
        .db()
        .with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM jobs WHERE device_id = ?1",
                [&device_id],
                |row| row.get::<_, i64>(0),
            )
        })
        .expect("the job count");
    assert_eq!(jobs, 0, "a refused command is not a turn");
    assert_eq!(provider.hits(), 0, "no provider call");

    harness.stop();
}

#[tokio::test]
async fn a_stop_command_cancels_the_devices_live_turn_without_a_model() {
    // A provider that holds the turn open long enough to still be running.
    let provider = StubProvider::start(&["a", "b", "c"], Duration::from_millis(400)).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];

    let started = harness
        .request("POST", "/v1/turns", &auth, Some(r#"{"text":"go"}"#))
        .await;
    assert_eq!(started.status, 202, "{}", started.body);
    let running = started.json()["job_id"]
        .as_str()
        .expect("a job id")
        .to_owned();

    let stopped = harness
        .request("POST", "/v1/turns", &auth, Some(r#"{"text":"/stop"}"#))
        .await;
    assert_eq!(stopped.status, 202, "{}", stopped.body);
    let job_id = stopped.json()["job_id"]
        .as_str()
        .expect("a command job id")
        .to_owned();

    let mut stream = Sse::open(
        harness.addr,
        &format!("/v1/turns/{job_id}/events"),
        &token,
        None,
    )
    .await;
    let frames = stream.frames_until_end(Duration::from_secs(10)).await;
    let text = frames
        .last()
        .and_then(|frame| frame.data["text"].as_str())
        .unwrap_or_default();
    assert!(
        text.contains("Stopped"),
        "the answer says what it stopped: {text}"
    );

    // The running turn is cancelled, not left to finish: its own stream ends with
    // a cancelled frame.
    let mut running_stream = Sse::open(
        harness.addr,
        &format!("/v1/turns/{running}/events"),
        &token,
        None,
    )
    .await;
    let running_frames = running_stream
        .frames_until_end(Duration::from_secs(10))
        .await;
    assert!(
        running_frames
            .iter()
            .any(|frame| frame.event == "cancelled"),
        "the stopped turn ends cancelled: {running_frames:?}"
    );

    // And a second /stop says there was nothing left to stop, rather than
    // pretending to have stopped something.
    let again = harness
        .request("POST", "/v1/turns", &auth, Some(r#"{"text":"/stop"}"#))
        .await;
    assert_eq!(again.status, 202, "{}", again.body);
    let second = again.json()["job_id"]
        .as_str()
        .expect("a command job id")
        .to_owned();
    let mut second_stream = Sse::open(
        harness.addr,
        &format!("/v1/turns/{second}/events"),
        &token,
        None,
    )
    .await;
    let second_frames = second_stream
        .frames_until_end(Duration::from_secs(10))
        .await;
    let second_text = second_frames
        .last()
        .and_then(|frame| frame.data["text"].as_str())
        .unwrap_or_default();
    assert!(
        second_text.contains("Nothing was running"),
        "the second stop is honest: {second_text}"
    );

    harness.stop();
}

#[tokio::test]
async fn a_command_that_needs_a_transcript_or_settings_is_refused_by_name() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];

    for name in ["new", "clear", "model", "memory"] {
        let response = harness
            .request(
                "POST",
                "/v1/turns",
                &auth,
                Some(&format!(r#"{{"text":"/{name}"}}"#)),
            )
            .await;
        assert_eq!(response.status, 400, "{}", response.body);
        let body = response.json();
        assert_eq!(body["code"], "command_not_available");
        let message = body["message"].as_str().expect("a message");
        assert!(message.contains(&format!("/{name}")), "{message}");
        assert!(
            message.contains("nothing was sent to a model"),
            "it says no model was asked: {message}"
        );
    }
    assert_eq!(provider.hits(), 0);

    harness.stop();
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

/// The token a device holds, minus one scope.
fn token_without(harness: &Harness, name: &str, scope: eris_host::auth::Scope) -> String {
    let db = harness.engine.db().clone();
    let device_id = AuthStore::new(db.clone())
        .register_device(name, "ios")
        .expect("device row");
    TokenService::new(db)
        .mint_access(
            &device_id,
            ScopeSet::device_default().without(scope),
            TokenOrigin::Device,
        )
        .expect("access token")
        .token()
        .to_owned()
}

#[tokio::test]
async fn projects_require_a_bearer_and_the_sessions_scope() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, token) = harness.device_with_token("iPhone");
    let scoped = token_without(&harness, "iPad", eris_host::auth::Scope::Sessions);

    // No bearer: 401 on all three, the same code every other route uses.
    for (method, path) in [
        ("GET", "/v1/projects"),
        ("POST", "/v1/projects"),
        ("POST", "/v1/projects/assign"),
    ] {
        let anonymous = harness.request(method, path, &[], None).await;
        assert_eq!(anonymous.status, 401, "{method} {path}: {}", anonymous.body);
        assert_eq!(anonymous.json()["code"], "token_invalid");
    }

    // A real token without `sessions`: 403, because the token is real and the
    // scope is simply not on it.
    for (method, path) in [
        ("GET", "/v1/projects"),
        ("POST", "/v1/projects"),
        ("POST", "/v1/projects/assign"),
    ] {
        let denied = harness
            .request(
                method,
                path,
                &[("authorization", format!("Bearer {scoped}"))],
                Some(r#"{"project_name":"Work","conversation":"c"}"#),
            )
            .await;
        assert_eq!(denied.status, 403, "{method} {path}: {}", denied.body);
        assert_eq!(denied.json()["code"], "scope_denied");
    }

    // The device's own token still lists, so the refusal above is about scope and
    // not about the route.
    let listed = harness
        .request(
            "GET",
            "/v1/projects",
            &[("authorization", format!("Bearer {token}"))],
            None,
        )
        .await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    assert_eq!(listed.json()["projects"], serde_json::json!([]));

    harness.stop();
}

#[tokio::test]
async fn projects_create_list_and_assign_round_trip() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (device_id, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];

    let created = harness
        .request(
            "POST",
            "/v1/projects",
            &auth,
            Some(r#"{"project_name":"Isaacs contract renewal"}"#),
        )
        .await;
    assert_eq!(created.status, 201, "{}", created.body);
    let body = created.json();
    assert_eq!(body["ok"], true);
    let project = body["project"].clone();
    let project_id = project["id"].as_str().expect("a project id").to_owned();
    assert_eq!(project["name"], "Isaacs contract renewal");
    assert!(
        project["created_at"]
            .as_str()
            .is_some_and(|at| !at.is_empty()),
        "the row carries its stamp: {project}"
    );

    let listed = harness.request("GET", "/v1/projects", &auth, None).await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    let projects = listed.json()["projects"].clone();
    assert_eq!(
        projects,
        serde_json::json!([{
            "id": project_id,
            "name": "Isaacs contract renewal",
            "created_at": project["created_at"],
        }]),
        "the list is the row that was created"
    );

    // A conversation to file. Filing is a column update on a conversation row, so
    // one has to exist first.
    let conversation =
        eris_host::web::chat::create_conversation(harness.engine.db()).expect("a conversation row");

    let assigned = harness
        .request(
            "POST",
            "/v1/projects/assign",
            &auth,
            Some(&format!(
                r#"{{"conversation":"{conversation}","project":"{project_id}"}}"#
            )),
        )
        .await;
    assert_eq!(assigned.status, 200, "{}", assigned.body);
    let body = assigned.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["conversation"], conversation.as_str());
    assert_eq!(body["project"], project_id.as_str());

    let filed =
        eris_host::web::chat::conversations(harness.engine.db(), 10).expect("the rail list reads");
    assert_eq!(filed.len(), 1);
    assert_eq!(filed[0].project_id.as_deref(), Some(project_id.as_str()));

    // An empty project is the "no project" entry, which is a clear rather than a
    // missing field.
    let cleared = harness
        .request(
            "POST",
            "/v1/projects/assign",
            &auth,
            Some(&format!(
                r#"{{"conversation":"{conversation}","project":""}}"#
            )),
        )
        .await;
    assert_eq!(cleared.status, 200, "{}", cleared.body);
    assert_eq!(cleared.json()["project"], serde_json::Value::Null);
    let unfiled =
        eris_host::web::chat::conversations(harness.engine.db(), 10).expect("the rail list reads");
    assert_eq!(unfiled[0].project_id, None, "it is ungrouped again");

    // A conversation or project the daemon does not know is refused, not reported
    // as a success: a client that reads `ok` has to be able to believe it.
    let refused = harness
        .request(
            "POST",
            "/v1/projects/assign",
            &auth,
            Some(r#"{"conversation":"no-such","project":"no-such"}"#),
        )
        .await;
    assert_eq!(refused.status, 400, "{}", refused.body);
    assert_eq!(refused.json()["code"], "project_assign_refused");
    let missing = harness
        .request(
            "POST",
            "/v1/projects/assign",
            &auth,
            Some(r#"{"project":""}"#),
        )
        .await;
    assert_eq!(missing.status, 400, "{}", missing.body);
    assert_eq!(missing.json()["code"], "conversation_missing");

    // Both writes are audited, and the actor is the device rather than loopback.
    let audited: Vec<(String, String)> = harness
        .engine
        .db()
        .with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT actor, action FROM audit_log \
                  WHERE action IN ('projects.created', 'projects.assigned') ORDER BY id",
            )?;
            let mut rows = statement.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push((row.get(0)?, row.get(1)?));
            }
            Ok(out)
        })
        .expect("the audit rows");
    assert_eq!(
        audited,
        vec![
            (device_id.clone(), "projects.created".to_owned()),
            (device_id.clone(), "projects.assigned".to_owned()),
            (device_id, "projects.assigned".to_owned()),
        ],
        "one create and two assignments, every one from the device"
    );

    harness.stop();
}

#[tokio::test]
async fn a_duplicate_project_name_is_refused_by_code() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];

    let first = harness
        .request(
            "POST",
            "/v1/projects",
            &auth,
            Some(r#"{"project_name":"Work"}"#),
        )
        .await;
    assert_eq!(first.status, 201, "{}", first.body);

    // Case is not a way around it, the same rule the dashboard applies.
    for name in ["Work", "work", "  WORK  "] {
        let duplicate = harness
            .request(
                "POST",
                "/v1/projects",
                &auth,
                Some(&format!(r#"{{"project_name":"{name}"}}"#)),
            )
            .await;
        assert_eq!(duplicate.status, 400, "{}", duplicate.body);
        let body = duplicate.json();
        assert_eq!(body["code"], "project_name_duplicate");
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|message| message.contains("already exists")),
            "the refusal says why: {}",
            body["message"]
        );
    }

    // A blank name is its own refusal, with its own code.
    let blank = harness
        .request(
            "POST",
            "/v1/projects",
            &auth,
            Some(r#"{"project_name":"   "}"#),
        )
        .await;
    assert_eq!(blank.status, 400, "{}", blank.body);
    assert_eq!(blank.json()["code"], "project_name_invalid");

    let projects = harness.request("GET", "/v1/projects", &auth, None).await;
    assert_eq!(
        projects.json()["projects"]
            .as_array()
            .expect("a list")
            .len(),
        1,
        "the refusals added no rows"
    );

    harness.stop();
}

// ---------------------------------------------------------------------------
// Conversations
// ---------------------------------------------------------------------------

/// A conversation row created directly, for a route that only needs an id.
fn a_conversation(harness: &Harness) -> String {
    eris_host::web::chat::create_conversation(harness.engine.db()).expect("a conversation row")
}

#[tokio::test]
async fn conversations_require_a_bearer_and_the_sessions_scope() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, token) = harness.device_with_token("iPhone");
    let scoped = token_without(&harness, "iPad", eris_host::auth::Scope::Sessions);
    let conversation = a_conversation(&harness);
    let one = format!("/v1/conversations/{conversation}");
    let clear = format!("/v1/conversations/{conversation}/clear");
    let routes = [
        ("GET", "/v1/conversations".to_owned()),
        ("POST", "/v1/conversations".to_owned()),
        ("GET", one.clone()),
        ("POST", clear.clone()),
    ];

    // No bearer: 401 on every one, with the code the rest of the surface uses.
    for (method, path) in &routes {
        let anonymous = harness.request(method, path, &[], None).await;
        assert_eq!(anonymous.status, 401, "{method} {path}: {}", anonymous.body);
        assert_eq!(anonymous.json()["code"], "token_invalid");
    }

    // A real token without `sessions`: 403, because the token is real and the
    // scope is simply not on it.
    for (method, path) in &routes {
        let denied = harness
            .request(
                method,
                path,
                &[("authorization", format!("Bearer {scoped}"))],
                None,
            )
            .await;
        assert_eq!(denied.status, 403, "{method} {path}: {}", denied.body);
        assert_eq!(denied.json()["code"], "scope_denied");
    }

    // The device's own token still answers, so the refusal above is about scope
    // and not about the route being unreachable.
    let listed = harness
        .request(
            "GET",
            "/v1/conversations",
            &[("authorization", format!("Bearer {token}"))],
            None,
        )
        .await;
    assert_eq!(listed.status, 200, "{}", listed.body);

    harness.stop();
}

#[tokio::test]
async fn conversations_create_list_and_fetch_round_trip() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (device_id, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];

    let created = harness
        .request("POST", "/v1/conversations", &auth, None)
        .await;
    assert_eq!(created.status, 201, "{}", created.body);
    let body = created.json();
    assert_eq!(body["ok"], true);
    assert_eq!(
        body["title"],
        eris_host::web::chat::NEW_CONVERSATION_TITLE,
        "a new conversation carries the existing placeholder title"
    );
    let conversation = body["id"].as_str().expect("a conversation id").to_owned();

    let listed = harness
        .request("GET", "/v1/conversations", &auth, None)
        .await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    let body = listed.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["count"], 1);
    assert_eq!(body["truncated"], false);
    let items = body["conversations"]
        .as_array()
        .expect("a conversation list");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], conversation.as_str());
    assert_eq!(items[0]["title"], body["conversations"][0]["title"]);
    assert_eq!(items[0]["message_count"], 0);
    assert!(
        items[0]["updated_at"]
            .as_str()
            .is_some_and(|at| !at.is_empty()),
        "the row carries its stamp: {}",
        items[0]
    );
    assert_eq!(items[0]["project_id"], serde_json::Value::Null);

    let fetched = harness
        .request(
            "GET",
            &format!("/v1/conversations/{conversation}"),
            &auth,
            None,
        )
        .await;
    assert_eq!(fetched.status, 200, "{}", fetched.body);
    let body = fetched.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["id"], conversation.as_str());
    assert_eq!(body["title"], eris_host::web::chat::NEW_CONVERSATION_TITLE);
    assert_eq!(body["truncated"], false);
    assert_eq!(
        body["messages"],
        serde_json::json!([]),
        "a fresh conversation has no messages"
    );

    // The write is audited with the device as the actor, not `loopback`.
    let audited: Vec<(String, String)> = harness
        .engine
        .db()
        .with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT actor, action FROM audit_log WHERE action = 'conversation.created'",
            )?;
            let mut rows = statement.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push((row.get(0)?, row.get(1)?));
            }
            Ok(out)
        })
        .expect("the audit rows");
    assert_eq!(
        audited,
        vec![(device_id, "conversation.created".to_owned())]
    );

    harness.stop();
}

#[tokio::test]
async fn clearing_a_conversation_empties_its_transcript() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (device_id, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];
    let conversation = a_conversation(&harness);
    let db = harness.engine.db();

    let role = |user: bool| {
        if user {
            eris_host::web::chat::Role::User
        } else {
            eris_host::web::chat::Role::Assistant
        }
    };
    eris_host::web::chat::append(
        db,
        &conversation,
        role(true),
        "what is in the workspace",
        eris_host::web::chat::State::Complete,
    )
    .expect("a question");
    eris_host::web::chat::append(
        db,
        &conversation,
        role(false),
        "two files",
        eris_host::web::chat::State::Complete,
    )
    .expect("an answer");

    let path = format!("/v1/conversations/{conversation}");
    let before = harness.request("GET", &path, &auth, None).await;
    assert_eq!(before.status, 200, "{}", before.body);
    assert_eq!(
        before.json()["messages"]
            .as_array()
            .expect("a message list")
            .len(),
        2
    );

    let cleared = harness
        .request("POST", &format!("{path}/clear"), &auth, None)
        .await;
    assert_eq!(cleared.status, 200, "{}", cleared.body);
    let body = cleared.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["cleared"], 2, "both rows went");

    let after = harness.request("GET", &path, &auth, None).await;
    assert_eq!(after.status, 200, "{}", after.body);
    let body = after.json();
    assert_eq!(
        body["messages"],
        serde_json::json!([]),
        "the transcript is empty"
    );
    assert_eq!(body["truncated"], false);

    // Clearing keeps the conversation itself: the row is still listable.
    let listed = harness
        .request("GET", "/v1/conversations", &auth, None)
        .await;
    let items = listed.json()["conversations"].clone();
    assert_eq!(items.as_array().expect("a list").len(), 1);
    assert_eq!(items[0]["message_count"], 0);

    // A second clear has nothing left to remove and says so rather than failing.
    let again = harness
        .request("POST", &format!("{path}/clear"), &auth, None)
        .await;
    assert_eq!(again.status, 200, "{}", again.body);
    assert_eq!(again.json()["cleared"], 0);

    let audited: Vec<(String, String)> = harness
        .engine
        .db()
        .with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT actor, action FROM audit_log WHERE action = 'conversation.cleared'",
            )?;
            let mut rows = statement.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push((row.get(0)?, row.get(1)?));
            }
            Ok(out)
        })
        .expect("the audit rows");
    assert_eq!(
        audited,
        vec![
            (device_id.clone(), "conversation.cleared".to_owned()),
            (device_id, "conversation.cleared".to_owned()),
        ]
    );

    harness.stop();
}

#[tokio::test]
async fn an_unknown_conversation_is_a_404_by_code_and_creates_no_turn() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (device_id, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];

    let missing = harness
        .request("GET", "/v1/conversations/no-such-conversation", &auth, None)
        .await;
    assert_eq!(missing.status, 404, "{}", missing.body);
    let body = missing.json();
    assert_eq!(body["ok"], false);
    assert_eq!(body["code"], "conversation_not_found");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "the refusal says something: {body}"
    );

    let cleared = harness
        .request(
            "POST",
            "/v1/conversations/no-such-conversation/clear",
            &auth,
            None,
        )
        .await;
    assert_eq!(cleared.status, 404, "{}", cleared.body);
    assert_eq!(cleared.json()["code"], "conversation_not_found");

    // A turn that names a conversation nobody holds is refused before a job
    // exists: no row, and no provider call.
    let refused = harness
        .request(
            "POST",
            "/v1/turns",
            &auth,
            Some(r#"{"text":"ok","conversation":"no-such-conversation"}"#),
        )
        .await;
    assert_eq!(refused.status, 404, "{}", refused.body);
    assert_eq!(refused.json()["code"], "conversation_not_found");
    let jobs = harness
        .engine
        .db()
        .with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM jobs WHERE device_id = ?1",
                [&device_id],
                |row| row.get::<_, i64>(0),
            )
        })
        .expect("job count");
    assert_eq!(jobs, 0, "a refused turn leaves no job behind");
    assert_eq!(provider.hits(), 0, "and no provider call");

    harness.stop();
}

#[tokio::test]
async fn a_turn_named_for_a_conversation_writes_both_rows_and_reports_it() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];
    let conversation = a_conversation(&harness);

    let response = harness
        .request(
            "POST",
            "/v1/turns",
            &auth,
            Some(&format!(
                r#"{{"text":"reply with the single word: ok","conversation":"{conversation}"}}"#
            )),
        )
        .await;
    assert_eq!(response.status, 202, "{}", response.body);
    let body = response.json();
    assert_eq!(body["ok"], true);
    assert_eq!(
        body["conversation_id"],
        conversation.as_str(),
        "the turn says which conversation it belongs to"
    );
    let job_id = body["job_id"].as_str().expect("a job id").to_owned();

    // Drain the turn so its answer is certainly settled.
    let mut stream = Sse::open(
        harness.addr,
        &format!("/v1/turns/{job_id}/events"),
        &token,
        None,
    )
    .await;
    let frames = stream.frames_until_end(Duration::from_secs(10)).await;
    assert_eq!(frames.last().expect("a terminal frame").event, "done");

    let fetched = harness
        .request(
            "GET",
            &format!("/v1/conversations/{conversation}"),
            &auth,
            None,
        )
        .await;
    assert_eq!(fetched.status, 200, "{}", fetched.body);
    let body = fetched.json();
    let messages = body["messages"].as_array().expect("a message list");
    assert_eq!(messages.len(), 2, "a question and an answer: {body}");
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["body"], "reply with the single word: ok");
    assert_eq!(messages[0]["state"], "complete");
    assert!(messages[0]["id"].as_str().is_some());
    assert!(messages[0]["created_at"].as_str().is_some());
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["body"], "ok");
    assert_eq!(messages[1]["state"], "complete");
    assert_eq!(
        messages[1]["tools"],
        serde_json::json!([]),
        "a turn that called nothing carries an empty tool list"
    );
    assert_eq!(messages[1]["reasoning"], serde_json::Value::Null);

    // The list reports the ground truth: two messages, and a title named from the
    // first question rather than the placeholder.
    let listed = harness
        .request("GET", "/v1/conversations", &auth, None)
        .await;
    let items = listed.json()["conversations"].clone();
    assert_eq!(items[0]["id"], conversation.as_str());
    assert_eq!(items[0]["message_count"], 2);
    assert_ne!(
        items[0]["title"],
        eris_host::web::chat::NEW_CONVERSATION_TITLE,
        "the first question names the conversation: {}",
        items[0]["title"]
    );

    harness.stop();
}

#[tokio::test]
async fn a_long_transcript_and_a_long_list_are_bounded_and_say_so() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];
    let db = harness.engine.db().clone();

    // One conversation holding more messages than a page.
    let conversation = a_conversation(&harness);
    for index in 0..205 {
        eris_host::web::chat::append(
            &db,
            &conversation,
            eris_host::web::chat::Role::User,
            &format!("message {index}"),
            eris_host::web::chat::State::Complete,
        )
        .expect("a message");
    }
    let fetched = harness
        .request(
            "GET",
            &format!("/v1/conversations/{conversation}"),
            &auth,
            None,
        )
        .await;
    assert_eq!(fetched.status, 200, "{}", fetched.body);
    let body = fetched.json();
    let messages = body["messages"].as_array().expect("a message list");
    assert_eq!(messages.len(), 200, "the transcript is capped at a page");
    assert_eq!(body["truncated"], true, "and it says the page was cut");
    assert_eq!(messages[0]["body"], "message 0", "oldest first");

    // More conversations than a page, too.
    for _ in 0..105 {
        a_conversation(&harness);
    }
    let listed = harness
        .request("GET", "/v1/conversations", &auth, None)
        .await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    let body = listed.json();
    assert_eq!(body["count"], 100, "the list is capped at a page");
    assert_eq!(body["truncated"], true);
    assert_eq!(
        body["conversations"].as_array().expect("a list").len(),
        100,
        "count and the array agree"
    );

    harness.stop();
}

#[tokio::test]
async fn a_cancelled_turn_does_not_settle_its_conversation_answer() {
    let provider = StubProvider::start(
        &["a", "b", "c", "d", "e", "f", "g", "h"],
        Duration::from_millis(150),
    )
    .await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];
    let conversation = a_conversation(&harness);

    let response = harness
        .request(
            "POST",
            "/v1/turns",
            &auth,
            Some(&format!(
                r#"{{"text":"ok","conversation":"{conversation}"}}"#
            )),
        )
        .await;
    assert_eq!(response.status, 202, "{}", response.body);
    let job_id = response.json()["job_id"]
        .as_str()
        .expect("a job id")
        .to_owned();

    let mut stream = Sse::open(
        harness.addr,
        &format!("/v1/turns/{job_id}/events"),
        &token,
        None,
    )
    .await;
    let _ = stream.next_frame(Duration::from_secs(5)).await;
    let cancelled = harness
        .request("POST", &format!("/v1/turns/{job_id}/cancel"), &auth, None)
        .await;
    assert_eq!(cancelled.status, 200, "{}", cancelled.body);
    let frames = stream.frames_until_end(Duration::from_secs(10)).await;
    assert_eq!(frames.last().expect("a terminal frame").event, "cancelled");

    // The provider call finishes on its own schedule; give it time to finish and
    // the worker time to close the row.
    tokio::time::sleep(Duration::from_millis(1800)).await;

    let fetched = harness
        .request(
            "GET",
            &format!("/v1/conversations/{conversation}"),
            &auth,
            None,
        )
        .await;
    assert_eq!(fetched.status, 200, "{}", fetched.body);
    let body = fetched.json();
    let messages = body["messages"].as_array().expect("a message list");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(
        messages[1]["state"], "failed",
        "the cancelled answer is closed, not settled: {}",
        messages[1]
    );
    assert_eq!(messages[1]["body"], "Stopped before it finished.");
    assert!(provider.hits() >= 1);

    harness.stop();
}

// ---------------------------------------------------------------------------
// Settings behind the admin scope
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_code_minted_with_the_box_ticked_yields_an_admin_token() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;

    // An ordinary code: the device defaults, which do not include admin, and the
    // settings route refuses the token it buys.
    let (ordinary_device, ordinary_code) = harness.device_with_code("iPhone");
    let ordinary = harness
        .request(
            "POST",
            "/v1/pair",
            &[],
            Some(&format!(
                r#"{{"code":"{ordinary_code}","device_name":"iPhone"}}"#
            )),
        )
        .await;
    assert_eq!(ordinary.status, 201, "{}", ordinary.body);
    let ordinary_body = ordinary.json();
    let ordinary_scopes = ordinary_body["scopes"]
        .as_array()
        .expect("a scope list")
        .clone();
    assert!(
        !ordinary_scopes.contains(&serde_json::json!("admin")),
        "an ordinary code mints no admin: {ordinary_scopes:?}"
    );
    let ordinary_access = ordinary_body["access_token"]
        .as_str()
        .expect("an access token")
        .to_owned();
    let refused = harness
        .request(
            "GET",
            "/v1/settings",
            &[("authorization", format!("Bearer {ordinary_access}"))],
            None,
        )
        .await;
    assert_eq!(refused.status, 403, "{}", refused.body);

    // A code the operator ticked the administration box for.
    let (admin_device, admin_code) = harness.device_with_admin_code("iPad");
    let admin = harness
        .request(
            "POST",
            "/v1/pair",
            &[],
            Some(&format!(
                r#"{{"code":"{admin_code}","device_name":"iPad"}}"#
            )),
        )
        .await;
    assert_eq!(admin.status, 201, "{}", admin.body);
    let admin_body = admin.json();
    assert_eq!(admin_body["device_id"], admin_device);
    let admin_scopes = admin_body["scopes"]
        .as_array()
        .expect("a scope list")
        .clone();
    assert!(
        admin_scopes.contains(&serde_json::json!("admin")),
        "the ticked box reaches the token: {admin_scopes:?}"
    );
    for expected in ["chat", "sessions", "files"] {
        assert!(
            admin_scopes.contains(&serde_json::json!(expected)),
            "the admin code keeps the defaults too: {admin_scopes:?}"
        );
    }
    let admin_access = admin_body["access_token"]
        .as_str()
        .expect("an access token")
        .to_owned();
    let admin_refresh = admin_body["refresh_token"]
        .as_str()
        .expect("a refresh token")
        .to_owned();

    // The token that code redeemed reads the snapshot, so the admin scope on it
    // is usable and not only listed.
    let readable = harness
        .request(
            "GET",
            "/v1/settings",
            &[("authorization", format!("Bearer {admin_access}"))],
            None,
        )
        .await;
    assert_eq!(readable.status, 200, "{}", readable.body);

    // The scope is on the stored row, not only in the response.
    let stored: Vec<String> = harness
        .engine
        .db()
        .with_conn(|conn| {
            let mut statement =
                conn.prepare("SELECT scope FROM auth_tokens WHERE device_id = ?1 ORDER BY id")?;
            let mut rows = statement.query([admin_device.as_str()])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(row.get::<_, String>(0)?);
            }
            Ok(out)
        })
        .expect("the stored scopes");
    assert_eq!(stored.len(), 2, "an access row and a refresh row");
    for scope in &stored {
        assert!(
            scope.split_whitespace().any(|word| word == "admin"),
            "the stored scope set carries admin: {scope}"
        );
    }

    // And it survives a refresh, because a token that expires in an hour is not
    // an administration grant, it is a one-hour one.
    let refreshed = harness
        .request(
            "POST",
            "/v1/token/refresh",
            &[],
            Some(&format!(r#"{{"refresh_token":"{admin_refresh}"}}"#)),
        )
        .await;
    assert_eq!(refreshed.status, 200, "{}", refreshed.body);
    let fresh = refreshed.json()["access_token"]
        .as_str()
        .expect("a fresh access token")
        .to_owned();
    let after_refresh = harness
        .request(
            "GET",
            "/v1/settings",
            &[("authorization", format!("Bearer {fresh}"))],
            None,
        )
        .await;
    assert_eq!(after_refresh.status, 200, "{}", after_refresh.body);

    // The ordinary device is untouched by all of that: its scope set is unchanged
    // and still cannot administer.
    let ordinary_stored: String = harness
        .engine
        .db()
        .with_conn(|conn| {
            conn.query_row(
                "SELECT scope FROM auth_tokens WHERE device_id = ?1 ORDER BY id LIMIT 1",
                [ordinary_device.as_str()],
                |row| row.get::<_, String>(0),
            )
        })
        .expect("the ordinary scope");
    assert_eq!(ordinary_stored, "chat sessions files");

    harness.stop();
}

#[tokio::test]
async fn settings_require_a_bearer_and_the_admin_scope() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, device_token) = harness.device_with_token("iPhone");
    let (_, admin_token) = harness.device_with_admin_token("iPad");

    // No bearer at all: the same 401 every other route answers.
    for method in ["GET", "POST"] {
        let anonymous = harness
            .request(method, "/v1/settings", &[], Some(r#"{"memory":"off"}"#))
            .await;
        assert_eq!(anonymous.status, 401, "{method}: {}", anonymous.body);
        assert_eq!(anonymous.json()["code"], "token_invalid");
    }

    // A real token without admin: 403, and the message says why rather than the
    // word forbidden.
    for method in ["GET", "POST"] {
        let denied = harness
            .request(
                method,
                "/v1/settings",
                &[("authorization", format!("Bearer {device_token}"))],
                Some(r#"{"memory":"off"}"#),
            )
            .await;
        assert_eq!(denied.status, 403, "{method}: {}", denied.body);
        let body = denied.json();
        assert_eq!(body["code"], "scope_denied");
        let message = body["message"].as_str().expect("a message");
        assert!(
            message.contains("paired without administration"),
            "the refusal says what actually happened: {message}"
        );
        assert!(
            !message.to_ascii_lowercase().contains("forbidden"),
            "the refusal is not the bare word: {message}"
        );
    }

    // Nothing was written by either refused post.
    let written: i64 = harness
        .engine
        .db()
        .with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM settings WHERE key = 'memory.backend'",
                [],
                |row| row.get(0),
            )
        })
        .expect("the memory row count");
    assert_eq!(written, 0, "a refused post writes nothing");

    // An admin token reads the snapshot.
    let read = harness
        .request(
            "GET",
            "/v1/settings",
            &[("authorization", format!("Bearer {admin_token}"))],
            None,
        )
        .await;
    assert_eq!(read.status, 200, "{}", read.body);
    let body = read.json();
    assert_eq!(body["ok"], true);
    let mut keys: Vec<&str> = body
        .as_object()
        .expect("an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "approval",
            "memory",
            "model",
            "ok",
            "sandbox",
            "timezone",
            "tool_rounds",
            "tools_enabled"
        ],
        "the body is the documented snapshot and nothing else: {body}"
    );
    assert_eq!(body["model"], "p8-model");
    assert_eq!(body["sandbox"], "read-only");
    assert_eq!(body["approval"], "ask");
    assert_eq!(body["memory"], "gbrain");
    assert_eq!(body["tools_enabled"], true);
    assert_eq!(body["tool_rounds"], 12);
    assert!(body["timezone"].as_str().is_some());

    // Nothing credential shaped is in it. A settings screen that carried a key,
    // a reference or a path would be the leak it exists to avoid.
    let raw = read.body.clone();
    for banned in [
        "env:", "http", "sk-", "key", "password", "token", "base_url",
    ] {
        assert!(
            !raw.to_ascii_lowercase().contains(banned),
            "the snapshot carries no {banned}: {raw}"
        );
    }
    assert!(!raw.contains('/'), "no path is in the snapshot: {raw}");

    harness.stop();
}

#[tokio::test]
async fn settings_post_changes_a_subset_and_refuses_an_invalid_value() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    // Two ids the endpoint publishes, so a model change is a real change.
    harness
        .engine
        .set_provider(
            &provider.url,
            TEST_KEY_REF,
            &["p8-model".to_owned(), "p8-model-alt".to_owned()],
        )
        .expect("the catalog");
    let (device_id, admin_token) = harness.device_with_admin_token("iPad");
    let auth = [("authorization", format!("Bearer {admin_token}"))];

    // A valid subset: two fields, one of them a model the endpoint published.
    let changed = harness
        .request(
            "POST",
            "/v1/settings",
            &auth,
            Some(r#"{"sandbox":"workspace-write","model":"p8-model-alt"}"#),
        )
        .await;
    assert_eq!(changed.status, 200, "{}", changed.body);
    let body = changed.json();
    assert_eq!(body["ok"], true);
    assert_eq!(
        body["changed"],
        serde_json::json!(["model", "sandbox"]),
        "the response lists what changed: {body}"
    );

    let after = harness.request("GET", "/v1/settings", &auth, None).await;
    assert_eq!(after.status, 200, "{}", after.body);
    assert_eq!(after.json()["sandbox"], "workspace-write");
    assert_eq!(after.json()["model"], "p8-model-alt");
    assert_eq!(
        after.json()["approval"],
        "ask",
        "a field the body did not name is untouched"
    );

    // The write is audited with the device as the actor, and the detail names the
    // fields and values that were written.
    let audited: Vec<(String, String, String)> = harness
        .engine
        .db()
        .with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT actor, action, detail FROM audit_log \
                  WHERE action = 'settings.device_update' ORDER BY id",
            )?;
            let mut rows = statement.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push((row.get(0)?, row.get(1)?, row.get(2)?));
            }
            Ok(out)
        })
        .expect("the audit rows");
    assert_eq!(audited.len(), 1, "one write, one row: {audited:?}");
    assert_eq!(audited[0].0, device_id, "the actor is the device");
    assert_eq!(audited[0].1, "settings.device_update");
    assert!(
        audited[0].2.contains("model=p8-model-alt"),
        "the detail names the model that was written: {}",
        audited[0].2
    );
    assert!(
        audited[0].2.contains("sandbox=workspace-write"),
        "the detail names the sandbox that was written: {}",
        audited[0].2
    );

    // An invalid value is a 400 that names the field and what it accepts, and it
    // writes nothing at all: the valid model in the same body is left alone.
    let refused = harness
        .request(
            "POST",
            "/v1/settings",
            &auth,
            Some(r#"{"model":"p8-model","sandbox":"yolo"}"#),
        )
        .await;
    assert_eq!(refused.status, 400, "{}", refused.body);
    let refusal = refused.json();
    assert_eq!(refusal["code"], "sandbox_invalid");
    let message = refusal["message"].as_str().expect("a message");
    assert!(message.contains("sandbox"), "names the field: {message}");
    assert!(message.contains("yolo"), "names the value: {message}");
    assert!(
        message.contains("read-only") && message.contains("workspace-write"),
        "names the allowed values: {message}"
    );

    let untouched = harness.request("GET", "/v1/settings", &auth, None).await;
    assert_eq!(untouched.status, 200, "{}", untouched.body);
    assert_eq!(
        untouched.json()["sandbox"],
        "workspace-write",
        "the refused value was not written"
    );
    assert_eq!(
        untouched.json()["model"],
        "p8-model-alt",
        "all or nothing: the valid model beside an invalid sandbox is not written either"
    );
    let rows: i64 = harness
        .engine
        .db()
        .with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'settings.device_update'",
                [],
                |row| row.get(0),
            )
        })
        .expect("the audit count");
    assert_eq!(rows, 1, "a refused write appends no audit row");

    // A body that names none of the four fields is refused rather than 200.
    let empty = harness
        .request("POST", "/v1/settings", &auth, Some(r#"{}"#))
        .await;
    assert_eq!(empty.status, 400, "{}", empty.body);
    assert_eq!(empty.json()["code"], "settings_empty");

    // A model the endpoint never published is refused by field too.
    let unknown = harness
        .request(
            "POST",
            "/v1/settings",
            &auth,
            Some(r#"{"model":"no-such-model"}"#),
        )
        .await;
    assert_eq!(unknown.status, 400, "{}", unknown.body);
    assert_eq!(unknown.json()["code"], "model_invalid");
    assert!(
        unknown.body.contains("p8-model-alt"),
        "the refusal names the ids the probe published: {}",
        unknown.body
    );

    // And the admin scope is what unlocks it: the same post from a device token
    // is a 403 that writes nothing.
    let (_, device_token) = harness.device_with_token("iPhone");
    let denied = harness
        .request(
            "POST",
            "/v1/settings",
            &[("authorization", format!("Bearer {device_token}"))],
            Some(r#"{"sandbox":"read-only"}"#),
        )
        .await;
    assert_eq!(denied.status, 403, "{}", denied.body);
    assert_eq!(denied.json()["code"], "scope_denied");
    let still = harness.request("GET", "/v1/settings", &auth, None).await;
    assert_eq!(still.json()["sandbox"], "workspace-write");

    harness.stop();
}

#[tokio::test]
async fn the_admin_scope_unlocks_admin_routes_and_nothing_else() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, admin_token) = harness.device_with_admin_token("iPad");
    let auth = [("authorization", format!("Bearer {admin_token}"))];

    // Admin is not chat and it is not sessions: a token carrying only admin is
    // still refused by the routes that need those, so one scope cannot silently
    // become the other.
    let admin_only = {
        let db = harness.engine.db().clone();
        let device = AuthStore::new(db.clone())
            .register_device("AdminOnly", "ios")
            .expect("device row");
        TokenService::new(db)
            .mint_access(
                &device,
                ScopeSet::EMPTY.with(Scope::Admin),
                TokenOrigin::PairedAdmin,
            )
            .expect("admin only token")
            .token()
            .to_owned()
    };
    let settings = harness
        .request(
            "GET",
            "/v1/settings",
            &[("authorization", format!("Bearer {admin_only}"))],
            None,
        )
        .await;
    assert_eq!(settings.status, 200, "{}", settings.body);
    // Admin also reaches the administration surface added beside the settings:
    // reading the devices, the trail and the identity are the same authority.
    for path in ["/v1/devices", "/v1/audit", "/v1/persona"] {
        let read = harness
            .request(
                "GET",
                path,
                &[("authorization", format!("Bearer {admin_only}"))],
                None,
            )
            .await;
        assert_eq!(read.status, 200, "GET {path}: {}", read.body);
    }
    for (method, path) in [
        ("GET", "/v1/projects"),
        ("GET", "/v1/conversations"),
        ("POST", "/v1/turns"),
    ] {
        let denied = harness
            .request(
                method,
                path,
                &[("authorization", format!("Bearer {admin_only}"))],
                Some(r#"{"text":"hello","project_name":"Work"}"#),
            )
            .await;
        assert_eq!(denied.status, 403, "{method} {path}: {}", denied.body);
        assert_eq!(denied.json()["code"], "scope_denied");
    }

    // The admin token that was paired with the defaults keeps them, so nothing
    // was widened on the routes that were already there.
    let conversations = harness
        .request("GET", "/v1/conversations", &auth, None)
        .await;
    assert_eq!(conversations.status, 200, "{}", conversations.body);

    harness.stop();
}

// ---------------------------------------------------------------------------
// The approvals queue and MCP health over /v1
// ---------------------------------------------------------------------------

/// Park one call the way a browser turn's gate parks it, stamped at a fixed
/// second so "newest first" is a fact rather than a millisecond tie.
fn park(harness: &Harness, tool: &str, second: u8, advice: Option<&str>) -> String {
    let db = harness.engine.db();
    let id = eris_host::approvals::queue(
        db,
        "conv-approvals",
        tool,
        &serde_json::json!({"path": "notes/a.txt", "content": "hi"}),
        advice,
    )
    .expect("queued");
    let stamp = format!("2026-01-01T00:00:{second:02}.000Z");
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE approvals SET created_at = ?1 WHERE id = ?2",
            (stamp.as_str(), id.as_str()),
        )
    })
    .expect("stamped");
    id
}

/// One approval row's status and who settled it, read back from the store.
fn approval_row(harness: &Harness, id: &str) -> (String, Option<String>) {
    harness
        .engine
        .db()
        .with_conn(|conn| {
            conn.query_row(
                "SELECT status, decided_by FROM approvals WHERE id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
        })
        .expect("the approval row")
}

/// How many audit rows the store holds.
fn audit_rows(harness: &Harness) -> i64 {
    harness
        .engine
        .db()
        .with_conn(|conn| conn.query_row("SELECT COUNT(*) FROM audit_log", [], |row| row.get(0)))
        .expect("the audit count")
}

/// How many decisions the audit log holds.
fn decision_audits(harness: &Harness) -> i64 {
    harness
        .engine
        .db()
        .with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM audit_log WHERE action = 'chat.approval.decided'",
                [],
                |row| row.get(0),
            )
        })
        .expect("the decision audit count")
}

/// The newest audit row: actor, action and detail.
fn latest_audit(harness: &Harness) -> (String, String, Option<String>) {
    harness
        .engine
        .db()
        .with_conn(|conn| {
            conn.query_row(
                "SELECT actor, action, detail FROM audit_log ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
        })
        .expect("the newest audit row")
}

/// Store an MCP server list, the way the settings save would.
fn store_servers(harness: &Harness, servers: &[(&str, &str)]) {
    let list: Vec<serde_json::Value> = servers
        .iter()
        .map(|(name, command)| serde_json::json!({"name": name, "command": command, "args": []}))
        .collect();
    harness
        .engine
        .set_setting(
            eris_host::mcp::SETTING_SERVERS,
            &serde_json::to_string(&list).expect("a server list"),
        )
        .expect("the stored list");
}

#[tokio::test]
async fn approvals_require_a_bearer_and_the_admin_scope() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let id = park(&harness, "write_file", 0, None);
    let (_, device_token) = harness.device_with_token("iPhone");
    let (_, admin_token) = harness.device_with_admin_token("iPad");

    // No bearer: the same 401 every other route answers.
    for (method, path) in [
        ("GET", "/v1/approvals".to_owned()),
        ("POST", format!("/v1/approvals/{id}/decide")),
        ("GET", "/v1/mcp".to_owned()),
    ] {
        let anonymous = harness
            .request(method, &path, &[], Some(r#"{"decision":"approved"}"#))
            .await;
        assert_eq!(anonymous.status, 401, "{method} {path}: {}", anonymous.body);
        assert_eq!(anonymous.json()["code"], "token_invalid");
    }

    // A real token without `admin`: 403, and the message says the device was
    // paired without administration rather than the bare word forbidden.
    for (method, path) in [
        ("GET", "/v1/approvals".to_owned()),
        ("POST", format!("/v1/approvals/{id}/decide")),
    ] {
        let denied = harness
            .request(
                method,
                &path,
                &[("authorization", format!("Bearer {device_token}"))],
                Some(r#"{"decision":"approved"}"#),
            )
            .await;
        assert_eq!(denied.status, 403, "{method} {path}: {}", denied.body);
        let body = denied.json();
        assert_eq!(body["code"], "scope_denied");
        let message = body["message"].as_str().expect("a message");
        assert!(
            message.contains("paired without administration"),
            "the refusal says what actually happened: {message}"
        );
        assert!(
            !message.to_ascii_lowercase().contains("forbidden"),
            "the refusal is not the bare word: {message}"
        );
    }

    // Neither refused post moved the row it named, and neither left a decision
    // in the audit log.
    assert_eq!(approval_row(&harness, &id).0, "pending");
    assert_eq!(
        decision_audits(&harness),
        0,
        "a refused decision audits nothing"
    );

    // The admin token reads the queue, so the refusals above were about scope.
    let read = harness
        .request(
            "GET",
            "/v1/approvals",
            &[("authorization", format!("Bearer {admin_token}"))],
            None,
        )
        .await;
    assert_eq!(read.status, 200, "{}", read.body);
    assert_eq!(read.json()["count"], 1);

    harness.stop();
}

#[tokio::test]
async fn the_approvals_list_is_newest_first_and_capped_with_truncated() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, admin_token) = harness.device_with_admin_token("iPad");
    let auth = [("authorization", format!("Bearer {admin_token}"))];

    let oldest = park(
        &harness,
        "write_file",
        0,
        Some("CAREFUL - overwrites a file that already exists"),
    );
    let listed = harness.request("GET", "/v1/approvals", &auth, None).await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    let body = listed.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["count"], 1);
    assert_eq!(body["truncated"], false);
    let row = &body["approvals"][0];
    assert_eq!(row["id"], oldest.as_str());
    assert_eq!(row["tool"], "write_file");
    assert_eq!(
        row["reason"], "CAREFUL - overwrites a file that already exists",
        "the approval role's reading rides along as the reason"
    );
    assert_eq!(
        row["arguments"],
        serde_json::json!({"path": "notes/a.txt", "content": "hi"}),
        "the parked arguments come back as JSON, not as quoted text"
    );
    assert_eq!(row["expires_at"], serde_json::Value::Null);
    let mut keys: Vec<&str> = row
        .as_object()
        .expect("an approval object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "arguments",
            "created_at",
            "expires_at",
            "id",
            "reason",
            "tool"
        ],
        "the row is the documented shape and nothing else: {row}"
    );

    // Nothing credential shaped and no workspace path, so a queue read over a
    // shoulder leaks nothing the call would not have written itself.
    let raw = listed.body.clone();
    assert!(
        !raw.contains(&harness._dir.path().display().to_string()),
        "no workspace path is in the list: {raw}"
    );
    assert!(!raw.contains("env:"), "no key reference is in it: {raw}");
    assert!(!raw.contains(TEST_KEY), "no provider key is in it: {raw}");

    // Fifty more rows, so the page has to bound itself and say so.
    let mut newest = oldest.clone();
    for second in 1..=50u8 {
        newest = park(&harness, "write_file", second, None);
    }
    let capped = harness.request("GET", "/v1/approvals", &auth, None).await;
    assert_eq!(capped.status, 200, "{}", capped.body);
    let body = capped.json();
    assert_eq!(body["count"], 50);
    assert_eq!(body["truncated"], true);
    let rows = body["approvals"].as_array().expect("a list").clone();
    assert_eq!(rows.len(), 50);
    assert_eq!(
        rows[0]["id"], newest,
        "the newest parked call is at the top of the page"
    );
    let ids: Vec<&str> = rows.iter().filter_map(|row| row["id"].as_str()).collect();
    assert!(
        !ids.contains(&oldest.as_str()),
        "the oldest call is what falls off the capped page"
    );

    harness.stop();
}

#[tokio::test]
async fn deciding_an_approval_moves_the_row_and_audits_the_device() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (device_id, admin_token) = harness.device_with_admin_token("iPad");
    let auth = [("authorization", format!("Bearer {admin_token}"))];
    // An approval runs the call now, so the sandbox has to accept the write or
    // the test would be asserting a refusal.
    let widened = harness
        .request_form("POST", "/v1/settings", &auth, "sandbox=workspace-write")
        .await;
    assert_eq!(widened.status, 200, "{}", widened.body);
    let allow_id = park(&harness, "write_file", 0, None);
    let deny_id = park(&harness, "write_file", 1, None);

    // Allow, as a form body: a curl acceptance run does not set a type.
    let allowed = harness
        .request_form(
            "POST",
            &format!("/v1/approvals/{allow_id}/decide"),
            &auth,
            "decision=approved",
        )
        .await;
    assert_eq!(allowed.status, 200, "{}", allowed.body);
    let body = allowed.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["id"], allow_id.as_str());
    assert_eq!(body["decision"], "approved");
    assert!(
        body["result"].as_str().is_some(),
        "the answer carries what the call said, not only that it was decided: {body}"
    );
    let mut keys: Vec<&str> = body
        .as_object()
        .expect("a decision object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["decision", "id", "ok", "result"],
        "the body is the documented shape and nothing else: {body}"
    );

    // The write actually happened, which is the whole point of approving it.
    let written = harness
        .engine
        .workspace_path()
        .expect("a workspace")
        .join("notes/a.txt");
    assert_eq!(
        std::fs::read_to_string(&written).expect("the approved write landed"),
        "hi"
    );

    // The row moved, and the actor is the device rather than the browser.
    let (status, decided_by) = approval_row(&harness, &allow_id);
    assert_eq!(status, "approved");
    assert_eq!(decided_by.as_deref(), Some(device_id.as_str()));
    let (actor, action, detail) = latest_audit(&harness);
    assert_eq!(actor, device_id);
    assert_eq!(action, "chat.approval.decided");
    let detail = detail.expect("a detail");
    assert!(detail.contains(&allow_id), "{detail}");
    assert!(detail.contains("tool=write_file"), "{detail}");
    assert!(detail.contains("decision=approved"), "{detail}");

    // Deny, as JSON, and the same history results.
    let denied = harness
        .request(
            "POST",
            &format!("/v1/approvals/{deny_id}/decide"),
            &auth,
            Some(r#"{"decision":"denied"}"#),
        )
        .await;
    assert_eq!(denied.status, 200, "{}", denied.body);
    assert_eq!(denied.json()["decision"], "denied");
    let (status, decided_by) = approval_row(&harness, &deny_id);
    assert_eq!(status, "denied");
    assert_eq!(decided_by.as_deref(), Some(device_id.as_str()));
    let (actor, action, detail) = latest_audit(&harness);
    assert_eq!(actor, device_id);
    assert_eq!(action, "chat.approval.decided");
    assert!(
        detail.expect("a detail").contains("decision=denied"),
        "the audit row says what the decision was"
    );

    // Both rows left the queue.
    let listed = harness.request("GET", "/v1/approvals", &auth, None).await;
    assert_eq!(listed.json()["count"], 0, "{}", listed.body);

    // A second decision on the same id is a 404 by code, and it changes nothing.
    let audited = audit_rows(&harness);
    let again = harness
        .request(
            "POST",
            &format!("/v1/approvals/{allow_id}/decide"),
            &auth,
            Some(r#"{"decision":"denied"}"#),
        )
        .await;
    assert_eq!(again.status, 404, "{}", again.body);
    assert_eq!(again.json()["code"], "approval_not_found");
    assert_eq!(approval_row(&harness, &allow_id).0, "approved");
    assert_eq!(
        audit_rows(&harness),
        audited,
        "a refused decide audits nothing"
    );

    // An id nobody queued is the same 404.
    let unknown = harness
        .request(
            "POST",
            "/v1/approvals/ap-never-issued/decide",
            &auth,
            Some(r#"{"decision":"approved"}"#),
        )
        .await;
    assert_eq!(unknown.status, 404, "{}", unknown.body);
    assert_eq!(unknown.json()["code"], "approval_not_found");

    // A decision that is not one of the two is a 400 that names them, and
    // leaves the row pending with nothing audited.
    let fresh = park(&harness, "write_file", 2, None);
    let audited = audit_rows(&harness);
    for bad in ["allow", "deny", "APPROVED", ""] {
        let refused = harness
            .request(
                "POST",
                &format!("/v1/approvals/{fresh}/decide"),
                &auth,
                Some(&format!(r#"{{"decision":"{bad}"}}"#)),
            )
            .await;
        assert_eq!(refused.status, 400, "{bad}: {}", refused.body);
        let body = refused.json();
        assert_eq!(body["code"], "decision_invalid");
        let message = body["message"].as_str().expect("a message");
        assert!(
            message.contains("approved") && message.contains("denied"),
            "the refusal names the values it would have taken: {message}"
        );
    }
    assert_eq!(approval_row(&harness, &fresh).0, "pending");
    assert_eq!(
        audit_rows(&harness),
        audited,
        "an invalid decision writes nothing"
    );

    harness.stop();
}

#[tokio::test]
async fn mcp_servers_and_their_health_need_only_the_sessions_scope() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    store_servers(
        &harness,
        &[("vault", "mcp-vault"), ("search", "mcp-search")],
    );
    let (_, token) = harness.device_with_token("iPhone");
    let auth = [("authorization", format!("Bearer {token}"))];
    let scoped = token_without(&harness, "iPad", eris_host::auth::Scope::Sessions);

    // A real token without `sessions` is 403: this route is a read of the
    // transcript's class, not an administration one.
    let denied = harness
        .request(
            "GET",
            "/v1/mcp",
            &[("authorization", format!("Bearer {scoped}"))],
            None,
        )
        .await;
    assert_eq!(denied.status, 403, "{}", denied.body);
    assert_eq!(denied.json()["code"], "scope_denied");

    // This service was handed no pool, so it has observed nothing: the honest
    // answer is "unknown", not a boolean per server.
    let read = harness.request("GET", "/v1/mcp", &auth, None).await;
    assert_eq!(read.status, 200, "{}", read.body);
    let body = read.json();
    assert_eq!(body["ok"], true);
    assert_eq!(
        body["health_known"], false,
        "a daemon that has made no attempt must not claim health: {body}"
    );
    let servers = body["servers"].as_array().expect("a server list");
    assert_eq!(servers.len(), 2);
    assert_eq!(servers[0]["name"], "vault");
    assert_eq!(servers[0]["command"], "mcp-vault");
    assert_eq!(
        servers[0]["healthy"],
        serde_json::Value::Null,
        "never started is unknown, not healthy"
    );
    assert_eq!(servers[0]["last_error"], serde_json::Value::Null);
    let mut keys: Vec<&str> = servers[0]
        .as_object()
        .expect("a server object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["command", "healthy", "last_error", "name"]);
    // The arguments are not part of this shape: they can carry a path the
    // operator never meant to publish.
    assert!(!read.body.contains("\"args\""), "{}", read.body);
    harness.stop();

    // A service that was handed the daemon's pool reports what that pool
    // actually observed, and the flag says the list is covered.
    let pool = Arc::new(eris_host::mcp::Pool::new(vec![
        eris_host::mcp::Server {
            name: "vault".to_owned(),
            command: "/nonexistent/mcp-vault".to_owned(),
            args: Vec::new(),
        },
        eris_host::mcp::Server {
            name: "search".to_owned(),
            command: "/nonexistent/mcp-search".to_owned(),
            args: Vec::new(),
        },
    ]));
    // The attempt the daemon makes when a turn asks for tools.
    let started = pool.specs();
    assert!(
        started.is_empty(),
        "a server that cannot start offers nothing"
    );
    let harness = Harness::start_with_mcp(&provider.url, DEFAULT_SSE_BUFFER, pool).await;
    store_servers(
        &harness,
        &[
            ("vault", "/nonexistent/mcp-vault"),
            ("search", "/nonexistent/mcp-search"),
        ],
    );
    let (_, token) = harness.device_with_token("iPhone2");
    let read = harness
        .request(
            "GET",
            "/v1/mcp",
            &[("authorization", format!("Bearer {token}"))],
            None,
        )
        .await;
    assert_eq!(read.status, 200, "{}", read.body);
    let body = read.json();
    assert_eq!(
        body["health_known"], true,
        "every server has an observed state: {body}"
    );
    assert_eq!(body["servers"][0]["healthy"], false);
    let error = body["servers"][0]["last_error"]
        .as_str()
        .expect("the reason the server is down");
    assert!(error.contains("cannot start"), "{error}");

    harness.stop();
}

/// The dashboard page, one in-process request through the router the daemon
/// serves. The browser surface is unchanged by the device routes; this is how it
/// is read here.
async fn dashboard(app: &axum::Router, cookie: &str) -> (u16, String) {
    use tower::ServiceExt;
    let request = axum::http::Request::builder()
        .method("GET")
        .uri("/dashboard")
        .header("accept", "text/html")
        .header("cookie", cookie)
        .body(axum::body::Body::empty())
        .expect("a dashboard request");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("the dashboard answers");
    let status = response.status().as_u16();
    let body = axum::body::to_bytes(response.into_body(), 4 * 1024 * 1024)
        .await
        .expect("the dashboard body");
    (status, String::from_utf8_lossy(&body).into_owned())
}

#[tokio::test]
async fn the_dashboard_reads_the_one_queue_a_device_answers() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let id = park(&harness, "write_file", 0, None);

    // The browser surface, built exactly as the daemon builds it.
    let app = eris_host::admin::router_with(eris_host::admin::AppState::new(
        harness.engine.clone(),
        false,
    ));
    let cookie = format!(
        "kallisti_admin={}",
        harness
            .engine
            .sessions()
            .mint()
            .expect("an admin session")
            .cookie_value
    );

    let (status, page) = dashboard(&app, &cookie).await;
    assert_eq!(status, 200, "the dashboard still answers: {page}");
    assert!(
        page.contains("parked"),
        "the dashboard still counts the parked call: {page}"
    );

    // The device answers it, and the same page stops counting it: one queue, one
    // history, whichever surface settled the row. The sandbox has to accept the
    // write, because approving it runs it.
    let (_, admin_token) = harness.device_with_admin_token("iPad");
    let widened = harness
        .request_form(
            "POST",
            "/v1/settings",
            &[("authorization", format!("Bearer {admin_token}"))],
            "sandbox=workspace-write",
        )
        .await;
    assert_eq!(widened.status, 200, "{}", widened.body);
    let decided = harness
        .request(
            "POST",
            &format!("/v1/approvals/{id}/decide"),
            &[("authorization", format!("Bearer {admin_token}"))],
            Some(r#"{"decision":"approved"}"#),
        )
        .await;
    assert_eq!(decided.status, 200, "{}", decided.body);

    let (status, page) = dashboard(&app, &cookie).await;
    assert_eq!(status, 200, "{page}");
    assert!(
        !page.contains("parked"),
        "a decision on the device empties the dashboard's counter: {page}"
    );

    harness.stop();
}

// ---------------------------------------------------------------------------
// The administration half: devices, revocation, the trail and the identity
// ---------------------------------------------------------------------------

/// One page through the browser router, the way the daemon serves it.
async fn browser_page(app: &axum::Router, cookie: &str, path: &str) -> (u16, String) {
    use tower::ServiceExt;
    let request = axum::http::Request::builder()
        .method("GET")
        .uri(path)
        .header("accept", "text/html")
        .header("cookie", cookie)
        .body(axum::body::Body::empty())
        .expect("a page request");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("the page answers");
    let status = response.status().as_u16();
    let body = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("the page body");
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// One form post through the browser router, so the revoke button is exercised
/// the way a browser sends it and not only through the device route.
async fn browser_post(app: &axum::Router, cookie: &str, path: &str, body: &str) -> (u16, String) {
    use tower::ServiceExt;
    let request = axum::http::Request::builder()
        .method("POST")
        .uri(path)
        .header("accept", "text/html")
        .header("cookie", cookie)
        .header("content-type", FORM)
        .body(axum::body::Body::from(body.to_owned()))
        .expect("a form request");
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("the form answers");
    let status = response.status().as_u16();
    let body = axum::body::to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("the form body");
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// The admin cookie for the in-process browser router.
fn browser_cookie(harness: &Harness) -> String {
    format!(
        "kallisti_admin={}",
        harness
            .engine
            .sessions()
            .mint()
            .expect("an admin session")
            .cookie_value
    )
}

use eris_host::web::store as web_store;

#[tokio::test]
async fn the_device_and_audit_routes_require_a_bearer_and_the_admin_scope() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, device_token) = harness.device_with_token("iPhone");
    let (other, _) = harness.device_with_token("iPad");

    // Every route this wave added, by method and path, so a route that forgot the
    // guard fails by name instead of by a survey of the whole listener.
    let cases: [(&str, String, Option<&str>); 5] = [
        ("GET", "/v1/devices".to_owned(), None),
        ("POST", format!("/v1/devices/{other}/revoke"), None),
        ("GET", "/v1/audit".to_owned(), None),
        ("GET", "/v1/persona".to_owned(), None),
        (
            "POST",
            "/v1/persona".to_owned(),
            Some(r#"{"persona":"you are Kallisti"}"#),
        ),
    ];

    for (method, path, body) in &cases {
        let anonymous = harness.request(method, path, &[], *body).await;
        assert_eq!(
            anonymous.status, 401,
            "{method} {path} without a bearer: {}",
            anonymous.body
        );
        assert_eq!(anonymous.json()["code"], "token_invalid");

        let denied = harness
            .request(
                method,
                path,
                &[("authorization", format!("Bearer {device_token}"))],
                *body,
            )
            .await;
        assert_eq!(
            denied.status, 403,
            "{method} {path} without admin: {}",
            denied.body
        );
        let refusal = denied.json();
        assert_eq!(refusal["code"], "scope_denied");
        let message = refusal["message"].as_str().expect("a message");
        assert!(
            message.contains("paired without administration"),
            "the refusal says what actually happened: {message}"
        );
        assert!(
            !message.to_ascii_lowercase().contains("forbidden"),
            "the refusal is not the bare word: {message}"
        );
    }

    // A refused revoke revokes nothing, and a refused persona post writes nothing.
    let row = AuthStore::new(harness.engine.db().clone())
        .device(&other)
        .expect("the device read")
        .expect("a device row");
    assert!(
        row.revoked_at.is_none(),
        "a 403 must not settle the row it named"
    );
    let persona_file =
        eris_host::config::Paths::for_root(harness._dir.path().join("root")).persona_file();
    assert!(
        !persona_file.exists(),
        "a 403 must not write the persona file: {}",
        persona_file.display()
    );

    harness.stop();
}

#[tokio::test]
async fn the_device_list_reports_pairing_and_administration_per_device() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;

    // Two devices through the real pairing route: one whose code the operator
    // ticked the box for, one ordinary code. The list has to keep them apart.
    let (admin_id, admin_code) = harness.device_with_admin_code("iPad");
    let paired_admin = harness
        .request(
            "POST",
            "/v1/pair",
            &[],
            Some(&format!(
                r#"{{"code":"{admin_code}","device_name":"iPad"}}"#
            )),
        )
        .await;
    assert_eq!(paired_admin.status, 201, "{}", paired_admin.body);
    let admin_token = paired_admin.json()["access_token"]
        .as_str()
        .expect("an access token")
        .to_owned();

    let (chatty_id, chatty_code) = harness.device_with_code("iPhone");
    let paired_chatty = harness
        .request(
            "POST",
            "/v1/pair",
            &[],
            Some(&format!(
                r#"{{"code":"{chatty_code}","device_name":"iPhone"}}"#
            )),
        )
        .await;
    assert_eq!(paired_chatty.status, 201, "{}", paired_chatty.body);

    let listed = harness
        .request(
            "GET",
            "/v1/devices",
            &[("authorization", format!("Bearer {admin_token}"))],
            None,
        )
        .await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    let body = listed.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["truncated"], false);
    assert_eq!(body["count"], 2);
    let mut top: Vec<&str> = body
        .as_object()
        .expect("a body object")
        .keys()
        .map(String::as_str)
        .collect();
    top.sort_unstable();
    assert_eq!(
        top,
        vec!["count", "devices", "ok", "truncated"],
        "the body is the documented shape and nothing else: {body}"
    );
    let rows = body["devices"].as_array().expect("a device list");
    assert_eq!(rows.len(), 2);

    // The row shape is the documented one and nothing else.
    for row in rows {
        let mut keys: Vec<&str> = row
            .as_object()
            .expect("a device object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "can_admin",
                "created_at",
                "id",
                "last_seen",
                "name",
                "paired"
            ],
            "the row is the documented shape and nothing else: {row}"
        );
        assert!(
            row["created_at"]
                .as_str()
                .is_some_and(|at| at.contains('T')),
            "created_at is the row's own stamp: {row}"
        );
    }

    let find = |id: &str| {
        rows.iter()
            .find(|row| row["id"] == id)
            .unwrap_or_else(|| panic!("{id} is missing from the list: {body}"))
            .clone()
    };
    let admin_row = find(&admin_id);
    assert_eq!(admin_row["name"], "iPad");
    assert_eq!(admin_row["paired"], true);
    assert_eq!(
        admin_row["can_admin"], true,
        "the ticked box is a fact the list reports: {admin_row}"
    );
    let chatty_row = find(&chatty_id);
    assert_eq!(chatty_row["paired"], true);
    assert_eq!(
        chatty_row["can_admin"], false,
        "an ordinary code grants no administration: {chatty_row}"
    );

    // No activity has been recorded for either device, so the honest answer is
    // null rather than a stamp invented for the field.
    assert_eq!(
        admin_row["last_seen"],
        serde_json::Value::Null,
        "nothing recorded means null: {admin_row}"
    );

    // Once the daemon records an audited change made as that device, the derived
    // stamp appears, and it is the recorded one rather than a fresh clock read.
    harness
        .engine
        .auth()
        .audit(
            &chatty_id,
            "settings.persona",
            Some("enabled=false; characters=0"),
        )
        .expect("an audit row");
    let recorded: String = harness
        .engine
        .db()
        .with_conn(|conn| {
            conn.query_row(
                "SELECT occurred_at FROM audit_log WHERE actor = ?1 ORDER BY id DESC LIMIT 1",
                [chatty_id.as_str()],
                |row| row.get(0),
            )
        })
        .expect("the recorded stamp");
    let again = harness
        .request(
            "GET",
            "/v1/devices",
            &[("authorization", format!("Bearer {admin_token}"))],
            None,
        )
        .await;
    assert_eq!(again.status, 200, "{}", again.body);
    let again_body = again.json();
    let chatty_again = again_body["devices"]
        .as_array()
        .expect("a list")
        .iter()
        .find(|row| row["id"] == chatty_id)
        .expect("the chatty device")
        .clone();
    assert_eq!(
        chatty_again["last_seen"],
        serde_json::Value::String(recorded),
        "last_seen is the stamp the daemon wrote: {chatty_again}"
    );

    // Newest first: the stamps the list reports never increase down the page.
    let stamps: Vec<&str> = again_body["devices"]
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|row| row["created_at"].as_str())
        .collect();
    assert!(
        stamps.windows(2).all(|pair| pair[0] >= pair[1]),
        "the list is newest first: {stamps:?}"
    );

    harness.stop();
}

#[tokio::test]
async fn the_device_list_is_capped_at_two_hundred_and_says_so() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, admin_token) = harness.device_with_admin_token("iPad");
    let store = AuthStore::new(harness.engine.db().clone());

    // The oldest row first, so the cap has a row it has to drop.
    let oldest = store
        .register_device("oldest", "ios")
        .expect("the oldest row");
    let mut newest = oldest.clone();
    for index in 0..204 {
        newest = store
            .register_device(&format!("phone-{index}"), "ios")
            .expect("a device row");
    }

    let listed = harness
        .request(
            "GET",
            "/v1/devices",
            &[("authorization", format!("Bearer {admin_token}"))],
            None,
        )
        .await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    let body = listed.json();
    assert_eq!(body["count"], 200);
    assert_eq!(body["truncated"], true);
    let ids: Vec<&str> = body["devices"]
        .as_array()
        .expect("a list")
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect();
    assert_eq!(ids.len(), 200);
    assert!(
        !ids.contains(&oldest.as_str()),
        "the oldest device is what falls off the capped page"
    );
    assert!(
        ids.contains(&newest.as_str()),
        "the device just registered is on the page"
    );

    harness.stop();
}

#[tokio::test]
async fn revoking_a_device_settles_the_row_audits_the_actor_and_kills_its_token() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (actor_id, admin_token) = harness.device_with_admin_token("iPad");
    let (victim_id, victim_token) = harness.device_with_admin_token("iPhone");
    let auth = [("authorization", format!("Bearer {admin_token}"))];

    let revoked = harness
        .request(
            "POST",
            &format!("/v1/devices/{victim_id}/revoke"),
            &auth,
            None,
        )
        .await;
    assert_eq!(revoked.status, 200, "{}", revoked.body);
    let body = revoked.json();
    let mut keys: Vec<&str> = body
        .as_object()
        .expect("a body object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["id", "ok", "revoked"],
        "the body is the documented shape and nothing else: {body}"
    );
    assert_eq!(body["ok"], true);
    assert_eq!(body["id"], victim_id.as_str());
    assert_eq!(
        body["revoked"], 1,
        "the one live token of that device moved: {body}"
    );

    // The row is settled, and every token of it is retired.
    let row = AuthStore::new(harness.engine.db().clone())
        .device(&victim_id)
        .expect("the device read")
        .expect("a device row");
    assert!(row.revoked_at.is_some(), "the device row carries a stamp");
    let dead = harness
        .request(
            "GET",
            "/v1/devices",
            &[("authorization", format!("Bearer {victim_token}"))],
            None,
        )
        .await;
    assert_eq!(
        dead.status, 401,
        "a revoked device's own token stops verifying: {}",
        dead.body
    );

    // The trail: the acting device is the actor, the action is the browser's own
    // name, and the detail is the device that moved. One history, one place.
    let entries = web_store::audit_recent(harness.engine.db(), 5).expect("the recent audit rows");
    let newest = entries.first().expect("a row");
    assert_eq!(newest.action, "dashboard.device_revoked");
    assert_eq!(newest.actor, actor_id);
    assert_eq!(newest.detail.as_deref(), Some(victim_id.as_str()));
    assert!(
        entries
            .iter()
            .any(|entry| entry.action == "auth.revoke_device"),
        "the store's own row is written too, so the browser history is complete: {entries:?}"
    );

    // A device may revoke the device whose token carried the call. Refusing it
    // would be a device able to keep itself unrevocable.
    let itself = harness
        .request(
            "POST",
            &format!("/v1/devices/{actor_id}/revoke"),
            &auth,
            None,
        )
        .await;
    assert_eq!(
        itself.status, 200,
        "a device may revoke itself: {}",
        itself.body
    );
    assert_eq!(itself.json()["revoked"], 1, "its own access token moved");
    let afterwards = harness.request("GET", "/v1/audit", &auth, None).await;
    assert_eq!(
        afterwards.status, 401,
        "the token that made the call stops verifying on the next request: {}",
        afterwards.body
    );

    harness.stop();
}

#[tokio::test]
async fn an_unknown_device_id_is_a_404_by_code() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, admin_token) = harness.device_with_admin_token("iPad");
    let auth = [("authorization", format!("Bearer {admin_token}"))];

    let missing = harness
        .request("POST", "/v1/devices/no-such-device/revoke", &auth, None)
        .await;
    assert_eq!(missing.status, 404, "{}", missing.body);
    assert_eq!(missing.json()["code"], "device_not_found");

    harness.stop();
}

#[tokio::test]
async fn the_audit_read_is_newest_first_bounded_and_reports_the_real_total() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, admin_token) = harness.device_with_admin_token("iPad");
    let auth = [("authorization", format!("Bearer {admin_token}"))];

    // More rows than any page this route will serve.
    for index in 0..205 {
        harness
            .engine
            .auth()
            .audit("loopback", "p8.audit", Some(&format!("n={index}")))
            .expect("an audit row");
    }
    let total = web_store::audit_count(harness.engine.db()).expect("the row count");
    assert!(
        total > 200,
        "the fixture has more rows than the cap: {total}"
    );

    // The default page: fifty rows, newest first, and the real total beside it.
    let page = harness.request("GET", "/v1/audit", &auth, None).await;
    assert_eq!(page.status, 200, "{}", page.body);
    let body = page.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["count"], 50);
    assert_eq!(body["total"], total);
    let mut top: Vec<&str> = body
        .as_object()
        .expect("a body object")
        .keys()
        .map(String::as_str)
        .collect();
    top.sort_unstable();
    assert_eq!(
        top,
        vec!["count", "entries", "ok", "total"],
        "the body is the documented shape and nothing else: {body}"
    );
    let entries = body["entries"].as_array().expect("an entry list");
    assert_eq!(entries.len(), 50);
    for entry in entries {
        let mut keys: Vec<&str> = entry
            .as_object()
            .expect("an entry object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec!["action", "actor", "at", "detail"],
            "the entry is the documented shape and nothing else: {entry}"
        );
        assert!(
            entry["at"].as_str().is_some_and(|at| at.contains('T')),
            "at is the row's own stamp: {entry}"
        );
    }
    assert_eq!(entries[0]["detail"], "n=204", "the newest row is first");
    assert_eq!(entries[1]["detail"], "n=203");

    // A named page is honoured.
    let five = harness
        .request("GET", "/v1/audit?limit=5", &auth, None)
        .await;
    assert_eq!(five.status, 200, "{}", five.body);
    let five_body = five.json();
    assert_eq!(five_body["count"], 5);
    assert_eq!(five_body["total"], total);
    let five_entries = five_body["entries"].as_array().expect("entries");
    assert_eq!(five_entries[0]["detail"], "n=204");
    assert_eq!(five_entries[4]["detail"], "n=200");

    // A page larger than the cap is capped, not refused.
    let capped = harness
        .request("GET", "/v1/audit?limit=500", &auth, None)
        .await;
    assert_eq!(capped.status, 200, "{}", capped.body);
    let capped_body = capped.json();
    assert_eq!(capped_body["count"], 200);
    assert_eq!(capped_body["total"], total);

    // A limit that is not a positive number is named, not silently defaulted.
    for bad in ["abc", "0", "-1"] {
        let refused = harness
            .request("GET", &format!("/v1/audit?limit={bad}"), &auth, None)
            .await;
        assert_eq!(refused.status, 400, "limit={bad}: {}", refused.body);
        assert_eq!(refused.json()["code"], "limit_invalid");
    }

    harness.stop();
}

#[tokio::test]
async fn the_browser_devices_page_still_lists_and_revokes() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (device_id, _) = harness.device_with_admin_token("iPad");
    let app = eris_host::admin::router_with(eris_host::admin::AppState::new(
        harness.engine.clone(),
        false,
    ));
    let cookie = browser_cookie(&harness);

    let (status, page) = browser_page(&app, &cookie, "/dashboard/devices").await;
    assert_eq!(status, 200, "{page}");
    assert!(page.contains("iPad"), "the device is listed: {page}");
    assert!(
        page.contains("can administer"),
        "the access column still states the token's scope: {page}"
    );
    assert!(
        page.contains(r#"action="/dashboard/device/revoke""#),
        "the revoke form is still on the page: {page}"
    );
    assert!(
        !page.replace("<!DOCTYPE html>", "").contains('!'),
        "house style holds on this page"
    );

    // The button still revokes, and the page reads the result back.
    let (status, after) = browser_post(
        &app,
        &cookie,
        "/dashboard/device/revoke",
        &format!("device_id={device_id}"),
    )
    .await;
    assert_eq!(status, 200, "{after}");
    assert!(
        after.contains("revoked 1 token(s)"),
        "the browser path reports what it revoked: {after}"
    );
    let row = AuthStore::new(harness.engine.db().clone())
        .device(&device_id)
        .expect("the device read")
        .expect("a device row");
    assert!(row.revoked_at.is_some());

    let (status, page) = browser_page(&app, &cookie, "/dashboard/devices").await;
    assert_eq!(status, 200, "{page}");
    assert!(
        !page.contains(r#"action="/dashboard/device/revoke""#),
        "a revoked device offers no second revoke: {page}"
    );

    harness.stop();
}

#[tokio::test]
async fn the_browser_activity_page_still_reads_the_trail_a_device_writes() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (actor_id, admin_token) = harness.device_with_admin_token("iPad");
    let (victim_id, _) = harness.device_with_admin_token("iPhone");

    // The device surface revokes, and the browser page reads the same row.
    let revoked = harness
        .request(
            "POST",
            &format!("/v1/devices/{victim_id}/revoke"),
            &[("authorization", format!("Bearer {admin_token}"))],
            None,
        )
        .await;
    assert_eq!(revoked.status, 200, "{}", revoked.body);

    let app = eris_host::admin::router_with(eris_host::admin::AppState::new(
        harness.engine.clone(),
        false,
    ));
    let cookie = browser_cookie(&harness);
    let (status, page) = browser_page(&app, &cookie, "/dashboard/activity").await;
    assert_eq!(status, 200, "{page}");
    assert!(
        page.contains("dashboard.device_revoked"),
        "the action a device wrote is on the page: {page}"
    );
    assert!(
        page.contains(&actor_id),
        "the acting device is the actor on the page: {page}"
    );
    assert!(
        page.contains(&victim_id),
        "the revoked device is the detail on the page: {page}"
    );
    assert!(
        !page.replace("<!DOCTYPE html>", "").contains('!'),
        "house style holds on this page"
    );

    harness.stop();
}

#[tokio::test]
async fn the_persona_reads_and_writes_the_same_file_the_settings_page_uses() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;
    let (_, admin_token) = harness.device_with_admin_token("iPad");
    let auth = [("authorization", format!("Bearer {admin_token}"))];
    let path = eris_host::config::Paths::for_root(harness._dir.path().join("root")).persona_file();

    // Nothing written yet: an empty body, the switch, and the cap.
    let empty = harness.request("GET", "/v1/persona", &auth, None).await;
    assert_eq!(empty.status, 200, "{}", empty.body);
    let body = empty.json();
    let mut keys: Vec<&str> = body
        .as_object()
        .expect("a body object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["enabled", "limit", "ok", "persona"],
        "the read body is the documented shape and nothing else: {body}"
    );
    assert_eq!(body["ok"], true);
    assert_eq!(body["persona"], "");
    assert_eq!(body["enabled"], true, "the documented default is on");
    assert_eq!(
        body["limit"].as_u64(),
        Some(eris_host::persona::MAX_PERSONA_BYTES as u64),
        "the cap is the one the settings page enforces: {body}"
    );
    assert!(!path.exists(), "reading does not create the file");

    let written = harness
        .request(
            "POST",
            "/v1/persona",
            &auth,
            Some(r#"{"persona":"You are Kallisti, the operator's chief of staff."}"#),
        )
        .await;
    assert_eq!(written.status, 200, "{}", written.body);
    let body = written.json();
    let mut keys: Vec<&str> = body
        .as_object()
        .expect("a body object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["characters", "enabled", "ok"],
        "the write body is the documented shape and nothing else: {body}"
    );
    assert_eq!(body["ok"], true);
    assert_eq!(
        body["characters"].as_u64(),
        Some(
            "You are Kallisti, the operator's chief of staff."
                .chars()
                .count() as u64
        )
    );
    assert_eq!(
        std::fs::read_to_string(&path).expect("the persona file"),
        "You are Kallisti, the operator's chief of staff.",
        "the text lands in the file the next turn reads"
    );

    // The trail carries the count and the switch, never the text.
    let entries = web_store::audit_recent(harness.engine.db(), 1).expect("the newest row");
    let newest = entries.first().expect("a row");
    assert_eq!(newest.action, "settings.persona");
    let detail = newest.detail.as_deref().expect("a detail");
    assert!(detail.contains("characters="), "{detail}");
    assert!(
        !detail.contains("chief of staff"),
        "the operator's words are not in the trail: {detail}"
    );

    // Over the cap is refused with the count and the cap, and nothing is written.
    let oversized = "x".repeat(eris_host::persona::MAX_PERSONA_BYTES + 1);
    let refused = harness
        .request(
            "POST",
            "/v1/persona",
            &auth,
            Some(&serde_json::json!({ "persona": oversized }).to_string()),
        )
        .await;
    assert_eq!(refused.status, 422, "{}", refused.body);
    assert_eq!(refused.json()["code"], "persona_too_large");
    assert_eq!(
        std::fs::read_to_string(&path).expect("the persona file"),
        "You are Kallisti, the operator's chief of staff.",
        "a refused save leaves the file alone"
    );

    // An enabled persona with no text is the same refusal the settings page makes.
    let blank = harness
        .request("POST", "/v1/persona", &auth, Some(r#"{"persona":"   "}"#))
        .await;
    assert_eq!(blank.status, 422, "{}", blank.body);
    assert_eq!(blank.json()["code"], "persona_empty");

    // Clearing it while the persona is off is allowed, because that is the state
    // that actually does nothing rather than claiming to do something.
    web_store::settings_put(
        harness.engine.db(),
        eris_host::persona::SETTING_PERSONA_ENABLED,
        "false",
    )
    .expect("the switch");
    let cleared = harness
        .request("POST", "/v1/persona", &auth, Some(r#"{"persona":""}"#))
        .await;
    assert_eq!(cleared.status, 200, "{}", cleared.body);
    assert_eq!(cleared.json()["enabled"], false);
    assert_eq!(
        std::fs::read_to_string(&path).expect("the persona file"),
        "",
        "an explicit empty field clears the text"
    );

    harness.stop();
}

/// A client with no credential can still learn what it is talking to, which is the
/// first question worth answering when pairing misbehaves.
#[tokio::test]
async fn the_daemon_says_what_it_is_without_a_credential() {
    let provider = StubProvider::start(&["o", "k"], Duration::ZERO).await;
    let harness = Harness::start(&provider.url, DEFAULT_SSE_BUFFER).await;

    let reply = harness.request("GET", "/v1/version", &[], None).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let body = reply.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["api"], "v1");
    assert_eq!(body["version"], eris_host::admin::VERSION);
    assert_eq!(body["build"], eris_host::admin::BUILD);
    let frames = body["frames"].as_array().expect("the frame vocabulary");
    assert_eq!(frames.len(), 6, "every frame kind is named: {body}");
    assert!(
        frames.iter().any(|f| f == "reasoning") && frames.iter().any(|f| f == "tool"),
        "a client can learn that reasoning and tools exist: {body}"
    );
    // Nothing here is a secret: no token, no key, no address of anything internal.
    let text = reply.body.to_ascii_lowercase();
    for forbidden in ["token", "key", "password", "secret"] {
        assert!(
            !text.contains(forbidden),
            "the version answer carries no {forbidden}: {}",
            reply.body
        );
    }

    harness.stop();
}
