//! The wizard and dashboard handlers.
//!
//! Two response modes, one handler each. A request is answered as HTML when its
//! `Accept` header names `text/html` (which is what a browser sends for a form
//! post), and as JSON otherwise (which is what `curl` sends). That is what makes
//! the page progressive: a browser gets a 303 to the next step, `curl` gets the
//! documented `{ok:true}` body, and both come from the same validation code.
//!
//! A failed POST is never a redirect. The browser gets the same page back with
//! status 400, the message in an `aria-live` region, and `autofocus` on the field
//! that failed; an API client gets `{ok:false, field, code, message}`.

use std::collections::BTreeMap;

use std::convert::Infallible;

use axum::body::Body;
use axum::extract::FromRequest;
use axum::extract::multipart::Multipart;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::sse::{Event as SseEvent, KeepAlive as SseKeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::admin::{
    AppState, ROUTE_ACTIVITY, ROUTE_BACKUPS, ROUTE_BACKUPS_CREATE, ROUTE_BACKUPS_RESTORE,
    ROUTE_CHAT, ROUTE_CHAT_CLEAR, ROUTE_CHAT_DELETE, ROUTE_CHAT_ID, ROUTE_CHAT_MODE,
    ROUTE_CHAT_NEW, ROUTE_CHAT_SEND, ROUTE_CHAT_SESSIONS, ROUTE_DASHBOARD, ROUTE_DEVICES,
    ROUTE_JOB_CANCEL, ROUTE_LOGIN, ROUTE_SESSION, ROUTE_SESSIONS, ROUTE_SETTINGS,
    ROUTE_SETTINGS_FULL_ACCESS, ROUTE_SETTINGS_PERMISSIONS, ROUTE_SETTINGS_PERSONA,
    ROUTE_SETTINGS_PROVIDER, ROUTE_SETTINGS_WORKSPACE, VERSION, session_path,
};
use crate::auth::{AuthStore, PairingService};
use crate::backup::BackupError;
use crate::error::{ApiError, RequestId};
use crate::onboarding::{
    OnboardingError, PermissionError, Permissions, SETTING_MODEL, SETTING_PROVIDER_BASE_URL,
    SETTING_PROVIDER_KEY_REF, SETTING_PROVIDER_PROBED_AT, SETTING_WORKSPACE, Step, Wizard, session,
    workspace,
};
use crate::persona::{MAX_PERSONA_BYTES, SETTING_PERSONA_ENABLED, character_count};
use crate::web::backups;
use crate::web::chat;
use crate::web::proxy::ClientContext;
use crate::web::store;
use crate::web::templates::{
    ActivityView, BackupsView, ChatLine, ChatView, DEVICE_NAME_LIMIT, DashboardView, DeviceRow,
    DevicesView, FormError, IssuedCodeView, SessionDetailView, SessionsView, SettingsGroup,
    SettingsView, SetupView, activity_page, backups_page, chat_page, dashboard_page, devices_page,
    error_page, login_page, session_page, sessions_page, settings_page, setup_page,
};
use crate::web::wallpaper;

/// The client context the middleware attached, or a plain direct context when a
/// test drives a handler without it.
///
/// The real router always attaches one; this keeps a handler honest about the
/// case where it did not, instead of panicking on an extractor.
fn context_of(context: Option<Extension<ClientContext>>) -> ClientContext {
    match context {
        Some(Extension(context)) => context,
        None => ClientContext::default(),
    }
}

/// The upload ceiling for one form post. Every field is a short string.
const MAX_BODY_BYTES: usize = 16 * 1024;

/// The wizard routes. Registered before the ready gate, because onboarding has to
/// be reachable while the daemon is in `setup`.
pub fn setup_routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/setup", get(wizard))
        .route("/setup/claim", post(claim))
        .route("/setup/provider", post(provider))
        .route("/setup/models", post(models))
        .route("/setup/permissions", post(permissions))
        .route("/setup/workspace", post(workspace))
        .route("/setup/device", post(device))
        .route("/setup/finish", post(finish))
}

/// The dashboard routes. Every one of them sits behind the ready gate, which the
/// admin router applies before merging this group.
///
/// `/login` is here rather than in the wizard group on purpose: signing in is
/// only meaningful once the wizard is closed, and before `ready` the claim form
/// is the surface that owns the credential.
pub fn dashboard_routes() -> axum::Router<AppState> {
    axum::Router::new()
        .route(ROUTE_LOGIN, get(login_form).post(login))
        .route(ROUTE_DASHBOARD, get(dashboard))
        .route(ROUTE_SETTINGS, get(settings))
        .route(ROUTE_SETTINGS_PROVIDER, post(save_provider))
        .route(ROUTE_SETTINGS_WORKSPACE, post(save_workspace))
        .route(ROUTE_SETTINGS_PERMISSIONS, post(save_permissions))
        .route(ROUTE_SETTINGS_PERSONA, post(save_persona))
        .route(ROUTE_SETTINGS_FULL_ACCESS, post(save_full_access))
        .route(ROUTE_ACTIVITY, get(activity))
        .route(ROUTE_SESSIONS, get(sessions))
        .route(ROUTE_SESSION, get(session_detail))
        .route(ROUTE_DEVICES, get(devices).post(mint_pairing_code))
        .route(ROUTE_BACKUPS, get(backups_page_handler))
        .route(ROUTE_BACKUPS_CREATE, post(create_backup))
        .route(ROUTE_BACKUPS_RESTORE, post(restore_backup))
        .route(ROUTE_JOB_CANCEL, post(cancel_job))
        .route(ROUTE_CHAT, get(chat_page_handler))
        .route(ROUTE_CHAT_SEND, post(chat_send))
        .route(ROUTE_CHAT_CLEAR, post(chat_clear))
        .route(ROUTE_CHAT_MODE, post(chat_mode))
        .route(ROUTE_CHAT_NEW, post(chat_new))
        .route(ROUTE_CHAT_SESSIONS, get(chat_sessions))
        .route(ROUTE_CHAT_ID, get(chat_conversation_handler))
        .route(ROUTE_CHAT_DELETE, post(chat_delete))
        .route(wallpaper::ROUTE_WALLPAPER_SAVE, post(save_wallpaper))
        .route(wallpaper::ROUTE_WALLPAPER_UPLOAD, post(upload_wallpaper))
        .route("/dashboard/provider/rotate", post(rotate_provider))
        .route("/dashboard/device/revoke", post(revoke_device))
        .route("/dashboard/mode", post(mode))
}

// ---------------------------------------------------------------------------
// Response helpers
// ---------------------------------------------------------------------------

/// A 200 page.
fn page(markup: maud::Markup) -> Response {
    Html(markup.into_string()).into_response()
}

/// A page with an explicit status, which is how a failed form post is answered.
fn page_with_status(markup: maud::Markup, status: StatusCode) -> Response {
    (status, Html(markup.into_string())).into_response()
}

/// A 303 to a path, which is what a browser should follow after a form post.
fn see_other(location: &'static str) -> Response {
    (
        StatusCode::SEE_OTHER,
        [(header::LOCATION, HeaderValue::from_static(location))],
    )
        .into_response()
}

/// A 303 to a path that is built at run time, as a cancel is: the browser goes
/// back to the page that offered the button.
fn see_other_to(location: String) -> Response {
    match HeaderValue::from_str(&location) {
        Ok(value) => (StatusCode::SEE_OTHER, [(header::LOCATION, value)]).into_response(),
        // A value that cannot be a header is a value a job id cannot be, so this
        // is a refusal rather than a redirect anywhere unexpected.
        Err(_) => see_other(ROUTE_SESSIONS),
    }
}

/// A JSON success body.
fn ok_json(value: serde_json::Value) -> Response {
    (StatusCode::OK, axum::Json(value)).into_response()
}

/// Whether the caller is a browser form post rather than an API client.
fn wants_html(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
}

/// The status a refusal deserves. Every code not listed is a field error.
fn failure_status(code: &str) -> StatusCode {
    match code {
        "rate_limited" => StatusCode::TOO_MANY_REQUESTS,
        "wizard_locked" | "step_out_of_order" | "step_incomplete" => StatusCode::CONFLICT,
        "no_session" => StatusCode::FORBIDDEN,
        "db_error" | "session_db_error" | "first_run_file_failed" => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
        _ => StatusCode::BAD_REQUEST,
    }
}

/// One failure in whichever mode the caller speaks.
fn fail(state: &AppState, as_html: bool, error: FormError) -> Response {
    let status = failure_status(&error.code);
    if as_html {
        match build_setup_view(state) {
            Ok(mut view) => {
                view.error = Some(error);
                page_with_status(setup_page(&view), status)
            }
            // A wizard that cannot be rebuilt cannot be described by a rendered
            // page either; the JSON body is still a complete answer.
            Err(build_error) => refusal(build_error),
        }
    } else {
        refusal(error)
    }
}

/// One failure on the dashboard.
fn fail_dashboard(state: &AppState, as_html: bool, error: FormError) -> Response {
    let status = failure_status(&error.code);
    if as_html {
        match build_dashboard_view(state) {
            Ok(mut view) => {
                view.error = Some(error);
                page_with_status(dashboard_page(&view), status)
            }
            Err(build_error) => refusal(build_error),
        }
    } else {
        refusal(error)
    }
}

/// The JSON refusal: `{ok:false, field, code, message}`.
fn refusal(error: FormError) -> Response {
    let status = failure_status(&error.code);
    (status, axum::Json(error.to_json())).into_response()
}

/// Map an engine refusal onto the wire shape.
fn form_error(error: &OnboardingError) -> FormError {
    FormError::new(error.field(), error.code(), error.to_string())
}

// ---------------------------------------------------------------------------
// Session handling
// ---------------------------------------------------------------------------

/// True when the request carries a live admin session cookie.
fn session_valid(state: &AppState, headers: &HeaderMap) -> bool {
    session::cookie_from_headers(headers).is_some_and(|value| state.sessions.verify(&value).is_ok())
}

/// The 403 a missing session earns, or a 303 to a page that can mint one.
///
/// The destination is a parameter because the two halves of the admin surface
/// have different ones: a wizard post sends the browser back to the wizard, and
/// the dashboard sends it to the sign-in form. An API client always gets the
/// documented `403 no_session`.
fn no_session_to(headers: &HeaderMap, request_id: &str, target: &'static str) -> Response {
    if wants_html(headers) {
        return see_other(target);
    }
    ApiError::new(
        StatusCode::FORBIDDEN,
        "no_session",
        "the admin session cookie is missing or no longer verifies",
        request_id,
    )
    .into_response()
}

/// The refusal for a wizard route: back to the wizard.
fn no_session(headers: &HeaderMap, request_id: &str) -> Response {
    no_session_to(headers, request_id, "/setup")
}

/// The refusal for the dashboard: to the sign-in form, which is the only route
/// that can mint a session once the wizard is closed.
fn no_dashboard_session(headers: &HeaderMap, request_id: &str) -> Response {
    no_session_to(headers, request_id, ROUTE_LOGIN)
}

// ---------------------------------------------------------------------------
// Form bodies
// ---------------------------------------------------------------------------

/// Parse a form body: `application/json` or `application/x-www-form-urlencoded`.
///
/// Both are accepted so `curl -d field=value` and `curl -d '{"field":...}'` work
/// without a second code path, and both arrive as a flat string map.
async fn read_fields(
    headers: &HeaderMap,
    body: Body,
) -> Result<BTreeMap<String, String>, FormError> {
    read_fields_within(headers, body, MAX_BODY_BYTES).await
}

/// Parse a form body with an explicit ceiling.
///
/// The persona form needs more than [`MAX_BODY_BYTES`]: the text is capped at
/// 32 KiB decoded, and form encoding can spend three bytes per character, so a
/// legitimate maximum-sized persona is up to 96 KiB on the wire. Every other
/// form keeps the smaller ceiling.
async fn read_fields_within(
    headers: &HeaderMap,
    body: Body,
    max_bytes: usize,
) -> Result<BTreeMap<String, String>, FormError> {
    let bytes = axum::body::to_bytes(body, max_bytes).await.map_err(|_| {
        FormError::new(
            None,
            "body_too_large",
            format!("the form body must be under {max_bytes} bytes"),
        )
    })?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let text = String::from_utf8(bytes.to_vec())
        .map_err(|_| FormError::new(None, "body_not_utf8", "the form body is not valid UTF-8"))?;

    if content_type.starts_with("application/json") {
        let value: serde_json::Value = serde_json::from_str(&text)
            .map_err(|_| FormError::new(None, "body_not_json", "the JSON body did not parse"))?;
        let object = value.as_object().ok_or_else(|| {
            FormError::new(None, "body_not_object", "the JSON body must be an object")
        })?;
        let mut fields = BTreeMap::new();
        for (key, value) in object {
            let text = match value {
                serde_json::Value::String(text) => text.clone(),
                serde_json::Value::Bool(flag) => flag.to_string(),
                serde_json::Value::Number(number) => number.to_string(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            };
            fields.insert(key.clone(), text);
        }
        return Ok(fields);
    }

    // Form encoded, the browser default.
    let mut fields = BTreeMap::new();
    for pair in text.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (name, value) = match pair.split_once('=') {
            Some((name, value)) => (name, value),
            None => (pair, ""),
        };
        let name = percent_decode(name)?;
        let value = percent_decode(value)?;
        fields.insert(name, value);
    }
    Ok(fields)
}

/// Percent-decode one form component, with `+` as a space.
fn percent_decode(input: &str) -> Result<String, FormError> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' => {
                if index + 3 > bytes.len() {
                    return Err(FormError::new(
                        None,
                        "body_malformed",
                        "a percent escape in the form body is truncated",
                    ));
                }
                let high = hex_digit(bytes[index + 1])?;
                let low = hex_digit(bytes[index + 2])?;
                out.push(high * 16 + low);
                index += 3;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| {
        FormError::new(
            None,
            "body_malformed",
            "a percent escape in the form body is not valid UTF-8",
        )
    })
}

/// One hex digit, or a typed refusal.
fn hex_digit(byte: u8) -> Result<u8, FormError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(FormError::new(
            None,
            "body_malformed",
            "a percent escape in the form body is not hexadecimal",
        )),
    }
}

/// One field, trimmed. A missing field is the empty string, so validation owns
/// the message rather than the extractor.
fn field<'a>(fields: &'a BTreeMap<String, String>, name: &str) -> &'a str {
    fields.get(name).map(String::as_str).unwrap_or("").trim()
}

