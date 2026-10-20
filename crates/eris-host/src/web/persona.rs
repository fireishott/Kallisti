//! The Assistant identity group on the Settings surface.
//!
//! Four claims:
//!
//! - the page shows the persona text the daemon would actually send, and how
//!   big it is;
//! - a save writes the file at mode 0644 and audits the character count and the
//!   enabled flag, never the text;
//! - a persona over the cap is refused with the count and the cap, and nothing
//!   is written;
//! - an empty persona that is still enabled is refused, because "enabled and
//!   blank" is a setting that silently does nothing.

use axum::http::StatusCode;

use crate::admin::{AppState, ROUTE_SETTINGS, router_with};
use crate::persona::{MAX_PERSONA_BYTES, SETTING_PERSONA_ENABLED};
use crate::web::templates::SettingsView;

use super::harness::{Call, PURPLE_DENYLIST, block_on, encode, ready_app, ready_engine, send};

/// The persona post.
const PERSONA_POST: &str = "/dashboard/settings/persona";

/// A persona form body. `enabled` is `true`, `false`, or `None` for unchecked.
fn persona_body(text: &str, enabled: Option<bool>) -> String {
    let mut body = format!("persona_text={}", encode(text));
    if enabled == Some(true) {
        body.push_str("&persona_enabled=true");
    }
    body
}

