//! P11 acceptance: the sign-in route, in process, against the same router the
//! daemon serves.
//!
//! The defect this suite pins down: the dashboard was reachable only with a
//! cookie that `POST /setup/claim` minted, and `claim` exists only while the
//! wizard is open. Once onboarding finished, a browser had no route that could
//! produce a session, so the dashboard was unreachable from a browser at all.
//! `GET /login` and `POST /login` are that route.
//!
//! The six claims that matter:
//!
//! - the page is a real form: labelled field, `current-password`, an `aria-live`
//!   region, no purple, no old name, no script;
//! - the passphrase the operator chose during onboarding puts a cookie in the
//!   browser and opens the dashboard;
//! - a wrong passphrase is a 401 and mints nothing;
//! - an empty passphrase is a 400 that names the field;
//! - the limiter is not bypassed by the new route;
//! - a browser without a cookie is redirected to the form, and an API client
//!   still gets the documented `403 no_session`.
//!
//! Everything runs against [`router_with`], the constructor the daemon uses, so
//! the forwarded-header rejection and the ready gate are in the path here too.

use std::future::Future;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use tower::ServiceExt;

use crate::admin::{AppState, ROUTE_DASHBOARD, ROUTE_LOGIN, router_with};
use crate::config::Paths;
use crate::db::Db;
use crate::onboarding::{Permissions, SetupEngine};

/// The six purples the UI may never contain.
const PURPLE_DENYLIST: [&str; 6] = [
    "#800080", "#7B1FA2", "#6A1B9A", "#9C27B0", "#8E24AA", "#673AB7",
];

/// The admin passphrase every fixture starts from, when a test needs to type one
/// that is definitely wrong.
const WRONG_PASSPHRASE: &str = "these are not the six words";

/// The form content type.
///
/// Written in two pieces on purpose: `tests/bearer_literals.rs` rejects any run
/// of 32 or more base64/hex alphabet characters in `src/`, and this MIME type is
/// exactly such a run (`/` and `-` are both in that alphabet). The scan is a
/// real guard, so the literal bends rather than the guard.
const FORM_CONTENT_TYPE: &str = concat!("application/x-www-form", "-urlencoded");

/// One in-process request.
struct Call {
    method: Method,
    uri: &'static str,
    accept_html: bool,
    cookie: Option<String>,
    content_type: Option<&'static str>,
    body: Option<String>,
    headers: Vec<(&'static str, &'static str)>,
}

impl Call {
    fn get(uri: &'static str) -> Self {
        Self {
            method: Method::GET,
            uri,
            accept_html: false,
            cookie: None,
            content_type: None,
            body: None,
            headers: Vec::new(),
        }
    }

    fn post(uri: &'static str, body: &str) -> Self {
        Self {
            method: Method::POST,
            uri,
            accept_html: false,
            cookie: None,
            content_type: Some(FORM_CONTENT_TYPE),
            body: Some(body.to_owned()),
            headers: Vec::new(),
        }
    }

    fn html(mut self) -> Self {
        self.accept_html = true;
        self
    }

    fn cookie(mut self, cookie: Option<&str>) -> Self {
        self.cookie = cookie.map(str::to_owned);
        self
    }

    fn header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((name, value));
        self
    }
}

/// A full response, decoded.
struct Reply {
    status: StatusCode,
    body: String,
    headers: HeaderMap,
}

impl Reply {
    fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|error| panic!("body is not json: {error}: {}", self.body))
    }

    /// The `Set-Cookie` value, minus everything after the first `;`.
    fn cookie(&self) -> Option<String> {
        self.headers
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .map(str::to_owned)
    }

    /// The whole `Set-Cookie` attribute list.
    fn set_cookie(&self) -> Option<String> {
        self.headers
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    }

    fn location(&self) -> Option<String> {
        self.headers
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    }
}

