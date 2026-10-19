//! The admin listener: loopback by default, one literal.
//!
//! The default address is [`ADMIN_ADDR`], a `const` in this file. It is not a
//! field in `config.toml` and not an environment variable, because "the admin
//! plane is reachable from this host only" has to survive a tired operator and a
//! copied config. `config.rs` refuses a `listen`/`bind` key outright.
//!
//! The one exception is `serve --admin-listen`, which can add a *second* listener
//! beside the loopback one. It cannot move or replace the loopback bind: the value
//! has to pass [`listen::parse_admin_listen`], which accepts only a loopback
//! address or this host's own tailnet address (`100.64.0.0/10`, and only when that
//! address is assigned locally). A LAN address, a wildcard, a public address and a
//! hostname are refused before anything binds. The point of the tailnet case is
//! that an iPad on the tailnet can open the UI without a proxy in front of it, and
//! everything below still applies to those requests.
//!
//! Three layers guard the whole surface, in this order:
//!
//! 1. **request id** (outermost) - every response, including the 403 and the 409,
//!    carries `x-request-id`;
//! 2. **forwarded-header rejection** - a request carrying `X-Forwarded-*`,
//!    `Forwarded` or any `Tailscale-*` header is a 403 `not_loopback`, no matter
//!    what it asked for, and on every listener including the tailnet one. Belt and
//!    braces against a future Serve mapping: nothing may sit in front of the admin
//!    plane and pretend to be the client;
//! 3. **the ready gate** on the dashboard - `409 not_ready` until onboarding
//!    finishes.
//!
//! `/healthz`, `/`, `/setup`, `/setup/*` and `/static/*` are the only paths that
//! answer before `ready`, and only `/setup` renders without an admin session.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Extension, Request, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Serialize;
use tokio::net::TcpListener;

use crate::config::Paths;
use crate::db::Db;
use crate::device::DeviceService;
use crate::device::turns::TurnStore;
use crate::error::{ApiError, RequestId, assign_request_id};
use crate::onboarding::{AdminSessions, ClaimLimiter, SetupEngine};
use crate::web::css::CSS_ROUTE;
use crate::web::routes::{dashboard_routes, setup_routes};

pub mod listen;

/// The default admin listener address. Loopback and literal: `serve` always binds
/// it, and `--admin-listen` can only add a second listener, never move this one.
pub const ADMIN_ADDR: &str = "127.0.0.1:7340";

/// The version every response reports. Mirrors `[workspace.package]`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `GET /healthz`.
pub const ROUTE_HEALTHZ: &str = "/healthz";

/// `GET /` - a 302, by state.
pub const ROUTE_ROOT: &str = "/";

/// `GET /setup` - the wizard.
pub const ROUTE_SETUP: &str = "/setup";

/// `GET /setup/state` - wizard progress as JSON.
pub const ROUTE_SETUP_STATE: &str = "/setup/state";

/// `GET /dashboard`.
pub const ROUTE_DASHBOARD: &str = "/dashboard";

/// `GET /dashboard/settings` - the three settings forms.
pub const ROUTE_SETTINGS: &str = "/dashboard/settings";

/// `POST /dashboard/settings/provider` - endpoint, key reference, model.
pub const ROUTE_SETTINGS_PROVIDER: &str = "/dashboard/settings/provider";

/// `POST /dashboard/settings/workspace` - the sandbox workspace path.
pub const ROUTE_SETTINGS_WORKSPACE: &str = "/dashboard/settings/workspace";

/// `POST /dashboard/settings/permissions` - the permission tuple.
pub const ROUTE_SETTINGS_PERMISSIONS: &str = "/dashboard/settings/permissions";

/// `GET /dashboard/activity` - the audit trail, newest first.
pub const ROUTE_ACTIVITY: &str = "/dashboard/activity";

/// `GET /login` and `POST /login` - the way a browser gets a session once the
/// wizard is closed. Sits behind the ready gate with the dashboard, because
/// before `ready` the wizard is the surface that owns the credential.
pub const ROUTE_LOGIN: &str = "/login";

/// Every GET route the listener serves, including the ones that need a session
/// or a finished setup. The phase 7 test walks this list.
pub const ALL_GET_ROUTES: &[&str] = &[
    ROUTE_ROOT,
    ROUTE_HEALTHZ,
    ROUTE_SETUP,
    ROUTE_SETUP_STATE,
    ROUTE_LOGIN,
    ROUTE_DASHBOARD,
    ROUTE_SETTINGS,
    ROUTE_ACTIVITY,
    CSS_ROUTE,
];

