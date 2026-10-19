//! Kallisti Lite - the on-device core surface.
//!
//! Wave 0 stub. The UniFFI proc-macro surface with `run_fake_turn` lands in
//! Wave 2 (P3); nothing here is exported across the FFI boundary yet.
//!
//! The dependency graph is the Lite security boundary, not a feature flag
//! (locked amendment 9). This crate depends on `core-domain` and nothing else.
//! It must never gain `eris-host`, `tokio`, `reqwest`, or a device framework:
//! Gate A of the Lite proof is an in-process fake provider with no listener and
//! no network (locked amendment 2).

use core_domain::HostCapabilities;

/// The capabilities Lite offers: none.
///
/// Lite is not a degraded build. It is the same core with no device host
/// attached (locked amendment 13), so this is a real answer, not a placeholder
/// for a missing call.
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
