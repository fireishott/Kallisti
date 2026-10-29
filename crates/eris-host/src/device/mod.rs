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

pub mod attachments;
pub mod control;
pub mod gateway;
pub mod handlers;
pub mod legacy;
pub mod notify;
pub mod turns;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::http::{Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
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
/// `POST /v1/attachments` - upload one file, get an id back.
pub const ROUTE_ATTACHMENTS: &str = "/v1/attachments";

/// `GET /v1/attachments/{id}` - the bytes behind an id a transcript names.
pub const ROUTE_ATTACHMENT: &str = "/v1/attachments/{id}";

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

/// `GET /v1/turns/{id}/asked` - the question a running turn is waiting on,
/// behind the `chat` scope.
///
/// The question also rides the job's own stream as an `asked` frame, which is how
/// a client watching the turn learns about it. This route exists for the client
/// that was not watching: a phone that reconnected, or a page that opened the
/// conversation after the frame went past. Without it the only way to learn the
/// question was to have been listening at the moment it was asked.
pub const ROUTE_ASKED: &str = "/v1/turns/{id}/asked";

/// `POST /v1/turns/{id}/answer` - settle the question a turn is waiting on.
///
/// The resume half of `/v1/turns`. A `clarify` call parks the turn's blocking
/// thread until somebody answers, so without this route a phone turn that asked a
/// question could only wait out the hour and proceed on its own judgement. The
/// answer lands on the same registry the dashboard's route lands on, so a question
/// asked by a phone turn can be answered from either surface.
pub const ROUTE_ANSWER: &str = "/v1/turns/{id}/answer";

/// `GET /v1/subagents` - the subagents this daemon has run, newest first,
/// behind the `sessions` scope.
///
/// The dashboard draws these on its top bar; before this the phone could dispatch
/// a child and then had no way to read it, so the tool's own answer ("dispatched,
/// id ...") was the last the operator heard. The list is the bar's: anything still
/// working plus anything that finished recently, so it is a live view rather than
/// a growing log. A row older than that is still readable by its id.
pub const ROUTE_SUBAGENTS: &str = "/v1/subagents";

/// `GET /v1/subagents/{id}` - one row, whether or not it is still in the bar.
///
/// The read that makes a dispatch durable: a result that landed while the phone
/// was closed is still there to fetch, and a row the bar has aged out is not lost.
pub const ROUTE_SUBAGENT: &str = "/v1/subagents/{id}";

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

/// `POST /v1/settings/full-access` - the operator's own switch for full host
/// access, from the device that is allowed to flip it.
///
/// The dashboard has had this switch since it had an operator; the phone could
/// reach every other setting and not this one, so `sandbox=full` was a value a
/// phone could pick and never set - the write is refused until this switch is on.
/// The same guard applies here: enabling needs the typed word, disabling needs
/// nothing, and turning it off demotes a `full` sandbox rather than leaving a
/// stored mode that disagrees with the switch that allowed it.
pub const ROUTE_FULL_ACCESS: &str = "/v1/settings/full-access";

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

/// `GET` and `POST /v1/models` - the probe's model list, and the pin.
pub const ROUTE_MODELS: &str = "/v1/models";

/// `GET` and `POST /v1/cron` - the schedule list, and a new schedule.
pub const ROUTE_CRON: &str = "/v1/cron";

/// `PATCH` and `DELETE /v1/cron/{id}` - edit or delete one schedule.
pub const ROUTE_CRON_ITEM: &str = "/v1/cron/{id}";

/// `GET /v1/skills` - the skill library as the model catalog sees it.
pub const ROUTE_SKILLS: &str = "/v1/skills";

/// `GET /v1/skills/{key}` - one skill's body.
pub const ROUTE_SKILL_ITEM: &str = "/v1/skills/{key}";

/// `GET` and `POST /v1/aux` - the auxiliary role pins, and one role's write.
pub const ROUTE_AUX: &str = "/v1/aux";

/// `GET /v1/gw/health` - the daemon's own state, in the legacy client's words.
pub const ROUTE_GW_HEALTH: &str = "/v1/gw/health";

/// `GET /v1/gw/restart/preflight` - the snapshot a restart confirmation shows.
pub const ROUTE_GW_PREFLIGHT: &str = "/v1/gw/restart/preflight";

/// `POST /v1/gw/restart` - schedule a self-restart.
pub const ROUTE_GW_RESTART: &str = "/v1/gw/restart";

/// `GET /v1/gw/update/check` - what this daemon is, and what is staged next.
pub const ROUTE_GW_UPDATE_CHECK: &str = "/v1/gw/update/check";

/// The legacy-parity route paths, in one block.
///
/// Each one exists because a shipped client asks for it and this listener used to
/// answer a typed 404, which the client renders as a failed decode. They are named
/// here, separately from the paths the current build uses, so the compatibility
/// surface is one block of the wiring rather than a scattering.
///
/// `POST /v1/hosts/current/revoke` is in this list and answers `403
/// host_revoke_unsupported` on purpose: see [`legacy::hosts_current_revoke`].
mod legacy_routes {
    /// `GET /v1/health` - the liveness probe.
    pub const HEALTH: &str = "/v1/health";
    /// `GET` and `PUT /v1/config` - the Config Editor's read and write pair.
    pub const CONFIG: &str = "/v1/config";
    /// `POST /v1/config/validate` - the same parse, writing nothing.
    pub const CONFIG_VALIDATE: &str = "/v1/config/validate";
    /// `GET /v1/gw/status` - the gateway snapshot the status screen reads.
    pub const GW_STATUS: &str = "/v1/gw/status";
    /// `GET /v1/gw/logs` - a bounded tail of this daemon's journal.
    pub const GW_LOGS: &str = "/v1/gw/logs";
    /// `GET /v1/gw/logs/stream` - the same rows, followed.
    pub const GW_LOGS_STREAM: &str = "/v1/gw/logs/stream";
    /// `POST /v1/auth/refresh` - the legacy spelling of `/v1/token/refresh`.
    pub const AUTH_REFRESH: &str = "/v1/auth/refresh";
    /// `POST /v1/auth/revoke` - revoke the calling device.
    pub const AUTH_REVOKE: &str = "/v1/auth/revoke";
    /// `POST /v1/device/register` - the presenting device's own row, stamped.
    pub const DEVICE_REGISTER: &str = "/v1/device/register";
    /// `GET` and `POST /v1/device/app-state` - the daemon's own state row.
    pub const DEVICE_APP_STATE: &str = "/v1/device/app-state";
    /// `GET /v1/hosts/current` - this host, because this host is the only one.
    pub const HOSTS_CURRENT: &str = "/v1/hosts/current";
    /// `POST /v1/hosts/current/revoke` - refused: there is no host to revoke.
    pub const HOSTS_CURRENT_REVOKE: &str = "/v1/hosts/current/revoke";
    /// `GET /v1/connector/events` - the daemon's own health, pushed.
    pub const CONNECTOR_EVENTS: &str = "/v1/connector/events";
    /// `GET /v1/profiles` - the one identity, with its real skill count.
    pub const PROFILES: &str = "/v1/profiles";
    /// `GET /v1/commands` - the command table and the skill library.
    pub const COMMANDS: &str = "/v1/commands";
    /// `GET /v1/canvas/processes/stream` - the subagent rows, as SSE.
    pub const CANVAS_PROCESSES: &str = "/v1/canvas/processes/stream";
}

/// Every legacy-parity path, for the test that keeps them all under `/v1`.
pub const LEGACY_ROUTES: [&str; 16] = [
    legacy_routes::HEALTH,
    legacy_routes::CONFIG,
    legacy_routes::CONFIG_VALIDATE,
    legacy_routes::GW_STATUS,
    legacy_routes::GW_LOGS,
    legacy_routes::GW_LOGS_STREAM,
    legacy_routes::AUTH_REFRESH,
    legacy_routes::AUTH_REVOKE,
    legacy_routes::DEVICE_REGISTER,
    legacy_routes::DEVICE_APP_STATE,
    legacy_routes::HOSTS_CURRENT,
    legacy_routes::HOSTS_CURRENT_REVOKE,
    legacy_routes::CONNECTOR_EVENTS,
    legacy_routes::PROFILES,
    legacy_routes::COMMANDS,
    legacy_routes::CANVAS_PROCESSES,
];

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
        // A 4xx is the caller's mistake, not a daemon fault: a wrong pairing
        // code must read as a refused request in the journal, never as an
        // outage. The code is carried because it is the word a client branches
        // on; the message and the request body are not logged, since both can
        // carry a token or a pairing code. The body sent to the client is
        // unchanged.
        if self.status.is_client_error() {
            tracing::warn!(
                status = self.status.as_u16(),
                code = self.code,
                "device request refused"
            );
        }
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
    /// The daemon's parked-question registry, when one was handed over.
    ///
    /// Shared with the dashboard on purpose: a question asked by a phone turn is
    /// answerable from the browser and the other way round, because both routes
    /// settle the same in-process row. `None` in a service a test built for another
    /// purpose, which is what makes a device turn refuse to ask rather than park a
    /// question no route can reach.
    clarify: Option<Arc<crate::web::clarify::Registry>>,
    /// The APNs sender, when this host is configured to deliver push.
    ///
    /// `None` is a real answer, not a missing wiring: no key in the environment
    /// means the send route keeps its honest 503. Half-configured is refused at
    /// start-up by [`crate::apns::ApnsSender::from_env`] and never reaches here.
    apns: Option<Arc<crate::apns::ApnsSender>>,
    /// Why APNs is not usable, when somebody configured it wrongly.
    ///
    /// Distinct from absent: `None` here and `None` above means nobody asked for
    /// push, which the send route answers with 503 `apns_not_configured`. A
    /// reason here means somebody did ask and got a variable wrong, and the
    /// route says so instead of pretending the feature was never configured.
    apns_fault: Option<String>,
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
        if let Some(store) = &push {
            // The worker needs the same store the routes use: a finished turn
            // looks up its own device's registration to announce the reply.
            hub.set_push(store.clone());
        }
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
            clarify: None,
            apns: None,
            apns_fault: None,
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