/// The GET routes that answer 200 with no session and no finished setup. The P5
/// empty-body test walks this list.
///
/// `ROUTE_LOGIN` is deliberately absent: it is behind the ready gate and answers
/// `409 not_ready` until onboarding finishes, so it cannot be in the
/// "200 while unconfigured" set. It is covered by [`ALL_GET_ROUTES`] and by the
/// `web::login` suite instead.
pub const GET_ROUTES: &[&str] = &[ROUTE_HEALTHZ];

/// Every POST route the listener serves.
pub const POST_ROUTES: &[&str] = &[
    "/setup/claim",
    "/setup/provider",
    "/setup/models",
    "/setup/permissions",
    "/setup/workspace",
    "/setup/device",
    "/setup/finish",
    ROUTE_LOGIN,
    ROUTE_SETTINGS_PROVIDER,
    ROUTE_SETTINGS_WORKSPACE,
    ROUTE_SETTINGS_PERMISSIONS,
    "/dashboard/provider/rotate",
    "/dashboard/device/revoke",
    "/dashboard/mode",
];

/// Shared handler state.
#[derive(Clone)]
pub struct AppState {
    /// The onboarding engine (and the database underneath it).
    pub engine: SetupEngine,
    /// The admin cookie sessions.
    pub sessions: AdminSessions,
    /// Claim attempt limiter, in process on purpose: it guards a loopback form,
    /// not a distributed API.
    pub limiter: Arc<ClaimLimiter>,
    /// True only when the process was started with `--allow-full`.
    pub allow_full: bool,
    /// The device listener's service. It binds nothing until `start` is called:
    /// at boot when the flag or the setting asks for it, and from the dashboard
    /// toggle afterwards.
    pub device: DeviceService,
}

impl AppState {
    /// Build state around an engine. `allow_full` comes from the command line.
    #[must_use]
    pub fn new(engine: SetupEngine, allow_full: bool) -> Self {
        let db = engine.db().clone();
        let device = DeviceService::new(engine.clone(), TurnStore::buffer_from_env());
        Self {
            engine,
            sessions: AdminSessions::new(db),
            limiter: Arc::new(ClaimLimiter::default()),
            allow_full,
            device,
        }
    }

    /// The database.
    #[must_use]
    pub fn db(&self) -> &Db {
        self.engine.db()
    }
}

/// `GET /healthz`, exactly. Field order is the wire order.
#[derive(Debug, Serialize)]
struct HealthBody {
    status: &'static str,
    version: &'static str,
    state: &'static str,
    db: &'static str,
}

/// Resolve the paths for a convenience router: `$KALLISTI_STATE_DIR` wins, then
/// `$HOME`.
#[must_use]
pub fn default_paths() -> Paths {
    if let Ok(dir) = std::env::var("KALLISTI_STATE_DIR")
        && !dir.trim().is_empty()
    {
        return Paths::for_root(PathBuf::from(dir));
    }
    Paths::from_home().unwrap_or_else(|_| Paths::for_root("/tmp/kallisti-fallback"))
}

/// Build the admin router.
///
/// This is the real constructor: it wires the request-id middleware, the
/// forwarded-header rejection, the ready gate, and the merged setup and dashboard
/// routers.
pub fn router_with(state: AppState) -> Router {
    let dashboard =
        dashboard_routes().layer(middleware::from_fn_with_state(state.clone(), require_ready));
    Router::new()
        .route(ROUTE_HEALTHZ, get(healthz))
        .route(ROUTE_ROOT, get(root))
        .route(ROUTE_SETUP_STATE, get(crate::web::routes::setup_state))
        .route(CSS_ROUTE, get(stylesheet))
        .merge(setup_routes())
        .merge(dashboard)
        .fallback(no_route)
        .method_not_allowed_fallback(wrong_method)
        .layer(middleware::from_fn(reject_forwarded))
        .layer(middleware::from_fn(assign_request_id))
        .with_state(state)
}

/// Build a router over a database alone.
///
/// Test-only convenience: the paths come from the environment, and no wizard
/// write happens on `/healthz`, on the 404 path, or on the 405 path. Serving the
/// real UI goes through [`router_with`], so the paths are the ones the daemon
/// resolved.
pub fn router(db: Db) -> Router {
    router_with(AppState::new(SetupEngine::new(db, default_paths()), false))
}

