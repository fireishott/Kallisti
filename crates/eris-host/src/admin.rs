//! The admin listener: a list of addresses, loopback by default.
//!
//! [`ADMIN_ADDR`] is still the default and still loopback, but it is no longer
//! the only possible bind. `admin.listen` in `config.toml` holds one or more
//! `ip:port` literals, every one of them is served simultaneously, and
//! `serve --admin-listen <ip:port>` (repeatable) adds to that list for a one-off
//! run. `admin/listen.rs` owns the rules; nothing in this file is specific to
//! Tailscale, to a LAN, or to a reverse proxy.
//!
//! Three layers guard the whole surface, in this order:
//!
//! 1. **request id** (outermost) - every response, including the 403 and the 409,
//!    carries `x-request-id`;
//! 2. **client context** ([`crate::web::proxy::client_context_middleware`]) -
//!    forwarding headers are believed only from a peer inside
//!    `admin.trusted_proxies` (loopback by default, so Tailscale Serve, nginx and
//!    Caddy work with no config). A peer outside the list that sends them is a
//!    403 `not_loopback`, because that is a spoofing attempt;
//! 3. **the ready gate** on the dashboard - `409 not_ready` until onboarding
//!    finishes.
//!
//! `/healthz`, `/`, `/setup`, `/setup/*` and `/static/*` are the only paths that
//! answer before `ready`, and only `/setup` renders without an admin session.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

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
use crate::persona::PersonaConfig;
use crate::web::css::CSS_ROUTE;
use crate::web::proxy::{AdminAccess, client_context_middleware};
use crate::web::routes::{dashboard_routes, setup_routes};
use crate::web::templates::Chrome;

pub mod listen;

/// The default admin listener address: loopback, and what an empty
/// `admin.listen` means. It is the first bind unless the config says otherwise;
/// it is no longer the only one the daemon can make.
pub const ADMIN_ADDR: &str = "127.0.0.1:7340";

/// The version every response reports. Mirrors `[workspace.package]`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The build number inside this version.
///
/// The marketing version moves when behaviour changes shape; this number moves
/// on every ship, so a fix can land without telling the world the product is a
/// different version. It rides in the footer, `--version`, and the wallpaper
/// fingerprint, which is what makes a rebuild visible to a cached browser.
pub const BUILD: u32 = 99;

/// `1.1.0 build 24`, for the places that state the whole identity.
#[must_use]
pub fn version_line() -> String {
    format!("{VERSION} build {BUILD}")
}

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

/// `GET /dashboard/chat` - the conversation surface: transcript, composer,
/// wallpaper.
pub const ROUTE_CHAT: &str = "/dashboard/chat";

/// `POST /dashboard/chat/send` - one turn, streamed when the caller asks for an
/// event stream and rendered on the next page when it does not.
pub const ROUTE_CHAT_SEND: &str = "/dashboard/chat/send";

/// `POST /dashboard/chat/mode` - the composer's sandbox-mode picker. One field,
/// so the quick switch does not have to submit the whole permissions form.
pub const ROUTE_CHAT_MODE: &str = "/dashboard/chat/mode";

/// `POST /dashboard/chat/clear` - empty the transcript.
pub const ROUTE_CHAT_CLEAR: &str = "/dashboard/chat/clear";

/// `GET /dashboard/chat/{id}` - one conversation.
pub const ROUTE_CHAT_ID: &str = "/dashboard/chat/{id}";

/// `GET /dashboard/chat/sessions.json` - the rail's list, for a page that stays
/// open while conversations change underneath it.
pub const ROUTE_CHAT_SESSIONS: &str = "/dashboard/chat/sessions.json";

/// `GET /dashboard/chat/live` - watch the turn running in one conversation.
///
/// The one dashboard route that answers with a stream rather than a page: the
/// page that asked for a turn is reading the response it posted, and this is how
/// a page that was never there - a reload, a tab that came back, a second screen -
/// reads the same turn.
pub const ROUTE_CHAT_LIVE: &str = "/dashboard/chat/live";

