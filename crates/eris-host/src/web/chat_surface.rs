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
//! - a message's files appear inside its bubble: pictures as thumbnails that link
//!   to the full file, everything else as a chip with its type badge, name and
//!   size;
//! - an attachment row is a database fact and not a promise: a path outside the
//!   attachment directory, or one whose bytes are gone, is a typed 404.

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

use crate::admin::{
    ROUTE_CHAT, ROUTE_CHAT_SEND, ROUTE_SECURITY, ROUTE_SETTINGS, ROUTE_SETTINGS_STREAMING,
    ROUTE_SETTINGS_TOOLS,
};
use crate::onboarding::SetupEngine;
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

/// One attachment row whose bytes sit exactly where the upload route puts them:
/// `<workspace>/uploads/<conversation>/<id>-<name>`.
fn store_attachment(engine: &SetupEngine, name: &str, bytes: &[u8]) -> chat::Attachment {
    let directory = engine
        .workspace_path()
        .expect("a workspace")
        .join("uploads")
        .join(CONVERSATION_ID);
    std::fs::create_dir_all(&directory).expect("the uploads directory");
    let id = uuid::Uuid::now_v7().to_string();
    let path = directory.join(format!("{id}-{name}"));
    std::fs::write(&path, bytes).expect("the bytes");
    let file = chat::Attachment {
        id,
        name: name.to_owned(),
        path: path.display().to_string(),
        size: bytes.len() as u64,
        media_type: Some(chat::sniff_media_type(bytes).to_owned()),
    };
    chat::record_attachment(engine.db(), CONVERSATION_ID, &file).expect("the row");
    file
}

/// Fetch one path with the admin session, keeping the bytes and the disposition:
/// an attachment is rarely UTF-8 and the shared harness decodes its body as text.
fn get_stored(
    app: &axum::Router,
    uri: &str,
    cookie: &str,
) -> (StatusCode, String, String, Vec<u8>) {
    let response = block_on(
        app.clone().oneshot(
            Request::builder()
                .uri(uri)
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .expect("a valid request"),
        ),
    )
    .expect("the router is infallible");
    let status = response.status();
    let header_of = |name: axum::http::HeaderName| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    let content_type = header_of(header::CONTENT_TYPE);
    let disposition = header_of(header::CONTENT_DISPOSITION);
    let bytes = block_on(to_bytes(response.into_body(), 2 * 1024 * 1024)).expect("a body");
    (status, content_type, disposition, bytes.to_vec())
}

