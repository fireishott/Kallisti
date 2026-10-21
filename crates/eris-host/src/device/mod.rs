//! The device listener: `127.0.0.1:7341`, `/v1` only, off by default.
//!
//! This is the surface a paired phone talks to. It is a second listener, not a
//! second admin plane: nothing from the wizard, the dashboard, or `/healthz`
//! exists here, and the fallback answers a typed 404 for every path outside
//! `/v1`. The port is a literal for the same reason the admin port is - "reachable
//! from this host only" must survive a copied config.
//!
//! `/v1/settings` is the one route here that touches a setting, and it is not an
//! admin plane either: it reads a fixed, credential-free snapshot and writes four
//! closed-set values, and only for a token whose pairing code was minted with the
//! administration box ticked.
//!
//! The approval queue and the MCP servers are reachable too: `GET /v1/approvals`
//! and `POST /v1/approvals/{id}/decide` behind `admin`, and `GET /v1/mcp` behind
//! `sessions`. They settle a row and report health. They do not run a parked call,
//! and they do not make a device turn park one: a turn on this surface refuses a
//! gated call instead of waiting, because it has no resume path over here.
//!
//! The administration half an operator holding a phone needs is here as well, all
//! of it behind `admin` and none of it a second implementation: `GET /v1/devices`
//! reads the same rows the devices page reads, `POST /v1/devices/{id}/revoke`
//! calls the same store function the revoke button calls, `GET /v1/audit` reads
//! the trail the Activity page renders, and `/v1/persona` reads and writes the
//! identity file the settings page writes. A phone that may change the settings
//! may also see what it changed and retire the device that should not be there.
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
use axum::routing::{get, post, put};
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

/// The `settings` row holding the addresses the device listener binds, as JSON.
pub const SETTING_LISTEN: &str = "device.listen";

/// The `settings` row holding the origin a phone should call, when the operator
/// has one. This is what the pairing QR carries, so it is the one address a
/// phone can actually route to rather than the one this host binds for itself.
pub const SETTING_RELAY: &str = "device.relay_url";

/// Trim and accept one device relay origin.
///
/// The phone app appends `/v1/...` to whatever the QR carries, so a path, a
/// query or a fragment would build a request no route serves. Requiring an
/// http(s) origin is the whole rule; it is checked here so a saved value and a
/// freshly typed one cannot disagree.
///
/// # Errors
///
/// The sentence the settings form shows next to the field.
pub fn validate_relay(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("the relay URL cannot be empty".to_owned());
    }
    let Some((scheme, rest)) = trimmed.split_once("://") else {
        return Err(format!(
            "`{trimmed}` needs http:// or https:// in front of the host"
        ));
    };
    if !matches!(scheme, "http" | "https") {
        return Err(format!(
            "`{scheme}` is not a scheme a phone can call; use http or https"
        ));
    }
    if rest.is_empty() || rest.contains('/') || rest.contains('?') || rest.contains('#') {
        return Err(
            "the relay is an origin only, like https://host.tailnet.ts.net - the app adds /v1"
                .to_owned(),
        );
    }
    if rest.contains('@') || rest.chars().any(char::is_whitespace) || rest.starts_with(':') {
        return Err(format!("`{rest}` is not a host a phone can reach"));
    }
    Ok(trimmed.to_owned())
}

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

/// `GET /v1/version` - which daemon a client is talking to, without a token.
///
/// A client that has just scanned a pairing code has no bearer yet, and the first
/// thing a person wants to know when pairing misbehaves is whether the phone and
/// the host are even on speaking terms. Nothing secret is in it: a version, a
/// build and the frame names this daemon speaks.
pub const ROUTE_VERSION: &str = "/v1/version";

/// `GET /v1/projects` and `POST /v1/projects` - the list, and naming one.
pub const ROUTE_PROJECTS: &str = "/v1/projects";

