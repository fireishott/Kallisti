//! The device listener: `127.0.0.1:7341`, `/v1` only, off by default.
//!
//! This is the surface a paired phone talks to. It is a second listener, not a
//! second admin plane: nothing from the wizard, the dashboard, or `/healthz`
//! exists here, and the fallback answers a typed 404 for every path outside
//! `/v1`. The port is a literal for the same reason the admin port is - "reachable
//! from this host only" must survive a copied config.
//!
//! Three things have to be true before a turn is accepted, in this order:
//!
//! 1. a bearer access token that verifies against a stored Argon2id verifier and
//!    carries the scope the route needs;
//! 2. onboarding finished (`409 not_ready`) - a device surface on a daemon that
//!    has no provider configured is a trap, not a feature;
//! 3. the operator has not switched the listener off (`409
//!    device_listener_disabled`).
//!
//! Binding is opt-in twice over: `eris serve --device-listen`, or the dashboard
//! toggle writing `permissions.device_listener_enabled`. Enabling from the
//! dashboard takes effect immediately, because the toggle owns the same
//! [`DeviceService`] the daemon built at start-up. Disabling stops new turns and
//! leaves the socket bound until the next restart, which is the documented
//! behaviour rather than a bug: rebinding a listener to signal "off" would drop
//! the SSE streams of turns that are already running.

pub mod handlers;
pub mod turns;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::http::{StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Serialize;
use tokio::net::TcpListener;

use crate::error::assign_request_id;
use crate::onboarding::SetupEngine;
use crate::persona::PersonaConfig;

use turns::TurnHub;

/// The device listener address. Loopback, literal, not configurable.
pub const DEVICE_ADDR: &str = "127.0.0.1:7341";

/// `POST /v1/pair` - redeem a pairing code for an access and refresh pair.
pub const ROUTE_PAIR: &str = "/v1/pair";

/// `POST /v1/token/refresh` - trade a refresh token for an access token.
pub const ROUTE_REFRESH: &str = "/v1/token/refresh";

/// `POST /v1/turns` - queue one turn.
pub const ROUTE_TURNS: &str = "/v1/turns";

/// `GET /v1/turns/{id}/events` - the SSE stream for one job.
pub const ROUTE_EVENTS: &str = "/v1/turns/{id}/events";

/// `POST /v1/turns/{id}/cancel` - stop a running turn.
pub const ROUTE_CANCEL: &str = "/v1/turns/{id}/cancel";

/// `GET /v1/sessions/{id}` - the snapshot a buffer miss points at.
pub const ROUTE_SESSION: &str = "/v1/sessions/{id}";

/// The bodies every device-surface error uses.
///
/// `ok` is always false, `code` is the thing a client branches on, and `message`
/// is for the human reading a `curl` transcript. The admin plane's error body is
/// a different shape on purpose: the phone is a machine client, and the phone
/// never renders an admin error.
#[derive(Debug, Serialize)]
pub struct DeviceErrorBody {
    /// Always `false`.
    pub ok: bool,
    /// Stable machine-readable code.
    pub code: String,
    /// Human-readable detail. Never carries a secret.
    pub message: String,
    /// Where to recover, on a `buffer_miss`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_url: Option<String>,
}

/// A refusal on the device surface.
#[derive(Debug)]
pub struct DeviceError {
    status: StatusCode,
    code: &'static str,
    message: String,
    snapshot_url: Option<String>,
}

impl DeviceError {
    /// Build one.
    #[must_use]
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            snapshot_url: None,
        }
    }

    /// 401 with the contract's code.
    #[must_use]
    pub fn unauthorized(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, code, message)
    }

    /// 400 for a body the route cannot read.
    #[must_use]
    pub fn bad_request(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, code, message)
    }

    /// 404 for a job the caller does not own, or one that never existed.
    #[must_use]
    pub fn job_not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "job_not_found",
            "no job with that id belongs to this device",
        )
    }

    /// 404 for a session the caller does not own.
    #[must_use]
    pub fn session_not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "session_not_found",
            "no session with that id belongs to this device",
        )
    }

    /// 409 for a resume point the retention window has already passed.
    #[must_use]
    pub fn buffer_miss(session_id: &str) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            code: "buffer_miss",
            message: "the events after that id are no longer retained; read the session snapshot"
                .to_owned(),
            snapshot_url: if session_id.is_empty() {
                None
            } else {
                Some(format!("/v1/sessions/{session_id}"))
            },
        }
    }

    /// 503 when the state store cannot answer.
    #[must_use]
    pub fn db_unavailable(message: impl Into<String>) -> Self {
        Self::new(StatusCode::SERVICE_UNAVAILABLE, "db_unavailable", message)
    }

    /// The status this error answers with.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The code this error answers with.
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl IntoResponse for DeviceError {
    fn into_response(self) -> Response {
        let body = DeviceErrorBody {
            ok: false,
            code: self.code.to_owned(),
            message: self.message,
            snapshot_url: self.snapshot_url,
        };
        (self.status, axum::Json(body)).into_response()
    }
}

