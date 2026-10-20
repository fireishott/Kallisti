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

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use tower::ServiceExt;

use crate::admin::{
    ROUTE_CHAT, ROUTE_CHAT_SEND, ROUTE_SECURITY, ROUTE_SETTINGS, ROUTE_SETTINGS_TOOLS,
};
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
        "the MCP section says no client exists"
    );
    assert!(
        body.contains("Nothing reads this row yet"),
        "an unconsumed role says so rather than looking wired"
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
