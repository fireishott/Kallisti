//! Runtime seams for Kallisti 1.0.
//!
//! Wave 0 ships the module skeleton and the decisions that are already locked.
//! The behaviour lands in later waves:
//!
//! | Module | Wave | What it will do |
//! |---|---|---|
//! | [`log`] | 1 (P2) | append-only session log, sealed segments, torn-tail recovery |
//! | [`prompt`] | 2 | prompt block order and compilation |
//! | [`provider`] | 4 (P4) | provider seam, `env:` key resolution, usage accounting |
//! | [`turn`] | 8 (P8) | turn machine, worker lease, cancellation |
//!
//! Nothing here performs I/O yet. `core-runtime` links into the Lite core, so it
//! must stay free of `tokio` and `reqwest` as well as any device framework.

pub mod log;
pub mod prompt;
pub mod provider;
pub mod turn;

pub use core_domain::VERSION;
