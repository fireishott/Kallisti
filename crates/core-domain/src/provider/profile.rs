//! Who a provider is, which model it serves, and what it can do.
//!
//! One shape covers 9router, Ollama, vLLM and OpenRouter. Nothing here names a
//! vendor: the adapter is chosen by `base_url` being OpenAI-compatible, not by
//! a product field, so a fifth gateway needs no new code.

use serde::{Deserialize, Serialize};

use super::error::LlmError;
use super::secret::SecretRef;

/// One configured provider endpoint.
///
/// Deliberately free of anything vendor-specific: a label for humans, the base
/// URL that ends in `/v1`, and a reference to the key. The label is not
/// consulted for behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderProfile {
    /// Human label, e.g. `9router`. Never used to pick a dialect.
    pub label: String,
    /// OpenAI-compatible base URL, without a trailing slash.
    pub base_url: String,
    /// Where the bearer token comes from. A name, never a value.
    pub api_key_ref: SecretRef,
}

impl ProviderProfile {
    /// Build a profile and normalise the base URL.
    ///
    /// A base URL that is empty, has whitespace, or is not `http(s)://` is a
    /// request we cannot construct, which is [`LlmError::Protocol`] - the same
    /// variant a malformed frame gets, because from the caller's side both mean
    /// "the thing we were about to send or receive was not well formed".
    pub fn new(
        label: impl Into<String>,
        base_url: impl Into<String>,
        api_key_ref: SecretRef,
    ) -> Result<Self, LlmError> {
        let base_url = base_url.into();
        let trimmed = base_url.trim_end_matches('/');
        if trimmed.is_empty() {
            return Err(LlmError::Protocol("provider base URL is empty".to_owned()));
        }
        if trimmed.chars().any(char::is_whitespace) {
            return Err(LlmError::Protocol(
                "provider base URL contains whitespace".to_owned(),
            ));
        }
        if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
            return Err(LlmError::Protocol(format!(
                "provider base URL must start with http:// or https://, got `{trimmed}`"
            )));
        }
        Ok(Self {
            label: label.into(),
            base_url: trimmed.to_owned(),
            api_key_ref,
        })
    }
}

/// The model this provider is configured to serve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelProfile {
    /// Wire model id, e.g. `ds/deepseek-v4-flash`.
    pub id: String,
    /// Context window in tokens, when the deployment knows it.
    pub context_window: Option<u32>,
    /// Largest output the model will emit, when the deployment knows it.
    pub max_output_tokens: Option<u32>,
    /// Whether the model accepts tool definitions.
    pub supports_tools: bool,
}

impl ModelProfile {
    /// A model with no declared limits, which is the honest state for a gateway
    /// that does not publish them.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            context_window: None,
            max_output_tokens: None,
            supports_tools: false,
        }
    }
}

/// How a provider reports prompt tokens served from its own cache.
///
/// This is the fact that decides whether a cache count has to be subtracted
/// from the prompt total, so it is a capability rather than a per-response
/// guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheReadReporting {
    /// Cache reads arrive in their own field and are *not* part of the prompt
    /// count (Anthropic's `cache_read_input_tokens`).
    Separate,
    /// Cache reads are folded into the prompt total, so they must be
    /// subtracted to get the billable input count (OpenAI's
    /// `prompt_tokens_details.cached_tokens`).
    FoldedIntoPrompt,
    /// The provider cannot report cache reads at all. Any cache number it does
    /// send is ignored, and usage says `provider_unavailable`.
    NotReported,
}

/// What a provider can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// Incremental SSE deltas. Everything in 1.0 requires this.
    pub streaming: bool,
    /// Tool definitions and streamed tool-call fragments.
    pub tool_calls: bool,
    /// How cache reads are reported.
    pub cache_read_reporting: CacheReadReporting,
    /// Image input on this model.
    pub vision: bool,
}

impl Capabilities {
    /// A modern OpenAI-compatible gateway: streaming, tools, and cache reads
    /// folded into the prompt total. This is the 9router shape.
    #[must_use]
    pub const fn openai_compatible() -> Self {
        Self {
            streaming: true,
            tool_calls: true,
            cache_read_reporting: CacheReadReporting::FoldedIntoPrompt,
            vision: false,
        }
    }

    /// Text streaming only, and no cache accounting at all. This is the honest
    /// description of a small local runtime, and it forces usage to report
    /// `provider_unavailable` rather than a fake zero.
    #[must_use]
    pub const fn stream_only() -> Self {
        Self {
            streaming: true,
            tool_calls: false,
            cache_read_reporting: CacheReadReporting::NotReported,
            vision: false,
        }
    }

    /// Whether this capability set can serve a turn at all.
    #[must_use]
    pub const fn is_usable(self) -> bool {
        self.streaming
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::openai_compatible()
    }
}

/// The seam: identity of a configured provider, and nothing about transport.
///
/// Object-safe on purpose so `&dyn Provider` works in the turn machine. The
/// adapter that owns the HTTP client implements this and adds its own
/// `stream_chat`, which is the part that needs an async runtime and therefore
/// cannot live in `core-domain`.
pub trait Provider: std::fmt::Debug + Send + Sync {
    /// Endpoint label, base URL, and key reference.
    fn profile(&self) -> &ProviderProfile;

    /// The configured model.
    fn model(&self) -> &ModelProfile;

    /// What this pair can do.
    fn capabilities(&self) -> Capabilities;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SecretRef {
        SecretRef::Env("NINEROUTER_API_KEY".to_owned())
    }

    #[test]
    fn a_base_url_is_normalised_without_a_trailing_slash() {
        let profile = ProviderProfile::new("9router", "http://127.0.0.1:20128/v1/", key())
            .expect("profile should build");
        assert_eq!(profile.base_url, "http://127.0.0.1:20128/v1");
        assert_eq!(profile.api_key_ref.env_name(), "NINEROUTER_API_KEY");
    }

    #[test]
    fn a_base_url_must_be_http() {
        for bad in ["", "   ", "127.0.0.1:20128/v1", "ftp://example.test/v1"] {
            assert!(
                matches!(
                    ProviderProfile::new("x", bad, key()),
                    Err(LlmError::Protocol(_))
                ),
                "expected {bad:?} to be rejected as Protocol"
            );
        }
    }

    #[test]
    fn whitespace_inside_a_base_url_is_rejected() {
        assert!(matches!(
            ProviderProfile::new("x", "http://exa mple.test/v1", key()),
            Err(LlmError::Protocol(_))
        ));
    }

    #[test]
    fn capabilities_describe_the_two_supported_shapes() {
        let gateway = Capabilities::openai_compatible();
        assert!(gateway.streaming && gateway.tool_calls);
        assert_eq!(
            gateway.cache_read_reporting,
            CacheReadReporting::FoldedIntoPrompt
        );

        let local = Capabilities::stream_only();
        assert!(local.streaming && !local.tool_calls);
        assert_eq!(local.cache_read_reporting, CacheReadReporting::NotReported);
        assert!(gateway.is_usable() && local.is_usable());
    }
}