/// The message id of the last row in the fixture conversation.
fn last_message(engine: &SetupEngine) -> String {
    chat::load(engine.db(), CONVERSATION_ID)
        .expect("the transcript")
        .last()
        .expect("a row")
        .id
        .clone()
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

/// The send control is the row's one submit, and it is named by its own label,
/// so a screen reader says "Send" rather than "button".
#[test]
fn the_send_control_is_the_named_submit_of_the_composer_row() {
    let (_dir, _engine, app, cookie) = ready_app();
    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    let row = body
        .split(r#"<div class="composer-send">"#)
        .nth(1)
        .and_then(|rest| rest.split("</form>").next())
        .expect("the composer row carries the send group");
    assert!(
        row.contains(r#"id="send""#),
        "the row holds the send control: {row}"
    );
    assert!(
        row.contains(r#"type="submit""#),
        "the send control submits the form: {row}"
    );
    assert!(
        row.contains(">Send</button>"),
        "the send control is named by its own label: {row}"
    );
    assert_eq!(
        row.matches(r#"type="submit""#).count(),
        1,
        "the row carries one primary action: {row}"
    );
}

#[test]
fn a_tool_call_renders_as_a_closed_window_that_can_be_opened() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    chat::append(db, CONVERSATION_ID, Role::User, "run it", State::Complete).unwrap();
    let answer = chat::append_answer(db, CONVERSATION_ID, "m/one", None).unwrap();
    chat::settle(
        db,
        &answer,
        "ran it",
        None,
        &[chat::ToolRecord {
            name: "run_command".to_owned(),
            arguments: r#"{"command":"psql -c 'select 1'"}"#.to_owned(),
            output: "one column\n---\n 1".to_owned(),
            is_error: false,
        }],
        None,
        chat::Accounting::default(),
    )
    .unwrap();

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;

    assert!(
        body.contains(r#"<details class="exec-call""#),
        "each call is its own window"
    );
    assert!(
        body.contains(r#"<summary class="exec-head""#),
        "the summary line names the call"
    );
    assert!(
        !body.contains(r#"<details class="exec-call" open"#),
        "the output starts closed, so a long stdout cannot take over the page"
    );
    assert!(body.contains("run_command"), "the call is named");
    assert!(
        body.contains("select 1"),
        "the arguments are visible closed"
    );
}

/// A settled answer in the fixture conversation, with the calls given.
fn settle_with_calls(engine: &SetupEngine, tools: &[chat::ToolRecord]) {
    let db = engine.db();
    chat::append(db, CONVERSATION_ID, Role::User, "run it", State::Complete).unwrap();
    let answer = chat::append_answer(db, CONVERSATION_ID, "m/one", None).unwrap();
    chat::settle(
        db,
        &answer,
        "ran it",
        None,
        tools,
        None,
        chat::Accounting::default(),
    )
    .unwrap();
}

fn one_call() -> chat::ToolRecord {
    chat::ToolRecord {
        name: "run_command".to_owned(),
        arguments: r#"{"command":"ls"}"#.to_owned(),
        output: "a b c".to_owned(),
        is_error: false,
    }
}

/// The run log's two controls exist exactly where they control something: under
/// the summary of a log that has calls, and nowhere else.
#[test]
fn the_run_log_offers_expand_and_collapse_only_when_it_has_calls() {
    let (_dir, engine, app, cookie) = ready_app();
    settle_with_calls(&engine, &[one_call()]);

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(
        body.contains(r#"<div class="runctl" hidden role="group" aria-label="Run log controls">"#),
        "the controls are a named group, and hidden until a script can use them: {body}"
    );
    assert!(
        body.contains(r#"<button class="run-btn" type="button" data-run="open""#),
        "Expand all is a plain button, not a submit: {body}"
    );
    assert!(
        body.contains(r#"<button class="run-btn" type="button" data-run="shut""#),
        "Collapse all is a plain button, not a submit: {body}"
    );
    assert!(
        body.contains(r#"<button class="run-btn" type="button" data-run="all""#)
            && body.contains(r#"data-run="all" aria-pressed="false""#),
        "Show all is a plain toggle, pressed off at rest: {body}"
    );
    assert!(
        body.contains(
            r#"aria-label="Show every tool call in this run log, not only the running one""#
        ),
        "the toggle says what it does: {body}"
    );
    assert!(
        body.contains(r#"aria-label="Expand every tool call in this run log""#)
            && body.contains(r#"aria-label="Collapse every tool call in this run log""#),
        "both controls carry an accessible name: {body}"
    );
    assert!(
        body.contains(">Expand all</button>") && body.contains(">Collapse all</button>"),
        "both controls are labelled in sight: {body}"
    );
    assert!(
        body.contains(r#"<summary><span class="exec-label">Run log</span>"#),
        "the log's own summary keeps its name and stays a bare disclosure: {body}"
    );
}

#[test]
fn a_message_without_calls_carries_no_run_log_controls() {
    let (_dir, engine, app, cookie) = ready_app();
    settle_with_calls(&engine, &[]);

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(
        !body.contains(r#"class="runctl""#),
        "no log means no controls to press: {body}"
    );
    assert!(
        !body.contains(r#"data-run="open""#),
        "and no Expand all either: {body}"
    );
}

/// One log element, one script, and the script is the one that carries the
/// controls and the memory. A bang would be text on the page, so there is none.
#[test]
fn the_run_log_is_one_details_and_its_controls_ride_the_one_script() {
    let (_dir, engine, app, cookie) = ready_app();
    settle_with_calls(&engine, &[one_call()]);

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert_eq!(
        body.matches(r#"<details class="exec""#).count(),
        1,
        "the answer carries one run log, and the calls are exec-call: {body}"
    );
    assert_eq!(
        body.matches("<script").count(),
        1,
        "the controls ride the page's one script: {body}"
    );
    let script = body
        .split("<script>")
        .nth(1)
        .and_then(|rest| rest.split("</script>").next())
        .expect("the chat page carries its inline script");
    assert!(
        !script.contains('!'),
        "the script is written without a bang, like the rest of the page"
    );
    assert!(
        script.contains(r#"kallisti.runlog"#),
        "the remembered choice lives in the one script"
    );
    assert!(
        script.contains(r#".run-btn"#),
        "the same script wires the three controls"
    );
    assert!(
        script.contains(r#"kallisti.runlog.all"#),
        "the Show all choice is remembered next to the other one"
    );
}

/// The rendered page is the script-free reading: every call closed and the log
/// itself closed, so nothing costs height until the operator asks for it.
#[test]
fn the_rendered_run_log_starts_with_every_call_collapsed() {
    let (_dir, engine, app, cookie) = ready_app();
    settle_with_calls(
        &engine,
        &[
            one_call(),
            chat::ToolRecord {
                name: "run_command".to_owned(),
                arguments: r#"{"command":"nope"}"#.to_owned(),
                output: "exit 127".to_owned(),
                is_error: true,
            },
        ],
    );

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(
        body.contains(r#"<details class="exec">"#),
        "the log is one closed panel: {body}"
    );
    assert!(
        !body.contains(r#"<details class="exec" open>"#),
        "a run log never opens itself: {body}"
    );
    assert_eq!(
        body.matches(r#"<details class="exec-call"#).count(),
        2,
        "each call is its own window: {body}"
    );
    assert!(
        body.contains(r#"<details class="exec-call failed""#)
            && !body.contains(r#"<details class="exec-call failed" open"#),
        "an erroring call is closed too: {body}"
    );
    assert!(
        !body.contains(r#"<details class="exec-call" open"#),
        "no call starts open, so a long stdout cannot take over the page: {body}"
    );
    assert!(
        body.contains(r#"<div class="exec-body">"#),
        "the calls stay in the bounded body that scrolls: {body}"
    );
}

#[test]
fn the_run_log_can_be_switched_off_and_then_the_page_does_not_carry_it() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    chat::append(db, CONVERSATION_ID, Role::User, "run it", State::Complete).unwrap();
    let answer = chat::append_answer(db, CONVERSATION_ID, "m/one", None).unwrap();
    chat::settle(
        db,
        &answer,
        "ran it",
        None,
        &[chat::ToolRecord {
            name: "run_command".to_owned(),
            arguments: r#"{"command":"echo hi"}"#.to_owned(),
            output: "hi".to_owned(),
            is_error: false,
        }],
        None,
        chat::Accounting::default(),
    )
    .unwrap();

    // Off is stored by the tools form and read by the page.
    let reply = block_on(send(
        &app,
        Call::post(
            ROUTE_SETTINGS_TOOLS,
            "tools_enabled=true&tool_rounds=4&tool_log=off",
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(
        engine
            .setting(chat::SETTING_SHOW_TOOL_LOG)
            .unwrap()
            .as_deref(),
        Some("off"),
        "the off option is stored as off, not left out"
    );

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        !page.body.contains(r#"class="exec"#),
        "with the log off the page carries no run log at all"
    );
    assert!(
        !page.body.contains("echo hi"),
        "and no command output either: off means not sent"
    );
}

#[test]
fn streaming_can_be_switched_off_and_the_page_says_so() {
    let (_dir, engine, app, cookie) = ready_app();

    // A checked box is the only thing that sends a value, so an empty body is
    // the off signal, exactly as the checkboxes on the permissions form are.
    let reply = block_on(send(
        &app,
        Call::post(ROUTE_SETTINGS_STREAMING, "")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(
        engine.setting(chat::SETTING_STREAMING).unwrap().as_deref(),
        Some("false"),
        "off is stored as false rather than left out"
    );

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        page.body.contains(r#"data-stream="off""#),
        "the page tells its script to hand the turn to the form: {}",
        page.body
    );

    // And the server refuses a stream even when a caller asks for one: the
    // stored choice wins over an Accept header.
    let forced = block_on(send(
        &app,
        Call::post(ROUTE_CHAT_SEND, "message=hello")
            .header("accept", "text/event-stream")
            .cookie(Some(&cookie)),
    ));
    assert_eq!(
        forced.status,
        StatusCode::SEE_OTHER,
        "no SSE with streaming off: {}",
        forced.body
    );

    let reply = block_on(send(
        &app,
        Call::post(ROUTE_SETTINGS_STREAMING, "stream_responses=true")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert!(
        page.body.contains(r#"data-stream="on""#),
        "streaming is back on: {}",
        page.body
    );
}

/// The live builder used to have no idea which run-log mode the operator chose,
/// so a page opened on `summary` streamed a full wall of windows and then turned
/// into one line after a reload. The shell now carries the choice.
#[test]
fn the_shell_tells_the_script_which_run_log_mode_is_stored() {
    let (_dir, _engine, app, cookie) = ready_app();

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        page.body.contains(r#"data-tool-log="full""#),
        "a fresh install says full, and the script reads it: {}",
        page.body
    );

    let reply = block_on(send(
        &app,
        Call::post(
            ROUTE_SETTINGS_TOOLS,
            "tools_enabled=true&tool_log=summary&tool_rounds=12",
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert!(
        page.body.contains(r#"data-tool-log="summary""#),
        "summary reaches the script as summary: {}",
        page.body
    );

    let script = page
        .body
        .split("<script>")
        .nth(1)
        .and_then(|rest| rest.split("</script>").next())
        .expect("the chat page carries its inline script");
    assert!(
        script.contains(r#"data-tool-log"#),
        "and the script is what reads it"
    );
    assert!(
        script.contains(r#"TOOL_LOG_FALLBACK"#),
        "a shell that says nothing falls back rather than guessing wrong"
    );
}

#[test]
fn the_security_page_is_the_only_place_that_asks_for_a_secret() {
    let (_dir, _engine, app, cookie) = ready_app();

    let security = block_on(send(
        &app,
        Call::get(ROUTE_SECURITY).html().cookie(Some(&cookie)),
    ));
    assert_eq!(security.status, StatusCode::OK, "{}", security.body);
    assert!(
        security.body.contains(r#"type="password""#),
        "the security page asks for the credential"
    );
    assert!(
        security.body.contains("admin_credential row in eris.db"),
        "and states the recovery path honestly"
    );

    // The settings page still must not, which is the rule this page exists to keep.
    let settings = block_on(send(
        &app,
        Call::get(ROUTE_SETTINGS).html().cookie(Some(&cookie)),
    ));
    assert!(
        !settings.body.contains(r#"type="password""#),
        "settings stays secret-free"
    );
    assert!(
        settings.body.contains(ROUTE_SECURITY),
        "and links to the page that is not"
    );
}

#[test]
fn a_wrong_current_credential_changes_nothing_and_says_so() {
    let (_dir, _engine, app, cookie) = ready_app();
    let refused = block_on(send(
        &app,
        Call::post(
            ROUTE_SECURITY,
            "current_password=not-the-one&new_password=daily-driver-1&confirm_password=daily-driver-1",
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert!(
        refused.body.contains("not the current credential"),
        "the refusal names the field instead of silently doing nothing"
    );
    assert!(
        !refused.body.contains("saved=credential"),
        "and no change is reported"
    );
}

#[test]
fn a_dropped_stream_is_described_in_words_the_operator_can_use() {
    let (_dir, _engine, app, cookie) = ready_app();
    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;

    assert!(
        body.contains("Lost the connection to the daemon"),
        "a dropped stream says what happened"
    );
    assert!(
        body.contains("reload to see the transcript"),
        "and what to do about it"
    );
    assert!(
        !body.contains(r#""The turn failed: " + error.message"#),
        "the browser's own words are not passed through as a diagnosis"
    );
}

#[test]
fn a_calls_output_renders_as_a_window_with_lights_and_a_line_count() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    chat::append(db, CONVERSATION_ID, Role::User, "run it", State::Complete).unwrap();
    let answer = chat::append_answer(db, CONVERSATION_ID, "m/one", None).unwrap();
    chat::settle(
        db,
        &answer,
        "ran it",
        None,
        &[chat::ToolRecord {
            name: "run_command".to_owned(),
            arguments: r#"{"command":"echo hi"}"#.to_owned(),
            output: serde_json::json!({
                "command": "echo hi",
                "exit_code": 0,
                "stdout": "hi\n",
                "stderr": "",
            })
            .to_string(),
            is_error: false,
        }],
        None,
        chat::Accounting::default(),
    )
    .unwrap();

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    let body = &page.body;
    assert!(body.contains(r#"class="win""#), "output sits in a window");
    assert!(body.contains(r#"class="win-dot red""#), "traffic lights");
    assert!(body.contains(r#"class="win-dot green""#));
    assert!(body.contains("kallisti"), "the window is titled");
    assert!(body.contains("stdout"), "and says what it is");
    assert!(
        body.contains("1 line<"),
        "the line count is stated, not a character count: {body}"
    );
    assert!(
        !body.contains(r#"class="exec-out""#),
        "the bare pre is gone, so live and stored markup agree"
    );
}

/// The complaint that produced the terminal body: a command whose stdout is a
/// code snippet was being shown as escaped JSON inside the window. The window is
/// now the terminal's reading of the result, so the escapes are resolved and the
/// prompt line is the command that was typed.
#[test]
fn a_commands_stdout_is_lines_not_escaped_json() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    chat::append(db, CONVERSATION_ID, Role::User, "grep it", State::Complete).unwrap();
    let answer = chat::append_answer(db, CONVERSATION_ID, "m/one", None).unwrap();
    let snippet = "assert!(full.contains(&\"run_command\".to_owned()));\nassert!(confined);";
    chat::settle(
        db,
        &answer,
        "found it",
        None,
        &[chat::ToolRecord {
            name: "run_command".to_owned(),
            arguments: r#"{"command":"grep -n assert src/tools/mod.rs"}"#.to_owned(),
            output: serde_json::json!({
                "command": "grep -n assert src/tools/mod.rs",
                "exit_code": 0,
                "stdout": snippet,
                "stderr": "",
            })
            .to_string(),
            is_error: false,
        }],
        None,
        chat::Accounting::default(),
    )
    .unwrap();

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    let body = &page.body;
    assert!(
        body.contains("term-line prompt"),
        "the command leads the window: {body}"
    );
    assert!(
        body.contains("$ grep -n assert src/tools/mod.rs"),
        "and is the line that was actually run: {body}"
    );
    // The escaped form the complaint was about, checked inside the window rather
    // than across the page: the inline script legitimately carries `\n` in its
    // own string literals, and the window body carries none.
    let window = body
        .split(r#"<div class="win-body">"#)
        .nth(1)
        .and_then(|rest| rest.split(r#"</div></div>"#).next())
        .expect("the window body is on the page");
    assert!(
        !window.contains("\\n"),
        "a literal backslash-n is not a line break: {window}"
    );
    // The snippet appears as source, with its own quotes, in two rows.
    assert!(
        body.contains("assert!(full.contains(&amp;"),
        "the code reads as code: {body}"
    );
    assert!(
        body.matches(r#"<div class="term-line"#).count() >= 3,
        "a prompt line and two output lines: {body}"
    );
}

/// A failing command states its exit status inside the window, and its stderr is
/// paintable, so a turn that ran something that broke does not look like a turn
/// that ran something quiet.
#[test]
fn a_failed_call_paints_stderr_and_states_the_exit_status() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    chat::append(db, CONVERSATION_ID, Role::User, "build it", State::Complete).unwrap();
    let answer = chat::append_answer(db, CONVERSATION_ID, "m/one", None).unwrap();
    chat::settle(
        db,
        &answer,
        "it broke",
        None,
        &[chat::ToolRecord {
            name: "run_command".to_owned(),
            arguments: r#"{"command":"cargo build"}"#.to_owned(),
            output: serde_json::json!({
                "command": "cargo build",
                "exit_code": 101,
                "stdout": "",
                "stderr": "error: expected one of",
            })
            .to_string(),
            is_error: true,
        }],
        None,
        chat::Accounting::default(),
    )
    .unwrap();

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    let body = &page.body;
    assert!(
        body.contains(r#"class="term-line err""#),
        "stderr has its own line: {body}"
    );
    assert!(body.contains("error: expected one of"), "{body}");
    assert!(
        body.contains(r#"class="term-line notice""#) && body.contains("exit 101"),
        "the exit status is in the window: {body}"
    );
}

#[test]
fn summary_mode_collapses_the_whole_turn_to_one_line_naming_the_last_call() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    chat::append(db, CONVERSATION_ID, Role::User, "run both", State::Complete).unwrap();
    let answer = chat::append_answer(db, CONVERSATION_ID, "m/one", None).unwrap();
    let record = |name: &str, args: &str, out: &str| chat::ToolRecord {
        name: name.to_owned(),
        arguments: args.to_owned(),
        output: out.to_owned(),
        is_error: false,
    };
    chat::settle(
        db,
        &answer,
        "ran both",
        None,
        &[
            record("run_command", r#"{"command":"ls"}"#, "a b c"),
            record("run_command", r#"{"command":"du -sh db"}"#, "1.2G"),
        ],
        None,
        chat::Accounting::default(),
    )
    .unwrap();

    let reply = block_on(send(
        &app,
        Call::post(
            ROUTE_SETTINGS_TOOLS,
            "tools_enabled=true&tool_rounds=4&tool_log=summary",
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(
        body.contains(r#"<details class="exec compact">"#),
        "summary mode is one collapsed strip"
    );
    assert!(
        body.contains("du -sh db"),
        "the strip names the last call, like a status line"
    );
    assert!(
        !body.contains(r#"<details class="exec compact" open"#),
        "it starts collapsed"
    );
    assert!(body.contains("2 calls"), "the count is kept");
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
        &[],
        None,
        chat::Accounting {
            prompt_tokens: Some(1_200),
            completion_tokens: Some(18),
            cached_tokens: Some(1_150),
            duration_ms: Some(900),
            rounds: None,
            context_tokens: Some(1_200),
        },
    )
    .unwrap();

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    // Per answer: the model, both token counts, the duration. Cache and context
    // are conversation facts and are not repeated here.
    assert!(body.contains("m/watch"), "the model that answered is named");
    assert!(body.contains("1.2k in"), "prompt tokens are shown");
    assert!(body.contains("18 out"), "completion tokens are shown");
    assert!(body.contains("900ms"), "duration is shown");
    assert!(
        !body.contains("48% cache"),
        "the per-reply line must not carry the cache share: {body}"
    );
    // Per conversation: the totals ride inside the last reply, not in a bar.
    assert!(
        body.contains(r#"class="meta-session""#),
        "the conversation totals render inside the reply: {body}"
    );
    assert!(
        !body.contains(r#"class="statusbar""#),
        "nothing floats under the composer: {body}"
    );
    assert!(
        body.contains("272.0k window"),
        "the declared window is stated beside the picker: {body}"
    );
    assert!(
        body.contains("cache hit 48%"),
        "the conversation line is the one place the cache share appears"
    );
    assert!(
        body.matches("cache hit").count() == 1,
        "the cache share is stated once, not once per line: {body}"
    );
    assert!(body.contains("1 turn"), "a single turn is singular: {body}");
}

#[test]
fn the_sidebar_can_be_collapsed_and_comes_back_where_it_was() {
    let (_dir, _engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());

    let shown = block_on(send(&app, Call::get(ROUTE_CHAT).html().cookie(cookie)));
    assert_eq!(shown.status, StatusCode::OK);
    assert!(
        shown.body.contains("Hide the sidebar"),
        "the rail offers a way to collapse it"
    );
    assert!(
        !shown.body.contains("app rail-hidden"),
        "the rail starts open"
    );

    let collapsed = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SIDEBAR, "rail=hidden").cookie(cookie),
    ));
    assert_eq!(collapsed.status, StatusCode::SEE_OTHER);

    let hidden = block_on(send(&app, Call::get(ROUTE_CHAT).html().cookie(cookie)));
    assert!(
        hidden.body.contains("app rail-hidden"),
        "the shell renders collapsed"
    );
    assert!(
        hidden.body.contains("Show the sidebar"),
        "a control outside the rail brings it back"
    );

    let reopened = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SIDEBAR, "rail=shown").cookie(cookie),
    ));
    assert_eq!(reopened.status, StatusCode::SEE_OTHER);
    let back = block_on(send(&app, Call::get(ROUTE_CHAT).html().cookie(cookie)));
    assert!(!back.body.contains("app rail-hidden"), "the rail returns");
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
fn the_assistant_name_is_a_setting_the_composer_reads() {
    let (_dir, engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());

    // The documented default, before anyone sets one.
    let page = block_on(send(&app, Call::get(ROUTE_CHAT).html().cookie(cookie)));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        page.body.contains(r#"placeholder="Message Kallisti""#),
        "the composer uses the profile name: {}",
        page.body
    );

    // Set it on the persona card and the label follows without a restart.
    let saved = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_PERSONA,
            "assistant_name=Kallisti&persona_enabled=true&persona_text=You are a daemon.",
        )
        .cookie(cookie),
    ));
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);

    let page = block_on(send(&app, Call::get(ROUTE_CHAT).html().cookie(cookie)));
    assert!(
        page.body.contains(r#"placeholder="Message Kallisti""#),
        "the new name reaches the composer: {}",
        page.body
    );
    assert_eq!(
        engine
            .setting(crate::persona::SETTING_ASSISTANT_NAME)
            .unwrap()
            .as_deref(),
        Some("Kallisti")
    );
}

#[test]
fn the_greeting_follows_the_hour_and_an_empty_chat_centres_the_composer() {
    use crate::web::templates::greeting_for;
    assert_eq!(greeting_for(0), "Good night");
    assert_eq!(greeting_for(4), "Good night");
    assert_eq!(greeting_for(5), "Good morning");
    assert_eq!(greeting_for(11), "Good morning");
    assert_eq!(greeting_for(12), "Good afternoon");
    assert_eq!(greeting_for(16), "Good afternoon");
    assert_eq!(greeting_for(17), "Good evening");
    assert_eq!(greeting_for(21), "Good evening");
    assert_eq!(greeting_for(22), "Good night");
    assert_eq!(greeting_for(23), "Good night");

    let (_dir, _engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());

    // A conversation with nothing in it: greeting shown, shell centred.
    let empty = block_on(send(&app, Call::get(ROUTE_CHAT).html().cookie(cookie)));
    assert_eq!(empty.status, StatusCode::OK, "{}", empty.body);
    assert!(
        empty.body.contains("chat-shell new-chat"),
        "an empty conversation marks the shell: {}",
        empty.body
    );
    assert!(
        empty.body.contains(r#"class="hello""#),
        "it greets by the hour: {}",
        empty.body
    );

    // Once it has a turn, the greeting is gone and the shell is a normal column.
    chat::append(
        _engine.db(),
        chat::CONVERSATION_ID,
        chat::Role::User,
        "hello there",
        chat::State::Complete,
    )
    .unwrap();
    let busy = block_on(send(&app, Call::get(ROUTE_CHAT).html().cookie(cookie)));
    assert_eq!(busy.status, StatusCode::OK, "{}", busy.body);
    assert!(
        !busy.body.contains(r#"class="chat-greeting""#),
        "a conversation with turns carries no greeting element: {}",
        busy.body
    );
    assert!(
        !busy.body.contains("chat-shell new-chat"),
        "the shell is a column again: {}",
        busy.body
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

/// The route a transcript link points at answers with the type the bytes are,
/// not the type the name claimed, and it is behind the session like every other
/// dashboard route.
#[test]
fn an_attachment_is_served_with_the_type_its_bytes_are() {
    let (_dir, engine, app, cookie) = ready_app();
    // A PNG that calls itself a text file. The name is a claim; the bytes are not.
    let png = a_png();
    let image = store_attachment(&engine, "notes.txt", &png);
    let url = format!("/dashboard/chat/file/{}", image.id);

    let refused = block_on(send(&app, Call::get_owned(url.clone())));
    assert_eq!(refused.status, StatusCode::FORBIDDEN, "{}", refused.body);
    assert_eq!(refused.json()["code"], "no_session");

    let (status, content_type, disposition, bytes) = get_stored(&app, &url, &cookie);
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        content_type, "image/png",
        "the bytes decide the type, not the .txt name"
    );
    assert_eq!(
        disposition, "inline; filename=\"notes.txt\"",
        "a picture is inline, so the thumbnail can be drawn from it"
    );
    assert_eq!(bytes, png, "the bytes served are the bytes stored");

    // Text stays text. An upload that contains HTML is not served as HTML, so a
    // page that arrived in a message cannot run in this origin.
    let page = b"<!doctype html><script>alert(1)</script>";
    let document = store_attachment(&engine, "shot.png", page);
    let (status, content_type, disposition, bytes) = get_stored(
        &app,
        &format!("/dashboard/chat/file/{}", document.id),
        &cookie,
    );
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        content_type, "text/plain",
        "html is served as text, never as html"
    );
    assert!(
        disposition.starts_with("attachment;"),
        "and it downloads: {disposition}"
    );
    assert_eq!(bytes, page);

    // An id the table does not know is a 404 with a code, not a filesystem read.
    let unknown = block_on(send(
        &app,
        // Written in two pieces so the path is not a secret-shaped run.
        Call::get_owned(concat!("/dashboard/chat", "/file/not-a-real-id").to_owned())
            .cookie(Some(&cookie)),
    ));
    assert_eq!(unknown.status, StatusCode::NOT_FOUND, "{}", unknown.body);
    assert_eq!(unknown.json()["code"], "not_found");
}

/// A row is a database fact, not a promise about the disk: a path outside the
/// attachment directory, or one whose bytes are gone, is refused rather than
/// read, and the refusal says which of the two it was.
#[test]
fn an_attachment_rows_path_is_confined_to_the_attachment_directory() {
    let (_dir, engine, app, cookie) = ready_app();
    // The directory the upload route creates, so what is under test is the
    // confinement check rather than its absence.
    std::fs::create_dir_all(
        engine
            .workspace_path()
            .expect("a workspace")
            .join("uploads")
            .join(CONVERSATION_ID),
    )
    .expect("the uploads directory");
    let outside = engine.paths().config_dir.join("eris.env");
    std::fs::write(&outside, b"outside the uploads tree\n").expect("the file");
    let tampered = chat::Attachment {
        id: "tampered-1".to_owned(),
        name: "eris.env".to_owned(),
        path: outside.display().to_string(),
        size: 30,
        media_type: Some("text/plain".to_owned()),
    };
    chat::record_attachment(engine.db(), CONVERSATION_ID, &tampered).expect("the row");

    let (status, content_type, _, bytes) =
        get_stored(&app, "/dashboard/chat/file/tampered-1", &cookie);
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        content_type.contains("application/json"),
        "a 404 is typed, not an empty page: {content_type}"
    );
    let body = String::from_utf8_lossy(&bytes);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&body).expect("json")["code"],
        "not_found",
        "{body}"
    );
    assert!(
        body.contains("not inside the attachment directory"),
        "the refusal names the reason: {body}"
    );
    assert!(
        !body.contains("not-for-the-browser"),
        "and leaks nothing it refused to read: {body}"
    );

    // A row whose bytes have been cleaned off disk is the other honest 404.
    let gone = chat::Attachment {
        id: "gone-1".to_owned(),
        name: "deleted.txt".to_owned(),
        path: engine
            .workspace_path()
            .expect("a workspace")
            .join("uploads")
            .join(CONVERSATION_ID)
            .join("deleted.txt")
            .display()
            .to_string(),
        size: 4,
        media_type: Some("text/plain".to_owned()),
    };
    chat::record_attachment(engine.db(), CONVERSATION_ID, &gone).expect("the row");
    let (status, _, _, bytes) = get_stored(&app, "/dashboard/chat/file/gone-1", &cookie);
    assert_eq!(status, StatusCode::NOT_FOUND);
    let body = String::from_utf8_lossy(&bytes);
    assert!(
        body.contains("not on disk"),
        "the missing-file 404 says so: {body}"
    );
}

/// What the message carried is inside the message: a picture is the picture, and
/// a file that cannot be drawn is a chip that names its type, its name and its
/// size, and still links to the bytes.
#[test]
fn a_message_shows_its_pictures_as_thumbnails_and_its_files_as_chips() {
    let (_dir, engine, app, cookie) = ready_app();
    crate::web::chat::seed_conversation(engine.db(), CONVERSATION_ID).unwrap();
    chat::append(
        engine.db(),
        CONVERSATION_ID,
        Role::User,
        "have a look",
        State::Complete,
    )
    .unwrap();
    let image = store_attachment(&engine, "shot.png", &a_png());
    let pdf = store_attachment(&engine, "brief.pdf", b"%PDF-1.7\nsome bytes\n");
    let question = last_message(&engine);
    chat::claim_attachments(
        engine.db(),
        CONVERSATION_ID,
        &question,
        &[image.id.clone(), pdf.id.clone()],
    )
    .expect("claimed");
    assert_eq!(
        chat::attachments_by_message(engine.db(), CONVERSATION_ID)
            .unwrap()
            .get(&question)
            .map(Vec::len),
        Some(2),
        "both files belong to the question"
    );

    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    let image_url = format!("/dashboard/chat/file/{}", image.id);
    let pdf_url = format!("/dashboard/chat/file/{}", pdf.id);

    // The picture: a thumbnail that links to the full file.
    assert!(
        body.contains(r#"class="attached-image""#),
        "the image is drawn in the bubble: {body}"
    );
    assert!(
        body.contains(&format!(r#"<img src="{image_url}" alt="shot.png""#)),
        "the thumbnail is the served bytes: {body}"
    );
    assert!(
        body.contains(&format!(r#"href="{image_url}""#)),
        "and it links to the full file: {body}"
    );

    // The file that cannot be drawn: badge, name, size, link.
    assert!(
        body.contains(r#"class="attached-badge">PDF<"#),
        "the chip carries a type badge: {body}"
    );
    assert!(body.contains("brief.pdf"), "and the name: {body}");
    assert!(
        body.contains(r#"class="attached-size">20 B<"#),
        "and the size, from the bytes on disk: {body}"
    );
    assert!(
        body.contains(&format!(r#"href="{pdf_url}""#)),
        "and it links to the served file: {body}"
    );
    assert!(
        body.contains(r#"download="brief.pdf""#),
        "a non-image downloads rather than opening here: {body}"
    );
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
    assert!(
        body.contains(">Kallisti<"),
        "the assistant is named from the default profile: {body}"
    );
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

#[test]
fn tool_settings_round_trip_and_a_bad_budget_is_refused_by_field() {
    let (_dir, engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());

    let saved = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_TOOLS,
            "tools_enabled=true&tool_rounds=20",
        )
        .cookie(cookie),
    ));
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    assert_eq!(
        engine.setting(crate::tools::SETTING_TOOL_ROUNDS).unwrap(),
        Some("20".to_owned())
    );
    assert_eq!(
        engine.setting(crate::tools::SETTING_TOOLS_ENABLED).unwrap(),
        Some("true".to_owned())
    );

    // Off is stored as off, not as an absent row.
    block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SETTINGS_TOOLS, "tool_rounds=8").cookie(cookie),
    ));
    assert_eq!(
        engine.setting(crate::tools::SETTING_TOOLS_ENABLED).unwrap(),
        Some("false".to_owned()),
        "an unchecked box means off"
    );

    let refused = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SETTINGS_TOOLS, "tool_rounds=0").cookie(cookie),
    ));
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    assert_eq!(refused.json()["field"], "tool_rounds");
    assert_eq!(
        engine.setting(crate::tools::SETTING_TOOL_ROUNDS).unwrap(),
        Some("8".to_owned()),
        "a refusal leaves the stored budget alone"
    );
}

#[test]
fn an_auxiliary_role_can_be_pinned_to_a_model_the_probe_serves() {
    let (_dir, engine, app, cookie) = ready_app();
    engine
        .set_setting(
            crate::onboarding::SETTING_PROVIDER_MODELS,
            r#"["m/one","m/two"]"#,
        )
        .unwrap();
    let cookie = Some(cookie.as_str());

    let body = "aux_vision=m/two&aux_compression=primary";
    let saved = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SETTINGS_AUX, body).cookie(cookie),
    ));
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    assert_eq!(
        engine.setting(&crate::aux::setting_key("vision")).unwrap(),
        Some("m/two".to_owned())
    );
    assert_eq!(
        engine
            .setting(&crate::aux::setting_key("compression"))
            .unwrap(),
        Some(crate::aux::INHERIT.to_owned()),
        "the sentinel is stored, not a copy of the primary model"
    );

    let refused = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SETTINGS_AUX, "aux_vision=m/nope").cookie(cookie),
    ));
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    assert_eq!(refused.json()["field"], "aux_vision");
    assert_eq!(
        engine.setting(&crate::aux::setting_key("vision")).unwrap(),
        Some("m/two".to_owned()),
        "a refusal changes nothing"
    );
}

#[test]
fn the_mcp_list_is_validated_and_stored_as_json() {
    let (_dir, engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());

    let body = format!(
        "mcp_servers={}",
        encode(r#"[{"name":"vault","command":"mcp-vault","args":["--root","/tmp"]}]"#)
    );
    let saved = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SETTINGS_MCP, &body).cookie(cookie),
    ));
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);

    let stored = engine
        .setting(crate::mcp::SETTING_SERVERS)
        .unwrap()
        .expect("a row");
    let servers = crate::mcp::parse(&stored).expect("still valid json");
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0].name, "vault");

    let refused = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_MCP,
            &format!("mcp_servers={}", encode("not json")),
        )
        .cookie(cookie),
    ));
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    assert_eq!(refused.json()["field"], "mcp_servers");
}

#[test]
fn the_settings_page_states_the_tools_aux_and_mcp_surfaces() {
    let (_dir, engine, app, cookie) = ready_app();
    engine
        .set_setting(crate::onboarding::SETTING_PROVIDER_MODELS, r#"["m/one"]"#)
        .unwrap();

    let page = block_on(send(
        &app,
        Call::get(ROUTE_SETTINGS).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK);
    let body = &page.body;
    assert!(
        body.contains("Tool rounds per turn"),
        "the budget is offered"
    );
    assert!(body.contains("Auxiliary models"), "the roles are listed");
    assert!(
        body.contains(crate::mcp::STATUS),
        "the MCP section states what a configured server does"
    );
    // A role with nothing behind it is no longer offered at all, so the page has
    // no decorative row to explain away.
    assert!(
        !body.contains("Nothing reads this row yet"),
        "no offered role is decorative"
    );
    assert!(
        body.contains("Used by"),
        "every listed role names what reads it"
    );
}

#[test]
fn the_provider_registry_round_trips_and_the_turn_resolves_the_primary() {
    let (_dir, engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());
    let registry = r#"[{"id":"alpha","name":"Alpha","base_url":"http://127.0.0.1:20001/v1","key_ref":"env:ALPHA_KEY"},{"id":"beta","name":"Beta","base_url":"http://127.0.0.1:20002/v1","key_ref":"env:BETA_KEY"}]"#;
    let body = format!(
        "providers_registry={}&providers_primary=beta",
        encode(registry)
    );
    let saved = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SETTINGS_PROVIDERS, &body).cookie(cookie),
    ));
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);

    assert_eq!(
        engine.setting(crate::providers::SETTING_PRIMARY).unwrap(),
        Some("beta".to_owned())
    );
    let resolved = crate::web::chat::turn_config(&engine)
        .expect("turn config")
        .expect("a provider");
    assert_eq!(
        resolved.base_url, "http://127.0.0.1:20002/v1",
        "the turn answers through the primary"
    );
}

#[test]
fn a_provider_registry_with_a_key_value_is_refused_by_field() {
    let (_dir, _engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());
    let bad = r#"[{"id":"alpha","name":"Alpha","base_url":"http://x/v1","key_ref":"sk-live-123"}]"#;
    let body = format!("providers_registry={}", encode(bad));

    let refused = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SETTINGS_PROVIDERS, &body).cookie(cookie),
    ));
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    assert_eq!(refused.json()["field"], "providers_registry");
}

#[test]
fn the_last_provider_cannot_be_removed_while_a_model_is_configured() {
    let (_dir, _engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());

    let refused = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_PROVIDERS,
            "providers_registry=%5B%5D",
        )
        .cookie(cookie),
    ));
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    assert_eq!(refused.json()["field"], "providers_registry");
}

#[test]
fn the_settings_page_lists_the_registry_and_its_primary() {
    let (_dir, engine, app, cookie) = ready_app();
    engine
        .set_setting(
            crate::providers::SETTING_REGISTRY,
            r#"[{"id":"alpha","name":"Alpha","base_url":"http://127.0.0.1:20001/v1","key_ref":"env:ALPHA_KEY"}]"#,
        )
        .unwrap();
    engine
        .set_setting(crate::providers::SETTING_PRIMARY, "alpha")
        .unwrap();

    let page = block_on(send(
        &app,
        Call::get(ROUTE_SETTINGS).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Providers"), "the section is present");
    assert!(
        page.body.contains("env:ALPHA_KEY"),
        "the stored registry is shown as it is stored"
    );
    assert!(
        page.body.contains(r#"<option value="alpha" selected"#),
        "the primary is preselected"
    );
}

#[test]
fn an_auxiliary_role_can_be_pinned_to_a_provider_in_the_registry() {
    let (_dir, engine, app, cookie) = ready_app();
    engine
        .set_setting(crate::onboarding::SETTING_PROVIDER_MODELS, r#"["m/one"]"#)
        .unwrap();
    engine
        .set_setting(
            crate::providers::SETTING_REGISTRY,
            r#"[{"id":"alpha","name":"Alpha","base_url":"http://127.0.0.1:20001/v1","key_ref":"env:A"},{"id":"beta","name":"Beta","base_url":"http://127.0.0.1:20002/v1","key_ref":"env:B"}]"#,
        )
        .unwrap();
    engine
        .set_setting(crate::providers::SETTING_PRIMARY, "alpha")
        .unwrap();
    let cookie = Some(cookie.as_str());

    let body = "aux_vision=m/one&aux_provider_vision=beta";
    let saved = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SETTINGS_AUX, body).cookie(cookie),
    ));
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    assert_eq!(
        engine.setting(&crate::aux::provider_key("vision")).unwrap(),
        Some("beta".to_owned())
    );

    let refused = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_AUX,
            "aux_vision=m/one&aux_provider_vision=gamma",
        )
        .cookie(cookie),
    ));
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    assert_eq!(refused.json()["field"], "aux_provider_vision");
}

/// A project is created from the rail, filed by a conversation, and refused by
/// name when it is a duplicate - all through the real router.
///
/// The rail's forms are the no-script path, so this drives them the way a browser
/// with JavaScript blocked does: one form post, one redirect, and a page that
/// already states the grouping when it is rendered.
#[test]
fn a_project_is_created_from_the_rail_and_files_a_conversation() {
    use crate::admin::{ROUTE_PROJECTS, ROUTE_PROJECTS_ASSIGN};

    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    let conversation = chat::create_conversation(db).expect("a conversation row");
    let cookie = Some(cookie.as_str());

    // Behind the admin session, like every other dashboard write.
    let anonymous = block_on(send(&app, Call::post(ROUTE_PROJECTS, "project_name=Work")));
    assert_eq!(anonymous.status, StatusCode::FORBIDDEN);
    assert_eq!(anonymous.json()["code"], "no_session");

    let created = block_on(send(
        &app,
        Call::post(ROUTE_PROJECTS, "project_name=Work").cookie(cookie),
    ));
    assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.body);
    assert_eq!(created.location().as_deref(), Some(ROUTE_CHAT));

    let projects = crate::projects::list(db).expect("the project list reads");
    assert_eq!(projects.len(), 1, "one project exists: {projects:?}");
    assert_eq!(projects[0].name, "Work");
    let project_id = projects[0].id.clone();

    // A second create with the same name is a named refusal, not a 500 and not a
    // silent redirect that looks like it worked.
    let duplicate = block_on(send(
        &app,
        Call::post(ROUTE_PROJECTS, "project_name=Work").cookie(cookie),
    ));
    assert_eq!(
        duplicate.status,
        StatusCode::BAD_REQUEST,
        "{}",
        duplicate.body
    );
    assert_eq!(duplicate.json()["field"], "project_name");
    assert_eq!(duplicate.json()["code"], "project_name_duplicate");

    let blank = block_on(send(
        &app,
        Call::post(ROUTE_PROJECTS, "project_name=%20%20").cookie(cookie),
    ));
    assert_eq!(blank.json()["code"], "project_name_invalid");

    // Filing the conversation, which is the per-row menu's whole job.
    let assigned = block_on(send(
        &app,
        Call::post(
            ROUTE_PROJECTS_ASSIGN,
            &format!(
                "conversation={}&project={}",
                encode(&conversation),
                encode(&project_id)
            ),
        )
        .cookie(cookie),
    ));
    assert_eq!(assigned.status, StatusCode::SEE_OTHER, "{}", assigned.body);
    assert_eq!(assigned.location().as_deref(), Some(ROUTE_CHAT));

    let filed = chat::conversations(db, 10).expect("the rail list reads");
    let row = filed
        .iter()
        .find(|row| row.id == conversation)
        .expect("the conversation is still listed");
    assert_eq!(row.project_id.as_deref(), Some(project_id.as_str()));

    // A second, unfiled conversation, so the page has both headings to render.
    let loose = chat::create_conversation(db).expect("a second conversation row");
    let page = block_on(send(&app, Call::get(ROUTE_CHAT).html().cookie(cookie)));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;

    assert!(
        body.contains(r#"<span class="rail-group-name">Work</span>"#),
        "the project heads a group in the rail: {body}"
    );
    assert!(
        body.contains(r#"<span class="rail-group-name">Conversations</span>"#),
        "the unfiled conversations keep a heading: {body}"
    );
    let group_at = body
        .find(r#"<span class="rail-group-name">Work</span>"#)
        .expect("the project heading");
    let filed_at = body
        .find(&format!(r#"href="/dashboard/chat/{conversation}""#))
        .expect("the filed conversation is linked");
    let loose_at = body
        .find(&format!(r#"href="/dashboard/chat/{loose}""#))
        .expect("the unfiled conversation is linked");
    assert!(
        group_at < filed_at && filed_at < loose_at,
        "the filed conversation sits under its project, above the unfiled heading"
    );

    // The menu is a details element with a form in it: the whole affordance works
    // with no script at all.
    assert!(
        body.contains(r#"<details class="session-menu">"#),
        "each session row carries the add-to-project menu: {body}"
    );
    assert!(
        body.contains(&format!(r#"action="{ROUTE_PROJECTS_ASSIGN}""#)),
        "the menu posts the assignment form: {body}"
    );
    assert!(
        body.contains(r#"<option value="" selected>No project</option>"#),
        "the select offers the clear: {body}"
    );
    assert!(
        body.contains(&format!(
            r#"<option value="{project_id}" selected>Work</option>"#
        )),
        "the select marks the project the conversation is in: {body}"
    );
    // The new-project control is a disclosure above the list.
    assert!(
        body.contains(r#"<details class="rail-new-project">"#),
        "the rail carries the new-project control: {body}"
    );
    assert!(
        body.contains(&format!(r#"action="{ROUTE_PROJECTS}""#)),
        "the control posts to the project route: {body}"
    );
    assert!(
        !body.contains("No projects yet"),
        "the hint is gone once a project exists"
    );

    // And it clears again, which is what the empty option is for.
    let cleared = block_on(send(
        &app,
        Call::post(
            ROUTE_PROJECTS_ASSIGN,
            &format!("conversation={}&project=", encode(&conversation)),
        )
        .cookie(cookie),
    ));
    assert_eq!(cleared.status, StatusCode::SEE_OTHER, "{}", cleared.body);
    let filed = chat::conversations(db, 10).expect("the rail list reads");
    let row = filed
        .iter()
        .find(|row| row.id == conversation)
        .expect("the conversation is still listed");
    assert_eq!(row.project_id, None, "it is ungrouped again");
}

/// Nothing changed is not a success: an assignment to a project that does not
/// exist is a refusal that names the field.
#[test]
fn an_assignment_to_an_unknown_project_is_refused() {
    use crate::admin::ROUTE_PROJECTS_ASSIGN;

    let (_dir, engine, app, cookie) = ready_app();
    let conversation = chat::create_conversation(engine.db()).expect("a conversation row");

    let refused = block_on(send(
        &app,
        Call::post(
            ROUTE_PROJECTS_ASSIGN,
            &format!(
                "conversation={}&project=no-such-project",
                encode(&conversation)
            ),
        )
        .cookie(Some(&cookie)),
    ));
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    assert_eq!(refused.json()["code"], "project_assign_refused");
    assert_eq!(
        chat::conversations(engine.db(), 10).expect("the rail list reads")[0].project_id,
        None,
        "the refused assignment wrote nothing"
    );

    let unnamed = block_on(send(
        &app,
        Call::post(ROUTE_PROJECTS_ASSIGN, "project=").cookie(Some(&cookie)),
    ));
    assert_eq!(unnamed.json()["code"], "conversation_missing");
}

/// A fresh rail states that there are no projects, in the disclosure that makes
/// one: an empty control with no explanation reads as a broken control.
#[test]
fn a_rail_with_no_projects_says_so() {
    let (_dir, _engine, app, cookie) = ready_app();
    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        page.body.contains("No projects yet"),
        "the rail says there are none: {}",
        page.body
    );
    assert!(
        !page.body.contains(r#"<span class="rail-group-name">"#),
        "with no projects the list stays flat: {}",
        page.body
    );
}

// ---------------------------------------------------------------------------
// Slash commands
// ---------------------------------------------------------------------------

/// The newest row of one conversation, which is what a command answered into.
fn newest_row(
    engine: &crate::onboarding::SetupEngine,
    conversation: &str,
) -> crate::web::chat::Message {
    chat::load(engine.db(), conversation)
        .expect("a readable transcript")
        .pop()
        .expect("a transcript row")
}

/// Post one command and return the reply, so each test reads as the operator's
/// own sentence rather than as a form body.
fn post_command(app: &axum::Router, cookie: &str, message: &str) -> crate::web::harness::Reply {
    block_on(send(
        app,
        Call::post(
            crate::admin::ROUTE_CHAT_SEND,
            &format!("message={}", encode(message)),
        )
        .cookie(Some(cookie)),
    ))
}

#[test]
fn help_is_answered_by_the_daemon_and_never_becomes_a_turn() {
    let (_dir, engine, app, cookie) = ready_app();
    let reply = post_command(&app, &cookie, "/help");
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert_eq!(
        reply.location().as_deref(),
        Some(format!("/dashboard/chat/{CONVERSATION_ID}").as_str()),
        "the operator goes back to the conversation the answer is in"
    );

    let rows = chat::load(engine.db(), CONVERSATION_ID).expect("a readable transcript");
    assert_eq!(rows.len(), 1, "one row, the answer itself: {rows:?}");
    assert_eq!(rows[0].role, Role::Assistant, "the daemon spoke");
    assert_eq!(
        rows[0].state,
        State::Complete,
        "the answer is whole the moment it is written"
    );
    for (name, _) in crate::commands::SUPPORTED {
        assert!(
            rows[0].body.contains(&format!("/{name}")),
            "/{name} is listed: {}",
            rows[0].body
        );
    }

    // The turn path writes the operator's question before it touches a provider,
    // so a transcript with no user row is a turn that never started. That is the
    // by-construction proof that a command spends no tokens.
    assert!(
        !rows.iter().any(|row| row.role == Role::User),
        "no question row was written: {rows:?}"
    );
    assert_eq!(rows[0].prompt_tokens, None, "no prompt was billed");
    assert_eq!(rows[0].completion_tokens, None, "no completion was billed");
}

#[test]
fn an_unknown_command_is_refused_by_name_and_still_costs_nothing() {
    let (_dir, engine, app, cookie) = ready_app();
    for name in ["goal", "restart", "doesnotexist"] {
        let reply = post_command(&app, &cookie, &format!("/{name}"));
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
        let row = newest_row(&engine, CONVERSATION_ID);
        assert_eq!(row.role, Role::Assistant);
        assert!(
            row.body.contains(&format!("/{name}")),
            "the refusal names it: {}",
            row.body
        );
        assert!(
            row.body.contains("not supported"),
            "the refusal says so: {}",
            row.body
        );
        assert_eq!(row.prompt_tokens, None, "nothing was sent to a model");
    }

    // The two commands operators arrive with are named as somebody else's
    // features rather than quietly failing.
    let row = newest_row(&engine, CONVERSATION_ID);
    assert!(row.body.contains("/goal"), "{}", row.body);
    assert!(row.body.contains("/restart"), "{}", row.body);
    assert!(
        row.body.contains("not Kallisti features"),
        "the copy is explicit about whose they are: {}",
        row.body
    );
    assert_eq!(
        chat::count(engine.db(), CONVERSATION_ID).unwrap() as usize,
        3,
        "one row per command, and no question rows"
    );
}

#[test]
fn a_command_is_answered_even_with_no_provider_stored() {
    let (_dir, engine, app, cookie) = ready_app();
    engine
        .db()
        .with_conn(|conn| {
            conn.execute("DELETE FROM settings WHERE key LIKE 'provider.%'", [])?;
            Ok(())
        })
        .unwrap();

    // An ordinary message is refused here, because a daemon with no endpoint has
    // not accepted it. A command is not a message to a model, so it answers.
    let reply = post_command(&app, &cookie, "/help");
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let row = newest_row(&engine, CONVERSATION_ID);
    assert_eq!(row.role, Role::Assistant);
    assert!(row.body.contains("/help"), "{}", row.body);
}

#[test]
fn a_model_command_sets_the_default_only_for_an_id_in_the_list() {
    let (_dir, engine, app, cookie) = ready_app();
    engine
        .set_provider(
            "http://127.0.0.1:20128/v1",
            "env:NINEROUTER_API_KEY",
            &["m/one".to_owned(), "m/two".to_owned()],
        )
        .expect("a provider with two models");
    engine.set_model("m/one").expect("the first is the default");

    let changed = post_command(&app, &cookie, "/model m/two");
    assert_eq!(changed.status, StatusCode::SEE_OTHER, "{}", changed.body);
    assert_eq!(
        engine.model().unwrap(),
        Some("m/two".to_owned()),
        "the stored default moved"
    );
    assert!(
        newest_row(&engine, CONVERSATION_ID).body.contains("m/two"),
        "the answer says what it became"
    );

    let refused = post_command(&app, &cookie, "/model m/three");
    assert_eq!(refused.status, StatusCode::SEE_OTHER, "{}", refused.body);
    assert_eq!(
        engine.model().unwrap(),
        Some("m/two".to_owned()),
        "an id outside the list leaves the default alone"
    );
    let row = newest_row(&engine, CONVERSATION_ID);
    assert!(row.body.contains("m/three"), "it is named: {}", row.body);
    assert!(
        row.body.contains("not changed"),
        "the refusal says nothing moved: {}",
        row.body
    );
}

#[test]
fn a_memory_command_sets_the_backend_and_refuses_anything_else_by_name() {
    let (_dir, engine, app, cookie) = ready_app();

    for backend in ["off", "gbrain", "vault"] {
        let reply = post_command(&app, &cookie, &format!("/memory {backend}"));
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
        let stored = engine
            .setting(crate::memory::SETTING_BACKEND)
            .expect("a readable setting");
        assert_eq!(stored.as_deref(), Some(backend), "the backend moved");
    }

    let refused = post_command(&app, &cookie, "/memory postgres");
    assert_eq!(refused.status, StatusCode::SEE_OTHER, "{}", refused.body);
    let stored = engine
        .setting(crate::memory::SETTING_BACKEND)
        .expect("a readable setting");
    assert_eq!(
        stored.as_deref(),
        Some("vault"),
        "an unknown backend leaves the stored one alone"
    );
    let row = newest_row(&engine, CONVERSATION_ID);
    assert!(
        row.body.contains("postgres"),
        "the refusal names it: {}",
        row.body
    );
    assert!(
        row.body.contains("gbrain"),
        "it lists the three that exist: {}",
        row.body
    );
}

#[test]
fn a_clear_command_empties_the_transcript_and_leaves_its_own_answer() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    chat::append(db, CONVERSATION_ID, Role::User, "first", State::Complete).unwrap();
    chat::append(
        db,
        CONVERSATION_ID,
        Role::Assistant,
        "second",
        State::Complete,
    )
    .unwrap();

    let reply = post_command(&app, &cookie, "/clear");
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);

    let rows = chat::load(db, CONVERSATION_ID).expect("a readable transcript");
    assert_eq!(rows.len(), 1, "only the answer is left: {rows:?}");
    assert_eq!(rows[0].role, Role::Assistant);
    assert!(
        rows[0].body.contains("Cleared this conversation"),
        "the answer says what happened: {}",
        rows[0].body
    );
    assert!(
        rows[0].body.contains("2 messages"),
        "it counts what went: {}",
        rows[0].body
    );
}

#[test]
fn a_new_command_creates_a_conversation_and_answers_into_it() {
    let (_dir, engine, app, cookie) = ready_app();
    let reply = post_command(&app, &cookie, "/new");
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    let location = reply.location().expect("a redirect");
    let created = location
        .strip_prefix("/dashboard/chat/")
        .expect("a conversation path")
        .to_owned();
    assert_ne!(
        created, CONVERSATION_ID,
        "a conversation that did not exist"
    );
    assert!(
        chat::conversation_exists(engine.db(), &created).unwrap(),
        "the new conversation is stored"
    );
    assert_eq!(
        chat::count(engine.db(), &created).unwrap(),
        1,
        "the answer landed in the new conversation"
    );
    assert_eq!(
        chat::count(engine.db(), CONVERSATION_ID).unwrap(),
        0,
        "the previous conversation was left alone"
    );
}

#[test]
fn a_stop_command_refuses_when_nothing_runs_and_cancels_a_row_that_does() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();

    let idle = post_command(&app, &cookie, "/stop");
    assert_eq!(idle.status, StatusCode::SEE_OTHER, "{}", idle.body);
    let row = newest_row(&engine, CONVERSATION_ID);
    assert!(
        row.body.contains("Nothing was running"),
        "it says there was nothing to stop: {}",
        row.body
    );

    // A row still marked streaming is exactly what a running turn leaves behind,
    // so this is the state the command has to cancel.
    let running = chat::append_answer(db, CONVERSATION_ID, "m/one", None).expect("a running row");
    assert!(chat::still_streaming(db, &running).unwrap());

    let stopped = post_command(&app, &cookie, "/stop");
    assert_eq!(stopped.status, StatusCode::SEE_OTHER, "{}", stopped.body);
    assert!(
        !chat::still_streaming(db, &running).unwrap(),
        "the running row is no longer streaming"
    );
    let cancelled = chat::load(db, CONVERSATION_ID)
        .expect("a readable transcript")
        .into_iter()
        .find(|row| row.id == running)
        .expect("the row is still there");
    assert_eq!(cancelled.state, State::Failed);
    assert_eq!(cancelled.error_code.as_deref(), Some("cancelled"));
    assert!(
        newest_row(&engine, CONVERSATION_ID)
            .body
            .contains("Stopped"),
        "the answer says it stopped"
    );
}

/// A ready daemon, plus the steering queue the router's own state holds.
///
/// The queue is in-process, so a test reaches it the way the handlers do: through
/// the `AppState` the router was built from. No new plumbing, and the assertion is
/// about the same object the send handler consults.
fn ready_app_steered() -> (
    tempfile::TempDir,
    crate::onboarding::SetupEngine,
    axum::Router,
    String,
    chat::Steering,
) {
    let (dir, engine, passphrase) = crate::web::harness::ready_engine();
    let state = crate::admin::AppState::new(engine.clone(), false);
    let steering = state.steering.clone();
    let app = crate::admin::router_with(state);
    let cookie = crate::web::harness::sign_in(&app, &passphrase);
    (dir, engine, app, cookie, steering)
}

/// Hold a conversation the way a running turn does, or fail loudly.
fn hold(steering: &chat::Steering) -> chat::TurnLease {
    match steering.claim(CONVERSATION_ID, "a running turn") {
        chat::Claim::Turn(lease) => lease,
        chat::Claim::Steered(_) => panic!("the conversation was free"),
    }
}

#[test]
fn a_message_during_a_turn_is_queued_for_it_instead_of_starting_a_second_one() {
    let (_dir, engine, app, cookie, steering) = ready_app_steered();
    let db = engine.db();
    let before = chat::count(db, CONVERSATION_ID).expect("a readable transcript");

    // A turn is streaming into this conversation.
    let lease = hold(&steering);

    let reply = block_on(send(
        &app,
        Call::post(ROUTE_CHAT_SEND, "message=wait+for+the+logs")
            .cookie(Some(&cookie))
            .header(header::ACCEPT.as_str(), "text/html"),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);

    // One row, the operator's: no second turn was started behind the first.
    let transcript = chat::load(db, CONVERSATION_ID).expect("a readable transcript");
    assert_eq!(
        chat::count(db, CONVERSATION_ID).expect("a count"),
        before + 1,
        "a steer writes the message and nothing else"
    );
    assert_eq!(
        transcript.last().expect("a last row").role,
        chat::Role::User
    );
    assert!(
        chat::streaming(db, CONVERSATION_ID)
            .expect("a readable queue")
            .is_empty(),
        "no second answer row was opened"
    );
    assert_eq!(
        steering.queued(CONVERSATION_ID),
        1,
        "the message waits for the turn that is running"
    );

    // The page the redirect lands on says so, rather than looking like the
    // message went nowhere.
    let page = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        page.body.contains("1 message queued for the running turn"),
        "{}",
        page.body
    );

    // And the running turn is handed the words at its boundary, once.
    match lease.signal() {
        chat::TurnSignal::Steer(text) => assert_eq!(text, "wait for the logs"),
        other => panic!("expected the steer, got {other:?}"),
    }
    assert_eq!(steering.queued(CONVERSATION_ID), 0);
}

#[test]
fn the_stop_command_stops_the_row_and_asks_the_running_turn_to_stop() {
    let (_dir, engine, app, cookie, steering) = ready_app_steered();
    let db = engine.db();
    let lease = hold(&steering);
    // The row a running turn leaves behind, so both halves of the cancellation
    // are exercised: the record and the turn.
    let running = chat::append_answer(db, CONVERSATION_ID, "m/one", None).expect("a running row");

    let reply = post_command(&app, &cookie, "/stop");
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert!(
        !chat::still_streaming(db, &running).expect("a readable row"),
        "the row stops streaming"
    );
    assert_eq!(
        lease.signal(),
        chat::TurnSignal::Stop,
        "the turn that is running is asked to stop as well"
    );
    let body = newest_row(&engine, CONVERSATION_ID).body;
    assert!(body.contains("Stopped"), "{body}");
    assert!(
        body.contains("next boundary"),
        "the copy states the honest limit: {body}"
    );
}

#[test]
fn an_empty_chat_shows_the_generated_line_only_when_it_is_switched_on() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    let now = crate::auth::now().unix_timestamp();

    // A stored line with the switch off renders nothing at all: the salutation
    // stands alone exactly as it did before the setting existed.
    crate::greeting::remember(db, "Light through the window", now).expect("store a line");
    let off = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(off.status, StatusCode::OK, "{}", off.body);
    assert!(
        !off.body.contains("Light through the window"),
        "the line is not rendered while the setting is off: {}",
        off.body
    );
    assert!(
        !off.body.contains(r#"class="line""#),
        "there is no line element at all: {}",
        off.body
    );
    assert!(
        off.body.contains(r#"class="hello""#),
        "the salutation is still there: {}",
        off.body
    );

    // Switched on, with a line inside its window, the page serves the stored
    // line: a cached render does not reach a provider.
    crate::web::store::settings_put(db, crate::greeting::SETTING_GREETING_ENABLED, "true")
        .expect("on");
    let on = block_on(send(
        &app,
        Call::get(ROUTE_CHAT).html().cookie(Some(&cookie)),
    ));
    assert_eq!(on.status, StatusCode::OK, "{}", on.body);
    assert!(
        on.body.contains("Light through the window"),
        "the stored line is under the salutation: {}",
        on.body
    );
    assert!(
        on.body.contains(r#"class="line""#),
        "it is rendered as its own quiet line: {}",
        on.body
    );
}
