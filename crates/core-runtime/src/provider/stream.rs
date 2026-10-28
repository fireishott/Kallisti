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

    /// Add one `arguments` fragment, tolerating a sender that replays what it
    /// already sent.
    ///
    /// The documented sender appends only new bytes. A gateway in front of
    /// 9router can instead send the whole object again on a later delta, and it
    /// can send the whole object growing, the same shapes the text path guards
    /// against in `antigravity_delta`. Appending blind turned `{...}` into
    /// `{...}{...}`, which fails as `trailing characters` or as `EOF while
    /// parsing a list` depending on where the cut fell, and the turn dies at the
    /// parser before any tool runs.
    ///
    /// A replay is recognised by position, never by content: the only fragment
    /// that can be a re-send is one that begins with everything already
    /// buffered, and its new suffix is the rest. Two earlier rules were content
    /// checks and both were wrong. "A fragment already contained in the buffer
    /// contributes nothing" dropped a legitimate continuation whenever the bytes
    /// happened to match the buffer's opening: a token-level stream sends a
    /// nested object's opener as its own fragment, so `{"` arrived inside
    /// `"questions": [` and was eaten because the buffer also starts with `{"`,
    /// leaving `[{"question"...` and `expected value` at the first question. That
    /// is the 535-byte `clarify` failure. A fragment that repeats a *short* run
    /// of bytes is indistinguishable from the next token, so nothing below is
    /// allowed to look at a fragment's content to decide whether it is new.
    ///
    /// A real truncation still ends short of valid JSON and is still refused in
    /// `finish`.
    fn push_arguments(&mut self, fragment: &str) {
        if fragment.is_empty() {
            return;
        }
        if self.arguments.is_empty() {
            self.arguments.push_str(fragment);
            return;
        }
        if let Some(fresh) = fragment.strip_prefix(self.arguments.as_str()) {
            // A growing re-send: only the suffix is new. An exact re-send strips
            // to empty and adds nothing, which is the whole-object replay case.
            self.arguments.push_str(fresh);
            return;
        }
        if fragment.trim().is_empty() {
            // Whitespace after a *complete* call is a frame boundary, not a new
            // serialisation, and treating it as one would throw the call away.
            // Whitespace before that is content: this provider streams token by
            // token, so a line of indentation, a blank line or the space between
            // two words arrives as its own fragment. Dropping those stripped
            // every script the model wrote down to one space of indent and turned
            // `defaults to 993` into `defaults to993`, which is why a written
            // Python file did not compile and the turn spent forty calls
            // rewriting it. The buffer has to be a whole value before this
            // fragment can be the whitespace between values.
            if looks_complete(&self.arguments) {
                return;
            }
            self.arguments.push_str(fragment);
            return;
        }
        // A fragment that is a complete JSON value on its own is a whole object,
        // not a continuation: the sender re-serialised the call between deltas
        // (a field reordered, whitespace moved, an escape spelled differently) so
        // it does not extend what is buffered, but it does say the same thing.
        // Appending it would glue two objects together and fail the parse at a
        // column nobody can explain from the payload; replacing keeps the newest
        // spelling, which is the one the sender meant. A genuine continuation is
        // never valid JSON on its own, so this cannot swallow one.
        if looks_complete(fragment) {
            self.arguments.clear();
            self.arguments.push_str(fragment);
            return;
        }
        // The buffer is already a whole JSON value and this fragment is neither a
        // repeat of it nor a new whole object, so appending could only ever
        // produce two values in one buffer, which is not JSON. That happens when
        // a gateway restarts its serialisation mid-object - the new spelling is
        // still incomplete, so it is not `looks_complete` yet - and the newest
        // fragment is the beginning of what the sender means. Later deltas extend
        // it normally because they start with it.
        if looks_complete(&self.arguments) {
            self.arguments.clear();
            self.arguments.push_str(fragment);
            return;
        }
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
            match serde_json::from_str(buffered) {
                Ok(value) => value,
                Err(error) => {
                    // Two objects glued together parse as neither. A sender that
                    // appends whole objects instead of fragments leaves exactly
                    // that, and the newest object is the one it meant, so the
                    // last complete one is recovered rather than the turn dying
                    // at the parser. A real truncation has no complete object to
                    // recover and is still refused, with the same message it
                    // always had.
                    match newest_object(buffered) {
                        Some(value) => value,
                        None => {
                            // The refusal carries one structural fact that costs
                            // nothing to compute and names the failure: a buffer
                            // with no object opener in it never got its first
                            // fragment. That is a different fault from a call cut
                            // short mid-object, and telling them apart from the
                            // phone is what stops this being diagnosed from the
                            // outside for the third time. No bytes of the payload
                            // are quoted: they can be file content.
                            let missing_opener = !buffered.contains('{');
                            let hint = if missing_opener {
                                "; the opening brace never arrived, so the call resumed mid-value"
                            } else {
                                ""
                            };
                            return Err(classify::protocol(format!(
                                "tool call `{}` arguments never completed into valid JSON after {} byte(s): {error}{hint}",
                                self.name,
                                buffered.len()
                            )));
                        }
                    }
                }
            }
        };

        ToolCall::new(self.index, id, self.name.clone(), arguments)
            .map(Some)
            .map_err(|error| classify::protocol(format!("tool call `{}`: {error}", self.name)))
    }
}

