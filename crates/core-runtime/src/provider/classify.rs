//! Turning provider answers into the six [`LlmError`] variants.
//!
//! Every function here is pure: it takes a status code, a body, or a frame, and
//! returns an error. No socket is opened, which is what lets the classification
//! tests run without a network.
//!
//! The classification order matters. A context overflow is decided by *body
//! shape* first, because gateways disagree about the status they put on it -
//! OpenAI answers 400, several proxies answer 413 or 422, and Ollama answers 400
//! with a plain-text body. Status alone therefore decides auth, rate limiting,
//! and upstream, and the body decides context length.

use core_domain::LlmError;

/// Longest provider snippet kept in an error detail. Enough to identify the
/// cause, short enough that a log line stays readable.
const DETAIL_LIMIT: usize = 240;

/// Substrings that mark a context-window overflow, matched case-insensitively.
///
/// These are the wire shapes seen from OpenAI, Anthropic, 9router's passthrough,
/// vLLM, and Ollama. A status code is not required: the body decides.
const CONTEXT_LENGTH_MARKERS: &[&str] = &[
    "context_length_exceeded",
    "maximum context length",
    "context window",
    "reduce the length of the messages",
    "prompt is too long",
    "input is too long",
    "too many tokens",
    "exceeds the maximum",
    "number of tokens exceeds",
];

/// Whether a body describes a prompt that does not fit.
#[must_use]
pub fn is_context_length_body(body: &str) -> bool {
    let lower = body.to_lowercase();
    CONTEXT_LENGTH_MARKERS
        .iter()
        .any(|marker| lower.contains(marker))
}

/// Trim a body down to one readable line for an error detail.
fn summarise(body: &str) -> String {
    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= DETAIL_LIMIT {
        return collapsed;
    }
    let truncated: String = collapsed.chars().take(DETAIL_LIMIT).collect();
    format!("{truncated}...")
}

/// Pull the human-readable message out of the error shapes providers actually
/// send, falling back to the raw body.
///
/// Recognised: OpenAI `{\"error\":{\"message\",\"type\",\"code\"}}`, Anthropic
/// `{\"type\":\"error\",\"error\":{\"type\",\"message\"}}`, and a plain-text
/// body (Ollama's 400s are prose).
#[must_use]
pub fn error_message(body: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return summarise(body);
    };

    let error = value.get("error").unwrap_or(&value);
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let kind = error
        .get("type")
        .or_else(|| error.get("status"))
        .and_then(serde_json::Value::as_str);
    let code = error.get("code").and_then(|code| match code {
        serde_json::Value::String(text) => Some(text.clone()),
        other if !other.is_null() => Some(other.to_string()),
        _ => None,
    });

    let mut detail = message.to_owned();
    if let Some(kind) = kind {
        detail.push_str(&format!(" [type={kind}]"));
    }
    if let Some(code) = code {
        detail.push_str(&format!(" [code={code}]"));
    }
    if detail.trim().is_empty() {
        summarise(body)
    } else {
        summarise(&detail)
    }
}

/// Classify a non-2xx HTTP response.
///
/// - 401, 403 -> [`LlmError::Auth`]
/// - 429 -> [`LlmError::RateLimit`], carrying `Retry-After` when present
/// - any status whose body says the prompt is too long ->
///   [`LlmError::ContextLength`]
/// - everything else -> [`LlmError::Upstream`], with the status preserved
#[must_use]
pub fn classify_http(status: u16, body: &str, retry_after_secs: Option<u64>) -> LlmError {
    let detail = error_message(body);
    if is_context_length_body(body) {
        return LlmError::ContextLength {
            detail: format!("status {status}: {detail}"),
        };
    }
    match status {
        401 | 403 => LlmError::Auth { detail },
        429 => LlmError::RateLimit {
            retry_after_secs,
            detail,
        },
        _ => LlmError::Upstream {
            status: Some(status),
            detail: format!("status {status}: {detail}"),
        },
    }
}

/// Classify an error frame that arrived inside an already-successful stream.
///
/// There is no HTTP status here, so `Upstream` carries `None`, and the body
/// shape still decides whether it was a context overflow or an auth failure a
/// gateway only surfaced mid-stream.
#[must_use]
pub fn classify_error_frame(value: &serde_json::Value) -> LlmError {
    let rendered = value.to_string();
    let detail = error_message(&rendered);
    if is_context_length_body(&rendered) {
        return LlmError::ContextLength { detail };
    }

    let error = value.get("error").unwrap_or(value);
    let kind = error
        .get("type")
        .or_else(|| error.get("code"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_lowercase();

    if kind.contains("auth") || kind.contains("permission") || kind.contains("invalid_api_key") {
        return LlmError::Auth { detail };
    }
    if kind.contains("rate_limit") || kind.contains("overloaded") {
        return LlmError::RateLimit {
            retry_after_secs: None,
            detail,
        };
    }
    LlmError::Upstream {
        status: None,
        detail,
    }
}

/// Transport failures, reduced to the facts the classification decides on.
///
/// Kept as its own enum so the connection-refused case can be tested without
/// dialling anything: the test suite must not open a socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportFailure {
    /// The connection could not be established. `connection refused` is here.
    Connect,
    /// The connection was established and then stalled or timed out.
    Timeout,
    /// The body ended early, or a chunk could not be decoded.
    Body,
    /// Anything else the client could not attribute.
    Other,
}

impl TransportFailure {
    /// The wire name used in the detail, so a log line names the failure kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connect => "connect",
            Self::Timeout => "timeout",
            Self::Body => "body",
            Self::Other => "request",
        }
    }
}

