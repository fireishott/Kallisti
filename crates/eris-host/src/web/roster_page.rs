//! The roster page: the table, the form, and the four refusals that keep a row
//! honest.
//!
//! The claim is narrow and it is the whole page: every row in the table is a row
//! the roster module returns, a save goes through the same `validate` and the
//! same two functions the dispatch path uses, and a refused save keeps the
//! operator's text in the form rather than losing it.

use axum::http::StatusCode;

use crate::admin::{ROUTE_ROSTER, ROUTE_ROSTER_DELETE, ROUTE_ROSTER_SAVE, ROUTE_ROSTER_TOGGLE};
use crate::roster::{self, Draft, ToolPolicy};

use super::harness::{Call, block_on, ready_app, send};

/// One draft that passes every bound, so a test that cares about one field can
/// leave the other five alone.
fn draft() -> Draft {
    Draft {
        name: "researcher".to_owned(),
        role: "Reads the tree before anyone edits it.".to_owned(),
        instructions: "Read the file you are about to change. Quote the line first.".to_owned(),
        tool_policy: ToolPolicy::Inherit,
        model_role: None,
        memory_scope: None,
    }
}

/// The form body a save posts, built from a draft.
fn form(draft: &Draft) -> String {
    let mut body = format!(
        "name={}&role={}&instructions={}&tool_policy={}",
        urlencode(&draft.name),
        urlencode(&draft.role),
        urlencode(&draft.instructions),
        draft.tool_policy.as_str()
    );
    if let Some(scope) = &draft.memory_scope {
        body.push_str(&format!("&memory_scope={}", urlencode(scope)));
    }
    if let Some(role) = &draft.model_role {
        body.push_str(&format!("&model_role={}", urlencode(role)));
    }
    body.push_str("&enabled=true");
    body
}

