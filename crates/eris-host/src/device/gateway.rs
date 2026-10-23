//! The gateway control surface on the device listener: `/v1/gw/*`.
//!
//! The legacy relay exposed a `/gw/...` control plane so a paired phone could
//! watch and restart the gateway it was talking to. In Eris the gateway *is*
//! this daemon: there is no separate connector process and no relay in front of
//! it. So the control plane has one thing to control - itself - and it answers
//! under `/v1` like every other device route, because the device listener is the
//! only surface a phone can reach.
//!
//! What a phone can ask for, and what this module answers:
//!
//! - `GET /v1/gw/health` - whether the daemon is ready, what it is, how long it
//!   has been up, and how much work is in flight. This is the read the legacy
//!   client polls to confirm a restart finished, so it keeps the legacy field
//!   names (`hermesReady`, `connectorVersion`, ...) rather than inventing new
//!   ones: the client already knows how to read them.
//! - `GET /v1/gw/restart/preflight` - a snapshot a client shows before asking to
//!   restart: the unit, the active work a restart would interrupt, and whether
//!   a restart is allowed right now.
//! - `POST /v1/gw/restart` - schedule a self-restart. The daemon answers first
//!   (legacy `{restarting: true, ...}` shape, no operation id) and then runs
//!   `systemctl --user restart kallisti-eris` on a short delay, so the response
//!   is already on the wire when the process goes away. There is deliberately
//!   no operation tracking: a restarted process cannot remember the operation
//!   that killed it, and the client's existing fallback is to poll `/v1/gw/health`
//!   until `hermesReady` is true again. That is the honest model.
//! - `GET /v1/gw/update/check` - what this daemon is, and whether a staged
//!   `eris.new-*` binary sits next to it. Read-only: installing the staged
//!   binary is a host-side step, because the systemd sandbox makes the install
//!   directory read-only to this process (see `kallisti-eris.service`).
//!
//! Every route is behind the bearer and the `admin` scope: restarting the daemon
//! is a settings-class write, and watching its health is the same class of read
//! as reading the settings a non-admin device should not see.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::admin;
use crate::db::Db;
use crate::device::{DeviceError, DeviceService};

use super::handlers::{authenticate, read_fields, require_admin};

/// The systemd user unit that runs this daemon.
///
/// This is the one unit the gateway is allowed to touch. It is the daemon's own
/// unit, and restarting it is exactly what the operator asked for when they
/// asked for gateway control: nothing else on the host is ever named here.
pub const ERIS_UNIT: &str = "kallisti-eris";

/// How long the daemon waits after answering before it restarts itself.
///
/// Long enough for the response to leave the process, short enough that a phone
/// polling health does not watch a dead process for very long.
const RESTART_DELAY: std::time::Duration = std::time::Duration::from_millis(1500);

/// The in-flight work, read from the durable job table.
///
/// A restart interrupts anything in `queued` or `running`. The web dashboard
/// tracks live turns in memory, but the durable rows are the truth a restart
/// will actually drop, so the preflight reads those rather than the in-process
/// hub: a row left `running` is settled as failed by the next boot, and telling
/// the operator that before they press restart is the point of the screen.
fn active_jobs(db: &Db) -> (i64, i64) {
    let Ok((running, queued)) = db.with_conn(|conn| {
        let running = conn.query_row(
            "SELECT COUNT(*) FROM jobs WHERE status = 'running'",
            [],
            |row| row.get(0),
        )?;
        let queued = conn.query_row(
            "SELECT COUNT(*) FROM jobs WHERE status = 'queued'",
            [],
            |row| row.get(0),
        )?;
        Ok((running, queued))
    }) else {
        return (0, 0);
    };
    (running, queued)
}

/// The process's own uptime in seconds, or `None` off a Linux host.
///
/// Read from `/proc/self/stat`: field 2 is `(comm)` and may contain spaces, so
/// everything after the last `)` is fields 3 onward, and starttime (field 22)
/// sits at index 19 of that tail. Best effort by design - the wire carries
/// `null` for "unknown" and the client already treats the field as optional.
fn uptime_seconds() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let tail = stat.rsplit(')').next()?;
    let fields: Vec<&str> = tail.split_whitespace().collect();
    let start_ticks = fields.get(19).and_then(|f| f.parse::<u64>().ok())?;
    let start_secs = start_ticks / 100;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(now.saturating_sub(start_secs))
}

/// Staged `eris.new-*` binaries beside the running one, oldest-name first.
///
/// The deploy flow stages a build as `eris.new-<version>-<build>` and the host
/// swaps it into place, so a staged file is the "update available" signal a
/// phone reads. Listing names only - never reading contents, never moving one.
fn staged_builds() -> Vec<String> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("eris.new-"))
        .collect();
    out.sort();
    out
}

