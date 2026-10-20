//! Server-rendered pages: one layout, one wizard, one dashboard.
//!
//! `maud 0.27` builds the markup in Rust, so there is no template directory, no
//! build step, no partial that can drift from a handler, and nothing for `curl`
//! to miss. Every page is a plain HTML document with one inline stylesheet and
//! no script.
//!
//! Accessibility rules that are structural rather than aspirational:
//!
//! - every input has an `id` and a matching `<label for>`;
//! - each form carries a region with `role="status"` and `aria-live="polite"`
//!   that holds the validation message;
//! - the input that failed is rendered with `autofocus` and `aria-invalid`, so
//!   focus lands on the first error without any JavaScript;
//! - colour is never the only signal: the alert carries the message text.

use std::net::SocketAddr;

use maud::{DOCTYPE, Markup, PreEscaped, html};

use crate::admin::{ADMIN_ADDR, BUILD, VERSION};
use crate::db::SetupState;
use crate::device::DEVICE_ADDR;
use crate::onboarding::{
    ApprovalPolicy, AutopilotCeiling, PermissionRow, Permissions, RowControl, SandboxMode, Step,
    Telemetry, Wizard,
};

use super::css::APP_CSS;
use super::store::{AuditEntry, DeviceOverview, JobRow, SessionRow};
use super::wallpaper;

/// A validation failure, shaped for the JSON body and for the inline alert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormError {
    /// The form field the message belongs to, when it has one.
    pub field: Option<String>,
    /// Stable machine-readable code.
    pub code: String,
    /// Human-readable message. No secret ever reaches this string.
    pub message: String,
}

impl FormError {
    /// Build one.
    #[must_use]
    pub fn new(field: Option<&str>, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.map(str::to_owned),
            code: code.into(),
            message: message.into(),
        }
    }

    /// The JSON body the API answers with.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "ok": false,
            "field": self.field,
            "code": self.code,
            "message": self.message,
        })
    }
}

/// One device, as the dashboard lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    /// Row id, which is what the revoke form posts.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Platform label.
    pub platform: String,
    /// When its row was created. Not the same thing as paired.
    pub paired_at: Option<String>,
    /// True only when a code was redeemed or a live token exists.
    pub paired: bool,
    /// True when one of its live tokens carries `admin`.
    pub admin: bool,
    /// When it was revoked, if it was.
    pub revoked_at: Option<String>,
}

impl DeviceRow {
    /// Whether this device may change the operator's settings, in plain words.
    #[must_use]
    pub fn admin_label(&self) -> &'static str {
        if self.admin {
            "can administer"
        } else {
            "chat only"
        }
    }

    /// The one-word state the table shows.
    #[must_use]
    pub fn state_label(&self) -> &'static str {
        if self.revoked_at.is_some() {
            "revoked"
        } else if self.paired {
            "paired"
        } else {
            "awaiting pairing"
        }
    }
}

/// Everything the wizard page needs.
#[derive(Debug, Clone)]
pub struct SetupView {
    /// The listener truth this page states in its footer.
    pub chrome: Chrome,
    /// The daemon's state.
    pub state: SetupState,
    /// Server-side wizard progress.
    pub wizard: Wizard,
    /// The validation failure to show, if this render follows a failed POST.
    pub error: Option<FormError>,
    /// A success line to show, if this render follows a step.
    pub notice: Option<String>,
    /// Stored provider base URL, for pre-filling.
    pub provider_base_url: String,
    /// Stored key reference, for pre-filling. A reference, never a value.
    pub provider_key_ref: String,
    /// Model ids the probe returned.
    pub models: Vec<String>,
    /// Stored default model.
    pub model: String,
    /// Current permission tuple.
    pub permissions: Permissions,
    /// The rendered permission table.
    pub permission_rows: Vec<PermissionRow>,
    /// Current workspace path.
    pub workspace: String,
    /// Whether the operator asked for the device listener.
    pub device_listener: bool,
    /// A freshly minted pairing code, shown once.
    pub pairing_code: Option<String>,
    /// When that code expires.
    pub pairing_expires: Option<String>,
    /// The version line in the footer.
    pub version: String,
}

impl SetupView {
    /// A view with the documented defaults filled in.
    #[must_use]
    pub fn new(state: SetupState, wizard: Wizard, version: impl Into<String>) -> Self {
        Self {
            chrome: Chrome::default(),
            state,
            wizard,
            error: None,
            notice: None,
            provider_base_url: String::new(),
            provider_key_ref: "env:NINEROUTER_API_KEY".to_owned(),
            models: Vec::new(),
            model: String::new(),
            permissions: Permissions::DEFAULT,
            permission_rows: Vec::new(),
            workspace: "~/kallisti-workspace".to_owned(),
            device_listener: false,
            pairing_code: None,
            pairing_expires: None,
            version: version.into(),
        }
    }
}

/// Everything the dashboard page needs.
#[derive(Debug, Clone)]
pub struct DashboardView {
    /// The listener truth this page states in its footer.
    pub chrome: Chrome,
    /// The daemon's state, which is `ready` on this page.
    pub state: SetupState,
    /// The version line.
    pub version: String,
    /// Whether the database answered the health read.
    pub db_ok: bool,
    /// Live admin sessions.
    pub sessions_live: usize,
    /// Device session rows, revoked or not.
    pub device_sessions: i64,
    /// Device rows known to this host, revoked or not.
    pub devices_known: i64,
    /// Devices that are genuinely paired: a redeemed code or a live token.
    pub paired_devices: usize,
    /// Job counts, every status present and zero filled.
    pub jobs: Vec<(&'static str, i64)>,
    /// Stored provider base URL.
    pub provider_base_url: Option<String>,
    /// Stored key reference.
    pub provider_key_ref: Option<String>,
    /// Stored default model.
    pub model: Option<String>,
    /// When the provider was last probed.
    pub probed_at: Option<String>,
    /// How many model ids the last probe listed.
    pub model_count: usize,
    /// Calls parked and waiting for a person. Non-zero blocks turns.
    pub approvals_waiting: usize,
    /// MCP servers configured, and how many are currently failing.
    pub mcp_servers: usize,
    /// MCP servers whose last start or call failed.
    pub mcp_errors: usize,
    /// Which memory backend a turn recalls from.
    pub memory_backend: String,
    /// The last finished turn, as one line, when there is one.
    pub last_turn: Option<String>,
    /// The database file size, as a human string.
    pub db_size: String,
    /// Known devices.
    pub devices: Vec<DeviceRow>,
    /// Current permission tuple.
    pub permissions: Permissions,
    /// The rendered permission table.
    pub permission_rows: Vec<PermissionRow>,
    /// Whether the operator asked for the device listener.
    pub device_listener: bool,
    /// Whether this process has bound the device port.
    pub device_listener_bound: bool,
    /// A validation failure to show.
    pub error: Option<FormError>,
    /// A success line to show.
    pub notice: Option<String>,
}

impl DashboardView {
    /// A view with the documented defaults filled in.
    #[must_use]
    pub fn new(state: SetupState, version: impl Into<String>) -> Self {
        Self {
            chrome: Chrome::default(),
            state,
            version: version.into(),
            db_ok: true,
            sessions_live: 0,
            device_sessions: 0,
            devices_known: 0,
            paired_devices: 0,
            jobs: crate::web::store::JOB_STATUSES
                .iter()
                .map(|status| (*status, 0))
                .collect(),
            provider_base_url: None,
            provider_key_ref: None,
            model: None,
            probed_at: None,
            model_count: 0,
            approvals_waiting: 0,
            mcp_servers: 0,
            mcp_errors: 0,
            memory_backend: crate::memory::DEFAULT_BACKEND.to_owned(),
            last_turn: None,
            db_size: String::new(),
            devices: Vec::new(),
            permissions: Permissions::DEFAULT,
            permission_rows: Vec::new(),
            device_listener: false,
            device_listener_bound: false,
            error: None,
            notice: None,
        }
    }

    /// The device listener as one line: on or off, and whether the socket exists.
    #[must_use]
    pub fn device_listener_line(&self) -> String {
        let wanted = if self.device_listener { "on" } else { "off" };
        let bound = if self.device_listener_bound {
            "bound"
        } else {
            "not bound"
        };
        format!("{wanted}, {bound} on {DEVICE_ADDR}")
    }

    /// The job counts as one line, every status named.
    #[must_use]
    pub fn jobs_line(&self) -> String {
        self.jobs
            .iter()
            .map(|(status, count)| format!("{status} {count}"))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// A value safe to use as an element id: lowercase, spaces to hyphens.
///
/// `L0 notify` becomes `l0-notify`, so `id` and `for` stay one token and the
/// label association is valid CSS-selectable HTML rather than a space inside an
/// id.
fn slug(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// The listener truth every page states in its footer, plus the public URL when
/// the operator configured one.
///
/// It is built from [`crate::admin::AppState::chrome`], which reads the addresses
/// that actually bound. A page never guesses a listener from a constant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chrome {
    /// The addresses that bound, in bind order.
    pub listeners: Vec<SocketAddr>,
    /// `admin.public_url`, when one is configured.
    pub public_url: Option<String>,
    /// The backdrop every signed-in page sits on, when one is chosen.
    ///
    /// It lives here rather than on one page because the wallpaper is the
    /// application's desk, not a chat decoration: Settings, Sessions and the
    /// dashboard all sit on the same surface.
    pub wallpaper_url: Option<String>,
    /// The projects the rail can head a group with, by name.
    ///
    /// A rail with no projects is a rail with one flat list, which is what it
    /// was before projects existed: nothing here is a prerequisite for history.
    pub projects: Vec<crate::projects::Project>,
    /// The conversations the rail lists, newest first.
    pub sessions: Vec<ChatSession>,
    /// The conversation this page is showing, when it is showing one.
    pub current_conversation: Option<String>,
    /// True when the operator collapsed the rail.
    pub rail_hidden: bool,
    /// The subagents worth reporting: anything still working, plus anything that
    /// finished recently. Empty means no control is rendered at all.
    ///
    /// The rows themselves rather than a copy, so the bar and a page that queries
    /// the table cannot disagree about a status or an elapsed time.
    pub subagents: Vec<crate::subagents::Subagent>,
}

/// One conversation in the rail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSession {
    /// Opaque id, also the route segment.
    pub id: String,
    /// What the operator reads.
    pub title: String,
    /// A short stamp for the rail.
    pub stamp: String,
    /// The project it is filed under, when it is filed under one this rail knows.
    pub project_id: Option<String>,
    /// That project's name, so the row can state where it lives without a lookup.
    pub project_name: Option<String>,
}

impl Default for Chrome {
    fn default() -> Self {
        Self {
            listeners: vec![
                ADMIN_ADDR
                    .parse()
                    .expect("the default admin address is a literal ip:port"),
            ],
            public_url: None,
            // The documented default, so a page rendered without a resolved
            // settings row still paints the Kallisti backdrop.
            wallpaper_url: super::wallpaper::url(super::wallpaper::DEFAULT_WALLPAPER),
            projects: Vec::new(),
            sessions: Vec::new(),
            current_conversation: None,
            rail_hidden: false,
            subagents: Vec::new(),
        }
    }
}

impl Chrome {
    /// The first bound address: the one the config or the flag named first.
    #[must_use]
    pub fn primary(&self) -> String {
        self.listeners
            .first()
            .map(ToString::to_string)
            .unwrap_or_else(|| ADMIN_ADDR.to_owned())
    }

