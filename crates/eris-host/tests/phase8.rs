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

use eris_host::auth::{AuthStore, PairingService, ScopeSet, TokenOrigin, TokenService};
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
        http(self.addr, method, path, headers, body).await
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
        request.push_str("content-type: application/json\r\n");
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
