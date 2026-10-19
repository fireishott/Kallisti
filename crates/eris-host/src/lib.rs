//! Kallisti 1.0 daemon internals.
//!
//! The binary is thin; everything testable lives here so the acceptance tests
//! can drive the same router the process serves.
//!
//! P5 surfaces: config (`eris.env` + `config.toml`), the SQLite state store, the
//! admin listener on loopback, the typed error body, and doctor. There is no
//! onboarding, no auth, no device listener, and no service unit yet - those are
//! P6 through P9.

pub mod admin;
pub mod config;
pub mod db;
pub mod doctor;
pub mod error;
pub mod probe;
