//! Recorded-fixture tests for the streaming parser.
//!
//! No test in this file opens a socket. Each one replays a checked-in `.sse`
//! transcript through `eventsource-stream` - the same parser the live adapter
//! uses - in 17-byte chunks, so a frame boundary can fall anywhere inside a
//! JSON payload, an `arguments` fragment, or a UTF-8 sequence. That is the
//! property the adapter depends on and the one a naive `body.split("\n\n")`
//! implementation would break.
//!
//! The two tool-call dialects have one fixture each:
//!
//! - `openai-tool-calls-stream.sse`: `tool_calls[index].function.arguments`
//!   split across three deltas, plus a second call interleaved by index;
//! - `anthropic-input-json-delta-stream.sse`: `content_block_start` of type
//!   `tool_use` followed by `input_json_delta.partial_json` fragments.
//!
//! Both assert the same invariant: a tool call is emitted once, complete, and
//! never before the provider says it is done.

use std::path::{Path, PathBuf};

use core_domain::{CacheReadReporting, Capabilities, LlmError, Usage};
use core_runtime::provider::{ChatStreamParser, Completion, ProviderEvent};
use eventsource_stream::Eventsource;
use tokio_stream::StreamExt;

/// `contract/fixtures/provider`, resolved from the crate root.
fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contract/fixtures/provider")
}

fn fixture(name: &str) -> Vec<u8> {
    let path = fixtures_dir().join(name);
    std::fs::read(&path).unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()))
}

/// One frame, and the events it produced on its own.
struct Frame {
    data: String,
    produced: Vec<ProviderEvent>,
}

struct Replay {
    frames: Vec<Frame>,
    completion: Completion,
}

/// Replay raw SSE bytes the way the adapter does: `eventsource-stream` over an
/// arbitrarily chunked byte stream, one `data:` payload at a time into the
/// parser.
async fn replay_bytes(bytes: &[u8]) -> Replay {
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
        bytes.chunks(17).map(|chunk| Ok(chunk.to_vec())).collect();

    let mut stream = tokio_stream::iter(chunks).eventsource();
    let mut parser = ChatStreamParser::new(Capabilities::openai_compatible());
    let mut frames = Vec::new();

    while let Some(event) = stream.next().await {
        let event = event.expect("the recorded fixture must parse as SSE");
        if event.data.is_empty() {
            continue;
        }
        let produced = parser
            .push(&event.data)
            .unwrap_or_else(|error| panic!("frame must parse: {error}\n{}", event.data));
        frames.push(Frame {
            data: event.data,
            produced,
        });
        if parser.is_done() {
            break;
        }
    }

    let completion = parser.finish().expect("the recorded stream must finish");
    Replay { frames, completion }
}

async fn replay_fixture(name: &str) -> Replay {
    replay_bytes(&fixture(name)).await
}

/// The invariant every tool-call fixture has to hold.
fn assert_no_partial_tool_call(replay: &Replay, fragment_marker: &str) {
    let mut seen_complete = 0;
    for frame in &replay.frames {
        let is_fragment = frame.data.contains(fragment_marker);
        let emitted: Vec<&ProviderEvent> = frame
            .produced
            .iter()
            .filter(|event| matches!(event, ProviderEvent::ToolCall(_)))
            .collect();
        if is_fragment && !emitted.is_empty() {
            // Allowed only when the same frame also carries the fragment that
            // completes the argument buffer, i.e. where the provider is done.
            panic!(
                "a tool call was emitted by a frame that only carried fragments:\n{}",
                frame.data
            );
        }
        seen_complete += emitted.len();
    }
    assert_eq!(
        seen_complete,
        replay.completion.tool_calls.len(),
        "every completed tool call must also have been streamed"
    );
}

