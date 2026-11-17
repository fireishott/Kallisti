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
    // The excerpt is the index's own snippet, so the matched word is emphasised.
    assert!(
        page.contains("<b>warehouse</b>"),
        "the snippet marks the match: {page}"
    );
    assert!(
        page.contains(&format!("{ROUTE_CHAT}?conversation=conv-1")),
        "a hit opens the conversation it came from: {page}"
    );
    // Two messages matched, so the page says so from the count query rather than
    // letting the row count stand in for the total.
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
    assert!(!page.contains("<b>warehouse</b>"), "{page}");
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

#[test]
fn a_scope_narrows_the_query_and_the_page_states_it() {
    let (_dir, engine, app, cookie) = ready_app();
    seed(&engine);
    engine
        .db()
        .with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO chat_conversations (id, title) VALUES ('conv-2', 'Kitchen');
                 INSERT INTO chat_messages (id, conversation_id, role, body)
                   VALUES ('msg-3', 'conv-2', 'user', 'the warehouse list is on the fridge');",
            )
        })
        .expect("seed");

    let everywhere = block_on(send(
        &app,
        Call::get_owned(format!("{ROUTE_SEARCH}?q=warehouse"))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(everywhere.status, StatusCode::OK, "{}", everywhere.body);
    assert!(
        everywhere.body.contains("3 messages"),
        "{}",
        everywhere.body
    );
    assert!(everywhere.body.contains("Kitchen"), "{}", everywhere.body);

    let held = block_on(send(
        &app,
        Call::get_owned(format!(
            "{ROUTE_SEARCH}?q=warehouse&scope=conversation&conversation_id=conv-1"
        ))
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(held.status, StatusCode::OK, "{}", held.body);
    assert!(held.body.contains("2 messages"), "{}", held.body);
    // The rail still lists every conversation, so the proof is the hit links.
    assert!(
        !held.body.contains("chat?conversation=conv-2"),
        "{}",
        held.body
    );
}

#[test]
fn a_role_filter_returns_only_that_speaker() {
    let (_dir, engine, app, cookie) = ready_app();
    seed(&engine);
    let page = block_on(send(
        &app,
        Call::get_owned(format!("{ROUTE_SEARCH}?q=warehouse&role=assistant"))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains("1 message"), "{}", page.body);
    assert!(page.body.contains("widening the timeout"), "{}", page.body);
    assert!(!page.body.contains("failing at 03:00"), "{}", page.body);
}

#[test]
fn the_limit_is_the_stored_setting_and_the_page_says_how_many_matched() {
    let (_dir, engine, app, cookie) = ready_app();
    seed(&engine);
    for index in 0..9 {
        engine
            .db()
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO chat_messages (id, conversation_id, role, body)
                     VALUES (?1, 'conv-1', 'user', 'warehouse note number')",
                    rusqlite::params![format!("bulk-{index}")],
                )?;
                Ok(())
            })
            .expect("seed");
    }
    crate::web::store::settings_put(engine.db(), crate::search::SETTING_SEARCH_LIMIT, "5")
        .expect("store");
    let page = block_on(send(
        &app,
        Call::get_owned(format!("{ROUTE_SEARCH}?q=warehouse"))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    // Eleven rows match, five are drawn, and the total is the count query's.
    assert!(page.body.contains("11 messages"), "{}", page.body);
    assert!(page.body.contains("5 rows"), "{}", page.body);
    assert!(
        page.body.contains("Next page"),
        "a cursor is offered while rows remain: {}",
        page.body
    );
}
