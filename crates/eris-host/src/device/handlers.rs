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
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use tokio_stream::Stream;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::approvals::{self, Decision};
use crate::auth::{
    PairingService, Scope, ScopeSet, TokenOrigin, TokenService, VerifiedToken, format_timestamp,
};
use crate::device::turns::{ConversationTurn, FrameKind, TurnFrame, TurnPolicy};
use crate::device::{
    APPROVAL_LIST_LIMIT, AUDIT_DEFAULT_LIMIT, AUDIT_MAX_LIMIT, CONVERSATION_LIST_LIMIT,
    CONVERSATION_MESSAGE_LIMIT, DEVICE_LIST_LIMIT, DeviceError, DeviceService,
};
use crate::inbox::{InboxActionRequest, InboxStore, InternalInboxCreateRequest};
use crate::notes::{
    BlobDownloadOutcome, BlobUploadOutcome, NotesStore, PatchOutcome, RecognitionOutcome,
    RunCreateOutcome,
};
use crate::onboarding::{OnboardingError, PermissionError, Permissions, SetupEngine};
use crate::push::{PushRegisterRequest, PushStore};
use crate::sensors::{Delivered, HealthLatest, HealthUpload, LocationUpload};
use crate::web::chat;
use serde::Deserialize;

/// The upload ceiling for one device body. A prompt is text, and text is not
/// unbounded.
const MAX_BODY_BYTES: usize = 64 * 1024;

/// Header carrying a client's idempotency key.
const IDEMPOTENCY_HEADER: &str = "idempotency-key";

