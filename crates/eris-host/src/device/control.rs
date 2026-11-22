//! The `/v1` control routes: models, schedules, skills and auxiliary roles.
//!
//! These four are settings-shaped reads and writes that the legacy relay served
//! on `/v1/models`, `/v1/cron`, `/v1/skills` and `/v1/aux`. A paired phone asks
//! for all four; the device listener answered 404 for every one of them, which is
//! what made a phone that could pair look like a phone that could not be
//! configured.
//!
//! Nothing here is a new engine. Every write calls the same store function the
//! dashboard's settings page calls, so a phone and a browser cannot disagree
//! about what a model pin or a schedule is. What this module adds is the wire.
//!
//! Rules carried over from [`super::handlers`]:
//!
//! - every route authenticates the bearer and then checks a scope, in that order,
//!   and a device paired without the administration box is refused by name rather
//!   than with a bare "forbidden";
//! - nothing returns a credential. A provider is an id, a name and whether it is
//!   primary. The `env:NAME` key reference and the base URL stay on the host;
//! - a schema read of a channel that does not exist is not a new resource. An
//!   unreadable skill library is a `200` carrying `library_error`, not a `503`:
//!   "the directory is not there" is a state an operator fixes from this screen,
//!   and a client that got an error instead of a body could not tell it from a
//!   daemon that is down.
//!
//! One deliberate difference from `/v1/settings`: that route writes a subset of
//! four closed-set values, and these write rows. So each of these validates every
//! value it was handed before it stores any of them, and a refusal names the field
//! and what it would have accepted.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};

use crate::auth::Scope;
use crate::aux;
use crate::device::{DeviceError, DeviceService};
use crate::onboarding::SetupEngine;
use crate::providers::Provider;
use crate::scheduler::{self, Schedule};
use crate::skills;
use crate::web::{chat, store};

use super::handlers::{
    authenticate, field, model_write_refusal, read_fields, refuse_value, require_admin,
    require_scope, resolve_model_value, settings_unavailable,
};

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

/// The audit action a phone's model write records.
///
/// The dashboard writes `settings.models`. A device write carries its own
/// spelling so the Activity page can say which surface changed a row, which is
/// the difference a phone needs in order to be visible in the trail at all.
const AUDIT_MODELS: &str = "settings.device_models";
/// See [`AUDIT_MODELS`].
const AUDIT_SCHEDULES: &str = "settings.device_schedules";

/// The audit action a workboard change writes.
const AUDIT_WORKBOARD: &str = "workboard.changed";
/// See [`AUDIT_MODELS`].
const AUDIT_AUX: &str = "settings.device_aux";
/// See [`AUDIT_MODELS`]. One action for both skill writes, because the trail
/// line already says which one happened.
const AUDIT_SKILLS: &str = "settings.device_skills";

/// The fields a `PATCH /v1/cron/{id}` body may carry, for a refusal that names
/// them.
pub const CRON_PATCH_FIELDS: [&str; 4] = ["enabled", "name", "schedule", "prompt"];

/// A truthy value, as a form or JSON body spells one.
fn parse_flag(field: &'static str, raw: &str) -> Result<bool, DeviceError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "on" | "yes" => Ok(true),
        "false" | "0" | "off" | "no" => Ok(false),
        other => Err(refuse_value(field, "flag_invalid", other, "true or false")),
    }
}

/// Append one audit row, or report that the trail could not be extended.
///
/// A write that landed with no audit row is the state the trail exists to
/// prevent, so a failure here is surfaced rather than swallowed.
fn audit(
    service: &DeviceService,
    device_id: &str,
    action: &str,
    detail: &str,
) -> Result<(), DeviceError> {
    service
        .engine()
        .auth()
        .audit(device_id, action, Some(detail))
        .map_err(|error| {
            DeviceError::db_unavailable(format!(
                "the change was written but the `{action}` audit row could not be appended: {error}"
            ))
        })
}

/// The provider list as a client reads it: id, name, and which one is primary.
///
/// The key reference and the base URL are deliberately absent: a settings read
/// somebody glances at over a shoulder should not name the endpoint, and a
/// reference travels further than a value would.
fn providers_json(registry: &[Provider], primary: Option<&str>) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = registry
        .iter()
        .map(|provider| {
            serde_json::json!({
                "id": provider.id,
                "name": provider.name,
                "primary": Some(provider.id.as_str()) == primary,
            })
        })
        .collect();
    serde_json::Value::Array(rows)
}

/// The registry and the id of the provider a turn actually runs on.
///
/// A primary id that is no longer in the registry resolves to the first entry
/// rather than to nothing, and the resolved one is what is reported: the id a
/// client shows should be the provider that would answer, not the one that was
/// typed once and has since been removed.
pub(crate) fn registry(
    engine: &SetupEngine,
) -> Result<(Vec<Provider>, Option<String>), DeviceError> {
    let providers = chat::provider_registry(engine).map_err(settings_unavailable)?;
    let stored = engine
        .setting(crate::providers::SETTING_PRIMARY)
        .map_err(settings_unavailable)?
        .unwrap_or_default();
    let resolved = crate::providers::resolve(&providers, &stored).map(|p| p.id.clone());
    Ok((providers, resolved))
}

// ---------------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------------

/// `GET /v1/models` - the catalog the last probe published, and the pin.
///
/// Behind the bearer and the `admin` scope: pinning the model is a settings write
/// on this daemon, so reading what the pin would accept is the same class of read
/// as reading the settings.
///
/// The catalog comes from `provider.models`, which the onboarding probe writes. It
/// is the probed provider's list, and each row says which provider that is. This
/// daemon does not probe every provider in the registry, so claiming a model from
/// a provider that was never probed would be a picker offering something the
/// endpoint never published.
pub async fn models(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    match read_models(service.engine()) {
        Ok(body) => axum::Json(body).into_response(),
        Err(error) => error.into_response(),
    }
}

/// The catalogue, for the RPC shim.
///
/// The shim serves the same answer as the route, through the same function: two
/// code paths for one method is the proliferation WP-13 exists to prevent.
///
/// # Errors
///
/// The settings or registry failure the route would have reported.
pub fn read_models_for_rpc(engine: &SetupEngine) -> Result<serde_json::Value, DeviceError> {
    read_models(engine)
}

/// The snapshot `GET /v1/models` answers with.
pub(crate) fn read_models(engine: &SetupEngine) -> Result<serde_json::Value, DeviceError> {
    let pinned = engine
        .model()
        .map_err(settings_unavailable)?
        .unwrap_or_default();
    let (providers, primary) = registry(engine)?;
    // The pin as a provider and a model, so a row is ticked by identity rather
    // than by string. A pin is stored `provider/model` while a row's `id` is the
    // bare id its own provider spells, so comparing the two strings could never
    // match and no row was ever drawn as the selected one.
    let pinned_choice = if pinned.trim().is_empty() {
        None
    } else {
        Some(chat::split_model_value(
            &pinned,
            &providers,
            primary.as_deref().unwrap_or_default(),
        ))
    };
    // Every provider the registry holds that a probe has actually published a
    // catalogue for, not only the primary.
    //
    // This route reported the primary's catalogue alone, which was right while
    // the daemon had one provider and became a lie the moment the registry could
    // hold six: a phone's picker showed one provider's models and called the
    // other five absent. Each catalogue here was written by a probe that ran
    // against that provider, so a row is still a statement that somebody measured
    // the model rather than a claim about an endpoint nobody has tried. A
    // provider with no published catalogue contributes nothing, which is the same
    // distinction the `probed` flag draws for the whole answer.
    let mut rows: Vec<serde_json::Value> = Vec::new();
    for provider in &providers {
        let catalog = engine
            .provider_catalog(&provider.id)
            .map_err(settings_unavailable)?;
        if catalog.is_empty() {
            continue;
        }
        let contexts = engine
            .provider_contexts_for(&provider.id)
            .map_err(settings_unavailable)?;
        for id in &catalog {
            rows.push(serde_json::json!({
                "id": id,
                "provider": provider.id,
                "context_window": contexts.get(id),
                // The pin this daemon holds. A picker can draw its tick from
                // this without comparing ids itself.
                "is_default": pinned_choice
                    .as_ref()
                    .is_some_and(|choice| choice.provider_id == provider.id && &choice.model == id),
            }));
        }
    }
    Ok(serde_json::json!({
        "ok": true,
        "model": pinned,
        // `false` means no probe has published a list, which is a different
        // state from a probe that published an empty one. A client that showed
        // "no models" for both would send an operator looking in the wrong place.
        "probed": !rows.is_empty(),
        "providers": providers_json(&providers, primary.as_deref()),
        "models": rows,
    }))
}

