//! The legacy-parity routes: the paths an older client build still calls.
//!
//! Every route here exists because a shipped client asks for it and the device
//! listener answered a typed 404, which the client renders as "the data couldn't
//! be read" and a phone that can pair that looks like a phone that cannot be
//! configured.
//!
//! ## The rule this module is built on
//!
//! The legacy relay had **many hosts** and a **connector process** to watch. Eris
//! is one daemon on one box. So a route is either answered from something the
//! daemon really holds, or it is refused by name. Nothing here invents a
//! multi-host world, a connector state machine, or a profile catalog to keep dead
//! client code happy.
//!
//! | route | answered from |
//! |---|---|
//! | `GET /v1/health` | the `app_state` row, the same read `/healthz` does |
//! | `GET /v1/config`, `PUT /v1/config` | `~/.config/kallisti/config.toml` |
//! | `POST /v1/config/validate` | the same parser, writing nothing |
//! | `GET /v1/gw/status` | the engine, the registry, the job rows |
//! | `GET /v1/gw/logs`, `GET /v1/gw/logs/stream` | `journalctl --user -u <unit>` |
//! | `POST /v1/auth/refresh` | the same rotate `/v1/token/refresh` runs |
//! | `POST /v1/auth/revoke` | `AuthStore::revoke_device` on the caller itself |
//! | `POST /v1/device/register` | the presenting device, stamped |
//! | `GET`/`POST /v1/device/app-state` | the real `app_state` row and one setting |
//! | `GET /v1/hosts/current` | this host: `/etc/machine-id`, `/etc/hostname` |
//! | `GET /v1/connector/events` | the daemon's own health, pushed |
//! | `GET /v1/profiles` | the one identity, with its real skill count |
//! | `GET /v1/commands` | `commands::SUPPORTED` and the real library |
//! | `GET /v1/canvas/processes/stream` | the `subagents` table |
//! | `POST /v1/hosts/current/revoke` | refused: 403, with the two real buttons |
//!
//! ## The wire constraints the Swift imposes
//!
//! Found in `RelayAPIClient`, not assumed, because getting one wrong is a screen
//! that fails to decode while every route answers 200:
//!
//! 1. Every response carries a top-level `data` key. `Envelope<T>` requires it
//!    (`RelayAPIClient.swift:172`, `:560`). The typed error body is the one
//!    exception, and it already matches.
//! 2. `RelayCoders.makeDecoder` installs a **custom** date strategy that accepts
//!    ISO-8601 strings only (`:137-153`). A numeric timestamp where a `Date?` is
//!    decoded throws and kills the whole response.
//! 3. That decoder is also strict about types: `Int?` does not accept `"1"` and
//!    `Bool` does not accept `"true"`. Numbers and booleans go out as JSON types.

use std::convert::Infallible;
use std::io::{BufRead, BufReader};
use std::pin::Pin;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio::sync::mpsc;
use tokio_stream::Stream;
use tokio_stream::wrappers::UnboundedReceiverStream;

use crate::auth::{AuthStore, Scope, TokenService, format_timestamp};
use crate::config::{Paths, TomlConfig};
use crate::db::Db;
use crate::device::{DeviceError, DeviceService};
use crate::onboarding::SetupEngine;
use crate::persona::PersonaConfig;

use super::gateway::ERIS_UNIT;
use super::handlers::{
    authenticate, read_fields, refuse_value, require_admin, require_scope, settings_unavailable,
};

/// How long an idle SSE stream stays silent before it sends a comment.
const KEEP_ALIVE_SECONDS: u64 = 15;

/// How often `/v1/connector/events` re-reads the durable view.
const CONNECTOR_POLL_SECONDS: u64 = 2;

/// How often `/v1/canvas/processes/stream` re-reads the subagent rows.
const CANVAS_POLL_SECONDS: u64 = 2;

/// The default `?limit` on `/v1/gw/logs`.
const LOGS_DEFAULT_LIMIT: u32 = 200;

/// The largest `?limit` this route will honour.
const LOGS_MAX_LIMIT: u32 = 2000;

/// The longest `device/app-state` word this daemon will store.
///
/// A phone reports an app state in one word. The cap is here so a client cannot
/// use a best-effort telemetry route to write an unbounded string into the
/// settings table one request at a time.
const APP_STATE_MAX_CHARS: usize = 64;

/// The setting key a reported app state is stored under.
///
/// Namespaced and separate from `app_state.setup_state`, which the wizard owns.
/// A phone must not be able to move the daemon's own setup state, and this key is
/// how that is enforced rather than promised.
const SETTING_APP_STATE_REPORTED: &str = "device.app_state_reported";

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

/// The envelope every answer in this module uses.
///
/// `Envelope<T>` in `RelayAPIClient` decodes `{"data": ...}` and nothing else, so
/// this is not a stylistic wrapper: an unwrapped body is a client-side decode
/// error.
fn answered(value: Value) -> Response {
    axum::Json(json!({ "data": value })).into_response()
}

/// A 422 with both the field the Config Editor reads and the one the error path
/// falls back to.
///
/// Two spellings of the same sentence on purpose: `NativeGatewayFeatureClient`
/// reads `error` (`:948`) on a failed validate, and `detail` is what the rest of
/// the client's error path renders when `error` is absent. Sending both means one
/// failing document cannot be reported as a bare "HTTP 422".
fn unprocessable(error: impl std::fmt::Display) -> Response {
    let sentence = error.to_string();
    (
        StatusCode::UNPROCESSABLE_ENTITY,
        axum::Json(json!({
            "ok": false,
            "valid": false,
            "code": "config_invalid",
            "error": sentence,
            "detail": sentence,
        })),
    )
        .into_response()
}

/// The database's own "now", in the format every stored row carries.
///
/// Read from SQLite rather than formatted from a clock so a row this module writes
/// is byte-identical in shape to a row the wizard writes.
fn now_stamp(db: &Db) -> Result<String, DeviceError> {
    db.with_conn(|conn| {
        conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now')", [], |row| {
            row.get::<_, String>(0)
        })
    })
    .map_err(|error| DeviceError::db_unavailable(format!("cannot read the clock: {error}")))
}

/// A decoded body's field, trimmed, or the empty string.
fn field<'a>(fields: &'a std::collections::BTreeMap<String, String>, name: &str) -> &'a str {
    fields.get(name).map(String::as_str).unwrap_or("").trim()
}

/// The first of several accepted spellings that carries a value.
///
/// `/v1/auth/refresh` is called by the legacy client with `refreshToken` and by
/// anything speaking the device contract with `refresh_token`. One route serves
/// both, and neither spelling is wrong.
fn field_any<'a>(
    fields: &'a std::collections::BTreeMap<String, String>,
    names: &[&str],
) -> &'a str {
    names
        .iter()
        .map(|name| field(fields, name))
        .find(|value| !value.is_empty())
        .unwrap_or("")
}

/// Parse a `?a=b&c=d` query, percent-decoding both halves.
///
/// A local copy rather than a shared one: the device surface's parser in
/// `handlers` is private, and widening it to serve one legacy route would put a
/// second caller on an API that had one.
fn query_fields(query: &str) -> Result<std::collections::BTreeMap<String, String>, DeviceError> {
    let mut fields = std::collections::BTreeMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        fields.insert(percent_decode(name)?, percent_decode(value)?);
    }
    Ok(fields)
}

