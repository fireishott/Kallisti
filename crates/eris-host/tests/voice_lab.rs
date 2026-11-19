//! The voice lab contract over a real socket.
//!
//! Three refusals, each with its own code, because "it returned a 4xx" is not a
//! contract: a client keys off the code, and a test that accepts any failure
//! cannot tell a missing engine from an empty clip.
//!
//! The live case is gated on the engine unit. A host without the lab - CI, a
//! laptop - skips it rather than failing green tests for a missing feature.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use eris_host::auth::{AuthStore, ScopeSet, TokenOrigin, TokenService};
use eris_host::config::Paths;
use eris_host::db::{Db, SetupState};
use eris_host::device::DeviceService;
use eris_host::device::turns::DEFAULT_SSE_BUFFER;
use eris_host::onboarding::SetupEngine;

const WAV: &str = "audio/wav";
const JSON: &str = "application/json";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct Lab {
    _dir: tempfile::TempDir,
    addr: SocketAddr,
    engine: SetupEngine,
    server: tokio::task::JoinHandle<()>,
}

impl Lab {
    async fn start() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = Paths::for_root(dir.path().join("root"));
        let db = Db::open(&paths.db_file).expect("db opens");
        let engine = SetupEngine::new(db.clone(), paths);
        db.set_setup_state(SetupState::Ready).expect("ready");

        let service = DeviceService::with_key_lookup(
            engine.clone(),
            DEFAULT_SSE_BUFFER,
            Arc::new(|_name: &str| None),
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
            server,
        }
    }

    /// A device and a live token, without going through `/v1/pair`.
    fn token(&self) -> String {
        let db = self.engine.db().clone();
        let device_id = AuthStore::new(db.clone())
            .register_device("voice-lab", "ios")
            .expect("device row");
        TokenService::new(db)
            .mint_access(&device_id, ScopeSet::device_default(), TokenOrigin::Device)
            .expect("access token")
            .token()
            .to_owned()
    }

    async fn post(&self, path: &str, token: &str, content_type: &str, body: &[u8]) -> Reply {
        let mut stream = TcpStream::connect(self.addr).await.expect("connect");
        let mut head = format!(
            "POST {path} HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\n\
             authorization: Bearer {token}\r\ncontent-type: {content_type}\r\n\
             content-length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes();
        head.extend_from_slice(body);
        stream.write_all(&head).await.expect("write the request");

        let mut raw = Vec::new();
        tokio::time::timeout(Duration::from_secs(30), stream.read_to_end(&mut raw))
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
        Reply { status, body }
    }

    fn stop(&self) {
        self.server.abort();
    }
}

struct Reply {
    status: u16,
    body: String,
}

impl Reply {
    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body).unwrap_or_else(|error| {
            panic!(
                "body is not json ({error}): status={} body={}",
                self.status, self.body
            )
        })
    }

    fn code(&self) -> String {
        self.json()
            .get("code")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }
}

/// Decode a chunked body. Axum uses chunked framing when the size is unknown.
fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size, tail)) = rest.split_once("\r\n") {
        let Ok(size) = usize::from_str_radix(size.trim(), 16) else {
            break;
        };
        if size == 0 || tail.len() < size {
            break;
        }
        out.push_str(&tail[..size]);
        rest = tail[size..].trim_start_matches("\r\n");
    }
    out
}

/// Whether the engine unit is running on this host.
fn engine_unit_active() -> bool {
    std::process::Command::new("systemctl")
        .args(["--user", "is-active", "kallisti-voicebox.service"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "active")
        .unwrap_or(false)
}

/// A 0.5 s silent 16 kHz mono clip, header and all.
fn silent_wav() -> Vec<u8> {
    let samples: u32 = 8_000;
    let data_len = samples * 2;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&16_000u32.to_le_bytes());
    wav.extend_from_slice(&32_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.resize(44 + data_len as usize, 0);
    wav
}

// ---------------------------------------------------------------------------
// The three refusals
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_missing_engine_is_refused_by_name() {
    let lab = Lab::start().await;
    let token = lab.token();
    let reply = lab.post("/v1/transcribe", &token, WAV, b"RIFF....").await;
    assert_eq!(reply.status, 503, "body: {}", reply.body);
    assert_eq!(reply.code(), "stt_not_configured");
    lab.stop();
}

#[tokio::test]
async fn an_empty_clip_is_refused_by_name() {
    let lab = Lab::start().await;
    let token = lab.token();
    let reply = lab.post("/v1/transcribe", &token, WAV, b"").await;
    assert_eq!(reply.status, 400, "body: {}", reply.body);
    assert_eq!(reply.code(), "stt_empty");
    lab.stop();
}

#[tokio::test]
async fn a_body_without_the_text_field_is_refused_by_name() {
    let lab = Lab::start().await;
    lab.engine
        .set_setting("tts.enabled", "true")
        .expect("speech on");
    let token = lab.token();
    // A JSON object carrying the wrong field name: the shape a client reaches for
    // when it assumes a JSON body is `{"text": ...}` but names it something else.
    let reply = lab
        .post("/v1/speak", &token, JSON, br#"{"voice":"amy"}"#)
        .await;
    assert_eq!(reply.status, 400, "body: {}", reply.body);
    assert_eq!(reply.code(), "tts_empty");
    lab.stop();
}

/// A JSON object carrying `text` is read as the same field a form carries.
///
/// `read_fields` parses a JSON object into fields before any handler sees it, so
/// the encoding is not what refuses a client: the field name is. With no provider
/// configured the call gets past the shape check and refuses at the next gate,
/// which is the assertion that separates "the body was understood" from "the body
/// was empty".
#[tokio::test]
async fn a_json_object_with_the_text_field_is_understood() {
    let lab = Lab::start().await;
    lab.engine
        .set_setting("tts.enabled", "true")
        .expect("speech on");
    let token = lab.token();
    let reply = lab
        .post("/v1/speak", &token, JSON, br#"{"text":"hi"}"#)
        .await;
    assert_eq!(reply.status, 503, "body: {}", reply.body);
    assert_eq!(reply.code(), "tts_not_configured");
    lab.stop();
}

// ---------------------------------------------------------------------------
// The live case, gated on the unit
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_clip_comes_back_as_text_when_the_engine_is_up() {
    if !engine_unit_active() {
        eprintln!("skipping: kallisti-voicebox.service is not active on this host");
        return;
    }
    let lab = Lab::start().await;
    lab.engine
        .set_setting("tts.base_url", "http://127.0.0.1:7342")
        .expect("engine base url");
    let token = lab.token();
    let reply = lab.post("/v1/transcribe", &token, WAV, &silent_wav()).await;
    assert_eq!(reply.status, 200, "body: {}", reply.body);
    assert!(
        reply.json().get("text").is_some(),
        "the engine body must carry text: {}",
        reply.body
    );
    lab.stop();
}
