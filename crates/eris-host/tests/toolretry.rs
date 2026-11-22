//! A tool call the provider cut off before its JSON closed is retried with a
//! correction, not replayed.
//!
//! One stub provider over real loopback TCP answers the same turn more than once,
//! so the retry under test is the one the daemon drives: the same
//! `OpenAiCompatProvider` stream, the same parser, the same turn loop. Each
//! request body is retained, which is how a test can read the correction the
//! model was actually sent rather than trust that it was built.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use core_runtime::provider::{ChatMessage, ChatRole, SecretKey, SecretRef};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use eris_host::onboarding::permissions::SandboxMode;
use eris_host::tools::Sandbox;
use eris_host::web::chat::{
    GateDecision, StreamPart, TurnConfig, TurnFailure, TurnOutcome, TurnSettings, TurnSignal,
    run_turn_with_key,
};

/// One SSE frame.
fn frame(value: serde_json::Value) -> String {
    format!("data: {value}\n\n")
}

/// A reply whose `write_file` call is cut off mid-string, exactly like the turn
/// that produced the EOF parse error. `finish_reason` is what the provider says
/// about the cut: `length` for an output limit, `None` for a transport that just
/// ended.
fn truncated_call(finish_reason: Option<&str>) -> String {
    // The buffer never closes its string or its object.
    let partial = "{\"path\":\"parts.txt\",\"content\":\"the first half of a long file";
    let arguments = serde_json::Value::String(partial.to_owned());
    let mut reply = frame(serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": "call-cut",
                    "type": "function",
                    "function": { "name": "write_file", "arguments": arguments }
                }]
            }
        }]
    }));
    if let Some(reason) = finish_reason {
        reply.push_str(&frame(serde_json::json!({
            "choices": [{ "index": 0, "delta": {}, "finish_reason": reason }]
        })));
    }
    reply.push_str("data: [DONE]\n\n");
    reply
}

/// A complete `write_file` call, the reply a corrected retry earns.
fn good_write_call() -> String {
    let arguments = serde_json::to_string(&serde_json::json!({
        "path": "parts.txt",
        "content": "hello"
    }))
    .expect("the arguments serialise");
    let mut reply = frame(serde_json::json!({
        "choices": [{
            "index": 0,
            "delta": {
                "role": "assistant",
                "tool_calls": [{
                    "index": 0,
                    "id": "call-good",
                    "type": "function",
                    "function": { "name": "write_file", "arguments": arguments }
                }]
            }
        }]
    }));
    reply.push_str(&frame(serde_json::json!({
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }]
    })));
    reply.push_str("data: [DONE]\n\n");
    reply
}

/// A plain prose answer, the reply that ends a turn.
fn answer(text: &str) -> String {
    let mut reply = frame(serde_json::json!({
        "choices": [{ "index": 0, "delta": { "content": text } }]
    }));
    reply.push_str(&frame(serde_json::json!({
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }]
    })));
    reply.push_str("data: [DONE]\n\n");
    reply
}

/// A provider stub that answers the nth request with the nth scripted reply.
struct Stub {
    url: String,
    hits: Arc<AtomicUsize>,
    /// The raw request head and body each call arrived with, in order.
    bodies: Arc<Mutex<Vec<String>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl Stub {
    async fn start(replies: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a stub provider listener");
        let addr = listener.local_addr().expect("a stub provider address");
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&bodies);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let index = counter.fetch_add(1, Ordering::SeqCst);
                let record = Arc::clone(&recorder);
                let reply = replies.get(index).cloned().unwrap_or_default();
                tokio::spawn(async move {
                    let request = read_full_request(&mut socket).await;
                    if let Ok(mut seen) = record.lock() {
                        seen.push(String::from_utf8_lossy(&request).into_owned());
                    }
                    let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                                cache-control: no-cache\r\nconnection: close\r\n\r\n";
                    if socket.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        Self {
            url: format!("http://{addr}/v1"),
            hits,
            bodies,
            _task: task,
        }
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    fn bodies(&self) -> Vec<String> {
        self.bodies
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }
}

/// Read one full HTTP request: headers plus the body `content-length` names.
async fn read_full_request(socket: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut buffer = Vec::new();
    let mut scratch = [0u8; 8192];
    let mut body_start: Option<usize> = None;
    let mut body_len: Option<usize> = None;
    loop {
        if let (Some(start), Some(len)) = (body_start, body_len)
            && buffer.len() >= start + len
        {
            break;
        }
        let read = socket.read(&mut scratch).await.unwrap_or(0);
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&scratch[..read]);
        if body_start.is_none()
            && let Some(pos) = buffer.windows(4).position(|window| window == b"\r\n\r\n")
        {
            let start = pos + 4;
            let headers = String::from_utf8_lossy(&buffer[..start]);
            let len = headers.lines().find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            });
            body_start = Some(start);
            body_len = Some(len.unwrap_or(0));
        }
    }
    buffer
}