#[test]
fn the_settings_page_shows_the_persona_and_its_size() {
    let (dir, engine, app, cookie) = ready_app();
    let path = crate::config::Paths::for_root(dir.path()).persona_file();
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("config dir");
    let text = "You are Kallisti, Sam's chief of staff.\n";
    std::fs::write(&path, text).expect("write");

    let reply = block_on(send(
        &app,
        Call::get(ROUTE_SETTINGS).html().cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let page = &reply.body;

    // The fourth group, its own form and its own route.
    assert!(
        page.contains(r#"action="/dashboard/settings/persona""#),
        "the persona form is missing: {page}"
    );
    assert!(page.contains(r#"id="persona_text""#), "{page}");
    assert!(page.contains(r#"id="persona_enabled""#), "{page}");
    assert!(
        page.contains(r#"<label for="persona_text">"#),
        "the textarea has no label: {page}"
    );

    // The text that would be sent is the text on the page.
    assert!(page.contains("You are Kallisti, Sam"), "{page}");

    // The size, and the plain-English consequence.
    assert!(
        page.contains(&format!("{} characters", text.len())),
        "the size is stated: {page}"
    );
    assert!(
        page.to_ascii_lowercase()
            .contains("every turn sends this text to the model as the system prompt"),
        "the consequence is stated: {page}"
    );

    // House style holds on this page too.
    let lowered = page.to_ascii_lowercase();
    assert!(!lowered.contains("herald"), "{page}");
    for banned in PURPLE_DENYLIST {
        assert!(!lowered.contains(&banned.to_ascii_lowercase()), "{banned}");
    }
    assert!(!page.contains('\u{2014}'), "no em dash reaches the page");
    let _ = engine;
}

#[test]
fn saving_the_persona_writes_the_file_and_audits_the_size_only() {
    let (dir, engine, app, cookie) = ready_app();
    let text = "You are Kallisti, Sam's chief of staff.\nShort lines only.";
    let reply = block_on(send(
        &app,
        Call::post(PERSONA_POST, &persona_body(text, Some(true)))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(
        reply.status,
        StatusCode::SEE_OTHER,
        "a successful save redirects: {}",
        reply.body
    );
    let location = reply.location().unwrap_or_default();
    assert!(
        location.contains("saved=persona"),
        "the notice names the group: {location}"
    );

    // The file exists, holds exactly the text, and is world-readable on purpose:
    // it is not a secret.
    let path = crate::config::Paths::for_root(dir.path()).persona_file();
    let on_disk = std::fs::read_to_string(&path).expect("the persona file was written");
    assert_eq!(on_disk, text);
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o644, "the persona is 0644, not 0600");

    // The toggle is stored, so the checkbox reads back.
    assert_eq!(
        engine.setting(SETTING_PERSONA_ENABLED).expect("read"),
        Some("true".to_owned())
    );

    // The audit row records the size and the flag, and never the text.
    let rows = super::harness::audit_rows(engine.db());
    let row = rows
        .iter()
        .find(|row| row.action == "settings.persona")
        .unwrap_or_else(|| panic!("no settings.persona audit row: {rows:?}"));
    let detail = row.detail.clone().unwrap_or_default();
    assert!(
        detail.contains(&text.len().to_string()),
        "the count is audited: {detail}"
    );
    assert!(detail.contains("enabled=true"), "{detail}");
    assert!(
        !detail.contains("Kallisti"),
        "the persona text never reaches the audit log: {detail}"
    );
    assert_eq!(row.actor, "loopback");

    // And the page reads the file back, with the notice.
    let page = block_on(send(
        &app,
        Call::get("/dashboard/settings?saved=persona")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains("assistant identity saved"),
        "{}",
        page.body
    );
    assert!(
        page.body.contains("You are Kallisti, Sam"),
        "the textarea holds the saved text: {}",
        page.body
    );
}

#[test]
fn an_oversized_persona_is_refused() {
    let (dir, engine, app, cookie) = ready_app();
    let base = crate::config::Paths::for_root(dir.path()).persona_file();
    std::fs::create_dir_all(base.parent().expect("a parent")).expect("config dir");
    std::fs::write(&base, "existing persona").expect("write");

    let huge = "x".repeat(MAX_PERSONA_BYTES + 1);
    let reply = block_on(send(
        &app,
        Call::post(PERSONA_POST, &persona_body(&huge, Some(true)))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(
        reply.status,
        StatusCode::BAD_REQUEST,
        "over the cap must be refused: {}",
        reply.body
    );
    assert!(
        reply.body.contains("persona_too_large"),
        "the refusal is named: {}",
        reply.body
    );
    let body_text = &reply.body;
    assert!(
        body_text.contains(&(MAX_PERSONA_BYTES + 1).to_string()),
        "the count is stated: {body_text}"
    );
    assert!(
        body_text.contains(&MAX_PERSONA_BYTES.to_string()),
        "the cap is stated: {body_text}"
    );

    // Nothing was overwritten, and nothing was audited.
    assert_eq!(
        std::fs::read_to_string(&base).expect("read"),
        "existing persona"
    );
    let rows = super::harness::audit_rows(engine.db());
    assert!(
        !rows.iter().any(|row| row.action == "settings.persona"),
        "{rows:?}"
    );

    // The API client gets the field and the code, not a page.
    let api = block_on(send(
        &app,
        Call::post(PERSONA_POST, &persona_body(&huge, Some(true))).cookie(Some(&cookie)),
    ));
    assert_eq!(api.status, StatusCode::BAD_REQUEST);
    assert_eq!(api.json()["field"], "persona_text");
    assert_eq!(api.json()["code"], "persona_too_large");
}

#[test]
fn an_empty_enabled_persona_is_refused() {
    let (dir, engine, app, cookie) = ready_app();

    let reply = block_on(send(
        &app,
        Call::post(PERSONA_POST, &persona_body("   \n  ", Some(true)))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(
        reply.status,
        StatusCode::BAD_REQUEST,
        "an enabled empty persona is a dead setting: {}",
        reply.body
    );
    assert!(
        reply.body.contains("persona_empty"),
        "the refusal is named: {}",
        reply.body
    );
    assert!(
        reply.body.to_ascii_lowercase().contains("disable"),
        "the refusal says what to do instead: {}",
        reply.body
    );

    // Nothing was written: the enabled row and the file are both absent.
    assert_eq!(engine.setting(SETTING_PERSONA_ENABLED).expect("read"), None);
    assert!(
        !crate::config::Paths::for_root(dir.path())
            .persona_file()
            .exists()
    );

    // Disabling with empty text is allowed, and clears the file.
    let off = block_on(send(
        &app,
        Call::post(PERSONA_POST, &persona_body("", None))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(off.status, StatusCode::SEE_OTHER, "{}", off.body);
    assert_eq!(
        engine.setting(SETTING_PERSONA_ENABLED).expect("read"),
        Some("false".to_owned())
    );
}

/// The view is what the handler renders from: this pins the two fields it reads
/// so a rename cannot silently empty the textarea.
#[test]
fn the_view_carries_the_persona_and_its_length() {
    let mut view = SettingsView::new(crate::db::SetupState::Ready, "test");
    view.persona_text = "abc".to_owned();
    view.persona_enabled = true;
    assert_eq!(view.persona_characters(), 3);
    view.persona_text = "abcd".to_owned();
    assert_eq!(view.persona_characters(), 4);
}

/// A ready engine whose persona path is the scratch root, for the two tests
/// above that need the router but no persona file.
#[allow(dead_code)]
fn app_without_persona() -> (tempfile::TempDir, axum::Router, String) {
    let (dir, engine, passphrase) = ready_engine();
    let app = router_with(AppState::new(engine, false));
    let cookie = super::harness::sign_in(&app, &passphrase);
    (dir, app, cookie)
}