/// `POST /v1/projects/assign` - file one conversation under a project.
pub const ROUTE_PROJECTS_ASSIGN: &str = "/v1/projects/assign";

/// `GET /v1/conversations` and `POST /v1/conversations` - the transcript list,
/// and starting one.
pub const ROUTE_CONVERSATIONS: &str = "/v1/conversations";

/// `GET /v1/conversations/{id}` - one conversation and its messages.
pub const ROUTE_CONVERSATION: &str = "/v1/conversations/{id}";

/// `POST /v1/conversations/{id}/clear` - drop that conversation's messages.
pub const ROUTE_CONVERSATION_CLEAR: &str = "/v1/conversations/{id}/clear";

/// `GET /v1/settings` and `POST /v1/settings` - the operator-facing settings,
/// behind the `admin` scope.
pub const ROUTE_SETTINGS: &str = "/v1/settings";

/// `GET /v1/approvals` - the calls a turn parked, behind the `admin` scope.
///
/// The queue was reachable from the dashboard's count and from `eris approvals`
/// on the host until this route existed: a phone could start a turn but could not
/// see the write it stopped on, so answering one meant walking to the machine.
pub const ROUTE_APPROVALS: &str = "/v1/approvals";

/// `POST /v1/approvals/{id}/decide` - settle one parked call, behind `admin`.
pub const ROUTE_APPROVAL_DECIDE: &str = "/v1/approvals/{id}/decide";

/// `GET /v1/mcp` - the configured MCP servers and the health the daemon
/// observed, behind the `sessions` scope.
///
/// Not `admin`: reading what is configured and whether it answered is the same
/// class of read as the transcript, and the server list carries no credential.
pub const ROUTE_MCP: &str = "/v1/mcp";

/// `GET /v1/devices` - every device this daemon knows, behind the `admin` scope.
///
/// The dashboard's devices page and this route read the same rows: a phone that
/// can change the settings can also see which devices exist and which of them
/// may administer.
pub const ROUTE_DEVICES: &str = "/v1/devices";

/// `POST /v1/devices/{id}/revoke` - retire one device and its tokens, behind the
/// `admin` scope.
///
/// The same store call the dashboard's revoke button makes, so there is one
/// revocation and not two: the browser form and this route cannot disagree about
/// what "revoked" means.
pub const ROUTE_DEVICE_REVOKE: &str = "/v1/devices/{id}/revoke";

/// `GET /v1/audit` - the operator's own trail, behind the `admin` scope.
///
/// The Activity page renders the same rows through the same store reads. It is
/// the trail of actions, not of content: a row names a device, a file or a
/// setting and never a secret.
pub const ROUTE_AUDIT: &str = "/v1/audit";

/// `GET /v1/persona` and `POST /v1/persona` - the assistant's identity text,
/// behind the `admin` scope.
///
/// The same file and the same cap the settings page uses. Reading it is reading
/// what the next turn would be told; writing it is writing the file, not a copy
/// in the database.
pub const ROUTE_PERSONA: &str = "/v1/persona";

/// How many devices one page of `GET /v1/devices` reports.
///
/// A device list on this host is tens of rows; the cap exists so the body cannot
/// grow without bound on a daemon that was paired many times, and the client is
/// told when it is looking at part of a longer list.
pub const DEVICE_LIST_LIMIT: usize = 200;

/// The page size `GET /v1/audit` uses when the request names none.
pub const AUDIT_DEFAULT_LIMIT: usize = 50;

/// The largest page `GET /v1/audit` will serve.
///
/// A trail is unbounded in principle: the newest two hundred rows are what a
/// person holding a phone reads, and `total` says how much more exists.
pub const AUDIT_MAX_LIMIT: usize = 200;

/// How many approvals one page of `GET /v1/approvals` reports.
///
/// A queue is a shortlist for a person holding a phone, not an archive: the
/// newest fifty are enough to answer, and a client is told when it is looking at
/// part of a longer list.
pub const APPROVAL_LIST_LIMIT: usize = 50;