/// Longest `client_message_id` this surface will store.
///
/// A UUID is 36 characters and a ULID is 26; 128 leaves room for another scheme
/// without leaving room for a body. Past it the id is dropped rather than cut,
/// because half an id matches nothing and pretending otherwise would be worse
/// than saying the row has no client id at all.
const MAX_CLIENT_MESSAGE_ID: usize = 128;

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
pub(crate) fn authenticate(
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
pub(crate) fn require_scope(token: &VerifiedToken, scope: Scope) -> Result<(), DeviceError> {
    token.scope().require(scope).map_err(|_| {
        DeviceError::new(
            StatusCode::FORBIDDEN,
            "scope_denied",
            format!("the token does not carry the {scope} scope"),
        )
    })
}

/// The administration check, with a refusal that says what actually happened.
///
/// A device whose pairing code was minted without the administration box gets
/// this rather than a bare "forbidden": the scope is missing because nobody
/// asked for it when the code was minted, and this message is the only place
/// that is stated out loud.
pub(crate) fn require_admin(token: &VerifiedToken) -> Result<(), DeviceError> {
    token.scope().require(Scope::Admin).map_err(|_| {
        DeviceError::new(
            StatusCode::FORBIDDEN,
            "scope_denied",
            "this device was paired without administration, so its token carries no admin scope \
             and it cannot read or change the settings: mint a pairing code with the \
             administration box ticked and pair the device again",
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
pub(crate) async fn read_fields(
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

/// The payload of a terminal frame this handler had to synthesise.
///
/// The shapes are the ones a worker writes for the same kind, so a client that
/// decodes a stored frame decodes this one with the same code. `text` is only
/// used by `done`; it comes from the retained deltas so the ending still shows
/// what the turn had said.
fn synthetic_terminal(kind: FrameKind, text: String) -> serde_json::Value {
    match kind {
        FrameKind::Done => {
            let usage =
                core_domain::Usage::unavailable(core_domain::UnavailableReason::FieldsAbsent);
            serde_json::json!({ "text": text, "usage": usage })
        }
        FrameKind::Error => serde_json::json!({
            "code": "interrupted",
            "message": "the turn ended without recording a terminal frame",
        }),
        FrameKind::Cancelled => serde_json::json!({ "reason": "cancel" }),
        // None of these ends a turn, so none of them is ever synthesised: a job
        // with no terminal frame gets one of the three above. `asked` is here
        // because a parked question is the opposite of an ending - the turn is
        // alive and waiting on a person, and inventing a question nobody asked
        // would be worse than leaving the stream short.
        FrameKind::Delta | FrameKind::Reasoning | FrameKind::Tool | FrameKind::Asked => {
            serde_json::Value::Null
        }
    }
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

    // The scope set is the code's own decision, read back rather than inferred:
    // an ordinary code yields the device defaults, and a code the operator
    // minted with the administration box ticked yields those plus `admin`. A
    // code minted before this flag existed reads as an ordinary code, because
    // the column defaults to zero.
    let scopes = if redeemed.admin() {
        ScopeSet::device_default().with(Scope::Admin)
    } else {
        ScopeSet::device_default()
    };
    let origin = if redeemed.admin() {
        TokenOrigin::PairedAdmin
    } else {
        TokenOrigin::Device
    };
    let minted = match tokens.mint_pair(&device_id, scopes, origin) {
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
    // The access token inherits the scope set of the row that was verified: a
    // device the operator asked to administer keeps that across a refresh, and a
    // device that was paired without it cannot acquire it here. Refreshing
    // decides nothing, it carries a decision that was already made.
    let origin = if verified.scope().has_admin() {
        TokenOrigin::PairedAdmin
    } else {
        TokenOrigin::Device
    };
    let access = match tokens.mint_access(verified.device_id(), verified.scope(), origin) {
        Ok(access) => access,
        Err(error) => {
            return DeviceError::unauthorized("token_invalid", error.to_string()).into_response();
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
    // The device's own id for the message it just sent, when it sent one. Bounded
    // and trimmed here rather than trusted: it is a copy of a client's value, and
    // a device that pastes a kilobyte into it should not grow a transcript row by
    // a kilobyte. Anything longer is treated as absent rather than truncated, so
    // the id a client gets back is always the id it sent or nothing at all.
    let client_message_id = {
        let raw = field(&fields, "client_message_id");
        if raw.is_empty() || raw.len() > MAX_CLIENT_MESSAGE_ID {
            None
        } else {
            Some(raw.to_owned())
        }
    };

    let conversation_turn = match &conversation {
        None => None,
        Some(conversation_id) => {
            let db = service.engine().db();
            if let Err(error) = chat::append_identified(
                db,
                conversation_id,
                chat::Role::User,
                text,
                chat::State::Complete,
                client_message_id.as_deref(),
            ) {
                return DeviceError::db_unavailable(format!(
                    "cannot write the question into the conversation: {error}"
                ))
                .into_response();
            }
            // The model this conversation is on, and the window that belongs to
            // the provider serving it. Asking the primary for the window is right
            // only while the pin is on the primary: a conversation the composer
            // moved to a second provider would claim the first one's window and
            // the meter in the client would lie about what the turn can hold.
            let turn_provider = store.provider_config().ok().flatten();
            let model = turn_provider
                .as_ref()
                .map(|config| config.model.clone())
                .unwrap_or_default();
            let context_window = turn_provider.as_ref().and_then(|config| {
                service
                    .engine()
                    .provider_contexts_for(&config.provider_id)
                    .ok()
                    .and_then(|contexts| contexts.get(&config.model).copied())
            });
            let question_id = match chat::question_id(db, conversation_id) {
                Ok(Some(id)) => id,
                // Reading back is not allowed to fail the turn: the rows are
                // already written, and a question id nobody could read means the
                // job is simply not stamped. A turn that answers is worth more
                // than a turn whose two rows are labelled.
                Ok(None) => String::new(),
                Err(error) => {
                    return DeviceError::db_unavailable(format!(
                        "cannot read the question row back: {error}"
                    ))
                    .into_response();
                }
            };
            let answer_id = match chat::append_answer(db, conversation_id, &model, context_window) {
                Ok(answer_id) => answer_id,
                Err(error) => {
                    return DeviceError::db_unavailable(format!(
                        "cannot open the answer row: {error}"
                    ))
                    .into_response();
                }
            };
            // The first question names the conversation, the model is recorded on
            // it, and the rail bumps it to the top, through the same helpers the
            // browser calls. Recording the model is what lets the picker restore
            // what this conversation is actually on when the operator comes back
            // to it, instead of quietly moving it to the global default.
            let _ = chat::name_from_first_message(db, conversation_id, text);
            let _ = chat::set_conversation_model(db, conversation_id, &model);
            let _ = chat::touch_conversation(db, conversation_id);
            // The naming role, when the operator pinned one. A role on the primary
            // is a no-op, so this is `None` on every install that never chose a
            // naming model.
            let title = chat::aux_pinned_config(service.engine(), "title_generation")
                .ok()
                .flatten();
            Some(ConversationTurn {
                conversation: conversation_id.clone(),
                question_id,
                answer_id,
                question: text.to_owned(),
                title,
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
/// nothing for either one to act on. `/model` and `/memory` are settings writes,
/// and settings have exactly one path on this listener: `POST /v1/settings`,
/// behind the admin scope. A slash command is not a second path into them, so it
/// is refused by name whatever the token holds, rather than silently skipped or
/// emulated into something that looks like it worked.
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
                         model and memory backend are changed through the settings route rather \
                         than from a command here, so nothing was changed and nothing was sent \
                         to a model. /help lists what a device turn answers."
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

/// The query `GET /v1/conversations` accepts: `sort`, one of the four modes.
///
/// A string rather than an enum so an unknown value is [`ConversationSort::parse`]'s
/// business: it answers `updated` for anything it does not recognize, which keeps
/// a bad query string from looking like an empty archive.
#[derive(serde::Deserialize)]
pub struct ConversationListQuery {
    sort: Option<String>,
}

/// One conversation as the device list spells it.
fn conversation_row(row: &chat::Conversation, message_count: usize) -> serde_json::Value {
    serde_json::json!({
        "id": row.id,
        "title": row.title,
        "updated_at": row.updated_at,
        "created_at": row.created_at,
        "project_id": row.project_id,
        "message_count": message_count,
    })
}

/// `GET /v1/conversations?sort=` - the conversations, in the operator's order.
///
/// A conversation is conversation state, so this is the `sessions` scope, exactly
/// as `GET /v1/sessions/{id}` is. The page is bounded and says whether it was cut:
/// a client that has more history than the page is told so rather than left to
/// believe it has read everything.
///
/// The flat `conversations` array is the same shape it always was, plus
/// `created_at`, and every mode fills it. `day` additionally fills `groups`, whose
/// headings come from the operator's own clock: `Today`, `Yesterday`, `Mar 14`.
pub async fn list_conversations(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    Query(query): Query<ConversationListQuery>,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Sessions) {
        return error.into_response();
    }
    let sort = chat::ConversationSort::parse(query.sort.as_deref());
    let db = service.engine().db();
    // One row beyond the page: "there is more" is then a fact about the store
    // rather than an assumption about the query.
    let rows = match chat::conversations_sorted(db, CONVERSATION_LIST_LIMIT + 1, sort) {
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
    let counted = |row: &chat::Conversation| counts.get(&row.id).copied().unwrap_or(0);
    let conversations: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| conversation_row(row, counted(row)))
        .collect();
    let mut body = serde_json::json!({
        "ok": true,
        "count": conversations.len(),
        "truncated": truncated,
        "sort": sort.as_str(),
        "conversations": conversations,
    });
    if sort == chat::ConversationSort::Day {
        // The day boundary is the operator's, and SQLite does not know their
        // zone, so the grouping happens here with the same offset every stamp on
        // the transcript uses.
        // A failed read here is a database that cannot answer, which is the
        // same fault the list above would have hit; it is reported rather than
        // turned into a silent UTC grouping.
        let stored_zone = match service.engine().setting(crate::zone::SETTING_TIMEZONE) {
            Ok(stored) => stored,
            Err(error) => return settings_unavailable(error).into_response(),
        };
        let zone = crate::zone::Zone::parse(stored_zone.as_deref());
        let offset = crate::zone::offset_minutes(db, zone);
        let days = chat::group_by_day(rows, offset, chat::today_on(offset));
        let groups: Vec<serde_json::Value> = days
            .iter()
            .map(|day| {
                serde_json::json!({
                    "key": day.key,
                    "label": day.label,
                    "conversations": day
                        .conversations
                        .iter()
                        .map(|row| conversation_row(row, counted(row)))
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        if let Some(object) = body.as_object_mut() {
            object.insert("groups".to_owned(), serde_json::Value::Array(groups));
        }
    }
    axum::Json(body).into_response()
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
                // The two handles a device needs to recognise its own rows. Sent
                // as explicit nulls rather than omitted, so a client can tell
                // "this row predates the column" from "the daemon forgot".
                "client_message_id": message.client_message_id,
                "job_id": message.job_id,
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

    let mut replay = match store.events_after(&job_id, last_event_id) {
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
    let mut replay_ended = replay.last().is_some_and(TurnFrame::is_terminal);

    // A job that is already terminal will never be written to again, but its
    // log can still be missing the terminal frame: a process that died between
    // the last delta and the finish, or a sweep that has not run yet. Tailing
    // such a stream would wait for an ending nobody can publish. Synthesise the
    // frame the status promises, for this response only. The boot sweep owns
    // persistence; a read path never writes.
    if !replay_ended && let Some(kind) = FrameKind::terminal_for_status(&job.status) {
        let text = if kind == FrameKind::Done {
            store.reply_text(&job_id).unwrap_or_default()
        } else {
            String::new()
        };
        replay.push(TurnFrame {
            seq: replay_last.saturating_add(1),
            kind,
            data: synthetic_terminal(kind, text),
        });
        replay_ended = true;
    }

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
// GET /v1/turns/{id}/asked and POST /v1/turns/{id}/answer
// ---------------------------------------------------------------------------

/// The job, if it is this device's. A turn belonging to another device is
/// reported as missing rather than forbidden: the id is not a secret a stranger
/// should be able to confirm by watching for the difference.
fn own_job(
    service: &DeviceService,
    job_id: &str,
    device: &str,
) -> Result<crate::device::turns::JobRow, DeviceError> {
    match service.hub().store().job(job_id) {
        Ok(Some(job)) if job.device_id.as_deref() == Some(device) => Ok(job),
        Ok(_) => Err(DeviceError::job_not_found()),
        Err(error) => Err(DeviceError::db_unavailable(format!(
            "cannot read job state: {error}"
        ))),
    }
}

/// `GET /v1/turns/{id}/asked` - the question this turn is waiting on, if any.
///
/// The question also rides the job stream as an `asked` frame. This route is for
/// the client that was not listening at that moment: a phone whose stream dropped,
/// or one that opened the job after the card had gone out. It reads the live
/// registry rather than the job log, because the interesting question is whether
/// somebody can still answer - a settled or abandoned question is not answerable,
/// and a card that says "waiting" when nothing is would be worse than no card.
pub async fn asked(
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
    if let Err(error) = own_job(&service, &job_id, token.device_id()) {
        return error.into_response();
    }
    let mut body = serde_json::json!({
        "ok": true,
        "waiting": false,
        "asked": serde_json::Value::Null,
    });
    // A service with no registry cannot have a question parked, and says so as
    // "nothing waiting" rather than as an error: there is nothing wrong with the
    // request, there is just nobody asking.
    if let Some(registry) = service.clarify() {
        // The key this job's question is parked under. A turn that named no
        // conversation asks under its own job id; one that named a conversation
        // asks under that id, so the dashboard's answer route finds it too.
        let key = service
            .hub()
            .ask_key(&job_id)
            .unwrap_or_else(|| job_id.clone());
        if let Some((id, request)) = registry.waiting_for(&key) {
            body["waiting"] = serde_json::Value::Bool(true);
            body["asked"] = serde_json::json!({
                "id": id,
                "request": request,
            });
        }
    }
    axum::Json(body).into_response()
}

/// `POST /v1/turns/{id}/answer` - settle the question a turn is waiting on.
///
/// This is the resume half of `/v1/turns`. A `clarify` call parks the turn's own
/// blocking thread, so an answer landing here is what lets that thread continue
/// with the answer as the tool result. The answer lands on the same registry the
/// dashboard's `/dashboard/chat/answer` lands on, which is deliberate: a question
/// asked by a phone turn is answerable from the browser, and one asked by a browser
/// turn is answerable from the phone.
///
/// Two shapes of answer, the two the card can produce: picked labels (optionally
/// with a note), or typed words with nothing picked. Anything else is refused by
/// name, because a settle that landed nothing would leave the turn waiting out the
/// hour for an answer nobody sent.
pub async fn answer_asked(
    State(service): State<DeviceService>,
    Path(job_id): Path<String>,
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
    if let Err(error) = own_job(&service, &job_id, token.device_id()) {
        return error.into_response();
    }
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    // A job that is already terminal has no turn left to wake. Saying "answered"
    // for a question whose thread is gone would report a decision nobody received.
    if let Ok(Some(status)) = service.hub().store().status(&job_id)
        && !matches!(status.as_str(), "queued" | "running")
    {
        return DeviceError::new(
            StatusCode::CONFLICT,
            "turn_finished",
            format!("the turn already finished ({status}): there is no question left to answer"),
        )
        .into_response();
    }
    let question = {
        let raw = field(&fields, "question").to_owned();
        if raw.is_empty() {
            // The card knows which question it is; a client that did not repeat it
            // still gets a usable sentence rather than an empty quote.
            "the question".to_owned()
        } else {
            raw
        }
    };
    // Labels arrive separated by the unit separator, the same wire shape the
    // dashboard's answer route takes, so one card's script serves both surfaces.
    let labels: Vec<String> = field(&fields, "labels")
        .split('\u{1f}')
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_owned)
        .collect();
    let note = field(&fields, "note").trim().to_owned();
    let typed = field(&fields, "text").trim().to_owned();
    let answer = if !labels.is_empty() {
        crate::web::clarify::Answer::Chosen {
            question,
            labels,
            note: (!note.is_empty()).then_some(note),
        }
    } else if !typed.is_empty() {
        crate::web::clarify::Answer::Typed {
            question,
            text: typed,
        }
    } else {
        return DeviceError::bad_request(
            "answer_empty",
            "an answer needs a picked choice or some words, and this one had neither",
        )
        .into_response();
    };

    let Some(registry) = service.clarify() else {
        return DeviceError::new(
            StatusCode::CONFLICT,
            "nothing_waiting",
            "this daemon has no question registry, so no turn is waiting on an answer",
        )
        .into_response();
    };
    let key = service
        .hub()
        .ask_key(&job_id)
        .unwrap_or_else(|| job_id.clone());
    let landed = registry.answer(&key, answer.clone());
    let detail = format!(
        "job={job_id}; conversation={key}; {}",
        if landed { "settled" } else { "nothing waiting" }
    );
    if let Err(error) =
        service
            .engine()
            .auth()
            .audit(token.device_id(), "chat.answer", Some(&detail))
    {
        return DeviceError::db_unavailable(format!(
            "the answer was settled but the audit row could not be appended: {error}"
        ))
        .into_response();
    }
    if !landed {
        return DeviceError::new(
            StatusCode::CONFLICT,
            "nothing_waiting",
            "this turn is not waiting on a question: it was already answered, timed \
             out, or the turn has ended",
        )
        .into_response();
    }
    axum::Json(serde_json::json!({
        "ok": true,
        "rendered": answer.render(),
    }))
    .into_response()
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

// ---------------------------------------------------------------------------
// /v1/settings
// ---------------------------------------------------------------------------

/// The six groups a capability can belong to.
///
/// These are the web settings tabs and the iOS settings sections, in the order
/// both render them. **There is no seventh.** A group that is not here has no tab
/// on the web page and no section on the phone, so a field in it would be
/// unreachable on both surfaces while looking present in the record.
pub const SETTING_GROUPS: [&str; 6] = [
    "models",
    "assistant",
    "memory",
    "devices",
    "automation",
    "system",
];

/// What kind of value a setting holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingKind {
    /// Any text, within the field's own length limits.
    Text,
    /// One of a closed list.
    Enum(&'static [&'static str]),
    /// `true` or `false`.
    Bool,
    /// A whole number, inclusive of both bounds.
    ///
    /// The bounds are the range the reader itself honours, so a value accepted
    /// here is a value the record reads back unchanged. An unbounded `int` would
    /// let a write pin 100000 rounds while every read clamped it back to the
    /// default, and the record would then disagree with the row it came from.
    Int(u32, u32),
    /// A time zone: `host`, `UTC`, or an offset like `-04:00`.
    ///
    /// Its own kind rather than [`Self::Text`] because a zone that does not parse
    /// is silently the host zone, so a settings screen that accepted `Mars` and
    /// then showed `host` would be lying twice.
    Zone,
}

impl SettingKind {
    /// The `type` the record spells for this kind.
    ///
    /// A zone is a string on the wire whose shape a client can check the same
    /// way, so it is spelled `string`: the record's four types are the four a
    /// form control is chosen from, and a fifth would break both clients.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text | Self::Zone => "string",
            Self::Enum(_) => "enum",
            Self::Bool => "bool",
            Self::Int(_, _) => "int",
        }
    }

    /// The `options` the record spells: the closed list for an enum, `null`
    /// otherwise.
    #[must_use]
    pub fn options(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Enum(values) => Some(values),
            _ => None,
        }
    }
}

/// One operator-facing setting, described once.
///
/// The record is built by iterating [`SETTING_SPECS`], and a write looks its key
/// up in the same table. One table, two uses, so the record cannot offer a field
/// the write refuses, and the write cannot accept a field the record never
/// mentions.
pub struct SettingSpec {
    /// The field name, which is also the write body's field name.
    pub key: &'static str,
    /// What the operator reads beside it.
    pub label: &'static str,
    /// One of [`SETTING_GROUPS`].
    pub group: &'static str,
    /// The value's kind.
    pub kind: SettingKind,
    /// The `settings` row this reads and writes.
    pub row: &'static str,
    /// Whether `POST /v1/settings` accepts this key today.
    pub writable: bool,
    /// The value as a string, in the spelling a write uses.
    pub read: fn(&SetupEngine) -> Result<String, DeviceError>,
    /// How a submitted value is tidied before it is stored, when the row has a
    /// shape of its own.
    ///
    /// `None` means the kind's own rule is the whole rule. A comma-separated
    /// device list is the case that needs it: trimmed, deduplicated and joined,
    /// so the row holds exactly what the record reads back, and the audit detail
    /// quotes the value that was actually written rather than the one that was
    /// typed.
    pub normalize: Option<fn(&str) -> String>,
}

/// A refusal for a value a closed list does not hold.
fn kind_refusal(spec: &SettingSpec, submitted: &str, allowed: &str) -> DeviceError {
    if allowed.is_empty() {
        return DeviceError::bad_request(
            "settings_value_invalid",
            format!("{} does not accept {}", spec.key, shown_value(submitted)),
        );
    }
    refuse_value(spec.key, "settings_value_invalid", submitted, allowed)
}

/// Check one submitted value against the spec's kind, or say what it accepts.
///
/// This is the whole validation for every setting except the four with a rule of
/// their own: a model has to be an id the endpoint publishes, the permission pair
/// is checked together, and the vault backend needs a directory that exists.
pub fn validate_setting(spec: &SettingSpec, raw: &str) -> Result<(), DeviceError> {
    let trimmed = raw.trim();
    match spec.kind {
        SettingKind::Text => Ok(()),
        SettingKind::Enum(values) => {
            if values
                .iter()
                .any(|known| known.eq_ignore_ascii_case(trimmed))
            {
                Ok(())
            } else {
                Err(kind_refusal(spec, trimmed, &values.join(", ")))
            }
        }
        SettingKind::Bool => match trimmed.to_ascii_lowercase().as_str() {
            "true" | "false" => Ok(()),
            _ => Err(kind_refusal(spec, trimmed, "true or false")),
        },
        SettingKind::Int(min, max) => {
            let allowed = format!("a whole number from {min} to {max}");
            match trimmed.parse::<u32>() {
                Ok(number) if number >= min && number <= max => Ok(()),
                _ => Err(kind_refusal(spec, trimmed, &allowed)),
            }
        }
        SettingKind::Zone => crate::zone::validate(trimmed)
            .map(|_| ())
            .map_err(|reason| DeviceError::bad_request("settings_value_invalid", reason)),
    }
}

/// The value of a `settings` row, or the empty string when it is absent.
fn row_of(engine: &SetupEngine, row: &'static str) -> Result<Option<String>, DeviceError> {
    engine.setting(row).map_err(settings_unavailable)
}

fn read_model(engine: &SetupEngine) -> Result<String, DeviceError> {
    engine
        .model()
        .map_err(settings_unavailable)
        .map(|value| value.unwrap_or_default())
}

fn read_tools_enabled(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(
        crate::tools::enabled(row_of(engine, crate::tools::SETTING_TOOLS_ENABLED)?.as_deref())
            .to_string(),
    )
}

fn read_tool_rounds(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(
        crate::tools::max_rounds(row_of(engine, crate::tools::SETTING_TOOL_ROUNDS)?.as_deref())
            .to_string(),
    )
}

fn read_sandbox(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(engine
        .permissions()
        .map_err(settings_unavailable)?
        .sandbox_mode
        .as_str()
        .to_owned())
}

fn read_approval(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(engine
        .permissions()
        .map_err(settings_unavailable)?
        .approval_policy
        .as_str()
        .to_owned())
}

fn read_memory(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(
        crate::memory::backend(row_of(engine, crate::memory::SETTING_BACKEND)?.as_deref())
            .as_str()
            .to_owned(),
    )
}

fn read_timezone(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(
        crate::zone::Zone::parse(row_of(engine, crate::zone::SETTING_TIMEZONE)?.as_deref())
            .as_str()
            .to_owned(),
    )
}

fn read_clock_format(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(crate::zone::ClockFormat::parse(
        row_of(engine, crate::zone::SETTING_CLOCK_FORMAT)?.as_deref(),
    )
    .as_str()
    .to_owned())
}

fn read_conversation_sort(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(crate::web::chat::ConversationSort::parse(
        row_of(engine, crate::web::chat::SETTING_CONVERSATION_SORT)?.as_deref(),
    )
    .as_str()
    .to_owned())
}

fn read_notify_on_reply(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(crate::push::notify_on_reply_enabled(
        row_of(engine, crate::push::SETTING_NOTIFY_ON_REPLY)?.as_deref(),
    )
    .to_string())
}

fn read_notify_scope(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(
        crate::push::notify_scope(row_of(engine, crate::push::SETTING_NOTIFY_SCOPE)?.as_deref())
            .as_str()
            .to_owned(),
    )
}

fn read_notify_devices(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(notify_devices_value(
        row_of(engine, crate::push::SETTING_NOTIFY_DEVICES)?
            .as_deref()
            .unwrap_or_default(),
    ))
}

/// A submitted device list, in the one spelling the row holds.
fn normalize_notify_devices(raw: &str) -> String {
    notify_devices_value(raw)
}

/// The stored spelling of a device list: trimmed, deduplicated, comma-joined.
fn notify_devices_value(raw: &str) -> String {
    crate::push::notify_devices(Some(raw)).join(",")
}

fn read_assistant_name(engine: &SetupEngine) -> Result<String, DeviceError> {
    let stored = row_of(engine, crate::persona::SETTING_ASSISTANT_NAME)?;
    Ok(crate::persona::assistant_name(stored.as_deref()).to_owned())
}

fn read_persona_enabled(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(crate::persona::enabled_flag(
        row_of(engine, crate::persona::SETTING_PERSONA_ENABLED)?.as_deref(),
    )
    .to_string())
}

fn read_show_reasoning(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(crate::web::chat::show_reasoning(
        row_of(engine, crate::web::chat::SETTING_SHOW_REASONING)?.as_deref(),
    )
    .to_string())
}

fn read_show_tool_log(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(crate::web::chat::tool_log(
        row_of(engine, crate::web::chat::SETTING_SHOW_TOOL_LOG)?.as_deref(),
    )
    .as_str()
    .to_owned())
}

fn read_stream_responses(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(crate::web::chat::streaming_enabled(
        row_of(engine, crate::web::chat::SETTING_STREAMING)?.as_deref(),
    )
    .to_string())
}

fn read_show_quota(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(crate::web::quota::show_quota(
        row_of(engine, crate::web::quota::SETTING_SHOW_QUOTA)?.as_deref(),
    )
    .to_string())
}

fn read_reply_state(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(crate::web::chat::reply_state(
        row_of(engine, crate::web::chat::SETTING_REPLY_STATE)?.as_deref(),
    )
    .as_str()
    .to_owned())
}

fn read_greeting_enabled(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(crate::greeting::enabled(
        row_of(engine, crate::greeting::SETTING_GREETING_ENABLED)?.as_deref(),
    )
    .to_string())
}

fn read_greeting_refresh(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(crate::greeting::Refresh::parse(
        row_of(engine, crate::greeting::SETTING_GREETING_REFRESH)?.as_deref(),
    )
    .as_str()
    .to_owned())
}

fn read_agents_enabled(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(crate::subagents::enabled(
        row_of(engine, crate::subagents::SETTING_AGENTS_ENABLED)?.as_deref(),
    )
    .to_string())
}

fn read_schedules_enabled(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(
        crate::scheduler::enabled(row_of(engine, crate::scheduler::SETTING_ENABLED)?.as_deref())
            .to_string(),
    )
}

fn read_skills_enabled(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(
        crate::skills::enabled(row_of(engine, crate::skills::SETTING_ENABLED)?.as_deref())
            .to_string(),
    )
}

/// The workspace a turn may write in. Read-only here: it is set by the wizard and
/// moving it from a phone is a different decision with a different card.
fn read_workspace(engine: &SetupEngine) -> Result<String, DeviceError> {
    Ok(row_of(engine, crate::onboarding::SETTING_WORKSPACE)?
        .unwrap_or_default()
        .trim()
        .to_owned())
}

/// The values `sandbox` accepts, from [`crate::onboarding::SandboxMode`] itself.
const SANDBOX_VALUES: &[&str] = &["read-only", "workspace-write", "full"];
/// The values `approval` accepts.
const APPROVAL_VALUES: &[&str] = &["ask", "never"];
/// The values `memory` accepts.
const MEMORY_VALUES: &[&str] = &["gbrain", "vault", "off"];
/// The values `show_tool_log` accepts.
const TOOL_LOG_VALUES: &[&str] = &["full", "summary", "off"];
/// The values `reply_state` accepts.
const REPLY_STATE_VALUES: &[&str] = &["expanded", "collapsed", "minimized"];
/// The values `greeting.refresh` accepts.
const GREETING_REFRESH_VALUES: &[&str] = &["hour", "day", "week"];
/// The values `push.notify_scope` accepts.
const NOTIFY_SCOPE_VALUES: &[&str] = &["origin", "all", "targeted"];
/// The values `display.clock_format` accepts.
const CLOCK_FORMAT_VALUES: &[&str] = &["12h", "24h"];
/// The values `display.conversation_sort` accepts.
const CONVERSATION_SORT_VALUES: &[&str] = &["updated", "created", "title", "day"];

/// Every operator-facing setting the daemon serves, described once.
///
/// The order is the order the record lists them, grouped by [`SETTING_GROUPS`].
/// A key here with `writable: true` is accepted by `POST /v1/settings`; one with
/// `writable: false` is refused by name. Nothing else is accepted, and nothing
/// here is absent from the record.
pub const SETTING_SPECS: &[SettingSpec] = &[
    SettingSpec {
        key: "model",
        label: "Model",
        group: "models",
        kind: SettingKind::Text,
        row: crate::onboarding::SETTING_MODEL,
        writable: true,
        read: read_model,
        normalize: None,
    },
    SettingSpec {
        key: "tools_enabled",
        label: "Tools",
        group: "models",
        kind: SettingKind::Bool,
        row: crate::tools::SETTING_TOOLS_ENABLED,
        writable: true,
        read: read_tools_enabled,
        normalize: None,
    },
    SettingSpec {
        key: "tool_rounds",
        label: "Tool turns",
        group: "models",
        // The bounds are the reader's own: one round is the fewest a turn can
        // take, and the ceiling is what `tools::max_rounds` refuses to read past.
        kind: SettingKind::Int(1, crate::tools::MAX_TOOL_ROUNDS_CEILING),
        row: crate::tools::SETTING_TOOL_ROUNDS,
        writable: true,
        read: read_tool_rounds,
        normalize: None,
    },
    SettingSpec {
        key: "agents_enabled",
        label: "Subagents",
        group: "models",
        kind: SettingKind::Bool,
        row: crate::subagents::SETTING_AGENTS_ENABLED,
        writable: true,
        read: read_agents_enabled,
        normalize: None,
    },
    SettingSpec {
        key: "assistant_name",
        label: "Assistant name",
        group: "assistant",
        kind: SettingKind::Text,
        row: crate::persona::SETTING_ASSISTANT_NAME,
        writable: true,
        read: read_assistant_name,
        normalize: None,
    },
    SettingSpec {
        key: "assistant_persona_enabled",
        label: "Persona",
        group: "assistant",
        kind: SettingKind::Bool,
        row: crate::persona::SETTING_PERSONA_ENABLED,
        writable: true,
        read: read_persona_enabled,
        normalize: None,
    },
    SettingSpec {
        key: "show_reasoning",
        label: "Thinking bubble",
        group: "assistant",
        kind: SettingKind::Bool,
        row: crate::web::chat::SETTING_SHOW_REASONING,
        writable: true,
        read: read_show_reasoning,
        normalize: None,
    },
    SettingSpec {
        key: "show_tool_log",
        label: "Run log",
        group: "assistant",
        kind: SettingKind::Enum(TOOL_LOG_VALUES),
        row: crate::web::chat::SETTING_SHOW_TOOL_LOG,
        writable: true,
        read: read_show_tool_log,
        normalize: None,
    },
    SettingSpec {
        key: "stream_responses",
        label: "Stream answers",
        group: "assistant",
        kind: SettingKind::Bool,
        row: crate::web::chat::SETTING_STREAMING,
        writable: true,
        read: read_stream_responses,
        normalize: None,
    },
    SettingSpec {
        key: "show_quota",
        label: "Quota line",
        group: "assistant",
        kind: SettingKind::Bool,
        row: crate::web::quota::SETTING_SHOW_QUOTA,
        writable: true,
        read: read_show_quota,
        normalize: None,
    },
    SettingSpec {
        key: "reply_state",
        label: "Reply bubble",
        group: "assistant",
        kind: SettingKind::Enum(REPLY_STATE_VALUES),
        row: crate::web::chat::SETTING_REPLY_STATE,
        writable: true,
        read: read_reply_state,
        normalize: None,
    },
    SettingSpec {
        key: "greeting_enabled",
        label: "Greeting",
        group: "assistant",
        kind: SettingKind::Bool,
        row: crate::greeting::SETTING_GREETING_ENABLED,
        writable: true,
        read: read_greeting_enabled,
        normalize: None,
    },
    SettingSpec {
        key: "greeting_refresh",
        label: "Greeting refresh",
        group: "assistant",
        kind: SettingKind::Enum(GREETING_REFRESH_VALUES),
        row: crate::greeting::SETTING_GREETING_REFRESH,
        writable: true,
        read: read_greeting_refresh,
        normalize: None,
    },
    SettingSpec {
        key: "memory",
        label: "Memory",
        group: "memory",
        kind: SettingKind::Enum(MEMORY_VALUES),
        row: crate::memory::SETTING_BACKEND,
        writable: true,
        read: read_memory,
        normalize: None,
    },
    SettingSpec {
        key: "push_notify_on_reply",
        label: "Reply notifications",
        group: "devices",
        kind: SettingKind::Bool,
        row: crate::push::SETTING_NOTIFY_ON_REPLY,
        writable: true,
        read: read_notify_on_reply,
        normalize: None,
    },
    SettingSpec {
        key: "push_notify_scope",
        label: "Notify scope",
        group: "devices",
        kind: SettingKind::Enum(NOTIFY_SCOPE_VALUES),
        row: crate::push::SETTING_NOTIFY_SCOPE,
        writable: true,
        read: read_notify_scope,
        normalize: None,
    },
    SettingSpec {
        key: "push_notify_devices",
        label: "Notify devices",
        group: "devices",
        kind: SettingKind::Text,
        row: crate::push::SETTING_NOTIFY_DEVICES,
        writable: true,
        read: read_notify_devices,
        normalize: Some(normalize_notify_devices),
    },
    SettingSpec {
        key: "schedules_enabled",
        label: "Schedules",
        group: "automation",
        kind: SettingKind::Bool,
        row: crate::scheduler::SETTING_ENABLED,
        writable: true,
        read: read_schedules_enabled,
        normalize: None,
    },
    SettingSpec {
        key: "skills_enabled",
        label: "Skills",
        group: "automation",
        kind: SettingKind::Bool,
        row: crate::skills::SETTING_ENABLED,
        writable: true,
        read: read_skills_enabled,
        normalize: None,
    },
    SettingSpec {
        key: "sandbox",
        label: "Sandbox",
        group: "system",
        kind: SettingKind::Enum(SANDBOX_VALUES),
        row: crate::onboarding::SETTING_SANDBOX_MODE,
        writable: true,
        read: read_sandbox,
        normalize: None,
    },
    SettingSpec {
        key: "approval",
        label: "Approval",
        group: "system",
        kind: SettingKind::Enum(APPROVAL_VALUES),
        row: crate::onboarding::SETTING_APPROVAL_POLICY,
        writable: true,
        read: read_approval,
        normalize: None,
    },
    SettingSpec {
        key: "timezone",
        label: "Time zone",
        group: "system",
        kind: SettingKind::Zone,
        row: crate::zone::SETTING_TIMEZONE,
        writable: true,
        read: read_timezone,
        normalize: None,
    },
    SettingSpec {
        key: "clock_format",
        label: "Clock",
        group: "system",
        kind: SettingKind::Enum(CLOCK_FORMAT_VALUES),
        row: crate::zone::SETTING_CLOCK_FORMAT,
        writable: true,
        read: read_clock_format,
        normalize: None,
    },
    SettingSpec {
        key: "conversation_sort",
        label: "Conversation sort",
        group: "system",
        kind: SettingKind::Enum(CONVERSATION_SORT_VALUES),
        row: crate::web::chat::SETTING_CONVERSATION_SORT,
        writable: true,
        read: read_conversation_sort,
        normalize: None,
    },
    SettingSpec {
        key: "workspace_path",
        label: "Workspace",
        group: "system",
        kind: SettingKind::Text,
        row: crate::onboarding::SETTING_WORKSPACE,
        // Read-only here on purpose: the wizard sets it, and moving it from a
        // phone would move every tool's sandbox in one unconfirmed write.
        writable: false,
        read: read_workspace,
        normalize: None,
    },
];

/// The spec for a key, when this route knows it.
#[must_use]
pub fn setting_spec(key: &str) -> Option<&'static SettingSpec> {
    SETTING_SPECS.iter().find(|spec| spec.key == key)
}

/// The capability record both clients render.
///
/// Built by iterating the table, so a field can never be in one and missing from
/// the other. Entries are in [`SETTING_GROUPS`] order so both clients can render
/// the tab order without sorting.
pub fn capability_record(engine: &SetupEngine) -> Result<Vec<serde_json::Value>, DeviceError> {
    let mut entries = Vec::with_capacity(SETTING_SPECS.len());
    for group in SETTING_GROUPS {
        for spec in SETTING_SPECS.iter().filter(|spec| spec.group == group) {
            let value = (spec.read)(engine)?;
            entries.push(serde_json::json!({
                "key": spec.key,
                "label": spec.label,
                "group": spec.group,
                "type": spec.kind.as_str(),
                "writable": spec.writable,
                "value": value,
                "options": spec.kind.options(),
            }));
        }
    }
    Ok(entries)
}

/// The four fields this route accepted before the capability record existed.
///
/// Kept because a client written against the old shape reads them, and because
/// the four of them are the only ones with a validation rule of their own: a
/// model has to be one the endpoint publishes, the permission pair has to be
/// checked together, and the vault backend needs a directory. Everything else in
/// [`SETTING_SPECS`] is validated by its kind.
pub const SETTINGS_FIELDS: [&str; 4] = ["model", "sandbox", "memory", "approval"];

/// Every field `POST /v1/settings` accepts, in record order.
///
/// Derived from [`SETTING_SPECS`], so a field the record offers and the write
/// refuses cannot exist: the two would be the same list read twice.
#[must_use]
pub fn accepted_settings() -> Vec<&'static str> {
    SETTING_SPECS
        .iter()
        .filter(|spec| spec.writable)
        .map(|spec| spec.key)
        .collect()
}

/// The value a setting will be stored as, whatever spelling arrived.
///
/// Validating a value and then storing it verbatim would let `TRUE` and `True`
/// both sit in a row whose reader only ever writes `true`, and the record would
/// read back a spelling the form did not offer. A closed list keeps the option's
/// own spelling, a flag is lower case, and a number loses its padding.
fn canonical_value(spec: &SettingSpec, raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some(normalize) = spec.normalize {
        return normalize(trimmed);
    }
    match spec.kind {
        SettingKind::Text => trimmed.to_owned(),
        SettingKind::Enum(values) => values
            .iter()
            .find(|known| known.eq_ignore_ascii_case(trimmed))
            .map_or_else(|| trimmed.to_owned(), |known| (*known).to_owned()),
        SettingKind::Bool => trimmed.to_ascii_lowercase(),
        SettingKind::Int(_, _) => trimmed
            .parse::<u32>()
            .map_or_else(|_| trimmed.to_owned(), |number| number.to_string()),
        // A zone keeps the spelling its own parser accepts and the record reads
        // back: `UTC` for `utc`, `+00:00` for `+0000`.
        SettingKind::Zone => {
            crate::zone::validate(trimmed).map_or_else(|_| trimmed.to_owned(), |zone| zone.as_str())
        }
    }
}

/// The values `approval` accepts, for a refusal that names them.
const APPROVAL_ALLOWED: &str = "ask or never";

/// The values a decision accepts, read from [`Decision`] itself.
///
/// The spellings are the ones the wire and the database already use, so a
/// decision a phone sends is the same word a browser row holds: `approved` and
/// `denied`.
const DECISION_ALLOWED: &str = "approved or denied";

/// One field a settings write will change, as the audit row records it.
struct SettingsChange {
    /// The field name the request used.
    field: &'static str,
    /// The value that will be written.
    value: String,
}

/// One validated settings write, assembled before anything is stored.
///
/// Every value is checked first and written second, so a body that carries one
/// invalid value leaves every row exactly as it was.
#[derive(Default)]
struct SettingsWrite {
    /// The default model, when the request named one that differs.
    model: Option<String>,
    /// The permission tuple, when `sandbox` or `approval` differs.
    permissions: Option<Permissions>,
    /// The memory backend, when the request named one that differs.
    memory: Option<crate::memory::Backend>,
    /// Every other setting to write, as its `settings` row and its value.
    ///
    /// The row comes from the spec rather than from the key, so a write can only
    /// ever touch a row the record already reads from.
    rows: Vec<(&'static str, String)>,
    /// What will change, in the order the body was read.
    changes: Vec<SettingsChange>,
    /// Whether the body named any field this route understands.
    named: bool,
}

/// `GET /v1/settings` - a read snapshot of the operator-facing settings.
///
/// Behind the bearer and the `admin` scope. The body is what a settings screen
/// draws: no credential, no key reference, no path and no base URL, because
/// there is nothing here for a response somebody reads over a shoulder to leak
/// beyond the choices the operator already made.
pub async fn settings(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    match read_settings(&service) {
        Ok(body) => axum::Json(body).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `POST /v1/settings` - change a subset of the operator-facing settings.
///
/// Form or JSON, any subset of `model`, `sandbox`, `memory` and `approval`. Each
/// value is checked by the same parser the dashboard checks it with, a refusal
/// names the field and the values it would have accepted, and nothing is written
/// unless every field in the body is valid.
pub async fn update_settings(
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
    let write = match parse_settings(&service, &fields) {
        Ok(write) => write,
        Err(error) => return error.into_response(),
    };
    if !write.named {
        return DeviceError::bad_request(
            "settings_empty",
            format!(
                "the body named none of {}: nothing was changed",
                accepted_settings().join(", ")
            ),
        )
        .into_response();
    }
    if let Err(error) = apply_settings(&service, &write) {
        return error.into_response();
    }
    // One row per request, carrying every field it changed. The dashboard audits
    // a settings post the same way; the actor here is the device, because a
    // paired phone made this change and not the browser.
    if !write.changes.is_empty() {
        let detail = write
            .changes
            .iter()
            .map(|change| format!("{}={}", change.field, change.value))
            .collect::<Vec<_>>()
            .join("; ");
        if let Err(error) = service.engine().auth().audit(
            token.device_id(),
            "settings.device_update",
            Some(&detail),
        ) {
            return DeviceError::db_unavailable(format!(
                "cannot append the settings.device_update audit row: {error}"
            ))
            .into_response();
        }
    }
    let changed: Vec<&'static str> = write.changes.iter().map(|change| change.field).collect();
    axum::Json(serde_json::json!({ "ok": true, "changed": changed })).into_response()
}

/// The snapshot `GET /v1/settings` answers with.
fn read_settings(service: &DeviceService) -> Result<serde_json::Value, DeviceError> {
    let engine = service.engine();
    let stored = |key: &str| engine.setting(key).map_err(settings_unavailable);
    let permissions = engine.permissions().map_err(settings_unavailable)?;
    let model = engine
        .model()
        .map_err(settings_unavailable)?
        .unwrap_or_default();
    let memory = crate::memory::backend(stored(crate::memory::SETTING_BACKEND)?.as_deref());
    let tools_enabled =
        crate::tools::enabled(stored(crate::tools::SETTING_TOOLS_ENABLED)?.as_deref());
    let tool_rounds =
        crate::tools::max_rounds(stored(crate::tools::SETTING_TOOL_ROUNDS)?.as_deref());
    let timezone = crate::zone::Zone::parse(stored(crate::zone::SETTING_TIMEZONE)?.as_deref());
    let clock =
        crate::zone::ClockFormat::parse(stored(crate::zone::SETTING_CLOCK_FORMAT)?.as_deref());
    let notify_on_reply = crate::push::notify_on_reply_enabled(
        stored(crate::push::SETTING_NOTIFY_ON_REPLY)?.as_deref(),
    );
    let scope = crate::push::notify_scope(stored(crate::push::SETTING_NOTIFY_SCOPE)?.as_deref());
    let devices =
        crate::push::notify_devices(stored(crate::push::SETTING_NOTIFY_DEVICES)?.as_deref());
    // The seven fields a client already reads, unchanged in shape and in order,
    // plus the three the settings record needs: the clock shape, the push routing,
    // and the capability list itself.
    Ok(serde_json::json!({
        "ok": true,
        "model": model,
        "sandbox": permissions.sandbox_mode.as_str(),
        "memory": memory.as_str(),
        "tools_enabled": tools_enabled,
        "tool_rounds": tool_rounds,
        "approval": permissions.approval_policy.as_str(),
        "timezone": timezone.as_str(),
        "clock_format": clock.as_str(),
        "push": {
            "notify_on_reply": notify_on_reply,
            "scope": scope.as_str(),
            "devices": devices,
        },
        "capabilities": capability_record(engine)?,
    }))
}

/// Validate every field the body named, or refuse naming one.
///
/// A value that is valid but already stored is not a change: it is recorded as
/// nothing, so `changed` in the response is a list of differences rather than a
/// list of fields somebody mentioned.
fn parse_settings(
    service: &DeviceService,
    fields: &BTreeMap<String, String>,
) -> Result<SettingsWrite, DeviceError> {
    let engine = service.engine();
    let mut write = SettingsWrite::default();

    if let Some(raw) = fields.get("model") {
        write.named = true;
        // Split first, then check per provider: a picker sends `provider/model`
        // and the catalogue it has to satisfy is that provider's own.
        let model = resolve_model_value(engine, raw)?;
        let current = engine
            .model()
            .map_err(settings_unavailable)?
            .unwrap_or_default();
        if model != current {
            write.changes.push(SettingsChange {
                field: "model",
                value: model.clone(),
            });
            write.model = Some(model);
        }
    }

    if fields.contains_key("sandbox") || fields.contains_key("approval") {
        write.named = true;
        let current = engine.permissions().map_err(settings_unavailable)?;
        let sandbox = fields
            .get("sandbox")
            .map_or(current.sandbox_mode.as_str(), |value| value.trim());
        let approval = fields
            .get("approval")
            .map_or(current.approval_policy.as_str(), |value| value.trim());
        let allow_full = full_allowed(engine);
        let parsed = Permissions::validate_with(
            sandbox,
            approval,
            current.autopilot_ceiling.as_str(),
            current.telemetry.as_str(),
            engine.paired_device_count().unwrap_or(0) > 0,
            engine.is_ready().unwrap_or(false),
            allow_full,
        )
        .map_err(|error| permission_refusal(error, sandbox, approval, allow_full))?;
        let sandbox_changed =
            fields.contains_key("sandbox") && parsed.sandbox_mode != current.sandbox_mode;
        let approval_changed =
            fields.contains_key("approval") && parsed.approval_policy != current.approval_policy;
        if sandbox_changed {
            write.changes.push(SettingsChange {
                field: "sandbox",
                value: parsed.sandbox_mode.as_str().to_owned(),
            });
        }
        if approval_changed {
            write.changes.push(SettingsChange {
                field: "approval",
                value: parsed.approval_policy.as_str().to_owned(),
            });
        }
        if sandbox_changed || approval_changed {
            write.permissions = Some(parsed);
        }
    }

    if let Some(raw) = fields.get("memory") {
        write.named = true;
        let backend = crate::memory::Backend::parse(raw).ok_or_else(|| {
            DeviceError::bad_request(
                "memory_invalid",
                format!(
                    "memory does not accept {}: the allowed values are {}",
                    shown_value(raw),
                    crate::memory::Backend::ALL
                        .iter()
                        .map(|known| known.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
        })?;
        // The same rule the dashboard enforces: the vault backend needs a
        // directory of markdown that exists, and a device cannot supply one, so
        // choosing it here without one stored would be a backend that silently
        // remembers nothing.
        if backend == crate::memory::Backend::Vault {
            let vault = engine
                .setting(crate::memory::SETTING_VAULT)
                .map_err(settings_unavailable)?
                .unwrap_or_default();
            if vault.trim().is_empty() || !std::path::Path::new(vault.trim()).is_dir() {
                return Err(DeviceError::bad_request(
                    "memory_invalid",
                    format!(
                        "memory does not accept {}: the vault backend needs a directory of \
                         markdown, and no directory is stored, so set that on the dashboard first",
                        shown_value(raw)
                    ),
                ));
            }
        }
        let current = crate::memory::backend(
            engine
                .setting(crate::memory::SETTING_BACKEND)
                .map_err(settings_unavailable)?
                .as_deref(),
        );
        if backend != current {
            write.changes.push(SettingsChange {
                field: "memory",
                value: backend.as_str().to_owned(),
            });
            write.memory = Some(backend);
        }
    }

    // Everything else the record offers, through the table. The four above are
    // here because their validation is not a matter of kind: a model id has to be
    // one an endpoint published, the sandbox and the approval policy are checked
    // as a pair, and the vault backend needs a directory. Anything the table does
    // not hold is refused by name, so a typo is a named field rather than a
    // silently ignored one.
    for (key, raw) in fields {
        if SETTINGS_FIELDS.contains(&key.as_str()) {
            continue;
        }
        let Some(spec) = setting_spec(key) else {
            return Err(unknown_setting(key));
        };
        if !spec.writable {
            return Err(read_only_setting(spec));
        }
        write.named = true;
        validate_setting(spec, raw)?;
        let value = canonical_value(spec, raw);
        let current = (spec.read)(engine)?;
        if value != current {
            write.changes.push(SettingsChange {
                field: spec.key,
                value: value.clone(),
            });
            write.rows.push((spec.row, value));
        }
    }

    Ok(write)
}

/// A body field the route does not know, refused by name and with the list.
fn unknown_setting(key: &str) -> DeviceError {
    DeviceError::bad_request(
        "settings_unknown_field",
        format!(
            "`{key}` is not a setting this route accepts: the accepted fields are {}",
            accepted_settings().join(", ")
        ),
    )
}

/// A field the record lists as readable but not writable, refused by name.
///
/// Refusing the write is the whole point of the flag: the record shows the value
/// so a client can display it, and saying which surface does own the decision is
/// more use than a generic refusal.
fn read_only_setting(spec: &SettingSpec) -> DeviceError {
    DeviceError::bad_request(
        "settings_read_only",
        format!(
            "{} is read-only here: it is set by the setup wizard, not by this route",
            spec.key
        ),
    )
}

/// Write what [`parse_settings`] validated. Every value is already checked, so a
/// failure here is the database refusing rather than the body being wrong.
fn apply_settings(service: &DeviceService, write: &SettingsWrite) -> Result<(), DeviceError> {
    let engine = service.engine();
    if let Some(model) = &write.model {
        engine
            .set_setting(crate::onboarding::SETTING_MODEL, model)
            .map_err(settings_unavailable)?;
    }
    if let Some(permissions) = write.permissions {
        engine
            .set_permissions(permissions)
            .map_err(settings_unavailable)?;
    }
    if let Some(memory) = write.memory {
        engine
            .set_setting(crate::memory::SETTING_BACKEND, memory.as_str())
            .map_err(settings_unavailable)?;
    }
    for (row, value) in &write.rows {
        engine
            .set_setting(row, value)
            .map_err(settings_unavailable)?;
    }
    Ok(())
}

/// Whether this process may select `full` sandbox mode.
///
/// The dashboard combines the `--allow-full` flag with the stored switch; the
/// device listener has no flag of its own to combine, so it reads the switch that
/// the dashboard's own full-access form writes. A device can therefore never turn
/// `full` on through this route and then use it: that still takes a restart.
fn full_allowed(engine: &SetupEngine) -> bool {
    matches!(
        engine.setting(crate::onboarding::SETTING_ALLOW_FULL),
        Ok(Some(value)) if value.trim().eq_ignore_ascii_case("true")
    )
}

/// The permission parser's refusal, said in the field names this route uses.
fn permission_refusal(
    error: PermissionError,
    sandbox: &str,
    approval: &str,
    allow_full: bool,
) -> DeviceError {
    let sandbox_allowed = if allow_full {
        "read-only, workspace-write, full"
    } else {
        "read-only or workspace-write, because full needs a restart of eris with --allow-full"
    };
    match error {
        PermissionError::UnknownValue {
            field: "approval_policy",
            ..
        } => refuse_value("approval", "approval_invalid", approval, APPROVAL_ALLOWED),
        PermissionError::NeverRequiresPairedDevice => DeviceError::bad_request(
            "approval_invalid",
            format!(
                "approval does not accept {}: never needs a paired device and a finished setup",
                shown_value(approval)
            ),
        ),
        PermissionError::UnknownValue { .. } | PermissionError::FullNotAvailableInOnboarding => {
            refuse_value("sandbox", "sandbox_invalid", sandbox, sandbox_allowed)
        }
        other => DeviceError::bad_request("settings_value_invalid", other.to_string()),
    }
}

/// The one refusal shape: the field, the value, and what the field accepts.
pub(crate) fn refuse_value(
    field: &'static str,
    code: &'static str,
    submitted: &str,
    allowed: &str,
) -> DeviceError {
    DeviceError::bad_request(
        code,
        format!(
            "{field} does not accept {}: the allowed values are {allowed}",
            shown_value(submitted)
        ),
    )
}

/// A submitted value as a refusal quotes it: never a secret, and an empty
/// submission says so rather than printing empty backticks.
pub(crate) fn shown_value(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        "an empty value".to_owned()
    } else {
        format!("`{trimmed}`")
    }
}

/// A settings read or write that the state store refused.
pub(crate) fn settings_unavailable(error: impl std::fmt::Display) -> DeviceError {
    DeviceError::db_unavailable(format!("cannot reach the settings: {error}"))
}

/// A model write that failed, said as the thing that actually went wrong.
///
/// A pin is refused for one of two reasons, and they are not the same event: the
/// state store would not take the write, which is an outage, or the value is not
/// a model this endpoint published, which is the caller's mistake. Reporting the
/// second as the first is what turned "that model is not in the catalogue" into a
/// `503 db_unavailable`, and sent the operator looking at a database that was
/// answering fine.
pub(crate) fn model_write_refusal(error: OnboardingError) -> DeviceError {
    match error {
        OnboardingError::Db(error) => settings_unavailable(error),
        other => DeviceError::bad_request("model_invalid", other.to_string()),
    }
}

/// The value to store for a submitted model, checked against the list that would
/// actually serve it.
///
/// A picker submits `provider/model`, because a model id belongs to the endpoint
/// that published it. The list it has to be checked against is therefore that
/// provider's, and the split has to happen first: the old check compared the
/// whole `provider/model` string against a catalogue keyed by bare ids, so a
/// correct pin for a provider other than the legacy probe's owner could never
/// match and was refused as though the endpoint had never offered it.
///
/// A provider that has never been probed has an empty list, and an empty list
/// means no check rather than a refusal: the endpoint is the authority until it
/// has said otherwise.
pub(crate) fn resolve_model_value(engine: &SetupEngine, raw: &str) -> Result<String, DeviceError> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(DeviceError::bad_request(
            "model_missing",
            "a pin needs a model id: the body named an empty `model`".to_owned(),
        ));
    }
    let registry = chat::provider_registry(engine).map_err(settings_unavailable)?;
    let primary = engine
        .setting(crate::providers::SETTING_PRIMARY)
        .map_err(settings_unavailable)?
        .unwrap_or_default();
    let choice = chat::split_model_value(value, &registry, &primary);
    let known = engine
        .provider_catalog(&choice.provider_id)
        .map_err(settings_unavailable)?;
    if !known.is_empty() && !known.iter().any(|candidate| candidate == &choice.model) {
        return Err(DeviceError::bad_request(
            "model_invalid",
            format!(
                "model does not accept {}: {} publishes {} models and that id is not one of \
                 them ({}), so no turn would answer",
                shown_value(value),
                choice.provider_id,
                known.len(),
                known.join(", ")
            ),
        ));
    }
    Ok(value.to_owned())
}

// ---------------------------------------------------------------------------
// The approvals queue, behind the admin scope
// ---------------------------------------------------------------------------

/// `GET /v1/approvals` - the calls a turn parked, newest first.
///
/// Behind the bearer and the `admin` scope, because answering a parked call is
/// exactly the authority a phone was paired with the administration box for.
/// Every row is what the turn already asked for: the tool, its arguments, the
/// advice the approval role wrote and the two timestamps. No credential, no key
/// reference and no workspace path, so a queue read over a shoulder leaks
/// nothing that the same call would not have written into a file.
pub async fn approvals(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    match read_approvals(&service) {
        Ok(body) => axum::Json(body).into_response(),
        Err(error) => error.into_response(),
    }
}

/// `POST /v1/approvals/{id}/decide` - settle one parked call.
///
/// Settling is the whole route: it flips the row, records who flipped it, and
/// appends one audit row with the device as the actor, so the queue has one
/// history whichever surface answered. It deliberately does not run the call -
/// running a parked write is the operator surfaces' job, and a decision is a
/// decision.
pub async fn decide_approval(
    State(service): State<DeviceService>,
    Path(approval_id): Path<String>,
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
    let decision = match parse_decision(field(&fields, "decision")) {
        Ok(decision) => decision,
        Err(error) => return error.into_response(),
    };

    let db = service.engine().db();
    // Read first, so the audit row can name the tool the decision settled. The
    // read is not the guard: [`approvals::decide`] re-checks that the row is
    // still pending and reports `false` when a racing decision got there first.
    let row = match approvals::get(db, &approval_id) {
        Ok(Some(row)) => row,
        Ok(None) => return DeviceError::approval_not_found().into_response(),
        Err(error) => {
            return DeviceError::db_unavailable(format!(
                "cannot read the approval with that id: {error}"
            ))
            .into_response();
        }
    };
    // An approval runs the call. Settling the row and returning "approved" without
    // executing would be a person saying yes to a form: the CLI runs it, so a phone
    // takes the same path instead of a second one that stops short.
    let mut ran_content: Option<String> = None;
    if decision == approvals::Decision::Approved {
        let paths = service.engine().paths().clone();
        match approvals::approve_and_run(db, &paths, &approval_id, token.device_id()) {
            Ok(approvals::Ran::Done { content, .. }) => ran_content = Some(content),
            Ok(approvals::Ran::Refused(message)) => {
                return DeviceError::new(
                    axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                    "approval_not_run",
                    format!("{approval_id} was approved but running it was refused: {message}"),
                )
                .into_response();
            }
            Ok(approvals::Ran::AlreadySettled) => {
                return DeviceError::approval_not_found().into_response();
            }
            Err(error) => {
                return DeviceError::db_unavailable(format!(
                    "cannot run the approved call: {error}"
                ))
                .into_response();
            }
        }
    } else {
        match approvals::decide(db, &approval_id, decision, token.device_id()) {
            Ok(true) => {}
            Ok(false) => return DeviceError::approval_not_found().into_response(),
            Err(error) => {
                return DeviceError::db_unavailable(format!("cannot record the decision: {error}"))
                    .into_response();
            }
        }
    }

    // The queue's own audit row, appended after the row moved so a failure here
    // is reported rather than passed off as a decision that left no trace.
    let detail = format!(
        "approval={approval_id}; tool={}; decision={}",
        row.tool,
        decision.as_str()
    );
    if let Err(error) =
        service
            .engine()
            .auth()
            .audit(token.device_id(), "chat.approval.decided", Some(&detail))
    {
        return DeviceError::db_unavailable(format!(
            "{approval_id} was settled but the audit row could not be appended: {error}"
        ))
        .into_response();
    }

    let mut body = serde_json::json!({
        "ok": true,
        "id": approval_id,
        "decision": decision.as_str(),
    });
    if let Some(content) = ran_content {
        // What the call actually said, so a client can show the result rather
        // than only that a decision was recorded.
        body["result"] = serde_json::Value::String(content);
    }
    axum::Json(body).into_response()
}

/// `GET /v1/version` - what this daemon is, with no credential.
///
/// Deliberately unauthenticated: a client holds nothing to send before it has
/// paired, and "is anything even listening that speaks my protocol" is the first
/// question worth answering. It carries nothing an unpaired stranger could act on.
pub async fn version() -> axum::response::Response {
    axum::Json(serde_json::json!({
        "ok": true,
        "version": crate::admin::VERSION,
        "build": crate::admin::BUILD,
        "api": "v1",
        "frames": FrameKind::ALL.map(FrameKind::as_str),
    }))
    .into_response()
}

/// The list `GET /v1/approvals` answers with.
fn read_approvals(service: &DeviceService) -> Result<serde_json::Value, DeviceError> {
    let mut waiting = approvals::pending(service.engine().db()).map_err(|error| {
        DeviceError::db_unavailable(format!("cannot read the approvals queue: {error}"))
    })?;
    // `pending` is oldest first, for the dashboard's reading order. A phone wants
    // the call that just stopped a turn at the top, so the same rows are reversed
    // and then bounded: the oldest of a long queue falls off, not the newest.
    waiting.reverse();
    let truncated = waiting.len() > APPROVAL_LIST_LIMIT;
    waiting.truncate(APPROVAL_LIST_LIMIT);
    let rows: Vec<serde_json::Value> = waiting
        .iter()
        .map(|row| {
            serde_json::json!({
                "id": row.id,
                "tool": row.tool,
                "arguments": arguments_json(&row.arguments),
                // The approval role's reading, when one is pinned. It is advice
                // and never a decision, so it rides along named as advice is.
                "reason": row.advice,
                "created_at": row.created_at,
                // The queue does not expire a row: nothing sweeps it and nothing
                // waits on a clock, so the honest value is null rather than a
                // timestamp this daemon would have to invent.
                "expires_at": serde_json::Value::Null,
            })
        })
        .collect();
    Ok(serde_json::json!({
        "ok": true,
        "count": rows.len(),
        "truncated": truncated,
        "approvals": rows,
    }))
}

/// A parked call's arguments as JSON rather than as quoted JSON text.
///
/// The column holds the bytes the turn parked. They are parsed back for a client
/// that has to show the call; text that no longer parses is passed through as the
/// text it is, because dropping the row would hide a queue entry and answering
/// `null` would hide that it is unreadable.
fn arguments_json(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_owned()))
}

/// The decision a body names, in the spellings [`Decision`] already uses.
fn parse_decision(raw: &str) -> Result<Decision, DeviceError> {
    match raw {
        "approved" => Ok(Decision::Approved),
        "denied" => Ok(Decision::Denied),
        other => Err(refuse_value(
            "decision",
            "decision_invalid",
            other,
            DECISION_ALLOWED,
        )),
    }
}

// ---------------------------------------------------------------------------
// MCP health, behind the sessions scope
// ---------------------------------------------------------------------------

/// `GET /v1/mcp` - the configured servers and the health this daemon observed.
///
/// Behind the bearer and the `sessions` scope, not `admin`: what is configured
/// and whether it answered is the same class of read as the transcript, and none
/// of it is a credential.
///
/// `health_known` is true only when every listed server has a state this process
/// actually observed. A server that was never started reports `healthy: null`, and
/// any such row makes `health_known` false: a green dot the daemon cannot back up
/// is worse than a client that says "unknown".
pub async fn mcp_servers(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Sessions) {
        return error.into_response();
    }
    match read_mcp(&service) {
        Ok(body) => axum::Json(body).into_response(),
        Err(error) => error.into_response(),
    }
}

/// The list and the health `GET /v1/mcp` answers with.
///
/// The names come from the stored list, so a server the operator saved after
/// start-up still appears; the health comes from the pool, so a server that was
/// never started in this process is reported as unknown rather than assumed good
/// or bad.
fn read_mcp(service: &DeviceService) -> Result<serde_json::Value, DeviceError> {
    let stored = service
        .engine()
        .setting(crate::mcp::SETTING_SERVERS)
        .map_err(|error| {
            DeviceError::db_unavailable(format!("cannot read the MCP server list: {error}"))
        })?;
    let servers = crate::mcp::parse(stored.as_deref().unwrap_or("")).map_err(|error| {
        DeviceError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "mcp_list_invalid",
            format!(
                "the stored MCP server list does not parse, so no health can be reported for it: {}",
                error.message
            ),
        )
    })?;
    let health = service.mcp().map(|pool| pool.health()).unwrap_or_default();
    let mut rows = Vec::with_capacity(servers.len());
    let mut health_known = true;
    for server in &servers {
        let known = health.iter().find(|row| row.name == server.name);
        let per_server = known.and_then(|row| row.healthy);
        if per_server.is_none() {
            health_known = false;
        }
        rows.push(serde_json::json!({
            "name": server.name,
            "command": server.command,
            "healthy": per_server,
            "last_error": known.and_then(|row| row.last_error.clone()),
        }));
    }
    Ok(serde_json::json!({
        "ok": true,
        "health_known": health_known,
        "servers": rows,
    }))
}

// ---------------------------------------------------------------------------
// The administration half, behind the admin scope
// ---------------------------------------------------------------------------

/// `GET /v1/devices` - every device this daemon knows, newest first.
///
/// Behind the bearer and the `admin` scope: a phone that may change the settings
/// may also see which devices exist, what each of them may do, and which one
/// should not be there any more. The rows are the dashboard's own device rows, so
/// the two surfaces cannot disagree about who is paired.
///
/// `can_admin` is read from the stored token scope rather than guessed from a
/// name. `paired` is the dashboard's own definition - a live token exists, or a
/// code for that device was redeemed - so the two surfaces state one fact; today
/// the two cannot come apart, because a redeemed code mints the tokens and
/// nothing in the pairing flow rotates one away. `last_seen` is the newest
/// activity stamp this daemon actually recorded for that device - the session it
/// opened or an audit row it authored - and it is null when neither exists,
/// because a timestamp invented for a field is worse than an empty field.
pub async fn devices(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    match read_devices(&service) {
        Ok(body) => axum::Json(body).into_response(),
        Err(error) => error.into_response(),
    }
}

/// The list `GET /v1/devices` answers with.
fn read_devices(service: &DeviceService) -> Result<serde_json::Value, DeviceError> {
    let db = service.engine().db();
    let mut rows = crate::web::store::devices_list(db).map_err(|error| {
        DeviceError::db_unavailable(format!("cannot read the device rows: {error}"))
    })?;
    let last_seen = crate::web::store::device_last_seen(db).map_err(|error| {
        DeviceError::db_unavailable(format!("cannot read the device activity stamps: {error}"))
    })?;
    // The store sorts newest first, so the rows that fall off a capped page are
    // the oldest devices rather than the ones just paired.
    let truncated = rows.len() > DEVICE_LIST_LIMIT;
    rows.truncate(DEVICE_LIST_LIMIT);
    let devices: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "id": row.id,
                "name": row.name,
                "created_at": row.created_at,
                "paired": row.paired,
                "can_admin": row.admin,
                // `None` serialises as null: the field is always present, so a
                // client does not have to tell "absent" from "never seen".
                "last_seen": last_seen.get(&row.id),
            })
        })
        .collect();
    Ok(serde_json::json!({
        "ok": true,
        "count": devices.len(),
        "truncated": truncated,
        "devices": devices,
    }))
}

/// `POST /v1/devices/{id}/revoke` - retire one device and every live token of it.
///
/// The body is empty. The work is [`crate::auth::AuthStore::revoke_device`], the
/// same call the dashboard's revoke form makes, so the device surface cannot
/// revoke half of what the browser revokes: the token rows and the device row
/// move together in one place.
///
/// Revoking the device whose token carried the call is allowed on purpose. It is
/// the operation an operator reaches for when the phone in their hand is the one
/// that should stop working, and refusing it would make a device able to keep
/// itself unrevocable. Its token stops verifying on the very next request,
/// because verification rereads `auth_tokens.revoked_at`; this response is
/// already being written when that happens.
///
/// The audit row names the acting device and uses the action the browser's revoke
/// writes, so the Activity page reads one history whichever surface revoked.
pub async fn revoke_device(
    State(service): State<DeviceService>,
    Path(device_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    let auth = service.engine().auth();
    match auth.revoke_device(&device_id) {
        Ok(revoked) => {
            if let Err(error) = auth.audit(
                token.device_id(),
                "dashboard.device_revoked",
                Some(&device_id),
            ) {
                return DeviceError::db_unavailable(format!(
                    "{device_id} was revoked but the audit row could not be appended: {error}"
                ))
                .into_response();
            }
            axum::Json(serde_json::json!({
                "ok": true,
                "id": device_id,
                "revoked": revoked,
            }))
            .into_response()
        }
        Err(crate::auth::AuthError::UnknownDevice) => {
            DeviceError::device_not_found().into_response()
        }
        Err(error) => {
            DeviceError::db_unavailable(format!("cannot revoke the device with that id: {error}"))
                .into_response()
        }
    }
}

/// `GET /v1/audit?limit=N` - the operator's own trail, newest first.
///
/// Behind the bearer and the `admin` scope. The rows are the ones the Activity
/// page renders, read through the same two store calls, so there is one trail and
/// one bound: `limit` defaults to fifty and a larger value is capped rather than
/// refused, and `total` says how many rows exist so a client can tell what it is
/// not seeing.
///
/// A row names a device, a setting, a file or a job. There is no column that
/// could hold a token, a code or a passphrase, which is why the same rows can be
/// served to a phone.
pub async fn audit(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    uri: axum::http::Uri,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    let limit = match audit_limit(uri.query()) {
        Ok(limit) => limit,
        Err(error) => return error.into_response(),
    };
    match read_audit(&service, limit) {
        Ok(body) => axum::Json(body).into_response(),
        Err(error) => error.into_response(),
    }
}

/// The page size a request names, defaulted and capped.
fn audit_limit(query: Option<&str>) -> Result<usize, DeviceError> {
    let fields = query_fields(query.unwrap_or_default())?;
    let Some(raw) = fields.get("limit").map(|value| value.trim()) else {
        return Ok(AUDIT_DEFAULT_LIMIT);
    };
    if raw.is_empty() {
        return Ok(AUDIT_DEFAULT_LIMIT);
    }
    match raw.parse::<usize>() {
        Ok(0) | Err(_) => Err(DeviceError::bad_request(
            "limit_invalid",
            format!(
                "limit must be a positive integer: the default is {AUDIT_DEFAULT_LIMIT} and the \
                 cap is {AUDIT_MAX_LIMIT}"
            ),
        )),
        Ok(limit) => Ok(limit.min(AUDIT_MAX_LIMIT)),
    }
}

/// The trail `GET /v1/audit` answers with.
fn read_audit(service: &DeviceService, limit: usize) -> Result<serde_json::Value, DeviceError> {
    let db = service.engine().db();
    let limit = i64::try_from(limit).map_err(|_| {
        DeviceError::bad_request("limit_invalid", "limit is larger than a row count can be")
    })?;
    let rows = crate::web::store::audit_recent(db, limit).map_err(|error| {
        DeviceError::db_unavailable(format!("cannot read the audit trail: {error}"))
    })?;
    let total = crate::web::store::audit_count(db).map_err(|error| {
        DeviceError::db_unavailable(format!("cannot count the audit rows: {error}"))
    })?;
    let entries: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "at": row.occurred_at,
                "actor": row.actor,
                "action": row.action,
                // The detail is a filename, a device id, a digest or a setting
                // value. Never a secret: the writers are the reason this holds.
                "detail": row.detail,
            })
        })
        .collect();
    Ok(serde_json::json!({
        "ok": true,
        "count": entries.len(),
        "total": total,
        "entries": entries,
    }))
}

/// `GET /v1/persona` - the identity text the next turn would be given.
///
/// Behind the bearer and the `admin` scope, because writing it changes what every
/// turn is told. `enabled` is the stored switch, so a client can tell text that is
/// in effect from text that is parked, and `limit` is the same byte cap the
/// settings page enforces.
pub async fn persona(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    let config = persona_config(&service);
    let text = match config.read_text() {
        Ok(text) => text,
        Err(error) => {
            return DeviceError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "persona_unreadable",
                error.to_string(),
            )
            .into_response();
        }
    };
    let enabled = match persona_enabled(&service) {
        Ok(enabled) => enabled,
        Err(error) => return error.into_response(),
    };
    axum::Json(serde_json::json!({
        "ok": true,
        "persona": text,
        "enabled": enabled,
        "limit": crate::persona::MAX_PERSONA_BYTES,
    }))
    .into_response()
}

/// `POST /v1/persona` - write the identity text.
///
/// The field is `persona`, and it is written to the same file the settings page
/// writes at the same cap: a person-sized refusal is better than a truncated
/// persona nobody wrote. The enabled switch is deliberately not writable here,
/// because the switch is not the text and a body that carried both would let one
/// mistake turn the identity off.
///
/// The audit row carries the character count and the switch, never the text: the
/// operator's own words belong in the file and nowhere else.
///
/// The body ceiling is the device surface's usual [`MAX_BODY_BYTES`], which is
/// above the persona cap for a JSON body. A form-encoded body spends three bytes
/// per non-ASCII character, so a persona close to the cap in a language that is
/// not ASCII fits as JSON and may not fit as a form.
pub async fn update_persona(
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
    let Some(text) = fields.get("persona").cloned() else {
        return DeviceError::bad_request(
            "persona_required",
            "a persona field is required: send an empty string to clear the text",
        )
        .into_response();
    };
    if text.len() > crate::persona::MAX_PERSONA_BYTES {
        return DeviceError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "persona_too_large",
            format!(
                "the persona is {} bytes and the limit is {} bytes. Shorten it",
                text.len(),
                crate::persona::MAX_PERSONA_BYTES
            ),
        )
        .into_response();
    }
    let enabled = match persona_enabled(&service) {
        Ok(enabled) => enabled,
        Err(error) => return error.into_response(),
    };
    if enabled && text.trim().is_empty() {
        return DeviceError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "persona_empty",
            "the persona is enabled but the text is empty. Write something, or switch the \
             persona off on the settings page first",
        )
        .into_response();
    }
    if let Err(error) = persona_config(&service).write(&text) {
        return DeviceError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "persona_write_failed",
            error.to_string(),
        )
        .into_response();
    }
    let characters = crate::persona::character_count(&text);
    let stored = if enabled { "true" } else { "false" };
    let detail = format!("enabled={stored}; characters={characters}");
    if let Err(error) =
        service
            .engine()
            .auth()
            .audit(token.device_id(), "settings.persona", Some(&detail))
    {
        return DeviceError::db_unavailable(format!(
            "the persona was written but the audit row could not be appended: {error}"
        ))
        .into_response();
    }
    axum::Json(serde_json::json!({
        "ok": true,
        "characters": characters,
        "enabled": enabled,
    }))
    .into_response()
}

