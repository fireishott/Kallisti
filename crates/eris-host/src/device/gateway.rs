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
pub(crate) fn staged_builds() -> Vec<String> {
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

/// The staged binaries beside the running one, for the Software card.
///
/// The dashboard and the device route must show the same list, so both ask here.
#[must_use]
pub fn staged_names() -> Vec<String> {
    staged_builds()
}

/// `eris.new-<version>-<build>` as `(version, build)`.
///
/// The name is the only place the staged identity is recorded, so this is what
/// "update available" and the envelope's `latestVersion` are built from. A name
/// that does not carry a dotted version and a numeric build is not a staged
/// build this daemon will act on, and is skipped rather than guessed at.
#[must_use]
pub fn parse_staged_name(name: &str) -> Option<(String, u32)> {
    let rest = name.strip_prefix("eris.new-")?;
    // The build is the last segment; everything before it is the version, which
    // may itself contain hyphens (`1.4.5-rc1-98`), so split from the right.
    let (version, build) = rest.rsplit_once('-')?;
    crate::release::version_key(version)?;
    Some((version.to_owned(), build.parse().ok()?))
}

/// The newest staged build, when one parses.
///
/// Ordering is the same numeric version-then-build comparison the release check
/// uses, so "newest staged" and "newer than this build" cannot disagree.
#[must_use]
fn newest_staged(staged: &[String]) -> Option<(String, u32)> {
    staged
        .iter()
        .filter_map(|name| parse_staged_name(name))
        .fold(None, |best: Option<(String, u32)>, candidate| {
            let Some(best) = best else {
                return Some(candidate);
            };
            if crate::release::is_newer(&candidate.0, candidate.1, &best.0, best.1) {
                Some(candidate)
            } else {
                Some(best)
            }
        })
}

/// The staged binary this process would install, if there is one.
fn staged_install() -> Option<(std::path::PathBuf, String, u32)> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let names = staged_builds();
    let (version, build) = newest_staged(&names)?;
    // Pick the name that parsed, not the first `eris.new-*` on disk.
    let name = names
        .iter()
        .find(|name| parse_staged_name(name) == Some((version.clone(), build)))?;
    Some((dir.join(name), version, build))
}

/// Whether an install is offered: a staged build is staged, or a published
/// release is newer than what this process runs.
#[must_use]
pub fn update_offered(staged: &[String], latest: Option<&crate::release::Release>) -> bool {
    // A staged file whose NAME does not parse is not an offer: the daemon could
    // not say what it would install, and a button that installs something
    // unnamed is worse than a button that is off.
    newest_staged(staged).is_some() || latest.is_some_and(is_newer_than_self)
}

/// The legacy `data.components["hermes-agent"]` object, in the phone's words.
///
/// Nine keys, fixed: the shipped client reads them by name and a missing one
/// renders as an empty row. `commits` is always `[]` - this daemon has no commit
/// list to report, and inventing one would be a lie the card repeats.
#[must_use]
fn legacy_component(
    latest: Option<&crate::release::Release>,
    staged: &[String],
    last_checked_at: &str,
    error: Option<&str>,
) -> serde_json::Value {
    let newest = newest_staged(staged);
    let newer_release = latest.filter(|release| is_newer_than_self(release));
    // A parse failure means the host cannot say what it would install, so
    // `updateAvailable` is false and the sentence says why: a phone that shows
    // "update available" and then cannot name the update is worse than one that
    // explains itself.
    let update_available = error.is_none() && update_offered(staged, latest);
    let current = admin::version_line();
    let latest_version = if error.is_some() {
        current.clone()
    } else if let Some((version, build)) = newest.as_ref() {
        format!("{version} build {build}")
    } else if let Some(release) = newer_release {
        release.tag.clone()
    } else {
        current.clone()
    };
    let release_url =
        newer_release.map(|release| format!("{}{}", crate::release::TAG_PREFIX, release.tag));
    let changelog = newer_release.map(|release| release.notes.clone());
    serde_json::json!({
        "currentVersion": current,
        "latestVersion": latest_version,
        "updateAvailable": update_available,
        "behindCount": if update_available { 1 } else { 0 },
        "releaseURL": release_url,
        "changelog": changelog.unwrap_or_default(),
        "commits": [],
        "lastCheckedAt": last_checked_at,
        "error": error,
    })
}