/// How many conversations one page of `GET /v1/conversations` reports.
///
/// A list is a shortlist, not an archive: a phone that pages through its history
/// gets the newest hundred and is told there is more, rather than a body that
/// grows with the account.
pub const CONVERSATION_LIST_LIMIT: usize = 100;

/// How many messages one `GET /v1/conversations/{id}` reports.
///
/// A transcript is unbounded in principle, so the route bounds it in practice and
/// says when it did.
pub const CONVERSATION_MESSAGE_LIMIT: usize = 200;

/// `GET /v1/backups` and `POST /v1/backups` - the archive list, and writing one,
/// behind the `admin` scope.
///
/// The list is the backups page's own listing and the create is the page's own
/// write, so a phone that may change the settings may also take a backup before
/// it, and see the one already on disk. There is no delete here because the page
/// has none: removing a backup from a phone is not a capability to invent.
pub const ROUTE_BACKUPS: &str = "/v1/backups";

/// `POST /v1/backups/{name}/restore` - replace the live state from one archive,
/// behind the `admin` scope.
///
/// The name is a bare filename in the path, validated by the same rule the
/// browser's restore form applies before anything touches the filesystem.
pub const ROUTE_BACKUP_RESTORE: &str = "/v1/backups/{name}/restore";

/// `POST /v1/devices/{id}/code` - mint one pairing code for that device, behind
/// the `admin` scope.
///
/// The same mint the devices page performs, so a second device is onboarded from
/// a phone that is already paired rather than from a browser on the host. The
/// code is single use, is stored only as an Argon2id verifier, and is readable
/// exactly once: in that response.
pub const ROUTE_DEVICE_CODE: &str = "/v1/devices/{id}/code";

/// `POST /v1/device/sensor/location` - record one location point.
pub const ROUTE_SENSOR_LOCATION: &str = "/v1/device/sensor/location";

/// `POST /v1/device/sensor/health` - record health samples.
pub const ROUTE_SENSOR_HEALTH: &str = "/v1/device/sensor/health";

/// `GET /v1/device/health/latest` - the latest reading per metric.
pub const ROUTE_HEALTH_LATEST: &str = "/v1/device/health/latest";

/// `GET /v1/device/location` - the most recent location point.
pub const ROUTE_LOCATION: &str = "/v1/device/location";

/// `GET /v1/inbox` - list the device's inbox items.
pub const ROUTE_INBOX: &str = "/v1/inbox";

/// `POST /v1/inbox/{item_id}/action` - act on one inbox item.
pub const ROUTE_INBOX_ACTION: &str = "/v1/inbox/{item_id}/action";

/// `POST /internal/inbox/create` - internal create, behind the `admin` scope.
///
/// This is the one device-listener path outside `/v1`. The legacy relay guarded
/// it with an `X-Relay-Internal-Key` header; Eris has no internal key, so it is
/// gated behind a bearer token that carries the `admin` scope instead.
pub const ROUTE_INTERNAL_INBOX_CREATE: &str = "/internal/inbox/create";

/// `GET /v1/notes` and `POST /v1/notes` - the list, and creating one.
pub const ROUTE_NOTES: &str = "/v1/notes";

/// `GET /v1/notes/{id}`, `PATCH /v1/notes/{id}` and `DELETE /v1/notes/{id}`.
pub const ROUTE_NOTE: &str = "/v1/notes/{note_id}";

/// `PUT /v1/notes/{id}/blobs/{revision}` and `GET` - the drawing blob.
pub const ROUTE_NOTE_BLOB: &str = "/v1/notes/{note_id}/blobs/{revision}";

/// `POST /v1/notes/{id}/recognitions` - store an OCR/handwriting result.
pub const ROUTE_NOTE_RECOGNITIONS: &str = "/v1/notes/{note_id}/recognitions";