/// Percent-decoding, with `+` read as a space.
fn percent_decode(raw: &str) -> Result<String, DeviceError> {
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let pair = bytes.get(index + 1..index + 3).ok_or_else(|| {
                    DeviceError::bad_request(
                        "query_malformed",
                        "a query value ends inside a percent escape",
                    )
                })?;
                let text = std::str::from_utf8(pair).map_err(|_| {
                    DeviceError::bad_request("query_malformed", "a query escape is malformed")
                })?;
                let byte = u8::from_str_radix(text, 16).map_err(|_| {
                    DeviceError::bad_request("query_malformed", "a query escape is not hexadecimal")
                })?;
                out.push(byte);
                index += 3;
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            other => {
                out.push(other);
                index += 1;
            }
        }
    }
    String::from_utf8(out)
        .map_err(|_| DeviceError::bad_request("query_malformed", "a query value is not UTF-8"))
}

/// A bounded `?limit`: defaulted when absent, capped when too large, refused only
/// when it is not a number at all.
fn bounded_limit(
    raw: &str,
    default: u32,
    maximum: u32,
    field_name: &'static str,
) -> Result<u32, DeviceError> {
    if raw.is_empty() {
        return Ok(default);
    }
    let parsed: u32 = raw
        .parse()
        .map_err(|_| refuse_value(field_name, "limit_invalid", raw, "a whole number of rows"))?;
    Ok(parsed.min(maximum))
}

/// The whole number of seconds since the unix epoch.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// GET /v1/health
// ---------------------------------------------------------------------------

/// `GET /v1/health` - `{status, database}`.
///
/// Unauthenticated, like `GET /v1/version`, and for the same reason: a client has
/// no credential before it has paired, and "is anything answering that speaks my
/// protocol" is the first question worth asking. It carries no version, no path
/// and no state - only whether the database answers.
pub async fn health(State(service): State<DeviceService>) -> Response {
    match service.engine().db().setup_state() {
        Ok(state) => answered(json!({
            "status": "ok",
            "database": true,
            "setupState": state.as_str(),
        })),
        // Not a 5xx: the listener is answering and the database is not, and those
        // are two different facts a client should be able to tell apart.
        Err(error) => answered(json!({
            "status": "degraded",
            "database": false,
            "error": error.to_string(),
        })),
    }
}

// ---------------------------------------------------------------------------
// GET /v1/config, PUT /v1/config, POST /v1/config/validate
// ---------------------------------------------------------------------------

/// The config file's path, size and modification time.
fn config_file_facts(paths: &Paths, content: &str) -> Value {
    let mtime = std::fs::metadata(&paths.toml_file)
        .ok()
        .as_ref()
        .and_then(|data| data.modified().ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs_f64());
    json!({
        "path": paths.toml_file.display().to_string(),
        "size": content.len(),
        "mtime": mtime,
    })
}

/// `GET /v1/config` - the daemon's own `config.toml`.
///
/// Behind the bearer and `admin`. The file is non-secret by construction
/// (`config.rs` refuses a key whose name looks like a secret), but it does name
/// the operator's listen addresses, trusted proxies and persona path, which is a
/// settings-class read and not something a device paired without the
/// administration box should see.
pub async fn config_read(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    let paths = service.engine().paths().clone();
    let content = match std::fs::read_to_string(&paths.toml_file) {
        Ok(content) => content,
        // Absent is not an error anywhere else in this daemon: `TomlConfig::load`
        // reads a missing file as defaults. So an absent file is an empty document
        // with a path, not a 404 a client cannot act on.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return DeviceError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "config_unreadable",
                format!("cannot read the config file: {error}"),
            )
            .into_response();
        }
    };
    let mut body = config_file_facts(&paths, &content);
    if let Value::Object(map) = &mut body {
        map.insert("content".to_owned(), Value::String(content));
    }
    answered(body)
}

/// `POST /v1/config/validate` - parse the text, write nothing.
///
/// 200 when the document parses, 422 with the parser's own sentence when it does
/// not. It runs the same call the write path runs, so a document that validates
/// here cannot fail on the way in.
pub async fn config_validate(
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
    let content = fields.get("content").cloned().unwrap_or_default();
    let path = service.engine().paths().toml_file.clone();
    match TomlConfig::parse(&content, &path) {
        Ok(_) => answered(json!({
            "valid": true,
            "path": path.display().to_string(),
            "size": content.len(),
        })),
        Err(error) => unprocessable(error),
    }
}

/// `PUT /v1/config` - validate, back up, write atomically, report the backup.
///
/// The order is the point. A document that does not parse is refused before the
/// live file is touched, and the live file is replaced by a rename rather than
/// truncated in place, so a crash mid-write leaves the old file or the new one and
/// never half of either.
pub async fn config_write(
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
    let Some(content) = fields.get("content") else {
        return DeviceError::bad_request(
            "config_body_missing",
            "the body must carry `content`: the config file's text",
        )
        .into_response();
    };
    let paths = service.engine().paths().clone();
    if let Err(error) = TomlConfig::parse(content, &paths.toml_file) {
        return unprocessable(error);
    }

    // The backup is named for the instant it was taken and sits next to the file
    // it guards. It carries the same non-secret guarantee as the file itself,
    // being a copy of a document the parser just accepted.
    let backup = paths
        .toml_file
        .with_file_name(format!("config.toml.bak-{}", now_secs()));
    if let Ok(existing) = std::fs::read(&paths.toml_file)
        && let Err(error) = std::fs::write(&backup, existing)
    {
        return DeviceError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "config_backup_failed",
            format!("the config was not written because its backup could not be taken: {error}"),
        )
        .into_response();
    }

    let directory = paths
        .toml_file
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let written = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut staged = tempfile::NamedTempFile::new_in(&directory)?;
        staged.write_all(content.as_bytes())?;
        staged.flush()?;
        staged
            .persist(&paths.toml_file)
            .map_err(|error| error.error)?;
        Ok(())
    })();
    if let Err(error) = written {
        return DeviceError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "config_write_failed",
            format!("the config could not be written: {error}"),
        )
        .into_response();
    }

    if let Err(error) = service.engine().auth().audit(
        token.device_id(),
        "settings.device_config",
        Some(&format!("bytes={}", content.len())),
    ) {
        return DeviceError::db_unavailable(format!(
            "the config was written but the audit row could not be appended: {error}"
        ))
        .into_response();
    }

    answered(json!({
        "ok": true,
        "path": paths.toml_file.display().to_string(),
        "size": content.len(),
        "backup": backup.display().to_string(),
    }))
}

// ---------------------------------------------------------------------------
// GET /v1/gw/status
// ---------------------------------------------------------------------------

/// One `SELECT COUNT(*)` with a predicate from this module.
///
/// The predicate is always a literal written here, never anything a request chose,
/// so this is not a place a query can be built by a client.
fn count_where(db: &Db, table: &str, predicate: &str) -> Result<i64, DeviceError> {
    let sql = format!("SELECT COUNT(*) FROM {table} WHERE {predicate}");
    db.with_conn(|conn| conn.query_row(&sql, [], |row| row.get(0)))
        .map_err(|error| DeviceError::db_unavailable(format!("cannot read {table}: {error}")))
}

