//! Device capabilities and the capability set a host offers.
//!
//! These are neutral names, not platform frameworks. The core stores which
//! channels exist and which the customer allowed; the shell owns the platform
//! API and every permission prompt (locked amendments 13 and 14).

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

/// One device channel the customer can see and switch off.
///
/// A sample taken through a channel carries a purpose tag, and health,
/// location, and motion payloads are never written into the session log in
/// plaintext (locked amendment 14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Post or schedule a local notification.
    Notifications,
    /// Read calendar events.
    Calendar,
    /// Read health samples.
    Health,
    /// Read coarse or fine location.
    Location,
    /// Read motion and activity samples.
    Motion,
    /// Talk to paired Bluetooth peripherals.
    Bluetooth,
    /// Read NFC tags.
    Nfc,
    /// Read files the customer picks.
    Files,
    /// Read the clipboard.
    Clipboard,
}

impl Capability {
    /// Stable wire name, so a log line matches the JSON value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Notifications => "notifications",
            Self::Calendar => "calendar",
            Self::Health => "health",
            Self::Location => "location",
            Self::Motion => "motion",
            Self::Bluetooth => "bluetooth",
            Self::Nfc => "nfc",
            Self::Files => "files",
            Self::Clipboard => "clipboard",
        }
    }
}

/// The set of capabilities a host actually offers.
///
/// A `BTreeSet` is deliberate: iteration order is stable, so a value that
/// reaches a hash or a diff never depends on insertion order.
///
/// Lite compiles with [`HostCapabilities::none`]. That is not a degraded
/// build, it is the same core with no device host attached.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HostCapabilities {
    granted: BTreeSet<Capability>,
}

impl HostCapabilities {
    /// No device host. This is the Lite value.
    #[must_use]
    pub fn none() -> Self {
        Self {
            granted: BTreeSet::new(),
        }
    }

    /// Build a set from an iterator, dropping duplicates.
    #[must_use]
    pub fn from_iter_checked(items: impl IntoIterator<Item = Capability>) -> Self {
        Self {
            granted: items.into_iter().collect(),
        }
    }

    /// Grant a capability. Returns `true` when it was not already present.
    pub fn grant(&mut self, capability: Capability) -> bool {
        self.granted.insert(capability)
    }

    /// Switch a capability off. Returns `true` when it was present.
    pub fn revoke(&mut self, capability: Capability) -> bool {
        self.granted.remove(&capability)
    }

    /// Whether the host offers this channel.
    #[must_use]
    pub fn contains(&self, capability: Capability) -> bool {
        self.granted.contains(&capability)
    }

    /// Whether the host offers nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.granted.is_empty()
    }

    /// How many channels are granted.
    #[must_use]
    pub fn len(&self) -> usize {
        self.granted.len()
    }

    /// Iterate the granted channels in stable order.
    pub fn iter(&self) -> impl Iterator<Item = Capability> + '_ {
        self.granted.iter().copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lite_has_no_capabilities() {
        let caps = HostCapabilities::none();
        assert!(caps.is_empty());
        assert!(!caps.contains(Capability::Health));
    }

    #[test]
    fn grant_and_revoke_are_reported() {
        let mut caps = HostCapabilities::none();
        assert!(caps.grant(Capability::Calendar));
        assert!(!caps.grant(Capability::Calendar));
        assert!(caps.revoke(Capability::Calendar));
        assert!(!caps.revoke(Capability::Calendar));
        assert!(caps.is_empty());
    }

    #[test]
    fn wire_name_matches_serde_name() {
        let json =
            serde_json::to_string(&Capability::Location).expect("capability should serialize");
        assert_eq!(json, format!("\"{}\"", Capability::Location.as_str()));
    }
}