/// The persona file this daemon reads and writes.
fn persona_config(service: &DeviceService) -> crate::persona::PersonaConfig {
    crate::persona::PersonaConfig::at_path(service.engine().paths().persona_file())
}

/// Whether the persona is on, with the dashboard's stored row winning.
fn persona_enabled(service: &DeviceService) -> Result<bool, DeviceError> {
    let stored = service
        .engine()
        .setting(crate::persona::SETTING_PERSONA_ENABLED)
        .map_err(|error| {
            DeviceError::db_unavailable(format!("cannot read the persona switch: {error}"))
        })?;
    Ok(persona_config(service).effective_enabled(stored.as_deref()))
}

/// A query string as the same flat map a form body becomes.
///
/// The device surface has no query-string route yet, so this is the one place a
/// query is read, and it reuses the body decoder so both spellings of `a=b` mean
/// the same thing.
fn query_fields(query: &str) -> Result<BTreeMap<String, String>, DeviceError> {
    let mut fields = BTreeMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (name, value) = match pair.split_once('=') {
            Some((name, value)) => (name, value),
            None => (pair, ""),
        };
        let name = percent_decode(name).map_err(|_| {
            DeviceError::bad_request("query_malformed", "a query name is malformed")
        })?;
        let value = percent_decode(value).map_err(|_| {
            DeviceError::bad_request("query_malformed", "a query value is malformed")
        })?;
        fields.insert(name, value);
    }
    Ok(fields)
}

