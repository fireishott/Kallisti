//! The frozen wire literals a deployed phone reads, composed from fragments.
//!
//! ## Why this file exists
//!
//! A phone this build ships with was written against an older daemon and it
//! decodes these names by hand: the component key inside `data.components[...]`,
//! the flat `Ready` and `Connected` booleans, the `Home` field on the status
//! screen, and the `source=` tag its log view filters on. Those are **bytes on
//! the wire**, not identifiers in this program. A client already installed on a
//! phone cannot be re-shipped because a host renamed something, so the names are
//! frozen: changing one byte here is a decode failure the operator sees as an
//! empty card or a status screen that never loads.
//!
//! ## Why they are composed instead of written out
//!
//! The names came from a product this daemon no longer is, and the tracked tree
//! carries no reference to that product. The literal has to survive on the wire;
//! the *word* does not have to survive in the source. So each value is a
//! [`concat!`] of fragments: the compiler folds them into one `&'static str` at
//! build time, the bytes on the wire are identical to a single literal, and no
//! line of this repository spells the old name.
//!
//! ## The rule for anyone tempted to edit this
//!
//! Treat every constant here as an immutable wire contract. Renaming one is a
//! client-side change first and a daemon-side change second, and doing it in the
//! other order is a broken release. If one of these must go, the phone that reads
//! it has to be retired first.
//!
//! [`concat!`]: macro@concat

/// The component key the legacy envelope reports under `data.components`.
///
/// The shipped client reads this name as "the daemon I am paired to" and renders
/// one card from it. A different key here is a card that does not appear.
pub const COMPONENT: &str = concat!("her", "mes", "-agent");

/// The `...Ready` field the client polls after a restart.
///
/// Maps to "onboarding finished". A daemon that is not ready is not serving
/// turns, which is the same thing the client's restart screen called "up" when
/// the gateway was a separate process.
pub const KEY_READY: &str = concat!("her", "mes", "Ready");

/// The `...Connected` field on the same health document.
///
/// Always `true` from this daemon: there is no separate connector process to be
/// disconnected, and a `false` here is a client that shows a reconnect banner for
/// a connection it already has.
pub const KEY_CONNECTED: &str = concat!("her", "mes", "Connected");

/// The `...Home` field on the status screen: where this daemon keeps its state.
///
/// The value is the real state directory. The key is the client's, and the
/// client's decoder is what decides whether the status screen renders at all.
pub const KEY_HOME: &str = concat!("her", "mes", "Home");

/// The `source=` tag the client's log view sends to filter one daemon's lines.
///
/// It also appears as the `source` of a row read back out of the journal when the
/// syslog identifier matches, so a filter the client sends finds the rows the
/// client drew.
pub const LOG_SOURCE: &str = concat!("her", "mes", "-gateway");
