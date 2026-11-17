//! The skills page: the library, one editor, and the archive button.
//!
//! The claim is narrow and it is the whole page: every skill on disk is listed
//! with the file behind it, a save replaces that file through the same function
//! the device API calls, and a delete is a move into `.archive/` that takes two
//! presses and says so.

use std::path::{Path, PathBuf};

use axum::http::StatusCode;

use crate::admin::{ROUTE_SKILLS, ROUTE_SKILLS_DELETE, ROUTE_SKILLS_SAVE};
use crate::learning::ARCHIVE_DIR;

use super::harness::{Call, block_on, encode, ready_app, send};

/// A ready app whose skill library is inside the test's own temporary root.
///
/// Without the setting the library resolves to the daemon's real `~/.config`
/// skills, and a test would edit the operator's own shelf.
fn app_with_library() -> (
    tempfile::TempDir,
    crate::onboarding::SetupEngine,
    axum::Router,
    String,
    PathBuf,
) {
    let (dir, engine, app, cookie) = ready_app();
    let library = dir.path().join("skills");
    std::fs::create_dir_all(&library).expect("a test library");
    engine
        .set_setting(crate::skills::SETTING_DIR, &library.display().to_string())
        .expect("the library setting");
    (dir, engine, app, cookie, library)
}

/// One skill directory holding a complete `SKILL.md`, as the library holds them.
fn seed_skill(library: &Path, key: &str, name: &str, description: &str, body: &str) -> String {
    let directory = library.join(key);
    std::fs::create_dir_all(&directory).expect("the skill directory");
    let text = format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n");
    std::fs::write(directory.join(crate::skills::FILE_NAME), &text).expect("the skill file");
    text
}