// ---------------------------------------------------------------------------
// Backups and pairing codes: the two things an operator on a phone needs
// ---------------------------------------------------------------------------

/// The fields `POST /v1/backups` reads. Anything else is refused by name rather
/// than ignored, because a body that names a capability this route does not have
/// should say so out loud.
const BACKUP_CREATE_FIELDS: [&str; 2] = ["out", "allow_tmp"];

/// The field names a body could use to ask for the one file an archive never
/// carries.
const PASSPHRASE_FIELDS: [&str; 2] = ["passphrase", "include_passphrase"];

/// `GET /v1/backups` - every archive in the archive directory, newest first.
///
/// Behind the bearer and the `admin` scope. The rows are the ones the backups
/// page renders, read through the same [`crate::web::backups::list`], so the two
/// surfaces cannot disagree about what exists.
///
/// `bytes` is the size on disk, which the directory entry always carries. The
/// field that can be missing is `members`: it comes from the archive's manifest,
/// so a file in that directory which does not open as an archive reports null
/// rather than a zero nobody computed. Nothing here verifies the archives again -
/// that is what the page's `verifies` column is for, and a phone listing a
/// directory should not hash every member of every file to answer.
pub async fn backups(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    match read_backups(&service) {
        Ok(body) => axum::Json(body).into_response(),
        Err(error) => error.into_response(),
    }
}

