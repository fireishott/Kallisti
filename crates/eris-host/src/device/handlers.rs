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
        FrameKind::Delta | FrameKind::Reasoning | FrameKind::Tool => serde_json::Value::Null,
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
            let model = store
                .provider_config()
                .ok()
                .flatten()
                .map(|config| config.model)
                .unwrap_or_default();
            let context_window = service
                .engine()
                .primary_provider_contexts()
                .ok()
                .and_then(|contexts| contexts.get(&model).copied());
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
            // The first question names the conversation and the rail bumps it to
            // the top, through the same two helpers the browser calls.
            let _ = chat::name_from_first_message(db, conversation_id, text);
            let _ = chat::touch_conversation(db, conversation_id);
            Some(ConversationTurn {
                conversation: conversation_id.clone(),
                question_id,
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

/// The four fields `POST /v1/settings` accepts, in the order the response lists
/// them. A body that names none of these changes nothing.
pub const SETTINGS_FIELDS: [&str; 4] = ["model", "sandbox", "memory", "approval"];

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
                SETTINGS_FIELDS.join(", ")
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
    Ok(serde_json::json!({
        "ok": true,
        "model": model,
        "sandbox": permissions.sandbox_mode.as_str(),
        "memory": memory.as_str(),
        "tools_enabled": tools_enabled,
        "tool_rounds": tool_rounds,
        "approval": permissions.approval_policy.as_str(),
        "timezone": timezone.as_str(),
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

    Ok(write)
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

/// `POST /v1/push/send` - stub. There is no APNs key on this host, so this is
/// the legacy route's own 503 (`apns_client is None`), behind the `admin` scope.
pub async fn send_push(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    tracing::info!(
        device_id = token.device_id(),
        "push send requested but APNs is not configured"
    );
    DeviceError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "apns_not_configured",
        "APNs not configured. Set APNS_KEY_PATH, APNS_KEY_ID, APNS_TEAM_ID.",
    )
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

        // Send is admin-only, and with admin it is the 503 APNs stub.
        let (status, _) = send(&fx.service, "POST", "/v1/push/send", &fx.chat, "{}").await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = send(&fx.service, "POST", "/v1/push/send", &fx.admin, "{}").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
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

    #[tokio::test]
    async fn a_doubled_v1_prefix_is_a_typed_404() {
        let fx = fixture();
        let (status, body) = send(&fx.service, "POST", "/v1/v1/pair", "", "{}").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["ok"], serde_json::json!(false));
        assert_eq!(body["code"], serde_json::json!("not_found"));
    }
}