/// `POST /v1/notes/{id}/runs` - start an enrichment run.
pub const ROUTE_NOTE_RUNS: &str = "/v1/notes/{note_id}/runs";

/// `GET /v1/note-runs/{run_id}` - one run.
pub const ROUTE_NOTE_RUN: &str = "/v1/note-runs/{run_id}";

/// `GET /v1/note-runs/{run_id}/events` - the run's event log.
pub const ROUTE_NOTE_RUN_EVENTS: &str = "/v1/note-runs/{run_id}/events";

/// `POST /v1/note-runs/{run_id}/cancel` - stop a run.
pub const ROUTE_NOTE_RUN_CANCEL: &str = "/v1/note-runs/{run_id}/cancel";

/// `POST /v1/push/register` - upsert a device's push token.
pub const ROUTE_PUSH_REGISTER: &str = "/v1/push/register";

/// `POST /v1/push/deactivate` - deactivate a device's push tokens.
pub const ROUTE_PUSH_DEACTIVATE: &str = "/v1/push/deactivate";

/// `POST /v1/push/send` - send a push (stubbed: 503 without an APNs key).
pub const ROUTE_PUSH_SEND: &str = "/v1/push/send";

/// `GET /v1/talk/readiness` - placeholder readiness probe.
pub const ROUTE_TALK_READINESS: &str = "/v1/talk/readiness";

/// `POST /v1/talk/session` - placeholder talk session create.
pub const ROUTE_TALK_SESSION: &str = "/v1/talk/session";

/// `POST /v1/talk/session/{id}/end` - placeholder talk session end.
pub const ROUTE_TALK_SESSION_END: &str = "/v1/talk/session/{id}/end";

/// `POST /v1/talk/session/{id}/turns` - placeholder talk turn create.
pub const ROUTE_TALK_TURNS: &str = "/v1/talk/session/{id}/turns";

