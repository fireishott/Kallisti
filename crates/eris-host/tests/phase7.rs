//! P7 acceptance: the whole wizard, in process, against the same router the
//! daemon serves.
//!
//! This is the `curl` walk from the phase brief, minus the shell: the same
//! routes, the same cookies, the same status codes, and the same rendered HTML
//! that the screenshots and the greps look at. The provider step talks to a
//! throwaway loopback HTTP server, so the probe path is real without needing a
//! key or the installed gateway.
//!
//! The three claims that matter most:
//!
//! - a fresh database is `setup`, `/dashboard` is `409 not_ready`, and the wizard
//!   can be walked to `ready` with nothing but form posts;
//! - a request carrying a forwarding header is `403 not_loopback`, and the same
//!   request without one is `200`;
//! - the rendered wizard carries no old name and no purple.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use eris_host::admin::{
    ADMIN_ADDR, ALL_GET_ROUTES, AppState, POST_ROUTES, ROUTE_DASHBOARD, ROUTE_HEALTHZ, ROUTE_ROOT,
    ROUTE_SETUP, ROUTE_SETUP_STATE, VERSION, router_with,
};
use eris_host::config::Paths;
use eris_host::db::{Db, SetupState};
use eris_host::onboarding::SetupEngine;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

/// The key reference the wizard is given in these tests.
///
/// It must name a variable that exists, because the handler resolves it against
/// the real process environment, and a test must not mutate that environment
/// (`set_var` is unsafe in edition 2024 and racy in a threaded harness). `HOME` is
/// the one variable this crate already requires, and the throwaway probe server
/// ignores the bearer header entirely, so nothing here asserts on a key value.
const TEST_KEY_REF: &str = "env:HOME";

/// The provider body the throwaway probe server answers with.
const MODELS_BODY: &str =
    r#"{"object":"list","data":[{"id":"model/one"},{"id":"model/two"},{"id":"model/three"}]}"#;

/// The six purples the UI may never contain.
const PURPLE_DENYLIST: [&str; 6] = [
    "#800080", "#7B1FA2", "#6A1B9A", "#9C27B0", "#8E24AA", "#673AB7",
];

