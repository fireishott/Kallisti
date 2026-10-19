//! The six failures the provider seam can report.
//!
//! Exactly six variants, because a caller should be able to match the seam
//! exhaustively and decide what to do once: re-authenticate, back off, shorten
//! the prompt, retry the transport, fix the parse, or surface an upstream fault.
//!
//! Only body text a provider sent ever reaches `detail`. A header, a request,
//! and a key reference never do, so an `LlmError` is safe to log and safe to
//! write into an event payload.

/// Why a provider call failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LlmError {
    /// The provider rejected our credentials, or the key reference resolved to
    /// nothing. HTTP 401 and 403 land here.
    #[error("authentication failed: {detail}")]
    Auth {
        /// Human-readable cause, from the provider body or from key resolution.
        detail: String,
    },

    /// The provider asked us to slow down. HTTP 429.
    #[error("rate limited: {detail}")]
    RateLimit {
        /// Seconds the provider asked us to wait, when it said.
        retry_after_secs: Option<u64>,
        /// Human-readable cause.
        detail: String,
    },

    /// The prompt does not fit the model's context window. The provider's
    /// context-overflow shape lands here whatever status carried it.
    #[error("prompt exceeds the model context window: {detail}")]
    ContextLength {
        /// Human-readable cause, from the provider body.
        detail: String,
    },

    /// The request never reached a response: DNS, TCP, TLS, timeout, or a
    /// stream that died mid-body.
    #[error("transport failure: {0}")]
    Transport(String),

    /// The wire was not what the adapter is allowed to accept: malformed SSE,
    /// a frame that is not JSON, a tool-call fragment that never completed into
    /// valid JSON, or a configured base URL we could not turn into a request.
    #[error("protocol failure: {0}")]
    Protocol(String),

    /// The provider answered with an error we have no specific reading for.
    #[error("provider error: {detail}")]
    Upstream {
        /// HTTP status the provider returned, when there was one. An error
        /// frame inside an already-200 stream has none. The rendered message
        /// carries the status too, so a log line stays self-describing.
        status: Option<u16>,
        /// Human-readable cause, from the provider body, prefixed with the
        /// status when there was one.
        detail: String,
    },
}

impl LlmError {
    /// Stable wire name for logging and for the event payload.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Auth { .. } => "auth",
            Self::RateLimit { .. } => "rate_limit",
            Self::ContextLength { .. } => "context_length",
            Self::Transport(_) => "transport",
            Self::Protocol(_) => "protocol",
            Self::Upstream { .. } => "upstream",
        }
    }

    /// Whether retrying the same request could plausibly succeed.
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::RateLimit { .. } | Self::Transport(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_has_a_stable_kind() {
        let cases = [
            (
                LlmError::Auth {
                    detail: String::new(),
                },
                "auth",
            ),
            (
                LlmError::RateLimit {
                    retry_after_secs: None,
                    detail: String::new(),
                },
                "rate_limit",
            ),
            (
                LlmError::ContextLength {
                    detail: String::new(),
                },
                "context_length",
            ),
            (LlmError::Transport(String::new()), "transport"),
            (LlmError::Protocol(String::new()), "protocol"),
            (
                LlmError::Upstream {
                    status: Some(500),
                    detail: String::new(),
                },
                "upstream",
            ),
        ];
        for (error, kind) in cases {
            assert_eq!(error.kind(), kind);
        }
    }

    #[test]
    fn only_rate_limit_and_transport_are_retryable() {
        assert!(
            LlmError::RateLimit {
                retry_after_secs: Some(2),
                detail: String::new()
            }
            .is_retryable()
        );
        assert!(LlmError::Transport("reset".to_owned()).is_retryable());
        for error in [
            LlmError::Auth {
                detail: String::new(),
            },
            LlmError::ContextLength {
                detail: String::new(),
            },
            LlmError::Protocol(String::new()),
            LlmError::Upstream {
                status: Some(503),
                detail: String::new(),
            },
        ] {
            assert!(
                !error.is_retryable(),
                "{} must not be retryable",
                error.kind()
            );
        }
    }
}