/// One attached file, by the id the transcript carries. Session-gated like every
/// other dashboard route: an upload is the operator's, not the world's.
pub const ROUTE_CHAT_FILE: &str = "/dashboard/chat/file/{id}";

/// `GET /dashboard/chat/workspace/{*path}` - one file a turn talked about.
pub const ROUTE_CHAT_WORKSPACE: &str = "/dashboard/chat/workspace/{*path}";

/// `POST /dashboard/chat/new` - start a conversation.
pub const ROUTE_CHAT_NEW: &str = "/dashboard/chat/new";

/// `POST /dashboard/chat/{id}/delete` - drop a conversation.
pub const ROUTE_CHAT_DELETE: &str = "/dashboard/chat/{id}/delete";

/// `POST /dashboard/projects` - start a project, from the rail.
pub const ROUTE_PROJECTS: &str = "/dashboard/projects";

/// `POST /dashboard/projects/assign` - file one conversation under a project, or
/// take it out of one. The form's empty `project` value means "no project".
pub const ROUTE_PROJECTS_ASSIGN: &str = "/dashboard/projects/assign";

/// `GET /dashboard/settings` - the three settings forms.
pub const ROUTE_SETTINGS: &str = "/dashboard/settings";

/// `POST /dashboard/settings/provider` - endpoint, key reference, model.
pub const ROUTE_SETTINGS_PROVIDER: &str = "/dashboard/settings/provider";

/// `POST /dashboard/settings/workspace` - the sandbox workspace path.
pub const ROUTE_SETTINGS_WORKSPACE: &str = "/dashboard/settings/workspace";

/// `POST /dashboard/settings/permissions` - the permission tuple.
pub const ROUTE_SETTINGS_PERMISSIONS: &str = "/dashboard/settings/permissions";

/// `POST /dashboard/settings/persona` - the assistant's identity.
pub const ROUTE_SETTINGS_PERSONA: &str = "/dashboard/settings/persona";

/// `POST /dashboard/settings/reasoning` - show or hide the thinking bubble.
pub const ROUTE_SETTINGS_REASONING: &str = "/dashboard/settings/reasoning";
/// `POST /dashboard/settings/streaming` - stream answers to the browser or not.
pub const ROUTE_SETTINGS_STREAMING: &str = "/dashboard/settings/streaming";
/// `POST /dashboard/settings/quota` - show the gateway's quota under each answer.
pub const ROUTE_SETTINGS_QUOTA: &str = "/dashboard/settings/quota";
/// Where the sign-in credential is changed. A page of its own because the
/// settings page is forbidden from containing a password input.
pub const ROUTE_SECURITY: &str = "/dashboard/security";
/// Where the device listener's bind addresses are set.
pub const ROUTE_SETTINGS_DEVICE: &str = "/dashboard/settings/device";
/// Where the memory backend is chosen.
pub const ROUTE_SETTINGS_MEMORY: &str = "/dashboard/settings/memory";

/// `POST /dashboard/settings/timezone` - the clock every stamp is shown in.
pub const ROUTE_SETTINGS_TIMEZONE: &str = "/dashboard/settings/timezone";

/// `POST /dashboard/settings/tools` - tool calling on or off, and its budget.
pub const ROUTE_SETTINGS_TOOLS: &str = "/dashboard/settings/tools";

/// `POST /dashboard/settings/providers` - the provider registry and its primary.
pub const ROUTE_SETTINGS_PROVIDERS: &str = "/dashboard/settings/providers";

/// `POST /dashboard/settings/aux` - the auxiliary model roles.
pub const ROUTE_SETTINGS_AUX: &str = "/dashboard/settings/aux";

/// `POST /dashboard/settings/mcp` - the configured MCP server list.
pub const ROUTE_SETTINGS_MCP: &str = "/dashboard/settings/mcp";

/// `POST /dashboard/settings/greeting` - the generated line on an empty chat page.
pub const ROUTE_SETTINGS_GREETING: &str = "/dashboard/settings/greeting";
/// `POST /dashboard/settings/schedules` - create, toggle and delete schedules.
pub const ROUTE_SETTINGS_SCHEDULES: &str = "/dashboard/settings/schedules";

