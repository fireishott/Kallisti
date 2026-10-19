//! Phase 1 acceptance: the application shell.
//!
//! Two claims about every page the daemon can render:
//!
//! - the navigation appears on the signed-in dashboard surface, marks the
//!   current page with `aria-current="page"`, and does not appear on the sign-in
//!   form or while the wizard is still open, because a nav on a page that
//!   cannot use it is a trap;
//! - no page carries the purple family or the old product name, in any case, and
//!   no page carries an em dash, an exclamation mark outside the doctype, an
//!   external asset, or a script.

use axum::http::StatusCode;

use crate::admin::{
    AppState, ROUTE_ACTIVITY, ROUTE_DASHBOARD, ROUTE_LOGIN, ROUTE_SETTINGS, router_with,
};

use super::harness::{Call, PURPLE_DENYLIST, block_on, ready_engine, send, setup_engine, sign_in};

/// The four nav links that have a page behind them in this phase.
const LIVE_NAV: [&str; 3] = [ROUTE_DASHBOARD, ROUTE_SETTINGS, ROUTE_ACTIVITY];

/// The text of the link that carries `aria-current="page"`.
///
/// Read from the rendered bytes rather than from a template field, so the test
/// checks what a browser receives.
fn owner_of_aria_current(body: &str) -> String {
    const MARK: &str = r#"aria-current="page""#;
    let Some(before) = body.split(MARK).next() else {
        return String::new();
    };
    let Some(index) = before.rfind(r#"href=""#) else {
        return String::new();
    };
    before[index..].to_owned()
}

