//! Phase 1 acceptance: the Activity page.
//!
//! The claim is narrow and testable: the page lists the audit trail newest
//! first, server-rendered, with the detail column truncated for display, and it
//! is the same rows `audit_log` holds rather than a second log written by the web
//! layer.

use axum::http::StatusCode;

use crate::admin::ROUTE_ACTIVITY;

use super::harness::{Call, audit_rows, block_on, ready_app, send};

#[test]
fn the_activity_page_lists_newest_first() {
    let (_dir, engine, app, cookie) = ready_app();

    // Two rows with distinguishable actions, written oldest first. The engine
    // already wrote its setup rows, so these are appended after them.
    engine
        .auth()
        .audit("loopback", "settings.provider.first", Some("older row"))
        .expect("audit");
    engine
        .auth()
        .audit("loopback", "settings.permissions.second", Some("newer row"))
        .expect("audit");

    let reply = block_on(send(
        &app,
        Call::get(ROUTE_ACTIVITY).html().cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let page = &reply.body;

    // The three column headings the brief asks for.
    for heading in ["Time", "Actor", "Action", "Detail"] {
        assert!(
            page.contains(heading),
            "the {heading} column is missing: {page}"
        );
    }

    // Both rows are present, and the newer one comes first.
    let older = page
        .find("settings.provider.first")
        .unwrap_or_else(|| panic!("the older row is missing: {page}"));
    let newer = page
        .find("settings.permissions.second")
        .unwrap_or_else(|| panic!("the newer row is missing: {page}"));
    assert!(
        newer < older,
        "the page lists newest first: newer at {newer}, older at {older}"
    );

    // The page is the audit table, not a copy of it: every action the database
    // holds is on the page, in the reverse of the order the table stores it.
    let rows = audit_rows(engine.db());
    assert!(rows.len() >= 2, "the harness wrote nothing");
    let mut cursor = 0usize;
    for row in rows.iter().rev() {
        let found = page[cursor..]
            .find(&row.action)
            .unwrap_or_else(|| panic!("{} is missing from the page", row.action));
        cursor += found + row.action.len();
    }
    assert!(
        rows.last()
            .is_some_and(|row| row.action == "settings.permissions.second"),
        "the last row written is last in the database"
    );

    // A long detail is truncated for display rather than pushing the table wide.
    let long = "x".repeat(400);
    engine
        .auth()
        .audit("loopback", "settings.long_detail", Some(&long))
        .expect("audit");
    let truncated = block_on(send(
        &app,
        Call::get(ROUTE_ACTIVITY).html().cookie(Some(&cookie)),
    ));
    assert_eq!(truncated.status, StatusCode::OK);
    assert!(
        !truncated.body.contains(&long),
        "the 400 character detail is truncated on the page"
    );
    assert!(
        truncated.body.contains("settings.long_detail"),
        "the row itself is still listed: {}",
        truncated.body
    );
    assert!(
        truncated.body.contains("..."),
        "truncation is visible as an ellipsis: {}",
        truncated.body
    );

    // Server-rendered: no script, and the page needs a session.
    assert!(!truncated.body.to_ascii_lowercase().contains("<script"));
    let anonymous = block_on(send(&app, Call::get(ROUTE_ACTIVITY)));
    assert_eq!(anonymous.status, StatusCode::FORBIDDEN);
    assert_eq!(anonymous.json()["code"], "no_session");
}

#[test]
fn the_activity_page_caps_the_list_at_one_hundred_rows() {
    let (_dir, engine, app, cookie) = ready_app();
    for index in 0..150 {
        engine
            .auth()
            .audit("loopback", &format!("activity.row.{index:03}"), None)
            .expect("audit");
    }

    let reply = block_on(send(
        &app,
        Call::get(ROUTE_ACTIVITY).html().cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);

    // Newest first, so the newest marker is present and the oldest is not.
    assert!(
        reply.body.contains("activity.row.149"),
        "the newest row is listed: {}",
        reply.body
    );
    assert!(
        !reply.body.contains("activity.row.000"),
        "the list stops at one hundred rows: {}",
        reply.body
    );
    let listed = reply.body.matches("activity.row.").count();
    assert!(
        listed <= 100,
        "the page listed {listed} activity rows, more than one hundred"
    );
}
