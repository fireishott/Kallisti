//! Kallisti 1.0 daemon internals.
//!
//! The binary is thin; everything testable lives here so the acceptance tests
//! can drive the same router the process serves.
//!
//! P5 surfaces: config (`eris.env` + `config.toml`), the SQLite state store, the
//! admin listener on loopback, the typed error body, and doctor.
//!
//! P6 adds `auth`: pairing codes, opaque access and refresh tokens, the scope
//! set, and the storage that holds only Argon2id verifiers.
//!
//! P7 adds `onboarding` (the first-run state machine, the six-word passphrase,
//! the permission matrix, the workspace rule, admin sessions and the claim rate
//! limiter) and `web` (maud templates, the one stylesheet, and the wizard and
//! dashboard routes).
//!
//! P7b adds `backup`: the archive format, the `VACUUM INTO` database snapshot,
//! verification, and the restore ordering that keeps a crash from leaving a
//! database that refers to session files that are not there.
//!
//! P8 adds `device`: the `/v1` listener on 127.0.0.1:7341, pairing, the turn
//! worker and its lease, the SSE stream with resume and a bounded retention
//! window, cancellation, and idempotency. The listener binds only when asked to,
//! and there is still no service unit, because that is P9.

pub mod admin;
pub mod auth;
pub mod backup;
pub mod config;
pub mod db;
pub mod device;
pub mod doctor;
pub mod error;
pub mod onboarding;
pub mod persona;
pub mod probe;
pub mod web;
