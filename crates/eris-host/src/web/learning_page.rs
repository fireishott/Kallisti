//! The learning page: what the loop learned, and the three controls.
//!
//! The claim is narrow: the page renders the ledger and the run log the loop
//! actually writes, and its buttons change exactly one row through the same
//! functions the worker uses.

use axum::http::StatusCode;

use crate::admin::{ROUTE_LEARNING, ROUTE_LEARNING_SAVE, ROUTE_LEARNING_SKILL};
use crate::learning;

use super::harness::{Call, block_on, ready_app, send};

/// A ready app whose skill library is inside the test's own temporary root.
///
/// `Shelf::for_engine` resolves the daemon's library from settings, and a test
/// that left the setting alone would write into the real `~/.config` library.
fn app_with_library() -> (
    tempfile::TempDir,
    crate::onboarding::SetupEngine,
    axum::Router,
    String,
) {
    let (dir, engine, app, cookie) = ready_app();
    let library = dir.path().join("skills");
    std::fs::create_dir_all(&library).expect("a test library");
    engine
        .set_setting(crate::skills::SETTING_DIR, &library.display().to_string())
        .expect("the library setting");
    (dir, engine, app, cookie)
}

fn seed_skill(engine: &crate::onboarding::SetupEngine, key: &str, description: &str) {
    let db = engine.db();
    learning::record_created(db, key, Some(description), None).expect("ledger row");
    learning::note_use(db, key).expect("a read");
}

#[test]
fn the_learning_page_lists_learned_skills_and_reviews() {
    let (_dir, engine, app, cookie) = app_with_library();
    let db = engine.db();
    seed_skill(&engine, "guard-first", "Ship behind the guard.");
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO learning_runs (id, conversation_id, answer_id, status, action, skill_key, summary) \
             VALUES ('run-1', 'conv-1', 'ans-1', 'done', 'create', 'guard-first', 'the operator corrected the order')",
            [],
        )
    })
    .expect("a run");

    let reply = block_on(send(
        &app,
        Call::get(ROUTE_LEARNING).html().cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let page = &reply.body;
    assert!(page.contains("Learning"), "{page}");
    assert!(page.contains("guard-first"), "{page}");
    assert!(page.contains("Ship behind the guard."), "{page}");
    assert!(page.contains("the operator corrected the order"), "{page}");
    assert!(page.contains("Run a pass now"), "{page}");
    // A learned skill needs a way out, and it is the same delete the skills page
    // performs: a link to that page's confirmation, not a second implementation.
    assert!(
        page.contains("/dashboard/skills?delete=guard-first"),
        "the row carries the delete link: {page}"
    );
}

#[test]
fn pinning_a_skill_changes_one_row_and_returns_to_the_page() {
    let (_dir, engine, app, cookie) = app_with_library();
    seed_skill(&engine, "guard-first", "Ship behind the guard.");

    let reply = block_on(send(
        &app,
        Call::post(ROUTE_LEARNING_SKILL, "key=guard-first&action=pin")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let location = reply.location().unwrap_or_default();
    assert!(location.starts_with(ROUTE_LEARNING), "{location}");
    assert!(location.contains("saved=skill"), "{location}");
    let row = learning::usage(engine.db(), "guard-first")
        .expect("db")
        .expect("a row");
    assert!(row.pinned);
}

#[test]
fn restoring_an_archived_skill_moves_it_back() {
    let (_dir, engine, app, cookie) = app_with_library();
    let shelf = learning::Shelf::for_engine(&engine).expect("a shelf");
    learning::create_skill(
        &shelf,
        "guard-first",
        "Guard First",
        "Ship behind the guard.",
        "## Procedure\n\nRun the guard first.\n",
    )
    .expect("the file");
    learning::record_created(
        engine.db(),
        "guard-first",
        Some("Ship behind the guard."),
        None,
    )
    .expect("ledger");
    learning::archive(&shelf, "guard-first").expect("archive");
    learning::mark_archived(engine.db(), "guard-first").expect("state");

    let reply = block_on(send(
        &app,
        Call::post(ROUTE_LEARNING_SKILL, "key=guard-first&action=restore")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert!(
        shelf.dir.join("guard-first").is_dir(),
        "the directory is back"
    );
    assert_eq!(
        learning::usage(engine.db(), "guard-first")
            .unwrap()
            .unwrap()
            .state,
        learning::State::Active
    );
}

#[test]
fn saving_the_switches_writes_the_three_rows() {
    let (_dir, engine, app, cookie) = app_with_library();
    let reply = block_on(send(
        &app,
        Call::post(
            ROUTE_LEARNING_SAVE,
            "learning_enabled=true&learning_review=false&learning_curator=true",
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let switches = learning::Switches::read(engine.db());
    assert!(switches.enabled);
    assert!(!switches.review);
    assert!(switches.curator);
}

#[test]
fn a_page_without_a_session_is_refused() {
    let (_dir, _engine, app, _cookie) = app_with_library();
    let reply = block_on(send(&app, Call::get(ROUTE_LEARNING).html()));
    assert!(
        reply.status == StatusCode::FORBIDDEN || reply.status == StatusCode::SEE_OTHER,
        "{}",
        reply.status
    );
}