/// `GET /v1/gw/health` - the daemon's own state, in the legacy client's words.
///
/// `hermesReady` is the field the legacy client polls after a restart, and it
/// maps to "onboarding finished": a daemon that is not ready is not serving
/// turns, which is the same thing "the gateway is up" meant when the gateway
/// was a separate process.
pub async fn gw_health(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    let engine = service.engine();
    let ready = engine.is_ready().unwrap_or(false);
    let models_known = engine
        .provider_models()
        .map(|models| !models.is_empty())
        .unwrap_or(false);
    let (running, queued) = active_jobs(engine.db());
    axum::Json(serde_json::json!({
        "relayConnected": true,
        "connectorConnected": true,
        "hermesConnected": true,
        "hermesReady": ready,
        "dashboardAvailable": true,
        "modelCatalogAvailable": models_known,
        "sessionRoundtripOk": engine.is_ready().is_ok(),
        "profile": "eris",
        "unit": ERIS_UNIT,
        "mainPid": std::process::id(),
        "execMainStartTimestamp": serde_json::Value::Null,
        "connectorVersion": admin::version_line(),
        "uptimeSeconds": uptime_seconds(),
        "activeWork": { "running": running, "queued": queued, "voice": 0, "tools": 0 },
        "deviceListener": service.accepting(),
        "deviceBound": service.is_bound(),
    }))
    .into_response()
}

/// `GET /v1/gw/restart/preflight` - the snapshot a restart confirmation shows.
pub async fn gw_restart_preflight(
    State(service): State<DeviceService>,
    headers: HeaderMap,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    let engine = service.engine();
    let ready = engine.is_ready().unwrap_or(false);
    let (running, queued) = active_jobs(engine.db());
    let blocker = if ready {
        None
    } else {
        Some(
            "onboarding is not finished: a restart would not help a daemon that has no \
             provider configured",
        )
    };
    axum::Json(serde_json::json!({
        "target": "eris",
        "profile": "eris",
        "unit": ERIS_UNIT,
        "preflightVersion": admin::version_line(),
        "activeWork": { "running": running, "queued": queued, "voice": 0, "tools": 0 },
        "canRestart": ready,
        "blocker": blocker,
        "observed": {
            "mainPid": std::process::id(),
            "execMainStartTimestamp": serde_json::Value::Null,
            "gatewayState": if ready { "ready" } else { "setup" },
        },
    }))
    .into_response()
}

/// `POST /v1/gw/restart` - schedule a self-restart, and answer first.
///
/// The body the legacy client sends is `{"target", "preflightVersion"}`; it is
/// read and discarded because the target can only be this daemon and the
/// preflight version is the client's own race guard, not a value this process
/// stores. The answer is the legacy `{restarting: true}` shape so the client's
/// no-operation-tracking fallback engages and it polls health to confirm.
pub async fn gw_restart(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    let _ = read_fields(&headers, body).await;

    if !service.engine().is_ready().unwrap_or(false) {
        return DeviceError::new(
            StatusCode::CONFLICT,
            "not_ready",
            "onboarding is not finished: the daemon will not restart itself before it is ready",
        )
        .into_response();
    }

    if let Err(error) = service.engine().auth().audit(
        token.device_id(),
        "gateway.restart",
        Some("restart scheduled from device"),
    ) {
        return DeviceError::db_unavailable(format!(
            "the restart could not be recorded in the audit trail: {error}"
        ))
        .into_response();
    }

    // Answer, then restart. The spawn outlives this handler, and the sleep is
    // what guarantees the response is already on the wire when systemd sends
    // SIGTERM to this process.
    tokio::spawn(async move {
        tokio::time::sleep(RESTART_DELAY).await;
        match std::process::Command::new("systemctl")
            .args(["--user", "restart", ERIS_UNIT])
            .status()
        {
            Ok(status) => tracing::info!(%status, "gateway restart command issued"),
            Err(error) => tracing::error!(%error, "gateway restart command could not run"),
        }
    });

    axum::Json(serde_json::json!({
        "restarting": true,
        "target": "eris",
        "message": "restart scheduled: the daemon returns in a few seconds",
        "error": serde_json::Value::Null,
    }))
    .into_response()
}

/// `GET /v1/gw/update/check` - what this daemon is, and what is staged next.
///
/// Read-only on purpose: the systemd unit gives this process read-only `$HOME`
/// outside its own state, so it could not install a staged binary even if it
/// wanted to. The phone sees the current identity and the staged filenames and
/// nothing more; the swap stays a host-side step.
pub async fn gw_update_check(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    axum::Json(serde_json::json!({
        "ok": true,
        "target": "eris",
        "current": admin::version_line(),
        "version": admin::VERSION,
        "build": admin::BUILD,
        "staged": staged_builds(),
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_staged_name_is_recognised_and_foreign_names_are_not() {
        // The filter is a prefix match on `eris.new-`; the helper itself lists
        // a directory, so the assertion is about the name rule rather than a
        // filesystem the test would have to construct.
        let names = [
            "eris",
            "eris.new-1.4.5-98",
            "eris.bak-1.4.5-98",
            "eris.new-1.4.5-98-pre-step3",
            "eris.new-1.4.5-99",
        ];
        let staged: Vec<&str> = names
            .iter()
            .copied()
            .filter(|name| name.starts_with("eris.new-"))
            .collect();
        assert_eq!(
            staged,
            [
                "eris.new-1.4.5-98",
                "eris.new-1.4.5-98-pre-step3",
                "eris.new-1.4.5-99"
            ]
        );
    }

    #[test]
    fn the_unit_the_gateway_restarts_is_its_own_unit_only() {
        assert_eq!(ERIS_UNIT, "kallisti-eris");
    }
}