/// Every transport failure is [`LlmError::Transport`], whatever the cause.
///
/// It is still worth modelling the cause: `connect` is retryable-and-hopeless
/// (the endpoint is down) while `timeout` and `body` are retryable-and-plausible,
/// so the detail has to say which happened.
#[must_use]
pub fn classify_transport(failure: TransportFailure, detail: &str) -> LlmError {
    LlmError::Transport(format!("{}: {detail}", failure.as_str()))
}

/// Map a failed request onto [`LlmError::Transport`].
#[must_use]
pub fn classify_request_failure(
    is_connect: bool,
    is_timeout: bool,
    is_body: bool,
    detail: &str,
) -> LlmError {
    let failure = if is_connect {
        TransportFailure::Connect
    } else if is_timeout {
        TransportFailure::Timeout
    } else if is_body {
        TransportFailure::Body
    } else {
        TransportFailure::Other
    };
    classify_transport(failure, detail)
}

/// A wire that could not be parsed is [`LlmError::Protocol`].
#[must_use]
pub fn protocol(detail: impl Into<String>) -> LlmError {
    LlmError::Protocol(detail.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unauthorized_is_auth() {
        let error = classify_http(
            401,
            r#"{"error":{"message":"Incorrect API key provided","type":"invalid_request_error","code":"invalid_api_key"}}"#,
            None,
        );
        assert!(matches!(error, LlmError::Auth { .. }), "{error:?}");
    }

    #[test]
    fn forbidden_is_also_auth() {
        let error = classify_http(403, r#"{"error":{"message":"no access"}}"#, None);
        assert!(matches!(error, LlmError::Auth { .. }), "{error:?}");
    }

    #[test]
    fn too_many_requests_is_rate_limit_and_keeps_retry_after() {
        let error = classify_http(
            429,
            r#"{"error":{"message":"Rate limit reached","type":"requests"}}"#,
            Some(12),
        );
        match error {
            LlmError::RateLimit {
                retry_after_secs,
                detail,
            } => {
                assert_eq!(retry_after_secs, Some(12));
                assert!(detail.contains("Rate limit reached"), "{detail}");
            }
            other => panic!("expected RateLimit, got {other:?}"),
        }
    }

    #[test]
    fn a_context_length_body_wins_over_its_status_code() {
        // OpenAI answers 400; the body is what identifies it.
        let error = classify_http(
            400,
            r#"{"error":{"message":"This model's maximum context length is 8192 tokens, however you requested 9000 tokens.","type":"invalid_request_error","code":"context_length_exceeded"}}"#,
            None,
        );
        assert!(matches!(error, LlmError::ContextLength { .. }), "{error:?}");

        // A plain-text body from a local runtime is the same fact.
        let ollama = classify_http(400, "input is too long for this model", None);
        assert!(
            matches!(ollama, LlmError::ContextLength { .. }),
            "{ollama:?}"
        );

        // And so is Anthropic's wording.
        let anthropic = classify_http(
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 210000 tokens > 200000 maximum"}}"#,
            None,
        );
        assert!(
            matches!(anthropic, LlmError::ContextLength { .. }),
            "{anthropic:?}"
        );
    }

    #[test]
    fn a_context_overflow_inside_a_stream_is_still_context_length() {
        let frame = serde_json::json!({
            "error": {"message": "This model's maximum context length is 8192 tokens", "code": "context_length_exceeded"}
        });
        assert!(matches!(
            classify_error_frame(&frame),
            LlmError::ContextLength { .. }
        ));
    }

    #[test]
    fn an_unrecognised_status_is_upstream_and_keeps_the_status() {
        let error = classify_http(500, "internal error", None);
        match error {
            LlmError::Upstream { status, detail } => {
                assert_eq!(status, Some(500));
                assert!(detail.contains("internal error"), "{detail}");
            }
            other => panic!("expected Upstream, got {other:?}"),
        }
        let not_found = classify_http(404, r#"{"error":{"message":"model not found"}}"#, None);
        assert!(matches!(
            not_found,
            LlmError::Upstream {
                status: Some(404),
                ..
            }
        ));
    }

    #[test]
    fn a_mid_stream_auth_frame_is_auth() {
        let frame = serde_json::json!({
            "type": "error",
            "error": {"type": "authentication_error", "message": "invalid x-api-key"}
        });
        assert!(matches!(
            classify_error_frame(&frame),
            LlmError::Auth { .. }
        ));
    }

    #[test]
    fn connection_refused_is_transport() {
        let error = classify_transport(TransportFailure::Connect, "connection refused");
        match error {
            LlmError::Transport(detail) => assert_eq!(detail, "connect: connection refused"),
            other => panic!("expected Transport, got {other:?}"),
        }
    }

    #[test]
    fn every_request_failure_kind_is_transport_and_nothing_else() {
        // Facts as a client reports them, without a socket in the loop.
        let cases = [
            (true, false, false, "connection refused"),
            (false, true, false, "operation timed out"),
            (false, false, true, "body error: connection reset by peer"),
            (false, false, false, "builder error"),
        ];
        for (is_connect, is_timeout, is_body, detail) in cases {
            let error = classify_request_failure(is_connect, is_timeout, is_body, detail);
            assert!(
                matches!(error, LlmError::Transport(_)),
                "{detail:?} should be Transport, got {error:?}"
            );
        }
    }

    #[test]
    fn details_never_carry_headers_or_keys() {
        // The detail is built from the body only. A body that happens to echo a
        // key reference string is not a key value, and a header is never read.
        let error = classify_http(401, r#"{"error":{"message":"bad credentials"}}"#, None);
        let LlmError::Auth { detail } = error else {
            panic!("expected Auth");
        };
        assert!(!detail.to_lowercase().contains("authorization"));
        assert!(!detail.contains("Bearer"));
    }
}