/// A checkbox-style field: present and not `false` means true.
fn flag(fields: &BTreeMap<String, String>, name: &str) -> bool {
    matches!(field(fields, name), "true" | "on" | "1" | "yes")
}

// ---------------------------------------------------------------------------
// View builders
// ---------------------------------------------------------------------------

/// Assemble the wizard view from stored state. Nothing here invents a value: a
/// missing setting reads as the documented default.
fn build_setup_view(state: &AppState) -> Result<SetupView, FormError> {
    let engine = &state.engine;
    let state_now = engine
        .reconcile_first_boot()
        .map_err(|error| form_error(&error))?;
    let wizard = engine.wizard().map_err(|error| form_error(&error))?;
    let mut view = SetupView::new(state_now, wizard, VERSION);
    view.chrome = state.chrome();
    view.provider_base_url = engine
        .provider_base_url()
        .map_err(|error| form_error(&error))?
        .unwrap_or_default();
    view.provider_key_ref = engine
        .provider_key_ref()
        .map_err(|error| form_error(&error))?
        .unwrap_or_else(|| "env:NINEROUTER_API_KEY".to_owned());
    view.models = engine
        .provider_models()
        .map_err(|error| form_error(&error))?;
    view.model = engine
        .model()
        .map_err(|error| form_error(&error))?
        .unwrap_or_default();
    view.permissions = engine.permissions().map_err(|error| form_error(&error))?;
    view.permission_rows = engine
        .permission_rows()
        .map_err(|error| form_error(&error))?;
    view.workspace = engine
        .workspace_path()
        .map_err(|error| form_error(&error))?
        .display()
        .to_string();
    view.device_listener = engine
        .device_listener_enabled()
        .map_err(|error| form_error(&error))?;
    Ok(view)
}

/// Assemble the dashboard view from stored state.
fn build_dashboard_view(state: &AppState) -> Result<DashboardView, FormError> {
    let engine = &state.engine;
    let db = engine.db();
    let mut view = DashboardView::new(engine.state().map_err(|error| form_error(&error))?, VERSION);
    view.chrome = state.chrome();
    view.db_ok = db.quick_check().is_ok();
    view.sessions_live = state.sessions.live_count().unwrap_or(0);
    view.device_sessions = store::session_count(db).map_err(|error| db_error(&error))?;
    view.devices_known = store::device_count(db).map_err(|error| db_error(&error))?;
    view.paired_devices = engine
        .paired_device_count()
        .map_err(|error| form_error(&error))?;
    view.jobs = store::jobs_by_status(db).map_err(|error| db_error(&error))?;
    view.provider_base_url = engine
        .provider_base_url()
        .map_err(|error| form_error(&error))?;
    view.provider_key_ref = engine
        .provider_key_ref()
        .map_err(|error| form_error(&error))?;
    view.model = engine.model().map_err(|error| form_error(&error))?;
    view.probed_at = engine
        .setting(SETTING_PROVIDER_PROBED_AT)
        .map_err(|error| form_error(&error))?;
    view.model_count = engine
        .provider_models()
        .map_err(|error| form_error(&error))?
        .len();
    view.devices = read_devices(&engine.auth()).map_err(|error| form_error(&error))?;
    view.permissions = engine.permissions().map_err(|error| form_error(&error))?;
    view.permission_rows = engine
        .permission_rows()
        .map_err(|error| form_error(&error))?;
    view.device_listener = engine
        .device_listener_enabled()
        .map_err(|error| form_error(&error))?;
    view.device_listener_bound = state.device.is_bound();
    Ok(view)
}

/// Assemble the settings view from stored state.
///
/// Every field is a database read with the documented default behind it, so the
/// page renders what is stored and never a placeholder that disagrees with the
/// rows.
fn build_settings_view(state: &AppState) -> Result<SettingsView, FormError> {
    let engine = &state.engine;
    let mut view = SettingsView::new(engine.state().map_err(|error| form_error(&error))?, VERSION);
    view.chrome = state.chrome();
    view.allow_full = state.allow_full();
    view.assistant_name = engine
        .setting(crate::persona::SETTING_ASSISTANT_NAME)
        .ok()
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| crate::persona::DEFAULT_ASSISTANT_NAME.to_owned());
    view.allow_full_from_flag = state.allow_full;
    view.allow_full_stored = matches!(
        engine.setting(crate::onboarding::SETTING_ALLOW_FULL),
        Ok(Some(ref value)) if value.trim().eq_ignore_ascii_case("true")
    );
    view.provider_base_url = engine
        .provider_base_url()
        .map_err(|error| form_error(&error))?
        .unwrap_or_default();
    view.provider_key_ref = engine
        .provider_key_ref()
        .map_err(|error| form_error(&error))?
        .unwrap_or_else(|| "env:NINEROUTER_API_KEY".to_owned());
    view.model = engine
        .model()
        .map_err(|error| form_error(&error))?
        .unwrap_or_default();
    view.workspace = engine
        .workspace_path()
        .map_err(|error| form_error(&error))?
        .display()
        .to_string();
    view.permissions = engine.permissions().map_err(|error| form_error(&error))?;
    view.paired_devices = engine
        .paired_device_count()
        .map_err(|error| form_error(&error))?;
    view.device_listener = engine
        .device_listener_enabled()
        .map_err(|error| form_error(&error))?;
    view.device_listener_bound = state.device.is_bound();
    // The persona is a file, not a row: the page shows what the next turn would
    // send, and a read failure is stated rather than rendered as an empty box.
    view.persona_path = state.persona.path().display().to_string();
    view.persona_text = state.persona.read_text().unwrap_or_default();
    view.persona_enabled = state.persona.effective_enabled(
        engine
            .setting(SETTING_PERSONA_ENABLED)
            .map_err(|error| form_error(&error))?
            .as_deref(),
    );
    let (selected, custom_present) = wallpaper_choice(state)?;
    view.wallpaper = selected;
    view.wallpaper_custom = custom_present;
    Ok(view)
}

/// Assemble the activity view: the newest hundred audit rows.
fn build_activity_view(state: &AppState) -> Result<ActivityView, FormError> {
    let engine = &state.engine;
    let mut view = ActivityView::new(engine.state().map_err(|error| form_error(&error))?);
    view.chrome = state.chrome();
    view.rows =
        store::audit_recent(engine.db(), AUDIT_PAGE_SIZE).map_err(|error| db_error(&error))?;
    view.total = store::audit_count(engine.db()).map_err(|error| db_error(&error))?;
    Ok(view)
}

/// How many audit rows the activity page shows.
const AUDIT_PAGE_SIZE: i64 = 100;

/// A database failure as a form error, so a page is never replaced by a blank
/// 200: the caller gets a 500 with a rendered message.
fn db_error(error: &crate::db::DbError) -> FormError {
    FormError::new(
        None,
        "db_error",
        format!("the database read failed: {error}"),
    )
}

/// Every device row, newest first.
fn read_devices(store: &AuthStore) -> Result<Vec<DeviceRow>, OnboardingError> {
    // The same facts that unlock `never`: a redeemed code or a live token. A row
    // with nothing behind it is "awaiting pairing", not "paired".
    const SQL: &str = "
SELECT d.id, d.name, d.platform, d.paired_at, d.revoked_at,
       (
         EXISTS (
           SELECT 1 FROM auth_tokens t
           WHERE t.device_id = d.id AND t.revoked_at IS NULL AND t.rotated_at IS NULL
         )
         OR EXISTS (
           SELECT 1 FROM pairing_codes p
           WHERE p.device_id = d.id AND p.consumed_at IS NOT NULL
         )
       ) AS paired
FROM devices d
ORDER BY d.created_at DESC";
    store
        .db()
        .with_conn(|conn| {
            let mut statement = conn.prepare(SQL)?;
            let mut rows = statement.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(DeviceRow {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    platform: row.get(2)?,
                    paired_at: row.get(3)?,
                    revoked_at: row.get(4)?,
                    paired: row.get(5)?,
                });
            }
            Ok(out)
        })
        .map_err(OnboardingError::Db)
}

/// The wire shape of a permission tuple.
fn permissions_json(permissions: Permissions) -> serde_json::Value {
    serde_json::json!({
        "sandbox_mode": permissions.sandbox_mode.as_str(),
        "approval_policy": permissions.approval_policy.as_str(),
        "autopilot_ceiling": permissions.autopilot_ceiling.as_str(),
        "telemetry": permissions.telemetry.as_str(),
        "external_comm": "draft only",
        "financial": "disabled",
        "network": "loopback only",
    })
}

/// Serialise a [`Wizard`].
fn wizard_json(wizard: &Wizard) -> serde_json::Value {
    let steps: Vec<serde_json::Value> = wizard
        .steps
        .iter()
        .map(|status| {
            serde_json::json!({
                "id": status.step.as_str(),
                "title": status.step.title(),
                "optional": status.optional,
                "completed_at": status.completed_at,
            })
        })
        .collect();
    serde_json::json!({
        "ok": true,
        "state": wizard.state.as_str(),
        "next_step": wizard.next_step.map(Step::as_str),
        "steps": steps,
    })
}

// ---------------------------------------------------------------------------
// GET handlers
// ---------------------------------------------------------------------------

/// `GET /setup` - the wizard, at whichever step the rows say.
async fn wizard(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
) -> Response {
    match build_setup_view(&state) {
        Ok(view) => page(setup_page(&view)),
        Err(error) => ApiError::internal(request_id.0, error.message).into_response(),
    }
}

/// `GET /setup/state` - wizard progress as JSON.
pub async fn setup_state(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_session(&headers, &request_id.0);
    }
    match state.engine.wizard() {
        Ok(wizard) => ok_json(wizard_json(&wizard)),
        Err(error) => refusal(form_error(&error)),
    }
}

/// `GET /dashboard` - health, provider, devices, kill switches.
///
/// A browser without a session is sent to the sign-in form rather than being
/// shown a JSON refusal: landing on a 403 page is a dead end, and the whole point
/// of the listener is a browser session. An API client keeps the documented
/// `403 no_session`.
async fn dashboard(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    match build_dashboard_view(&state) {
        Ok(view) => page(dashboard_page(&view)),
        Err(error) => ApiError::internal(request_id.0, error.message).into_response(),
    }
}

/// `GET /dashboard/settings` - the three forms, pre-filled from the database.
///
/// The optional `saved` query value names the group that was just written, so a
/// successful save lands on the page with the notice in that group's own live
/// region rather than in a banner at the top that the reader has to connect to
/// the form below it.
async fn settings(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    match build_settings_view(&state) {
        Ok(mut view) => {
            if let Some(group) = saved_group(&uri) {
                view.notice = Some((group, group.saved_notice().to_owned()));
            }
            page(settings_page(&view))
        }
        Err(error) => ApiError::internal(request_id.0, error.message).into_response(),
    }
}

/// `GET /dashboard/activity` - the audit trail, newest first.
async fn activity(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    match build_activity_view(&state) {
        Ok(view) => page(activity_page(&view)),
        Err(error) => ApiError::internal(request_id.0, error.message).into_response(),
    }
}

/// `GET /dashboard/sessions` - every device session, newest first.
async fn sessions(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    match build_sessions_view(&state) {
        Ok(view) => page(sessions_page(&view)),
        Err(error) => ApiError::internal(request_id.0, error.message).into_response(),
    }
}

/// `GET /dashboard/sessions/{id}` - one session, its jobs, and their events.
///
/// An id that names no row is a rendered 404 with the error template, never a
/// 200 with an empty page: a blank page looks like a session with nothing in it,
/// which is a different and wrong answer.
async fn session_detail(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    match build_session_detail_view(&state, &id) {
        Ok(Some(view)) => page(session_page(&view)),
        Ok(None) => missing_page(
            &state,
            &request_id.0,
            &headers,
            "session_not_found",
            &format!("no session with id {id} exists on this host"),
        ),
        Err(error) => ApiError::internal(request_id.0, error.message).into_response(),
    }
}