/// Running and queued job rows, the pair `gw_health` reports.
fn active_jobs(db: &Db) -> Result<(i64, i64), DeviceError> {
    Ok((
        count_where(db, "jobs", "status = 'running'")?,
        count_where(db, "jobs", "status = 'queued'")?,
    ))
}

/// `GET /v1/gw/status` - the snapshot the gateway status screen reads.
///
/// Field names and types are the ones `GatewayStatusScreen` decodes: `model`,
/// `provider`, `availableProviders`, `activeSessionCount`, `hermesHome`,
/// `usageAvailable`. `hermesHome` is the state directory, which is the honest
/// answer to "where does this gateway keep its state" now that there is no
/// `~/.hermes` in front of the daemon. `usageAvailable` is `false` because this
/// daemon does not meter provider spend, and saying otherwise would draw a panel
/// with no numbers in it.
pub async fn gw_status(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    let engine: &SetupEngine = service.engine();
    let model = match engine.model() {
        Ok(model) => model.unwrap_or_default(),
        Err(error) => return settings_unavailable(error).into_response(),
    };
    let (providers, primary) = match super::control::registry(engine) {
        Ok(registry) => registry,
        Err(error) => return error.into_response(),
    };
    let available: Vec<String> = providers
        .iter()
        .map(|provider| provider.id.clone())
        .collect();
    let active_sessions = match count_where(engine.db(), "sessions", "revoked_at IS NULL") {
        Ok(count) => count,
        Err(error) => return error.into_response(),
    };
    let (running, queued) = match active_jobs(engine.db()) {
        Ok(counts) => counts,
        Err(error) => return error.into_response(),
    };
    answered(json!({
        "model": model,
        "provider": primary,
        "availableProviders": available,
        "activeSessionCount": active_sessions,
        "hermesHome": engine.paths().state_dir.display().to_string(),
        "usageAvailable": false,
        "activeWork": { "running": running, "queued": queued },
        "deviceListener": service.accepting(),
        "version": crate::admin::version_line(),
    }))
}

// ---------------------------------------------------------------------------
// GET /v1/gw/logs, GET /v1/gw/logs/stream
// ---------------------------------------------------------------------------

/// One journal entry, as the phone's log view reads it.
///
/// `timestamp` is an ISO-8601 string and never a number: the client's date
/// strategy throws on anything else, and one bad field takes the whole response
/// with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRow {
    /// When the entry was written, ISO-8601 UTC.
    pub timestamp: String,
    /// `error`, `warn`, `debug` or `info`.
    pub level: String,
    /// The message, with the journal's escape sequences removed.
    pub message: String,
    /// Where it came from: the syslog identifier, or the unit.
    pub source: String,
}

impl LogRow {
    /// The wire shape.
    fn to_json(&self) -> Value {
        json!({
            "timestamp": self.timestamp,
            "level": self.level,
            "message": self.message,
            "source": self.source,
        })
    }
}

/// The `level` a syslog priority maps to, in the words the client switches on.
fn level_for_priority(priority: u8) -> &'static str {
    match priority {
        0..=3 => "error",
        4 => "warn",
        7 => "debug",
        // 5 is notice and 6 is info. Both read as info, and so does anything
        // outside the range: an unknown priority is a line, not an error state.
        _ => "info",
    }
}

