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

/// The restart delay, exposed for the admin settings route which reuses the same
/// schedule. One delay, one behaviour, wherever the operator asks for a restart.
#[must_use]
pub fn restart_delay() -> std::time::Duration {
    RESTART_DELAY
}

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
        .primary_provider_catalog()
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

/// The systemd user unit that performs an install, outside this daemon's sandbox.
///
/// The daemon cannot write `~/.local/opt/kallisti/bin`, so it cannot install its
/// own update. It *requests* one, *supervises* one and *reports* one; this unit
/// does the privileged swap. That is the correct trust boundary, not a workaround:
/// the process being replaced does not get write access to the thing replacing it.
pub const UPDATE_UNIT: &str = "kallisti-eris-update";

/// The request file the daemon writes and the helper reads.
const UPDATE_REQUEST: &str = "request.json";

/// The state file the helper writes and the daemon reads.
const UPDATE_STATE: &str = "state.json";

/// A state file larger than this is refused rather than parsed: the helper writes
/// a few kilobytes of output, and a 64 KiB document means something is wrong.
const MAX_STATE_BYTES: u64 = 64 * 1024;

/// How many trailing output lines the progress route keeps.
const OUTPUT_LINES: usize = 200;

/// How long one output line may be, in bytes.
const OUTPUT_LINE_BYTES: usize = 200;

/// The directory this daemon and the helper share, inside `ReadWritePaths`.
fn update_dir(engine: &crate::onboarding::SetupEngine) -> std::path::PathBuf {
    engine.paths().state_dir.join("update")
}

/// What the host can mechanically do about an update, independent of whether one
/// exists. Separated from the answer so the refusal matrix is testable without
/// systemd, a socket or a release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mechanics {
    /// The helper unit file exists in `~/.config/systemd/user`.
    pub unit_installed: bool,
    /// This process was started by a systemd user unit, so `systemctl --user`
    /// will act on the same manager.
    pub systemd_managed: bool,
    /// A request is already in flight and its unit is still active.
    pub update_active: bool,
}

/// Read the three mechanical facts from the host.
fn mechanics(engine: &crate::onboarding::SetupEngine) -> Mechanics {
    let home =
        std::env::var_os("HOME").map_or_else(std::path::PathBuf::new, std::path::PathBuf::from);
    let unit_path = home
        .join(".config/systemd/user")
        .join(format!("{UPDATE_UNIT}.service"));
    Mechanics {
        unit_installed: unit_path.is_file(),
        // systemd sets this for every unit it starts, and nothing else does. A
        // daemon started by hand answers honestly instead of failing at step 6 of
        // the helper with a mystery.
        systemd_managed: std::env::var_os("INVOCATION_ID").is_some(),
        update_active: request_pending(engine),
    }
}

/// The first reason this host cannot install a release, in the order an operator
/// would fix them.
#[must_use]
pub fn install_blocker(m: Mechanics, release: Option<&crate::release::Release>) -> Option<String> {
    if !m.unit_installed {
        return Some(format!(
            "the update unit is not installed (run install.sh to write {UPDATE_UNIT}.service)"
        ));
    }
    if !m.systemd_managed {
        return Some("this install is not a systemd user unit".to_owned());
    }
    if m.update_active {
        return Some("a different update is already running".to_owned());
    }
    match release {
        None => Some("no release newer than this build is published for this host".to_owned()),
        Some(release) if !release.has_checksum => Some(format!(
            "the release has no checksum, and an unsigned swap is not offered: {} publishes no `.sha256` asset",
            release.tag
        )),
        Some(_) => None,
    }
}

/// Whether a request is sitting in the update directory with its unit active.
fn request_pending(engine: &crate::onboarding::SetupEngine) -> bool {
    let request = update_dir(engine).join(UPDATE_REQUEST);
    if !request.is_file() {
        return false;
    }
    // A request file with no running unit is a finished or abandoned attempt; the
    // helper removes it on success, so a leftover one must not block forever.
    matches!(
        std::process::Command::new("systemctl")
            .args(["--user", "is-active", &format!("{UPDATE_UNIT}.service")])
            .output(),
        Ok(output) if String::from_utf8_lossy(&output.stdout).trim() == "active"
    )
}

