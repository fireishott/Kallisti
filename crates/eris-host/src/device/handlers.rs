//! The `/v1` handlers.
//!
//! Every route does the same three things in the same order: authenticate the
//! bearer, check the gate that route is subject to, then act on the state store.
//! Nothing here decides what a scope means - [`ScopeSet::require`] does - and
//! nothing here writes an event directly: the hub owns the log so a handler can
//! never interleave with the worker.
//!
//! Bodies are JSON, with a form-encoded fallback so a `curl --data-urlencode`
//! acceptance run works without a `-H 'Content-Type: application/json'`.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::pin::Pin;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use tokio_stream::Stream;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::auth::{
    PairingService, Scope, ScopeSet, TokenOrigin, TokenService, VerifiedToken, format_timestamp,
};
use crate::device::turns::{ConversationTurn, FrameKind, TurnFrame, TurnPolicy};
use crate::device::{
    CONVERSATION_LIST_LIMIT, CONVERSATION_MESSAGE_LIMIT, DeviceError, DeviceService,
};
use crate::web::chat;

/// The upload ceiling for one device body. A prompt is text, and text is not
/// unbounded.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Header carrying a client's idempotency key.
const IDEMPOTENCY_HEADER: &str = "idempotency-key";

/// Header a reconnecting SSE client sends.
const LAST_EVENT_ID_HEADER: &str = "last-event-id";

/// How often an idle stream sends a comment, so a dead peer is noticed.
const KEEP_ALIVE_SECONDS: u64 = 15;

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

/// The presented bearer, when the header is shaped like one.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?;
    let token = token.trim();
    if token.is_empty() { None } else { Some(token) }
}

/// Verify the bearer against the stored verifiers.
///
/// One 401 for every failure mode - missing, malformed, expired, revoked - so a
/// client cannot use the difference between them to probe whether a token it
/// guessed ever existed.
fn authenticate(
    service: &DeviceService,
    headers: &HeaderMap,
) -> Result<VerifiedToken, DeviceError> {
    let presented = bearer(headers).ok_or_else(|| {
        DeviceError::unauthorized(
            "token_invalid",
            "the Authorization header must carry a Bearer access token",
        )
    })?;
    TokenService::new(service.engine().db().clone())
        .verify_access(presented)
        .map_err(|_| {
            DeviceError::unauthorized(
                "token_invalid",
                "the access token does not verify: it is unknown, expired, or revoked",
            )
        })
}

/// The scope check, as a 403 rather than a 401: the token is real, it just is not
/// allowed to do this.
fn require_scope(token: &VerifiedToken, scope: Scope) -> Result<(), DeviceError> {
    token.scope().require(scope).map_err(|_| {
        DeviceError::new(
            StatusCode::FORBIDDEN,
            "scope_denied",
            format!("the token does not carry the {scope} scope"),
        )
    })
}

/// The not-ready gate: a device surface on a daemon with no provider is a trap.
fn require_ready(service: &DeviceService) -> Result<(), DeviceError> {
    match service.engine().is_ready() {
        Ok(true) => Ok(()),
        Ok(false) => Err(DeviceError::new(
            StatusCode::CONFLICT,
            "not_ready",
            "onboarding is not finished: the device surface answers 409 until setup completes",
        )),
        Err(error) => Err(DeviceError::db_unavailable(format!(
            "cannot read the setup state: {error}"
        ))),
    }
}

/// The kill switch half of the dashboard toggle.
fn require_accepting(service: &DeviceService) -> Result<(), DeviceError> {
    if service.accepting() {
        return Ok(());
    }
    Err(DeviceError::new(
        StatusCode::CONFLICT,
        "device_listener_disabled",
        "the operator switched the device listener off: new turns resume when it is switched back on",
    ))
}

/// Read one body as a flat string map: JSON first, form-encoded otherwise.
async fn read_fields(
    headers: &HeaderMap,
    body: Body,
) -> Result<BTreeMap<String, String>, DeviceError> {
    let bytes = axum::body::to_bytes(body, MAX_BODY_BYTES)
        .await
        .map_err(|_| {
            DeviceError::bad_request(
                "body_too_large",
                format!("the body must be under {MAX_BODY_BYTES} bytes"),
            )
        })?;
    let text = String::from_utf8(bytes.to_vec())
        .map_err(|_| DeviceError::bad_request("body_not_utf8", "the body is not valid UTF-8"))?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(BTreeMap::new());
    }

    let looks_json = content_type.starts_with("application/json") || trimmed.starts_with('{');
    if looks_json {
        let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|_| {
            DeviceError::bad_request("body_not_json", "the JSON body did not parse")
        })?;
        let object = value.as_object().ok_or_else(|| {
            DeviceError::bad_request("body_not_object", "the JSON body must be an object")
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

    let mut fields = BTreeMap::new();
    for pair in trimmed.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (name, value) = match pair.split_once('=') {
            Some((name, value)) => (name, value),
            None => (pair, ""),
        };
        fields.insert(percent_decode(name)?, percent_decode(value)?);
    }
    Ok(fields)
}