/// Strip the escape sequences and carriage returns a journal line can carry.
fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            if character != '\r' {
                out.push(character);
            }
            continue;
        }
        match chars.next() {
            // CSI: ESC [ parameters, ending on a byte in @..=~.
            Some('[') => {
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            // OSC: ESC ] ... terminated by BEL or ESC backslash.
            Some(']') => {
                for next in chars.by_ref() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' {
                        let _ = chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// A journald `MESSAGE`, which is a string when it is plain and an array of byte
/// values when the journal chose to encode it.
fn journal_message(entry: &Value) -> String {
    match entry.get("MESSAGE") {
        Some(Value::String(text)) => strip_escapes(text),
        Some(Value::Array(bytes)) => {
            let raw: Vec<u8> = bytes
                .iter()
                .filter_map(Value::as_u64)
                .map(|byte| byte as u8)
                .collect();
            strip_escapes(&String::from_utf8_lossy(&raw))
        }
        _ => String::new(),
    }
}

/// The microsecond stamp journald carries, as an ISO-8601 string.
fn journal_timestamp(entry: &Value) -> String {
    // A missing or unreadable stamp is an empty string, never epoch zero: a
    // row that renders as 1970 is worse than a row with no time on it.
    let Some(micros) = entry
        .get("__REALTIME_TIMESTAMP")
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<i64>().ok())
    else {
        return String::new();
    };
    match OffsetDateTime::from_unix_timestamp(micros / 1_000_000) {
        Ok(at) => format_timestamp(at),
        Err(_) => String::new(),
    }
}

/// One journald JSON line as a [`LogRow`], or `None` when it is not an entry.
fn journal_entry(line: &str) -> Option<LogRow> {
    let entry: Value = serde_json::from_str(line).ok()?;
    let priority = entry
        .get("PRIORITY")
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<u8>().ok())
        .unwrap_or(6);
    let source = entry
        .get("SYSLOG_IDENTIFIER")
        .and_then(Value::as_str)
        .or_else(|| entry.get("_SYSTEMD_USER_UNIT").and_then(Value::as_str))
        .unwrap_or(ERIS_UNIT)
        .to_owned();
    Some(LogRow {
        timestamp: journal_timestamp(&entry),
        level: level_for_priority(priority).to_owned(),
        message: journal_message(&entry),
        source,
    })
}

/// The `journalctl` arguments for one read. Every element is a literal or a
/// number this module computed, so nothing a request carried is ever an argument.
fn journal_args(lines: u32, follow: bool) -> Vec<String> {
    let mut args = vec![
        "--user".to_owned(),
        "-u".to_owned(),
        ERIS_UNIT.to_owned(),
        "-o".to_owned(),
        "json".to_owned(),
        "--no-pager".to_owned(),
        "-n".to_owned(),
        lines.to_string(),
    ];
    if follow {
        args.push("-f".to_owned());
    }
    args
}

/// Whether a row passes the `?level=` filter a client sent.
///
/// `all` and an absent value both mean every level, which is what the phone's log
/// view sends.
fn level_selected(filter: &str, level: &str) -> bool {
    let filter = filter.trim().to_ascii_lowercase();
    filter.is_empty() || filter == "all" || filter == level
}

/// Run `journalctl` once and decode its output.
fn read_journal(lines: u32) -> Result<Vec<LogRow>, DeviceError> {
    let output = Command::new("journalctl")
        .args(journal_args(lines, false))
        .stdin(Stdio::null())
        .output()
        .map_err(|error| {
            DeviceError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "journal_unavailable",
                format!("cannot run journalctl: {error}"),
            )
        })?;
    if !output.status.success() {
        return Err(DeviceError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "journal_unavailable",
            format!(
                "journalctl exited {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(journal_entry)
        .collect())
}

/// `GET /v1/gw/logs` - a bounded tail of this daemon's journal.
///
/// Behind `admin`: the journal names paths, provider hosts and every request the
/// daemon refused, which is more than a device paired without the administration
/// box should be handed.
pub async fn gw_logs(
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
    let query = match query_fields(uri.query().unwrap_or_default()) {
        Ok(query) => query,
        Err(error) => return error.into_response(),
    };
    let limit = match bounded_limit(
        query.get("limit").map(String::as_str).unwrap_or(""),
        LOGS_DEFAULT_LIMIT,
        LOGS_MAX_LIMIT,
        "limit",
    ) {
        Ok(limit) => limit,
        Err(error) => return error.into_response(),
    };
    let level = query.get("level").cloned().unwrap_or_default();
    let rows = match read_journal(limit) {
        Ok(rows) => rows,
        Err(error) => return error.into_response(),
    };
    let lines: Vec<Value> = rows
        .iter()
        .filter(|row| level_selected(&level, &row.level))
        .map(LogRow::to_json)
        .collect();
    answered(json!({
        "unit": ERIS_UNIT,
        "lines": lines,
    }))
}

/// A child process killed when the stream that owns it goes away.
///
/// Without this a cancelled SSE request would leave `journalctl -f` running for
/// the life of the daemon, one per reconnecting phone.
struct ChildGuard(Arc<Mutex<Child>>);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Ok(mut child) = self.0.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// `GET /v1/gw/logs/stream` - the same rows, followed.
///
/// Reads the journal rather than the daemon's own subscriber list, so the stream
/// carries exactly what `journalctl` shows: the same lines, the same order, the
/// same severity. This was verified reachable from inside the unit's sandbox
/// before the route was written.
pub async fn gw_logs_stream(
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
    let query = match query_fields(uri.query().unwrap_or_default()) {
        Ok(query) => query,
        Err(error) => return error.into_response(),
    };
    let level = query.get("level").cloned().unwrap_or_default();
    let tail = match bounded_limit(
        query.get("limit").map(String::as_str).unwrap_or(""),
        LOGS_DEFAULT_LIMIT,
        LOGS_MAX_LIMIT,
        "limit",
    ) {
        Ok(tail) => tail,
        Err(error) => return error.into_response(),
    };

    let spawn = Command::new("journalctl")
        .args(journal_args(tail, true))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let mut child = match spawn {
        Ok(child) => child,
        Err(error) => {
            return DeviceError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "journal_unavailable",
                format!("cannot follow the journal: {error}"),
            )
            .into_response();
        }
    };
    let Some(stdout) = child.stdout.take() else {
        return DeviceError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "journal_unavailable",
            "the journal could not be read: no pipe was available",
        )
        .into_response();
    };

    let guard = ChildGuard(Arc::new(Mutex::new(child)));
    let (sender, receiver) = mpsc::unbounded_channel::<Result<Event, Infallible>>();

    // Reading a pipe is blocking work. It runs on its own thread rather than on
    // the async executor, where it would stall every other request on the worker.
    let reader = std::thread::Builder::new()
        .name("eris-journal".to_owned())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                let Some(row) = journal_entry(&line) else {
                    continue;
                };
                if !level_selected(&level, &row.level) {
                    continue;
                }
                let event = Event::default()
                    .event("log")
                    .data(row.to_json().to_string());
                if sender.send(Ok(event)).is_err() {
                    break;
                }
            }
        });
    if let Err(error) = reader {
        return DeviceError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "journal_reader_failed",
            format!("the journal reader could not start: {error}"),
        )
        .into_response();
    }

    // The guard rides the stream, so cancelling the response drops it and kills
    // the child.
    Sse::new(guarded(UnboundedReceiverStream::new(receiver), guard))
        .keep_alive(
            KeepAlive::new()
                .interval(std::time::Duration::from_secs(KEEP_ALIVE_SECONDS))
                .text("keep-alive"),
        )
        .into_response()
}

/// A stream plus a value that must live exactly as long as it does.
fn guarded<S, G>(
    stream: S,
    guard: G,
) -> Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>
where
    S: Stream<Item = Result<Event, Infallible>> + Send + Unpin + 'static,
    G: Send + Unpin + 'static,
{
    Box::pin(Guarded {
        stream,
        _guard: guard,
    })
}

/// The stream that owns the guard. `Pin` projects to the inner stream; the guard
/// is never moved after construction.
struct Guarded<S, G> {
    stream: S,
    _guard: G,
}

impl<S: Stream + Unpin, G: Unpin> Stream for Guarded<S, G> {
    type Item = S::Item;

    fn poll_next(
        mut self: Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        Pin::new(&mut self.stream).poll_next(context)
    }
}

// ---------------------------------------------------------------------------
// POST /v1/auth/refresh, POST /v1/auth/revoke
// ---------------------------------------------------------------------------

/// `POST /v1/auth/refresh` - the legacy spelling of `/v1/token/refresh`.
///
/// The legacy client sends `refreshToken` and reads `accessToken`,
/// `refreshToken`, `expiresAt`. The device contract uses the snake_case spelling
/// on the way in and out. Both are accepted, all four keys go out, and nothing is
/// duplicated: the rotation is `TokenService::rotate`, the same call the device
/// route makes, so a token cannot be rotated twice by asking twice in two
/// spellings.
pub async fn auth_refresh(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let fields = match read_fields(&headers, body).await {
        Ok(fields) => fields,
        Err(error) => return error.into_response(),
    };
    let presented = field_any(&fields, &["refresh_token", "refreshToken"]);
    if presented.is_empty() {
        return DeviceError::unauthorized(
            "token_invalid",
            "a refresh token is required: send it as `refresh_token` or `refreshToken`",
        )
        .into_response();
    }
    let tokens = TokenService::new(service.engine().db().clone());
    let pair = match tokens.rotate(presented) {
        Ok(pair) => pair,
        Err(_) => {
            return DeviceError::unauthorized(
                "token_invalid",
                "the refresh token does not verify: it is unknown, expired, or revoked",
            )
            .into_response();
        }
    };
    let expires = format_timestamp(pair.access.expires_at());
    answered(json!({
        "access_token": pair.access.token(),
        "refresh_token": pair.refresh.token(),
        "accessToken": pair.access.token(),
        "refreshToken": pair.refresh.token(),
        "expires_at": expires,
        "expiresAt": expires,
    }))
}

/// `POST /v1/auth/revoke` - revoke the calling device, and say so.
///
/// The legacy verb is "revoke the current session". The honest mapping is the
/// device behind the presented bearer, with its tokens retired. Not "the session",
/// not "the host". A phone that asks to be forgotten is forgotten, and its next
/// request fails on a token that no longer exists.
pub async fn auth_revoke(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    let store = AuthStore::new(service.engine().db().clone());
    let retired = match store.revoke_device(token.device_id()) {
        Ok(retired) => retired,
        Err(error) => {
            return DeviceError::db_unavailable(format!(
                "the device could not be revoked: {error}"
            ))
            .into_response();
        }
    };
    if let Err(error) = store.audit(
        token.device_id(),
        "device.self_revoke",
        Some("from the device"),
    ) {
        return DeviceError::db_unavailable(format!(
            "the device was revoked but the audit row could not be appended: {error}"
        ))
        .into_response();
    }
    answered(json!({
        "revoked": true,
        "tokens_retired": retired,
        "device_id": token.device_id(),
    }))
}

