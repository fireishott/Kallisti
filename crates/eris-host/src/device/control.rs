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
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};

use crate::aux;
use crate::device::{DeviceError, DeviceService};
use crate::onboarding::SetupEngine;
use crate::providers::Provider;
use crate::scheduler::{self, Schedule};
use crate::skills;
use crate::web::{chat, store};

use super::handlers::{
    authenticate, model_write_refusal, read_fields, refuse_value, require_admin,
    resolve_model_value, settings_unavailable,
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
/// See [`AUDIT_MODELS`].
const AUDIT_AUX: &str = "settings.device_aux";

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

/// The snapshot `GET /v1/models` answers with.
fn read_models(engine: &SetupEngine) -> Result<serde_json::Value, DeviceError> {
    // The primary provider's own catalogue: this route reports the models the
    // endpoint a turn resolves to has published, keyed by that provider.
    let catalog = engine
        .primary_provider_catalog()
        .map_err(settings_unavailable)?;
    let contexts = engine
        .primary_provider_contexts()
        .map_err(settings_unavailable)?;
    let pinned = engine
        .model()
        .map_err(settings_unavailable)?
        .unwrap_or_default();
    let (providers, primary) = registry(engine)?;
    let rows: Vec<serde_json::Value> = catalog
        .iter()
        .map(|id| {
            serde_json::json!({
                "id": id,
                "provider": primary,
                "context_window": contexts.get(id),
                // The pin this daemon holds. A picker can draw its tick from
                // this without comparing ids itself.
                "is_default": id == &pinned,
            })
        })
        .collect();
    Ok(serde_json::json!({
        "ok": true,
        "model": pinned,
        // `false` means no probe has published a list, which is a different
        // state from a probe that published an empty one. A client that showed
        // "no models" for both would send an operator looking in the wrong place.
        "probed": !catalog.is_empty(),
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
}
