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
    AppState, ROUTE_ACTIVITY, ROUTE_BACKUPS, ROUTE_CHAT, ROUTE_CHAT_NEW, ROUTE_DASHBOARD,
    ROUTE_DEVICES, ROUTE_LOGIN, ROUTE_SESSIONS, ROUTE_SETTINGS, router_with,
};

use super::harness::{
    Call, PURPLE_DENYLIST, block_on, ready_app, ready_engine, send, setup_engine, sign_in,
};

/// Every nav link that has a page behind it. Since P14 that is all of them: the
/// backups page exists, so nothing is left as a placeholder.
const LIVE_NAV: [&str; 7] = [
    ROUTE_CHAT,
    ROUTE_DASHBOARD,
    ROUTE_SESSIONS,
    ROUTE_DEVICES,
    ROUTE_SETTINGS,
    ROUTE_ACTIVITY,
    ROUTE_BACKUPS,
];

/// The text of the `title` element, or a panic that prints the page.
fn title_of(body: &str) -> String {
    let open = "<title>";
    let start = body
        .find(open)
        .unwrap_or_else(|| panic!("no title element: {body}"))
        + open.len();
    let end = body[start..]
        .find("</title>")
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("no closing title element: {body}"));
    body[start..end].to_owned()
}

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
fn the_dashboard_lists_every_bound_listener() {
    let (_dir, engine, passphrase) = ready_engine();
    let state = AppState::new(engine, false);
    let bound: Vec<std::net::SocketAddr> = vec![
        "127.0.0.1:7340".parse().expect("a literal address"),
        "192.168.1.10:7340".parse().expect("a literal address"),
    ];
    state.set_bound_listeners(bound);
    let app = router_with(state);
    let cookie = sign_in(&app, &passphrase);

    let page = block_on(send(
        &app,
        Call::get(ROUTE_DASHBOARD).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);

    for addr in ["127.0.0.1:7340", "192.168.1.10:7340"] {
        assert!(
            page.body.contains(addr),
            "the System card lists the bound address {addr}: {}",
            page.body
        );
    }
    assert!(
        page.body.contains("(2 listeners)"),
        "the footer counts the bound listeners: {}",
        page.body
    );
    assert!(
        !page.body.contains("(1 listener)"),
        "two listeners is not one: {}",
        page.body
    );
}

#[test]
fn the_rail_carries_new_chat_the_sessions_and_the_cog() {
    let (_dir, engine, passphrase) = ready_engine();
    // A conversation with a message, so the rail has something to list.
    crate::web::chat::append(
        engine.db(),
        crate::web::chat::CONVERSATION_ID,
        crate::web::chat::Role::User,
        "a question that names the conversation",
        crate::web::chat::State::Complete,
    )
    .unwrap();
    crate::web::chat::name_from_first_message(
        engine.db(),
        crate::web::chat::CONVERSATION_ID,
        "a question that names the conversation",
    )
    .unwrap();
    let app = router_with(AppState::new(engine, false));
    let cookie = sign_in(&app, &passphrase);

    let dashboard = block_on(send(
        &app,
        Call::get(ROUTE_DASHBOARD).html().cookie(Some(&cookie)),
    ));
    assert_eq!(dashboard.status, StatusCode::OK, "{}", dashboard.body);
    let body = &dashboard.body;

    assert!(body.contains(r#"<header class="side""#), "the rail renders");
    assert!(body.contains(">Kallisti<"), "the rail names the product");
    // New chat is an action, not a link: it creates a conversation.
    assert!(
        body.contains(&format!(r#"action="{ROUTE_CHAT_NEW}""#)),
        "the rail offers new chat: {body}"
    );
    // The conversation is listed and linked.
    assert!(
        body.contains(&format!(
            r#"href="/dashboard/chat/{}""#,
            crate::web::chat::CONVERSATION_ID
        )),
        "the rail lists the conversation: {body}"
    );
    assert!(
        body.contains("a question that names the conversation"),
        "the rail shows the title taken from the first message: {body}"
    );
    // The cog is the way into everything else.
    assert!(
        body.contains(&format!(r#"href="{ROUTE_SETTINGS}""#)),
        "the cog reaches settings: {body}"
    );
    assert!(
        body.contains(r#"aria-label="Settings""#),
        "the cog is labelled"
    );
    // The dashboard does carry the section row: it is a control surface, and the
    // row is how you leave it.
    assert!(
        body.contains(r#"class="sections""#),
        "the dashboard carries the section list: {body}"
    );

    // With the rail hidden the foot collapses into the corner it occupied: the
    // show control and the cog, together, instead of a stray button at the top.
    let hidden = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SIDEBAR, "rail=hidden")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(hidden.status, StatusCode::SEE_OTHER, "{}", hidden.body);
    let collapsed = block_on(send(
        &app,
        Call::get(ROUTE_DASHBOARD).html().cookie(Some(&cookie)),
    ));
    let body = &collapsed.body;
    assert!(
        body.contains(r#"class="rail-restore""#),
        "the foot collapses into one corner cluster: {body}"
    );
    assert!(
        body.contains(r#"aria-label="Show the sidebar""#),
        "the show control is in it"
    );
    assert!(
        body.contains(&format!(r#"href="{ROUTE_SETTINGS}""#)),
        "and so is the cog, where the word used to be"
    );
    assert!(
        body.contains(r#"aria-label="Settings""#),
        "the collapsed cog is still labelled for a reader"
    );

    // Put it back so the rest of the test reads a shown rail.
    let _ = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SIDEBAR, "rail=shown")
            .html()
            .cookie(Some(&cookie)),
    ));

    // Settings is where the sections live, and it marks itself.
    let settings = block_on(send(
        &app,
        Call::get(ROUTE_SETTINGS).html().cookie(Some(&cookie)),
    ));
    assert_eq!(settings.status, StatusCode::OK, "{}", settings.body);
    assert!(
        settings.body.contains(r#"class="sections""#),
        "settings lists the sections: {}",
        settings.body
    );
    let marked = owner_of_aria_current(&settings.body);
    assert!(
        marked.contains(ROUTE_SETTINGS),
        "settings marks itself current, found {marked}"
    );

    // The sign-in form is minimal: no rail actions, no conversations, no cog.
    let login = block_on(send(&app, Call::get(ROUTE_LOGIN).html()));
    assert_eq!(login.status, StatusCode::OK, "{}", login.body);
    assert!(
        !login
            .body
            .contains(&format!(r#"action="{ROUTE_CHAT_NEW}""#)),
        "the login page offers no new chat: {}",
        login.body
    );
    assert!(
        !login.body.contains(r#"class="sessions""#),
        "the login page lists no conversations: {}",
        login.body
    );
    assert!(
        !login.body.contains(&format!(r#"href="{ROUTE_SETTINGS}""#)),
        "the login page does not link settings: {}",
        login.body
    );

    // While the wizard is open there is no rail action either.
    let (_setup_dir, setup, _words) = setup_engine();
    let wizard_app = router_with(AppState::new(setup, false));
    let wizard = block_on(send(&wizard_app, Call::get("/setup")));
    assert_eq!(wizard.status, StatusCode::OK, "{}", wizard.body);
    assert!(
        !wizard
            .body
            .contains(&format!(r#"action="{ROUTE_CHAT_NEW}""#)),
        "the wizard offers no new chat: {}",
        wizard.body
    );
}

#[test]
fn the_settings_page_reaches_every_section_and_none_is_a_placeholder() {
    let (_dir, _engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());

    let settings = block_on(send(&app, Call::get(ROUTE_SETTINGS).html().cookie(cookie)));
    assert_eq!(settings.status, StatusCode::OK, "{}", settings.body);
    for route in LIVE_NAV {
        assert!(
            settings.body.contains(&format!(r#"href="{route}""#)),
            "settings links {route}: {}",
            settings.body
        );
    }
    assert_eq!(
        settings.body.matches(r#"aria-disabled="true""#).count(),
        0,
        "no section is unavailable: {}",
        settings.body
    );

    // Every admin page keeps its own identity and can get back to the cog.
    for route in [
        ROUTE_CHAT,
        ROUTE_DASHBOARD,
        ROUTE_SESSIONS,
        ROUTE_DEVICES,
        ROUTE_ACTIVITY,
        ROUTE_BACKUPS,
    ] {
        let page = block_on(send(&app, Call::get(route).html().cookie(cookie)));
        assert_eq!(page.status, StatusCode::OK, "{route}: {}", page.body);
        assert!(
            page.body.contains(&format!(r#"href="{ROUTE_SETTINGS}""#)),
            "{route} keeps the cog: {}",
            page.body
        );
        // Every surface except the conversation carries the section row. Chat is
        // the conversation: the row fights the transcript, and the cog is the way
        // out of it.
        if route == ROUTE_CHAT {
            assert!(
                !page.body.contains(r#"class="sections""#),
                "chat stays clear of the section row: {}",
                page.body
            );
        } else {
            assert!(
                page.body.contains(r#"class="sections""#),
                "{route} carries the section list: {}",
                page.body
            );
        }
    }
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
        ROUTE_CHAT,
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
        if label == ROUTE_CHAT {
            // The one exception, and it is deliberate: the chat page carries a
            // single inline script that upgrades its own form into a streaming
            // conversation. It has no `src`, reaches no network of its own, and
            // the form still works when it is blocked. Every other page, and any
            // second script anywhere, still fails this test.
            assert_eq!(
                lowered.matches("<script").count(),
                1,
                "{label} carries exactly one script"
            );
            assert!(
                !lowered.contains("<script src") && !lowered.contains("http://"),
                "{label} script is inline and fetches nothing"
            );
        } else {
            assert!(!lowered.contains("<script"), "{label} carries a script");
        }
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

/// Every page names itself and the product in its `title`, and names it the same
/// way: `<page> - Kallisti 1.0`.
///
/// The title is the one string a browser shows when the tab is the only thing
/// visible, so it is worth pinning rather than leaving to whichever template
/// happened to be copied.
#[test]
fn every_page_title_names_the_page_and_the_product() {
    let (_dir, _engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());

    let pages = [
        (ROUTE_DASHBOARD, "Dashboard"),
        (ROUTE_SESSIONS, "Sessions"),
        (ROUTE_DEVICES, "Devices"),
        (ROUTE_SETTINGS, "Settings"),
        (ROUTE_ACTIVITY, "Activity"),
        (ROUTE_BACKUPS, "Backups"),
    ];
    for (route, label) in pages {
        let page = block_on(send(&app, Call::get(route).html().cookie(cookie)));
        assert_eq!(page.status, StatusCode::OK, "{route}: {}", page.body);
        let title = title_of(&page.body);
        assert_eq!(
            title,
            format!("{label} - Kallisti 1.0"),
            "{route} names its page and the product"
        );
    }

    // The two pages outside the nav carry the same product name, so a tab is
    // never ambiguous.
    let login = block_on(send(&app, Call::get(ROUTE_LOGIN).html()));
    assert_eq!(login.status, StatusCode::OK, "{}", login.body);
    assert_eq!(title_of(&login.body), "Sign in - Kallisti 1.0");

    let (_setup_dir, setup, _words) = setup_engine();
    let wizard_app = router_with(AppState::new(setup, false));
    let wizard = block_on(send(&wizard_app, Call::get("/setup")));
    assert_eq!(wizard.status, StatusCode::OK, "{}", wizard.body);
    assert_eq!(title_of(&wizard.body), "Onboarding - Kallisti 1.0");
}