/// `GET /dashboard/devices` - every device, and the form that mints a code.
async fn devices(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    match build_devices_view(&state) {
        Ok(view) => page(devices_page(&view)),
        Err(error) => ApiError::internal(request_id.0, error.message).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Backups
//
// The heavy lifting is `crate::backup`, unchanged. These handlers add three
// things and nothing else: a page over the archive directory, one POST that
// writes an archive, and one POST that restores one behind a filename-twice
// confirmation gate.
// ---------------------------------------------------------------------------

/// `GET /dashboard/backups` - every archive, whether each verifies, and the two
/// forms.
///
/// The query value names what just happened (`created` or `restored`) so the
/// notice survives the 303 that follows a form post. It is a filename in a URL,
/// not a path: it was generated here or validated as a bare name before it got
/// into the redirect.
async fn backups_page_handler(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    match build_backups_view(&state) {
        Ok(mut view) => {
            view.notice = backup_notice(&view, &uri);
            page(backups_page(&view))
        }
        Err(error) => ApiError::internal(request_id.0, error.message).into_response(),
    }
}

/// `POST /dashboard/backups/create` - write one archive into the archive
/// directory and audit it.
///
/// The name is generated by the daemon, never supplied by the caller, so this
/// route has no field that could name a path. A failure is never an empty 200:
/// the browser gets the page back with the reason and the CLI's exit class, and
/// an API client gets the code and the class in the body.
async fn create_backup(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let as_html = wants_html(&headers);
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    let paths = state.engine.paths().clone();
    match backups::create_archive(&paths) {
        Ok(report) => {
            let name = report
                .archive
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            // A digest is not a secret, and it is the one field that lets an
            // operator match the audit row to the file on disk.
            let detail = format!(
                "filename={name} sha256={} bytes={}",
                report.archive_sha256, report.archive_bytes
            );
            match store::audit_write(state.db(), "loopback", "backup.create", Some(&detail)) {
                Ok(()) => see_other_to(format!(
                    "{ROUTE_BACKUPS}?created={}",
                    backups::query_encode(&name)
                )),
                Err(error) => backup_refusal(
                    &state,
                    as_html,
                    &request_id.0,
                    FormError::new(
                        None,
                        "db_error",
                        format!(
                            "the archive {name} was written and verified, but the audit row could not be recorded: {error}"
                        ),
                    ),
                    StatusCode::INTERNAL_SERVER_ERROR,
                ),
            }
        }
        Err(error) => backup_failure(&state, as_html, &request_id.0, &error),
    }
}

/// `POST /dashboard/backups/restore` - replace the live state from one archive.
///
/// Destructive, so the gate is in this order and every step refuses before the
/// one below it can touch the filesystem:
///
/// 1. `archive` must be a bare filename. A `/`, `..`, backslash, leading dot, or
///    absolute path is a `400 bad_archive`, and the refusal never echoes the
///    value back.
/// 2. It must name a regular file in the archive directory.
/// 3. `confirm` must equal that filename exactly, or the request is a `400
///    confirm_mismatch` that says which name it expected.
/// 4. Then, and only then, the existing restore path runs: verify every member
///    into a sink, refuse a newer schema, take a safety copy, place `eris.db`
///    last. Nothing here restarts the service.
///
/// A verification failure is answered 422, an I/O or database failure 500, and a
/// refusal 400, so the CLI's exit classes stay visible over HTTP.
async fn restore_backup(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let as_html = wants_html(&headers);
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => {
            return backup_refusal(
                &state,
                as_html,
                &request_id.0,
                error,
                StatusCode::BAD_REQUEST,
            );
        }
    };

    // 1. A bare filename, checked before any filesystem call.
    let archive = field(&fields, "archive").to_owned();
    if let Err(reason) = backups::validate_archive_name(&archive) {
        return backup_refusal(
            &state,
            as_html,
            &request_id.0,
            FormError::new(
                Some("archive"),
                "bad_archive",
                format!("that is not an archive in the backups directory: {reason}"),
            ),
            StatusCode::BAD_REQUEST,
        );
    }

    // 2. It has to be a regular file in that one directory. `symlink_metadata`
    //    does not follow a link, so a link planted there is not an archive.
    let path = backups::directory(state.engine.paths()).join(&archive);
    let is_file = std::fs::symlink_metadata(&path)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false);
    if !is_file {
        return backup_refusal(
            &state,
            as_html,
            &request_id.0,
            FormError::new(
                Some("archive"),
                "archive_not_found",
                "no archive with that name is in the backups directory",
            ),
            StatusCode::BAD_REQUEST,
        );
    }

    // 3. The typed confirmation, exactly.
    let confirm = fields.get("confirm").map(String::as_str).unwrap_or("");
    if confirm != archive {
        return backup_refusal(
            &state,
            as_html,
            &request_id.0,
            FormError::new(
                Some("confirm"),
                "confirm_mismatch",
                format!("type the archive filename exactly to confirm: {archive}"),
            ),
            StatusCode::BAD_REQUEST,
        );
    }

    // 4. The verified, safety-copied restore.
    let paths = state.engine.paths().clone();
    match backups::restore_archive(&paths, &archive) {
        Ok(report) => {
            let safety = report
                .safety_copy
                .as_ref()
                .map_or_else(|| "none".to_owned(), |path| path.display().to_string());
            let detail = format!(
                "archive={archive} safety_copy={safety} sha256={} members={}",
                report.archive_sha256, report.members
            );
            match backups::audit_placed_restore(&paths, &detail) {
                Ok(_) => see_other_to(format!(
                    "{ROUTE_BACKUPS}?restored={}",
                    backups::query_encode(&archive)
                )),
                Err(error) => backup_refusal(
                    &state,
                    as_html,
                    &request_id.0,
                    FormError::new(
                        Some("archive"),
                        "audit_write_failed",
                        format!(
                            "the archive {archive} was restored, but recording it in the restored database failed: {error}"
                        ),
                    ),
                    StatusCode::INTERNAL_SERVER_ERROR,
                ),
            }
        }
        Err(error) => backup_failure(&state, as_html, &request_id.0, &error),
    }
}

/// Assemble the backups view: the state, and every archive with its own
/// verification answer.
fn build_backups_view(state: &AppState) -> Result<BackupsView, FormError> {
    let engine = &state.engine;
    let mut view = BackupsView::new(engine.state().map_err(|error| form_error(&error))?);
    view.chrome = state.chrome();
    view.rows = backups::list(engine.paths()).map_err(|error| {
        FormError::new(
            None,
            error.code(),
            format!("cannot read the archive directory: {error}"),
        )
    })?;
    Ok(view)
}

/// The notice a redirect carries, or `None` for a plain visit.
fn backup_notice(view: &BackupsView, uri: &Uri) -> Option<String> {
    if let Some(name) = query_value(uri, "created") {
        let size = view
            .rows
            .iter()
            .find(|row| row.name == name)
            .map(|row| row.bytes);
        return Some(match size {
            Some(bytes) => format!("Created {name} ({bytes} bytes) and verified every member."),
            None => format!("Created {name}."),
        });
    }
    if let Some(name) = query_value(uri, "restored") {
        return Some(format!(
            "Restored {name}. The daemon still holds the replaced database file open, so run \
             systemctl --user restart kallisti-eris for the restored state to take effect."
        ));
    }
    None
}

/// One query value, percent-decoded, or `None`.
fn query_value(uri: &Uri, key: &str) -> Option<String> {
    for pair in uri.query()?.split('&') {
        let (name, value) = match pair.split_once('=') {
            Some((name, value)) => (name, value),
            None => (pair, ""),
        };
        if name == key {
            return Some(backups::query_decode(value));
        }
    }
    None
}