/// `POST /v1/models` - pin the default model.
///
/// The body is `{"model": "provider/name"}`. It is checked with the same
/// [`SetupEngine::check_model`] the wizard and the settings page use, so an id the
/// last probe never published is refused here for the reason it is refused there:
/// it would never answer.
pub async fn set_model(
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
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let Some(raw) = fields.get("model") else {
        return DeviceError::bad_request(
            "model_missing",
            "the body named no `model`: a pin needs the id to pin".to_owned(),
        )
        .into_response();
    };
    let engine = service.engine();
    // The value is checked against the catalogue of the provider that would
    // serve it, and only then stored. Storing through `set_model` re-checked it
    // against the legacy probe's list, which on an install whose primary has
    // moved is a different provider's catalogue: a correct pin was refused
    // there, and the refusal was reported as a database outage.
    let model = match resolve_model_value(engine, raw) {
        Ok(model) => model,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = engine.store_model(&model) {
        return model_write_refusal(error).into_response();
    }
    // The default moves for new chats, which is what this row has always meant.
    // The pin is what makes the choice belong to the chat the operator is in: the
    // next turn takes it, records it on its conversation, and clears it, so two
    // chats on two providers stop fighting over one row.
    if let Err(error) = store::settings_put(
        service.engine().db(),
        crate::device::turns::SETTING_PENDING_MODEL,
        &model,
    ) {
        return DeviceError::db_unavailable(format!(
            "the default model was stored but the pin for this chat could not be: {error}"
        ))
        .into_response();
    }
    if let Err(error) = audit(
        &service,
        token.device_id(),
        AUDIT_MODELS,
        &format!("model={model}"),
    ) {
        return error.into_response();
    }
    axum::Json(serde_json::json!({ "ok": true, "model": model })).into_response()
}

// ---------------------------------------------------------------------------
// Schedules
// ---------------------------------------------------------------------------

/// One schedule as the wire carries it.
///
/// The stamps are the RFC 3339 UTC strings the database already holds, passed
/// through rather than reformatted: the client knows which clock it is drawing
/// in, and a daemon that formatted for it would be guessing at a locale it does
/// not own. `created_at` and `updated_at` are on the row but not here; the list
/// row draws neither, and a field nothing reads is a field nobody notices going
/// wrong.
fn schedule_json(row: &Schedule) -> serde_json::Value {
    serde_json::json!({
        "id": row.id,
        "name": row.name,
        // `schedule` is the field name the client's row already uses for the
        // five-field expression, so the wire matches the model rather than the
        // other way round.
        "schedule": row.cron,
        "prompt": row.prompt,
        "enabled": row.enabled,
        "next_run": row.next_fire_at,
        "last_run": row.last_fired_at,
        "last_status": row.last_status,
        "last_error": row.last_error,
        "fired_count": row.fired_count,
        // What happens to the answer. A client that cannot see this cannot tell a
        // schedule that pings the phone from one that writes and stays quiet.
        "deliver": row.deliver,
    })
}

/// The offset this daemon's schedules are read and written in.
fn schedule_offset(engine: &SetupEngine) -> time::UtcOffset {
    let zone = crate::zone::Zone::parse(
        engine
            .setting(crate::zone::SETTING_TIMEZONE)
            .ok()
            .flatten()
            .as_deref(),
    );
    scheduler::local_offset(engine.db(), &zone.modifier())
}

/// The stored zone's name, so a client that shows "next" can say which clock.
fn schedule_zone(engine: &SetupEngine) -> String {
    crate::zone::Zone::parse(
        engine
            .setting(crate::zone::SETTING_TIMEZONE)
            .ok()
            .flatten()
            .as_deref(),
    )
    .as_str()
}

/// The scheduler's failure, as a refusal that names the field it belongs to.
fn schedule_failure(error: scheduler::ScheduleError, field: &'static str) -> DeviceError {
    match error {
        scheduler::ScheduleError::Missing => DeviceError::new(
            axum::http::StatusCode::NOT_FOUND,
            "schedule_unknown",
            "no schedule with that id exists",
        ),
        scheduler::ScheduleError::Cron(message) | scheduler::ScheduleError::Text(message) => {
            DeviceError::bad_request("schedule_invalid", format!("{field}: {message}"))
        }
        scheduler::ScheduleError::Db(error) => {
            DeviceError::db_unavailable(format!("cannot reach the schedules: {error}"))
        }
    }
}

/// The schedule a body just wrote, or the reason it cannot be read back.
///
/// A row that was written and cannot be read is reported rather than answered with
/// the values that were submitted: the client would otherwise draw a row this
/// daemon may not actually hold.
fn read_back(engine: &SetupEngine, id: &str) -> Result<serde_json::Value, DeviceError> {
    match scheduler::get(engine.db(), id) {
        Ok(Some(row)) => Ok(serde_json::json!({ "ok": true, "job": schedule_json(&row) })),
        Ok(None) => Err(DeviceError::new(
            axum::http::StatusCode::NOT_FOUND,
            "schedule_unknown",
            "no schedule with that id exists",
        )),
        Err(error) => Err(DeviceError::db_unavailable(format!(
            "the schedule was written and could not be read back: {error}"
        ))),
    }
}

/// `GET /v1/cron` - the schedules, and whether the ticker runs at all.
///
/// The `enabled` flag is the global switch, and it is on the same read as the rows
/// on purpose: a list of schedules with the ticker off explains a client's "why
/// did nothing fire" without a second request.
pub async fn cron(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    let engine = service.engine();
    let rows = match scheduler::list(engine.db()) {
        Ok(rows) => rows,
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot read the schedules: {error}"))
                .into_response();
        }
    };
    let enabled = scheduler::enabled(
        engine
            .setting(scheduler::SETTING_ENABLED)
            .ok()
            .flatten()
            .as_deref(),
    );
    let jobs: Vec<serde_json::Value> = rows.iter().map(schedule_json).collect();
    axum::Json(serde_json::json!({
        "ok": true,
        "enabled": enabled,
        "timezone": schedule_zone(engine),
        "count": jobs.len(),
        "jobs": jobs,
    }))
    .into_response()
}