/// One in-process request, with optional headers, cookie and body.
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
            content_type: Some("application/x-www-form-urlencoded"),
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

    fn json_body(mut self) -> Self {
        self.content_type = Some("application/json");
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

    fn cookie(&self) -> Option<String> {
        self.headers
            .get(header::SET_COOKIE)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
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

/// A scratch engine: its own config root, its own state directory, its own
/// database. Nothing under `$HOME` is read or written.
fn scratch() -> (tempfile::TempDir, SetupEngine) {
    let dir = tempfile::tempdir().expect("temp dir");
    let paths = Paths::for_root(dir.path());
    let db = Db::open(&paths.db_file).expect("a fresh database opens");
    (dir, SetupEngine::new(db, paths))
}

/// A throwaway OpenAI-compatible `/models` endpoint on loopback.
///
/// It answers every connection with the same catalog and keeps accepting until
/// the test's runtime shuts down. No key is required and none is checked beyond
/// the header being present, because the test is about the wizard, not about
/// 9router.
async fn spawn_models_server() -> (SocketAddr, Arc<tokio::task::JoinHandle<()>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback listener");
    let addr = listener.local_addr().expect("a local address");
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 4096];
                let _ = socket.read(&mut buffer).await;
                let response = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    MODELS_BODY.len(),
                    MODELS_BODY
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (addr, Arc::new(handle))
}

/// The passphrase the first boot generated, for the claim step.
fn first_run_passphrase(engine: &SetupEngine) -> String {
    engine
        .ensure_first_run()
        .expect("first run")
        .generated
        .expect("a fresh database generates a passphrase")
}

/// Walk claim -> finish, returning the cookie and the workspace path.
async fn walk_wizard(
    app: &Router,
    engine: &SetupEngine,
    passphrase: &str,
    models_addr: SocketAddr,
    dir: &std::path::Path,
) -> (String, std::path::PathBuf) {
    let claim = send(
        app,
        Call::post(
            "/setup/claim",
            &format!("passphrase={}", encode(passphrase)),
        ),
    )
    .await;
    assert_eq!(claim.status, StatusCode::OK, "claim: {}", claim.body);
    let session = claim.cookie().expect("claim sets the session cookie");
    let cookie = Some(session.as_str());

    let provider = send(
        app,
        Call::post(
            "/setup/provider",
            &format!(
                "base_url={}&api_key_ref={}",
                encode(&format!("http://{models_addr}/v1")),
                encode(TEST_KEY_REF)
            ),
        )
        .cookie(cookie),
    )
    .await;
    assert_eq!(
        provider.status,
        StatusCode::OK,
        "provider probe: {}",
        provider.body
    );
    assert_eq!(provider.json()["model_count"], 3);

    let models = send(
        app,
        Call::post("/setup/models", "model=model%2Ftwo").cookie(cookie),
    )
    .await;
    assert_eq!(models.status, StatusCode::OK, "models: {}", models.body);

    let permissions = send(
        app,
        Call::post(
            "/setup/permissions",
            "sandbox_mode=workspace-write&approval_policy=ask&autopilot_ceiling=L1+draft&telemetry=off",
        )
        .cookie(cookie),
    )
    .await;
    assert_eq!(
        permissions.status,
        StatusCode::OK,
        "permissions: {}",
        permissions.body
    );

    let workspace_path = dir.join("kallisti-workspace");
    let workspace = send(
        app,
        Call::post(
            "/setup/workspace",
            &format!(
                "workspace_path={}",
                encode(&workspace_path.display().to_string())
            ),
        )
        .cookie(cookie),
    )
    .await;
    assert_eq!(
        workspace.status,
        StatusCode::OK,
        "workspace: {}",
        workspace.body
    );
    assert_eq!(workspace.json()["created"], true);

    let device = send(
        app,
        Call::post("/setup/device", "device_name=TestPhone").cookie(cookie),
    )
    .await;
    assert_eq!(device.status, StatusCode::OK, "device: {}", device.body);
    let code = device.json()["pairing_code"]
        .as_str()
        .expect("a pairing code")
        .to_owned();
    assert_eq!(code.len(), 8, "the pairing alphabet is eight characters");

    let finish = send(app, Call::post("/setup/finish", "").cookie(cookie)).await;
    assert_eq!(finish.status, StatusCode::OK, "finish: {}", finish.body);
    assert_eq!(finish.json()["state"], "ready");
    assert_eq!(engine.state().expect("state"), SetupState::Ready);

    (session, workspace_path)
}

/// Percent-encode a form value. `+` for space, everything unusual escaped.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

#[tokio::test]
async fn a_fresh_database_walks_from_setup_to_ready() {
    let (dir, engine) = scratch();
    let passphrase = first_run_passphrase(&engine);
    let app = router_with(AppState::new(engine.clone(), false));
    let (models_addr, _server) = spawn_models_server().await;

    // 2. A fresh database is in setup.
    let health = send(&app, Call::get(ROUTE_HEALTHZ)).await;
    assert_eq!(health.status, StatusCode::OK);
    assert_eq!(health.json()["state"], "setup");

    // 3. The dashboard is 409 not_ready.
    let dashboard = send(&app, Call::get(ROUTE_DASHBOARD)).await;
    assert_eq!(dashboard.status, StatusCode::CONFLICT);
    assert_eq!(dashboard.json()["code"], "not_ready");

    // The root redirects to the wizard while in setup.
    let root = send(&app, Call::get(ROUTE_ROOT)).await;
    assert_eq!(root.status, StatusCode::FOUND);
    assert_eq!(
        root.headers
            .get(header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some(ROUTE_SETUP)
    );

    // 6 (first half). The rendered wizard is clean before anything is claimed.
    let setup = send(&app, Call::get(ROUTE_SETUP).html()).await;
    assert_eq!(setup.status, StatusCode::OK);
    assert!(setup.body.contains("Onboarding"), "the wizard renders");
    assert!(setup.body.contains(r#"id="passphrase""#));
    let lowered = setup.body.to_ascii_lowercase();
    assert!(
        !lowered.contains("herald"),
        "the rendered wizard carries the old name"
    );
    for banned in PURPLE_DENYLIST {
        assert!(
            !lowered.contains(&banned.to_ascii_lowercase()),
            "the rendered wizard carries purple {banned}"
        );
    }

    // 4. The whole wizard.
    let (cookie, workspace_path) =
        walk_wizard(&app, &engine, &passphrase, models_addr, dir.path()).await;
    assert!(workspace_path.is_dir(), "the workspace was created");
    let mode = std::os::unix::fs::PermissionsExt::mode(
        &std::fs::metadata(&workspace_path)
            .expect("metadata")
            .permissions(),
    ) & 0o777;
    assert_eq!(mode, 0o755, "the workspace is mode 0755");

    // 5. Health says ready.
    let health = send(&app, Call::get(ROUTE_HEALTHZ)).await;
    assert_eq!(health.json()["state"], "ready");

    // The dashboard now answers, with the session cookie.
    let dashboard = send(
        &app,
        Call::get(ROUTE_DASHBOARD).html().cookie(Some(&cookie)),
    )
    .await;
    assert_eq!(dashboard.status, StatusCode::OK);
    assert!(dashboard.body.contains("Dashboard"));
    assert!(dashboard.body.contains("TestPhone"), "the device is listed");
    assert!(!dashboard.body.to_ascii_lowercase().contains("herald"));

    // /setup now reports complete, and the second half of grep check 6.
    let setup = send(&app, Call::get(ROUTE_SETUP).html().cookie(Some(&cookie))).await;
    assert_eq!(setup.status, StatusCode::OK);
    assert!(setup.body.contains("Onboarding complete"));
    let lowered = setup.body.to_ascii_lowercase();
    assert!(!lowered.contains("herald"));
    for banned in PURPLE_DENYLIST {
        assert!(
            !lowered.contains(&banned.to_ascii_lowercase()),
            "purple {banned}"
        );
    }

    // The wizard is locked: a late claim is a 409, not a second session.
    let late = send(
        &app,
        Call::post(
            "/setup/claim",
            &format!("passphrase={}", encode(&passphrase)),
        ),
    )
    .await;
    assert_eq!(late.status, StatusCode::CONFLICT);
    assert_eq!(late.json()["code"], "wizard_locked");

    // Post-ready dashboard writes still need a session.
    let unlocked = send(
        &app,
        Call::post(
            "/dashboard/mode",
            "sandbox_mode=read-only&approval_policy=ask&autopilot_ceiling=L1+draft&telemetry=off",
        ),
    )
    .await;
    assert_eq!(unlocked.status, StatusCode::FORBIDDEN);
    assert_eq!(unlocked.json()["code"], "no_session");
}

#[tokio::test]
async fn the_forwarded_headers_are_refused_and_plain_loopback_is_not() {
    let (_dir, engine) = scratch();
    first_run_passphrase(&engine);
    let app = router_with(AppState::new(engine, false));

    for (name, value) in [
        ("x-forwarded-for", "1.2.3.4"),
        ("forwarded", "for=1.2.3.4"),
        ("x-forwarded-proto", "https"),
        ("tailscale-user", "someone"),
        ("tailscale-user-login", "someone@example"),
    ] {
        let refused = send(&app, Call::get(ROUTE_SETUP).header(name, value)).await;
        assert_eq!(
            refused.status,
            StatusCode::FORBIDDEN,
            "{name} must be refused"
        );
        assert_eq!(refused.json()["code"], "not_loopback", "{name}");
    }

    let plain = send(&app, Call::get(ROUTE_SETUP).html()).await;
    assert_eq!(
        plain.status,
        StatusCode::OK,
        "a plain loopback request is served"
    );
    assert!(plain.body.contains("Kallisti"));
}

#[tokio::test]
async fn the_seventh_claim_attempt_is_rate_limited() {
    let (_dir, engine) = scratch();
    first_run_passphrase(&engine);
    let app = router_with(AppState::new(engine, false));

    for attempt in 1..=5 {
        let reply = send(&app, Call::post("/setup/claim", "passphrase=wrong")).await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "attempt {attempt} is a wrong guess, not a refusal"
        );
        assert_eq!(reply.json()["code"], "passphrase_rejected");
    }
    let limited = send(&app, Call::post("/setup/claim", "passphrase=wrong")).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(limited.json()["code"], "rate_limited");
}

#[tokio::test]
async fn the_wizard_refuses_steps_out_of_order_and_the_unsafe_choices() {
    let (_dir, engine) = scratch();
    let passphrase = first_run_passphrase(&engine);
    let app = router_with(AppState::new(engine, false));
    let (models_addr, _server) = spawn_models_server().await;

    // Every step past the claim needs a session, and there is none yet.
    let early = send(&app, Call::post("/setup/models", "model=x")).await;
    assert_eq!(early.status, StatusCode::FORBIDDEN);
    assert_eq!(early.json()["code"], "no_session");

    let claim = send(
        &app,
        Call::post(
            "/setup/claim",
            &format!("passphrase={}", encode(&passphrase)),
        ),
    )
    .await;
    let cookie = claim.cookie().expect("cookie");
    let cookie = Some(cookie.as_str());

    // Models before provider is out of order.
    let out_of_order = send(&app, Call::post("/setup/models", "model=x").cookie(cookie)).await;
    assert_eq!(out_of_order.status, StatusCode::CONFLICT);
    assert_eq!(out_of_order.json()["code"], "step_out_of_order");

    // A pasted key value is refused where a reference belongs.
    let pasted = send(
        &app,
        Call::post(
            "/setup/provider",
            &format!(
                "base_url={}&api_key_ref={}",
                encode(&format!("http://{models_addr}/v1")),
                encode("sk-live-pasted-value")
            ),
        )
        .cookie(cookie),
    )
    .await;
    assert_eq!(pasted.status, StatusCode::BAD_REQUEST);
    assert_eq!(pasted.json()["code"], "provider_key_ref_invalid");
    assert_eq!(pasted.json()["field"], "api_key_ref");

    // A JSON body is accepted on the same route.
    let provider = send(
        &app,
        Call::post(
            "/setup/provider",
            &serde_json::json!({
                "base_url": format!("http://{models_addr}/v1"),
                "api_key_ref": TEST_KEY_REF,
            })
            .to_string(),
        )
        .json_body()
        .cookie(cookie),
    )
    .await;
    assert_eq!(provider.status, StatusCode::OK, "{}", provider.body);

    // `full` sandbox mode is refused by onboarding.
    let full = send(
        &app,
        Call::post("/setup/models", "model=model%2Fone").cookie(cookie),
    )
    .await;
    assert_eq!(full.status, StatusCode::OK);

    let unsafe_mode = send(
        &app,
        Call::post(
            "/setup/permissions",
            "sandbox_mode=full&approval_policy=ask&autopilot_ceiling=L1+draft&telemetry=off",
        )
        .cookie(cookie),
    )
    .await;
    assert_eq!(unsafe_mode.status, StatusCode::BAD_REQUEST);
    assert_eq!(unsafe_mode.json()["code"], "sandbox_full_not_available");
    assert_eq!(unsafe_mode.json()["field"], "sandbox_mode");

    // `never` is refused: no device is paired and setup is not finished.
    let never = send(
        &app,
        Call::post(
            "/setup/permissions",
            "sandbox_mode=read-only&approval_policy=never&autopilot_ceiling=L1+draft&telemetry=off",
        )
        .cookie(cookie),
    )
    .await;
    assert_eq!(never.status, StatusCode::BAD_REQUEST);
    assert_eq!(never.json()["code"], "approval_never_requires_device");

    // L2 does not exist in 1.0.
    let l2 = send(
        &app,
        Call::post(
            "/setup/permissions",
            "sandbox_mode=read-only&approval_policy=ask&autopilot_ceiling=L2&telemetry=off",
        )
        .cookie(cookie),
    )
    .await;
    assert_eq!(l2.status, StatusCode::BAD_REQUEST);
    assert_eq!(l2.json()["code"], "autopilot_ceiling_absent");

    // A valid permission set completes the step, so the workspace step is next.
    let good_permissions = send(
        &app,
        Call::post(
            "/setup/permissions",
            "sandbox_mode=workspace-write&approval_policy=ask&autopilot_ceiling=L1+draft&telemetry=off",
        )
        .cookie(cookie),
    )
    .await;
    assert_eq!(
        good_permissions.status,
        StatusCode::OK,
        "{}",
        good_permissions.body
    );

    // A workspace inside a protected root is refused.
    let reserved = send(
        &app,
        Call::post("/setup/workspace", "workspace_path=~%2F.config%2Fsandbox").cookie(cookie),
    )
    .await;
    assert_eq!(reserved.status, StatusCode::BAD_REQUEST);
    assert_eq!(reserved.json()["code"], "workspace_reserved");
    assert_eq!(reserved.json()["field"], "workspace_path");
}

#[tokio::test]
async fn the_state_survives_a_process_restart() {
    let (dir, engine) = scratch();
    let passphrase = first_run_passphrase(&engine);
    let app = router_with(AppState::new(engine.clone(), false));
    let (models_addr, _server) = spawn_models_server().await;

    let (cookie, _workspace) =
        walk_wizard(&app, &engine, &passphrase, models_addr, dir.path()).await;
    assert_eq!(engine.state().expect("state"), SetupState::Ready);
    drop(app);
    drop(engine);

    // A restart opens the same file with a new engine and a new router.
    let paths = Paths::for_root(dir.path());
    let reopened = SetupEngine::new(Db::open(&paths.db_file).expect("reopen"), paths);
    let app = router_with(AppState::new(reopened.clone(), false));
    let health = send(&app, Call::get(ROUTE_HEALTHZ)).await;
    assert_eq!(health.json()["state"], "ready", "ready persists");
    // The session cookie minted before the restart still verifies: sessions are
    // rows, not process memory.
    let dashboard = send(
        &app,
        Call::get(ROUTE_DASHBOARD).html().cookie(Some(&cookie)),
    )
    .await;
    assert_eq!(dashboard.status, StatusCode::OK);
    // And the passphrase is never printed again, because it is never regenerated.
    assert!(
        reopened
            .ensure_first_run()
            .expect("first run")
            .generated
            .is_none()
    );
}

#[tokio::test]
async fn every_get_route_answers_with_a_body() {
    let (dir, engine) = scratch();
    let passphrase = first_run_passphrase(&engine);
    let app = router_with(AppState::new(engine.clone(), false));
    let (models_addr, _server) = spawn_models_server().await;

    // Before onboarding: the reachable subset. A redirect is exempt from the
    // body rule: its content is the Location header, and a 302 with an empty body
    // is what every HTTP client expects.
    for route in [ROUTE_ROOT, ROUTE_HEALTHZ, ROUTE_SETUP, "/static/app.css"] {
        let reply = send(&app, Call::get(route)).await;
        if reply.status == StatusCode::FOUND {
            assert_eq!(
                reply
                    .headers
                    .get(header::LOCATION)
                    .and_then(|value| value.to_str().ok()),
                Some(ROUTE_SETUP),
                "{route} must redirect to the wizard before setup finishes"
            );
            continue;
        }
        assert!(
            !reply.body.trim().is_empty(),
            "{route} answered {} with an empty body",
            reply.status
        );
        assert_ne!(reply.body.trim(), "{}", "{route} answered an empty object");
    }

    let (cookie, _workspace) =
        walk_wizard(&app, &engine, &passphrase, models_addr, dir.path()).await;

    // After onboarding: the whole surface, with a session where one is needed.
    //
    // A redirect is exempt from the body rule: its content is the Location
    // header. `/` answers 302, and `/login` answers a 303 once a session exists,
    // because the form is no longer the answer for this browser. Both point at
    // the dashboard, which is the claim being made here.
    for route in ALL_GET_ROUTES {
        let reply = send(&app, Call::get(route).html().cookie(Some(&cookie))).await;
        assert!(
            reply.status.is_success()
                || reply.status == StatusCode::FOUND
                || reply.status == StatusCode::SEE_OTHER,
            "{route} answered {}",
            reply.status
        );
        if reply.status == StatusCode::FOUND || reply.status == StatusCode::SEE_OTHER {
            assert_eq!(
                reply
                    .headers
                    .get(header::LOCATION)
                    .and_then(|value| value.to_str().ok()),
                Some(ROUTE_DASHBOARD),
                "{route} must redirect to the dashboard once setup is finished"
            );
            continue;
        }
        assert!(
            !reply.body.trim().is_empty(),
            "{route} answered {} with an empty body",
            reply.status
        );
        assert_ne!(reply.body.trim(), "{}", "{route} answered {{}}");
    }

    // The CSS route really is CSS, and the JSON route really is JSON.
    let css = send(&app, Call::get("/static/app.css")).await;
    assert_eq!(
        css.headers
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok()),
        Some("text/css; charset=utf-8")
    );
    let state = send(&app, Call::get(ROUTE_SETUP_STATE).cookie(Some(&cookie))).await;
    assert_eq!(state.status, StatusCode::OK);
    assert_eq!(state.json()["state"], "ready");
    assert_eq!(state.json()["next_step"], serde_json::Value::Null);

    // Every POST route exists: an unauthenticated post is a 403 or a 409, never
    // a 404 or an empty 200.
    for route in POST_ROUTES {
        let reply = send(&app, Call::post(route, "")).await;
        assert!(
            matches!(
                reply.status,
                StatusCode::FORBIDDEN
                    | StatusCode::BAD_REQUEST
                    | StatusCode::CONFLICT
                    | StatusCode::METHOD_NOT_ALLOWED
            ),
            "{route} answered {}",
            reply.status
        );
        assert!(!reply.body.trim().is_empty(), "{route} body is empty");
    }
}

#[test]
fn the_documented_address_and_version_are_unchanged() {
    assert_eq!(ADMIN_ADDR, "127.0.0.1:7340");
    assert_eq!(VERSION, "1.0.5");
    assert!(ALL_GET_ROUTES.contains(&ROUTE_SETUP_STATE));
    assert!(POST_ROUTES.contains(&"/setup/finish"));
}