/// The status a backup failure deserves.
///
/// This is the CLI's exit class, kept visible: a tampered archive is a
/// verification failure and its own status, so a script cannot mistake it for
/// the disk being full.
fn backup_status(error: &BackupError) -> StatusCode {
    match error.exit_code() {
        crate::backup::EXIT_VERIFICATION => StatusCode::UNPROCESSABLE_ENTITY,
        crate::backup::EXIT_USAGE => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// The CLI exit code that matches an HTTP refusal, for the JSON body and the
/// rendered line.
fn cli_exit(status: StatusCode) -> u8 {
    match status {
        StatusCode::UNPROCESSABLE_ENTITY => crate::backup::EXIT_VERIFICATION,
        StatusCode::BAD_REQUEST => crate::backup::EXIT_USAGE,
        _ => crate::backup::EXIT_FAILURE,
    }
}

/// One backup layer failure, in whichever mode the caller speaks.
fn backup_failure(
    state: &AppState,
    as_html: bool,
    request_id: &str,
    error: &BackupError,
) -> Response {
    backup_refusal(
        state,
        as_html,
        request_id,
        FormError::new(Some("archive"), error.code(), error.to_string()),
        backup_status(error),
    )
}

/// A backups refusal: the page with the reason for a browser, the code and the
/// CLI exit class for an API client.
fn backup_refusal(
    state: &AppState,
    as_html: bool,
    request_id: &str,
    error: FormError,
    status: StatusCode,
) -> Response {
    let exit = cli_exit(status);
    if !as_html {
        let mut body = error.to_json();
        if let Some(object) = body.as_object_mut() {
            object.insert("exit_code".to_owned(), serde_json::json!(exit));
        }
        return (status, axum::Json(body)).into_response();
    }
    // The rendered line carries the class too: the status line is the first
    // thing a browser throws away, and "which kind of failure" is the one thing
    // a person reading the page needs.
    let FormError {
        field,
        code,
        message,
    } = error;
    let shown = FormError::new(
        field.as_deref(),
        code,
        format!("{message} (CLI exit code {exit})"),
    );
    match build_backups_view(state) {
        Ok(mut view) => {
            view.error = Some(shown);
            page_with_status(backups_page(&view), status)
        }
        // A page that cannot be rebuilt cannot describe the failure either; the
        // JSON body is still a complete answer, and a 500 is honest.
        Err(build) => ApiError::internal(request_id.to_owned(), build.message).into_response(),
    }
}

/// `POST /dashboard/devices` - mint one pairing code for a device name.
///
/// The code exists in this response and nowhere else: the pairing service stores
/// only an Argon2id verifier, this handler never writes the value, and the audit
/// row carries the device name. Registering the device it names is deliberate:
/// the row is what the table then shows as `awaiting code` until a phone redeems
/// it.
async fn mint_pairing_code(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let as_html = wants_html(&headers);
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return mint_refusal(&state, as_html, &request_id.0, error),
    };
    let name = field(&fields, "device_name").trim().to_owned();
    if name.is_empty() {
        return mint_refusal(
            &state,
            as_html,
            &request_id.0,
            FormError::new(
                Some("device_name"),
                "device_required",
                "a device name is required to mint a pairing code",
            ),
        );
    }
    if name.chars().count() > DEVICE_NAME_LIMIT {
        return mint_refusal(
            &state,
            as_html,
            &request_id.0,
            FormError::new(
                Some("device_name"),
                "device_name_too_long",
                format!("a device name must be at most {DEVICE_NAME_LIMIT} characters"),
            ),
        );
    }

    let db = state.engine.db();
    let store = state.engine.auth();
    let device_id = match store::device_id_by_name(db, &name) {
        Ok(Some(id)) => id,
        Ok(None) => match store.register_device(&name, "unknown") {
            Ok(id) => id,
            Err(error) => {
                return mint_refusal(
                    &state,
                    as_html,
                    &request_id.0,
                    FormError::new(Some("device_name"), error.code(), error.to_string()),
                );
            }
        },
        Err(error) => {
            return ApiError::internal(request_id.0, db_error(&error).message).into_response();
        }
    };

    // The one pairing path in the binary: the same service the wizard mints from.
    let pairing = PairingService::new(db.clone());
    let issued = match pairing.issue_for_device(&device_id) {
        Ok(issued) => issued,
        Err(error) => {
            return mint_refusal(
                &state,
                as_html,
                &request_id.0,
                FormError::new(None, error.code(), error.to_string()),
            );
        }
    };

    // The audit row names the device and never the code.
    if let Err(error) = store.audit("loopback", "device.pairing_code_issued", Some(&name)) {
        return ApiError::internal(
            request_id.0,
            format!("the code was minted but the audit row could not be written: {error}"),
        )
        .into_response();
    }

    let expires_at = crate::auth::format_timestamp(issued.expires_at());
    if as_html {
        match build_devices_view(&state) {
            Ok(mut view) => {
                view.issued = Some(IssuedCodeView {
                    code: issued.code().to_owned(),
                    expires_at,
                });
                page(devices_page(&view))
            }
            Err(error) => ApiError::internal(request_id.0, error.message).into_response(),
        }
    } else {
        ok_json(serde_json::json!({
            "ok": true,
            "device_id": device_id,
            "device_name": name,
            "code": issued.code(),
            "expires_at": expires_at,
        }))
    }
}

/// `POST /dashboard/jobs/{id}/cancel` - cancel a queued or running job.
///
/// A job that is already terminal is a 400 with the reason a person needs, and
/// the response writes nothing: a refusal that half-applies is worse than no
/// button.
async fn cancel_job(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let as_html = wants_html(&headers);
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    let db = state.engine.db();
    let job = match store::job_get(db, &id) {
        Ok(Some(job)) => job,
        Ok(None) => {
            return missing_page(
                &state,
                &request_id.0,
                &headers,
                "job_not_found",
                &format!("no job with id {id} exists on this host"),
            );
        }
        Err(error) => {
            return ApiError::internal(request_id.0, db_error(&error).message).into_response();
        }
    };

    if !job.is_cancellable() {
        let error = FormError::new(
            None,
            "job_not_cancellable",
            format!(
                "job {id} is already {}: only a queued or running job can be cancelled",
                job.status
            ),
        );
        return cancel_refusal(
            &state,
            as_html,
            &request_id.0,
            job.session_id.as_deref(),
            error,
        );
    }

    let at = crate::auth::format_timestamp(crate::auth::now());
    match store::cancel_job(db, &id, &at) {
        Ok(true) => {
            if let Err(error) = state
                .engine
                .auth()
                .audit("loopback", "job.cancel", Some(&id))
            {
                return ApiError::internal(
                    request_id.0,
                    format!("the job was cancelled but the audit row failed: {error}"),
                )
                .into_response();
            }
            if as_html {
                let target = job
                    .session_id
                    .as_deref()
                    .map(session_path)
                    .unwrap_or_else(|| ROUTE_SESSIONS.to_owned());
                see_other_to(target)
            } else {
                ok_json(serde_json::json!({
                    "ok": true,
                    "job_id": id,
                    "status": "cancelled",
                }))
            }
        }
        // The row changed between the read and the update. The conditional
        // update is why this branch exists at all.
        Ok(false) => cancel_refusal(
            &state,
            as_html,
            &request_id.0,
            job.session_id.as_deref(),
            FormError::new(
                None,
                "job_not_cancellable",
                format!(
                    "job {id} reached a terminal status while this request was in flight: only a                      queued or running job can be cancelled"
                ),
            ),
        ),
        Err(error) => ApiError::internal(request_id.0, db_error(&error).message).into_response(),
    }
}

/// Assemble the sessions list.
fn build_sessions_view(state: &AppState) -> Result<SessionsView, FormError> {
    let engine = &state.engine;
    let mut view = SessionsView::new(engine.state().map_err(|error| form_error(&error))?);
    view.chrome = state.chrome();
    view.rows = store::sessions_list(engine.db()).map_err(|error| db_error(&error))?;
    Ok(view)
}

/// Assemble one session's page, or `None` when the id names no row.
fn build_session_detail_view(
    state: &AppState,
    id: &str,
) -> Result<Option<SessionDetailView>, FormError> {
    let db = state.engine.db();
    let Some(session) = store::session_get(db, id).map_err(|error| db_error(&error))? else {
        return Ok(None);
    };
    let mut view = SessionDetailView::new(
        state.engine.state().map_err(|error| form_error(&error))?,
        session,
    );
    view.chrome = state.chrome();
    view.jobs = store::jobs_for_session(db, id).map_err(|error| db_error(&error))?;
    Ok(Some(view))
}

/// Assemble the devices page.
fn build_devices_view(state: &AppState) -> Result<DevicesView, FormError> {
    let engine = &state.engine;
    let mut view = DevicesView::new(engine.state().map_err(|error| form_error(&error))?);
    view.chrome = state.chrome();
    view.rows = store::devices_list(engine.db()).map_err(|error| db_error(&error))?;
    Ok(view)
}

/// A rendered 404 for a browser, the documented JSON refusal for an API client.
fn missing_page(
    state: &AppState,
    request_id: &str,
    headers: &HeaderMap,
    code: &str,
    message: &str,
) -> Response {
    if !wants_html(headers) {
        return ApiError::new(StatusCode::NOT_FOUND, code, message, request_id).into_response();
    }
    match state.engine.state() {
        Ok(state_now) => page_with_status(
            error_page(state_now, code, message, &state.chrome()),
            StatusCode::NOT_FOUND,
        ),
        Err(error) => {
            ApiError::internal(request_id, format!("cannot read the setup state: {error}"))
                .into_response()
        }
    }
}

/// A refused cancel. The browser gets the session page back with the reason in
/// its live region, so the button is not a dead end; an API client gets JSON.
fn cancel_refusal(
    state: &AppState,
    as_html: bool,
    request_id: &str,
    session_id: Option<&str>,
    error: FormError,
) -> Response {
    if !as_html {
        return refusal(error);
    }
    if let Some(session_id) = session_id
        && let Ok(Some(mut view)) = build_session_detail_view(state, session_id)
    {
        view.error = Some(error);
        return page_with_status(session_page(&view), StatusCode::BAD_REQUEST);
    }
    match state.engine.state() {
        Ok(state_now) => page_with_status(
            error_page(state_now, &error.code, &error.message, &state.chrome()),
            StatusCode::BAD_REQUEST,
        ),
        Err(engine_error) => ApiError::internal(
            request_id,
            format!("cannot read the setup state: {engine_error}"),
        )
        .into_response(),
    }
}

/// A refused mint: the devices page again, with the reason in its live region.
fn mint_refusal(state: &AppState, as_html: bool, request_id: &str, error: FormError) -> Response {
    let status = failure_status(&error.code);
    if !as_html {
        return refusal(error);
    }
    match build_devices_view(state) {
        Ok(mut view) => {
            view.error = Some(error);
            page_with_status(devices_page(&view), status)
        }
        Err(build_error) => ApiError::internal(request_id, build_error.message).into_response(),
    }
}

/// `GET /login` - the sign-in form, or a shortcut past it.
async fn login_form(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    if session_valid(&state, &headers) {
        return see_other(ROUTE_DASHBOARD);
    }
    match state.engine.state() {
        Ok(state_now) => page(login_page(state_now, None, &state.chrome())),
        Err(error) => ApiError::internal(
            request_id.0,
            format!("cannot read the setup state: {error}"),
        )
        .into_response(),
    }
}

/// `POST /login` - verify the admin passphrase and mint the session cookie.
///
/// This is the route the daemon was missing: after onboarding finished, nothing
/// could turn the operator's passphrase into a session, so the dashboard was
/// unreachable from a browser. It writes nothing on the way in, and a refusal
/// never mints a cookie.
///
/// A successful post is always a 303 to the dashboard, whatever `Accept` says:
/// the cookie is the answer, and the destination is the page the caller asked
/// for by signing in.
async fn login(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    context: Option<Extension<ClientContext>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let as_html = wants_html(&headers);
    let context = context_of(context);

    // Rate limit before parsing, exactly as the claim form does, so a malformed
    // flood costs an attempt too and the budget cannot be reset by garbage. The
    // budget is per effective client.
    if !state.limiter.allow_for(context.client_ip) {
        let error = FormError::new(
            None,
            "rate_limited",
            format!(
                "too many sign-in attempts: {} per {} seconds, then the window has to pass",
                state.limiter.limit(),
                crate::onboarding::CLAIM_WINDOW.as_secs()
            ),
        );
        return login_refusal(&state, as_html, error);
    }

    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return login_refusal(&state, as_html, error),
    };
    let passphrase = field(&fields, "passphrase");
    if passphrase.is_empty() {
        let error = FormError::new(
            Some("passphrase"),
            "passphrase_required",
            "the admin passphrase is required",
        );
        return login_refusal(&state, as_html, error);
    }

    if let Err(error) = state.engine.login(passphrase) {
        return login_refusal(&state, as_html, login_error(&error));
    }

    // Minted exactly as the claim step mints it: one row, one cookie, and the
    // secret exists only in this response.
    let minted = match state.sessions.mint() {
        Ok(minted) => minted,
        Err(error) => {
            return ApiError::internal(
                request_id.0,
                format!("cannot mint the admin session: {error}"),
            )
            .into_response();
        }
    };
    if let Err(error) = store::audit_write(
        state.db(),
        &context.actor(),
        "admin.signed_in",
        Some(if context.trusted {
            "login via trusted proxy"
        } else {
            "login direct"
        }),
    ) {
        return ApiError::internal(
            request_id.0,
            format!("cannot write the sign-in audit row: {error}"),
        )
        .into_response();
    }
    let cookie =
        session::set_cookie_header(&minted.cookie_value, state.access.cookie_secure(&context));
    let mut response = see_other(ROUTE_DASHBOARD);
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

/// The status a failed sign-in deserves.
///
/// A credential that did not verify, or a host with none stored, is a 401: the
/// caller is not authenticated. Everything else keeps the shared map, so a rate
/// limit is still a 429 and an empty field is still a 400.
fn login_status(code: &str) -> StatusCode {
    match code {
        "admin_passphrase_rejected" | "admin_passphrase_required" => StatusCode::UNAUTHORIZED,
        other => failure_status(other),
    }
}

/// One sign-in failure in whichever mode the caller speaks.
fn login_refusal(state: &AppState, as_html: bool, error: FormError) -> Response {
    let status = login_status(&error.code);
    if !as_html {
        return (status, axum::Json(error.to_json())).into_response();
    }
    match state.engine.state() {
        Ok(state_now) => {
            page_with_status(login_page(state_now, Some(&error), &state.chrome()), status)
        }
        // The page needs the state badge; without it the JSON body still tells
        // the truth, so the refusal is never a 200 with a hole in it.
        Err(_) => (status, axum::Json(error.to_json())).into_response(),
    }
}

/// The wire shape of a sign-in refusal, in plain English.
///
/// The codes are the engine's, unchanged, so a script can branch on them. The
/// prose is written here because the engine's variants are named for the
/// `eris setup --reset` path they were added for.
fn login_error(error: &OnboardingError) -> FormError {
    match error {
        OnboardingError::AdminPassphraseRejected => FormError::new(
            Some("passphrase"),
            "admin_passphrase_rejected",
            "that passphrase does not match the admin credential on this host",
        ),
        OnboardingError::AdminPassphraseRequired => FormError::new(
            Some("passphrase"),
            "admin_passphrase_required",
            "this host has no admin credential stored yet",
        ),
        other => form_error(other),
    }
}

// ---------------------------------------------------------------------------
// Wizard POST handlers
// ---------------------------------------------------------------------------

/// `POST /setup/claim`.
async fn claim(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    context: Option<Extension<ClientContext>>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let as_html = wants_html(&headers);
    let context = context_of(context);

    // The wizard is locked once the daemon is ready, before anything is read.
    if let Err(error) = state.engine.ensure_wizard_open() {
        return fail(&state, as_html, form_error(&error));
    }

    // Rate limit before parsing, so a malformed flood costs an attempt too. The
    // budget is per effective client, so a proxy in front of this daemon cannot
    // pool every client into one budget.
    if !state.limiter.allow_for(context.client_ip) {
        let error = FormError::new(
            None,
            "rate_limited",
            format!(
                "too many claim attempts: {} per {} seconds, then the window has to pass",
                state.limiter.limit(),
                crate::onboarding::CLAIM_WINDOW.as_secs()
            ),
        );
        return refusal(error);
    }

    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return refusal(error),
    };
    let passphrase = field(&fields, "passphrase");
    if passphrase.is_empty() {
        let error = FormError::new(
            Some("passphrase"),
            "passphrase_required",
            "the six-word setup passphrase is required",
        );
        return fail(&state, as_html, error);
    }

    if let Err(error) = state.engine.accept_claim(passphrase) {
        return fail(&state, as_html, form_error(&error));
    }

    // The session is minted once, here, and the secret exists only in this
    // response.
    let minted = match state.sessions.mint() {
        Ok(minted) => minted,
        Err(error) => {
            return ApiError::internal(
                request_id.0,
                format!("cannot mint the admin session: {error}"),
            )
            .into_response();
        }
    };
    // The sign-in is audited against the real client, not the proxy in front of
    // it. The detail names the route, never a credential.
    if let Err(error) = store::audit_write(
        state.db(),
        &context.actor(),
        "admin.signed_in",
        Some(if context.trusted {
            "setup/claim via trusted proxy"
        } else {
            "setup/claim direct"
        }),
    ) {
        return ApiError::internal(
            request_id.0,
            format!("cannot write the sign-in audit row: {error}"),
        )
        .into_response();
    }
    let cookie =
        session::set_cookie_header(&minted.cookie_value, state.access.cookie_secure(&context));
    let mut response = if as_html {
        see_other("/setup")
    } else {
        ok_json(serde_json::json!({
            "ok": true,
            "state": "setup",
            "next_step": "provider",
            "session_expires_at": crate::auth::format_timestamp(minted.expires_at),
        }))
    };
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

/// `POST /setup/provider` - probe the endpoint, store the reference and the list.
async fn provider(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let as_html = wants_html(&headers);
    if !session_valid(&state, &headers) {
        return no_session(&headers, &request_id.0);
    }
    if let Err(error) = state.engine.ensure_wizard_open() {
        return fail(&state, as_html, form_error(&error));
    }
    if let Err(error) = state.engine.require_step(Step::Provider) {
        return fail(&state, as_html, form_error(&error));
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return refusal(error),
    };

    let base_url = field(&fields, "base_url").to_owned();
    let key_ref = field(&fields, "api_key_ref").to_owned();
    if base_url.is_empty() {
        return fail(
            &state,
            as_html,
            FormError::new(
                Some("base_url"),
                "provider_endpoint_invalid",
                "an endpoint is required",
            ),
        );
    }

    // Validate the reference before resolving it, so a pasted key value is
    // refused by name instead of becoming an environment lookup.
    let reference = match core_domain::SecretRef::try_from(key_ref.as_str()) {
        Ok(reference) => reference,
        Err(source) => {
            return fail(
                &state,
                as_html,
                FormError::new(
                    Some("api_key_ref"),
                    "provider_key_ref_invalid",
                    format!("the key reference must be env:NAME: {source}"),
                ),
            );
        }
    };
    let key = match core_runtime::provider::resolve_api_key(&reference) {
        Ok(key) => key,
        Err(error) => {
            return fail(
                &state,
                as_html,
                FormError::new(
                    Some("api_key_ref"),
                    "provider_key_unresolved",
                    format!("{error}. The daemon resolves env:NAME from its own environment."),
                ),
            );
        }
    };
    let profile = match core_domain::ProviderProfile::new(
        "onboarding-probe",
        base_url.clone(),
        reference.clone(),
    ) {
        Ok(profile) => profile,
        Err(error) => {
            return fail(
                &state,
                as_html,
                FormError::new(
                    Some("base_url"),
                    "provider_endpoint_invalid",
                    format!("{error}"),
                ),
            );
        }
    };

    // The probe is the point: one real call to the endpoint the operator typed,
    // before anything is stored.
    let catalog = match core_runtime::provider::list_models(&profile, &key).await {
        Ok(catalog) => catalog,
        Err(error) => {
            return fail(
                &state,
                as_html,
                FormError::new(
                    Some("base_url"),
                    "provider_probe_failed",
                    format!("the probe against {base_url} failed: {error}"),
                ),
            );
        }
    };

    if let Err(error) = state
        .engine
        .set_provider(&base_url, &reference.wire(), &catalog.models)
    {
        return fail(&state, as_html, form_error(&error));
    }
    let _ = state.engine.set_provider_contexts(&catalog.context_lengths);

    if as_html {
        see_other("/setup")
    } else {
        ok_json(serde_json::json!({
            "ok": true,
            "base_url": base_url,
            "api_key_ref": reference.wire(),
            "model_count": catalog.models.len(),
            "models": catalog.models,
            "next_step": "models",
        }))
    }
}

/// `POST /setup/models`.
async fn models(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let as_html = wants_html(&headers);
    if !session_valid(&state, &headers) {
        return no_session(&headers, &request_id.0);
    }
    if let Err(error) = state.engine.ensure_wizard_open() {
        return fail(&state, as_html, form_error(&error));
    }
    if let Err(error) = state.engine.require_step(Step::Models) {
        return fail(&state, as_html, form_error(&error));
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return refusal(error),
    };
    let model = field(&fields, "model");
    match state.engine.set_model(model) {
        Ok(()) => {
            if as_html {
                see_other("/setup")
            } else {
                ok_json(serde_json::json!({
                    "ok": true,
                    "model": model,
                    "next_step": "permissions",
                }))
            }
        }
        Err(error) => fail(&state, as_html, form_error(&error)),
    }
}

/// `POST /setup/permissions`.
async fn permissions(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let as_html = wants_html(&headers);
    if !session_valid(&state, &headers) {
        return no_session(&headers, &request_id.0);
    }
    if let Err(error) = state.engine.ensure_wizard_open() {
        return fail(&state, as_html, form_error(&error));
    }
    if let Err(error) = state.engine.require_step(Step::Permissions) {
        return fail(&state, as_html, form_error(&error));
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return refusal(error),
    };

    // Onboarding passes `ready = false`, so `never` is refused here by design,
    // and `full` is refused because `validate` never allows it.
    let parsed = Permissions::validate(
        field(&fields, "sandbox_mode"),
        field(&fields, "approval_policy"),
        field(&fields, "autopilot_ceiling"),
        field(&fields, "telemetry"),
        state.engine.paired_device_count().unwrap_or(0) > 0,
        false,
    );
    let permissions = match parsed {
        Ok(permissions) => permissions,
        Err(error) => {
            return fail(
                &state,
                as_html,
                FormError::new(Some(error.field()), error.code(), error.to_string()),
            );
        }
    };

    match state.engine.set_permissions(permissions) {
        Ok(()) => {
            if as_html {
                see_other("/setup")
            } else {
                ok_json(serde_json::json!({
                    "ok": true,
                    "permissions": permissions_json(permissions),
                    "next_step": "workspace",
                }))
            }
        }
        Err(error) => fail(&state, as_html, form_error(&error)),
    }
}

/// `POST /setup/workspace`.
async fn workspace(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let as_html = wants_html(&headers);
    if !session_valid(&state, &headers) {
        return no_session(&headers, &request_id.0);
    }
    if let Err(error) = state.engine.ensure_wizard_open() {
        return fail(&state, as_html, form_error(&error));
    }
    if let Err(error) = state.engine.require_step(Step::Workspace) {
        return fail(&state, as_html, form_error(&error));
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return refusal(error),
    };
    match state.engine.set_workspace(field(&fields, "workspace_path")) {
        Ok(prepared) => {
            if as_html {
                see_other("/setup")
            } else {
                ok_json(serde_json::json!({
                    "ok": true,
                    "workspace": prepared.path.display().to_string(),
                    "created": prepared.created,
                    "next_step": "device",
                }))
            }
        }
        Err(error) => fail(&state, as_html, form_error(&error)),
    }
}

/// `POST /setup/device` - mint one pairing code, the optional step.
async fn device(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let as_html = wants_html(&headers);
    if !session_valid(&state, &headers) {
        return no_session(&headers, &request_id.0);
    }
    if let Err(error) = state.engine.ensure_wizard_open() {
        return fail(&state, as_html, form_error(&error));
    }
    if let Err(error) = state.engine.require_step(Step::Device) {
        return fail(&state, as_html, form_error(&error));
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return refusal(error),
    };
    let submitted = field(&fields, "device_name");
    let name = if submitted.is_empty() {
        "iPhone".to_owned()
    } else {
        submitted.to_owned()
    };

    let store = state.engine.auth();
    let device_id = match store.register_device(&name, "ios") {
        Ok(id) => id,
        Err(error) => {
            return fail(
                &state,
                as_html,
                FormError::new(Some("device_name"), error.code(), error.to_string()),
            );
        }
    };
    let issued = match PairingService::new(state.db().clone()).issue_for_device(&device_id) {
        Ok(issued) => issued,
        Err(error) => {
            return fail(
                &state,
                as_html,
                FormError::new(None, error.code(), error.to_string()),
            );
        }
    };
    let expires_at = crate::auth::format_timestamp(issued.expires_at());
    let code = issued.code().to_owned();
    if let Err(error) = state.engine.complete_step(Step::Device) {
        return fail(&state, as_html, form_error(&error));
    }

    if as_html {
        // The code is shown once, on the page that just minted it.
        match build_setup_view(&state) {
            Ok(mut view) => {
                view.pairing_code = Some(code);
                view.pairing_expires = Some(expires_at);
                page(setup_page(&view))
            }
            Err(build_error) => refusal(build_error),
        }
    } else {
        ok_json(serde_json::json!({
            "ok": true,
            "device_id": device_id,
            "device_name": name,
            "pairing_code": code,
            "expires_at": expires_at,
            "next_step": "finish",
        }))
    }
}

/// `POST /setup/finish` - lock the wizard and move to `ready`.
async fn finish(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let as_html = wants_html(&headers);
    if !session_valid(&state, &headers) {
        return no_session(&headers, &request_id.0);
    }
    match state.engine.finish() {
        Ok(()) => {
            if as_html {
                see_other("/dashboard")
            } else {
                ok_json(serde_json::json!({
                    "ok": true,
                    "state": "ready",
                    "next_step": serde_json::Value::Null,
                }))
            }
        }
        Err(error) => fail(&state, as_html, form_error(&error)),
    }
}

// ---------------------------------------------------------------------------
// Dashboard POST handlers
// ---------------------------------------------------------------------------

/// `POST /dashboard/provider/rotate` - re-read the key reference and re-probe.
///
/// Rotation is about the environment, not the database: what is stored is a
/// reference (`env:NAME`), so replacing the key in the daemon's environment and
/// rotating here is the whole operation. Nothing is ever written back as a key.
async fn rotate_provider(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    let as_html = wants_html(&headers);
    if !session_valid(&state, &headers) {
        return no_session(&headers, &request_id.0);
    }
    let base_url = match state.engine.provider_base_url() {
        Ok(Some(base_url)) => base_url,
        Ok(None) => {
            return fail_dashboard(
                &state,
                as_html,
                FormError::new(
                    Some("base_url"),
                    "provider_endpoint_invalid",
                    "no endpoint is stored",
                ),
            );
        }
        Err(error) => return refusal(form_error(&error)),
    };
    let key_ref = match state.engine.provider_key_ref() {
        Ok(Some(key_ref)) => key_ref,
        Ok(None) => {
            return fail_dashboard(
                &state,
                as_html,
                FormError::new(
                    Some("api_key_ref"),
                    "provider_key_ref_invalid",
                    "no key reference is stored",
                ),
            );
        }
        Err(error) => return refusal(form_error(&error)),
    };
    let reference = match core_domain::SecretRef::try_from(key_ref.as_str()) {
        Ok(reference) => reference,
        Err(source) => {
            return fail_dashboard(
                &state,
                as_html,
                FormError::new(
                    Some("api_key_ref"),
                    "provider_key_ref_invalid",
                    format!("{source}"),
                ),
            );
        }
    };
    let key = match core_runtime::provider::resolve_api_key(&reference) {
        Ok(key) => key,
        Err(error) => {
            return fail_dashboard(
                &state,
                as_html,
                FormError::new(
                    Some("api_key_ref"),
                    "provider_key_unresolved",
                    format!("{error}"),
                ),
            );
        }
    };
    let profile = match core_domain::ProviderProfile::new(
        "dashboard-probe",
        base_url.clone(),
        reference.clone(),
    ) {
        Ok(profile) => profile,
        Err(error) => {
            return fail_dashboard(
                &state,
                as_html,
                FormError::new(
                    Some("base_url"),
                    "provider_endpoint_invalid",
                    format!("{error}"),
                ),
            );
        }
    };

    match core_runtime::provider::list_models(&profile, &key).await {
        Ok(catalog) => {
            // A model that the provider no longer lists is cleared, so a stale
            // default cannot outlive the endpoint that served it.
            if let Ok(Some(model)) = state.engine.model()
                && !catalog.models.iter().any(|known| known == &model)
            {
                let _ = state.engine.clear_setting(SETTING_MODEL);
            }
            if let Err(error) =
                state
                    .engine
                    .set_provider(&base_url, &reference.wire(), &catalog.models)
            {
                return refusal(form_error(&error));
            }
            let _ = state.engine.set_provider_contexts(&catalog.context_lengths);
            if as_html {
                render_dashboard_ok(&state, format!("probe ok: {} models", catalog.models.len()))
            } else {
                ok_json(serde_json::json!({
                    "ok": true,
                    "base_url": base_url,
                    "model_count": catalog.models.len(),
                    "models": catalog.models,
                    "context_lengths": catalog.context_lengths,
                }))
            }
        }
        Err(error) => fail_dashboard(
            &state,
            as_html,
            FormError::new(
                Some("base_url"),
                "provider_probe_failed",
                format!("the probe against {base_url} failed: {error}"),
            ),
        ),
    }
}

/// A dashboard render with a success line.
fn render_dashboard_ok(state: &AppState, notice: String) -> Response {
    match build_dashboard_view(state) {
        Ok(mut view) => {
            view.notice = Some(notice);
            page(dashboard_page(&view))
        }
        Err(error) => refusal(error),
    }
}

/// `POST /dashboard/device/revoke`.
async fn revoke_device(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let as_html = wants_html(&headers);
    if !session_valid(&state, &headers) {
        return no_session(&headers, &request_id.0);
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return refusal(error),
    };
    let device_id = field(&fields, "device_id");
    if device_id.is_empty() {
        return fail_dashboard(
            &state,
            as_html,
            FormError::new(
                Some("device_id"),
                "device_required",
                "a device id is required",
            ),
        );
    }
    let store = state.engine.auth();
    match store.revoke_device(device_id) {
        Ok(revoked) => {
            let _ = store.audit("loopback", "dashboard.device_revoked", Some(device_id));
            if as_html {
                render_dashboard_ok(&state, format!("revoked {revoked} token(s) for the device"))
            } else {
                ok_json(serde_json::json!({
                    "ok": true,
                    "device_id": device_id,
                    "tokens_revoked": revoked,
                }))
            }
        }
        Err(error) => fail_dashboard(
            &state,
            as_html,
            FormError::new(Some("device_id"), error.code(), error.to_string()),
        ),
    }
}

/// `POST /dashboard/mode` - the kill switches.
async fn mode(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let as_html = wants_html(&headers);
    if !session_valid(&state, &headers) {
        return no_session(&headers, &request_id.0);
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return refusal(error),
    };

    let paired = state.engine.paired_device_count().unwrap_or(0) > 0;
    let parsed = Permissions::validate_with(
        field(&fields, "sandbox_mode"),
        field(&fields, "approval_policy"),
        field(&fields, "autopilot_ceiling"),
        field(&fields, "telemetry"),
        paired,
        true,
        state.allow_full(),
    );
    let permissions = match parsed {
        Ok(permissions) => permissions,
        Err(error) => {
            return fail_dashboard(
                &state,
                as_html,
                FormError::new(Some(error.field()), error.code(), error.to_string()),
            );
        }
    };
    if let Err(error) = state.engine.set_permissions(permissions) {
        return refusal(form_error(&error));
    }
    let listener = flag(&fields, "device_listener_enabled");
    if let Err(error) = state.engine.set_device_listener(listener) {
        return refusal(form_error(&error));
    }

    // The switch is not only a row. Enabling binds 127.0.0.1:7341 now, in this
    // process, with the same `DeviceService` the daemon built at start-up;
    // disabling stops new turns immediately and leaves the socket bound, so the
    // SSE streams of running turns are not cut off by a kill switch.
    state.device.set_accepting(listener);
    if listener {
        state.device.start();
    }

    if as_html {
        render_dashboard_ok(&state, "kill switches saved".to_owned())
    } else {
        ok_json(serde_json::json!({
            "ok": true,
            "permissions": permissions_json(permissions),
            "device_listener_enabled": listener,
            "device_listener_bound": state.device.is_bound(),
        }))
    }
}

// ---------------------------------------------------------------------------
// Settings POST handlers
// ---------------------------------------------------------------------------

/// The redirect after a successful save, one per group.
///
/// The query value is the notice: the page reads it and renders the sentence in
/// that group's own live region, so success is never silent and never a banner
/// the reader has to match to a form.
const SAVED_PROVIDER: &str = "/dashboard/settings?saved=provider";
const SAVED_WORKSPACE: &str = "/dashboard/settings?saved=workspace";
const SAVED_PERMISSIONS: &str = "/dashboard/settings?saved=permissions";
const SAVED_PERSONA: &str = "/dashboard/settings?saved=persona";

/// The group named by `?saved=`, when it names one this build knows.
fn saved_group(uri: &Uri) -> Option<SettingsGroup> {
    let query = uri.query()?;
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if name != "saved" {
            continue;
        }
        return match value {
            "provider" => Some(SettingsGroup::Provider),
            "workspace" => Some(SettingsGroup::Workspace),
            "permissions" => Some(SettingsGroup::Permissions),
            "persona" => Some(SettingsGroup::Persona),
            _ => None,
        };
    }
    None
}

