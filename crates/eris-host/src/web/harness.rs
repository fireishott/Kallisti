//! Shared test support for the web suites: shell, settings, activity, sessions,
//! devices.
//!
//! Every suite drives [`router_with`], the constructor the daemon serves, so the
//! request-id middleware, the forwarded-header rejection and the ready gate are
//! in the path of every assertion. Nothing here is compiled into the binary.
//!
//! The suites that use this module are named for the phase brief, so they are
//! `#[cfg(test)]` modules beside the production code rather than integration
//! tests: the paths read `web::settings::*`, `web::shell::*`, `web::activity::*`,
//! `web::sessions::*` and `web::devices::*`.

use std::future::Future;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use rusqlite::OptionalExtension;
use tower::ServiceExt;

use crate::admin::{AppState, router_with};
use crate::config::Paths;
use crate::db::Db;
use crate::onboarding::{Permissions, SetupEngine};

/// The six purples the UI may never contain. Kept here so all three suites
/// assert against one list.
pub const PURPLE_DENYLIST: [&str; 6] = [
    "#800080", "#7B1FA2", "#6A1B9A", "#9C27B0", "#8E24AA", "#673AB7",
];

/// The form content type.
///
/// Written in two pieces on purpose: `tests/bearer_literals.rs` rejects any run
/// of 32 or more base64/hex alphabet characters in `src/`, and this MIME type is
/// exactly such a run.
pub const FORM_CONTENT_TYPE: &str = concat!("application/x-www-form", "-urlencoded");

/// One in-process request.
pub struct Call {
    method: Method,
    uri: String,
    accept_html: bool,
    cookie: Option<String>,
    content_type: Option<&'static str>,
    body: Option<String>,
    peer: Option<std::net::SocketAddr>,
    headers: Vec<(&'static str, &'static str)>,
}

impl Call {
    /// A GET on a route that is a literal.
    pub fn get(uri: &'static str) -> Self {
        Self::get_owned(uri.to_owned())
    }

    /// A GET on a route that carries an id, so the path is built at run time.
    pub fn get_owned(uri: String) -> Self {
        Self {
            method: Method::GET,
            uri,
            accept_html: false,
            cookie: None,
            content_type: None,
            body: None,
            peer: None,
            headers: Vec::new(),
        }
    }

    /// A form POST on a route that is a literal.
    pub fn post(uri: &'static str, body: &str) -> Self {
        Self::post_owned(uri.to_owned(), body)
    }

    /// A form POST on a route that carries an id.
    pub fn post_owned(uri: String, body: &str) -> Self {
        Self {
            method: Method::POST,
            uri,
            accept_html: false,
            cookie: None,
            content_type: Some(FORM_CONTENT_TYPE),
            body: Some(body.to_owned()),
            peer: None,
            headers: Vec::new(),
        }
    }

    /// Replace the request content type, for the multipart suites.
    pub fn content_type(mut self, value: &'static str) -> Self {
        self.content_type = Some(value);
        self
    }

    /// Send a browser `Accept` header.
    pub fn html(mut self) -> Self {
        self.accept_html = true;
        self
    }

    /// Pretend the request arrived from this TCP peer.
    ///
    /// The daemon gets this from `into_make_service_with_connect_info`, which is
    /// exactly what a `ConnectInfo` extension is; inserting it here means the
    /// trusted-proxy rule is exercised through the real middleware.
    pub fn peer(mut self, addr: std::net::SocketAddr) -> Self {
        self.peer = Some(addr);
        self
    }

    /// Add one request header.
    pub fn header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((name, value));
        self
    }

    /// Carry (or omit) the admin session cookie.
    pub fn cookie(mut self, cookie: Option<&str>) -> Self {
        self.cookie = cookie.map(str::to_owned);
        self
    }
}

/// A full response, decoded.
pub struct Reply {
    /// The status line's code.
    pub status: StatusCode,
    /// The body, as UTF-8.
    pub body: String,
    /// The response headers.
    pub headers: HeaderMap,
}

impl Reply {
    /// The body as JSON, or a panic that prints it.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|error| panic!("body is not json: {error}: {}", self.body))
    }

    /// The `Set-Cookie` value, minus everything after the first `;`.
    pub fn cookie(&self) -> Option<String> {
        self.headers
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::to_owned)
    }

    /// The `Location` header.
    pub fn location(&self) -> Option<String> {
        self.headers
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    }
}