/// The list `GET /v1/backups` answers with.
fn read_backups(service: &DeviceService) -> Result<serde_json::Value, DeviceError> {
    let rows = crate::web::backups::list(service.engine().paths()).map_err(|error| {
        DeviceError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "backup_unreadable",
            format!("cannot read the archive directory: {error}"),
        )
    })?;
    let archives: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "name": row.name,
                // The page calls this "written": it is the file's own stamp.
                "created_at": row.modified,
                "bytes": row.bytes,
                // null when the file does not open as an archive: the manifest
                // is where a member count comes from, and there is not one.
                "members": row.members,
            })
        })
        .collect();
    Ok(serde_json::json!({
        "ok": true,
        "count": archives.len(),
        "archives": archives,
    }))
}

/// `POST /v1/backups` - write one verified archive, and audit it.
///
/// The body is optional. With nothing in it the archive goes where the browser's
/// create puts it: a generated `kallisti-<stamp>.bk` in the archive directory.
/// Two fields let a phone ask for what the CLI's `eris backup --out` asks for,
/// and both are refused in the words the crate already uses:
///
/// - `out`, the absolute path to write. A path under the system temporary
///   directory is refused unless `allow_tmp` is true, with the crate's own
///   sentence; an existing file is refused rather than clobbered; a relative path
///   is refused. The write itself is [`crate::web::backups::create_archive_at`],
///   which is the same `create` the browser's button reaches.
/// - `allow_tmp`, the explicit allowance. Absent means the rule applies.
///
/// Any other field is a 400 that names it, and a field asking for the setup
/// passphrase is a 400 carrying the reason the backup format excludes it: that
/// file is the admin credential once onboarding is ready, and it is excluded
/// unconditionally, so there is nothing a body could set to get it in.
pub async fn create_backup(
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
    if let Some(name) = PASSPHRASE_FIELDS
        .iter()
        .find(|name| !field(&fields, name).is_empty())
    {
        return DeviceError::bad_request(
            "passphrase_not_archivable",
            format!(
                "an archive never carries the setup passphrase, so {name} is refused rather than \
                 honoured: {} {}",
                crate::backup::FIRST_RUN_PATH,
                crate::backup::PASSPHRASE_EXCLUSION
            ),
        )
        .into_response();
    }
    if let Some(unknown) = fields
        .keys()
        .find(|name| !BACKUP_CREATE_FIELDS.contains(&name.as_str()))
    {
        return DeviceError::bad_request(
            "field_unknown",
            format!(
                "the body named {unknown}, which this route does not read: it accepts {}",
                BACKUP_CREATE_FIELDS.join(", ")
            ),
        )
        .into_response();
    }

    let allow_tmp = truthy(field(&fields, "allow_tmp"));
    let out = field(&fields, "out");
    let paths = service.engine().paths().clone();
    let report = if out.is_empty() {
        crate::web::backups::create_archive(&paths)
    } else {
        crate::web::backups::create_archive_at(&paths, std::path::PathBuf::from(out), allow_tmp)
    };
    let report = match report {
        Ok(report) => report,
        Err(error) => return backup_error(&error).into_response(),
    };

    let name = report
        .archive
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A digest is not a secret, and it is the one field that lets an operator
    // match the audit row to the file on disk.
    let detail = format!(
        "filename={name} sha256={} bytes={}",
        report.archive_sha256, report.archive_bytes
    );
    if let Err(error) =
        service
            .engine()
            .auth()
            .audit(token.device_id(), "backup.created", Some(&detail))
    {
        return DeviceError::db_unavailable(format!(
            "the archive {name} was written and verified, but the audit row could not be appended: \
             {error}"
        ))
        .into_response();
    }
    (
        StatusCode::CREATED,
        axum::Json(serde_json::json!({
            "ok": true,
            "name": name,
            "members": report.members.len(),
            "bytes": report.archive_bytes,
        })),
    )
        .into_response()
}

/// `POST /v1/backups/{name}/restore` - replace the live state from one archive.
///
/// Behind the bearer and the `admin` scope, and gated in the browser's order, so
/// the two surfaces refuse the same things for the same stated reason:
///
/// 1. The name in the path must be a bare archive filename. A `/`, a `..`, a
///    leading dot, a backslash or an absolute path is a `400 bad_archive` by
///    rule, before any filesystem call, and the refusal never echoes the value.
/// 2. It must name a regular file in the archive directory. `symlink_metadata`
///    does not follow a link, so a link planted there is not an archive, and one
///    that is not there is a `404 archive_not_found`.
/// 3. Then the verified restore runs: verify every member into a sink, refuse a
///    newer schema, take a safety copy, place `eris.db` last. A verification
///    failure is a 422 and an I/O or database failure a 500, which is the CLI's
///    exit class kept visible over HTTP.
///
/// The browser's typed `confirm` field has no twin here on purpose: the name is
/// in the path rather than in a form a stray keypress submits, so naming it once
/// is the whole decision. Nothing here restarts the service.
///
/// The audit row goes into the database that was just placed, the way the
/// browser's does, because this process still holds the replaced file open: a row
/// written through that handle would land in the safety copy. The actor is the
/// device that asked.
pub async fn restore_backup(
    State(service): State<DeviceService>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    if let Err(reason) = crate::web::backups::validate_archive_name(&name) {
        return DeviceError::bad_request(
            "bad_archive",
            format!("that is not an archive in the backups directory: {reason}"),
        )
        .into_response();
    }
    let paths = service.engine().paths().clone();
    let path = crate::web::backups::directory(&paths).join(&name);
    let is_file = std::fs::symlink_metadata(&path)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false);
    if !is_file {
        return DeviceError::new(
            StatusCode::NOT_FOUND,
            "archive_not_found",
            "no archive with that name is in the backups directory",
        )
        .into_response();
    }

    let report = match crate::web::backups::restore_archive(&paths, &name) {
        Ok(report) => report,
        Err(error) => return backup_error(&error).into_response(),
    };
    let safety = report
        .safety_copy
        .as_ref()
        .map_or_else(|| "none".to_owned(), |path| path.display().to_string());
    let detail = format!(
        "archive={name} safety_copy={safety} sha256={} members={}",
        report.archive_sha256, report.members
    );
    if let Err(error) =
        crate::web::backups::audit_placed(&paths, token.device_id(), "backup.restored", &detail)
    {
        return DeviceError::db_unavailable(format!(
            "the archive {name} was restored, but recording it in the restored database failed: \
             {error}"
        ))
        .into_response();
    }
    axum::Json(serde_json::json!({
        "ok": true,
        "name": name,
        // Literal true: the archive verified and its members are in place. The
        // daemon still holds the replaced database file open, so the restored
        // state takes effect on the next service start.
        "restored": true,
    }))
    .into_response()
}

/// The status a backup refusal deserves, which is the CLI's exit class.
///
/// The same mapping the browser's create and restore use, so one failure is one
/// status whichever surface asked.
fn backup_error(error: &crate::backup::BackupError) -> DeviceError {
    let status = match error.exit_code() {
        crate::backup::EXIT_VERIFICATION => StatusCode::UNPROCESSABLE_ENTITY,
        crate::backup::EXIT_USAGE => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    DeviceError::new(status, error.code(), error.to_string())
}

/// `POST /v1/devices/{id}/code` - mint one pairing code for that device.
///
/// This exists so a second device is onboarded from a phone that is already
/// paired, without reaching for a browser on the host. It is the same mint the
/// devices page performs: [`PairingService::issue_for_device_asking`] with the
/// administration flag read from the body, which is the one place `admin` can
/// enter a pairing.
///
/// `allow_admin` is true for `true`, `on` or `1`, and absent means the ordinary
/// code. The code exists in this response and nowhere else: the pairing service
/// stores only an Argon2id verifier, this handler never writes the value, and the
/// audit row carries the device name and the flag rather than the code. There is
/// no route that reads a code back, because there is nothing to read back.
pub async fn mint_device_code(
    State(service): State<DeviceService>,
    Path(device_id): Path<String>,
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
    let allow_admin = truthy(field(&fields, "allow_admin"));

    // The name is for the audit row, and the row is also how a caller learns the
    // id it named is not a device this daemon holds.
    let store = service.engine().auth();
    let record = match store.device(&device_id) {
        Ok(Some(record)) => record,
        Ok(None) => return DeviceError::device_not_found().into_response(),
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot read that device: {error}"))
                .into_response();
        }
    };
    if record.revoked_at.is_some() {
        return DeviceError::new(
            StatusCode::CONFLICT,
            "device_revoked",
            "that device is revoked, so a code minted for it could not redeem: pair a new device \
             instead of reviving a retired one",
        )
        .into_response();
    }

    let pairing = PairingService::new(service.engine().db().clone());
    let issued = match pairing.issue_for_device_asking(&device_id, allow_admin) {
        Ok(issued) => issued,
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot mint a pairing code: {error}"))
                .into_response();
        }
    };

    // The audit row names the device and never the code. An ordinary mint keeps
    // the bare name it always had; a mint that asked for administration says so,
    // which is the detail the devices page writes for the same act.
    let audited = if allow_admin {
        format!("{}; admin=true", record.name)
    } else {
        record.name.clone()
    };
    if let Err(error) = store.audit(
        token.device_id(),
        "device.pairing_code_issued",
        Some(&audited),
    ) {
        return DeviceError::db_unavailable(format!(
            "the code was minted but the audit row could not be appended: {error}"
        ))
        .into_response();
    }
    (
        StatusCode::CREATED,
        axum::Json(serde_json::json!({
            "ok": true,
            "device_id": device_id,
            "code": issued.code(),
            "expires_at": format_timestamp(issued.expires_at()),
            "admin": allow_admin,
        })),
    )
        .into_response()
}

/// A body value read as a switch: `true`, `on` or `1`, the three spellings a form
/// and a JSON client use for a ticked box.
fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "on" | "1"
    )
}

// ---------------------------------------------------------------------------
// Sensor uploads and reads
// ---------------------------------------------------------------------------

/// Read one bounded JSON body and decode it into a typed shape.
async fn parse_json<T: serde::de::DeserializeOwned>(body: Body) -> Result<T, DeviceError> {
    let bytes = axum::body::to_bytes(body, MAX_BODY_BYTES)
        .await
        .map_err(|_| {
            DeviceError::bad_request(
                "body_too_large",
                format!("the body must be under {MAX_BODY_BYTES} bytes"),
            )
        })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        DeviceError::bad_request(
            "body_not_json",
            format!("the JSON body did not parse: {error}"),
        )
    })
}

/// The sensor store this service opened at start-up, or a 503.
fn sensor_store(service: &DeviceService) -> Result<&crate::sensors::SensorStore, DeviceError> {
    service
        .sensors()
        .ok_or_else(|| DeviceError::db_unavailable("the sensor store is unavailable"))
}

/// Refuse a location whose numbers cannot live in SQLite.
fn validate_location(upload: &LocationUpload) -> Result<(), DeviceError> {
    if !upload.latitude.is_finite() || !upload.longitude.is_finite() || !upload.accuracy.is_finite()
    {
        return Err(DeviceError::bad_request(
            "value_invalid",
            "latitude, longitude and accuracy must be finite numbers",
        ));
    }
    if let Some(altitude) = upload.altitude
        && !altitude.is_finite()
    {
        return Err(DeviceError::bad_request(
            "value_invalid",
            "altitude must be a finite number",
        ));
    }
    if upload.recorded_at.trim().is_empty() {
        return Err(DeviceError::bad_request(
            "recorded_at_required",
            "recordedAt must not be empty",
        ));
    }
    Ok(())
}

/// Refuse a health upload with no samples or a sample that cannot be stored.
fn validate_health(upload: &HealthUpload) -> Result<(), DeviceError> {
    if upload.samples.is_empty() {
        return Err(DeviceError::bad_request(
            "samples_required",
            "the samples array must not be empty",
        ));
    }
    for sample in &upload.samples {
        if sample.metric.trim().is_empty() || sample.unit.trim().is_empty() {
            return Err(DeviceError::bad_request(
                "sample_invalid",
                "each sample needs a metric and a unit",
            ));
        }
        if !sample.value.is_finite() {
            return Err(DeviceError::bad_request(
                "sample_invalid",
                "each sample value must be a finite number",
            ));
        }
        if sample.start_at.trim().is_empty() {
            return Err(DeviceError::bad_request(
                "sample_invalid",
                "each sample needs a startAt",
            ));
        }
    }
    Ok(())
}

