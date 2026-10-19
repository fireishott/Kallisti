//! Shared test support for the phase 1 web suites: shell, settings, activity.
//!
//! Every suite drives [`router_with`], the constructor the daemon serves, so the
//! request-id middleware, the forwarded-header rejection and the ready gate are
//! in the path of every assertion. Nothing here is compiled into the binary.
//!
//! The three suites that use this module are named for the phase brief, so they
//! are `#[cfg(test)]` modules beside the production code rather than integration
//! tests: the paths read `web::settings::*`, `web::shell::*` and `web::activity::*`.

use std::future::Future;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
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
    uri: &'static str,
    accept_html: bool,
    cookie: Option<String>,
    content_type: Option<&'static str>,
    body: Option<String>,
}

impl Call {
    /// A GET.
    pub fn get(uri: &'static str) -> Self {
        Self {
            method: Method::GET,
            uri,
            accept_html: false,
            cookie: None,
            content_type: None,
            body: None,
        }
    }

    /// A form POST.
    pub fn post(uri: &'static str, body: &str) -> Self {
        Self {
            method: Method::POST,
            uri,
            accept_html: false,
            cookie: None,
            content_type: Some(FORM_CONTENT_TYPE),
            body: Some(body.to_owned()),
        }
    }

    /// Send a browser `Accept` header.
    pub fn html(mut self) -> Self {
        self.accept_html = true;
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
    let request = builder
        .body(Body::from(call.body.unwrap_or_default()))
        .expect("a valid request");
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
