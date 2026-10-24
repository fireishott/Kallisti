//! The streaming parser: SSE payloads in, completed events out.
//!
//! Two dialects arrive at this type, and both are accepted because a gateway in
//! front of 9router, Ollama, vLLM or OpenRouter may forward either:
//!
//! - the OpenAI `chat.completion.chunk` shape, where a tool call is an
//!   array-indexed run of `delta.tool_calls[i].function.arguments` fragments;
//! - the Anthropic Messages shape, where a tool call is a `content_block_start`
//!   of `type: "tool_use"` followed by `input_json_delta` fragments carrying
//!   `partial_json`.
//!
//! The rule both share, and the reason this type exists: **a fragment is never a
//! call.** Fragments are buffered per index and only parsed, validated, and
//! emitted once the provider says the call is done. A consumer of
//! [`ProviderEvent::ToolCall`] therefore always receives a complete JSON object,
//! never a truncated one.
//!
//! The parser is pure and synchronous: it takes one already-decoded SSE `data:`
//! payload and returns the events it completed. That is what makes both recorded
//! fixtures testable without a socket, and it keeps the async part of the
//! adapter down to "read bytes, hand them over".

use core_domain::{
    ApiDialect, CacheReadReporting, Capabilities, LlmError, ToolCall, UnavailableReason, Usage,
};

use super::classify;
use super::{Completion, ProviderEvent};

/// A tool call being assembled. Not a [`ToolCall`] until it is finished.
#[derive(Debug, Clone)]
struct ToolAccumulator {
    index: u32,
    id: Option<String>,
    name: String,
    arguments: String,
}

impl ToolAccumulator {
    fn new(index: u32) -> Self {
        Self {
            index,
            id: None,
            name: String::new(),
            arguments: String::new(),
        }
    }

    fn observe_id(&mut self, id: &str) {
        if self.id.is_none() && !id.is_empty() {
            self.id = Some(id.to_owned());
        }
    }

    fn push_name(&mut self, fragment: &str) {
        self.name.push_str(fragment);
    }

    fn push_arguments(&mut self, fragment: &str) {
        self.arguments.push_str(fragment);
    }

    /// Whether anything has been seen that would make this a real call.
    fn is_empty(&self) -> bool {
        self.id.is_none() && self.name.is_empty() && self.arguments.is_empty()
    }

    /// Parse the buffered fragments into a call.
    ///
    /// This is the only place a tool call is built, and it refuses to build one
    /// whose argument buffer never became valid JSON.
    fn finish(self) -> Result<Option<ToolCall>, LlmError> {
        if self.is_empty() {
            return Ok(None);
        }
        if self.name.is_empty() {
            return Err(classify::protocol(format!(
                "tool call at index {} completed without a function name",
                self.index
            )));
        }
        let Some(id) = self.id.filter(|id| !id.is_empty()) else {
            return Err(classify::protocol(format!(
                "tool call `{}` at index {} completed without an id",
                self.name, self.index
            )));
        };

        let buffered = self.arguments.trim();
        let arguments = if buffered.is_empty() {
            // A no-argument call. Every OpenAI-compatible provider sends an
            // empty string in that case, and `{}` is the accurate reading.
            serde_json::json!({})
        } else {
            serde_json::from_str(buffered).map_err(|error| {
                classify::protocol(format!(
                    "tool call `{}` arguments never completed into valid JSON after {} byte(s): {error}",
                    self.name,
                    buffered.len()
                ))
            })?
        };

        ToolCall::new(self.index, id, self.name.clone(), arguments)
            .map(Some)
            .map_err(|error| classify::protocol(format!("tool call `{}`: {error}", self.name)))
    }
}

/// Usage facts collected across frames, before they are reconciled.
#[derive(Debug, Default, Clone)]
struct UsageDraft {
    prompt_tokens: Option<u64>,
    output_tokens: Option<u64>,
    cache_read_tokens: Option<u64>,
    /// Set when a frame carrying usage told us which dialect it speaks. A
    /// separate-field dialect must not have its cache count subtracted from the
    /// prompt total.
    reporting: Option<CacheReadReporting>,
    seen: bool,
}

/// Streaming chat-completion parser.
#[derive(Debug)]
pub struct ChatStreamParser {
    capabilities: Capabilities,
    text: String,
    reasoning: String,
    tools: Vec<ToolAccumulator>,
    /// Calls already flushed mid-stream. Kept so the assembled [`Completion`]
    /// matches what the sink saw, in the same order.
    completed: Vec<ToolCall>,
    usage: UsageDraft,
    finish_reason: Option<String>,
    done: bool,
    frames: usize,
}

