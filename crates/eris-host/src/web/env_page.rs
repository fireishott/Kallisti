//! The environment page: what it lists, and what it refuses to say.
//!
//! Three claims a reviewer would reject the feature for:
//!
//! - the list carries names and never a value, so a page, a cache or a
//!   screenshot cannot carry a key;
//! - a saved value lands in `eris.env` at mode 0600, quoted so a `#` or a space
//!   survives the file, and resolves in the run that wrote it without a restart;
//! - a removed value leaves the file *and* the live overlay, so the next call
//!   fails instead of authenticating with a credential the file no longer lists.

use axum::http::StatusCode;

use crate::admin::{ROUTE_ENV, ROUTE_ENV_DELETE};

use super::harness::{Call, block_on, encode, ready_app, send};

/// The environment file inside one test's own root.
fn env_file(dir: &tempfile::TempDir) -> std::path::PathBuf {
    crate::config::Paths::for_root(dir.path()).env_file
}

/// Write a line the way an operator would have before this page existed.
fn seed_env_file(dir: &tempfile::TempDir, body: &str) {
    let path = env_file(dir);
    std::fs::create_dir_all(path.parent().expect("a config dir")).expect("config dir");
    std::fs::write(&path, body).expect("eris.env");
}

#[test]
fn the_page_lists_names_and_never_a_value() {
    let (dir, engine, app, cookie) = ready_app();
    seed_env_file(
        &dir,
        "ANTHROPIC_API_KEY=\"sk-live-do-not-print\"\nMY_USERNAME=someone\n",
    );

    let reply = block_on(send(
        &app,
        Call::get(ROUTE_ENV).html().cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let page = &reply.body;

    assert!(page.contains("ANTHROPIC_API_KEY"), "{page}");
    assert!(page.contains("MY_USERNAME"), "{page}");
    assert!(
        page.contains("set"),
        "the value column says a value is set: {page}"
    );

    // The value is nowhere in the response, in either spelling.
    assert!(
        !page.contains("sk-live-do-not-print"),
        "the page echoed a value: {page}"
    );
    assert!(!page.contains("someone"), "the page echoed a value: {page}");

    // The row carries the daemon's own environment too, names only.
    let _ = engine;
}

#[test]
fn a_saved_value_is_sealed_quoted_and_live_in_this_run() {
    let (dir, _engine, app, cookie) = ready_app();
    seed_env_file(&dir, "EXISTING=1\n");

    let name = "ENVTEST_SAVED_9F3A";
    let value = "a b#c\"d";
    let body = format!(
        "env_name={}&env_value={}",
        encode(name),
        encode(value)
    );
    let reply = block_on(send(
        &app,
        Call::post(ROUTE_ENV, &body).html().cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let location = reply.location().unwrap_or_default();
    assert!(location.contains("saved="), "{location}");

    // The file is 0600, keeps the line that was there, and holds the new one
    // quoted so the space and the `#` survive a systemd read.
    let path = env_file(&dir);
    let text = std::fs::read_to_string(&path).expect("eris.env was written");
    assert!(text.contains("EXISTING=1"), "{text}");
    assert!(
        text.contains(&format!("{name}=\"{}\"", "a b#c\\\"d")),
        "the value is quoted and escaped: {text}"
    );
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "eris.env stays 0600");

    // The overlay carries the same value, so a provider call in this run
    // resolves it without a restart.
    assert_eq!(
        crate::oauth::credential::env_value(name),
        Some(value.to_owned()),
        "a saved value is live for this run"
    );
    crate::oauth::credential::forget_env(name);
}

#[test]
fn removing_a_variable_empties_the_file_and_the_overlay() {
    let (dir, _engine, app, cookie) = ready_app();
    let name = "ENVTEST_REMOVED_A17B";
    seed_env_file(&dir, &format!("KEEPME=1\n{name}=\"gone-soon\"\n"));
    crate::oauth::credential::remember_env(name, "gone-soon");

    let body = format!("env_name={}", encode(name));
    let reply = block_on(send(
        &app,
        Call::post(ROUTE_ENV_DELETE, &body).html().cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert!(
        reply.location().unwrap_or_default().contains("deleted="),
        "{:?}",
        reply.location()
    );

    let text = std::fs::read_to_string(env_file(&dir)).expect("eris.env");
    assert!(!text.contains(name), "{text}");
    assert!(text.contains("KEEPME=1"), "{text}");
    assert_eq!(
        crate::oauth::credential::env_value(name),
        None,
        "the overlay forgot the removed value"
    );
}

#[test]
fn the_daemons_own_names_are_refused() {
    let (dir, _engine, app, cookie) = ready_app();
    seed_env_file(&dir, "EXISTING=1\n");

    for name in ["PATH", "HOME", "KALLISTI_API_KEY_REF", "LD_PRELOAD", "1BAD"] {
        let body = format!("env_name={}&env_value=value", encode(name));
        let reply = block_on(send(
            &app,
            Call::post(ROUTE_ENV, &body).html().cookie(Some(&cookie)),
        ));
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{name} must be refused: {}",
            reply.body
        );
        assert!(
            reply.body.contains("env_name"),
            "the refusal names the field: {}",
            reply.body
        );
    }

    // Nothing was written by any of those.
    let text = std::fs::read_to_string(env_file(&dir)).expect("eris.env");
    assert_eq!(text, "EXISTING=1\n", "{text}");
}

#[test]
fn a_page_without_a_session_is_refused() {
    let (_dir, _engine, app, _cookie) = ready_app();
    let reply = block_on(send(&app, Call::get(ROUTE_ENV).html()));
    assert!(
        reply.status == StatusCode::FORBIDDEN || reply.status == StatusCode::SEE_OTHER,
        "{}",
        reply.status
    );
}