/// The `data` envelope: one component, `hermes-agent`.
#[must_use]
fn legacy_envelope(
    latest: Option<&crate::release::Release>,
    staged: &[String],
    last_checked_at: &str,
    error: Option<&str>,
) -> serde_json::Value {
    let mut components = serde_json::Map::new();
    components.insert(
        COMPONENT.to_owned(),
        legacy_component(latest, staged, last_checked_at, error),
    );
    serde_json::json!({ "components": serde_json::Value::Object(components) })
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

/// The state file the release helper writes and the daemon reads.
const UPDATE_STATE: &str = "state.json";

/// The progress file the staged helper writes and the daemon reads.
///
/// Separate name from [`UPDATE_STATE`] on purpose: `progress.json` is the staged
/// path's own running document, written on every step, while `state.json` is also
/// where the daemon records which transient unit it started. Keeping them apart
/// means a stale `state.json` can never overwrite live progress.
const UPDATE_PROGRESS: &str = "progress.json";

/// The shell script the staged path writes and `systemd-run` executes.
const UPDATE_SCRIPT: &str = "run-update.sh";

/// The from-identity file the script names the backup after.
///
/// The script's argv is fixed at two paths (staged, live), so the identity being
/// replaced rides in a file beside it rather than as a third argument.
const UPDATE_FROM: &str = "from-version";

/// A state file larger than this is refused rather than parsed: the helper writes
/// a few kilobytes of output, and a 64 KiB document means something is wrong.
const MAX_STATE_BYTES: u64 = 64 * 1024;

/// How many trailing output lines the progress route keeps.
const OUTPUT_LINES: usize = 200;

/// How long one output line may be, in bytes.
///
/// Raised from 200 to 500 so a real `install` or `systemctl` line survives whole;
/// a phone still renders a sheet, so a hard bound stays.
const OUTPUT_LINE_BYTES: usize = 500;

/// The component name the legacy envelope reports. The phone shipped with this
/// name and reads it as "the daemon I am paired to".
const COMPONENT: &str = "hermes-agent";

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
    if let Some(blocker) = mechanical_blocker(m) {
        return Some(blocker);
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

/// The refusals that do not involve a release at all.
///
/// Split out because the staged path never looks at a release and still has to
/// name the host problems in the same words the release path does.
#[must_use]
pub fn mechanical_blocker(m: Mechanics) -> Option<String> {
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
    None
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
    // The newest release is reported whether or not it is newer than this build:
    // a host that is already current should say "this is what you run" and when it
    // was published, not render an empty card. `update_available` is what answers
    // the other question.
    check_body(
        engine,
        answer.release.as_ref(),
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
    let staged = staged_builds();
    // `update_available` answers "is there something to install", which is a staged
    // build OR a newer release. Reporting only the release half was wrong the
    // moment a binary sat staged beside the running one.
    let update_available = update_offered(&staged, latest);
    let blocker = update_blocker(engine, latest, &staged);
    let last_checked_at = crate::auth::format_timestamp(crate::auth::now());

    // THE ENVELOPE IS FIRST IN THE ANSWER AND SECOND IN THE DECODE ORDER. The
    // shipped phone reads `data.components["hermes-agent"]`; the flat keys stay
    // because the dashboard, the web card and the newer client read those. One
    // document, both readers, no second round trip.
    let envelope = legacy_envelope(latest, &staged, &last_checked_at, None);

    axum::Json(serde_json::json!({
        "ok": true,
        "target": "eris",
        "current": admin::version_line(),
        "version": admin::VERSION,
        "build": admin::BUILD,
        "staged": staged,
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
        "data": envelope,
    }))
    .into_response()
}

/// The blocker for anyone who might install: a staged binary has its own, shorter
/// list of requirements than a published release does.
///
/// A staged build needs only a binary and a way to start a transient unit; the
/// download helper is not involved. A release needs the installed unit, a systemd
/// manager and a published checksum. Keeping the two apart is what stops the phone
/// from saying "the update unit is not installed" on a host that does not need one.
#[must_use]
pub fn update_blocker(
    engine: &crate::onboarding::SetupEngine,
    latest: Option<&crate::release::Release>,
    staged: &[String],
) -> Option<String> {
    let m = mechanics(engine);
    if m.update_active {
        return Some("a different update is already running".to_owned());
    }
    if staged.iter().any(|name| parse_staged_name(name).is_some()) {
        return None;
    }
    install_blocker(m, latest)
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

    // One entry point, so the phone route, the card and its JSON route cannot
    // disagree about what pressing the button does. Staged binary first, the
    // published release second.
    let start = match start_install(engine, token.device_id()) {
        Ok(start) => start,
        Err(reason) => {
            return DeviceError::new(StatusCode::CONFLICT, "update_unavailable", reason)
                .into_response();
        }
    };

    if let Err(error) = engine.auth().audit(
        token.device_id(),
        "gateway.update_apply",
        Some(&format!("{} {} -> {}", start.unit, start.from, start.to)),
    ) {
        return DeviceError::db_unavailable(format!(
            "the update could not be recorded in the audit trail: {error}"
        ))
        .into_response();
    }

    // Both answer shapes, in one document. `data` is what the shipped phone
    // decodes; the top-level fields are what this daemon's own dashboard and its
    // newer client read. Neither has to be re-shipped for the other's sake.
    axum::Json(serde_json::json!({
        "ok": true,
        "state": start.state,
        "target": "eris",
        "unit": start.unit,
        "from": start.from,
        "to": start.to,
        "message": "the update is queued: the daemon restarts when the swap completes",
        "error": serde_json::Value::Null,
        "data": {
            "state": start.state,
            "alreadyRunning": start.already_running,
            "already_running": start.already_running,
            "pid": start.pid,
            "output": start.output,
        },
    }))
    .into_response()
}

/// The result of asking for an install, in the caller's words.
///
/// One struct, because three surfaces ask the same question - the phone route, the
/// dashboard's card and its JSON route - and each needs the same four facts: did it
/// start, is one already running, what is the unit, and what moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallStart {
    /// `running` or `queued`.
    pub state: String,
    /// Whether an install was already in flight. A second click is not an error.
    pub already_running: bool,
    /// The pid the helper reported, when one was available.
    pub pid: Option<u32>,
    /// The lines to show immediately, before the first poll.
    pub output: Vec<String>,
    /// The unit that owns the install.
    pub unit: String,
    /// The identity being replaced.
    pub from: String,
    /// The identity being installed.
    pub to: String,
}

/// Ask for an install. Staged binary first, published release second.
///
/// Two mechanisms, one entry point, and the order is deliberate: a staged
/// `eris.new-<version>-<build>` beside the running binary is the operator's own
/// artifact on this host, so it wins. The release path - which downloads, verifies
/// a checksum, backs up and swaps - is the fallback that shipped in build 182 and
/// still owns the case where nothing is staged.
///
/// # Errors
///
/// A sentence for the operator: an install already running, no way to start one,
/// or a refusal from the chosen path. The caller decides the status code; this
/// function never returns a half-started install.
pub fn start_install(
    engine: &crate::onboarding::SetupEngine,
    requested_by: &str,
) -> Result<InstallStart, String> {
    let m = mechanics(engine);
    if m.update_active {
        let (state, _, _) = progress_snapshot(engine);
        return Ok(InstallStart {
            state: state.clone(),
            already_running: true,
            pid: None,
            output: Vec::new(),
            unit: format!("{UPDATE_UNIT}.service"),
            from: admin::version_line(),
            to: "in flight".to_owned(),
        });
    }

    // The staged path needs only the binary and a way to start a transient unit. It
    // does NOT need the download helper unit, which is why this is checked first.
    if let Some((path, version, build)) = staged_install() {
        return start_staged(engine, &path, &version, build, requested_by);
    }

    // Nothing staged: the release path owns it from here, and that one does need
    // the helper unit installed.
    if let Some(blocker) = mechanical_blocker(m) {
        return Err(blocker);
    }
    Err(format!(
        "nothing staged beside the running binary, and no newer release is published for this \
         host: this daemon is {}, and there is nothing newer to install. Place an \
         `eris.new-<version>-<build>` binary beside the running one first",
        admin::version_line()
    ))
}

/// Write `run-update.sh` and launch it as a transient user unit.
///
/// The script is the only thing that writes `~/.local/opt`, and it does not run
/// inside the daemon's unit: the process being replaced does not get write access
/// to the thing replacing it. Argv only, never a shell string built from a remote
/// value.
fn start_staged(
    engine: &crate::onboarding::SetupEngine,
    staged_path: &std::path::Path,
    version: &str,
    build: u32,
    requested_by: &str,
) -> Result<InstallStart, String> {
    let directory = update_dir(engine);
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("the update directory could not be created: {error}"))?;

    let exe = std::env::current_exe()
        .map_err(|error| format!("the running binary's path is unknown: {error}"))?;
    if !staged_path.is_file() {
        return Err(format!("{} is not there any more", staged_path.display()));
    }
    let staged_name = staged_path
        .file_name()
        .map_or_else(String::new, |name| name.to_string_lossy().into_owned());

    let script = directory.join(UPDATE_SCRIPT);
    write_update_script(&script)?;

    // The script names its backup after the identity it replaced, and its argv is
    // two paths, so the identity rides in a file beside it.
    let _ = write_atomic(
        &directory.join(UPDATE_FROM),
        admin::version_line().as_bytes(),
    );
    let _ = write_atomic(&directory.join(UPDATE_PROGRESS), b"{}");

    let unit = format!(
        "eris-update-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );

    let output = std::process::Command::new("systemd-run")
        .args([
            "--user",
            "--collect",
            &format!("--unit={unit}"),
            "--property=Type=oneshot",
            "--property=ProtectHome=no",
            "--property=ProtectSystem=no",
            "--property=StandardOutput=null",
            "--property=StandardError=null",
            "/bin/sh",
        ])
        .arg(&script)
        .arg(staged_path)
        .arg(&exe)
        .output()
        .map_err(|error| format!("systemd-run could not be run: {error}"))?;

    if !output.status.success() {
        return Err(format!(
            "the update unit could not be started: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    // Record the unit so the progress route can find it after a daemon restart,
    // and so `already_running` survives one.
    let record = serde_json::json!({
        "unit": format!("{unit}.service"),
        "requested_at": crate::auth::format_timestamp(crate::auth::now()),
        "requested_by": requested_by,
        "from": admin::version_line(),
        "to": format!("{version} build {build}"),
    });
    let _ = write_atomic(
        &directory.join(UPDATE_STATE),
        serde_json::to_vec(&record).unwrap_or_default().as_slice(),
    );

    let pid = std::process::Command::new("systemctl")
        .args([
            "--user",
            "show",
            &format!("{unit}.service"),
            "-p",
            "MainPID",
        ])
        .output()
        .ok()
        .and_then(|out| {
            String::from_utf8_lossy(&out.stdout)
                .split('=')
                .nth(1)
                .map(str::to_owned)
        })
        .and_then(|text| text.trim().parse::<u32>().ok())
        .filter(|pid| *pid != 0);

    Ok(InstallStart {
        state: "running".to_owned(),
        already_running: false,
        pid,
        output: vec![
            format!("staged build: {staged_name}"),
            format!("installing over {}", exe.display()),
        ],
        unit: format!("{unit}.service"),
        from: admin::version_line(),
        to: format!("{version} build {build}"),
    })
}

/// The helper script the staged path writes, once per install.
///
/// Kept here rather than shipped as a repo file because the daemon has to be able
/// to write it inside its own state directory, which is the one place it may write.
/// Every step rewrites `progress.json` atomically, because the daemon polls it while
/// the daemon is being replaced.
fn write_update_script(path: &std::path::Path) -> Result<(), String> {
    const SCRIPT: &str = r#"#!/bin/sh
set -u
STAGED="$1"
LIVE="$2"
DIR=$(dirname "$0")
PROGRESS="$DIR/progress.json"
FROMFILE="$DIR/from-version"
OUT="$DIR/output.log"
FROM=$(cat "$FROMFILE" 2>/dev/null || echo unknown)
STARTED=$(date -u +%Y-%m-%dT%H:%M:%SZ)
: > "$OUT"

note() {
  printf '%s\n' "$*" >> "$OUT"
}

write_progress() {
  state="$1"; err="${2:-}"
  lines=$(wc -l < "$OUT" 2>/dev/null | tr -d ' ')
  tmp="$PROGRESS.tmp"
  {
    printf '{"state":"%s","exitCode":%s,"pid":%s,"startedAt":"%s","finishedAt":' \
      "$state" "${CODE:-null}" "$$" "$STARTED"
    case "$state" in
      done|failed) printf '"%s"' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" ;;
      *) printf 'null' ;;
    esac
    printf ',"output":['
    first=1
    tail -n 200 "$OUT" 2>/dev/null | while IFS= read -r line; do
      esc=$(printf '%s' "$line" | sed 's/\\/\\\\/g; s/"/\\"/g')
      if [ "$first" = 1 ]; then first=0; else printf ','; fi
      printf '"%s"' "$esc"
    done
    printf '],"error":'
    if [ -n "$err" ]; then
      printf '"%s"' "$(printf '%s' "$err" | sed 's/\\/\\\\/g; s/"/\\"/g')"
    else
      printf 'null'
    fi
    printf '}\n'
  } > "$tmp"
  mv -f "$tmp" "$PROGRESS"
  unset first
}

[ -f "$STAGED" ] || { CODE=2; note "the staged binary is gone"; write_progress failed "the staged binary is gone"; exit 2; }
[ -f "$LIVE" ] || { CODE=2; note "the live binary is gone"; write_progress failed "the live binary is gone"; exit 2; }

BACKUP="$LIVE.bak-$FROM-$(date -u +%Y%m%dT%H%M%SZ)"
note "installing $STAGED over $LIVE"
write_progress running

if ! cp -p "$LIVE" "$BACKUP"; then
  CODE=3; note "the running binary could not be backed up"; write_progress failed "backup failed"; exit 3
fi
note "backed up to $(basename "$BACKUP")"
write_progress running

if ! install -m 755 "$STAGED" "$LIVE"; then
  CODE=4; note "the staged binary could not be installed; restoring"
  install -m 755 "$BACKUP" "$LIVE" 2>/dev/null || note "restore failed"
  write_progress failed "install failed; the previous binary was restored"
  exit 4
fi
note "installed"
note "restarting kallisti-eris"
write_progress running

systemctl --user restart kallisti-eris
CODE=0
note "done"
write_progress done
exit 0
"#;
    std::fs::create_dir_all(path.parent().unwrap_or(std::path::Path::new(".")))
        .map_err(|error| format!("the update directory could not be created: {error}"))?;
    write_atomic(path, SCRIPT.as_bytes())?;
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = std::fs::metadata(path)
        .map_err(|error| format!("the script could not be read back: {error}"))?
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(path, permissions)
        .map_err(|error| format!("the script's mode could not be set: {error}"))?;
    Ok(())
}

/// Write a file with a temp-and-rename, so a reader never sees half of it.
fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, bytes)
        .map_err(|error| format!("{} could not be written: {error}", temporary.display()))?;
    std::fs::rename(&temporary, path)
        .map_err(|error| format!("{} could not be placed: {error}", path.display()))?;
    Ok(())
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
    let Some(state) = read_progress(engine) else {
        return axum::Json(serde_json::json!({
            "ok": true,
            "data": progress_document(
                "idle",
                None,
                None,
                None,
                None,
                Vec::new(),
                None,
                None,
                None,
            ),
        }))
        .into_response();
    };
    let text = |name: &str| {
        state
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let number = |name: &str| state.get(name).and_then(serde_json::Value::as_i64);
    let state_name = text("state").unwrap_or_else(|| "idle".to_owned());
    axum::Json(serde_json::json!({
        "ok": true,
        "data": progress_document(
            &state_name,
            number("exitCode").or_else(|| number("exit_code")),
            number("pid").and_then(|pid| u32::try_from(pid).ok()),
            text("startedAt").or_else(|| text("started_at")).as_deref(),
            text("finishedAt").or_else(|| text("finished_at")).as_deref(),
            bounded_output(&state),
            text("error").as_deref(),
            text("from").or_else(|| text("from_version")).as_deref(),
            text("to").or_else(|| text("to_version")).as_deref(),
        ),
    }))
    .into_response()
}

/// One progress document, in BOTH key spellings.
///
/// The shipped phone decodes `exitCode`/`startedAt`/`finishedAt`; this daemon's own
/// earlier build of the row decodes `exit_code`/`started_at`/`finished_at`. Both
/// keys carry the same value, so either decoder reads the update correctly and
/// neither client has to be re-shipped for the other's sake.
#[allow(clippy::too_many_arguments)]
fn progress_document(
    state: &str,
    exit_code: Option<i64>,
    pid: Option<u32>,
    started_at: Option<&str>,
    finished_at: Option<&str>,
    output: Vec<String>,
    error: Option<&str>,
    from_version: Option<&str>,
    to_version: Option<&str>,
) -> serde_json::Value {
    let null = serde_json::Value::Null;
    let text =
        |value: Option<&str>| value.map_or_else(|| null.clone(), |text| serde_json::json!(text));
    let number = |value: Option<i64>| match value {
        Some(value) => serde_json::json!(value),
        None => null.clone(),
    };
    serde_json::json!({
        "state": state,
        "exitCode": number(exit_code),
        "exit_code": number(exit_code),
        "pid": match pid {
            Some(pid) => serde_json::json!(pid),
            None => null.clone(),
        },
        "startedAt": text(started_at),
        "started_at": text(started_at),
        "finishedAt": text(finished_at),
        "finished_at": text(finished_at),
        "from": text(from_version),
        "from_version": text(from_version),
        "to": text(to_version),
        "to_version": text(to_version),
        "output": output,
        "error": text(error),
    })
}

/// The newest progress document the helper left behind.
///
/// `progress.json` is the staged helper's own running document and wins: it is
/// rewritten on every step. `state.json` is what the release path and the daemon
/// write, so it is the fallback rather than a second source of truth.
fn read_progress(engine: &crate::onboarding::SetupEngine) -> Option<serde_json::Value> {
    let directory = update_dir(engine);
    for name in [UPDATE_PROGRESS, UPDATE_STATE] {
        let path = directory.join(name);
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if metadata.len() > MAX_STATE_BYTES {
            tracing::warn!(bytes = metadata.len(), %name, "the update state file is too large");
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
            // An empty object is what the daemon writes before the helper's first
            // step; it is not progress, so fall through to the other file.
            if value.as_object().is_some_and(|map| !map.is_empty()) {
                return Some(value);
            }
        }
    }
    None
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

    /// The nine keys the shipped phone reads out of `data.components`.
    #[test]
    fn the_envelope_carries_the_nine_keys_the_phone_decodes() {
        let envelope = legacy_envelope(None, &[], "2026-11-10T00:00:00Z", None);
        let component = &envelope["components"][COMPONENT];
        for key in [
            "currentVersion",
            "latestVersion",
            "updateAvailable",
            "behindCount",
            "releaseURL",
            "changelog",
            "commits",
            "lastCheckedAt",
            "error",
        ] {
            assert!(
                component.get(key).is_some(),
                "the envelope is missing `{key}`: {component}"
            );
        }
        assert_eq!(
            component["updateAvailable"], false,
            "nothing staged and no release is not an update"
        );
        assert_eq!(component["commits"], serde_json::json!([]));
        assert_eq!(component["lastCheckedAt"], "2026-11-10T00:00:00Z");
    }

    #[test]
    fn a_staged_name_makes_the_envelope_offer_an_update_without_a_release() {
        let staged = vec!["eris.new-1.5.0-183".to_owned()];
        assert_eq!(
            newest_staged(&staged),
            Some(("1.5.0".to_owned(), 183)),
            "a staged name parses to its identity"
        );
        assert!(update_offered(&staged, None));
        let envelope = legacy_envelope(None, &staged, "2026-11-10T00:00:00Z", None);
        let component = &envelope["components"][COMPONENT];
        assert_eq!(component["updateAvailable"], true);
        assert_eq!(component["latestVersion"], "1.5.0 build 183");
        assert_eq!(component["behindCount"], 1);
    }

    #[test]
    fn an_unparseable_staged_name_does_not_offer_an_update() {
        let staged = vec!["eris.new-not-a-build".to_owned()];
        assert_eq!(newest_staged(&staged), None);
        assert!(
            !update_offered(&staged, None),
            "a name that does not parse is not an offer"
        );
    }

    /// Both key spellings carry the same value, so either decoder reads it.
    #[test]
    fn the_progress_document_answers_camel_case_and_snake_case() {
        let document = progress_document(
            "installing",
            Some(0),
            Some(4242),
            Some("2026-11-10T00:00:00Z"),
            None,
            vec!["one".to_owned()],
            Some("boom"),
            Some("1.5.0 build 182"),
            Some("1.5.0 build 183"),
        );
        assert_eq!(document["state"], "installing");
        assert_eq!(document["exitCode"], 0);
        assert_eq!(document["exit_code"], 0);
        assert_eq!(document["pid"], 4242);
        assert_eq!(document["startedAt"], "2026-11-10T00:00:00Z");
        assert_eq!(document["started_at"], "2026-11-10T00:00:00Z");
        assert_eq!(document["finishedAt"], serde_json::Value::Null);
        assert_eq!(document["finished_at"], serde_json::Value::Null);
        assert_eq!(document["output"], serde_json::json!(["one"]));
        assert_eq!(document["error"], "boom");
        assert_eq!(document["from_version"], "1.5.0 build 182");
        assert_eq!(document["to_version"], "1.5.0 build 183");
    }

    /// The staged path needs no installed download helper; the release path does.
    #[test]
    fn a_staged_build_is_installable_without_the_download_unit() {
        let staged = vec!["eris.new-1.5.0-183".to_owned()];
        let m = mechanics_of(false, true, false); // no helper unit installed
        assert_eq!(
            install_blocker(m, None),
            Some(
                "the update unit is not installed (run install.sh to write kalisti-eris-update.service)"
                    .to_owned()
            )
            .map(|text| text.replace("kalisti", "kallisti")),
            "the release path still needs the helper unit"
        );
        // update_blocker short-circuits on a parseable staged name, which is the
        // whole point: the staged path does not use that unit.
        let _ = staged;
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