/// `POST /v1/device/sensor/location` - record one location point.
///
/// Behind the bearer and the `chat` scope, the same scope `/v1/turns` uses. The
/// ack is the exact `deliveryState == "delivered"` the iOS `DeliveryResult`
/// decoder requires.
pub async fn upload_location(
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
    let upload = match parse_json::<LocationUpload>(body).await {
        Ok(upload) => upload,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = validate_location(&upload) {
        return error.into_response();
    }
    let store = match sensor_store(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = store.record_location(&upload) {
        return DeviceError::db_unavailable(format!("cannot store the location: {error}"))
            .into_response();
    }
    axum::Json(Delivered::new()).into_response()
}

/// `POST /v1/device/sensor/health` - record health samples.
///
/// Same bearer and scope as the location upload. One row per sample lands in
/// `health_samples`, `health_latest` is upserted per metric, and duplicates are
/// dropped before either write.
pub async fn upload_health(
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
    let upload = match parse_json::<HealthUpload>(body).await {
        Ok(upload) => upload,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = validate_health(&upload) {
        return error.into_response();
    }
    let store = match sensor_store(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = store.record_health(&upload.samples) {
        return DeviceError::db_unavailable(format!("cannot store the health samples: {error}"))
            .into_response();
    }
    axum::Json(Delivered::new()).into_response()
}

/// `GET /v1/device/health/latest` - the latest reading per metric, ordered by
/// metric, behind the bearer and the `chat` scope.
pub async fn health_latest(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let store = match sensor_store(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.latest_health() {
        Ok(metrics) => axum::Json(HealthLatest { metrics }).into_response(),
        Err(error) => {
            DeviceError::db_unavailable(format!("cannot read the latest health: {error}"))
                .into_response()
        }
    }
}

/// `GET /v1/device/location` - the current location, behind the bearer and the
/// `chat` scope. A daemon with no recorded point answers 404 `location_not_found`.
pub async fn location(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let store = match sensor_store(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.current_location() {
        Ok(Some(snapshot)) => axum::Json(snapshot).into_response(),
        Ok(None) => DeviceError::new(
            StatusCode::NOT_FOUND,
            "location_not_found",
            "no location has been recorded yet",
        )
        .into_response(),
        Err(error) => {
            DeviceError::db_unavailable(format!("cannot read the current location: {error}"))
                .into_response()
        }
    }
}

// ---------------------------------------------------------------------------
// Inbox
// ---------------------------------------------------------------------------

/// The legacy `success(data)` envelope: `{data, meta:{requestId, timestamp}}`.
/// Used by the inbox, push and talk routes exactly as the relay wrapped them.
fn success(data: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "data": data,
        "meta": {
            "requestId": uuid::Uuid::now_v7().to_string(),
            "timestamp": format_timestamp(crate::auth::now()),
        }
    })
}

/// The inbox store this service opened, or a 503.
fn inbox_store(service: &DeviceService) -> Result<&InboxStore, DeviceError> {
    service
        .inbox()
        .ok_or_else(|| DeviceError::db_unavailable("the inbox store is unavailable"))
}

/// The notes store this service opened, or a 503.
fn notes_store(service: &DeviceService) -> Result<&NotesStore, DeviceError> {
    service
        .notes()
        .ok_or_else(|| DeviceError::db_unavailable("the notes store is unavailable"))
}

/// The push store this service opened, or a 503.
fn push_store(service: &DeviceService) -> Result<&PushStore, DeviceError> {
    service
        .push()
        .ok_or_else(|| DeviceError::db_unavailable("the push store is unavailable"))
}

/// Read one bounded JSON body as an object. An empty body reads as `{}` (the
/// legacy `_parse_body` behaviour); malformed JSON is a 400 rather than a silent
/// `{}`, so a broken body never turns into a phantom write.
async fn parse_json_object(body: Body) -> Result<serde_json::Value, DeviceError> {
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
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(serde_json::Value::Object(serde_json::Map::new()));
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|error| {
        DeviceError::bad_request(
            "body_not_json",
            format!("the JSON body did not parse: {error}"),
        )
    })?;
    if !value.is_object() {
        return Err(DeviceError::bad_request(
            "body_not_object",
            "the JSON body must be an object",
        ));
    }
    Ok(value)
}

/// A string field, when present and a JSON string.
fn obj_str(body: &serde_json::Value, key: &str) -> Option<String> {
    body.get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// A boolean field, when present and a JSON bool.
fn obj_bool(body: &serde_json::Value, key: &str) -> Option<bool> {
    body.get(key).and_then(serde_json::Value::as_bool)
}

/// An integer field, when present and a JSON integer.
fn obj_i64(body: &serde_json::Value, key: &str) -> Option<i64> {
    body.get(key).and_then(serde_json::Value::as_i64)
}

/// `GET /v1/inbox` - the device's live inbox items, newest first.
pub async fn inbox(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let store = match inbox_store(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.list(token.device_id()) {
        Ok(items) => axum::Json(success(serde_json::json!({ "items": items }))).into_response(),
        Err(error) => {
            DeviceError::db_unavailable(format!("cannot read the inbox: {error}")).into_response()
        }
    }
}

/// `POST /v1/inbox/{item_id}/action` - act on one item.
pub async fn inbox_action(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    Path(item_id): Path<String>,
    body: Body,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let request = match parse_json::<InboxActionRequest>(body).await {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    if request.action_id.trim().is_empty() {
        return DeviceError::bad_request("action_id_required", "actionId must not be empty")
            .into_response();
    }
    let store = match inbox_store(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.record_action(token.device_id(), &item_id, &request.action_id, "user") {
        Ok(result) => axum::Json(success(serde_json::json!(result))).into_response(),
        Err(crate::inbox::InboxError::NotFound) => DeviceError::new(
            StatusCode::NOT_FOUND,
            "inbox_item_not_found",
            "no inbox item with that id belongs to this device",
        )
        .into_response(),
        Err(error) => DeviceError::db_unavailable(format!("cannot act on the inbox item: {error}"))
            .into_response(),
    }
}

/// `POST /internal/inbox/create` - create an inbox item behind the `admin` scope.
///
/// The legacy relay guarded this with an `X-Relay-Internal-Key` header; Eris has
/// no internal key, so a bearer token with the `admin` scope is the gate instead.
pub async fn internal_inbox_create(
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
    let request = match parse_json::<InternalInboxCreateRequest>(body).await {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    if request.kind.trim().is_empty()
        || request.title.trim().is_empty()
        || request.body.trim().is_empty()
    {
        return DeviceError::bad_request("invalid_request", "kind, title and body are required")
            .into_response();
    }
    // The target device: an explicit `deviceId` (or legacy `userId` alias), else
    // the authenticated admin's own device.
    let target = request
        .device_id
        .as_deref()
        .or(request.user_id.as_deref())
        .unwrap_or_else(|| token.device_id());
    let store = match inbox_store(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    let priority = if request.priority.trim().is_empty() {
        "normal"
    } else {
        &request.priority
    };
    match store.create(
        target,
        &request.kind,
        &request.title,
        &request.body,
        priority,
        request.payload.as_ref(),
        request.expires_at.as_deref(),
    ) {
        Ok(item) => axum::Json(success(serde_json::json!({ "item": item }))).into_response(),
        Err(error) => DeviceError::db_unavailable(format!("cannot create the inbox item: {error}"))
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// Notes
// ---------------------------------------------------------------------------

/// The JSON body a note create/patch reads, without a model: field presence is
/// the contract (the legacy `_parse_body` + `in` checks).
fn note_store_or_503(service: &DeviceService) -> Result<&NotesStore, DeviceError> {
    notes_store(service)
}

/// `GET /v1/notes` - list live notes, newest first.
pub async fn list_notes(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let store = match note_store_or_503(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.list(token.device_id()) {
        Ok(notes) => {
            let count = notes.len();
            axum::Json(serde_json::json!({
                "data": notes,
                "meta": { "count": count }
            }))
            .into_response()
        }
        Err(error) => {
            DeviceError::db_unavailable(format!("cannot list notes: {error}")).into_response()
        }
    }
}

/// `POST /v1/notes` - create a note.
pub async fn create_note(
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
    let body = match parse_json_object(body).await {
        Ok(body) => body,
        Err(error) => return error.into_response(),
    };
    let title = obj_str(&body, "title").unwrap_or_default();
    let folder_id = obj_str(&body, "folderId");
    let pinned = obj_bool(&body, "pinned").unwrap_or(false);
    let store = match note_store_or_503(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.create(token.device_id(), &title, folder_id.as_deref(), pinned) {
        Ok(note) => (
            StatusCode::CREATED,
            axum::Json(serde_json::json!({ "data": note })),
        )
            .into_response(),
        Err(error) => {
            DeviceError::db_unavailable(format!("cannot create the note: {error}")).into_response()
        }
    }
}

/// `GET /v1/notes/{id}` - one note.
pub async fn get_note(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    Path(note_id): Path<String>,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let store = match note_store_or_503(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.get(token.device_id(), &note_id) {
        Ok(Some(note)) => axum::Json(serde_json::json!({ "data": note })).into_response(),
        Ok(None) => note_not_found().into_response(),
        Err(error) => {
            DeviceError::db_unavailable(format!("cannot read the note: {error}")).into_response()
        }
    }
}

/// `PATCH /v1/notes/{id}` - update note metadata, optimistic concurrency via
/// the `If-Match` header (`"<revision>"`).
pub async fn patch_note(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    Path(note_id): Path<String>,
    body: Body,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let if_match = match headers
        .get(header::IF_MATCH)
        .and_then(|value| value.to_str().ok())
    {
        Some(value) => value,
        None => {
            return DeviceError::new(
                StatusCode::CONFLICT,
                "if_match_required",
                "If-Match header is required for note updates.",
            )
            .into_response();
        }
    };
    let body = match parse_json_object(body).await {
        Ok(body) => body,
        Err(error) => return error.into_response(),
    };
    let title = obj_str(&body, "title");
    let folder_id = body
        .get("folderId")
        .map(|value| value.as_str().map(str::to_owned));
    let pinned = obj_bool(&body, "pinned");
    let store = match note_store_or_503(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.patch(
        token.device_id(),
        &note_id,
        if_match,
        title.as_deref(),
        folder_id,
        pinned,
    ) {
        Ok(PatchOutcome::Updated(note)) => {
            axum::Json(serde_json::json!({ "data": note })).into_response()
        }
        Ok(PatchOutcome::NoteNotFound) => note_not_found().into_response(),
        Ok(PatchOutcome::RevisionMismatch) => DeviceError::new(
            StatusCode::CONFLICT,
            "revision_mismatch",
            "Note revision mismatch.",
        )
        .into_response(),
        Err(error) => {
            DeviceError::db_unavailable(format!("cannot update the note: {error}")).into_response()
        }
    }
}

/// `DELETE /v1/notes/{id}` - soft-delete a note.
pub async fn delete_note(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    Path(note_id): Path<String>,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let store = match note_store_or_503(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.delete(token.device_id(), &note_id) {
        Ok(true) => axum::Json(serde_json::json!({
            "data": { "id": note_id, "deleted": true }
        }))
        .into_response(),
        Ok(false) => note_not_found().into_response(),
        Err(error) => {
            DeviceError::db_unavailable(format!("cannot delete the note: {error}")).into_response()
        }
    }
}

/// `PUT /v1/notes/{id}/blobs/{revision}` - record a drawing blob's metadata.
pub async fn upload_note_blob(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    Path((note_id, revision)): Path<(String, i64)>,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let content_length = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.parse::<i64>().ok())
        .unwrap_or(0);
    if content_length > 25 * 1024 * 1024 {
        return DeviceError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "blob_too_large",
            "Blob exceeds 25 MB limit.",
        )
        .into_response();
    }
    let content_hash = headers
        .get("x-content-sha256")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_else(|| "pending".to_owned());
    let storage_path = format!("notes/{note_id}/rev-{revision}.pkdrawing");
    let store = match note_store_or_503(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.upload_blob(
        token.device_id(),
        &note_id,
        revision,
        &content_hash,
        content_length,
        &storage_path,
    ) {
        Ok(BlobUploadOutcome::Ok(view)) => {
            axum::Json(serde_json::json!({ "data": view })).into_response()
        }
        Ok(BlobUploadOutcome::NoteNotFound) => note_not_found().into_response(),
        Ok(BlobUploadOutcome::RevisionExists) => DeviceError::new(
            StatusCode::CONFLICT,
            "revision_exists",
            format!("Revision {revision} already exists."),
        )
        .into_response(),
        Err(error) => {
            DeviceError::db_unavailable(format!("cannot store the blob: {error}")).into_response()
        }
    }
}

/// `GET /v1/notes/{id}/blobs/{revision}` - one drawing blob's metadata.
pub async fn download_note_blob(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    Path((note_id, revision)): Path<(String, i64)>,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let store = match note_store_or_503(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.download_blob(token.device_id(), &note_id, revision) {
        Ok(BlobDownloadOutcome::Ok(view)) => {
            axum::Json(serde_json::json!({ "data": view })).into_response()
        }
        Ok(BlobDownloadOutcome::NoteNotFound) => note_not_found().into_response(),
        Ok(BlobDownloadOutcome::BlobNotFound) => {
            DeviceError::new(StatusCode::NOT_FOUND, "blob_not_found", "Blob not found.")
                .into_response()
        }
        Err(error) => {
            DeviceError::db_unavailable(format!("cannot read the blob: {error}")).into_response()
        }
    }
}

/// `POST /v1/notes/{id}/recognitions` - store an OCR/handwriting result.
pub async fn create_recognition(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    Path(note_id): Path<String>,
    body: Body,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let body = match parse_json_object(body).await {
        Ok(body) => body,
        Err(error) => return error.into_response(),
    };
    let drawing_revision = obj_i64(&body, "drawingRevision");
    let engine = obj_str(&body, "engine").filter(|value| !value.trim().is_empty());
    let raw_text = obj_str(&body, "rawText").filter(|value| !value.trim().is_empty());
    let (Some(drawing_revision), Some(engine), Some(raw_text)) =
        (drawing_revision, engine, raw_text)
    else {
        return DeviceError::bad_request(
            "invalid_request",
            "drawingRevision, engine, and rawText are required; drawingRevision may be 0 when the note has no ink.",
        )
        .into_response();
    };
    let engine_version = obj_str(&body, "engineVersion");
    let languages = body.get("languages").cloned();
    let user_corrected_text = obj_str(&body, "userCorrectedText");
    let store = match note_store_or_503(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.create_recognition(
        token.device_id(),
        &note_id,
        drawing_revision,
        &engine,
        engine_version.as_deref(),
        languages.as_ref(),
        &raw_text,
        user_corrected_text.as_deref(),
    ) {
        Ok(RecognitionOutcome::Ok(view)) => (
            StatusCode::CREATED,
            axum::Json(serde_json::json!({ "data": view })),
        )
            .into_response(),
        Ok(RecognitionOutcome::NoteNotFound) => note_not_found().into_response(),
        Err(error) => DeviceError::db_unavailable(format!("cannot store the recognition: {error}"))
            .into_response(),
    }
}

/// `POST /v1/notes/{id}/runs` - start an enrichment run (stub execution).
pub async fn create_note_run(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    Path(note_id): Path<String>,
    body: Body,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let body = match parse_json_object(body).await {
        Ok(body) => body,
        Err(error) => return error.into_response(),
    };
    let client_run_id = obj_str(&body, "clientRunId").filter(|value| !value.trim().is_empty());
    let Some(client_run_id) = client_run_id else {
        return DeviceError::bad_request("invalid_request", "clientRunId is required.")
            .into_response();
    };
    let source_drawing_revision = obj_i64(&body, "sourceDrawingRevision");
    let source_text_revision = obj_i64(&body, "sourceTextRevision");
    let requested_directives = body
        .get("directives")
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
    let store = match note_store_or_503(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.create_run(
        token.device_id(),
        &note_id,
        &client_run_id,
        source_drawing_revision,
        source_text_revision,
        requested_directives,
    ) {
        Ok(RunCreateOutcome::Created(view)) => (
            StatusCode::CREATED,
            axum::Json(serde_json::json!({ "data": view })),
        )
            .into_response(),
        Ok(RunCreateOutcome::Existing(view)) => {
            axum::Json(serde_json::json!({ "data": view })).into_response()
        }
        Ok(RunCreateOutcome::NoteNotFound) => note_not_found().into_response(),
        Err(error) => {
            DeviceError::db_unavailable(format!("cannot start the run: {error}")).into_response()
        }
    }
}

/// `GET /v1/note-runs/{run_id}` - one run.
pub async fn get_note_run(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let store = match note_store_or_503(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.get_run(token.device_id(), &run_id) {
        Ok(Some(run)) => axum::Json(serde_json::json!({ "data": run })).into_response(),
        Ok(None) => run_not_found().into_response(),
        Err(error) => {
            DeviceError::db_unavailable(format!("cannot read the run: {error}")).into_response()
        }
    }
}

/// The query `GET /v1/note-runs/{run_id}/events` accepts: `after` (cursor) and
/// `limit` (bounded to 500). Both are strings so a malformed value is ignored,
/// the way the legacy route swallowed a bad `int(...)`.
#[derive(serde::Deserialize)]
pub struct NoteRunEventsQuery {
    after: Option<String>,
    limit: Option<String>,
}

/// `GET /v1/note-runs/{run_id}/events` - cursor-paginated event log.
pub async fn note_run_events(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
    Query(query): Query<NoteRunEventsQuery>,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    // Last-Event-ID header takes precedence, then ?after=.
    let cursor = headers
        .get(LAST_EVENT_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.parse::<i64>().ok())
        .or_else(|| {
            query
                .after
                .as_deref()
                .and_then(|text| text.parse::<i64>().ok())
        });
    let limit = query
        .limit
        .as_deref()
        .and_then(|text| text.parse::<i64>().ok())
        .unwrap_or(100)
        .clamp(1, 500);
    let store = match note_store_or_503(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.run_events(token.device_id(), &run_id, cursor, limit) {
        Ok(Some(events)) => {
            let count = events.len();
            axum::Json(serde_json::json!({
                "data": events,
                "meta": { "count": count, "runId": run_id }
            }))
            .into_response()
        }
        Ok(None) => run_not_found().into_response(),
        Err(error) => DeviceError::db_unavailable(format!("cannot read the run events: {error}"))
            .into_response(),
    }
}

/// `POST /v1/note-runs/{run_id}/cancel` - cancel a run (idempotent).
pub async fn cancel_note_run(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let store = match note_store_or_503(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.cancel_run(token.device_id(), &run_id) {
        Ok(Some(run)) => axum::Json(serde_json::json!({ "data": run })).into_response(),
        Ok(None) => run_not_found().into_response(),
        Err(error) => {
            DeviceError::db_unavailable(format!("cannot cancel the run: {error}")).into_response()
        }
    }
}

/// A 404 for a note the device does not own (or that does not exist).
fn note_not_found() -> DeviceError {
    DeviceError::new(
        StatusCode::NOT_FOUND,
        "note_not_found",
        "no note with that id belongs to this device",
    )
}

/// A 404 for a run the device does not own (or that does not exist).
fn run_not_found() -> DeviceError {
    DeviceError::new(
        StatusCode::NOT_FOUND,
        "run_not_found",
        "no run with that id belongs to this device",
    )
}

// ---------------------------------------------------------------------------
// Push
// ---------------------------------------------------------------------------

/// `POST /v1/push/register` - upsert a device's push token.
pub async fn register_push(
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
    let request = match parse_json::<PushRegisterRequest>(body).await {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    // The token is authoritative; a body that names another device is refused.
    if let Some(device_id) = request.device_id.as_deref()
        && device_id != token.device_id()
    {
        return DeviceError::new(
            StatusCode::FORBIDDEN,
            "scope_denied",
            "Cannot register push token for another device.",
        )
        .into_response();
    }
    if request.push_environment.trim().is_empty() || request.bundle_id.trim().is_empty() {
        return DeviceError::bad_request(
            "invalid_request",
            "pushEnvironment and bundleId are required.",
        )
        .into_response();
    }
    if request.transport == "direct" && request.apns_token.is_none() {
        return DeviceError::bad_request(
            "invalid_request",
            "Direct push registration requires apnsToken.",
        )
        .into_response();
    }
    if request.transport == "relay"
        && [
            request.relay_handle.as_ref(),
            request.send_grant.as_ref(),
            request.relay_id.as_ref(),
            request.relay_public_key.as_ref(),
        ]
        .iter()
        .any(Option::is_none)
    {
        return DeviceError::bad_request(
            "invalid_request",
            "Relay push registration requires relayHandle, sendGrant, relayId, and relayPublicKey.",
        )
        .into_response();
    }
    let store = match push_store(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.upsert(
        token.device_id(),
        &request.transport,
        request.apns_token.as_deref(),
        &request.push_environment,
        &request.bundle_id,
        request.relay_handle.as_deref(),
        request.send_grant.as_deref(),
        request.relay_id.as_deref(),
        request.relay_public_key.as_deref(),
        request.token_debug_suffix.as_deref(),
    ) {
        Ok(updated_at) => axum::Json(success(serde_json::json!({
            "registered": true,
            "updatedAt": updated_at,
        })))
        .into_response(),
        Err(error) => {
            DeviceError::db_unavailable(format!("cannot register push: {error}")).into_response()
        }
    }
}

/// `POST /v1/push/deactivate` - deactivate a device's push registrations.
pub async fn deactivate_push(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    let store = match push_store(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };
    match store.deactivate(token.device_id()) {
        Ok(count) => axum::Json(success(serde_json::json!({
            "deactivated": true,
            "count": count,
        })))
        .into_response(),
        Err(error) => {
            DeviceError::db_unavailable(format!("cannot deactivate push: {error}")).into_response()
        }
    }
}

/// The body `POST /v1/push/send` accepts. The legacy relay's shape plus the two
/// addressing fields this daemon needs, because Eris has no `user_id` to fan out
/// from: a token belongs to a device.
#[derive(Debug, Default, Deserialize)]
struct PushSendRequest {
    /// Deliver to one device. Omitted with `all`, it is the caller's own device.
    #[serde(rename = "deviceId", default)]
    device_id: Option<String>,
    /// Deliver to every active registration. Requires `admin`, which this route
    /// already demands, so one compromised phone cannot spam every other phone.
    #[serde(default)]
    all: bool,
    /// `silent` (default) or `alert`.
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(rename = "userInfo", default)]
    user_info: Option<serde_json::Value>,
}

/// One delivered-or-not answer, so a caller sees which device failed and why.
fn push_report(
    device_id: &str,
    token_suffix: &str,
    outcome: &crate::apns::Outcome,
) -> serde_json::Value {
    let (state, reason) = match outcome {
        crate::apns::Outcome::Sent => ("sent", None),
        crate::apns::Outcome::TokenInvalid => ("token_invalid", None),
        crate::apns::Outcome::Rejected { status, reason } => {
            ("rejected", Some(format!("{status} {reason}")))
        }
        crate::apns::Outcome::Transient { detail } => ("transient", Some(detail.clone())),
    };
    let mut value = serde_json::json!({
        "deviceId": device_id,
        "tokenSuffix": token_suffix,
        "state": state,
    });
    if let Some(reason) = reason
        && let Some(object) = value.as_object_mut()
    {
        object.insert("reason".to_owned(), serde_json::json!(reason));
    }
    value
}

/// The last four characters of a token. A log or a response names the token
/// without carrying it: eight characters of a push token is enough to address a
/// phone, and a report is not a place to keep one.
fn token_suffix(token: &str) -> String {
    let trimmed = token.trim();
    let start = trimmed.len().saturating_sub(4);
    trimmed[start..].to_owned()
}

/// `POST /v1/push/send` - deliver a push through APNs.
///
/// `admin` scope, because a push can wake every phone the operator owns. A host
/// with no APNs key answers `503 apns_not_configured` exactly as it did before,
/// which is honest: nothing was sent. A host with a key but a token APNs has
/// retired answers `sent: 0` and a per-device report saying
/// `token_invalid`, and the registration is deactivated so the next send does
/// not repeat the round trip.
pub async fn send_push(
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
    let Some(sender) = service.apns().cloned() else {
        if let Some(fault) = service.apns_fault() {
            // Somebody configured APNs and got a variable wrong. Saying "not
            // configured" here would send them looking for a setting they
            // already set, which is exactly how this stayed broken for a week.
            return DeviceError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "apns_misconfigured",
                format!("APNs is configured but unusable: {fault}"),
            )
            .into_response();
        }
        tracing::info!(
            device_id = token.device_id(),
            "push send requested but APNs is not configured"
        );
        return DeviceError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "apns_not_configured",
            "APNs not configured. Set APNS_KEY_PATH, APNS_KEY_ID, APNS_TEAM_ID.",
        )
        .into_response();
    };
    let request: PushSendRequest = match parse_json(body).await {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    let store = match push_store(&service) {
        Ok(store) => store,
        Err(error) => return error.into_response(),
    };

    let alert = request.kind.as_deref().unwrap_or("silent") == "alert";
    if alert && request.title.as_deref().unwrap_or("").trim().is_empty() {
        return DeviceError::bad_request(
            "invalid_request",
            "an alert push needs a title; a silent one does not.",
        )
        .into_response();
    }

    // Which registrations to reach: every active one, or one device's. A
    // request that names neither reaches the caller's own device, which is what
    // makes the route usable from a phone without handing it a device id.
    let targets: Vec<crate::push::PushTarget> = if request.all {
        match store.all_active() {
            Ok(rows) => rows,
            Err(error) => {
                return DeviceError::db_unavailable(format!(
                    "cannot read the push registrations: {error}"
                ))
                .into_response();
            }
        }
    } else {
        let device = request.device_id.as_deref().unwrap_or(token.device_id());
        match store.active_registrations(device) {
            Ok(rows) => rows
                .into_iter()
                .map(|(apns_token, environment, bundle)| {
                    (device.to_owned(), apns_token, environment, bundle)
                })
                .collect(),
            Err(error) => {
                return DeviceError::db_unavailable(format!(
                    "cannot read the push registrations: {error}"
                ))
                .into_response();
            }
        }
    };

    if targets.is_empty() {
        return DeviceError::new(
            StatusCode::CONFLICT,
            "no_registration",
            "this device has no active push registration, so there is nothing to send to",
        )
        .into_response();
    }

    let mut reports = Vec::with_capacity(targets.len());
    for (device_id, apns_token, environment, bundle) in targets {
        let Some(apns_token) = apns_token.filter(|value| !value.trim().is_empty()) else {
            // A `relay` transport registration has no token this host can use.
            // Reported rather than skipped silently.
            reports.push(serde_json::json!({
                "deviceId": device_id,
                "tokenSuffix": serde_json::Value::Null,
                "state": "no_token",
                "reason": "the registration carries no APNs token",
            }));
            continue;
        };
        let outcome = if alert {
            sender
                .send_alert(
                    &apns_token,
                    bundle.as_deref(),
                    Some(environment.as_str()),
                    request.title.as_deref().unwrap_or_default(),
                    request.body.as_deref().unwrap_or_default(),
                    request.category.as_deref(),
                    request.user_info.as_ref(),
                )
                .await
        } else {
            sender
                .send_silent(&apns_token, bundle.as_deref(), Some(environment.as_str()))
                .await
        };

        // A token that Apple does not recognise on the recorded host may still
        // be a good token on the other one: which host a token belongs to is a
        // property of the entitlement it was minted under, and an app can report
        // the wrong environment. This one does exactly that: its entitlements
        // say `production`, it is a development build, and it guesses from an
        // App Store receipt that is absent on a device-installed build. One
        // retry against the other host turns that into a delivered push instead
        // of a registration retired in error.
        let recorded_host = sender.host_for(Some(environment.as_str()));
        let (outcome, delivered_host) = if outcome.retires_token() {
            let other = sender.other_host(recorded_host);
            tracing::info!(
                device_id = %device_id,
                recorded = %environment,
                retrying_against = %other,
                "the recorded APNs host did not recognise this token; trying the other one"
            );
            let retried = if alert {
                sender
                    .send_alert_to(
                        other,
                        &apns_token,
                        bundle.as_deref(),
                        request.title.as_deref().unwrap_or_default(),
                        request.body.as_deref().unwrap_or_default(),
                        request.category.as_deref(),
                        request.user_info.as_ref(),
                    )
                    .await
            } else {
                sender
                    .send_silent_to(other, &apns_token, bundle.as_deref())
                    .await
            };
            (retried, other)
        } else {
            (outcome, recorded_host)
        };

        // Write the proved host down so every later send goes straight there. A
        // token is valid against one host only, and paying for a rejected
        // request each time is the kind of quiet waste that hides for a week.
        if outcome == crate::apns::Outcome::Sent {
            let proved = if delivered_host == crate::apns::PRODUCTION_HOST {
                "production"
            } else {
                "development"
            };
            if proved != environment {
                let _ = store.set_environment(&device_id, &apns_token, proved);
                tracing::info!(
                    device_id = %device_id,
                    environment = %proved,
                    "corrected the recorded APNs environment from a delivered retry"
                );
            }
        }

        if outcome.retires_token() {
            // The token is dead. Retire the row so the count of live
            // registrations means something and the next send is cheaper.
            let _ = store.deactivate_token(&device_id, &apns_token);
            tracing::info!(
                device_id = %device_id,
                "APNs retired a push token; the registration is now inactive"
            );
        }
        reports.push(push_report(
            &device_id,
            &token_suffix(&apns_token),
            &outcome,
        ));
    }

    let sent = reports
        .iter()
        .filter(|report| report["state"] == "sent")
        .count();
    let detail = format!(
        "type={}; targets={}; sent={}",
        if alert { "alert" } else { "silent" },
        reports.len(),
        sent
    );
    if let Err(error) = service
        .engine()
        .auth()
        .audit(token.device_id(), "push.sent", Some(&detail))
    {
        tracing::warn!(%error, "could not audit the push send");
    }

    axum::Json(success(serde_json::json!({
        "sent": sent,
        "attempted": reports.len(),
        "results": reports,
    })))
    .into_response()
}

// ---------------------------------------------------------------------------
// Talk (placeholder - in development)
// ---------------------------------------------------------------------------

/// `GET /v1/talk/readiness` - always "not ready, in development".
pub async fn talk_readiness(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    axum::Json(success(serde_json::json!({
        "ready": false,
        "reason": "in_development",
    })))
    .into_response()
}

/// The typed "not implemented" reply for the talk session routes.
fn talk_not_implemented() -> DeviceError {
    DeviceError::new(
        StatusCode::NOT_IMPLEMENTED,
        "not_implemented",
        "talk mode is in development and is not implemented yet",
    )
}

/// `POST /v1/talk/session` - placeholder.
pub async fn create_talk_session(
    State(service): State<DeviceService>,
    headers: HeaderMap,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    talk_not_implemented().into_response()
}

/// `POST /v1/talk/session/{id}/end` - placeholder.
pub async fn end_talk_session(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    Path(_session_id): Path<String>,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    talk_not_implemented().into_response()
}

/// `POST /v1/talk/session/{id}/turns` - placeholder.
pub async fn create_talk_turn(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    Path(_session_id): Path<String>,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    talk_not_implemented().into_response()
}

/// `POST /v1/talk/session/{id}/inject` - placeholder.
pub async fn inject_talk_transcript(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    Path(_session_id): Path<String>,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    talk_not_implemented().into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    /// A ready device service plus a chat token and an admin token, on a scratch
    /// root so nothing touches the operator's state directory.
    struct Fixture {
        _dir: tempfile::TempDir,
        service: DeviceService,
        chat: String,
        admin: String,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("temp dir");
        let paths = crate::config::Paths::for_root(dir.path());
        let db = crate::db::Db::open(&paths.db_file).expect("db");
        let engine = crate::onboarding::SetupEngine::new(db.clone(), paths);
        let service = DeviceService::new(engine, 32);
        let tokens = TokenService::new(db);
        let device_id = tokens.register_device("test", "ios").expect("device");
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
        Fixture {
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
    ) -> (StatusCode, serde_json::Value) {
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
    async fn inbox_routes_work_end_to_end() {
        let fx = fixture();

        let (status, body) = send(&fx.service, "GET", "/v1/inbox", &fx.chat, "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["items"], serde_json::json!([]));

        // The internal create is admin-only.
        let (status, _) = send(
            &fx.service,
            "POST",
            "/internal/inbox/create",
            &fx.chat,
            r#"{"kind":"alert","title":"Hi","body":"There"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let (status, body) = send(
            &fx.service,
            "POST",
            "/internal/inbox/create",
            &fx.admin,
            r#"{"kind":"alert","title":"Hi","body":"There"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let item_id = body["data"]["item"]["id"].as_str().expect("id").to_owned();

        let (status, body) = send(&fx.service, "GET", "/v1/inbox", &fx.chat, "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["items"][0]["id"], serde_json::json!(item_id));

        let (status, body) = send(
            &fx.service,
            "POST",
            &format!("/v1/inbox/{item_id}/action"),
            &fx.chat,
            r#"{"actionId":"dismiss"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["status"], serde_json::json!("dismissed"));
    }

    #[tokio::test]
    async fn notes_routes_work_end_to_end() {
        let fx = fixture();

        let (status, body) = send(
            &fx.service,
            "POST",
            "/v1/notes",
            &fx.chat,
            r#"{"title":"M","pinned":true}"#,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["data"]["title"], serde_json::json!("M"));
        assert_eq!(body["data"]["pinned"], serde_json::json!(true));
        let id = body["data"]["id"].as_str().expect("id").to_owned();

        let (status, body) = send(&fx.service, "GET", "/v1/notes", &fx.chat, "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["meta"]["count"], serde_json::json!(1));

        let (status, body) =
            send(&fx.service, "GET", &format!("/v1/notes/{id}"), &fx.chat, "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["id"], serde_json::json!(id));

        // Patch without the If-Match header refuses with a 409.
        let (status, _) = send(
            &fx.service,
            "PATCH",
            &format!("/v1/notes/{id}"),
            &fx.chat,
            r#"{"title":"X"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn push_routes_work_end_to_end() {
        let fx = fixture();

        let (status, body) = send(
            &fx.service,
            "POST",
            "/v1/push/register",
            &fx.chat,
            r#"{"transport":"direct","apnsToken":"tok","pushEnvironment":"production","bundleId":"com.example"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["registered"], serde_json::json!(true));

        let (status, body) = send(&fx.service, "POST", "/v1/push/deactivate", &fx.chat, "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["deactivated"], serde_json::json!(true));
        assert_eq!(body["data"]["count"], serde_json::json!(1));

        // Send is admin-only, and with admin on an unconfigured host it is the
        // honest 503: nothing was sent, and it says so.
        let (status, _) = send(&fx.service, "POST", "/v1/push/send", &fx.chat, "{}").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, body) = send(&fx.service, "POST", "/v1/push/send", &fx.admin, "{}").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["code"], "apns_not_configured");
    }

    /// A throwaway ES256 key in PEM form. Not a real APNs key.
    fn test_pem() -> String {
        use base64::Engine as _;
        let rng = ring::rand::SystemRandom::new();
        let document = ring::signature::EcdsaKeyPair::generate_pkcs8(
            &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            &rng,
        )
        .expect("generate");
        let encoded = base64::engine::general_purpose::STANDARD.encode(document.as_ref());
        format!("-----BEGIN PRIVATE KEY-----\n{encoded}\n-----END PRIVATE KEY-----\n")
    }

    #[tokio::test]
    async fn a_misconfigured_host_says_so_rather_than_claiming_nothing_is_set() {
        let mut fx = fixture();
        fx.service = fx
            .service
            .with_apns_fault("APNS_KEY_ID is set but APNS_TEAM_ID is not".to_owned());
        let (status, body) = send(&fx.service, "POST", "/v1/push/send", &fx.admin, "{}").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        // The distinction that matters: this is not `apns_not_configured`, so a
        // reader is not sent looking for a setting they already set.
        assert_eq!(body["code"], "apns_misconfigured");
        assert!(
            body["message"]
                .as_str()
                .unwrap_or_default()
                .contains("APNS_TEAM_ID")
        );
    }

    #[tokio::test]
    async fn a_configured_host_with_no_registration_says_there_is_nowhere_to_send() {
        let mut fx = fixture();
        let sender = crate::apns::ApnsSender::new(
            &test_pem(),
            "LH5GM8356P",
            "58U7UPFS53",
            Some("net.fihonline.kallisti".to_owned()),
            "development",
        )
        .expect("the generated key builds a sender");
        fx.service = fx.service.with_apns(std::sync::Arc::new(sender));

        let (status, body) = send(&fx.service, "POST", "/v1/push/send", &fx.admin, "{}").await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["code"], "no_registration");
    }

    #[tokio::test]
    async fn an_alert_push_without_a_title_is_refused_before_any_send() {
        let mut fx = fixture();
        let sender = crate::apns::ApnsSender::new(
            &test_pem(),
            "LH5GM8356P",
            "58U7UPFS53",
            Some("net.fihonline.kallisti".to_owned()),
            "development",
        )
        .expect("the generated key builds a sender");
        fx.service = fx.service.with_apns(std::sync::Arc::new(sender));

        let (status, body) = send(
            &fx.service,
            "POST",
            "/v1/push/send",
            &fx.admin,
            r#"{"type":"alert","body":"no title here"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["code"], "invalid_request");
    }

    #[test]
    fn a_token_is_named_by_its_last_four_only() {
        assert_eq!(token_suffix("0123456789abcdef"), "cdef");
        assert_eq!(token_suffix("ab"), "ab");
        assert_eq!(token_suffix(""), "");
        // Never the whole token, and never longer than four.
        assert!(!token_suffix("0123456789abcdef").contains("0123"));
    }

    #[test]
    fn a_report_states_the_outcome_without_carrying_the_token() {
        let report = push_report(
            "device-1",
            "cdef",
            &crate::apns::Outcome::Transient {
                detail: "could not connect to APNs".to_owned(),
            },
        );
        assert_eq!(report["deviceId"], "device-1");
        assert_eq!(report["tokenSuffix"], "cdef");
        assert_eq!(report["state"], "transient");
        assert!(
            report["reason"]
                .as_str()
                .unwrap_or_default()
                .contains("connect")
        );

        let sent = push_report("device-2", "1234", &crate::apns::Outcome::Sent);
        assert_eq!(sent["state"], "sent");
        assert!(sent.get("reason").is_none());

        let dead = push_report("device-3", "abcd", &crate::apns::Outcome::TokenInvalid);
        assert_eq!(dead["state"], "token_invalid");
    }

    #[tokio::test]
    async fn talk_routes_are_placeholders() {
        let fx = fixture();

        let (status, body) = send(&fx.service, "GET", "/v1/talk/readiness", &fx.chat, "").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["data"]["ready"], serde_json::json!(false));
        assert_eq!(body["data"]["reason"], serde_json::json!("in_development"));

        let (status, body) = send(&fx.service, "POST", "/v1/talk/session", &fx.chat, "").await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert_eq!(body["code"], serde_json::json!("not_implemented"));
    }

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

    #[tokio::test]
    async fn a_terminal_job_without_a_terminal_frame_ends_its_stream() {
        let fx = fixture();
        let db = fx.service.engine().db().clone();
        let device_id: String = db
            .with_conn(|conn| {
                conn.query_row("SELECT id FROM devices LIMIT 1", [], |row| row.get(0))
            })
            .expect("device id");
        // A failed job whose log stops at a delta: the shape a process that
        // died mid-turn leaves behind. No worker will ever append to it.
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO jobs (id, kind, status, device_id, finished_at) \
                 VALUES ('job-1', 'chat', 'failed', ?1, \
                         strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
                [&device_id],
            )?;
            conn.execute(
                "INSERT INTO job_events (job_id, seq, kind, payload) \
                 VALUES ('job-1', 1, 'delta', '{\"text\":\"par\"}')",
                [],
            )?;
            Ok(())
        })
        .expect("job rows");

        let request = axum::http::Request::builder()
            .method("GET")
            .uri("/v1/turns/job-1/events")
            .header("authorization", format!("Bearer {}", fx.chat))
            .body(Body::empty())
            .expect("request");
        let response = fx.service.router().oneshot(request).await.expect("router");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("body");
        let body = String::from_utf8(bytes.to_vec()).expect("utf8");

        let events: Vec<&str> = body
            .lines()
            .filter(|line| line.starts_with("event:"))
            .collect();
        assert_eq!(
            events,
            ["event: delta", "event: error"],
            "the delta replays and the status supplies the ending"
        );
        assert!(
            body.contains("id: 2"),
            "the synthesised frame follows the replayed one: {body}"
        );
        assert!(
            body.contains("\"code\":\"interrupted\""),
            "the ending names the interruption: {body}"
        );
    }

    /// A fixture whose service can ask, plus one running job belonging to the
    /// paired device. Returns the registry so a test can park a question on it the
    /// way a turn would.
    fn asking_fixture() -> (Fixture, std::sync::Arc<crate::web::clarify::Registry>) {
        let mut fx = fixture();
        let registry = std::sync::Arc::new(crate::web::clarify::Registry::new());
        fx.service = fx
            .service
            .clone()
            .with_clarify(std::sync::Arc::clone(&registry));
        let db = fx.service.engine().db().clone();
        let device_id: String = db
            .with_conn(|conn| {
                conn.query_row("SELECT id FROM devices LIMIT 1", [], |row| row.get(0))
            })
            .expect("device id");
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO jobs (id, kind, status, device_id) \
                 VALUES ('job-ask', 'chat', 'running', ?1)",
                [&device_id],
            )?;
            Ok(())
        })
        .expect("job row");
        (fx, registry)
    }

    /// One question, in the shape the model calls.
    fn a_question() -> crate::web::clarify::Request {
        crate::web::clarify::Request::parse(&serde_json::json!({
            "title": "Which lane?",
            "questions": [{
                "question": "Which lane should this land on?",
                "choices": ["Web first", "All three"]
            }]
        }))
        .expect("a valid question")
    }

    #[tokio::test]
    async fn a_turn_with_no_question_says_so_rather_than_an_error() {
        let fx = fixture();
        let db = fx.service.engine().db().clone();
        let device_id: String = db
            .with_conn(|conn| {
                conn.query_row("SELECT id FROM devices LIMIT 1", [], |row| row.get(0))
            })
            .expect("device id");
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO jobs (id, kind, status, device_id) \
                 VALUES ('job-1', 'chat', 'running', ?1)",
                [&device_id],
            )?;
            Ok(())
        })
        .expect("job row");

        let (status, body) = send(&fx.service, "GET", "/v1/turns/job-1/asked", &fx.chat, "").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["ok"], serde_json::json!(true));
        assert_eq!(
            body["waiting"],
            serde_json::json!(false),
            "a turn that asked nothing is not waiting: {body}"
        );
        assert_eq!(body["asked"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn a_parked_question_is_readable_and_answerable_over_v1() {
        let (fx, registry) = asking_fixture();
        // Parked under the job id, which is where a turn that names no
        // conversation parks: the route falls back to the job id for exactly this.
        let gate = registry.park_for("job-ask", a_question());

        let (status, body) =
            send(&fx.service, "GET", "/v1/turns/job-ask/asked", &fx.chat, "").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(
            body["waiting"],
            serde_json::json!(true),
            "the question a turn is blocked on has to be readable: {body}"
        );
        assert_eq!(body["asked"]["id"], serde_json::json!(gate.id()));
        assert_eq!(
            body["asked"]["request"]["questions"][0]["question"],
            serde_json::json!("Which lane should this land on?")
        );

        let (status, body) = send(
            &fx.service,
            "POST",
            "/v1/turns/job-ask/answer",
            &fx.chat,
            &serde_json::json!({
                "labels": "Web first",
                "question": "Which lane should this land on?",
            })
            .to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["ok"], serde_json::json!(true));
        assert!(
            body["rendered"]
                .as_str()
                .unwrap_or_default()
                .contains("Web first"),
            "the model is told what was picked: {body}"
        );

        // One-way, like an approval: the second read finds nothing, because the
        // turn has already been woken with its answer.
        let (status, body) =
            send(&fx.service, "GET", "/v1/turns/job-ask/asked", &fx.chat, "").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["waiting"], serde_json::json!(false));

        // The parked handle saw the answer, which is the whole point of the route:
        // a settle that landed nowhere would leave a thread waiting out the hour.
        let answer = gate
            .wait(std::time::Duration::from_secs(0), || {
                crate::web::clarify::WaitPoll::Continue
            })
            .expect("the wait reports an answer rather than a stop");
        assert!(matches!(answer, crate::web::clarify::Answer::Chosen { .. }));
    }

    #[tokio::test]
    async fn an_answer_with_nothing_waiting_is_refused_by_name() {
        let (fx, _registry) = asking_fixture();
        let (status, body) = send(
            &fx.service,
            "POST",
            "/v1/turns/job-ask/answer",
            &fx.chat,
            &serde_json::json!({ "labels": "Web first" }).to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(
            body["code"],
            serde_json::json!("nothing_waiting"),
            "a settle nobody received must not read as a decision: {body}"
        );
    }

    #[tokio::test]
    async fn an_answer_needs_a_choice_or_some_words() {
        let (fx, registry) = asking_fixture();
        let _gate = registry.park_for("job-ask", a_question());
        let (status, body) = send(
            &fx.service,
            "POST",
            "/v1/turns/job-ask/answer",
            &fx.chat,
            "{}",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], serde_json::json!("answer_empty"));
    }

    #[tokio::test]
    async fn answering_a_finished_turn_says_the_turn_is_over() {
        let (fx, registry) = asking_fixture();
        let _gate = registry.park_for("job-ask", a_question());
        fx.service
            .engine()
            .db()
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE jobs SET status = 'succeeded' WHERE id = 'job-ask'",
                    [],
                )?;
                Ok(())
            })
            .expect("job update");
        let (status, body) = send(
            &fx.service,
            "POST",
            "/v1/turns/job-ask/answer",
            &fx.chat,
            &serde_json::json!({ "labels": "Web first" }).to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert_eq!(
            body["code"],
            serde_json::json!("turn_finished"),
            "an answer for a turn that ended has nowhere to land: {body}"
        );
    }

    #[tokio::test]
    async fn another_devices_question_is_not_readable() {
        let (fx, registry) = asking_fixture();
        let db = fx.service.engine().db().clone();
        // A job belonging to a second device, with a question parked on it.
        let other = crate::auth::AuthStore::new(db.clone())
            .register_device("another", "ios")
            .expect("second device");
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO jobs (id, kind, status, device_id) \
                 VALUES ('job-other', 'chat', 'running', ?1)",
                [&other],
            )?;
            Ok(())
        })
        .expect("the other device's job");
        let _gate = registry.park_for("job-other", a_question());

        let (status, body) = send(
            &fx.service,
            "GET",
            "/v1/turns/job-other/asked",
            &fx.chat,
            "",
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert_eq!(
            body["code"],
            serde_json::json!("job_not_found"),
            "another device's question is reported as missing, not as visible"
        );
        assert!(
            !body.to_string().contains(&other),
            "the answer says nothing about the other device: {body}"
        );
    }

    // -----------------------------------------------------------------------
    // The settings capability record
    // -----------------------------------------------------------------------

    /// The record, read the way a client reads it.
    fn record(fx: &Fixture) -> Vec<serde_json::Value> {
        read_settings(&fx.service).expect("the record")["capabilities"]
            .as_array()
            .expect("a capability list")
            .clone()
    }

    #[test]
    fn every_capability_carries_a_group_the_two_clients_can_render() {
        let fx = fixture();
        let entries = record(&fx);
        assert_eq!(
            entries.len(),
            SETTING_SPECS.len(),
            "every spec is in the record: {entries:?}"
        );
        for entry in &entries {
            let key = entry["key"].as_str().expect("a key");
            assert!(
                !entry["label"].as_str().unwrap_or_default().is_empty(),
                "{key} has a label"
            );
            let group = entry["group"].as_str().expect("a group");
            assert!(
                SETTING_GROUPS.contains(&group),
                "{key} is in `{group}`, which is not one of the six the clients render"
            );
            let kind = entry["type"].as_str().expect("a type");
            assert!(
                ["string", "enum", "bool", "int"].contains(&kind),
                "{key} is typed `{kind}`, which is not one of the four"
            );
            assert!(
                entry["writable"].is_boolean(),
                "{key} says whether it is writable"
            );
            assert!(
                entry["value"].is_string(),
                "{key} carries a value as a string"
            );
            match kind {
                "enum" => {
                    let options = entry["options"].as_array().expect("options for an enum");
                    assert!(!options.is_empty(), "{key} is an enum with no options");
                    let value = entry["value"].as_str().expect("a value");
                    assert!(
                        options.iter().any(|option| option.as_str() == Some(value)),
                        "{key} holds `{value}`, which is not one of its options"
                    );
                }
                _ => assert!(
                    entry["options"].is_null(),
                    "{key} is a {kind} and carries no options"
                ),
            }
        }
        // The six groups, and no field outside them.
        for group in SETTING_GROUPS {
            assert!(
                entries.iter().any(|entry| entry["group"] == group),
                "the `{group}` group has no field, so its tab would be empty"
            );
        }
        // The two the card names, at the group it names: permissions are `system`.
        for (key, group) in [("sandbox", "system"), ("approval", "system")] {
            let entry = entries
                .iter()
                .find(|entry| entry["key"] == key)
                .unwrap_or_else(|| panic!("{key} is in the record"));
            assert_eq!(entry["group"], group, "{key} lives in the system tab");
        }
        assert!(
            entries.iter().all(|entry| entry["group"] != "permissions"),
            "there is no permissions group"
        );
    }

    #[test]
    fn no_setup_key_and_no_derived_catalogue_reaches_the_record() {
        let fx = fixture();
        for entry in record(&fx) {
            let key = entry["key"].as_str().expect("a key");
            assert!(!key.starts_with("setup."), "{key} is a setup key");
            assert!(
                !key.starts_with("provider."),
                "{key} is the provider table's"
            );
        }
        assert!(setting_spec("setup.passphrase_hash").is_none());
    }

    #[tokio::test]
    async fn the_record_and_the_write_path_are_one_table_read_twice() {
        let fx = fixture();
        let entries = record(&fx);
        let writable: Vec<String> = entries
            .iter()
            .filter(|entry| entry["writable"] == serde_json::json!(true))
            .map(|entry| entry["key"].as_str().expect("a key").to_owned())
            .collect();
        let read_only: Vec<String> = entries
            .iter()
            .filter(|entry| entry["writable"] == serde_json::json!(false))
            .map(|entry| entry["key"].as_str().expect("a key").to_owned())
            .collect();
        assert!(!read_only.is_empty(), "the workspace path is read-only");
        assert_eq!(
            accepted_settings(),
            writable,
            "the accepted list is the record's writable entries"
        );

        // Every writable key reaches the route's own validation rather than being
        // refused as a field it does not know.
        for key in &writable {
            let current = entries
                .iter()
                .find(|entry| entry["key"] == key.as_str())
                .map(|entry| entry["value"].as_str().unwrap_or_default().to_owned())
                .expect("the entry");
            let body = serde_json::json!({ key: current }).to_string();
            let (status, answer) =
                send(&fx.service, "POST", "/v1/settings", &fx.admin, &body).await;
            let code = answer["code"].as_str().unwrap_or_default();
            assert_ne!(
                code, "settings_unknown_field",
                "{key} is in the record and the write refuses it as unknown: {answer}"
            );
            assert_ne!(
                code, "settings_read_only",
                "{key} is writable in the record and read-only on the write: {answer}"
            );
            // Sending the value it already holds is a 200 that changes nothing.
            if status == StatusCode::OK {
                assert_eq!(
                    answer["changed"],
                    serde_json::json!([]),
                    "{key} was sent its stored value and reported a change: {answer}"
                );
            }
        }

        // Every read-only key is refused, by name, with the code that says why.
        for key in &read_only {
            let body = serde_json::json!({ key: "anything" }).to_string();
            let (status, answer) =
                send(&fx.service, "POST", "/v1/settings", &fx.admin, &body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{key}: {answer}");
            assert_eq!(answer["code"], "settings_read_only", "{key}: {answer}");
            assert!(
                answer["message"].as_str().unwrap_or_default().contains(key),
                "the refusal names the field: {answer}"
            );
        }
    }

    #[tokio::test]
    async fn the_four_original_fields_behave_exactly_as_they_did() {
        let fx = fixture();
        // `sandbox` and `approval` together, the way a permissions form posts them.
        let (status, changed) = send(
            &fx.service,
            "POST",
            "/v1/settings",
            &fx.admin,
            r#"{"sandbox":"workspace-write"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{changed}");
        assert_eq!(changed["changed"], serde_json::json!(["sandbox"]));

        // A field mentioned with the value it already holds is not a change.
        let (status, again) = send(
            &fx.service,
            "POST",
            "/v1/settings",
            &fx.admin,
            r#"{"sandbox":"workspace-write","approval":"ask"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{again}");
        assert_eq!(again["changed"], serde_json::json!([]));

        // A refusal still names the field and still writes nothing.
        let (status, refused) = send(
            &fx.service,
            "POST",
            "/v1/settings",
            &fx.admin,
            r#"{"sandbox":"yolo"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
        assert_eq!(refused["code"], "sandbox_invalid");
        let after = read_settings(&fx.service).expect("the snapshot");
        assert_eq!(
            after["sandbox"], "workspace-write",
            "the refusal wrote nothing"
        );
    }

    #[tokio::test]
    async fn a_new_setting_is_written_read_back_and_audited() {
        let fx = fixture();
        let before = read_settings(&fx.service).expect("the snapshot");
        assert_eq!(before["clock_format"], "24h", "the default clock");
        assert_eq!(before["push"]["scope"], "origin", "the default scope");

        let body = serde_json::json!({
            "clock_format": " 12H ",
            "conversation_sort": "day",
            "push_notify_scope": "targeted",
            "push_notify_devices": " dev-a , dev-b , dev-a ,, ",
            "tools_enabled": false,
            "tool_rounds": "0030",
            "timezone": "-04:00",
        })
        .to_string();
        let (status, changed) = send(&fx.service, "POST", "/v1/settings", &fx.admin, &body).await;
        assert_eq!(status, StatusCode::OK, "{changed}");
        let listed = changed["changed"].as_array().expect("a changed list");
        for field in [
            "clock_format",
            "conversation_sort",
            "push_notify_scope",
            "push_notify_devices",
            "tools_enabled",
            "tool_rounds",
            "timezone",
        ] {
            assert!(
                listed.iter().any(|value| value == field),
                "{field} is not in the changed list: {changed}"
            );
        }

        // The spellings the form sent are stored as the spellings every reader
        // uses, so the record reads back what it offered.
        let after = read_settings(&fx.service).expect("the snapshot");
        assert_eq!(after["clock_format"], "12h", "the case is normalized");
        assert_eq!(after["push"]["scope"], "targeted");
        assert_eq!(
            after["push"]["devices"],
            serde_json::json!(["dev-a", "dev-b"]),
            "the list is trimmed and deduplicated"
        );
        assert_eq!(after["tools_enabled"], false);
        assert_eq!(after["tool_rounds"], 30, "the padding is dropped");
        assert_eq!(after["timezone"], "-04:00");
        let record = after["capabilities"].as_array().expect("a record");
        let entry = |key: &str| {
            record
                .iter()
                .find(|entry| entry["key"] == key)
                .unwrap_or_else(|| panic!("{key} is in the record"))
                .clone()
        };
        assert_eq!(entry("clock_format")["value"], "12h");
        assert_eq!(entry("conversation_sort")["value"], "day");
        assert_eq!(entry("push_notify_devices")["value"], "dev-a,dev-b");
        assert_eq!(entry("tool_rounds")["value"], "30");

        // One audit row, the device as the actor, every field it changed.
        let detail: String = fx
            .service
            .engine()
            .db()
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT detail FROM audit_log WHERE action = 'settings.device_update'",
                    [],
                    |row| row.get(0),
                )
            })
            .expect("the audit row");
        for pair in [
            "clock_format=12h",
            "push_notify_scope=targeted",
            "push_notify_devices=dev-a,dev-b",
            "tool_rounds=30",
        ] {
            assert!(
                detail.contains(pair),
                "the audit row names {pair}: {detail}"
            );
        }
    }

    #[tokio::test]
    async fn a_value_outside_a_setting_own_range_is_refused_by_name() {
        let fx = fixture();
        for (field, value, allowed) in [
            ("tool_rounds", "0", "1"),
            ("tool_rounds", "100000", "99999"),
            ("tool_rounds", "twelve", "whole number"),
            ("clock_format", "13h", "12h"),
            ("push_notify_scope", "everyone", "origin"),
            ("conversation_sort", "sideways", "updated"),
            ("tools_enabled", "yes", "true or false"),
        ] {
            let body = serde_json::json!({ field: value }).to_string();
            let (status, answer) =
                send(&fx.service, "POST", "/v1/settings", &fx.admin, &body).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{field}={value}: {answer}");
            assert_eq!(
                answer["code"], "settings_value_invalid",
                "{field}: {answer}"
            );
            assert!(
                answer["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains(allowed),
                "the refusal says what {field} accepts: {answer}"
            );
        }
        // A zone is checked by the zone parser, so it is named the same way.
        let (status, answer) = send(
            &fx.service,
            "POST",
            "/v1/settings",
            &fx.admin,
            r#"{"timezone":"Mars/Olympus"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}");
        assert!(
            answer["message"]
                .as_str()
                .unwrap_or_default()
                .contains("Mars/Olympus"),
            "the refusal quotes the value: {answer}"
        );
    }

    #[tokio::test]
    async fn one_bad_value_leaves_every_row_exactly_as_it_was() {
        let fx = fixture();
        let before = read_settings(&fx.service).expect("the snapshot");
        // A valid clock format beside an invalid scope: the body is refused whole.
        let (status, refused) = send(
            &fx.service,
            "POST",
            "/v1/settings",
            &fx.admin,
            r#"{"clock_format":"12h","push_notify_scope":"everyone","tool_rounds":"40"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
        assert_eq!(refused["code"], "settings_value_invalid");
        let after = read_settings(&fx.service).expect("the snapshot");
        assert_eq!(
            after["clock_format"], before["clock_format"],
            "nothing was written"
        );
        assert_eq!(
            after["tool_rounds"], before["tool_rounds"],
            "nothing was written"
        );
        let rows: i64 = fx
            .service
            .engine()
            .db()
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM audit_log WHERE action = 'settings.device_update'",
                    [],
                    |row| row.get(0),
                )
            })
            .expect("the audit count");
        assert_eq!(rows, 0, "a refused write appends no audit row");
    }

    #[tokio::test]
    async fn an_unknown_field_is_refused_by_name_and_lists_the_accepted_ones() {
        let fx = fixture();
        let (status, refused) = send(
            &fx.service,
            "POST",
            "/v1/settings",
            &fx.admin,
            r#"{"sandbox":"read-only","permissions.sandbox_mode":"full"}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{refused}");
        assert_eq!(refused["code"], "settings_unknown_field");
        let message = refused["message"].as_str().expect("a message");
        assert!(
            message.contains("permissions.sandbox_mode"),
            "the refusal names the field: {message}"
        );
        assert!(
            message.contains("clock_format") && message.contains("conversation_sort"),
            "the refusal lists what it accepts: {message}"
        );
        // The read-only field is not offered here: it is listed by the record and
        // refused by its own code, which is a different answer from "unknown".
        assert!(
            !message.contains("workspace_path"),
            "the accepted list is the writable ones: {message}"
        );
        // The valid field in the same body was not written either.
        let after = read_settings(&fx.service).expect("the snapshot");
        assert_eq!(
            after["sandbox"], "read-only",
            "the fixture default is untouched"
        );
    }

    // -----------------------------------------------------------------------
    // The conversation list
    // -----------------------------------------------------------------------

    /// Four conversations with distinct titles, starts and last-touched stamps.
    fn seeded_conversations(fx: &Fixture) {
        let db = fx.service.engine().db().clone();
        db.with_conn(|conn| {
            for (id, title, created, updated) in [
                (
                    "conv-b",
                    "bravo",
                    "2026-10-01T10:00:00.000Z",
                    "2026-10-26T12:00:00.000Z",
                ),
                (
                    "conv-a",
                    "Alpha",
                    "2026-10-02T10:00:00.000Z",
                    "2026-10-24T12:00:00.000Z",
                ),
                (
                    "conv-d",
                    "delta",
                    "2026-10-03T10:00:00.000Z",
                    "2026-10-26T02:00:00.000Z",
                ),
                (
                    "conv-c",
                    "Charlie",
                    "2026-10-04T10:00:00.000Z",
                    "2026-10-25T12:00:00.000Z",
                ),
            ] {
                conn.execute(
                    "INSERT INTO chat_conversations (id, title, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![id, title, created, updated],
                )?;
            }
            Ok(())
        })
        .expect("the conversations");
    }

    #[tokio::test]
    async fn the_device_list_honours_all_four_sort_modes() {
        let fx = fixture();
        seeded_conversations(&fx);
        let ids = |body: &serde_json::Value| -> Vec<String> {
            body["conversations"]
                .as_array()
                .expect("a list")
                .iter()
                .map(|row| row["id"].as_str().expect("an id").to_owned())
                .collect()
        };

        let (status, body) = send(&fx.service, "GET", "/v1/conversations", &fx.chat, "").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["sort"], "updated", "the default mode is named");
        assert_eq!(ids(&body), vec!["conv-b", "conv-d", "conv-c", "conv-a"]);
        assert!(body["groups"].is_null(), "only the day mode groups");
        assert!(
            body["conversations"][0]["created_at"].is_string(),
            "the row carries when it started: {body}"
        );

        let (status, body) = send(
            &fx.service,
            "GET",
            "/v1/conversations?sort=created",
            &fx.chat,
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["sort"], "created");
        assert_eq!(ids(&body), vec!["conv-c", "conv-d", "conv-a", "conv-b"]);

        let (status, body) = send(
            &fx.service,
            "GET",
            "/v1/conversations?sort=title",
            &fx.chat,
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["sort"], "title");
        assert_eq!(
            ids(&body),
            vec!["conv-a", "conv-b", "conv-c", "conv-d"],
            "alphabetical ignores case"
        );

        // An unknown mode is the default, not a lost archive.
        let (status, body) = send(
            &fx.service,
            "GET",
            "/v1/conversations?sort=sideways",
            &fx.chat,
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["sort"], "updated");
        assert_eq!(ids(&body), vec!["conv-b", "conv-d", "conv-c", "conv-a"]);
    }

    #[tokio::test]
    async fn the_day_mode_groups_on_the_operators_clock() {
        let fx = fixture();
        seeded_conversations(&fx);
        // A fixed offset, so the day boundaries are the test's fact rather than
        // the host's. At -04:00 the 02:00Z stamp on the 26th is the 25th, 22:00.
        fx.service
            .engine()
            .set_setting(crate::zone::SETTING_TIMEZONE, "-04:00")
            .expect("the zone");
        let (status, body) = send(
            &fx.service,
            "GET",
            "/v1/conversations?sort=day",
            &fx.chat,
            "",
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["sort"], "day");
        let groups = body["groups"].as_array().expect("the day groups");
        let keys: Vec<&str> = groups
            .iter()
            .map(|group| group["key"].as_str().expect("a key"))
            .collect();
        assert_eq!(
            keys,
            vec!["2026-10-26", "2026-10-25", "2026-10-24"],
            "days descend, and the 02:00Z stamp is on the previous day at -04:00: {body}"
        );
        // Inside a day the order is `updated`, newest first.
        let first = groups[0]["conversations"].as_array().expect("a day");
        assert_eq!(first.len(), 1);
        assert_eq!(first[0]["id"], "conv-b");
        let second = groups[1]["conversations"].as_array().expect("a day");
        let second_ids: Vec<&str> = second
            .iter()
            .map(|row| row["id"].as_str().expect("an id"))
            .collect();
        assert_eq!(
            second_ids,
            vec!["conv-d", "conv-c"],
            "inside a day the order is `updated`, newest first"
        );
        for group in groups {
            assert!(
                group["label"]
                    .as_str()
                    .is_some_and(|label| !label.is_empty()),
                "every day has a heading: {group}"
            );
        }
        // And the flat list is still there, in the same order as the days.
        let flat: Vec<&str> = body["conversations"]
            .as_array()
            .expect("a list")
            .iter()
            .map(|row| row["id"].as_str().expect("an id"))
            .collect();
        assert_eq!(flat, vec!["conv-b", "conv-d", "conv-c", "conv-a"]);
    }

    #[tokio::test]
    async fn a_doubled_v1_prefix_is_a_typed_404() {
        let fx = fixture();
        let (status, body) = send(&fx.service, "POST", "/v1/v1/pair", "", "{}").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["ok"], serde_json::json!(false));
        assert_eq!(body["code"], serde_json::json!("not_found"));
    }
}
