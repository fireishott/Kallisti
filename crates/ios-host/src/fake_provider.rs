//! The Lite fake provider.
//!
//! Gate A of the Lite proof is that a turn can complete with no listener and
//! no network (locked amendment 2). This provider therefore has no address, no
//! socket, no URL, and no runtime: it formats a string.
//!
//! It exists to prove the plumbing - FFI surface, record shape, test harness -
//! and nothing else. The real provider seam is P4, in `core-runtime`, and it is
//! not reachable from this crate on purpose.

/// The fixture prefix every fake turn returns.
pub const FIXTURE_PREFIX: &str = "fixture-ok: ";

/// Return the fixed fixture response for `input`.
#[must_use]
pub fn respond(input: &str) -> String {
    let mut out = String::with_capacity(FIXTURE_PREFIX.len() + input.len());
    out.push_str(FIXTURE_PREFIX);
    out.push_str(input);
    out
}