#[tokio::test]
async fn openai_argument_fragments_assemble_into_complete_calls() {
    let replay = replay_fixture("openai-tool-calls-stream.sse").await;
    let completion = &replay.completion;

    assert_eq!(completion.text, "Checking ");
    assert_eq!(completion.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(
        completion.tool_calls.len(),
        2,
        "{:?}",
        completion.tool_calls
    );

    let weather = &completion.tool_calls[0];
    assert_eq!(weather.index, 0);
    assert_eq!(weather.id, "call_a");
    assert_eq!(weather.name, "get_weather");
    assert_eq!(
        weather.arguments,
        serde_json::json!({"city": "Paris", "units": "celsius"})
    );
    assert!(weather.arguments.is_object());

    let clock = &completion.tool_calls[1];
    assert_eq!(clock.index, 1);
    assert_eq!(clock.id, "call_b");
    assert_eq!(clock.name, "get_time");
    assert_eq!(
        clock.arguments,
        serde_json::json!({"timezone": "America/New_York"})
    );

    // Fragments are buffered per array index, so the second call is not
    // corrupted by the first one's tail.
    assert_no_partial_tool_call(&replay, "\"arguments\"");

    // Folded-in cache read: 1200 prompt tokens, 900 of them cached.
    assert_eq!(
        completion.usage.to_string(),
        "usage: input=300 output=40 cache_read=900"
    );
    assert_eq!(completion.usage.cache_read_tokens(), Some(900));
}

#[tokio::test]
async fn anthropic_input_json_deltas_assemble_into_one_complete_call() {
    let replay = replay_fixture("anthropic-input-json-delta-stream.sse").await;
    let completion = &replay.completion;

    assert_eq!(completion.text, "Let me check the weather.");
    assert_eq!(completion.finish_reason.as_deref(), Some("tool_use"));
    assert_eq!(
        completion.tool_calls.len(),
        1,
        "{:?}",
        completion.tool_calls
    );

    let call = &completion.tool_calls[0];
    assert_eq!(call.index, 1);
    assert_eq!(call.id, "toolu_p4fixture_01");
    assert_eq!(call.name, "get_weather");
    assert_eq!(
        call.arguments,
        serde_json::json!({"city": "Paris", "units": "celsius"})
    );

    assert_no_partial_tool_call(&replay, "partial_json");

    // Anthropic's cache read sits beside `input_tokens`, not inside it: 214 is
    // already the uncached count, so nothing is subtracted.
    assert_eq!(
        completion.usage.cache_read(),
        core_domain::CacheRead::Reported {
            tokens: 1800,
            folded_into_prompt: false
        }
    );
    assert_eq!(completion.usage.input_tokens(), Some(214));
    assert_eq!(completion.usage.output_tokens(), Some(57));
    assert_eq!(
        completion.usage.to_string(),
        "usage: input=214 output=57 cache_read=1800"
    );
}

#[tokio::test]
async fn a_folded_cache_read_is_subtracted_from_the_streamed_usage() {
    let replay = replay_fixture("usage-folded-cache.sse").await;
    assert_eq!(replay.completion.text, "ok");
    assert_eq!(
        replay.completion.usage.to_string(),
        "usage: input=300 output=40 cache_read=900"
    );
    assert_eq!(replay.completion.usage.input_tokens(), Some(300));
    assert_eq!(replay.completion.usage.total_tokens(), Some(340));
}

#[tokio::test]
async fn an_absent_cache_read_streams_as_provider_unavailable_not_zero() {
    let replay = replay_fixture("usage-cache-absent.sse").await;
    let usage = replay.completion.usage;

    assert_eq!(usage.cache_read_tokens(), None);
    assert_ne!(usage.cache_read_tokens(), Some(0));
    assert!(usage.cache_read().is_unavailable());
    assert!(usage.to_string().contains("cache_read=unavailable"));
    assert_eq!(
        usage.to_string(),
        "usage: input=100 output=5 cache_read=unavailable"
    );
}

#[tokio::test]
async fn a_provider_that_cannot_report_cache_reads_never_yields_a_number() {
    // The same recorded stream, read through a capability set that says this
    // deployment cannot account for cache reads.
    let bytes = fixture("usage-folded-cache.sse");
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
        bytes.chunks(64).map(|chunk| Ok(chunk.to_vec())).collect();
    let mut stream = tokio_stream::iter(chunks).eventsource();
    let mut parser = ChatStreamParser::new(Capabilities::stream_only());

    while let Some(event) = stream.next().await {
        let event = event.expect("fixture must parse");
        if event.data.is_empty() {
            continue;
        }
        parser.push(&event.data).expect("frame must parse");
        if parser.is_done() {
            break;
        }
    }
    let completion = parser.finish().expect("stream must finish");
    assert_eq!(completion.usage.cache_read_tokens(), None);
    assert_eq!(
        completion.usage.cache_read().unavailable_reason(),
        Some(core_domain::UnavailableReason::NotSupportedByProvider)
    );
    assert!(
        completion
            .usage
            .to_string()
            .contains("cache_read=unavailable")
    );
}

#[tokio::test]
async fn a_frame_that_is_not_json_is_a_protocol_error() {
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"}}]}\n\n\
                data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"tru\n\n";
    let error = replay_bytes_expecting_error(body.as_bytes())
        .await
        .expect_err("malformed JSON must not be accepted");
    match error {
        LlmError::Protocol(detail) => assert!(detail.contains("not valid JSON"), "{detail}"),
        other => panic!("expected Protocol, got {other:?}"),
    }
}

#[tokio::test]
async fn a_stream_that_stops_mid_arguments_never_yields_a_tool_call() {
    // The provider died after the first fragment: the name and id are known and
    // the argument buffer is a truncated `{"city":"Pa`.
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_a\",\"type\":\"function\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"{\\\"city\\\":\\\"Pa\"}}]}}]}\n\n";
    let error = replay_bytes_expecting_error(body.as_bytes())
        .await
        .expect_err("an unfinished tool call must not be emitted");
    match error {
        LlmError::Protocol(detail) => {
            assert!(
                detail.contains("never completed into valid JSON"),
                "{detail}"
            );
        }
        other => panic!("expected Protocol, got {other:?}"),
    }
}

/// Replay a transcript without asserting that it succeeds, returning whatever
/// the parser or the finish step rejected.
async fn replay_bytes_expecting_error(bytes: &[u8]) -> Result<Completion, LlmError> {
    let chunks: Vec<Result<Vec<u8>, std::io::Error>> =
        bytes.chunks(13).map(|chunk| Ok(chunk.to_vec())).collect();
    let mut stream = tokio_stream::iter(chunks).eventsource();
    let mut parser = ChatStreamParser::new(Capabilities::openai_compatible());

    while let Some(event) = stream.next().await {
        let event = event.map_err(|error| LlmError::Transport(error.to_string()))?;
        if event.data.is_empty() {
            continue;
        }
        parser.push(&event.data)?;
        if parser.is_done() {
            break;
        }
    }
    parser.finish()
}

#[test]
fn both_dialect_fixtures_are_checked_in() {
    for name in [
        "openai-tool-calls-stream.sse",
        "anthropic-input-json-delta-stream.sse",
        "usage-folded-cache.sse",
        "usage-cache-absent.sse",
    ] {
        let path = fixtures_dir().join(name);
        let bytes = fixture(name);
        assert!(!bytes.is_empty(), "{} is empty", path.display());
        assert!(
            std::str::from_utf8(&bytes).is_ok(),
            "{} must be UTF-8 SSE",
            path.display()
        );
        assert_eq!(
            path.extension().and_then(std::ffi::OsStr::to_str),
            Some("sse"),
            "{} must carry the .sse extension so .gitignore's *.jsonl rule cannot swallow it",
            path.display()
        );
    }
}

#[test]
fn the_fixtures_directory_is_not_empty() {
    let entries = std::fs::read_dir(fixtures_dir()).expect("fixture directory must exist");
    let count = entries.count();
    assert!(count >= 4, "expected at least four fixtures, found {count}");
}

#[test]
fn usage_constructors_agree_on_the_folded_contract() {
    // A direct check of the arithmetic the streamed fixtures assert, so the
    // fixture test cannot pass on a coincidence.
    let folded = Usage::reconcile(
        CacheReadReporting::FoldedIntoPrompt,
        Some(1200),
        Some(900),
        Some(40),
    );
    assert_eq!(folded.input_tokens(), Some(300));
    let absent = Usage::reconcile(
        CacheReadReporting::FoldedIntoPrompt,
        Some(1200),
        None,
        Some(40),
    );
    assert_eq!(absent.input_tokens(), Some(1200));
    assert!(absent.cache_read().is_unavailable());
}