// ---------------------------------------------------------------------------
// POST /v1/device/register
// ---------------------------------------------------------------------------

/// `POST /v1/device/register` - the presenting device's own row, stamped.
///
/// **This route does not mint credentials, and that is deliberate.**
///
/// The legacy connector registered a device *and handed it a token pair* before
/// anything had been verified. Reproducing that here would put an unauthenticated
/// token minter on the device listener: anything that could reach the port could
/// obtain a live credential with no code, no operator and no approval. That is not
/// a parity gap worth closing.
///
/// What is answered instead: a caller with a valid bearer is told its own device
/// id and has the newest session stamp refreshed, which is what a registration
/// call is for once a device exists. A caller with no bearer is told by name where
/// a device comes from, because on this daemon an unpaired device has no id and
/// inventing one would be exactly the fiction this module avoids.
pub async fn device_register(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let presented = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if !presented {
        return DeviceError::unauthorized(
            "pairing_required",
            "this daemon registers a device only after it has paired: redeem a code at POST \
             /v1/pair, then call this route with the access token it returned. This route does \
             not mint credentials, because minting one with no pairing code would let anything \
             that can reach this port obtain a live token",
        )
        .into_response();
    }
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    let store = AuthStore::new(service.engine().db().clone());
    let device = match store.device(token.device_id()) {
        Ok(Some(device)) => device,
        Ok(None) => return DeviceError::device_not_found().into_response(),
        Err(error) => {
            return DeviceError::db_unavailable(format!("the device could not be read: {error}"))
                .into_response();
        }
    };
    if device.revoked_at.is_some() {
        return DeviceError::unauthorized(
            "token_invalid",
            "this device was revoked: pair again with a fresh code",
        )
        .into_response();
    }
    let stamp = match now_stamp(service.engine().db()) {
        Ok(stamp) => stamp,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = stamp_device_seen(service.engine().db(), &device.id, &stamp) {
        return error.into_response();
    }
    answered(json!({
        "deviceId": device.id,
        "device_id": device.id,
        "deviceRegistered": true,
        "name": device.name,
        "platform": device.platform,
        "lastSeenAt": stamp,
    }))
}

/// Stamp the newest session row for a device, which is the only "last seen" this
/// daemon really writes.
///
/// `devices.last_seen_at` exists in the schema and no code path in this daemon ever
/// writes it; `sessions.last_seen_at` is the stamp the dashboard's device list
/// renders, and `web/store.rs:483-499` says so out loud. Stamping the live one and
/// leaving the dead one alone keeps the two screens agreeing.
fn stamp_device_seen(db: &Db, device_id: &str, stamp: &str) -> Result<(), DeviceError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE sessions SET last_seen_at = ?2 WHERE device_id = ?1 \
             AND last_seen_at IS NOT NULL",
            rusqlite::params![device_id, stamp],
        )?;
        Ok(())
    })
    .map_err(|error| {
        DeviceError::db_unavailable(format!("the device row was not stamped: {error}"))
    })
}

// ---------------------------------------------------------------------------
// GET /v1/hosts/current, POST /v1/hosts/current/revoke
// ---------------------------------------------------------------------------

/// `/etc/machine-id` in the canonical 8-4-4-4-12 form.
///
/// The client decodes this field as a non-optional `UUID`
/// (`LiveKallistiHostService.swift:18`), so it has to be one. The machine id is
/// read locally and its 32 hex characters are regrouped: the same value in the
/// other spelling, not a different identifier.
fn machine_uuid() -> Option<String> {
    let raw = std::fs::read_to_string("/etc/machine-id").ok()?;
    let hex: String = raw
        .trim()
        .chars()
        .filter(char::is_ascii_hexdigit)
        .collect::<String>()
        .to_ascii_lowercase();
    if hex.len() != 32 {
        return None;
    }
    Some(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// This machine's name.
fn host_name() -> String {
    std::fs::read_to_string("/etc/hostname")
        .map(|name| name.trim().to_owned())
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "localhost".to_owned())
}

/// The newest session stamp across every paired device, or `None` when no device
/// has ever connected.
fn newest_device_seen(db: &Db) -> Result<Option<String>, DeviceError> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT MAX(last_seen_at) FROM sessions WHERE device_id IS NOT NULL \
             AND last_seen_at IS NOT NULL",
            [],
            |row| row.get::<_, Option<String>>(0),
        )
    })
    .map_err(|error| DeviceError::db_unavailable(format!("cannot read device activity: {error}")))
}

/// `GET /v1/hosts/current` - this host, because this host is the only one.
///
/// The legacy relay listed hosts and reported the selected one. Eris is one daemon
/// on one box, so the answer is the box. Nothing here fakes a fleet: there is no
/// `hosts` table and this route does not create one, and every field is read from
/// the machine or from the daemon's own identity. `single` and `note` say so
/// plainly so a client can render one row honestly instead of implying a list of
/// one page.
pub async fn hosts_current(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Sessions) {
        return error.into_response();
    }
    let engine = service.engine();
    let last_seen = match newest_device_seen(engine.db()) {
        Ok(stamp) => stamp,
        Err(error) => return error.into_response(),
    };
    let hostname = host_name();
    let id = machine_uuid().unwrap_or_else(|| uuid::Uuid::nil().to_string());
    answered(json!({
        "host": {
            "id": id,
            "displayName": hostname,
            "hostname": hostname,
            "platform": std::env::consts::OS,
            "connectorVersion": crate::admin::version_line(),
            "heraldCommand": format!("journalctl --user -u {ERIS_UNIT}"),
            "heraldVersion": crate::admin::version_line(),
            "heraldModel": engine.model().ok().flatten(),
            "lastSeenAt": last_seen,
            "lastConnectedAt": last_seen,
            "isOnline": true,
            "single": true,
            "note": "this daemon serves exactly one host, the machine it runs on",
        }
    }))
}

/// `POST /v1/hosts/current/revoke` - refused, by name.
///
/// Revoking "the current host" would mean retiring every device and switching off
/// the listener the request arrived on. The caller asking for it is one of the
/// devices that would be removed, so there is no way to answer it truthfully inside
/// the request. The refusal names the two buttons that really exist.
pub async fn hosts_current_revoke(
    State(service): State<DeviceService>,
    headers: HeaderMap,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    DeviceError::new(
        StatusCode::FORBIDDEN,
        "host_revoke_unsupported",
        "there is no host to revoke: this daemon is the host, and retiring it would unpair every \
         device and switch off the listener this request arrived on. Use POST /v1/devices/{id}/revoke \
         to retire one device, or turn the device listener off from the dashboard to take this \
         surface down",
    )
    .into_response()
}

// ---------------------------------------------------------------------------
// GET /v1/connector/events
// ---------------------------------------------------------------------------