/// `POST /dashboard/settings/skills` - the skill library switch, directory and rescan.
pub const ROUTE_SETTINGS_SKILLS: &str = "/dashboard/settings/skills";

/// `POST /dashboard/sidebar` - show or hide the conversation rail.
pub const ROUTE_SIDEBAR: &str = "/dashboard/sidebar";

/// The `settings` row that remembers whether the rail is collapsed.
pub const SETTING_RAIL_HIDDEN: &str = "display.rail_hidden";

/// `POST /dashboard/settings/full-access` - the operator's own switch for full
/// host sandbox mode, behind a typed confirmation.
pub const ROUTE_SETTINGS_FULL_ACCESS: &str = "/dashboard/settings/full-access";

/// `GET /dashboard/activity` - the audit trail, newest first.
pub const ROUTE_ACTIVITY: &str = "/dashboard/activity";

/// `GET /dashboard/sessions` - every device session, newest first.
pub const ROUTE_SESSIONS: &str = "/dashboard/sessions";

/// `GET /dashboard/devices` and `POST /dashboard/devices` - the device table and
/// the one form that mints a pairing code.
pub const ROUTE_DEVICES: &str = "/dashboard/devices";

/// `POST /dashboard/device/remove` - the bulk action on the device table. One
/// route, three verbs: revoke the selected rows, delete the selected rows, or
/// delete every row that is already revoked.
pub const ROUTE_DEVICE_REMOVE: &str = "/dashboard/device/remove";

/// `GET /dashboard/backups` - every archive on this host, whether each one
/// verifies, and the two forms: create one, restore one.
pub const ROUTE_BACKUPS: &str = "/dashboard/backups";

/// `POST /dashboard/backups/create` - write one archive into the archive
/// directory.
pub const ROUTE_BACKUPS_CREATE: &str = "/dashboard/backups/create";

/// `POST /dashboard/backups/restore` - replace the live state from one archive.
/// Destructive, so it takes the filename twice and never a path.
pub const ROUTE_BACKUPS_RESTORE: &str = "/dashboard/backups/restore";

/// One session's page. Built by [`session_path`] so the route and every link to
/// it come from one place.
pub const ROUTE_SESSION: &str = "/dashboard/sessions/{id}";

/// One job's cancel route, built by [`job_cancel_path`].
pub const ROUTE_JOB_CANCEL: &str = "/dashboard/jobs/{id}/cancel";

/// The path of one session's detail page.
#[must_use]
pub fn session_path(id: &str) -> String {
    format!("{ROUTE_SESSIONS}/{id}")
}

/// The path that cancels one job.
#[must_use]
pub fn job_cancel_path(id: &str) -> String {
    format!("/dashboard/jobs/{id}/cancel")
}

/// `GET /login` and `POST /login` - the way a browser gets a session once the
/// wizard is closed. Sits behind the ready gate with the dashboard, because
/// before `ready` the wizard is the surface that owns the credential.
pub const ROUTE_LOGIN: &str = "/login";

/// Sign out: clears the session cookie and returns to the sign-in form. The only
/// way a signed-in browser can reach the form again, which is also how the
/// operator gets back to the "keep me signed in" checkbox.
pub const ROUTE_LOGOUT: &str = "/logout";