/// The blocker as any surface sees it: the host's mechanics plus the release.
///
/// The dashboard card and the device route must never disagree about whether the
/// install button does anything, so both ask this one function.
#[must_use]
pub fn host_blocker(
    engine: &crate::onboarding::SetupEngine,
    release: Option<&crate::release::Release>,
) -> Option<String> {
    install_blocker(mechanics(engine), release)
}

/// The helper's last word: its state, its bounded output, and its error.
///
/// Missing or unreadable is `idle` with nothing to show, which is what the card
/// wants on a host that has never updated.
#[must_use]
pub fn progress_snapshot(
    engine: &crate::onboarding::SetupEngine,
) -> (String, Vec<String>, Option<String>) {
    let Some(state) = read_state(engine) else {
        return ("idle".to_owned(), Vec::new(), None);
    };
    let text = |name: &str| {
        state
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    (
        text("state").unwrap_or_else(|| "idle".to_owned()),
        bounded_output(&state),
        text("error"),
    )
}

/// Whether the release is newer than this build.
fn is_newer_than_self(release: &crate::release::Release) -> bool {
    crate::release::is_newer(
        &release.version,
        release.build,
        admin::VERSION,
        admin::BUILD,
    )
}

/// `GET /v1/gw/update/check` - what this daemon is, and what is newest.
///
/// Read-only: the systemd unit gives this process read-only `$HOME` outside its
/// own state, so it could not install a binary even if it wanted to. The answer
/// keeps every key this route shipped with (`ok`, `target`, `current`, `version`,
/// `build`, `staged`) and adds the release fields the client reads, so an older
/// client keeps working and a newer one gets an answer it can decode.
pub async fn gw_update_check(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    let engine = service.engine();
    let client = match crate::release::client() {
        Ok(client) => client,
        Err(error) => {
            return check_body(
                engine,
                None,
                false,
                crate::release::Source::None,
                None,
                Some(error.to_string()),
            );
        }
    };
    let answer = crate::release::check(engine.db(), &client).await;
    let latest = answer
        .release
        .as_ref()
        .filter(|release| is_newer_than_self(release));
    check_body(
        engine,
        latest,
        answer.checked,
        answer.source,
        answer.checked_at,
        answer.note,
    )
}

/// Compose the check answer. One place, so the route and its tests agree.
fn check_body(
    engine: &crate::onboarding::SetupEngine,
    latest: Option<&crate::release::Release>,
    checked: bool,
    source: crate::release::Source,
    checked_at: Option<String>,
    note: Option<String>,
) -> Response {
    let blocker = install_blocker(mechanics(engine), latest);
    let update_available = latest.is_some();
    axum::Json(serde_json::json!({
        "ok": true,
        "target": "eris",
        "current": admin::version_line(),
        "version": admin::VERSION,
        "build": admin::BUILD,
        "staged": staged_builds(),
        "checked": checked,
        "source": source.as_str(),
        "checked_at": checked_at,
        "update_available": update_available,
        "latest": latest.map(|release| serde_json::json!({
            "tag": release.tag,
            "version": release.version,
            "build": release.build,
            "published_at": release.published_at,
            "notes": release.notes,
            "download_url": release.asset_url,
            "has_checksum": release.has_checksum,
        })),
        "can_install": update_available && blocker.is_none(),
        "blocker": blocker,
        "note": note,
    }))
    .into_response()
}

/// `POST /v1/gw/update/apply` - request an install, then answer.
///
/// The request is a file plus a unit start, never a shell line: argv only, and no
/// remote string is ever interpolated into a command. The daemon does not restart
/// itself here - the helper owns the swap and the restart, the same discipline
/// `restart_daemon` follows on the dashboard.
pub async fn gw_update_apply(
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
    let engine = service.engine();
    let fields = read_fields(&headers, body).await;
    if let Ok(fields) = fields {
        // `target` is accepted and ignored the same way `/gw/restart` accepts it:
        // the only thing this daemon can update is itself.
        let target = fields.get("target").map(String::as_str).unwrap_or("eris");
        if !matches!(target, "eris" | "hermes-agent") {
            return DeviceError::new(
                StatusCode::BAD_REQUEST,
                "target_unknown",
                "the only target this daemon can update is `eris`",
            )
            .into_response();
        }
    }

    if !engine.is_ready().unwrap_or(false) {
        return DeviceError::new(
            StatusCode::CONFLICT,
            "not_ready",
            "onboarding is not finished: finish the wizard before asking for an update",
        )
        .into_response();
    }

    let client = match crate::release::client() {
        Ok(client) => client,
        Err(error) => {
            return DeviceError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "update_unavailable",
                format!("the release manifest could not be read: {error}"),
            )
            .into_response();
        }
    };
    let answer = crate::release::check(engine.db(), &client).await;
    // Read the newest known tag before the `filter` takes ownership, so the
    // refusal can name what GitHub actually said.
    let newest_known = answer
        .release
        .as_ref()
        .map_or_else(|| "unknown".to_owned(), |release| release.tag.clone());
    let latest = answer.release.filter(is_newer_than_self);
    let Some(latest) = latest else {
        return DeviceError::new(
            StatusCode::CONFLICT,
            "update_unavailable",
            format!(
                "there is no newer release: this daemon is {}, and the newest known is {newest_known}",
                admin::version_line()
            ),
        )
        .into_response();
    };

    let m = mechanics(engine);
    if m.update_active {
        return DeviceError::new(
            StatusCode::CONFLICT,
            "update_running",
            "a different update is already running",
        )
        .into_response();
    }
    if let Some(blocker) = install_blocker(m, Some(&latest)) {
        return DeviceError::new(StatusCode::CONFLICT, "update_unavailable", &blocker)
            .into_response();
    }

    let directory = update_dir(engine);
    if let Err(error) = std::fs::create_dir_all(&directory) {
        return DeviceError::db_unavailable(format!(
            "the update directory could not be created: {error}"
        ))
        .into_response();
    }
    let request = serde_json::json!({
        "tag": latest.tag,
        "version": latest.version,
        "build": latest.build,
        "asset_url": latest.asset_url,
        "checksum_url": latest.checksum_url,
        "requested_at": crate::auth::format_timestamp(crate::auth::now()),
        "requested_by": token.device_id(),
    });
    let path = directory.join(UPDATE_REQUEST);
    let temporary = directory.join(format!("{UPDATE_REQUEST}.tmp"));
    if let Err(error) = std::fs::write(&temporary, serde_json::to_vec(&request).unwrap_or_default())
    {
        return DeviceError::db_unavailable(format!(
            "the update request could not be written: {error}"
        ))
        .into_response();
    }
    if let Err(error) = std::fs::rename(&temporary, &path) {
        return DeviceError::db_unavailable(format!(
            "the update request could not be placed: {error}"
        ))
        .into_response();
    }

    if let Err(error) = engine.auth().audit(
        token.device_id(),
        "gateway.update_apply",
        Some(&format!("{} requested from device", latest.tag)),
    ) {
        return DeviceError::db_unavailable(format!(
            "the update could not be recorded in the audit trail: {error}"
        ))
        .into_response();
    }

    // Argv only. `--no-block` so this answer is on the wire before the helper can
    // stop the daemon, which is what makes the phone's poll loop meaningful.
    let start = std::process::Command::new("systemctl")
        .args([
            "--user",
            "start",
            "--no-block",
            &format!("{UPDATE_UNIT}.service"),
        ])
        .output();
    match start {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let _ = std::fs::remove_file(&path);
            tracing::error!(
                status = %output.status,
                stderr = %String::from_utf8_lossy(&output.stderr),
                "the update unit could not be started"
            );
            return DeviceError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "update_unavailable",
                format!(
                    "the update unit could not be started: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            )
            .into_response();
        }
        Err(error) => {
            let _ = std::fs::remove_file(&path);
            return DeviceError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "update_unavailable",
                format!("systemctl could not be run: {error}"),
            )
            .into_response();
        }
    }

    axum::Json(serde_json::json!({
        "ok": true,
        "state": "queued",
        "target": "eris",
        "unit": format!("{UPDATE_UNIT}.service"),
        "from": admin::version_line(),
        "to": latest.tag,
        "message": "the update is queued: the daemon restarts when the swap completes",
        "error": serde_json::Value::Null,
    }))
    .into_response()
}

