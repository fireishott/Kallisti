//! What a device host can do, and the trait the shell implements.
//!
//! The core takes a value ([`DeviceSnapshot`]) and a trait
//! ([`DeviceContext`]). It never imports HealthKit, CoreLocation, CoreMotion,
//! CoreBluetooth, CoreNFC, or EventKit, and it never prompts for permission
//! (locked amendment 13). The iOS shell owns every prompt.

use serde::{Deserialize, Serialize};

use crate::capability::{Capability, HostCapabilities};
use crate::ids::DeviceId;

/// A point-in-time view of one device host.
///
/// Snapshots are values, not handles: the daemon can store and compare one
/// without holding a reference to the shell that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSnapshot {
    /// Which device this describes.
    pub device_id: DeviceId,
    /// Customer-facing name, chosen by the customer, not the OS.
    pub display_name: String,
    /// Channels this host offers and the customer allowed.
    pub capabilities: HostCapabilities,
    /// Unix milliseconds when the shell captured the snapshot.
    pub captured_at_ms: i64,
}

impl DeviceSnapshot {
    /// Build a snapshot with no capabilities granted.
    #[must_use]
    pub fn new(device_id: DeviceId, display_name: impl Into<String>, captured_at_ms: i64) -> Self {
        Self {
            device_id,
            display_name: display_name.into(),
            capabilities: HostCapabilities::none(),
            captured_at_ms,
        }
    }

    /// Whether this host offers a channel.
    #[must_use]
    pub fn offers(&self, capability: Capability) -> bool {
        self.capabilities.contains(capability)
    }
}

/// The seam the iOS (or any) shell implements.
///
/// Implementations live outside `core-domain` and `core-runtime`. Everything
/// here is synchronous and allocation-cheap: a shell returns the last known
/// snapshot rather than blocking the core on a platform call.
pub trait DeviceContext {
    /// The most recent snapshot the shell captured.
    fn snapshot(&self) -> DeviceSnapshot;

    /// The capabilities the shell currently offers.
    fn capabilities(&self) -> HostCapabilities;

    /// Whether this host offers a channel. Defaults to a capability lookup.
    fn offers(&self, capability: Capability) -> bool {
        self.capabilities().contains(capability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct LiteHost {
        device_id: DeviceId,
    }

    impl DeviceContext for LiteHost {
        fn snapshot(&self) -> DeviceSnapshot {
            DeviceSnapshot::new(self.device_id, "lite", 0)
        }

        fn capabilities(&self) -> HostCapabilities {
            HostCapabilities::none()
        }
    }

    #[test]
    fn a_host_with_no_capabilities_still_reports_a_snapshot() {
        let host = LiteHost {
            device_id: DeviceId::now_v7(),
        };
        let snapshot = host.snapshot();
        assert!(snapshot.capabilities.is_empty());
        assert!(!host.offers(Capability::Motion));
        assert_eq!(snapshot.display_name, "lite");
    }
}
