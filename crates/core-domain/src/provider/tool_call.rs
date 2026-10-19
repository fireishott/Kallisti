//! A completed tool call.
//!
//! Fragments are never represented here. A provider streams a tool call as a
//! run of partial argument chunks, and a caller that treated any chunk as a call
//! would execute a function with truncated JSON. So this type is constructible
//! only from a parsed, complete argument object: the adapter buffers fragments
//! per array index and builds one of these when the call is finished, never
//! before.

use serde::{Deserialize, Serialize};

/// One tool call the model asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    /// Position in the assistant message's `tool_calls` array. Preserved so an
    /// assembled message can be replayed in the order the model produced it.
    pub index: u32,
    /// Provider-assigned call id, echoed back in the tool result.
    pub id: String,
    /// Function name.
    pub name: String,
    /// Arguments, parsed. Always a JSON object, never a fragment.
    pub arguments: serde_json::Value,
}

impl ToolCall {
    /// Build a call from a complete argument object.
    ///
    /// A non-object argument is rejected: every tool schema in 1.0 takes an
    /// object, and a bare string or array is a provider bug worth surfacing as
    /// a protocol failure rather than passing on to a dispatcher.
    pub fn new(
        index: u32,
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Result<Self, ToolCallError> {
        if !arguments.is_object() {
            return Err(ToolCallError::ArgumentsNotAnObject);
        }
        Ok(Self {
            index,
            id: id.into(),
            name: name.into(),
            arguments,
        })
    }
}

/// Why a buffered tool call could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ToolCallError {
    /// The parsed arguments were valid JSON but not an object.
    #[error("tool-call arguments must be a JSON object")]
    ArgumentsNotAnObject,
}