/// Whether a fragment is a complete JSON value on its own.
///
/// Used to tell "the sender re-serialised the whole call" from "this is the next
/// chunk of a long string". A chunk of a JSON document is virtually never valid
/// JSON by itself; a whole object always is.
fn looks_complete(fragment: &str) -> bool {
    let trimmed = fragment.trim();
    if trimmed.len() < 2 {
        return false;
    }
    matches!(trimmed.as_bytes()[0], b'{' | b'[')
        && serde_json::from_str::<serde_json::Value>(trimmed).is_ok()
}

/// The newest complete JSON object inside a buffer of concatenated ones.
///
/// A sender that appends whole objects leaves `{...}{...}`. The last object is
/// the newest and therefore the one the sender meant; earlier ones are stale.
/// String-aware, so a brace inside a quoted value is not a boundary. Only the
/// last few candidates are tried, so a pathological buffer cannot turn a refused
/// turn into a slow one.
fn newest_object(text: &str) -> Option<serde_json::Value> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut starts: Vec<usize> = Vec::new();
    for (index, byte) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => {
                if depth == 0 {
                    starts.push(index);
                }
                depth += 1;
            }
            b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    // Newest first, and at most a handful: if the whole buffer were one object
    // the parse that got us here would have succeeded, so a candidate at index 0
    // is only worth trying when nothing later works.
    for start in starts.iter().rev().take(4) {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text[*start..]) {
            return Some(value);
        }
    }
    None
}

/// The two things an inlined chain of thought can be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThinkState {
    /// Text landing in the answer.
    Answer,
    /// Text landing in the reasoning.
    Thinking,
}

/// Splits `<think>`-tagged content back into an answer and a reasoning.
///
/// Several OpenAI-compatible endpoints do not separate a chain of thought from
/// the reply. MiniMax inlines it into `delta.content` wrapped in `<think>` tags
/// by default, and a Qwen behind a gateway does the same. Read as plain content
/// those bytes land in the answer bubble and the reasoning column stays empty,
/// which is the whole bug this type exists to fix.
///
/// It is a per-fragment state machine rather than a whole-buffer scan because a
/// tag is routinely cut in half by the transport: `<thi` arrives on one frame and
/// `nk>` on the next. Any suffix that is a so-far-matching prefix of the tag is
/// therefore held and re-examined with the next fragment, so a split tag is
/// recognised and a `<` in ordinary prose (`a < b`) is released as soon as the
/// next character rules it out.
///
/// A held suffix is flushed when a frame says the message is finished, and again
/// at end of stream, so nothing is ever swallowed.
#[derive(Debug)]
struct ThinkDemux {
    state: ThinkState,
    /// Bytes held back: a strict, so-far-matching prefix of the tag that would
    /// change state.
    held: String,
}

/// Tag that opens reasoning.
const THINK_OPEN: &str = "<think>";
/// Tag that closes it.
const THINK_CLOSE: &str = "</think>";

impl ThinkDemux {
    fn new() -> Self {
        Self {
            state: ThinkState::Answer,
            held: String::new(),
        }
    }

    /// Whether the demuxer is holding bytes that have not been emitted.
    fn is_holding(&self) -> bool {
        !self.held.is_empty()
    }

