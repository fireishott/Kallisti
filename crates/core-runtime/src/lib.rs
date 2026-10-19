//! Runtime seams for Kallisti 1.0.
//!
//! Wave 0 shipped the module skeleton and the decisions that are already locked.
//! The behaviour lands wave by wave:
//!
//! | Module | Wave | What it does |
//! |---|---|---|
//! | [`log`] | 1 (P2) | append-only session log, sealed segments, torn-tail recovery |
//! | [`prompt`] | 2 | prompt block order and compilation |
//! | [`provider`] | 4 (P4) | provider seam, `env:` key resolution, usage accounting |
//! | [`turn`] | 8 (P8) | turn machine, worker lease, cancellation |
//!
//! Two modules perform I/O. [`log`] writes through blocking `std::fs`, because the
//! WAL has to be fsynced before an ack and hiding that behind an async runtime
//! would make the fsync look optional. [`provider::openai_compat`] owns the HTTP
//! client, which is the one place a network stack belongs.
//!
//! The Lite boundary is `crates/ios-host`'s dependency graph, not this crate's:
//! Lite is `ios-host` + `core-domain`, so it never links `core-runtime` and
//! therefore never links `reqwest` or `tokio` (locked amendment 9, enforced by
//! `crates/ios-host/tests/no_network.rs` and the CI grep over its sources).

pub mod log;
pub mod prompt;
pub mod provider;
pub mod turn;

pub use core_domain::VERSION;