/// `POST /v1/cron` - create one schedule.
///
/// The body is `{"name": ..., "schedule": ..., "prompt": ...}`. The expression is
/// parsed before the row is written, by the same validation the form uses, so a
/// schedule that would never fire is refused while somebody is looking at it.
pub async fn create_cron(
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
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let name = fields.get("name").cloned().unwrap_or_default();
    let expression = fields.get("schedule").cloned().unwrap_or_default();
    let prompt = fields.get("prompt").cloned().unwrap_or_default();
    let engine = service.engine();
    let offset = schedule_offset(engine);
    let id = match scheduler::create(engine.db(), &name, &expression, &prompt, offset) {
        Ok(id) => id,
        Err(error) => return schedule_failure(error, "schedule").into_response(),
    };
    // The channel rides on the create when the body names one; otherwise the
    // column's default (`push`) stands.
    if let Some(channel) = fields.get("deliver")
        && let Err(error) = scheduler::set_deliver(engine.db(), &id, channel)
    {
        return schedule_failure(error, "schedule").into_response();
    }
    if let Err(error) = audit(
        &service,
        token.device_id(),
        AUDIT_SCHEDULES,
        &format!("create id={id}"),
    ) {
        return error.into_response();
    }
    match read_back(engine, &id) {
        Ok(body) => axum::Json(body).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `PATCH /v1/cron/{id}` - turn one schedule on or off, or edit its content.
///
/// Two shapes, told apart by the fields present. `{"enabled": true}` is the switch
/// the list row draws, and it recomputes the next fire so a schedule armed over
/// its own slot waits for the next one. `{"name", "schedule", "prompt"}` is the
/// editor, and all three are required together: a partial edit has no single
/// description, and storing one would make the row's own last-write story
/// unreadable.
pub async fn patch_cron(
    State(service): State<DeviceService>,
    Path(id): Path<String>,
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
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let engine = service.engine();
    let offset = schedule_offset(engine);

    if let Some(raw) = fields.get("enabled") {
        let on = match parse_flag("enabled", raw) {
            Ok(on) => on,
            Err(error) => return error.into_response(),
        };
        if let Err(error) = scheduler::set_enabled(engine.db(), &id, on, offset) {
            return schedule_failure(error, "enabled").into_response();
        }
        if let Err(error) = audit(
            &service,
            token.device_id(),
            AUDIT_SCHEDULES,
            &format!("toggle id={id} on={on}"),
        ) {
            return error.into_response();
        }
    } else if let Some(channel) = fields
        .get("deliver")
        .map(|value| value.trim().to_lowercase())
    {
        // The delivery picker on the cron page and on the phone sends one field:
        // where the answer goes. It is a closed set and refused by name.
        if !crate::delivery::valid(&channel) {
            return DeviceError::bad_request(
                "schedule_patch_invalid",
                format!(
                    "`{channel}` is not a delivery channel; the daemon has {}",
                    crate::delivery::DEFAULT_VALUES.join(", ")
                ),
            )
            .into_response();
        }
        if let Err(error) = scheduler::set_deliver(engine.db(), &id, &channel) {
            return schedule_failure(error, "deliver").into_response();
        }
        if let Err(error) = audit(
            &service,
            token.device_id(),
            AUDIT_SCHEDULES,
            &format!("deliver id={id} channel={channel}"),
        ) {
            return error.into_response();
        }
    } else if fields.contains_key("name")
        || fields.contains_key("schedule")
        || fields.contains_key("prompt")
    {
        if let Some(missing) = ["name", "schedule", "prompt"]
            .into_iter()
            .find(|key| !fields.contains_key(*key))
        {
            return DeviceError::bad_request(
                "schedule_edit_incomplete",
                format!(
                    "editing a schedule rewrites all three of its fields, and the body named no \
                     `{missing}`"
                ),
            )
            .into_response();
        }
        let name = fields.get("name").cloned().unwrap_or_default();
        let expression = fields.get("schedule").cloned().unwrap_or_default();
        let prompt = fields.get("prompt").cloned().unwrap_or_default();
        if let Err(error) = scheduler::update(engine.db(), &id, &name, &expression, &prompt, offset)
        {
            return schedule_failure(error, "schedule").into_response();
        }
        if let Err(error) = audit(
            &service,
            token.device_id(),
            AUDIT_SCHEDULES,
            &format!("edit id={id}"),
        ) {
            return error.into_response();
        }
    } else {
        return DeviceError::bad_request(
            "schedule_patch_empty",
            format!(
                "the body named none of {}: nothing was changed",
                CRON_PATCH_FIELDS.join(", ")
            ),
        )
        .into_response();
    }

    match read_back(engine, &id) {
        Ok(body) => axum::Json(body).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `DELETE /v1/cron/{id}` - delete one schedule and its run history.
pub async fn delete_cron(
    State(service): State<DeviceService>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    match scheduler::delete(service.engine().db(), &id) {
        Ok(()) => {
            if let Err(error) = audit(
                &service,
                token.device_id(),
                AUDIT_SCHEDULES,
                &format!("delete id={id}"),
            ) {
                return error.into_response();
            }
            axum::Json(serde_json::json!({ "ok": true, "id": id })).into_response()
        }
        Err(error) => schedule_failure(error, "id").into_response(),
    }
}

// ---------------------------------------------------------------------------
// Skills
// ---------------------------------------------------------------------------

/// `GET /v1/skills` - the library, as the settings page reads it.
///
/// An unreadable library is a `200` carrying `library_error` rather than a `503`.
/// The directory being wrong is a state an operator fixes from this screen, so a
/// client must be able to draw the screen; a status code that reads as "the daemon
/// is broken" would hide the field that needs changing.
pub async fn skills_list(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    let engine = service.engine();
    let stored = engine.setting(skills::SETTING_DIR).ok().flatten();
    let on = skills::enabled(
        engine
            .setting(skills::SETTING_ENABLED)
            .ok()
            .flatten()
            .as_deref(),
    );
    match skills::list(engine.db(), stored.as_deref()) {
        Ok((found, dir)) => {
            let rows: Vec<serde_json::Value> = found
                .iter()
                .map(|skill| {
                    serde_json::json!({
                        "key": skill.key,
                        "name": skill.name,
                        "description": skill.description,
                        "path": skill.path,
                        "enabled": skill.enabled,
                        "bytes": skill.bytes,
                        "origin": match skill.origin {
                            crate::learning::Origin::Agent => "learned",
                            crate::learning::Origin::Operator => "operator",
                        },
                        "state": skill.state.as_str(),
                        "pinned": skill.pinned,
                        "editable": skill.editable,
                        // What the ledger actually records, so a client can tell
                        // the library apart from the part of it a prompt carries.
                        "use_count": skill.use_count,
                        "last_used": skill.last_used,
                        "archived_at": skill.archived_at,
                        "reason": skill.archived_reason,
                    })
                })
                .collect();
            axum::Json(serde_json::json!({
                "ok": true,
                "enabled": on,
                "dir": dir.display().to_string(),
                "skills": rows,
            }))
            .into_response()
        }
        Err(error) => axum::Json(serde_json::json!({
            "ok": true,
            "enabled": on,
            "dir": stored,
            "library_error": error.to_string(),
            "skills": [],
        }))
        .into_response(),
    }
}

/// What a search asked for.
///
/// Every field the tool takes is here, because the two are one query: a phone and
/// a turn that ask the same question get the same bytes.
#[derive(Debug, serde::Deserialize)]
pub struct SearchQuery {
    /// The words to look for.
    #[serde(default)]
    q: String,
    /// `all` or `conversation`; the stored default when absent.
    #[serde(default)]
    scope: Option<String>,
    /// The conversation to hold to.
    #[serde(default)]
    conversation_id: Option<String>,
    /// `all`, `user` or `assistant`.
    #[serde(default)]
    role: Option<String>,
    /// ISO 8601 lower bound.
    #[serde(default)]
    since: Option<String>,
    /// ISO 8601 upper bound.
    #[serde(default)]
    until: Option<String>,
    /// `discovery` or `browse`.
    #[serde(default)]
    mode: Option<String>,
    /// Discovery: a `cursor` from the previous page. Browse: a message id.
    #[serde(default)]
    cursor: Option<String>,
    /// How many hits; clamped by [`crate::search`] and its setting.
    #[serde(default)]
    limit: Option<usize>,
}

/// `GET /v1/search` - the transcript, searched.
///
/// The phone had no way to ask what it had already talked about; the only search
/// it could reach covered skills, notes, models and logs. This route reads the
/// same FTS5 index the turn's `search_sessions` tool reads, through the same
/// function, so what the operator types on the phone and what the assistant
/// recalls are one transcript and one answer.
///
/// The body of an empty query is a `200` with no hits, not a `400`: a search field
/// arrives empty on every screen that draws it, and telling the client that an
/// empty box is a bad request would make a normal first paint look like a
/// failure.
pub async fn search_transcript(
    State(service): State<DeviceService>,
    Query(params): Query<SearchQuery>,
    headers: HeaderMap,
) -> Response {
    // A paired device reading the transcript it already chats in is an ordinary
    // assistant capability, not a settings change. Requiring the administration
    // scope here made search dead on every phone paired without it, which is every
    // phone this operator has.
    if let Err(error) = authenticate(&service, &headers) {
        return error.into_response();
    }
    let db = service.engine().db();
    let text = params.q.trim().to_owned();
    let mut request = crate::search::Request {
        query: text.clone(),
        scope: params.scope.unwrap_or_default(),
        conversation_id: params
            .conversation_id
            .filter(|value| !value.trim().is_empty()),
        role: params.role.unwrap_or_else(|| "all".to_owned()),
        since: params.since.filter(|value| !value.is_empty()),
        until: params.until.filter(|value| !value.is_empty()),
        mode: params
            .mode
            .as_deref()
            .map(crate::search::Mode::parse)
            .unwrap_or(crate::search::Mode::Discovery),
        limit: params.limit.unwrap_or(0),
        cursor: params.cursor.filter(|value| !value.is_empty()),
    };
    if request.mode == crate::search::Mode::Discovery && request.conversation_id.is_some() {
        request.mode = crate::search::Mode::Browse;
    }
    crate::search::apply_defaults(db, &mut request);
    if text.is_empty() && request.conversation_id.is_none() {
        return axum::Json(serde_json::json!({
            "hits": [],
            "total": 0,
            "cursor": null,
            "took_ms": 0,
        }))
        .into_response();
    }
    match crate::search::answer(db, &request) {
        // `search::answer` is the seam the tool writes its `ToolResult` from, so
        // the phone and the assistant cannot answer the same question differently.
        Ok(body) => (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            body.to_string(),
        )
            .into_response(),
        Err(error) => DeviceError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "search_failed",
            format!("the transcript could not be searched: {error}"),
        )
        .into_response(),
    }
}

/// `GET /v1/voices` - the voice list, from the daemon rather than the client.
///
/// Published even with speech off: a phone draws its picker before it knows
/// whether speaking works, and an empty list would look like a missing feature
/// rather than a switch.
pub async fn voices(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    if let Err(error) = authenticate(&service, &headers) {
        return error.into_response();
    }
    let db = service.engine().db();
    let voices: Vec<serde_json::Value> = crate::speech::VOICES
        .iter()
        .map(|(key, label)| serde_json::json!({"id": key, "label": label}))
        .collect();
    axum::Json(serde_json::json!({
        "ok": true,
        "enabled": crate::speech::enabled(db),
        "voice": crate::speech::voice(db),
        "voices": voices,
    }))
    .into_response()
}

/// `GET /v1/speech` - the speech model in force, the voices, and the models this
/// daemon already has provisioned.
///
/// The list is the provider catalogue the daemon was set up with, because the
/// phone should not have to guess a model name or hold a second cloud key. A
/// model outside the catalogue is still accepted on a `PUT`: the catalogue is
/// what was probed, not a closed set, and an audio model is usually absent from
/// it.
///
/// `chat_base_url` and `chat_model` are the two rows the chat-audio engine reads,
/// reported as stored and `null` when unset. `voice_models` reports the model
/// that engine would actually ask for, not only the stored row, so a client's
/// voice picker is never empty for an engine that can speak.
pub async fn speech_get(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let db = service.engine().db();
    let models: Vec<String> = crate::web::store::settings_get(db, "provider.models")
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .unwrap_or_default();
    // The provider is read once: it decides whether the chat engine is
    // configured, and its model is the model that engine falls back to.
    let provider = crate::web::chat::turn_config(service.engine())
        .ok()
        .flatten();
    // The engine decides what "configured" means. The classic engine is the
    // stored base URL; the chat engine is its own override or the provider base
    // URL it falls back to. A chat model alone names no endpoint, so it does not
    // count as configured.
    let engine = crate::speech::engine(db);
    let configured = match engine {
        crate::speech::Engine::AudioEndpoint => crate::speech::engine_base(db).is_some(),
        crate::speech::Engine::ChatAudio => {
            crate::speech::chat_base_url(db).is_some() || provider.is_some()
        }
    };
    let voice_models =
        crate::speech::voice_models(db, provider.as_ref().map(|config| config.model.as_str()));
    axum::Json(serde_json::json!({
        "ok": true,
        "engine": engine.as_str(),
        "engines": crate::speech::ENGINES
            .iter()
            .map(|(id, label)| serde_json::json!({ "id": id, "label": label }))
            .collect::<Vec<_>>(),
        "model": crate::speech::model(db),
        "default_model": crate::speech::MODEL,
        "models": models,
        "chat_base_url": crate::speech::chat_base_url(db),
        "chat_model": crate::speech::chat_model(db),
        "voices": crate::speech::VOICES
            .iter()
            .map(|(id, label)| serde_json::json!({ "id": id, "label": label }))
            .collect::<Vec<_>>(),
        "voice": crate::speech::voice(db),
        "voice_models": voice_models,
        "configured": configured,
    }))
    .into_response()
}

/// `PUT /v1/speech/engine` - choose which engine speaks and transcribes.
///
/// Administration, because the value decides which endpoint the daemon calls
/// and with which credentials. A value outside the two the daemon knows is
/// refused by name rather than stored and discovered on the first answer.
pub async fn speech_engine_put(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Admin) {
        return error.into_response();
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let raw = field(&fields, "engine");
    let Some(engine) = crate::speech::Engine::parse(raw) else {
        return DeviceError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "speech_engine_invalid",
            "engine must be audio-endpoint or chat-audio",
        )
        .into_response();
    };
    if let Err(error) = crate::web::store::settings_put(
        service.engine().db(),
        crate::speech::SETTING_ENGINE,
        engine.as_str(),
    ) {
        return DeviceError::db_unavailable(format!("cannot store the speech engine: {error}"))
            .into_response();
    }
    let _ = audit(&service, token.device_id(), "tts.engine", engine.as_str());
    axum::Json(serde_json::json!({ "ok": true, "engine": engine.as_str() })).into_response()
}

/// `PUT /v1/speech/chat` - point the chat-audio engine at an aggregation endpoint
/// and a model.
///
/// Both keys are optional and are read independently. A key the body does not
/// carry leaves its row untouched; a key carried as `null`, as an empty string,
/// or as whitespace clears it. Clearing is a delete rather than an empty row, so
/// a read of `/v1/speech` answers the same `null` a never-written row does.
///
/// Administration, because the value decides which endpoint the daemon calls and
/// with which credentials. Every value is checked before anything is stored, so a
/// refused body cannot leave half a change behind.
pub async fn speech_chat_put(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Admin) {
        return error.into_response();
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };

    // `None` is a key the body did not carry; `Some(None)` clears the row;
    // `Some(Some(value))` stores the trimmed value.
    let base_url: Option<Option<String>> = match fields.get("base_url") {
        None => None,
        Some(raw) => {
            let value = raw.trim();
            if value.is_empty() {
                Some(None)
            } else if crate::speech::valid_chat_base_url(value) {
                Some(Some(value.to_owned()))
            } else {
                return DeviceError::new(
                    axum::http::StatusCode::BAD_REQUEST,
                    "speech_base_url_invalid",
                    "base_url must be an http:// or https:// URL",
                )
                .into_response();
            }
        }
    };
    let model: Option<Option<String>> = match fields.get("model") {
        None => None,
        Some(raw) => {
            let value = raw.trim();
            if value.is_empty() {
                Some(None)
            } else if value.chars().count() > crate::speech::MAX_CHAT_MODEL_CHARS {
                return DeviceError::new(
                    axum::http::StatusCode::BAD_REQUEST,
                    "speech_model_invalid",
                    format!(
                        "model must be at most {} characters",
                        crate::speech::MAX_CHAT_MODEL_CHARS
                    ),
                )
                .into_response();
            } else {
                Some(Some(value.to_owned()))
            }
        }
    };

    let db = service.engine().db();
    let mut changes: Vec<String> = Vec::new();
    for (key, name, change) in [
        (crate::speech::SETTING_CHAT_BASE_URL, "base_url", base_url),
        (crate::speech::SETTING_CHAT_MODEL, "model", model),
    ] {
        let Some(change) = change else {
            continue;
        };
        let outcome: Result<String, String> = match change {
            Some(value) => crate::web::store::settings_put(db, key, &value)
                .map(|()| format!("{name} stored"))
                .map_err(|error| error.to_string()),
            None => service
                .engine()
                .clear_setting(key)
                .map(|()| format!("{name} cleared"))
                .map_err(|error| error.to_string()),
        };
        match outcome {
            Ok(change) => changes.push(change),
            Err(error) => {
                return DeviceError::db_unavailable(format!(
                    "cannot store the speech {name}: {error}"
                ))
                .into_response();
            }
        }
    }
    let detail = if changes.is_empty() {
        "no change".to_owned()
    } else {
        changes.join(", ")
    };
    let _ = audit(&service, token.device_id(), "tts.chat", &detail);
    axum::Json(serde_json::json!({
        "ok": true,
        "base_url": crate::speech::chat_base_url(db),
        "model": crate::speech::chat_model(db),
    }))
    .into_response()
}

/// `PUT /v1/speech` - choose the model the daemon speaks with.
///
/// Administration, because it points a provider call at a model of the
/// operator's choosing.
pub async fn speech_put(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Admin) {
        return error.into_response();
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let model = field(&fields, "model").trim().to_owned();
    if model.is_empty() {
        return DeviceError::new(
            axum::http::StatusCode::BAD_REQUEST,
            "speech_model_empty",
            "a speech model name is required",
        )
        .into_response();
    }
    if let Err(error) = crate::web::store::settings_put(
        service.engine().db(),
        &crate::aux::setting_key("speech"),
        &model,
    ) {
        return DeviceError::db_unavailable(format!("cannot store the speech model: {error}"))
            .into_response();
    }
    let _ = audit(&service, token.device_id(), "tts.model", &model);
    axum::Json(serde_json::json!({ "ok": true, "model": model })).into_response()
}

/// The audit action a routing write records.
///
/// One action for the whole write, with the detail naming each row that moved:
/// the trail answers "who turned routing on, and where did they point it", which
/// is one question and not five.
const AUDIT_ROUTING: &str = "routing.write";

/// The model fields `PUT /v1/routing` accepts, and the intent each one is for.
///
/// The field names are the wire's, the rows are this daemon's. Keeping the pair
/// in one table means the body this route validates and the rows it writes cannot
/// disagree about which intent a field belongs to; a new intent is one row here
/// and one line in [`routing_json`].
const ROUTING_MODEL_FIELDS: [(&str, crate::routing::Intent); 3] = [
    ("coding_model", crate::routing::Intent::Coding),
    ("heavy_model", crate::routing::Intent::Heavy),
    ("general_model", crate::routing::Intent::General),
];

/// The routing settings as a client reads them.
///
/// `provider_model` is the model a turn would inherit with no routing row
/// pinned, reported as `null` on a daemon that has no provider configured yet.
/// Every model field is the empty string when it inherits, which is the same
/// value a client sends back to clear it.
fn routing_json(db: &crate::db::Db, provider_model: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "ok": true,
        "auto": crate::routing::auto_enabled(db),
        "coding_model": crate::routing::model_setting(db, crate::routing::Intent::Coding),
        "heavy_model": crate::routing::model_setting(db, crate::routing::Intent::Heavy),
        "general_model": crate::routing::model_setting(db, crate::routing::Intent::General),
        "harness": crate::routing::harness_setting(db),
        "harnesses": crate::routing::HARNESSES,
        "provider_model": provider_model,
    })
}