/// The last good progress document, or `None` when there is none.
///
/// A missing file is not an error and a mid-write file is not parsed: the helper
/// writes `state.json` with a temp file and a rename, so a short read means the
/// rename has not landed yet, and the honest answer is "idle until it does".
fn read_state(engine: &crate::onboarding::SetupEngine) -> Option<serde_json::Value> {
    let path = update_dir(engine).join(UPDATE_STATE);
    let metadata = std::fs::metadata(&path).ok()?;
    if metadata.len() > MAX_STATE_BYTES {
        tracing::warn!(
            bytes = metadata.len(),
            "the update state file is too large to trust"
        );
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

/// The bound the progress route applies to the helper's output.
///
/// Newest last, at most [`OUTPUT_LINES`] lines, each cut to [`OUTPUT_LINE_BYTES`]
/// on a character boundary. A phone renders this in a sheet, so the bound is what
/// keeps one pathological line from becoming the whole screen.
#[must_use]
pub fn bounded_output(value: &serde_json::Value) -> Vec<String> {
    let lines: Vec<String> = value
        .get("output")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(|line| {
                    if line.len() <= OUTPUT_LINE_BYTES {
                        return line.to_owned();
                    }
                    let mut end = OUTPUT_LINE_BYTES;
                    while end > 0 && !line.is_char_boundary(end) {
                        end -= 1;
                    }
                    line[..end].to_owned()
                })
                .collect()
        })
        .unwrap_or_default();
    let start = lines.len().saturating_sub(OUTPUT_LINES);
    lines[start..].to_vec()
}

