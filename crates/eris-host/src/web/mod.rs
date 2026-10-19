//! Server-rendered pages and the routes that serve them.
//!
//! `maud` for the markup, one inline stylesheet, no script, no build step, and no
//! CDN: `curl` can read every step and a browser with JavaScript disabled works.

pub mod css;
pub mod routes;
pub mod templates;

/// P11 acceptance for the sign-in route. Test-only: the handlers live with the
/// other handlers in `routes`, and this module is the suite that pins the
/// browser path to the dashboard.
#[cfg(test)]
mod login;