    /// Every bound address as one line, for the System card.
    #[must_use]
    pub fn listens_line(&self) -> String {
        if self.listeners.is_empty() {
            return ADMIN_ADDR.to_owned();
        }
        self.listeners
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The footer line: the primary address, and the count when there is more
    /// than one listener.
    #[must_use]
    pub fn footer_line(&self) -> String {
        let count = self.listeners.len();
        if count > 1 {
            format!("{} ({count} listeners)", self.primary())
        } else {
            self.primary()
        }
    }
}

/// The document shell every page shares, without navigation.
///
/// Used by the three pages that must stay minimal: the wizard while it is open,
/// the sign-in form, and an error page. A nav on any of those is a trap, because
/// the pages it points at are not reachable from there.
pub fn layout(title: &str, state: SetupState, body: Markup, chrome: &Chrome) -> Markup {
    shell(title, state, None, body, chrome)
}

/// The signed-in application shell: top bar, nav, page, footer.
///
/// The nav is a `nav` element with an accessible name and one entry per section,
/// and the current page is marked with `aria-current="page"` in addition to its
/// colour, so the marker survives a reader, a monochrome screen, and a grep.
///
/// `Nav::Sessions` and `Nav::Devices` render as unavailable entries rather than
/// links: those two pages are not part of this phase, and a nav that answers a
/// click with a 404 is worse than a nav that says so.
pub fn shell(
    title: &str,
    state: SetupState,
    nav: Option<Nav>,
    body: Markup,
    chrome: &Chrome,
) -> Markup {
    let state_class = match state {
        SetupState::Setup => "badge setup",
        SetupState::Ready => "badge ready",
    };
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="referrer" content="no-referrer";
                title { (title) " - Kallisti 1.0" }
                style { (PreEscaped(APP_CSS)) }
            }
            body {
                @if let Some(url) = &chrome.wallpaper_url {
                    // The backdrop is a field: the artwork's own edges fade to the
                    // page colour, so `cover` has nothing to seam against.
                    @let art = url.ends_with(".jpg") || url.ends_with(".jpeg")
                        || url.ends_with(".png") || url.ends_with("custom");
                    div class=(if art { "app-wall art" } else { "app-wall" })
                        style=(format!("background-image:url('{url}')")) {}
                }
                div class=(if chrome.rail_hidden { "app rail-hidden" } else { "app" }) {
                    header .side {
                        div .brand {
                            span .mark aria-hidden="true" { "K" }
                            span .word {
                                strong { "Kallisti" }
                                span .ver { "1.0 Eris daemon" }
                            }
                        }
                        @if nav.is_some() {
                            form .rail-hide method="post" action=(crate::admin::ROUTE_SIDEBAR) {
                                input type="hidden" name="rail" value="hidden";
                                button type="submit" title="Hide the sidebar"
                                    aria-label="Hide the sidebar" { "\u{00AB}" }
                            }
                            form .side-action method="post" action=(crate::admin::ROUTE_CHAT_NEW) {
                                button type="submit" { "+  New chat" }
                            }
                            (new_project_control(chrome))
                            nav .sessions id="rail-sessions" aria-label="Conversations" {
                                @if chrome.sessions.is_empty() {
                                    span .side-empty { "No conversations yet" }
                                }
                                @if chrome.projects.is_empty() {
                                    /* No project has been named, so there is nothing to
                                       head a group with: the list stays the flat one it
                                       has always been. */
                                    @for session in &chrome.sessions {
                                        (rail_session_row(chrome, session))
                                    }
                                } @else {
                                    @for project in &chrome.projects {
                                        @let members: Vec<&ChatSession> = chrome
                                            .sessions
                                            .iter()
                                            .filter(|session| {
                                                session.project_id.as_deref()
                                                    == Some(project.id.as_str())
                                            })
                                            .collect();
                                        @if !members.is_empty() {
                                            div .rail-group {
                                                div .rail-group-head {
                                                    span .rail-group-name { (project.name) }
                                                    span .rail-group-count { (members.len()) }
                                                }
                                                @for session in members {
                                                    (rail_session_row(chrome, session))
                                                }
                                            }
                                        }
                                    }
                                    @let unfiled: Vec<&ChatSession> = chrome
                                        .sessions
                                        .iter()
                                        .filter(|session| session.project_id.is_none())
                                        .collect();
                                    @if !unfiled.is_empty() {
                                        /* The ungrouped heading keeps its own name: the
                                           conversations that were here before projects
                                           existed did not move anywhere. */
                                        div .rail-group {
                                            div .rail-group-head {
                                                span .rail-group-name { "Conversations" }
                                                span .rail-group-count { (unfiled.len()) }
                                            }
                                            @for session in unfiled {
                                                (rail_session_row(chrome, session))
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        div .side-foot {
                            /* The cog is the way into everything that configures
                               the daemon, and it only exists for a signed-in
                               browser: a settings link on the sign-in form would
                               only bounce back to the sign-in form. */
                            @if nav.is_some() {
                                @let on_settings = nav == Some(Nav::Settings);
                                a .cog href=(crate::admin::ROUTE_SETTINGS)
                                    aria-label="Settings"
                                    aria-current=[on_settings.then_some("page")] {
                                    span .cog-glyph aria-hidden="true" { "\u{2699}" }
                                    span .cog-label { "Settings" }
                                }
                            }
                            span class=(state_class) { (state.as_str()) }
                            span .ver { "Eris " (VERSION) " build " (BUILD) }
                            /* The version strip that used to sit under every page, here
                               instead: the content column is for content, and this is
                               the app talking about itself. */
                            span .listener title=(format!(
                                "admin listener {} | configured in config.toml",
                                chrome.footer_line(),
                            )) {
                                (chrome.footer_line())
                            }
                            @if let Some(url) = &chrome.public_url {
                                span .listener title=(url) { (url) }
                            }
                            span .sign { "to the most beautiful" }
                        }
                    }
                    div .wrap {
                        @if chrome.rail_hidden {
                            /* The show control sits where the hide control sits - at
                               the top of where the rail was - so the pair reads as
                               one toggle that moves the rail, not as a stray button
                               at the top of the page or a passenger on the cog. */
                            form .rail-show .rail-show-top method="post" action=(crate::admin::ROUTE_SIDEBAR) {
                                input type="hidden" name="rail" value="shown";
                                button type="submit" title="Show the sidebar"
                                    aria-label="Show the sidebar" { "\u{00BB}" }
                            }
                        }
                        @if chrome.rail_hidden && nav.is_some() {
                            /* The rail's foot, collapsed: the word "Settings" is what
                               the hidden sidebar took, so the gear stays in the corner
                               it has always occupied and only its label goes. */
                            div .rail-restore {
                                a .cog .cog-only href=(crate::admin::ROUTE_SETTINGS)
                                    aria-label="Settings"
                                    aria-current=[(nav == Some(Nav::Settings)).then_some("page")] {
                                    span .cog-glyph aria-hidden="true" { "\u{2699}" }
                                }
                            }
                        }
                        /* The section nav is shell furniture, except on the
                           conversation: chat is the surface, and a row of section
                           chips above the transcript fights it. Every other page
                           carries the row, and the cog in the rail reaches
                           settings from chat. */
                        @let sections = nav.is_some_and(|current| current != Nav::Chat);
                        @if sections || !chrome.subagents.is_empty() {
                            /* The subagents control sits in this row because that is
                               where the app talks about itself, and on the
                               conversation it is the only thing in the row: a
                               dispatch from a chat turn is exactly when the operator
                               needs to see it. */
                            div .top-nav {
                                @if sections {
                                    @if let Some(current) = nav {
                                        (nav_bar(current))
                                    }
                                }
                                (subagents_control(chrome))
                            }
                        }
                        (body)
                    }
                }
            }
        }
    }
}

/// The "New project" control, directly under "New chat".
///
/// It is a `details` element because that is the one disclosure the browser
/// opens and closes without a script, and the rail may carry no script at all.
/// The control is deliberately quieter than "New chat": starting a conversation
/// is the common action, and naming a group to put one in is the occasional one.
fn new_project_control(chrome: &Chrome) -> Markup {
    html! {
        details .rail-new-project {
            summary title="New project" {
                span .rail-add-glyph aria-hidden="true" { "+" }
                span .rail-add-label { "New project" }
            }
            form method="post" action=(crate::admin::ROUTE_PROJECTS) {
                label .sr-only for="project_name" { "Project name" }
                input type="text" id="project_name" name="project_name"
                    placeholder="Project name" autocomplete="off"
                    maxlength=(crate::projects::MAX_NAME_CHARS);
                button type="submit" { "Create" }
                @if chrome.projects.is_empty() {
                    span .rail-hint { "No projects yet" }
                }
            }
        }
    }
}

/// One conversation in the rail, with the menu that files it.
///
/// The menu is a `details` element as well, for the same reason: the select and
/// its submit post a plain form, so "add to project" works with every script
/// blocked. The menu only shows about its own row, which is why it sits inside
/// the row rather than in one list-wide control.
fn rail_session_row(chrome: &Chrome, session: &ChatSession) -> Markup {
    let open = chrome.current_conversation.as_deref() == Some(session.id.as_str());
    let field_id = format!("project-{}", slug(&session.id));
    html! {
        div .session-row .open[open] {
            a href=(format!("/dashboard/chat/{}", session.id))
                aria-current=[open.then_some("page")]
                title=(session.title) {
                span .session-title { (session.title) }
                span .session-stamp { (session.stamp) }
            }
            details .session-menu {
                summary title="Add to a project"
                    aria-label=(format!("Add {} to a project", session.title)) {
                    "\u{2026}"
                }
                form .session-menu-form method="post"
                    action=(crate::admin::ROUTE_PROJECTS_ASSIGN) {
                    input type="hidden" name="conversation" value=(session.id.as_str());
                    label .sr-only for=(field_id.as_str()) { "Project" }
                    select id=(field_id.as_str()) name="project" aria-label="Project" {
                        option value="" selected[session.project_id.is_none()] { "No project" }
                        @for project in &chrome.projects {
                            @let current = session.project_id.as_deref() == Some(project.id.as_str());
                            option value=(project.id.as_str()) selected[current] { (project.name) }
                        }
                    }
                    button type="submit" { "Move" }
                }
            }
            @if open {
                form .session-delete method="post"
                    action=(format!("/dashboard/chat/{}/delete", session.id)) {
                    button type="submit" title="Delete this conversation"
                        aria-label="Delete this conversation" { "×" }
                }
            }
        }
    }
}

/// One section of the application shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    /// The conversation surface: the transcript, the composer, the wallpaper.
    Chat,
    /// The dashboard home.
    Dashboard,
    /// Every device session, and the jobs each one started.
    Sessions,
    /// Every device, and the form that mints a pairing code.
    Devices,
    /// The settings surface, the only page that writes behaviour.
    Settings,
    /// The audit trail.
    Activity,
    /// The archive list, the create button, and the restore form.
    Backups,
}

impl Nav {
    /// Every entry, in display order. Every one of them is a page: the nav has
    /// no placeholder in it.
    pub const ALL: [Self; 7] = [
        Self::Chat,
        Self::Dashboard,
        Self::Sessions,
        Self::Devices,
        Self::Settings,
        Self::Activity,
        Self::Backups,
    ];

    /// The path this section is served at.
    #[must_use]
    pub fn path(self) -> &'static str {
        match self {
            Self::Chat => crate::admin::ROUTE_CHAT,
            Self::Dashboard => crate::admin::ROUTE_DASHBOARD,
            Self::Sessions => crate::admin::ROUTE_SESSIONS,
            Self::Devices => crate::admin::ROUTE_DEVICES,
            Self::Settings => crate::admin::ROUTE_SETTINGS,
            Self::Activity => crate::admin::ROUTE_ACTIVITY,
            Self::Backups => crate::admin::ROUTE_BACKUPS,
        }
    }

    /// The label a person reads.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Dashboard => "Dashboard",
            Self::Sessions => "Sessions",
            Self::Devices => "Devices",
            Self::Settings => "Settings",
            Self::Activity => "Activity",
            Self::Backups => "Backups",
        }
    }
}

/// The top-bar navigation.
///
/// Every entry is a link to a page that exists, and the current one is marked
/// with `aria-current="page"` as well as its colour. Nothing here is a disabled
/// placeholder: the last one became a page, so the nav no longer promises a
/// section the listener does not serve.
fn nav_bar(current: Nav) -> Markup {
    html! {
        nav class="sections" aria-label="Sections" {
            @for entry in Nav::ALL {
                a href=(entry.path()) aria-current=[(current == entry).then_some("page")] {
                    (entry.label())
                }
            }
        }
    }
}

/// The top-bar control that reports dispatched work.
///
/// A `details` element, for the same reason the rail's menus are: it is the one
/// disclosure a browser opens and closes with no script at all, and this control
/// exists to be readable with JavaScript blocked. Nothing here links anywhere,
/// because a subagent has no page of its own: the row is the whole record, and its
/// result is the text it produced.
///
/// It renders nothing when there is nothing to report, so the top bar is exactly
/// as wide as it was before a dispatch existed.
fn subagents_control(chrome: &Chrome) -> Markup {
    if chrome.subagents.is_empty() {
        return Markup::default();
    }
    let running = chrome
        .subagents
        .iter()
        .filter(|row| !row.status.settled())
        .count();
    html! {
        details .subagents {
            summary title="Work the model dispatched, newest first" {
                span .subagents-word { "Subagents" }
                span .subagents-count { (chrome.subagents.len()) }
                @if running > 0 {
                    span .subagents-live { (running) " running" }
                }
            }
            div .subagents-list aria-label="Dispatched subagents" {
                @for row in &chrome.subagents {
                    (subagent_entry(row))
                }
            }
        }
    }
}

/// One subagent in the bar: what it is, where it is, and what came back.
fn subagent_entry(row: &crate::subagents::Subagent) -> Markup {
    html! {
        div .subagent {
            div .subagent-head {
                span .subagent-label { (row.label) }
                span class=(format!("subagent-status {}", row.status.as_str())) {
                    (row.status.as_str())
                }
                span .subagent-elapsed { (row.elapsed_label()) }
                span .subagent-model { (row.model) }
            }
            @if let Some(result) = row.result_preview() {
                pre .subagent-result { (result) }
            }
        }
    }
}

/// The step rail. Completed steps are marked in text, not only in colour.
fn rail(wizard: &Wizard) -> Markup {
    html! {
        ol .rail aria-label="Onboarding steps" {
            @for (index, status) in wizard.steps.iter().enumerate() {
                @let done = status.completed_at.is_some();
                @let current = wizard.next_step == Some(status.step) && wizard.state == SetupState::Setup;
                @let class = match (done, current) {
                    (_, true) => "current",
                    (true, false) => "done",
                    (false, false) => "",
                };
                li class=(class) {
                    span .num { "Step " (index + 1) }
                    (status.step.title())
                    @if done { " " span .tag { "done" } }
                    @if status.optional { " " span .tag { "optional" } }
                }
            }
        }
    }
}

/// The validation region. `role="status"` plus `aria-live` is the whole
/// announcement mechanism: no script is involved.
fn alert_region(error: Option<&FormError>, notice: Option<&str>) -> Markup {
    html! {
        div id="form-error" role="status" aria-live="polite" {
            @if let Some(error) = error {
                div .alert {
                    strong { (error.code) }
                    " "
                    (error.message)
                }
            } @else if let Some(notice) = notice {
                div .alert .notice {
                    strong { "ok" }
                    " "
                    (notice)
                }
            }
        }
    }
}

/// A labelled text input, with the error ring and focus target applied when this
/// is the field that failed.
fn text_field(
    name: &str,
    label: &str,
    value: &str,
    hint: Option<&str>,
    error: Option<&FormError>,
    autofocus: bool,
    secret: bool,
) -> Markup {
    // The element id is the field name, so `label for` and the input can never
    // drift apart and a fragment link can target the field.
    let invalid = error.is_some_and(|error| error.field.as_deref() == Some(name));
    html! {
        div .field {
            label for=(name) { (label) }
            @if secret {
                input id=(name) type="password" name=(name) value=(value) autocomplete="off"
                    spellcheck="false" .invalid[invalid] aria-invalid=[invalid.then_some("true")]
                    autofocus[autofocus];
            } @else {
                input id=(name) type="text" name=(name) value=(value) autocomplete="off"
                    spellcheck="false" .invalid[invalid] aria-invalid=[invalid.then_some("true")]
                    autofocus[autofocus];
            }
            @if let Some(hint) = hint { span .hint { (hint) } }
        }
    }
}

/// One radio group: a labelled fieldset the browser can navigate natively.
///
/// When this group is the one that failed, the first *selectable* option takes
/// the focus: a disabled input cannot hold it, so focusing the first option
/// blindly would leave the caret nowhere.
fn radio_group(
    legend: &str,
    name: &str,
    options: &[(&'static str, &'static str, &'static str)],
    selected: &str,
    disabled_values: &[&str],
    error: Option<&FormError>,
) -> Markup {
    let invalid = error.is_some_and(|error| error.field.as_deref() == Some(name));
    let focus_index = options
        .iter()
        .position(|(value, _, _)| !disabled_values.contains(value));
    html! {
        fieldset .field aria-invalid=[invalid.then_some("true")] {
            legend { (legend) }
            div .choices {
                @for (index, (value, label, why)) in options.iter().enumerate() {
                    @let checked = *value == selected;
                    @let disabled = disabled_values.contains(value);
                    div .choice .selected[checked] .disabled[disabled] {
                        input type="radio" id=(format!("{name}-{}", slug(value))) name=(name) value=(value)
                            checked[checked] disabled[disabled]
                            autofocus[invalid && focus_index == Some(index)];
                        label for=(format!("{name}-{}", slug(value))) {
                            span .name { (label) }
                            span .why { (why) }
                        }
                    }
                }
            }
        }
    }
}

/// The permission table. Read-only rows say so; nothing is hidden.
pub fn permission_table(rows: &[PermissionRow]) -> Markup {
    html! {
        table {
            caption {
                "What a turn may do. Each row states its consequence."
            }
            thead {
                tr {
                    th scope="col" { "Permission" }
                    th scope="col" { "Value" }
                    th scope="col" { "What that means" }
                }
            }
            tbody {
                @for row in rows {
                    @let locked = row.control == RowControl::Locked;
                    tr class=[locked.then_some("locked")] {
                        th scope="row" {
                            (row.name)
                            @if let Some(reason) = row.lock_reason {
                                @if locked {
                                    div .small { "locked: " (reason) }
                                }
                            }
                        }
                        td .value {
                            (row.value)
                            @if locked { " " span .tag { "locked" } }
                        }
                        td .why { (row.consequence) }
                    }
                }
            }
        }
    }
}

/// The permission step's own form controls.
fn permission_form(view: &SetupView) -> Markup {
    let error = view.error.as_ref();
    let never_available = view.device_listener && view.state == SetupState::Ready;
    let disabled: &[&str] = if never_available { &[] } else { &["never"] };
    html! {
        form method="post" action="/setup/permissions" novalidate {
            (radio_group(
                "Sandbox mode",
                "sandbox_mode",
                &SandboxMode::ONBOARDING
                    .iter()
                    .map(|mode| (mode.as_str(), mode.label(), mode.consequence()))
                    .collect::<Vec<_>>(),
                view.permissions.sandbox_mode.as_str(),
                &[],
                error,
            ))
            (radio_group(
                "Approval policy",
                "approval_policy",
                &ApprovalPolicy::ALL
                    .iter()
                    .map(|policy| (policy.as_str(), policy.label(), policy.consequence()))
                    .collect::<Vec<_>>(),
                view.permissions.approval_policy.as_str(),
                disabled,
                error,
            ))
            (radio_group(
                "Autopilot ceiling",
                "autopilot_ceiling",
                &AutopilotCeiling::ALL
                    .iter()
                    .map(|level| (level.as_str(), level.label(), level.consequence()))
                    .collect::<Vec<_>>(),
                view.permissions.autopilot_ceiling.as_str(),
                &[],
                error,
            ))
            (radio_group(
                "Telemetry",
                "telemetry",
                &Telemetry::ALL
                    .iter()
                    .map(|sink| (sink.as_str(), sink.label(), sink.consequence()))
                    .collect::<Vec<_>>(),
                view.permissions.telemetry.as_str(),
                &[],
                error,
            ))
            p .small {
                "Full host sandbox mode is not offered here. Only a restart of eris with "
                code { "--allow-full" } " can select it."
            }
            div .actions {
                button type="submit" { "Save permissions" }
            }
        }
    }
}

/// The body of a finished wizard: the summary, and the way to the dashboard.
fn step_body_ready(view: &SetupView) -> Markup {
    html! {
        div .card {
            h3 { "Onboarding complete" }
            p { "The daemon is ready. The wizard is locked and no longer writes." }
            dl .kv {
                dt { "Provider" } dd { (view.provider_base_url) }
                dt { "Key reference" } dd { (view.provider_key_ref) }
                dt { "Model" } dd { (view.model) }
                dt { "Sandbox mode" } dd { (view.permissions.sandbox_mode.as_str()) }
                dt { "Approval policy" } dd { (view.permissions.approval_policy.as_str()) }
                dt { "Autopilot ceiling" } dd { (view.permissions.autopilot_ceiling.as_str()) }
                dt { "Telemetry" } dd { (view.permissions.telemetry.as_str()) }
                dt { "Workspace" } dd { (view.workspace) }
            }
            div .actions {
                a .button href="/dashboard" { "Open the dashboard" }
            }
        }
    }
}

/// The body of the current step, plus everything already done.
fn step_body(view: &SetupView) -> Markup {
    let error = view.error.as_ref();
    let belongs = |field: &str| error.is_some_and(|error| error.field.as_deref() == Some(field));

    // The first error's field gets the focus, and only that field does.
    let focus_claim = belongs("passphrase");
    let focus_provider = belongs("base_url") || belongs("api_key_ref");
    let focus_model = belongs("model");
    let focus_workspace = belongs("workspace_path");

    if view.state == SetupState::Ready {
        return step_body_ready(view);
    }
    html! {
        @match view.wizard.next_step {
            Some(Step::Claim) => {
                form method="post" action="/setup/claim" novalidate {
                    (text_field(
                        "passphrase",
                        "Setup passphrase",
                        "",
                        Some("Six words, separated by spaces, from the first boot."),
                        error,
                        focus_claim,
                        true,
                    ))
                    div .actions {
                        button type="submit" { "Claim this daemon" }
                    }
                }
            }
            Some(Step::Provider) => {
                form method="post" action="/setup/provider" novalidate {
                    (text_field(
                        "base_url",
                        "Endpoint",
                        if view.provider_base_url.is_empty() {
                            "http://127.0.0.1:20128/v1"
                        } else {
                            view.provider_base_url.as_str()
                        },
                        Some("An OpenAI-compatible base URL. The probe calls /models on it."),
                        error,
                        focus_provider,
                        false,
                    ))
                    (text_field(
                        "api_key_ref",
                        "Key reference",
                        &view.provider_key_ref,
                        Some("env:NAME, resolved from the daemon's environment. A key value is refused here."),
                        error,
                        false,
                        false,
                    ))
                    div .actions {
                        button type="submit" { "Probe and store" }
                    }
                }
            }
            Some(Step::Models) => {
                form method="post" action="/setup/models" novalidate {
                    div .field {
                        label for="model" { "Default model" }
                        select id="model" name="model" autofocus[focus_model] {
                            @for model in &view.models {
                                option value=(model) selected[*model == view.model] { (model) }
                            }
                        }
                        span .hint { "The list came from the provider's own /models response." }
                    }
                    div .actions {
                        button type="submit" { "Use this model" }
                    }
                }
            }
            Some(Step::Permissions) => { (permission_form(view)) }
            Some(Step::Workspace) => {
                form method="post" action="/setup/workspace" novalidate {
                    (text_field(
                        "workspace_path",
                        "Workspace path",
                        &view.workspace,
                        Some("Created at mode 0755 if absent. Refused if it overlaps ~/.dsh, ~/.config, ~/.ssh or ~/vault."),
                        error,
                        focus_workspace,
                        false,
                    ))
                    div .actions {
                        button type="submit" { "Use this workspace" }
                    }
                }
            }
            Some(Step::Device) => {
                @if let Some(code) = &view.pairing_code {
                    div .card .tight {
                        h3 { "Pairing code" }
                        p .pairing-code .mono { (code) }
                        p .small {
                            "Single use. Expires "
                            @if let Some(expires) = &view.pairing_expires { (expires) } @else { "in five minutes" }
                            ". It is stored only as a hash, so this is the one time it is readable."
                        }
                    }
                }
                div .stack {
                    form method="post" action="/setup/device" novalidate {
                        (text_field(
                            "device_name",
                            "Device name",
                            "iPhone",
                            Some("A label for the phone. The code is minted for it."),
                            error,
                            false,
                            false,
                        ))
                        div .actions {
                            button type="submit" class="secondary" { "Mint a pairing code" }
                        }
                    }
                    form method="post" action="/setup/finish" novalidate {
                        div .actions {
                            button type="submit" { "Continue to finish" }
                        }
                    }
                }
            }
            Some(Step::Finish) | None => {
                {
                    dl .kv {
                        dt { "Provider" } dd { (view.provider_base_url) }
                        dt { "Key reference" } dd { (view.provider_key_ref) }
                        dt { "Model" } dd { (view.model) }
                        dt { "Sandbox mode" } dd { (view.permissions.sandbox_mode.as_str()) }
                        dt { "Approval policy" } dd { (view.permissions.approval_policy.as_str()) }
                        dt { "Autopilot ceiling" } dd { (view.permissions.autopilot_ceiling.as_str()) }
                        dt { "Telemetry" } dd { (view.permissions.telemetry.as_str()) }
                        dt { "Workspace" } dd { (view.workspace) }
                    }
                    form method="post" action="/setup/finish" novalidate {
                        div .actions {
                            button type="submit" { "Finish onboarding" }
                        }
                    }
                }
            }
        }
    }
}

/// The wizard page.
pub fn setup_page(view: &SetupView) -> Markup {
    let error = view.error.as_ref();
    let notice = view.notice.as_deref();
    let body = html! {
        h1 {
            @if view.state == SetupState::Ready { "Onboarding complete" } @else { "Onboarding" }
        }
        p .lede {
            @if view.state == SetupState::Ready {
                "Everything 1.0 needs is configured. The wizard no longer writes."
            } @else {
                "Seven steps, stored on the server as you go. A refresh never loses a step."
            }
        }
        (rail(&view.wizard))
        (alert_region(error, notice))
        section .card aria-labelledby="step-heading" {
            h2 id="step-heading" {
                @if view.state == SetupState::Ready {
                    "Summary"
                } @else if let Some(step) = view.wizard.next_step {
                    (step.title())
                } @else {
                    "Summary"
                }
            }
            @if view.state == SetupState::Ready {
                (step_body(view))
            } @else if let Some(step) = view.wizard.next_step {
                p .lede { (step.summary()) }
                (step_body(view))
                @if step == Step::Device || step == Step::Claim {
                    p .small { "Step 6 is optional. Finishing without it leaves the phone out." }
                }
            } @else {
                (step_body(view))
            }
        }
        section aria-labelledby="permissions-heading" {
            h2 id="permissions-heading" { "Permissions" }
            div .card {
                (permission_table(&view.permission_rows))
            }
        }
    };
    layout("Onboarding", view.state, body, &view.chrome)
}

/// The sign-in page: the way into the dashboard from a browser.
///
/// `POST /setup/claim` minted a session exactly once, while the wizard was open.
/// After onboarding finishes that form is gone, so without this page the
/// dashboard had no browser path in at all. It asks for the same credential the
/// claim did, and it writes nothing: a successful post mints a cookie and
/// redirects.
pub fn login_page(state: SetupState, error: Option<&FormError>, chrome: &Chrome) -> Markup {
    let invalid = error.is_some_and(|error| error.field.as_deref() == Some("passphrase"));
    let body = html! {
        div .auth {
            div .mark aria-hidden="true" { "K" }
            h1 { "Sign in" }
            p .lede { "The dashboard answers only a browser holding an admin session. This form mints one." }
            section .card aria-labelledby="login-heading" {
                h2 id="login-heading" { "Admin sign-in" }
                (alert_region(error, None))
                form method="post" action="/login" novalidate {
                    div .field {
                        label for="passphrase" { "Admin passphrase" }
                        input id="passphrase" type="password" name="passphrase" value=""
                            autocomplete="current-password" spellcheck="false"
                            .invalid[invalid] aria-invalid=[invalid.then_some("true")]
                            autofocus[invalid];
                        span .hint {
                            "The passphrase you chose during onboarding: six words, separated by spaces."
                        }
                    }
                    div .actions {
                        button type="submit" { "Sign in" }
                    }
                }
            }
            p .small {
                "Still onboarding? " a href="/setup" { "The wizard" }
                " is the way in before setup finishes."
            }
        }
    };
    layout("Sign in", state, body, chrome)
}

/// The dashboard page.
pub fn dashboard_page(view: &DashboardView) -> Markup {
    let error = view.error.as_ref();
    let notice = view.notice.as_deref();
    let device_listener_label = if view.device_listener { "on" } else { "off" };
    let body = html! {
        h1 { "Dashboard" }
        p .lede { "Health, provider, devices and the kill switches for this host." }
        (alert_region(error, notice))

        section .card aria-labelledby="at-a-glance" {
            h2 id="at-a-glance" { "At a glance" }
            p .small {
                "Only things the daemon can prove. Nothing here is a guess, and a number that is not "
                "known says so instead of showing zero."
            }
            dl .kv {
                dt { "Approvals waiting" }
                dd {
                    @if view.approvals_waiting == 0 {
                        "none"
                    } @else {
                        strong { (view.approvals_waiting) } " parked, blocking the turns that asked"
                    }
                }
                dt { "MCP servers" }
                dd {
                    (view.mcp_servers) " configured"
                    @if view.mcp_errors > 0 {
                        " - " span .warn { (view.mcp_errors) " failing" }
                    }
                }
                dt { "Memory" }
                dd { (view.memory_backend) }
                dt { "Last answer" }
                dd {
                    @if let Some(line) = &view.last_turn {
                        span .mono { (line) }
                    } @else {
                        "nothing answered yet"
                    }
                }
                dt { "Database" }
                dd { (view.db_size) " on disk" }
            }
        }

        section .card aria-labelledby="quick-actions" {
            h2 id="quick-actions" { "Quick actions" }
            div .actions {
                a .button href=(Nav::Chat.path()) { "Open chat" }
                a .button.secondary href=(Nav::Settings.path()) { "Settings" }
                a .button.secondary href=(Nav::Sessions.path()) { "Sessions" }
                a .button.secondary href=(Nav::Devices.path()) { "Devices" }
                a .button.secondary href=(Nav::Activity.path()) { "Activity" }
            }
            p .small {
                "Sessions lists every device sign-in with the jobs it started. Devices lists "
                "every device, revokes one, and mints the pairing code that adds the next."
            }
        }

        section .card aria-labelledby="health" {
            h2 id="health" { "Health" }
            dl .kv {
                dt { "State" } dd { (view.state.as_str()) }
                dt { "Database" } dd { @if view.db_ok { "ok" } @else { "unavailable" } }
                dt { "Version" } dd { (view.version) }
            }
        }

        section .card aria-labelledby="system" {
            h2 id="system" { "System" }
            dl .kv {
                dt { "Setup state" } dd { (view.state.as_str()) }
                dt { "Version" } dd { (view.version) }
                dt { "Database" } dd { @if view.db_ok { "ok" } @else { "unavailable" } }
                dt { "Admin sessions live" } dd { (view.sessions_live) }
                dt { "Device sessions" } dd { (view.device_sessions) }
                dt { "Paired devices" } dd { (view.paired_devices) " of " (view.devices_known) }
                dt { "Admin listeners" } dd { (view.chrome.listens_line()) }
                @if let Some(url) = &view.chrome.public_url {
                    dt { "Public URL" } dd { (url) }
                }
                dt { "Device listener" } dd { (view.device_listener_line()) }
            }
            div .stats aria-label="Jobs by status" {
                @for (status, count) in &view.jobs {
                    div .stat {
                        span .n { (count) }
                        span .k { (status) }
                    }
                }
            }
            p .small {
                "The admin listener binds every address in admin.listen, and every one of them serves "
                "this page. admin.listen, admin.public_url and admin.trusted_proxies live in config.toml. "
                "The device listener binds only when it is switched on below."
            }
        }

        section .card aria-labelledby="provider" {
            h2 id="provider" { "Provider" }
            dl .kv {
                dt { "Endpoint" } dd { (view.provider_base_url.as_deref().unwrap_or("not set")) }
                dt { "Key reference" } dd { (view.provider_key_ref.as_deref().unwrap_or("not set")) }
                dt { "Model" } dd { (view.model.as_deref().unwrap_or("not set")) }
                dt { "Last probe" } dd { (view.probed_at.as_deref().unwrap_or("never")) }
                dt { "Models offered" } dd { (view.model_count) }
            }
            form method="post" action="/dashboard/provider/rotate" novalidate {
                div .actions {
                    button type="submit" class="secondary" { "Rotate: re-read the key and re-probe" }
                }
            }
            p .small {
                "Rotation re-reads the environment variable the key reference names and probes again. The value itself is never stored."
                " The endpoint, the reference and the model are edited on the "
                a href=(Nav::Settings.path()) { "settings page" } "."
            }
        }

        section .card aria-labelledby="devices" {
            h2 id="devices" { "Devices" }
            @if view.devices.is_empty() {
                p .empty {
                    strong { "No device paired yet" }
                    "Mint a code on the "
                    a href=(Nav::Devices.path()) { "devices page" }
                    " and pair the first one."
                }
            } @else {
                table {
                    thead {
                        tr {
                            th scope="col" { "Device" }
                            th scope="col" { "Platform" }
                            th scope="col" { "State" }
                            th scope="col" { "Access" }
                            th scope="col" { "Registered" }
                            th scope="col" { "Action" }
                        }
                    }
                    tbody {
                        @for device in &view.devices {
                            tr {
                                td { (device.name) }
                                td { (device.platform) }
                                td { (device.state_label()) }
                                td .small { (device.admin_label()) }
                                td .mono { (device.paired_at.as_deref().unwrap_or("not yet")) }
                                td {
                                    @if device.revoked_at.is_some() {
                                        span .small { "revoked" }
                                    } @else {
                                        form .inline-form method="post" action="/dashboard/device/revoke" novalidate {
                                            input type="hidden" name="device_id" value=(device.id);
                                            button type="submit" class="danger" { "Revoke" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        section .card aria-labelledby="switches" {
            h2 id="switches" { "Kill switches" }
            p .small {
                "The full permission surface, with a consequence on every option, is on the "
                a href=(Nav::Settings.path()) { "settings page" } "."
            }
            form method="post" action="/dashboard/mode" novalidate {
                (radio_group(
                    "Sandbox mode",
                    "sandbox_mode",
                    &SandboxMode::ALL
                        .iter()
                        .filter(|mode| **mode != SandboxMode::Full)
                        .map(|mode| (mode.as_str(), mode.label(), mode.consequence()))
                        .collect::<Vec<_>>(),
                    view.permissions.sandbox_mode.as_str(),
                    &[],
                    error,
                ))
                (radio_group(
                    "Approval policy",
                    "approval_policy",
                    &ApprovalPolicy::ALL
                        .iter()
                        .map(|policy| (policy.as_str(), policy.label(), policy.consequence()))
                        .collect::<Vec<_>>(),
                    view.permissions.approval_policy.as_str(),
                    &[],
                    error,
                ))
                (radio_group(
                    "Autopilot ceiling",
                    "autopilot_ceiling",
                    &AutopilotCeiling::ALL
                        .iter()
                        .map(|level| (level.as_str(), level.label(), level.consequence()))
                        .collect::<Vec<_>>(),
                    view.permissions.autopilot_ceiling.as_str(),
                    &[],
                    error,
                ))
                (radio_group(
                    "Telemetry",
                    "telemetry",
                    &Telemetry::ALL
                        .iter()
                        .map(|sink| (sink.as_str(), sink.label(), sink.consequence()))
                        .collect::<Vec<_>>(),
                    view.permissions.telemetry.as_str(),
                    &[],
                    error,
                ))
                div .field {
                    label for="device_listener_enabled" {
                        input type="checkbox" id="device_listener_enabled" name="device_listener_enabled" value="true"
                            checked[view.device_listener];
                        " Enable the device listener"
                    }
                    span .hint {
                        "The listener stays on " (DEVICE_ADDR) ", loopback only. This switch is the "
                        "only way to change it, and it is currently " (device_listener_label) "."
                    }
                }
                div .actions {
                    button type="submit" { "Save kill switches" }
                }
            }
            div .mt {
                (permission_table(&view.permission_rows))
            }
        }
    };
    shell(
        "Dashboard",
        view.state,
        Some(Nav::Dashboard),
        body,
        &view.chrome,
    )
}

/// One group of the settings page: one form, one route, one live region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsGroup {
    /// Endpoint, key reference, model.
    Provider,
    /// The sandbox workspace path.
    Workspace,
    /// The permission tuple.
    Permissions,
    /// The assistant's identity: the persona text and its switch.
    Persona,
    /// The generated line on an empty conversation: its switch, model, window.
    Greeting,
}

impl SettingsGroup {
    /// Every group, in page order.
    pub const ALL: [Self; 5] = [
        Self::Provider,
        Self::Workspace,
        Self::Permissions,
        Self::Persona,
        Self::Greeting,
    ];

    /// The anchor and error-region id for this group.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Workspace => "workspace",
            Self::Permissions => "permissions",
            Self::Persona => "persona",
            Self::Greeting => "greeting",
        }
    }

    /// Whether a validation failure belongs to this group.
    ///
    /// The field names are disjoint across the four forms, so the failure
    /// carries its own group and the page never has to guess. A failure with no
    /// field (a database error) belongs to none of them and lands in the page
    /// level region instead.
    #[must_use]
    pub fn owns(self, error: &FormError) -> bool {
        let Some(field) = error.field.as_deref() else {
            return false;
        };
        match self {
            Self::Provider => matches!(field, "base_url" | "api_key_ref" | "model"),
            Self::Workspace => field == "workspace_path",
            Self::Permissions => matches!(
                field,
                "sandbox_mode" | "approval_policy" | "autopilot_ceiling" | "telemetry"
            ),
            Self::Persona => matches!(field, "persona_text" | "persona_enabled"),
            Self::Greeting => matches!(
                field,
                "greeting_enabled" | "greeting_model" | "greeting_refresh"
            ),
        }
    }

    /// The sentence a successful save renders.
    #[must_use]
    pub fn saved_notice(self) -> &'static str {
        match self {
            Self::Provider => "provider settings saved",
            Self::Workspace => "workspace saved",
            Self::Permissions => "permissions saved",
            Self::Persona => "assistant identity saved",
            Self::Greeting => "greeting saved",
        }
    }
}

/// One auxiliary role as the settings page shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuxRow {
    /// The settings-row segment and form field suffix.
    pub key: String,
    /// The label.
    pub label: String,
    /// One line on what the job does.
    pub purpose: String,
    /// The model chosen, or `primary`.
    pub selected: String,
    /// The provider chosen, or `primary`.
    pub provider_selected: String,
    /// What consumes it today, when anything does.
    pub consumed_by: String,
}

/// Everything the settings page needs.
#[derive(Debug, Clone)]
pub struct SettingsView {
    /// The listener truth this page states in its footer.
    pub chrome: Chrome,
    /// The daemon's state, which is `ready` on this page.
    pub state: SetupState,
    /// The version line.
    pub version: String,
    /// True when full host sandbox mode may be selected (flag or stored switch).
    pub allow_full: bool,
    /// Whether the operator's stored switch is on.
    pub allow_full_stored: bool,
    /// Whether this process was started with `--allow-full`.
    pub allow_full_from_flag: bool,
    /// Stored provider base URL.
    pub provider_base_url: String,
    /// Stored key reference. A reference, never a value.
    pub provider_key_ref: String,
    /// Stored default model.
    pub model: String,
    /// The resolved workspace path.
    pub workspace: String,
    /// The permission tuple as stored.
    pub permissions: Permissions,
    /// Devices that are genuinely paired.
    pub paired_devices: usize,
    /// Whether the operator asked for the device listener.
    pub device_listener: bool,
    /// Whether this process has bound the device port.
    pub device_listener_bound: bool,
    /// What the operator calls the assistant.
    pub assistant_name: String,
    /// The persona text the daemon would send, as the file holds it.
    pub persona_text: String,
    /// Whether the persona is switched on.
    pub persona_enabled: bool,
    /// Whether the model's reasoning is shown in a collapsed bubble.
    pub show_reasoning: bool,
    /// Whether the run log is shown at all.
    pub show_tool_log: bool,
    /// `full`, `summary` or `off`, for the select.
    pub tool_log_mode: String,
    /// The device listener's bind addresses, comma separated for the form.
    pub device_listen: String,
    /// Which memory backend a turn recalls from.
    pub memory_backend: String,
    /// The vault directory, when the vault backend is chosen.
    pub memory_vault: String,
    /// The clock every stamp is shown in: `host`, `UTC`, or an offset.
    pub timezone: String,
    /// The file the persona lives in, stated on the page.
    pub persona_path: String,
    /// The stored wallpaper id: a preset name, `custom`, or `none`.
    pub wallpaper: String,
    /// Whether an uploaded image is on disk and usable.
    pub wallpaper_custom: bool,
    /// The provider registry, as the editor shows it.
    pub providers_json: String,
    /// The id of the primary provider.
    pub providers_primary: String,
    /// Every provider id in the registry, for the primary picker.
    pub provider_ids: Vec<String>,
    /// Whether a turn may call tools at all.
    pub tools_enabled: bool,
    /// How many tool rounds one turn may take.
    pub tool_rounds: String,
    /// Whether a turn may dispatch subagents.
    pub agents_enabled: bool,
    /// Whether a generated line is written on an empty conversation at all.
    pub greeting_enabled: bool,
    /// The model that writes it, or `primary` to follow the chat model.
    pub greeting_model: String,
    /// `hour`, `day` or `week`: how old the stored line may get.
    pub greeting_refresh: String,
    /// The line under the salutation, as the form shows it.
    pub chat_hint: String,
    /// One row per auxiliary role: what it is, what it does, what it uses.
    pub aux_roles: Vec<AuxRow>,
    /// The models the last probe returned, for every picker on the page.
    pub models: Vec<String>,
    /// The stored MCP server list, as the editor shows it.
    pub mcp_servers: String,
    /// The last start or call failure per server, so a broken one is visible.
    pub mcp_errors: Vec<(String, String)>,
    /// The sentence stating what a configured server does, and that it is gated.
    pub mcp_status: String,
    /// The failure to show, when this render follows a refused post.
    pub error: Option<FormError>,
    /// The group that was saved and the sentence to show, after a redirect.
    pub notice: Option<(SettingsGroup, String)>,
}

impl SettingsView {
    /// A view with the documented defaults filled in.
    #[must_use]
    pub fn new(state: SetupState, version: impl Into<String>) -> Self {
        Self {
            chrome: Chrome::default(),
            state,
            version: version.into(),
            allow_full: false,
            allow_full_stored: false,
            allow_full_from_flag: false,
            provider_base_url: String::new(),
            provider_key_ref: String::new(),
            model: String::new(),
            workspace: crate::onboarding::workspace::DEFAULT_WORKSPACE.to_owned(),
            permissions: Permissions::DEFAULT,
            paired_devices: 0,
            device_listener: false,
            device_listener_bound: false,
            assistant_name: crate::persona::DEFAULT_ASSISTANT_NAME.to_owned(),
            persona_text: String::new(),
            persona_enabled: true,
            show_reasoning: true,
            show_tool_log: true,
            tool_log_mode: "full".to_owned(),
            device_listen: "127.0.0.1:7341".to_owned(),
            memory_backend: crate::memory::DEFAULT_BACKEND.to_owned(),
            memory_vault: String::new(),
            timezone: crate::zone::HOST.to_owned(),
            persona_path: crate::persona::DEFAULT_PERSONA_PATH.to_owned(),
            wallpaper: wallpaper::DEFAULT_WALLPAPER.to_owned(),
            wallpaper_custom: false,
            providers_json: "[]".to_owned(),
            providers_primary: crate::providers::DEFAULT_ID.to_owned(),
            provider_ids: Vec::new(),
            tools_enabled: true,
            tool_rounds: crate::tools::DEFAULT_TOOL_ROUNDS.to_string(),
            agents_enabled: true,
            greeting_enabled: false,
            greeting_model: crate::greeting::INHERIT.to_owned(),
            greeting_refresh: crate::greeting::Refresh::DEFAULT.as_str().to_owned(),
            chat_hint: crate::greeting::DEFAULT_HINT.to_owned(),
            aux_roles: Vec::new(),
            models: Vec::new(),
            mcp_servers: "[]".to_owned(),
            mcp_errors: Vec::new(),
            mcp_status: crate::mcp::STATUS.to_owned(),
            error: None,
            notice: None,
        }
    }

    /// How many characters the persona text is: the number the page states and
    /// the audit row records.
    #[must_use]
    pub fn persona_characters(&self) -> usize {
        crate::persona::character_count(&self.persona_text)
    }

    /// The failure that belongs to one group, if any.
    #[must_use]
    pub fn group_error(&self, group: SettingsGroup) -> Option<&FormError> {
        self.error.as_ref().filter(|error| group.owns(error))
    }

    /// The notice that belongs to one group, if any.
    #[must_use]
    pub fn group_notice(&self, group: SettingsGroup) -> Option<&str> {
        self.notice
            .as_ref()
            .filter(|(saved, _)| *saved == group)
            .map(|(_, text)| text.as_str())
    }

    /// `never` is offered only when a device is paired and the daemon is ready.
    #[must_use]
    pub fn never_available(&self) -> bool {
        self.paired_devices > 0 && self.state == SetupState::Ready
    }
}

/// A live region for one settings group.
///
/// The id is the group's, so a caller that lands on `#provider` after a save can
/// read the outcome without a script.
fn group_region(view: &SettingsView, group: SettingsGroup) -> Markup {
    let error = view.group_error(group);
    let notice = view.group_notice(group);
    html! {
        div id=(format!("{}-error", group.id())) role="status" aria-live="polite" {
            @if let Some(error) = error {
                div .alert {
                    strong { (error.code) }
                    " "
                    (error.message)
                }
            } @else if let Some(notice) = notice {
                div .alert .notice {
                    strong { "ok" }
                    " "
                    (notice)
                }
            }
        }
    }
}

/// The provider group: endpoint, key reference, model.
fn provider_group(view: &SettingsView) -> Markup {
    let error = view.error.as_ref();
    let focuses = |name: &str| error.is_some_and(|error| error.field.as_deref() == Some(name));
    // The first failing field of this form takes the focus, and only the first.
    let first = ["base_url", "api_key_ref", "model"]
        .into_iter()
        .find(|name| focuses(name));
    html! {
        form method="post" action="/dashboard/settings/provider" novalidate {
            (group_region(view, SettingsGroup::Provider))
            (text_field(
                "base_url",
                "Endpoint",
                &view.provider_base_url,
                Some("An OpenAI-compatible base URL. Must start with http:// or https://."),
                error,
                first == Some("base_url"),
                false,
            ))
            (text_field(
                "api_key_ref",
                "Key reference",
                &view.provider_key_ref,
                Some("env:NAME, with the variable name in capitals. The daemon reads the variable; this page never stores or shows a key value."),
                error,
                first == Some("api_key_ref"),
                false,
            ))
            (text_field(
                "model",
                "Model",
                &view.model,
                Some("The model id every turn starts with. Rotate on the dashboard to re-probe the endpoint."),
                error,
                first == Some("model"),
                false,
            ))
            div .actions {
                button type="submit" { "Save provider" }
            }
            p .small {
                "Saving here writes the three rows and nothing else. It does not call the endpoint: "
                "the rotate button on the dashboard is the probe."
            }
        }
    }
}

/// The workspace group: one path, one form.
fn workspace_group(view: &SettingsView) -> Markup {
    let error = view.error.as_ref();
    let focused = error.is_some_and(|error| error.field.as_deref() == Some("workspace_path"));
    html! {
        form method="post" action="/dashboard/settings/workspace" novalidate {
            (group_region(view, SettingsGroup::Workspace))
            (text_field(
                "workspace_path",
                "Workspace path",
                &view.workspace,
                Some("Created at mode 0755 if absent. Refused if it overlaps ~/.dsh, ~/.config, ~/.ssh or ~/vault, and refused if a relative path or a .. is in it."),
                error,
                focused,
                false,
            ))
            div .actions {
                button type="submit" { "Save workspace" }
            }
            p .small {
                "This is the one directory a turn may write to. Everything outside it is out of reach, "
                "so a path inside a reserved root would hand a turn the daemon's own configuration."
            }
        }
    }
}

/// The permissions group: four closed sets, each with a consequence on the option.
fn permissions_group(view: &SettingsView) -> Markup {
    let error = view.error.as_ref();
    let full_disabled: &[&str] = if view.allow_full { &[] } else { &["full"] };
    let never_disabled: &[&str] = if view.never_available() {
        &[]
    } else {
        &["never"]
    };
    html! {
        form method="post" action="/dashboard/settings/permissions" novalidate {
            (group_region(view, SettingsGroup::Permissions))
            (radio_group(
                "Sandbox mode",
                "sandbox_mode",
                &SandboxMode::ALL
                    .iter()
                    .map(|mode| (mode.as_str(), mode.label(), mode.consequence()))
                    .collect::<Vec<_>>(),
                view.permissions.sandbox_mode.as_str(),
                full_disabled,
                error,
            ))
            @if !view.allow_full {
                p .small {
                    "Full host is off. Turn it on below, or start eris with " code { "--allow-full" }
                    " which overrides the stored switch for the life of the process."
                }
            }
            (radio_group(
                "Approval policy",
                "approval_policy",
                &ApprovalPolicy::ALL
                    .iter()
                    .map(|policy| (policy.as_str(), policy.label(), policy.consequence()))
                    .collect::<Vec<_>>(),
                view.permissions.approval_policy.as_str(),
                never_disabled,
                error,
            ))
            @if !view.never_available() {
                p .small {
                    "Never ask is not selectable yet. It unlocks after a device is paired, which is "
                    "the only proof that someone can answer an approval prompt."
                }
            }
            (radio_group(
                "Autopilot ceiling",
                "autopilot_ceiling",
                &AutopilotCeiling::ALL
                    .iter()
                    .map(|level| (level.as_str(), level.label(), level.consequence()))
                    .collect::<Vec<_>>(),
                view.permissions.autopilot_ceiling.as_str(),
                &[],
                error,
            ))
            (radio_group(
                "Telemetry",
                "telemetry",
                &Telemetry::ALL
                    .iter()
                    .map(|sink| (sink.as_str(), sink.label(), sink.consequence()))
                    .collect::<Vec<_>>(),
                view.permissions.telemetry.as_str(),
                &[],
                error,
            ))
            div .actions {
                button type="submit" { "Save permissions" }
            }
            p .small {
                "L0 notify and L1 draft are the only autopilot levels in 1.0. There is no L2 and no L3, "
                "and no cloud telemetry sink."
            }
        }
    }
}

/// The assistant identity group: one textarea, one switch, one measured size.
///
/// The text is the operator's own words, so it is rendered as an editable value
/// and nothing else: no preview pane, no transformation, and no copy of it in a
/// second place on the page that could disagree with the file.
fn persona_group(view: &SettingsView) -> Markup {
    let error = view.error.as_ref();
    let focuses = |name: &str| error.is_some_and(|error| error.field.as_deref() == Some(name));
    let first = ["persona_text", "persona_enabled"]
        .into_iter()
        .find(|name| focuses(name));
    html! {
        form method="post" action="/dashboard/settings/persona" novalidate {
            (group_region(view, SettingsGroup::Persona))
            div .field {
                label for="assistant_name" { "Name" }
                input id="assistant_name" type="text" name="assistant_name" value=(view.assistant_name)
                    autocomplete="off" spellcheck="false";
                span .hint {
                    "What the chat surface calls the assistant: the composer placeholder and the "
                    "label on every answer. It is a label, not part of the system prompt."
                }
            }
            div .field {
                label for="persona_text" { "Assistant identity" }
                textarea id="persona_text" name="persona_text" rows="12" spellcheck="false"
                    autofocus[first == Some("persona_text")] { (view.persona_text) }
                span .hint {
                    "The file is " code { (view.persona_path) } ". It is read on every turn, so an "
                    "edit here takes effect on the next one without a restart."
                }
            }
            p .small {
                "This text is " (view.persona_characters()) " characters"
                @if view.persona_characters() > 0 {
                    " and is sent on every turn."
                } @else {
                    "."
                }
                " The limit is " (crate::persona::MAX_PERSONA_BYTES) " bytes."
            }
            p .small {
                "Every turn sends this text to the model as the system prompt. Turning it off "
                "sends no system prompt at all, which is what a bare model gets."
            }
            div .field {
                label for="persona_enabled" {
                    input type="checkbox" id="persona_enabled" name="persona_enabled" value="true"
                        autofocus[first == Some("persona_enabled")]
                        checked[view.persona_enabled];
                    " Send the assistant identity on every turn"
                }
                span .hint {
                    "Off by default here is honest: a persona that is switched on with nothing in "
                    "it would send an empty instruction."
                }
            }
            div .actions {
                button type="submit" { "Save assistant identity" }
            }
            p .small {
                "This text is not a secret and is written to disk readable, at mode 0644. The audit "
                "row records only how big it is and whether it is on."
            }
        }
    }
}

/// The thinking bubble: one switch, on by default.
///
/// Presentational only. With it off the reasoning is neither rendered nor
/// streamed, which is the difference between "hidden" and "not sent".
/// Where the daemon's memory comes from.
fn memory_group(view: &SettingsView) -> Markup {
    let error = view.error.as_ref();
    html! {
        form method="post" action=(crate::admin::ROUTE_SETTINGS_MEMORY) novalidate {
            div .field {
                label for="memory_backend" { "Backend" }
                select id="memory_backend" name="memory_backend" {
                    option value="gbrain" selected[view.memory_backend == "gbrain"] {
                        "gbrain - the page store on this host"
                    }
                    option value="vault" selected[view.memory_backend == "vault"] {
                        "Vault - a directory of markdown"
                    }
                    option value="off" selected[view.memory_backend == "off"] {
                        "Off - answer from the prompt alone"
                    }
                }
                span .hint {
                    "Before a turn answers, the daemon asks this backend what it already knows and "
                    "adds what it finds to the prompt. gbrain is the default because it is already "
                    "the long-term brain here."
                }
            }
            div .field {
                label for="memory_vault" { "Vault directory" }
                input id="memory_vault" type="text" name="memory_vault" value=(view.memory_vault)
                    placeholder="~/.vault";
                @if let Some(error) = error.filter(|error| error.field.as_deref() == Some("memory_vault")) {
                    span .error { (error.message) }
                }
                span .hint {
                    "Only read by the vault backend. A path that does not exist is refused rather "
                    "than left to return nothing."
                }
            }
            div .actions {
                button type="submit" { "Save memory" }
            }
        }
    }
}

fn reasoning_group(view: &SettingsView) -> Markup {
    html! {
        form method="post" action=(crate::admin::ROUTE_SETTINGS_REASONING) novalidate {
            div .field {
                label for="show_reasoning" {
                    input type="checkbox" id="show_reasoning" name="show_reasoning" value="true"
                        checked[view.show_reasoning];
                    " Show the model's reasoning in a collapsed bubble"
                }
                span .hint {
                    "The bubble is opaque and closed by default, and it is never part of the answer. "
                    "Off means the reasoning is not rendered and not streamed to the browser at all."
                }
            }
            div .actions {
                button type="submit" { "Save thinking bubble" }
            }
        }
    }
}

/// The generated line on an empty conversation: the switch, the model, the window.
///
/// Off by default, and the card says so: a line that costs a provider call is not
/// something anybody should get without asking for it.
fn greeting_group(view: &SettingsView) -> Markup {
    let error = view.error.as_ref();
    let focuses = |name: &str| error.is_some_and(|error| error.field.as_deref() == Some(name));
    let mut models: Vec<String> = Vec::with_capacity(view.models.len() + 1);
    models.push(crate::greeting::INHERIT.to_owned());
    models.extend(view.models.iter().cloned());
    html! {
        form method="post" action=(crate::admin::ROUTE_SETTINGS_GREETING) novalidate {
            (group_region(view, SettingsGroup::Greeting))
            div .field {
                label for="greeting_enabled" {
                    input type="checkbox" id="greeting_enabled" name="greeting_enabled"
                        value="true" checked[view.greeting_enabled];
                    " Write a line under the salutation"
                }
                span .hint {
                    "It sits under the time of day on an empty conversation. The model is told "
                    "nothing about you: no name, no path, no hostname, no memory, no transcript. "
                    "Off by default, because every rewrite is a billed provider call."
                }
            }
            div .field {
                label for="greeting_model" { "Model" }
                select id="greeting_model" name="greeting_model"
                    autofocus[focuses("greeting_model")] {
                    @for option in &models {
                        @let label = if option == crate::greeting::INHERIT {
                            "primary model"
                        } else {
                            option.as_str()
                        };
                        option value=(option) selected[view.greeting_model == *option] { (label) }
                    }
                }
                @if let Some(error) = error.filter(|e| e.field.as_deref() == Some("greeting_model")) {
                    span .error { (error.message) }
                }
                span .hint {
                    "primary follows whatever the provider section uses. A model pinned here keeps "
                    "that choice when the primary moves."
                }
            }
            div .field {
                label for="chat_hint" { "Line under the salutation" }
                input id="chat_hint" type="text" name="chat_hint" value=(view.chat_hint)
                    maxlength=(crate::greeting::MAX_HINT_CHARS)
                    autofocus[focuses("chat_hint")];
                @if let Some(error) = error.filter(|e| e.field.as_deref() == Some("chat_hint")) {
                    span .error { (error.message) }
                }
                span .hint {
                    "The sentence that sits under the salutation on an empty conversation. Blank "
                    "restores the default. The generated line, when it is on, appears above this."
                }
            }
            div .field {
                label for="greeting_refresh" { "Rewrite it" }
                select id="greeting_refresh" name="greeting_refresh"
                    autofocus[focuses("greeting_refresh")] {
                    @for refresh in crate::greeting::Refresh::ALL {
                        option value=(refresh.as_str())
                            selected[view.greeting_refresh == refresh.as_str()] {
                            (refresh.label())
                        }
                    }
                }
                span .hint {
                    "The stored line is served until it is older than this, so a render only asks a "
                    "model when one is due. With no line stored yet the salutation stands alone."
                }
            }
            div .actions {
                button type="submit" { "Save greeting" }
            }
        }
    }
}

/// The clock the operator reads, as its own form.
///
/// Free text rather than a fixed list: a dropdown of zones would either be a
/// short list that omits his zone or a long one that never ends. The three
/// accepted shapes are stated next to the field.
fn clock_group(view: &SettingsView) -> Markup {
    let error = view.error.as_ref();
    let focuses = |name: &str| error.is_some_and(|error| error.field.as_deref() == Some(name));
    html! {
        form method="post" action=(crate::admin::ROUTE_SETTINGS_TIMEZONE) novalidate {
            div .field {
                label for="timezone" { "Time zone" }
                input id="timezone" type="text" name="timezone" value=(view.timezone)
                    list="timezone-options" autocomplete="off" spellcheck="false"
                    autofocus[focuses("timezone")];
                datalist id="timezone-options" {
                    option value=(crate::zone::HOST);
                    option value="UTC";
                    option value="-04:00";
                    option value="-05:00";
                    option value="-06:00";
                    option value="-07:00";
                    option value="-08:00";
                    option value="+00:00";
                    option value="+01:00";
                    option value="+05:30";
                    option value="+09:00";
                }
                @if let Some(error) = error.filter(|error| error.field.as_deref() == Some("timezone")) {
                    span .error { (error.message) }
                }
                span .hint {
                    "`host` follows this machine and handles daylight saving. `UTC` is UTC. "
                    "An offset like `-04:00` is fixed and does not move when the clocks do."
                }
            }
            div .actions {
                button type="submit" { "Save time zone" }
            }
        }
    }
}

/// The provider registry: every endpoint this daemon may talk to.
fn providers_group(view: &SettingsView) -> Markup {
    let error = view.error.as_ref();
    let focuses = |name: &str| error.is_some_and(|error| error.field.as_deref() == Some(name));
    html! {
        form method="post" action=(crate::admin::ROUTE_SETTINGS_PROVIDERS) novalidate {
            div .field {
                label for="providers_registry" { "Providers" }
                textarea id="providers_registry" name="providers_registry" rows="10"
                    spellcheck="false" autofocus[focuses("providers_registry")] {
                    (view.providers_json)
                }
                @if let Some(error) = error.filter(|error| error.field.as_deref() == Some("providers_registry")) {
                    span .error { (error.message) }
                }
                span .hint {
                    "A JSON array of {\"id\", \"name\", \"base_url\", \"key_ref\"}. The id is "
                    "what auxiliary roles point at. A key reference like env:NAME, never a key."
                }
            }
            div .field {
                label for="providers_primary" { "Primary" }
                select id="providers_primary" name="providers_primary"
                    autofocus[focuses("providers_primary")] {
                    @for id in &view.provider_ids {
                        option value=(id) selected[view.providers_primary == *id] { (id) }
                    }
                }
                @if let Some(error) = error.filter(|error| error.field.as_deref() == Some("providers_primary")) {
                    span .error { (error.message) }
                }
                span .hint {
                    "The endpoint a turn uses unless a role says otherwise. When the registry is "
                    "empty the three provider fields above still seed a default entry."
                }
            }
            div .actions {
                button type="submit" { "Save providers" }
            }
        }
    }
}

/// Tools: whether a turn may call one, and how many rounds it gets.
fn tools_group(view: &SettingsView) -> Markup {
    let error = view.error.as_ref();
    let focuses = |name: &str| error.is_some_and(|error| error.field.as_deref() == Some(name));
    html! {
        form method="post" action=(crate::admin::ROUTE_SETTINGS_TOOLS) novalidate {
            div .field {
                label for="tools_enabled" {
                    input type="checkbox" id="tools_enabled" name="tools_enabled" value="true"
                        checked[view.tools_enabled];
                    " Let a turn call tools"
                }
                span .hint {
                    "Off means the model is offered nothing and answers from the prompt alone. "
                    "On, it can read, write and run commands within the sandbox mode above."
                }
            }
            div .field {
                label for="agents_enabled" {
                    input type="checkbox" id="agents_enabled" name="agents_enabled" value="true"
                        checked[view.agents_enabled];
                    " Let a turn dispatch subagents"
                }
                span .hint {
                    "Offers dispatch_agent, and only in Full sandbox mode. Each subagent is a "
                    "background turn with this daemon's own system prompt and its own budget. It "
                    "gets no tools: it cannot read or write files, run commands, or dispatch "
                    "another subagent. At most " (crate::subagents::MAX_RUNNING) " run at once and "
                    (crate::subagents::MAX_PER_TURN) " per turn, each with a "
                    (crate::subagents::DEADLINE.as_secs()) " second deadline. What it produced "
                    "appears in the subagents control in the top bar, which needs no script."
                }
            }
            div .field {
                label for="tool_log" { "Run log" }
                select id="tool_log" name="tool_log" {
                    option value="full" selected[view.tool_log_mode == "full"] {
                        "Full - one openable window per call"
                    }
                    option value="summary" selected[view.tool_log_mode == "summary"] {
                        "Summary - one collapsed line for the whole turn"
                    }
                    option value="off" selected[view.tool_log_mode == "off"] { "Off - do not show it" }
                }
                span .hint {
                    "What the turn executed. Summary keeps a single line naming the last call, which "
                    "opens to the full list when you want it. Off means it is neither rendered nor "
                    "streamed, so a turn that ran commands shows only its answer."
                }
            }
            div .field {
                label for="tool_rounds" { "Tool rounds per turn" }
                input id="tool_rounds" type="number" name="tool_rounds" value=(view.tool_rounds)
                    min="1" max=(crate::tools::MAX_TOOL_ROUNDS_CEILING) step="1"
                    autofocus[focuses("tool_rounds")];
                @if let Some(error) = error.filter(|error| error.field.as_deref() == Some("tool_rounds")) {
                    span .error { (error.message) }
                }
                span .hint {
                    "How many times the model may call a tool before the last round is forced to "
                    "answer with what it has. Each round is a billed provider call. The ceiling is "
                    (crate::tools::MAX_TOOL_ROUNDS_CEILING) "."
                }
            }
            div .actions {
                button type="submit" { "Save tools" }
            }
        }
    }
}

/// Auxiliary models: one picker per role.
fn aux_group(view: &SettingsView) -> Markup {
    let mut options: Vec<String> = Vec::with_capacity(view.models.len() + 1);
    options.push(crate::aux::INHERIT.to_owned());
    options.extend(view.models.iter().cloned());

    html! {
        form method="post" action=(crate::admin::ROUTE_SETTINGS_AUX) novalidate {
            @for row in &view.aux_roles {
                div .field {
                    label for=(format!("aux_provider_{}", row.key)) { (row.label) }
                    select id=(format!("aux_provider_{}", row.key))
                        name=(format!("aux_provider_{}", row.key)) {
                        option value=(crate::aux::INHERIT)
                            selected[row.provider_selected == crate::aux::INHERIT] {
                            "primary provider"
                        }
                        @for id in &view.provider_ids {
                            option value=(id) selected[row.provider_selected == *id] { (id) }
                        }
                    }
                    select id=(format!("aux_{}", row.key)) name=(format!("aux_{}", row.key))
                        aria-label=(format!("{} model", row.label)) {
                        @for option in &options {
                            option value=(option) selected[row.selected == *option] { (option) }
                        }
                    }
                    span .hint {
                        (row.purpose) " Used by " (&row.consumed_by) "."
                    }
                }
            }
            div .actions {
                button type="submit" { "Save auxiliary models" }
            }
        }
    }
}

/// MCP servers: an editor for a list nothing runs yet.
fn mcp_group(view: &SettingsView) -> Markup {
    let error = view.error.as_ref();
    html! {
        form method="post" action=(crate::admin::ROUTE_SETTINGS_MCP) novalidate {
            div .field {
                label for="mcp_servers" { "Servers" }
                textarea id="mcp_servers" name="mcp_servers" rows="8" spellcheck="false"
                    autofocus[error.is_some_and(|error| error.field.as_deref() == Some("mcp_servers"))] {
                    (view.mcp_servers)
                }
                @if let Some(error) = error.filter(|error| error.field.as_deref() == Some("mcp_servers")) {
                    span .error { (error.message) }
                }
                span .hint {
                    "A JSON array of {\"name\", \"command\", \"args\"}. Validation rejects a "
                    "missing name or command and a duplicate name."
                }
            }
            @if !view.mcp_errors.is_empty() {
                dl .kv {
                    @for (name, error) in &view.mcp_errors {
                        dt { (name) }
                        dd .warn { (error) }
                    }
                }
            }
            p .small .warn { (view.mcp_status) }
            div .actions {
                button type="submit" { "Save MCP servers" }
            }
        }
    }
}

/// The settings page: four forms, then the rows that are read-only by design.
/// The security page: the one surface allowed to ask for a secret.
///
/// The settings page is tested to contain no `type="password"` at all, because
/// there is nothing stored there to mask - only references. Rotating the sign-in
/// credential is the opposite kind of surface, so it gets its own page rather than
/// an exception.
pub fn security_page(view: &SettingsView) -> Markup {
    html! {
        h1 { "Security" }
        p .lede {
            "The credential you sign in with, and nothing else. Changing it signs every device out."
        }
        (alert_region(view.error.as_ref().filter(|error| error.field.is_none()), None))

        section .card aria-labelledby="credential-heading" {
            h2 id="credential-heading" { "Sign-in credential" }
            p .small {
                "The six-word passphrase exists because a fresh host has to be claimable by something "
                "long and unguessable. Once it is yours, a credential you will actually type is a "
                "better trade - as long as it is not four digits."
            }
            form method="post" action=(crate::admin::ROUTE_SECURITY) novalidate {
                div .field {
                    label for="current_password" { "Current credential" }
                    input id="current_password" type="password" name="current_password"
                        autocomplete="current-password" autofocus;
                    @if let Some(error) = view.error.as_ref().filter(|e| e.field.as_deref() == Some("current_password")) {
                        span .error { (error.message) }
                    }
                }
                div .field {
                    label for="new_password" { "New credential" }
                    input id="new_password" type="password" name="new_password"
                        autocomplete="new-password";
                    @if let Some(error) = view.error.as_ref().filter(|e| e.field.as_deref() == Some("new_password")) {
                        span .error { (error.message) }
                    }
                    span .hint {
                        "At least " (crate::web::routes::MIN_SIGN_IN_CHARS)
                        " characters. Stored as an argon2id verifier, never as text."
                    }
                }
                div .field {
                    label for="confirm_password" { "Repeat it" }
                    input id="confirm_password" type="password" name="confirm_password"
                        autocomplete="new-password";
                    @if let Some(error) = view.error.as_ref().filter(|e| e.field.as_deref() == Some("confirm_password")) {
                        span .error { (error.message) }
                    }
                }
                div .actions {
                    button type="submit" { "Change credential" }
                }
            }
            p .small .warn {
                "Every signed-in session is dropped when this changes, including this one, so you will "
                "sign in again with the new credential. There is no recovery e-mail on a self-hosted "
                "daemon: if this is forgotten, the admin_credential row in eris.db is the only way back."
            }
        }
    }
}

pub fn settings_page(view: &SettingsView) -> Markup {
    let page_error = view.error.as_ref().filter(|error| error.field.is_none());
    let body = html! {
        h1 { "Settings" }
        p .lede {
            "Each group is its own form and its own route. A save redirects and says so; "
            "a refusal re-renders this page with the reason next to the field that caused it."
        }
        (alert_region(page_error, None))

        section .card aria-labelledby="sections-heading" {
            h2 id="sections-heading" { "Where everything lives" }
            p .small {
                "The rail is the conversation list. The surfaces that configure this daemon are here, "
                "and the cog in the rail comes back to this page from anywhere."
            }
            (nav_bar(Nav::Settings))
            p .small {
                "The sign-in credential lives on " a href=(crate::admin::ROUTE_SECURITY) { "its own page" }
                ", because this one is never allowed to ask for a secret."
            }
        }

        section .card aria-labelledby="provider-heading" {
            h2 id="provider-heading" { "Provider" }
            p .small {
                "The endpoint every turn is sent to, and the name of the environment variable that "
                "holds its key. A reference travels; a key value never does."
            }
            (provider_group(view))
        }

        section .card aria-labelledby="workspace-heading" {
            h2 id="workspace-heading" { "Workspace" }
            p .small {
                "The one directory a turn may write to."
            }
            (workspace_group(view))
        }

        section .card aria-labelledby="permissions-heading" {
            h2 id="permissions-heading" { "Permissions" }
            p .small {
                "What a turn may do, and who has to approve it. The defaults are the narrow ones: "
                "read only, ask first, drafts only, telemetry off."
            }
            (permissions_group(view))
            dl .kv .mt {
                dt { "Paired devices" } dd { (view.paired_devices) }
                dt { "Device listener" } dd {
                    @if view.device_listener { "on" } @else { "off" }
                    ", "
                    @if view.device_listener_bound { "bound" } @else { "not bound" }
                    " on " (DEVICE_ADDR)
                }
            }
        }

        section .card aria-labelledby="full-access-heading" {
            h2 id="full-access-heading" { "Full host access" }
            p .small {
                "Whether this operator may select full host sandbox mode at all. It is off by "
                "default, it needs a typed confirmation to enable, and every change is audited. "
                "While it is off the option stays greyed out in the permissions table above."
            }
            dl .kv {
                dt { "Stored switch" } dd {
                    @if view.allow_full_stored { "on" } @else { "off" }
                }
                dt { "Command line" } dd {
                    @if view.allow_full_from_flag { "started with --allow-full" } @else { "not passed" }
                }
                dt { "Effective" } dd {
                    @if view.allow_full { "full host selectable" } @else { "refused" }
                }
            }
            form method="post" action="/dashboard/settings/full-access" novalidate {
                @if !view.allow_full_stored {
                    div .field {
                        label for="confirm" { "Type full to enable" }
                        input id="confirm" type="text" name="confirm" value=""
                            autocomplete="off" spellcheck="false"
                            placeholder="full";
                        span .hint {
                            "This unlocks command execution on the whole host for every turn. The "
                            "rest of the daemon does not change; this is the one switch that decides "
                            "whether the sandbox may be opened."
                        }
                    }
                    div .actions {
                        button type="submit" { "Enable full host" }
                    }
                } @else {
                    p .small {
                        "Full host is enabled. Turning it off refuses the option immediately; a turn "
                        "that is already running is not interrupted."
                    }
                    div .actions {
                        button type="submit" class="danger" { "Disable full host" }
                    }
                }
            }
        }

        section .card aria-labelledby="persona-heading" {
            h2 id="persona-heading" { "Assistant identity" }
            p .small {
                "Who this assistant is. The text here is the system prompt every turn carries, and "
                "it is the one setting that changes how the model talks rather than what it can reach."
            }
            (persona_group(view))
        }

        section .card aria-labelledby="reasoning-heading" {
            h2 id="reasoning-heading" { "Thinking bubble" }
            p .small {
                "Some models send a separate reasoning stream that is not the reply. When this is on, "
                "it appears above the answer in a collapsed, faded bubble you can open."
            }
            (reasoning_group(view))
        }

        section .card aria-labelledby="greeting-heading" {
            h2 id="greeting-heading" { "Greeting" }
            p .small {
                "The salutation on an empty conversation comes from the hour. This card adds a "
                "second line under it, written by a model, cached in the database, and rewritten "
                "only when it goes stale."
            }
            (greeting_group(view))
        }

        section .card aria-labelledby="device-bind-heading" {
            h2 id="device-bind-heading" { "Device listener" }
            p .small {
                "Where paired phones reach this daemon. Loopback only by default, which is why an "
                "iPhone cannot pair until you add an address it can route to - your tailnet address, "
                "or the LAN address if you are on the same network."
            }
            form method="post" action=(crate::admin::ROUTE_SETTINGS_DEVICE) novalidate {
                div .field {
                    label for="device_listen" { "Bind addresses" }
                    input id="device_listen" type="text" name="device_listen" value=(view.device_listen)
                        placeholder="127.0.0.1:7341, 100.x.y.z:7341";
                    @if let Some(error) = view.error.as_ref().filter(|e| e.field.as_deref() == Some("device_listen")) {
                        span .error { (error.message) }
                    }
                    span .hint {
                        "Comma separated. The device API is /v1 only and every call needs a paired "
                        "device token, so opening an address does not open the dashboard. Applied at "
                        "the next start: a bind is not moved under a live phone."
                    }
                }
                div .actions {
                    button type="submit" { "Save bind addresses" }
                }
            }
        }

        section .card aria-labelledby="memory-heading" {
            h2 id="memory-heading" { "Memory" }
            p .small {
                "Recalled notes are added to the prompt as context and marked as possibly out of "
                "date. A backend that cannot answer is recorded and the turn goes on without it."
            }
            (memory_group(view))
        }

        section .card aria-labelledby="clock-heading" {
            h2 id="clock-heading" { "Clock" }
            p .small {
                "Every stamp in the transcript and the rail is shown in this zone. Timestamps are "
                "stored in UTC and converted for display, so changing this never rewrites history."
            }
            (clock_group(view))
        }


        section .card aria-labelledby="providers-heading" {
            h2 id="providers-heading" { "Providers" }
            p .small {
                "Every endpoint this daemon may talk to. One of them is primary, and the auxiliary "
                "roles below can point at any of them."
            }
            (providers_group(view))
        }

        section .card aria-labelledby="tools-heading" {
            h2 id="tools-heading" { "Tools" }
            p .small {
                "What a turn may call, and how long it may keep calling. The sandbox mode above "
                "decides what each tool is allowed to touch."
            }
            (tools_group(view))
        }

        section .card aria-labelledby="aux-heading" {
            h2 id="aux-heading" { "Auxiliary models" }
            p .small {
                "A turn answers with the primary model. These are the jobs beside it. A role set to "
                "primary follows whatever the provider section uses; a role pinned to a model keeps "
                "that model when the primary moves."
            }
            p .small {
                "Pinning a model to a role is what turns that role on: left on primary, the role does "
                "nothing, because re-running the primary model for a job the turn already did would be "
                "cost with no answer behind it. Each row states whether anything reads it yet."
            }
            (aux_group(view))
        }

        section .card aria-labelledby="mcp-heading" {
            h2 id="mcp-heading" { "MCP servers" }
            (mcp_group(view))
        }

        section .card aria-labelledby="appearance-heading" {
            h2 id="appearance-heading" { "Appearance" }
            p .small {
                "The chat backdrop. It sits behind the conversation only, so the chrome keeps the "
                "fixed palette and the text on top never depends on which picture is loaded."
            }
            form method="post" action=(wallpaper::ROUTE_WALLPAPER_SAVE) novalidate {
                div .choices {
                    @for preset in wallpaper::PRESETS {
                        @let chosen = view.wallpaper == preset.id;
                        div .choice .selected[chosen] {
                            input type="radio" id=(format!("wallpaper-{}", preset.id))
                                name="wallpaper" value=(preset.id) checked[chosen];
                            label for=(format!("wallpaper-{}", preset.id)) {
                                span .name { (preset.label) }
                                span .why { (preset.hint) }
                            }
                        }
                    }
                    @if view.wallpaper_custom {
                        @let chosen = view.wallpaper == wallpaper::CUSTOM;
                        div .choice .selected[chosen] {
                            input type="radio" id="wallpaper-custom" name="wallpaper"
                                value=(wallpaper::CUSTOM) checked[chosen];
                            label for="wallpaper-custom" {
                                span .name { "Custom image" }
                                span .why { "The image you uploaded below." }
                            }
                        }
                    }
                }
                div .actions {
                    button type="submit" { "Save wallpaper" }
                }
            }
            h3 .mt { "Upload an image" }
            form method="post" action=(wallpaper::ROUTE_WALLPAPER_UPLOAD)
                enctype="multipart/form-data" novalidate {
                div .field {
                    label for="wallpaper-image" { "PNG, JPEG or WebP" }
                    input type="file" id="wallpaper-image" name="image"
                        accept="image/png,image/jpeg,image/webp";
                    span .hint {
                        "Up to 8 MiB, checked by the bytes rather than by the filename. SVG is "
                        "refused: an SVG can carry script, and a wallpaper is not worth an injection."
                    }
                }
                div .actions {
                    button type="submit" class="secondary" { "Upload and use" }
                }
            }
        }

        section .card aria-labelledby="read-only-heading" {
            h2 id="read-only-heading" { "Read-only" }
            p .small {
                "These rows describe 1.0. They have no control here because there is nothing behind "
                "them to switch."
            }
            table {
                caption {
                    "What 1.0 does not do, stated on the page rather than left to a guess."
                }
                thead {
                    tr {
                        th scope="col" { "Setting" }
                        th scope="col" { "Value" }
                        th scope="col" { "What that means" }
                    }
                }
                tbody {
                    tr .locked {
                        th scope="row" { "External communication" }
                        td .value { "draft only" " " span .tag { "locked" } }
                        td .why { "1.0 never transmits in your voice. A draft stops at the review screen." }
                    }
                    tr .locked {
                        th scope="row" { "Financial" }
                        td .value { "disabled" " " span .tag { "locked" } }
                        td .why { "No payment, banking, or brokerage action exists in 1.0." }
                    }
                    tr .locked {
                        th scope="row" { "Network" }
                        td .value { "configured" " " span .tag { "config.toml" } }
                        td .why {
                            "The UI can be reached on this host, on a LAN address, on a tailnet "
                            "address, or through a reverse proxy in front of any of them. That is "
                            "decided by admin.listen, admin.public_url and admin.trusted_proxies in "
                            "config.toml, not by anything compiled in. The device listener stays on "
                            (DEVICE_ADDR) ", loopback only, and the assistant itself reaches only the "
                            "provider endpoint you configure."
                        }
                    }
                    tr {
                        th scope="row" { "Telemetry" }
                        td .value { (view.permissions.telemetry.label()) }
                        td .why { (view.permissions.telemetry.consequence()) }
                    }
                    tr .locked {
                        th scope="row" { "Admin listener" }
                        td .value { (view.chrome.listens_line()) " " span .tag { "config.toml" } }
                        td .why {
                            "Set by admin.listen in config.toml, plus --admin-listen on the command "
                            "line, which adds to the list. Every listed address is served at once. "
                            "0.0.0.0 binds every interface this host has and is a decision to make on "
                            "purpose, out loud."
                        }
                    }
                }
            }
        }
    };
    shell(
        "Settings",
        view.state,
        Some(Nav::Settings),
        body,
        &view.chrome,
    )
}

/// Everything the activity page needs.
#[derive(Debug, Clone)]
pub struct ActivityView {
    /// The listener truth this page states in its footer.
    pub chrome: Chrome,
    /// The daemon's state.
    pub state: SetupState,
    /// The newest audit rows, newest first.
    pub rows: Vec<AuditEntry>,
    /// How many rows the table holds in total.
    pub total: i64,
    /// A failure to show, when the read failed.
    pub error: Option<FormError>,
    /// A success line, unused today but the page shares the shell's contract.
    pub notice: Option<String>,
}

impl ActivityView {
    /// An empty view, for the failure path.
    #[must_use]
    pub fn new(state: SetupState) -> Self {
        Self {
            chrome: Chrome::default(),
            state,
            rows: Vec::new(),
            total: 0,
            error: None,
            notice: None,
        }
    }
}

/// How many characters of a detail line the table shows.
///
/// An audit detail is a label or an id, and a 400 character one is a paste
/// accident. Truncating keeps the table readable without hiding the row.
const DETAIL_DISPLAY_LIMIT: usize = 120;

/// One detail cell, truncated with an ASCII ellipsis.
#[must_use]
pub fn display_detail(detail: Option<&str>) -> String {
    let Some(detail) = detail else {
        return "-".to_owned();
    };
    if detail.chars().count() <= DETAIL_DISPLAY_LIMIT {
        return detail.to_owned();
    }
    let mut out: String = detail.chars().take(DETAIL_DISPLAY_LIMIT).collect();
    out.push_str("...");
    out
}

/// The audit trail, newest first, server-rendered.
pub fn activity_page(view: &ActivityView) -> Markup {
    let error = view.error.as_ref();
    let body = html! {
        h1 { "Activity" }
        p .lede {
            "The last hundred rows of the audit log, newest first. Every settings save, every "
            "sign-in, every device change lands here, and nothing in the log can be edited from a page."
        }
        (alert_region(error, view.notice.as_deref()))
        section .card aria-labelledby="audit-heading" {
            h2 id="audit-heading" { "Audit log" }
            @if view.rows.is_empty() {
                p .empty {
                    strong { "Nothing recorded yet" }
                    "No audit row has been written yet."
                }
            } @else {
                table {
                    thead {
                        tr {
                            th scope="col" { "Time" }
                            th scope="col" { "Actor" }
                            th scope="col" { "Action" }
                            th scope="col" { "Detail" }
                        }
                    }
                    tbody {
                        @for row in &view.rows {
                            tr {
                                td .mono { (row.occurred_at) }
                                td { (row.actor) }
                                td { (row.action) }
                                td .why.mono { (display_detail(row.detail.as_deref())) }
                            }
                        }
                    }
                }
            }
            p .small {
                "Showing "
                (view.rows.len())
                " of "
                (view.total)
                " rows. The table is the source of truth; this page is a read of it."
            }
        }
    };
    shell(
        "Activity",
        view.state,
        Some(Nav::Activity),
        body,
        &view.chrome,
    )
}

/// Everything the backups page needs.
#[derive(Debug, Clone)]
pub struct BackupsView {
    /// The listener truth this page states in its footer.
    pub chrome: Chrome,
    /// The daemon's state.
    pub state: SetupState,
    /// Every archive in the archive directory, newest first, each verified.
    pub rows: Vec<crate::web::backups::ArchiveRow>,
    /// A success line from the redirect that followed a create or a restore.
    pub notice: Option<String>,
    /// A failure to show, when a create or a restore was refused.
    pub error: Option<FormError>,
}

impl BackupsView {
    /// An empty view for a given state.
    #[must_use]
    pub fn new(state: SetupState) -> Self {
        Self {
            chrome: Chrome::default(),
            state,
            rows: Vec::new(),
            notice: None,
            error: None,
        }
    }
}

/// The backups page: the create button, the archive table, and the restore form
/// that lives in each row.
///
/// The restore form asks for the filename twice - once hidden, once typed - and
/// the typed value has to be the exact filename. That is not ceremony: a restore
/// replaces the live database, and a button that could be hit by reflex is a
/// button that eventually is.
///
/// The page also states the one consequence it cannot perform for the operator:
/// the running daemon still holds the replaced database file open, so the new
/// state only takes effect after a service restart. The route never restarts it.
pub fn backups_page(view: &BackupsView) -> Markup {
    let body = html! {
        h1 { "Backups" }
        p .lede {
            "Every archive on this host, in one directory, with whether each one still verifies. "
            "Creating one snapshots the live database rather than copying the file, so a backup "
            "taken while the daemon runs is a complete one."
        }
        (alert_region(view.error.as_ref(), view.notice.as_deref()))
        section .card aria-labelledby="create-heading" {
            h2 id="create-heading" { "Create an archive" }
            p .small {
                "One file in the archive directory on this host, verified as it is written. It "
                "carries the database, the session logs, and the two config files. The onboarding "
                "passphrase is never inside one."
            }
            form method="post" action=(crate::admin::ROUTE_BACKUPS_CREATE) novalidate {
                div .actions {
                    button type="submit" { "Create a backup now" }
                }
            }
        }
        section .card aria-labelledby="archives-heading" {
            h2 id="archives-heading" { "Archives" }
            @if view.rows.is_empty() {
                p .empty {
                    strong { "No archive yet" }
                    "No archive exists yet. Create one now, and it will appear here with its size "
                    "and whether every member verifies against its manifest."
                }
            } @else {
                table {
                    thead {
                        tr {
                            th scope="col" { "Archive" }
                            th scope="col" { "Written" }
                            th scope="col" { "Size" }
                            th scope="col" { "Verifies" }
                            th scope="col" { "Action" }
                        }
                    }
                    tbody {
                        @for row in &view.rows {
                            @let confirm_id = format!("confirm-{}", row.name);
                            tr {
                                td .mono { (row.name) }
                                td .mono { (row.modified) }
                                td .mono { (row.bytes) " bytes" }
                                td {
                                    span class=(state_class(row.state_label())) {
                                        (row.state_label())
                                    }
                                    div .small { (row.verification_line()) }
                                }
                                td {
                                    form .stack method="post"
                                        action=(crate::admin::ROUTE_BACKUPS_RESTORE) novalidate {
                                        input type="hidden" name="archive" value=(row.name);
                                        label for=(confirm_id) { "Type the filename to confirm" }
                                        input type="text" id=(confirm_id) name="confirm"
                                            autocomplete="off" spellcheck="false" required;
                                        button type="submit" class="danger" {
                                            "Restore this archive"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        section .card aria-labelledby="restart-heading" {
            h2 id="restart-heading" { "A restore needs a restart" }
            p {
                "A restore replaces the state files on disk. This page does not restart the "
                "daemon and will not: the running daemon keeps its open handle to the replaced "
                "database file, so the state it serves stays the old one until the service is "
                "restarted."
            }
            p .small {
                "Run "
                code { "systemctl --user restart kallisti-eris" }
                " after a restore for the new state to take effect. Nothing is lost in the "
                "meantime: what was replaced is moved to a pre-restore directory beside the "
                "database, and the audit log names it."
            }
        }
    };
    shell(
        "Backups",
        view.state,
        Some(Nav::Backups),
        body,
        &view.chrome,
    )
}

/// A standalone error page, for a 403 or a 409 that a browser will read.
pub fn error_page(state: SetupState, code: &str, message: &str, chrome: &Chrome) -> Markup {
    let body = html! {
        h1 { "Not available" }
        (alert_region(Some(&FormError::new(None, code, message)), None))
        p .lede {
            "This is the admin listener on 127.0.0.1. Nothing else is reachable here."
        }
        div .actions {
            a href="/" { "Back to the start" }
        }
    };
    layout("Not available", state, body, chrome)
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// How many characters of a user agent the sessions table shows.
const AGENT_LIMIT: usize = 60;

/// How many characters of a job's request text a job block shows.
pub const REQUEST_LIMIT: usize = 120;

/// How many characters of an event payload the timeline shows.
const PAYLOAD_LIMIT: usize = 80;

/// One string for display: whole when it fits, truncated with an ASCII ellipsis
/// when it does not, and a dash when there is nothing.
#[must_use]
pub fn display_text(text: Option<&str>, limit: usize) -> String {
    let Some(text) = text else {
        return "-".to_owned();
    };
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push_str("...");
    out
}

/// The class a session or job state carries. The word is always printed too, so
/// colour is never the only signal.
fn state_class(state: &str) -> &'static str {
    match state {
        "live" | "succeeded" | "paired" | "verified" => "badge live",
        "running" | "expired" | "awaiting code" => "badge expired",
        _ => "badge revoked",
    }
}

/// Everything the sessions list needs.
#[derive(Debug, Clone)]
pub struct SessionsView {
    /// The listener truth this page states in its footer.
    pub chrome: Chrome,
    /// The daemon's state.
    pub state: SetupState,
    /// Every session, newest first.
    pub rows: Vec<SessionRow>,
    /// A failure to show, when a read failed.
    pub error: Option<FormError>,
    /// A success line, unused today but the shell's contract is shared.
    pub notice: Option<String>,
}

impl SessionsView {
    /// An empty view for a given state.
    #[must_use]
    pub fn new(state: SetupState) -> Self {
        Self {
            chrome: Chrome::default(),
            state,
            rows: Vec::new(),
            error: None,
            notice: None,
        }
    }
}

/// Everything one session's page needs.
#[derive(Debug, Clone)]
pub struct SessionDetailView {
    /// The listener truth this page states in its footer.
    pub chrome: Chrome,
    /// The daemon's state.
    pub state: SetupState,
    /// The session itself.
    pub session: SessionRow,
    /// Its jobs, newest first, each with its events.
    pub jobs: Vec<JobRow>,
    /// A failure to show, when a cancel was refused.
    pub error: Option<FormError>,
    /// A success line, when a job was just cancelled.
    pub notice: Option<String>,
}

impl SessionDetailView {
    /// A view around one session, with no jobs read yet.
    #[must_use]
    pub fn new(state: SetupState, session: SessionRow) -> Self {
        Self {
            chrome: Chrome::default(),
            state,
            session,
            jobs: Vec::new(),
            error: None,
            notice: None,
        }
    }
}

/// The sessions list: the table, or one friendly line when there is nothing.
pub fn sessions_page(view: &SessionsView) -> Markup {
    let body = html! {
        h1 { "Sessions" }
        p .lede {
            "Every device session this host has opened, newest first. A session is one sign-in; "
            "the jobs it started and every event they recorded are on its own page."
        }
        (alert_region(view.error.as_ref(), view.notice.as_deref()))
        section .card aria-labelledby="sessions-heading" {
            h2 id="sessions-heading" { "Device sessions" }
            @if view.rows.is_empty() {
                p .empty {
                    strong { "No device session yet" }
                    "No device session has been opened yet. A session appears here the first time "
                    "a paired device signs in through the device listener, and this page will list "
                    "it with the jobs it ran."
                }
            } @else {
                table {
                    thead {
                        tr {
                            th scope="col" { "Created" }
                            th scope="col" { "Device" }
                            th scope="col" { "Agent" }
                            th scope="col" { "Last seen" }
                            th scope="col" { "Expires" }
                            th scope="col" { "State" }
                            th scope="col" { "Jobs" }
                        }
                    }
                    tbody {
                        @for row in &view.rows {
                            tr {
                                td .mono {
                                    a href=(crate::admin::session_path(&row.id)) { (row.created_at) }
                                }
                                td { (row.device_name.as_deref().unwrap_or("unknown")) }
                                td .why { (display_text(row.user_agent.as_deref(), AGENT_LIMIT)) }
                                td .mono { (row.last_seen_at.as_deref().unwrap_or("never")) }
                                td .mono { (row.expires_at) }
                                td { span class=(state_class(&row.state)) { (row.state) } }
                                td .mono { (row.job_count) }
                            }
                        }
                    }
                }
                p .small {
                    (view.rows.len())
                    (if view.rows.len() == 1 { " session, " } else { " sessions, " })
                    "revoked and expired ones included. Open one to read its jobs and its events."
                }
            }
        }
    };
    shell(
        "Sessions",
        view.state,
        Some(Nav::Sessions),
        body,
        &view.chrome,
    )
}

/// One job: its status, the facts about it, a cancel button while it can still
/// be cancelled, and its events as a timeline in `seq` order.
fn job_block(job: &JobRow) -> Markup {
    html! {
        article .job aria-label=(format!("Job {}", job.id)) {
            div .job-head {
                span class=(state_class(&job.status)) { (job.status) }
                span .mono { (job.id) }
                span .small { "kind " (job.kind) ", attempts " (job.attempts) }
            }
            dl .kv {
                dt { "Request" }
                dd .why { (display_text(job.request_text.as_deref(), REQUEST_LIMIT)) }
                dt { "Created" }
                dd { (job.created_at) }
                dt { "Finished" }
                dd { (job.finished_at.as_deref().unwrap_or("not yet")) }
                @if job.error_code.is_some() || job.error_message.is_some() {
                    dt { "Error" }
                    dd .why {
                        (job.error_code.as_deref().unwrap_or("error"))
                        " "
                        (job.error_message.as_deref().unwrap_or(""))
                    }
                }
            }
            @if job.is_cancellable() {
                form .inline-form method="post" action=(crate::admin::job_cancel_path(&job.id)) novalidate {
                    button type="submit" class="danger" { "Cancel job" }
                }
            }
            h3 { "Events" }
            @if job.events.is_empty() {
                p .empty { "No event has been recorded for this job yet." }
            } @else {
                ol .timeline {
                    @for event in &job.events {
                        li {
                            span .mono { (event.occurred_at) }
                            span .kind { (event.kind) }
                            span .why {
                                "seq " (event.seq) ": "
                                (display_text(event.payload.as_deref(), PAYLOAD_LIMIT))
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One session: its identity, then its jobs and their events.
pub fn session_page(view: &SessionDetailView) -> Markup {
    let session = &view.session;
    let where_it_came_from = session.user_agent.as_deref().unwrap_or("unknown");
    let body = html! {
        h1 { "Session" }
        p .lede {
            "One device session, the jobs it started, and every event each job recorded."
        }
        (alert_region(view.error.as_ref(), view.notice.as_deref()))
        section .card aria-labelledby="session-heading" {
            h2 id="session-heading" { "Identity" }
            dl .kv {
                dt { "Session" } dd { (session.id) }
                dt { "Device" }
                dd { (session.device_name.as_deref().unwrap_or("unknown")) }
                dt { "User agent" } dd .why { (where_it_came_from) }
                dt { "Created" } dd { (session.created_at) }
                dt { "Last seen" } dd { (session.last_seen_at.as_deref().unwrap_or("never")) }
                dt { "Expires" } dd { (session.expires_at) }
                dt { "Revoked" } dd { (session.revoked_at.as_deref().unwrap_or("no")) }
                dt { "State" } dd { span class=(state_class(&session.state)) { (session.state) } }
                dt { "Jobs" } dd { (session.job_count) }
            }
            p .small {
                "A session is a sign-in, not a device: a device that signs in twice has two of "
                "these, and revoking the device closes both."
            }
        }
        section .card aria-labelledby="jobs-heading" {
            h2 id="jobs-heading" { "Jobs" }
            @if view.jobs.is_empty() {
                p .empty {
                    strong { "No jobs yet" }
                    "This session has not started a job yet. A job appears here when the device "
                    "asks for a turn through the device listener."
                }
            } @else {
                p .small {
                    "Newest first. A cancel is offered only while a job is queued or running, and "
                    "it writes an audit row."
                }
                @for job in &view.jobs {
                    (job_block(job))
                }
            }
        }
        div .actions {
            a .button.secondary href=(Nav::Sessions.path()) { "All sessions" }
            a .button.secondary href=(Nav::Devices.path()) { "Devices" }
        }
    };
    shell(
        "Session",
        view.state,
        Some(Nav::Sessions),
        body,
        &view.chrome,
    )
}

// ---------------------------------------------------------------------------
// Devices
// ---------------------------------------------------------------------------

/// The longest device name the form accepts. A name is a label in a table, not
/// a place to paste a paragraph.
pub const DEVICE_NAME_LIMIT: usize = 64;

/// A pairing code that was just minted, on its way to the one response that will
/// ever carry it.
#[derive(Debug, Clone)]
pub struct IssuedCodeView {
    /// The eight characters, read from the pairing service once.
    pub code: String,
    /// When it stops redeeming.
    pub expires_at: String,
    /// The address a phone should call, as the QR carries it.
    pub relay: String,
    /// The pairing payload drawn as an SVG a camera can read.
    pub qr: String,
}

/// Everything the devices page needs.
#[derive(Debug, Clone)]
pub struct DevicesView {
    /// The listener truth this page states in its footer.
    pub chrome: Chrome,
    /// The daemon's state.
    pub state: SetupState,
    /// Every device, newest first, revoked ones included.
    pub rows: Vec<DeviceOverview>,
    /// The code minted by this response, if this render follows a mint. It is
    /// never read back from the database: there is nothing there to read.
    pub issued: Option<IssuedCodeView>,
    /// A failure to show, when a mint was refused.
    pub error: Option<FormError>,
    /// A success line for anything other than a mint.
    pub notice: Option<String>,
}

impl DevicesView {
    /// An empty view for a given state.
    #[must_use]
    pub fn new(state: SetupState) -> Self {
        Self {
            chrome: Chrome::default(),
            state,
            rows: Vec::new(),
            issued: None,
            error: None,
            notice: None,
        }
    }
}

/// The devices page: the table, the one-shot code when there is one, and the
/// form that mints the next one.
pub fn devices_page(view: &DevicesView) -> Markup {
    let body = html! {
        h1 { "Devices" }
        p .lede {
            "Every device this host knows about, paired or not, and the one form that mints a "
            "pairing code for the next one."
        }
        (alert_region(view.error.as_ref(), view.notice.as_deref()))
        @if let Some(issued) = &view.issued {
            section .card aria-labelledby="issued-heading" {
                h2 id="issued-heading" { "Pairing code" }
                p .small {
                    "This code is shown once, in this response and nowhere else. Only a salted "
                    "verifier is stored, so it cannot be shown again. Type it on the device before "
                    "it expires."
                }
                p .pairing-code .mono { (issued.code) }
                p .small { "Expires " (issued.expires_at) "." }
                @if !issued.qr.is_empty() {
                    div .pairing-scan {
                        div .pairing-qr { (PreEscaped(&issued.qr)) }
                        p .small {
                            "Or scan it. The code carries the address your phone should reach: "
                            span .mono { (&issued.relay) }
                        }
                    }
                }
            }
        }
        section .card aria-labelledby="devices-heading" {
            h2 id="devices-heading" { "Devices" }
            @if view.rows.is_empty() {
                p .small {
                    "No device has been registered yet. Mint a code below and pair one, or finish "
                    "the device step in the wizard."
                }
            } @else {
                table {
                    thead {
                        tr {
                            th scope="col" { "Name" }
                            th scope="col" { "Platform" }
                            th scope="col" { "Paired" }
                            th scope="col" { "Last seen" }
                            th scope="col" { "State" }
                            th scope="col" { "Access" }
                            th scope="col" { "Action" }
                        }
                    }
                    tbody {
                        @for row in &view.rows {
                            tr {
                                td { (row.name) }
                                td { (row.platform) }
                                td .mono {
                                    @if row.paired {
                                        (row.paired_at.as_deref().unwrap_or("not yet"))
                                    } @else {
                                        "not yet"
                                    }
                                }
                                td .mono { (row.last_seen_at.as_deref().unwrap_or("never")) }
                                td {
                                    span class=(state_class(row.state_label())) {
                                        (row.state_label())
                                    }
                                }
                                td .small { (row.admin_label()) }
                                td {
                                    @if row.revoked_at.is_some() {
                                        span .small { "revoked" }
                                    } @else {
                                        form .inline-form method="post"
                                            action="/dashboard/device/revoke" novalidate {
                                            input type="hidden" name="device_id" value=(row.id);
                                            button type="submit" class="danger" { "Revoke" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        section .card aria-labelledby="pair-heading" {
            h2 id="pair-heading" { "Mint a pairing code" }
            form method="post" action=(crate::admin::ROUTE_DEVICES) novalidate {
                div .field {
                    label for="device_name" { "Device name" }
                    input type="text" id="device_name" name="device_name" value=""
                        autocomplete="off" maxlength=(DEVICE_NAME_LIMIT) required;
                    span .hint {
                        "The name the device appears under in the table above. A name that is "
                        "already known binds the code to that device instead of adding a second row."
                    }
                }
                div .field {
                    label for="allow_admin" {
                        input type="checkbox" id="allow_admin" name="allow_admin" value="true";
                        " Allow this device to administer"
                    }
                    span .hint {
                        "Ticked, the code redeems into a token that may read and change the "
                        "operator's settings as well as chat; unticked, it can chat and file "
                        "conversations and nothing more."
                    }
                }
                div .actions {
                    button type="submit" { "Mint pairing code" }
                }
            }
            p .small {
                "One code per press: eight characters, five minutes, single use. The code is never "
                "written to the database in readable form, never logged, and never recorded in the "
                "audit log, which notes the device name and whether administration was asked for."
            }
        }
    };
    shell(
        "Devices",
        view.state,
        Some(Nav::Devices),
        body,
        &view.chrome,
    )
}

/// One line of the conversation, as the page renders it.
///
/// The body is raw text and is rendered by [`blocks`]: an assistant answer can
/// carry a fenced code block, and a chat that renders code as one grey paragraph
/// is not a chat worth using.
#[derive(Debug, Clone)]
pub struct ChatLine {
    /// True for the operator's own message.
    pub user: bool,
    /// The name over the bubble.
    pub who: String,
    /// What was said, so far.
    pub body: String,
    /// The clock time, for the line under the bubble.
    pub stamp: String,
    /// True when the turn was refused.
    pub failed: bool,
    /// The refusal's code, when it was.
    pub error_code: Option<String>,
    /// True while the answer is still arriving.
    pub pending: bool,
    /// The model that answered this row, when it is an answer.
    pub model: Option<String>,
    /// Prompt tokens the provider reported.
    pub prompt_tokens: Option<u64>,
    /// Completion tokens the provider reported.
    pub completion_tokens: Option<u64>,
    /// Prompt tokens served from cache.
    pub cached_tokens: Option<u64>,
    /// Wall time from request to last byte.
    pub duration_ms: Option<u64>,
    /// The context window declared for the model that answered.
    pub context_window: Option<u64>,
    /// The model's own reasoning for this reply, when it streamed any.
    ///
    /// Rendered above the answer, and only when the thinking bubble is switched
    /// on.
    pub reasoning: Option<String>,
    /// The tool calls behind this reply, in the order they ran.
    pub tools: Vec<crate::web::chat::ToolRecord>,
    /// The files this message carried, by name and size.
    ///
    /// Kept on the line rather than looked up per message: the page reads them
    /// in one query and hands each row its own list, so a transcript with forty
    /// turns costs the same as one with four.
    pub files: Vec<Attachment>,
}

/// One file a message carried, as the transcript states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// What it was called when it arrived.
    pub name: String,
    /// Size in bytes, for the chip.
    pub size: u64,
    /// Whether it can be shown as a picture rather than named.
    pub is_image: bool,
    /// Where the browser may fetch it, when the file is one this route serves.
    pub url: Option<String>,
    /// The three or four characters that stand in for a type icon.
    ///
    /// A picture is shown as the picture, so this is what a file that cannot be
    /// shown gets instead of a blank square: `PDF`, `TXT`, `FILE`.
    pub badge: &'static str,
}

/// The badge a chip shows for a file of this type.
///
/// Read from the type the bytes were sniffed to be when the file landed, so the
/// chip names what a click actually downloads. A row recorded before sniffing
/// existed carries an extension-derived value and may disagree with the bytes;
/// the serving route is the authority either way.
#[must_use]
pub fn type_badge(media_type: Option<&str>) -> &'static str {
    match media_type {
        Some("image/png") => "PNG",
        Some("image/jpeg") => "JPG",
        Some("image/webp") => "WEBP",
        Some("image/gif") => "GIF",
        Some("application/pdf") => "PDF",
        Some("text/plain") => "TXT",
        Some("text/markdown") => "MD",
        Some("text/csv") => "CSV",
        Some("application/json") => "JSON",
        Some("application/zip") => "ZIP",
        _ => "FILE",
    }
}

/// A size for a human: `812 B`, `14 kB`, `3.4 MB`.
///
/// The same rounding the composer's queue uses in its script, so a file's size
/// before it is sent and after it is in the transcript read the same way.
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    if bytes < 1024 * 1024 {
        return format!("{} kB", (bytes as f64 / 1024.0).round() as u64);
    }
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

impl ChatLine {
    /// What this one reply cost: `1.2k in / 18 out / 900ms`.
    ///
    /// Deliberately narrow. Cache share and context fill describe the whole
    /// conversation, so they are stated once, in the conversation line, instead
    /// of appearing here with a per-reply value that disagrees with it. A number
    /// the provider did not report is left out rather than shown as zero.
    #[must_use]
    pub fn usage_line(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(prompt) = self.prompt_tokens {
            parts.push(format!("{} in", human_tokens(prompt)));
        }
        if let Some(out) = self.completion_tokens {
            parts.push(format!("{} out", human_tokens(out)));
        }
        if let Some(ms) = self.duration_ms {
            if ms >= 1000 {
                parts.push(format!("{:.1}s", ms as f64 / 1000.0));
            } else {
                parts.push(format!("{ms}ms"));
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" · "))
        }
    }
}

/// A token count for a human: `812`, `2.4k`, `61.1M`.
#[must_use]
pub fn human_tokens(count: u64) -> String {
    if count < 1_000 {
        return count.to_string();
    }
    if count < 1_000_000 {
        return format!("{:.1}k", count as f64 / 1_000.0);
    }
    format!("{:.1}M", count as f64 / 1_000_000.0)
}

/// Everything the chat surface needs.
#[derive(Debug, Clone)]
pub struct ChatView {
    /// The listener truth this page states in its footer.
    pub chrome: Chrome,
    /// The daemon's state, which is `ready` on this page.
    pub state: SetupState,
    /// The version line.
    pub version: String,
    /// The configured model, shown as a chip so the answer has a return address.
    pub model: String,
    /// The conversation this page shows.
    pub conversation: String,
    /// Its name, for the heading and the delete confirmation.
    pub conversation_title: String,
    /// The transcript, oldest first.
    pub messages: Vec<ChatLine>,
    /// False until a provider is stored; the composer is disabled when it is.
    pub provider_ready: bool,
    /// A refusal to show above the transcript.
    pub error: Option<FormError>,
    /// What this conversation has cost so far.
    pub stats: crate::web::chat::Stats,
    /// Every model the last probe returned, for the composer's picker.
    pub models: Vec<String>,
    /// The model the picker starts on.
    pub selected_model: String,
    /// Whether to render the model's reasoning in a collapsed bubble.
    pub show_reasoning: bool,
    /// Whether to render the run log at all.
    pub show_tool_log: bool,
    /// `full` or `summary`, choosing how it renders.
    pub tool_log_mode: String,
    /// The salutation an empty conversation opens with, from the hour.
    pub greeting: String,
    /// The line under the salutation, editable in settings.
    pub hint: String,
    /// The generated line under that salutation, when one is switched on and
    /// stored. `None` renders nothing extra at all.
    pub line: Option<String>,
    /// What the operator calls the assistant, for the composer and the labels.
    pub assistant_name: String,
    /// The sandbox mode the composer's picker starts on.
    pub sandbox_mode: String,
    /// Whether full host mode may be offered in that picker at all.
    pub allow_full: bool,
    /// The window declared for the selected model, when one is known.
    pub context_window: Option<u64>,
    /// How many messages are waiting for a turn that is already running here.
    ///
    /// Rendered so a form post that steered a turn lands on a page that says so,
    /// rather than on one that looks like the message was ignored.
    pub queued: usize,
}

/// The greeting a fresh conversation opens with, from the hour of the day.
///
/// Pure so the boundaries can be tested without waiting for the clock to move.
#[must_use]
pub fn greeting_for(hour: u32) -> &'static str {
    match hour {
        5..=11 => "Good morning",
        12..=16 => "Good afternoon",
        17..=21 => "Good evening",
        _ => "Good night",
    }
}

/// Render message text into blocks: paragraphs, and fenced code as code.
///
/// This is deliberately not a markdown engine. A fence is the one structure an
/// answer from a model needs in order to be readable, and everything else stays
/// literal text so nothing in an answer can become markup.
#[must_use]
pub fn blocks(text: &str) -> Markup {
    if text.trim().is_empty() {
        return html! { p .muted-line { "Streaming" } };
    }
    let mut pieces: Vec<Markup> = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("```") {
        let (before, after) = rest.split_at(start);
        if !before.trim().is_empty() {
            pieces.push(html! { p { (before.trim()) } });
        }
        let after = &after[3..];
        let (language, body_start) = match after.find('\n') {
            Some(index) => (&after[..index], index + 1),
            None => ("", after.len()),
        };
        let body = &after[body_start..];
        let Some(end) = body.find("```") else {
            pieces.push(html! {
                pre .code { (body.trim_end()) }
            });
            rest = "";
            break;
        };
        let code = &body[..end];
        pieces.push(html! {
            div .code-block {
                @if !language.trim().is_empty() {
                    span .code-lang { (language.trim()) }
                }
                pre .code { (code.trim_end()) }
            }
        });
        rest = &body[end + 3..];
    }
    if !rest.trim().is_empty() {
        pieces.push(html! { p { (rest.trim()) } });
    }
    html! { @for piece in pieces { (piece) } }
}

/// The conversation's running totals, as the last line inside a reply.
///
/// These used to sit in a bar under the composer, floating over the wallpaper and
/// saying nothing about which turn produced them. They belong to the answer being
/// The conversation totals, stated once.
///
/// This is the only place cache share and context fill appear, so the numbers on
/// a page cannot contradict each other. A value with no denominator is said to be
/// unknown rather than printed as a confident zero, and a context share under one
/// percent keeps a decimal instead of rounding to nothing.
fn session_line(view: &ChatView) -> Markup {
    let stats = &view.stats;
    let turns = match stats.turns {
        1 => "1 turn".to_owned(),
        count => format!("{count} turns"),
    };
    let rate = match stats.tokens_per_second {
        Some(rate) => format!("{rate} tok/s"),
        None => "tok/s unknown".to_owned(),
    };
    let cache = match stats.cache_hit_pct {
        Some(pct) => format!("cache hit {pct}%"),
        None => "cache unknown".to_owned(),
    };
    let context = context_share(stats.context_used, stats.context_window);
    html! {
        div .meta-session aria-label="Conversation totals" {
            span { (turns) }
            span .dot { "·" }
            span { (human_tokens(stats.total_tokens)) " tokens" }
            span .dot { "·" }
            span { (rate) }
            span .dot { "·" }
            span { (cache) }
            span .dot { "·" }
            span { (context) }
        }
    }
}

/// How full the model's window is, with enough precision to be useful.
///
/// A transcript of a few thousand tokens against a million-token window is a
/// fifth of a percent: printing that as `0%` reads as a bug even though the
/// arithmetic is right.
#[must_use]
pub fn context_share(used: Option<u64>, window: Option<u64>) -> String {
    let (Some(used), Some(window)) = (used, window) else {
        return "context unknown".to_owned();
    };
    if window == 0 {
        return "context unknown".to_owned();
    }
    let pct = used as f64 * 100.0 / window as f64;
    if pct >= 10.0 {
        format!("context {pct:.0}%")
    } else if pct >= 1.0 {
        format!("context {pct:.1}%")
    } else if pct > 0.0 {
        format!("context {pct:.2}%")
    } else {
        "context 0%".to_owned()
    }
}

/// The chat surface: the transcript, the composer, and the backdrop behind it.
///
/// The form is a real POST, so the page works with no script at all and `curl`
/// can drive it. The inline script at the bottom upgrades that form into a
/// streaming conversation: it posts the same body, reads the same route as an
/// event stream, and appends what arrives. It is the only script in the UI, it is
/// inline (no `src`, no network, no third party), and every other page is still
/// script-free.
/// One tool call's output as an application window, the way the iOS client draws
/// it: traffic lights, a title, the line count, then the readout.
///
/// The body is not the tool's JSON. A result is parsed by [`super::term`] into a
/// prompt line and the lines the command actually printed, because a window
/// titled `stdout` whose body was `{"stdout":"...\n..."}` was showing the wire
/// format instead of the terminal. One `div` per line rather than one `pre`: a
/// monospace `pre` is fine for a paragraph, but stderr has to be paintable and a
/// thousand-line log has to be scrollable, and both want structure.
fn tool_window(tool: &crate::web::chat::ToolRecord) -> Markup {
    let term = super::term::render(&tool.name, &tool.arguments, &tool.output, tool.is_error);
    let total = term.lines.len();
    html! {
        div .win {
            div .win-bar {
                span .win-dots {
                    span .win-dot .red {}
                    span .win-dot .yellow {}
                    span .win-dot .green {}
                }
                span .win-title { "kallisti \u{2014} stdout" }
                // The live dot is present and hidden: a settled call has no dot,
                // and the markup a live turn builds is this same markup, so the
                // title bar does not reflow when the last chunk lands.
                span .win-live hidden aria-label="Streaming" { "\u{25CF}" }
                span .win-size {
                    (total)
                    @if total == 1 { " line" } @else { " lines" }
                }
            }
            div .win-body {
                div .term-line .prompt { (term.prompt) }
                @for line in &term.lines {
                    div class=(line.kind.class()) { (line.text) }
                }
            }
        }
    }
}

/// The two controls that open or close every call in one run log.
///
/// They live in a row directly under the log's summary, not inside it. A
/// focusable button inside a `summary` competes with the disclosure's own Enter
/// and Space, and the summary's accessible name would become the buttons'
/// labels instead of "Run log, n calls". Under the summary the log keeps its own
/// name and a keyboard user has one disclosure and two plain buttons, in that
/// order.
///
/// The row is rendered `hidden` and the page script reveals it. A reader without
/// the script cannot press a button into doing anything, so a dead control is
/// worse than no control: every call is already its own native disclosure, and
/// the whole log opens and closes natively too.
fn run_log_controls() -> Markup {
    html! {
        div .runctl hidden role="group" aria-label="Run log controls" {
            button type="button" .run-btn data-run="open"
                aria-label="Expand every tool call in this run log" { "Expand all" }
            button type="button" .run-btn data-run="shut"
                aria-label="Collapse every tool call in this run log" { "Collapse all" }
        }
    }
}

/// Every call of one run log, each a closed window that opens itself.
///
/// Closed by default in both modes: one line naming the call, and the reading is
/// one press away. The body scrolls rather than growing the conversation, so a
/// long log costs one window.
fn run_log_calls(tools: &[crate::web::chat::ToolRecord]) -> Markup {
    html! {
        @for tool in tools {
            details .exec-call .failed[tool.is_error] {
                summary .exec-head {
                    span .exec-name { (tool.name) }
                    span .exec-args { (tool.arguments) }
                }
                (tool_window(tool))
            }
        }
    }
}

pub fn chat_page(view: &ChatView) -> Markup {
    let body = html! {
        div .chat-shell .new-chat[view.messages.is_empty()]
            data-conversation=(view.conversation.clone())
            data-assistant=(view.assistant_name.clone()) {
        (alert_region(view.error.as_ref(), None))
        section .thread id="thread" aria-label="Conversation" {
            @let last_answer = view.messages.iter().rposition(|line| !line.user);
            @for (index, line) in view.messages.iter().enumerate() {
                @let klass = format!(
                    "msg {}{}",
                    if line.user { "user" } else { "assistant" },
                    if line.pending { " pending" } else { "" },
                );
                article class=(klass) {
                    div .who { (line.who) }
                    @if view.show_reasoning {
                        @if let Some(reasoning) = line.reasoning.as_ref().filter(|text| !text.trim().is_empty()) {
                            details .think open {
                                summary {
                                    span .think-label { "Thinking" }
                                    div .think-actions {
                                        button type="button" .think-max
                                            title="Expand" aria-label="Expand the thinking bubble" { "\u{2922}" }
                                    }
                                }
                                div .think-body { (reasoning) }
                            }
                        }
                    }
                    @if view.show_tool_log {
                        @if !line.tools.is_empty() {
                            @if view.tool_log_mode == "summary" {
                                details .exec .compact {
                                    summary {
                                        span .exec-label { "Run log" }
                                        span .exec-count {
                                            (line.tools.len())
                                            @if line.tools.len() == 1 { " call" } @else { " calls" }
                                        }
                                        @if let Some(last) = line.tools.last() {
                                            span .exec-last {
                                                (last.name) " " (last.arguments)
                                            }
                                        }
                                    }
                                    (run_log_controls())
                                    div .exec-body { (run_log_calls(&line.tools)) }
                                }
                            } @else {
                                details .exec open {
                                    summary {
                                        span .exec-label { "Run log" }
                                        span .exec-count {
                                            (line.tools.len())
                                            @if line.tools.len() == 1 { " call" } @else { " calls" }
                                        }
                                    }
                                    (run_log_controls())
                                    div .exec-body { (run_log_calls(&line.tools)) }
                                }
                            }
                        }
                    }
                    div .bubble .failed[line.failed] {
                        (blocks(&line.body))
                        @if !line.files.is_empty() {
                            div .attached {
                                @for file in &line.files {
                                    @if file.is_image {
                                        @if let Some(url) = &file.url {
                                            a .attached-image href=(url) target="_blank"
                                                rel="noreferrer" title=(file.name) {
                                                img src=(url) alt=(file.name) loading="lazy";
                                            }
                                        } @else {
                                            span .attached-chip {
                                                span .attached-badge { (file.badge) }
                                                span .attached-name { (file.name) }
                                            }
                                        }
                                    } @else {
                                        @if let Some(url) = &file.url {
                                            a .attached-chip href=(url)
                                                download=(file.name) title=(file.name) {
                                                span .attached-badge { (file.badge) }
                                                span .attached-name { (file.name) }
                                                span .attached-size {
                                                    (human_bytes(file.size))
                                                }
                                            }
                                        } @else {
                                            span .attached-chip title=(file.name) {
                                                span .attached-badge { (file.badge) }
                                                span .attached-name { (file.name) }
                                                span .attached-size {
                                                    (human_bytes(file.size))
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    div .meta {
                        span .mono { (line.stamp) }
                        @if let Some(model) = &line.model {
                            span .meta-model { (model) }
                        }
                        @if let Some(usage) = line.usage_line() {
                            span .meta-usage { (usage) }
                        }
                        @if let Some(code) = &line.error_code {
                            span .tag { (code) }
                        }
                    }
                    @if Some(index) == last_answer && !line.pending && view.provider_ready {
                        (session_line(view))
                    }
                }
            }
        }
        @if view.messages.is_empty() {
            div .chat-greeting {
                p .hello { (view.greeting) }
                @if let Some(line) = &view.line {
                    p .line { (line) }
                }
                p .hint { (view.hint) }
            }
        }
        @if !view.provider_ready {
            p .small .chat-note {
                "No provider is configured yet. Set the endpoint and model on the settings page and "
                "the composer appears."
            }
        }
        @if view.provider_ready {
            form .composer id="composer-form" method="post"
                action=(crate::admin::ROUTE_CHAT_SEND) novalidate
                enctype="multipart/form-data" {
                input type="hidden" name="conversation" value=(view.conversation);
                label .sr-only for="composer" { "Message" }
                textarea id="composer" name="message" rows="1" spellcheck="false"
                    placeholder=(format!("Message {}", view.assistant_name)) {}
                div .composer-bar {
                    label .attach for="attachment" title="Attach files" {
                        span aria-hidden="true" { "+" }
                        span .sr-only { "Attach files" }
                    }
                    // `multiple`, so one trip to the picker can carry a screenshot,
                    // a log and a JSON file. The no-script path posts every part to
                    // the same route under the same name; the script below collects
                    // them into a queue so a second trip adds to the first.
                    input type="file" id="attachment" name="file" .attach-input multiple
                        aria-label="Attach files";
                    div .attach-queue id="attach-queue" {}
                    div .composer-pick {
                        label .sr-only for="mode" { "Access" }
                        select id="mode" name="mode" aria-label="Access level" {
                            @for mode in crate::onboarding::SandboxMode::ONBOARDING {
                                option value=(mode.as_str())
                                    selected[*mode.as_str() == view.sandbox_mode] {
                                    (mode.label())
                                }
                            }
                            @if view.allow_full {
                                option value=(crate::onboarding::SandboxMode::Full.as_str())
                                    selected[view.sandbox_mode == crate::onboarding::SandboxMode::Full.as_str()] {
                                    (crate::onboarding::SandboxMode::Full.label())
                                }
                            }
                        }
                        label .sr-only for="model" { "Model" }
                        select id="model" name="model" aria-label="Model" {
                            @for id in &view.models {
                                option value=(id) selected[*id == view.selected_model] { (id) }
                            }
                        }
                        @if let Some(window) = view.context_window {
                            span .small .nowrap { (human_tokens(window)) " window" }
                        }
                    }
                    div .composer-send {
                        span .hint id="composer-status" {}
                        // The send control is never disabled by a running turn: a
                        // second message steers it. The hint says so while the
                        // turn streams, and this one states the count a reloaded
                        // page also knows about.
                        @if view.queued > 0 {
                            span .hint id="composer-queued" {
                                (view.queued)
                                @if view.queued == 1 {
                                    " message queued for the running turn"
                                } @else {
                                    " messages queued for the running turn"
                                }
                            }
                        }
                        button type="button" id="stop" class="secondary" hidden { "Stop" }
                        button type="submit" id="send" { "Send" }
                    }
                }
            }
        } @else {
            p .empty {
                strong { "No provider yet" }
                "Finish onboarding, or set the endpoint and model on the settings page, and the "
                "composer appears."
            }
        }
        }
        script { (PreEscaped(CHAT_JS)) }
    };
    shell("Chat", view.state, Some(Nav::Chat), body, &view.chrome)
}

/// The one script in the UI: progressive enhancement for the composer.
///
/// It posts the same form body the plain form posts, asks for the event stream,
/// appends deltas, and offers a stop. With it off, the form still posts and the
/// answer arrives on the next render.
const CHAT_JS: &str = r##"(function () {
  var form = document.getElementById("composer-form");
  var box = document.getElementById("composer");
  var thread = document.getElementById("thread");
  var stop = document.getElementById("stop");
  var status = document.getElementById("composer-status");
  var attachmentInput = document.getElementById("attachment");

  // ---- The run log: two controls, and the choice they remember ------------
  // Two buttons open or close every call of one log, and the choice carries into
  // the next answer. It is a preference, not a fourth Settings mode: the server
  // still renders the default from `full` or `summary`, and this only refines the
  // per-call state of a full log. Storage can be unavailable (a private window, a
  // blocked origin), and a browser that refuses to remember is not an error worth
  // showing, so every read and write is guarded.
  var RUNLOG_KEY = "kallisti.runlog";

  function rememberedRunLog() {
    try {
      var stored = window.localStorage.getItem(RUNLOG_KEY);
      if (stored === "expanded") { return true; }
      if (stored === "collapsed") { return false; }
    } catch (error) {
      return null;
    }
    return null;
  }

  function rememberRunLog(expanded) {
    try {
      window.localStorage.setItem(RUNLOG_KEY, expanded ? "expanded" : "collapsed");
    } catch (error) {
      return;
    }
  }

  function runLogButton(kind, label, name) {
    var button = document.createElement("button");
    button.type = "button";
    button.className = "run-btn";
    button.setAttribute("data-run", kind);
    button.setAttribute("aria-label", name);
    button.textContent = label;
    return button;
  }

  // Open or close every call of one log. The log itself is left as the operator
  // put it: closing all the calls is not the same request as closing the log.
  function setRunLogCalls(log, expanded) {
    var calls = log.querySelectorAll(".exec-call");
    for (var index = 0; index < calls.length; index += 1) {
      calls[index].open = expanded;
    }
  }

  // The server renders the two controls hidden, because without this script a
  // button cannot open anything. Revealing them, and honouring a remembered
  // Expand all, happens before the composer guard below: a transcript outlives
  // the provider that wrote it, and its run logs still work.
  var runLogs = document.querySelectorAll(".msg .exec");
  var remembersAll = rememberedRunLog();
  for (var logIndex = 0; logIndex < runLogs.length; logIndex += 1) {
    var bar = runLogs[logIndex].querySelector(".runctl");
    if (bar) { bar.hidden = false; }
    // A remembered Expand all carries onto the page as it was rendered: the
    // operator left the calls open, so that is how they come back. A summary
    // strip is one deliberate line and is not touched; the default there is
    // still the server's.
    if (remembersAll === true && runLogs[logIndex].classList.contains("compact") === false) {
      setRunLogCalls(runLogs[logIndex], true);
    }
  }

  // One listener for every run log control, present and future, so a streamed
  // answer and a re-rendered transcript behave the same. The row is outside the
  // summary, so nothing here competes with the disclosure's own toggle; the
  // handler still stops the event so a click cannot reach the summary.
  if (thread) {
    thread.addEventListener("click", function (event) {
      var button = event.target.closest(".run-btn");
      if (button === null) { return; }
      event.preventDefault();
      event.stopPropagation();
      var log = button.closest(".exec");
      if (log === null) { return; }
      var expanded = button.getAttribute("data-run") === "open";
      setRunLogCalls(log, expanded);
      rememberRunLog(expanded);
    });
  }

  if (form === null || box === null || thread === null) { return; }

  var busy = false;
  var controller = null;

  // ---- Follow the bottom, unless the operator scrolled up ----------------
  // Following every fragment is what a live turn should do. Yanking the page
  // back after someone has deliberately scrolled up to read is what makes a
  // long turn unreadable, so the pin is released by a scroll and restored by
  // the jump control.
  var pinned = true;
  var NEAR = 64;
  function atBottom() {
    return thread.scrollHeight - thread.scrollTop - thread.clientHeight <= NEAR;
  }
  function toBottom() {
    if (pinned) { thread.scrollTop = thread.scrollHeight; }
  }

  var jump = document.createElement("button");
  jump.type = "button";
  jump.className = "jump-latest";
  jump.textContent = "Jump to latest";
  jump.hidden = true;
  jump.addEventListener("click", function () {
    pinned = true;
    jump.hidden = true;
    thread.scrollTop = thread.scrollHeight;
  });
  if (form.parentNode) {
    form.parentNode.insertBefore(jump, form);
  }

  thread.addEventListener("scroll", function () {
    var next = atBottom();
    if (next === pinned) { return; }
    pinned = next;
    jump.hidden = next;
  });

  // ---- One window per call, streamed -------------------------------------
  // The daemon sends `tool_output` while a command runs and one `tool` event
  // when it settles. The window opens on the first chunk, so a twenty second
  // command looks like a terminal filling in rather than a receipt printed at
  // the end. `tool` then replaces the provisional body with the same lines the
  // server would have rendered, so a live turn and a reload agree.
  function windowFor(call) {
    var win = document.createElement("div");
    win.className = "win";
    var bar = document.createElement("div");
    bar.className = "win-bar";
    var dots = document.createElement("span");
    dots.className = "win-dots";
    ["red", "yellow", "green"].forEach(function (tone) {
      var dot = document.createElement("span");
      dot.className = "win-dot " + tone;
      dots.appendChild(dot);
    });
    var title = document.createElement("span");
    title.className = "win-title";
    title.textContent = "kallisti \u2014 stdout";
    var live = document.createElement("span");
    live.className = "win-live";
    live.textContent = "\u25CF";
    live.hidden = true;
    live.setAttribute("aria-label", "Streaming");
    var size = document.createElement("span");
    size.className = "win-size";
    size.textContent = "0 lines";
    bar.appendChild(dots);
    bar.appendChild(title);
    bar.appendChild(live);
    bar.appendChild(size);
    var out = document.createElement("div");
    out.className = "win-body";
    win.appendChild(bar);
    win.appendChild(out);
    call.appendChild(win);
    return { win: win, out: out, live: live, size: size };
  }

  function countLines(text) {
    if (text === "") { return 0; }
    return text.split("\n").length;
  }

  // Write the settled reading into a window: a prompt line, then one row per
  // line. An older daemon sends no `term`, and the raw result is still the
  // truth, so it is shown as one block instead of being dropped.
  function paint(parts, tool) {
    parts.out.textContent = "";
    var term = tool.term;
    if (term === undefined || term === null) {
      var fallback = document.createElement("div");
      fallback.className = "term-line";
      fallback.textContent = tool.output;
      parts.out.appendChild(fallback);
      parts.size.textContent = "1 line";
      return;
    }
    var prompt = document.createElement("div");
    prompt.className = "term-line prompt";
    prompt.textContent = term.prompt;
    parts.out.appendChild(prompt);
    (term.lines || []).forEach(function (entry) {
      var row = document.createElement("div");
      row.className = entry.kind === "err"
        ? "term-line err"
        : (entry.kind === "notice" ? "term-line notice" : "term-line");
      row.textContent = entry.text;
      parts.out.appendChild(row);
    });
    var total = (term.lines || []).length;
    parts.size.textContent = total + (total === 1 ? " line" : " lines");
  }

  function line(role, who) {
    var article = document.createElement("article");
    article.className = "msg " + role;
    var label = document.createElement("div");
    label.className = "who";
    label.textContent = who;
    // Hidden until the first reasoning fragment, so a turn that never thinks
    // does not leave an empty shell above its answer.
    var think = document.createElement("details");
    think.className = "think";
    think.hidden = true;
    think.open = true;
    var thinkSummary = document.createElement("summary");
    var thinkLabel = document.createElement("span");
    thinkLabel.className = "think-label";
    thinkLabel.textContent = "Thinking";
    var thinkActions = document.createElement("div");
    thinkActions.className = "think-actions";
    var thinkMax = document.createElement("button");
    thinkMax.type = "button";
    thinkMax.className = "think-max";
    thinkMax.title = "Expand";
    thinkMax.setAttribute("aria-label", "Expand the thinking bubble");
    thinkMax.textContent = "\u2922";
    thinkActions.appendChild(thinkMax);
    thinkSummary.appendChild(thinkLabel);
    thinkSummary.appendChild(thinkActions);
    var thinkBody = document.createElement("div");
    thinkBody.className = "think-body";
    think.appendChild(thinkSummary);
    think.appendChild(thinkBody);

    // The run log starts hidden and appears at the first executed call, so a
    // turn that called nothing does not leave an empty panel behind.
    var exec = document.createElement("details");
    exec.className = "exec";
    exec.hidden = true;
    exec.open = true;
    var execSummary = document.createElement("summary");
    var execLabel = document.createElement("span");
    execLabel.className = "exec-label";
    execLabel.textContent = "Run log";
    var execCount = document.createElement("span");
    execCount.className = "exec-count";
    execSummary.appendChild(execLabel);
    execSummary.appendChild(execCount);
    exec.appendChild(execSummary);
    // The same two controls the server renders, in the same place: under the
    // summary, never in it. This builder is only reachable with the script, so
    // here they are live immediately.
    var execCtl = document.createElement("div");
    execCtl.className = "runctl";
    execCtl.setAttribute("role", "group");
    execCtl.setAttribute("aria-label", "Run log controls");
    execCtl.appendChild(runLogButton("open", "Expand all",
      "Expand every tool call in this run log"));
    execCtl.appendChild(runLogButton("shut", "Collapse all",
      "Collapse every tool call in this run log"));
    exec.appendChild(execCtl);
    var body = document.createElement("div");
    body.className = "exec-body";
    exec.appendChild(body);

    var calls = 0;
    var open = [];
    function refreshCount() {
      execCount.textContent = calls + (calls === 1 ? " call" : " calls");
    }

    function attachHead(head, name) {
      var headName = document.createElement("span");
      headName.className = "exec-name";
      headName.textContent = name;
      var headArgs = document.createElement("span");
      headArgs.className = "exec-args";
      head.appendChild(headName);
      head.appendChild(headArgs);
      return headArgs;
    }

    // A provisional call, opened by its first chunk of output.
    function streamStart(name) {
      for (var index = 0; index < open.length; index += 1) {
        if (open[index].name === name) { return open[index]; }
      }
      calls += 1;
      refreshCount();
      var call = document.createElement("details");
      call.className = "exec-call streaming";
      call.open = true;
      var head = document.createElement("summary");
      head.className = "exec-head";
      var args = attachHead(head, name);
      args.textContent = "";
      call.appendChild(head);
      var parts = windowFor(call);
      parts.live.hidden = false;
      body.appendChild(call);
      exec.hidden = false;
      var entry = {
        name: name,
        call: call,
        head: head,
        args: args,
        parts: parts,
        buffer: ""
      };
      open.push(entry);
      return entry;
    }

    function streamChunk(name, chunk) {
      var entry = streamStart(name);
      // Bounded like the server's own window: a command that prints all day
      // stops growing here rather than in the tab's memory.
      if (entry.buffer.length < 131072) {
        entry.buffer += chunk;
      }
      entry.parts.out.textContent = entry.buffer;
      entry.parts.size.textContent = countLines(entry.buffer) + " lines";
      entry.parts.out.scrollTop = entry.parts.out.scrollHeight;
      exec.open = true;
      toBottom();
    }

    // The settled reading for one call. A provisional window with the same name
    // is finished in place, which keeps it where it appeared.
    function addTool(tool) {
      var entry = null;
      for (var index = 0; index < open.length; index += 1) {
        if (open[index].name === tool.name) {
          entry = open[index];
          open.splice(index, 1);
          break;
        }
      }
      if (entry === null) {
        calls += 1;
        refreshCount();
        entry = { name: tool.name, call: document.createElement("details") };
        entry.call.className = "exec-call";
        entry.head = document.createElement("summary");
        entry.head.className = "exec-head";
        entry.args = attachHead(entry.head, tool.name);
        entry.call.appendChild(entry.head);
        entry.parts = windowFor(entry.call);
        // A call that printed nothing before it settled was never opened by a
        // stream, so it takes the state the operator last chose. A provisional
        // call that already showed its output stays open: the reading is what
        // they were watching.
        entry.call.open = rememberedRunLog() === true;
        body.appendChild(entry.call);
      }
      if (tool.is_error === true) {
        entry.call.className = "exec-call failed";
      } else {
        entry.call.className = "exec-call";
      }
      entry.args.textContent = tool.arguments;
      entry.parts.live.hidden = true;
      paint(entry.parts, tool);
      exec.hidden = false;
    }

    var bubble = document.createElement("div");
    bubble.className = "bubble";
    var text = document.createElement("p");
    bubble.appendChild(text);
    var meta = document.createElement("div");
    meta.className = "meta";
    article.appendChild(label);
    article.appendChild(think);
    article.appendChild(exec);
    article.appendChild(bubble);
    article.appendChild(meta);
    thread.appendChild(article);
    return {
      article: article,
      bubble: bubble,
      text: text,
      meta: meta,
      think: think,
      thinkBody: thinkBody,
      streamChunk: streamChunk,
      addTool: addTool
    };
  }

  // The operator's own line, in the shape the server renders, so a live turn and
  // a reloaded transcript put the message in the same place. Used by a send and by
  // a steer, because both are a message the operator typed.
  function appendUser(text, files) {
    var article = document.createElement("article");
    article.className = "msg user";
    var who = document.createElement("div");
    who.className = "who";
    who.textContent = "You";
    var bubble = document.createElement("div");
    bubble.className = "bubble";
    var body = document.createElement("p");
    body.textContent = text;
    bubble.appendChild(body);
    if (files.length > 0) {
      var listed = document.createElement("div");
      listed.className = "attached";
      files.forEach(function (file) {
        var chip = document.createElement("span");
        chip.className = "attached-chip";
        chip.textContent = file.name + " \u00b7 " + humanSize(file.size);
        listed.appendChild(chip);
      });
      bubble.appendChild(listed);
    }
    article.appendChild(who);
    article.appendChild(bubble);
    thread.appendChild(article);
    return article;
  }

  // One listener for every thinking bubble on the page: present and future, so
  // a streamed reply and a re-rendered transcript behave the same.
  thread.addEventListener("click", function (event) {
    var button = event.target.closest(".think-max");
    if (button === null) { return; }
    event.preventDefault();
    event.stopPropagation();
    var think = button.closest(".think");
    if (think === null) { return; }
    think.classList.toggle("max");
  });

  function human(count) {
    if (count === null || count === undefined) { return ""; }
    if (count < 1000) { return String(count); }
    if (count < 1000000) { return (count / 1000).toFixed(1) + "k"; }
    return (count / 1000000).toFixed(1) + "M";
  }

  function metaFor(payload) {
    var parts = [];
    if (payload.model) { parts.push(payload.model); }
    if (typeof payload.prompt_tokens === "number") {
      parts.push(human(payload.prompt_tokens) + " in");
    }
    if (typeof payload.completion_tokens === "number") {
      parts.push(human(payload.completion_tokens) + " out");
    }
    var prompt = payload.prompt_tokens || 0;
    var cached = payload.cached_tokens || 0;
    if (prompt + cached > 0) {
      parts.push(Math.round((cached * 100) / (prompt + cached)) + "% cache");
    }
    if (payload.duration_ms) {
      parts.push(payload.duration_ms >= 1000
        ? (payload.duration_ms / 1000).toFixed(1) + "s"
        : payload.duration_ms + "ms");
    }
    return parts.join(" \u00b7 ");
  }

  // While a turn streams the composer stays usable, and the send control says
  // what a second message does: it steers the running turn instead of starting a
  // second one. Nothing here is disabled, because the operator's correction is
  // the most useful thing they can send mid-turn.
  function setBusy(next) {
    busy = next;
    var send = document.getElementById("send");
    if (send) {
      send.textContent = next ? "Steer" : "Send";
      send.disabled = false;
    }
    if (stop) { stop.hidden = next === false; }
    if (status) {
      status.textContent = next ? "streaming - a second message steers this turn" : "";
    }
  }

  function parseFrame(frame) {
    var event = "message";
    var data = "";
    var lines = frame.split("\n");
    for (var index = 0; index < lines.length; index += 1) {
      if (lines[index].indexOf("event:") === 0) {
        event = lines[index].slice(6).trim();
      }
      if (lines[index].indexOf("data:") === 0) {
        data += lines[index].slice(5).trim();
      }
    }
    return { event: event, data: data };
  }

  // ---- Subagents: the bar's count, without a reload ----------------------
  // The control is rendered by the daemon, so this only ever adjusts a number
  // that is already on the page. A turn that dispatched nothing has no control,
  // and a page with no control simply does nothing: the next load renders the
  // row, which is the same answer a browser with no script gets.
  var subagentsSeen = {};
  function noteSubagent(change) {
    if (change.id === undefined) { return; }
    if (subagentsSeen[change.id] === true) { return; }
    subagentsSeen[change.id] = true;
    var control = document.querySelector(".subagents");
    if (control === null) { return; }
    var count = control.querySelector(".subagents-count");
    if (count === null) { return; }
    count.textContent = String(parseInt(count.textContent, 10) + 1);
  }

  // A conversation stops being new the moment a turn starts: drop the empty
  // state layout so the transcript is visible.
  function leaveEmptyState() {
    var shell = document.querySelector(".chat-shell");
    if (shell) { shell.classList.remove("new-chat"); }
    var greeting = document.querySelector(".chat-greeting");
    if (greeting) { greeting.remove(); }
  }

  // ---- Files: a queue, not one slot --------------------------------------
  // A multiple picker plus a second trip to the picker plus a drag plus a paste
  // all land in the same queue, and every file leaves as its own `file` part on
  // the same message. Files are held in memory until the turn is sent, so a
  // mis-picked file is one click to remove rather than a reload.
  var queue = [];
  var LIMIT = 8;
  var queueBox = document.getElementById("attach-queue");

  function humanSize(bytes) {
    if (bytes < 1024) { return bytes + " B"; }
    if (bytes < 1048576) { return Math.round(bytes / 1024) + " kB"; }
    return (bytes / 1048576).toFixed(1) + " MB";
  }

  function renderQueue() {
    if (queueBox === null) { return; }
    queueBox.textContent = "";
    queue.forEach(function (file, index) {
      var chip = document.createElement("span");
      chip.className = "attach-chip";
      var name = document.createElement("span");
      name.className = "attach-chip-name";
      name.textContent = file.name;
      name.title = file.name;
      var size = document.createElement("span");
      size.className = "attach-chip-size";
      size.textContent = humanSize(file.size);
      var drop = document.createElement("button");
      drop.type = "button";
      drop.className = "attach-chip-drop";
      drop.setAttribute("aria-label", "Remove " + file.name);
      drop.textContent = "\u00d7";
      drop.addEventListener("click", function () {
        queue.splice(index, 1);
        renderQueue();
      });
      chip.appendChild(name);
      chip.appendChild(size);
      chip.appendChild(drop);
      queueBox.appendChild(chip);
    });
    queueBox.hidden = queue.length === 0;
  }

  function addFiles(list) {
    var refused = 0;
    for (var index = 0; index < list.length; index += 1) {
      var file = list[index];
      if (queue.length >= LIMIT) { refused += 1; continue; }
      var duplicate = queue.some(function (existing) {
        return existing.name === file.name && existing.size === file.size;
      });
      if (duplicate) { continue; }
      queue.push(file);
    }
    renderQueue();
    if (status) {
      status.textContent = refused > 0
        ? "at most " + LIMIT + " files per message"
        : "";
    }
  }

  if (attachmentInput) {
    attachmentInput.addEventListener("change", function () {
      // The input is emptied straight away so the same file can be picked
      // again after a removal; the queue holds the File objects themselves.
      addFiles(attachmentInput.files);
      attachmentInput.value = "";
    });
  }

  // A drop anywhere on the composer is a file for the message, which is the
  // one gesture everybody tries first.
  ["dragenter", "dragover"].forEach(function (name) {
    form.addEventListener(name, function (event) {
      event.preventDefault();
      form.classList.add("dropping");
    });
  });
  ["dragleave", "drop"].forEach(function (name) {
    form.addEventListener(name, function (event) {
      event.preventDefault();
      if (name === "dragleave" && form.contains(event.relatedTarget)) { return; }
      form.classList.remove("dropping");
    });
  });
  form.addEventListener("drop", function (event) {
    if (event.dataTransfer === null) { return; }
    if (event.dataTransfer.files.length > 0) {
      addFiles(event.dataTransfer.files);
    }
  });

  // A screenshot pasted straight into the composer is the same as an attach.
  box.addEventListener("paste", function (event) {
    if (event.clipboardData === null) { return; }
    var files = event.clipboardData.files;
    if (files && files.length > 0) {
      event.preventDefault();
      addFiles(files);
    }
  });

  // A message sent while a turn is streaming steers it. It goes to the same route
  // the composer posts to, and the daemon queues it for the turn that is already
  // running: one turn, one bill, and the instruction the operator just typed is
  // what the model continues with at its next boundary.
  async function steer(text) {
    var shell = document.querySelector(".chat-shell");
    var conversation = shell === null ? "" : shell.getAttribute("data-conversation");
    var body = new URLSearchParams();
    body.append("message", text);
    body.append("conversation", conversation);
    box.value = "";
    appendUser(text, []);
    toBottom();
    // The count a reloaded page rendered is about to be wrong, so it goes: the
    // status line below carries the live answer instead.
    var stale = document.getElementById("composer-queued");
    if (stale) { stale.remove(); }
    try {
      var response = await fetch("/dashboard/chat/send", {
        method: "POST",
        headers: {
          "Content-Type": "application/x-www-form" + "-urlencoded"
        },
        body: body
      });
      if (response.ok === false) {
        if (status) { status.textContent = "that message was not queued"; }
        return;
      }
      if (status) {
        // A steer is text: the running turn built its prompt before any newly
        // picked file existed, so the files stay queued for the next turn rather
        // than pretending to have been read.
        status.textContent = queue.length > 0
          ? "queued - the files stay queued for the next turn"
          : "queued - steers the turn that is running";
      }
    } catch (error) {
      if (status) { status.textContent = "that message was not queued"; }
    }
  }

  async function send() {
    var text = box.value.trim();
    if (text === "") { return; }
    if (busy) {
      await steer(text);
      return;
    }
    leaveEmptyState();
    var picker = document.getElementById("model");
    var chosen = picker === null ? "" : picker.value;
    var shell = document.querySelector(".chat-shell");
    var conversation = shell === null ? "" : shell.getAttribute("data-conversation");
    var sending = queue.slice();
    var userLine = appendUser(text, sending);
    box.value = "";
    setBusy(true);
    toBottom();
    var shellName = document.querySelector(".chat-shell");
    var assistantName = shellName === null ? "assistant" : (shellName.getAttribute("data-assistant") || "assistant");
    var target = line("assistant", assistantName);
    target.article.className = "msg assistant pending";
    var answer = "";
    var thinking = "";
    controller = new AbortController();
    try {
      var response = await fetch("/dashboard/chat/send", {
        method: "POST",
        // No Content-Type here on purpose: the body is FormData, and setting the
        // header by hand omits the multipart boundary, which the server then
        // cannot parse. The browser writes the correct header itself.
        headers: { "Accept": "text/event-stream" },
        body: (function () {
          var body = new FormData();
          body.append("message", text);
          body.append("model", chosen);
          body.append("conversation", conversation);
          sending.forEach(function (file) {
            body.append("file", file, file.name);
          });
          return body;
        })(),
        signal: controller.signal
      });
      if (response.ok === false) { throw new Error("HTTP " + response.status); }
      queue = [];
      renderQueue();
      if (response.body === null || typeof response.body.getReader === "undefined") {
        // No streaming in this browser: hand the turn to the plain form, whose
        // answer arrives rendered. The queue is written back into the picker so
        // the files are not lost on the way.
        target.article.remove();
        userLine.remove();
        box.value = text;
        setBusy(false);
        if (attachmentInput && typeof DataTransfer === "object") {
          var carrier = new DataTransfer();
          sending.forEach(function (file) { carrier.items.add(file); });
          attachmentInput.files = carrier.files;
        }
        form.submit();
        return;
      }
      var reader = response.body.getReader();
      var decoder = new TextDecoder();
      var buffer = "";
      var running = true;
      while (running) {
        var chunk = await reader.read();
        if (chunk.done) { running = false; break; }
        buffer += decoder.decode(chunk.value, { stream: true });
        var frames = buffer.split("\n\n");
        buffer = frames.pop();
        for (var index = 0; index < frames.length; index += 1) {
          var parsed = parseFrame(frames[index]);
          if (parsed.event === "delta") {
            answer += JSON.parse(parsed.data).text;
            target.text.textContent = answer;
            toBottom();
          }
          if (parsed.event === "tool_output") {
            var output = JSON.parse(parsed.data);
            target.streamChunk(output.name, output.chunk);
          }
          if (parsed.event === "tool") {
            target.addTool(JSON.parse(parsed.data));
            toBottom();
          }
          if (parsed.event === "reasoning") {
            thinking += JSON.parse(parsed.data).text;
            target.thinkBody.textContent = thinking;
            target.think.hidden = false;
            // The bubble is bounded and scrolls itself, so the newest thought
            // stays visible without the bubble growing over the answer.
            target.thinkBody.scrollTop = target.thinkBody.scrollHeight;
            toBottom();
          }
          if (parsed.event === "done") {
            var final = JSON.parse(parsed.data);
            answer = final.text;
            target.meta.textContent = metaFor(final);
            if (typeof final.reasoning === "string" && final.reasoning.trim().length > 0) {
              target.thinkBody.textContent = final.reasoning;
              target.think.hidden = false;
            }
          }
          if (parsed.event === "error") {
            var failure = JSON.parse(parsed.data);
            target.bubble.className = "bubble failed";
            target.text.textContent = failure.message;
            target.meta.textContent = failure.code;
          }
          // Not a failure of the answer: a message that arrived after the turn had
          // finished, said out loud rather than left to disappear.
          if (parsed.event === "notice") {
            var notice = JSON.parse(parsed.data);
            if (status) { status.textContent = notice.message || ""; }
          }
          // A subagent started or finished. The count is the only thing a stream
          // can be sure of: the row itself is rendered on the next load, which is
          // what an operator with no script sees.
          if (parsed.event === "subagent") {
            noteSubagent(JSON.parse(parsed.data));
          }
        }
      }
      target.text.textContent = answer;
    } catch (error) {
      if (controller && controller.signal.aborted) {
        target.text.textContent = answer + " [stopped]";
      } else {
        // A dropped stream is not a failed turn. The daemon keeps working after
        // the browser lets go, so the answer is very likely on its way to the
        // transcript. Printing the browser's own words ("Load failed") would be
        // jargon standing in for a fact the operator can act on.
        target.bubble.className = "bubble failed";
        target.text.textContent =
          "Lost the connection to the daemon. The turn may have finished anyway - " +
          "reload to see the transcript, or send again.";
      }
    }
    controller = null;
    target.article.className = "msg assistant";
    setBusy(false);
    // Nothing is waiting any more: the turn this page said had a message queued
    // for it has just ended, so the count goes rather than going stale.
    var drained = document.getElementById("composer-queued");
    if (drained) { drained.remove(); }
    toBottom();
    await refreshRail();
    box.focus();
  }

  form.addEventListener("submit", function (event) {
    event.preventDefault();
    send();
  });

  box.addEventListener("keydown", function (event) {
    if (event.key === "Enter" && event.shiftKey === false) {
      event.preventDefault();
      send();
    }
  });

  // The rail is a server-rendered list, so a conversation that earns its name
  // during this turn would not show up until a reload. One small JSON read after
  // the turn keeps the list honest, headings and menus included: the script never
  // paints a rail the page itself would not have rendered.
  function railLink(session, openId) {
    var link = document.createElement("a");
    link.href = "/dashboard/chat/" + session.id;
    link.title = session.title;
    if (session.id === openId) { link.setAttribute("aria-current", "page"); }
    var title = document.createElement("span");
    title.className = "session-title";
    title.textContent = session.title;
    var stamp = document.createElement("span");
    stamp.className = "session-stamp";
    stamp.textContent = session.stamp;
    link.appendChild(title);
    link.appendChild(stamp);
    return link;
  }

  // The same select the server renders: one option per project, plus the empty
  // "No project" entry a clear sends.
  function railMenu(session, projects) {
    var menu = document.createElement("details");
    menu.className = "session-menu";
    var summary = document.createElement("summary");
    summary.title = "Add to a project";
    summary.setAttribute("aria-label", "Add " + session.title + " to a project");
    summary.textContent = "\u2026";
    menu.appendChild(summary);
    var form = document.createElement("form");
    form.className = "session-menu-form";
    form.method = "post";
    form.action = "/dashboard/projects/assign";
    var hidden = document.createElement("input");
    hidden.type = "hidden";
    hidden.name = "conversation";
    hidden.value = session.id;
    form.appendChild(hidden);
    var field = document.createElement("select");
    field.name = "project";
    field.setAttribute("aria-label", "Project");
    var none = document.createElement("option");
    none.value = "";
    none.textContent = "No project";
    if (session.project_id === null) { none.selected = true; }
    field.appendChild(none);
    for (var index = 0; index < projects.length; index += 1) {
      var option = document.createElement("option");
      option.value = projects[index].id;
      option.textContent = projects[index].name;
      if (session.project_id === projects[index].id) { option.selected = true; }
      field.appendChild(option);
    }
    form.appendChild(field);
    var submit = document.createElement("button");
    submit.type = "submit";
    submit.textContent = "Move";
    form.appendChild(submit);
    menu.appendChild(form);
    return menu;
  }

  function railRow(session, projects, openId) {
    var row = document.createElement("div");
    row.className = session.id === openId ? "session-row open" : "session-row";
    row.appendChild(railLink(session, openId));
    row.appendChild(railMenu(session, projects));
    return row;
  }

  function railGroup(name, sessions, projects, openId) {
    var group = document.createElement("div");
    group.className = "rail-group";
    var head = document.createElement("div");
    head.className = "rail-group-head";
    var label = document.createElement("span");
    label.className = "rail-group-name";
    label.textContent = name;
    var count = document.createElement("span");
    count.className = "rail-group-count";
    count.textContent = String(sessions.length);
    head.appendChild(label);
    head.appendChild(count);
    group.appendChild(head);
    for (var index = 0; index < sessions.length; index += 1) {
      group.appendChild(railRow(sessions[index], projects, openId));
    }
    return group;
  }

  async function refreshRail() {
    var rail = document.getElementById("rail-sessions");
    if (rail === null) { return; }
    try {
      var response = await fetch("/dashboard/chat/sessions.json", {
        headers: { "Accept": "application/json" }
      });
      if (response.ok === false) { return; }
      var payload = await response.json();
      var shell = document.querySelector(".chat-shell");
      var openId = shell === null ? "" : shell.getAttribute("data-conversation");
      var projects = payload.projects || [];
      rail.textContent = "";
      if (payload.sessions.length === 0) {
        var empty = document.createElement("span");
        empty.className = "side-empty";
        empty.textContent = "No conversations yet";
        rail.appendChild(empty);
        return;
      }
      if (projects.length === 0) {
        for (var flat = 0; flat < payload.sessions.length; flat += 1) {
          rail.appendChild(railRow(payload.sessions[flat], projects, openId));
        }
        return;
      }
      for (var p = 0; p < projects.length; p += 1) {
        var members = [];
        for (var s = 0; s < payload.sessions.length; s += 1) {
          if (payload.sessions[s].project_id === projects[p].id) {
            members.push(payload.sessions[s]);
          }
        }
        if (members.length > 0) {
          rail.appendChild(railGroup(projects[p].name, members, projects, openId));
        }
      }
      var unfiled = [];
      for (var u = 0; u < payload.sessions.length; u += 1) {
        var known = false;
        for (var k = 0; k < projects.length; k += 1) {
          if (payload.sessions[u].project_id === projects[k].id) { known = true; }
        }
        if (known === false) { unfiled.push(payload.sessions[u]); }
      }
      if (unfiled.length > 0) {
        rail.appendChild(railGroup("Conversations", unfiled, projects, openId));
      }
    } catch (error) {
      // A rail that cannot refresh is not worth interrupting the conversation for.
    }
  }

  var mode = document.getElementById("mode");
  if (mode) {
    mode.addEventListener("change", async function () {
      var previous = mode.getAttribute("data-last") || "";
      var body = new URLSearchParams({ sandbox_mode: mode.value });
      try {
        var response = await fetch("/dashboard/chat/mode", {
          method: "POST",
          headers: {
            "Accept": "application/json",
            "Content-Type": "application/x-www-form" + "-urlencoded"
          },
          body: body
        });
        if (response.ok === false) {
          var refusal = await response.json().catch(function () { return {}; });
          if (previous) { mode.value = previous; }
          if (status) { status.textContent = refusal.message || "access level refused"; }
          return;
        }
        mode.setAttribute("data-last", mode.value);
        if (status) { status.textContent = ""; }
      } catch (error) {
        if (previous) { mode.value = previous; }
      }
    });
    mode.setAttribute("data-last", mode.value);
  }

  if (stop) {
    stop.addEventListener("click", async function () {
      // The daemon stops the turn, not the browser: a page that stops reading a
      // stream leaves the turn running on the server. This is the same `/stop`
      // command the composer accepts, so there is one cancellation path, and the
      // turn stops at its next boundary - the copy says so rather than implying a
      // socket was cut.
      stop.disabled = true;
      if (status) { status.textContent = "stopping at the next boundary"; }
      var shell = document.querySelector(".chat-shell");
      var conversation = shell === null ? "" : shell.getAttribute("data-conversation");
      var body = new URLSearchParams();
      body.append("message", "/stop");
      body.append("conversation", conversation);
      try {
        await fetch("/dashboard/chat/send", {
          method: "POST",
          headers: {
            "Content-Type": "application/x-www-form-url" + "-urlencoded"
          },
          body: body
        });
      } catch (error) {
        // The abort below is still the operator's own answer if the post fails.
      }
      stop.disabled = false;
      // Then stop listening, so the page is not left waiting on a stream whose
      // answer has already been decided.
      if (controller) { controller.abort(); }
    });
  }

  box.focus();
})();"##;

#[cfg(test)]
mod tests {
    #[test]
    fn a_tiny_context_share_keeps_enough_precision_to_not_read_as_zero() {
        // 2.2k of a 1M window is a fifth of a percent.
        assert_eq!(
            super::context_share(Some(2_200), Some(1_000_000)),
            "context 0.22%"
        );
        assert_eq!(
            super::context_share(Some(50_000), Some(1_000_000)),
            "context 5.0%"
        );
        assert_eq!(
            super::context_share(Some(250_000), Some(1_000_000)),
            "context 25%"
        );
        assert_eq!(
            super::context_share(None, Some(1_000_000)),
            "context unknown"
        );
        assert_eq!(
            super::context_share(Some(2_200), None),
            "context unknown",
            "a share with no denominator is unknown, not zero"
        );
    }

    use super::*;
    use crate::onboarding::Wizard;

    #[test]
    fn a_chip_names_the_type_its_bytes_were_sniffed_to_be() {
        assert_eq!(type_badge(Some("image/png")), "PNG");
        assert_eq!(type_badge(Some("image/gif")), "GIF");
        assert_eq!(type_badge(Some("application/pdf")), "PDF");
        assert_eq!(type_badge(Some("text/plain")), "TXT");
        assert_eq!(
            type_badge(Some("application/octet-stream")),
            "FILE",
            "a format nothing recognised is a file, not a guess"
        );
        assert_eq!(type_badge(None), "FILE");
    }

    #[test]
    fn a_chip_states_a_size_the_way_the_composer_did() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(812), "812 B");
        assert_eq!(human_bytes(1024), "1 kB");
        assert_eq!(human_bytes(14_336), "14 kB");
        assert_eq!(human_bytes(3_565_158), "3.4 MB");
    }

    fn ready_wizard() -> Wizard {
        Wizard {
            state: SetupState::Setup,
            steps: Step::ALL
                .iter()
                .map(|step| crate::onboarding::StepStatus {
                    step: *step,
                    completed_at: None,
                    optional: step.is_optional(),
                })
                .collect(),
            next_step: Some(Step::Claim),
        }
    }

    #[test]
    fn the_claim_form_labels_its_input_and_focuses_the_error() {
        let mut view = SetupView::new(SetupState::Setup, ready_wizard(), "1.0.0");
        view.error = Some(FormError::new(
            Some("passphrase"),
            "passphrase_rejected",
            "the setup passphrase was rejected",
        ));
        let page = setup_page(&view).into_string();
        assert!(page.contains(r#"<label for="passphrase">"#), "{page}");
        assert!(page.contains(r#"id="passphrase""#));
        assert!(page.contains("autofocus"), "the failed field takes focus");
        assert!(page.contains(r#"aria-live="polite""#));
        assert!(page.contains("passphrase_rejected"));
        assert!(page.contains(r#"aria-invalid="true""#));
        assert!(!page.contains("<script"), "there is no script in the UI");
    }

    #[test]
    fn a_fenced_block_becomes_code_and_everything_else_stays_text() {
        let rendered = blocks("Use this:\n```sh\nls -l\n```\nThat is all.").into_string();
        assert!(
            rendered.contains(r#"<span class="code-lang">sh</span>"#),
            "{rendered}"
        );
        assert!(
            rendered.contains(r#"<pre class="code">ls -l</pre>"#),
            "{rendered}"
        );
        assert!(rendered.contains("<p>Use this:</p>"), "{rendered}");
        assert!(rendered.contains("<p>That is all.</p>"), "{rendered}");

        // An empty answer is a placeholder, not a blank bubble.
        assert!(blocks("   ").into_string().contains("Streaming"));

        // Markup in an answer stays text: the renderer never trusts the model.
        let hostile = blocks("<script>alert(1)</script>").into_string();
        assert!(!hostile.contains("<script>"), "{hostile}");
        assert!(hostile.contains("&lt;script&gt;"), "{hostile}");
    }

    #[test]
    fn the_clock_comes_from_the_database_stamp() {
        // The clock lives in `crate::zone` so every surface shares one conversion.
        assert_eq!(
            crate::zone::clock_in("2026-10-19T23:41:43.937Z", 0),
            "23:41"
        );
        assert_eq!(crate::zone::clock_in("nonsense", 0), "nonsense");
    }

    #[test]
    fn the_pages_carry_no_herald_and_no_purple() {
        let view = SetupView::new(SetupState::Setup, ready_wizard(), "1.0.0");
        let setup = setup_page(&view).into_string().to_ascii_lowercase();
        let dashboard = dashboard_page(&DashboardView::new(SetupState::Ready, "1.0.0"))
            .into_string()
            .to_ascii_lowercase();
        let chrome = Chrome::default();
        let error = error_page(SetupState::Setup, "not_loopback", "denied", &chrome)
            .into_string()
            .to_ascii_lowercase();
        for page in [&setup, &dashboard, &error] {
            assert!(!page.contains("herald"), "the old name is gone");
            for banned in [
                "#800080", "#7b1fa2", "#6a1b9a", "#9c27b0", "#8e24aa", "#673ab7",
            ] {
                assert!(
                    !page.contains(banned),
                    "purple {banned} is in a rendered page"
                );
            }
            assert!(!page.contains('\u{2014}'), "no em dash reaches a page");
        }
    }

    #[test]
    fn the_ready_wizard_says_so_and_links_the_dashboard() {
        let mut view = SetupView::new(SetupState::Ready, ready_wizard(), "1.0.0");
        view.state = SetupState::Ready;
        let page = setup_page(&view).into_string();
        assert!(page.contains("Onboarding complete"));
        assert!(page.contains(r#"href="/dashboard""#));
    }

    #[test]
    fn the_device_listener_is_presented_as_off_by_default() {
        let page = dashboard_page(&DashboardView::new(SetupState::Ready, "1.0.0")).into_string();
        assert!(page.contains("Device listener"));
        assert!(page.contains("127.0.0.1:7341"));
        assert!(
            !page.contains(r#"value="true" checked"#),
            "the listener checkbox starts unchecked"
        );
    }
}
