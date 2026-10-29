//! The Google Antigravity request shape: `v1internal`.
//!
//! Antigravity is not an OpenAI-compatible surface with a different base URL.
//! Its request is *wrapped*: the model, the billable cloud project, a user-agent
//! string, a request type and a request id all sit beside the conversation
//! rather than inside it, and the conversation itself is Gemini's
//! `contents`/`parts` shape with `functionCall` and `functionResponse` blocks
//! instead of `tool_calls`.
//!
//! The shape below was read off the bundle that actually serves it -
//! `requestType`, `functionDeclarations`, `toolConfig.functionCallingConfig` and
//! the `agent/<id>/<epoch>/...` request id all come from there. Every field is
//! optional on the wire except `project`, which the endpoint requires and which
//! therefore fails here, where the message can say what is wrong, instead of as
//! an opaque 400 before the model runs.
//!
//! Two rules carried from the rest of the provider layer:
//!
//! - this module is pure. It builds a [`serde_json::Value`] and some strings and
//!   performs no I/O, so the wire shape is testable without a socket;
//! - nothing here invents a value. A missing project is refused, a missing cap
//!   falls back to the provider's own, and a model id is passed through exactly
//!   as the catalogue spelled it.

use core_domain::{LlmError, ProviderProfile};

use super::{ChatMessage, ChatRequest, ChatRole, ToolSpec};

/// The largest answer asked for when a caller sets no cap on an Antigravity
/// turn.
///
/// The provider's own client sends 8192, and a request without a cap is refused
/// by the surface rather than defaulted, so this is a floor every model there
/// clears rather than a spend: no provider bills for tokens it did not generate.
pub const DEFAULT_ANTIGRAVITY_MAX_OUTPUT: u32 = 8_192;

/// The `User-Agent` the Antigravity surface expects.
///
/// Not decoration: the endpoint's own framing depends on it, which is why the
/// client that serves Antigravity sends a fixed one rather than its real
/// identity.
pub const ANTIGRAVITY_USER_AGENT: &str = "antigravity/ide/2.1.1 darwin/arm64";

/// What kind of turn this is, as Antigravity spells it.
///
/// `agent` is a normal turn and is what every Kallisti turn sends. `search` is
/// the grounded variant; the daemon does not use it, and the value exists here so
/// the vocabulary lives in one place rather than as a bare literal at a call
/// site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestType {
    /// An ordinary interactive turn.
    Agent,
    /// A grounded turn, for a caller that wants search results folded in.
    Search,
}

impl RequestType {
    /// The wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Search => "search",
        }
    }
}

/// The streaming URL for one profile's base.
///
/// Antigravity's streaming form is `streamGenerateContent` with `alt=sse`; the
/// non-streaming form is `generateContent`. Both hang off the same `v1internal`
/// verb, so the base URL carries no version of its own.
#[must_use]
pub fn stream_url(base_url: &str) -> String {
    format!(
        "{}/v1internal:streamGenerateContent?alt=sse",
        base_url.trim_end_matches('/')
    )
}

/// The non-streaming URL, for a caller that wants the whole answer in one body.
#[must_use]
pub fn generate_url(base_url: &str) -> String {
    format!(
        "{}/v1internal:generateContent",
        base_url.trim_end_matches('/')
    )
}

/// The cloud project a request is billed to, or a refusal that names the fix.
///
/// # Errors
///
/// [`LlmError::Protocol`] when the profile names the Antigravity dialect and
/// carries no project.
pub fn project_of(profile: &ProviderProfile) -> Result<&str, LlmError> {
    match profile.project.as_deref().map(str::trim) {
        Some(project) if !project.is_empty() => Ok(project),
        _ => Err(LlmError::Protocol(format!(
            "the `{}` provider speaks Antigravity and names no cloud project; \
             reconnect the account so one is discovered",
            profile.label
        ))),
    }
}

