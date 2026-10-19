//! Provider seam.
//!
//! The vocabulary lives in `core-domain` (that crate stays I/O-free, so it is
//! shared unchanged with the Lite core). What lives here is the part that needs
//! an HTTP client:
//!
//! | Item | What it is |
//! |---|---|
//! | [`OpenAiCompatProvider`] | one adapter for 9router, Ollama, vLLM, OpenRouter |
//! | [`ChatStreamParser`] | SSE chunks in, completed events out (pure, fixture-tested) |
//! | [`classify`] | status and body shape -> one of six [`LlmError`] variants (pure) |
//! | [`resolve_api_key`] | `env:NAME` -> process environment, and nothing else |
//!
//! Two locked decisions are enforced structurally rather than documented:
//!
//! - `api_key_ref` is the only way a key enters this layer, and it can only name
//!   an environment variable. [`SecretKey`] exists so a value cannot be printed
//!   by accident: its `Debug` is redacted and it has no `Display`;
//! - usage the provider did not report becomes
//!   [`core_domain::TokenCount::ProviderUnavailable`] /
//!   [`core_domain::CacheRead::ProviderUnavailable`], never zero. See
//!   [`core_domain::Usage::reconcile`], the only constructor that touches
//!   provider numbers.

pub mod classify;
pub mod models;
pub mod openai_compat;
pub mod stream;

use std::fmt;

use core_domain::LlmError;

pub use core_domain::{API_KEY_REF_ENV_PREFIX, SecretRef, SecretRefError};
pub use models::{ModelCatalog, list_models, parse_model_list};
pub use openai_compat::OpenAiCompatProvider;
pub use stream::ChatStreamParser;

/// Where a key comes from: the process environment, at call time.
///
/// The lookup is injected rather than read inside, so the resolver is testable
/// without mutating the process environment - and mutating it from a test would
/// be racy in a multi-threaded harness anyway.
pub fn resolve_api_key_with(
    reference: &SecretRef,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<SecretKey, LlmError> {
    match reference {
        SecretRef::Env(name) => match lookup(name) {
            Some(value) if !value.trim().is_empty() => SecretKey::new(value),
            Some(_) => Err(LlmError::Auth {
                detail: format!(
                    "api key reference `env:{name}` resolved to an empty value in the process environment"
                ),
            }),
            None => Err(LlmError::Auth {
                detail: format!(
                    "api key reference `env:{name}` is not set in the process environment"
                ),
            }),
        },
    }
}

/// Resolve a reference against the real process environment.
pub fn resolve_api_key(reference: &SecretRef) -> Result<SecretKey, LlmError> {
    resolve_api_key_with(reference, |name| std::env::var(name).ok())
}

/// A resolved secret.
///
/// Deliberately not `Display` and not `Serialize`: the only way out is
/// [`SecretKey::expose`], which is called once, at the point the request header
/// is built. `Debug` is redacted so `{:?}` on a containing struct cannot leak it.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretKey(String);

impl SecretKey {
    /// Wrap a resolved value. Empty values are rejected.
    pub fn new(value: impl Into<String>) -> Result<Self, LlmError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(LlmError::Auth {
                detail: "resolved api key is empty".to_owned(),
            });
        }
        Ok(Self(value))
    }

    /// The value, for the one place that has to put it in a header.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey(<redacted>)")
    }
}

/// Who said what in the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    /// Instruction preamble.
    System,
    /// The customer.
    User,
    /// A previous model turn.
    Assistant,
}

impl ChatRole {
    /// Stable wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// One message in the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    /// Who said it.
    pub role: ChatRole,
    /// What they said. Text only in 1.0.
    pub content: String,
}

/// A tool the model may call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSpec {
    /// Function name.
    pub name: String,
    /// What it does, for the model.
    pub description: String,
    /// JSON Schema for the arguments object.
    pub parameters: serde_json::Value,
}

/// One turn's request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChatRequest {
    /// Prompt messages, in order.
    pub messages: Vec<ChatMessage>,
    /// Output cap, when the caller wants one.
    pub max_output_tokens: Option<u32>,
    /// Tools offered to the model. Empty means none.
    pub tools: Vec<ToolSpec>,
}

impl ChatRequest {
    /// A one-message prompt.
    #[must_use]
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            messages: vec![ChatMessage {
                role: ChatRole::User,
                content: text.into(),
            }],
            ..Self::default()
        }
    }

    /// Add a message.
    #[must_use]
    pub fn with_message(mut self, role: ChatRole, content: impl Into<String>) -> Self {
        self.messages.push(ChatMessage {
            role,
            content: content.into(),
        });
        self
    }
}

/// Something that happened during the stream, delivered as it happens.
///
/// Tool calls only ever appear here complete: the parser buffers fragments and
/// emits one [`ProviderEvent::ToolCall`] per finished call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderEvent {
    /// Assistant text fragment.
    Text(String),
    /// Reasoning fragment, when the model exposes it. Never shown as the reply.
    Reasoning(String),
    /// A finished tool call.
    ToolCall(core_domain::ToolCall),
}

/// Everything one stream produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    /// Assistant text, assembled.
    pub text: String,
    /// Reasoning text, assembled. Empty when the model exposes none.
    pub reasoning: String,
    /// Finished tool calls, in the order the model produced them.
    pub tool_calls: Vec<core_domain::ToolCall>,
    /// Token accounting, with unreported numbers marked unavailable.
    pub usage: core_domain::Usage,
    /// The provider's finish reason, when it sent one.
    pub finish_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference() -> SecretRef {
        SecretRef::Env("KALLISTI_TEST_API_KEY".to_owned())
    }

    #[test]
    fn a_reference_resolves_from_the_lookup_by_name() {
        let key = resolve_api_key_with(&reference(), |name| {
            assert_eq!(name, "KALLISTI_TEST_API_KEY");
            Some("resolved-value".to_owned())
        })
        .expect("key should resolve");
        assert_eq!(key.expose(), "resolved-value");
    }

    #[test]
    fn a_missing_variable_is_an_auth_failure_naming_the_reference_not_a_value() {
        let error = resolve_api_key_with(&reference(), |_| None).expect_err("must fail");
        let LlmError::Auth { detail } = error else {
            panic!("expected Auth");
        };
        assert!(detail.contains("env:KALLISTI_TEST_API_KEY"), "{detail}");
        assert!(detail.contains("not set"), "{detail}");
    }

    #[test]
    fn an_empty_variable_is_an_auth_failure() {
        let error =
            resolve_api_key_with(&reference(), |_| Some("   ".to_owned())).expect_err("must fail");
        assert!(matches!(error, LlmError::Auth { .. }));
    }

    #[test]
    fn a_secret_key_cannot_be_printed_by_accident() {
        let key = SecretKey::new("super-secret-value").expect("key should build");
        assert_eq!(format!("{key:?}"), "SecretKey(<redacted>)");
        assert!(!format!("{key:?}").contains("super-secret-value"));
        assert_eq!(key.expose(), "super-secret-value");
    }

    #[test]
    fn an_empty_secret_key_is_rejected() {
        assert!(SecretKey::new("").is_err());
        assert!(SecretKey::new("  ").is_err());
    }

    #[test]
    fn a_one_shot_prompt_is_a_single_user_message() {
        let request = ChatRequest::user_text("reply with the single word: ok");
        assert_eq!(request.messages.len(), 1);
        assert_eq!(request.messages[0].role, ChatRole::User);
        assert_eq!(
            request.messages[0].content,
            "reply with the single word: ok"
        );
    }
}
