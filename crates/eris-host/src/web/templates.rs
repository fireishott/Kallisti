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

use maud::{DOCTYPE, Markup, PreEscaped, html};

use crate::admin::{ADMIN_ADDR, VERSION};
use crate::db::SetupState;
use crate::device::DEVICE_ADDR;
use crate::onboarding::{
    ApprovalPolicy, AutopilotCeiling, PermissionRow, Permissions, RowControl, SandboxMode, Step,
    Telemetry, Wizard,
};

use super::css::APP_CSS;
use super::store::AuditEntry;

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
    /// When it was revoked, if it was.
    pub revoked_at: Option<String>,
}

impl DeviceRow {
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

/// The document shell every page shares, without navigation.
///
/// Used by the three pages that must stay minimal: the wizard while it is open,
/// the sign-in form, and an error page. A nav on any of those is a trap, because
/// the pages it points at are not reachable from there.
pub fn layout(title: &str, state: SetupState, body: Markup) -> Markup {
    shell(title, state, None, body)
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
pub fn shell(title: &str, state: SetupState, nav: Option<Nav>, body: Markup) -> Markup {
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
                div .wrap {
                    header .top {
                        div .brand { strong { "Kallisti" } " 1.0 Eris daemon" }
                        @if let Some(current) = nav { (nav_bar(current)) }
                        span class=(state_class) { (state.as_str()) }
                    }
                    (body)
                    footer {
                        "Eris " (VERSION)
                        " | admin listener " (ADMIN_ADDR) ", loopback only"
                        " | reach it over SSH, never a proxy"
                    }
                }
            }
        }
    }
}

/// One section of the application shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    /// The dashboard home.
    Dashboard,
    /// The settings surface, the only page that writes behaviour.
    Settings,
    /// The audit trail.
    Activity,
}

impl Nav {
    /// Every entry, in display order. The two unavailable entries sit between
    /// the dashboard and the settings page because that is where they belong
    /// once they exist.
    pub const ALL: [Self; 3] = [Self::Dashboard, Self::Settings, Self::Activity];

    /// The path this section is served at.
    #[must_use]
    pub fn path(self) -> &'static str {
        match self {
            Self::Dashboard => crate::admin::ROUTE_DASHBOARD,
            Self::Settings => crate::admin::ROUTE_SETTINGS,
            Self::Activity => crate::admin::ROUTE_ACTIVITY,
        }
    }

    /// The label a person reads.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Dashboard => "Dashboard",
            Self::Settings => "Settings",
            Self::Activity => "Activity",
        }
    }
}

