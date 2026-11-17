//! The envelope: one line of a session log, and the hash chain that links them.
//!
//! # Canonical hash
//!
//! `hash` is the lowercase hex SHA-256 of a byte string built from the fields
//! *above* it, in the order they are declared, with the field names included so
//! that no field can be moved without changing the digest:
//!
//! ```text
//! preimage = fp("kallisti.envelope.v1")
//!          | fp("schema_version") | fp(schema_version.to_be_bytes())
//!          | fp("event_id")       | fp(event_id string)
//!          | fp("session_id")     | fp(session_id string)
//!          | fp("seq")            | fp(seq.to_be_bytes())
//!          | fp("occurred_at")    | fp(occurred_at.to_be_bytes())
//!          | fp("kind")           | fp(canonical kind bytes)
//!          | fp("payload")        | fp(canonical payload JSON)
//!          | fp("prev_hash")      | fp(prev_hash bytes)
//!
//! fp(x) = u64 big-endian length of x, then x
//! ```
//!
//! Three properties fall out of that definition, and each one is load-bearing:
//!
//! - it is a *defined* order, not an accident of `HashMap` iteration, so the
//!   same envelope hashes to the same bytes on a replay on any machine;
//! - the payload is canonicalised through a sorted map ([`std::collections::BTreeMap`])
//!   before hashing, so a map's iteration order can never leak into the digest;
//! - `occurred_at` is an envelope field, inside the hash, so a timestamp can
//!   never live only in a hashed unordered map.
//!
//! `deny_unknown_fields` on both the struct and its serialised form means a
//! future writer cannot smuggle an extra field past an older reader: it fails
//! to parse instead of silently ignoring it.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use core_domain::{EventId, EventKind, SessionId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::log::LogError;

/// Envelope schema version this build writes and understands.
pub const SCHEMA_VERSION: u16 = 1;

/// Domain separator, so an envelope hash can never be confused with any other
/// SHA-256 this system computes.
const HASH_DOMAIN: &[u8] = b"kallisti.envelope.v1";

/// One durable event.
///
/// Field order is the canonical hash order and the JSON key order. Do not
/// reorder these fields: that is a wire and on-disk format change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    /// Schema version of this envelope, always [`SCHEMA_VERSION`] today.
    pub schema_version: u16,
    /// Unique, time-ordered id of this event.
    pub event_id: EventId,
    /// Session this event belongs to.
    pub session_id: SessionId,
    /// Per-session sequence number: starts at 1, gap-free.
    pub seq: u64,
    /// Unix milliseconds, inside the envelope and therefore inside the hash.
    pub occurred_at: i64,
    /// What happened.
    pub kind: EventKind,
    /// Event-specific body. Canonicalised before hashing.
    pub payload: Value,
    /// Hex SHA-256 of the previous envelope; `""` for `seq == 1`.
    pub prev_hash: String,
    /// Hex SHA-256 over the canonical field order described in the module docs.
    pub hash: String,
}

impl Envelope {
    /// Build one envelope and seal its hash.
    ///
    /// The caller supplies the chain coordinates (`seq` and `prev_hash`); they
    /// are trusted only after [`crate::log::recover`] re-validates every one of
    /// them on replay.
    #[must_use]
    pub fn new(
        session_id: SessionId,
        event_id: EventId,
        seq: u64,
        occurred_at: i64,
        kind: EventKind,
        payload: Value,
        prev_hash: String,
    ) -> Self {
        let mut envelope = Self {
            schema_version: SCHEMA_VERSION,
            event_id,
            session_id,
            seq,
            occurred_at,
            kind,
            payload,
            prev_hash,
            hash: String::new(),
        };
        envelope.hash = envelope
            .compute_hash()
            .unwrap_or_else(|_| String::from("unhashed"));
        envelope
    }

    /// The exact bytes the hash is computed over.
    ///
    /// # Errors
    ///
    /// Only if the payload or kind cannot be re-encoded, which cannot happen for
    /// values this crate produced or parsed.
    pub fn canonical_preimage(&self) -> Result<Vec<u8>, LogError> {
        let kind_bytes =
            serde_json::to_vec(&self.kind).map_err(|source| LogError::Serialize { source })?;
        let payload_bytes = canonical_json(&self.payload)?.into_bytes();

        let mut out = Vec::new();
        push_len_prefixed(&mut out, HASH_DOMAIN);
        push_field(
            &mut out,
            "schema_version",
            &self.schema_version.to_be_bytes(),
        );
        push_field(&mut out, "event_id", self.event_id.to_string().as_bytes());
        push_field(
            &mut out,
            "session_id",
            self.session_id.to_string().as_bytes(),
        );
        push_field(&mut out, "seq", &self.seq.to_be_bytes());
        push_field(&mut out, "occurred_at", &self.occurred_at.to_be_bytes());
        push_field(&mut out, "kind", &kind_bytes);
        push_field(&mut out, "payload", &payload_bytes);
        push_field(&mut out, "prev_hash", self.prev_hash.as_bytes());
        Ok(out)
    }