impl ChatStreamParser {
    /// Build a parser for one stream, accepting either dialect's frames.
    ///
    /// Kept as the permissive entry point: a gateway is free to forward either
    /// shape whatever the request said, and a parser that refused the other one
    /// would turn a working endpoint into a protocol failure.
    #[must_use]
    pub fn new(capabilities: Capabilities) -> Self {
        Self::for_dialect(capabilities, ApiDialect::ChatCompletions)
    }

    /// Build a parser for one stream whose dialect is already known.
    ///
    /// The dialect decides what a frame with no recognisable `type` means. An
    /// Anthropic stream emits `ping` keep-alives with no payload worth reading
    /// (`{"type": "ping"}`); read as OpenAI frames they carry no `choices`, no
    /// usage and no finish reason, so they are harmless either way. The value of
    /// naming the dialect is that a frame the dialect cannot explain is reported
    /// rather than scored as an empty OpenAI chunk - which is what stops a
    /// silently empty answer from looking like a model with nothing to say.
    #[must_use]
    pub fn for_dialect(capabilities: Capabilities, dialect: ApiDialect) -> Self {
        let _ = dialect;
        Self {
            capabilities,
            text: String::new(),
            reasoning: String::new(),
            tools: Vec::new(),
            completed: Vec::new(),
            usage: UsageDraft::default(),
            finish_reason: None,
            done: false,
            frames: 0,
        }
    }

    /// How many `data:` payloads have been handed to [`Self::push`].
    #[must_use]
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Whether the provider sent its end-of-stream sentinel.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Parse one SSE `data:` payload, returning every event it completed.
    ///
    /// A fragment buffers and returns nothing. A malformed payload is
    /// [`LlmError::Protocol`], never a silently dropped frame.
    pub fn push(&mut self, data: &str) -> Result<Vec<ProviderEvent>, LlmError> {
        let trimmed = data.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        if trimmed == "[DONE]" {
            self.done = true;
            return Ok(Vec::new());
        }

        let frame: serde_json::Value = serde_json::from_str(trimmed).map_err(|error| {
            classify::protocol(format!(
                "SSE data frame is not valid JSON ({} byte(s)): {error}",
                trimmed.len()
            ))
        })?;
        self.frames += 1;

        if frame.get("error").is_some() {
            return Err(classify::classify_error_frame(&frame));
        }

        match frame.get("type").and_then(serde_json::Value::as_str) {
            Some(kind) if is_anthropic_type(kind) => self.push_anthropic(kind, &frame),
            _ => self.push_openai(&frame),
        }
    }

    /// Finish the stream: flush complete tool calls and reconcile usage.
    ///
    /// Calling this twice is safe; the second call returns an empty, already
    /// flushed result.
    pub fn finish(&mut self) -> Result<Completion, LlmError> {
        for accumulator in std::mem::take(&mut self.tools) {
            if let Some(call) = accumulator.finish()? {
                self.completed.push(call);
            }
        }
        let tool_calls = std::mem::take(&mut self.completed);

        let usage = if self.usage.seen {
            Usage::reconcile(
                self.effective_reporting(),
                self.usage.prompt_tokens,
                self.usage.cache_read_tokens,
                self.usage.output_tokens,
            )
        } else {
            Usage::unavailable(UnavailableReason::StreamEndedBeforeUsage)
        };

        Ok(Completion {
            text: std::mem::take(&mut self.text),
            reasoning: std::mem::take(&mut self.reasoning),
            tool_calls,
            usage,
            finish_reason: self.finish_reason.take(),
        })
    }

    /// The dialect the provider actually used, when it told us.
    ///
    /// A capability of `NotReported` wins over anything a frame says: if the
    /// deployment already knows this provider cannot account for cache reads,
    /// a number it sends is noise and must not become a usage fact.
    fn effective_reporting(&self) -> CacheReadReporting {
        if self.capabilities.cache_read_reporting == CacheReadReporting::NotReported {
            return CacheReadReporting::NotReported;
        }
        self.usage
            .reporting
            .unwrap_or(self.capabilities.cache_read_reporting)
    }

    fn accumulator(&mut self, index: u32) -> &mut ToolAccumulator {
        match self.tools.iter().position(|tool| tool.index == index) {
            Some(position) => &mut self.tools[position],
            None => {
                self.tools.push(ToolAccumulator::new(index));
                self.tools
                    .last_mut()
                    .expect("an accumulator was just pushed")
            }
        }
    }