    /// Attach the web dashboard's live table, so a device turn that names a
    /// conversation is visible there for as long as it runs. Called once by
    /// [`crate::admin`] while it builds its state.
    #[must_use]
    pub fn with_live(self, live: crate::web::live::LiveHub) -> Self {
        self.hub.set_live(live);
        self
    }

    /// Attach the daemon's parked-question registry, so a device turn can ask and
    /// a `/v1` route can answer. Called once by [`crate::admin`] while it builds
    /// its state, with the same registry the dashboard holds.
    #[must_use]
    pub fn with_clarify(mut self, clarify: Arc<crate::web::clarify::Registry>) -> Self {
        self.hub.set_clarify(Arc::clone(&clarify));
        self.clarify = Some(clarify);
        self
    }

    /// The parked-question registry, when this service was handed one.
    #[must_use]
    pub fn clarify(&self) -> Option<&Arc<crate::web::clarify::Registry>> {
        self.clarify.as_ref()
    }

    /// Attach the APNs sender, so `/v1/push/send` delivers instead of answering
    /// 503. Called once by [`crate::admin`] after it resolved the environment.
    #[must_use]
    pub fn with_apns(mut self, apns: Arc<crate::apns::ApnsSender>) -> Self {
        // The hub holds the same sender: a finished turn announces itself from
        // the worker, not from a route, so the sender has to be reachable there.
        self.hub.set_apns(Arc::clone(&apns));
        self.apns = Some(apns);
        self
    }

