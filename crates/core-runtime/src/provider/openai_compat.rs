//! The OpenAI-compatible adapter.
//!
//! One implementation, four deployments. 9router on loopback, Ollama, vLLM, and
//! OpenRouter all speak the same `/chat/completions` shape, so nothing here
//! branches on a vendor: the deployment differs only by `base_url`, the model
//! id, and which environment variable holds the key. A fifth gateway needs a new
//! config entry, not a new adapter.
//!
//! What this type is responsible for:
//!
//! - streaming, always: the request asks for SSE with usage included, so the
//!   caller never waits for a whole reply and never has to make a second call to
//!   find out what the turn cost;
//! - classifying every failure into an [`LlmError`] variant, from the status for
//!   auth, rate limits and upstream faults, and from the body shape for a
//!   context overflow;
//! - never buffering the whole body: bytes go straight into [`ChatStreamParser`]
//!   as they arrive, and only the parser's assembled result is kept.

use std::time::Duration;

use core_domain::{Capabilities, LlmError, ModelProfile, Provider, ProviderProfile};
use eventsource_stream::Eventsource;
use reqwest::Client;
use tokio_stream::StreamExt;

use super::classify;
use super::stream::ChatStreamParser;
use super::{ChatRequest, ChatRole, Completion, ProviderEvent, SecretKey};

/// How long to wait for the connection itself. Separate from the stream: a slow
/// model is normal, an endpoint that will not accept a TCP connection is not.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long one read may go without a byte before the stream is declared dead.
///
/// A gap, not a ceiling: as long as bytes keep arriving the turn can run for as
/// long as it needs, but a provider that stops mid-answer can no longer hold a
/// conversation row on `streaming` forever.
const STREAM_STALL: Duration = Duration::from_secs(60);

/// An OpenAI-compatible chat-completions provider.
#[derive(Debug)]
pub struct OpenAiCompatProvider {
    http: Client,
    profile: ProviderProfile,
    model: ModelProfile,
    capabilities: Capabilities,
}

impl OpenAiCompatProvider {
    /// Build an adapter for one configured endpoint.
    ///
    /// Refuses a capability set without streaming: this adapter does not know how
    /// to do a blocking completion, and pretending otherwise would hide a
    /// deployment mistake until the first turn.
    pub fn new(
        profile: ProviderProfile,
        model: ModelProfile,
        capabilities: Capabilities,
    ) -> Result<Self, LlmError> {
        if !capabilities.streaming {
            return Err(classify::protocol(format!(
                "provider `{}` is configured without streaming, which this adapter does not implement",
                profile.label
            )));
        }
        let http = Client::builder()
            .user_agent(concat!("kallisti-eris/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|error| {
                classify::protocol(format!("could not build the HTTP client: {error}"))
            })?;

        Ok(Self {
            http,
            profile,
            model,
            capabilities,
        })
    }

    /// The chat-completions endpoint for this profile.
    #[must_use]
    pub fn chat_completions_url(&self) -> String {
        format!("{}/chat/completions", self.profile.base_url)
    }

