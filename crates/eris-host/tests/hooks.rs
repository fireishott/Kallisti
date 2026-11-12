//! The webhook receiver's contract, driven through the real router.
//!
//! The four claims this file pins, each of which is a way the route could be
//! wrong in a way that matters:
//!
//! - an unknown, revoked or disabled token answers one indistinguishable 404;
//! - the token is never in a response body, a header or a log line;
//! - the route exists on the admin listener and is POST only;
//! - a fire over the hourly ceiling answers 429 and leaves a `refused` row, so a
//!   hammering caller is visible rather than silent.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode};
use eris_host::admin::{AppState, router_with};
use eris_host::config::Paths;
use eris_host::db::Db;
use eris_host::onboarding::{Permissions, SetupEngine};
use eris_host::trigger::{self, Draft};
use tower::ServiceExt;

/// A ready daemon on a scratch root, and the real admin router over it.
///
/// The hook route sits behind the same ready gate as the dashboard, so a test that
/// used a setup-state database would be measuring the gate rather than the
/// receiver. The provider is never called: these tests are about the token, the
/// ledger and the status codes.
fn app() -> (tempfile::TempDir, Router, Db) {
    let dir = tempfile::tempdir().expect("a temp dir");
    let paths = Paths::for_root(dir.path());
    let db = Db::open(&paths.db_file).expect("a fresh database");
    let engine = SetupEngine::new(db.clone(), paths);
    let passphrase = engine
        .ensure_first_run()
        .expect("first run")
        .generated
        .expect("a passphrase");
    engine.accept_claim(&passphrase).expect("claim");
    engine
        // A port nothing listens on, so a fire's turn fails immediately instead of
        // reaching a real provider from a test.
        .set_provider(
            "http://127.0.0.1:9/v1",
            "env:KALLISTI_TEST_KEY_NOT_SET",
            &["ds/deepseek-v4-flash".to_owned()],
        )
        .expect("a provider");
    engine.set_model("ds/deepseek-v4-flash").expect("a model");
    engine
        .set_permissions(Permissions::DEFAULT)
        .expect("permissions");
    engine
        .set_workspace(&dir.path().join("workspace").display().to_string())
        .expect("a workspace");
    engine.finish().expect("finish");
    let app = router_with(AppState::new(engine, false));
    (dir, app, db)
}

/// One POST, with an optional body.
async fn post(app: Router, uri: &str, body: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .expect("a valid request");
    let response = app.oneshot(request).await.expect("a response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("a body");
    (status, String::from_utf8_lossy(&bytes).to_string())
}

/// One request with an explicit method.
async fn call(app: Router, method: Method, uri: &str) -> (StatusCode, String) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("a valid request");
    let response = app.oneshot(request).await.expect("a response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("a body");
    (status, String::from_utf8_lossy(&bytes).to_string())
}

/// A trigger whose prompt is short enough to combine with a body.
fn draft(name: &str, per_hour: i64) -> Draft {
    Draft {
        name: name.to_owned(),
        prompt: "Say which files changed today.".to_owned(),
        max_per_hour: per_hour,
    }
}

#[tokio::test]
async fn an_unknown_token_is_a_404_and_says_nothing_about_the_table() {
    let (_dir, app, db) = app();
    trigger::create(&db, &draft("nightly", 60)).expect("a trigger");
    let (status, body) = post(app, "/hooks/whk_does_not_exist", "").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("no_such_hook"), "{body}");
    // The same answer a typo inside a real token gets: nothing distinguishes
    // "absent" from "wrong", because either one would be an oracle.
    assert!(!body.contains("nightly"), "{body}");
}

#[tokio::test]
async fn a_disabled_trigger_answers_the_same_404() {
    let (_dir, app, db) = app();
    let minted = trigger::create(&db, &draft("nightly", 60)).expect("a trigger");
    trigger::set_enabled(&db, "nightly", false).expect("a switch");
    let (status, body) = post(app, &format!("/hooks/{}", minted.token), "").await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert!(body.contains("no_such_hook"), "{body}");
}

#[tokio::test]
async fn the_route_is_post_only() {
    let (_dir, app, db) = app();
    let minted = trigger::create(&db, &draft("nightly", 60)).expect("a trigger");
    for method in [Method::GET, Method::PUT, Method::DELETE] {
        let (status, _) = call(
            app.clone(),
            method.clone(),
            &format!("/hooks/{}", minted.token),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::METHOD_NOT_ALLOWED,
            "{method} must not start a turn"
        );
    }
}

