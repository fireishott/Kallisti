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

use axum::body::Body;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};

use crate::admin::{
    AppState, ROUTE_ACTIVITY, ROUTE_DASHBOARD, ROUTE_LOGIN, ROUTE_SETTINGS,
    ROUTE_SETTINGS_PERMISSIONS, ROUTE_SETTINGS_PROVIDER, ROUTE_SETTINGS_WORKSPACE, VERSION,
};
use crate::auth::{AuthStore, PairingService};
use crate::error::{ApiError, RequestId};
use crate::onboarding::{
    OnboardingError, PermissionError, Permissions, SETTING_MODEL, SETTING_PROVIDER_BASE_URL,
    SETTING_PROVIDER_KEY_REF, SETTING_PROVIDER_PROBED_AT, SETTING_WORKSPACE, Step, Wizard, session,
    workspace,
};
use crate::web::store;
use crate::web::templates::{
    ActivityView, DashboardView, DeviceRow, FormError, SettingsGroup, SettingsView, SetupView,
    activity_page, dashboard_page, login_page, settings_page, setup_page,
};

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
        .route(ROUTE_ACTIVITY, get(activity))
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
    let bytes = axum::body::to_bytes(body, MAX_BODY_BYTES)
        .await
        .map_err(|_| {
            FormError::new(
                None,
                "body_too_large",
                format!("the form body must be under {MAX_BODY_BYTES} bytes"),
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
    view.allow_full = state.allow_full;
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
    Ok(view)
}

/// Assemble the activity view: the newest hundred audit rows.
fn build_activity_view(state: &AppState) -> Result<ActivityView, FormError> {
    let engine = &state.engine;
    let mut view = ActivityView::new(engine.state().map_err(|error| form_error(&error))?);
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
        Ok(state_now) => page(login_page(state_now, None)),
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
    headers: HeaderMap,
    body: Body,
) -> Response {
    let as_html = wants_html(&headers);

    // Rate limit before parsing, exactly as the claim form does, so a malformed
    // flood costs an attempt too and the budget cannot be reset by garbage.
    if !state.limiter.allow() {
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
    let cookie = session::set_cookie_header(&minted.cookie_value);
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
        Ok(state_now) => page_with_status(login_page(state_now, Some(&error)), status),
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
    headers: HeaderMap,
    body: Body,
) -> Response {
    let as_html = wants_html(&headers);

    // The wizard is locked once the daemon is ready, before anything is read.
    if let Err(error) = state.engine.ensure_wizard_open() {
        return fail(&state, as_html, form_error(&error));
    }

    // Rate limit before parsing, so a malformed flood costs an attempt too.
    if !state.limiter.allow() {
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
    let cookie = session::set_cookie_header(&minted.cookie_value);
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
            if as_html {
                render_dashboard_ok(&state, format!("probe ok: {} models", catalog.models.len()))
            } else {
                ok_json(serde_json::json!({
                    "ok": true,
                    "base_url": base_url,
                    "model_count": catalog.models.len(),
                    "models": catalog.models,
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
        state.allow_full,
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
        state.allow_full,
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