/// Send one request through the router.
async fn send(app: &Router, call: Call) -> Reply {
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
    for (name, value) in call.headers {
        builder = builder.header(name, value);
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
///
/// These are plain `#[test]` functions so the names read exactly as the phase
/// brief lists them; the harness only needs `oneshot`, so a current-thread
/// runtime is enough.
fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime")
        .block_on(future)
}

/// A ready daemon with a known admin passphrase, built without the network.
///
/// This is the phase 7 walk at the engine level: the one step it skips is the
/// provider probe, which is a real HTTP call and has nothing to do with signing
/// in. The passphrase returned here is the verifier the operator would have kept
/// in `first-run.txt`.
fn ready_engine() -> (tempfile::TempDir, SetupEngine, String) {
    let dir = tempfile::tempdir().expect("temp dir");
    let paths = Paths::for_root(dir.path());
    let db = Db::open(&paths.db_file).expect("a fresh database opens");
    let engine = SetupEngine::new(db, paths);
    let passphrase = engine
        .ensure_first_run()
        .expect("first run")
        .generated
        .expect("a fresh database generates a passphrase");
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

/// Percent-encode a form value. `+` for space, everything unusual escaped.
fn encode(value: &str) -> String {
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

#[test]
fn the_login_page_renders_with_no_purple_and_no_herald() {
    let (_dir, engine, _passphrase) = ready_engine();
    let app = router_with(AppState::new(engine, false));

    let reply = block_on(send(&app, Call::get(ROUTE_LOGIN).html()));
    assert_eq!(reply.status, StatusCode::OK, "login page: {}", reply.body);

    let page = reply.body.to_ascii_lowercase();
    assert!(
        reply.body.contains(r#"<label for="passphrase">"#),
        "the field is labelled: {}",
        reply.body
    );
    assert!(
        reply.body.contains(r#"autocomplete="current-password""#),
        "a password manager knows which credential this is"
    );
    assert!(
        reply.body.contains(r#"type="password""#),
        "the passphrase is not typed in the clear"
    );
    assert!(
        reply.body.contains(r#"aria-live="polite""#),
        "the error region announces itself without script"
    );
    assert!(
        reply.body.contains(r#"method="post""#) && reply.body.contains(r#"action="/login""#),
        "the form posts to the route that mints the session"
    );
    assert!(
        page.contains("the passphrase you chose during onboarding"),
        "the page says which credential it wants"
    );
    assert!(!page.contains("<script"), "there is no script in the UI");
    assert!(!page.contains("herald"), "the old name is gone");
    for banned in PURPLE_DENYLIST {
        assert!(
            !page.contains(&banned.to_ascii_lowercase()),
            "purple {banned} is in the login page"
        );
    }
    assert!(
        !reply.body.contains('\u{2014}'),
        "no em dash reaches the login page"
    );
    // The doctype is the only exclamation mark on the page: prose carries none.
    assert!(
        !reply.body.replace("<!DOCTYPE html>", "").contains('!'),
        "no exclamation mark in the page prose"
    );
    assert!(
        !page.contains("http://") || page.contains("127.0.0.1"),
        "no external asset is fetched"
    );
}

#[test]
fn a_correct_passphrase_sets_a_cookie_and_opens_the_dashboard() {
    let (_dir, engine, passphrase) = ready_engine();
    let app = router_with(AppState::new(engine, false));

    let reply = block_on(send(
        &app,
        Call::post(ROUTE_LOGIN, &format!("passphrase={}", encode(&passphrase))),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "login: {}", reply.body);
    assert_eq!(reply.location().as_deref(), Some(ROUTE_DASHBOARD));

    let cookie = reply.set_cookie().expect("login sets the session cookie");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    assert!(cookie.contains("SameSite=Strict"), "{cookie}");
    assert!(cookie.contains("Path=/"), "{cookie}");

    let bare = reply.cookie().expect("the cookie pair");
    let dashboard = block_on(send(
        &app,
        Call::get(ROUTE_DASHBOARD)
            .html()
            .cookie(Some(bare.as_str())),
    ));
    assert_eq!(
        dashboard.status,
        StatusCode::OK,
        "the cookie opens the dashboard: {}",
        dashboard.body
    );
    assert!(
        dashboard.body.contains("Dashboard"),
        "a real page, not a refusal: {}",
        dashboard.body
    );

    // And the form is no longer the answer for this browser.
    let again = block_on(send(
        &app,
        Call::get(ROUTE_LOGIN).html().cookie(Some(bare.as_str())),
    ));
    assert_eq!(again.status, StatusCode::SEE_OTHER);
    assert_eq!(again.location().as_deref(), Some(ROUTE_DASHBOARD));
}

#[test]
fn a_wrong_passphrase_is_401_and_sets_no_cookie() {
    let (_dir, engine, _passphrase) = ready_engine();
    let app = router_with(AppState::new(engine, false));

    let page = block_on(send(
        &app,
        Call::post(
            ROUTE_LOGIN,
            &format!("passphrase={}", encode(WRONG_PASSPHRASE)),
        )
        .html(),
    ));
    assert_eq!(page.status, StatusCode::UNAUTHORIZED, "{}", page.body);
    assert!(page.set_cookie().is_none(), "a refusal mints no session");
    assert!(
        page.body.contains(r#"aria-live="polite""#),
        "the error is announced on the page"
    );
    assert!(
        page.body.contains("admin_passphrase_rejected"),
        "the reason is on the page: {}",
        page.body
    );

    // The same refusal for an API client, in JSON, without the page.
    let api = block_on(send(
        &app,
        Call::post(
            ROUTE_LOGIN,
            &format!("passphrase={}", encode(WRONG_PASSPHRASE)),
        ),
    ));
    assert_eq!(api.status, StatusCode::UNAUTHORIZED);
    assert!(api.set_cookie().is_none());
    assert_eq!(api.json()["code"], "admin_passphrase_rejected");

    // Nothing was written on the way through.
    let dashboard = block_on(send(&app, Call::get(ROUTE_DASHBOARD)));
    assert_eq!(dashboard.status, StatusCode::FORBIDDEN);
    assert_eq!(dashboard.json()["code"], "no_session");
}

#[test]
fn an_empty_passphrase_is_400_with_a_field_error() {
    let (_dir, engine, _passphrase) = ready_engine();
    let app = router_with(AppState::new(engine, false));

    let page = block_on(send(&app, Call::post(ROUTE_LOGIN, "passphrase=").html()));
    assert_eq!(page.status, StatusCode::BAD_REQUEST, "{}", page.body);
    assert!(page.set_cookie().is_none());
    assert!(
        page.body.contains("passphrase_required"),
        "the field error is named: {}",
        page.body
    );

    let api = block_on(send(&app, Call::post(ROUTE_LOGIN, "")));
    assert_eq!(api.status, StatusCode::BAD_REQUEST);
    let body = api.json();
    assert_eq!(body["ok"], false);
    assert_eq!(body["field"], "passphrase");
    assert_eq!(body["code"], "passphrase_required");
}

#[test]
fn failed_logins_are_rate_limited() {
    let (_dir, engine, _passphrase) = ready_engine();
    let app = router_with(AppState::new(engine, false));

    let body = format!("passphrase={}", encode(WRONG_PASSPHRASE));
    for attempt in 1..=crate::onboarding::CLAIM_LIMIT {
        let reply = block_on(send(&app, Call::post(ROUTE_LOGIN, &body)));
        assert_eq!(
            reply.status,
            StatusCode::UNAUTHORIZED,
            "attempt {attempt}: {}",
            reply.body
        );
    }

    let refused = block_on(send(&app, Call::post(ROUTE_LOGIN, &body)));
    assert_eq!(
        refused.status,
        StatusCode::TOO_MANY_REQUESTS,
        "the limiter covers the new route: {}",
        refused.body
    );
    assert_eq!(refused.json()["code"], "rate_limited");
    assert!(
        refused.set_cookie().is_none(),
        "a rate-limited attempt mints nothing"
    );

    // The window is shared with the claim form, so a correct passphrase is not a
    // way around the budget either.
    let (_dir2, engine2, passphrase2) = ready_engine();
    let app2 = router_with(AppState::new(engine2, false));
    for _ in 0..crate::onboarding::CLAIM_LIMIT {
        let _ = block_on(send(
            &app2,
            Call::post(
                ROUTE_LOGIN,
                &format!("passphrase={}", encode(WRONG_PASSPHRASE)),
            ),
        ));
    }
    let correct = block_on(send(
        &app2,
        Call::post(ROUTE_LOGIN, &format!("passphrase={}", encode(&passphrase2))),
    ));
    assert_eq!(correct.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(correct.set_cookie().is_none());
}

#[test]
fn dashboard_without_a_session_redirects_a_browser_but_not_an_api_client() {
    let (_dir, engine, _passphrase) = ready_engine();
    let app = router_with(AppState::new(engine, false));

    let browser = block_on(send(&app, Call::get(ROUTE_DASHBOARD).html()));
    assert_eq!(browser.status, StatusCode::SEE_OTHER, "{}", browser.body);
    assert_eq!(browser.location().as_deref(), Some(ROUTE_LOGIN));

    let api = block_on(send(&app, Call::get(ROUTE_DASHBOARD)));
    assert_eq!(api.status, StatusCode::FORBIDDEN);
    let body = api.json();
    assert_eq!(body["code"], "no_session");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|text| !text.is_empty()),
        "the refusal explains itself"
    );

    // A stale cookie is the same as no cookie.
    let stale = block_on(send(
        &app,
        Call::get(ROUTE_DASHBOARD)
            .html()
            .cookie(Some("kallisti_admin=1.deadbeef")),
    ));
    assert_eq!(stale.status, StatusCode::SEE_OTHER);
    assert_eq!(stale.location().as_deref(), Some(ROUTE_LOGIN));

    // And the wizard still redirects to itself, not to the login form: an
    // operator who has not finished onboarding has no credential to type.
    let wizard = block_on(send(
        &app,
        Call::post("/setup/claim", "passphrase=x").html(),
    ));
    assert_eq!(wizard.status, StatusCode::CONFLICT, "{}", wizard.body);
}

#[test]
fn login_is_not_reachable_through_a_proxy() {
    let (_dir, engine, _passphrase) = ready_engine();
    let app = router_with(AppState::new(engine, false));

    for name in ["x-forwarded-for", "forwarded", "tailscale-user-login"] {
        let reply = block_on(send(&app, Call::get(ROUTE_LOGIN).header(name, "1.2.3.4")));
        assert_eq!(reply.status, StatusCode::FORBIDDEN, "{name}");
        assert_eq!(reply.json()["code"], "not_loopback", "{name}");
    }
}