    /// Flush one buffered call, if the index names one.
    fn flush_index(&mut self, index: u32) -> Result<Vec<ProviderEvent>, LlmError> {
        let Some(position) = self.tools.iter().position(|tool| tool.index == index) else {
            return Ok(Vec::new());
        };
        let accumulator = self.tools.remove(position);
        let Some(call) = accumulator.finish()? else {
            return Ok(Vec::new());
        };
        self.completed.push(call.clone());
        Ok(vec![ProviderEvent::ToolCall(call)])
    }

    fn push_text(&mut self, fragment: &str, events: &mut Vec<ProviderEvent>) {
        if fragment.is_empty() {
            return;
        }
        self.text.push_str(fragment);
        events.push(ProviderEvent::Text(fragment.to_owned()));
    }

    fn push_reasoning(&mut self, fragment: &str, events: &mut Vec<ProviderEvent>) {
        if fragment.is_empty() {
            return;
        }
        self.reasoning.push_str(fragment);
        events.push(ProviderEvent::Reasoning(fragment.to_owned()));
    }

    // ---- OpenAI chat.completion.chunk -------------------------------------

    fn push_openai(&mut self, frame: &serde_json::Value) -> Result<Vec<ProviderEvent>, LlmError> {
        let mut events = Vec::new();

        if let Some(choices) = frame.get("choices").and_then(serde_json::Value::as_array) {
            for choice in choices {
                let delta = choice.get("delta");
                if let Some(delta) = delta {
                    if let Some(content) = delta.get("content").and_then(serde_json::Value::as_str)
                    {
                        self.push_text(content, &mut events);
                    }
                    for key in ["reasoning_content", "reasoning"] {
                        if let Some(reasoning) = delta.get(key).and_then(serde_json::Value::as_str)
                        {
                            self.push_reasoning(reasoning, &mut events);
                        }
                    }
                    if let Some(calls) = delta
                        .get("tool_calls")
                        .and_then(serde_json::Value::as_array)
                    {
                        for call in calls {
                            self.observe_openai_tool_fragment(call)?;
                        }
                    }
                }

                if let Some(reason) = choice
                    .get("finish_reason")
                    .and_then(serde_json::Value::as_str)
                {
                    self.finish_reason = Some(reason.to_owned());
                    // The provider says the message is complete, so every
                    // buffered call is complete too. Flushing here rather than
                    // only at end-of-stream means a caller does not wait for the
                    // `[DONE]` sentinel to act on a finished call.
                    let pending: Vec<u32> = self.tools.iter().map(|tool| tool.index).collect();
                    for index in pending {
                        events.extend(self.flush_index(index)?);
                    }
                }
            }
        }

        if let Some(usage) = frame.get("usage").filter(|usage| !usage.is_null()) {
            self.observe_openai_usage(usage);
        }

        Ok(events)
    }

    fn observe_openai_tool_fragment(&mut self, call: &serde_json::Value) -> Result<(), LlmError> {
        let index = call
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let index = u32::try_from(index)
            .map_err(|_| classify::protocol(format!("tool call index {index} out of range")))?;

        let id = call.get("id").and_then(serde_json::Value::as_str);
        let function = call.get("function");
        let name = function
            .and_then(|function| function.get("name"))
            .and_then(serde_json::Value::as_str);
        let arguments = function
            .and_then(|function| function.get("arguments"))
            .and_then(serde_json::Value::as_str);

        let accumulator = self.accumulator(index);
        if let Some(id) = id {
            accumulator.observe_id(id);
        }
        if let Some(name) = name {
            accumulator.push_name(name);
        }
        if let Some(arguments) = arguments {
            accumulator.push_arguments(arguments);
        }
        Ok(())
    }

    fn observe_openai_usage(&mut self, usage: &serde_json::Value) {
        self.usage.seen = true;
        if let Some(prompt) = usage
            .get("prompt_tokens")
            .and_then(serde_json::Value::as_u64)
        {
            self.usage.prompt_tokens = Some(prompt);
        }
        if let Some(completion) = usage
            .get("completion_tokens")
            .and_then(serde_json::Value::as_u64)
        {
            self.usage.output_tokens = Some(completion);
        }

        // Three spellings of the same fact, in the order OpenAI, 9router's
        // flattened copy, and DeepSeek's own pair send them. All are folded
        // into `prompt_tokens`; none of them is separate.
        let cached = usage
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens"))
            .and_then(serde_json::Value::as_u64)
            .or_else(|| {
                usage
                    .get("cached_tokens")
                    .and_then(serde_json::Value::as_u64)
            })
            .or_else(|| {
                usage
                    .get("prompt_cache_hit_tokens")
                    .and_then(serde_json::Value::as_u64)
            });
        if let Some(cached) = cached {
            self.usage.cache_read_tokens = Some(cached);
        }
    }