    /// Recomputed hash over the canonical field order.
    ///
    /// # Errors
    ///
    /// As [`Envelope::canonical_preimage`].
    pub fn compute_hash(&self) -> Result<String, LogError> {
        Ok(hex::encode(Sha256::digest(self.canonical_preimage()?)))
    }

    /// Whether the stored hash matches the one recomputed from the fields.
    ///
    /// Replay calls this on every envelope, which is what makes "hash the same
    /// bytes on replay" a checked property and not a claim.
    #[must_use]
    pub fn has_valid_hash(&self) -> bool {
        self.compute_hash().is_ok_and(|hash| hash == self.hash)
    }

    /// Serialise to the single line that goes into a WAL, without the newline.
    ///
    /// # Errors
    ///
    /// If the envelope cannot be encoded.
    pub fn to_line(&self) -> Result<String, LogError> {
        serde_json::to_string(self).map_err(|source| LogError::Serialize { source })
    }

    /// Parse one line, without its trailing newline.
    ///
    /// # Errors
    ///
    /// If the bytes are not a single JSON object matching this schema. Unknown
    /// fields are rejected by `deny_unknown_fields`.
    pub fn from_line(line: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(line)
    }
}

/// Current wall clock as Unix milliseconds, for [`Envelope::occurred_at`].
#[must_use]
pub fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

fn push_len_prefixed(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(bytes);
}

fn push_field(out: &mut Vec<u8>, name: &str, value: &[u8]) {
    push_len_prefixed(out, name.as_bytes());
    push_len_prefixed(out, value);
}

/// Deterministic JSON for hashing: object keys sorted, no insignificant space.
///
/// `serde_json`'s own map is a `BTreeMap` by default, but this rebuilds through
/// an explicitly sorted map so the digest cannot change if the `preserve_order`
/// feature is ever turned on somewhere else in the dependency graph.
fn canonical_json(value: &Value) -> Result<String, LogError> {
    serde_json::to_string(&canonical_value(value)).map_err(|source| LogError::Serialize { source })
}

fn canonical_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            // BTreeMap: keys are hashed in sorted order, never in insertion or
            // hash order.
            let sorted: BTreeMap<&String, Value> = map
                .iter()
                .map(|(key, value)| (key, canonical_value(value)))
                .collect();
            Value::Object(
                sorted
                    .into_iter()
                    .map(|(key, value)| (key.clone(), value))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_value).collect()),
        scalar => scalar.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope(seq: u64, payload: Value) -> Envelope {
        Envelope::new(
            SessionId::now_v7(),
            EventId::now_v7(),
            seq,
            now_millis(),
            EventKind::TurnCompleted {
                text: String::from("hi"),
            },
            payload,
            String::new(),
        )
    }

    #[test]
    fn hash_is_stable_for_the_same_fields() {
        let one = envelope(1, json!({"a": 1}));
        let two = one.clone();
        assert_eq!(one.hash, two.hash);
        assert!(one.has_valid_hash());
    }

    #[test]
    fn payload_key_order_does_not_change_the_hash() {
        let mut first = envelope(1, json!({}));
        first.payload = json!({"beta": 2, "alpha": 1, "nested": {"z": 1, "a": 2}});
        let mut second = first.clone();
        // Same fields, different source order: the digest must not move.
        second.payload = json!({"alpha": 1, "nested": {"a": 2, "z": 1}, "beta": 2});
        assert_eq!(first.compute_hash().ok(), second.compute_hash().ok());
        assert_eq!(first.hash, second.hash);
    }

    #[test]
    fn tampering_with_a_field_breaks_the_hash() {
        let mut tampered = envelope(1, json!({"a": 1}));
        tampered.seq = 99;
        assert!(!tampered.has_valid_hash());
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let line = br#"{"schema_version":1,"event_id":"018f0b5e-0000-7000-8000-000000000000","session_id":"018f0b5e-0000-7000-8000-000000000001","seq":1,"occurred_at":0,"kind":{"type":"session_closed"},"payload":null,"prev_hash":"","hash":"","extra":true}"#;
        assert!(Envelope::from_line(line).is_err());
    }
}
