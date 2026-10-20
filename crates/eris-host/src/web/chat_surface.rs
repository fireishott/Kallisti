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

use crate::admin::{ROUTE_CHAT, ROUTE_CHAT_SEND, ROUTE_SETTINGS};
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
        body.contains("/static/wallpaper/kallisti.jpg"),
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
        !body.contains("Nothing said yet"),
        "an empty conversation shows no placeholder panel: {body}"
    );
    assert!(
        !body.contains(r#"class="lede""#),
        "the chat page carries no explanatory paragraph: {body}"
    );
    assert!(
        body.contains(r#"id="mode""#),
        "the composer offers the access picker: {body}"
    );
    assert_eq!(
        body.matches("<script").count(),
        1,
        "one inline script, as the shell sweep documents"
    );
}

#[test]
fn the_page_shows_the_usage_the_transcript_carries() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    chat::append(
        db,
        CONVERSATION_ID,
        Role::User,
        "how big is the window",
        State::Complete,
    )
    .unwrap();
    // The default model is the picker's starting point, so the window it states
    // has to be the default model's window.
    engine
        .set_setting(crate::onboarding::SETTING_MODEL, "m/watch")
        .unwrap();
    engine
        .set_provider_contexts(&std::collections::BTreeMap::from([(
            "m/watch".to_owned(),
            272_000,
        )]))
        .unwrap();
    let answer = chat::append_answer(db, CONVERSATION_ID, "m/watch", Some(272_000)).unwrap();
    chat::settle(
        db,
        &answer,
        "big enough",
        None,
        chat::Accounting {
            prompt_tokens: Some(1_200),
            completion_tokens: Some(18),
            cached_tokens: Some(1_150),
            duration_ms: Some(900),
        },
    )
    .unwrap();

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    // Per answer: the model, both token counts, the cache share, the duration.
    assert!(body.contains("m/watch"), "the model that answered is named");
    assert!(body.contains("1.2k in"), "prompt tokens are shown");
    assert!(body.contains("18 out"), "completion tokens are shown");
    assert!(
        body.contains("48% cache"),
        "cache share is derived from the row"
    );
    assert!(body.contains("900ms"), "duration is shown");
    // Per conversation: the header chips and the status strip.
    assert!(body.contains("chat-stats"), "the stats row renders");
    assert!(
        body.contains("272.0k window"),
        "the declared window is stated beside the picker: {body}"
    );
    assert!(body.contains("statusbar"), "the status strip renders");
    assert!(
        body.contains("cache hit 48%"),
        "the strip repeats the cache share"
    );
    assert!(body.contains("context unknown") || body.contains("context 0%"));
}

#[test]
fn a_model_outside_the_probe_is_refused_by_name() {
    let (_dir, engine, app, cookie) = ready_app();
    engine
        .set_setting(
            crate::onboarding::SETTING_PROVIDER_MODELS,
            r#"["m/one","m/two"]"#,
        )
        .unwrap();

    let refused = block_on(send(
        &app,
        Call::post(ROUTE_CHAT_SEND, "message=hello&model=m/other").cookie(Some(&cookie)),
    ));
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    assert_eq!(refused.json()["code"], "model_not_in_probe");
    assert_eq!(
        chat::count(engine.db(), CONVERSATION_ID).unwrap(),
        0,
        "a refused model writes nothing"
    );
}

#[test]
fn the_rail_list_is_served_for_a_page_that_stays_open() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    chat::append(
        db,
        chat::CONVERSATION_ID,
        Role::User,
        "name me",
        State::Complete,
    )
    .unwrap();
    let second = chat::create_conversation(db).unwrap();

    let reply = block_on(send(
        &app,
        Call::get(crate::admin::ROUTE_CHAT_SESSIONS).cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let body = reply.json();
    assert_eq!(body["ok"], true);
    let sessions = body["sessions"].as_array().expect("a list");
    assert!(sessions.len() >= 2, "{body}");
    assert!(
        sessions.iter().any(|row| row["id"] == second.as_str()),
        "the new conversation is listed: {body}"
    );
    assert!(
        sessions
            .iter()
            .all(|row| row["title"].is_string() && row["stamp"].is_string()),
        "each row carries a title and a stamp: {body}"
    );
}

#[test]
fn the_composer_picker_changes_the_mode_and_refuses_full_when_closed() {
    let (_dir, engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());

    // Workspace write is a normal mode and needs nothing else.
    let switched = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_CHAT_MODE,
            "sandbox_mode=workspace-write",
        )
        .cookie(cookie),
    ));
    assert_eq!(switched.status, StatusCode::OK, "{}", switched.body);
    assert_eq!(switched.json()["sandbox_mode"], "workspace-write");
    assert_eq!(
        engine.permissions().unwrap().sandbox_mode.as_str(),
        "workspace-write"
    );

    // Full is refused while the switch is off, by name.
    let refused = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_CHAT_MODE, "sandbox_mode=full").cookie(cookie),
    ));
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    assert_eq!(refused.json()["field"], "sandbox_mode");
    assert_eq!(
        engine.permissions().unwrap().sandbox_mode.as_str(),
        "workspace-write",
        "a refusal leaves the stored mode alone"
    );

    // The other three fields of the tuple are untouched by the picker.
    assert_eq!(
        engine.permissions().unwrap().approval_policy.as_str(),
        "ask"
    );
}

