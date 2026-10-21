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
use std::io::Read as _;

use axum::body::Body;
use axum::extract::FromRequest;
use axum::extract::multipart::Multipart;
use axum::extract::{DefaultBodyLimit, Extension, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::sse::{Event as SseEvent, KeepAlive as SseKeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::admin::{
    AppState, ROUTE_ACTIVITY, ROUTE_BACKUPS, ROUTE_BACKUPS_CREATE, ROUTE_BACKUPS_RESTORE,
    ROUTE_CHAT, ROUTE_CHAT_CLEAR, ROUTE_CHAT_DELETE, ROUTE_CHAT_FILE, ROUTE_CHAT_ID,
    ROUTE_CHAT_MODE, ROUTE_CHAT_NEW, ROUTE_CHAT_SEND, ROUTE_CHAT_SESSIONS, ROUTE_CHAT_WORKSPACE,
    ROUTE_DASHBOARD, ROUTE_DEVICES, ROUTE_JOB_CANCEL, ROUTE_LOGIN, ROUTE_LOGOUT, ROUTE_PROJECTS,
    ROUTE_PROJECTS_ASSIGN, ROUTE_SECURITY, ROUTE_SESSION, ROUTE_SESSIONS, ROUTE_SETTINGS,
    ROUTE_SETTINGS_AUX, ROUTE_SETTINGS_DEVICE, ROUTE_SETTINGS_FULL_ACCESS, ROUTE_SETTINGS_GREETING,
    ROUTE_SETTINGS_MCP, ROUTE_SETTINGS_MEMORY, ROUTE_SETTINGS_PERMISSIONS, ROUTE_SETTINGS_PERSONA,
    ROUTE_SETTINGS_PROVIDER, ROUTE_SETTINGS_PROVIDERS, ROUTE_SETTINGS_QUOTA,
    ROUTE_SETTINGS_REASONING, ROUTE_SETTINGS_STREAMING, ROUTE_SETTINGS_TIMEZONE,
    ROUTE_SETTINGS_TOOLS, ROUTE_SETTINGS_WORKSPACE, ROUTE_SIDEBAR, VERSION, session_path,
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
    ActivityView, Attachment as ChatAttachment, BackupsView, ChatLine, ChatView, DEVICE_NAME_LIMIT,
    DashboardView, DeviceRow, DevicesView, FormError, IssuedCodeView, SessionDetailView,
    SessionsView, SettingsGroup, SettingsView, SetupView, activity_page, backups_page, chat_page,
    dashboard_page, devices_page, error_page, login_page, session_page, sessions_page,
    settings_page, setup_page, type_badge,
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

/// The ceiling on one chat send: every attachment the turn may carry, plus the
/// multipart framing and the text fields around them.
const CHAT_BODY_BYTES: usize = chat::MAX_ATTACHMENT_TOTAL_BYTES + 8 * 1024 * 1024;

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
        .route(ROUTE_LOGOUT, post(logout))
        .route(ROUTE_DASHBOARD, get(dashboard))
        .route(ROUTE_SETTINGS, get(settings))
        .route(ROUTE_SECURITY, get(security).post(save_credential))
        .route(ROUTE_SETTINGS_DEVICE, post(save_device_listen))
        .route(ROUTE_SETTINGS_PROVIDER, post(save_provider))
        .route(ROUTE_SETTINGS_WORKSPACE, post(save_workspace))
        .route(ROUTE_SETTINGS_PERMISSIONS, post(save_permissions))
        .route(ROUTE_SETTINGS_PERSONA, post(save_persona))
        .route(ROUTE_SETTINGS_REASONING, post(save_reasoning))
        .route(ROUTE_SETTINGS_STREAMING, post(save_streaming))
        .route(ROUTE_SETTINGS_QUOTA, post(save_quota))
        .route(ROUTE_SETTINGS_MEMORY, post(save_memory))
        .route(ROUTE_SETTINGS_TIMEZONE, post(save_timezone))
        .route(ROUTE_SIDEBAR, post(toggle_sidebar))
        .route(ROUTE_SETTINGS_TOOLS, post(save_tools))
        .route(ROUTE_SETTINGS_PROVIDERS, post(save_providers))
        .route(ROUTE_SETTINGS_AUX, post(save_aux))
        .route(ROUTE_SETTINGS_MCP, post(save_mcp))
        .route(ROUTE_SETTINGS_GREETING, post(save_greeting))
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
        // Attachments are capped at 64 MiB across a message and 25 MiB each.
        // Axum's 2 MiB default would refuse the operator's first photo before
        // the route's own, better-worded checks ever ran.
        .route(
            ROUTE_CHAT_SEND,
            post(chat_send).layer(DefaultBodyLimit::max(CHAT_BODY_BYTES)),
        )
        .route(ROUTE_CHAT_CLEAR, post(chat_clear))
        .route(ROUTE_CHAT_MODE, post(chat_mode))
        .route(ROUTE_CHAT_NEW, post(chat_new))
        .route(ROUTE_CHAT_SESSIONS, get(chat_sessions))
        .route(ROUTE_CHAT_FILE, get(chat_file))
        .route(ROUTE_CHAT_WORKSPACE, get(chat_workspace_file))
        .route(ROUTE_CHAT_ID, get(chat_conversation_handler))
        .route(ROUTE_CHAT_DELETE, post(chat_delete))
        .route(ROUTE_PROJECTS, post(save_project))
        .route(ROUTE_PROJECTS_ASSIGN, post(save_project_assignment))
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

/// Whether the composer's script asked for the event stream.
///
/// One question with two readers: a turn streams its answer over it, and a
/// command uses it to answer in a frame the page can act on rather than in a
/// redirect a `fetch` would swallow.
fn wants_stream(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"))
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
    // Facts this page was missing while the rest of the list grew: what is
    // waiting on the operator, what is broken, and when the last answer landed.
    view.approvals_waiting = crate::approvals::pending(db)
        .map(|rows| rows.len())
        .unwrap_or(0);
    view.mcp_servers = state.mcp.names().len();
    view.mcp_errors = state.mcp.errors().len();
    view.memory_backend = crate::memory::backend(
        state
            .engine
            .setting(crate::memory::SETTING_BACKEND)
            .ok()
            .flatten()
            .as_deref(),
    )
    .as_str()
    .to_owned();
    view.last_turn = db
        .with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT created_at, coalesce(model, 'unknown model'), state
                   FROM chat_messages WHERE role = 'assistant'
                  ORDER BY created_at DESC LIMIT 1",
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
        .ok()
        .and_then(|rows| rows.into_iter().next())
        .map(|(at, model, state)| format!("{at} - {model} - {state}"));
    view.db_size = std::fs::metadata(engine.paths().db_file.as_path())
        .map(|meta| {
            let bytes = meta.len();
            if bytes > 1_048_576 {
                format!("{:.1} MiB", bytes as f64 / 1_048_576.0)
            } else {
                format!("{:.0} KiB", bytes as f64 / 1024.0)
            }
        })
        .unwrap_or_else(|_| "unknown".to_owned());
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
    view.timezone = crate::zone::Zone::parse(
        engine
            .setting(crate::zone::SETTING_TIMEZONE)
            .map_err(|error| form_error(&error))?
            .as_deref(),
    )
    .as_str();
    let registry = chat::provider_registry(engine).unwrap_or_default();
    view.provider_ids = registry.iter().map(|p| p.id.clone()).collect();
    view.providers_json = crate::providers::to_json(&registry);
    view.providers_primary = engine
        .setting(crate::providers::SETTING_PRIMARY)
        .map_err(|error| form_error(&error))?
        .unwrap_or_else(|| crate::providers::DEFAULT_ID.to_owned());
    view.tools_enabled = crate::tools::enabled(
        engine
            .setting(crate::tools::SETTING_TOOLS_ENABLED)
            .map_err(|error| form_error(&error))?
            .as_deref(),
    );
    view.tool_rounds = engine
        .setting(crate::tools::SETTING_TOOL_ROUNDS)
        .map_err(|error| form_error(&error))?
        .map(|stored| crate::tools::max_rounds(Some(&stored)))
        .unwrap_or_else(|| crate::tools::DEFAULT_TOOL_ROUNDS)
        .to_string();
    view.agents_enabled = crate::subagents::enabled(
        engine
            .setting(crate::subagents::SETTING_AGENTS_ENABLED)
            .map_err(|error| form_error(&error))?
            .as_deref(),
    );
    view.greeting_enabled = crate::greeting::enabled(
        engine
            .setting(crate::greeting::SETTING_GREETING_ENABLED)
            .map_err(|error| form_error(&error))?
            .as_deref(),
    );
    view.greeting_model = engine
        .setting(crate::greeting::SETTING_GREETING_MODEL)
        .map_err(|error| form_error(&error))?
        .map(|stored| crate::greeting::store_model(&stored))
        .unwrap_or_else(|| crate::greeting::INHERIT.to_owned());
    view.chat_hint = crate::greeting::hint(
        state
            .engine
            .setting(crate::greeting::SETTING_HINT)
            .ok()
            .flatten()
            .as_deref(),
    );
    view.greeting_refresh = crate::greeting::Refresh::parse(
        engine
            .setting(crate::greeting::SETTING_GREETING_REFRESH)
            .map_err(|error| form_error(&error))?
            .as_deref(),
    )
    .as_str()
    .to_owned();
    view.models = engine.provider_models().unwrap_or_default();
    view.aux_roles = crate::aux::ROLES
        .iter()
        .map(|role| {
            let stored = engine
                .setting(&crate::aux::setting_key(role.key))
                .ok()
                .flatten();
            crate::web::templates::AuxRow {
                key: role.key.to_owned(),
                label: role.label.to_owned(),
                purpose: role.purpose.to_owned(),
                selected: stored
                    .as_deref()
                    .map(crate::aux::store_value)
                    .unwrap_or_else(|| crate::aux::INHERIT.to_owned()),
                provider_selected: engine
                    .setting(&crate::aux::provider_key(role.key))
                    .ok()
                    .flatten()
                    .map(|id| crate::aux::store_value(&id))
                    .unwrap_or_else(|| crate::aux::INHERIT.to_owned()),
                consumed_by: role.consumed_by.to_owned(),
            }
        })
        .collect();
    view.mcp_errors = state.mcp.errors();
    view.mcp_servers = engine
        .setting(crate::mcp::SETTING_SERVERS)
        .map_err(|error| form_error(&error))?
        .and_then(|stored| crate::mcp::parse(&stored).ok())
        .map(|servers| crate::mcp::to_json(&servers))
        .unwrap_or_else(|| "[]".to_owned());
    view.device_listen = state
        .engine
        .setting(crate::device::SETTING_LISTEN)
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .map(|list| list.join(", "))
        .unwrap_or_else(|| crate::device::DEVICE_ADDR.to_owned());
    view.device_relay = state
        .engine
        .setting(crate::device::SETTING_RELAY)
        .ok()
        .flatten()
        .unwrap_or_default();
    view.memory_backend = crate::memory::backend(
        state
            .engine
            .setting(crate::memory::SETTING_BACKEND)
            .ok()
            .flatten()
            .as_deref(),
    )
    .as_str()
    .to_owned();
    view.memory_vault = state
        .engine
        .setting(crate::memory::SETTING_VAULT)
        .ok()
        .flatten()
        .unwrap_or_default();
    view.tool_log_mode = {
        let stored = state
            .engine
            .setting(chat::SETTING_SHOW_TOOL_LOG)
            .ok()
            .flatten();
        chat::tool_log(stored.as_deref()).as_str().to_owned()
    };
    view.show_tool_log = chat::show_tool_log(
        state
            .engine
            .setting(chat::SETTING_SHOW_TOOL_LOG)
            .ok()
            .flatten()
            .as_deref(),
    );
    view.show_reasoning = chat::show_reasoning(
        engine
            .setting(chat::SETTING_SHOW_REASONING)
            .map_err(|error| form_error(&error))?
            .as_deref(),
    );
    view.streaming = chat::streaming_enabled(
        engine
            .setting(chat::SETTING_STREAMING)
            .map_err(|error| form_error(&error))?
            .as_deref(),
    );
    view.show_quota = crate::web::quota::show_quota(
        engine
            .setting(crate::web::quota::SETTING_SHOW_QUOTA)
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
       ) AS paired,
       EXISTS (
         SELECT 1 FROM auth_tokens a
         WHERE a.device_id = d.id AND a.revoked_at IS NULL AND a.rotated_at IS NULL
           AND instr(' ' || a.scope || ' ', ' admin ') > 0
       ) AS admin
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
                    admin: row.get(6)?,
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
    // The box is read here and nowhere else, and it is stored on the code rather
    // than on the device: the decision belongs to the moment of pairing, and
    // redemption has nobody left to ask.
    let allow_admin = matches!(field(&fields, "allow_admin").trim(), "on" | "true" | "1");
    let pairing = PairingService::new(db.clone());
    let issued = match pairing.issue_for_device_asking(&device_id, allow_admin) {
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

    // The audit row names the device and never the code. An ordinary mint keeps
    // the bare name it always had; a mint that asked for administration says so,
    // because the audit trail is a surface that shows this too.
    let audited = if allow_admin {
        format!("{name}; admin=true")
    } else {
        name.clone()
    };
    if let Err(error) = store.audit("loopback", "device.pairing_code_issued", Some(&audited)) {
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
                // The address the phone should call. A QR carrying 127.0.0.1 is
                // a code that cannot work, so the stored relay or a non-loopback
                // bind wins over the loopback default.
                let relay = pairing_relay(&state);
                let pairing = crate::web::qr::Pairing::new(issued.code(), &relay);
                let qr = crate::web::qr::svg(&pairing.payload(), 6).unwrap_or_default();
                view.issued = Some(IssuedCodeView {
                    code: issued.code().to_owned(),
                    expires_at,
                    relay,
                    qr,
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
            "admin": allow_admin,
            "code": issued.code(),
            "relay": pairing_relay(&state),
            "expires_at": expires_at,
        }))
    }
}

/// `GET /dashboard/chat/workspace/{*path}` - one workspace file, read for a page.
///
/// A turn writes pictures into `generated/` and other files wherever it likes
/// under the workspace, and the transcript has to be able to show them. The
/// path is resolved with the same three-layer confinement the tools use, so a
/// link in an answer cannot reach out of the workspace; the bytes decide the
/// media type, so a `.png` full of HTML is downloaded rather than rendered.
async fn chat_workspace_file(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(path): Path<String>,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    let workspace = match state.engine.workspace_path() {
        Ok(workspace) => workspace,
        Err(error) => {
            return refusal(FormError::new(
                None,
                "workspace_unreadable",
                error.to_string(),
            ));
        }
    };
    let Ok(root) = std::fs::canonicalize(&workspace) else {
        return ApiError::not_found(&request_id.0, "no workspace directory exists").into_response();
    };
    let Ok(resolved) = crate::tools::resolve(&workspace, &path) else {
        return ApiError::not_found(&request_id.0, "that path is not inside the workspace")
            .into_response();
    };
    let Ok(real) = std::fs::canonicalize(&resolved) else {
        return ApiError::not_found(&request_id.0, "no file exists at that path").into_response();
    };
    if !real.starts_with(&root) {
        return ApiError::not_found(&request_id.0, "that file is not inside the workspace")
            .into_response();
    }
    let Ok(metadata) = std::fs::metadata(&real) else {
        return ApiError::not_found(&request_id.0, "that file cannot be read").into_response();
    };
    if !metadata.is_file() {
        return ApiError::not_found(&request_id.0, "that path is not a file").into_response();
    }
    if metadata.len() > chat::MAX_ATTACHMENT_BYTES as u64 {
        return ApiError::not_found(
            &request_id.0,
            format!(
                "that file is larger than the {} bytes this route serves",
                chat::MAX_ATTACHMENT_BYTES
            ),
        )
        .into_response();
    }
    let Ok(bytes) = std::fs::read(&real) else {
        return ApiError::not_found(&request_id.0, "that file cannot be read").into_response();
    };
    let media_type = chat::sniff_media_type(&bytes);
    let disposition = if media_type.starts_with("image/") {
        "inline"
    } else {
        "attachment"
    };
    let name: String = real
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_owned())
        .chars()
        .map(|character| {
            if character == '"' || character.is_control() {
                '_'
            } else {
                character
            }
        })
        .collect();
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, media_type.to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("{disposition}; filename=\"{name}\""),
            ),
            (header::CACHE_CONTROL, "private, max-age=300".to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        ],
        bytes,
    )
        .into_response()
}