/// The ready gate: `409 not_ready` for every dashboard path until onboarding
/// finishes.
async fn require_ready(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.0.clone())
        .unwrap_or_default();
    match state.engine.is_ready() {
        Ok(true) => next.run(request).await,
        Ok(false) => ApiError::new(
            StatusCode::CONFLICT,
            "not_ready",
            "onboarding is not finished: the dashboard answers 409 until setup completes",
            request_id,
        )
        .into_response(),
        Err(error) => {
            ApiError::internal(request_id, format!("cannot read the setup state: {error}"))
                .into_response()
        }
    }
}

/// Refuse anything that arrived through a proxy.
///
/// A request carrying a forwarding header did not come from a browser on this
/// host, whatever its peer address says. Tailscale Serve adds `Tailscale-*`
/// identity headers; a conventional reverse proxy adds `X-Forwarded-*`. Both are
/// a 403 with `code: "not_loopback"`.
async fn reject_forwarded(request: Request, next: Next) -> Response {
    if let Some(name) = forwarded_header(request.headers()) {
        let request_id = request
            .extensions()
            .get::<RequestId>()
            .map(|id| id.0.clone())
            .unwrap_or_default();
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "not_loopback",
            format!(
                "the admin listener answers only a direct loopback client; this request carried the {name} header"
            ),
            request_id,
        )
        .into_response();
    }
    next.run(request).await
}

/// The first forwarding header present, if any.
#[must_use]
pub fn forwarded_header(headers: &HeaderMap) -> Option<&'static str> {
    const NAMED: [&str; 4] = [
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "forwarded",
    ];
    for name in NAMED {
        if headers.contains_key(name) {
            return Some(name);
        }
    }
    if headers
        .keys()
        .any(|key| key.as_str().starts_with("tailscale-"))
    {
        return Some("tailscale-");
    }
    None
}

/// Read the setup state and answer. A database that cannot answer is a 503 with
/// the same typed body as every other error, never a 200 with a hole in it.
async fn healthz(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    let db = state.db().clone();
    // One row read. It happens off the async executor so a busy writer cannot
    // stall a runtime worker.
    match tokio::task::spawn_blocking(move || db.setup_state()).await {
        Ok(Ok(setup_state)) => axum::Json(HealthBody {
            status: "ok",
            version: VERSION,
            state: setup_state.as_str(),
            db: "ok",
        })
        .into_response(),
        Ok(Err(error)) => {
            ApiError::db_unavailable(request_id.0, format!("cannot read setup state: {error}"))
                .into_response()
        }
        Err(error) => {
            ApiError::internal(request_id.0, format!("health check task failed: {error}"))
                .into_response()
        }
    }
}

/// `GET /` - a 302 to the wizard or the dashboard, by state.
async fn root(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    let location = match state.engine.is_ready() {
        Ok(true) => ROUTE_DASHBOARD,
        Ok(false) => ROUTE_SETUP,
        Err(error) => {
            return ApiError::internal(
                request_id.0,
                format!("cannot read the setup state: {error}"),
            )
            .into_response();
        }
    };
    (
        StatusCode::FOUND,
        [(axum::http::header::LOCATION, location)],
    )
        .into_response()
}

/// `GET /static/app.css` - the one stylesheet, also inlined on every page.
async fn stylesheet() -> Response {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")],
        crate::web::css::APP_CSS,
    )
        .into_response()
}

/// Unknown path: a typed 404, not axum's empty body.
async fn no_route(uri: Uri, Extension(request_id): Extension<RequestId>) -> Response {
    ApiError::not_found(
        request_id.0,
        format!("{} is not a route on the admin listener", uri.path()),
    )
    .into_response()
}

/// Known path, wrong method: a typed 405.
async fn wrong_method(uri: Uri, Extension(request_id): Extension<RequestId>) -> Response {
    ApiError::method_not_allowed(
        request_id.0,
        format!("{} does not accept that method", uri.path()),
    )
    .into_response()
}

