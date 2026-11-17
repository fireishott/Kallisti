//! Opaque identifiers.
//!
//! Every id is a newtype over a UUID so the type system keeps a session id from
//! being passed where a device id belongs. [`EventId`] is generated as a v7
//! UUID so ids sort in creation order, which keeps event ordering readable in
//! logs and stable in tests.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Why a string failed to parse as an id.
#[derive(Debug, thiserror::Error)]
#[error("invalid {kind} id: {source}")]
pub struct IdError {
    /// Which id type was being parsed.
    pub kind: &'static str,
    /// The underlying UUID parse failure.
    pub source: uuid::Error,
}

macro_rules! uuid_id {
    ($(#[$meta:meta])* $name:ident, $kind:literal) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Wrap an existing UUID.
            #[must_use]
            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            /// Borrow the inner UUID.
            #[must_use]
            pub const fn as_uuid(&self) -> &Uuid {
                &self.0
            }

            /// Take the inner UUID.
            #[must_use]
            pub const fn into_uuid(self) -> Uuid {
                self.0
            }

            /// Generate a new time-ordered (v7) id.
            #[must_use]
            pub fn now_v7() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = IdError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self).map_err(|source| IdError { kind: $kind, source })
            }
        }

        impl From<Uuid> for $name {
            fn from(value: Uuid) -> Self {
                Self(value)
            }
        }
    };
}

uuid_id!(
    /// Identifies one conversation session. The event log is keyed by it.
    SessionId,
    "session"
);
uuid_id!(
    /// Identifies one envelope in a session log.
    EventId,
    "event"
);
uuid_id!(
    /// Identifies one paired device.
    DeviceId,
    "device"
);
uuid_id!(
    /// Identifies one turn through the turn machine.
    TurnId,
    "turn"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_round_trips_as_string() {
        let id = SessionId::now_v7();
        let parsed: SessionId = id.to_string().parse().expect("v7 id should parse");
        assert_eq!(id, parsed);
    }

    #[test]
    fn bad_id_reports_which_kind_failed() {
        let err = "not-a-uuid"
            .parse::<DeviceId>()
            .expect_err("garbage must not parse");
        assert_eq!(err.kind, "device");
    }

    #[test]
    fn ids_serialize_transparently() {
        let id = EventId::now_v7();
        let json = serde_json::to_string(&id).expect("id should serialize");
        assert_eq!(json, format!("\"{id}\""));
    }
}
