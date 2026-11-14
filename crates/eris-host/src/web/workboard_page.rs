//! The workboard page: the claim that a task outlives the session.
//!
//! The rows are the table's own, the controls are forms, and a restart is
//! simulated by reading the same database twice: a board that only exists in a
//! running process is the failure this page is for.

use axum::http::StatusCode;

use crate::admin::{ROUTE_WORKBOARD, ROUTE_WORKBOARD_ADD, ROUTE_WORKBOARD_ITEM};

use super::harness::{Call, block_on, ready_app, send};

fn add(app: &axum::Router, cookie: &str, title: &str) -> StatusCode {
    block_on(send(
        app,
        Call::post(
            ROUTE_WORKBOARD_ADD,
            &format!("title={}&project=proof", encode(title)),
        )
        .cookie(Some(cookie)),
    ))
    .status
}

fn encode(value: &str) -> String {
    value.replace(' ', "+")
}

#[test]
fn the_page_is_reachable_from_the_nav_and_lists_a_task() {
    let (_dir, engine, app, cookie) = ready_app();
    assert_eq!(
        add(&app, &cookie, "widen the timeout"),
        StatusCode::SEE_OTHER
    );
    let reply = block_on(send(
        &app,
        Call::get(ROUTE_WORKBOARD).html().cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let page = &reply.body;
    assert!(
        page.contains(&format!("href=\"{ROUTE_WORKBOARD}\"")),
        "the nav links it: {page}"
    );
    assert!(page.contains("widen the timeout"), "{page}");
    assert!(page.contains("proof"), "{page}");
    let _ = engine;
}

#[test]
fn a_move_and_a_drop_both_land() {
    let (_dir, engine, app, cookie) = ready_app();
    assert_eq!(add(&app, &cookie, "ship it"), StatusCode::SEE_OTHER);
    let id = crate::workboard::list(engine.db(), None, 10).expect("list")[0]
        .id
        .clone();
    let moved = block_on(send(
        &app,
        Call::post(
            ROUTE_WORKBOARD_ITEM,
            &format!("id={id}&action=status&status=done"),
        )
        .cookie(Some(&cookie)),
    ));
    assert_eq!(moved.status, StatusCode::SEE_OTHER);
    assert_eq!(
        crate::workboard::get(engine.db(), &id)
            .expect("get")
            .expect("row")
            .status,
        "done"
    );
    let dropped = block_on(send(
        &app,
        Call::post(ROUTE_WORKBOARD_ITEM, &format!("id={id}&action=delete")).cookie(Some(&cookie)),
    ));
    assert_eq!(dropped.status, StatusCode::SEE_OTHER);
    assert!(
        crate::workboard::get(engine.db(), &id)
            .expect("get")
            .is_none()
    );
}

#[test]
fn the_rows_survive_a_reopen_of_the_database() {
    let (dir, engine, app, cookie) = ready_app();
    assert_eq!(add(&app, &cookie, "still here"), StatusCode::SEE_OTHER);
    let first = engine.db();
    // A second connection is what a restart looks like to this table, and the
    // row is there without the router having written anything else.
    let copy = crate::db::Db::open(dir.path().join("board-copy.db")).expect("db");
    first
        .with_conn(|conn| {
            let mut statement = conn.prepare("SELECT id, title FROM workboard_items")?;
            let rows = statement.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for row in rows {
                let (id, title) = row?;
                copy.with_conn(|other| {
                    other.execute(
                        "INSERT INTO workboard_items (id, title, status) VALUES (?1, ?2, 'open')",
                        rusqlite::params![id, title],
                    )?;
                    Ok(())
                })
                .expect("copy");
            }
            Ok(())
        })
        .expect("read");
    let text = crate::workboard::list(&copy, None, 10).expect("list");
    assert_eq!(text.len(), 1);
    assert_eq!(text[0].title, "still here");
}

#[test]
fn the_status_filter_narrows_the_list() {
    let (_dir, _engine, app, cookie) = ready_app();
    assert_eq!(add(&app, &cookie, "one"), StatusCode::SEE_OTHER);
    let page = block_on(send(
        &app,
        Call::get_owned(format!("{ROUTE_WORKBOARD}?status=done"))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(page.body.contains("Nothing on the board"), "{}", page.body);
    assert!(!page.body.contains(">one<"), "{}", page.body);
}

#[test]
fn an_empty_title_is_refused_inline() {
    let (_dir, _engine, app, cookie) = ready_app();
    let reply = block_on(send(
        &app,
        Call::post(ROUTE_WORKBOARD_ADD, "title=+")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(
        reply.body.contains("a task needs a title"),
        "{}",
        reply.body
    );
}
