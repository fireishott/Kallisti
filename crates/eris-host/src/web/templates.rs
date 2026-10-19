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

use crate::db::SetupState;
use crate::onboarding::{
    ApprovalPolicy, AutopilotCeiling, PermissionRow, Permissions, RowControl, SandboxMode, Step,
    Telemetry, Wizard,
};

use super::css::APP_CSS;

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
            provider_base_url: None,
            provider_key_ref: None,
            model: None,
            probed_at: None,
            model_count: 0,
            devices: Vec::new(),
            permissions: Permissions::DEFAULT,
            permission_rows: Vec::new(),
            device_listener: false,
            error: None,
            notice: None,
        }
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

/// The document shell every page shares.
pub fn layout(title: &str, state: SetupState, body: Markup) -> Markup {
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
                        div .brand { strong { "Kallisti 1.0" } "  Eris daemon" }
                        span class=(state_class) { (state.as_str()) }
                    }
                    (body)
                    footer {
                        "Eris " (crate::admin::VERSION)
                        " | admin listener 127.0.0.1:7340, loopback only"
                        " | reach it over SSH, never a proxy"
                    }
                }
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
fn radio_group(
    legend: &str,
    name: &str,
    options: &[(&'static str, &'static str, &'static str)],
    selected: &str,
    disabled_values: &[&str],
    error: Option<&FormError>,
) -> Markup {
    let invalid = error.is_some_and(|error| error.field.as_deref() == Some(name));
    html! {
        fieldset .field style="border:0;padding:0;margin:0 0 18px" aria-invalid=[invalid.then_some("true")] {
            legend style="font-size:13px;font-weight:600;margin-bottom:8px" { (legend) }
            div .choices {
                @for (value, label, why) in options {
                    @let checked = *value == selected;
                    @let disabled = disabled_values.contains(value);
                    div .choice .selected[checked] .disabled[disabled] {
                        input type="radio" id=(format!("{name}-{}", slug(value))) name=(name) value=(value)
                            checked[checked] disabled[disabled];
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

/// The dashboard page.
pub fn dashboard_page(view: &DashboardView) -> Markup {
    let error = view.error.as_ref();
    let notice = view.notice.as_deref();
    let device_listener_label = if view.device_listener { "on" } else { "off" };
    let body = html! {
        h1 { "Dashboard" }
        p .lede { "Health, provider, devices and the kill switches for this host." }
        (alert_region(error, notice))

        section .card aria-labelledby="health" {
            h2 id="health" { "Health" }
            dl .kv {
                dt { "State" } dd { (view.state.as_str()) }
                dt { "Version" } dd { (view.version) }
                dt { "Database" } dd { @if view.db_ok { "ok" } @else { "unavailable" } }
                dt { "Admin sessions" } dd { (view.sessions_live) }
                dt { "Device listener" } dd { (device_listener_label) " (127.0.0.1:7341, owned by the next phase)" }
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
                    span .hint { "The listener stays on 127.0.0.1:7341 and is off until this is enabled." }
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
    layout("Dashboard", view.state, body)
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