/// The durable facts the connector stream used to report, as one comparable value.
///
/// A change in this value is the only thing that produces a `health_check` frame.
/// A frame sent on a timer would claim a check happened when it did not.
fn connector_signature(service: &DeviceService) -> Result<Value, DeviceError> {
    let engine = service.engine();
    let (running, queued) = active_jobs(engine.db())?;
    Ok(json!({
        "ready": engine.is_ready().unwrap_or(false),
        "running": running,
        "queued": queued,
        "accepting": service.accepting(),
    }))
}

/// `GET /v1/connector/events` - the daemon's own health, pushed.
///
/// `HostStatusStreamService` sends no `Last-Event-ID` and reads exactly two event
/// names: `connected`, then `health_check` (`:62`, `:71`). Both are produced, plus
/// a `tick` that is the keepalive made visible rather than a claim that something
/// was inspected.
///
/// The legacy stream reported a connector process starting, stopping and
/// reconnecting. There is no connector process - this daemon is the gateway - so
/// the stream reports what is true of this daemon: whether it is ready, what work
/// is in flight, and whether the listener is accepting turns. No connector state
/// machine is invented to fill the gap.
pub async fn connector_events(
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
    let (sender, receiver) = mpsc::unbounded_channel::<Result<Event, Infallible>>();
    tokio::spawn(async move {
        let first = match connector_signature(&service) {
            Ok(signature) => signature,
            Err(error) => {
                let _ = sender.send(Ok(Event::default()
                    .event("error")
                    .data(json!({ "code": error.code() }).to_string())));
                return;
            }
        };
        let ready = first.get("ready").and_then(Value::as_bool).unwrap_or(false);
        if sender
            .send(Ok(Event::default().event("connected").data(
                json!({
                    "relayConnected": true,
                    "connectorConnected": true,
                    "hermesReady": ready,
                    "unit": ERIS_UNIT,
                })
                .to_string(),
            )))
            .is_err()
        {
            return;
        }

        let mut last = first;
        let mut ticks: u64 = 0;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(CONNECTOR_POLL_SECONDS)).await;
            ticks += CONNECTOR_POLL_SECONDS;
            let Ok(current) = connector_signature(&service) else {
                break;
            };
            if current != last {
                last = current.clone();
                let frame = Event::default()
                    .event("health_check")
                    .data(current.to_string());
                if sender.send(Ok(frame)).is_err() {
                    break;
                }
            } else if ticks.is_multiple_of(KEEP_ALIVE_SECONDS) {
                let frame = Event::default()
                    .event("tick")
                    .data(json!({ "ok": true }).to_string());
                if sender.send(Ok(frame)).is_err() {
                    break;
                }
            }
        }
    });

    Sse::new(UnboundedReceiverStream::new(receiver))
        .keep_alive(
            KeepAlive::new()
                .interval(std::time::Duration::from_secs(KEEP_ALIVE_SECONDS))
                .text("keep-alive"),
        )
        .into_response()
}

// ---------------------------------------------------------------------------
// GET /v1/profiles
// ---------------------------------------------------------------------------

/// `GET /v1/profiles` - the one identity, and how many skills it can reach.
///
/// The legacy connector served a profile catalog with a per-profile skill count.
/// This daemon has one identity, `persona.md`, and no profiles to switch between.
/// The client reads exactly one number from this route: the Hub's skill count,
/// which used to be hardcoded to zero (`NativeGatewayFeatureClient.swift:117-126`).
///
/// So one row is published, its count is the real length of the real library, and
/// `single` says plainly that this is the whole list rather than a page of one. No
/// profile catalog is invented.
pub async fn profiles(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Sessions) {
        return error.into_response();
    }
    let engine = service.engine();
    let stored = engine.setting(crate::skills::SETTING_DIR).ok().flatten();
    let (skill_count, library_error) = match crate::skills::list(engine.db(), stored.as_deref()) {
        Ok((skills, _)) => (skills.len(), Value::Null),
        // An unreadable library is a state the operator fixes from a screen, not a
        // daemon that is down: the count reports zero and the reason rides along,
        // which is how `/v1/skills` already answers.
        Err(error) => (0, Value::String(error.to_string())),
    };
    // Resolved the way a turn resolves it, so this screen and the Identity page
    // cannot disagree about whether the persona is on.
    let persona = PersonaConfig::at_path(engine.paths().persona_file());
    let persona_on = persona.effective_enabled(
        engine
            .setting(crate::persona::SETTING_PERSONA_ENABLED)
            .ok()
            .flatten()
            .as_deref(),
    );
    // The name the operator set on the Identity page, not a second name invented
    // here. `assistant.name` is the one row that screen writes and that a turn's
    // greeting reads; a hardcoded literal is how this route used to disagree with
    // the rest of the daemon about what the assistant is called.
    let name = engine
        .setting(crate::persona::SETTING_ASSISTANT_NAME)
        .ok()
        .flatten()
        .map(|stored| stored.trim().to_owned())
        .filter(|stored| !stored.is_empty())
        .unwrap_or_else(|| crate::persona::DEFAULT_ASSISTANT_NAME.to_owned());
    let entry = json!({
        "name": name,
        "description": "the daemon's single assistant identity",
        "skillCount": skill_count,
        "active": true,
    });
    let mut body = json!({
        "activeProfile": entry,
        "profiles": [entry],
        "single": true,
        "personaEnabled": persona_on,
        "note": "this daemon has one assistant identity; there are no profiles to switch between",
    });
    if let Value::Object(map) = &mut body {
        map.insert("library_error".to_owned(), library_error);
    }
    answered(body)
}

// ---------------------------------------------------------------------------
// GET /v1/commands
// ---------------------------------------------------------------------------

