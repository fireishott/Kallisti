//! Server-rendered pages and the routes that serve them.
//!
//! `maud` for the markup, one inline stylesheet, no build step, and no CDN: a
//! browser with JavaScript disabled can read and use every page. The chat
//! surface adds one small inline script on that one page, as progressive
//! enhancement: with it the answer streams in, without it the form still works
//! and the reply arrives on the next page.

pub mod backups;
pub mod chat;
pub mod css;
pub mod dsml;
pub mod proxy;
pub mod routes;
pub mod store;
pub mod templates;
pub mod wallpaper;

/// P11 acceptance for the sign-in route. Test-only: the handlers live with the
/// other handlers in `routes`, and this module is the suite that pins the
/// browser path to the dashboard.
#[cfg(test)]
mod login;

/// Shared request plumbing for the suites below. Test-only.
#[cfg(test)]
mod harness;

/// Phase 1: the application shell, its nav, and the house style.
#[cfg(test)]
mod shell;

/// Phase 1: the Settings surface, the only page that writes behaviour.
#[cfg(test)]
mod settings;

/// Phase 1: the Activity page over `audit_log`.
#[cfg(test)]
mod activity;

/// Phase 2: the Sessions list, the session detail page, and job cancellation.
#[cfg(test)]
mod sessions;

/// Phase 2: the Devices list, its revoke button, and the pairing-code form.
#[cfg(test)]
mod devices;

/// The Assistant identity group: the persona file, its size, and the two
/// refusals that keep an enabled-but-blank persona from existing.
#[cfg(test)]
mod persona;

/// The conversation surface and the wallpaper: session gating, the transcript,
/// the streaming POST's refusals, and the preset and upload paths.
#[cfg(test)]
mod chat_surface;