    /// The streaming request body for one turn.
    ///
    /// Pure, so the wire shape is testable without a socket. `stream_options.
    /// include_usage` is unconditional on purpose: without it several
    /// OpenAI-compatible servers omit the usage frame entirely, and the seam
    /// would then have to report usage as unavailable for a provider that can
    /// actually report it.
    ///
    /// The system prompt, when there is one, is the first message whatever the
    /// caller did with `messages`: the dialect expects the instruction preamble
    /// there, and a turn that put it anywhere else would be ignored by some
    /// servers and half-honoured by others.
    #[must_use]
    pub fn build_body(&self, request: &ChatRequest) -> serde_json::Value {
        let mut messages: Vec<serde_json::Value> = Vec::with_capacity(request.messages.len() + 1);
        if let Some(system) = request
            .system
            .as_deref()
            .filter(|text| !text.trim().is_empty())
        {
            messages.push(serde_json::json!({
                "role": ChatRole::System.as_str(),
                "content": system,
            }));
        }
        // A vision request carries its images on the final message. Everything
        // else takes the plain-text path it always took, so adding this cannot
        // change the bytes of a request that has no images.
        let last = request.messages.len().saturating_sub(1);
        messages.extend(request.messages.iter().enumerate().map(|(index, message)| {
            match message.role {
                ChatRole::User if index == last && !request.images.is_empty() => {
                    let mut parts: Vec<serde_json::Value> = Vec::new();
                    if !message.content.trim().is_empty() {
                        parts.push(serde_json::json!({
                            "type": "text",
                            "text": message.content,
                        }));
                    }
                    for url in &request.images {
                        parts.push(serde_json::json!({
                            "type": "image_url",
                            "image_url": { "url": url },
                        }));
                    }
                    serde_json::json!({
                        "role": ChatRole::User.as_str(),
                        "content": parts,
                    })
                }
                ChatRole::Tool => serde_json::json!({
                    "role": ChatRole::Tool.as_str(),
                    // Never invented: a message with no id serializes as null and
                    // the provider rejects it, which is louder than guessing.
                    "tool_call_id": message.tool_call_id,
                    "content": message.content,
                }),
                ChatRole::Assistant if !message.tool_calls.is_empty() => {
                    let calls: Vec<serde_json::Value> = message
                        .tool_calls
                        .iter()
                        .map(|call| {
                            serde_json::json!({
                                "id": call.id,
                                "type": "function",
                                "function": {
                                    "name": call.name,
                                    // The wire format carries arguments as a JSON
                                    // string, not as an object.
                                    "arguments": call.arguments.to_string(),
                                }
                            })
                        })
                        .collect();
                    // A call-only assistant turn has no text. Several
                    // OpenAI-compatible servers reject an empty string here, so
                    // absence is null rather than "".
                    let content = if message.content.is_empty() {
                        serde_json::Value::Null
                    } else {
                        serde_json::Value::String(message.content.clone())
                    };
                    serde_json::json!({
                        "role": ChatRole::Assistant.as_str(),
                        "content": content,
                        "tool_calls": calls,
                    })
                }
                _ => serde_json::json!({
                    "role": message.role.as_str(),
                    "content": message.content,
                }),
            }
        }));

        let mut body = serde_json::json!({
            "model": self.model.id,
            "messages": messages,
            "stream": true,
            "stream_options": { "include_usage": true },
        });

        if let Some(max) = request.max_output_tokens {
            body["max_tokens"] = serde_json::Value::from(max);
        }

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        }
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(tools);
        }

        body
    }

    /// Send one turn and stream it back.
    ///
    /// `sink` receives text, reasoning and completed tool calls as they parse.
    /// The returned [`Completion`] carries the assembled reply and the usage
    /// that the provider actually reported - with anything it did not report
    /// marked unavailable rather than zeroed.
    pub async fn stream_chat(
        &self,
        request: &ChatRequest,
        key: &SecretKey,
        sink: &mut dyn FnMut(ProviderEvent),
    ) -> Result<Completion, LlmError> {
        let url = self.chat_completions_url();
        let body = self.build_body(request);

        let response = self
            .http
            .post(&url)
            .bearer_auth(key.expose())
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .json(&body)
            .send()
            .await
            .map_err(|error| {
                classify::classify_request_failure(
                    error.is_connect(),
                    error.is_timeout(),
                    error.is_body(),
                    &describe_request_error(&url, &error),
                )
            })?;

        let status = response.status();
        if !status.is_success() {
            let retry_after_secs = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok());
            // The body is the diagnostic; it is provider text, and no header or
            // key ever reaches it.
            let body = response.text().await.unwrap_or_default();
            return Err(classify::classify_http(
                status.as_u16(),
                &body,
                retry_after_secs,
            ));
        }

        let mut stream = response.bytes_stream().eventsource();
        let mut parser = ChatStreamParser::new(self.capabilities);

        loop {
            // A provider that accepts the request and then sends nothing is not
            // "thinking", it is dead. A whole-request timeout would kill a long
            // reasoning turn, so the bound is per read: any gap this wide with no
            // bytes on the wire is a stall, and the turn is failed so a fallback
            // (or the operator) can act rather than the conversation freezing on
            // "streaming" forever.
            let next = tokio::time::timeout(STREAM_STALL, stream.next())
                .await
                .map_err(|_| {
                    classify::classify_transport(
                        classify::TransportFailure::Timeout,
                        &format!(
                            "the provider sent no bytes for {} seconds, so the stream was declared dead",
                            STREAM_STALL.as_secs()
                        ),
                    )
                })?;
            let Some(event) = next else {
                break;
            };
            let event = event.map_err(|error| {
                // A stream that dies mid-body is a transport failure, not a
                // protocol one: the bytes may have been perfect up to the cut.
                classify::classify_transport(
                    classify::TransportFailure::Body,
                    &format!("SSE stream ended early: {error}"),
                )
            })?;
            if event.data.is_empty() {
                continue;
            }
            for provider_event in parser.push(&event.data)? {
                sink(provider_event);
            }
            if parser.is_done() {
                break;
            }
        }

        if parser.frames() == 0 {
            return Err(classify::protocol(
                "the stream carried no data frames (expected at least one SSE payload)".to_owned(),
            ));
        }

        parser.finish()
    }
}

impl Provider for OpenAiCompatProvider {
    fn profile(&self) -> &ProviderProfile {
        &self.profile
    }

    fn model(&self) -> &ModelProfile {
        &self.model
    }

    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }
}

