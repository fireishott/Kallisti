//! The search page: the field, the results, and the link out.
//!
//! The claim is narrow: the page renders what the index actually holds, and a hit
//! opens the conversation it came from. The index itself is tested in
//! [`crate::search`]; what is tested here is that the page is the index's reader
//! rather than a second, prettier guess.

use axum::http::StatusCode;

use crate::admin::{ROUTE_CHAT, ROUTE_SEARCH};

use super::harness::{Call, block_on, ready_app, send};

/// One conversation with two messages that both mention the warehouse.
fn seed(engine: &crate::onboarding::SetupEngine) {
    engine
        .db()
        .with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO chat_conversations (id, title) VALUES ('conv-1', 'Warehouse lane health');
                 INSERT INTO chat_messages (id, conversation_id, role, body)
                   VALUES ('msg-1', 'conv-1', 'user',
                           'the warehouse refresh keeps failing at 03:00');
                 INSERT INTO chat_messages (id, conversation_id, role, body)
                   VALUES ('msg-2', 'conv-1', 'assistant',
                           'the warehouse refresh was fixed by widening the timeout');",
            )
        })
        .expect("seed");
}

#[test]
fn the_page_lists_the_conversation_that_matched() {
    let (_dir, engine, app, cookie) = ready_app();
    seed(&engine);
    let reply = block_on(send(
        &app,
        Call::get_owned(format!("{ROUTE_SEARCH}?q=warehouse"))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let page = &reply.body;
    assert!(page.contains("Warehouse lane health"), "{page}");
    // The excerpt is the index's own snippet, so the matched word is bracketed.
    assert!(
        page.contains("[warehouse]"),
        "the snippet marks the match: {page}"
    );
    assert!(
        page.contains(&format!("{ROUTE_CHAT}?conversation=conv-1")),
        "a hit opens the conversation it came from: {page}"
    );
    // Two messages matched, so the row says so rather than listing the same
    // conversation twice.
    assert!(page.contains("2 messages"), "{page}");
}

#[test]
fn an_empty_field_draws_the_page_rather_than_a_result_list() {
    let (_dir, engine, app, cookie) = ready_app();
    seed(&engine);
    let reply = block_on(send(
        &app,
        Call::get(ROUTE_SEARCH).html().cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let page = &reply.body;
    assert!(page.contains("Search"), "{page}");
    assert!(page.contains("Type a word or two"), "{page}");
    // The rail lists the conversation title on every page, so the proof that no
    // result list was drawn is the absence of a bracketed snippet, not of a title.
    assert!(!page.contains("[warehouse]"), "{page}");
    assert!(!page.contains("matched."), "{page}");
}

#[test]
fn a_query_that_matches_nothing_says_so() {
    let (_dir, engine, app, cookie) = ready_app();
    seed(&engine);
    let reply = block_on(send(
        &app,
        Call::get_owned(format!("{ROUTE_SEARCH}?q=zeppelin"))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(reply.body.contains("Nothing matched"), "{}", reply.body);
}

#[test]
fn the_search_section_is_in_the_nav_and_the_rail_offers_it() {
    let (_dir, engine, app, cookie) = ready_app();
    seed(&engine);
    let reply = block_on(send(
        &app,
        Call::get(ROUTE_SEARCH).html().cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let page = &reply.body;
    assert!(
        page.contains(&format!("href=\"{ROUTE_SEARCH}\"")),
        "the nav links the page: {page}"
    );
    assert!(
        page.contains("class=\"rail-search\""),
        "the rail carries the field: {page}"
    );
}

#[test]
fn the_page_needs_a_dashboard_session() {
    let (_dir, _engine, app, _cookie) = ready_app();
    let reply = block_on(send(&app, Call::get(ROUTE_SEARCH).html()));
    assert_ne!(
        reply.status,
        StatusCode::OK,
        "an unauthenticated read must not serve the transcript"
    );
}
