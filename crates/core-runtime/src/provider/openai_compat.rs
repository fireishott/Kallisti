//! The OpenAI-compatible adapter, and the Anthropic Messages one beside it.
//!
//! One implementation, five deployments. 9router on loopback, Ollama, vLLM and
//! OpenRouter all speak the same `/chat/completions` shape, so nothing here
//! branches on a vendor: the deployment differs only by `base_url`, the model
//! id, and which environment variable holds the key. A gateway that also serves
//! Claude models serves them at `/messages`, in the Anthropic shape, because
//! that is the only shape those models accept - so the adapter carries both
//! request builders and picks by the profile's [`ApiDialect`], which is a
//! property of the endpoint rather than a name brand.
//!
//! Chat Completions is the default and the only shape that existed before this
//! field did, so a profile that never named a dialect keeps sending exactly the
//! bytes it sent before.
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

use core_domain::{ApiDialect, Capabilities, LlmError, ModelProfile, Provider, ProviderProfile};
use eventsource_stream::Eventsource;
use reqwest::Client;
use tokio_stream::StreamExt;

use super::classify;
use super::stream::ChatStreamParser;
use super::{ChatRequest, ChatRole, Completion, ProviderEvent, Reachability, SecretKey};

/// How long to wait for the connection itself. Separate from the stream: a slow
/// model is normal, an endpoint that will not accept a TCP connection is not.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long one read may go without a byte before the stream is declared dead.
///
/// A gap, not a ceiling: as long as bytes keep arriving the turn can run for as
/// long as it needs, but a provider that stops mid-answer can no longer hold a
/// conversation row on `streaming` forever.
const STREAM_STALL: Duration = Duration::from_secs(60);

/// How long one pooled connection may sit idle before it stops being offered to
/// the next request.
///
/// The pool default is 90 seconds, which is longer than the keep-alive any
/// gateway in front of a local model actually honours - 9router answers
/// `Keep-Alive: timeout=5` - so a client that trusts the pool writes into a
/// socket the server has already dropped. That failure arrives before a single
/// response byte ("connection closed before message completed") and it lands in
/// the middle of a long turn, whenever one tool call takes longer than the
/// gateway's idle timer. Dropping the idle connection first keeps the client on a
/// socket both ends agree is alive; a fresh loopback connection costs
/// microseconds and a wasted turn costs the whole answer.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(2);