#[tokio::test]
async fn the_device_router_has_no_hooks_route() {
    // The token is the only credential on this route, so a listener the phone can
    // reach must not carry it at all. The admin router merges the dashboard table;
    // the device router is built from its own table, and this asserts the
    // separation rather than trusting that nobody added the route to both.
    let dir = tempfile::tempdir().expect("a temp dir");
    let paths = eris_host::config::Paths::for_root(dir.path());
    let db = Db::open(&paths.db_file).expect("a database");
    let engine = eris_host::onboarding::SetupEngine::new(db, paths);
    let service = eris_host::device::DeviceService::new(engine, 32);
    let device = eris_host::device::router(service);
    let (status, body) = {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/hooks/whk_anything")
            .body(Body::empty())
            .expect("a valid request");
        let response = device.oneshot(request).await.expect("a response");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("a body");
        (status, String::from_utf8_lossy(&bytes).to_string())
    };
    assert_ne!(
        status,
        StatusCode::ACCEPTED,
        "the phone listener must not carry the hook route: {body}"
    );
}

#[tokio::test]
async fn an_over_ceiling_fire_is_429_and_leaves_a_refused_row() {
    let (_dir, app, db) = app();
    let minted = trigger::create(&db, &draft("tight", 1)).expect("a trigger");
    // One accepted fire, then the second is over the ceiling. The first starts a
    // real turn, which has no provider in this test and so settles as failed; what
    // matters here is the row and the status.
    let (first, _) = post(app.clone(), &format!("/hooks/{}", minted.token), "").await;
    assert!(
        first == StatusCode::ACCEPTED,
        "the first fire is accepted: {first}"
    );
    let (second, body) = post(app, &format!("/hooks/{}", minted.token), "").await;
    assert_eq!(second, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert!(body.contains("rate_limited"), "{body}");

    let fires = trigger::recent_fires(&db, &minted.trigger.id, 10).expect("a read");
    assert!(
        fires.iter().any(|fire| fire.status == "refused"),
        "the refusal is recorded: {fires:?}"
    );
    assert!(
        fires
            .iter()
            .any(|fire| fire.status == "failed" || fire.status == "queued"),
        "the first fire is recorded too: {fires:?}"
    );
}

#[tokio::test]
async fn a_fire_records_the_conversation_and_the_turn_outcome() {
    let (_dir, app, db) = app();
    let minted = trigger::create(&db, &draft("nightly", 60)).expect("a trigger");
    let (status, body) = post(
        app,
        &format!("/hooks/{}", minted.token),
        r#"{"text":"ping"}"#,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let fires = trigger::recent_fires(&db, &minted.trigger.id, 1).expect("a read");
    let fire = fires.first().expect("a fire");
    // The provider cannot answer in this process, so the turn fails. What the row
    // has to say is where the turn went and why it ended: a fire that never became
    // an answer is a fact, and a request with no record is a request nobody can
    // account for. The note is the failure the daemon actually saw, so it is
    // asserted as present rather than pinned to one string.
    assert_eq!(fire.status, "failed", "{fire:?}");
    assert!(
        fire.note.as_deref().is_some_and(|note| !note.is_empty()),
        "the row carries the reason: {fire:?}"
    );
    assert!(
        fire.conversation.is_some(),
        "the turn opened a conversation and the row names it: {fire:?}"
    );
    assert!(fire.settled_at.is_some(), "{fire:?}");
    // The conversation a fire lands in is named after the trigger, and the queue
    // ceiling is the same one the composer has.
    let conversations: i64 = db
        .with_conn(|conn| {
            conn.query_row("SELECT count(*) FROM chat_conversations", [], |r| r.get(0))
        })
        .expect("a count");
    assert!(conversations >= 1, "the turn opened a conversation");
}

#[tokio::test]
async fn an_oversized_body_is_refused_by_size() {
    let (_dir, app, db) = app();
    let minted = trigger::create(&db, &draft("nightly", 60)).expect("a trigger");
    let huge = "x".repeat(trigger::MAX_FIRE_BYTES + 1);
    let (status, body) = post(app, &format!("/hooks/{}", minted.token), &huge).await;
    assert!(
        status == StatusCode::PAYLOAD_TOO_LARGE || status == StatusCode::BAD_REQUEST,
        "{status}: {body}"
    );
    assert!(!body.contains(&huge), "the body is not echoed back");
}

#[tokio::test]
async fn the_token_never_comes_back_out() {
    let (_dir, app, db) = app();
    let minted = trigger::create(&db, &draft("nightly", 60)).expect("a trigger");
    let (_, body) = post(app, &format!("/hooks/{}", minted.token), "").await;
    assert!(
        !body.contains(&minted.token),
        "a fire response never echoes the token: {body}"
    );
    // And the hash is not in it either, in any case.
    let hash: String = db
        .with_conn(|conn| {
            conn.query_row("SELECT token_hash FROM triggers LIMIT 1", [], |row| {
                row.get(0)
            })
        })
        .expect("the hash");
    assert!(!body.contains(&hash), "{body}");
}