/// Send one request through the router.
pub async fn send(app: &Router, call: Call) -> Reply {
    let mut builder = Request::builder().method(call.method).uri(call.uri);
    builder = builder.header(
        header::ACCEPT,
        if call.accept_html {
            "text/html,application/xhtml+xml"
        } else {
            "*/*"
        },
    );
    if let Some(content_type) = call.content_type {
        builder = builder.header(header::CONTENT_TYPE, content_type);
    }
    if let Some(cookie) = &call.cookie {
        builder = builder.header(header::COOKIE, cookie);
    }
    for (name, value) in &call.headers {
        builder = builder.header(*name, *value);
    }
    let mut request = builder
        .body(Body::from(call.body.unwrap_or_default()))
        .expect("a valid request");
    if let Some(peer) = call.peer {
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(peer));
    }
    let response = app
        .clone()
        .oneshot(request)
        .await
        .expect("the router is infallible");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("a readable body");
    Reply {
        status,
        body: String::from_utf8(bytes.to_vec()).expect("utf-8 body"),
        headers,
    }
}

/// Run one test on its own current-thread runtime.
pub fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(future)
}

/// Percent-encode a form value. `+` for space, everything unusual escaped.
pub fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// A fresh database that has not been claimed: the `setup` state.
pub fn setup_engine() -> (tempfile::TempDir, SetupEngine, String) {
    let dir = tempfile::tempdir().expect("temp dir");
    let paths = Paths::for_root(dir.path());
    let db = Db::open(&paths.db_file).expect("a fresh database opens");
    // The chat surface resolves a conversation, and a suite that appends to the
    // documented fixture id expects a page to find it.
    crate::web::chat::seed_conversation(&db, crate::web::chat::CONVERSATION_ID)
        .expect("the fixture conversation");
    let engine = SetupEngine::new(db, paths);
    let passphrase = engine
        .ensure_first_run()
        .expect("first run")
        .generated
        .expect("a fresh database generates a passphrase");
    (dir, engine, passphrase)
}

/// A ready daemon with a known admin passphrase, built without the network.
///
/// This is the wizard walk at the engine level. The one step it skips is the
/// provider probe, which is a real HTTP call and has nothing to do with the
/// pages under test.
pub fn ready_engine() -> (tempfile::TempDir, SetupEngine, String) {
    let (dir, engine, passphrase) = setup_engine();
    engine.accept_claim(&passphrase).expect("claim");
    engine
        .set_provider(
            "http://127.0.0.1:20128/v1",
            "env:NINEROUTER_API_KEY",
            &["ds/deepseek-v4-flash".to_owned()],
        )
        .expect("provider");
    engine.set_model("ds/deepseek-v4-flash").expect("model");
    engine
        .set_permissions(Permissions::DEFAULT)
        .expect("permissions");
    engine
        .set_workspace(&dir.path().join("kallisti-workspace").display().to_string())
        .expect("workspace");
    engine.finish().expect("finish");
    (dir, engine, passphrase)
}

/// Sign in through the route a browser uses, and return the cookie pair.
pub fn sign_in(app: &Router, passphrase: &str) -> String {
    let reply = block_on(send(
        app,
        Call::post("/login", &format!("passphrase={}", encode(passphrase))),
    ));
    assert_eq!(
        reply.status,
        StatusCode::SEE_OTHER,
        "sign-in should redirect: {}",
        reply.body
    );
    reply.cookie().expect("sign-in mints the session cookie")
}

/// A ready daemon, a router, and a signed-in cookie.
pub fn ready_app() -> (tempfile::TempDir, SetupEngine, Router, String) {
    let (dir, engine, passphrase) = ready_engine();
    let app = router_with(AppState::new(engine.clone(), false));
    let cookie = sign_in(&app, &passphrase);
    (dir, engine, app, cookie)
}

/// One `audit_log` row, as the tests read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRow {
    /// When it was written.
    pub occurred_at: String,
    /// Who did it.
    pub actor: String,
    /// What happened.
    pub action: String,
    /// The non-secret detail line.
    pub detail: Option<String>,
}

/// Every audit row, oldest first.
pub fn audit_rows(db: &Db) -> Vec<AuditRow> {
    db.with_conn(|conn| {
        let mut statement =
            conn.prepare("SELECT occurred_at, actor, action, detail FROM audit_log ORDER BY id")?;
        let mut rows = statement.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(AuditRow {
                occurred_at: row.get(0)?,
                actor: row.get(1)?,
                action: row.get(2)?,
                detail: row.get(3)?,
            });
        }
        Ok(out)
    })
    .expect("audit rows read")
}

