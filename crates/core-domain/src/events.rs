//! Event shape shared by the log, the turn machine, and the wire contract.
//!
//! The envelope itself (payload, hashes, timestamps) lands in `core-runtime` in
//! Wave 1. This module holds the parts both sides must agree on: the per-session
//! sequence number and the tagged event kind.

use serde::{Deserialize, Serialize};

/// Per-session sequence number. Starts at 1 and is gap-free.
///
/// Zero is not a valid sequence number, so the only way to build one is
/// [`EventSeq::FIRST`] or a checked conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventSeq(u64);

impl EventSeq {
    /// The first envelope in a session.
    pub const FIRST: Self = Self(1);

    /// The raw value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The next sequence number.
    #[must_use]
    pub fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// Why a number is not a sequence number.
#[derive(Debug, thiserror::Error)]
pub enum SeqError {
    /// Sequence numbers start at 1; zero is never valid.
    #[error("event sequence numbers start at 1, got 0")]
    Zero,
}

impl TryFrom<u64> for EventSeq {
    type Error = SeqError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if value == 0 {
            return Err(SeqError::Zero);
        }
        Ok(Self(value))
    }
}

/// What happened. The wire form is internally tagged with `type`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    /// A session started.
    SessionOpened {
        /// Optional customer-visible title.
        title: Option<String>,
    },
    /// A session ended.
    SessionClosed,
    /// A turn was requested by the customer.
    TurnRequested {
        /// The prompt text as typed.
        text: String,
    },
    /// A turn streamed one incremental text delta.
    TurnToken {
        /// The delta as received from the provider.
        delta: String,
    },
    /// A turn produced text.
    TurnCompleted {
        /// The assembled reply text.
        text: String,
    },
    /// A turn failed. The code is stable and machine-readable.
    TurnFailed {
        /// Stable error code, never a formatted message.
        code: String,
    },
    /// Recovery truncated a torn tail and recorded that it did so.
    RecoveryTruncated {
        /// Bytes dropped from the tail of the WAL.
        dropped_bytes: u64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_starts_at_one_and_is_strictly_increasing() {
        let first = EventSeq::FIRST;
        assert_eq!(first.get(), 1);
        assert_eq!(first.next().get(), 2);
        assert!(first < first.next());
    }

    #[test]
    fn zero_is_not_a_sequence_number() {
        assert!(EventSeq::try_from(0_u64).is_err());
        assert_eq!(EventSeq::try_from(7_u64).expect("7 is valid").get(), 7);
    }

    #[test]
    fn event_kind_is_internally_tagged() {
        let json = serde_json::to_value(EventKind::TurnFailed {
            code: "provider_unavailable".to_string(),
        })
        .expect("event should serialize");
        assert_eq!(json["type"], "turn_failed");
        assert_eq!(json["code"], "provider_unavailable");
    }
}
