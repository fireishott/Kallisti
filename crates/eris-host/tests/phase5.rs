//! P5 acceptance: SQLite shape, the healthz contract, and the error body.
//!
//! These drive the same router `eris serve` serves, in process, so they are the
//! same code path a `curl` hits. The two tests the phase brief names are
//! `migrations_are_idempotent_and_create_all_nine_tables` and
//! `no_route_returns_an_empty_200_body`.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Method, Request, StatusCode, header};
use eris_host::admin::{self, ADMIN_ADDR, GET_ROUTES, ROUTE_HEALTHZ, VERSION, router};
use eris_host::db::{Db, TABLES};
use eris_host::error::REQUEST_ID_HEADER;
use tower::ServiceExt;

const HEALTHZ_EXACT: &str = r#"{"status":"ok","version":"1.4.4","state":"setup","db":"ok"}"#;

/// Send one request through the router and return status, body text and headers.
async fn send(
    app: Router,
    method: Method,
    uri: &str,
) -> (StatusCode, String, axum::http::HeaderMap) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .expect("a valid request");
    let response = app
        .oneshot(request)
        .await
        .expect("the router is infallible");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 256 * 1024)
        .await
        .expect("a readable body");
    let text = String::from_utf8(bytes.to_vec()).expect("utf-8 body");
    (status, text, headers)
}

fn test_db() -> (tempfile::TempDir, Db) {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = Db::open(dir.path().join("eris.db")).expect("a fresh database opens");
    (dir, db)
}

#[test]
fn migrations_are_idempotent_and_create_all_nine_tables() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("state/eris.db");

    // First open: the file does not exist, so every migration runs.
    // (The name above is the P5 contract name; P6 added 0002 and a tenth table,
    // P7 added 0003 and an eleventh, P8 added 0004 which adds columns to `jobs`
    // and no table at all, 1.0.1 added 0005 and `chat_messages`, 0013 added
    // `projects` and the conversation column that files one, 0014 added
    // `subagents`, 0015 added the `admin` flag on `pairing_codes`, 0017 added
    // `schedules` and `schedule_runs`, and 0018 added `context_tokens`, so the
    // version and count below are the current ones and `TABLES` is the list.)
    let first = Db::open(&path).expect("first open");
    assert_eq!(first.schema_version().expect("version"), 18);
    assert_eq!(first.applied_migration_count().expect("count"), 18);

    let mut names = first.table_names().expect("table names");
    for expected in TABLES {
        assert!(
            names.iter().any(|actual| actual == expected),
            "missing table {expected}; found {names:?}"
        );
    }
    assert!(
        names.iter().any(|name| name == "schema_migrations"),
        "the embedded schema version table must exist: {names:?}"
    );
    names.retain(|name| !TABLES.contains(&name.as_str()) && name != "schema_migrations");
    assert!(names.is_empty(), "unexpected extra tables: {names:?}");

    assert_eq!(
        first.setup_state().expect("setup state").as_str(),
        "setup",
        "a fresh database is in setup"
    );
    drop(first);

    // Second open of the same file: no error, no duplicate version row, no
    // duplicate app_state row.
    let second = Db::open(&path).expect("second open must not fail");
    assert_eq!(second.schema_version().expect("version"), 18);
    assert_eq!(
        second.applied_migration_count().expect("count"),
        18,
        "re-running migrations must not add a version row"
    );
    assert_eq!(
        second.row_count("app_state").expect("rows"),
        1,
        "re-running migrations must not duplicate the seed row"
    );
    assert_eq!(
        second.table_names().expect("tables").len(),
        TABLES.len() + 1,
        "the documented tables plus schema_migrations, after two opens"
    );
}

#[test]
fn the_nine_tables_are_the_documented_ones() {
    let (_dir, db) = test_db();
    for table in TABLES {
        assert!(
            db.column_names(table).is_ok(),
            "{table} must exist and be introspectable"
        );
    }
    let columns = db.column_names("audit_log").expect("audit_log");
    for expected in ["occurred_at", "actor", "action", "detail"] {
        assert!(
            columns.iter().any(|column| column == expected),
            "audit_log is missing {expected}: {columns:?}"
        );
    }
    let settings = db.column_names("settings").expect("settings");
    for expected in ["key", "value", "created_at", "updated_at"] {
        assert!(
            settings.iter().any(|column| column == expected),
            "settings is missing {expected}: {settings:?}"
        );
    }
    let state = db.column_names("app_state").expect("app_state");
    assert!(state.iter().any(|column| column == "setup_state"));
}