    /// Feed one content fragment, returning `(answer, reasoning)` to emit.
    ///
    /// Either returned string may be empty. Concatenating every `answer` in
    /// order reproduces the reply, and every `reasoning` reproduces the chain of
    /// thought, whether or not the endpoint separated them.
    fn feed(&mut self, fragment: &str) -> (String, String) {
        let mut answer = String::new();
        let mut reasoning = String::new();
        if fragment.is_empty() && self.held.is_empty() {
            return (answer, reasoning);
        }

        let mut combined = std::mem::take(&mut self.held);
        combined.push_str(fragment);
        let tag = match self.state {
            ThinkState::Answer => THINK_OPEN,
            ThinkState::Thinking => THINK_CLOSE,
        };

        let mut index = 0usize;
        while index < combined.len() {
            let rest = &combined[index..];
            if rest.starts_with(tag) {
                self.state = match self.state {
                    ThinkState::Answer => ThinkState::Thinking,
                    ThinkState::Thinking => ThinkState::Answer,
                };
                index += tag.len();
                continue;
            }
            // A strict prefix of the tag cannot be decided yet: the next
            // fragment may complete it, and emitting it now would split a tag.
            if rest.len() < tag.len() && tag.starts_with(rest) {
                self.held.push_str(rest);
                break;
            }
            let character = rest
                .chars()
                .next()
                .expect("a non-empty slice has a first character");
            match self.state {
                ThinkState::Answer => answer.push(character),
                ThinkState::Thinking => reasoning.push(character),
            }
            index += character.len_utf8();
        }

        (answer, reasoning)
    }