/// The model a turn would inherit, when this daemon has one.
fn routing_provider_model(engine: &SetupEngine) -> Option<String> {
    crate::web::chat::turn_config(engine)
        .ok()
        .flatten()
        .map(|config| config.model)
}

/// `GET /v1/routing` - the routing settings and what they inherit from.
///
/// A chat-scope read: a client showing a composer needs to know which model each
/// kind of work would run on, and none of these values is a credential. The
/// harness list rides along so a picker cannot offer a name this daemon refuses.
pub async fn routing_get(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let provider_model = routing_provider_model(service.engine());
    axum::Json(routing_json(
        service.engine().db(),
        provider_model.as_deref(),
    ))
    .into_response()
}

/// `PUT /v1/routing` - choose the models and the harness routing may use.
///
/// Administration, because each row decides which model a turn spends on and
/// which external tool code work may name. Every value is checked before any is
/// stored, so a refused body cannot leave half a change behind.
///
/// A key the body does not carry leaves its row untouched, and an empty model
/// string clears the row back to "inherit the daemon's model". Clearing deletes
/// the row rather than storing an empty one, so a read of `/v1/routing` answers
/// the same empty string a never-written row does.
pub async fn routing_put(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Admin) {
        return error.into_response();
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };

    let auto = match fields.get("auto") {
        None => None,
        Some(raw) => match parse_flag("auto", raw) {
            Ok(auto) => Some(auto),
            Err(error) => return error.into_response(),
        },
    };
    // `None` is a key the body did not carry; `Some(None)` clears the row;
    // `Some(Some(value))` stores the trimmed value.
    let mut models: Vec<(crate::routing::Intent, Option<String>)> = Vec::new();
    for (name, intent) in ROUTING_MODEL_FIELDS {
        let Some(raw) = fields.get(name) else {
            continue;
        };
        let value = raw.trim();
        models.push((intent, (!value.is_empty()).then(|| value.to_owned())));
    }
    // The harness is a closed set, so an unknown name is refused by name rather
    // than stored and discovered by a client that cannot act on it. An empty
    // string is not "none": none is a harness value, and clearing is spelled by
    // sending it.
    let harness = match fields.get("harness") {
        None => None,
        Some(raw) => {
            let value = raw.trim();
            if !crate::routing::valid_harness(value) {
                return DeviceError::new(
                    axum::http::StatusCode::BAD_REQUEST,
                    "routing_harness_invalid",
                    format!(
                        "harness must be one of: {}",
                        crate::routing::HARNESSES.join(", ")
                    ),
                )
                .into_response();
            }
            Some(value.to_owned())
        }
    };

    let db = service.engine().db();
    let mut changes: Vec<String> = Vec::new();
    if let Some(auto) = auto {
        let value = if auto { "true" } else { "false" };
        if let Err(error) = crate::web::store::settings_put(db, crate::routing::SETTING_AUTO, value)
        {
            return DeviceError::db_unavailable(format!(
                "cannot store whether routing is automatic: {error}"
            ))
            .into_response();
        }
        changes.push(format!("auto={value}"));
    }
    for (intent, change) in models {
        let key = intent.setting_key();
        let outcome: Result<String, String> = match change {
            Some(value) => crate::web::store::settings_put(db, key, &value)
                .map(|()| format!("{}={value}", intent.as_str()))
                .map_err(|error| error.to_string()),
            None => service
                .engine()
                .clear_setting(key)
                .map(|()| format!("{} cleared", intent.as_str()))
                .map_err(|error| error.to_string()),
        };
        match outcome {
            Ok(change) => changes.push(change),
            Err(error) => {
                return DeviceError::db_unavailable(format!(
                    "cannot store the {} routing model: {error}",
                    intent.as_str()
                ))
                .into_response();
            }
        }
    }
    if let Some(harness) = &harness {
        if let Err(error) =
            crate::web::store::settings_put(db, crate::routing::SETTING_HARNESS, harness)
        {
            return DeviceError::db_unavailable(format!("cannot store the harness: {error}"))
                .into_response();
        }
        changes.push(format!("harness={harness}"));
    }
    let detail = if changes.is_empty() {
        "no change".to_owned()
    } else {
        changes.join(", ")
    };
    if let Err(error) = audit(&service, token.device_id(), AUDIT_ROUTING, &detail) {
        return error.into_response();
    }
    let provider_model = routing_provider_model(service.engine());
    axum::Json(routing_json(db, provider_model.as_deref())).into_response()
}

/// `POST /v1/routing/preview` - what routing would do with one message.
///
/// Chat scope, because it sends nothing: it answers the same decision a turn
/// would make, from the same settings and the same pure classifier, so a client
/// can show "this will run on the coding model" before the operator commits. A
/// preview that called a provider would be a turn by another name.
pub async fn routing_preview(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let message = field(&fields, "message");
    let provider_model = routing_provider_model(service.engine());
    let route = crate::routing::route(service.engine().db(), message, provider_model.as_deref());
    let mut answer = route.json();
    if let Some(object) = answer.as_object_mut() {
        object.insert("ok".to_owned(), serde_json::Value::Bool(true));
    }
    axum::Json(answer).into_response()
}

/// `POST /v1/speak` - synthesize one answer, as audio bytes.
///
/// The same aux path vision takes: the base URL, key and model come from the
/// configured provider, so nothing new is stored and no second client exists.
pub async fn speak(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let text = field(&fields, "text").to_owned();
    let config = crate::web::chat::turn_config(service.engine())
        .ok()
        .flatten();
    match crate::speech::speak(service.engine().db(), config.as_ref(), &text).await {
        Ok((bytes, voice)) => {
            let _ = audit(
                &service,
                token.device_id(),
                "tts.spoken",
                &format!("voice={voice} bytes={}", bytes.len()),
            );
            let mut out = axum::http::HeaderMap::new();
            let content_type = if bytes.starts_with(b"RIFF") {
                "audio/wav"
            } else {
                "audio/mpeg"
            };
            out.insert(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static(content_type),
            );
            if let Ok(value) = axum::http::HeaderValue::from_str(&voice) {
                out.insert(
                    axum::http::header::HeaderName::from_static("x-tts-voice"),
                    value,
                );
            }
            (out, bytes).into_response()
        }
        Err(refusal) => DeviceError::new(
            axum::http::StatusCode::from_u16(refusal.status)
                .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
            refusal.code,
            refusal.detail.unwrap_or_else(|| {
                match refusal.code {
                    "tts_disabled" => "speech is switched off on the daemon",
                    "tts_empty" => "there is nothing to say",
                    "tts_not_configured" => {
                        "no provider is configured with a speech endpoint, so nothing was spoken"
                    }
                    _ => "the provider refused the speech request",
                }
                .to_owned()
            }),
        )
        .into_response(),
    }
}

/// `POST /v1/transcribe` - turn one clip into text.
///
/// Raw bytes, one way only, the same shape `attachments::upload` takes: the
/// phone has the clip in hand and a multipart encoder is one more thing to get
/// wrong.
pub async fn transcribe(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let bytes = match axum::body::to_bytes(body, crate::speech::MAX_TRANSCRIBE_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return DeviceError::new(
                axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                "stt_too_large",
                "one clip may be at most 32 MB",
            )
            .into_response();
        }
    };
    let config = crate::web::chat::turn_config(service.engine())
        .ok()
        .flatten();
    match crate::speech::transcribe(service.engine().db(), config.as_ref(), &bytes).await {
        Ok(heard) => {
            let chars = heard
                .get("text")
                .and_then(serde_json::Value::as_str)
                .map_or(0, |text| text.chars().count());
            let _ = audit(
                &service,
                token.device_id(),
                "stt.transcribed",
                &format!("bytes={} chars={chars}", bytes.len()),
            );
            axum::Json(heard).into_response()
        }
        Err(refusal) => DeviceError::new(
            axum::http::StatusCode::from_u16(refusal.status)
                .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
            refusal.code,
            refusal.detail.unwrap_or_else(|| {
                match refusal.code {
                    "stt_empty" => "the upload carried no audio",
                    "stt_too_large" => "the upload is past the 32 MB clip cap",
                    "stt_not_configured" => {
                        "no engine base URL is configured, so nothing was transcribed"
                    }
                    _ => "the engine refused the transcription",
                }
                .to_owned()
            }),
        )
        .into_response(),
    }
}

/// What the workboard list accepts.
#[derive(Debug, serde::Deserialize)]
pub struct WorkQuery {
    /// `open`, `doing`, `blocked` or `done`; every status when absent.
    #[serde(default)]
    status: Option<String>,
    /// How many rows, clamped here.
    #[serde(default)]
    limit: Option<usize>,
}

/// `GET /v1/workboard` - the board, in-flow first.
pub async fn workboard(
    State(service): State<DeviceService>,
    Query(params): Query<WorkQuery>,
    headers: HeaderMap,
) -> Response {
    // The board is ordinary assistant work, not administration: a phone that can
    // chat but not read its own task list is a phone with half a feature.
    if let Err(error) = authenticate(&service, &headers) {
        return error.into_response();
    }
    let status = params
        .status
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| crate::workboard::STATUSES.contains(&value.as_str()));
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    match crate::workboard::list(service.engine().db(), status.as_deref(), limit) {
        Ok(rows) => {
            let items: Vec<serde_json::Value> =
                rows.iter().map(crate::workboard::Item::json).collect();
            axum::Json(serde_json::json!({"ok": true, "items": items, "returned": items.len()}))
                .into_response()
        }
        Err(error) => DeviceError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "workboard_failed",
            format!("the board could not be read: {error}"),
        )
        .into_response(),
    }
}

/// `POST /v1/workboard` - add a task.
pub async fn create_work(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let draft = crate::workboard::Draft {
        title: field(&fields, "title").to_owned(),
        status: fields.get("status").cloned(),
        project: fields.get("project").cloned(),
        // The phone says what it is; everything it posts is an agent's row.
        owner_kind: Some("agent".to_owned()),
        due: fields.get("due").cloned(),
        source_conversation: fields.get("conversation").cloned(),
    };
    match crate::workboard::create(service.engine().db(), &draft) {
        Ok(id) => {
            let _ = audit(
                &service,
                token.device_id(),
                AUDIT_WORKBOARD,
                &format!("create id={id}"),
            );
            axum::Json(serde_json::json!({"ok": true, "id": id})).into_response()
        }
        Err(error) => DeviceError::bad_request("workboard_invalid", error).into_response(),
    }
}