    /// The APNs sender, when this host is configured to deliver push.
    #[must_use]
    pub fn apns(&self) -> Option<&Arc<crate::apns::ApnsSender>> {
        self.apns.as_ref()
    }

    /// Record why APNs could not be built, so the send route can say so.
    #[must_use]
    pub fn with_apns_fault(mut self, reason: String) -> Self {
        self.apns_fault = Some(reason);
        self
    }

    /// The reason APNs is unusable, when it was configured wrongly.
    #[must_use]
    pub fn apns_fault(&self) -> Option<&str> {
        self.apns_fault.as_deref()
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
            clarify: None,
            apns: None,
            apns_fault: None,
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
        .route(ROUTE_ASKED, get(handlers::asked))
        .route(ROUTE_ANSWER, post(handlers::answer_asked))
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
        .route(ROUTE_ATTACHMENTS, post(attachments::upload))
        .route(ROUTE_ATTACHMENT, get(attachments::bytes))
        .route(ROUTE_CONVERSATION, get(handlers::conversation))
        .route(ROUTE_CONVERSATION_CLEAR, post(handlers::clear_conversation))
        .route(
            ROUTE_SETTINGS,
            get(handlers::settings).post(handlers::update_settings),
        )
        .route(ROUTE_SUBAGENTS, get(handlers::subagents))
        .route(ROUTE_SUBAGENT, get(handlers::subagent))
        .route(ROUTE_APPROVALS, get(handlers::approvals))
        .route(ROUTE_APPROVAL_DECIDE, post(handlers::decide_approval))
        .route(ROUTE_MCP, get(handlers::mcp_servers))
        .route(ROUTE_DEVICES, get(handlers::devices))
        .route(ROUTE_DEVICE_REVOKE, post(handlers::revoke_device))
        .route(ROUTE_DEVICE_CODE, post(handlers::mint_device_code))
        .route(ROUTE_FULL_ACCESS, post(handlers::set_full_access))
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
        .route(ROUTE_MODELS, get(control::models).post(control::set_model))
        .route(ROUTE_CRON, get(control::cron).post(control::create_cron))
        .route(
            ROUTE_CRON_ITEM,
            patch(control::patch_cron).delete(control::delete_cron),
        )
        .route(ROUTE_SKILLS, get(control::skills_list))
        .route(ROUTE_SKILL_ITEM, get(control::skill_detail))
        .route(ROUTE_AUX, get(control::aux_roles).post(control::set_aux))
        .route(ROUTE_GW_HEALTH, get(gateway::gw_health))
        .route(ROUTE_GW_PREFLIGHT, get(gateway::gw_restart_preflight))
        .route(ROUTE_GW_RESTART, post(gateway::gw_restart))
        .route(ROUTE_GW_UPDATE_CHECK, get(gateway::gw_update_check))
        .route(legacy_routes::HEALTH, get(legacy::health))
        .route(
            legacy_routes::CONFIG,
            get(legacy::config_read).put(legacy::config_write),
        )
        .route(
            legacy_routes::CONFIG_VALIDATE,
            post(legacy::config_validate),
        )
        .route(legacy_routes::GW_STATUS, get(legacy::gw_status))
        .route(legacy_routes::GW_LOGS, get(legacy::gw_logs))
        .route(legacy_routes::GW_LOGS_STREAM, get(legacy::gw_logs_stream))
        .route(legacy_routes::AUTH_REFRESH, post(legacy::auth_refresh))
        .route(legacy_routes::AUTH_REVOKE, post(legacy::auth_revoke))
        .route(
            legacy_routes::DEVICE_REGISTER,
            post(legacy::device_register),
        )
        .route(
            legacy_routes::DEVICE_APP_STATE,
            get(legacy::app_state_get).post(legacy::app_state_post),
        )
        .route(legacy_routes::HOSTS_CURRENT, get(legacy::hosts_current))
        .route(
            legacy_routes::HOSTS_CURRENT_REVOKE,
            post(legacy::hosts_current_revoke),
        )
        .route(
            legacy_routes::CONNECTOR_EVENTS,
            get(legacy::connector_events),
        )
        .route(legacy_routes::PROFILES, get(legacy::profiles))
        .route(legacy_routes::COMMANDS, get(legacy::commands))
        .route(
            legacy_routes::CANVAS_PROCESSES,
            get(legacy::canvas_processes),
        )
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
///
/// The refusal is logged with the path exactly as it arrived, because an
/// unlogged 404 is nearly impossible to diagnose from the client side: a phone
/// that asks for `/v1/v1/pair` should leave a line the operator can grep for,
/// with the doubled prefix visible in it.
async fn not_here(method: Method, uri: Uri) -> DeviceError {
    tracing::warn!(
        http_method = %method,
        path = %uri.path(),
        "unknown device route: this listener serves /v1 only"
    );
    DeviceError::new(
        StatusCode::NOT_FOUND,
        "not_found",
        format!(
            "{} is not a route on the device listener: this surface serves /v1 only",
            uri.path()
        ),
    )
}

/// Known path, wrong method. Logged for the same reason as [`not_here`].
async fn wrong_method(method: Method, uri: Uri) -> DeviceError {
    tracing::warn!(
        http_method = %method,
        path = %uri.path(),
        "device route does not accept that method"
    );
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
            ROUTE_FULL_ACCESS,
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
            ROUTE_MODELS,
            ROUTE_CRON,
            ROUTE_CRON_ITEM,
            ROUTE_SKILLS,
            ROUTE_SKILL_ITEM,
            ROUTE_AUX,
            ROUTE_GW_HEALTH,
            ROUTE_GW_PREFLIGHT,
            ROUTE_GW_RESTART,
            ROUTE_GW_UPDATE_CHECK,
        ]
        .into_iter()
        .chain(LEGACY_ROUTES)
        {
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

    #[tokio::test]
    async fn a_refusal_keeps_its_body_byte_for_byte() {
        // Logging a 4xx must not change what the client parses.
        let response = DeviceError::unauthorized("pairing_code_invalid", "nope").into_response();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let bytes = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .expect("body");
        assert_eq!(
            String::from_utf8(bytes.to_vec()).expect("utf8"),
            r#"{"ok":false,"code":"pairing_code_invalid","message":"nope"}"#
        );
    }
}
