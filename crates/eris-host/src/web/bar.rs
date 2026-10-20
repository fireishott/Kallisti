//! The subagents control in the top bar, and the setting that arms it.
//!
//! Two claims, both checked against the bytes a browser receives:
//!
//! - the control is server-rendered from the `subagents` rows and appears only
//!   when there is something to report, so an operator with JavaScript blocked
//!   sees the same list a scripted page would, and a daemon that has dispatched
//!   nothing has a top bar exactly as wide as it was before;
//! - `agents.enabled` off removes `dispatch_agent` from what a `Full` turn is
//!   offered, which is asserted through the same function the turn calls rather
//!   than through a sentence on the settings page.

use axum::http::StatusCode;

use crate::admin::{ROUTE_CHAT, ROUTE_DASHBOARD, ROUTE_SETTINGS, ROUTE_SETTINGS_TOOLS};
use crate::onboarding::permissions::SandboxMode;
use crate::subagents;

use super::harness::{Call, block_on, ready_app, send};
use super::templates::{Chrome, Nav};

/// The attributes that mark the control in rendered bytes. Read here rather than
/// from a template field so the assertions are about what a browser gets.
const CONTROL: &str = r#"<details class="subagents">"#;
const RESULT: &str = r#"class="subagent-result""#;

/// Put one row in the table the way a turn would, then return its id.
fn dispatched(db: &crate::db::Db, label: &str, prompt: &str, turn: &str) -> String {
    subagents::reserve(
        db,
        &subagents::Request {
            conversation: crate::web::chat::CONVERSATION_ID,
            parent_turn: Some(turn),
            label,
            prompt,
            model: "stub/model",
        },
    )
    .expect("a reserved row")
}

/// The names a `Full` turn is offered, with the operator's switch applied.
fn full_offering(agents: bool) -> Vec<String> {
    crate::tools::specs_offering(SandboxMode::Full, agents)
        .into_iter()
        .map(|spec| spec.name)
        .collect()
}

#[test]
fn the_bar_is_absent_until_something_is_dispatched() {
    let (_dir, _engine, app, cookie) = ready_app();
    for route in [ROUTE_DASHBOARD, ROUTE_CHAT] {
        let page = block_on(send(&app, Call::get(route).html().cookie(Some(&cookie))));
        assert_eq!(page.status, StatusCode::OK, "{route}: {}", page.body);
        assert!(
            !page.body.contains(CONTROL),
            "{route} renders no subagents control when there is nothing to report: {}",
            page.body
        );
    }
}

#[test]
fn the_bar_lists_a_finished_subagent_with_its_status_model_and_result() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    let id = dispatched(db, "nightly report", "summarise yesterday", "answer-1");
    let outcome = subagents::Outcome::succeeded(
        "the report is three pages and nothing is on fire",
        &core_domain::Usage::reconcile(
            core_domain::CacheReadReporting::NotReported,
            Some(20),
            None,
            Some(9),
        ),
    );
    assert!(subagents::finish(db, &id, subagents::Status::Succeeded, &outcome).expect("settled"));

    for route in [ROUTE_DASHBOARD, ROUTE_CHAT] {
        let page = block_on(send(&app, Call::get(route).html().cookie(Some(&cookie))));
        assert_eq!(page.status, StatusCode::OK, "{route}: {}", page.body);
        let body = &page.body;
        assert!(
            body.contains(CONTROL),
            "{route} carries the control: {body}"
        );
        assert!(
            body.contains("<summary"),
            "{route} opens as a disclosure: {body}"
        );
        assert!(
            body.contains(">nightly report<"),
            "{route} names the subagent: {body}"
        );
        assert!(
            body.contains(">succeeded<"),
            "{route} states the status in words: {body}"
        );
        assert!(
            body.contains(">stub/model<"),
            "{route} states the model: {body}"
        );
        assert!(
            body.contains(RESULT),
            "{route} renders the result in its own block: {body}"
        );
        assert!(
            body.contains("the report is three pages and nothing is on fire"),
            "{route} carries the result text: {body}"
        );
        // A subagent has no page, so the control links nowhere.
        let start = body.find(CONTROL).expect("the control");
        let window = &body[start..];
        let end = window.find("</details>").unwrap_or(window.len());
        assert!(
            !window[..end].contains("<a "),
            "{route} links nothing from the bar: {}",
            &window[..end]
        );
        // And it is server-rendered: the page needs no script to show it.
        assert!(
            !window[..end].contains("<script"),
            "{route} needs no script for the bar"
        );
        assert!(
            !window[..end].contains('!'),
            "{route} carries no exclamation mark in the bar"
        );
    }

    // The conversation still keeps clear of the section row: the control is the
    // only thing in that row on chat.
    let chat = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(chat.status, StatusCode::OK, "{}", chat.body);
    assert!(
        !chat.body.contains(r#"class="sections""#),
        "chat still carries no section chips: {}",
        chat.body
    );
}

