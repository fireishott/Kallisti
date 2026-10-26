//! Provider seam vocabulary.
//!
//! Everything a provider adapter has to agree on with the rest of the core, and
//! nothing that performs I/O. No socket, no HTTP client, no async runtime, no
//! file read: `core-domain` is shared unchanged with the Lite on-device core, so
//! the adapter that actually speaks the wire protocol lives in `core-runtime`.
//!
//! The trait here is the seam's *identity*, not its transport. It is
//! object-safe on purpose, so the turn machine can hold a `&dyn Provider` and
//! still not care whether the bytes arrive over rustls, a unix socket, or a
//! recorded fixture.
//!
//! Four decisions are locked and are modelled in the types, not in prose:
//!
//! - a key is referenced, never stored: [`SecretRef`] can only name an
//!   environment variable (locked amendment 6);
//! - usage a provider did not report is [`TokenCount::ProviderUnavailable`],
//!   never zero, and cache reads that were not reported are
//!   [`CacheRead::ProviderUnavailable`], also never zero;
//! - a cache read that a provider folded into its prompt total is subtracted
//!   from [`Usage::input_tokens`], so a folded provider and a separate-field
//!   provider produce the same billable input count;
//! - [`LlmError`] has exactly six variants. A caller matches six shapes, or it
//!   does not match at all.

pub mod error;
pub mod profile;
pub mod secret;
pub mod token;
pub mod tool_call;
pub mod usage;

pub use error::LlmError;
pub use profile::{
    ApiDialect, CacheReadReporting, Capabilities, ModelProfile, Provider, ProviderProfile,
};
pub use secret::{API_KEY_REF_ENV_PREFIX, SecretRef, SecretRefError};
pub use token::{
    TOKEN_REF_PREFIX, TokenError, TokenOrigin, TokenRecord, TokenRef, TokenStore, refresh_lead_secs,
};
pub use tool_call::{ToolCall, ToolCallError};
pub use usage::{CacheRead, TokenCount, UnavailableReason, Usage};
