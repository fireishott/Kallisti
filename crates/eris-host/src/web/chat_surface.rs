//! The chat surface and the wallpaper, against the real router.
//!
//! What this suite pins:
//!
//! - the chat page is behind the same admin session as every other dashboard
//!   page, and an unauthenticated request gets the documented refusal rather than
//!   a page;
//! - an empty message is a named refusal and leaves no row behind;
//! - a daemon with no provider refuses the turn *before* the transcript grows, so
//!   a misconfigured host does not accumulate questions that can never be
//!   answered;
//! - a settled transcript renders both speakers, keeps code fences as code, and
//!   clears on demand;
//! - the wallpaper choice round-trips, an unknown one is refused by name, and
//!   choosing `custom` with no upload is refused instead of painting a hole;
//! - an uploaded image is served with the type its bytes actually are, and a
//!   direct request for one that does not exist is a 404 rather than a 500.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

use crate::admin::{ROUTE_CHAT, ROUTE_CHAT_CLEAR, ROUTE_CHAT_SEND, ROUTE_SETTINGS};
use crate::web::chat::{self, CONVERSATION_ID, Role, State};
use crate::web::templates::SettingsView;
use crate::web::wallpaper;

use super::harness::{Call, block_on, encode, ready_app, send};

/// One wallpaper save body.
fn wallpaper_body(value: &str) -> String {
    format!("wallpaper={}", encode(value))
}

/// Fetch one path and keep the bytes, because an image response is not UTF-8 and
/// the shared harness decodes its body as text.
fn get_bytes(app: &axum::Router, uri: &str) -> (StatusCode, String, Vec<u8>) {
    let response = block_on(
        app.clone().oneshot(
            Request::builder()
                .uri(uri)
                .body(Body::empty())
                .expect("a valid request"),
        ),
    )
    .expect("the router is infallible");
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let bytes = block_on(to_bytes(response.into_body(), 1024 * 1024)).expect("a body");
    (status, content_type, bytes.to_vec())
}

