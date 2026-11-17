//! A gateway that drops the connection mid-conversation must not take the turn
//! with it.
//!
//! This is the failure that killed a real turn on 2026-10-22: twenty-two rounds
//! of one conversation went through 9router, the twenty-third was written into a
//! socket the gateway had already closed, and the turn failed with "connection
//! closed before message completed" before a single response byte arrived. The
//! gateway's own log had no line for the request at all, because the request
//! never reached its handler.
//!
//! The server here is that gateway, deliberately: it answers the first request,
//! then drops the connection under the next one without answering, then answers
//! normally. A client that gives up on the drop fails this test; a client that
//! puts the request on a fresh connection passes it.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use core_domain::{Capabilities, ModelProfile, ProviderProfile, SecretRef};
use core_runtime::provider::{ChatRequest, OpenAiCompatProvider, ProviderEvent, SecretKey};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// One complete OpenAI-shaped streaming completion.
const REPLY: &str = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";

/// Read one HTTP request head and any announced body.
async fn read_request(stream: &mut TcpStream) -> Option<String> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        if let Some(head_end) = head_end(&buffer) {
            let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
            let body_bytes: usize = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().ok())
                        .flatten()
                })
                .unwrap_or(0);
            while buffer.len() < head_end + 4 + body_bytes {
                let read = stream.read(&mut chunk).await.ok()?;
                if read == 0 {
                    return None;
                }
                buffer.extend_from_slice(&chunk[..read]);
            }
            return Some(head);
        }
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
}

fn head_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

async fn answer(stream: &mut TcpStream) {
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
        REPLY.len(),
        REPLY
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
}

fn provider_on(port: u16) -> OpenAiCompatProvider {
    let profile = ProviderProfile::new(
        "gateway",
        format!("http://127.0.0.1:{port}/v1"),
        SecretRef::Env("UNUSED_IN_THIS_TEST".to_owned()),
    )
    .expect("profile should build");
    OpenAiCompatProvider::new(
        profile,
        ModelProfile::new("test/model"),
        Capabilities::openai_compatible(),
    )
    .expect("provider should build")
}

#[tokio::test]
async fn a_connection_dropped_before_the_answer_does_not_fail_the_turn() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let seen = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));

    tokio::spawn({
        let seen = Arc::clone(&seen);
        let dropped = Arc::clone(&dropped);
        async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                let seen = Arc::clone(&seen);
                let dropped = Arc::clone(&dropped);
                tokio::spawn(async move {
                    while read_request(&mut stream).await.is_some() {
                        let index = seen.fetch_add(1, Ordering::SeqCst) + 1;
                        if index == 2 {
                            dropped.fetch_add(1, Ordering::SeqCst);
                            drop(stream);
                            return;
                        }
                        answer(&mut stream).await;
                    }
                });
            }
        }
    });

    let provider = provider_on(port);
    let key = SecretKey::new("test-key").expect("key");
    let mut text = String::new();
    let mut sink = |event: ProviderEvent| {
        if let ProviderEvent::Text(chunk) = event {
            text.push_str(&chunk);
        }
    };

    let first = provider
        .stream_chat(&ChatRequest::user_text("first"), &key, &mut sink)
        .await;
    assert!(first.is_ok(), "the first turn must succeed: {first:?}");

    let second = provider
        .stream_chat(&ChatRequest::user_text("second"), &key, &mut sink)
        .await;
    assert!(
        second.is_ok(),
        "a request the gateway dropped before answering must be retried once, \
         not reported as a dead turn: {second:?}"
    );
    assert_eq!(
        text, "okok",
        "the retried request must deliver the answer, not a duplicate or an empty reply"
    );
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "the fixture must actually have dropped exactly one connection"
    );
    assert!(
        seen.load(Ordering::SeqCst) >= 3,
        "the retried request must have reached the server on a fresh connection"
    );
}