/// Every GET route the listener serves, including the ones that need a session
/// or a finished setup. The phase 7 test walks this list.
pub const ALL_GET_ROUTES: &[&str] = &[
    ROUTE_ROOT,
    ROUTE_HEALTHZ,
    ROUTE_SETUP,
    ROUTE_SETUP_STATE,
    ROUTE_LOGIN,
    ROUTE_DASHBOARD,
    ROUTE_SESSIONS,
    ROUTE_DEVICES,
    ROUTE_SETTINGS,
    ROUTE_ACTIVITY,
    ROUTE_BACKUPS,
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
    ROUTE_LOGOUT,
    ROUTE_SETTINGS_PROVIDER,
    ROUTE_SETTINGS_WORKSPACE,
    ROUTE_SETTINGS_PERMISSIONS,
    ROUTE_SETTINGS_PERSONA,
    ROUTE_SETTINGS_REASONING,
    ROUTE_SETTINGS_STREAMING,
    ROUTE_SETTINGS_QUOTA,
    ROUTE_SETTINGS_MEMORY,
    ROUTE_SECURITY,
    ROUTE_SETTINGS_DEVICE,
    ROUTE_SETTINGS_TIMEZONE,
    ROUTE_SETTINGS_TOOLS,
    ROUTE_SETTINGS_PROVIDERS,
    ROUTE_SETTINGS_AUX,
    ROUTE_SETTINGS_MCP,
    ROUTE_SETTINGS_GREETING,
    ROUTE_SETTINGS_SCHEDULES,
    ROUTE_SETTINGS_SKILLS,
    ROUTE_SIDEBAR,
    "/dashboard/provider/rotate",
    "/dashboard/device/revoke",
    ROUTE_DEVICE_REMOVE,
    "/dashboard/mode",
    ROUTE_DEVICES,
    // The two backup posts. Neither carries a path: the restore form names an
    // archive in the archive directory, or it is refused.
    ROUTE_BACKUPS_CREATE,
    ROUTE_BACKUPS_RESTORE,
    // One literal cancel path, because the route list is a static slice and the
    // real path carries a job id. The phase 7 test only needs one path that
    // answers without a session.
    "/dashboard/jobs/job-id/cancel",
];

/// Shared handler state.
#[derive(Clone)]
pub struct AppState {
    /// The onboarding engine (and the database underneath it).
    pub engine: SetupEngine,
    /// The configured MCP servers, started on demand.
    pub mcp: Arc<crate::mcp::Pool>,
    /// The admin cookie sessions.
    pub sessions: AdminSessions,
    /// Claim attempt limiter, in process on purpose: it guards one form, so it
    /// counts attempts per effective client rather than per proxy.
    pub limiter: Arc<ClaimLimiter>,
    /// True when the process was started with `--allow-full`.
    ///
    /// The command line is the override; [`AppState::allow_full`] is the answer
    /// the UI and the validator should use, because it also honours the stored
    /// switch an operator flipped from the dashboard.
    pub allow_full: bool,
    /// Which conversation is running a turn, and what has been said to it.
    ///
    /// In process on purpose and shared by every handler: a steer is a message to
    /// a thread that is already running in this process, so the queue is the same
    /// kind of thing as the approval queue's own in-memory bookkeeping rather than
    /// a row. A restart ends the turns, and with them the steers waiting for them.
    pub steering: crate::web::chat::Steering,
    /// The turns this process is running, by conversation.
    ///
    /// In process for the same reason as the steering queue: a turn is a task, and
    /// a page that wants to watch one is asking this process a question. It is
    /// what tells a reloaded page that a turn is still running - and what tells a
    /// row that claims to be streaming that nothing is writing it any more.
    pub live: crate::web::live::LiveHub,
    /// The device listener's service. It binds nothing until `start` is called:
    /// at boot when the flag or the setting asks for it, and from the dashboard
    /// toggle afterwards.
    pub device: DeviceService,
    /// Who may front the listener, and what it is called in public.
    pub access: Arc<AdminAccess>,
    /// The assistant's identity: where the file is, and whether it is on.
    /// Resolved from `[persona]` in `config.toml` plus the paths a run chose.
    pub persona: PersonaConfig,
    /// The addresses actually bound, in bind order. Written once by `serve`
    /// before anything serves, read by the pages that report the truth.
    listeners: Arc<RwLock<Vec<SocketAddr>>>,
}