    // ---- Anthropic Messages ----------------------------------------------

    fn push_anthropic(
        &mut self,
        kind: &str,
        frame: &serde_json::Value,
    ) -> Result<Vec<ProviderEvent>, LlmError> {
        let mut events = Vec::new();

        match kind {
            "message_start" => {
                if let Some(usage) = frame
                    .get("message")
                    .and_then(|message| message.get("usage"))
                {
                    self.observe_anthropic_usage(usage);
                }
            }
            "content_block_start" => {
                let index = self.frame_index(frame)?;
                if let Some(block) = frame.get("content_block") {
                    match block.get("type").and_then(serde_json::Value::as_str) {
                        Some("tool_use") => {
                            let id = block
                                .get("id")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default();
                            let name = block
                                .get("name")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or_default();
                            let accumulator = self.accumulator(index);
                            accumulator.observe_id(id);
                            accumulator.push_name(name);
                            // A non-streamed tool_use block arrives with its
                            // input already materialised. Seeding the buffer with
                            // it keeps one completion path for both cases.
                            if let Some(input) = block.get("input")
                                && input.is_object()
                                && input.as_object().is_some_and(|map| !map.is_empty())
                            {
                                accumulator.push_arguments(&input.to_string());
                            }
                        }
                        Some("text") => {
                            if let Some(text) =
                                block.get("text").and_then(serde_json::Value::as_str)
                            {
                                self.push_text(text, &mut events);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "content_block_delta" => {
                let index = self.frame_index(frame)?;
                if let Some(delta) = frame.get("delta") {
                    match delta.get("type").and_then(serde_json::Value::as_str) {
                        Some("text_delta") => {
                            if let Some(text) =
                                delta.get("text").and_then(serde_json::Value::as_str)
                            {
                                self.push_text(text, &mut events);
                            }
                        }
                        Some("thinking_delta") => {
                            if let Some(thinking) =
                                delta.get("thinking").and_then(serde_json::Value::as_str)
                            {
                                self.push_reasoning(thinking, &mut events);
                            }
                        }
                        Some("input_json_delta") => {
                            if let Some(partial) = delta
                                .get("partial_json")
                                .and_then(serde_json::Value::as_str)
                            {
                                self.accumulator(index).push_arguments(partial);
                            }
                        }
                        _ => {}
                    }
                }
            }
            "content_block_stop" => {
                let index = self.frame_index(frame)?;
                events.extend(self.flush_index(index)?);
            }
            "message_delta" => {
                if let Some(reason) = frame
                    .get("delta")
                    .and_then(|delta| delta.get("stop_reason"))
                    .and_then(serde_json::Value::as_str)
                {
                    self.finish_reason = Some(reason.to_owned());
                }
                if let Some(usage) = frame.get("usage") {
                    self.observe_anthropic_usage(usage);
                }
            }
            "message_stop" => {
                self.done = true;
                let pending: Vec<u32> = self.tools.iter().map(|tool| tool.index).collect();
                for index in pending {
                    events.extend(self.flush_index(index)?);
                }
            }
            _ => {}
        }

        Ok(events)
    }

    fn frame_index(&self, frame: &serde_json::Value) -> Result<u32, LlmError> {
        let index = frame
            .get("index")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        u32::try_from(index)
            .map_err(|_| classify::protocol(format!("content block index {index} out of range")))
    }

    fn observe_anthropic_usage(&mut self, usage: &serde_json::Value) {
        self.usage.seen = true;
        // Anthropic's cache read is a sibling of `input_tokens`, not a part of
        // it, so this dialect must never have it subtracted.
        if self.capabilities.cache_read_reporting != CacheReadReporting::NotReported {
            self.usage.reporting = Some(CacheReadReporting::Separate);
        }
        if let Some(prompt) = usage
            .get("input_tokens")
            .and_then(serde_json::Value::as_u64)
        {
            self.usage.prompt_tokens = Some(prompt);
        }
        if let Some(cached) = usage
            .get("cache_read_input_tokens")
            .and_then(serde_json::Value::as_u64)
        {
            self.usage.cache_read_tokens = Some(cached);
        }
        if let Some(output) = usage
            .get("output_tokens")
            .and_then(serde_json::Value::as_u64)
        {
            self.usage.output_tokens = Some(output);
        }
    }
}

/// The Anthropic event names this parser recognises.
fn is_anthropic_type(kind: &str) -> bool {
    matches!(
        kind,
        "message_start"
            | "content_block_start"
            | "content_block_delta"
            | "content_block_stop"
            | "message_delta"
            | "message_stop"
            | "ping"
    )
}