/// `PATCH /v1/workboard/{id}` - move or retitle one task.
pub async fn patch_work(
    State(service): State<DeviceService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    if fields.is_empty() {
        return DeviceError::bad_request(
            "workboard_patch_empty",
            "the body named no title, status, project or due; nothing was changed",
        )
        .into_response();
    }
    let change = crate::workboard::Change {
        title: fields.get("title").cloned(),
        status: fields.get("status").cloned(),
        project: fields.get("project").cloned(),
        due: fields.get("due").cloned(),
    };
    match crate::workboard::patch(service.engine().db(), &id, &change) {
        Ok(true) => {
            let _ = audit(
                &service,
                token.device_id(),
                AUDIT_WORKBOARD,
                &format!("patch id={id}"),
            );
            match crate::workboard::get(service.engine().db(), &id) {
                Ok(Some(item)) => {
                    axum::Json(serde_json::json!({"ok": true, "item": item.json()})).into_response()
                }
                _ => axum::Json(serde_json::json!({"ok": true})).into_response(),
            }
        }
        Ok(false) => DeviceError::new(
            axum::http::StatusCode::NOT_FOUND,
            "workboard_missing",
            format!("no task `{id}` on this board"),
        )
        .into_response(),
        Err(error) => DeviceError::bad_request("workboard_invalid", error).into_response(),
    }
}

/// `DELETE /v1/workboard/{id}` - drop one task.
pub async fn delete_work(
    State(service): State<DeviceService>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    match crate::workboard::remove(service.engine().db(), &id) {
        Ok(true) => {
            let _ = audit(
                &service,
                token.device_id(),
                AUDIT_WORKBOARD,
                &format!("delete id={id}"),
            );
            axum::Json(serde_json::json!({"ok": true})).into_response()
        }
        Ok(false) => DeviceError::new(
            axum::http::StatusCode::NOT_FOUND,
            "workboard_missing",
            format!("no task `{id}` on this board"),
        )
        .into_response(),
        Err(error) => DeviceError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "workboard_failed",
            format!("the task could not be removed: {error}"),
        )
        .into_response(),
    }
}

/// `GET /v1/memory` - the assistant's two bounded note files.
///
/// Read-only. The write path is the `memory_manage` tool inside a turn, because a
/// note that changes without passing the injection scan is a note nobody checked.
pub async fn memory_files(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    if let Err(error) = authenticate(&service, &headers) {
        return error.into_response();
    }
    let engine = service.engine();
    // The mirror is attached so the record carries each entry's provenance and the
    // cap that is actually in force, not the compiled default.
    let access =
        crate::selfmemory::Access::new(&engine.paths().state_dir).with_db(engine.db().clone());
    axum::Json(serde_json::json!({
        "ok": true,
        "memory": access.snapshot(),
    }))
    .into_response()
}

/// `GET /v1/roster` - the standing roster, names and roles only.
///
/// Shallow on purpose: the standing instructions are the operator's brief to a
/// specialist, and this route answers who exists rather than what they know.
/// The rows come from the same read the dashboard makes, so the two surfaces
/// cannot disagree about who is on the roster.
pub async fn roster(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    if let Err(error) = authenticate(&service, &headers) {
        return error.into_response();
    }
    let db = service.engine().db().clone();
    match crate::roster::list(&db) {
        Ok(rows) => {
            let agents: Vec<serde_json::Value> = rows
                .iter()
                .map(|row| {
                    serde_json::json!({
                        "name": row.name,
                        "role": row.role,
                        "enabled": row.enabled,
                    })
                })
                .collect();
            axum::Json(serde_json::json!({ "ok": true, "agents": agents })).into_response()
        }
        Err(error) => {
            tracing::warn!(%error, "the roster could not be listed");
            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                axum::Json(serde_json::json!({ "ok": false, "error": "roster_unavailable" })),
            )
                .into_response()
        }
    }
}

/// `GET /v1/metrics?window=24h|7d|all&conversation=<id>` - cost and latency.
///
/// The phone's chat footer needs three numbers for the chat it is showing: the
/// model in use, the cache hit rate and the p95 latency. It reads them here rather
/// than deriving them, because a client that computes a percentile computes a
/// different one than the page. `conversation` narrows the same rows; with no
/// window named, a conversation is read over its whole life rather than the last
/// day, because a chat is not a day.
pub async fn metrics(
    State(service): State<DeviceService>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    // Read-only numbers about this operator's own turns: authenticated, not
    // administrative, for the same reason search is.
    if let Err(error) = authenticate(&service, &headers) {
        return error.into_response();
    }
    let engine = service.engine();
    let conversation = params
        .get("conversation")
        .map(String::as_str)
        .filter(|value| !value.trim().is_empty());
    let window = params
        .get("window")
        .and_then(|value| crate::metrics::Window::parse(value))
        .unwrap_or(if conversation.is_some() {
            // One chat's numbers are its own, not the last twenty-four hours of
            // it: the footer is answering "what has this chat cost me", and a
            // window would quietly answer a different question.
            crate::metrics::Window::All
        } else {
            crate::metrics::Window::Day
        });
    let summary = match crate::metrics::summary_in(engine.db(), window, conversation) {
        Ok(summary) => summary,
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot read the transcript: {error}"))
                .into_response();
        }
    };
    let rows: Vec<serde_json::Value> = summary
        .rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "model": row.model,
                "calls": row.calls,
                "failed": row.failed,
                "median_ms": row.median_ms,
                "p95_ms": row.p95_ms,
                "prompt_tokens": row.prompt_tokens,
                "completion_tokens": row.completion_tokens,
                "cached_tokens": row.cached_tokens,
                "cache_rate": row.cache_rate(),
            })
        })
        .collect();
    axum::Json(serde_json::json!({
        "ok": true,
        "window": window.label(),
        "conversation": conversation,
        "calls": summary.calls,
        "failed": summary.failed,
        "prompt_tokens": summary.prompt_tokens,
        "completion_tokens": summary.completion_tokens,
        "cached_tokens": summary.cached_tokens,
        "cache_rate": summary.cache_rate(),
        "unmeasured": summary.unmeasured,
        "models": rows,
    }))
    .into_response()
}

/// `GET /v1/processes` - background work this daemon is running.
///
/// Background work used to be invisible off the host: a `nohup` shell a turn left
/// behind was in no table and on no screen. This route reads the same handle table
/// the `process` tool reads, so the phone and the model see one list.
pub async fn processes(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    // Seeing what is running is a read of this operator's own work. Stopping it is
    // an administrative act and is gated below.
    if let Err(error) = authenticate(&service, &headers) {
        return error.into_response();
    }
    let engine = service.engine();
    let workspace = engine
        .workspace_path()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let desk = crate::process::Desk::new(engine.db().clone(), &workspace);
    let snapshot = desk.snapshot();
    axum::Json(serde_json::json!({
        "ok": true,
        "processes": snapshot["processes"],
        "running": snapshot["running"],
        "total": snapshot["total"],
    }))
    .into_response()
}

/// `POST /v1/processes/{id}/kill` - stop one, politely and then not.
pub async fn kill_process(
    State(service): State<DeviceService>,
    Path(id): Path<String>,
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
    let workspace = engine
        .workspace_path()
        .unwrap_or_else(|_| std::path::PathBuf::from("."));
    let desk = crate::process::Desk::new(engine.db().clone(), &workspace);
    axum::Json(serde_json::json!({
        "ok": true,
        "process": desk.kill_by_id(&id),
    }))
    .into_response()
}

/// `GET /v1/skills/{key}` - one skill's body.
///
/// A switched-off skill is absent here for the same reason it is absent from the
/// catalog a turn carries: half-off is not a state this daemon has.
pub async fn skill_detail(
    State(service): State<DeviceService>,
    Path(key): Path<String>,
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
    let stored = engine.setting(skills::SETTING_DIR).ok().flatten();
    let described = skills::list(engine.db(), stored.as_deref())
        .ok()
        .and_then(|(found, _)| found.into_iter().find(|skill| skill.key == key));
    match skills::read(engine.db(), stored.as_deref(), &key) {
        Ok(content) => axum::Json(serde_json::json!({
            "ok": true,
            "skill": {
                "key": key,
                "name": described
                    .as_ref()
                    .map_or_else(|| key.clone(), |skill| skill.name.clone()),
                "description": described.as_ref().and_then(|skill| skill.description.clone()),
                "path": described.as_ref().map(|skill| skill.path.clone()),
                "content": content,
            },
        }))
        .into_response(),
        Err(skills::SkillError::Unknown { key }) => DeviceError::new(
            axum::http::StatusCode::NOT_FOUND,
            "skill_unknown",
            format!("no skill named `{key}` is in this library"),
        )
        .into_response(),
        Err(error) => DeviceError::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "skill_unreadable",
            error.to_string(),
        )
        .into_response(),
    }
}

/// The body `PUT /v1/skills/{key}` accepts: the whole file, and nothing else.
#[derive(Debug, Default, serde::Deserialize)]
struct SkillWriteBody {
    /// The complete `SKILL.md`, frontmatter included.
    #[serde(default)]
    content: String,
}

/// The ceiling on one write's request body.
///
/// The semantic cap is [`skills::MAX_SKILL_BYTES`], checked on the decoded text;
/// this one only bounds what this route will read off the wire, because JSON
/// escaping can cost several bytes per content byte (a newline is two, a quote
/// is two, a control character is six).
const SKILL_BODY_BYTES: usize = 4 * skills::MAX_SKILL_BYTES as usize + 4096;

/// Map a skill refusal onto the wire, with the code a client branches on.
///
/// The codes are the closed set a phone acts on: a key its own validator allowed
/// and this one did not (`skill_key_invalid`), a body it sent empty
/// (`skill_content_empty`), a body over the cap (`skill_content_too_large`), and
/// a key that names nothing here (`skill_unknown`). A library that cannot be read
/// keeps the spelling `GET /v1/skills/{key}` already uses.
fn skill_failure(error: skills::SkillError) -> DeviceError {
    use axum::http::StatusCode;
    match error {
        skills::SkillError::KeyInvalid { key } => DeviceError::new(
            StatusCode::BAD_REQUEST,
            "skill_key_invalid",
            format!(
                "`{key}` is not a usable skill key: letters, digits, dash, underscore or dot, at \
                 most 80 characters, and not starting with a dot"
            ),
        ),
        skills::SkillError::Empty { key } => DeviceError::new(
            StatusCode::BAD_REQUEST,
            "skill_content_empty",
            format!("the body of `{key}` is blank, and a SKILL.md must carry its procedure"),
        ),
        skills::SkillError::BodyTooLarge { bytes, limit } => DeviceError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "skill_content_too_large",
            format!("the body is {bytes} bytes, over the {limit}-byte ceiling for a skill"),
        ),
        skills::SkillError::Unknown { key } => DeviceError::new(
            StatusCode::NOT_FOUND,
            "skill_unknown",
            format!("no skill named `{key}` is in this library"),
        ),
        skills::SkillError::Symlink { path } => DeviceError::new(
            StatusCode::BAD_REQUEST,
            "skill_symlink_refused",
            format!("`{path}` is a symlink, and this daemon does not write or archive through one"),
        ),
        skills::SkillError::NoLibrary { path } => DeviceError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "skill_library_missing",
            format!("the skill library `{path}` is not a directory"),
        ),
        other => DeviceError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "skill_io_failed",
            other.to_string(),
        ),
    }
}