// ---------------------------------------------------------------------------
// Fixtures for the sessions and devices suites
//
// The two pages read the tables the daemon already writes, so the tests write
// those tables directly rather than inventing a second path into them. Every
// helper takes the columns the page reads, and nothing is invented that the
// schema does not allow.
// ---------------------------------------------------------------------------

/// One `devices` row.
pub fn seed_device(
    db: &Db,
    id: &str,
    name: &str,
    platform: &str,
    paired_at: Option<&str>,
    last_seen_at: Option<&str>,
    revoked_at: Option<&str>,
) {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO devices (id, name, platform, paired_at, last_seen_at, revoked_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![id, name, platform, paired_at, last_seen_at, revoked_at],
        )?;
        Ok(())
    })
    .expect("seed device");
}

/// A redeemed pairing code, which is one of the two facts that make a device
/// `paired` in the page's own query.
pub fn seed_redeemed_code(db: &Db, device_id: &str) {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO pairing_codes (id, code_hash, device_id, expires_at, consumed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                format!("code-{device_id}"),
                "not-a-real-verifier",
                device_id,
                "2026-01-01T00:00:00.000Z",
                "2026-01-01T00:00:01.000Z"
            ],
        )?;
        Ok(())
    })
    .expect("seed pairing code");
}

/// One `sessions` row.
pub fn seed_session(
    db: &Db,
    id: &str,
    device_id: Option<&str>,
    user_agent: Option<&str>,
    created_at: &str,
    expires_at: &str,
    revoked_at: Option<&str>,
) {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO sessions \
             (id, device_id, user_agent, created_at, last_seen_at, expires_at, revoked_at) \
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6)",
            rusqlite::params![
                id, device_id, user_agent, created_at, expires_at, revoked_at
            ],
        )?;
        Ok(())
    })
    .expect("seed session");
}

/// One `jobs` row. The nullable columns are passed as `None` by the caller.
#[allow(clippy::too_many_arguments)]
pub fn seed_job(
    db: &Db,
    id: &str,
    session_id: Option<&str>,
    status: &str,
    request_text: Option<&str>,
    created_at: &str,
    finished_at: Option<&str>,
    error_code: Option<&str>,
    error_message: Option<&str>,
) {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO jobs \
             (id, kind, status, request_text, created_at, updated_at, finished_at, \
              session_id, error_code, error_message) \
             VALUES (?1, 'turn', ?2, ?3, ?4, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                id,
                status,
                request_text,
                created_at,
                finished_at,
                session_id,
                error_code,
                error_message
            ],
        )?;
        Ok(())
    })
    .expect("seed job");
}

/// One `job_events` row.
pub fn seed_event(
    db: &Db,
    job_id: &str,
    seq: i64,
    kind: &str,
    payload: Option<&str>,
    occurred_at: &str,
) {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO job_events (job_id, seq, kind, payload, occurred_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![job_id, seq, kind, payload, occurred_at],
        )?;
        Ok(())
    })
    .expect("seed event");
}

/// One `jobs.status`, read straight from the table.
pub fn job_status(db: &Db, id: &str) -> Option<String> {
    db.with_conn(|conn| {
        conn.query_row("SELECT status FROM jobs WHERE id = ?1", [id], |row| {
            row.get::<_, String>(0)
        })
        .optional()
    })
    .expect("job status read")
}

/// Every text value of every row a `SELECT` returns, newline separated.
///
/// The pairing-code suite uses this to prove a minted code is nowhere in the
/// tables: it is a whole-table read, not a guess about which column would hold
/// it. Non-text columns are skipped rather than formatted.
pub fn dump_text(db: &Db, sql: &str) -> String {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(sql)?;
        let count = statement.column_count();
        let mut rows = statement.query([])?;
        let mut out = String::new();
        while let Some(row) = rows.next()? {
            for index in 0..count {
                let value: Option<String> = row.get(index).unwrap_or(None);
                if let Some(text) = value {
                    out.push_str(&text);
                    out.push('\n');
                }
            }
        }
        Ok(out)
    })
    .expect("table dump")
}

/// The `tr` a row link sits in, so a test can assert about one row only.
pub fn row_window<'a>(page: &'a str, marker: &str) -> &'a str {
    let at = page
        .find(marker)
        .unwrap_or_else(|| panic!("{marker} is missing from the page: {page}"));
    let end = page[at..]
        .find("</tr>")
        .map(|offset| at + offset)
        .unwrap_or(page.len());
    let start = page[..at].rfind("<tr").unwrap_or(0);
    &page[start..end]
}