fn config(url: String) -> TurnConfig {
    TurnConfig {
        base_url: url,
        key_ref: SecretRef::Env("KALLISTI_TOOLRETRY_TEST_KEY".to_owned()),
        model: "stub".to_owned(),
        dialect: core_domain::ApiDialect::ChatCompletions,
        reasoning_split: false,
        direct: false,
        project: None,
    }
}

/// Drive one turn against `provider`. The temp workspace is returned alive so a
/// test can inspect what a call actually wrote.
async fn run(
    provider: String,
    max_rounds: usize,
) -> (Result<TurnOutcome, TurnFailure>, tempfile::TempDir) {
    tokio::task::spawn_blocking(move || {
        let dir = tempfile::tempdir().expect("a temp workspace");
        let sandbox = Sandbox::new(dir.path().to_path_buf(), SandboxMode::WorkspaceWrite);
        let key = SecretKey::new("test-key").expect("a resolved key");
        let mut gate = |_call: &core_domain::ToolCall| GateDecision::Run;
        let mut poll = || TurnSignal::Continue;
        let mut sink = |_part: StreamPart<'_>| {};
        let outcome = run_turn_with_key(
            &config(provider),
            &key,
            // A system prompt, as the live route always passes. Without one the
            // wire body folds every inline system message into the top-level
            // `system` field, which would hide the correction's position and
            // role from this suite.
            Some("You are a test turn.".to_owned()),
            vec![ChatMessage::text(ChatRole::User, "write parts.txt")],
            &sandbox,
            &mut gate,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            TurnSettings {
                tools: true,
                max_rounds,
                agents: false,
                index: false,
                context_budget: None,
                db: None,
                conversation: None,
                soft_cap: None,
            },
            None,
            &mut poll,
            &mut sink,
        );
        (outcome, dir)
    })
    .await
    .expect("the turn returns")
}

/// The first correction, as a reader of the request body sees it.
fn asserts_first_correction(body: &str) {
    assert!(
        body.contains("cut off before its JSON closed"),
        "the correction says what happened: {body}"
    );
    assert!(
        body.contains("Re-issue that call now"),
        "the correction asks for the call again: {body}"
    );
    assert!(
        body.contains("write it in smaller parts"),
        "the correction names the split for a large payload: {body}"
    );
}

/// The JSON body of one recorded HTTP request.
fn request_json(body: &str) -> serde_json::Value {
    let (_, payload) = body
        .split_once("\r\n\r\n")
        .expect("a recorded request has a header/body split");
    serde_json::from_str(payload).expect("the request body is JSON")
}