#[test]
fn the_credential_table_has_no_plaintext_column() {
    let (_dir, db) = test_db();
    let columns = db
        .column_names("admin_credential")
        .expect("admin_credential");
    assert!(
        columns.iter().any(|column| column == "password_hash"),
        "the hash column must exist: {columns:?}"
    );
    for forbidden in ["password", "passwd", "plaintext", "secret", "code"] {
        assert!(
            !columns.iter().any(|column| column == forbidden),
            "admin_credential must not hold a plaintext {forbidden} column: {columns:?}"
        );
    }
    let pairing = db.column_names("pairing_codes").expect("pairing_codes");
    assert!(
        pairing.iter().any(|column| column == "code_hash"),
        "pairing codes are stored as verifiers: {pairing:?}"
    );
    assert!(!pairing.iter().any(|column| column == "code"));
}

#[test]
fn the_database_is_in_wal_mode_with_foreign_keys_a_busy_timeout_and_full_sync() {
    let (_dir, db) = test_db();
    assert_eq!(
        db.journal_mode().expect("journal mode"),
        "wal",
        "WAL is not optional: a reader must not block the writer"
    );
    assert!(
        db.foreign_keys_enabled().expect("foreign keys"),
        "foreign_keys must be on for the cascade to be real"
    );
    assert!(
        db.busy_timeout_ms().expect("busy timeout") >= 1000,
        "a writer must wait rather than fail immediately"
    );
    assert_eq!(
        db.synchronous_level().expect("synchronous"),
        2,
        "ADR 0002 chose synchronous=FULL for a daemon that acks a turn"
    );
    assert_eq!(db.quick_check().expect("quick check"), "ok");
}

#[test]
fn the_setup_state_is_read_from_the_database() {
    let (_dir, db) = test_db();
    assert_eq!(db.setup_state().expect("state").as_str(), "setup");
    db.set_setup_state(eris_host::db::SetupState::Ready)
        .expect("write");
    assert_eq!(
        db.setup_state().expect("state").as_str(),
        "ready",
        "healthz reports what app_state holds, not a constant"
    );
}

#[tokio::test]
async fn healthz_returns_exactly_the_documented_body() {
    let (_dir, db) = test_db();
    let (status, body, headers) = send(router(db), Method::GET, ROUTE_HEALTHZ).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body, HEALTHZ_EXACT,
        "the health body is exact: no trailing prose, keys in wire order"
    );
    assert_eq!(
        headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok()),
        Some("application/json")
    );
    assert!(
        headers.get(REQUEST_ID_HEADER).is_some(),
        "every response carries a request id"
    );

    let parsed: serde_json::Value = serde_json::from_str(&body).expect("healthz is json");
    assert_eq!(parsed["state"], "setup");
    assert_eq!(parsed["db"], "ok");
    assert_eq!(parsed["version"], "1.4.4");
    assert_eq!(VERSION, "1.4.4");
}

#[tokio::test]
async fn healthz_reports_ready_once_the_state_moves() {
    let (_dir, db) = test_db();
    db.set_setup_state(eris_host::db::SetupState::Ready)
        .expect("write");
    let (status, body, _) = send(router(db), Method::GET, ROUTE_HEALTHZ).await;
    assert_eq!(status, StatusCode::OK);
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["state"], "ready");
}

/// The brief's rule: no handler may answer 200 with an empty body or `{}`.
#[tokio::test]
async fn no_route_returns_an_empty_200_body() {
    let (_dir, db) = test_db();
    assert!(!GET_ROUTES.is_empty(), "the route list must not be empty");

    for route in GET_ROUTES {
        let (status, body, _) = send(router(db.clone()), Method::GET, route).await;
        assert_eq!(status, StatusCode::OK, "{route} should answer 200");
        assert!(
            !body.trim().is_empty(),
            "{route} answered 200 with an empty body"
        );
        assert_ne!(
            body.trim(),
            "{}",
            "{route} answered 200 with an empty object"
        );
        let parsed: serde_json::Value = serde_json::from_str(&body)
            .unwrap_or_else(|error| panic!("{route} body is not json: {error}: {body}"));
        assert!(
            parsed.as_object().is_some_and(|object| !object.is_empty()),
            "{route} must answer a non-empty JSON object"
        );
    }
}