/// Percent-encode the four characters a form body cares about here.
fn urlencode(value: &str) -> String {
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
fn an_empty_roster_says_so_and_still_offers_the_form() {
    let (_dir, _engine, app, cookie) = ready_app();
    let reply = block_on(send(
        &app,
        Call::get(ROUTE_ROSTER).html().cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let page = &reply.body;

    assert!(page.contains("No standing agents yet."), "{page}");
    assert!(page.contains(ROUTE_ROSTER_SAVE), "{page}");
    // Every input carries an id and a label, which is the house rule.
    for field in [
        "roster_name",
        "roster_role",
        "roster_instructions",
        "roster_tool_policy",
        "roster_memory_scope",
        "roster_model_role",
    ] {
        assert!(page.contains(&format!("id=\"{field}\"")), "{field}: {page}");
        assert!(
            page.contains(&format!("for=\"{field}\"")),
            "{field}: {page}"
        );
    }
    // A form region that announces its own result, and no script anywhere.
    assert!(page.contains(r#"role="status""#), "{page}");
    assert!(!page.contains("<script"), "{page}");
}

#[test]
fn a_saved_row_appears_with_its_role_and_state() {
    let (_dir, engine, app, cookie) = ready_app();
    let saved = draft();
    let reply = block_on(send(
        &app,
        Call::post(ROUTE_ROSTER_SAVE, &form(&saved))
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
    assert!(location.contains("saved=create"), "{location}");

    // The row is in the table, and the table is a read of the same function the
    // dispatch path uses.
    let stored = roster::get_by_name(engine.db(), "researcher")
        .expect("a read")
        .expect("the row");
    assert_eq!(stored.role, saved.role);
    assert_eq!(stored.tool_policy, ToolPolicy::Inherit);
    assert!(stored.enabled);

    let page = block_on(send(
        &app,
        Call::get(ROUTE_ROSTER).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains("researcher"), "{}", page.body);
    assert!(page.body.contains(&saved.role), "{}", page.body);
    assert!(page.body.contains("enabled"), "{}", page.body);
    assert!(page.body.contains(ROUTE_ROSTER_TOGGLE), "{}", page.body);
    assert!(page.body.contains(ROUTE_ROSTER_DELETE), "{}", page.body);
}

#[test]
fn a_name_outside_the_alphabet_is_refused_and_the_brief_is_kept() {
    let (_dir, engine, app, cookie) = ready_app();
    let mut bad = draft();
    bad.name = "has spaces".to_owned();
    let reply = block_on(send(
        &app,
        Call::post(ROUTE_ROSTER_SAVE, &form(&bad))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    let page = &reply.body;

    assert!(page.contains("roster_name_invalid"), "{page}");
    // The text the operator typed is still in the form, including the brief.
    assert!(page.contains("has spaces"), "{page}");
    assert!(page.contains(&bad.instructions), "{page}");
    // The failed input takes focus and says so to a screen reader.
    assert!(
        page.contains(r#"id="roster_name""#) && page.contains("aria-invalid=\"true\""),
        "{page}"
    );
    // Nothing was written.
    assert!(
        roster::list(engine.db()).expect("a read").is_empty(),
        "a refused save writes nothing"
    );
}

#[test]
fn a_read_only_row_stores_its_policy_and_a_toggle_flips_it() {
    let (_dir, engine, app, cookie) = ready_app();
    let mut saved = draft();
    saved.tool_policy = ToolPolicy::ReadOnly;
    let reply = block_on(send(
        &app,
        Call::post(ROUTE_ROSTER_SAVE, &form(&saved))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let stored = roster::get_by_name(engine.db(), "researcher")
        .expect("a read")
        .expect("the row");
    assert_eq!(stored.tool_policy, ToolPolicy::ReadOnly);

    let reply = block_on(send(
        &app,
        Call::post(ROUTE_ROSTER_TOGGLE, "name=researcher&enabled=false")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let stored = roster::get_by_name(engine.db(), "researcher")
        .expect("a read")
        .expect("the row");
    assert!(!stored.enabled, "the toggle writes the column");
}

#[test]
fn a_delete_asks_first_and_only_the_second_press_removes_the_row() {
    let (_dir, engine, app, cookie) = ready_app();
    roster::create(engine.db(), &draft()).expect("a row");

    // First press: a confirmation, and the row is still there.
    let reply = block_on(send(
        &app,
        Call::post(ROUTE_ROSTER_DELETE, "name=researcher")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.body.contains("Delete"), "{}", reply.body);
    assert!(reply.body.contains("researcher"), "{}", reply.body);
    assert!(
        roster::get_by_name(engine.db(), "researcher")
            .expect("a read")
            .is_some(),
        "the first press deletes nothing"
    );

    // Second press: gone, and the redirect names the notice.
    let reply = block_on(send(
        &app,
        Call::post(ROUTE_ROSTER_DELETE, "name=researcher&confirm=true")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert!(
        roster::get_by_name(engine.db(), "researcher")
            .expect("a read")
            .is_none(),
        "the second press removes the row"
    );
}

#[test]
fn an_edit_route_fills_the_form_from_the_row() {
    let (_dir, engine, app, cookie) = ready_app();
    roster::create(engine.db(), &draft()).expect("a row");
    let reply = block_on(send(
        &app,
        Call::get_owned(format!("{ROUTE_ROSTER}?edit=researcher"))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let page = &reply.body;
    assert!(page.contains("Edit"), "{page}");
    assert!(page.contains(&draft().role), "{page}");
    assert!(page.contains(&draft().instructions), "{page}");
}

#[test]
fn the_page_needs_a_session() {
    let (_dir, _engine, app, _cookie) = ready_app();
    let reply = block_on(send(&app, Call::get(ROUTE_ROSTER).html()));
    assert!(
        reply.status == StatusCode::FORBIDDEN || reply.status == StatusCode::SEE_OTHER,
        "{}",
        reply.status
    );
}