/// A PNG, by its first eight bytes, so the sniffer and the file path are
/// exercised with something that is really a PNG.
fn a_png() -> Vec<u8> {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    bytes.extend_from_slice(&[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
    bytes
}

#[test]
fn the_chat_page_needs_a_session_and_renders_the_default_wallpaper() {
    let (_dir, _engine, app, cookie) = ready_app();

    let refused = block_on(send(&app, Call::get(ROUTE_CHAT)));
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    assert_eq!(refused.json()["code"], "no_session");

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(
        body.contains("/static/wallpaper/kallisti.svg"),
        "the default backdrop is painted: {body}"
    );
    assert!(
        body.contains(r#"id="composer-form""#),
        "the composer is a real form: {body}"
    );
    assert!(
        body.contains(r#"action="/dashboard/chat/send""#),
        "the form posts to the send route: {body}"
    );
    assert!(
        body.contains("Nothing said yet"),
        "an empty transcript says so: {body}"
    );
    assert!(
        !body.contains(r#"action="/dashboard/chat/clear""#),
        "there is nothing to clear yet: {body}"
    );
    assert_eq!(
        body.matches("<script").count(),
        1,
        "one inline script, as the shell sweep documents"
    );
}

#[test]
fn an_empty_message_is_refused_by_name_and_writes_nothing() {
    let (_dir, engine, app, cookie) = ready_app();
    let reply = block_on(send(
        &app,
        Call::post(ROUTE_CHAT_SEND, "message=%20%20%20").cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert_eq!(reply.json()["code"], "message_invalid");
    assert_eq!(reply.json()["field"], "message");
    assert_eq!(
        chat::count(engine.db(), CONVERSATION_ID).unwrap(),
        0,
        "a refused message leaves the transcript empty"
    );
}

#[test]
fn a_turn_without_a_provider_is_refused_before_the_transcript_grows() {
    let (_dir, engine, app, cookie) = ready_app();
    engine
        .db()
        .with_conn(|conn| {
            conn.execute("DELETE FROM settings WHERE key LIKE 'provider.%'", [])?;
            Ok(())
        })
        .unwrap();

    let reply = block_on(send(
        &app,
        Call::post(ROUTE_CHAT_SEND, "message=hello").cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert_eq!(reply.json()["code"], "provider_not_configured");
    assert_eq!(
        chat::count(engine.db(), CONVERSATION_ID).unwrap(),
        0,
        "the question was not written, because it could not be answered"
    );
}

#[test]
fn a_settled_transcript_renders_both_speakers_keeps_code_and_clears() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    chat::append(
        db,
        CONVERSATION_ID,
        Role::User,
        "how do I list files",
        State::Complete,
    )
    .unwrap();
    chat::append(
        db,
        CONVERSATION_ID,
        Role::Assistant,
        "Use this:\n```sh\nls -la\n```\nThat is the whole answer.",
        State::Complete,
    )
    .unwrap();

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(body.contains(">You<"), "the operator is named: {body}");
    assert!(body.contains(">Eris<"), "the assistant is named: {body}");
    assert!(body.contains("how do I list files"));
    assert!(
        body.contains(r#"<pre class="code">ls -la</pre>"#),
        "the fence became code: {body}"
    );
    assert!(
        body.contains(r#"class="code-lang">sh<"#),
        "the fence's language is shown: {body}"
    );
    assert!(
        body.contains(r#"action="/dashboard/chat/clear""#),
        "a non-empty transcript offers the clear: {body}"
    );

    let cleared = block_on(send(
        &app,
        Call::post(ROUTE_CHAT_CLEAR, "").cookie(Some(&cookie)),
    ));
    assert_eq!(cleared.status, StatusCode::SEE_OTHER, "{}", cleared.body);
    assert_eq!(cleared.location().as_deref(), Some(ROUTE_CHAT));
    assert_eq!(chat::count(db, CONVERSATION_ID).unwrap(), 0);
}

#[test]
fn a_wallpaper_choice_round_trips_and_refusals_name_the_field() {
    let (_dir, engine, app, cookie) = ready_app();

    let saved = block_on(send(
        &app,
        Call::post(wallpaper::ROUTE_WALLPAPER_SAVE, &wallpaper_body("none")).cookie(Some(&cookie)),
    ));
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    assert_eq!(saved.location().as_deref(), Some(ROUTE_SETTINGS));
    assert_eq!(
        engine
            .setting(wallpaper::SETTING_WALLPAPER)
            .unwrap()
            .as_deref(),
        Some("none")
    );

    // The chat page follows the setting: `none` paints no backdrop at all.
    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert!(
        !page.body.contains(r#"class="chat-wall""#),
        "none means no backdrop element: {}",
        page.body
    );

    let unknown = block_on(send(
        &app,
        Call::post(
            wallpaper::ROUTE_WALLPAPER_SAVE,
            &wallpaper_body("../../etc/passwd"),
        )
        .cookie(Some(&cookie)),
    ));
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST, "{}", unknown.body);
    assert_eq!(unknown.json()["code"], "wallpaper_unknown");

    let missing = block_on(send(
        &app,
        Call::post(wallpaper::ROUTE_WALLPAPER_SAVE, &wallpaper_body("custom"))
            .cookie(Some(&cookie)),
    ));
    assert_eq!(missing.status, StatusCode::BAD_REQUEST, "{}", missing.body);
    assert_eq!(missing.json()["code"], "wallpaper_not_uploaded");
    assert_eq!(
        engine
            .setting(wallpaper::SETTING_WALLPAPER)
            .unwrap()
            .as_deref(),
        Some("none"),
        "a refusal does not change the stored choice"
    );
}

#[test]
fn the_presets_are_served_as_svg_and_an_uploaded_image_keeps_its_real_type() {
    let (_dir, engine, app, cookie) = ready_app();

    let preset = block_on(send(
        &app,
        Call::get_owned(wallpaper::url("kallisti").unwrap()),
    ));
    assert_eq!(preset.status, StatusCode::OK, "{}", preset.body);
    assert_eq!(preset.headers.get("content-type").unwrap(), "image/svg+xml");
    assert!(preset.body.starts_with("<svg"));

    // Before any upload, the custom path is an honest 404.
    let (absent_status, _absent_type, _) = get_bytes(&app, &wallpaper::url("custom").unwrap());
    assert_eq!(absent_status, StatusCode::NOT_FOUND);

    let path = wallpaper::custom_file(engine.paths());
    std::fs::write(&path, a_png()).expect("write the image");
    engine
        .set_setting(wallpaper::SETTING_CUSTOM_TYPE, "image/png")
        .unwrap();
    engine
        .set_setting(wallpaper::SETTING_WALLPAPER, "custom")
        .unwrap();

    let (custom_status, custom_type, custom_bytes) =
        get_bytes(&app, &wallpaper::url("custom").unwrap());
    assert_eq!(custom_status, StatusCode::OK);
    assert_eq!(custom_type, "image/png");
    assert_eq!(
        custom_bytes,
        a_png(),
        "the bytes served are the bytes stored"
    );

    // Choosing custom now works, and the page paints it.
    let saved = block_on(send(
        &app,
        Call::post(wallpaper::ROUTE_WALLPAPER_SAVE, &wallpaper_body("custom"))
            .cookie(Some(&cookie)),
    ));
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert!(
        page.body.contains("/static/wallpaper/custom"),
        "the chat page paints the upload: {}",
        page.body
    );

    // A corrupt media-type row must not become a served type.
    engine
        .set_setting(wallpaper::SETTING_CUSTOM_TYPE, "text/html")
        .unwrap();
    let (coerced_status, coerced_type, _) = get_bytes(&app, &wallpaper::url("custom").unwrap());
    assert_eq!(coerced_status, StatusCode::OK);
    assert_eq!(
        coerced_type, "image/png",
        "a stored type outside the image set falls back instead of being served"
    );
}

#[test]
fn an_unknown_wallpaper_path_is_a_404_with_a_body() {
    let (_dir, _engine, app, _cookie) = ready_app();
    let reply = block_on(send(
        &app,
        Call::get_owned("/static/wallpaper/nope.svg".to_owned()),
    ));
    assert_eq!(reply.status, StatusCode::NOT_FOUND, "{}", reply.body);
    assert_eq!(reply.json()["code"], "not_found");
}

#[test]
fn the_settings_page_offers_every_wallpaper_and_the_upload() {
    let (_dir, _engine, app, cookie) = ready_app();
    let page = block_on(send(
        &app,
        Call::get(ROUTE_SETTINGS).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    for preset in wallpaper::PRESETS {
        assert!(
            body.contains(&format!(r#"id="wallpaper-{}""#, preset.id)),
            "{} is offered: {body}",
            preset.id
        );
    }
    assert!(
        body.contains(r#"enctype="multipart/form-data""#),
        "the upload is a real multipart form: {body}"
    );
    assert!(
        body.contains(r#"accept="image/png,image/jpeg,image/webp""#),
        "the picker names what it accepts: {body}"
    );
    assert!(
        body.contains("SVG is refused"),
        "the refusal is stated rather than discovered: {body}"
    );
}

#[test]
fn a_settings_view_defaults_to_the_kallisti_backdrop() {
    let view = SettingsView::new(crate::db::SetupState::Ready, "1.0.1");
    assert_eq!(view.wallpaper, wallpaper::DEFAULT_WALLPAPER);
    assert!(!view.wallpaper_custom);
}
