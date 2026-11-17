//! Phase 2 acceptance: the Sessions pages and the job cancel route.
//!
//! Four claims, all read back from the bytes a browser receives rather than from
//! a template field:
//!
//! - the list is the `sessions` table, newest first, with the device name joined
//!   in, the user agent truncated, one state per row and a job count that is a
//!   count of that session's own jobs;
//! - a session with no rows is a friendly line, not a table with no body;
//! - the detail page is the session's identity, its jobs newest first, and each
//!   job's `job_events` in `seq` order as a timeline;
//! - a cancel is a state change plus an audit row, and it is refused for a job
//!   that is already terminal.

use axum::http::StatusCode;

use crate::admin::{ROUTE_SESSIONS, job_cancel_path, session_path};

use super::harness::{
    Call, audit_rows, block_on, job_status, ready_app, seed_device, seed_event, seed_job,
    seed_session, send,
};

/// The user agent a seeded session carries, so the page has something to show.
const AGENT: &str = "kallisti-test-agent/1.0";

/// A live expiry, far enough out that no test run can cross it.
const LATER: &str = "2099-01-01T00:00:00.000Z";

/// An expiry in the past, so the row reports `expired`.
const EARLIER: &str = "2000-01-01T00:00:00.000Z";

#[test]
fn the_list_renders_real_rows_and_counts_jobs() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();

    seed_device(
        db,
        "dev-1",
        "iPad",
        "ios",
        Some("2026-10-19T09:00:00.000Z"),
        None,
        None,
    );
    seed_session(
        db,
        "sess-old",
        Some("dev-1"),
        Some(AGENT),
        "2026-10-19T10:00:00.000Z",
        LATER,
        None,
    );
    // A long user agent, which the table truncates rather than wraps wide.
    let long_agent = "z".repeat(200);
    // A session whose device row is gone: the join has nothing, so the page must
    // say `unknown` rather than print an empty cell.
    seed_session(
        db,
        "sess-anon",
        None,
        Some(long_agent.as_str()),
        "2026-10-19T10:30:00.000Z",
        LATER,
        None,
    );
    seed_session(
        db,
        "sess-new",
        None,
        Some("curl/8.0"),
        "2026-10-19T11:00:00.000Z",
        LATER,
        None,
    );
    seed_session(
        db,
        "sess-gone",
        None,
        Some("curl/8.0"),
        "2026-10-19T08:00:00.000Z",
        EARLIER,
        None,
    );
    // A revoked session is a third state, and the row says which one it is.
    seed_session(
        db,
        "sess-revoked",
        None,
        Some("curl/8.0"),
        "2026-10-19T07:00:00.000Z",
        LATER,
        Some("2026-10-19T07:30:00.000Z"),
    );

    for index in 0..3 {
        seed_job(
            db,
            &format!("job-old-{index}"),
            Some("sess-old"),
            "succeeded",
            Some("say hello"),
            "2026-10-19T10:01:00.000Z",
            Some("2026-10-19T10:01:01.000Z"),
            None,
            None,
        );
    }
    seed_job(
        db,
        "job-new-0",
        Some("sess-new"),
        "failed",
        Some("say hello"),
        "2026-10-19T11:01:00.000Z",
        Some("2026-10-19T11:01:01.000Z"),
        Some("provider_error"),
        Some("the provider refused"),
    );

    let reply = block_on(send(
        &app,
        Call::get(ROUTE_SESSIONS).html().cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let page = &reply.body;

    for heading in [
        "Created",
        "Device",
        "Agent",
        "Last seen",
        "Expires",
        "State",
        "Jobs",
    ] {
        assert!(
            page.contains(heading),
            "the {heading} column is missing: {page}"
        );
    }

    // Newest first: every session link is in descending `created_at` order.
    let order = [
        session_path("sess-new"),
        session_path("sess-anon"),
        session_path("sess-old"),
        session_path("sess-gone"),
    ];
    let mut cursor = 0usize;
    for path in &order {
        let found = page[cursor..]
            .find(path.as_str())
            .unwrap_or_else(|| panic!("{path} is missing from the page: {page}"));
        cursor += found + path.len();
    }

    // The device name is joined in, and a session with no device row says so.
    assert!(page.contains("iPad"), "the joined device name: {page}");
    assert!(
        page.contains("unknown"),
        "a session with no device row says unknown: {page}"
    );

    // The job count is that session's own count, in its own row.
    let old_row = super::harness::row_window(page, &session_path("sess-old"));
    assert!(
        old_row.contains(r#"<td class="mono">3</td>"#),
        "the three jobs of sess-old are counted in its row: {old_row}"
    );
    let new_row = super::harness::row_window(page, &session_path("sess-new"));
    assert!(
        new_row.contains(r#"<td class="mono">1</td>"#),
        "the one job of sess-new is counted in its row: {new_row}"
    );
    let anon_row = super::harness::row_window(page, &session_path("sess-anon"));
    assert!(
        anon_row.contains(r#"<td class="mono">0</td>"#),
        "a session with no jobs counts zero: {anon_row}"
    );

    // One state per row, in text, so colour is never the only signal.
    assert!(page.contains(">live<"), "live state text: {page}");
    let gone_row = super::harness::row_window(page, &session_path("sess-gone"));
    assert!(
        gone_row.contains(">expired<"),
        "a past expiry reads expired: {gone_row}"
    );
    let revoked_row = super::harness::row_window(page, &session_path("sess-revoked"));
    assert!(
        revoked_row.contains(">revoked<"),
        "a revoked session reads revoked even with a live expiry: {revoked_row}"
    );

    // A long user agent is truncated for the table rather than wrapping it wide.
    assert!(
        !page.contains(&long_agent),
        "the 200 character agent is truncated: {page}"
    );
    assert!(page.contains("..."), "truncation is visible: {page}");

    // Every row is a link to its own detail page.
    for path in &order {
        assert!(page.contains(path.as_str()), "{path} is a link");
    }

    // And the page needs a session like every other dashboard page.
    let anonymous = block_on(send(&app, Call::get(ROUTE_SESSIONS)));
    assert_eq!(anonymous.status, StatusCode::FORBIDDEN);
    assert_eq!(anonymous.json()["code"], "no_session");
}

#[test]
fn the_list_has_a_friendly_empty_state() {
    let (_dir, _engine, app, cookie) = ready_app();

    let reply = block_on(send(
        &app,
        Call::get(ROUTE_SESSIONS).html().cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let page = &reply.body;

    // A sentence a person can read, and no table with an empty body.
    assert!(
        page.contains("No device session has been opened yet"),
        "the empty state is a line: {page}"
    );
    assert!(
        !page.contains("<table"),
        "an empty list is not an empty table: {page}"
    );
    assert!(
        !page.contains("<tbody"),
        "an empty list is not an empty body: {page}"
    );
    // The page still explains what would appear here.
    assert!(
        page.contains("device listener"),
        "the empty state says where a session would come from: {page}"
    );
}

#[test]
fn a_detail_page_shows_jobs_and_their_events_in_seq_order() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();

    seed_device(
        db,
        "dev-1",
        "iPad",
        "ios",
        Some("2026-10-19T09:00:00.000Z"),
        None,
        None,
    );
    seed_session(
        db,
        "sess-a",
        Some("dev-1"),
        Some(AGENT),
        "2026-10-19T10:00:00.000Z",
        LATER,
        None,
    );

    let long_request = "r".repeat(300);
    seed_job(
        db,
        "job-ok",
        Some("sess-a"),
        "succeeded",
        Some(&long_request),
        "2026-10-19T10:01:00.000Z",
        Some("2026-10-19T10:01:05.000Z"),
        None,
        None,
    );
    seed_job(
        db,
        "job-bad",
        Some("sess-a"),
        "failed",
        Some("say hello"),
        "2026-10-19T10:05:00.000Z",
        Some("2026-10-19T10:05:05.000Z"),
        Some("provider_timeout"),
        Some("the provider did not answer"),
    );
    for (seq, kind, payload) in [
        (1i64, "turn.started", Some("accepted")),
        (2, "turn.delta", Some("hello")),
        (3, "turn.finished", None),
    ] {
        seed_event(
            db,
            "job-ok",
            seq,
            kind,
            payload,
            &format!("2026-10-19T10:01:0{seq}.000Z"),
        );
    }

    let reply = block_on(send(
        &app,
        Call::get_owned(session_path("sess-a"))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let page = &reply.body;

    // The identity card is a `dl .kv` with every field the brief names.
    assert!(page.contains(r#"<dl class="kv">"#), "the kv card: {page}");
    for label in [
        "Session",
        "Device",
        "User agent",
        "Created",
        "Last seen",
        "Expires",
        "Revoked",
    ] {
        assert!(page.contains(label), "the {label} label: {page}");
    }
    assert!(page.contains("sess-a"), "the id itself: {page}");
    assert!(page.contains("iPad"), "the joined device name: {page}");
    assert!(page.contains(AGENT), "the full user agent: {page}");

    // Jobs newest first.
    let newer = page
        .find("job-bad")
        .unwrap_or_else(|| panic!("job-bad is missing: {page}"));
    let older = page
        .find("job-ok")
        .unwrap_or_else(|| panic!("job-ok is missing: {page}"));
    assert!(newer < older, "the newest job comes first: {page}");

    // A failed job carries its code and its message, in plain English.
    assert!(page.contains("provider_timeout"), "the error code: {page}");
    assert!(
        page.contains("the provider did not answer"),
        "the error message: {page}"
    );

    // The long request text is truncated rather than pasted whole.
    assert!(
        !page.contains(&long_request),
        "the 300 character request is truncated: {page}"
    );
    assert!(
        page.contains("say hello"),
        "a short request is intact: {page}"
    );

    // The events are a timeline, in seq order.
    assert!(page.contains(r#"class="timeline""#), "the timeline: {page}");
    let started = page
        .find("turn.started")
        .unwrap_or_else(|| panic!("turn.started is missing: {page}"));
    let delta = page
        .find("turn.delta")
        .unwrap_or_else(|| panic!("turn.delta is missing: {page}"));
    let finished = page
        .find("turn.finished")
        .unwrap_or_else(|| panic!("turn.finished is missing: {page}"));
    assert!(
        started < delta && delta < finished,
        "the events are in seq order: {started}, {delta}, {finished}"
    );

    let anonymous = block_on(send(&app, Call::get_owned(session_path("sess-a"))));
    assert_eq!(anonymous.status, StatusCode::FORBIDDEN);
    assert_eq!(anonymous.json()["code"], "no_session");
}

#[test]
fn an_unknown_session_is_a_404_page_not_a_blank_200() {
    let (_dir, _engine, app, cookie) = ready_app();

    let reply = block_on(send(
        &app,
        Call::get_owned(session_path("no-such-session"))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(
        reply.status,
        StatusCode::NOT_FOUND,
        "an unknown id is a 404: {}",
        reply.body
    );
    // The existing error template, with the code and a message a person reads.
    assert!(reply.body.contains("Not available"), "{}", reply.body);
    assert!(
        reply.body.contains("session_not_found"),
        "the error carries its code: {}",
        reply.body
    );
    assert!(
        reply.body.contains("no session with id no-such-session"),
        "the error names the id it could not find: {}",
        reply.body
    );
    assert!(!reply.body.trim().is_empty(), "a 404 is never a blank body");

    // An API client gets the same refusal as JSON.
    let json = block_on(send(
        &app,
        Call::get_owned(session_path("no-such-session")).cookie(Some(&cookie)),
    ));
    assert_eq!(json.status, StatusCode::NOT_FOUND);
    assert_eq!(json.json()["code"], "session_not_found");
}

#[test]
fn cancelling_a_queued_job_audits_and_changes_status() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();

    seed_session(
        db,
        "sess-a",
        None,
        Some(AGENT),
        "2026-10-19T10:00:00.000Z",
        LATER,
        None,
    );
    seed_job(
        db,
        "job-queued",
        Some("sess-a"),
        "queued",
        Some("say hello"),
        "2026-10-19T10:01:00.000Z",
        None,
        None,
        None,
    );
    seed_job(
        db,
        "job-running",
        Some("sess-a"),
        "running",
        Some("say hello"),
        "2026-10-19T10:02:00.000Z",
        None,
        None,
        None,
    );

    assert_eq!(job_status(db, "job-queued").as_deref(), Some("queued"));

    let reply = block_on(send(
        &app,
        Call::post_owned(job_cancel_path("job-queued"), "").cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.json()["ok"], true);
    assert_eq!(reply.json()["job_id"], "job-queued");
    assert_eq!(job_status(db, "job-queued").as_deref(), Some("cancelled"));

    // The audit row names the action and the job, and nothing else.
    let rows = audit_rows(db);
    let cancel = rows
        .iter()
        .find(|row| row.action == "job.cancel" && row.detail.as_deref() == Some("job-queued"))
        .unwrap_or_else(|| panic!("no job.cancel audit row: {rows:?}"));
    assert_eq!(cancel.actor, "loopback", "the listener wrote the row");

    // A running job is cancellable too, and the browser goes back to the
    // session page that offered the button.
    let html = block_on(send(
        &app,
        Call::post_owned(job_cancel_path("job-running"), "")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(html.status, StatusCode::SEE_OTHER, "{}", html.body);
    assert_eq!(
        html.location().as_deref(),
        Some(session_path("sess-a").as_str())
    );
    assert_eq!(job_status(db, "job-running").as_deref(), Some("cancelled"));

    // The detail page then says so, and offers no cancel button for either job.
    let page = block_on(send(
        &app,
        Call::get_owned(session_path("sess-a"))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        page.body.matches(">cancelled<").count() >= 2,
        "both jobs read cancelled: {}",
        page.body
    );
    assert!(
        !page.body.contains(job_cancel_path("job-queued").as_str()),
        "a terminal job offers no cancel form: {}",
        page.body
    );

    // A job that does not exist is a 404, not a 200 that changed nothing.
    let missing = block_on(send(
        &app,
        Call::post_owned(job_cancel_path("no-such-job"), "").cookie(Some(&cookie)),
    ));
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
    assert_eq!(missing.json()["code"], "job_not_found");
}

#[test]
fn cancelling_a_finished_job_is_refused() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();

    seed_session(
        db,
        "sess-a",
        None,
        Some(AGENT),
        "2026-10-19T10:00:00.000Z",
        LATER,
        None,
    );
    seed_job(
        db,
        "job-done",
        Some("sess-a"),
        "succeeded",
        Some("say hello"),
        "2026-10-19T10:01:00.000Z",
        Some("2026-10-19T10:01:05.000Z"),
        None,
        None,
    );

    let reply = block_on(send(
        &app,
        Call::post_owned(job_cancel_path("job-done"), "").cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert_eq!(reply.json()["code"], "job_not_cancellable");
    let message = reply.json()["message"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(
        message.contains("already succeeded"),
        "the refusal says why in plain English: {message}"
    );

    // Nothing changed, and nothing was audited.
    assert_eq!(job_status(db, "job-done").as_deref(), Some("succeeded"));
    assert!(
        !audit_rows(db)
            .iter()
            .any(|row| row.action == "job.cancel" && row.detail.as_deref() == Some("job-done")),
        "a refused cancel writes no audit row"
    );

    // The browser gets the same reason, rendered, at the same status.
    let html = block_on(send(
        &app,
        Call::post_owned(job_cancel_path("job-done"), "")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(html.status, StatusCode::BAD_REQUEST, "{}", html.body);
    assert!(
        html.body.contains("already succeeded"),
        "the rendered refusal says why: {}",
        html.body
    );
    assert!(
        html.body.contains("sess-a") && html.body.contains("<h2 id=\"session-heading\">"),
        "the refusal renders the session the job belongs to: {}",
        html.body
    );
    assert!(
        html.body.contains("job-done"),
        "the job is still named on the page: {}",
        html.body
    );
}