/// The top-bar navigation.
fn nav_bar(current: Nav) -> Markup {
    html! {
        nav class="nav" aria-label="Sections" {
            a href=(Nav::Dashboard.path()) aria-current=[(current == Nav::Dashboard).then_some("page")] {
                (Nav::Dashboard.label())
            }
            span class="soon" aria-disabled="true" {
                "Sessions " span .tag { "soon" }
            }
            span class="soon" aria-disabled="true" {
                "Devices " span .tag { "soon" }
            }
            a href=(Nav::Settings.path()) aria-current=[(current == Nav::Settings).then_some("page")] {
                (Nav::Settings.label())
            }
            a href=(Nav::Activity.path()) aria-current=[(current == Nav::Activity).then_some("page")] {
                (Nav::Activity.label())
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
        fieldset .field style="border:0;padding:0;margin:0 0 18px" aria-invalid=[invalid.then_some("true")] {
            legend style="font-size:13px;font-weight:600;margin-bottom:8px" { (legend) }
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
            caption style="text-align:left;color:#9aa4b2;font-size:12px;padding-bottom:8px" {
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
                        p .mono style="font-size:22px;letter-spacing:0.18em" { (code) }
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
    layout("Onboarding", view.state, body)
}

/// The sign-in page: the way into the dashboard from a browser.
///
/// `POST /setup/claim` minted a session exactly once, while the wizard was open.
/// After onboarding finishes that form is gone, so without this page the
/// dashboard had no browser path in at all. It asks for the same credential the
/// claim did, and it writes nothing: a successful post mints a cookie and
/// redirects.
pub fn login_page(state: SetupState, error: Option<&FormError>) -> Markup {
    let invalid = error.is_some_and(|error| error.field.as_deref() == Some("passphrase"));
    let body = html! {
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
    };
    layout("Sign in", state, body)
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

        section .card aria-labelledby="quick-actions" {
            h2 id="quick-actions" { "Quick actions" }
            div .actions {
                a .button href=(Nav::Settings.path()) { "Settings" }
                a .button.secondary href=(Nav::Activity.path()) { "Activity" }
                span .button.secondary.soon aria-disabled="true" {
                    "Sessions " span .tag { "soon" }
                }
                span .button.secondary.soon aria-disabled="true" {
                    "Devices " span .tag { "soon" }
                }
            }
            p .small {
                "Sessions and Devices are the next two pages. They are named here rather than "
                "linked, so nothing on this page answers a click with a 404."
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
                dt { "Jobs" } dd { (view.jobs_line()) }
                dt { "Admin listener" } dd { (ADMIN_ADDR) ", bound, loopback only" }
                dt { "Device listener" } dd { (view.device_listener_line()) }
            }
            p .small {
                "Both listeners are literals in the binary. The admin listener answers this page; "
                "the device listener binds only when it is switched on below."
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
                p .small { "No device has been paired yet. Mint a code from the wizard's device step." }
            } @else {
                table {
                    thead {
                        tr {
                            th scope="col" { "Device" }
                            th scope="col" { "Platform" }
                            th scope="col" { "State" }
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
            div style="margin-top:18px" {
                (permission_table(&view.permission_rows))
            }
        }
    };
    shell("Dashboard", view.state, Some(Nav::Dashboard), body)
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
}

impl SettingsGroup {
    /// Every group, in page order.
    pub const ALL: [Self; 3] = [Self::Provider, Self::Workspace, Self::Permissions];

    /// The anchor and error-region id for this group.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Workspace => "workspace",
            Self::Permissions => "permissions",
        }
    }

    /// Whether a validation failure belongs to this group.
    ///
    /// The field names are disjoint across the three forms, so the failure
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
        }
    }

    /// The sentence a successful save renders.
    #[must_use]
    pub fn saved_notice(self) -> &'static str {
        match self {
            Self::Provider => "provider settings saved",
            Self::Workspace => "workspace saved",
            Self::Permissions => "permissions saved",
        }
    }
}

/// Everything the settings page needs.
#[derive(Debug, Clone)]
pub struct SettingsView {
    /// The daemon's state, which is `ready` on this page.
    pub state: SetupState,
    /// The version line.
    pub version: String,
    /// True only when the process was started with `--allow-full`.
    pub allow_full: bool,
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
            state,
            version: version.into(),
            allow_full: false,
            provider_base_url: String::new(),
            provider_key_ref: String::new(),
            model: String::new(),
            workspace: crate::onboarding::workspace::DEFAULT_WORKSPACE.to_owned(),
            permissions: Permissions::DEFAULT,
            paired_devices: 0,
            device_listener: false,
            device_listener_bound: false,
            error: None,
            notice: None,
        }
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
                    "Full host is not selectable on this process. It needs a restart of eris with "
                    code { "--allow-full" } ", which is a decision made at a keyboard on this host."
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

/// The settings page: three forms, then the rows that are read-only by design.
pub fn settings_page(view: &SettingsView) -> Markup {
    let page_error = view.error.as_ref().filter(|error| error.field.is_none());
    let body = html! {
        h1 { "Settings" }
        p .lede {
            "Three groups, each its own form and its own route. A save redirects and says so; "
            "a refusal re-renders this page with the reason next to the field that caused it."
        }
        (alert_region(page_error, None))

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
            dl .kv style="margin-top:18px" {
                dt { "Paired devices" } dd { (view.paired_devices) }
                dt { "Device listener" } dd {
                    @if view.device_listener { "on" } @else { "off" }
                    ", "
                    @if view.device_listener_bound { "bound" } @else { "not bound" }
                    " on " (DEVICE_ADDR)
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
                caption style="text-align:left;color:#9aa4b2;font-size:12px;padding-bottom:8px" {
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
                        td .value { "loopback only" " " span .tag { "locked" } }
                        td .why {
                            "Admin on " (ADMIN_ADDR) " and device on " (DEVICE_ADDR)
                            ", both literals in the binary. Nothing listens on a LAN address."
                        }
                    }
                    tr {
                        th scope="row" { "Telemetry" }
                        td .value { (view.permissions.telemetry.label()) }
                        td .why { (view.permissions.telemetry.consequence()) }
                    }
                    tr .locked {
                        th scope="row" { "Admin listener" }
                        td .value { (ADMIN_ADDR) " " span .tag { "fixed" } }
                        td .why {
                            "Not configurable: there is no listen key in the config file, no flag, "
                            "and no environment variable that moves this port."
                        }
                    }
                }
            }
        }
    };
    shell("Settings", view.state, Some(Nav::Settings), body)
}

/// Everything the activity page needs.
#[derive(Debug, Clone)]
pub struct ActivityView {
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
                p .small { "No audit row has been written yet." }
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
    shell("Activity", view.state, Some(Nav::Activity), body)
}

/// A standalone error page, for a 403 or a 409 that a browser will read.
pub fn error_page(state: SetupState, code: &str, message: &str) -> Markup {
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
    layout("Not available", state, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboarding::Wizard;

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
    fn the_pages_carry_no_herald_and_no_purple() {
        let view = SetupView::new(SetupState::Setup, ready_wizard(), "1.0.0");
        let setup = setup_page(&view).into_string().to_ascii_lowercase();
        let dashboard = dashboard_page(&DashboardView::new(SetupState::Ready, "1.0.0"))
            .into_string()
            .to_ascii_lowercase();
        let error = error_page(SetupState::Setup, "not_loopback", "denied")
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