#[tokio::test]
async fn an_unknown_route_is_a_typed_404_not_an_empty_body() {
    let (_dir, db) = test_db();
    let (status, body, headers) = send(router(db), Method::GET, "/does-not-exist").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["code"], "not_found");
    assert!(parsed["message"].as_str().is_some_and(|m| !m.is_empty()));
    assert!(parsed["request_id"].as_str().is_some_and(|r| !r.is_empty()));
    assert!(headers.get(REQUEST_ID_HEADER).is_some());
}

#[tokio::test]
async fn the_wrong_method_is_a_typed_405() {
    let (_dir, db) = test_db();
    let (status, body, _) = send(router(db), Method::POST, ROUTE_HEALTHZ).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("json");
    assert_eq!(parsed["code"], "method_not_allowed");
}

#[tokio::test]
async fn no_response_carries_an_empty_body() {
    let (_dir, db) = test_db();
    for (method, uri) in [
        (Method::GET, ROUTE_HEALTHZ),
        (Method::GET, "/nope"),
        (Method::GET, "/healthz/extra"),
        (Method::POST, ROUTE_HEALTHZ),
        (Method::DELETE, "/nope"),
    ] {
        let (_status, body, _) = send(router(db.clone()), method.clone(), uri).await;
        assert!(
            !body.trim().is_empty(),
            "{method} {uri} answered with an empty body"
        );
        assert_ne!(body.trim(), "{}", "{method} {uri} answered {{}}");
    }
}

#[test]
fn the_admin_address_is_the_literal_loopback_port() {
    assert_eq!(ADMIN_ADDR, "127.0.0.1:7340");
    let address: std::net::SocketAddr = ADMIN_ADDR.parse().expect("literal socket address");
    assert!(address.ip().is_loopback());
    assert_eq!(address.port(), 7340);
    // The address is a const in the module, not a field on the config types.
    let paths = eris_host::config::Paths::from_home().expect("HOME");
    let loaded_toml = eris_host::config::TomlConfig::defaults();
    let _ = (paths, loaded_toml, admin::GET_ROUTES);
}

/// One request over a real TCP socket, so the test covers the listener and not
/// just the router.
async fn http_get_once(addr: SocketAddr, path: &str) -> (u16, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect to the listener");
    let request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write the request");

    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut raw))
        .await
        .expect("the response arrives within the timeout")
        .expect("read the response");

    let text = String::from_utf8(raw).expect("a utf-8 response");
    let (head, body) = text.split_once("\r\n\r\n").expect("a header block");
    let status: u16 = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("a status line");
    let chunked = head
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked");
    (
        status,
        if chunked {
            dechunk(body)
        } else {
            body.to_owned()
        },
    )
}

/// Minimal HTTP/1.1 chunk decoder. hyper usually sends `content-length` for a
/// fixed-size JSON body, but the test must not depend on that.
fn dechunk(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some((size_line, remainder)) = rest.split_once("\r\n") {
        let size = usize::from_str_radix(size_line.trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        if remainder.len() < size {
            out.push_str(remainder);
            break;
        }
        out.push_str(&remainder[..size]);
        rest = remainder[size..]
            .strip_prefix("\r\n")
            .unwrap_or(&remainder[size..]);
    }
    out
}

#[tokio::test]
async fn the_served_socket_answers_healthz_with_the_exact_body() {
    let (dir, db) = test_db();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("an ephemeral loopback listener");
    let addr = listener.local_addr().expect("a local address");
    assert!(addr.ip().is_loopback(), "{addr} must be loopback");

    let state = eris_host::admin::AppState::new(
        eris_host::onboarding::SetupEngine::new(db, eris_host::config::Paths::for_root(dir.path())),
        false,
    );
    let server = tokio::spawn(eris_host::admin::serve_on(listener, state));
    let (status, body) = http_get_once(addr, ROUTE_HEALTHZ).await;

    assert_eq!(status, 200);
    assert_eq!(body, HEALTHZ_EXACT);
    server.abort();
}
