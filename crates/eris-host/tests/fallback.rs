//! The fallback role: a pinned model retries a turn the primary model failed.
//!
//! Two providers over real loopback TCP, so the seam is the same `OpenAiCompat`
//! stream the daemon drives: one answers 500, one streams a completion. The turn
//! machine itself is untouched - only the resilient wrapper's retry decision is
//! under test here.

use core_runtime::provider::{ChatMessage, ChatRole, SecretKey, SecretRef};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use eris_host::onboarding::permissions::SandboxMode;
use eris_host::tools::Sandbox;
use eris_host::web::chat::{
    GateDecision, ResilientNote, StreamPart, TurnConfig, TurnSettings, TurnSignal,
    run_turn_resilient_with_keys,
};

/// A provider that accepts the request and answers 500.
async fn failing_provider() -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a failing provider listener");
    let addr = listener.local_addr().expect("a failing provider address");
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut scratch = [0u8; 4096];
                let _ = socket.read(&mut scratch).await;
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                    )
                    .await;
            });
        }
    });
    format!("http://{addr}/v1")
}

/// A provider that streams the answer `ok` in one delta.
async fn ok_provider(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ok provider listener");
    let addr = listener.local_addr().expect("an ok provider address");
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut scratch = [0u8; 4096];
                let _ = socket.read(&mut scratch).await;
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n",
                    )
                    .await;
                let frame = format!(
                    "data: {}\n\ndata: [DONE]\n\n",
                    serde_json::json!({
                        "choices": [{ "index": 0, "delta": { "content": body } }]
                    })
                );
                let _ = socket.write_all(frame.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    format!("http://{addr}/v1")
}

fn config(url: String, model: &str) -> TurnConfig {
    TurnConfig {
        base_url: url,
        key_ref: SecretRef::Env("KALLISTI_FALLBACK_TEST_KEY".to_owned()),
        model: model.to_owned(),
        dialect: core_domain::ApiDialect::ChatCompletions,
        reasoning_split: false,
        direct: false,
        project: None,
    }
}

/// The shared arguments around the resilient call, so the two tests stay short.
///
/// The resilient turn is synchronous, but it builds its own current-thread
/// runtime for the provider stream, so it must run off the test runtime - exactly
/// how `spawn_turn` does it in production.
#[allow(clippy::too_many_arguments)]
async fn run(
    primary: TurnConfig,
    fallback: Option<TurnConfig>,
) -> (
    Result<eris_host::web::chat::TurnOutcome, eris_host::web::chat::TurnFailure>,
    ResilientNote,
) {
    tokio::task::spawn_blocking(move || {
        let dir = tempfile::tempdir().expect("a temp workspace");
        let sandbox = Sandbox::new(dir.path().to_path_buf(), SandboxMode::ReadOnly);
        let key = SecretKey::new("test-key").expect("a resolved key");
        let mut gate = |_call: &core_domain::ToolCall| GateDecision::Run;
        let mut poll = || TurnSignal::Continue;
        let mut sink = |_part: StreamPart<'_>| {};
        run_turn_resilient_with_keys(
            &primary,
            &key,
            fallback.as_ref(),
            fallback.as_ref().map(|_| &key),
            None,
            vec![ChatMessage::text(ChatRole::User, "hi")],
            &sandbox,
            &mut gate,
            None,
            None,
            None,
            None,
            // Nothing to attach a file to in this suite: the fallback path is
            // what is under test.
            None,
            // No skill library in this suite: the fallback path is what is under
            // test, and a catalog would only add prompt text to compare.
            None,
            TurnSettings {
                tools: false,
                max_rounds: 1,
                agents: false,
                // No window in this suite: the fallback path is about retries,
                // and a budget would only make the prompt a moving target.
                context_budget: None,
            },
            // No operator to ask: this suite is about the fallback path, and a
            // parked question would block the turn it is trying to time.
            None,
            &mut poll,
            &mut sink,
        )
    })
    .await
    .expect("the resilient turn returns")
}

#[tokio::test]
async fn a_pinned_fallback_retries_a_turn_the_primary_failed() {
    let primary = config(failing_provider().await, "primary");
    let fallback = config(ok_provider("ok").await, "fallback");

    let (outcome, note) = run(primary, Some(fallback)).await;

    let turn = outcome.expect("the fallback answer lands");
    assert_eq!(turn.text, "ok");
    assert!(
        matches!(
            note,
            ResilientNote::Retried {
                ref primary_model,
                ref primary_code,
                ref model,
            } if primary_model == "primary" && model == "fallback" && !primary_code.is_empty()
        ),
        "the retry note names the failing primary and the answering model: {note:?}"
    );
}

#[tokio::test]
async fn without_a_pinned_fallback_the_primary_failure_stands() {
    let primary = config(failing_provider().await, "primary");

    let (outcome, note) = run(primary, None).await;

    assert!(
        outcome.is_err(),
        "no fallback means the failure is returned"
    );
    assert_eq!(note, ResilientNote::Primary);
}

#[tokio::test]
async fn a_healthy_primary_is_not_touched_by_a_pinned_fallback() {
    let primary = config(ok_provider("primary-ok").await, "primary");
    let fallback = config(ok_provider("fallback-ok").await, "fallback");

    let (outcome, note) = run(primary, Some(fallback)).await;

    assert_eq!(outcome.expect("the primary answers").text, "primary-ok");
    assert_eq!(note, ResilientNote::Primary);
}