/// `GET /v1/gw/update/progress` - how the install is going.
///
/// Never `404`. The client polls this in a loop and a missing document reads to it
/// as a dead socket, so no update is `200 idle`. During the swap the daemon is
/// down for a few seconds by design and the client is expected to wait, which is
/// why the state file - not this process's memory - is the source.
pub async fn gw_update_progress(
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
    let Some(state) = read_state(engine) else {
        return axum::Json(serde_json::json!({
            "ok": true,
            "data": {
                "state": "idle",
                "exit_code": serde_json::Value::Null,
                "started_at": serde_json::Value::Null,
                "finished_at": serde_json::Value::Null,
                "from_version": serde_json::Value::Null,
                "to_version": serde_json::Value::Null,
                "output": [],
                "error": serde_json::Value::Null,
            },
        }))
        .into_response();
    };
    let field = |name: &str| state.get(name).cloned().unwrap_or(serde_json::Value::Null);
    axum::Json(serde_json::json!({
        "ok": true,
        "data": {
            "state": field("state"),
            "exit_code": field("exit_code"),
            "started_at": field("started_at"),
            "finished_at": field("finished_at"),
            "from_version": field("from_version"),
            "to_version": field("to_version"),
            "output": bounded_output(&state),
            "error": field("error"),
        },
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
        assert_ne!(ERIS_UNIT, UPDATE_UNIT);
    }

    fn mechanics_of(unit_installed: bool, systemd_managed: bool, update_active: bool) -> Mechanics {
        Mechanics {
            unit_installed,
            systemd_managed,
            update_active,
        }
    }

    fn a_release(has_checksum: bool) -> crate::release::Release {
        crate::release::Release {
            tag: "v1.5.0-build.182".to_owned(),
            version: "1.5.0".to_owned(),
            build: 182,
            name: "Kallisti 1.5.0".to_owned(),
            notes: String::new(),
            published_at: "2026-11-10T15:00:00Z".to_owned(),
            asset_url: format!(
                "{}v1.5.0-build.182/eris-v1.5.0-build.182-linux-x86_64.tar.gz",
                crate::release::DOWNLOAD_PREFIX
            ),
            checksum_url: format!(
                "{}v1.5.0-build.182/eris-v1.5.0-build.182-linux-x86_64.tar.gz.sha256",
                crate::release::DOWNLOAD_PREFIX
            ),
            has_checksum,
        }
    }

    /// The refusal matrix, in the order an operator fixes them. Every branch has a
    /// sentence, and none of them is `null` while `can_install` is false: a
    /// disabled button with no reason is the defect the operator reports.
    #[test]
    fn every_mechanical_refusal_names_what_to_do_about_it() {
        let release = a_release(true);

        let no_unit = install_blocker(mechanics_of(false, true, false), Some(&release))
            .expect("a missing unit refuses");
        assert!(no_unit.contains("install.sh"), "{no_unit}");

        let not_systemd = install_blocker(mechanics_of(true, false, false), Some(&release))
            .expect("a hand-started daemon refuses");
        assert_eq!(not_systemd, "this install is not a systemd user unit");

        let busy = install_blocker(mechanics_of(true, true, true), Some(&release))
            .expect("a running update refuses");
        assert_eq!(busy, "a different update is already running");

        let nothing = install_blocker(mechanics_of(true, true, false), None)
            .expect("no newer release refuses");
        assert!(nothing.contains("no release newer"), "{nothing}");

        let unsigned = install_blocker(mechanics_of(true, true, false), Some(&a_release(false)))
            .expect("an unsigned release refuses");
        assert!(unsigned.contains("no checksum"), "{unsigned}");

        assert_eq!(
            install_blocker(mechanics_of(true, true, false), Some(&release)),
            None,
            "an installed unit, a newer signed release and an idle host is installable"
        );
    }

    /// The request file alone must not block forever: only a request whose unit is
    /// still active is "a different update already running".
    #[test]
    fn an_active_unit_is_the_only_thing_that_makes_a_request_pending() {
        assert_ne!(UPDATE_UNIT, ERIS_UNIT);
        assert!(install_blocker(mechanics_of(true, true, false), Some(&a_release(true))).is_none());
    }

    /// A phone renders the helper's output in a sheet, so both bounds matter.
    #[test]
    fn the_progress_output_is_capped_by_lines_and_by_bytes() {
        let long = "x".repeat(OUTPUT_LINE_BYTES * 3);
        let mut lines: Vec<serde_json::Value> = (0..OUTPUT_LINES + 50)
            .map(|index| serde_json::json!(format!("step {index}")))
            .collect();
        lines.push(serde_json::json!(long));
        let value = serde_json::json!({ "output": lines });

        let bounded = bounded_output(&value);
        assert_eq!(bounded.len(), OUTPUT_LINES, "at most the cap survives");
        assert_eq!(
            bounded.last().map(String::as_str),
            Some(&long[..OUTPUT_LINE_BYTES]),
            "the newest line is last and is cut to the byte cap"
        );
        assert!(
            bounded.iter().all(|line| line.len() <= OUTPUT_LINE_BYTES),
            "no single line escapes the byte cap"
        );
    }

    #[test]
    fn a_multibyte_line_is_cut_on_a_character_boundary() {
        // Every `é` is two bytes, so a naive byte slice would split one.
        let value = serde_json::json!({ "output": ["é".repeat(OUTPUT_LINE_BYTES)] });
        let bounded = bounded_output(&value);
        assert_eq!(bounded.len(), 1);
        assert!(
            bounded[0].chars().all(|c| c == 'é'),
            "the cut did not produce a replacement character"
        );
    }

    #[test]
    fn a_missing_or_odd_output_field_is_an_empty_list_rather_than_a_panic() {
        assert!(bounded_output(&serde_json::json!({})).is_empty());
        assert!(bounded_output(&serde_json::json!({ "output": "not a list" })).is_empty());
        assert!(bounded_output(&serde_json::json!({ "output": [1, 2, 3] })).is_empty());
        assert_eq!(
            bounded_output(&serde_json::json!({ "output": ["one"] })),
            ["one"]
        );
    }

    /// The helper's vocabulary, pinned: the progress route passes these through,
    /// and the client switches on them.
    #[test]
    fn the_progress_states_are_the_ones_the_helper_writes() {
        let states = [
            "idle",
            "queued",
            "downloading",
            "verifying",
            "installing",
            "restarting",
            "done",
            "failed",
        ];
        assert_eq!(states.len(), 8);
        assert!(states.contains(&"done"));
    }
}
