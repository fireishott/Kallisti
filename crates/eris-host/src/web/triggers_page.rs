//! The triggers page: the table, the form, the one-time token, and the two-press
//! delete.
//!
//! The claim that matters most here is the token: a mint renders it once, and every
//! render after that shows the stored prefix and nothing usable.

use axum::http::StatusCode;

use crate::admin::{
    ROUTE_TRIGGERS, ROUTE_TRIGGERS_DELETE, ROUTE_TRIGGERS_SAVE, ROUTE_TRIGGERS_TOGGLE,
};
use crate::trigger::{self, Draft};

use super::harness::{Call, block_on, ready_app, send};

/// A draft that passes every bound.
fn draft() -> Draft {
    Draft {
        name: "nightly".to_owned(),
        prompt: "Say which files changed today.".to_owned(),
        max_per_hour: 60,
    }
}

/// A form body that posts a draft.
fn form(draft: &Draft) -> String {
    format!(
        "name={}&prompt={}&max_per_hour={}&enabled=true",
        encode(&draft.name),
        encode(&draft.prompt),
        draft.max_per_hour
    )
}

/// Percent-encode the characters a form body cares about here.
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
fn an_empty_table_says_so_and_offers_the_form() {
    let (_dir, _engine, app, cookie) = ready_app();
    let reply = block_on(send(
        &app,
        Call::get(ROUTE_TRIGGERS).html().cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let page = &reply.body;
    assert!(page.contains("No triggers yet."), "{page}");
    assert!(page.contains(ROUTE_TRIGGERS_SAVE), "{page}");
    for field in ["trigger_name", "trigger_prompt", "trigger_max_per_hour"] {
        assert!(page.contains(&format!("id=\"{field}\"")), "{field}: {page}");
        assert!(
            page.contains(&format!("for=\"{field}\"")),
            "{field}: {page}"
        );
    }
    assert!(!page.contains("<script"), "no script on this page: {page}");
}

#[test]
fn a_mint_shows_the_token_once_and_never_again() {
    let (_dir, engine, app, cookie) = ready_app();
    let reply = block_on(send(
        &app,
        Call::post(ROUTE_TRIGGERS_SAVE, &form(&draft()))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let location = reply
        .headers
        .get("location")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    assert!(location.contains("minted=whk_"), "{location}");

    // The one render that carried the token shows the whole URL.
    let page = block_on(send(
        &app,
        Call::get_owned(location.clone())
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains("/hooks/whk_"), "{}", page.body);
    assert!(
        page.body.contains("the only time this token is shown"),
        "{}",
        page.body
    );

    // Every later render shows the prefix and no usable token.
    let later = block_on(send(
        &app,
        Call::get(ROUTE_TRIGGERS).html().cookie(Some(&cookie)),
    ));
    assert_eq!(later.status, StatusCode::OK, "{}", later.body);
    assert!(!later.body.contains("/hooks/"), "{}", later.body);
    let stored = trigger::get_by_name(engine.db(), "nightly")
        .expect("a read")
        .expect("the row");
    assert!(later.body.contains(&stored.token_prefix), "{}", later.body);
}

#[test]
fn a_bad_rate_is_refused_and_the_prompt_is_kept() {
    let (_dir, engine, app, cookie) = ready_app();
    // The name is a row's identity, so a save over an existing name rotates it.
    // The refusal path is a value outside its bound, and it must keep the text the
    // operator typed: a bound that loses a prompt is a data-loss bug wearing a
    // validation message.
    let body = format!(
        "name=nightly&prompt={}&max_per_hour={}&enabled=true",
        encode(&draft().prompt),
        trigger::MAX_PER_HOUR + 1
    );
    let reply = block_on(send(
        &app,
        Call::post(ROUTE_TRIGGERS_SAVE, &body)
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(
        reply.body.contains("trigger_rate_invalid"),
        "{}",
        reply.body
    );
    assert!(reply.body.contains(&draft().prompt), "{}", reply.body);
    assert!(
        reply.body.contains("aria-invalid=\"true\""),
        "{}",
        reply.body
    );
    assert!(
        trigger::list(engine.db()).expect("a read").is_empty(),
        "a refused save writes nothing"
    );
}

#[test]
fn a_save_of_an_existing_row_rotates_the_token() {
    let (_dir, engine, app, cookie) = ready_app();
    let first = trigger::create(engine.db(), &draft()).expect("a row");
    let reply = block_on(send(
        &app,
        Call::post(ROUTE_TRIGGERS_SAVE, &form(&draft()))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(
        trigger::list(engine.db()).expect("a read").len(),
        1,
        "a save is an update, not a second row"
    );
    assert!(
        trigger::resolve_by_token(engine.db(), &first.token)
            .expect("a read")
            .is_none(),
        "the old URL is dead"
    );
}

#[test]
fn a_toggle_flips_the_column_and_a_delete_takes_two_presses() {
    let (_dir, engine, app, cookie) = ready_app();
    trigger::create(engine.db(), &draft()).expect("a row");

    let reply = block_on(send(
        &app,
        Call::post(ROUTE_TRIGGERS_TOGGLE, "name=nightly&enabled=false")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert!(
        !trigger::get_by_name(engine.db(), "nightly")
            .expect("a read")
            .expect("the row")
            .enabled
    );

    // First press asks, and the row is still there.
    let reply = block_on(send(
        &app,
        Call::post(ROUTE_TRIGGERS_DELETE, "name=nightly")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.body.contains("Delete"), "{}", reply.body);
    assert!(
        trigger::get_by_name(engine.db(), "nightly")
            .expect("a read")
            .is_some(),
        "the first press deletes nothing"
    );

    // Second press removes it.
    let reply = block_on(send(
        &app,
        Call::post(ROUTE_TRIGGERS_DELETE, "name=nightly&confirm=true")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert!(
        trigger::get_by_name(engine.db(), "nightly")
            .expect("a read")
            .is_none()
    );
}

#[test]
fn the_page_needs_a_session() {
    let (_dir, _engine, app, _cookie) = ready_app();
    let reply = block_on(send(&app, Call::get(ROUTE_TRIGGERS).html()));
    assert!(
        reply.status == StatusCode::FORBIDDEN || reply.status == StatusCode::SEE_OTHER,
        "{}",
        reply.status
    );
}
