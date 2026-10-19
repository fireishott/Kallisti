//! Kallisti 1.0 daemon internals.
//!
//! The binary is thin; everything testable lives here so the acceptance tests
//! can drive the same router the process serves.
//!
//! P5 surfaces: config (`eris.env` + `config.toml`), the SQLite state store, the
//! admin listener on loopback, the typed error body, and doctor.
//!
//! P6 adds `auth`: pairing codes, opaque access and refresh tokens, the scope
//! set, and the storage that holds only Argon2id verifiers. It is design and
//! storage only - no route, no handler, no wizard. There is still no onboarding,
//! no device listener and no service unit; those are P7 through P9.

pub mod admin;
pub mod auth;
pub mod config;
pub mod db;
pub mod doctor;
pub mod error;
pub mod probe;