impl AppState {
    /// Build state around an engine. `allow_full` comes from the command line.
    #[must_use]
    pub fn new(engine: SetupEngine, allow_full: bool) -> Self {
        let db = engine.db().clone();
        let persona = PersonaConfig::at_path(engine.paths().persona_file());
        // Read once at construction: the list only changes through a settings
        // save, and a turn should not pay for a database read per tool call.
        let servers = match engine.setting(crate::mcp::SETTING_SERVERS) {
            Ok(Some(raw)) => match crate::mcp::parse(&raw) {
                Ok(servers) => servers,
                Err(error) => {
                    eprintln!("eris: the MCP server list is invalid: {}", error.message);
                    Vec::new()
                }
            },
            Ok(None) => Vec::new(),
            Err(error) => {
                eprintln!("eris: cannot read the MCP server list: {error}");
                Vec::new()
            }
        };
        if !servers.is_empty() {
            eprintln!("eris: {} MCP server(s) configured", servers.len());
        }
        let mcp = Arc::new(crate::mcp::Pool::new(servers));
        // The web and device surfaces share one table of running turns, so a page
        // that asks about a conversation is told the truth whichever surface
        // started the turn in it.
        let live = crate::web::live::LiveHub::new();
        // The device surface answers `GET /v1/mcp` from the same pool the turns
        // use, so a phone sees the health this process observed rather than a
        // guess. Same process, same pool: one daemon, one answer.
        let device = DeviceService::new(engine.clone(), TurnStore::buffer_from_env())
            .with_mcp(Arc::clone(&mcp))
            .with_live(live.clone());
        Self {
            engine,
            mcp,
            sessions: AdminSessions::new(db),
            limiter: Arc::new(ClaimLimiter::default()),
            allow_full,
            steering: crate::web::chat::Steering::new(),
            live,
            device,
            access: Arc::new(AdminAccess::loopback_only()),
            persona,
            listeners: Arc::new(RwLock::new(vec![
                ADMIN_ADDR
                    .parse()
                    .expect("the default is a literal ip:port"),
            ])),
        }
    }

    /// Apply the `[persona]` table: the file the dashboard reads and writes, and
    /// the default on/off state. `serve` calls this once, before anything binds.
    #[must_use]
    pub fn with_persona(mut self, persona: PersonaConfig) -> Self {
        self.device.set_persona(persona.clone());
        self.persona = persona;
        self
    }

    /// Attach the access model the config resolved.
    #[must_use]
    pub fn with_access(mut self, access: AdminAccess) -> Self {
        self.access = Arc::new(access);
        self
    }

    /// Record the addresses that actually bound. Called by `serve_listeners`
    /// before the first request is answered.
    pub fn set_bound_listeners(&self, listeners: Vec<SocketAddr>) {
        if let Ok(mut slot) = self.listeners.write() {
            *slot = listeners;
        }
    }

    /// The bound addresses, for the pages that report them.
    #[must_use]
    pub fn bound_listeners(&self) -> Vec<SocketAddr> {
        self.listeners
            .read()
            .map(|slot| slot.clone())
            .unwrap_or_else(|_| vec![ADMIN_ADDR.parse().expect("the default is a literal")])
    }

    /// What every page needs to state the listener truth in its shell.
    #[must_use]
    pub fn chrome(&self) -> Chrome {
        Chrome {
            listeners: self.bound_listeners(),
            public_url: self.access.public_url.clone(),
            wallpaper_url: self.wallpaper_url(),
            projects: self.rail_projects(),
            sessions: self.rail_sessions(),
            current_conversation: None,
            rail_hidden: self.rail_hidden(),
            subagents: self.bar_subagents(),
        }
    }

    /// The subagents the top bar reports: anything still working, plus anything
    /// that finished recently.
    ///
    /// Empty when the table cannot be read, for the same reason the session list
    /// is: a bar that cannot say is a bar with nothing on it, not a failed page.
    fn bar_subagents(&self) -> Vec<crate::subagents::Subagent> {
        crate::subagents::bar(self.engine.db(), crate::subagents::BAR_LIMIT).unwrap_or_default()
    }