/// Percent-decode one form component, with `+` as a space.
fn percent_decode(input: &str) -> Result<String, DeviceError> {
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
                    return Err(DeviceError::bad_request(
                        "body_malformed",
                        "a percent escape in the body is truncated",
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
        DeviceError::bad_request(
            "body_malformed",
            "a percent escape in the body is not valid UTF-8",
        )
    })
}

/// One hex digit.
fn hex_digit(byte: u8) -> Result<u8, DeviceError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(DeviceError::bad_request(
            "body_malformed",
            "a percent escape in the body is not hexadecimal",
        )),
    }
}

/// One field, trimmed. Missing reads as empty, so validation owns the message.
fn field<'a>(fields: &'a BTreeMap<String, String>, name: &str) -> &'a str {
    fields.get(name).map(String::as_str).unwrap_or("").trim()
}

/// One SSE frame: `id`, `event`, `data`, in that order.
fn frame_event(frame: &TurnFrame) -> Event {
    Event::default()
        .id(frame.seq.to_string())
        .event(frame.kind.as_str())
        .data(frame.data.to_string())
}

// ---------------------------------------------------------------------------
// POST /v1/pair
// ---------------------------------------------------------------------------

/// Redeem a pairing code for an access and refresh pair.
///
/// The code is single use and is verified against an Argon2id PHC string; the
/// plaintext exists only in the request. A code that is unknown, expired or
/// already consumed gets the same 401 with the same code, because the three are
/// not distinguishable to a client that should not be trying to guess.
pub async fn pair(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let code = field(&fields, "code");
    if code.is_empty() {
        return DeviceError::unauthorized("pairing_code_invalid", "a pairing code is required")
            .into_response();
    }

    let db = service.engine().db().clone();
    let redeemed = match PairingService::new(db.clone()).redeem(code) {
        Ok(redeemed) => redeemed,
        Err(_) => {
            return DeviceError::unauthorized(
                "pairing_code_invalid",
                "the pairing code is invalid, expired, or already used",
            )
            .into_response();
        }
    };

    let tokens = TokenService::new(db);
    let device_id = match redeemed.device_id() {
        Some(device_id) => device_id.to_owned(),
        None => {
            // An unbound code (minted by `eris setup` before the phone existed):
            // the name on the request is the only name this device will have.
            let name = field(&fields, "device_name");
            let name = if name.is_empty() { "device" } else { name };
            match tokens.register_device(name, "unknown") {
                Ok(device_id) => device_id,
                Err(error) => {
                    return DeviceError::new(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "device_register_failed",
                        error.to_string(),
                    )
                    .into_response();
                }
            }
        }
    };

    let scopes = ScopeSet::device_default();
    let minted = match tokens.mint_pair(&device_id, scopes, TokenOrigin::Device) {
        Ok(minted) => minted,
        Err(error) => {
            let status = match error {
                crate::auth::AuthError::DeviceRevoked => StatusCode::UNAUTHORIZED,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            return DeviceError::new(status, "pairing_rejected", error.to_string()).into_response();
        }
    };

    let list: Vec<&'static str> = scopes.iter().map(Scope::as_str).collect();
    (
        StatusCode::CREATED,
        axum::Json(serde_json::json!({
            "ok": true,
            "device_id": device_id,
            "access_token": minted.access.token(),
            "refresh_token": minted.refresh.token(),
            "scopes": list,
            "expires_at": format_timestamp(minted.access.expires_at()),
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// POST /v1/token/refresh
// ---------------------------------------------------------------------------

/// Trade a refresh token for one fresh access token.
///
/// The refresh token is verified and left alone rather than rotated: the
/// documented response carries only an access token, so a rotation here would
/// retire the caller's only means of refreshing and hand the new one to nobody.
/// Rotation is a P6 primitive the pairing flow does not use yet, and inventing it
/// into this endpoint would make a second refresh fail.
pub async fn refresh(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let presented = field(&fields, "refresh_token");
    if presented.is_empty() {
        return DeviceError::unauthorized("token_invalid", "a refresh token is required")
            .into_response();
    }
    let tokens = TokenService::new(service.engine().db().clone());
    let verified = match tokens.verify_refresh(presented) {
        Ok(verified) => verified,
        Err(_) => {
            return DeviceError::unauthorized(
                "token_invalid",
                "the refresh token does not verify: it is unknown, expired, or revoked",
            )
            .into_response();
        }
    };
    if verified.scope().has_admin() {
        return DeviceError::unauthorized(
            "token_invalid",
            "an admin-scoped refresh token is not accepted on the device listener",
        )
        .into_response();
    }
    let access =
        match tokens.mint_access(verified.device_id(), verified.scope(), TokenOrigin::Device) {
            Ok(access) => access,
            Err(error) => {
                return DeviceError::unauthorized("token_invalid", error.to_string())
                    .into_response();
            }
        };
    axum::Json(serde_json::json!({
        "ok": true,
        "access_token": access.token(),
        "expires_at": format_timestamp(access.expires_at()),
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// POST /v1/turns
// ---------------------------------------------------------------------------

/// Queue one turn and answer `202` before the provider has been called.
pub async fn create_turn(
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
    if let Err(error) = require_ready(&service) {
        return error.into_response();
    }
    if let Err(error) = require_accepting(&service) {
        return error.into_response();
    }

    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let text = field(&fields, "text");
    if text.is_empty() {
        return DeviceError::bad_request("text_required", "the turn text must not be empty")
            .into_response();
    }

    // A conversation is optional. Naming one makes this turn a transcript row
    // pair, so a client can read history after the stream closes; leaving it out
    // keeps the turn a session of its own, exactly as before. An id the daemon
    // does not hold is refused here, before anything is written, so a refused
    // turn leaves no half a conversation behind.
    let conversation = {
        let requested = field(&fields, "conversation").to_owned();
        if requested.is_empty() {
            None
        } else {
            match chat::conversation(service.engine().db(), &requested) {
                Ok(Some(row)) => Some(row.id),
                Ok(None) => return DeviceError::conversation_not_found().into_response(),
                Err(error) => {
                    return DeviceError::db_unavailable(format!(
                        "cannot read the conversation: {error}"
                    ))
                    .into_response();
                }
            }
        }
    };

    let idempotency_key = headers
        .get(IDEMPOTENCY_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let store = service.hub().store();
    if let Some(key) = idempotency_key {
        match store.find_idempotent(token.device_id(), key) {
            Ok(Some(job_id)) => {
                let job = store.job(&job_id).ok().flatten();
                let session_id = job
                    .as_ref()
                    .and_then(|job| job.session_id.clone())
                    .unwrap_or_default();
                let state = job
                    .map(|job| job.status)
                    .unwrap_or_else(|| "queued".to_owned());
                return (
                    StatusCode::ACCEPTED,
                    axum::Json(serde_json::json!({
                        "ok": true,
                        "job_id": job_id,
                        "session_id": session_id,
                        "state": state,
                        "conversation_id": conversation,
                    })),
                )
                    .into_response();
            }
            Ok(None) => {}
            Err(error) => {
                return DeviceError::db_unavailable(format!("cannot read job state: {error}"))
                    .into_response();
            }
        }
    }

    // A message that begins with a slash is a command, not a prompt. It is
    // dispatched here, before a session or a job exists and before the provider
    // settings are even read: a command has no turn to spend a token on, so the
    // guarantee is structural rather than a rule somebody has to remember. This
    // is the same parser and the same answer copy the browser's send handler
    // uses.
    if let Some(command) = crate::commands::parse(text) {
        return run_command(&service, token.device_id(), text, &command, idempotency_key);
    }

    // The writing half of a named conversation: the question and the answer row
    // are created before the provider is called, so a client that reads history
    // mid-answer sees the question and a row that says it is still streaming,
    // rather than a transcript that appears only after the stream closes. The
    // prompt itself is built by the worker from the same text, so a turn cannot
    // stream one thing and store another.
    let conversation_turn = match &conversation {
        None => None,
        Some(conversation_id) => {
            let db = service.engine().db();
            if let Err(error) = chat::append(
                db,
                conversation_id,
                chat::Role::User,
                text,
                chat::State::Complete,
            ) {
                return DeviceError::db_unavailable(format!(
                    "cannot write the question into the conversation: {error}"
                ))
                .into_response();
            }
            let model = store
                .provider_config()
                .ok()
                .flatten()
                .map(|config| config.model)
                .unwrap_or_default();
            let context_window = service
                .engine()
                .provider_contexts()
                .ok()
                .and_then(|contexts| contexts.get(&model).copied());
            let answer_id = match chat::append_answer(db, conversation_id, &model, context_window) {
                Ok(answer_id) => answer_id,
                Err(error) => {
                    return DeviceError::db_unavailable(format!(
                        "cannot open the answer row: {error}"
                    ))
                    .into_response();
                }
            };
            // The first question names the conversation and the rail bumps it to
            // the top, through the same two helpers the browser calls.
            let _ = chat::name_from_first_message(db, conversation_id, text);
            let _ = chat::touch_conversation(db, conversation_id);
            Some(ConversationTurn {
                conversation: conversation_id.clone(),
                answer_id,
            })
        }
    };

    let session_id = match store.ensure_session(token.device_id()) {
        Ok(session_id) => session_id,
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot open a session: {error}"))
                .into_response();
        }
    };
    let job_id = match store.create_job(token.device_id(), &session_id, idempotency_key, text) {
        Ok(job_id) => job_id,
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot queue the turn: {error}"))
                .into_response();
        }
    };

    // The channel exists before the response does, so a client that subscribes
    // the instant it reads the 202 cannot miss the first frame. The policy is
    // what the dashboard last saved: the same sandbox, gate, tool and display
    // settings the browser's turn reads.
    let sender = service.hub().begin(&job_id);
    service.hub().spawn(
        job_id.clone(),
        sender,
        TurnPolicy::from_engine(service.engine()),
        conversation_turn,
    );

    (
        StatusCode::ACCEPTED,
        axum::Json(serde_json::json!({
            "ok": true,
            "job_id": job_id,
            "session_id": session_id,
            "state": "queued",
            "conversation_id": conversation,
        })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Slash commands over /v1
// ---------------------------------------------------------------------------

/// Answer a message that is a slash command.
///
/// No provider is called and no token is spent: the reply is written as the
/// `done` frame of a job that never had a worker, so the device reads it through
/// the same stream it reads any other turn through and renders it the same way.
/// The parser, the help text and the refusal copy are `crate::commands`, which is
/// the same module the browser's send handler dispatches through.
///
/// Four of the six cannot mean the same thing here as they do in the browser.
/// `/new` and `/clear` act on the conversation the browser is showing, and a
/// command is answered before this surface writes any transcript row, so there is
/// nothing for either one to act on. `/model` and `/memory` are settings writes: a
/// device is minted `chat`, `sessions` and `files`, so changing the stored model
/// or memory backend behind that token would be a settings write with no settings
/// scope to check. All four are refused by name with the reason, rather than
/// silently skipped or emulated into something that looks like it worked.
fn run_command(
    service: &DeviceService,
    device_id: &str,
    text: &str,
    command: &crate::commands::Command,
    idempotency_key: Option<&str>,
) -> Response {
    let reply = match crate::commands::immediate(command) {
        Some(crate::commands::Outcome::Reply(reply)) => reply,
        Some(crate::commands::Outcome::Unsupported(name)) => {
            return DeviceError::bad_request(
                "command_unsupported",
                crate::commands::unsupported(&name),
            )
            .into_response();
        }
        None => match command.name.as_str() {
            "stop" => match stop_device_turns(service, device_id) {
                Ok(reply) => reply,
                Err(error) => return error.into_response(),
            },
            other => {
                return DeviceError::bad_request(
                    "command_not_available",
                    format!(
                        "/{other} is handled by this daemon but not on this surface: a command \
                         is answered before a turn or a transcript write exists, and the stored \
                         model and memory backend sit behind a settings scope this token does \
                         not hold, so nothing was changed and nothing was sent to a model. \
                         /help lists what a device turn answers."
                    ),
                )
                .into_response();
            }
        },
    };

    // Recorded before the answer is written, as the browser records it: a command
    // that changed something and was not recorded did not happen as far as this
    // product is concerned. The detail is the command, never the answer, so the
    // audit log stays a log of actions rather than a second transcript.
    if let Err(error) =
        service
            .engine()
            .auth()
            .audit(device_id, "chat.command", Some(&command_detail(command)))
    {
        return DeviceError::db_unavailable(format!(
            "cannot append the chat.command audit row: {error}"
        ))
        .into_response();
    }

    let store = service.hub().store();
    let session_id = match store.ensure_session(device_id) {
        Ok(session_id) => session_id,
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot open a session: {error}"))
                .into_response();
        }
    };
    let job_id = match store.create_job(device_id, &session_id, idempotency_key, text) {
        Ok(job_id) => job_id,
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot queue the turn: {error}"))
                .into_response();
        }
    };
    // No number was reported because no model ran, so the usage is unreported
    // rather than zero: a client that sums a command's "tokens" would otherwise
    // read a confident 0 where the truth is that nothing was billed.
    let usage = core_domain::Usage::unavailable(core_domain::UnavailableReason::FieldsAbsent);
    let data = serde_json::json!({
        "text": reply,
        "usage": usage,
        "command": format!("/{}", command.name),
    });
    if let Err(error) = store.finish(&job_id, FrameKind::Done, &data) {
        return DeviceError::db_unavailable(format!("cannot record the answer: {error}"))
            .into_response();
    }
    (
        StatusCode::ACCEPTED,
        axum::Json(serde_json::json!({
            "ok": true,
            "job_id": job_id,
            "session_id": session_id,
            "state": "succeeded",
        })),
    )
        .into_response()
}

/// The audit detail for one command: its name and the argument it carried.
///
/// Bounded, because the argument is client text and an audit row is not the place
/// for a paragraph.
fn command_detail(command: &crate::commands::Command) -> String {
    let argument: String = command.argument.chars().take(120).collect();
    if argument.is_empty() {
        format!("/{}", command.name)
    } else {
        format!("/{} {}", command.name, argument)
    }
}

/// Cancel every live turn this device owns, and say what that did.
///
/// The same ownership rule every job route enforces: a turn belongs to the device
/// that queued it, so this cannot stop somebody else's work. The copy says what
/// was stopped and that nothing was sent to a model, and it carries no exclamation
/// mark and no em dash.
fn stop_device_turns(service: &DeviceService, device_id: &str) -> Result<String, DeviceError> {
    let store = service.hub().store();
    let live = store.live_jobs(device_id).map_err(|error| {
        DeviceError::db_unavailable(format!("cannot read the turn list: {error}"))
    })?;
    let mut stopped = 0usize;
    for job_id in &live {
        match service.hub().cancel(job_id) {
            Ok(Some(_)) => stopped += 1,
            Ok(None) => {}
            Err(error) => {
                return Err(DeviceError::db_unavailable(format!(
                    "cannot cancel a turn: {error}"
                )));
            }
        }
    }
    Ok(match stopped {
        0 => "Nothing was running on this device, so there was nothing to stop.".to_owned(),
        1 => "Stopped the one turn that was running on this device: its stream ends with a \
              cancelled frame, and nothing else was sent to a model."
            .to_owned(),
        count => format!(
            "Stopped the {count} turns that were running on this device: each stream ends with a \
             cancelled frame, and nothing else was sent to a model."
        ),
    })
}

// ---------------------------------------------------------------------------
// Projects
// ---------------------------------------------------------------------------

/// `GET /v1/projects` - the project list, in the order the rail shows it.
///
/// A project is conversation state, so this is the `sessions` scope, exactly as
/// `GET /v1/sessions/{id}` is. Nothing here depends on which device asked: the
/// list is the operator's filing system, and a paired device holds no admin
/// scope to change what it means.
pub async fn projects(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Sessions) {
        return error.into_response();
    }
    let projects = match crate::projects::list(service.engine().db()) {
        Ok(projects) => projects,
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot read the project list: {error}"))
                .into_response();
        }
    };
    let projects: Vec<serde_json::Value> = projects
        .into_iter()
        .map(|project| {
            serde_json::json!({
                "id": project.id,
                "name": project.name,
                "created_at": project.created_at,
            })
        })
        .collect();
    axum::Json(serde_json::json!({ "ok": true, "projects": projects })).into_response()
}

/// `POST /v1/projects` - name a project, `201` with the row that was created.
///
/// The name rules are `crate::projects`' own, so a blank, oversized or duplicate
/// name is refused by the same code that refuses it on the dashboard, and the
/// refusal names the field it came from.
pub async fn create_project(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Sessions) {
        return error.into_response();
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let name = field(&fields, "project_name").to_owned();
    let created = match crate::projects::create(service.engine().db(), &name) {
        Ok(id) => id,
        Err(crate::db::DbError::InvalidProjectName { max }) => {
            return DeviceError::bad_request(
                "project_name_invalid",
                format!("a project name is 1 to {max} characters"),
            )
            .into_response();
        }
        Err(crate::db::DbError::DuplicateProjectName { name }) => {
            return DeviceError::bad_request(
                "project_name_duplicate",
                format!("a project named `{name}` already exists"),
            )
            .into_response();
        }
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot create the project: {error}"))
                .into_response();
        }
    };
    // The actor is the device, not `loopback`: the dashboard logs a person at a
    // browser, and this row has to say which paired device changed the filing.
    if let Err(error) = service.engine().auth().audit(
        token.device_id(),
        "projects.created",
        Some(&format!("{name}; id={created}")),
    ) {
        return DeviceError::db_unavailable(format!(
            "cannot append the projects.created audit row: {error}"
        ))
        .into_response();
    }
    // The row that was just written, read back rather than assembled from the
    // request: the stamp on it is the database's, and a client that files a
    // conversation under this project should be able to show the same one.
    let created_at = match crate::projects::list(service.engine().db()) {
        Ok(projects) => projects
            .into_iter()
            .find(|project| project.id == created)
            .map(|project| project.created_at)
            .unwrap_or_default(),
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot read the project row: {error}"))
                .into_response();
        }
    };
    (
        StatusCode::CREATED,
        axum::Json(serde_json::json!({
            "ok": true,
            "project": { "id": created, "name": name.trim(), "created_at": created_at },
        })),
    )
        .into_response()
}

/// `POST /v1/projects/assign` - file one conversation under a project, or take it
/// out of one.
///
/// An empty `project` field is the select's "No project" entry, which is a clear
/// rather than a missing value. Nothing changed is not a success: a conversation
/// or project the daemon does not know is refused by code, because a client that
/// reads `ok` must be able to believe it.
pub async fn assign_project(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Sessions) {
        return error.into_response();
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let conversation = field(&fields, "conversation").to_owned();
    if conversation.is_empty() {
        return DeviceError::bad_request(
            "conversation_missing",
            "the body did not name a conversation",
        )
        .into_response();
    }
    let requested = field(&fields, "project").to_owned();
    let project = if requested.is_empty() {
        None
    } else {
        Some(requested.as_str())
    };
    match crate::projects::assign(service.engine().db(), &conversation, project) {
        Ok(0) => DeviceError::bad_request(
            "project_assign_refused",
            "that conversation or that project is not one this daemon knows",
        )
        .into_response(),
        Ok(_) => {
            let detail = match project {
                Some(id) => format!("{conversation}; project={id}"),
                None => format!("{conversation}; project=none"),
            };
            if let Err(error) =
                service
                    .engine()
                    .auth()
                    .audit(token.device_id(), "projects.assigned", Some(&detail))
            {
                return DeviceError::db_unavailable(format!(
                    "cannot append the projects.assigned audit row: {error}"
                ))
                .into_response();
            }
            axum::Json(serde_json::json!({
                "ok": true,
                "conversation": conversation,
                "project": project,
            }))
            .into_response()
        }
        Err(error) => DeviceError::db_unavailable(format!("cannot file the conversation: {error}"))
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// Conversations
// ---------------------------------------------------------------------------

/// `GET /v1/conversations` - the newest conversations, newest first.
///
/// A conversation is conversation state, so this is the `sessions` scope, exactly
/// as `GET /v1/sessions/{id}` is. The page is bounded and says whether it was cut:
/// a client that has more history than the page is told so rather than left to
/// believe it has read everything.
pub async fn list_conversations(
    State(service): State<DeviceService>,
    headers: HeaderMap,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Sessions) {
        return error.into_response();
    }
    let db = service.engine().db();
    // One row beyond the page: "there is more" is then a fact about the store
    // rather than an assumption about the query.
    let rows = match chat::conversations(db, CONVERSATION_LIST_LIMIT + 1) {
        Ok(rows) => rows,
        Err(error) => {
            return DeviceError::db_unavailable(format!(
                "cannot read the conversation list: {error}"
            ))
            .into_response();
        }
    };
    let truncated = rows.len() > CONVERSATION_LIST_LIMIT;
    let rows: Vec<chat::Conversation> = rows.into_iter().take(CONVERSATION_LIST_LIMIT).collect();
    let counts = match chat::message_counts(db) {
        Ok(counts) => counts,
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot count the messages: {error}"))
                .into_response();
        }
    };
    let conversations: Vec<serde_json::Value> = rows
        .into_iter()
        .map(|row| {
            let message_count = counts.get(&row.id).copied().unwrap_or(0);
            serde_json::json!({
                "id": row.id,
                "title": row.title,
                "updated_at": row.updated_at,
                "project_id": row.project_id,
                "message_count": message_count,
            })
        })
        .collect();
    axum::Json(serde_json::json!({
        "ok": true,
        "count": conversations.len(),
        "truncated": truncated,
        "conversations": conversations,
    }))
    .into_response()
}

/// `POST /v1/conversations` - start an empty conversation, `201` with its id.
///
/// The title is the existing placeholder constant, not a second spelling of it:
/// the first question names the conversation through the same code the browser
/// uses, so a device-created conversation is renameable by the browser and the
/// other way round.
pub async fn create_conversation(
    State(service): State<DeviceService>,
    headers: HeaderMap,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Sessions) {
        return error.into_response();
    }
    let id = match chat::create_conversation(service.engine().db()) {
        Ok(id) => id,
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot create the conversation: {error}"))
                .into_response();
        }
    };
    // The actor is the device, not `loopback`: this row says which paired client
    // started the conversation the transcript will be filed under.
    if let Err(error) = service.engine().auth().audit(
        token.device_id(),
        "conversation.created",
        Some(&format!("id={id}")),
    ) {
        return DeviceError::db_unavailable(format!(
            "cannot append the conversation.created audit row: {error}"
        ))
        .into_response();
    }
    (
        StatusCode::CREATED,
        axum::Json(serde_json::json!({
            "ok": true,
            "id": id,
            "title": chat::NEW_CONVERSATION_TITLE,
        })),
    )
        .into_response()
}

/// `GET /v1/conversations/{id}` - one conversation and its messages, oldest first.
///
/// An id the daemon does not hold is a typed 404 rather than an empty transcript:
/// a client that asked about a conversation it lost the id of has to be able to
/// tell "gone" from "nothing said yet".
pub async fn conversation(
    State(service): State<DeviceService>,
    Path(conversation_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Sessions) {
        return error.into_response();
    }
    let db = service.engine().db();
    let row = match chat::conversation(db, &conversation_id) {
        Ok(Some(row)) => row,
        Ok(None) => return DeviceError::conversation_not_found().into_response(),
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot read the conversation: {error}"))
                .into_response();
        }
    };
    let (messages, truncated) =
        match chat::load_bounded(db, &conversation_id, CONVERSATION_MESSAGE_LIMIT) {
            Ok(read) => read,
            Err(error) => {
                return DeviceError::db_unavailable(format!(
                    "cannot read the conversation messages: {error}"
                ))
                .into_response();
            }
        };
    let messages: Vec<serde_json::Value> = messages
        .into_iter()
        .map(|message| {
            serde_json::json!({
                "id": message.id,
                "role": message.role.as_str(),
                "body": message.body,
                "state": message.state.as_str(),
                "created_at": message.created_at,
                "reasoning": message.reasoning,
                "tools": message.tools,
            })
        })
        .collect();
    axum::Json(serde_json::json!({
        "ok": true,
        "id": row.id,
        "title": row.title,
        "messages": messages,
        "truncated": truncated,
    }))
    .into_response()
}

/// `POST /v1/conversations/{id}/clear` - drop that conversation's messages.
///
/// The deletion is [`chat::clear`], the same statement the dashboard's clear runs,
/// so a device clear and a browser clear cannot leave the transcript in two
/// different states. The conversation row stays: clearing a conversation is not
/// deleting it.
pub async fn clear_conversation(
    State(service): State<DeviceService>,
    Path(conversation_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Sessions) {
        return error.into_response();
    }
    let db = service.engine().db();
    match chat::conversation(db, &conversation_id) {
        Ok(Some(_)) => {}
        Ok(None) => return DeviceError::conversation_not_found().into_response(),
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot read the conversation: {error}"))
                .into_response();
        }
    }
    let cleared = match chat::clear(db, &conversation_id) {
        Ok(cleared) => cleared,
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot clear the conversation: {error}"))
                .into_response();
        }
    };
    if let Err(error) = service.engine().auth().audit(
        token.device_id(),
        "conversation.cleared",
        Some(&format!("id={conversation_id}; {cleared} messages")),
    ) {
        return DeviceError::db_unavailable(format!(
            "cannot append the conversation.cleared audit row: {error}"
        ))
        .into_response();
    }
    axum::Json(serde_json::json!({
        "ok": true,
        "cleared": cleared,
    }))
    .into_response()
}

// ---------------------------------------------------------------------------
// GET /v1/turns/{id}/events
// ---------------------------------------------------------------------------

/// The SSE stream for one job: replay strictly after `Last-Event-ID`, then tail.
///
/// A resume point older than the retention window is a `409 buffer_miss` with a
/// snapshot URL, never a partial stream that looks complete: a client that has
/// lost frames has to know it lost them.
pub async fn events(
    State(service): State<DeviceService>,
    Path(job_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Sessions) {
        return error.into_response();
    }

    let store = service.hub().store();
    let job = match store.job(&job_id) {
        Ok(Some(job)) if job.device_id.as_deref() == Some(token.device_id()) => job,
        Ok(_) => return DeviceError::job_not_found().into_response(),
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot read job state: {error}"))
                .into_response();
        }
    };

    let last_event_id = match headers.get(LAST_EVENT_ID_HEADER) {
        Some(value) => {
            let text = value.to_str().unwrap_or("").trim();
            match text.parse::<i64>() {
                Ok(seq) if seq >= 0 => seq,
                _ => {
                    return DeviceError::bad_request(
                        "last_event_id_invalid",
                        "Last-Event-ID must be a non-negative event sequence number",
                    )
                    .into_response();
                }
            }
        }
        None => 0,
    };

    // Subscribe before reading, so a frame written between the read and the tail
    // is delivered by the channel rather than lost in the gap.
    let receiver = service.hub().subscribe(&job_id);

    let replay = match store.events_after(&job_id, last_event_id) {
        Ok(replay) => replay,
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot read the event log: {error}"))
                .into_response();
        }
    };
    let oldest = match store.min_seq(&job_id) {
        Ok(oldest) => oldest,
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot read the event log: {error}"))
                .into_response();
        }
    };
    if let Some(oldest) = oldest
        && oldest > last_event_id.saturating_add(1)
    {
        return DeviceError::buffer_miss(job.session_id.as_deref().unwrap_or("")).into_response();
    }

    let replay_last = replay
        .last()
        .map(|frame| frame.seq)
        .unwrap_or(last_event_id);
    let replay_ended = replay.last().is_some_and(TurnFrame::is_terminal);
    let replay_events: Vec<Result<Event, Infallible>> =
        replay.iter().map(|frame| Ok(frame_event(frame))).collect();

    let stream: Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>> =
        match (replay_ended, receiver) {
            (false, Some(receiver)) => {
                let live = BroadcastStream::new(receiver).filter_map(move |item| match item {
                    Ok(frame) if frame.seq > replay_last => {
                        Some(Ok::<_, Infallible>(frame_event(&frame)))
                    }
                    // A lagged subscriber missed frames in this process. The ids
                    // it already has tell it what to ask for next, and a resume
                    // past the retention window gets the documented 409.
                    _ => None,
                });
                Box::pin(tokio_stream::iter(replay_events).chain(live))
            }
            // A job with no live worker (finished, or from an earlier process)
            // is answered from the log alone.
            _ => Box::pin(tokio_stream::iter(replay_events)),
        };

    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(std::time::Duration::from_secs(KEEP_ALIVE_SECONDS))
                .text("keep-alive"),
        )
        .into_response()
}