/// The stored library directory, as both skill writes read it.
fn stored_skill_dir(engine: &SetupEngine) -> Option<String> {
    engine.setting(skills::SETTING_DIR).ok().flatten()
}

/// `PUT /v1/skills/{key}` - replace one skill's whole file.
///
/// The body is `{"content": "<the complete SKILL.md>"}` and it is written as
/// given. The write is [`skills::write`], the same function the dashboard's edit
/// form calls, so the two surfaces cannot disagree about what a valid body is or
/// where it lands.
pub async fn skill_write(
    State(service): State<DeviceService>,
    Path(key): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    use axum::http::StatusCode;
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    // A body sent with the wrong type is refused by name rather than decoded as
    // JSON, so a client that forgot the header is told which header to send
    // instead of being told its content was empty. A request with no type at all
    // is decoded anyway: a bare `PUT -d` from a shell script is a JSON body.
    if let Some(value) = headers.get(axum::http::header::CONTENT_TYPE) {
        let value = value.to_str().unwrap_or_default().to_ascii_lowercase();
        if !value.starts_with("application/json") {
            return DeviceError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "skill_content_type",
                "a skill write takes `Content-Type: application/json` with {\"content\": \
                 \"<the complete SKILL.md>\"}",
            )
            .into_response();
        }
    }
    if body.len() > SKILL_BODY_BYTES {
        return DeviceError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "skill_content_too_large",
            format!(
                "the request body is over the {SKILL_BODY_BYTES}-byte ceiling for a skill write"
            ),
        )
        .into_response();
    }
    let parsed: SkillWriteBody = match serde_json::from_slice(&body) {
        Ok(parsed) => parsed,
        Err(error) => {
            return DeviceError::new(
                StatusCode::BAD_REQUEST,
                "skill_content_invalid",
                format!("the body is not the JSON object a skill write takes: {error}"),
            )
            .into_response();
        }
    };
    let engine = service.engine();
    let stored = stored_skill_dir(engine);
    match skills::write(engine.db(), stored.as_deref(), &key, &parsed.content) {
        Ok(bytes) => {
            let detail = format!("write {key} ({bytes} bytes)");
            if let Err(error) = audit(&service, token.device_id(), AUDIT_SKILLS, &detail) {
                return error.into_response();
            }
            axum::Json(serde_json::json!({ "ok": true, "key": key, "bytes": bytes }))
                .into_response()
        }
        Err(error) => skill_failure(error).into_response(),
    }
}