    /// Every project the rail can head a group with, by name.
    ///
    /// Empty when the table cannot be read, for the same reason the session list
    /// is: a rail that cannot offer groups is a rail with one heading, not a
    /// failed page.
    fn rail_projects(&self) -> Vec<crate::projects::Project> {
        crate::projects::list(self.engine.db()).unwrap_or_default()
    }

    /// The conversations the rail lists: newest first, bounded.
    ///
    /// Read here rather than in a handler so every page that renders the rail
    /// shows the same list. A database that cannot answer is an empty rail, not a
    /// failed page: the chat surface still opens, it just cannot offer history.
    fn rail_sessions(&self) -> Vec<crate::web::templates::ChatSession> {
        use crate::web::chat;
        use crate::web::templates::ChatSession;
        // The rail reads the same clock as the transcript. Two lists of the same
        // timestamps in two zones is how a product looks broken while both are
        // individually correct.
        let zone = crate::zone::Zone::parse(
            self.engine
                .setting(crate::zone::SETTING_TIMEZONE)
                .ok()
                .flatten()
                .as_deref(),
        );
        let offset = crate::zone::offset_minutes(self.engine.db(), zone);
        let Ok(rows) = chat::conversations(self.engine.db(), chat::SESSION_LIMIT) else {
            return Vec::new();
        };
        // One project read for the whole rail, not one per conversation: the name
        // a heading shows is resolved here so the template never has to look it
        // up, and a conversation filed under a project that is gone is grouped
        // with the unfiled ones rather than vanishing from the rail.
        let projects = self.rail_projects();
        let name_of = |id: &str| {
            projects
                .iter()
                .find(|project| project.id == id)
                .map(|project| project.name.clone())
        };
        rows.into_iter()
            .map(|row| {
                let project_name = row.project_id.as_deref().and_then(name_of);
                // A dangling id is not a group: the rail files it with the
                // conversations that have no project at all.
                let project_id = if project_name.is_some() {
                    row.project_id.clone()
                } else {
                    None
                };
                ChatSession {
                    id: row.id,
                    title: row.title,
                    stamp: crate::zone::clock_in(&row.updated_at, offset),
                    project_id,
                    project_name,
                }
            })
            .collect()
    }

    /// Whether the operator collapsed the rail. Absent means shown.
    #[must_use]
    pub fn rail_hidden(&self) -> bool {
        matches!(
            self.engine
                .setting(SETTING_RAIL_HIDDEN)
                .ok()
                .flatten()
                .as_deref()
                .map(str::trim),
            Some("true")
        )
    }

    /// The backdrop every signed-in page sits on.
    ///
    /// Resolved here rather than per page so the answer cannot drift between
    /// pages: one settings row, one uploaded file, one URL. A `custom` choice
    /// with no file behind it falls back to the default instead of a hole.
    fn wallpaper_url(&self) -> Option<String> {
        let stored = self
            .engine
            .setting(crate::web::wallpaper::SETTING_WALLPAPER)
            .ok()
            .flatten();
        let selected = crate::web::wallpaper::current(stored.as_deref());
        let custom_present = crate::web::wallpaper::custom_file(self.engine.paths()).is_file();
        if selected == crate::web::wallpaper::CUSTOM && !custom_present {
            crate::web::wallpaper::url(crate::web::wallpaper::DEFAULT_WALLPAPER)
        } else {
            crate::web::wallpaper::url(&selected)
        }
    }