/// Describe a failed request without leaking anything but the URL and the cause.
fn describe_request_error(url: &str, error: &reqwest::Error) -> String {
    let mut message = format!("{url}: {error}");
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        message.push_str(&format!(" <- {cause}"));
        source = cause.source();
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_domain::{CacheReadReporting, SecretRef};

    use super::super::{ChatRole, ToolSpec};

    fn provider() -> OpenAiCompatProvider {
        let profile = ProviderProfile::new(
            "9router",
            "http://127.0.0.1:20128/v1",
            SecretRef::Env("NINEROUTER_API_KEY".to_owned()),
        )
        .expect("profile should build");
        let model = ModelProfile::new("ds/deepseek-v4-flash");
        OpenAiCompatProvider::new(profile, model, Capabilities::openai_compatible())
            .expect("provider should build")
    }

    #[test]
    fn the_endpoint_is_the_base_url_plus_chat_completions() {
        assert_eq!(
            provider().chat_completions_url(),
            "http://127.0.0.1:20128/v1/chat/completions"
        );
    }

    #[test]
    fn the_body_always_asks_for_streaming_with_usage() {
        let body = provider().build_body(&ChatRequest::user_text("hi"));
        assert_eq!(body["model"], "ds/deepseek-v4-flash");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "hi");
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("tools").is_none());
    }

    #[test]
    fn a_generation_cap_and_tools_reach_the_wire() {
        let request = ChatRequest::user_text("weather?").with_message(ChatRole::System, "be brief");
        let request = ChatRequest {
            max_output_tokens: Some(64),
            tools: vec![ToolSpec {
                name: "get_weather".to_owned(),
                description: "Current weather".to_owned(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            ..request
        };
        let body = provider().build_body(&request);
        assert_eq!(body["max_tokens"], 64);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "get_weather");
        assert_eq!(body["messages"][1]["role"], "system");
    }

    #[test]
    fn a_tool_result_reaches_the_wire_as_a_tool_message() {
        let request =
            ChatRequest::user_text("weather?").with_tool_result("call_1", r#"{"temp_c":21}"#);
        let body = provider().build_body(&request);
        assert_eq!(body["messages"][1]["role"], "tool");
        assert_eq!(body["messages"][1]["tool_call_id"], "call_1");
        assert_eq!(body["messages"][1]["content"], r#"{"temp_c":21}"#);
    }

    #[test]
    fn an_assistant_turn_carries_its_tool_calls_back_as_a_json_string() {
        let call = core_domain::ToolCall::new(
            0,
            "call_1",
            "read_file",
            serde_json::json!({"path": "a.txt"}),
        )
        .expect("a call");
        let request = ChatRequest::user_text("read a.txt").with_tool_calls(vec![call]);
        let body = provider().build_body(&request);

        assert_eq!(body["messages"][1]["role"], "assistant");
        assert!(
            body["messages"][1]["content"].is_null(),
            "a call-only turn sends null content, not an empty string"
        );
        assert_eq!(body["messages"][1]["tool_calls"][0]["id"], "call_1");
        assert_eq!(body["messages"][1]["tool_calls"][0]["type"], "function");
        assert_eq!(
            body["messages"][1]["tool_calls"][0]["function"]["name"],
            "read_file"
        );

        // Arguments travel as a JSON string, not as a nested object.
        let arguments = body["messages"][1]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .expect("arguments must be a string on the wire");
        let parsed: serde_json::Value =
            serde_json::from_str(arguments).expect("arguments must be valid JSON");
        assert_eq!(parsed["path"], "a.txt");
    }

    #[test]
    fn a_plain_assistant_message_carries_no_tool_calls_key() {
        let request = ChatRequest::user_text("hi").with_message(ChatRole::Assistant, "hello");
        let body = provider().build_body(&request);
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(body["messages"][1]["content"], "hello");
        assert!(
            body["messages"][1].get("tool_calls").is_none(),
            "a text-only turn must not grow a tool_calls key"
        );
    }

    #[test]
    fn images_ride_as_a_content_array_on_the_last_message() {
        let request = ChatRequest {
            images: vec!["data:image/png;base64,AAA".to_owned()],
            ..ChatRequest::user_text("what is this")
        };
        let body = provider().build_body(&request);
        let content = &body["messages"][0]["content"];
        assert!(
            content.is_array(),
            "an image request sends parts: {content}"
        );
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "what is this");
        assert_eq!(content[1]["type"], "image_url");
        assert_eq!(content[1]["image_url"]["url"], "data:image/png;base64,AAA");
    }

    #[test]
    fn a_request_without_images_keeps_the_plain_string_content() {
        let body = provider().build_body(&ChatRequest::user_text("no pictures here"));
        assert_eq!(
            body["messages"][0]["content"], "no pictures here",
            "a text turn must not change shape because a vision path exists"
        );
    }

    #[test]
    fn the_profile_and_capabilities_are_visible_through_the_trait() {
        let provider = provider();
        let as_trait: &dyn Provider = &provider;
        assert_eq!(as_trait.profile().label, "9router");
        assert_eq!(as_trait.model().id, "ds/deepseek-v4-flash");
        assert_eq!(
            as_trait.capabilities().cache_read_reporting,
            CacheReadReporting::FoldedIntoPrompt
        );
    }

    #[test]
    fn a_non_streaming_capability_set_is_rejected_at_construction() {
        let profile = ProviderProfile::new(
            "ollama",
            "http://127.0.0.1:11434/v1",
            SecretRef::Env("OLLAMA_KEY".to_owned()),
        )
        .expect("profile should build");
        let mut capabilities = Capabilities::stream_only();
        capabilities.streaming = false;
        assert!(matches!(
            OpenAiCompatProvider::new(profile, ModelProfile::new("llama3"), capabilities),
            Err(LlmError::Protocol(_))
        ));
    }
}