/// The request id Antigravity's own client sends.
///
/// The shape is `agent/<conversation>/<epoch-millis>/<trajectory>/<step>`. The
/// ids and the clock are supplied by the caller: this module has no clock and no
/// random source of its own, which is what keeps it pure.
#[must_use]
pub fn request_id(conversation: &str, trajectory: &str, epoch_millis: u128, step: usize) -> String {
    format!("agent/{conversation}/{epoch_millis}/{trajectory}/{step}")
}

/// Build the streaming request body for one turn.
///
/// # Errors
///
/// [`LlmError::Protocol`] when the profile names no project, and when the model
/// id is empty.
pub fn build_body(
    profile: &ProviderProfile,
    model: &str,
    request: &ChatRequest,
    request_type: RequestType,
    request_id: &str,
    session_id: &str,
) -> Result<serde_json::Value, LlmError> {
    let project = project_of(profile)?;
    if model.trim().is_empty() {
        return Err(LlmError::Protocol(
            "an Antigravity request needs a model id".to_owned(),
        ));
    }

    let mut contents: Vec<serde_json::Value> = Vec::with_capacity(request.messages.len());
    let last = request.messages.len().saturating_sub(1);
    for (index, message) in request.messages.iter().enumerate() {
        match message.role {
            // The preamble is not a message in this dialect; it is
            // `systemInstruction` beside the contents, built below.
            ChatRole::System => continue,
            ChatRole::Tool => {
                // Antigravity addresses a result by the function's name, not by
                // a call id: `functionResponse.name` must match the
                // `functionCall.name` it answers.
                let name = message.tool_call_id.clone().unwrap_or_default();
                contents.push(serde_json::json!({
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": name,
                            "response": { "content": message.content },
                        }
                    }],
                }));
            }
            ChatRole::Assistant => {
                let mut parts: Vec<serde_json::Value> = Vec::new();
                if !message.content.trim().is_empty() {
                    parts.push(serde_json::json!({ "text": message.content }));
                }
                for call in &message.tool_calls {
                    let mut part = serde_json::json!({
                        "functionCall": { "name": call.name, "args": call.arguments },
                    });
                    // A 2.5 thinking model issues a signature with every call and
                    // refuses the next round without it: "Function call is missing
                    // a thought_signature in functionCall parts". It goes back
                    // beside the call, unread, exactly as it arrived.
                    if let Some(signature) = &call.thought_signature {
                        part["thoughtSignature"] = serde_json::Value::String(signature.clone());
                    }
                    parts.push(part);
                }
                if !parts.is_empty() {
                    contents.push(serde_json::json!({ "role": "model", "parts": parts }));
                }
            }
            ChatRole::User if index == last && !request.images.is_empty() => {
                let mut parts: Vec<serde_json::Value> = Vec::new();
                if !message.content.trim().is_empty() {
                    parts.push(serde_json::json!({ "text": message.content }));
                }
                for url in &request.images {
                    if let Some((media, data)) = split_data_url(url) {
                        parts.push(serde_json::json!({
                            "inlineData": { "mimeType": media, "data": data },
                        }));
                    } else {
                        // A remote URL is named rather than fetched: this daemon
                        // does not reach out on a model's behalf, so the
                        // endpoint resolves a URI it can see.
                        parts.push(serde_json::json!({
                            "fileData": { "fileUri": url },
                        }));
                    }
                }
                contents.push(serde_json::json!({ "role": "user", "parts": parts }));
            }
            _ => {
                contents.push(serde_json::json!({
                    "role": "user",
                    "parts": [{ "text": message.content }],
                }));
            }
        }
    }

    // The preamble: the turn's own system text, or the system messages the
    // transcript carried when the caller did not lift them out. Dropping the
    // operator's instructions would be the worse failure, so they are joined.
    let preamble = request
        .system
        .as_deref()
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
        .or_else(|| {
            let joined = request
                .messages
                .iter()
                .filter(|message| message.role == ChatRole::System)
                .map(|message: &ChatMessage| message.content.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            (!joined.trim().is_empty()).then_some(joined)
        });

    // The provider's own sampling defaults, sent explicitly: they are what its
    // client uses, and a different triple changes the shape of the answer rather
    // than only its tone.
    let generation = serde_json::json!({
        "maxOutputTokens": request
            .max_output_tokens
            .unwrap_or(DEFAULT_ANTIGRAVITY_MAX_OUTPUT),
        "temperature": 1.0,
        "topP": 0.95,
        "topK": 40.0,
    });

    let mut inner = serde_json::json!({
        "sessionId": session_id,
        "contents": contents,
        "generationConfig": generation,
    });
    if let Some(preamble) = preamble {
        inner["systemInstruction"] = serde_json::json!({
            "role": "user",
            "parts": [{ "text": preamble }],
        });
    }
    if !request.tools.is_empty() {
        inner["tools"] = serde_json::json!([{
            "functionDeclarations": request
                .tools
                .iter()
                .map(declaration)
                .collect::<Vec<serde_json::Value>>(),
        }]);
        // VALIDATED, the mode the provider's own client asks for: the schema is
        // enforced by the server rather than only suggested by the prompt.
        inner["toolConfig"] = serde_json::json!({
            "functionCallingConfig": { "mode": "VALIDATED" },
        });
    }

    Ok(serde_json::json!({
        "project": project,
        "model": model,
        "userAgent": "antigravity",
        "requestType": request_type.as_str(),
        "requestId": request_id,
        "request": inner,
    }))
}