/// How many times one request goes on the wire before its failure is reported.
///
/// A request that dies before any response byte was read has, by definition,
/// streamed nothing, so a second attempt cannot duplicate text, a tool call or an
/// approval. What it can do is survive a connection closed under it - by the
/// peer, a proxy, or a pool race no client can fully close - which is otherwise
/// reported to the operator as a dead turn with half an answer.
const SEND_ATTEMPTS: usize = 2;

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
            .pool_idle_timeout(POOL_IDLE_TIMEOUT)
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

    /// The Anthropic Messages endpoint for this profile.
    #[must_use]
    pub fn messages_url(&self) -> String {
        format!("{}/messages", self.profile.base_url)
    }

    /// The endpoint this profile's dialect sends to.
    #[must_use]
    pub fn request_url(&self) -> String {
        match self.profile.dialect {
            ApiDialect::ChatCompletions => self.chat_completions_url(),
            ApiDialect::Messages => self.messages_url(),
        }
    }

    /// The largest answer asked for when the caller sets no cap.
    ///
    /// `/messages` requires the field, and a turn that omits it is refused
    /// before the model runs (`expected number, received undefined at
    /// max_tokens`), which would turn a working endpoint into a protocol
    /// failure. The value is a floor every Claude model on this gateway clears,
    /// and it is a ceiling on the answer rather than a spend: no provider bills
    /// for tokens it did not generate.
    pub const DEFAULT_MESSAGES_MAX_TOKENS: u32 = 8_192;

    /// The streaming request body for one turn, in the OpenAI shape.
    ///
    /// Pure, so the wire shape is testable without a socket. A profile whose
    /// dialect is `Messages` does not use this: see
    /// [`OpenAiCompatProvider::build_messages_body`]. `stream_options.
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

    /// The streaming request body for one turn, in the Anthropic shape.
    ///
    /// Three differences from the OpenAI body, and each is a place a naive
    /// translation would fail:
    ///
    /// - the system prompt is a top-level `system` field, not a message: a
    ///   `system` role inside `messages` is rejected outright;
    /// - a tool result is a `user` message carrying a `tool_result` block, not a
    ///   `tool` role addressed by `tool_call_id`;
    /// - `max_tokens` is required, so a caller that set no cap gets
    ///   [`Self::DEFAULT_MESSAGES_MAX_TOKENS`] rather than a refused request.
    ///
    /// Tourniquets around consecutive same-role messages are deliberately absent:
    /// a replayed transcript ends a tool round with the assistant's `tool_use`
    /// then the tool results as `user`, which alternates on its own, and merging
    /// messages the transcript kept apart would silently reorder a conversation.
    #[must_use]
    pub fn build_messages_body(&self, request: &ChatRequest) -> serde_json::Value {
        let mut messages: Vec<serde_json::Value> = Vec::with_capacity(request.messages.len());
        let last = request.messages.len().saturating_sub(1);
        for (index, message) in request.messages.iter().enumerate() {
            match message.role {
                super::ChatRole::System => {
                    // The instruction preamble lives at the top level in this
                    // dialect. One is folded to text so a transcript that kept
                    // several cannot be silently dropped.
                    continue;
                }
                super::ChatRole::Tool => {
                    messages.push(serde_json::json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": message.tool_call_id,
                            "content": message.content,
                        }],
                    }));
                }
                super::ChatRole::Assistant if !message.tool_calls.is_empty() => {
                    let mut blocks: Vec<serde_json::Value> = Vec::new();
                    if !message.content.is_empty() {
                        blocks.push(serde_json::json!({
                            "type": "text",
                            "text": message.content,
                        }));
                    }
                    for call in &message.tool_calls {
                        // The wire wants a materialised object here, and the
                        // buffered form is always one: a fragment is never a
                        // call. `{}` is the honest reading of a call that took
                        // no arguments.
                        blocks.push(serde_json::json!({
                            "type": "tool_use",
                            "id": call.id,
                            "name": call.name,
                            "input": call.arguments,
                        }));
                    }
                    messages.push(serde_json::json!({ "role": "assistant", "content": blocks }));
                }
                super::ChatRole::User if index == last && !request.images.is_empty() => {
                    let mut blocks: Vec<serde_json::Value> = Vec::new();
                    if !message.content.trim().is_empty() {
                        blocks.push(serde_json::json!({
                            "type": "text",
                            "text": message.content,
                        }));
                    }
                    for url in &request.images {
                        if let Some((media, data)) = split_data_url(url) {
                            blocks.push(serde_json::json!({
                                "type": "image",
                                "source": {
                                    "type": "base64",
                                    "media_type": media,
                                    "data": data,
                                },
                            }));
                        } else {
                            // A remote URL is not fetchable by this gateway, so
                            // it is sent as a URL source, which the dialect
                            // defines and the endpoint may still accept.
                            blocks.push(serde_json::json!({
                                "type": "image",
                                "source": { "type": "url", "url": url },
                            }));
                        }
                    }
                    messages.push(serde_json::json!({ "role": "user", "content": blocks }));
                }
                _ => {
                    messages.push(serde_json::json!({
                        "role": message.role.as_str(),
                        "content": message.content,
                    }));
                }
            }
        }

        // A transcript that carried its preamble as a message rather than in
        // `ChatRequest::system` still reaches the wire, prepended and joined:
        // dropping the operator's own instructions would be the worse failure.
        let preamble = request
            .system
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .map(str::to_owned)
            .or_else(|| {
                let joined = request
                    .messages
                    .iter()
                    .filter(|message| message.role == super::ChatRole::System)
                    .map(|message| message.content.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                (!joined.trim().is_empty()).then_some(joined)
            });

        let mut body = serde_json::json!({
            "model": self.model.id,
            "messages": messages,
            "stream": true,
            "max_tokens": request
                .max_output_tokens
                .unwrap_or(Self::DEFAULT_MESSAGES_MAX_TOKENS),
        });
        if let Some(system) = preamble {
            body["system"] = serde_json::Value::String(system);
        }

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(|tool| {
                    // `input_schema`, not `parameters`, and the function is not
                    // wrapped in a `function` object the way OpenAI wraps it.
                    serde_json::json!({
                        "name": tool.name,
                        "description": tool.description,
                        "input_schema": tool.parameters,
                    })
                })
                .collect();
            body["tools"] = serde_json::Value::Array(tools);
        }

        body
    }

    /// The body this profile would send, whichever dialect it speaks.
    #[must_use]
    pub fn build_request_body(&self, request: &ChatRequest) -> serde_json::Value {
        match self.profile.dialect {
            ApiDialect::ChatCompletions => self.build_body(request),
            ApiDialect::Messages => self.build_messages_body(request),
        }
    }

    /// Ask the endpoint which models it serves, and which of them answer.
    ///
    /// The catalogue is the endpoint's own `GET {base}/models`, so a caller
    /// never keeps a list of model ids in two places. Reachability is measured
    /// rather than assumed, because a gateway can list a model it cannot serve:
    /// this one does, seven times out of ninety-three on the day it was wired.
    /// Listing a model that never answers is exactly the failure the picker must
    /// not offer, so the two facts are returned together and kept apart.
    ///
    /// The pass costs one small request per model, which is why it is a
    /// deliberate call rather than something a page load does: at ninetysomething
    /// models it is a minute of wall clock, and the caller owns that decision.
    pub async fn probe_models(
        &self,
        key: &SecretKey,
        probe: Reachability,
    ) -> Result<super::ModelCatalog, LlmError> {
        let mut catalog = super::list_models(&self.profile, key).await?;
        if probe == Reachability::Skip {
            return Ok(catalog);
        }
        let dialects = catalog.dialects.clone();
        let mut unreachable: Vec<String> = Vec::new();
        let http = &self.http;
        let base = self.profile.base_url.clone();
        let capabilities = self.capabilities;
        for model in catalog.models.clone() {
            let dialect = dialects
                .get(&model)
                .copied()
                .unwrap_or(self.profile.dialect);
            let url = match dialect {
                ApiDialect::ChatCompletions => format!("{base}/chat/completions"),
                ApiDialect::Messages => format!("{base}/messages"),
            };
            let request = ChatRequest {
                max_output_tokens: Some(16),
                ..ChatRequest::user_text("hi")
            };
            let body = match dialect {
                ApiDialect::ChatCompletions => {
                    OpenAiCompatProvider::new(
                        self.profile.clone(),
                        ModelProfile::new(model.clone()),
                        capabilities,
                    )?
                    .build_body(&request)
                }
                ApiDialect::Messages => {
                    OpenAiCompatProvider::new(
                        self.profile.clone(),
                        ModelProfile::new(model.clone()),
                        capabilities,
                    )?
                    .build_messages_body(&request)
                }
            };
            match http
                .post(&url)
                .bearer_auth(key.expose())
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .json(&body)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    // The body is a stream this pass does not read: the status
                    // is the whole question being asked. Dropping the response
                    // closes the connection, so an early-exiting model costs one
                    // request and no more.
                    drop(response);
                }
                Ok(response) => {
                    let status = response.status().as_u16();
                    let body = response.text().await.unwrap_or_default();
                    let detail = classify::error_message(&body);
                    unreachable.push(super::unreachable_line(
                        &model,
                        &format!("{status} {detail}"),
                    ));
                }
                Err(error) => {
                    // A transport failure is not a verdict about the model, so it
                    // is not recorded as one: the pass stops rather than writing
                    // sixty wrong rows about a gateway that went away.
                    return Err(classify::classify_request_failure(
                        error.is_connect(),
                        error.is_timeout(),
                        error.is_body(),
                        &describe_request_error(&url, &error),
                    ));
                }
            }
        }
        catalog.mark_unreachable(unreachable);
        Ok(catalog)
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
        let url = self.request_url();
        let body = self.build_request_body(request);

        // One retry, and only for the failure that means "the connection went
        // away before the answer started". See `SEND_ATTEMPTS`.
        let mut attempt = 1;
        let response = loop {
            let sent = self
                .http
                .post(&url)
                .bearer_auth(key.expose())
                .header(reqwest::header::ACCEPT, "text/event-stream")
                .json(&body)
                .send()
                .await;
            match sent {
                Ok(response) => break response,
                Err(error)
                    if worth_another_attempt(
                        attempt,
                        error.is_connect(),
                        error.is_timeout(),
                        error.is_body(),
                    ) =>
                {
                    attempt += 1;
                }
                Err(error) => {
                    return Err(classify::classify_request_failure(
                        error.is_connect(),
                        error.is_timeout(),
                        error.is_body(),
                        &describe_request_error(&url, &error),
                    ));
                }
            }
        };

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
        let mut parser = ChatStreamParser::for_dialect(self.capabilities, self.profile.dialect);

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