/// One failure on the settings page: the same page back with the message in the
/// failed group's live region.
///
/// The status is the shared map's, which is 400 for a field error, 429 for a rate
/// limit and 500 for a database failure. A refusal is never a redirect and never
/// an empty body.
fn fail_settings(state: &AppState, as_html: bool, error: FormError) -> Response {
    let status = failure_status(&error.code);
    if as_html {
        match build_settings_view(state) {
            Ok(mut view) => {
                view.error = Some(error);
                page_with_status(settings_page(&view), status)
            }
            Err(build_error) => refusal(build_error),
        }
    } else {
        refusal(error)
    }
}

/// A database write failure, as a 500 with a body rather than a silent redirect.
fn store_failure(request_id: &str, what: &str, error: crate::db::DbError) -> Response {
    ApiError::internal(
        request_id.to_owned(),
        format!("cannot store {what}: {error}"),
    )
    .into_response()
}

/// An audit write failure, which is a 500 for the same reason: a settings change
/// that was not recorded did not happen as far as the product is concerned.
fn audit_failure(request_id: &str, action: &str, error: crate::auth::AuthError) -> Response {
    ApiError::internal(
        request_id.to_owned(),
        format!("cannot append the {action} audit row: {error}"),
    )
    .into_response()
}

/// `POST /dashboard/settings/provider` - endpoint, key reference, model.
///
/// What is stored is three strings, one of which is the *name* of an environment
/// variable. No key value is accepted, stored, echoed, or logged: the reference
/// is validated against `env:NAME` and nothing resolves it here.
async fn save_provider(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let as_html = wants_html(&headers);
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return fail_settings(&state, as_html, error),
    };
    let (base_url, key_ref, model) = match validate_provider(
        field(&fields, "base_url"),
        field(&fields, "api_key_ref"),
        field(&fields, "model"),
    ) {
        Ok(parts) => parts,
        Err(error) => return fail_settings(&state, as_html, error),
    };

    let db = state.engine.db();
    for (key, value) in [
        (SETTING_PROVIDER_BASE_URL, base_url.as_str()),
        (SETTING_PROVIDER_KEY_REF, key_ref.as_str()),
        (SETTING_MODEL, model.as_str()),
    ] {
        if let Err(error) = store::settings_put(db, key, value) {
            return store_failure(&request_id.0, key, error);
        }
    }

    let detail = format!("base_url={base_url}; api_key_ref={key_ref}; model={model}");
    if let Err(error) = state
        .engine
        .auth()
        .audit("loopback", "settings.provider", Some(&detail))
    {
        return audit_failure(&request_id.0, "settings.provider", error);
    }

    // Success is always a redirect, whatever `Accept` says: these three routes
    // exist for a form, the page behind the redirect carries the notice, and a
    // second success body would be a third way to read one answer. A refusal
    // still answers an API client in JSON, because a script needs the field and
    // the code.
    see_other(SAVED_PROVIDER)
}

/// `POST /dashboard/settings/workspace` - the one directory a turn may write to.
///
/// The validation is [`workspace::prepare`], which is the same function the
/// onboarding step calls: one rule, one implementation, so the dashboard cannot
/// drift into accepting a path the wizard refuses.
async fn save_workspace(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let as_html = wants_html(&headers);
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return fail_settings(&state, as_html, error),
    };
    let prepared = match workspace::prepare(field(&fields, "workspace_path")) {
        Ok(prepared) => prepared,
        Err(error) => {
            return fail_settings(
                &state,
                as_html,
                FormError::new(Some("workspace_path"), error.code(), error.to_string()),
            );
        }
    };
    let path = prepared.path.display().to_string();
    if let Err(error) = store::settings_put(state.engine.db(), SETTING_WORKSPACE, &path) {
        return store_failure(&request_id.0, SETTING_WORKSPACE, error);
    }
    if let Err(error) = state
        .engine
        .auth()
        .audit("loopback", "settings.workspace", Some(&path))
    {
        return audit_failure(&request_id.0, "settings.workspace", error);
    }

    see_other(SAVED_WORKSPACE)
}