    /// Release whatever is held, into the stream it belongs to.
    fn flush(&mut self) -> (String, String) {
        let held = std::mem::take(&mut self.held);
        match self.state {
            ThinkState::Answer => (held, String::new()),
            ThinkState::Thinking => (String::new(), held),
        }
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
    /// Which shape the *request* was sent in. It decides which frame families a
    /// reply is read with, and it is recorded rather than guessed so a frame
    /// nobody can explain is reported instead of scored as an empty answer.
    dialect: ApiDialect,
    /// What has already been emitted for the Antigravity dialect, per channel.
    ///
    /// Gemini's streaming surface is not one of the two delta shapes this parser
    /// was built for: `/v1internal:streamGenerateContent` sends candidate parts
    /// that may be incremental or already accumulated, and the difference is not
    /// announced. So the parser remembers what it has emitted and emits only the
    /// suffix. A sender that repeats itself costs nothing; a sender that
    /// accumulates is not printed twice.
    antigravity_seen: [String; 2],
    text: String,
    reasoning: String,
    tools: Vec<ToolAccumulator>,
    /// Calls already flushed mid-stream. Kept so the assembled [`Completion`]
    /// matches what the sink saw, in the same order.
    completed: Vec<ToolCall>,
    usage: UsageDraft,
    /// Splits an inlined chain of thought out of `delta.content`.
    think: ThinkDemux,
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
        Self {
            capabilities,
            dialect,
            antigravity_seen: [String::new(), String::new()],
            text: String::new(),
            reasoning: String::new(),
            tools: Vec::new(),
            completed: Vec::new(),
            usage: UsageDraft::default(),
            think: ThinkDemux::new(),
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

        // Antigravity first, and by shape rather than by dialect: a Cloud Code
        // frame is the only one that nests its answer under `response`, and a
        // rerouted reply that arrives in that shape must be read the same way
        // whatever this parser was told to expect.
        if is_antigravity_frame(&frame) {
            return self.push_antigravity(&frame);
        }
        match frame.get("type").and_then(serde_json::Value::as_str) {
            Some(kind) if is_anthropic_type(kind) => self.push_anthropic(kind, &frame),
            _ => {
                // A stream that was told it is an Antigravity stream, and then
                // sends a frame belonging to none of the three shapes, is
                // reported rather than scored as an empty OpenAI chunk: an empty
                // chunk looks like a model with nothing to say, and that is the
                // failure this check exists to stop.
                if self.dialect == ApiDialect::Antigravity
                    && frame.get("choices").is_none()
                    && frame.get("candidates").is_none()
                    && frame.get("response").is_none()
                {
                    return Err(classify::protocol(
                        "an Antigravity stream sent a frame with no candidates and no choices"
                            .to_owned(),
                    ));
                }
                self.push_openai(&frame)
            }
        }
    }

    /// The dialect this parser was built for.
    #[must_use]
    pub fn dialect(&self) -> ApiDialect {
        self.dialect
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
        // A tag cut off by the end of the stream must not be lost: whatever is
        // held belongs to the answer or to the reasoning, never to neither. The
        // events are dropped because the sink has already been served; the
        // accumulated strings are what this call is assembling.
        let mut unserved = Vec::new();
        self.flush_held(&mut unserved);

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

    /// Release any content the demuxer is holding, as events.
    fn flush_held(&mut self, events: &mut Vec<ProviderEvent>) {
        let (answer, reasoning) = self.think.flush();
        self.push_reasoning(&reasoning, events);
        self.push_text(&answer, events);
    }

    // ---- OpenAI chat.completion.chunk -------------------------------------

    fn push_openai(&mut self, frame: &serde_json::Value) -> Result<Vec<ProviderEvent>, LlmError> {
        let mut events = Vec::new();
        // Whether this frame carried content for any choice. A frame that did
        // not is a boundary - a usage-only frame, a keep-alive, the last frame
        // before the sentinel - so a held tag prefix cannot be completed by a
        // later one and is safe to release.
        let mut carried_content = false;

        if let Some(choices) = frame.get("choices").and_then(serde_json::Value::as_array) {
            for choice in choices {
                let delta = choice.get("delta");
                if let Some(delta) = delta {
                    if let Some(content) = delta.get("content").and_then(serde_json::Value::as_str)
                    {
                        carried_content = true;
                        let (answer, reasoning) = self.think.feed(content);
                        self.push_reasoning(&reasoning, &mut events);
                        self.push_text(&answer, &mut events);
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

        if !carried_content && self.think.is_holding() {
            self.flush_held(&mut events);
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
        let arguments = function.and_then(|function| function.get("arguments"));

        let accumulator = self.accumulator(index);
        if let Some(id) = id {
            accumulator.observe_id(id);
        }
        if let Some(name) = name {
            accumulator.push_name(name);
        }
        // The documented shape is a string, appended fragment by fragment. A
        // gateway in front of the provider can instead materialise the whole
        // object on a delta, and reading only strings silently dropped that
        // frame: the opener `{"` never reached the buffer, every later fragment
        // was a continuation of nothing, and the turn died with `expected value
        // at line 1 column 1` on a call the model had written correctly. An
        // object is therefore taken as the whole call, the same way the
        // Anthropic block path already treats a materialised `input`.
        match arguments {
            Some(serde_json::Value::String(text)) => accumulator.push_arguments(text),
            Some(value @ (serde_json::Value::Object(_) | serde_json::Value::Array(_))) => {
                accumulator.push_arguments(&value.to_string());
            }
            _ => {}
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

    // ---- Google Antigravity, v1internal ----------------------------------

    /// Read one `v1internal` frame.
    ///
    /// The shape, measured from the bundle that serves it: the whole Gemini
    /// response is nested under `response`, the answer is
    /// `candidates[0].content.parts[]`, a part marked `thought: true` is the
    /// reasoning channel rather than the reply, a part carrying `functionCall`
    /// is a tool call, and the counters are `usageMetadata`. A non-streaming
    /// `generateContent` answer is the same frame with the same nesting, so both
    /// arrive here.
    fn push_antigravity(
        &mut self,
        frame: &serde_json::Value,
    ) -> Result<Vec<ProviderEvent>, LlmError> {
        let mut events = Vec::new();
        let response = frame.get("response").unwrap_or(frame);

        if let Some(metadata) = response.get("usageMetadata") {
            self.observe_antigravity_usage(metadata);
        }
        let Some(candidate) = response
            .get("candidates")
            .and_then(serde_json::Value::as_array)
            .and_then(|candidates| candidates.first())
        else {
            return Ok(events);
        };
        if let Some(parts) = candidate
            .get("content")
            .and_then(|content| content.get("parts"))
            .and_then(serde_json::Value::as_array)
        {
            for part in parts {
                if let Some(call) = part.get("functionCall") {
                    let name = call
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default();
                    if !name.is_empty() {
                        let id = call
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .filter(|id| !id.is_empty())
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("call_{}", self.completed.len()));
                        // The arguments arrive as an object here, not as a JSON
                        // string, so the accumulator is seeded whole and flushed
                        // at once: there is no fragment to wait for.
                        let arguments = call
                            .get("args")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({}));
                        let id = call
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .filter(|id| !id.is_empty())
                            .map_or(id, str::to_owned);
                        let accumulator = self.accumulator(self.tools.len() as u32);
                        accumulator.observe_id(&id);
                        accumulator.push_name(name);
                        accumulator.push_arguments(&arguments.to_string());
                        events.extend(self.flush_index(accumulator_index(&self.tools))?);
                    }
                    continue;
                }
                let Some(text) = part.get("text").and_then(serde_json::Value::as_str) else {
                    continue;
                };
                if text.is_empty() {
                    continue;
                }
                if part.get("thought").and_then(serde_json::Value::as_bool) == Some(true) {
                    let fresh = antigravity_delta(&mut self.antigravity_seen[1], text);
                    self.push_reasoning(&fresh, &mut events);
                } else {
                    let fresh = antigravity_delta(&mut self.antigravity_seen[0], text);
                    self.push_text(&fresh, &mut events);
                }
            }
        }
        if let Some(reason) = candidate
            .get("finishReason")
            .and_then(serde_json::Value::as_str)
        {
            self.finish_reason = Some(reason.to_owned());
            // `STOP` here means "this candidate finished", which is the end of
            // the turn unless a tool call says otherwise. Anything else (or a
            // tool call) is recorded rather than turned into a decision: the
            // caller reads `finish_reason` and the calls together.
            if reason == "STOP" && self.tools.is_empty() {
                self.done = true;
            }
        }
        Ok(events)
    }

    fn observe_antigravity_usage(&mut self, metadata: &serde_json::Value) {
        self.usage.seen = true;
        // Gemini reports its cached count as a part of `promptTokenCount`, so
        // this dialect must have it subtracted to bill the same as a
        // separate-field provider. `Thoughts` ride inside the candidate count
        // and belong to the output total, not to the reply.
        if self.capabilities.cache_read_reporting != CacheReadReporting::NotReported {
            self.usage.reporting = Some(CacheReadReporting::FoldedIntoPrompt);
        }
        let output = metadata
            .get("candidatesTokenCount")
            .and_then(serde_json::Value::as_u64)
            .map(|count| {
                count
                    + metadata
                        .get("thoughtsTokenCount")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0)
            });
        if let Some(output) = output {
            self.usage.output_tokens = Some(output);
        }
        if let Some(prompt) = metadata
            .get("promptTokenCount")
            .and_then(serde_json::Value::as_u64)
        {
            self.usage.prompt_tokens = Some(prompt);
        }
        if let Some(cached) = metadata
            .get("cachedContentTokenCount")
            .and_then(serde_json::Value::as_u64)
        {
            self.usage.cache_read_tokens = Some(cached);
        }
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

/// The index of the accumulator just pushed, for the flush that follows it.
fn accumulator_index(tools: &[ToolAccumulator]) -> u32 {
    tools.last().map(|tool| tool.index).unwrap_or(0)
}

/// Whether a frame is a Cloud Code `v1internal` frame.
fn is_antigravity_frame(frame: &serde_json::Value) -> bool {
    match frame.get("response") {
        Some(response) => response.get("candidates").is_some(),
        None => frame.get("candidates").is_some(),
    }
}

/// The part of `text` that has not been emitted yet, and the running record.
///
/// Three senders are possible and only one of them is documented: incremental
/// fragments, the whole text again on every frame, and the whole text growing.
/// Comparing against what was already emitted handles all three, and a repeated
/// frame produces an empty delta rather than a duplicate paragraph.
fn antigravity_delta(seen: &mut String, text: &str) -> String {
    if text.starts_with(seen.as_str()) {
        let fresh = text[seen.len()..].to_owned();
        seen.push_str(&fresh);
        return fresh;
    }
    if seen.starts_with(text) {
        // The sender repeated a prefix it already sent: nothing new.
        return String::new();
    }
    // The sender rewrote its own text. Emitting the whole thing would duplicate
    // the answer, so only the part beyond the last thing said is taken when the
    // new text extends the old one, and nothing is taken when it does not.
    seen.push_str(text);
    text.to_owned()
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

#[cfg(test)]
mod argument_assembly_tests {
    use super::*;

    fn accumulator(fragments: &[&str]) -> ToolAccumulator {
        let mut acc = ToolAccumulator::new(0);
        acc.observe_id("call-1");
        acc.push_name("clarify");
        for fragment in fragments {
            acc.push_arguments(fragment);
        }
        acc
    }

    /// The shape the three-way heuristic exists for: the documented sender
    /// appends only new bytes.
    #[test]
    fn plain_fragments_append() {
        let acc = accumulator(&[r#"{"title":"#, r#""deck","#, r#""questions":"#, r#"[]}"#]);
        let call = acc.finish().expect("a call").expect("a tool call");
        assert_eq!(call.arguments["title"], "deck");
    }

    /// A sender that re-serialises the whole call between deltas: the second
    /// spelling is not a continuation of the first, and appending the two would
    /// produce two objects glued together.
    #[test]
    fn a_whole_object_resend_replaces_the_buffer() {
        let acc = accumulator(&[
            r#"{"title":"deck","questions":[]}"#,
            r#"{"questions":[],"title":"deck for Cheyne"}"#,
        ]);
        let call = acc.finish().expect("a call").expect("a tool call");
        assert_eq!(
            call.arguments["title"], "deck for Cheyne",
            "the newest whole object is the one the sender meant"
        );
    }

    /// A gateway that appends whole objects leaves `{...}{...}`. Neither parses as
    /// one value, the newest is the intended one, and the turn should not die at
    /// the parser.
    #[test]
    fn two_objects_glued_together_recover_the_newest() {
        let mut acc = ToolAccumulator::new(0);
        acc.observe_id("call-1");
        acc.push_name("clarify");
        // Written straight into the buffer to model a sender this client cannot
        // classify, which is exactly the case recovery is for.
        acc.arguments
            .push_str(r#"{"title":"first"}{"title":"second","questions":[]}"#);
        let call = acc.finish().expect("a call").expect("a tool call");
        assert_eq!(call.arguments["title"], "second");
    }

    /// A brace inside a quoted value is not a boundary: recovery must not cut a
    /// string in half and call the tail a tool call.
    #[test]
    fn a_brace_in_a_string_is_not_a_boundary() {
        let value = r#"{"title":"a } in the title","questions":[]}"#;
        assert!(newest_object(value).is_some());
        let glued = format!(r#"{{"title":"x"}}{value}"#);
        let recovered = newest_object(&glued).expect("the newest object");
        assert_eq!(recovered["title"], "a } in the title");
    }

    /// The case that must keep failing: a genuine truncation has no complete
    /// object to recover, and the message names the tool and the size.
    #[test]
    fn a_real_truncation_is_still_refused() {
        let acc = accumulator(&[r#"{"title":"deck","questions":[{"question":"which form"#]);
        let error = acc.finish().expect_err("a refusal");
        let text = error.to_string();
        assert!(
            text.contains("never completed into valid JSON"),
            "the refusal keeps its message: {text}"
        );
        assert!(text.contains("clarify"), "and names the tool: {text}");
    }

    /// A fragment that is a complete value but not a continuation must not
    /// replace a buffer that is genuinely mid-string.
    #[test]
    fn a_continuation_is_never_mistaken_for_a_whole_object() {
        assert!(!looks_complete(
            r#""deck for Cheyne"\#));
        assert!(!looks_complete("deck"));
        assert!(looks_complete(r#"{"a":1}"#
        ));
    }

    /// A gateway that restarts its serialisation mid-object: one finished
    /// snapshot, then the first half of a new spelling that is not valid JSON on
    /// its own, then the rest of it. Appending the unfinished half onto the
    /// finished object is what produced `expected value at line 1 column 14` on a
    /// 473-byte `clarify` buffer, and the turn died with the call assembled
    /// around a corrupt middle.
    #[test]
    fn a_restarted_serialisation_replaces_a_finished_object() {
        let acc = accumulator(&[
            r#"{"questions":[{"question":"which deck"}]}"#,
            r#"{"title":"deck","quest"#,
            r#"{"title":"deck","questions":[{"question":"which deck"}]}"#,
        ]);
        let call = acc.finish().expect("a call").expect("a tool call");
        assert_eq!(call.arguments["title"], "deck");
        assert_eq!(call.arguments["questions"][0]["question"], "which deck");
    }

    /// The same restart when the stream ends before the new spelling completes:
    /// the buffer is a real truncation of the newest serialisation, and the
    /// finished object before it is stale, so the refusal keeps its message
    /// instead of building a call out of glue.
    #[test]
    fn a_restarted_serialisation_that_ends_early_is_still_refused() {
        let acc = accumulator(&[r#"{"questions":[]}"#, r#"{"title":"deck","quest"#]);
        let error = acc.finish().expect_err("a refusal");
        assert!(
            error
                .to_string()
                .contains("never completed into valid JSON"),
            "{error}"
        );
    }

    /// The bug that failed every `clarify` question on the phone: a token-level
    /// stream sends a nested object's opener as its own fragment, so `{"` arrives
    /// in the middle of `"questions": [` and is byte-identical to the prefix the
    /// buffer already starts with. Reading that as "the sender repeated itself"
    /// dropped it and left `[{"question"...`, which is `expected value at line 1
    /// column 62` on a 535-byte buffer. Every question object opener was eaten,
    /// so every clarify card died before the card was shown.
    #[test]
    fn a_nested_object_opener_is_a_continuation_not_a_repeat() {
        let acc = accumulator(&[
            r#"{"title": "Pick a title", "questions": ["#,
            r#"{"#,
            r#""question": "Which one?", "choices": [{"label": "A"}, {"label": "B"}]"#,
            r#"}]}"#,
        ]);
        let call = acc.finish().expect("a call").expect("a tool call");
        assert_eq!(call.arguments["questions"][0]["question"], "Which one?");
        assert_eq!(call.arguments["questions"][0]["choices"][1]["label"], "B");
    }

    /// The replay this rule exists for still works: the same whole object twice,
    /// byte for byte, contributes the second copy not at all.
    #[test]
    fn an_exact_whole_object_resend_adds_nothing() {
        let one = r#"{"title":"deck","questions":[]}"#;
        let acc = accumulator(&[one, one, one]);
        assert_eq!(acc.arguments, one, "one copy, not three");
        let call = acc.finish().expect("a call").expect("a tool call");
        assert_eq!(call.arguments["title"], "deck");
    }

    /// Whitespace after a finished call is a frame boundary, not a new
    /// serialisation, and must not throw the call away.
    #[test]
    fn trailing_whitespace_does_not_replace_the_buffer() {
        let acc = accumulator(&[r#"{"title":"deck","questions":[]}"#, "\n", "  "]);
        let call = acc.finish().expect("a call").expect("a tool call");
        assert_eq!(call.arguments["title"], "deck");
    }

    /// A token-level stream sends a line of indentation as its own fragment. It
    /// is content inside the value, not a frame boundary: dropping it is what
    /// stripped a written script down to one space of indent and turned
    /// `defaults to 993` into `defaults to993` on the way to disk.
    #[test]
    fn whitespace_inside_a_value_is_content() {
        let acc = accumulator(&[
            r#"{"command":"def decode(value):\n"#,
            "    ",
            r#"return value"}"#,
        ]);
        let call = acc.finish().expect("a call").expect("a tool call");
        assert_eq!(
            call.arguments["command"], "def decode(value):\n    return value",
            "the indentation is part of the command, not a frame to throw away"
        );
    }

    /// A gateway that materialises `arguments` as an object instead of streaming
    /// a string. Reading only strings silently dropped the frame that carried the
    /// opening brace, and every later fragment became a continuation of nothing.
    #[test]
    fn a_materialised_object_arguments_frame_is_accepted() {
        let mut parser = ChatStreamParser::new(Capabilities::openai_compatible());
        let frame = r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","type":"function","function":{"name":"run_command","arguments":{"command":"ls -la"}}}]}}]}"#;
        std::mem::drop(parser.push(frame).expect("the frame parses"));
        std::mem::drop(
            parser
                .push(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#)
                .expect("the finish frame parses"),
        );
        let completion = parser.finish().expect("the stream finishes");
        assert_eq!(completion.tool_calls.len(), 1, "one call, assembled");
        assert_eq!(completion.tool_calls[0].arguments["command"], "ls -la");
    }

    /// A refusal names the one structural fact that tells the two failures
    /// apart: a buffer with no object opener in it never got its first fragment.
    #[test]
    fn a_buffer_with_no_opener_says_so() {
        let acc = accumulator(&[r#"command":"ls -la"}"#]);
        let text = acc.finish().expect_err("a refusal").to_string();
        assert!(
            text.contains("the opening brace never arrived"),
            "the refusal has to name the lost opener: {text}"
        );
    }
}
