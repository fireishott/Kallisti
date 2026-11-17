//! Kallisti Lite - the on-device core surface.
//!
//! Wave 2 (P3): the UniFFI proc-macro surface. Exactly one function crosses
//! the FFI boundary, [`run_fake_turn`], and it is backed by an in-process fake
//! provider that returns `fixture-ok: <input>`.
//!
//! The dependency graph is the Lite security boundary, not a feature flag
//! (locked amendment 9). This crate depends on `core-domain` and `uniffi`, and
//! on nothing else. It must never gain `eris-host`, an async runtime, an HTTP
//! client, or a device framework: Gate A of the Lite proof is an in-process
//! provider with no listener and no network (locked amendment 2).
//!
//! No `build.rs`, no UDL file, no `generate_scaffolding`: uniffi 0.32.2's
//! `#[uniffi::export]` macro emits the per-function scaffolding itself, and one
//! `setup_scaffolding!()` call emits the crate plumbing. The proc macros are the
//! whole surface.

pub mod fake_provider;

// uniffi 0.32.2 requires exactly one `setup_scaffolding!()` call at crate root.
// It is a proc macro: it emits `UniFfiTag`, the FFI contract-version symbol, and
// the four RustBuffer helpers in-process. No `build.rs`, no UDL, and no
// `generate_scaffolding` call are involved.
uniffi::setup_scaffolding!();

use core_domain::HostCapabilities;

/// The result of one Lite turn.
///
/// A UniFFI `Record`, which is what crosses the boundary as a Swift struct.
/// Deliberately plain data: no handles, no callbacks, no lifetime.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TurnResult {
    /// The assistant text for this turn.
    pub text: String,
}

/// Run one turn against the in-process fake provider.
///
/// This is the only function exported across the FFI boundary (`swift`:
/// `runFakeTurn(input:)`). It opens no socket, resolves no name, reads no
/// environment, and performs no I/O of any kind.
#[must_use]
#[uniffi::export]
pub fn run_fake_turn(input: String) -> TurnResult {
    TurnResult {
        text: fake_provider::respond(&input),
    }
}

/// The capabilities Lite offers: none.
///
/// Lite is not a degraded build. It is the same core with no device host
/// attached (locked amendment 13), so this is a real answer, not a placeholder
/// for a missing call. Not exported over FFI: the boundary carries one
/// function only.
#[must_use]
pub fn lite_capabilities() -> HostCapabilities {
    HostCapabilities::none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lite_offers_no_device_capabilities() {
        assert!(lite_capabilities().is_empty());
    }
}