/// `POST /dashboard/settings/permissions` - the closed permission tuple.
///
/// Three facts decide it, and none of them come from the form: whether this
/// process was started with `--allow-full`, whether a device is genuinely paired,
/// and whether the daemon is ready. A crafted post cannot supply any of them.
/// The audit detail carries both sides of every value, because "who turned
/// approvals off" is the question this row exists to answer.
async fn save_permissions(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let as_html = wants_html(&headers);
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return fail_settings(&state, as_html, error),
    };

    let old = match state.engine.permissions() {
        Ok(permissions) => permissions,
        Err(error) => return refusal(form_error(&error)),
    };
    let paired = match state.engine.paired_device_count() {
        Ok(count) => count > 0,
        Err(error) => return refusal(form_error(&error)),
    };
    let ready = match state.engine.is_ready() {
        Ok(ready) => ready,
        Err(error) => return refusal(form_error(&error)),
    };

    let parsed = Permissions::validate_with(
        field(&fields, "sandbox_mode"),
        field(&fields, "approval_policy"),
        field(&fields, "autopilot_ceiling"),
        field(&fields, "telemetry"),
        paired,
        ready,
        state.allow_full(),
    );
    let permissions = match parsed {
        Ok(permissions) => permissions,
        Err(error) => return fail_settings(&state, as_html, permission_error(&error)),
    };

    if let Err(error) = state.engine.set_permissions(permissions) {
        return refusal(form_error(&error));
    }
    let detail = format!(
        "sandbox_mode {} -> {}; approval_policy {} -> {}; autopilot_ceiling {} -> {}; telemetry {} -> {}",
        old.sandbox_mode.as_str(),
        permissions.sandbox_mode.as_str(),
        old.approval_policy.as_str(),
        permissions.approval_policy.as_str(),
        old.autopilot_ceiling.as_str(),
        permissions.autopilot_ceiling.as_str(),
        old.telemetry.as_str(),
        permissions.telemetry.as_str(),
    );
    if let Err(error) = state
        .engine
        .auth()
        .audit("loopback", "settings.permissions", Some(&detail))
    {
        return audit_failure(&request_id.0, "settings.permissions", error);
    }

    see_other(SAVED_PERMISSIONS)
}

/// The largest form body the persona route will read.
///
/// The decoded text is capped at [`MAX_PERSONA_BYTES`]; form encoding can spend
/// three bytes per character, so the wire ceiling is four times the cap and no
/// legitimate save is refused for size before the person-sized check runs.
const PERSONA_BODY_BYTES: usize = MAX_PERSONA_BYTES * 4;

/// `POST /dashboard/settings/persona` - the assistant's identity.
///
/// Two facts decide this save and neither is negotiable:
///
/// - over [`MAX_PERSONA_BYTES`] is refused with the count and the cap, because a
///   truncated persona is a persona the operator did not write;
/// - an enabled persona with no text is refused too. "On, with nothing in it" is
///   a setting that silently does nothing, so the page says which way to go
///   instead of storing a switch that has no effect.
///
/// The audit row carries the count and the flag. The text is the operator's own
/// words: it is written to a 0644 file and never to the audit log or a log line.
async fn save_persona(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let as_html = wants_html(&headers);
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    let fields = match read_fields_within(&headers, body, PERSONA_BODY_BYTES).await {
        Ok(fields) => fields,
        Err(error) => return fail_settings(&state, as_html, error),
    };

    let enabled = match fields.get("persona_enabled") {
        Some(_) => flag(&fields, "persona_enabled"),
        // An unchecked checkbox sends nothing. The form always renders the
        // checkbox, so "absent" is "off" here rather than "leave it alone".
        None => false,
    };
    let text = fields.get("persona_text").cloned().unwrap_or_default();

    // The name is a label, not persona text: it is stored even when the persona
    // is off, and an empty box means "the documented default" rather than "".
    let name = field(&fields, "assistant_name").trim().to_owned();
    let name = if name.is_empty() {
        crate::persona::DEFAULT_ASSISTANT_NAME.to_owned()
    } else {
        name
    };
    if name.chars().count() > 40 {
        return fail_settings(
            &state,
            as_html,
            FormError::new(
                Some("assistant_name"),
                "name_too_long",
                "the assistant name must be 40 characters or fewer",
            ),
        );
    }
    if let Err(error) = state
        .engine
        .set_setting(crate::persona::SETTING_ASSISTANT_NAME, &name)
    {
        return fail_settings(
            &state,
            as_html,
            FormError::new(None, "settings_unwritable", error.to_string()),
        );
    }

    if text.len() > MAX_PERSONA_BYTES {
        return fail_settings(
            &state,
            as_html,
            FormError::new(
                Some("persona_text"),
                "persona_too_large",
                format!(
                    "the persona is {} bytes and the limit is {MAX_PERSONA_BYTES} bytes. \
                     Shorten it, or disable the persona",
                    text.len()
                ),
            ),
        );
    }
    if enabled && text.trim().is_empty() {
        return fail_settings(
            &state,
            as_html,
            FormError::new(
                Some("persona_text"),
                "persona_empty",
                "the persona is enabled but the text is empty. Write something, or clear the \
                 checkbox to disable the persona instead",
            ),
        );
    }

    if let Err(error) = state.persona.write(&text) {
        return ApiError::internal(request_id.0.clone(), error.to_string()).into_response();
    }
    let stored = if enabled { "true" } else { "false" };
    if let Err(error) = store::settings_put(state.engine.db(), SETTING_PERSONA_ENABLED, stored) {
        return store_failure(&request_id.0, SETTING_PERSONA_ENABLED, error);
    }
    let detail = format!("enabled={stored}; characters={}", character_count(&text));
    if let Err(error) = state
        .engine
        .auth()
        .audit("loopback", "settings.persona", Some(&detail))
    {
        return audit_failure(&request_id.0, "settings.persona", error);
    }

    see_other(SAVED_PERSONA)
}

/// A permission refusal in the words of the dashboard, not the wizard.
///
/// The engine's `full` message says "onboarding cannot select", which is the
/// wrong sentence on a page that is not onboarding. The code is unchanged, so a
/// script still branches on `sandbox_full_not_available`.
fn permission_error(error: &PermissionError) -> FormError {
    match error {
        PermissionError::FullNotAvailableInOnboarding => FormError::new(
            Some("sandbox_mode"),
            error.code(),
            "full sandbox mode is not available on this process: it needs a restart of eris with --allow-full",
        ),
        PermissionError::NeverRequiresPairedDevice => FormError::new(
            Some("approval_policy"),
            error.code(),
            "never ask needs a paired device and a finished setup: pair a phone first, so there is something that can answer an approval",
        ),
        other => FormError::new(Some(other.field()), other.code(), other.to_string()),
    }
}

/// The rules for a provider save, in one place so the three fields are checked
/// before anything is written.
///
/// The endpoint goes through [`core_domain::ProviderProfile::new`], the same
/// constructor the runtime uses to build a request, so a URL this page accepts is
/// a URL a turn can send to. The reference is checked twice: the shared parser
/// proves the `env:` prefix, and the capitalisation rule proves the name is a
/// variable name rather than a pasted value.
fn validate_provider(
    base_url: &str,
    key_ref: &str,
    model: &str,
) -> Result<(String, String, String), FormError> {
    let reference = core_domain::SecretRef::try_from(key_ref).map_err(|source| {
        FormError::new(
            Some("api_key_ref"),
            "provider_key_ref_invalid",
            format!(
                "the key reference must be env:NAME, with the variable name in capitals: {source}"
            ),
        )
    })?;
    if !is_env_var_name(reference.env_name()) {
        return Err(FormError::new(
            Some("api_key_ref"),
            "provider_key_ref_invalid",
            "env:NAME needs a variable name in capitals: letters, digits and underscores, starting with a letter or an underscore",
        ));
    }

    let profile = core_domain::ProviderProfile::new("settings", base_url, reference.clone())
        .map_err(|source| {
            FormError::new(
                Some("base_url"),
                "provider_endpoint_invalid",
                format!("{source}"),
            )
        })?;
    let base_url = profile.base_url;
    if !endpoint_has_host(&base_url) {
        return Err(FormError::new(
            Some("base_url"),
            "provider_endpoint_invalid",
            format!("{base_url} needs a host after the scheme, as in http://127.0.0.1:20128/v1"),
        ));
    }
    let model = model.trim();
    if model.is_empty() {
        return Err(FormError::new(
            Some("model"),
            "provider_model_required",
            "a model id is required: it is the model every turn starts with",
        ));
    }
    Ok((base_url, reference.wire(), model.to_owned()))
}

/// Whether a string is a shell-style environment variable name in capitals.
fn is_env_var_name(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    if !(first.is_ascii_uppercase() || first == '_') {
        return false;
    }
    characters.all(|character| {
        character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
    })
}

/// Whether an `http(s)` URL carries a host, so `http://` alone is refused.
fn endpoint_has_host(url: &str) -> bool {
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"))
        .unwrap_or("");
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    !authority.is_empty() && !authority.starts_with(':') && !authority.ends_with(':')
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

/// The wallpaper the operator chose, and whether an uploaded image backs it.
fn wallpaper_choice(state: &AppState) -> Result<(String, bool), FormError> {
    let stored = state
        .engine
        .setting(wallpaper::SETTING_WALLPAPER)
        .map_err(|error| FormError::new(None, "settings_unreadable", error.to_string()))?;
    let selected = wallpaper::current(stored.as_deref());
    let custom = wallpaper::custom_file(state.engine.paths()).is_file();
    Ok((selected, custom))
}

/// Whether a provider is stored, which decides whether the composer renders.
fn provider_ready(state: &AppState) -> bool {
    chat::turn_config(&state.engine).ok().flatten().is_some()
}

/// Everything the chat page states, in one place, so the page and every refusal
/// that re-renders it agree.
fn build_chat_view(state: &AppState, requested: Option<&str>) -> Result<ChatView, FormError> {
    let db = state.engine.db();
    let conversation = chat::resolve_conversation(db, requested)
        .map_err(|error| FormError::new(None, "chat_unreadable", error.to_string()))?;
    let transcript = chat::load(db, &conversation)
        .map_err(|error| FormError::new(None, "chat_unreadable", error.to_string()))?;
    let model = state
        .engine
        .setting(SETTING_MODEL)
        .ok()
        .flatten()
        .unwrap_or_default();
    let assistant_name = state
        .engine
        .setting(crate::persona::SETTING_ASSISTANT_NAME)
        .ok()
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| crate::persona::DEFAULT_ASSISTANT_NAME.to_owned());
    let messages = transcript
        .iter()
        .map(|message| {
            let user = message.role == chat::Role::User;
            ChatLine {
                user,
                who: if user {
                    "You".to_owned()
                } else {
                    assistant_name.clone()
                },
                body: message.body.clone(),
                stamp: chat_clock(&message.created_at),
                failed: message.state == chat::State::Failed,
                error_code: message.error_code.clone(),
                pending: message.state == chat::State::Streaming,
                model: message.model.clone(),
                prompt_tokens: message.prompt_tokens,
                completion_tokens: message.completion_tokens,
                cached_tokens: message.cached_tokens,
                duration_ms: message.duration_ms,
                context_window: message.context_window,
            }
        })
        .collect();

    let models = state.engine.provider_models().unwrap_or_default();
    let selected_model = if model.is_empty() {
        models.first().cloned().unwrap_or_default()
    } else {
        model
    };
    let context_window = state
        .engine
        .provider_contexts()
        .ok()
        .and_then(|contexts| contexts.get(&selected_model).copied());
    let mut chrome = state.chrome();
    chrome.current_conversation = Some(conversation.clone());
    let title = chat::conversations(db, chat::SESSION_LIMIT)
        .ok()
        .and_then(|rows| {
            rows.into_iter()
                .find(|row| row.id == conversation)
                .map(|row| row.title)
        })
        .unwrap_or_else(|| chat::NEW_CONVERSATION_TITLE.to_owned());

    Ok(ChatView {
        chrome,
        conversation,
        conversation_title: title,
        state: state.engine.state().unwrap_or(crate::db::SetupState::Ready),
        version: VERSION.to_owned(),
        model: selected_model.clone(),
        messages,
        provider_ready: provider_ready(state),
        error: None,
        stats: chat::stats(&transcript),
        models,
        selected_model,
        assistant_name,
        greeting: crate::web::templates::greeting_for(chat::local_hour(db).unwrap_or(9)).to_owned(),
        sandbox_mode: state
            .engine
            .permissions()
            .map(|permissions| permissions.sandbox_mode.as_str().to_owned())
            .unwrap_or_else(|_| crate::onboarding::SandboxMode::DEFAULT.as_str().to_owned()),
        allow_full: state.allow_full(),
        context_window,
    })
}

/// A clock label, re-exported so this file does not depend on the template's
/// module path for one string operation.
fn chat_clock(stamp: &str) -> String {
    crate::web::templates::clock(stamp)
}

/// `GET /dashboard/chat` - the newest conversation.
async fn chat_page_handler(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    match build_chat_view(&state, None) {
        Ok(view) => page(chat_page(&view)),
        Err(error) => refusal(error),
    }
}

/// `GET /dashboard/chat/{id}` - one named conversation.
async fn chat_conversation_handler(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    match build_chat_view(&state, Some(&id)) {
        Ok(view) => page(chat_page(&view)),
        Err(error) => refusal(error),
    }
}

/// `GET /dashboard/chat/sessions.json`.
///
/// The rail is server-rendered, which means a conversation that gains its name
/// while the page is open has no way to appear until a reload. The streaming
/// script calls this after every turn and re-renders the list in place.
async fn chat_sessions(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    match chat::conversations(state.engine.db(), chat::SESSION_LIMIT) {
        Ok(rows) => {
            let sessions: Vec<serde_json::Value> = rows
                .into_iter()
                .map(|row| {
                    serde_json::json!({
                        "id": row.id,
                        "title": row.title,
                        "stamp": crate::web::templates::clock(&row.updated_at),
                    })
                })
                .collect();
            ok_json(serde_json::json!({ "ok": true, "sessions": sessions }))
        }
        Err(error) => ApiError::internal(
            request_id.0,
            format!("cannot read the conversation list: {error}"),
        )
        .into_response(),
    }
}

/// `POST /dashboard/chat/new` - start a conversation and go to it.
async fn chat_new(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    match chat::create_conversation(state.engine.db()) {
        Ok(id) => see_other_owned(conversation_path(&id)),
        Err(error) => refusal(FormError::new(None, "chat_write_failed", error.to_string())),
    }
}

/// `POST /dashboard/chat/{id}/delete` - drop a conversation and its transcript.
async fn chat_delete(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    match chat::delete_conversation(state.engine.db(), &id) {
        Ok(removed) => {
            let _ = state.engine.auth().audit(
                "loopback",
                "chat.conversation_deleted",
                Some(&format!("{removed} messages")),
            );
            see_other(ROUTE_CHAT)
        }
        Err(error) => refusal(FormError::new(None, "chat_write_failed", error.to_string())),
    }
}

