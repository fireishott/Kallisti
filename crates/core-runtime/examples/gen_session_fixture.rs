//! Regenerate `contract/fixtures/session-fixture.jsonl`.
//!
//! Run with `cargo run -p core-runtime --example gen_session_fixture`. The output
//! is deterministic - fixed session id, fixed event ids, fixed timestamps - so
//! regenerating it on a clean tree produces no diff. `log::recover::tests::
//! fixture_replay_is_byte_identical` proves the fixture still parses, still
//! verifies its own hashes, and still replays byte for byte.
//!
//! The generator is a developer tool, not runtime code: it is the only thing in
//! this crate that writes outside the state directory.

use std::path::PathBuf;

use core_domain::{EventId, EventKind, SessionId};
use core_runtime::log::Envelope;
use serde_json::json;
use uuid::Uuid;

/// Base wall clock for the fixture, so timestamps are stable but look real.
const BASE_MILLIS: i64 = 1_767_225_600_000;

/// Envelopes in the fixture. The plan fixes this at 20.
const ENVELOPE_COUNT: u64 = 20;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let session_id = fixture_session_id();
    let mut envelopes = Vec::with_capacity(ENVELOPE_COUNT as usize);
    let mut prev_hash = String::new();

    for seq in 1..=ENVELOPE_COUNT {
        let (kind, payload) = event(seq);
        let envelope = Envelope::new(
            session_id,
            fixture_event_id(seq),
            seq,
            BASE_MILLIS + (seq as i64 * 250),
            kind,
            payload,
            prev_hash.clone(),
        );
        prev_hash.clone_from(&envelope.hash);
        envelopes.push(envelope);
    }

    let mut out = String::new();
    for envelope in &envelopes {
        out.push_str(&envelope.to_line()?);
        out.push('\n');
    }

    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("contract")
        .join("fixtures")
        .join("session-fixture.jsonl");
    std::fs::write(&path, out)?;
    println!("wrote {} envelopes to {}", envelopes.len(), path.display());
    Ok(())
}

/// A fixed, syntactically valid v7 UUID used as the fixture's session id.
fn fixture_session_id() -> SessionId {
    SessionId::from_uuid(v7_shaped(0x5e55_10a0, 0))
}

/// Deterministic v7-shaped event ids: `seq` in the tail, base millis in the front.
fn fixture_event_id(seq: u64) -> EventId {
    EventId::from_uuid(v7_shaped(BASE_MILLIS as u64, seq))
}

fn v7_shaped(millis: u64, seq: u64) -> Uuid {
    let mut bytes = [0_u8; 16];
    bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
    bytes[6] = 0x70; // version 7
    bytes[8] = 0x80; // RFC 4122 variant
    bytes[9..].copy_from_slice(&seq.to_be_bytes()[1..]);
    Uuid::from_bytes(bytes)
}

/// A short, plausible session: two turns, a token stream, an error, a truncation.
fn event(seq: u64) -> (EventKind, serde_json::Value) {
    match seq {
        1 => (
            EventKind::SessionOpened {
                title: Some(String::from("fixture session")),
            },
            json!({ "client": "contract-fixture" }),
        ),
        2 => (
            EventKind::TurnRequested {
                text: String::from("what changed in the deployment plan?"),
            },
            json!({ "turn": "t-1" }),
        ),
        3 | 4 => (
            EventKind::TurnToken {
                delta: String::from("the plan gained "),
            },
            json!({ "turn": "t-1", "index": seq - 3 }),
        ),
        5 => (
            EventKind::TurnCompleted {
                text: String::from("the plan gained a seal step"),
            },
            json!({ "turn": "t-1", "usage": { "input": 21, "output": 42 } }),
        ),
        6 => (
            EventKind::TurnRequested {
                text: String::from("show me the guard output"),
            },
            json!({ "turn": "t-2" }),
        ),
        7 => (
            EventKind::TurnCompleted {
                text: String::from("GUARD PASS: 29 checks unchanged"),
            },
            json!({ "turn": "t-2", "usage": { "input": 12, "output": 8 } }),
        ),
        8 => (
            EventKind::TurnFailed {
                code: String::from("provider_unavailable"),
            },
            json!({ "turn": "t-3" }),
        ),
        9 => (
            EventKind::TurnRequested {
                text: String::from("retry the last turn"),
            },
            json!({ "turn": "t-4" }),
        ),
        10 => (
            EventKind::TurnToken {
                delta: String::from("retried"),
            },
            json!({ "turn": "t-4", "index": 0 }),
        ),
        11 => (
            EventKind::TurnCompleted {
                text: String::from("retried and answered"),
            },
            json!({ "turn": "t-4", "usage": { "input": 9, "output": 4 } }),
        ),
        12 => (EventKind::SessionClosed, json!({ "reason": "idle" })),
        13 => (
            EventKind::SessionOpened {
                title: Some(String::from("second visit")),
            },
            json!({ "client": "contract-fixture" }),
        ),
        14 => (
            EventKind::TurnRequested {
                text: String::from("and the event log?"),
            },
            json!({ "turn": "t-5" }),
        ),
        15 => (
            EventKind::TurnCompleted {
                text: String::from("plain jsonl, fsynced before ack"),
            },
            json!({ "turn": "t-5", "usage": { "input": 15, "output": 11 } }),
        ),
        16 => (
            EventKind::RecoveryTruncated { dropped_bytes: 137 },
            json!({ "recovered_from": "wal.jsonl" }),
        ),
        17 => (
            EventKind::TurnRequested {
                text: String::from("continue after recovery"),
            },
            json!({ "turn": "t-6" }),
        ),
        18 => (
            EventKind::TurnToken {
                delta: String::from("continuing "),
            },
            json!({ "turn": "t-6", "index": 0 }),
        ),
        19 => (
            EventKind::TurnCompleted {
                text: String::from("continuing without a gap"),
            },
            json!({ "turn": "t-6", "usage": { "input": 7, "output": 3 } }),
        ),
        _ => (
            EventKind::SessionClosed,
            json!({ "reason": "fixture complete" }),
        ),
    }
}
