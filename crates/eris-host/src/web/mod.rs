//! Server-rendered pages and the routes that serve them.
//!
//! `maud` for the markup, one inline stylesheet, no script, no build step, and no
//! CDN: `curl` can read every step and a browser with JavaScript disabled works.

pub mod css;
pub mod routes;
pub mod templates;