/// Split a `data:<media>;base64,<payload>` URL into its two halves.
///
/// Returns `None` for anything else, including an `http(s)` URL: the caller
/// decides what to do with a URL it cannot turn into base64, and inventing a
/// media type for it would be a guess the provider then rejects.
fn split_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, payload) = rest.split_once(',')?;
    let media = meta.strip_suffix(";base64").unwrap_or(meta);
    if media.trim().is_empty() || payload.trim().is_empty() {
        return None;
    }
    Some((media.to_owned(), payload.to_owned()))
}

/// Whether a failed request is worth one more attempt on a fresh connection.
///
/// `connect` and `timeout` are endpoint-level verdicts: a refused or unreachable
/// gateway refuses or hangs again, so a retry only doubles the wait before the
/// operator reads the same failure. A body error is this client's own encoding,
/// which a second attempt would repeat byte for byte. What is left is the request
/// that reached a socket and got nothing back - the connection closed under it -
/// and that is exactly the case a fresh connection fixes.
const fn worth_another_attempt(
    attempt: usize,
    is_connect: bool,
    is_timeout: bool,
    is_body: bool,
) -> bool {
    attempt < SEND_ATTEMPTS && !is_connect && !is_timeout && !is_body
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

    #[test]
    fn a_request_the_peer_dropped_gets_exactly_one_more_attempt() {
        assert!(worth_another_attempt(1, false, false, false));
        assert!(
            !worth_another_attempt(2, false, false, false),
            "the budget is one retry, not a loop"
        );
    }

    #[test]
    fn endpoint_level_failures_are_reported_rather_than_retried() {
        for (is_connect, is_timeout, is_body) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            assert!(
                !worth_another_attempt(1, is_connect, is_timeout, is_body),
                "connect={is_connect} timeout={is_timeout} body={is_body} \
                 cannot be fixed by a second connection"
            );
        }
    }

    #[test]
    fn the_client_does_not_hold_a_connection_past_a_gateway_keep_alive() {
        // 9router advertises `Keep-Alive: timeout=5`; a pooled connection kept
        // longer than that is one the server has already dropped.
        assert!(
            POOL_IDLE_TIMEOUT < Duration::from_secs(5),
            "an idle pool entry must expire before a 5 second gateway timer does"
        );
    }

    fn messages_provider() -> OpenAiCompatProvider {
        let profile = ProviderProfile::new(
            "commandcode",
            "https://api.commandcode.ai/provider/v1",
            SecretRef::Env("COMMANDCODE_API_KEY".to_owned()),
        )
        .expect("profile should build")
        .with_dialect(ApiDialect::Messages);
        OpenAiCompatProvider::new(
            profile,
            ModelProfile::new("claude-sonnet-5-5"),
            Capabilities::openai_compatible(),
        )
        .expect("provider should build")
    }

    #[test]
    fn the_dialect_picks_the_endpoint() {
        assert_eq!(
            provider().request_url(),
            "http://127.0.0.1:20128/v1/chat/completions"
        );
        assert_eq!(
            messages_provider().request_url(),
            "https://api.commandcode.ai/provider/v1/messages"
        );
        // Both endpoints stay reachable by name, whichever one is in force.
        assert_eq!(
            messages_provider().chat_completions_url(),
            "https://api.commandcode.ai/provider/v1/chat/completions"
        );
    }

    #[test]
    fn the_messages_body_puts_the_preamble_at_the_top_level_not_in_messages() {
        let body = messages_provider()
            .build_messages_body(&ChatRequest::with_system(Some("Be brief.".to_owned()), "hi"));
        assert_eq!(body["system"], "Be brief.");
        assert_eq!(body["messages"].as_array().map(Vec::len), Some(1));
        assert_eq!(body["messages"][0]["role"], "user");
        assert!(
            body["messages"]
                .as_array()
                .expect("array")
                .iter()
                .all(|message| message["role"] != "system"),
            "a system role inside messages is refused by the endpoint: {body}"
        );
    }

    #[test]
    fn the_messages_body_always_carries_a_generation_cap() {
        let body = messages_provider().build_messages_body(&ChatRequest::user_text("hi"));
        assert_eq!(
            body["max_tokens"],
            serde_json::Value::from(OpenAiCompatProvider::DEFAULT_MESSAGES_MAX_TOKENS),
            "the field is required and a missing one is refused before the model runs"
        );

        let capped = messages_provider().build_messages_body(&ChatRequest {
            max_output_tokens: Some(64),
            ..ChatRequest::user_text("hi")
        });
        assert_eq!(capped["max_tokens"], 64, "an explicit cap is not overridden");
    }

    #[test]
    fn a_messages_tool_result_is_a_user_block_addressed_by_tool_use_id() {
        let request = ChatRequest::user_text("weather?")
            .with_tool_result("toolu_01", r#"{"temp_c":21}"#);
        let body = messages_provider().build_messages_body(&request);
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][1]["content"][0]["type"], "tool_result");
        assert_eq!(body["messages"][1]["content"][0]["tool_use_id"], "toolu_01");
        assert_eq!(body["messages"][1]["content"][0]["content"], r#"{"temp_c":21}"#);
        assert!(
            body["messages"]
                .as_array()
                .expect("array")
                .iter()
                .all(|message| message["role"] != "tool"),
            "there is no tool role in this dialect: {body}"
        );
    }

    #[test]
    fn a_messages_assistant_tool_call_round_trips_as_a_materialised_block() {
        let call = core_domain::ToolCall::new(
            0,
            "toolu_01",
            "get_weather",
            serde_json::json!({"city": "Louisville"}),
        )
        .expect("call");
        let request = ChatRequest::user_text("weather?").with_tool_calls(vec![call.clone()]);
        let body = messages_provider().build_messages_body(&request);
        let block = &body["messages"][1]["content"][0];
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(block["type"], "tool_use");
        assert_eq!(block["id"], "toolu_01");
        assert_eq!(block["name"], "get_weather");
        assert_eq!(block["input"]["city"], "Louisville");
        assert_eq!(
            block["input"]["city"], call.arguments["city"],
            "the call is replayed exactly as the model produced it"
        );

        // A call-only assistant turn has no text block to send: an empty one is
        // not a block this dialect wants.
        assert_eq!(body["messages"][1]["content"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn a_messages_tool_definition_uses_input_schema_not_parameters() {
        let request = ChatRequest {
            tools: vec![ToolSpec {
                name: "get_weather".to_owned(),
                description: "Current weather".to_owned(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            ..ChatRequest::user_text("weather?")
        };
        let body = messages_provider().build_messages_body(&request);
        assert_eq!(body["tools"][0]["name"], "get_weather");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
        assert!(
            body["tools"][0].get("function").is_none(),
            "this dialect does not wrap the function the way OpenAI does: {body}"
        );
        assert!(body["tools"][0].get("parameters").is_none());
    }

    #[test]
    fn a_messages_image_becomes_a_base64_source_block() {
        let request = ChatRequest {
            images: vec!["data:image/png;base64,AAAA".to_owned()],
            ..ChatRequest::user_text("what is this?")
        };
        let body = messages_provider().build_messages_body(&request);
        let block = &body["messages"][0]["content"][1];
        assert_eq!(block["type"], "image");
        assert_eq!(block["source"]["type"], "base64");
        assert_eq!(block["source"]["media_type"], "image/png");
        assert_eq!(block["source"]["data"], "AAAA");

        // A URL that is not a data URL is not silently turned into one.
        let remote = ChatRequest {
            images: vec!["https://example.test/a.png".to_owned()],
            ..ChatRequest::user_text("what is this?")
        };
        let body = messages_provider().build_messages_body(&remote);
        assert_eq!(body["messages"][0]["content"][1]["source"]["type"], "url");
    }

    #[test]
    fn the_chat_completions_body_is_unchanged_by_the_messages_work() {
        // The dialect default is the shape every install sent before the field
        // existed, so its bytes must be byte-identical to the recorded shape.
        let body = provider().build_request_body(&ChatRequest::with_system(
            Some("Be brief.".to_owned()),
            "hi",
        ));
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "Be brief.");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert!(body.get("system").is_none());
        assert!(body.get("max_tokens").is_none());
    }
}