#[test]
fn a_running_subagent_is_reported_as_running_in_words() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    let id = dispatched(db, "triage", "read the alert", "answer-2");
    assert!(subagents::mark_running(db, &id).expect("claimed"));

    let page = block_on(send(
        &app,
        Call::get(ROUTE_DASHBOARD).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        page.body.contains(r#"class="subagent-status running""#),
        "the status is the row's own: {}",
        page.body
    );
    assert!(
        page.body.contains("1 running"),
        "the summary counts what is still working: {}",
        page.body
    );
}

/// The bar is a live view, not a log: a row that finished long ago ages out on
/// its own.
#[test]
fn a_finished_row_older_than_the_window_ages_out_of_the_bar() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    let id = dispatched(db, "old news", "something from this morning", "answer-3");
    let outcome = subagents::Outcome::failed("stub", "nothing came back");
    assert!(subagents::finish(db, &id, subagents::Status::Failed, &outcome).expect("settled"));
    // Two hours, against a window measured in minutes.
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE subagents
                SET created_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-2 hours'),
                    finished_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-2 hours')
              WHERE id = ?1",
            rusqlite::params![id],
        )
    })
    .expect("aged");

    let page = block_on(send(
        &app,
        Call::get(ROUTE_DASHBOARD).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        !page.body.contains(CONTROL),
        "an old row is not reported: {}",
        page.body
    );
    assert_eq!(
        subagents::bar(db, subagents::BAR_LIMIT)
            .expect("rows")
            .len(),
        0,
        "the query agrees with the page"
    );
}

/// The switch, end to end: the form writes the setting, the setting removes the
/// tool from the list a `Full` turn is offered, and the page shows it off.
#[test]
fn turning_dispatching_off_removes_it_from_the_full_offering() {
    let (_dir, engine, app, cookie) = ready_app();

    assert!(
        full_offering(subagents::enabled(None)).contains(&subagents::TOOL.to_owned()),
        "dispatching is offered by default in Full mode"
    );

    let off = block_on(send(
        &app,
        Call::post(
            ROUTE_SETTINGS_TOOLS,
            "tools_enabled=true&tool_log=full&tool_rounds=12",
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(off.status, StatusCode::SEE_OTHER, "{}", off.body);

    let stored = engine
        .setting(subagents::SETTING_AGENTS_ENABLED)
        .expect("readable")
        .expect("written");
    assert_eq!(
        stored, "false",
        "an unchecked box writes the explicit false"
    );
    let offering = full_offering(subagents::enabled(Some(&stored)));
    assert!(
        !offering.contains(&subagents::TOOL.to_owned()),
        "agents.enabled off means the tool is not in specs(Full): {offering:?}"
    );
    // Everything else a Full turn may call is untouched.
    assert!(
        offering.contains(&"run_command".to_owned()),
        "the switch is not a sandbox change: {offering:?}"
    );

    let page = block_on(send(
        &app,
        Call::get(ROUTE_SETTINGS).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let at = page
        .body
        .find(r#"id="agents_enabled""#)
        .expect("the box is on the page");
    let tag = &page.body[at..];
    let end = tag.find('>').expect("the tag closes");
    assert!(
        !tag[..end].contains("checked"),
        "the box shows off after saving off: {}",
        &tag[..end]
    );
    // The settings page still holds no password field: the box is a box.
    assert!(
        !page.body.contains(r#"type="password""#),
        "the settings page holds no password field"
    );

    let on = block_on(send(
        &app,
        Call::post(
            ROUTE_SETTINGS_TOOLS,
            "tools_enabled=true&tool_log=full&tool_rounds=12&agents_enabled=true",
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(on.status, StatusCode::SEE_OTHER, "{}", on.body);
    let stored = engine
        .setting(subagents::SETTING_AGENTS_ENABLED)
        .expect("readable")
        .expect("written");
    assert_eq!(stored, "true");
    assert!(
        full_offering(subagents::enabled(Some(&stored))).contains(&subagents::TOOL.to_owned()),
        "the tool comes back when the box is ticked"
    );
    let page = block_on(send(
        &app,
        Call::get(ROUTE_SETTINGS).html().cookie(Some(&cookie)),
    ));
    assert!(
        page.body.contains("dispatch_agent"),
        "the page names what the switch controls: {}",
        page.body
    );
}

/// The four sections a `Full` turn is offered, and the fact that a confined one
/// is offered neither dispatching nor commands.
#[test]
fn dispatching_is_offered_only_in_full_mode() {
    assert!(full_offering(true).contains(&subagents::TOOL.to_owned()));
    for mode in [SandboxMode::ReadOnly, SandboxMode::WorkspaceWrite] {
        let names: Vec<String> = crate::tools::specs_offering(mode, true)
            .into_iter()
            .map(|spec| spec.name)
            .collect();
        assert!(
            !names.contains(&subagents::TOOL.to_owned()),
            "{mode:?} is offered no dispatching: {names:?}"
        );
    }
}

/// The control is rendered from resolved rows and nothing else, so a page built
/// without a request still says what the table says. The shell is the seam.
#[test]
fn the_control_reads_only_the_rows_it_is_given() {
    let empty = Chrome::default();
    let page = crate::web::templates::shell(
        "Dashboard",
        crate::db::SetupState::Ready,
        Some(Nav::Dashboard),
        maud::Markup::default(),
        &empty,
    )
    .into_string();
    assert!(
        page.contains(r#"class="sections""#),
        "the shell still carries the section row: {page}"
    );
    assert!(
        !page.contains(CONTROL),
        "an empty list renders no control: {page}"
    );
    assert!(
        page.contains(r#"class="top-nav""#),
        "the row itself is unchanged: {page}"
    );
}

/// The id and the label a state change carries are what the stream sends, so a
/// live page can recount without reading a result.
#[test]
fn a_report_carries_only_what_the_bar_needs() {
    let report = subagents::Report {
        id: "row-1",
        label: "triage",
        status: subagents::Status::Running.as_str(),
    };
    let frame = super::routes::subagent_frame(&report);
    assert_eq!(frame["id"], "row-1");
    assert_eq!(frame["label"], "triage");
    assert_eq!(frame["status"], "running");
    assert_eq!(
        frame.as_object().map(serde_json::Map::len),
        Some(3),
        "a subagent frame carries no result and no third-party field"
    );
}