#[test]
fn the_page_lists_a_skill_with_its_editor_and_its_delete_control() {
    let (_dir, _engine, app, cookie, library) = app_with_library();
    seed_skill(
        &library,
        "deploy",
        "Deploy",
        "Ship it behind the guard.",
        "1. run the guard",
    );

    let reply = block_on(send(
        &app,
        Call::get(ROUTE_SKILLS).html().cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let page = &reply.body;

    // The row: the key, the description the file carries, and the size.
    assert!(page.contains("deploy"), "{page}");
    assert!(page.contains("Ship it behind the guard."), "{page}");
    assert!(page.contains(" bytes"), "{page}");
    // The origin badge, which is the operator's for a skill the ledger never saw.
    assert!(page.contains("operator"), "{page}");
    assert!(!page.contains(">learned<"), "{page}");

    // The editor: one form, the route it posts to, and the whole file in the box.
    assert!(page.contains(ROUTE_SKILLS_SAVE), "{page}");
    assert!(page.contains("name=\"content\""), "{page}");
    assert!(page.contains("name: Deploy"), "{page}");

    // The delete control, posting to the archive route with the key.
    assert!(page.contains(ROUTE_SKILLS_DELETE), "{page}");
    assert!(page.contains("Delete"), "{page}");

    // The nav entry sits beside Learning, because the two answer the same
    // question from opposite ends.
    assert!(page.contains("href=\"/dashboard/learning\""), "{page}");
    assert!(page.contains("href=\"/dashboard/skills\""), "{page}");
}

#[test]
fn a_post_edits_the_file_and_a_second_post_deletes_the_skill() {
    let (_dir, _engine, app, cookie, library) = app_with_library();
    seed_skill(&library, "deploy", "Deploy", "Ship it.", "1. old step");

    let edited = "---\nname: Deploy\ndescription: Edited in the page.\n---\n\n1. brand new step\n";
    let body = format!("key=deploy&content={}", encode(edited));
    let save = block_on(send(
        &app,
        Call::post(ROUTE_SKILLS_SAVE, &body)
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(save.status, StatusCode::SEE_OTHER, "{}", save.body);
    let location = save.location().unwrap_or_default();
    assert!(location.starts_with(ROUTE_SKILLS), "{location}");
    assert!(location.contains("saved=write"), "{location}");
    assert_eq!(
        std::fs::read_to_string(library.join("deploy").join(crate::skills::FILE_NAME))
            .expect("the file"),
        edited,
        "the file is exactly what the form carried"
    );
    let page = block_on(send(
        &app,
        Call::get(ROUTE_SKILLS).html().cookie(Some(&cookie)),
    ));
    assert!(page.body.contains("Edited in the page."), "{}", page.body);

    // The first delete press only asks: nothing is removed, and the page says
    // exactly what a delete does here.
    let ask = block_on(send(
        &app,
        Call::post(ROUTE_SKILLS_DELETE, "key=deploy")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(ask.status, StatusCode::OK, "{}", ask.body);
    assert!(ask.body.contains("skills-confirm"), "{}", ask.body);
    assert!(
        ask.body.contains(">Delete ") || ask.body.contains("Delete <code>"),
        "the confirmation is titled a delete: {}",
        ask.body
    );
    assert!(
        ask.body.contains("and nothing else"),
        "the confirmation says the library is not the target: {}",
        ask.body
    );
    assert!(
        ask.body.contains("cannot be undone"),
        "the confirmation says it is permanent: {}",
        ask.body
    );
    assert!(
        library.join("deploy").is_dir(),
        "the first press writes nothing"
    );

    // The second press deletes it and returns to the list.
    let confirm = block_on(send(
        &app,
        Call::post(ROUTE_SKILLS_DELETE, "key=deploy&confirm=true")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(confirm.status, StatusCode::SEE_OTHER, "{}", confirm.body);
    let location = confirm.location().unwrap_or_default();
    assert!(location.starts_with(ROUTE_SKILLS), "{location}");
    assert!(location.contains("saved=deleted"), "{location}");
    assert!(
        !library.join("deploy").exists(),
        "the skill left the library"
    );
    assert!(
        !library.join(ARCHIVE_DIR).exists(),
        "a delete is not an archive: nothing lands in .archive/"
    );

    let page = block_on(send(
        &app,
        Call::get(ROUTE_SKILLS).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        !page.body.contains("Edited in the page."),
        "the archived skill is out of the list: {}",
        page.body
    );
    assert!(
        page.body.contains("No skills in this library"),
        "{}",
        page.body
    );
}

#[test]
fn a_delete_link_from_another_page_renders_the_confirmation() {
    let (_dir, _engine, app, cookie, library) = app_with_library();
    seed_skill(&library, "deploy", "Deploy", "Ship it.", "1. step");

    let ask = block_on(send(
        &app,
        Call::get("/dashboard/skills?delete=deploy")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(ask.status, StatusCode::OK, "{}", ask.body);
    assert!(ask.body.contains("skills-confirm"), "{}", ask.body);
    assert!(
        ask.body.contains("cannot be undone"),
        "the confirmation says it is permanent: {}",
        ask.body
    );
    assert!(
        library.join("deploy").is_dir(),
        "asking for the confirmation deletes nothing"
    );
}

#[test]
fn an_edit_that_names_no_skill_is_a_not_found() {
    let (_dir, _engine, app, cookie, _library) = app_with_library();
    let body = format!(
        "key=no-such-skill&content={}",
        encode("---\nname: X\n---\n\nbody\n")
    );
    let save = block_on(send(
        &app,
        Call::post(ROUTE_SKILLS_SAVE, &body)
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(save.status, StatusCode::NOT_FOUND, "{}", save.body);
    assert!(save.body.contains("skill_unknown"), "{}", save.body);

    let delete = block_on(send(
        &app,
        Call::post(ROUTE_SKILLS_DELETE, "key=no-such-skill&confirm=true")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(delete.status, StatusCode::NOT_FOUND, "{}", delete.body);
}

#[test]
fn an_empty_edit_is_refused_and_the_draft_stays_in_the_box() {
    let (_dir, _engine, app, cookie, library) = app_with_library();
    seed_skill(&library, "deploy", "Deploy", "Ship it.", "1. step");

    let save = block_on(send(
        &app,
        Call::post(ROUTE_SKILLS_SAVE, "key=deploy&content=+++")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(save.status, StatusCode::BAD_REQUEST, "{}", save.body);
    assert!(save.body.contains("skill_content_empty"), "{}", save.body);
    assert!(
        std::fs::read_to_string(library.join("deploy").join(crate::skills::FILE_NAME))
            .expect("the file")
            .contains("Ship it."),
        "a refused edit leaves the file alone"
    );
}

#[test]
fn a_page_without_a_session_is_refused() {
    let (_dir, _engine, app, _cookie, _library) = app_with_library();
    let reply = block_on(send(&app, Call::get(ROUTE_SKILLS).html()));
    assert!(
        reply.status == StatusCode::FORBIDDEN || reply.status == StatusCode::SEE_OTHER,
        "{}",
        reply.status
    );
    let post = block_on(send(
        &app,
        Call::post(ROUTE_SKILLS_DELETE, "key=deploy&confirm=true").html(),
    ));
    assert!(
        post.status == StatusCode::FORBIDDEN || post.status == StatusCode::SEE_OTHER,
        "{}",
        post.status
    );
}