/// `POST /dashboard/settings/full-access`.
///
/// Turning it on requires the operator to type the word, because it is the one
/// switch that can hand a turn the whole host. Turning it off requires nothing:
/// a refusal should cost one click, not a ritual.
async fn save_full_access(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return refusal(error),
    };
    let stored_now = matches!(
        state.engine.setting(crate::onboarding::SETTING_ALLOW_FULL),
        Ok(Some(ref value)) if value.trim().eq_ignore_ascii_case("true")
    );

    if !stored_now {
        // Enabling: the typed word is the whole guard.
        if !field(&fields, "confirm")
            .trim()
            .eq_ignore_ascii_case("full")
        {
            return fail_settings(
                &state,
                wants_html(&headers),
                FormError::new(
                    Some("confirm"),
                    "confirm_required",
                    "type full to enable full host access; nothing was changed",
                ),
            );
        }
        if let Err(error) = state
            .engine
            .set_setting(crate::onboarding::SETTING_ALLOW_FULL, "true")
        {
            return fail_settings(
                &state,
                wants_html(&headers),
                FormError::new(None, "settings_unwritable", error.to_string()),
            );
        }
    } else if let Err(error) = state
        .engine
        .set_setting(crate::onboarding::SETTING_ALLOW_FULL, "false")
    {
        return fail_settings(
            &state,
            wants_html(&headers),
            FormError::new(None, "settings_unwritable", error.to_string()),
        );
    }

    // Revoking the switch also revokes the mode it enabled: leaving `full`
    // selected while the switch is off would mean the stored tuple disagrees with
    // the rule the next save enforces.
    if stored_now
        && let Ok(mut permissions) = state.engine.permissions()
        && permissions.sandbox_mode == crate::onboarding::SandboxMode::Full
    {
        permissions.sandbox_mode = crate::onboarding::SandboxMode::DEFAULT;
        let _ = state.engine.set_permissions(permissions);
    }

    let detail = if stored_now {
        "full host off"
    } else {
        "full host on"
    };
    if let Err(error) = state
        .engine
        .auth()
        .audit("loopback", "settings.full_access", Some(detail))
    {
        return audit_failure(&request_id.0, "settings.full_access", error);
    }
    see_other(ROUTE_SETTINGS)
}

/// The path of one conversation, built in one place.
fn conversation_path(id: &str) -> String {
    format!("/dashboard/chat/{id}")
}

/// A 303 to a path that was built at run time.
fn see_other_owned(location: String) -> Response {
    (
        StatusCode::SEE_OTHER,
        [(header::LOCATION, location.as_str())],
        Body::empty(),
    )
        .into_response()
}

/// `POST /dashboard/chat/send`.
///
/// Two modes, one validation path. A browser that asked for the event stream gets
/// the answer as it arrives; a plain form post waits for the whole turn and comes
/// back to the transcript. The user's message is written before the provider is
/// touched, so a refused turn still shows what was asked and why.
async fn chat_send(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    request: axum::extract::Request,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    // One route, two encodings: a plain form post (`curl -d`, a browser with no
    // script) and a multipart post that can carry a file. The browser's composer
    // always sends multipart once a file is chosen.
    let multipart = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .to_ascii_lowercase()
                .starts_with("multipart/form-data")
        });

    let (fields, files) = if multipart {
        match read_multipart_chat(state.clone(), request).await {
            Ok(pair) => pair,
            Err(error) => return fail_dashboard(&state, wants_html(&headers), error),
        }
    } else {
        let (parts, body) = request.into_parts();
        match read_fields_within(&parts.headers, body, chat::MAX_MESSAGE_BYTES * 4).await {
            Ok(fields) => (fields, Vec::new()),
            Err(error) => return fail_dashboard(&state, wants_html(&headers), error),
        }
    };
    let text = match chat::accept_text(field(&fields, "message")) {
        Ok(text) => text,
        Err(message) => {
            let error = FormError::new(Some("message"), "message_invalid", message);
            return fail_dashboard(&state, wants_html(&headers), error);
        }
    };

    // The provider is checked before anything is written: a daemon with no
    // endpoint has not accepted the message, so the transcript does not gain a
    // question that will never have an answer.
    let Some(config) = chat::turn_config(&state.engine).ok().flatten() else {
        let error = FormError::new(
            None,
            "provider_not_configured",
            "no provider is stored: set the endpoint, the key reference and the model on the settings page",
        );
        return fail_dashboard(&state, wants_html(&headers), error);
    };

    // Which model answers: the composer's choice when it sent one, otherwise the
    // stored default. A choice outside the probe's list is refused by name rather
    // than silently replaced, because a picker that lies about itself is worse
    // than one that is missing.
    let requested = field(&fields, "model").trim().to_owned();
    let model = match resolve_chat_model(&state, &requested, &config.model) {
        Ok(model) => model,
        Err(error) => return fail_dashboard(&state, wants_html(&headers), error),
    };
    let context_window = state
        .engine
        .provider_contexts()
        .ok()
        .and_then(|contexts| contexts.get(&model).copied());

    let db = state.engine.db().clone();
    let conversation = match chat::resolve_conversation(&db, Some(field(&fields, "conversation"))) {
        Ok(conversation) => conversation,
        Err(error) => {
            return refusal(FormError::new(None, "chat_unreadable", error.to_string()));
        }
    };
    if let Err(error) = chat::append(
        &db,
        &conversation,
        chat::Role::User,
        &text,
        chat::State::Complete,
    ) {
        return refusal(FormError::new(None, "chat_write_failed", error.to_string()));
    }

    // The assistant row exists before the first token, so a refresh mid-answer
    // shows the partial reply rather than nothing at all, and it carries the
    // model and window now so a half-finished row is still readable.
    let answer_id = match chat::append_answer(&db, &conversation, &model, context_window) {
        Ok(id) => id,
        Err(error) => return refusal(FormError::new(None, "chat_write_failed", error.to_string())),
    };
    // The turn may run on a model other than the stored default, so the turn
    // config is rebuilt with the chosen id.
    let mut config = config;
    config.model = model.clone();

    // The first question names the conversation, and the rail bumps it to the top.
    let _ = chat::name_from_first_message(&db, &conversation, &text);
    let _ = chat::touch_conversation(&db, &conversation);
    let transcript = match chat::load(&db, &conversation) {
        Ok(transcript) => transcript,
        Err(error) => return refusal(FormError::new(None, "chat_unreadable", error.to_string())),
    };
    let mut history = chat::history(&transcript, &answer_id);
    // Attachments ride after the question, as a second user message built for
    // this turn only: the transcript keeps what the operator typed, and the
    // model still learns what came with it.
    if !files.is_empty() {
        let described = chat::describe_attachments(&files);
        if !described.trim().is_empty() {
            history.push(core_runtime::provider::ChatMessage::text(
                core_runtime::provider::ChatRole::User,
                described,
            ));
        }
    }

    let streaming = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"));

    if !streaming {
        let task = spawn_turn(
            &state,
            config,
            conversation.clone(),
            answer_id,
            history,
            None,
        );
        let _ = task.await;
        return see_other(ROUTE_CHAT);
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<SseEvent, Infallible>>();
    spawn_turn(
        &state,
        config,
        conversation.clone(),
        answer_id,
        history,
        Some(tx),
    );
    Sse::new(UnboundedReceiverStream::new(rx))
        .keep_alive(SseKeepAlive::default())
        .into_response()
}

/// Which model this turn runs on.
///
/// The picker's value when it sent one, checked against the probe's list so a
/// hand-crafted form cannot send a model the endpoint never offered; the stored
/// default otherwise. An empty value is the default, not an error: the plain form
/// posts no model field at all.
fn resolve_chat_model(
    state: &AppState,
    requested: &str,
    default_model: &str,
) -> Result<String, FormError> {
    if requested.is_empty() {
        return Ok(default_model.to_owned());
    }
    if requested.len() > 200 {
        return Err(FormError::new(
            Some("model"),
            "model_invalid",
            "that model id is longer than any endpoint serves",
        ));
    }
    match state.engine.provider_models() {
        Ok(known) if !known.is_empty() => {
            if known.iter().any(|id| id == requested) {
                Ok(requested.to_owned())
            } else {
                Err(FormError::new(
                    Some("model"),
                    "model_not_in_probe",
                    format!("`{requested}` is not in the model list this endpoint returned"),
                ))
            }
        }
        // No probe has run. The endpoint is the authority, so the request is
        // taken at face value rather than refused for want of a list.
        _ => Ok(requested.to_owned()),
    }
}

/// Read a chat form that may carry one file.
///
/// Everything the composer needs fits in one multipart body, so the no-script
/// path is the same route: the browser posts `message`, `model`, `conversation`
/// and an optional `file`. The file is written into the workspace before the
/// turn runs, and the row that records it is written here too, so a file on disk
/// always has a row and a row always has a file.
async fn read_multipart_chat(
    state: AppState,
    request: axum::extract::Request,
) -> Result<
    (
        std::collections::BTreeMap<String, String>,
        Vec<chat::Attachment>,
    ),
    FormError,
> {
    let mut multipart = axum::extract::Multipart::from_request(request, &state)
        .await
        .map_err(|error| {
            FormError::new(
                None,
                "upload_unreadable",
                format!("the form did not parse as multipart: {error}"),
            )
        })?;

    let mut fields = std::collections::BTreeMap::new();
    let mut files: Vec<chat::Attachment> = Vec::new();
    while let Some(mut field) = multipart.next_field().await.map_err(|error| {
        FormError::new(
            None,
            "upload_unreadable",
            format!("the form did not parse as multipart: {error}"),
        )
    })? {
        let name = field.name().unwrap_or_default().to_owned();
        if name != "file" {
            let value = field.text().await.map_err(|error| {
                FormError::new(None, "upload_unreadable", format!("field {name}: {error}"))
            })?;
            fields.insert(name, value);
            continue;
        }
        // A file part. An empty picker sends an empty part with no filename, and
        // that is "no attachment", not an error.
        let Some(original) = field.file_name().map(str::to_owned) else {
            while field
                .chunk()
                .await
                .map_err(|error| {
                    FormError::new(None, "upload_unreadable", format!("file part: {error}"))
                })?
                .is_some()
            {}
            continue;
        };
        if original.trim().is_empty() {
            continue;
        }
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = field.chunk().await.map_err(|error| {
            FormError::new(
                Some("file"),
                "upload_unreadable",
                format!("file part: {error}"),
            )
        })? {
            if bytes.len() + chunk.len() > chat::MAX_ATTACHMENT_BYTES {
                return Err(FormError::new(
                    Some("file"),
                    "upload_too_large",
                    format!(
                        "an attachment must be under {} bytes",
                        chat::MAX_ATTACHMENT_BYTES
                    ),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() {
            continue;
        }
        let safe = chat::safe_file_name(&original);
        let conversation = chat::resolve_conversation(
            state.engine.db(),
            Some(field_value(&fields, "conversation")),
        )
        .map_err(|error| FormError::new(None, "chat_unreadable", error.to_string()))?;
        let directory = state
            .engine
            .workspace_path()
            .map_err(|error| FormError::new(None, "workspace_unreadable", error.to_string()))?
            .join("uploads")
            .join(&conversation);
        std::fs::create_dir_all(&directory).map_err(|error| {
            FormError::new(
                None,
                "upload_write_failed",
                format!("cannot create {}: {error}", directory.display()),
            )
        })?;
        let id = uuid::Uuid::now_v7().to_string();
        let path = directory.join(format!("{id}-{safe}"));
        write_atomically(&path, &bytes).map_err(|error| {
            FormError::new(
                None,
                "upload_write_failed",
                format!("cannot write {}: {error}", path.display()),
            )
        })?;
        let attachment = chat::Attachment {
            id,
            name: safe,
            path: path.display().to_string(),
            size: bytes.len() as u64,
            media_type: chat::media_type_for(&original),
        };
        chat::record_attachment(state.engine.db(), &conversation, &attachment)
            .map_err(|error| FormError::new(None, "chat_write_failed", error.to_string()))?;
        let _ = state.engine.auth().audit(
            "loopback",
            "chat.attachment",
            Some(&format!(
                "{} bytes, {}",
                attachment.size,
                attachment.media_type.as_deref().unwrap_or("unknown type")
            )),
        );
        files.push(attachment);
    }
    Ok((fields, files))
}

/// A field from a map, as a `&str` that outlives the call.
fn field_value<'a>(fields: &'a std::collections::BTreeMap<String, String>, name: &str) -> &'a str {
    fields.get(name).map(String::as_str).unwrap_or("")
}

/// `POST /dashboard/chat/mode`.
///
/// The composer's picker posts one field. Everything else in the permission
/// tuple is left exactly as it was, the same validator runs, and `full` is still
/// refused unless the switch allows it.
async fn chat_mode(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return refusal(error),
    };
    let current = match state.engine.permissions() {
        Ok(permissions) => permissions,
        Err(error) => return refusal(form_error(&error)),
    };
    let parsed = Permissions::validate_with(
        field(&fields, "sandbox_mode"),
        current.approval_policy.as_str(),
        current.autopilot_ceiling.as_str(),
        current.telemetry.as_str(),
        state.engine.paired_device_count().unwrap_or(0) > 0,
        state.engine.is_ready().unwrap_or(false),
        state.allow_full(),
    );
    let mut permissions = match parsed {
        Ok(permissions) => permissions,
        Err(error) => {
            return refusal(FormError::new(
                Some(error.field()),
                error.code(),
                error.to_string(),
            ));
        }
    };
    permissions.approval_policy = current.approval_policy;
    permissions.autopilot_ceiling = current.autopilot_ceiling;
    permissions.telemetry = current.telemetry;
    if let Err(error) = state.engine.set_permissions(permissions) {
        return refusal(form_error(&error));
    }
    let detail = format!(
        "{} -> {}",
        current.sandbox_mode.as_str(),
        permissions.sandbox_mode.as_str()
    );
    if let Err(error) = state
        .engine
        .auth()
        .audit("loopback", "chat.mode", Some(&detail))
    {
        return audit_failure(&request_id.0, "chat.mode", error);
    }
    ok_json(serde_json::json!({
        "ok": true,
        "sandbox_mode": permissions.sandbox_mode.as_str(),
    }))
}

/// `POST /dashboard/chat/clear`.
async fn chat_clear(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return refusal(error),
    };
    let db = state.engine.db();
    let conversation = match chat::resolve_conversation(db, Some(field(&fields, "conversation"))) {
        Ok(conversation) => conversation,
        Err(error) => {
            return refusal(FormError::new(None, "chat_unreadable", error.to_string()));
        }
    };
    match chat::clear(db, &conversation) {
        Ok(removed) => {
            if let Err(error) = state.engine.auth().audit(
                "loopback",
                "chat.cleared",
                Some(&format!("{removed} messages")),
            ) {
                return audit_failure(&request_id.0, "chat.cleared", error);
            }
            see_other(ROUTE_CHAT)
        }
        Err(error) => refusal(FormError::new(None, "chat_clear_failed", error.to_string())),
    }
}