/// The origin a phone should use for this daemon.
///
/// The operator's stored relay wins, because only they know which address a
/// phone can route to. Without one, the first non-loopback bind is the honest
/// answer; then `admin.public_url`, which is what a reverse proxy publishes; and
/// only then loopback, which at least states the truth rather than inventing a
/// hostname.
fn pairing_relay(state: &AppState) -> String {
    if let Some(raw) = state
        .engine
        .setting(crate::device::SETTING_RELAY)
        .ok()
        .flatten()
        .and_then(|raw| crate::device::validate_relay(&raw).ok())
    {
        return raw;
    }
    let bind = state.device.bind();
    if let Some(address) = bind.iter().find(|address| !address.ip().is_loopback()) {
        return format!("http://{address}");
    }
    if let Some(origin) = state.access.public_url.as_deref().and_then(url_origin) {
        return origin;
    }
    let address = bind.first().copied().unwrap_or_else(|| {
        crate::device::DEVICE_ADDR
            .parse()
            .expect("a literal ip:port")
    });
    format!("http://{address}")
}

/// The scheme and authority of an absolute URL, with no path or query.
fn url_origin(raw: &str) -> Option<String> {
    let (scheme, rest) = raw.trim().split_once("://")?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    if authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{authority}"))
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
        // Already signed in: straight to the conversation, which is what this
        // daemon is for. The dashboard is a place to check on it, not to start.
        return see_other(ROUTE_CHAT);
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

    // "Keep me signed in" mints the thirty-day session; the default stays the
    // short one. Both go through the same mint, only the lifetime differs.
    let remember = matches!(field(&fields, "remember").trim(), "on" | "true" | "1");
    let minted = match if remember {
        state.sessions.mint_persistent()
    } else {
        state.sessions.mint()
    } {
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
        Some(if remember {
            "login direct (remember this device)"
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
    let max_age_secs = if remember {
        session::PERSISTENT_TTL.as_secs()
    } else {
        session::SESSION_TTL.as_secs()
    };
    let cookie = session::set_cookie_header(
        &minted.cookie_value,
        state.access.cookie_secure(&context),
        max_age_secs,
    );
    // Signing in lands on the conversation. A control panel as the landing page
    // made chat reachable only through a menu, which is a dead end for the one
    // thing an operator came to do.
    let mut response = see_other(ROUTE_CHAT);
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
}

/// `POST /logout` - clear the session cookie and return to the sign-in form.
///
/// The only route that revokes the operator's own session. It needs no session
/// to run: clearing an already-expired cookie is harmless, and the point is to
/// land back on the form where the passphrase and the "keep me signed in" box
/// live.
async fn logout(
    State(state): State<AppState>,
    context: Option<Extension<ClientContext>>,
    headers: HeaderMap,
) -> Response {
    let context = context_of(context);
    // Revoke the server row, not just the cookie: a signed-out browser must stop
    // working server-side even if the cookie value leaked somewhere.
    if let Some(cookie) = session::cookie_from_headers(&headers) {
        let _ = state.sessions.revoke(&cookie);
    }
    let _ = store::audit_write(
        state.db(),
        &context.actor(),
        "admin.signed_out",
        Some("logout"),
    );
    let mut response = see_other(ROUTE_LOGIN);
    if let Ok(value) = HeaderValue::from_str(&session::clear_cookie_header()) {
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
    let cookie = session::set_cookie_header(
        &minted.cookie_value,
        state.access.cookie_secure(&context),
        session::SESSION_TTL.as_secs(),
    );
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

    // The address the phone should call, drawn into the same QR the devices page
    // draws. The wizard is where a phone is most often paired, so a code with no
    // camera-readable form here is a code that sends the operator hunting.
    let relay = pairing_relay(&state);
    let pairing = crate::web::qr::Pairing::new(&code, &relay);
    let qr = crate::web::qr::svg(&pairing.payload(), 6).unwrap_or_default();

    if as_html {
        // The code is shown once, on the page that just minted it.
        match build_setup_view(&state) {
            Ok(mut view) => {
                view.pairing_code = Some(code);
                view.pairing_expires = Some(expires_at);
                view.pairing_relay = Some(relay);
                view.pairing_qr = Some(qr);
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
            "relay": relay,
            "qr_svg": qr,
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

/// Where the reasoning toggle returns to.
const SAVED_REASONING: &str = "/dashboard/settings?saved=reasoning";
/// Where the streaming toggle returns to.
const SAVED_STREAMING: &str = "/dashboard/settings?saved=streaming";
/// Where the quota toggle returns to.
const SAVED_QUOTA: &str = "/dashboard/settings?saved=quota";

/// Where the provider registry returns to.
const SAVED_PROVIDERS: &str = "/dashboard/settings?saved=providers";

/// Where the tools settings return to.
const SAVED_TOOLS: &str = "/dashboard/settings?saved=tools";

/// Where the auxiliary roles return to.
const SAVED_AUX: &str = "/dashboard/settings?saved=aux";

/// Where the MCP list returns to.
const SAVED_MCP: &str = "/dashboard/settings?saved=mcp";

/// Where the clock setting returns to.
const SAVED_TIMEZONE: &str = "/dashboard/settings?saved=timezone";

/// Where the memory form returns to.
const SAVED_MEMORY: &str = "/dashboard/settings?saved=memory";

/// The shortest sign-in credential this build accepts.
///
/// The six-word passphrase exists because a fresh host has to be claimable by
/// something long and unguessable. Once it is yours, a shorter credential you will
/// actually type is the better trade - provided it is not four digits, and provided
/// the current credential has to be presented to change it.
pub const MIN_SIGN_IN_CHARS: usize = 8;

/// `GET /dashboard/security` - where a secret may be typed.
///
/// This is the only page allowed to render a password input. The settings page is
/// tested to contain none, because nothing stored there is a secret value.
async fn security(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    match build_settings_view(&state) {
        Ok(view) => page(crate::web::templates::security_page(&view)),
        Err(error) => ApiError::internal(request_id.0, error.message).into_response(),
    }
}

/// A refusal on the security page renders that page, not the settings one.
fn fail_security(state: &AppState, error: FormError) -> Response {
    match build_settings_view(state) {
        Ok(mut view) => {
            view.error = Some(error);
            page(crate::web::templates::security_page(&view))
        }
        Err(inner) => ApiError::internal("security", inner.message).into_response(),
    }
}

/// `POST /dashboard/security` - rotate the sign-in credential.
///
/// The current credential is required even though the request already carries an
/// admin session: a borrowed browser should not be able to lock the owner out.
async fn save_credential(
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

    let current = field(&fields, "current_password");
    let new_password = field(&fields, "new_password");
    let confirm = field(&fields, "confirm_password");

    if state.engine.auth().verify_admin_password(current).is_err() {
        return fail_security(
            &state,
            FormError::new(
                Some("current_password"),
                "credential_rejected",
                "that is not the current credential, so nothing changed".to_owned(),
            ),
        );
    }
    if new_password != confirm {
        return fail_security(
            &state,
            FormError::new(
                Some("confirm_password"),
                "credential_mismatch",
                "the two entries do not match".to_owned(),
            ),
        );
    }
    if new_password.trim().chars().count() < MIN_SIGN_IN_CHARS {
        return fail_security(
            &state,
            FormError::new(
                Some("new_password"),
                "credential_weak",
                format!("use at least {MIN_SIGN_IN_CHARS} characters"),
            ),
        );
    }

    if let Err(error) = state.engine.auth().set_admin_password(new_password) {
        return fail_security(
            &state,
            FormError::new(
                Some("new_password"),
                "credential_store_failed",
                format!("the credential could not be stored: {error}"),
            ),
        );
    }

    // Sessions minted under the old credential do not survive the rotation: a
    // change that leaves old cookies working is not a change.
    let dropped = state
        .engine
        .db()
        .with_conn(|conn| conn.execute("DELETE FROM admin_sessions", []))
        .unwrap_or(0);
    let _ = state.engine.auth().audit(
        "loopback",
        "settings.credential",
        Some(&format!("rotated; sessions_cleared={dropped}")),
    );

    see_other(SAVED_LOGIN)
}

/// Where a successful rotation sends the operator: back to sign in.
const SAVED_LOGIN: &str = "/login?saved=credential";

/// `POST /dashboard/settings/device` - where the device listener binds.
///
/// A bind cannot be moved under a live phone, so the change is stored and applied
/// at the next start. The page says so rather than pretending it is instant.
async fn save_device_listen(
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

    // The relay is read first: a bad one is refused before anything is stored,
    // so the form cannot half-save a bind and a URL that contradicts it. A body
    // with no relay field at all is an older page that only knew about binds, so
    // it leaves the stored origin alone; an empty field is a deliberate clear.
    let relay = match fields.get("device_relay") {
        None => state
            .engine
            .setting(crate::device::SETTING_RELAY)
            .ok()
            .flatten()
            .and_then(|raw| crate::device::validate_relay(&raw).ok())
            .unwrap_or_default(),
        Some(raw) => {
            let raw = raw.trim();
            if raw.is_empty() {
                String::new()
            } else {
                match crate::device::validate_relay(raw) {
                    Ok(relay) => relay,
                    Err(message) => {
                        return fail_settings(
                            &state,
                            as_html,
                            FormError::new(Some("device_relay"), "device_relay_invalid", message),
                        );
                    }
                }
            }
        }
    };

    let raw = field(&fields, "device_listen");
    let list: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect();
    let addresses = match crate::admin::listen::parse_listen_list(&list) {
        Ok(addresses) => addresses,
        Err(error) => {
            return fail_settings(
                &state,
                as_html,
                FormError::new(
                    Some("device_listen"),
                    "device_listen_invalid",
                    error.to_string(),
                ),
            );
        }
    };
    let encoded = serde_json::to_string(
        &addresses
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_owned());
    if let Err(error) =
        store::settings_put(state.engine.db(), crate::device::SETTING_LISTEN, &encoded)
    {
        return store_failure(&request_id.0, crate::device::SETTING_LISTEN, error);
    }
    if let Err(error) = store::settings_put(state.engine.db(), crate::device::SETTING_RELAY, &relay)
    {
        return store_failure(&request_id.0, crate::device::SETTING_RELAY, error);
    }
    // The relay is not a secret: it is the address printed in a QR. The bind is
    // recorded too, because "which addresses does this daemon answer on" is the
    // question the next session starts with.
    let _ = state.engine.auth().audit(
        "loopback",
        "settings.device_listen",
        Some(&format!("binds={encoded}; relay={relay}")),
    );
    see_other(SAVED_DEVICE)
}

/// Where the device-bind form returns to.
const SAVED_DEVICE: &str = "/dashboard/settings?saved=device";
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
            "greeting" => Some(SettingsGroup::Greeting),
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

/// One failure on the chat surface: the page back, with the message in the live
/// region the page already has.
///
/// The rail's project forms live on the chat page, so that is the page that has
/// to report a refused name or a refused assignment. `requested` is the
/// conversation the caller was looking at, when it named one, so a refusal does
/// not silently move the operator to a different transcript.
fn fail_chat(
    state: &AppState,
    as_html: bool,
    requested: Option<&str>,
    error: FormError,
) -> Response {
    let status = failure_status(&error.code);
    if as_html {
        match build_chat_view(state, requested) {
            Ok(mut view) => {
                view.error = Some(error);
                page_with_status(chat_page(&view), status)
            }
            Err(build_error) => refusal(build_error),
        }
    } else {
        refusal(error)
    }
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

/// `POST /dashboard/settings/reasoning` - the thinking bubble on or off.
///
/// Purely presentational. It decides whether the model's reasoning is rendered
/// and whether it is streamed to the browser at all; it never changes what a turn
/// is allowed to do.
async fn save_reasoning(
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
    // An unchecked box sends nothing at all, so absence is the off signal here.
    let enabled = matches!(field(&fields, "show_reasoning").trim(), "on" | "true" | "1");
    let stored = if enabled { "true" } else { "false" };
    if let Err(error) = store::settings_put(state.engine.db(), chat::SETTING_SHOW_REASONING, stored)
    {
        return store_failure(&request_id.0, chat::SETTING_SHOW_REASONING, error);
    }
    if let Err(error) = state
        .engine
        .auth()
        .audit("loopback", "settings.reasoning", Some(stored))
    {
        return audit_failure(&request_id.0, "settings.reasoning", error);
    }

    see_other(SAVED_REASONING)
}

/// `POST /dashboard/settings/streaming` - stream answers or render one page.
///
/// The switch the composer reads. Off means the browser hands the turn to the
/// plain form and the finished transcript comes back in one response, which is
/// the same path a browser with no script uses; it changes nothing about the
/// turn itself.
async fn save_streaming(
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
    // An unchecked box sends nothing at all, so absence is the off signal here.
    let enabled = matches!(
        field(&fields, "stream_responses").trim(),
        "on" | "true" | "1"
    );
    let stored = if enabled { "true" } else { "false" };
    if let Err(error) = store::settings_put(state.engine.db(), chat::SETTING_STREAMING, stored) {
        return store_failure(&request_id.0, chat::SETTING_STREAMING, error);
    }
    if let Err(error) = state
        .engine
        .auth()
        .audit("loopback", "settings.streaming", Some(stored))
    {
        return audit_failure(&request_id.0, "settings.streaming", error);
    }

    see_other(SAVED_STREAMING)
}

/// `POST /dashboard/settings/quota` - name the provider's quota under answers.
async fn save_quota(
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
    // An unchecked box sends nothing at all, so absence is the off signal here.
    let enabled = matches!(field(&fields, "show_quota").trim(), "on" | "true" | "1");
    let stored = if enabled { "true" } else { "false" };
    let key = crate::web::quota::SETTING_SHOW_QUOTA;
    if let Err(error) = store::settings_put(state.engine.db(), key, stored) {
        return store_failure(&request_id.0, key, error);
    }
    if let Err(error) = state
        .engine
        .auth()
        .audit("loopback", "settings.quota", Some(stored))
    {
        return audit_failure(&request_id.0, "settings.quota", error);
    }
    see_other(SAVED_QUOTA)
}

/// `POST /dashboard/settings/timezone` - the clock every stamp is shown in.
///
/// A typo is refused next to the field rather than silently replaced with the
/// host zone, because a clock that quietly ignores what was typed is worse than
/// one that says it did not understand.
/// `POST /dashboard/settings/memory` - which brain a turn recalls from.
///
/// The vault directory is only meaningful for the vault backend, but it is stored
/// whichever is chosen: an operator switching back should not have to re-type a
/// path they already gave once.
async fn save_memory(
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

    let backend = match field(&fields, "memory_backend").trim() {
        "vault" => "vault",
        "off" => "off",
        _ => "gbrain",
    };
    let vault = field(&fields, "memory_vault").trim().to_owned();
    // A path that does not exist is refused here rather than at recall time: a
    // backend that silently returns nothing looks exactly like a brain that is
    // empty.
    if backend == "vault" {
        if vault.is_empty() {
            return fail_settings(
                &state,
                as_html,
                FormError::new(
                    Some("memory_vault"),
                    "memory_vault_missing",
                    "the vault backend needs a directory of markdown".to_owned(),
                ),
            );
        }
        if !std::path::Path::new(&vault).is_dir() {
            return fail_settings(
                &state,
                as_html,
                FormError::new(
                    Some("memory_vault"),
                    "memory_vault_missing",
                    format!("`{vault}` is not a directory on this host"),
                ),
            );
        }
    }

    if let Err(error) =
        store::settings_put(state.engine.db(), crate::memory::SETTING_BACKEND, backend)
    {
        return store_failure(&request_id.0, crate::memory::SETTING_BACKEND, error);
    }
    if let Err(error) = store::settings_put(state.engine.db(), crate::memory::SETTING_VAULT, &vault)
    {
        return store_failure(&request_id.0, crate::memory::SETTING_VAULT, error);
    }
    if let Err(error) = state.engine.auth().audit(
        "loopback",
        "settings.memory",
        Some(&format!("backend={backend}; vault={vault}")),
    ) {
        return audit_failure(&request_id.0, "settings.memory", error);
    }

    see_other(SAVED_MEMORY)
}

async fn save_timezone(
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
    let zone = match crate::zone::validate(field(&fields, "timezone")) {
        Ok(zone) => zone,
        Err(message) => {
            return fail_settings(
                &state,
                as_html,
                FormError::new(Some("timezone"), "timezone_invalid", message),
            );
        }
    };
    let stored = zone.as_str();
    if let Err(error) =
        store::settings_put(state.engine.db(), crate::zone::SETTING_TIMEZONE, &stored)
    {
        return store_failure(&request_id.0, crate::zone::SETTING_TIMEZONE, error);
    }
    if let Err(error) = state
        .engine
        .auth()
        .audit("loopback", "settings.timezone", Some(&stored))
    {
        return audit_failure(&request_id.0, "settings.timezone", error);
    }

    see_other(SAVED_TIMEZONE)
}

/// `POST /dashboard/sidebar` - show or hide the conversation rail.
///
/// Server-rendered and stored, so the choice survives a reload and applies to
/// every page rather than to one tab's memory.
async fn toggle_sidebar(
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
    let stored = if matches!(field(&fields, "rail"), "hidden") {
        "true"
    } else {
        "false"
    };
    if let Err(error) =
        store::settings_put(state.engine.db(), crate::admin::SETTING_RAIL_HIDDEN, stored)
    {
        return store_failure(&request_id.0, crate::admin::SETTING_RAIL_HIDDEN, error);
    }
    if let Err(error) = state
        .engine
        .auth()
        .audit("loopback", "sidebar.toggle", Some(stored))
    {
        return audit_failure(&request_id.0, "sidebar.toggle", error);
    }

    // Back where the button was. Only a local path is honoured, so a crafted
    // Referer cannot turn this into a redirect anywhere else.
    let back = headers
        .get(header::REFERER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once("://").map(|(_scheme, rest)| rest))
        .and_then(|rest| rest.find('/').map(|at| rest[at..].to_owned()))
        .filter(|path| path.starts_with('/') && !path.starts_with("//"))
        .unwrap_or_else(|| crate::admin::ROUTE_CHAT.to_owned());
    see_other_to(back)
}

/// `POST /dashboard/settings/tools` - tool calling and its round budget.
async fn save_tools(
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

    let enabled = flag(&fields, "tools_enabled");
    let mode = match field(&fields, "tool_log").trim() {
        "off" => "off",
        "summary" => "summary",
        _ => "full",
    };
    let raw = field(&fields, "tool_rounds").trim().to_owned();
    let rounds: u32 = match raw.parse::<u32>() {
        Ok(rounds) if (1..=crate::tools::MAX_TOOL_ROUNDS_CEILING).contains(&rounds) => rounds,
        _ => {
            return fail_settings(
                &state,
                as_html,
                FormError::new(
                    Some("tool_rounds"),
                    "tool_rounds_invalid",
                    format!(
                        "tool rounds must be a whole number from 1 to {}",
                        crate::tools::MAX_TOOL_ROUNDS_CEILING
                    ),
                ),
            );
        }
    };

    if let Err(error) = store::settings_put(
        state.engine.db(),
        crate::tools::SETTING_TOOLS_ENABLED,
        if enabled { "true" } else { "false" },
    ) {
        return store_failure(&request_id.0, crate::tools::SETTING_TOOLS_ENABLED, error);
    }
    if let Err(error) = store::settings_put(
        state.engine.db(),
        crate::tools::SETTING_TOOL_ROUNDS,
        &rounds.to_string(),
    ) {
        return store_failure(&request_id.0, crate::tools::SETTING_TOOL_ROUNDS, error);
    }
    if let Err(error) = store::settings_put(state.engine.db(), chat::SETTING_SHOW_TOOL_LOG, mode) {
        return store_failure(&request_id.0, chat::SETTING_SHOW_TOOL_LOG, error);
    }
    // Dispatching lives in this form because it is a tool switch: it decides what
    // the model is offered, and the page group that owns "what a turn may call" is
    // the only place an operator would look for it.
    let agents = flag(&fields, "agents_enabled");
    if let Err(error) = store::settings_put(
        state.engine.db(),
        crate::subagents::SETTING_AGENTS_ENABLED,
        if agents { "true" } else { "false" },
    ) {
        return store_failure(
            &request_id.0,
            crate::subagents::SETTING_AGENTS_ENABLED,
            error,
        );
    }
    if let Err(error) = state.engine.auth().audit(
        "loopback",
        "settings.tools",
        Some(&format!(
            "enabled={enabled}; max_rounds={rounds}; run_log={mode}; agents={agents}"
        )),
    ) {
        return audit_failure(&request_id.0, "settings.tools", error);
    }

    see_other(SAVED_TOOLS)
}

/// `POST /dashboard/settings/providers` - the registry and its primary.
///
/// An empty registry is only allowed while no model is configured: once a turn
/// can run, it needs somewhere to run, and a save that quietly removed the last
/// endpoint would be a save that stopped the product answering.
async fn save_providers(
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
    let providers = match crate::providers::parse(field(&fields, "providers_registry")) {
        Ok(providers) => providers,
        Err(error) => {
            return fail_settings(
                &state,
                as_html,
                FormError::new(
                    Some("providers_registry"),
                    "providers_invalid",
                    error.message,
                ),
            );
        }
    };
    let model_configured = state
        .engine
        .setting(crate::onboarding::SETTING_MODEL)
        .ok()
        .flatten()
        .is_some_and(|model| !model.trim().is_empty());
    if providers.is_empty() && model_configured {
        return fail_settings(
            &state,
            as_html,
            FormError::new(
                Some("providers_registry"),
                "providers_empty",
                "a model is configured, so at least one provider is needed for a turn to answer",
            ),
        );
    }

    let requested_primary = field(&fields, "providers_primary").trim().to_owned();
    let primary = if providers.iter().any(|p| p.id == requested_primary) {
        requested_primary
    } else {
        providers
            .first()
            .map_or_else(|| crate::providers::DEFAULT_ID.to_owned(), |p| p.id.clone())
    };

    if let Err(error) = store::settings_put(
        state.engine.db(),
        crate::providers::SETTING_REGISTRY,
        &crate::providers::to_json(&providers),
    ) {
        return store_failure(&request_id.0, crate::providers::SETTING_REGISTRY, error);
    }
    if let Err(error) = store::settings_put(
        state.engine.db(),
        crate::providers::SETTING_PRIMARY,
        &primary,
    ) {
        return store_failure(&request_id.0, crate::providers::SETTING_PRIMARY, error);
    }
    if let Err(error) = state.engine.auth().audit(
        "loopback",
        "settings.providers",
        Some(&format!("providers={}; primary={primary}", providers.len())),
    ) {
        return audit_failure(&request_id.0, "settings.providers", error);
    }

    see_other(SAVED_PROVIDERS)
}

/// `POST /dashboard/settings/aux` - one model per auxiliary role.
///
/// A pinned model is checked against the probe when the probe has anything to
/// say: storing an id the gateway does not serve would be a setting that looks
/// configured and fails at the first use.
async fn save_aux(
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
    let known = state.engine.provider_models().unwrap_or_default();
    let providers = chat::provider_registry(&state.engine).unwrap_or_default();

    let mut stored_any = 0usize;
    for role in crate::aux::ROLES {
        let field_name = format!("aux_{}", role.key);
        let value = crate::aux::store_value(field(&fields, &field_name));
        // Only chat roles are checked against the chat-model probe. A role that
        // posts to an image endpoint would be refused for appearing in the wrong
        // catalogue, which is a check of the wrong thing.
        if role.probe_validated
            && value != crate::aux::INHERIT
            && !known.is_empty()
            && !known.iter().any(|m| m == &value)
        {
            return fail_settings(
                &state,
                as_html,
                FormError::new(
                    Some(&field_name),
                    "aux_model_unknown",
                    format!(
                        "`{value}` is not a model the last probe returned, so it would never answer"
                    ),
                ),
            );
        }
        let provider_field = format!("aux_provider_{}", role.key);
        let provider = crate::aux::store_value(field(&fields, &provider_field));
        if provider != crate::aux::INHERIT
            && !providers.iter().any(|candidate| candidate.id == provider)
        {
            return fail_settings(
                &state,
                as_html,
                FormError::new(
                    Some(&provider_field),
                    "aux_provider_unknown",
                    format!(
                        "`{provider}` is not in the provider registry, so the role could not run"
                    ),
                ),
            );
        }
        if let Err(error) = store::settings_put(
            state.engine.db(),
            &crate::aux::setting_key(role.key),
            &value,
        ) {
            return store_failure(&request_id.0, &crate::aux::setting_key(role.key), error);
        }
        if let Err(error) = store::settings_put(
            state.engine.db(),
            &crate::aux::provider_key(role.key),
            &provider,
        ) {
            return store_failure(&request_id.0, &crate::aux::provider_key(role.key), error);
        }
        stored_any += 1;
    }

    if let Err(error) = state.engine.auth().audit(
        "loopback",
        "settings.aux",
        Some(&format!("roles={stored_any}")),
    ) {
        return audit_failure(&request_id.0, "settings.aux", error);
    }

    see_other(SAVED_AUX)
}

/// Where the greeting form returns to.
const SAVED_GREETING: &str = "/dashboard/settings?saved=greeting";

/// `POST /dashboard/settings/greeting` - the generated line on an empty page.
///
/// Three rows, one form. A model the last probe did not return is refused next to
/// the picker, for the same reason the auxiliary roles refuse it: a model that is
/// not in the catalogue would never write anything. A refresh value outside the
/// closed set is stored as the default rather than refused, because the select
/// cannot produce one and a hand-edited row is not worth a failed save.
async fn save_greeting(
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

    // An unchecked box sends nothing at all, so absence is the off signal.
    let enabled = crate::greeting::enabled(Some(field(&fields, "greeting_enabled")));
    let model = crate::greeting::store_model(field(&fields, "greeting_model"));
    let known = state.engine.provider_models().unwrap_or_default();
    if model != crate::greeting::INHERIT
        && !known.is_empty()
        && !known.iter().any(|candidate| candidate == &model)
    {
        return fail_settings(
            &state,
            as_html,
            FormError::new(
                Some("greeting_model"),
                "greeting_model_unknown",
                format!(
                    "`{model}` is not a model the last probe returned, so no line would be written"
                ),
            ),
        );
    }
    let refresh = crate::greeting::Refresh::parse(Some(field(&fields, "greeting_refresh")));
    let stored_enabled = if enabled { "true" } else { "false" };

    let hint = field(&fields, "chat_hint").trim().to_owned();
    if hint.chars().count() > crate::greeting::MAX_HINT_CHARS {
        return fail_settings(
            &state,
            as_html,
            FormError::new(
                Some("chat_hint"),
                "greeting_hint_too_long",
                format!(
                    "keep it under {} characters",
                    crate::greeting::MAX_HINT_CHARS
                ),
            ),
        );
    }
    for (key, value) in [
        (crate::greeting::SETTING_HINT, hint.as_str()),
        (crate::greeting::SETTING_GREETING_ENABLED, stored_enabled),
        (crate::greeting::SETTING_GREETING_MODEL, model.as_str()),
        (crate::greeting::SETTING_GREETING_REFRESH, refresh.as_str()),
    ] {
        if let Err(error) = store::settings_put(state.engine.db(), key, value) {
            return store_failure(&request_id.0, key, error);
        }
    }

    if let Err(error) = state.engine.auth().audit(
        "loopback",
        "settings.greeting",
        Some(&format!(
            "enabled={stored_enabled}; model={model}; refresh={}",
            refresh.as_str()
        )),
    ) {
        return audit_failure(&request_id.0, "settings.greeting", error);
    }

    see_other(SAVED_GREETING)
}

/// `POST /dashboard/settings/mcp` - the configured MCP server list.
async fn save_mcp(
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
    let servers = match crate::mcp::parse(field(&fields, "mcp_servers")) {
        Ok(servers) => servers,
        Err(error) => {
            return fail_settings(
                &state,
                as_html,
                FormError::new(Some("mcp_servers"), "mcp_invalid", error.message),
            );
        }
    };
    let stored = crate::mcp::to_json(&servers);
    if let Err(error) = store::settings_put(state.engine.db(), crate::mcp::SETTING_SERVERS, &stored)
    {
        return store_failure(&request_id.0, crate::mcp::SETTING_SERVERS, error);
    }
    if let Err(error) = state.engine.auth().audit(
        "loopback",
        "settings.mcp",
        Some(&format!("servers={}", servers.len())),
    ) {
        return audit_failure(&request_id.0, "settings.mcp", error);
    }

    see_other(SAVED_MCP)
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
    // One offset for the whole page: every stamp in a transcript has to agree,
    // or a reply can appear to precede its own question.
    let zone = crate::zone::Zone::parse(
        state
            .engine
            .setting(crate::zone::SETTING_TIMEZONE)
            .ok()
            .flatten()
            .as_deref(),
    );
    let offset = crate::zone::offset_minutes(db, zone);
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
    // One query for every file the conversation carries, then each line takes
    // its own: a page with forty turns costs two queries, not forty-one.
    let carried = chat::attachments_by_message(db, &conversation).unwrap_or_default();
    let show_quota = crate::web::quota::show_quota(
        state
            .engine
            .setting(crate::web::quota::SETTING_SHOW_QUOTA)
            .ok()
            .flatten()
            .as_deref(),
    );
    let messages = transcript
        .iter()
        .map(|message| {
            let user = message.role == chat::Role::User;
            let files = carried
                .get(&message.id)
                .map(|rows| {
                    rows.iter()
                        .map(|file| ChatAttachment {
                            name: file.name.clone(),
                            size: file.size,
                            is_image: chat::is_image(file),
                            badge: type_badge(file.media_type.as_deref()),
                            url: Some(format!("/dashboard/chat/file/{}", file.id)),
                        })
                        .collect()
                })
                .unwrap_or_default();
            ChatLine {
                files,
                user,
                who: if user {
                    "You".to_owned()
                } else {
                    assistant_name.clone()
                },
                body: message.body.clone(),
                stamp: crate::zone::clock_in(&message.created_at, offset),
                failed: message.state == chat::State::Failed,
                error_code: message.error_code.clone(),
                pending: message.state == chat::State::Streaming,
                model: message.model.clone(),
                prompt_tokens: message.prompt_tokens,
                completion_tokens: message.completion_tokens,
                cached_tokens: message.cached_tokens,
                duration_ms: message.duration_ms,
                context_window: message.context_window,
                reasoning: message.reasoning.clone(),
                tools: message.tools.clone(),
                rounds: message.rounds,
                quota: show_quota
                    .then(|| message.quota.as_ref().map(crate::web::quota::Quota::line))
                    .flatten(),
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

    // Read live, so the page a steer redirects to says a message is waiting for
    // the turn that is still running in it.
    let queued = state.steering.queued(&conversation);

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
        streaming: chat::streaming_enabled(
            state
                .engine
                .setting(chat::SETTING_STREAMING)
                .ok()
                .flatten()
                .as_deref(),
        ),
        show_reasoning: chat::show_reasoning(
            state
                .engine
                .setting(chat::SETTING_SHOW_REASONING)
                .ok()
                .flatten()
                .as_deref(),
        ),
        show_tool_log: chat::show_tool_log(
            state
                .engine
                .setting(chat::SETTING_SHOW_TOOL_LOG)
                .ok()
                .flatten()
                .as_deref(),
        ),
        tool_log_mode: chat::tool_log(
            state
                .engine
                .setting(chat::SETTING_SHOW_TOOL_LOG)
                .ok()
                .flatten()
                .as_deref(),
        )
        .as_str()
        .to_owned(),
        greeting: crate::web::templates::greeting_for(crate::zone::hour_in(offset)).to_owned(),
        hint: crate::greeting::hint(
            state
                .engine
                .setting(crate::greeting::SETTING_HINT)
                .ok()
                .flatten()
                .as_deref(),
        ),
        // Filled by the handlers, which are the only place a refresh can be
        // awaited. A view built anywhere else renders the salutation alone.
        line: None,
        sandbox_mode: state
            .engine
            .permissions()
            .map(|permissions| permissions.sandbox_mode.as_str().to_owned())
            .unwrap_or_else(|_| crate::onboarding::SandboxMode::DEFAULT.as_str().to_owned()),
        allow_full: state.allow_full(),
        context_window,
        queued,
    })
}

/// How long a page will wait for a new line before serving the stored one.
///
/// A greeting is a decoration, so it is the first thing to drop. The stored line,
/// or the bare salutation, is always an answer.
const GREETING_WAIT: std::time::Duration = std::time::Duration::from_secs(6);

/// The line under the salutation, refreshing it when it is due.
///
/// Three rules, in order: off means nothing is rendered, a line inside its
/// refresh window is served from the database, and only a due one reaches a
/// provider. The provider call blocks, so it is moved onto a blocking thread
/// whenever there is an executor to move it to, and the page falls back to the
/// stored line when the call fails or the deadline passes. A page never waits on
/// a model with no ceiling.
async fn greeting_line(state: &AppState) -> Option<String> {
    let engine = &state.engine;
    let read = |key: &str| engine.setting(key).ok().flatten();
    if !crate::greeting::enabled(read(crate::greeting::SETTING_GREETING_ENABLED).as_deref()) {
        return None;
    }
    let refresh =
        crate::greeting::Refresh::parse(read(crate::greeting::SETTING_GREETING_REFRESH).as_deref());
    let cached = crate::greeting::stored(engine.db());
    let now = crate::auth::now().unix_timestamp();
    if let Some(cached) = &cached
        && !crate::greeting::is_due(cached.made_at, refresh, now)
    {
        return Some(cached.text.clone());
    }
    let Some(config) = greeting_config(state) else {
        return cached.map(|cached| cached.text);
    };
    let db = engine.db().clone();
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        // No executor: this is the synchronous path, and the call runs here.
        return crate::greeting::regenerate(&db, &config, now).or_else(|| cached.map(|c| c.text));
    };
    let task = handle.spawn_blocking(move || crate::greeting::regenerate(&db, &config, now));
    // A cancelled blocking task cannot be stopped, so the timeout is about the
    // page and not about the call: what is dropped is the wait, not the thread.
    match tokio::time::timeout(GREETING_WAIT, task).await {
        Ok(Ok(Some(line))) => Some(line),
        _ => cached.map(|cached| cached.text),
    }
}

/// Where a line is written: the greeting model, or the chat model.
fn greeting_config(state: &AppState) -> Option<chat::TurnConfig> {
    let primary = chat::turn_config(&state.engine).ok().flatten()?;
    let stored = state
        .engine
        .setting(crate::greeting::SETTING_GREETING_MODEL)
        .ok()
        .flatten();
    Some(chat::TurnConfig {
        model: crate::greeting::model_for(stored.as_deref(), &primary.model),
        ..primary
    })
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
        Ok(mut view) => {
            if view.messages.is_empty() {
                view.line = greeting_line(&state).await;
            }
            page(chat_page(&view))
        }
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
        Ok(mut view) => {
            if view.messages.is_empty() {
                view.line = greeting_line(&state).await;
            }
            page(chat_page(&view))
        }
        Err(error) => refusal(error),
    }
}

/// `GET /dashboard/chat/file/{id}` - one file that came with a message.
///
/// Behind the same admin session as the transcript it appears in, because an
/// uploaded screenshot is the operator's own material. Four rules, in this order:
///
/// 1. The row comes from the attachment table. An id that is not in it is a 404,
///    so the route cannot be used to read a file by guessing its name.
/// 2. The path is resolved with symlinks followed and must still land inside the
///    attachment directory, so a row that was tampered with, or a link planted
///    in the uploads tree, is a refusal rather than a read of `/etc/passwd`.
/// 3. The read is bounded by the ceiling the upload route enforces. The bytes
///    were bounded on the way in; a row is not a promise about what is on disk
///    now, so the read is bounded again on the way out.
/// 4. The type is sniffed from the bytes. Neither the filename nor the stored
///    `media_type` column decides it: a browser renders `text/html` in this
///    origin, and an upload named `.png` is not evidence of a PNG.
async fn chat_file(
    State(state): State<AppState>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if !session_valid(&state, &headers) {
        return no_dashboard_session(&headers, &request_id.0);
    }
    let row = match chat::attachment(state.engine.db(), &id) {
        Ok(Some(row)) => row,
        Ok(None) => {
            return ApiError::not_found(&request_id.0, "no attachment with that id is recorded")
                .into_response();
        }
        Err(error) => {
            return refusal(FormError::new(None, "chat_unreadable", error.to_string()));
        }
    };
    // The directory the upload route writes into, resolved once. Every attachment
    // this daemon accepted is under it, so anything else is not one.
    let root = match state.engine.workspace_path() {
        Ok(workspace) => workspace.join("uploads"),
        Err(error) => {
            return refusal(FormError::new(
                None,
                "workspace_unreadable",
                error.to_string(),
            ));
        }
    };
    let Ok(root) = std::fs::canonicalize(root) else {
        return ApiError::not_found(&request_id.0, "no attachment directory exists")
            .into_response();
    };
    let Ok(real) = std::fs::canonicalize(&row.path) else {
        // A row whose bytes have been cleaned off disk is not a 500: the honest
        // answer to a direct request is that the file is gone.
        return ApiError::not_found(
            &request_id.0,
            "that attachment is recorded but its bytes are not on disk",
        )
        .into_response();
    };
    if !real.starts_with(&root) {
        return ApiError::not_found(
            &request_id.0,
            "that attachment is not inside the attachment directory",
        )
        .into_response();
    }
    let Ok(file) = std::fs::File::open(&real) else {
        return ApiError::not_found(
            &request_id.0,
            "that attachment is recorded but cannot be opened",
        )
        .into_response();
    };
    let mut bytes: Vec<u8> = Vec::new();
    let budget = (chat::MAX_ATTACHMENT_BYTES as u64) + 1;
    if file.take(budget).read_to_end(&mut bytes).is_err() {
        return ApiError::not_found(
            &request_id.0,
            "that attachment is recorded but cannot be read",
        )
        .into_response();
    }
    if bytes.len() > chat::MAX_ATTACHMENT_BYTES {
        return ApiError::not_found(
            &request_id.0,
            format!(
                "that attachment is larger than the {} bytes this route serves",
                chat::MAX_ATTACHMENT_BYTES
            ),
        )
        .into_response();
    }
    let media_type = chat::sniff_media_type(&bytes);
    // An image is inline, because the transcript shows it. Anything else is an
    // attachment, so a browser downloads an upload rather than executing it in
    // this origin. The name is stripped of the two characters that could end the
    // header, because a row is data and a header is not.
    let disposition = if media_type.starts_with("image/") {
        "inline"
    } else {
        "attachment"
    };
    let name: String = row
        .name
        .chars()
        .map(|character| {
            if character == '"' || character.is_control() {
                '_'
            } else {
                character
            }
        })
        .collect();
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, media_type.to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("{disposition}; filename=\"{name}\""),
            ),
            (header::CACHE_CONTROL, "private, max-age=300".to_owned()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        ],
        bytes,
    )
        .into_response()
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
    // The rail is refreshed by the page's own script, so it resolves the offset
    // the same way the page did rather than carrying a second clock.
    let offset = crate::zone::offset_minutes(
        state.engine.db(),
        crate::zone::Zone::parse(
            state
                .engine
                .setting(crate::zone::SETTING_TIMEZONE)
                .ok()
                .flatten()
                .as_deref(),
        ),
    );
    match chat::conversations(state.engine.db(), chat::SESSION_LIMIT) {
        Ok(rows) => {
            // The rail draws the same headings the server does, so the payload
            // carries the grouping, not only the rows: a script that was handed a
            // flat list could only ever paint a flat rail.
            let projects: Vec<serde_json::Value> = crate::projects::list(state.engine.db())
                .unwrap_or_default()
                .into_iter()
                .map(|project| serde_json::json!({ "id": project.id, "name": project.name }))
                .collect();
            let sessions: Vec<serde_json::Value> = rows
                .into_iter()
                .map(|row| {
                    serde_json::json!({
                        "id": row.id,
                        "title": row.title,
                        "stamp": crate::zone::clock_in(&row.updated_at, offset),
                        "project_id": row.project_id,
                    })
                })
                .collect();
            ok_json(serde_json::json!({ "ok": true, "projects": projects, "sessions": sessions }))
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

/// `POST /dashboard/projects` - name a project, then go back to the rail it
/// appears in.
///
/// The control that posts this is in the rail, and the rail is on the chat
/// surface, so that is where the browser returns: the form stays a form rather
/// than a JSON endpoint that happens to be rendered as one. A refusal is the chat
/// page back, with the message in the page's own live region, because the field
/// that failed is on that page.
async fn save_project(
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
        Err(error) => return fail_chat(&state, as_html, None, error),
    };

    let name = field(&fields, "project_name").trim().to_owned();
    match crate::projects::create(state.engine.db(), &name) {
        Ok(id) => {
            if let Err(error) = state.engine.auth().audit(
                "loopback",
                "projects.created",
                Some(&format!("{name}; id={id}")),
            ) {
                return audit_failure(&request_id.0, "projects.created", error);
            }
            see_other(ROUTE_CHAT)
        }
        // A name that is blank, too long, or taken is the operator's to fix, so
        // it is a field error on the page that offered the field - not a 500 and
        // not a silent redirect that looks like it worked.
        Err(crate::db::DbError::InvalidProjectName { max }) => fail_chat(
            &state,
            as_html,
            None,
            FormError::new(
                Some("project_name"),
                "project_name_invalid",
                format!("a project name is 1 to {max} characters"),
            ),
        ),
        Err(crate::db::DbError::DuplicateProjectName { name }) => fail_chat(
            &state,
            as_html,
            None,
            FormError::new(
                Some("project_name"),
                "project_name_duplicate",
                format!("a project named `{name}` already exists"),
            ),
        ),
        Err(error) => store_failure(&request_id.0, "the project", error),
    }
}

/// `POST /dashboard/projects/assign` - file one conversation under a project, or
/// take it out of one.
///
/// An empty `project` field is the select's "No project" entry, which is a clear
/// rather than a missing value: a form that could only ever file conversations
/// would make a mistake permanent.
async fn save_project_assignment(
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
        Err(error) => return fail_chat(&state, as_html, None, error),
    };

    let conversation = field(&fields, "conversation").trim().to_owned();
    if conversation.is_empty() {
        return fail_chat(
            &state,
            as_html,
            None,
            FormError::new(
                Some("conversation"),
                "conversation_missing",
                "the form did not name a conversation",
            ),
        );
    }
    let requested = field(&fields, "project").trim().to_owned();
    let project = if requested.is_empty() {
        None
    } else {
        Some(requested.as_str())
    };

    match crate::projects::assign(state.engine.db(), &conversation, project) {
        // One row was updated, or none was: nothing changed is not a success, and
        // saying so is the difference between a working form and one that lies.
        Ok(0) => fail_chat(
            &state,
            as_html,
            Some(&conversation),
            FormError::new(
                Some("project"),
                "project_assign_refused",
                "that conversation or that project is not one this daemon knows",
            ),
        ),
        Ok(_) => {
            let detail = match project {
                Some(id) => format!("{conversation}; project={id}"),
                None => format!("{conversation}; project=none"),
            };
            if let Err(error) =
                state
                    .engine
                    .auth()
                    .audit("loopback", "projects.assigned", Some(&detail))
            {
                return audit_failure(&request_id.0, "projects.assigned", error);
            }
            see_other(ROUTE_CHAT)
        }
        Err(error) => store_failure(&request_id.0, "the assignment", error),
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

// ---------------------------------------------------------------------------
// Slash commands
// ---------------------------------------------------------------------------

/// Answer a message that is a slash command.
///
/// The caller reaches this before a turn exists, so no provider is called and no
/// context is built: there is no turn for a command to spend a token on.
///
/// The answer is written as an assistant transcript row and the operator is
/// redirected to the conversation it belongs in. That is the same mechanism a
/// settled turn uses, so a command's result is read exactly like an answer: it
/// survives a refresh, it appears in the rail's transcript, and nothing had to
/// invent a second place to put it. `append` with [`chat::State::Complete`] says
/// the text is whole already, which it is.
///
/// When the request asked for the event stream the answer is framed as one
/// `command` event instead of a redirect, because a page that posted with `fetch`
/// never sees a 303: it would follow it internally and leave the operator looking
/// at the conversation the command has just left. `/new` is exactly that case, and
/// the frame carries the conversation the answer landed in so the page can move
/// to it without a second request.
///
/// A refusal is answered the same way rather than as a failed request. The command
/// was handled, and "that model id is not in the list" is the answer to it, not an
/// error in the form: the operator typed a command, and the transcript is where
/// the daemon speaks.
fn run_command(
    state: &AppState,
    requested: &str,
    command: &crate::commands::Command,
    request_id: &str,
    streamed: bool,
) -> Response {
    let db = state.engine.db().clone();
    // Resolved before the match: every command but `/new` answers into the
    // conversation the operator is looking at, and `/new` answers into the one it
    // has just created.
    let current = match chat::resolve_conversation(&db, Some(requested)) {
        Ok(conversation) => conversation,
        Err(error) => {
            return refusal(FormError::new(None, "chat_unreadable", error.to_string()));
        }
    };

    let answered = match crate::commands::immediate(command) {
        Some(crate::commands::Outcome::Reply(text)) => Ok((current.clone(), text)),
        Some(crate::commands::Outcome::Unsupported(name)) => {
            Ok((current.clone(), crate::commands::unsupported(&name)))
        }
        None => command_effect(state, &current, command),
    };
    let (conversation, text) = match answered {
        Ok(pair) => pair,
        Err(error) => return refusal(error),
    };

    // Recorded before the row is written, because a command that changed a
    // setting and was not recorded did not happen as far as this product is
    // concerned. The detail is the command itself, never the answer, so the audit
    // log stays a log of actions rather than a second transcript.
    if let Err(error) =
        state
            .engine
            .auth()
            .audit("loopback", "chat.command", Some(&command_detail(command)))
    {
        return audit_failure(request_id, "chat.command", error);
    }

    let _ = chat::touch_conversation(&db, &conversation);
    match chat::append(
        &db,
        &conversation,
        chat::Role::Assistant,
        &text,
        chat::State::Complete,
    ) {
        Ok(_) if streamed => command_frame(&conversation, &text),
        Ok(_) => see_other_owned(conversation_path(&conversation)),
        Err(error) => refusal(FormError::new(None, "chat_write_failed", error.to_string())),
    }
}

/// The one frame a command answers with on the event stream.
///
/// It carries the conversation and the text, which is everything the page needs
/// to say what happened and, for `/new`, to carry on in the conversation the
/// command created. `url` is the same path the plain form's redirect would have
/// gone to, so the script has one value to follow and the page has one shape.
fn command_frame(conversation: &str, text: &str) -> Response {
    let event = SseEvent::default().event("command").data(
        serde_json::json!({
            "conversation": conversation,
            "url": conversation_path(conversation),
            "text": text,
        })
        .to_string(),
    );
    let stream = tokio_stream::iter(vec![Ok::<SseEvent, Infallible>(event)]);
    Sse::new(stream)
        .keep_alive(SseKeepAlive::default())
        .into_response()
}

/// The audit detail for one command: the name and the argument it carried.
///
/// Bounded, because the argument is operator text and an audit row is not the
/// place for a paragraph.
fn command_detail(command: &crate::commands::Command) -> String {
    let argument: String = command.argument.chars().take(120).collect();
    if argument.is_empty() {
        format!("/{}", command.name)
    } else {
        format!("/{} {}", command.name, argument)
    }
}

/// The commands that read or write the store.
///
/// Each returns the conversation the answer belongs to and the text to write into
/// it. `Err` is reserved for a store that could not be read or written: a command
/// that refuses on its own terms ("that id is not in the list") is still an
/// answer, and it carries the reason in its own words.
fn command_effect(
    state: &AppState,
    current: &str,
    command: &crate::commands::Command,
) -> Result<(String, String), FormError> {
    match command.name.as_str() {
        // Same effect as the new-chat route: one new conversation, and the answer
        // lands in it, so the operator is looking at the empty transcript the
        // command promised.
        "new" => match chat::create_conversation(state.engine.db()) {
            Ok(id) => Ok((
                id,
                "Started a new conversation, and this is it. The last one is untouched and \
                 still in the session list."
                    .to_owned(),
            )),
            Err(error) => Err(FormError::new(None, "chat_write_failed", error.to_string())),
        },
        // The same clear the clear button runs, so there is one behaviour and not
        // two that drift.
        "clear" => match chat::clear(state.engine.db(), current) {
            Ok(removed) => Ok((
                current.to_owned(),
                format!(
                    "Cleared this conversation: {} removed. This line is the only one left in it.",
                    message_count(removed)
                ),
            )),
            Err(error) => Err(FormError::new(None, "chat_write_failed", error.to_string())),
        },
        "stop" => stop_turn(state, current),
        "model" => set_model(state, current, &command.argument),
        "memory" => set_memory(state, current, &command.argument),
        // Unreachable while `immediate` handles every name outside the list, and
        // answered honestly rather than with a panic if that ever changes.
        other => Ok((current.to_owned(), crate::commands::unsupported(other))),
    }
}

/// A count of transcript rows, for copy that has to read like a sentence.
fn message_count(count: usize) -> String {
    if count == 1 {
        "1 message".to_owned()
    } else {
        format!("{count} messages")
    }
}

/// Cancel a turn that is running in this conversation.
///
/// Two halves, and both are needed now.
///
/// The row half: a running turn is a row still marked `streaming`, read from the
/// database, and it is what the streaming task checks before it settles. Marking
/// it `cancelled` means the answer that is being written stops landing.
///
/// The turn half: the row being failed does not, on its own, stop a turn that is
/// in the middle of a provider round trip, so the running turn is also asked to
/// stop at its next boundary. That is the honest limit and this copy states it -
/// the provider request already streaming is not aborted mid-flight, because
/// `stream_chat` is one blocking call with no seam inside it to interrupt. What
/// stops, promptly, is everything after it: no further round trips, no further
/// tool calls, and no answer written.
fn stop_turn(state: &AppState, conversation: &str) -> Result<(String, String), FormError> {
    // Asked first, so a turn that finishes while this is running still sees it.
    let signalled = state.steering.stop(conversation);
    let db = state.engine.db();
    let running = match chat::streaming(db, conversation) {
        Ok(ids) => ids,
        Err(error) => return Err(FormError::new(None, "chat_unreadable", error.to_string())),
    };
    if running.is_empty() && !signalled {
        return Ok((
            conversation.to_owned(),
            "Nothing was running in this conversation, so there was nothing to stop.".to_owned(),
        ));
    }
    for id in &running {
        if let Err(error) = chat::fail(
            db,
            id,
            "cancelled",
            "Stopped by the operator before it finished.",
        ) {
            return Err(FormError::new(None, "chat_write_failed", error.to_string()));
        }
    }
    let stopped = match running.len() {
        // A signalled turn whose row is already gone is still the turn that was
        // running: the row is what the answer writes to, not what runs.
        0 | 1 => "the turn that was running".to_owned(),
        count => format!("the {} that were running", message_count(count)),
    };
    let limit = if signalled {
        " It stops at its next boundary: no further provider round trips, and no answer \
         written. The request already streaming is not interrupted."
    } else {
        " Its answer will not be written into the transcript."
    };
    Ok((
        conversation.to_owned(),
        format!("Stopped {stopped}.{limit}"),
    ))
}

/// Set the stored default model, if the id is one the endpoint published.
///
/// An id outside the list is refused by name and the stored model is left alone:
/// a default that silently changed to something the endpoint never offered would
/// fail on the next turn, which is the worst place to discover a typo.
fn set_model(
    state: &AppState,
    current: &str,
    argument: &str,
) -> Result<(String, String), FormError> {
    let id = argument.trim();
    if id.is_empty() {
        return Ok((
            current.to_owned(),
            "That needs a model id: /model <id>. The stored default was not changed.".to_owned(),
        ));
    }
    if id.len() > 200 {
        return Ok((
            current.to_owned(),
            "That model id is longer than any endpoint serves, so the stored default was not \
             changed."
                .to_owned(),
        ));
    }
    let known = match state.engine.provider_models() {
        Ok(known) => known,
        Err(error) => {
            return Err(FormError::new(
                None,
                "settings_unreadable",
                error.to_string(),
            ));
        }
    };
    if known.is_empty() {
        return Ok((
            current.to_owned(),
            format!(
                "No model list has been stored yet, so {id} cannot be checked against one and \
                 the stored default was not changed. Run the probe on the settings page, then \
                 ask again."
            ),
        ));
    }
    if !known.iter().any(|known| known == id) {
        return Ok((
            current.to_owned(),
            format!(
                "{id} is not in the model list this endpoint returned, so the stored default was \
                 not changed. /model takes an id from that list."
            ),
        ));
    }
    match store::settings_put(state.engine.db(), SETTING_MODEL, id) {
        Ok(()) => Ok((
            current.to_owned(),
            format!("The stored default model is now {id}."),
        )),
        Err(error) => Err(FormError::new(
            None,
            "settings_write_failed",
            error.to_string(),
        )),
    }
}

/// Set the memory backend, refusing anything outside the three that exist.
///
/// Choosing `vault` without a directory is allowed and then reported: the setting
/// is the backend, not the path, and a host that has not given a path yet should
/// hear that from the answer rather than be told its command was invalid.
fn set_memory(
    state: &AppState,
    current: &str,
    argument: &str,
) -> Result<(String, String), FormError> {
    let backend = match crate::commands::memory_backend(argument) {
        Ok(backend) => backend,
        Err(refusal) => return Ok((current.to_owned(), refusal)),
    };
    if let Err(error) = store::settings_put(
        state.engine.db(),
        crate::memory::SETTING_BACKEND,
        backend.as_str(),
    ) {
        return Err(FormError::new(
            None,
            "settings_write_failed",
            error.to_string(),
        ));
    }
    let mut text = format!("The memory backend is now {}.", backend.as_str());
    if backend == crate::memory::Backend::Vault {
        let stored = state
            .engine
            .setting(crate::memory::SETTING_VAULT)
            .ok()
            .flatten()
            .unwrap_or_default();
        if stored.is_empty() || !std::path::Path::new(&stored).is_dir() {
            text.push_str(
                " No vault directory is set, so a turn will recall nothing until one is: set it \
                 on the settings page.",
            );
        }
    }
    Ok((current.to_owned(), text))
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

    // One answer for both halves: the operator's stored switch wins over a
    // request header, because a page that says "do not stream" must not stream
    // just because a script asked nicely.
    let streaming = chat::streaming_enabled(
        state
            .engine
            .setting(chat::SETTING_STREAMING)
            .ok()
            .flatten()
            .as_deref(),
    );
    let wants_stream = wants_stream(&headers) && streaming;

    // A message that begins with a slash is a command, not a prompt. It is
    // dispatched here, before a turn is built and before the provider is even
    // read: a command has no turn to spend a token on, so the guarantee is
    // structural rather than a rule somebody has to remember.
    if let Some(command) = crate::commands::parse(&text) {
        return run_command(
            &state,
            field(&fields, "conversation"),
            &command,
            &request_id.0,
            wants_stream,
        );
    }

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

    // One turn at a time, per conversation. The claim below is taken before the
    // first row is written, so two sends that arrive together cannot both start a
    // turn: one owns the conversation and the other is queued for it. That is what
    // a second message during streaming is - a steer, not a second turn.
    let mut steer_text = text.clone();
    // A file rides with a steer exactly the way it rides with a question: the
    // running turn cannot see its bytes, so it is told what came with the message.
    if !files.is_empty() {
        let described = chat::describe_attachments(&files);
        if !described.trim().is_empty() {
            steer_text.push_str("\n\n");
            steer_text.push_str(&described);
        }
    }
    let lease = match state.steering.claim(&conversation, &steer_text) {
        chat::Claim::Turn(lease) => lease,
        chat::Claim::Steered(waiting) => {
            // The operator's words are recorded either way. What changes is who
            // answers them: the turn that is already streaming, at its next
            // boundary, instead of a second turn starting behind it and billing
            // the same conversation twice.
            let steered = match chat::append(
                &db,
                &conversation,
                chat::Role::User,
                &text,
                chat::State::Complete,
            ) {
                Ok(id) => id,
                Err(error) => {
                    return refusal(FormError::new(None, "chat_write_failed", error.to_string()));
                }
            };
            let attachment_ids: Vec<String> = files.iter().map(|file| file.id.clone()).collect();
            let _ = chat::claim_attachments(&db, &conversation, &steered, &attachment_ids);
            let _ = chat::touch_conversation(&db, &conversation);
            // Recorded without the words, like every other chat audit row: a steer
            // is an action on a turn, and the transcript is not the audit log.
            let _ = state.engine.auth().audit(
                "loopback",
                "chat.steer",
                Some(&format!("queued={waiting}")),
            );
            // The normal redirect, to the page that now says a message is waiting
            // for the turn that is running in it.
            return see_other(ROUTE_CHAT);
        }
    };

    let question_id = match chat::append(
        &db,
        &conversation,
        chat::Role::User,
        &text,
        chat::State::Complete,
    ) {
        Ok(id) => id,
        Err(error) => {
            return refusal(FormError::new(None, "chat_write_failed", error.to_string()));
        }
    };
    // The files landed before the message existed. Now that it does, they are
    // claimed by it, so a reloaded transcript can say what went with the
    // question rather than showing a bare sentence.
    let attachment_ids: Vec<String> = files.iter().map(|file| file.id.clone()).collect();
    let _ = chat::claim_attachments(&db, &conversation, &question_id, &attachment_ids);

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
    // Turns the window leaves behind. With a compression role pinned they are
    // summarised into the prompt; without one they are dropped exactly as before.
    let dropped = chat::dropped(&transcript, &answer_id);
    let compression = chat::aux_pinned_config(&state.engine, "compression")
        .ok()
        .flatten();
    let vision = chat::aux_pinned_config(&state.engine, "vision")
        .ok()
        .flatten();
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

    if !wants_stream {
        let task = spawn_turn(
            &state,
            config,
            TurnJob {
                conversation: conversation.clone(),
                question: text,
                answer_id,
                compression,
                vision,
                files: files.clone(),
                dropped,
                history,
            },
            lease,
            None,
        );
        let _ = task.await;
        return see_other(ROUTE_CHAT);
    }

    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Result<SseEvent, Infallible>>();
    spawn_turn(
        &state,
        config,
        TurnJob {
            conversation: conversation.clone(),
            question: text,
            answer_id,
            compression,
            vision,
            files: files.clone(),
            dropped,
            history,
        },
        lease,
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

/// Read a chat form that may carry files.
///
/// Everything the composer needs fits in one multipart body, so the no-script
/// path is the same route: the browser posts `message`, `model`, `conversation`
/// and any number of `file` parts, up to the caps in [`chat`]. Each file is
/// written into the workspace before the turn runs, and the row that records it
/// is written here too, so a file on disk always has a row and a row always has
/// a file.
///
/// The part name is `file` whichever way it arrives: a browser picker names its
/// parts itself, and a script that appends several files under `file` is the same
/// shape. `files` is accepted as a spelling too, because a form that collects a
/// multiple picker naturally calls the field by its plural.
/// One multipart failure as the form the page can show.
///
/// Axum's own "length limit exceeded" is the default body ceiling, not a bad
/// form: saying so, with the number the daemon actually accepts, is the
/// difference between "try again" and "attach a smaller file".
fn multipart_failure(context: &str, error: impl std::fmt::Display) -> FormError {
    let text = error.to_string();
    if text.contains("length limit") {
        return FormError::new(
            Some("file"),
            "upload_too_large",
            format!(
                "that message is larger than the {} bytes this daemon accepts in one send;                  attach fewer or smaller files",
                CHAT_BODY_BYTES
            ),
        );
    }
    FormError::new(None, "upload_unreadable", format!("{context}: {text}"))
}

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
        .map_err(|error| multipart_failure("the form did not parse as multipart", error))?;

    let mut fields = std::collections::BTreeMap::new();
    let mut files: Vec<chat::Attachment> = Vec::new();
    let mut total_bytes = 0usize;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|error| multipart_failure("the form did not parse as multipart", error))?
    {
        let name = field.name().unwrap_or_default().to_owned();
        if name != "file" && name != "files" {
            let value = field
                .text()
                .await
                .map_err(|error| multipart_failure(&format!("field {name}"), error))?;
            fields.insert(name, value);
            continue;
        }
        // The cap is checked before the bytes are read, so a ninth file is
        // refused without first being written to memory.
        if files.len() >= chat::MAX_ATTACHMENTS {
            return Err(FormError::new(
                Some("file"),
                "too_many_attachments",
                format!(
                    "a message may carry at most {} files; this one carries more",
                    chat::MAX_ATTACHMENTS
                ),
            ));
        }
        // A file part. An empty picker sends an empty part with no filename, and
        // that is "no attachment", not an error.
        let Some(original) = field.file_name().map(str::to_owned) else {
            while field
                .chunk()
                .await
                .map_err(|error| multipart_failure("file part", error))?
                .is_some()
            {}
            continue;
        };
        if original.trim().is_empty() {
            continue;
        }
        let mut bytes: Vec<u8> = Vec::new();
        while let Some(chunk) = field
            .chunk()
            .await
            .map_err(|error| multipart_failure("file part", error))?
        {
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
            if total_bytes + bytes.len() + chunk.len() > chat::MAX_ATTACHMENT_TOTAL_BYTES {
                return Err(FormError::new(
                    Some("file"),
                    "upload_too_large",
                    format!(
                        "the attachments on one message must total under {} bytes",
                        chat::MAX_ATTACHMENT_TOTAL_BYTES
                    ),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() {
            continue;
        }
        total_bytes += bytes.len();
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
            media_type: Some(chat::sniff_media_type(&bytes).to_owned()),
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

/// Everything one turn needs that is already decided before it starts.
///
/// Grouped so the signature stays readable: a turn takes nine things, and nine
/// positional arguments is how a call site quietly passes the wrong one.
struct TurnJob {
    conversation: String,
    question: String,
    answer_id: String,
    compression: Option<chat::TurnConfig>,
    vision: Option<chat::TurnConfig>,
    files: Vec<chat::Attachment>,
    dropped: Vec<chat::Message>,
    history: Vec<core_runtime::provider::ChatMessage>,
}

/// Run one chat turn on its own thread, persisting the outcome.
///
/// The sink forwards each fragment to the channel when there is one, and the
/// final row is written before the terminal event is sent, so a browser that
/// reloads right after `done` reads the same text it just saw.
///
/// The lease is the conversation's claim while this turn runs, and the turn's ear
/// while it streams: it is moved into the blocking task so the entry lives exactly
/// as long as the turn does. Dropping it at the end of the task is what makes the
/// next message a turn of its own again.
fn spawn_turn(
    state: &AppState,
    config: chat::TurnConfig,
    job: TurnJob,
    lease: chat::TurnLease,
    sender: Option<tokio::sync::mpsc::UnboundedSender<Result<SseEvent, Infallible>>>,
) -> tokio::task::JoinHandle<()> {
    let TurnJob {
        conversation,
        question,
        answer_id,
        compression,
        vision,
        files,
        dropped,
        history,
    } = job;
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
    let mcp = state.mcp.clone();
    let approval_policy = state
        .engine
        .permissions()
        .map(|permissions| permissions.approval_policy)
        .unwrap_or(crate::onboarding::permissions::ApprovalPolicy::Ask);
    let show_tool_log = chat::show_tool_log(
        state
            .engine
            .setting(chat::SETTING_SHOW_TOOL_LOG)
            .ok()
            .flatten()
            .as_deref(),
    );
    let show_reasoning = chat::show_reasoning(
        state
            .engine
            .setting(chat::SETTING_SHOW_REASONING)
            .ok()
            .flatten()
            .as_deref(),
    );
    let tools_enabled = crate::tools::enabled(
        state
            .engine
            .setting(crate::tools::SETTING_TOOLS_ENABLED)
            .ok()
            .flatten()
            .as_deref(),
    );
    // A role on the primary is a no-op, so this is None unless the operator
    // pinned a model for naming conversations.
    let title_config = chat::aux_pinned_config(&state.engine, "title_generation")
        .ok()
        .flatten();
    let title_audit = state.engine.auth().clone();
    // The quota line is read after the turn from the gateway's own database, so
    // the path is resolved now, while the app state is still borrowable.
    let quota_db = crate::web::quota::show_quota(
        state
            .engine
            .setting(crate::web::quota::SETTING_SHOW_QUOTA)
            .ok()
            .flatten()
            .as_deref(),
    )
    .then(|| {
        crate::web::quota::gateway_db(
            state
                .engine
                .setting(crate::web::quota::SETTING_GATEWAY_DB)
                .ok()
                .flatten()
                .as_deref(),
        )
    })
    .flatten();
    let web_extract = chat::aux_pinned_config(&state.engine, "web_extract")
        .ok()
        .flatten();
    let approval_role = chat::aux_pinned_config(&state.engine, "approval")
        .ok()
        .flatten();
    let image = chat::aux_pinned_config(&state.engine, "image")
        .ok()
        .flatten();
    // A fallback is opt-in: unpinned means the role inherits the primary, and
    // `aux_pinned_config` returns None for that, so a turn fails as it always did.
    let fallback = chat::aux_pinned_config(&state.engine, "fallback")
        .ok()
        .flatten();
    // Read before the blocking task: the closure owns strings, not a borrow of
    // the app state.
    let memory_backend = crate::memory::backend(
        state
            .engine
            .setting(crate::memory::SETTING_BACKEND)
            .ok()
            .flatten()
            .as_deref(),
    );
    let memory_vault = state
        .engine
        .setting(crate::memory::SETTING_VAULT)
        .ok()
        .flatten();
    // The unit's PATH does not carry ~/.bun/bin, so the binary is resolved here
    // rather than trusted to the shell.
    let memory_gbrain = state
        .engine
        .setting(crate::memory::SETTING_GBRAIN_BIN)
        .ok()
        .flatten()
        .and_then(|configured| crate::memory::gbrain_binary(Some(&configured)))
        .or_else(|| crate::memory::gbrain_binary(None));
    let tool_rounds = crate::tools::max_rounds(
        state
            .engine
            .setting(crate::tools::SETTING_TOOL_ROUNDS)
            .ok()
            .flatten()
            .as_deref(),
    );
    let stored_enabled = state
        .engine
        .setting(crate::persona::SETTING_PERSONA_ENABLED)
        .ok()
        .flatten();
    // Read before the blocking task, like every other setting: the closure owns
    // strings, not a borrow of the app state.
    let agents_enabled = crate::subagents::enabled(
        state
            .engine
            .setting(crate::subagents::SETTING_AGENTS_ENABLED)
            .ok()
            .flatten()
            .as_deref(),
    );
    // The parent's stream hears about every status change, so a live page can
    // recount the bar without polling. A plain form post has no channel and needs
    // none: the row in the database is the answer, and the next page load reads
    // it.
    let notify_sender = sender.clone();
    let notify: crate::subagents::Notify =
        std::sync::Arc::new(move |report: &crate::subagents::Report<'_>| {
            send_terminal(&notify_sender, "subagent", subagent_frame(report));
        });

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
            // A write, or any call into an MCP server: both are code the operator
            // installed acting on their behalf, so both wait under `ask`.
            let gated = call.name == "write_file"
                || crate::mcp::protocol::split_qualified(&call.name).is_some();
            if !gated || approval_policy == crate::onboarding::permissions::ApprovalPolicy::Never {
                return chat::GateDecision::Run;
            }
            // Advice before the row: the person deciding sees a reading, and the
            // reading is never what decides.
            let advice = approval_role.as_ref().and_then(|config| {
                chat::advise_approval(
                    config,
                    &call.name,
                    &call.arguments.to_string(),
                    sandbox.mode().as_str(),
                )
            });
            if approval_role.is_some() {
                let detail = match &advice {
                    Some(text) => text.clone(),
                    None => "no advice".to_owned(),
                };
                let _ = title_audit.audit("loopback", "chat.approval", Some(&detail));
            }
            match crate::approvals::queue(
                &db,
                &conversation,
                &call.name,
                &call.arguments,
                advice.as_deref(),
            ) {
                Ok(approval_id) => chat::GateDecision::Park { approval_id },
                Err(error) => chat::GateDecision::Refuse {
                    reason: format!(
                        "the approval queue is unavailable ({error}), so nothing was written"
                    ),
                },
            }
        };

        // The picture has to reach a model that can see, before the turn runs.
        let vision_note = vision
            .as_ref()
            .and_then(|config| chat::describe_images(config, &files));
        let images = files.iter().filter(|file| chat::is_image(file)).count();
        if vision.is_some() {
            let detail = match &vision_note {
                Some(text) => format!("images={}; description_chars={}", files.len(), text.len()),
                None => format!("images={}; no description", files.len()),
            };
            let _ = title_audit.audit("loopback", "chat.vision", Some(&detail));
        }
        // A picture that never reached a model used to vanish: the turn ran blind
        // and nobody was told. Now the operator hears it on the stream and the
        // model is told in the prompt, so neither pretends the image was seen.
        let vision_lost = images > 0 && vision_note.is_none();
        if vision_lost {
            let reason = match &vision {
                Some(config) => format!(
                    "the vision model ({}) did not describe the attached image - it may be rate limited",
                    config.model
                ),
                None => "no vision role is pinned, so the attached image was not read".to_owned(),
            };
            send_terminal(
                &sender,
                "notice",
                serde_json::json!({ "code": "vision_unavailable", "message": reason }),
            );
        }
        let mut history = history;
        if vision_lost {
            history.push(core_runtime::provider::ChatMessage::text(
                core_runtime::provider::ChatRole::User,
                "The attached image could not be read by a vision model this turn. Say so \
                 rather than guessing at its contents."
                    .to_owned(),
            ));
        }
        if let Some(text) = &vision_note {
            history.push(core_runtime::provider::ChatMessage::text(
                core_runtime::provider::ChatRole::User,
                format!("What the vision model sees in the attached image:\n{text}"),
            ));
        }

        // What the daemon already knows, before it is asked to answer.
        let backend = memory_backend;
        let (recalls, recall_error) = crate::memory::recall_with(
            backend,
            memory_gbrain.as_deref(),
            memory_vault.as_deref().map(std::path::Path::new),
            &question,
            crate::memory::MAX_RECALLS,
        );
        let detail = match &recall_error {
            Some(reason) => format!("backend={}; recalled=0; reason={reason}", backend.as_str()),
            None => format!("backend={}; recalled={}", backend.as_str(), recalls.len()),
        };
        let _ = title_audit.audit("loopback", "memory.recall", Some(&detail));
        let recalled = crate::memory::render(&recalls);

        let summary = compression
            .as_ref()
            .and_then(|config| chat::summarise(config, &dropped));
        // Recorded either way: a role that never lands must not look wired.
        if compression.is_some() {
            let detail = match &summary {
                Some(text) => format!("dropped={}; summary_chars={}", dropped.len(), text.len()),
                None => format!("dropped={}; no summary", dropped.len()),
            };
            let _ = title_audit.audit("loopback", "chat.compression", Some(&detail));
        }
        let system = match (
            system,
            if recalled.is_empty() {
                None
            } else {
                Some(recalled)
            },
        ) {
            (Some(system), Some(recalled)) => Some(format!("{system}\n\n{recalled}")),
            (None, Some(recalled)) => Some(recalled),
            (system, None) => system,
        };
        let system = match (system, summary) {
            (Some(system), Some(summary)) => Some(format!(
                "{system}\n\nEarlier in this conversation, summarised:\n{summary}"
            )),
            (None, Some(summary)) => Some(format!(
                "Earlier in this conversation, summarised:\n{summary}"
            )),
            (system, None) => system,
        };

        // What the page can draw, said to the model: a workspace path in
        // `![caption](path)` or `[name](path)` becomes a thumbnail or a chip.
        let system = crate::persona::PersonaConfig::with_capabilities(system);

        // The dispatch surface, when this turn may dispatch at all. Both gates
        // are checked here and again in the turn: the operator's switch, and the
        // sandbox mode. A confined turn cannot spawn children, because a child
        // runs with the daemon's own reach and confinement that can be escaped by
        // proxy is not confinement.
        let desk = (agents_enabled
            && sandbox.mode() == crate::onboarding::permissions::SandboxMode::Full)
            .then(|| {
                crate::subagents::Desk::new(
                    db.clone(),
                    config.clone(),
                    system.clone(),
                    sandbox.workspace().to_path_buf(),
                    conversation.clone(),
                    Some(answer_id.clone()),
                )
                .with_notify(notify.clone())
            });

        let mut streamed = String::new();
        let mut sink = |part: chat::StreamPart<'_>| match part {
            chat::StreamPart::Text(fragment) => {
                streamed.push_str(fragment);
                send_terminal(&sender, "delta", serde_json::json!({ "text": fragment }));
            }
            chat::StreamPart::Reasoning(fragment) => {
                // Switched off means not sent at all, so a disabled setting also
                // stops the reasoning leaving the daemon.
                if show_reasoning {
                    send_terminal(
                        &sender,
                        "reasoning",
                        serde_json::json!({ "text": fragment }),
                    );
                }
            }
            chat::StreamPart::Tool {
                name,
                arguments,
                output,
                is_error,
            } => {
                // Behind its own switch, not the reasoning one. Off means the run
                // log is neither rendered nor streamed, so a disabled log does not
                // still ship command output to the browser.
                if show_tool_log {
                    // The window's own reading of the result, computed here so the
                    // streaming script and the rendered page cannot disagree about
                    // what a command printed. `output` still rides along: it is what
                    // the transcript stores, and a script that wanted the raw
                    // result should not have to reconstruct it.
                    let term = crate::web::term::render(name, arguments, output, is_error);
                    send_terminal(
                        &sender,
                        "tool",
                        serde_json::json!({
                            "name": name,
                            "arguments": arguments,
                            "output": output,
                            "is_error": is_error,
                            "term": term,
                        }),
                    );
                }
            }
            chat::StreamPart::ToolOutput { name, chunk } => {
                // The same switch as the finished call, for the same reason: a
                // log that is off must not stream a command's output either.
                if show_tool_log {
                    send_terminal(
                        &sender,
                        "tool_output",
                        serde_json::json!({ "name": name, "chunk": chunk }),
                    );
                }
            }
        };
        // The turn's ear: the same lease that claims the conversation answers
        // what the operator typed while it runs. A `&mut dyn FnMut` closure rather
        // than a second copy of the loop, so the steer lands inside this turn.
        let mut poll = || lease.signal();
        let started = std::time::Instant::now();
        let (outcome, note) = chat::run_turn_resilient(
            &config,
            fallback.as_ref(),
            system,
            history,
            &sandbox,
            &mut gate,
            Some(&mcp),
            web_extract.as_ref(),
            image.as_ref(),
            desk.as_ref(),
            chat::TurnSettings {
                tools: tools_enabled,
                max_rounds: tool_rounds,
                agents: agents_enabled,
            },
            &mut poll,
            &mut sink,
        );
        // Say what the fallback did, in the audit and on the stream. A retry is
        // an operator-visible fact: the answer that follows did not come from the
        // model the composer asked for.
        let answered_model = match &note {
            chat::ResilientNote::Retried {
                primary_model,
                primary_code,
                model,
            } => {
                let _ = title_audit.audit(
                    "loopback",
                    "chat.fallback",
                    Some(&format!(
                        "primary={primary_model}; primary_code={primary_code}; fallback={model}"
                    )),
                );
                send_terminal(
                    &sender,
                    "notice",
                    serde_json::json!({
                        "code": "fallback",
                        "message": format!(
                            "the primary model failed ({primary_code}); retrying with {model}"
                        ),
                    }),
                );
                Some(model.clone())
            }
            chat::ResilientNote::FallbackUnusable(reason) => {
                let _ = title_audit.audit(
                    "loopback",
                    "chat.fallback",
                    Some(&format!("unusable; {reason}")),
                );
                None
            }
            chat::ResilientNote::Primary => None,
        };
        let duration_ms = started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
        drop(sink);
        // Anything still waiting had no round left to join, and is said out loud
        // rather than dropped. It is a race measured in microseconds, and the row
        // it already wrote stays in the transcript.
        let stranded = lease.drain();

        match outcome {
            Ok(turn) => {
                // The row is the record, so its state decides whether this answer
                // belongs in the transcript. `/stop` marks a running row failed,
                // and settling over that would replace the operator's decision
                // with the answer they already cancelled. Only a definite "no
                // longer streaming" stops the write: a read that failed leaves the
                // answer to land, because losing a finished turn to a transient
                // read error is the worse of the two failures.
                if matches!(chat::still_streaming(&db, &answer_id), Ok(false)) {
                    send_terminal(
                        &sender,
                        "error",
                        serde_json::json!({
                            "code": "cancelled",
                            "message": "the operator stopped this turn, so its answer was not written",
                        }),
                    );
                    send_stranded(&sender, &stranded);
                    return;
                }
                let usage = turn.usage;
                let accounting = chat::Accounting {
                    prompt_tokens: usage.input_tokens(),
                    completion_tokens: usage.output_tokens(),
                    cached_tokens: usage.cache_read_tokens(),
                    duration_ms: Some(duration_ms),
                    rounds: Some(turn.rounds as u64),
                };
                let raw = serde_json::to_value(usage).ok();
                let _ = chat::settle(
                    &db,
                    &answer_id,
                    &turn.text,
                    Some(&turn.reasoning),
                    &turn.tools,
                    raw.as_ref(),
                    accounting,
                );
                // A fallback answer is badged with the model that produced it,
                // not the primary that failed. The transcript is the record.
                let model = answered_model.as_deref().unwrap_or(&config.model);
                let _ = chat::set_model(&db, &answer_id, model);
                let quota = quota_db
                    .as_deref()
                    .and_then(|path| crate::web::quota::read(path, model));
                if let Some(quota) = &quota {
                    let _ = chat::set_quota(&db, &answer_id, quota);
                }
                send_terminal(
                    &sender,
                    "done",
                    serde_json::json!({
                        "id": answer_id,
                        "text": turn.text,
                        "reasoning": turn.reasoning,
                        "model": model,
                        "prompt_tokens": accounting.prompt_tokens,
                        "completion_tokens": accounting.completion_tokens,
                        "cached_tokens": accounting.cached_tokens,
                        "duration_ms": accounting.duration_ms,
                        "rounds": accounting.rounds,
                        "quota": quota.as_ref().map(crate::web::quota::Quota::line),
                    }),
                );
                // Rename only after the operator already has the answer: a title
                // is a nicety and must never sit between them and the reply.
                if let Some(config) = &title_config {
                    let suggested = chat::suggest_title(config, &question, &turn.text);
                    let renamed = suggested.as_deref().and_then(|title| {
                        chat::set_title(&db, &conversation, &question, title)
                            .ok()
                            .map(|rows| (title, rows))
                    });
                    // Recorded either way: a role that never lands is exactly the
                    // kind of quiet no-op this platform refuses to ship.
                    let detail = match &renamed {
                        Some((title, rows)) => format!("title=`{title}`; rows={rows}"),
                        None => "no title suggested".to_owned(),
                    };
                    let _ = title_audit.audit("loopback", "chat.title", Some(&detail));
                }
                send_stranded(&sender, &stranded);
            }
            Err((code, message)) => {
                // A limit is the one failure the operator can act on by waiting,
                // so it says how long. The row keeps the reason in its body when
                // nothing streamed, so a reload shows the same sentence instead of
                // an empty failed bubble.
                let wait = (code == "rate_limit")
                    .then(|| core_runtime::provider::classify::reset_after_secs(&message))
                    .flatten();
                let quota = quota_db
                    .as_deref()
                    .and_then(|path| crate::web::quota::read(path, &config.model));
                let resets = wait
                    .or_else(|| quota.as_ref().and_then(|quota| quota.resets_in_secs))
                    .map(crate::web::quota::human_wait);
                let shown = match (&code[..], &resets) {
                    ("rate_limit", Some(wait)) => {
                        format!(
                            "Rate or usage limit on {} - resets in {wait}. {message}",
                            config.model
                        )
                    }
                    ("rate_limit", None) => {
                        format!("Rate or usage limit on {}. {message}", config.model)
                    }
                    _ => message.clone(),
                };
                let body = if streamed.trim().is_empty() {
                    shown.clone()
                } else {
                    format!("{streamed}\n\n[{shown}]")
                };
                let _ = chat::fail(&db, &answer_id, &code, &body);
                if let Some(quota) = &quota {
                    let _ = chat::set_quota(&db, &answer_id, quota);
                }
                let _ = title_audit.audit(
                    "loopback",
                    "chat.failed",
                    Some(&format!("code={code}; model={}", config.model)),
                );
                send_terminal(
                    &sender,
                    "error",
                    serde_json::json!({
                        "code": code,
                        "message": shown,
                        "resets_in": resets,
                        "quota": quota.as_ref().map(crate::web::quota::Quota::line),
                    }),
                );
                send_stranded(&sender, &stranded);
            }
        }
    })
}

/// Tell the page about a steer that lost the race with the end of its turn.
///
/// The loop checks for a steer at its last boundary and then writes the answer, so
/// a message sent in between has no round left to join. That is microseconds, and
/// it is still reported: an operator who typed something is owed the fact that it
/// did not land, rather than a message that quietly disappears.
fn send_stranded(
    sender: &Option<tokio::sync::mpsc::UnboundedSender<Result<SseEvent, Infallible>>>,
    stranded: &[String],
) {
    if stranded.is_empty() {
        return;
    }
    let message = if stranded.len() == 1 {
        "A steering message arrived after the turn had finished, so nothing applied it. \
         Send it again and it starts a turn of its own."
            .to_owned()
    } else {
        format!(
            "{} steering messages arrived after the turn had finished, so nothing applied \
             them. Send them again and they start a turn of their own.",
            stranded.len()
        )
    };
    send_terminal(
        sender,
        "notice",
        serde_json::json!({ "code": "steer_not_applied", "message": message }),
    );
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

/// The `subagent` frame a live page recounts the bar from.
///
/// Three fields, because three is what the bar needs: the id to match the row,
/// the label to show before a reload, and the status to count. The result is not
/// in the frame: it is in the row, and a live page that wanted it would have to
/// re-render the control anyway, which is what the next page load does.
pub(crate) fn subagent_frame(report: &crate::subagents::Report<'_>) -> serde_json::Value {
    serde_json::json!({
        "id": report.id,
        "label": report.label,
        "status": report.status,
    })
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