/// `DELETE /v1/skills/{key}` - delete one skill for good and drop its ledger row.
///
/// The directory and its `SKILL.md` are removed from disk. This is not an archive
/// move: `.archive/` is the curator's ageing attic, and nothing about a delete is
/// reversible. The answer names the directory that went, so a client can say what
/// was destroyed rather than imply it can be restored.
pub async fn skill_delete(
    State(service): State<DeviceService>,
    Path(key): Path<String>,
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
    let stored = stored_skill_dir(engine);
    match skills::delete(engine.db(), stored.as_deref(), &key) {
        Ok(removed) => {
            let detail = format!("delete {key} ({removed})");
            if let Err(error) = audit(&service, token.device_id(), AUDIT_SKILLS, &detail) {
                return error.into_response();
            }
            axum::Json(serde_json::json!({
                "ok": true,
                "key": key,
                "removed": removed,
            }))
            .into_response()
        }
        Err(error) => skill_failure(error).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Auxiliary roles
// ---------------------------------------------------------------------------

/// `GET /v1/aux` - every auxiliary role, what it runs on, and what may be pinned.
///
/// `models` and `providers` ride along so a picker can be drawn from one read
/// rather than three. Neither is a credential: the model ids are the ones the
/// models route already publishes and a provider is an id, a name and a flag.
pub async fn aux_roles(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    match read_aux(service.engine()) {
        Ok(body) => axum::Json(body).into_response(),
        Err(error) => error.into_response(),
    }
}

/// The snapshot `GET /v1/aux` answers with.
fn read_aux(engine: &SetupEngine) -> Result<serde_json::Value, DeviceError> {
    let primary_model = engine
        .model()
        .map_err(settings_unavailable)?
        .unwrap_or_default();
    let (providers, primary_provider) = registry(engine)?;
    let primary_id = primary_provider.clone().unwrap_or_default();

    let mut roles = Vec::new();
    for role in aux::ROLES {
        let chosen = engine
            .setting(&aux::setting_key(role.key))
            .map_err(settings_unavailable)?;
        let provider = engine
            .setting(&aux::provider_key(role.key))
            .map_err(settings_unavailable)?;
        // What the role would actually run, which is the primary when nothing is
        // pinned. `pinned` is what is stored, so a client can say "follows the
        // primary" without inferring it from a match that might be a coincidence.
        let effective_provider = aux::provider_for(provider.as_deref(), &providers, &primary_id)
            .map(|provider| provider.id.clone())
            .unwrap_or_else(|| primary_id.clone());
        // Read before either option is consumed below: a role that stores no
        // model is the same state as one that stores the sentinel, and both mean
        // "follow the primary".
        let follows = aux::inherits(chosen.as_deref());
        let follows_provider = aux::inherits(provider.as_deref());
        roles.push(serde_json::json!({
            "key": role.key,
            "label": role.label,
            "purpose": role.purpose,
            "consumed_by": role.consumed_by,
            "model": aux::effective(chosen.as_deref(), &primary_model),
            "pinned": if follows {
                serde_json::Value::Null
            } else {
                chosen.map_or(serde_json::Value::Null, serde_json::Value::String)
            },
            "inherits": follows,
            "provider": effective_provider,
            "pinned_provider": if follows_provider {
                serde_json::Value::Null
            } else {
                provider.map_or(serde_json::Value::Null, serde_json::Value::String)
            },
            // Whether this daemon checks the pin against the chat-model probe.
            // False for the image role, which posts to an image endpoint: a
            // client that greyed out that row's picker would be refusing the
            // right pin for the wrong reason.
            "probe_validated": role.probe_validated,
        }));
    }
    Ok(serde_json::json!({
        "ok": true,
        "primary_model": primary_model,
        "roles": roles,
        "models": engine
            .primary_provider_catalog()
            .map_err(settings_unavailable)?,
        // Every provider's own list, so a client can offer the pin each one
        // would actually answer. `models` above stays the primary's, which is
        // what a role that follows the primary pins against.
        "catalogs": engine
            .catalog_map(
                &providers
                    .iter()
                    .map(|candidate| candidate.id.clone())
                    .collect::<Vec<String>>()
            )
            .map_err(settings_unavailable)?,
        "providers": providers_json(&providers, primary_provider.as_deref()),
    }))
}

/// `POST /v1/aux` - pin one role's model and provider.
///
/// One role per request. The dashboard writes all seven rows from one form, which
/// is right for a page that shows all seven; a phone edits the row it opened, and
/// a body carrying seven rows would let one mistyped id refuse the six that were
/// fine.
///
/// Both values are optional, and both validate before either is written, so a role
/// is never left holding a model from a request whose provider half never landed.
/// `"primary"` is the sentinel that means "follow the primary".
pub async fn set_aux(
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
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let Some(key) = fields.get("role").map(|raw| raw.trim().to_owned()) else {
        return DeviceError::bad_request(
            "aux_role_missing",
            "the body named no `role`: one role is pinned per request".to_owned(),
        )
        .into_response();
    };
    let Some(role) = aux::role(&key) else {
        let known: Vec<&str> = aux::ROLES.iter().map(|role| role.key).collect();
        return DeviceError::bad_request(
            "aux_role_unknown",
            format!(
                "role does not accept `{key}`: this build knows {}",
                known.join(", ")
            ),
        )
        .into_response();
    };

    let engine = service.engine();
    let (providers, primary_stored) = match registry(engine) {
        Ok(registry) => registry,
        Err(error) => return error.into_response(),
    };
    let primary_id = primary_stored.unwrap_or_else(|| crate::providers::DEFAULT_ID.to_owned());
    // The provider this role runs on, which is the one whose catalogue decides
    // whether the pin would answer. A model the second provider published is a
    // valid pin on the second provider and on nothing else.
    let role_provider = fields
        .get("provider")
        .map(|raw| aux::store_value(raw))
        .and_then(|value| {
            aux::provider_for(Some(&value), &providers, &primary_id)
                .map(|candidate| candidate.id.clone())
        })
        .unwrap_or_else(|| primary_id.clone());
    let mut write: Vec<(String, String)> = Vec::new();

    if let Some(raw) = fields.get("model") {
        let value = aux::store_value(raw);
        let known_models = engine.provider_catalog(&role_provider).unwrap_or_default();
        if role.probe_validated
            && value != aux::INHERIT
            && !known_models.is_empty()
            && !known_models.contains(&value)
        {
            return DeviceError::bad_request(
                "aux_model_unknown",
                format!(
                    "`{value}` is not a model the last probe returned, so it would never answer"
                ),
            )
            .into_response();
        }
        write.push((aux::setting_key(role.key), value));
    }
    if let Some(raw) = fields.get("provider") {
        let value = aux::store_value(raw);
        if value != aux::INHERIT && !providers.iter().any(|candidate| candidate.id == value) {
            return DeviceError::bad_request(
                "aux_provider_unknown",
                format!("`{value}` is not in the provider registry, so the role could not run"),
            )
            .into_response();
        }
        write.push((aux::provider_key(role.key), value));
    }
    if write.is_empty() {
        return DeviceError::bad_request(
            "aux_empty",
            "the body named neither `model` nor `provider`: nothing was changed".to_owned(),
        )
        .into_response();
    }

    let db = engine.db();
    for (key, value) in &write {
        if let Err(error) = store::settings_put(db, key, value) {
            return DeviceError::db_unavailable(format!("cannot write `{key}`: {error}"))
                .into_response();
        }
    }
    let detail = write
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("; ");
    if let Err(error) = audit(&service, token.device_id(), AUDIT_AUX, &detail) {
        return error.into_response();
    }
    match read_aux(engine) {
        Ok(body) => axum::Json(body).into_response(),
        Err(error) => error.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The phone's picker showed one provider's models and called the rest
    /// absent, because this route only ever read the primary's catalogue. Six
    /// providers held a probed catalogue on a real daemon; the picker showed one.
    /// Each catalogue here was written by a probe against that provider, so a row
    /// is a measured claim, and a provider nobody probed contributes nothing.
    #[test]
    fn every_probed_provider_is_in_the_model_list() {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = crate::config::Paths::for_root(dir.path());
        let db = crate::db::Db::open(&paths.db_file).expect("db");
        let engine = crate::onboarding::SetupEngine::new(db, paths);

        let registry = r#"[
            {"id":"alpha","name":"Alpha","base_url":"https://alpha.test/v1","key_ref":"env:ALPHA"},
            {"id":"beta","name":"Beta","base_url":"https://beta.test/v1","key_ref":"env:BETA"},
            {"id":"gamma","name":"Gamma","base_url":"https://gamma.test/v1","key_ref":"env:GAMMA"}
        ]"#;
        engine
            .set_setting(crate::providers::SETTING_REGISTRY, registry)
            .expect("registry");
        engine
            .set_setting(crate::providers::SETTING_PRIMARY, "alpha")
            .expect("primary");
        for (provider, models) in [
            ("alpha", r#"["alpha-1","alpha-2"]"#),
            ("beta", r#"["beta-1"]"#),
            // Gamma was never probed: nothing to publish, and nothing invented.
            ("gamma", "[]"),
        ] {
            engine
                .set_setting(
                    &crate::onboarding::SetupEngine::provider_catalog_key(provider),
                    models,
                )
                .expect("catalogue");
        }
        engine
            .set_setting(
                &crate::onboarding::SetupEngine::provider_contexts_key("beta"),
                r#"{"beta-1":128000}"#,
            )
            .expect("contexts");
        // Written straight to the row: this test is about the list, and the
        // pin's own validation is `check_model`'s job, tested where it lives.
        engine
            .set_setting(crate::onboarding::SETTING_MODEL, "alpha-1")
            .expect("pin");

        let body = read_models(&engine).expect("the snapshot");
        let rows = body["models"].as_array().expect("rows");
        let mut seen: Vec<(String, String)> = rows
            .iter()
            .map(|row| {
                (
                    row["provider"].as_str().unwrap_or_default().to_owned(),
                    row["id"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect();
        seen.sort();
        assert_eq!(
            seen,
            vec![
                ("alpha".to_owned(), "alpha-1".to_owned()),
                ("alpha".to_owned(), "alpha-2".to_owned()),
                ("beta".to_owned(), "beta-1".to_owned()),
            ],
            "every probed provider is listed, and an unprobed one is not: {body}"
        );

        // The pin is still one row, on the provider that holds it.
        let defaults: Vec<&serde_json::Value> = rows
            .iter()
            .filter(|row| row["is_default"] == true)
            .collect();
        assert_eq!(defaults.len(), 1, "exactly one row carries the tick");
        assert_eq!(defaults[0]["provider"], "alpha");

        // A context window travels with the provider it was measured for.
        let beta = rows
            .iter()
            .find(|row| row["id"] == "beta-1")
            .expect("the beta row");
        assert_eq!(beta["context_window"], 128000);
    }

    #[test]
    fn every_cron_edit_field_is_named_in_the_empty_body_refusal() {
        assert_eq!(
            CRON_PATCH_FIELDS,
            ["enabled", "name", "schedule", "prompt"],
            "a refusal that lists the fields must list the fields the route reads"
        );
    }

    #[test]
    fn a_flag_is_read_in_both_spellings_a_body_can_use() {
        for raw in ["true", "TRUE", " on ", "1", "yes"] {
            assert!(matches!(parse_flag("enabled", raw), Ok(true)), "{raw}");
        }
        for raw in ["false", "False", "off", "0", "no"] {
            assert!(matches!(parse_flag("enabled", raw), Ok(false)), "{raw}");
        }
        let refusal = parse_flag("enabled", "maybe").expect_err("a refusal");
        assert_eq!(refusal.code(), "flag_invalid");
    }

    #[test]
    fn a_provider_row_carries_no_endpoint_and_no_key_reference() {
        let registry = vec![Provider {
            id: "alpha".to_owned(),
            name: "Alpha".to_owned(),
            base_url: "http://127.0.0.1:20128/v1".to_owned(),
            key_ref: "env:NINEROUTER_API_KEY".to_owned(),
            dialect: None,
            reasoning_split: None,
            direct: None,
            project: None,
        }];
        let rendered = providers_json(&registry, Some("alpha")).to_string();
        assert!(rendered.contains("\"id\":\"alpha\""), "{rendered}");
        assert!(
            !rendered.contains("20128") && !rendered.contains("NINEROUTER"),
            "a provider read must not name the endpoint or the key reference: {rendered}"
        );
    }

    #[test]
    fn only_the_primary_provider_is_flagged_primary() {
        let registry = vec![
            Provider {
                id: "alpha".to_owned(),
                name: "Alpha".to_owned(),
                base_url: "http://a/v1".to_owned(),
                key_ref: "env:A".to_owned(),
                dialect: None,
                reasoning_split: None,
                direct: None,
                project: None,
            },
            Provider {
                id: "beta".to_owned(),
                name: "Beta".to_owned(),
                base_url: "http://b/v1".to_owned(),
                key_ref: "env:B".to_owned(),
                dialect: None,
                reasoning_split: None,
                direct: None,
                project: None,
            },
        ];
        let rows = providers_json(&registry, Some("beta"));
        let rows = rows.as_array().expect("an array");
        assert_eq!(rows[0]["primary"], false);
        assert_eq!(rows[1]["primary"], true);
    }

    // -----------------------------------------------------------------------
    // /v1/routing - the model each kind of work runs on
    // -----------------------------------------------------------------------

    use crate::auth::{ScopeSet, TokenOrigin, TokenService};
    use tower::ServiceExt;

    /// A ready device service plus a chat token and an admin token, on a scratch
    /// root so nothing touches the operator's state directory.
    struct RoutingFixture {
        _dir: tempfile::TempDir,
        service: DeviceService,
        chat: String,
        admin: String,
    }

    fn routing_fixture() -> RoutingFixture {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = crate::config::Paths::for_root(dir.path());
        let db = crate::db::Db::open(&paths.db_file).expect("db");
        let engine = crate::onboarding::SetupEngine::new(db.clone(), paths);
        let service = DeviceService::new(engine, 32);
        let tokens = TokenService::new(db);
        let device_id = tokens.register_device("routing", "ios").expect("device");
        let chat = tokens
            .mint_access(&device_id, ScopeSet::device_default(), TokenOrigin::Device)
            .expect("chat token");
        let admin = tokens
            .mint_access(
                &device_id,
                ScopeSet::device_default().with(Scope::Admin),
                TokenOrigin::Loopback,
            )
            .expect("admin token");
        RoutingFixture {
            _dir: dir,
            service,
            chat: chat.token().to_owned(),
            admin: admin.token().to_owned(),
        }
    }

    /// Send one request to the device router and return status + parsed JSON.
    async fn send(
        service: &DeviceService,
        method: &str,
        path: &str,
        token: &str,
        body: &str,
    ) -> (axum::http::StatusCode, serde_json::Value) {
        let mut builder = axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if !token.is_empty() {
            builder = builder.header("authorization", format!("Bearer {token}"));
        }
        let request = builder.body(Body::from(body.to_owned())).expect("request");
        let response = service.router().oneshot(request).await.expect("router");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body");
        let value = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, value)
    }

    #[tokio::test]
    async fn the_routing_read_names_every_field_a_client_draws() {
        let fx = routing_fixture();
        let (status, body) = send(&fx.service, "GET", "/v1/routing", &fx.chat, "").await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["ok"], true);
        assert_eq!(body["auto"], true, "routing ships armed: {body}");
        assert_eq!(body["coding_model"], "");
        assert_eq!(body["heavy_model"], "");
        assert_eq!(body["general_model"], "");
        assert_eq!(body["harness"], "none");
        assert_eq!(
            body["harnesses"],
            serde_json::json!(["none", "dsh", "claude-code", "antigravity"])
        );
        assert_eq!(
            body["provider_model"],
            serde_json::Value::Null,
            "an unconfigured daemon inherits nothing: {body}"
        );

        // With a provider configured, the inherited model is named, because
        // that is what a turn with no routing row would run on.
        let engine = fx.service.engine();
        engine
            .set_setting(
                crate::providers::SETTING_REGISTRY,
                r#"[{"id":"alpha","name":"Alpha","base_url":"https://alpha.test/v1","key_ref":"env:ALPHA"}]"#,
            )
            .expect("registry");
        engine
            .set_setting(crate::providers::SETTING_PRIMARY, "alpha")
            .expect("primary");
        engine
            .set_setting(crate::onboarding::SETTING_MODEL, "alpha/m1")
            .expect("pin");
        let (status, body) = send(&fx.service, "GET", "/v1/routing", &fx.chat, "").await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["provider_model"], "m1");
    }

    #[tokio::test]
    async fn a_routing_write_lands_reads_back_and_leaves_absent_keys_alone() {
        let fx = routing_fixture();
        let (status, body) = send(
            &fx.service,
            "PUT",
            "/v1/routing",
            &fx.admin,
            r#"{"auto":false,"coding_model":"coder","heavy_model":"thinker",
                "general_model":"talker","harness":"dsh"}"#,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["auto"], false);
        assert_eq!(body["coding_model"], "coder");
        assert_eq!(body["heavy_model"], "thinker");
        assert_eq!(body["general_model"], "talker");
        assert_eq!(body["harness"], "dsh");

        // A second body carries one key: the other rows keep what they held.
        let (status, body) = send(
            &fx.service,
            "PUT",
            "/v1/routing",
            &fx.admin,
            r#"{"coding_model":"coder-2"}"#,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["coding_model"], "coder-2");
        assert_eq!(body["heavy_model"], "thinker");
        assert_eq!(body["general_model"], "talker");
        assert_eq!(body["harness"], "dsh");
        assert_eq!(body["auto"], false);

        // An empty model string clears the row back to inherit, which reads as
        // the same empty string a never-written row answers.
        let (status, body) = send(
            &fx.service,
            "PUT",
            "/v1/routing",
            &fx.admin,
            r#"{"coding_model":""}"#,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["coding_model"], "");
        let (_, read) = send(&fx.service, "GET", "/v1/routing", &fx.admin, "").await;
        assert_eq!(read["coding_model"], "");
        assert_eq!(read["heavy_model"], "thinker");
    }

    #[tokio::test]
    async fn a_routing_write_needs_administration() {
        let fx = routing_fixture();
        let (status, body) = send(
            &fx.service,
            "PUT",
            "/v1/routing",
            &fx.chat,
            r#"{"auto":false}"#,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::FORBIDDEN, "{body}");
        // The refusal is a refusal, not a write that was reported as one.
        let (_, read) = send(&fx.service, "GET", "/v1/routing", &fx.chat, "").await;
        assert_eq!(read["auto"], true);
    }

    #[tokio::test]
    async fn an_unknown_harness_is_refused_and_the_rest_of_the_body_with_it() {
        let fx = routing_fixture();
        let (status, body) = send(
            &fx.service,
            "PUT",
            "/v1/routing",
            &fx.admin,
            r#"{"auto":false,"harness":"some-other-tool"}"#,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], "routing_harness_invalid");
        // Nothing in the refused body was stored, including the flag that would
        // have turned routing off.
        let (_, read) = send(&fx.service, "GET", "/v1/routing", &fx.admin, "").await;
        assert_eq!(read["harness"], "none");
        assert_eq!(read["auto"], true);
    }

    #[tokio::test]
    async fn the_preview_answers_the_decision_without_sending_a_turn() {
        let fx = routing_fixture();
        // Nothing pinned yet: code work, and no model to name.
        let (status, body) = send(
            &fx.service,
            "POST",
            "/v1/routing/preview",
            &fx.chat,
            r#"{"message":"please implement a parser"}"#,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK, "{body}");
        assert_eq!(body["ok"], true);
        assert_eq!(body["intent"], "coding");
        assert_eq!(body["model"], serde_json::Value::Null);
        assert_eq!(body["harness"], serde_json::Value::Null);
        assert_eq!(body["reason"], "asks to implement");

        // Pinned: the preview reports the model and the harness a turn would get.
        let (status, _) = send(
            &fx.service,
            "PUT",
            "/v1/routing",
            &fx.admin,
            r#"{"coding_model":"coder","harness":"claude-code"}"#,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let (_, body) = send(
            &fx.service,
            "POST",
            "/v1/routing/preview",
            &fx.chat,
            r#"{"message":"please implement a parser"}"#,
        )
        .await;
        assert_eq!(body["intent"], "coding");
        assert_eq!(body["model"], "coder");
        assert_eq!(body["harness"], "claude-code");

        // A heavy ask is not code work, so it carries no harness.
        let (_, body) = send(
            &fx.service,
            "POST",
            "/v1/routing/preview",
            &fx.chat,
            r#"{"message":"please research the market"}"#,
        )
        .await;
        assert_eq!(body["intent"], "heavy");
        assert_eq!(body["harness"], serde_json::Value::Null);

        // Switched off, the preview says so and promises the daemon's own model.
        let (status, _) = send(
            &fx.service,
            "PUT",
            "/v1/routing",
            &fx.admin,
            r#"{"auto":false}"#,
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let (_, body) = send(
            &fx.service,
            "POST",
            "/v1/routing/preview",
            &fx.chat,
            r#"{"message":"please implement a parser"}"#,
        )
        .await;
        assert_eq!(body["intent"], "general");
        assert_eq!(body["reason"], "automatic routing is off");
        assert_eq!(body["harness"], serde_json::Value::Null);
    }
}

// ---------------------------------------------------------------------------
// /v1/steward - durable duties, field notes and the charter seed
// ---------------------------------------------------------------------------

/// Map a steward refusal to the device surface: a missing duty is 404, a stale
/// revision or a closed duty is 409 naming what to do next, a broken rule is 400.
fn steward_refusal(error: crate::steward::DutyError) -> DeviceError {
    use crate::steward::DutyError;
    let status = match &error {
        DutyError::NotFound(_) => axum::http::StatusCode::NOT_FOUND,
        DutyError::Closed(_) | DutyError::StaleRevision { .. } => axum::http::StatusCode::CONFLICT,
        DutyError::NeedsYouRequiresCall
        | DutyError::CloseRequiresCloseDuty
        | DutyError::InvalidPhase(_)
        | DutyError::Invalid(_) => axum::http::StatusCode::BAD_REQUEST,
        DutyError::Corrupt(_) | DutyError::Db(_) | DutyError::Sqlite(_) => {
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    let code = match &error {
        DutyError::NotFound(_) => "steward_not_found",
        DutyError::Closed(_) => "steward_closed",
        DutyError::StaleRevision { .. } => "steward_stale_revision",
        DutyError::NeedsYouRequiresCall | DutyError::CloseRequiresCloseDuty => {
            "steward_phase_refused"
        }
        DutyError::InvalidPhase(_) | DutyError::Invalid(_) => "steward_invalid",
        DutyError::Corrupt(_) | DutyError::Db(_) | DutyError::Sqlite(_) => "steward_store",
    };
    let message = match &error {
        DutyError::StaleRevision { current } => format!(
            "stale revision: the duty is at revision {current}. Re-read it, then retry at revision {current}."
        ),
        other => other.to_string(),
    };
    DeviceError::new(status, code, message)
}

/// A revision from a form or body field. Every mutation names the revision it read.
fn revision_field(fields: &std::collections::BTreeMap<String, String>) -> Result<i64, DeviceError> {
    field(fields, "revision")
        .trim()
        .parse::<i64>()
        .map_err(|_| {
            DeviceError::bad_request(
                "steward_revision_required",
                "name the revision you read: `revision` is a whole number",
            )
        })
}

/// `GET /v1/steward` - the steward's settings, its duties and its latest notes.
pub async fn steward_overview(
    State(service): State<DeviceService>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = authenticate(&service, &headers) {
        return error.into_response();
    }
    let db = service.engine().db();
    let duties = match crate::steward::list_duties(db, None) {
        Ok(duties) => duties,
        Err(error) => return steward_refusal(error).into_response(),
    };
    let notes = match crate::steward::list_notes(db, 8, None) {
        Ok(notes) => notes,
        Err(error) => return steward_refusal(error).into_response(),
    };
    axum::Json(serde_json::json!({
        "ok": true,
        "settings": {
            "enabled": crate::steward::enabled(db),
            "watch_seconds": crate::steward::watch_seconds(db),
            "max_rounds": crate::steward::max_rounds(db),
            "note_window": crate::steward::note_window(db),
            "field_note_days": crate::steward::field_note_days(db),
            "delivery_default": crate::steward::delivery_default(db),
            "call_urgency_default": crate::steward::call_urgency_default(db),
            "charter": crate::steward::charter(db),
            "board_columns": crate::steward::board_columns(db),
        },
        "duties": duties.iter().map(crate::steward::Duty::json).collect::<Vec<_>>(),
        "notes": notes.iter().map(crate::steward::FieldNote::json).collect::<Vec<_>>(),
    }))
    .into_response()
}

/// What the duty list accepts.
#[derive(Debug, serde::Deserialize)]
pub struct DutyQuery {
    /// One phase, or every phase when absent.
    #[serde(default)]
    phase: Option<String>,
}

/// `GET /v1/steward/duties` - every duty, optionally in one phase.
pub async fn steward_duties_list(
    State(service): State<DeviceService>,
    Query(params): Query<DutyQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = authenticate(&service, &headers) {
        return error.into_response();
    }
    let phase = match params
        .phase
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        None => None,
        Some(raw) => match raw.parse::<crate::steward::Phase>() {
            Ok(phase) => Some(phase),
            Err(error) => return steward_refusal(error).into_response(),
        },
    };
    match crate::steward::list_duties(service.engine().db(), phase) {
        Ok(duties) => axum::Json(serde_json::json!({
            "ok": true,
            "count": duties.len(),
            "duties": duties.iter().map(crate::steward::Duty::json).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(error) => steward_refusal(error).into_response(),
    }
}

/// `POST /v1/steward/duties` - add one duty, optionally with its own cadence.
pub async fn steward_duty_create(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(error) = authenticate(&service, &headers) {
        return error.into_response();
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let every = match field(&fields, "every_seconds").trim() {
        "" => None,
        raw => match raw.parse::<i64>() {
            Ok(seconds) => Some(seconds),
            Err(_) => {
                return DeviceError::bad_request(
                    "steward_invalid",
                    "every_seconds is a whole number",
                )
                .into_response();
            }
        },
    };
    match crate::steward::create_duty(service.engine().db(), field(&fields, "statement"), every) {
        Ok(duty) => {
            axum::Json(serde_json::json!({"ok": true, "duty": duty.json()})).into_response()
        }
        Err(error) => steward_refusal(error).into_response(),
    }
}

/// `PATCH /v1/steward/duties/{id}` - one compare-and-set change. The body names
/// the revision the change was read at.
pub async fn steward_duty_patch(
    State(service): State<DeviceService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(error) = authenticate(&service, &headers) {
        return error.into_response();
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let revision = match revision_field(&fields) {
        Ok(revision) => revision,
        Err(error) => return error.into_response(),
    };
    let phase = match field(&fields, "phase").trim() {
        "" => None,
        raw => match raw.parse::<crate::steward::Phase>() {
            Ok(phase) => Some(phase),
            Err(error) => return steward_refusal(error).into_response(),
        },
    };
    let every_seconds = match field(&fields, "every_seconds").trim() {
        "" => None,
        raw => match raw.parse::<i64>() {
            Ok(seconds) => Some(Some(seconds)),
            Err(_) => {
                return DeviceError::bad_request(
                    "steward_invalid",
                    "every_seconds is a whole number",
                )
                .into_response();
            }
        },
    };
    let code = field(&fields, "blocker_code").trim().to_owned();
    let detail = field(&fields, "blocker_detail").trim().to_owned();
    let blocker = (!code.is_empty() || !detail.is_empty())
        .then_some(Some(crate::steward::Blocker { code, detail }));
    let statement = fields
        .get("statement")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let patch = crate::steward::DutyPatch {
        statement,
        phase,
        every_seconds,
        blocker,
        ..crate::steward::DutyPatch::default()
    };
    match crate::steward::update_duty(service.engine().db(), &id, revision, &patch) {
        Ok(duty) => {
            axum::Json(serde_json::json!({"ok": true, "duty": duty.json()})).into_response()
        }
        Err(error) => steward_refusal(error).into_response(),
    }
}

/// `POST /v1/steward/duties/{id}/close` - close one duty. Terminal.
pub async fn steward_duty_close(
    State(service): State<DeviceService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(error) = authenticate(&service, &headers) {
        return error.into_response();
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let revision = match revision_field(&fields) {
        Ok(revision) => revision,
        Err(error) => return error.into_response(),
    };
    match crate::steward::close_duty(
        service.engine().db(),
        &id,
        revision,
        field(&fields, "outcome"),
    ) {
        Ok(duty) => {
            axum::Json(serde_json::json!({"ok": true, "duty": duty.json()})).into_response()
        }
        Err(error) => steward_refusal(error).into_response(),
    }
}

/// `POST /v1/steward/duties/{id}/answer` - a person answers the open call, which
/// clears `needs_you` and arms the duty again. Only a person's request reaches here.
pub async fn steward_call_answer(
    State(service): State<DeviceService>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(error) = authenticate(&service, &headers) {
        return error.into_response();
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let revision = match revision_field(&fields) {
        Ok(revision) => revision,
        Err(error) => return error.into_response(),
    };
    match crate::steward::answer_call(service.engine().db(), &id, revision) {
        Ok(duty) => {
            axum::Json(serde_json::json!({"ok": true, "duty": duty.json()})).into_response()
        }
        Err(error) => steward_refusal(error).into_response(),
    }
}

/// What the notes list accepts.
#[derive(Debug, serde::Deserialize)]
pub struct NoteQuery {
    /// One duty's notes and the steward-wide ones; every note when absent.
    #[serde(default)]
    duty: Option<String>,
    /// How many notes, clamped here.
    #[serde(default)]
    limit: Option<usize>,
}

/// `GET /v1/steward/notes` - field notes, newest first.
pub async fn steward_notes_list(
    State(service): State<DeviceService>,
    Query(params): Query<NoteQuery>,
    headers: HeaderMap,
) -> Response {
    if let Err(error) = authenticate(&service, &headers) {
        return error.into_response();
    }
    let limit = params.limit.unwrap_or(50).clamp(1, 200);
    match crate::steward::list_notes(service.engine().db(), limit, params.duty.as_deref()) {
        Ok(notes) => axum::Json(serde_json::json!({
            "ok": true,
            "count": notes.len(),
            "notes": notes.iter().map(crate::steward::FieldNote::json).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(error) => steward_refusal(error).into_response(),
    }
}

/// `POST /v1/steward/seed` - one duty per non-empty line of the stored charter.
/// Returns what it created, what it skipped, and what it refused.
pub async fn steward_seed(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    if let Err(error) = authenticate(&service, &headers) {
        return error.into_response();
    }
    let db = service.engine().db();
    let charter = crate::steward::charter(db);
    match crate::steward::seed_from_text(db, &charter) {
        Ok(seeded) => axum::Json(serde_json::json!({
            "ok": true,
            "created": seeded.created.iter().map(crate::steward::Duty::json).collect::<Vec<_>>(),
            "skipped": seeded.skipped,
            "refused": seeded.refused,
        }))
        .into_response(),
        Err(error) => steward_refusal(error).into_response(),
    }
}