// ---------------------------------------------------------------------------
// POST /v1/turns/{id}/cancel
// ---------------------------------------------------------------------------

/// Stop a running turn. Idempotent in the sense that matters: a second cancel is
/// a `409 turn_not_cancellable`, not a second `cancelled` frame.
pub async fn cancel(
    State(service): State<DeviceService>,
    Path(job_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let store = service.hub().store();
    match store.job(&job_id) {
        Ok(Some(job)) if job.device_id.as_deref() == Some(token.device_id()) => {}
        Ok(_) => return DeviceError::job_not_found().into_response(),
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot read job state: {error}"))
                .into_response();
        }
    }
    match service.hub().cancel(&job_id) {
        Ok(Some(_)) => axum::Json(serde_json::json!({
            "ok": true,
            "state": "cancelled",
        }))
        .into_response(),
        Ok(None) => DeviceError::new(
            StatusCode::CONFLICT,
            "turn_not_cancellable",
            "the turn already finished: there is nothing left to cancel",
        )
        .into_response(),
        Err(error) => {
            DeviceError::db_unavailable(format!("cannot cancel the turn: {error}")).into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// GET /v1/sessions/{id}
// ---------------------------------------------------------------------------

/// The snapshot a `buffer_miss` points at: the session and every turn in it.
pub async fn session(
    State(service): State<DeviceService>,
    Path(session_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Sessions) {
        return error.into_response();
    }
    let store = service.hub().store();
    let row = match store.session(&session_id) {
        Ok(Some(row)) if row.device_id.as_deref() == Some(token.device_id()) => row,
        Ok(_) => return DeviceError::session_not_found().into_response(),
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot read the session: {error}"))
                .into_response();
        }
    };
    let state = if row.revoked_at.is_some() {
        "revoked"
    } else if crate::auth::parse_timestamp(&row.expires_at)
        .map(|at| crate::auth::now() >= at)
        .unwrap_or(true)
    {
        "expired"
    } else {
        "active"
    };
    let turns = match store.session_turns(&session_id) {
        Ok(turns) => turns,
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot read the session turns: {error}"))
                .into_response();
        }
    };
    let turns: Vec<serde_json::Value> = turns
        .into_iter()
        .map(|turn| {
            serde_json::json!({
                "job_id": turn.job_id,
                "state": turn.status,
                "text": turn.text,
                "created_at": turn.created_at,
                "finished_at": turn.finished_at,
            })
        })
        .collect();
    axum::Json(serde_json::json!({
        "ok": true,
        "session_id": session_id,
        "state": state,
        "turns": turns,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bearer_is_read_only_from_the_authorization_header() {
        let mut headers = HeaderMap::new();
        assert!(bearer(&headers).is_none());
        headers.insert(header::AUTHORIZATION, "Basic abc".parse().expect("value"));
        assert!(bearer(&headers).is_none());
        headers.insert(
            header::AUTHORIZATION,
            "Bearer  deadbeef ".parse().expect("value"),
        );
        assert_eq!(bearer(&headers), Some("deadbeef"));
        headers.insert(header::AUTHORIZATION, "Bearer   ".parse().expect("value"));
        assert!(bearer(&headers).is_none());
    }

    #[tokio::test]
    async fn a_form_body_decodes_and_a_json_body_decodes_to_the_same_map() {
        let mut headers = HeaderMap::new();
        let body = Body::from("code=AB2CD3EF&device_name=iPhone");
        let form = read_fields(&headers, body).await.expect("form body");
        assert_eq!(form.get("code").map(String::as_str), Some("AB2CD3EF"));
        assert_eq!(form.get("device_name").map(String::as_str), Some("iPhone"));

        headers.insert(
            header::CONTENT_TYPE,
            "application/json".parse().expect("value"),
        );
        let body = Body::from(r#"{"code":"AB2CD3EF","device_name":"iPhone"}"#);
        let json = read_fields(&headers, body).await.expect("json body");
        assert_eq!(json, form);
    }

    #[test]
    fn a_frame_reports_its_own_termination() {
        let delta = TurnFrame {
            seq: 1,
            kind: FrameKind::Delta,
            data: serde_json::json!({ "text": "ok" }),
        };
        assert!(!delta.is_terminal());
        for kind in [FrameKind::Done, FrameKind::Error, FrameKind::Cancelled] {
            let frame = TurnFrame {
                seq: 2,
                kind,
                data: serde_json::Value::Null,
            };
            assert!(frame.is_terminal(), "{kind:?} must end the stream");
        }
    }
}
