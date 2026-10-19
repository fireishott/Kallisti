//! The admin listener: loopback, one port, one literal.
//!
//! The address is [`ADMIN_ADDR`], a `const` in this file. It is not a field in
//! `config.toml`, not an environment variable, and not a command-line flag,
//! because "the admin plane is reachable from this host only" has to survive a
//! tired operator and a copied config. `config.rs` refuses a `listen`/`bind` key
//! outright, so there is no second way to set it.
//!
//! P5 serves health and nothing else. Onboarding routes and POST handlers belong
//! to P7, the device listener to P8.

use std::io;

use axum::Router;
use axum::extract::{Extension, State};
use axum::http::Uri;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Serialize;
use tokio::net::TcpListener;

use crate::db::Db;
use crate::error::{ApiError, RequestId, assign_request_id};

/// The admin listener address. Loopback, literal, not configurable.
pub const ADMIN_ADDR: &str = "127.0.0.1:7340";

/// The version every response reports. Mirrors `[workspace.package]`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The one route P5 serves.
pub const ROUTE_HEALTHZ: &str = "/healthz";

/// Every GET route the admin listener serves. The empty-body test walks this
/// list, so a new route cannot be added without being covered by it.
pub const GET_ROUTES: &[&str] = &[ROUTE_HEALTHZ];

/// Shared handler state. The database is the only thing in it.
#[derive(Clone)]
pub struct AppState {
    /// The one SQLite handle.
    pub db: Db,
}

/// `GET /healthz`, exactly. Field order is the wire order.
#[derive(Debug, Serialize)]
struct HealthBody {
    status: &'static str,
    version: &'static str,
    state: &'static str,
    db: &'static str,
}

/// Build the admin router. The request-id middleware is layered over the whole
/// router, so the 404 and 405 paths are covered as well as `/healthz`.
pub fn router(db: Db) -> Router {
    Router::new()
        .route(ROUTE_HEALTHZ, get(healthz))
        .fallback(no_route)
        .method_not_allowed_fallback(wrong_method)
        .layer(middleware::from_fn(assign_request_id))
        .with_state(AppState { db })
}

/// Read the setup state and answer. A database that cannot answer is a 503 with
/// the same typed body as every other error, never a 200 with a hole in it.
async fn healthz(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    let db = state.db.clone();
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
    /// Serving failed.
    #[error("admin listener failed: {0}")]
    Serve(#[source] io::Error),
}

/// Bind [`ADMIN_ADDR`] and serve until a shutdown signal arrives.
///
/// The address is not a parameter on purpose: this function is the only way the
/// daemon listens for HTTP, and its address is a literal.
pub async fn serve(db: Db) -> Result<(), ServeError> {
    let listener = TcpListener::bind(ADMIN_ADDR)
        .await
        .map_err(|source| ServeError::Bind {
            addr: ADMIN_ADDR,
            source,
        })?;
    let addr = listener.local_addr().map_err(ServeError::LocalAddr)?;
    tracing::info!(%addr, "admin listener bound (loopback only)");
    serve_on(listener, db).await
}

/// Serve an already-bound listener. Used by the socket test; `serve` is the only
/// caller in the binary.
pub async fn serve_on(listener: TcpListener, db: Db) -> Result<(), ServeError> {
    let app = router(db);
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(ServeError::Serve)?;
    tracing::info!("admin listener stopped");
    Ok(())
}

/// Wait for Ctrl-C or SIGTERM. Both are a clean stop, which is what the shell
/// job in the P5 acceptance needs.
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
}