/// Run one chat turn on its own thread, persisting the outcome.
///
/// The sink forwards each fragment to the channel when there is one, and the
/// final row is written before the terminal event is sent, so a browser that
/// reloads right after `done` reads the same text it just saw.
fn spawn_turn(
    state: &AppState,
    config: chat::TurnConfig,
    conversation: String,
    answer_id: String,
    history: Vec<core_runtime::provider::ChatMessage>,
    sender: Option<tokio::sync::mpsc::UnboundedSender<Result<SseEvent, Infallible>>>,
) -> tokio::task::JoinHandle<()> {
    let db = state.engine.db().clone();
    let persona = state.persona.clone();
    // The sandbox is a boundary, so an unreadable workspace or permission row
    // falls back to the narrowest mode rather than to a permissive default.
    let sandbox = {
        let workspace = state
            .engine
            .workspace_path()
            .unwrap_or_else(|_| std::path::PathBuf::from("."));
        let mode = state
            .engine
            .permissions()
            .map(|permissions| permissions.sandbox_mode)
            .unwrap_or(crate::onboarding::permissions::SandboxMode::ReadOnly);
        crate::tools::Sandbox::new(workspace, mode)
    };
    let approval_policy = state
        .engine
        .permissions()
        .map(|permissions| permissions.approval_policy)
        .unwrap_or(crate::onboarding::permissions::ApprovalPolicy::Ask);
    let stored_enabled = state
        .engine
        .setting(crate::persona::SETTING_PERSONA_ENABLED)
        .ok()
        .flatten();

    tokio::task::spawn_blocking(move || {
        let system = match persona.load(stored_enabled.as_deref()) {
            Ok(text) => text,
            Err(error) => {
                let message = error.to_string();
                let _ = chat::fail(&db, &answer_id, error.code(), "");
                send_terminal(
                    &sender,
                    "error",
                    serde_json::json!({
                        "code": error.code(),
                        "message": message,
                    }),
                );
                return;
            }
        };

        // The only place a write can be stopped from reaching the filesystem.
        // `Never` runs in-sandbox work unattended; `Ask` parks every write. A
        // queue that cannot be written refuses the call rather than running it
        // unapproved, because an unrecordable policy is not permission.
        let mut gate = |call: &core_domain::ToolCall| -> chat::GateDecision {
            if call.name != "write_file"
                || approval_policy == crate::onboarding::permissions::ApprovalPolicy::Never
            {
                return chat::GateDecision::Run;
            }
            match crate::approvals::queue(&db, &conversation, &call.name, &call.arguments) {
                Ok(approval_id) => chat::GateDecision::Park { approval_id },
                Err(error) => chat::GateDecision::Refuse {
                    reason: format!(
                        "the approval queue is unavailable ({error}), so nothing was written"
                    ),
                },
            }
        };

        let mut streamed = String::new();
        let mut sink = |fragment: &str| {
            streamed.push_str(fragment);
            send_terminal(&sender, "delta", serde_json::json!({ "text": fragment }));
        };
        let started = std::time::Instant::now();
        let outcome = chat::run_turn(&config, system, history, &sandbox, &mut gate, &mut sink);
        let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        drop(sink);

        match outcome {
            Ok((text, usage)) => {
                let accounting = chat::Accounting {
                    prompt_tokens: usage.input_tokens(),
                    completion_tokens: usage.output_tokens(),
                    cached_tokens: usage.cache_read_tokens(),
                    duration_ms: Some(duration_ms),
                };
                let raw = serde_json::to_value(usage).ok();
                let _ = chat::settle(&db, &answer_id, &text, raw.as_ref(), accounting);
                send_terminal(
                    &sender,
                    "done",
                    serde_json::json!({
                        "id": answer_id,
                        "text": text,
                        "model": config.model,
                        "prompt_tokens": accounting.prompt_tokens,
                        "completion_tokens": accounting.completion_tokens,
                        "cached_tokens": accounting.cached_tokens,
                        "duration_ms": accounting.duration_ms,
                    }),
                );
            }
            Err((code, message)) => {
                let _ = chat::fail(&db, &answer_id, &code, &streamed);
                send_terminal(
                    &sender,
                    "error",
                    serde_json::json!({
                        "code": code,
                        "message": message,
                    }),
                );
            }
        }
    })
}

/// One event, when somebody is listening. A plain form post has no channel and
/// needs none: the row in the database is the answer.
fn send_terminal(
    sender: &Option<tokio::sync::mpsc::UnboundedSender<Result<SseEvent, Infallible>>>,
    event: &str,
    data: serde_json::Value,
) {
    if let Some(sender) = sender {
        let _ = sender.send(Ok(SseEvent::default().event(event).data(data.to_string())));
    }
}

// ---------------------------------------------------------------------------
// Wallpaper
// ---------------------------------------------------------------------------

/// `POST /dashboard/settings/wallpaper`.
async fn save_wallpaper(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return fail_settings(&state, wants_html(&headers), error),
    };
    let choice = field(&fields, "wallpaper");
    if !wallpaper::Preset::is_storable(choice) {
        return fail_settings(
            &state,
            wants_html(&headers),
            FormError::new(
                Some("wallpaper"),
                "wallpaper_unknown",
                format!("`{choice}` is not a wallpaper this build knows"),
            ),
        );
    }
    if choice == wallpaper::CUSTOM && !wallpaper::custom_file(state.engine.paths()).is_file() {
        return fail_settings(
            &state,
            wants_html(&headers),
            FormError::new(
                Some("wallpaper"),
                "wallpaper_not_uploaded",
                "there is no custom image yet: upload one first, then choose it",
            ),
        );
    }
    if let Err(error) = state
        .engine
        .set_setting(wallpaper::SETTING_WALLPAPER, choice)
    {
        return fail_settings(
            &state,
            wants_html(&headers),
            FormError::new(None, "settings_unwritable", error.to_string()),
        );
    }
    if let Err(error) = state
        .engine
        .auth()
        .audit("loopback", "settings.wallpaper", Some(choice))
    {
        return audit_failure(&request_id.0, "settings.wallpaper", error);
    }
    see_other(ROUTE_SETTINGS)
}

/// `POST /dashboard/settings/wallpaper/image`.
///
/// The bytes are checked, not the label: only PNG, JPEG and WebP pass, and the
/// file is written to a temporary name and renamed, so a half-uploaded image is
/// never served.
async fn upload_wallpaper(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }

    let mut bytes: Vec<u8> = Vec::new();
    let mut declared: Option<String> = None;
    loop {
        let next = match multipart.next_field().await {
            Ok(next) => next,
            Err(error) => {
                return fail_settings(
                    &state,
                    wants_html(&headers),
                    FormError::new(
                        None,
                        "upload_unreadable",
                        format!("the upload did not parse: {error}"),
                    ),
                );
            }
        };
        let Some(mut field) = next else { break };
        if field.name() != Some("image") {
            continue;
        }
        declared = field.file_name().map(str::to_owned);
        while let Some(chunk) = match field.chunk().await {
            Ok(chunk) => chunk,
            Err(error) => {
                return fail_settings(
                    &state,
                    wants_html(&headers),
                    FormError::new(
                        None,
                        "upload_unreadable",
                        format!("the upload did not parse: {error}"),
                    ),
                );
            }
        } {
            if bytes.len() + chunk.len() > wallpaper::MAX_UPLOAD_BYTES {
                return fail_settings(
                    &state,
                    wants_html(&headers),
                    FormError::new(
                        Some("image"),
                        "upload_too_large",
                        format!(
                            "the image must be under {} bytes",
                            wallpaper::MAX_UPLOAD_BYTES
                        ),
                    ),
                );
            }
            bytes.extend_from_slice(&chunk);
        }
    }

    if bytes.is_empty() {
        return fail_settings(
            &state,
            wants_html(&headers),
            FormError::new(Some("image"), "upload_empty", "choose an image first"),
        );
    }
    let Some(media_type) = wallpaper::sniff_image(&bytes) else {
        return fail_settings(
            &state,
            wants_html(&headers),
            FormError::new(
                Some("image"),
                "upload_not_an_image",
                "that is not a PNG, JPEG or WebP image; SVG is refused on purpose",
            ),
        );
    };

    let target = wallpaper::custom_file(state.engine.paths());
    let written = write_atomically(&target, &bytes);
    if let Err(error) = written {
        return fail_settings(
            &state,
            wants_html(&headers),
            FormError::new(None, "upload_write_failed", error.to_string()),
        );
    }
    let _ = state
        .engine
        .set_setting(wallpaper::SETTING_CUSTOM_TYPE, media_type);
    let _ = state
        .engine
        .set_setting(wallpaper::SETTING_WALLPAPER, wallpaper::CUSTOM);
    let detail = format!(
        "{media_type}, {} bytes, {}",
        bytes.len(),
        declared.as_deref().unwrap_or("unnamed")
    );
    if let Err(error) =
        state
            .engine
            .auth()
            .audit("loopback", "settings.wallpaper_image", Some(&detail))
    {
        return audit_failure(&request_id.0, "settings.wallpaper_image", error);
    }
    see_other(ROUTE_SETTINGS)
}

/// Write a file beside its destination and rename it into place.
fn write_atomically(target: &std::path::Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let temporary = target.with_extension("upload");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, target)?;
    Ok(())
}

/// `GET /static/wallpaper/{name}`: the presets, and the uploaded image.
///
/// Served before the ready gate, like the stylesheet, because a backdrop is part
/// of the page rather than part of the dashboard.
pub async fn wallpaper_file(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    if name == wallpaper::CUSTOM {
        let stored = state
            .engine
            .setting(wallpaper::SETTING_CUSTOM_TYPE)
            .ok()
            .flatten()
            .unwrap_or_else(|| "image/png".to_owned());
        let media_type = match stored.as_str() {
            "image/jpeg" | "image/webp" => stored,
            _ => "image/png".to_owned(),
        };
        return match std::fs::read(wallpaper::custom_file(state.engine.paths())) {
            Ok(bytes) => (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, media_type),
                    (header::CACHE_CONTROL, "no-cache".to_owned()),
                ],
                bytes,
            )
                .into_response(),
            // A selection with no file behind it is not a 500: the page falls back
            // to the default, and this is the honest answer to a direct request.
            Err(_) => ApiError::not_found(
                "wallpaper".to_owned(),
                "no uploaded wallpaper exists at /static/wallpaper/custom",
            )
            .into_response(),
        };
    }

    if let Some(id) = name
        .strip_suffix(".jpg")
        .or_else(|| name.strip_suffix(".jpeg"))
    {
        return match wallpaper::raster(id) {
            Some(raster) => (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, raster.media_type.to_owned()),
                    (header::CACHE_CONTROL, "public, max-age=86400".to_owned()),
                ],
                raster.bytes,
            )
                .into_response(),
            None => ApiError::not_found(
                name.clone(),
                "that wallpaper does not exist; the image presets are served as .jpg",
            )
            .into_response(),
        };
    }

    let id = name.strip_suffix(".svg").unwrap_or(&name);
    match wallpaper::svg(id) {
        Some(svg) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "image/svg+xml".to_owned()),
                (header::CACHE_CONTROL, "public, max-age=86400".to_owned()),
            ],
            svg,
        )
            .into_response(),
        None => ApiError::not_found(
            name.clone(),
            "that wallpaper does not exist; presets are served as .jpg or .svg",
        )
        .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::SetupState;
    use crate::onboarding::StepStatus;

    #[test]
    fn percent_decoding_handles_spaces_and_escapes() {
        assert_eq!(
            percent_decode("six+words+here").expect("decode"),
            "six words here"
        );
        assert_eq!(
            percent_decode("env%3ANINEROUTER_API_KEY").expect("decode"),
            "env:NINEROUTER_API_KEY"
        );
        assert_eq!(
            percent_decode("~%2Fkallisti").expect("decode"),
            "~/kallisti"
        );
        assert!(percent_decode("%zz").is_err());
        assert!(percent_decode("%4").is_err());
    }

    #[test]
    fn a_browser_is_recognised_by_its_accept_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            "text/html,application/xhtml+xml".parse().expect("value"),
        );
        assert!(wants_html(&headers));
        let mut api = HeaderMap::new();
        api.insert(header::ACCEPT, "*/*".parse().expect("value"));
        assert!(!wants_html(&api));
        assert!(!wants_html(&HeaderMap::new()));
    }

    #[test]
    fn the_status_mapping_is_the_documented_one() {
        assert_eq!(
            failure_status("rate_limited"),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(failure_status("wizard_locked"), StatusCode::CONFLICT);
        assert_eq!(failure_status("step_out_of_order"), StatusCode::CONFLICT);
        assert_eq!(failure_status("no_session"), StatusCode::FORBIDDEN);
        assert_eq!(
            failure_status("db_error"),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        assert_eq!(
            failure_status("passphrase_rejected"),
            StatusCode::BAD_REQUEST
        );
    }

    #[test]
    fn the_wizard_json_shape_is_stable() {
        let wizard = Wizard {
            state: SetupState::Setup,
            steps: Step::ALL
                .iter()
                .map(|step| StepStatus {
                    step: *step,
                    completed_at: None,
                    optional: step.is_optional(),
                })
                .collect(),
            next_step: Some(Step::Claim),
        };
        let json = wizard_json(&wizard);
        assert_eq!(json["ok"], true);
        assert_eq!(json["state"], "setup");
        assert_eq!(json["next_step"], "claim");
        assert_eq!(json["steps"].as_array().map(Vec::len), Some(7));
        assert_eq!(json["steps"][5]["optional"], true);
        assert_eq!(json["steps"][5]["id"], "device");
    }

    #[test]
    fn a_checkbox_field_reads_the_browser_spellings() {
        let mut fields = BTreeMap::new();
        fields.insert("device_listener_enabled".to_owned(), "on".to_owned());
        assert!(flag(&fields, "device_listener_enabled"));
        fields.insert("device_listener_enabled".to_owned(), "false".to_owned());
        assert!(!flag(&fields, "device_listener_enabled"));
        assert!(!flag(&fields, "absent"));
    }
}