/// One tool as Antigravity declares it.
///
/// `parameters` keeps its JSON Schema. The provider's own client repairs schemas
/// whose types it does not accept; this daemon does not, so a schema the provider
/// rejects is reported as a provider refusal with the schema in the message
/// rather than silently rewritten into something else.
fn declaration(tool: &ToolSpec) -> serde_json::Value {
    serde_json::json!({
        "name": tool.name,
        "description": tool.description,
        "parameters": tool.parameters,
    })
}

/// Split a `data:` URL into its media type and its base64 payload.
///
/// Returns `None` for anything that is not a well-formed base64 data URL, which
/// the caller turns into a file reference instead of an inline block.
fn split_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    if !meta.ends_with(";base64") {
        return None;
    }
    let media = meta.trim_end_matches(";base64").trim();
    if media.is_empty() || data.is_empty() {
        return None;
    }
    Some((media.to_owned(), data.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_domain::{ApiDialect, SecretRef, ToolCall};

    fn profile(project: Option<&str>) -> ProviderProfile {
        ProviderProfile::new(
            "antigravity",
            "https://daily-cloudcode-pa.googleapis.com",
            SecretRef::Token("antigravity".to_owned()),
        )
        .expect("profile")
        .with_dialect(ApiDialect::Antigravity)
        .with_direct(true)
        .with_project(project.map(str::to_owned))
    }

    fn body(project: Option<&str>) -> serde_json::Value {
        build_body(
            &profile(project),
            "gemini-3-flash",
            &ChatRequest::with_system(Some("Be brief.".to_owned()), "weather?"),
            RequestType::Agent,
            "agent/c1/1700000000000/t1/1",
            "session-1",
        )
        .expect("builds")
    }

    #[test]
    fn a_replayed_call_carries_the_signature_the_model_issued() {
        // Gemini 2.5 thinking models return `thoughtSignature` with every
        // functionCall and refuse the next round without it. This is the 400 the
        // user hit: "Function call is missing a thought_signature".
        let mut call = ToolCall::new(
            0,
            "call_1",
            "run_command",
            serde_json::json!({ "command": "ls" }),
        )
        .expect("a call");
        call.thought_signature = Some("sig-abc".to_owned());
        let request = ChatRequest {
            system: Some("Be brief.".to_owned()),
            messages: vec![
                ChatMessage::text(ChatRole::User, "list the directory"),
                ChatMessage {
                    role: ChatRole::Assistant,
                    content: String::new(),
                    tool_calls: vec![call],
                    tool_call_id: None,
                },
            ],
            ..ChatRequest::default()
        };
        let body = build_body(
            &profile(Some("proj-1")),
            "gemini-2.5-flash",
            &request,
            RequestType::Agent,
            "agent/c1/1700000000000/t1/1",
            "session-1",
        )
        .expect("builds");
        let parts = &body["request"]["contents"][1]["parts"];
        let part = &parts[0];
        assert_eq!(part["functionCall"]["name"], "run_command");
        assert_eq!(
            part["thoughtSignature"], "sig-abc",
            "the signature goes back beside the call, or the next round is a 400"
        );
    }

    #[test]
    fn the_stream_endpoint_is_the_v1internal_verb_with_sse() {
        assert_eq!(
            stream_url("https://daily-cloudcode-pa.googleapis.com/"),
            "https://daily-cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            generate_url("https://daily-cloudcode-pa.googleapis.com"),
            "https://daily-cloudcode-pa.googleapis.com/v1internal:generateContent"
        );
    }

    #[test]
    fn a_profile_without_a_project_is_refused_by_name() {
        let error = build_body(
            &profile(None),
            "gemini-3-flash",
            &ChatRequest::user_text("hi"),
            RequestType::Agent,
            "agent/c/t/s/1",
            "s",
        )
        .expect_err("must refuse");
        let LlmError::Protocol(detail) = error else {
            panic!("expected a protocol refusal");
        };
        assert!(detail.contains("cloud project"), "{detail}");
        assert!(detail.contains("antigravity"), "{detail}");
    }

    #[test]
    fn the_wrapper_carries_the_project_model_and_request_kind() {
        let body = body(Some("aicode-123"));
        assert_eq!(body["project"], "aicode-123");
        assert_eq!(body["model"], "gemini-3-flash");
        assert_eq!(body["userAgent"], "antigravity");
        assert_eq!(body["requestType"], "agent");
        assert_eq!(body["requestId"], "agent/c1/1700000000000/t1/1");
        // The conversation is nested, not top level: the whole point of the
        // wrapper is that the model and the project sit beside the request.
        assert!(body["request"]["contents"].is_array());
        assert!(body.get("contents").is_none());
        assert_eq!(body["request"]["sessionId"], "session-1");
    }

    #[test]
    fn the_preamble_is_an_instruction_beside_the_contents_not_a_message_in_them() {
        let body = body(Some("aicode-123"));
        assert_eq!(
            body["request"]["systemInstruction"]["parts"][0]["text"],
            "Be brief."
        );
        let contents = body["request"]["contents"].as_array().expect("an array");
        assert_eq!(contents.len(), 1, "{contents:?}");
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[0]["parts"][0]["text"], "weather?");
    }

    #[test]
    fn an_assistant_turn_is_a_model_role_with_a_function_call_part() {
        let request = ChatRequest::user_text("what is the disk doing?")
            .with_tool_calls(vec![
                ToolCall::new(
                    0,
                    "run_command",
                    "run_command",
                    serde_json::json!({ "command": "df -h" }),
                )
                .expect("call"),
            ])
            .with_tool_result("run_command", "12% used");
        let body = build_body(
            &profile(Some("aicode-123")),
            "gemini-3-flash",
            &request,
            RequestType::Agent,
            "agent/c/t/s/1",
            "s",
        )
        .expect("builds");
        let contents = body["request"]["contents"].as_array().expect("array");
        let model_role = contents
            .iter()
            .find(|entry| entry["role"] == "model")
            .expect("a model turn");
        assert_eq!(
            model_role["parts"][0]["functionCall"]["name"],
            "run_command"
        );
        assert_eq!(
            model_role["parts"][0]["functionCall"]["args"]["command"],
            "df -h"
        );
        let tool = contents
            .iter()
            .find(|entry| {
                entry
                    .get("parts")
                    .and_then(|parts| parts.get(0))
                    .and_then(|part| part.get("functionResponse"))
                    .is_some()
            })
            .expect("a function response");
        assert_eq!(tool["role"], "user");
        assert_eq!(
            tool["parts"][0]["functionResponse"]["name"], "run_command",
            "a result is addressed by the function's name, not by a call id"
        );
        assert_eq!(
            tool["parts"][0]["functionResponse"]["response"]["content"],
            "12% used"
        );
    }

    #[test]
    fn tools_are_function_declarations_with_validated_calling() {
        let mut request = ChatRequest::user_text("weather?");
        request.tools = vec![ToolSpec {
            name: "echo_ping".to_owned(),
            description: "Replies with pong.".to_owned(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "words": { "type": "string" } },
                "required": ["words"],
            }),
        }];
        let body = build_body(
            &profile(Some("aicode-123")),
            "gemini-3-flash",
            &request,
            RequestType::Agent,
            "agent/c/t/s/1",
            "s",
        )
        .expect("builds");
        let declarations = &body["request"]["tools"][0]["functionDeclarations"][0];
        assert_eq!(declarations["name"], "echo_ping");
        // `parameters`, not `input_schema` and not a wrapped `function` object.
        assert_eq!(
            declarations["parameters"]["properties"]["words"]["type"],
            "string"
        );
        assert!(declarations.get("input_schema").is_none());
        assert_eq!(
            body["request"]["toolConfig"]["functionCallingConfig"]["mode"],
            "VALIDATED"
        );
    }

    #[test]
    fn an_image_rides_as_inline_data_and_a_remote_url_as_a_file_reference() {
        let mut request = ChatRequest::user_text("what is this?");
        request.images = vec![
            "data:image/png;base64,AAAA".to_owned(),
            "https://example.test/pic.png".to_owned(),
        ];
        let body = build_body(
            &profile(Some("aicode-123")),
            "gemini-3-flash",
            &request,
            RequestType::Agent,
            "agent/c/t/s/1",
            "s",
        )
        .expect("builds");
        let parts = &body["request"]["contents"][0]["parts"];
        assert_eq!(parts[0]["text"], "what is this?");
        assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
        assert_eq!(parts[1]["inlineData"]["data"], "AAAA");
        assert_eq!(
            parts[2]["fileData"]["fileUri"],
            "https://example.test/pic.png"
        );
    }

    #[test]
    fn the_generation_config_always_carries_a_cap_and_the_providers_own_sampling() {
        let body = body(Some("aicode-123"));
        let generation = &body["request"]["generationConfig"];
        assert_eq!(
            generation["maxOutputTokens"],
            super::DEFAULT_ANTIGRAVITY_MAX_OUTPUT
        );
        assert_eq!(generation["temperature"], 1.0);
        assert_eq!(generation["topP"], 0.95);
        assert_eq!(generation["topK"], 40.0);

        let mut capped = ChatRequest::user_text("hi");
        capped.max_output_tokens = Some(256);
        let body = build_body(
            &profile(Some("aicode-123")),
            "gemini-3-flash",
            &capped,
            RequestType::Agent,
            "agent/c/t/s/1",
            "s",
        )
        .expect("builds");
        assert_eq!(body["request"]["generationConfig"]["maxOutputTokens"], 256);
    }

    #[test]
    fn the_request_id_is_the_shape_its_own_client_sends() {
        assert_eq!(
            request_id("conv", "traj", 1_700_000_000_000, 3),
            "agent/conv/1700000000000/traj/3"
        );
        assert_eq!(RequestType::Agent.as_str(), "agent");
        assert_eq!(RequestType::Search.as_str(), "search");
    }
}