    /// Whether full host sandbox mode may be selected.
    ///
    /// Either the process was started with `--allow-full`, or the operator turned
    /// it on from the dashboard. The stored switch is read on every call rather
    /// than cached, so flipping it takes effect on the next save without a
    /// restart, and revoking it is immediate for the same reason.
    #[must_use]
    pub fn allow_full(&self) -> bool {
        if self.allow_full {
            return true;
        }
        matches!(
            self.engine.setting(crate::onboarding::SETTING_ALLOW_FULL),
            Ok(Some(value)) if value.trim().eq_ignore_ascii_case("true")
        )
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
        .route(
            "/static/wallpaper/{name}",
            get(crate::web::routes::wallpaper_file),
        )
        .merge(setup_routes())
        .merge(dashboard)
        .fallback(no_route)
        .method_not_allowed_fallback(wrong_method)
        .layer(middleware::from_fn_with_state(
            state.clone(),
            client_context_middleware,
        ))
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
    /// One of the configured addresses could not be bound.
    #[error("cannot bind an admin listener on {addr}: {source}")]
    Bind {
        /// The configured address.
        addr: SocketAddr,
        /// Underlying I/O error.
        source: io::Error,
    },
    /// A listener address could not be read back.
    #[error("cannot read a bound admin address: {0}")]
    LocalAddr(#[source] io::Error),
    /// Serving failed.
    #[error("admin listener failed: {0}")]
    Serve(#[source] io::Error),
}

/// Bind [`ADMIN_ADDR`] and serve until a shutdown signal arrives.
///
/// This is the shape the tests and `serve_on` callers use: one listener, the
/// documented default.
pub async fn serve(state: AppState) -> Result<(), ServeError> {
    let default: SocketAddr = ADMIN_ADDR
        .parse()
        .expect("the default is a literal ip:port");
    serve_listeners(state, vec![default]).await
}

/// Bind every address in `addrs` and serve the same router on all of them.
///
/// Every address must bind before any of them serves, so a failure is a startup
/// failure that names the address rather than a daemon that quietly lost the
/// listener the operator asked for. The addresses that did bind are recorded in
/// [`AppState`] before the first request, which is what lets the dashboard and
/// the footer state the truth instead of a constant.
///
/// A wildcard bind is called out in the log at `warn` level, naming the exposure:
/// `0.0.0.0` is every interface this host has, and that is a decision the
/// operator should see every time the daemon starts.
pub async fn serve_listeners(state: AppState, addrs: Vec<SocketAddr>) -> Result<(), ServeError> {
    let (addrs, covered) = listen::effective_binds(&addrs);
    for addr in &covered {
        tracing::info!(
            addr = %addr,
            "admin listener is covered by a wildcard bind on the same port, so it gets no socket of its own"
        );
    }
    let mut bound = Vec::with_capacity(addrs.len());
    let mut listeners = Vec::with_capacity(addrs.len());
    for addr in addrs {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|source| ServeError::Bind { addr, source })?;
        let local = listener.local_addr().map_err(ServeError::LocalAddr)?;
        bound.push(local);
        listeners.push(listener);
    }

    state.set_bound_listeners(bound.clone());
    for addr in &bound {
        if listen::is_wildcard(*addr) {
            tracing::warn!(
                addr = %addr,
                "admin listener bound to the wildcard address: the UI answers on every interface this host has, including any LAN and tailnet address"
            );
        }
        if listen::is_tailnet(addr.ip()) {
            tracing::info!(addr = %addr, "admin listener bound on a tailnet address");
        }
    }
    tracing::info!(
        listeners = ?bound,
        count = bound.len(),
        "admin listeners bound"
    );

    let mut tasks = tokio::task::JoinSet::new();
    for listener in listeners {
        let state = state.clone();
        tasks.spawn(async move { serve_on(listener, state).await });
    }

    let mut outcome: Result<(), ServeError> = Ok(());
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                if outcome.is_ok() {
                    outcome = Err(error);
                }
            }
            Err(join) => {
                if outcome.is_ok() && join.is_panic() {
                    outcome = Err(ServeError::Serve(io::Error::other(
                        "an admin listener task panicked",
                    )));
                }
            }
        }
        // One listener stopping is the shutdown signal for all of them: they
        // share the same signal and the same router.
        tasks.abort_all();
    }
    outcome
}

/// Serve an already-bound listener. Used by the socket tests and by
/// [`serve_listeners`]; both get the client's peer address, because the
/// trusted-proxy rule cannot work without it.
pub async fn serve_on(listener: TcpListener, state: AppState) -> Result<(), ServeError> {
    let app = router_with(state);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
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
        assert_eq!(VERSION, "1.4.6");
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
