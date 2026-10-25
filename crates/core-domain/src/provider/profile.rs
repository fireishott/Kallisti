//! Who a provider is, which model it serves, and what it can do.
//!
//! One shape covers 9router, Ollama, vLLM and OpenRouter. Nothing here names a
//! vendor: the adapter is chosen by `base_url` being OpenAI-compatible, not by
//! a product field, so a fifth gateway needs no new code.

use serde::{Deserialize, Serialize};

use super::error::LlmError;
use super::secret::SecretRef;

/// The request shape one endpoint speaks.
///
/// Two dialogs cover every provider 1.0 talks to. The OpenAI shape is
/// `POST {base}/chat/completions` with `choices[].delta` frames; the Anthropic
/// shape is `POST {base}/messages` with `content_block_delta` frames. Both are
/// streaming-first and both are parsed by the same [`core_runtime`] parser, so
/// the dialect decides the request we build and the events we accept, nothing
/// else.
///
/// This is a property of the endpoint, not of a vendor name: a field on the
/// profile rather than an `if` on a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiDialect {
    /// OpenAI chat completions: `{base}/chat/completions`.
    #[default]
    ChatCompletions,
    /// Anthropic Messages: `{base}/messages`. Claude models are served this way
    /// only, on every gateway that carries them.
    Messages,
}

impl ApiDialect {
    /// Stable wire name, also the value stored in the settings row.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChatCompletions => "chat_completions",
            Self::Messages => "messages",
        }
    }

    /// Read a stored value. Anything unrecognised is the OpenAI shape, which is
    /// what every deployment used before this field existed.
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw.trim() {
            "messages" => Self::Messages,
            _ => Self::ChatCompletions,
        }
    }
}

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
    /// Which request shape this endpoint speaks.
    ///
    /// Defaulted rather than required so a profile written before this field
    /// existed still parses, and so the OpenAI shape stays the default.
    #[serde(default)]
    pub dialect: ApiDialect,
    /// Whether this endpoint splits reasoning out of the answer when asked.
    ///
    /// Not every OpenAI-compatible endpoint separates a chain of thought from
    /// the reply, and the two ways it fails are opposite. An endpoint that
    /// supports the split and is not asked for it inlines the reasoning into
    /// `delta.content` wrapped in `<think>` tags, which lands in the answer. An
    /// endpoint that does not support it can refuse the field outright. So this
    /// is a property of the deployment, declared on its registry entry, rather
    /// than something the adapter sends to everybody on a guess.
    ///
    /// Defaulted for the same reason as [`Self::dialect`]: every profile that
    /// existed before this field sent exactly the bytes it sends now.
    #[serde(default)]
    pub reasoning_split: bool,
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
            dialect: ApiDialect::ChatCompletions,
            reasoning_split: false,
        })
    }

    /// The same profile, speaking another shape.
    ///
    /// A separate step rather than a constructor argument so every existing
    /// caller keeps building the OpenAI shape without changing a line.
    #[must_use]
    pub fn with_dialect(mut self, dialect: ApiDialect) -> Self {
        self.dialect = dialect;
        self
    }

    /// The same profile, asking the endpoint to split reasoning from the answer.
    ///
    /// A separate step for the same reason as [`Self::with_dialect`]: a caller
    /// that never heard of the field keeps sending the bytes it always sent.
    #[must_use]
    pub fn with_reasoning_split(mut self, split: bool) -> Self {
        self.reasoning_split = split;
        self
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
    fn a_profile_speaks_the_openai_shape_unless_it_is_told_otherwise() {
        let profile = ProviderProfile::new("9router", "http://127.0.0.1:20128/v1", key())
            .expect("profile should build");
        assert_eq!(profile.dialect, ApiDialect::ChatCompletions);

        let messages = profile.clone().with_dialect(ApiDialect::Messages);
        assert_eq!(messages.dialect, ApiDialect::Messages);
        // The rest of the profile is untouched by the dialect.
        assert_eq!(messages.base_url, profile.base_url);
        assert_eq!(messages.api_key_ref, profile.api_key_ref);
    }

    #[test]
    fn a_profile_does_not_ask_for_a_reasoning_split_unless_it_is_told_to() {
        let profile = ProviderProfile::new("9router", "http://127.0.0.1:20128/v1", key())
            .expect("profile should build");
        assert!(!profile.reasoning_split, "the default asks for nothing new");

        let split = profile.clone().with_reasoning_split(true);
        assert!(split.reasoning_split);
        assert_eq!(split.base_url, profile.base_url);
        assert_eq!(split.dialect, profile.dialect);
    }

    #[test]
    fn a_stored_profile_without_the_split_field_still_parses() {
        // The registry row written before this field existed.
        let raw = r#"{"label":"minimax","base_url":"https://api.minimax.io/v1","api_key_ref":"env:MINIMAX_API_KEY"}"#;
        let parsed: ProviderProfile = serde_json::from_str(raw).expect("an old profile parses");
        assert!(!parsed.reasoning_split);
        assert_eq!(parsed.dialect, ApiDialect::ChatCompletions);
    }

    #[test]
    fn a_dialect_round_trips_through_its_wire_name() {
        for dialect in [ApiDialect::ChatCompletions, ApiDialect::Messages] {
            assert_eq!(ApiDialect::parse(dialect.as_str()), dialect);
        }
        // A row nobody recognises is the shape every install already used.
        assert_eq!(ApiDialect::parse(""), ApiDialect::ChatCompletions);
        assert_eq!(ApiDialect::parse("ywimc"), ApiDialect::ChatCompletions);
        assert_eq!(ApiDialect::parse("  messages "), ApiDialect::Messages);
    }

    #[test]
    fn a_profile_written_before_the_dialect_existed_still_parses() {
        // The wire form of a key reference is the string `env:NAME`, which is
        // also what the config file carries.
        let json = r#"{"label":"9router","base_url":"http://127.0.0.1:20128/v1",
                       "api_key_ref":"env:NINEROUTER_API_KEY"}"#;
        let profile: ProviderProfile = serde_json::from_str(json).expect("parses");
        assert_eq!(profile.dialect, ApiDialect::ChatCompletions);
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