/// The device surface: the listener, the auth it trusts, and the turn hub.
#[derive(Clone)]
pub struct DeviceService {
    engine: SetupEngine,
    hub: Arc<TurnHub>,
    accepting: Arc<AtomicBool>,
    bound: Arc<AtomicBool>,
}

impl DeviceService {
    /// Build the service. It binds nothing; `start` does that.
    ///
    /// The identity comes from the paths this engine resolved, so a scratch root
    /// reads its own persona file and a test can never touch the operator's.
    #[must_use]
    pub fn new(engine: SetupEngine, buffer: usize) -> Self {
        let persona = PersonaConfig::at_path(engine.paths().persona_file());
        let hub = Arc::new(TurnHub::new(engine.db().clone(), buffer).with_persona(persona));
        Self {
            engine,
            hub,
            accepting: Arc::new(AtomicBool::new(true)),
            bound: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Build the service with an injected environment lookup, for tests. The
    /// process environment cannot be mutated from a test in this workspace: the
    /// crate forbids `unsafe`, and edition 2024 makes `set_var` unsafe.
    #[must_use]
    pub fn with_key_lookup(engine: SetupEngine, buffer: usize, lookup: turns::KeyLookup) -> Self {
        let persona = PersonaConfig::at_path(engine.paths().persona_file());
        let hub = Arc::new(
            TurnHub::with_key_lookup(engine.db().clone(), buffer, lookup).with_persona(persona),
        );
        Self {
            engine,
            hub,
            accepting: Arc::new(AtomicBool::new(true)),
            bound: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Replace the identity the turn worker uses. Called by `serve` once the
    /// `[persona]` table has been resolved against the config file.
    pub fn set_persona(&self, persona: PersonaConfig) {
        self.hub.set_persona(persona);
    }

    /// The turn hub.
    #[must_use]
    pub fn hub(&self) -> &Arc<TurnHub> {
        &self.hub
    }

    /// The onboarding engine, which owns the settings the gates read.
    #[must_use]
    pub fn engine(&self) -> &SetupEngine {
        &self.engine
    }

    /// Whether new turns are accepted. This is the in-process half of the
    /// dashboard kill switch; the socket half is the bind.
    #[must_use]
    pub fn accepting(&self) -> bool {
        self.accepting.load(Ordering::SeqCst)
    }

    /// Stop or resume accepting new turns. Documented as "stops new turns, socket
    /// stays bound until restart".
    pub fn set_accepting(&self, accepting: bool) {
        self.accepting.store(accepting, Ordering::SeqCst);
    }

    /// Whether this process has already bound the device port.
    #[must_use]
    pub fn is_bound(&self) -> bool {
        self.bound.load(Ordering::SeqCst)
    }

    /// Bind `127.0.0.1:7341` in a background task. Idempotent: a second call is a
    /// no-op, so a dashboard toggle cannot double-bind.
    ///
    /// The bind is attempted off the caller's path, because the dashboard POST
    /// has already answered a form and must not fail half-way through if the port
    /// is somehow taken: a bind failure is logged and the switch is left off.
    pub fn start(&self) -> bool {
        if self.bound.swap(true, Ordering::SeqCst) {
            return true;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            self.bound.store(false, Ordering::SeqCst);
            return false;
        };
        let service = self.clone();
        handle.spawn(async move {
            match serve_address(DEVICE_ADDR, service.clone()).await {
                Ok(()) => {
                    service.bound.store(false, Ordering::SeqCst);
                }
                Err(error) => {
                    tracing::error!(%error, "device listener could not start");
                    service.bound.store(false, Ordering::SeqCst);
                }
            }
        });
        true
    }

    /// The router this service serves.
    pub fn router(&self) -> Router {
        router(self.clone())
    }
}

/// Build the device router. `/v1` only; everything else is a typed 404.
pub fn router(service: DeviceService) -> Router {
    Router::new()
        .route(ROUTE_PAIR, post(handlers::pair))
        .route(ROUTE_REFRESH, post(handlers::refresh))
        .route(ROUTE_TURNS, post(handlers::create_turn))
        .route(ROUTE_EVENTS, get(handlers::events))
        .route(ROUTE_CANCEL, post(handlers::cancel))
        .route(ROUTE_SESSION, get(handlers::session))
        .fallback(not_here)
        .method_not_allowed_fallback(wrong_method)
        .layer(axum::middleware::from_fn(assign_request_id))
        .with_state(service)
}

/// Serve an already-bound listener. Tests use this; `start` is the binary's path.
pub async fn serve_on(listener: TcpListener, service: DeviceService) -> std::io::Result<()> {
    axum::serve(listener, router(service))
        .await
        .map_err(std::io::Error::other)
}

/// Bind and serve one address. The only place the device port is bound.
pub async fn serve_address(addr: &str, service: DeviceService) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    tracing::info!(%local, "device listener bound (loopback only, /v1 only)");
    serve_on(listener, service).await
}

/// Unknown path. The device listener serves `/v1` and nothing else: the wizard,
/// the dashboard and `/healthz` are admin-plane paths and never answer here.
async fn not_here(uri: Uri) -> DeviceError {
    DeviceError::new(
        StatusCode::NOT_FOUND,
        "not_found",
        format!(
            "{} is not a route on the device listener: this surface serves /v1 only",
            uri.path()
        ),
    )
}

/// Known path, wrong method.
async fn wrong_method(uri: Uri) -> DeviceError {
    DeviceError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        format!("{} does not accept that method", uri.path()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_device_address_is_the_literal_loopback_port() {
        assert_eq!(DEVICE_ADDR, "127.0.0.1:7341");
        let address: std::net::SocketAddr = DEVICE_ADDR.parse().expect("a literal socket address");
        assert!(address.ip().is_loopback(), "{address} must be loopback");
        assert_eq!(address.port(), 7341);
    }

    #[test]
    fn the_only_routes_are_the_v1_ones() {
        for route in [
            ROUTE_PAIR,
            ROUTE_REFRESH,
            ROUTE_TURNS,
            ROUTE_EVENTS,
            ROUTE_CANCEL,
            ROUTE_SESSION,
        ] {
            assert!(route.starts_with("/v1/"), "{route} must live under /v1");
        }
    }

    #[test]
    fn the_error_body_is_the_documented_shape() {
        let error = DeviceError::unauthorized("pairing_code_invalid", "nope");
        let body = serde_json::to_string(&DeviceErrorBody {
            ok: false,
            code: error.code().to_owned(),
            message: "nope".to_owned(),
            snapshot_url: None,
        })
        .expect("serialisable");
        assert_eq!(
            body,
            r#"{"ok":false,"code":"pairing_code_invalid","message":"nope"}"#
        );
        assert_eq!(error.status(), StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn a_buffer_miss_carries_the_snapshot_url() {
        let error = DeviceError::buffer_miss("session-1");
        assert_eq!(error.status(), StatusCode::CONFLICT);
        assert_eq!(error.code(), "buffer_miss");
        assert_eq!(
            error.snapshot_url.as_deref(),
            Some("/v1/sessions/session-1")
        );
    }
}