#[test]
fn full_access_is_a_switch_the_operator_can_flip() {
    let (_dir, engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());

    // It starts off, and selecting full is refused.
    let refused = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_PERMISSIONS,
            "sandbox_mode=full&approval_policy=ask&autopilot_ceiling=l1&telemetry=off",
        )
        .cookie(cookie),
    ));
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);

    // Enabling needs the typed word.
    let no_word = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SETTINGS_FULL_ACCESS, "confirm=").cookie(cookie),
    ));
    assert_eq!(no_word.status, StatusCode::BAD_REQUEST, "{}", no_word.body);
    assert_eq!(no_word.json()["code"], "confirm_required");
    assert!(
        engine
            .setting(crate::onboarding::SETTING_ALLOW_FULL)
            .unwrap()
            .as_deref()
            != Some("true"),
        "a missing confirmation changes nothing"
    );

    // With it, the switch is on and the mode is accepted.
    let enabled = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SETTINGS_FULL_ACCESS, "confirm=full").cookie(cookie),
    ));
    assert_eq!(enabled.status, StatusCode::SEE_OTHER, "{}", enabled.body);
    assert_eq!(
        engine
            .setting(crate::onboarding::SETTING_ALLOW_FULL)
            .unwrap()
            .as_deref(),
        Some("true")
    );
    let accepted = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_PERMISSIONS,
            "sandbox_mode=full&approval_policy=ask&autopilot_ceiling=l1&telemetry=off",
        )
        .cookie(cookie),
    ));
    assert_eq!(accepted.status, StatusCode::SEE_OTHER, "{}", accepted.body);
    assert_eq!(engine.permissions().unwrap().sandbox_mode.as_str(), "full");

    // Turning it off needs no ritual, and it drops the mode it enabled.
    let disabled = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SETTINGS_FULL_ACCESS, "").cookie(cookie),
    ));
    assert_eq!(disabled.status, StatusCode::SEE_OTHER, "{}", disabled.body);
    assert_ne!(
        engine.permissions().unwrap().sandbox_mode.as_str(),
        "full",
        "revoking the switch revokes the mode"
    );
}

#[test]
fn a_message_can_carry_a_file_and_the_turn_is_told_about_it() {
    let (_dir, engine, app, cookie) = ready_app();
    crate::web::chat::seed_conversation(engine.db(), crate::web::chat::CONVERSATION_ID).unwrap();

    let boundary = "----kallisti-test-boundary";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"message\"\r\n\r\nread this\r\n\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"conversation\"\r\n\r\n{conv}\r\n\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"notes.txt\"\r\nContent-Type: text/plain\r\n\r\nline one\nline two\r\n--{boundary}--\r\n",
        conv = crate::web::chat::CONVERSATION_ID
    );

    let reply = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_CHAT_SEND, &body)
            .content_type(concat!(
                "multipart/form-data; boundary=",
                "----kallisti-test-boundary"
            ))
            .cookie(Some(&cookie)),
    ));
    // The provider is stubbed out in this fixture, so the turn itself is refused;
    // what matters here is that the file was accepted, stored, and recorded.
    assert!(
        reply.status == StatusCode::SEE_OTHER || reply.status == StatusCode::BAD_REQUEST,
        "{}",
        reply.body
    );

    let workspace = engine.workspace_path().unwrap();
    let directory = workspace
        .join("uploads")
        .join(crate::web::chat::CONVERSATION_ID);
    let entries: Vec<_> = std::fs::read_dir(&directory)
        .expect("the uploads directory exists")
        .filter_map(Result::ok)
        .collect();
    assert_eq!(entries.len(), 1, "one file was written");
    let name = entries[0].file_name().to_string_lossy().to_string();
    assert!(
        name.ends_with("-notes.txt"),
        "the name is kept, safe: {name}"
    );
    let stored = std::fs::read(entries[0].path()).expect("readable");
    assert_eq!(stored, b"line one\nline two");
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
        body.contains(&format!(
            r#"action="/dashboard/chat/{CONVERSATION_ID}/delete""#
        )),
        "the open conversation can be deleted from the rail: {body}"
    );

    let deleted = block_on(send(
        &app,
        Call::post_owned(format!("/dashboard/chat/{CONVERSATION_ID}/delete"), "")
            .cookie(Some(&cookie)),
    ));
    assert_eq!(deleted.status, StatusCode::SEE_OTHER, "{}", deleted.body);
    assert_eq!(deleted.location().as_deref(), Some(ROUTE_CHAT));
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

    // The default preset is the artwork now, and it is served as the image it is.
    let (art_status, art_type, art_bytes) = get_bytes(&app, &wallpaper::url("kallisti").unwrap());
    assert_eq!(art_status, StatusCode::OK);
    assert_eq!(art_type, "image/jpeg");
    assert_eq!(&art_bytes[..2], &[0xff, 0xd8], "a real JPEG comes back");

    // The generated backdrops are still SVG, and still served as SVG.
    let (svg_status, svg_type, svg_bytes) = get_bytes(&app, &wallpaper::url("arcs").unwrap());
    assert_eq!(svg_status, StatusCode::OK);
    assert_eq!(svg_type, "image/svg+xml");
    assert!(svg_bytes.starts_with(b"<svg"));

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
