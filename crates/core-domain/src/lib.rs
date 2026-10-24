//! Kallisti 1.0 domain types.
//!
//! Types only. This crate performs no I/O, opens no socket, and depends on no
//! async runtime. It is shared unchanged by the Full daemon and the Lite
//! on-device core, so it must stay host-neutral: no device framework may be
//! referenced here (locked amendment 13).

pub mod capability;
pub mod device;
pub mod events;
pub mod ids;
pub mod provider;

pub use capability::{Capability, HostCapabilities};
pub use device::{DeviceContext, DeviceSnapshot};
pub use events::{EventKind, EventSeq};
pub use ids::{DeviceId, EventId, IdError, SessionId, TurnId};
pub use provider::{
    API_KEY_REF_ENV_PREFIX, ApiDialect, CacheRead, CacheReadReporting, Capabilities, LlmError,
    ModelProfile, Provider, ProviderProfile, SecretRef, SecretRefError, TokenCount, ToolCall,
    ToolCallError, UnavailableReason, Usage,
};

/// Crate version, mirrored from `[workspace.package]`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
