//! Phase 2 acceptance: the Devices page, its revoke button, and the one form
//! that mints a pairing code.
//!
//! The claims that matter here are the ones a reviewer would reject the phase
//! for:
//!
//! - the table lists every device, revoked ones included, with a state in words;
//! - the revoke button posts to the route that already existed, so there is one
//!   revoke path in the binary and not two;
//! - a minted code is eight characters from the pairing alphabet, is shown in the
//!   response body, is stored only as a verifier, is absent from every text
//!   column of every table, and is absent from the audit detail.

use axum::http::StatusCode;

use crate::admin::ROUTE_DEVICES;
use crate::auth::pairing::{ALPHABET, CODE_LENGTH};

use super::harness::{
    Call, audit_rows, block_on, dump_text, ready_app, row_window, seed_device, seed_redeemed_code,
    send,
};

/// The route the revoke form posts to. It predates this page: the test asserts
/// the page reuses it rather than adding a second one.
const REVOKE: &str = "/dashboard/device/revoke";

/// The markup the mint response puts the code in, so a test can find it without
/// guessing which eight characters on the page are the code.
const CODE_MARK: &str = r#"<p class="pairing-code mono">"#;

#[test]
fn the_list_shows_paired_and_revoked_states() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();

    seed_device(
        db,
        "dev-paired",
        "iPhone",
        "ios",
        Some("2026-10-19T09:00:00.000Z"),
        Some("2026-10-19T12:00:00.000Z"),
        None,
    );
    seed_redeemed_code(db, "dev-paired");
    seed_device(
        db,
        "dev-waiting",
        "iPad",
        "ios",
        Some("2026-10-19T09:30:00.000Z"),
        None,
        None,
    );
    seed_device(
        db,
        "dev-revoked",
        "OldPhone",
        "android",
        Some("2026-10-01T09:00:00.000Z"),
        None,
        Some("2026-10-10T09:00:00.000Z"),
    );

    let reply = block_on(send(
        &app,
        Call::get(ROUTE_DEVICES).html().cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let page = &reply.body;

    for heading in ["Name", "Platform", "Paired", "Last seen", "State"] {
        assert!(
            page.contains(heading),
            "the {heading} column is missing: {page}"
        );
    }

    // Every device, in one table, whatever its state.
    for name in ["iPhone", "iPad", "OldPhone"] {
        assert!(page.contains(name), "{name} is listed: {page}");
    }

    // One state in words per row.
    let paired_row = row_window(page, "iPhone");
    assert!(
        paired_row.contains(">paired<"),
        "a redeemed code reads paired: {paired_row}"
    );
    let waiting_row = row_window(page, "iPad");
    assert!(
        waiting_row.contains(">awaiting code<"),
        "a device with no code redeemed reads awaiting code: {waiting_row}"
    );
    let revoked_row = row_window(page, "OldPhone");
    assert!(
        revoked_row.contains(">revoked<"),
        "a revoked device keeps its row and says so: {revoked_row}"
    );

    // The revoke button is the existing route, with the device id in the body.
    assert!(
        page.contains(&format!(r#"action="{REVOKE}""#)),
        "the revoke form posts to the existing route: {page}"
    );
    assert!(
        paired_row.contains(r#"name="device_id" value="dev-paired""#),
        "the row posts its own id: {paired_row}"
    );
    assert!(
        waiting_row.contains(r#"name="device_id" value="dev-waiting""#),
        "every unrevoked row can be revoked: {waiting_row}"
    );
    // A revoked device is not offered a second revoke.
    assert!(
        !revoked_row.contains(&format!(r#"action="{REVOKE}""#)),
        "a revoked row offers no revoke form: {revoked_row}"
    );

    // And the page needs a session like every other dashboard page.
    let anonymous = block_on(send(&app, Call::get(ROUTE_DEVICES)));
    assert_eq!(anonymous.status, StatusCode::FORBIDDEN);
    assert_eq!(anonymous.json()["code"], "no_session");
}

#[test]
fn minting_a_code_shows_it_once_and_never_stores_it() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();

    let reply = block_on(send(
        &app,
        Call::post(ROUTE_DEVICES, "device_name=iPad").cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let body = reply.json();
    let code = body["code"]
        .as_str()
        .expect("the response carries the code")
        .to_owned();
    assert_eq!(code.chars().count(), CODE_LENGTH, "eight characters");
    assert!(
        code.chars().all(|character| ALPHABET.contains(character)),
        "the code is drawn from the pairing alphabet: {code}"
    );
    let expires_at = body["expires_at"]
        .as_str()
        .expect("the response carries the expiry")
        .to_owned();
    assert!(
        expires_at.len() >= 20,
        "the expiry is a timestamp the operator can read: {expires_at}"
    );
    assert_eq!(body["device_name"], "iPad");

    // The device it was minted for is now a row, awaiting the code.
    let named: i64 = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM devices WHERE name = ?1 AND revoked_at IS NULL",
                ["iPad"],
                |row| row.get(0),
            )
        })
        .expect("device count");
    assert_eq!(named, 1, "the mint registers the device it names");

    // The code is not stored: not in `pairing_codes`, not in the audit log, not
    // anywhere else the daemon writes text. This is a whole-table read.
    for table in ["pairing_codes", "audit_log", "devices", "sessions", "jobs"] {
        let dumped = dump_text(db, &format!("SELECT * FROM {table}"));
        assert!(
            !dumped.contains(&code),
            "{table} holds the pairing code in plaintext"
        );
    }
    // The verifier is there, and it is not the code.
    let verifier = dump_text(db, "SELECT code_hash FROM pairing_codes");
    assert!(
        verifier.contains("$argon2"),
        "the stored pairing value is an Argon2id verifier: {verifier}"
    );

    // The audit row names the device and never the code.
    let issued = audit_rows(db)
        .into_iter()
        .find(|row| row.action == "device.pairing_code_issued")
        .expect("the mint is audited");
    assert_eq!(issued.detail.as_deref(), Some("iPad"));
    assert!(
        !format!("{issued:?}").contains(&code),
        "the audit row carries no code"
    );

    // Shown once: a later render of the page does not repeat it.
    let page = block_on(send(
        &app,
        Call::get(ROUTE_DEVICES).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        !page.body.contains(&code),
        "the code is not re-shown on the page: {}",
        page.body
    );
    // The device it was minted for is listed, awaiting the code.
    let minted_row = row_window(&page.body, "iPad");
    assert!(
        minted_row.contains(">awaiting code<"),
        "the minted device waits for its code: {minted_row}"
    );

    // The browser path shows the code in the response body exactly once.
    let html = block_on(send(
        &app,
        Call::post(ROUTE_DEVICES, "device_name=iPhone")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(html.status, StatusCode::OK, "{}", html.body);
    let at = html
        .body
        .find(CODE_MARK)
        .unwrap_or_else(|| panic!("the mint response shows the code: {}", html.body))
        + CODE_MARK.len();
    let shown = html.body[at..at + CODE_LENGTH].to_owned();
    assert!(
        shown.chars().all(|character| ALPHABET.contains(character)),
        "the shown value is a code: {shown}"
    );
    assert_eq!(
        html.body.matches(&shown).count(),
        1,
        "the code appears once in the response: {}",
        html.body
    );
    assert!(
        html.body.contains("shown once"),
        "the page says the code is shown once: {}",
        html.body
    );
    let dumped = dump_text(db, "SELECT * FROM pairing_codes");
    assert!(
        !dumped.contains(&shown),
        "the second code is not stored in plaintext either"
    );

    // A missing name is a refusal that mints nothing.
    let before: i64 = db
        .with_conn(|conn| {
            conn.query_row("SELECT COUNT(*) FROM pairing_codes", [], |row| row.get(0))
        })
        .expect("pairing count");
    let refused = block_on(send(
        &app,
        Call::post(ROUTE_DEVICES, "device_name=").cookie(Some(&cookie)),
    ));
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    assert_eq!(refused.json()["code"], "device_required");
    let after: i64 = db
        .with_conn(|conn| {
            conn.query_row("SELECT COUNT(*) FROM pairing_codes", [], |row| row.get(0))
        })
        .expect("pairing count");
    assert_eq!(before, after, "a refused mint writes no code");
}

#[test]
fn revoking_a_device_keeps_the_row_and_marks_it() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();

    seed_device(
        db,
        "dev-a",
        "iPhone",
        "ios",
        Some("2026-10-19T09:00:00.000Z"),
        None,
        None,
    );
    seed_redeemed_code(db, "dev-a");

    // The page's own form, posted to the route the page names.
    let reply = block_on(send(
        &app,
        Call::post(REVOKE, "device_id=dev-a").cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.json()["ok"], true);
    assert_eq!(reply.json()["device_id"], "dev-a");

    let revoked: Option<String> = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT revoked_at FROM devices WHERE id = ?1",
                ["dev-a"],
                |row| row.get::<_, Option<String>>(0),
            )
        })
        .expect("revoked_at read");
    assert!(revoked.is_some(), "the device row is marked revoked");

    // The row stays listed, and it now reads revoked with no second button.
    let page = block_on(send(
        &app,
        Call::get(ROUTE_DEVICES).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let row = row_window(&page.body, "iPhone");
    assert!(
        row.contains("iPhone"),
        "the revoked row keeps its name: {row}"
    );
    assert!(
        row.contains(">revoked<"),
        "the revoked row says revoked: {row}"
    );
    assert!(
        !row.contains(&format!(r#"action="{REVOKE}""#)),
        "a revoked device is not revoked twice: {row}"
    );

    // A revoke of a device that was never there is a refusal, not a quiet 200.
    let unknown = block_on(send(
        &app,
        Call::post(REVOKE, "device_id=no-such-device").cookie(Some(&cookie)),
    ));
    assert!(
        !unknown.status.is_success(),
        "an unknown device is refused: {} {}",
        unknown.status,
        unknown.body
    );
}