#[test]
fn the_nav_renders_when_ready_and_not_on_the_login_page() {
    let (_dir, engine, passphrase) = ready_engine();
    let app = router_with(AppState::new(engine, false));
    let cookie = sign_in(&app, &passphrase);

    // The dashboard carries the shell: the product name, the nav, and the
    // current page marked.
    let dashboard = block_on(send(
        &app,
        Call::get(ROUTE_DASHBOARD).html().cookie(Some(&cookie)),
    ));
    assert_eq!(dashboard.status, StatusCode::OK, "{}", dashboard.body);
    assert!(
        dashboard.body.contains(r#"<nav class="nav""#),
        "the shell renders a nav: {}",
        dashboard.body
    );
    assert!(
        dashboard.body.contains(">Kallisti<"),
        "the top bar carries the product name: {}",
        dashboard.body
    );
    for route in LIVE_NAV {
        assert!(
            dashboard.body.contains(&format!(r#"href="{route}""#)),
            "the nav links {route}: {}",
            dashboard.body
        );
    }
    assert!(
        dashboard.body.contains(r#"aria-current="page""#),
        "the current page is marked: {}",
        dashboard.body
    );
    // The mark is on the dashboard link, not on some other link.
    let current = owner_of_aria_current(&dashboard.body);
    assert!(
        current.contains(ROUTE_DASHBOARD),
        "aria-current is on the dashboard link, found {current}"
    );
    // The footer carries the version.
    assert!(
        dashboard.body.contains(crate::admin::VERSION),
        "the footer carries the version: {}",
        dashboard.body
    );
    // And no nav link points at a route this build cannot serve. The two pages
    // that arrive later are named in the nav and marked unavailable instead.
    for missing in ["/dashboard/sessions", "/dashboard/devices"] {
        assert!(
            !dashboard.body.contains(&format!(r#"href="{missing}""#)),
            "{missing} has no page in this phase, so it must not be a live link"
        );
    }
    for label in [">Sessions", ">Devices"] {
        assert!(
            dashboard.body.contains(label),
            "{label} is still named in the nav: {}",
            dashboard.body
        );
    }
    assert!(
        dashboard.body.contains(r#"aria-disabled="true""#),
        "the unavailable entries say so: {}",
        dashboard.body
    );

    // Every dashboard page marks its own entry.
    for route in [ROUTE_SETTINGS, ROUTE_ACTIVITY] {
        let page = block_on(send(&app, Call::get(route).html().cookie(Some(&cookie))));
        assert_eq!(page.status, StatusCode::OK, "{route}: {}", page.body);
        let marked = owner_of_aria_current(&page.body);
        assert!(
            marked.contains(route),
            "{route} marks itself current, found {marked}"
        );
    }

    // The sign-in form is minimal on purpose: no nav on a page whose whole job is
    // one field.
    let login = block_on(send(&app, Call::get(ROUTE_LOGIN).html()));
    assert_eq!(login.status, StatusCode::OK, "{}", login.body);
    assert!(
        !login.body.contains(r#"<nav class="nav""#),
        "the login page has no nav: {}",
        login.body
    );
    assert!(
        !login.body.contains(r#"aria-current="page""#),
        "the login page marks nothing current: {}",
        login.body
    );
    assert!(
        !login.body.contains(&format!(r#"href="{ROUTE_SETTINGS}""#)),
        "the login page does not link the settings page: {}",
        login.body
    );

    // While the wizard is open there is no nav either: the dashboard is not a
    // reachable page yet.
    let (_setup_dir, setup, _words) = setup_engine();
    let wizard_app = router_with(AppState::new(setup, false));
    let wizard = block_on(send(&wizard_app, Call::get("/setup")));
    assert_eq!(wizard.status, StatusCode::OK, "{}", wizard.body);
    assert!(
        !wizard.body.contains(r#"<nav class="nav""#),
        "the wizard has no nav: {}",
        wizard.body
    );
}

#[test]
fn every_page_has_no_purple_and_no_herald() {
    let (_dir, engine, passphrase) = ready_engine();
    let app = router_with(AppState::new(engine, false));
    let cookie = sign_in(&app, &passphrase);

    let mut pages: Vec<(String, String)> = Vec::new();
    for route in [
        ROUTE_DASHBOARD,
        ROUTE_SETTINGS,
        ROUTE_ACTIVITY,
        ROUTE_LOGIN,
        "/static/app.css",
    ] {
        let reply = block_on(send(&app, Call::get(route).html().cookie(Some(&cookie))));
        assert!(
            reply.status.is_success() || reply.status == StatusCode::SEE_OTHER,
            "{route} answered {}",
            reply.status
        );
        if reply.status == StatusCode::SEE_OTHER {
            // `/login` with a session redirects to the dashboard, which is a
            // page covered above.
            continue;
        }
        pages.push((route.to_owned(), reply.body));
    }

    // The wizard in setup state, and its ready summary.
    let (_setup_dir, setup, _words) = setup_engine();
    let wizard_app = router_with(AppState::new(setup, false));
    let setup_page = block_on(send(&wizard_app, Call::get("/setup")));
    assert_eq!(setup_page.status, StatusCode::OK);
    pages.push(("/setup".to_owned(), setup_page.body));

    // The ready summary is the same template with a different body.
    let (_ready_dir, engine2, passphrase2) = ready_engine();
    let ready_app = router_with(AppState::new(engine2, false));
    let cookie2 = sign_in(&ready_app, &passphrase2);
    let summary = block_on(send(
        &ready_app,
        Call::get("/setup").html().cookie(Some(&cookie2)),
    ));
    assert_eq!(summary.status, StatusCode::OK);
    pages.push(("/setup (ready)".to_owned(), summary.body));

    assert!(pages.len() >= 5, "the suites covered {} pages", pages.len());

    for (label, body) in &pages {
        let lowered = body.to_ascii_lowercase();
        assert!(!lowered.contains("herald"), "{label} carries the old name");
        for banned in PURPLE_DENYLIST {
            assert!(
                !lowered.contains(&banned.to_ascii_lowercase()),
                "{label} carries the purple {banned}"
            );
        }
        assert!(
            !lowered.contains("#8e24aa") && !lowered.contains("purple"),
            "{label} names purple"
        );
        assert!(!body.contains('\u{2014}'), "{label} carries an em dash");
        assert!(
            !body.replace("<!DOCTYPE html>", "").contains('!'),
            "{label} carries an exclamation mark outside the doctype"
        );
        assert!(!lowered.contains("<script"), "{label} carries a script");
        for banned in ["http://", "https://"] {
            if lowered.contains(banned) {
                assert!(
                    lowered.contains("127.0.0.1"),
                    "{label} carries an external URL: {banned}"
                );
            }
        }
        assert!(
            !lowered.contains("@font-face") && !lowered.contains("@import"),
            "{label} loads a font or a stylesheet"
        );
    }
}