#[tokio::test]
async fn a_truncated_call_is_retried_once_with_a_correction() {
    let stub = Stub::start(vec![
        truncated_call(Some("length")),
        good_write_call(),
        answer("wrote it"),
    ])
    .await;

    let (outcome, dir) = run(stub.url.clone(), 4).await;
    let turn = outcome.expect("the corrected retry answers");
    assert_eq!(turn.text, "wrote it");
    assert_eq!(turn.tools.len(), 1, "the re-issued call ran");
    assert_eq!(turn.tools[0].name, "write_file");
    assert!(
        dir.path().join("parts.txt").exists(),
        "the good call landed on disk"
    );
    assert_eq!(
        stub.hits(),
        3,
        "one truncation costs exactly one extra request"
    );

    let bodies = stub.bodies();
    assert!(
        !bodies[0].contains("Re-issue that call now"),
        "the first request cannot carry a correction for a failure that had not happened"
    );
    asserts_first_correction(&bodies[1]);
    assert!(
        !bodies[1].contains("one file per call"),
        "the stronger second correction is for the second failure only: {}",
        bodies[1]
    );

    // The correction is an instruction the model is steered by, not an answer
    // it wrote: a system message appended after everything already in the
    // turn, so it is the last thing the retried request says.
    let retried = request_json(&bodies[1]);
    let messages = retried["messages"]
        .as_array()
        .expect("the request carries a message array");
    let correction = messages.last().expect("the correction is the last message");
    assert_eq!(
        correction["role"], "system",
        "the correction is a system message: {correction}"
    );
    assert!(
        correction["content"]
            .as_str()
            .unwrap_or_default()
            .contains("Re-issue that call now"),
        "the retried request ends with the correction: {correction}"
    );
}

#[tokio::test]
async fn two_truncations_spend_the_budget_and_fail_with_instructions() {
    let stub = Stub::start(vec![
        truncated_call(Some("length")),
        truncated_call(Some("length")),
        truncated_call(Some("length")),
    ])
    .await;

    let (outcome, _dir) = run(stub.url.clone(), 4).await;
    let (_code, message) = outcome.expect_err("a third truncation ends the turn");
    assert_eq!(stub.hits(), 3, "two retries, and not a third");
    assert!(
        message.contains("split"),
        "the operator is told what the model was asked to do: {message}"
    );
    assert!(
        message.contains("nothing ran"),
        "and that nothing happened: {message}"
    );
    assert!(
        !message.starts_with("protocol failure"),
        "a failed row is not a bare parser message: {message}"
    );

    let bodies = stub.bodies();
    asserts_first_correction(&bodies[1]);
    assert!(
        bodies[2].contains("one file per call"),
        "the second correction demands the split: {}",
        bodies[2]
    );
    assert!(
        bodies[2].contains("smaller than the output limit"),
        "and names the limit: {}",
        bodies[2]
    );
}

#[tokio::test]
async fn a_cut_off_answer_is_not_retried() {
    // Two good tool rounds spend the budget; the third request is the answer
    // round, where tools are off. A cut there is not a tool call to correct, so
    // it must not buy a retry.
    let stub = Stub::start(vec![
        good_write_call(),
        good_write_call(),
        truncated_call(Some("length")),
        answer("summary"),
    ])
    .await;

    let (outcome, _dir) = run(stub.url.clone(), 3).await;
    let turn = outcome.expect("the forced answer still lands");
    assert_eq!(turn.text, "summary");
    assert!(turn.capped, "the answer was forced, not written");
    assert_eq!(
        stub.hits(),
        4,
        "three rounds plus the forced answer, with no retry after the cut"
    );
    let bodies = stub.bodies();
    assert!(
        bodies
            .iter()
            .all(|body| !body.contains("Re-issue that call now")),
        "no round in this turn was retried: {bodies:?}"
    );
}

#[tokio::test]
async fn the_failure_names_the_provider_finish_reason() {
    // No `finish_reason` at all is a transport that ended, not an output limit.
    let stub = Stub::start(vec![
        truncated_call(None),
        truncated_call(None),
        truncated_call(None),
    ])
    .await;

    let (outcome, _dir) = run(stub.url.clone(), 4).await;
    let (_code, message) = outcome.expect_err("the truncation ends the turn");
    assert!(
        message.contains("finish_reason=none"),
        "the failure names what the provider did and did not say: {message}"
    );
}