/// `POST /v1/talk/session/{id}/inject` - placeholder transcript inject.
pub const ROUTE_TALK_INJECT: &str = "/v1/talk/session/{id}/inject";

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

    /// 404 for a conversation id the daemon does not hold.
    ///
    /// A conversation is not owned by a device: it is the operator's transcript,
    /// so there is no ownership half to hide. An id that is not there is simply
    /// not there.
    #[must_use]
    pub fn conversation_not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "conversation_not_found",
            "no conversation with that id exists on this daemon",
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

    /// 404 for an approval id that is not waiting.
    ///
    /// One code for both "never existed" and "already decided", because a row
    /// that a decision has settled is no longer an approval anybody can answer:
    /// telling the two apart would let a client poll for which ids were real.
    #[must_use]
    pub fn approval_not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "approval_not_found",
            "no approval with that id is waiting: it was never queued, or a decision already settled it",
        )
    }

    /// 404 for a device id this daemon does not hold.
    ///
    /// The browser's revoke form answers the same situation with the auth
    /// store's own `unknown_device`; this code is the device surface's name for
    /// it, so a phone branches on one word rather than on a prose message.
    #[must_use]
    pub fn device_not_found() -> Self {
        Self::new(
            StatusCode::NOT_FOUND,
            "device_not_found",
            "no device with that id exists on this daemon",
        )
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
    /// Where the listener binds. Loopback by default; a paired phone cannot reach
    /// loopback, so this is the setting that makes pairing possible off-host.
    bind: Arc<std::sync::Mutex<Vec<std::net::SocketAddr>>>,
    /// The daemon's MCP pool, when one was handed over at start-up.
    ///
    /// `/v1/mcp` reports health from here. A service built without a pool says
    /// so rather than guessing: a pooled daemon knows which servers started, and
    /// a bare service knows nothing, which is not the same as healthy.
    mcp: Option<Arc<crate::mcp::Pool>>,
    /// The dedicated sensor store (`sensors.db`), when it opened. `None` means
    /// the file could not be opened, and every sensor route answers 503 rather
    /// than pretending a write landed.
    sensors: Option<crate::sensors::SensorStore>,
    /// The inbox store (`inbox.db`), when it opened.
    inbox: Option<crate::inbox::InboxStore>,
    /// The notes store (`notes.db`), when it opened.
    notes: Option<crate::notes::NotesStore>,
    /// The push store (`push.db`), when it opened.
    push: Option<crate::push::PushStore>,
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
        let sensors = open_sensors(&engine);
        let inbox = open_inbox(&engine);
        let notes = open_notes(&engine);
        let push = open_push(&engine);
        Self {
            engine,
            hub,
            accepting: Arc::new(AtomicBool::new(true)),
            bound: Arc::new(AtomicBool::new(false)),
            bind: Arc::new(std::sync::Mutex::new(vec![
                DEVICE_ADDR
                    .parse()
                    .expect("the default is a literal ip:port"),
            ])),
            mcp: None,
            sensors,
            inbox,
            notes,
            push,
        }
    }

    /// Attach the daemon's MCP pool, so `/v1/mcp` can report observed health
    /// instead of "unknown" for everything. Called once by [`crate::admin`] while
    /// it builds its state; nothing else needs it.
    #[must_use]
    pub fn with_mcp(mut self, pool: Arc<crate::mcp::Pool>) -> Self {
        self.mcp = Some(pool);
        self
    }

    /// The MCP pool this service was handed, if any.
    #[must_use]
    pub fn mcp(&self) -> Option<&Arc<crate::mcp::Pool>> {
        self.mcp.as_ref()
    }

    /// Set the addresses the listener binds. Takes effect on the next `start`,
    /// which is to say on the next daemon start: a bind is not moved under a live
    /// phone.
    pub fn set_bind(&self, addresses: Vec<std::net::SocketAddr>) {
        if addresses.is_empty() {
            return;
        }
        if let Ok(mut bind) = self.bind.lock() {
            *bind = addresses;
        }
    }

    /// What the listener is currently told to bind.
    #[must_use]
    pub fn bind(&self) -> Vec<std::net::SocketAddr> {
        self.bind
            .lock()
            .map(|bind| bind.clone())
            .unwrap_or_default()
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
        let sensors = open_sensors(&engine);
        let inbox = open_inbox(&engine);
        let notes = open_notes(&engine);
        let push = open_push(&engine);
        Self {
            engine,
            hub,
            accepting: Arc::new(AtomicBool::new(true)),
            bound: Arc::new(AtomicBool::new(false)),
            bind: Arc::new(std::sync::Mutex::new(vec![
                DEVICE_ADDR
                    .parse()
                    .expect("the default is a literal ip:port"),
            ])),
            mcp: None,
            sensors,
            inbox,
            notes,
            push,
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

    /// The sensor store, when it opened at start-up.
    #[must_use]
    pub fn sensors(&self) -> Option<&crate::sensors::SensorStore> {
        self.sensors.as_ref()
    }

    /// The inbox store, when it opened at start-up.
    #[must_use]
    pub fn inbox(&self) -> Option<&crate::inbox::InboxStore> {
        self.inbox.as_ref()
    }

    /// The notes store, when it opened at start-up.
    #[must_use]
    pub fn notes(&self) -> Option<&crate::notes::NotesStore> {
        self.notes.as_ref()
    }

    /// The push store, when it opened at start-up.
    #[must_use]
    pub fn push(&self) -> Option<&crate::push::PushStore> {
        self.push.as_ref()
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
        let addresses = self.bind();
        handle.spawn(async move {
            // Every configured address is served at once, the same contract the
            // admin listener has. One failure does not take the others down.
            let mut first = addresses.clone();
            let rest: Vec<std::net::SocketAddr> = first.drain(1..).collect();
            for address in rest {
                let extra = service.clone();
                tokio::spawn(async move {
                    if let Err(error) = serve_address(address, extra).await {
                        tracing::error!(%error, %address, "device listener could not start");
                    }
                });
            }
            let first = addresses.first().copied().unwrap_or_else(|| {
                DEVICE_ADDR
                    .parse()
                    .expect("the default is a literal ip:port")
            });
            match serve_address(first, service.clone()).await {
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

/// Open the dedicated sensor store for this engine, or log and return `None`.
///
/// The sensor database lives beside `eris.db` in the same state directory. A
/// failure to open it is non-fatal: the daemon still serves turns, and every
/// sensor route answers 503 instead of pretending a write landed.
fn open_sensors(engine: &SetupEngine) -> Option<crate::sensors::SensorStore> {
    match crate::sensors::SensorStore::open(engine.paths().state_dir.join("sensors.db")) {
        Ok(store) => Some(store),
        Err(error) => {
            tracing::error!(%error, "sensor store unavailable; sensor routes will answer 503");
            None
        }
    }
}

/// Open the dedicated inbox store for this engine, or log and return `None`.
fn open_inbox(engine: &SetupEngine) -> Option<crate::inbox::InboxStore> {
    match crate::inbox::InboxStore::open(engine.paths().state_dir.join("inbox.db")) {
        Ok(store) => Some(store),
        Err(error) => {
            tracing::error!(%error, "inbox store unavailable; inbox routes will answer 503");
            None
        }
    }
}

/// Open the dedicated notes store for this engine, or log and return `None`.
fn open_notes(engine: &SetupEngine) -> Option<crate::notes::NotesStore> {
    match crate::notes::NotesStore::open(engine.paths().state_dir.join("notes.db")) {
        Ok(store) => Some(store),
        Err(error) => {
            tracing::error!(%error, "notes store unavailable; notes routes will answer 503");
            None
        }
    }
}

/// Open the dedicated push store for this engine, or log and return `None`.
fn open_push(engine: &SetupEngine) -> Option<crate::push::PushStore> {
    match crate::push::PushStore::open(engine.paths().state_dir.join("push.db")) {
        Ok(store) => Some(store),
        Err(error) => {
            tracing::error!(%error, "push store unavailable; push routes will answer 503");
            None
        }
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
        .route(ROUTE_VERSION, get(handlers::version))
        .route(
            ROUTE_PROJECTS,
            get(handlers::projects).post(handlers::create_project),
        )
        .route(ROUTE_PROJECTS_ASSIGN, post(handlers::assign_project))
        .route(
            ROUTE_CONVERSATIONS,
            get(handlers::list_conversations).post(handlers::create_conversation),
        )
        .route(ROUTE_CONVERSATION, get(handlers::conversation))
        .route(ROUTE_CONVERSATION_CLEAR, post(handlers::clear_conversation))
        .route(
            ROUTE_SETTINGS,
            get(handlers::settings).post(handlers::update_settings),
        )
        .route(ROUTE_APPROVALS, get(handlers::approvals))
        .route(ROUTE_APPROVAL_DECIDE, post(handlers::decide_approval))
        .route(ROUTE_MCP, get(handlers::mcp_servers))
        .route(ROUTE_DEVICES, get(handlers::devices))
        .route(ROUTE_DEVICE_REVOKE, post(handlers::revoke_device))
        .route(ROUTE_DEVICE_CODE, post(handlers::mint_device_code))
        .route(ROUTE_AUDIT, get(handlers::audit))
        .route(
            ROUTE_BACKUPS,
            get(handlers::backups).post(handlers::create_backup),
        )
        .route(ROUTE_BACKUP_RESTORE, post(handlers::restore_backup))
        .route(
            ROUTE_PERSONA,
            get(handlers::persona).post(handlers::update_persona),
        )
        .route(ROUTE_SENSOR_LOCATION, post(handlers::upload_location))
        .route(ROUTE_SENSOR_HEALTH, post(handlers::upload_health))
        .route(ROUTE_HEALTH_LATEST, get(handlers::health_latest))
        .route(ROUTE_LOCATION, get(handlers::location))
        .route(ROUTE_INBOX, get(handlers::inbox))
        .route(ROUTE_INBOX_ACTION, post(handlers::inbox_action))
        .route(
            ROUTE_INTERNAL_INBOX_CREATE,
            post(handlers::internal_inbox_create),
        )
        .route(
            ROUTE_NOTES,
            get(handlers::list_notes).post(handlers::create_note),
        )
        .route(
            ROUTE_NOTE,
            get(handlers::get_note)
                .patch(handlers::patch_note)
                .delete(handlers::delete_note),
        )
        .route(
            ROUTE_NOTE_BLOB,
            put(handlers::upload_note_blob).get(handlers::download_note_blob),
        )
        .route(ROUTE_NOTE_RECOGNITIONS, post(handlers::create_recognition))
        .route(ROUTE_NOTE_RUNS, post(handlers::create_note_run))
        .route(ROUTE_NOTE_RUN, get(handlers::get_note_run))
        .route(ROUTE_NOTE_RUN_EVENTS, get(handlers::note_run_events))
        .route(ROUTE_NOTE_RUN_CANCEL, post(handlers::cancel_note_run))
        .route(ROUTE_PUSH_REGISTER, post(handlers::register_push))
        .route(ROUTE_PUSH_DEACTIVATE, post(handlers::deactivate_push))
        .route(ROUTE_PUSH_SEND, post(handlers::send_push))
        .route(ROUTE_TALK_READINESS, get(handlers::talk_readiness))
        .route(ROUTE_TALK_SESSION, post(handlers::create_talk_session))
        .route(ROUTE_TALK_SESSION_END, post(handlers::end_talk_session))
        .route(ROUTE_TALK_TURNS, post(handlers::create_talk_turn))
        .route(ROUTE_TALK_INJECT, post(handlers::inject_talk_transcript))
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
pub async fn serve_address(
    addr: std::net::SocketAddr,
    service: DeviceService,
) -> std::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    let local = listener.local_addr()?;
    tracing::info!(%local, "device listener bound (/v1 only)");
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
            ROUTE_PROJECTS,
            ROUTE_PROJECTS_ASSIGN,
            ROUTE_CONVERSATIONS,
            ROUTE_CONVERSATION,
            ROUTE_CONVERSATION_CLEAR,
            ROUTE_SETTINGS,
            ROUTE_APPROVALS,
            ROUTE_APPROVAL_DECIDE,
            ROUTE_MCP,
            ROUTE_DEVICES,
            ROUTE_DEVICE_REVOKE,
            ROUTE_DEVICE_CODE,
            ROUTE_AUDIT,
            ROUTE_BACKUPS,
            ROUTE_BACKUP_RESTORE,
            ROUTE_PERSONA,
            ROUTE_SENSOR_LOCATION,
            ROUTE_SENSOR_HEALTH,
            ROUTE_HEALTH_LATEST,
            ROUTE_LOCATION,
            ROUTE_INBOX,
            ROUTE_INBOX_ACTION,
            ROUTE_NOTES,
            ROUTE_NOTE,
            ROUTE_NOTE_BLOB,
            ROUTE_NOTE_RECOGNITIONS,
            ROUTE_NOTE_RUNS,
            ROUTE_NOTE_RUN,
            ROUTE_NOTE_RUN_EVENTS,
            ROUTE_NOTE_RUN_CANCEL,
            ROUTE_PUSH_REGISTER,
            ROUTE_PUSH_DEACTIVATE,
            ROUTE_PUSH_SEND,
            ROUTE_TALK_READINESS,
            ROUTE_TALK_SESSION,
            ROUTE_TALK_SESSION_END,
            ROUTE_TALK_TURNS,
            ROUTE_TALK_INJECT,
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