/// Why `serve` could not run.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The admin port could not be bound.
    #[error("cannot bind the admin listener on {addr}: {source}")]
    Bind {
        /// The fixed address.
        addr: &'static str,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// The listener address could not be read back.
    #[error("cannot read the bound admin address: {0}")]
    LocalAddr(#[source] io::Error),
    /// The opt-in second listener could not be bound. Startup fails: a daemon
    /// that was asked for two listeners and got one is not what the operator
    /// asked for.
    #[error("cannot bind the second admin listener on {addr}: {source}")]
    SecondBind {
        /// The validated address from `--admin-listen`.
        addr: SocketAddr,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// Serving failed.
    #[error("admin listener failed: {0}")]
    Serve(#[source] io::Error),
}

/// Bind [`ADMIN_ADDR`] and serve until a shutdown signal arrives.
///
/// The address is not a parameter on purpose: this function is the only way the
/// daemon listens for HTTP, and its address is a literal.
pub async fn serve(state: AppState) -> Result<(), ServeError> {
    serve_with(state, None).await
}

/// Bind [`ADMIN_ADDR`] first, then an optional second listener on `second`, and
/// serve the same router on both.
///
/// The loopback bind always happens and always happens first. `second` is the
/// validated `--admin-listen` value, or `None` for the default loopback-only
/// daemon. Both listeners must bind before either starts serving, so a failure to
/// bind the second one is a startup failure rather than a daemon that quietly
/// lost the listener the operator asked for.
pub async fn serve_with(state: AppState, second: Option<SocketAddr>) -> Result<(), ServeError> {
    let listener = TcpListener::bind(ADMIN_ADDR)
        .await
        .map_err(|source| ServeError::Bind {
            addr: ADMIN_ADDR,
            source,
        })?;
    let loopback = listener.local_addr().map_err(ServeError::LocalAddr)?;

    let Some(second) = second else {
        tracing::info!(addr = %loopback, "admin listener bound (loopback only)");
        return serve_on(listener, state).await;
    };

    // Asking for the default by name is not a second listener: binding it twice
    // would fail with the address already in use.
    if second == loopback {
        tracing::info!(
            addr = %loopback,
            note = "--admin-listen names the default address, so there is one listener",
            "admin listener bound (loopback only)"
        );
        return serve_on(listener, state).await;
    }

    let tailnet = TcpListener::bind(second)
        .await
        .map_err(|source| ServeError::SecondBind {
            addr: second,
            source,
        })?;
    let tailnet_addr = tailnet.local_addr().map_err(ServeError::LocalAddr)?;
    tracing::info!(
        loopback = %loopback,
        tailnet = %tailnet_addr,
        "admin listeners bound (loopback and opt-in tailnet)"
    );

    // Same `AppState`, same router: `serve_on` calls `router_with` for each.
    tokio::select! {
        result = serve_on(listener, state.clone()) => result,
        result = serve_on(tailnet, state) => result,
    }
}

/// Serve an already-bound listener. Used by the socket test; `serve` is the only
/// caller in the binary.
pub async fn serve_on(listener: TcpListener, state: AppState) -> Result<(), ServeError> {
    let app = router_with(state);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(ServeError::Serve)?;
    tracing::info!("admin listener stopped");
    Ok(())
}

/// Wait for Ctrl-C or SIGTERM. Both are a clean stop, which is what the shell
/// job in the acceptance needs.
async fn shutdown_signal() {
    let ctrl_c = async {
        if tokio::signal::ctrl_c().await.is_err() {
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_admin_address_is_loopback_only() {
        assert_eq!(ADMIN_ADDR, "127.0.0.1:7340");
        let address: std::net::SocketAddr = ADMIN_ADDR.parse().expect("a literal socket address");
        assert!(address.ip().is_loopback(), "{address} must be loopback");
    }

    #[test]
    fn the_version_is_the_workspace_version() {
        assert_eq!(VERSION, "1.0.0");
    }

    #[test]
    fn every_forwarding_header_is_recognised() {
        for name in [
            "x-forwarded-for",
            "x-forwarded-host",
            "x-forwarded-proto",
            "forwarded",
            "tailscale-user",
            "tailscale-user-login",
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).expect("header name"),
                "1.2.3.4".parse().expect("header value"),
            );
            assert!(
                forwarded_header(&headers).is_some(),
                "{name} must be refused"
            );
        }
        let mut clean = HeaderMap::new();
        clean.insert(
            axum::http::header::HOST,
            "127.0.0.1:7340".parse().expect("host"),
        );
        assert!(
            forwarded_header(&clean).is_none(),
            "a plain loopback request is not refused"
        );
    }
}