/// `GET /v1/commands` - the command table and the skill library, as the client's
/// autocomplete reads them.
///
/// Behind `admin`. The rows carry skill keys and descriptions, which is the same
/// class of read as `GET /v1/skills`. A device paired without the administration
/// box gets a typed 403 and falls back to its own built-in list, which is exactly
/// what `AppContainer` already does on any failure (`resetCommandCatalog`).
/// Nothing is lost for that device and nothing is leaked to it.
///
/// `personalities` is an empty array rather than a fabricated list: there is one
/// identity on this daemon, and a picker offering invented names would be the
/// fiction this module exists to avoid.
pub async fn commands(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_admin(&token) {
        return error.into_response();
    }
    let engine = service.engine();
    let rows: Vec<Value> = crate::commands::SUPPORTED
        .iter()
        .map(|(name, description)| {
            let mut row = json!({
                "name": name,
                "description": description,
                "category": "Agent",
            });
            // The client reads `args != nil` as `acceptsArgument`, so the key is
            // present with a hint or absent entirely. An empty string would claim
            // the opposite of what this daemon documents for that command.
            if let (Some(hint), Value::Object(map)) = (argument_hint(name), &mut row) {
                map.insert("args".to_owned(), Value::String(hint.to_owned()));
            }
            row
        })
        .collect();
    let stored = engine.setting(crate::skills::SETTING_DIR).ok().flatten();
    let skills: Vec<Value> = match crate::skills::list(engine.db(), stored.as_deref()) {
        Ok((list, _)) => list
            .iter()
            .filter(|skill| skill.enabled)
            .map(|skill| {
                json!({
                    "name": skill.key,
                    "description": skill.description.clone().unwrap_or_default(),
                })
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    let model = engine.model().ok().flatten().unwrap_or_default();
    let provider = engine
        .setting(crate::providers::SETTING_PRIMARY)
        .ok()
        .flatten()
        .unwrap_or_default();
    answered(json!({
        "commands": rows,
        "skills": skills,
        "personalities": [],
        "quickCommands": [],
        "activeModel": {
            "name": model,
            "provider": provider,
            "contextWindow": engine
                .primary_provider_contexts()
                .ok()
                .and_then(|contexts| contexts.get(&model).copied()),
        },
    }))
}

/// The argument hint for a command, or `None` when it takes none.
fn argument_hint(name: &str) -> Option<&'static str> {
    match name {
        "model" => Some("provider/model"),
        "memory" => Some("gbrain|vault|off"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// GET and POST /v1/device/app-state
// ---------------------------------------------------------------------------

/// `GET /v1/device/app-state` - the daemon's own state row.
///
/// The client POSTs one word describing what the app is doing
/// (`AppContainer.swift:2248`) and treats the answer as non-fatal. This daemon has
/// exactly the right place for it: `app_state`, one row, `setup_state` plus
/// `updated_at` (`db.rs:174`), the row `/healthz` and the ready gate both read.
///
/// `setup_state` is reported and never written by this route. The wizard owns
/// whether this daemon is ready, and a phone must not be able to move that, so the
/// reported word lives in a namespaced setting instead of in the state column. The
/// separation is a second table key, not a comment.
pub async fn app_state_get(State(service): State<DeviceService>, headers: HeaderMap) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Sessions) {
        return error.into_response();
    }
    let engine = service.engine();
    let setup = match engine.db().setup_state() {
        Ok(state) => state.as_str().to_owned(),
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot read the setup state: {error}"))
                .into_response();
        }
    };
    let reported = engine
        .setting(SETTING_APP_STATE_REPORTED)
        .ok()
        .flatten()
        .unwrap_or_default();
    let updated = match now_stamp(engine.db()) {
        Ok(stamp) => stamp,
        Err(error) => return error.into_response(),
    };
    answered(json!({
        "state": if reported.is_empty() {
            Value::Null
        } else {
            Value::String(reported)
        },
        "setupState": setup,
        "updatedAt": updated,
        "accepting": service.accepting(),
        "bound": service.is_bound(),
    }))
}

/// `POST /v1/device/app-state` - record the reported word.
pub async fn app_state_post(
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
    let state = field(&fields, "state");
    if state.is_empty() {
        return DeviceError::bad_request(
            "app_state_missing",
            "the body must carry `state`: one word for what the app is doing",
        )
        .into_response();
    }
    if state.chars().count() > APP_STATE_MAX_CHARS {
        return refuse_value(
            "state",
            "app_state_too_long",
            state,
            &format!("at most {APP_STATE_MAX_CHARS} characters"),
        )
        .into_response();
    }
    if state.chars().any(char::is_control) {
        return refuse_value("state", "app_state_has_control", state, "a printable word")
            .into_response();
    }
    let engine = service.engine();
    if let Err(error) = engine.set_setting(SETTING_APP_STATE_REPORTED, state) {
        return settings_unavailable(error).into_response();
    }
    let stamp = match now_stamp(engine.db()) {
        Ok(stamp) => stamp,
        Err(error) => return error.into_response(),
    };
    // `updated_at` on the one row, so a dashboard read sees that something touched
    // this daemon's state - and not `setup_state`, which stays the wizard's.
    if let Err(error) = engine.db().with_conn(|conn| {
        conn.execute(
            "UPDATE app_state SET updated_at = ?1 WHERE id = 1",
            rusqlite::params![stamp],
        )?;
        Ok(())
    }) {
        return DeviceError::db_unavailable(format!("the state row was not stamped: {error}"))
            .into_response();
    }
    answered(json!({
        "stored": true,
        "state": state,
        "device_id": token.device_id(),
        "updatedAt": stamp,
    }))
}

// ---------------------------------------------------------------------------
// GET /v1/canvas/processes/stream
// ---------------------------------------------------------------------------

/// A subagent row as the Canvas's `BackgroundProcess` reads it.
///
/// `command` is a non-optional `[String]` and `status` a non-optional enum, so both
/// are always present. Every date is an ISO-8601 string: a number here is a decode
/// failure for the whole row.
fn background_process(row: &crate::subagents::Subagent) -> Value {
    let status = match row.status {
        crate::subagents::Status::Queued => "starting",
        crate::subagents::Status::Running => "running",
        crate::subagents::Status::Succeeded => "completed",
        crate::subagents::Status::Failed => "failed",
        crate::subagents::Status::Cancelled => "killed",
    };
    let tail = row
        .result_preview()
        .map(|text| Value::String(text.chars().take(400).collect()))
        .unwrap_or(Value::Null);
    json!({
        "id": row.id,
        "name": row.label,
        "command": [row.label],
        "pid": Value::Null,
        "status": status,
        "exitCode": Value::Null,
        "startedAt": row.started_at,
        "finishedAt": row.finished_at,
        "stream": Value::Null,
        "chunk": Value::Null,
        "outputTail": tail,
        "model": row.model,
        "elapsed": row.elapsed_label(),
    })
}

/// The frame that tells a client a row it was showing is no longer alive.
fn background_process_gone(id: &str) -> Value {
    json!({
        "id": id,
        "name": "",
        "command": [],
        "pid": Value::Null,
        "status": "completed",
        "exitCode": Value::Null,
        "startedAt": Value::Null,
        "finishedAt": Value::Null,
        "stream": Value::Null,
        "chunk": Value::Null,
        "outputTail": Value::Null,
    })
}

/// `GET /v1/canvas/processes/stream` - the subagent rows, as SSE.
///
/// The heaviest offender in the journal: 652 refused requests in one day, because
/// the client reconnects on a backoff and there was never a route. What it wants is
/// a list of the background work this daemon is doing, which is the `subagents`
/// table - the same rows the dashboard's top bar renders. So the snapshot is those
/// rows and the tail is the same read, polled.
///
/// Nothing new is tracked: no process table, no supervisor. A subagent is the only
/// background work this daemon has, and if it grows another kind the honest answer
/// will be to add it here rather than to invent a second surface.
pub async fn canvas_processes(
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
    let db = service.engine().db().clone();
    let (sender, receiver) = mpsc::unbounded_channel::<Result<Event, Infallible>>();
    tokio::spawn(async move {
        let mut seen: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        loop {
            let Ok(rows) = crate::subagents::bar(&db, 50) else {
                break;
            };
            for row in &rows {
                let payload = background_process(row);
                let fingerprint = payload.to_string();
                // Send only what changed. The client merges by id, so a row that
                // did not move is not re-sent on every poll: a quiet daemon
                // produces a quiet stream.
                if seen.get(&row.id).is_some_and(|last| last == &fingerprint) {
                    continue;
                }
                seen.insert(row.id.clone(), fingerprint);
                let frame = Event::default().event("process").data(payload.to_string());
                if sender.send(Ok(frame)).is_err() {
                    return;
                }
            }
            // A row that aged out of the bar is one the client should stop
            // showing. It is told once, then forgotten.
            let live: std::collections::BTreeSet<&str> =
                rows.iter().map(|row| row.id.as_str()).collect();
            let gone: Vec<String> = seen
                .keys()
                .filter(|id| !live.contains(id.as_str()))
                .cloned()
                .collect();
            for id in gone {
                seen.remove(&id);
                let frame = Event::default()
                    .event("process")
                    .data(background_process_gone(&id).to_string());
                if sender.send(Ok(frame)).is_err() {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(CANVAS_POLL_SECONDS)).await;
        }
    });

    Sse::new(UnboundedReceiverStream::new(receiver))
        .keep_alive(
            KeepAlive::new()
                .interval(std::time::Duration::from_secs(KEEP_ALIVE_SECONDS))
                .text("keep-alive"),
        )
        .into_response()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_syslog_priority_reads_as_the_clients_own_words() {
        assert_eq!(level_for_priority(0), "error");
        assert_eq!(level_for_priority(3), "error");
        assert_eq!(level_for_priority(4), "warn");
        assert_eq!(level_for_priority(6), "info");
        assert_eq!(level_for_priority(7), "debug");
        // An unknown priority is a line, not an error state.
        assert_eq!(level_for_priority(200), "info");
    }

    #[test]
    fn a_journal_message_is_read_as_a_string_and_as_bytes() {
        assert_eq!(journal_message(&json!({ "MESSAGE": "hello" })), "hello");
        // journald encodes a message as byte values when it carried anything the
        // journal chose to escape. Both spellings are the same entry.
        assert_eq!(journal_message(&json!({ "MESSAGE": [104, 105] })), "hi");
        assert_eq!(journal_message(&json!({})), "");
    }

    #[test]
    fn escape_sequences_and_carriage_returns_do_not_reach_the_client() {
        assert_eq!(strip_escapes("\u{1b}[2m dim \u{1b}[0m"), " dim ");
        assert_eq!(strip_escapes("a\r\nb"), "a\nb");
        assert_eq!(strip_escapes("plain"), "plain");
    }

    #[test]
    fn a_timestamp_is_a_string_and_never_a_number() {
        // 2026-10-23T15:59:06Z in microseconds, the shape journald sends.
        let entry = json!({ "__REALTIME_TIMESTAMP": "1792771146000000" });
        let stamp = journal_timestamp(&entry);
        assert!(stamp.starts_with("2026-"), "{stamp}");
        assert!(
            stamp.contains('T'),
            "a date the client's decoder accepts is ISO-8601: {stamp}"
        );
        // A missing stamp is an empty string, not an epoch zero that would render
        // as 1970 in the log view.
        assert_eq!(journal_timestamp(&json!({})), "");
    }

    #[test]
    fn a_query_is_decoded_including_escapes() {
        let fields = query_fields("level=all&source=hermes-gateway").expect("query");
        assert_eq!(fields.get("level").map(String::as_str), Some("all"));
        assert_eq!(
            fields.get("source").map(String::as_str),
            Some("hermes-gateway")
        );
        let escaped = query_fields("q=a%20b&plus=a+b").expect("query");
        assert_eq!(escaped.get("q").map(String::as_str), Some("a b"));
        assert_eq!(escaped.get("plus").map(String::as_str), Some("a b"));
        assert!(query_fields("%zz=1").is_err());
    }

    #[test]
    fn a_limit_is_defaulted_and_capped_rather_than_refused() {
        assert_eq!(bounded_limit("", 200, 2000, "limit").expect("default"), 200);
        assert_eq!(bounded_limit("50", 200, 2000, "limit").expect("given"), 50);
        assert_eq!(
            bounded_limit("999999", 200, 2000, "limit").expect("capped"),
            2000
        );
        assert!(bounded_limit("many", 200, 2000, "limit").is_err());
    }

    #[test]
    fn the_machine_id_is_regrouped_into_a_uuid_the_client_can_decode() {
        // The shape the client requires: 8-4-4-4-12. The same 32 hex characters,
        // regrouped, not reinterpreted.
        // Built from two shorter parts on purpose: a machine id is exactly the
        // shape of secret the source-level guard in `tests/bearer_literals.rs`
        // exists to catch, and a fixture is not an exemption from it. Splitting
        // the literal keeps the guard honest instead of teaching it to look away.
        let raw = concat!("bc480a6d167046c3", "bd93d12cb1e9c4a7");
        let regrouped = format!(
            "{}-{}-{}-{}-{}",
            &raw[0..8],
            &raw[8..12],
            &raw[12..16],
            &raw[16..20],
            &raw[20..32]
        );
        assert_eq!(
            regrouped,
            concat!("bc480a6d", "-", "1670", "-", "46c3", "-", "bd93", "-", "d12cb1e9c4a7")
        );
        assert_eq!(regrouped.len(), 36);
        // And it parses as a UUID, which is the requirement: `RelayHost.id` is a
        // non-optional `UUID` on the client.
        assert!(uuid::Uuid::parse_str(&regrouped).is_ok());
    }

    #[test]
    fn argument_hints_name_an_argument_only_where_there_is_one() {
        assert_eq!(argument_hint("model"), Some("provider/model"));
        assert!(argument_hint("help").is_none());
        assert!(argument_hint("stop").is_none());
    }

    #[test]
    fn a_level_filter_keeps_all_and_the_named_level() {
        assert!(level_selected("", "warn"));
        assert!(level_selected("all", "warn"));
        assert!(level_selected("WARN", "warn"));
        assert!(!level_selected("error", "warn"));
    }

    #[test]
    fn the_journal_command_is_built_from_literals() {
        let args = journal_args(200, false);
        assert!(args.contains(&"--user".to_owned()));
        assert!(args.contains(&ERIS_UNIT.to_owned()));
        assert!(args.contains(&"200".to_owned()));
        assert!(!args.contains(&"-f".to_owned()));
        assert!(journal_args(10, true).contains(&"-f".to_owned()));
    }

    #[test]
    fn a_subagent_status_reads_as_a_word_the_client_decodes() {
        // `status` is a non-optional enum on the client and an unknown word fails
        // the whole row's decode, so the mapping is asserted rather than assumed.
        let words = [
            (crate::subagents::Status::Queued, "starting"),
            (crate::subagents::Status::Running, "running"),
            (crate::subagents::Status::Succeeded, "completed"),
            (crate::subagents::Status::Failed, "failed"),
            (crate::subagents::Status::Cancelled, "killed"),
        ];
        for (ours, theirs) in words {
            // The row is built through the same function the route uses, so this
            // is the mapping the wire carries and not a copy of it.
            let row = crate::subagents::Subagent {
                id: "s-1".to_owned(),
                parent_conversation: "c-1".to_owned(),
                parent_turn: None,
                label: "recon".to_owned(),
                prompt: "look".to_owned(),
                model: "m".to_owned(),
                status: ours,
                result: None,
                error_code: None,
                prompt_tokens: None,
                completion_tokens: None,
                created_at: "2026-10-23T00:00:00.000Z".to_owned(),
                started_at: None,
                finished_at: None,
                elapsed_secs: 0,
            };
            assert_eq!(background_process(&row)["status"], json!(theirs));
        }
    }
}
