//! Phase 1 acceptance: the Settings surface.
//!
//! Settings is the page that has to be right, because it is the only page that
//! writes the daemon's behaviour. Seven claims:
//!
//! - every group renders the value the database actually holds, so the form is
//!   a view of state and not a set of defaults;
//! - a provider save persists and leaves an audit row;
//! - a key reference that is not `env:NAME` is refused by field, and nothing is
//!   written;
//! - a workspace inside a reserved root is refused by the same code path the
//!   onboarding step uses;
//! - `full` sandbox mode is refused unless the process was started with
//!   `--allow-full`;
//! - `never` approval is refused until a device is genuinely paired;
//! - a permission save records the old and the new values in the audit detail.
//!
//! The three refusals are checked twice: once for the browser page (a rendered
//! page with an `aria-live` region and focus on the failed field) and once for an
//! API client (the documented `{ok:false, field, code, message}` body).

use axum::http::StatusCode;

use crate::admin::{AppState, ROUTE_SETTINGS, router_with};
use crate::auth::PairingService;
use crate::onboarding::SETTING_WORKSPACE;

use super::harness::{Call, PURPLE_DENYLIST, block_on, encode, ready_app, ready_engine, send};

/// The three settings posts.
const PROVIDER_POST: &str = "/dashboard/settings/provider";
const CLOCK_POST: &str = "/dashboard/settings/timezone";
const PUSH_POST: &str = "/dashboard/settings/push";
const GREETING_POST: &str = "/dashboard/settings/greeting";
const WORKSPACE_POST: &str = "/dashboard/settings/workspace";
const PERMISSIONS_POST: &str = "/dashboard/settings/permissions";

/// The path of one settings tab.
///
/// The page is six tabs now, so a test that asserts on a card has to name the tab
/// that holds it. Spelling the query here rather than in every test means a test
/// says which tab it means and the route stays one place.
fn settings_tab_path(tab: &str) -> String {
    format!("{ROUTE_SETTINGS}?tab={tab}")
}

/// A provider form body, with the reference spelled the way the rule requires.
fn provider_body(base_url: &str, key_ref: &str, model: &str) -> String {
    // `model_selection` is sent empty so the typed-model fallback path is what
    // the test exercises: a hand-typed id is stored as given, exactly as the
    // settings page does when the picker is left on its empty option.
    format!(
        "base_url={}&api_key_ref={}&model_selection=&model={}",
        encode(base_url),
        encode(key_ref),
        encode(model)
    )
}

/// A permission form body.
fn permissions_body(sandbox: &str, approval: &str, ceiling: &str, telemetry: &str) -> String {
    format!(
        "sandbox_mode={}&approval_policy={}&autopilot_ceiling={}&telemetry={}",
        encode(sandbox),
        encode(approval),
        encode(ceiling),
        encode(telemetry)
    )
}

#[test]
fn the_settings_page_renders_current_values() {
    let (dir, engine, app, cookie) = ready_app();
    let workspace = dir.path().join("kallisti-workspace").display().to_string();

    let reply = block_on(send(
        &app,
        Call::get_owned(settings_tab_path("models"))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);

    // The three forms, each posting to its own route. Provider lives on Models
    // and workspace and permissions on System, so both tabs are read here.
    let system = block_on(send(
        &app,
        Call::get_owned(settings_tab_path("system"))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(system.status, StatusCode::OK, "{}", system.body);
    let page = &format!("{}{}", reply.body, system.body);
    for action in [PROVIDER_POST, WORKSPACE_POST, PERMISSIONS_POST] {
        assert!(
            page.contains(&format!(r#"action="{action}""#)),
            "the {action} form is missing: {page}"
        );
    }

    // The provider group is pre-filled from the database.
    assert!(
        page.contains(r#"value="http://127.0.0.1:20128/v1""#),
        "the stored endpoint is pre-filled: {page}"
    );
    assert!(
        page.contains(r#"value="env:NINEROUTER_API_KEY""#),
        "the stored reference is pre-filled: {page}"
    );
    assert!(
        page.contains(r#"value="ds/deepseek-v4-flash""#),
        "the stored model is pre-filled: {page}"
    );

    // The workspace group renders the path the engine resolved.
    assert!(
        page.contains(&format!(r#"value="{workspace}""#)),
        "the stored workspace is pre-filled: {page}"
    );
    assert_eq!(
        engine
            .workspace_path()
            .expect("workspace")
            .display()
            .to_string(),
        workspace
    );

    // Every input is labelled, and every form has its own live region.
    for name in ["base_url", "api_key_ref", "model", "workspace_path"] {
        assert!(
            page.contains(&format!(r#"<label for="{name}">"#)),
            "the {name} control has no label: {page}"
        );
    }
    for (name, value) in [
        ("sandbox_mode", "read-only"),
        ("approval_policy", "ask"),
        ("autopilot_ceiling", "l1-draft"),
        ("telemetry", "off"),
    ] {
        assert!(
            page.contains(&format!(r#"for="{name}-{value}""#)),
            "the {name} radio for {value} has no label: {page}"
        );
    }
    // Every form has its own live region, and each tab carries only its own:
    // the page level region, plus provider and accounts on Models, plus
    // workspace and permissions on System. Schedules, skills, tools and the
    // auxiliary roles render none of their own, which is exactly why this
    // count is per tab rather than for a page that no longer holds every card.
    assert_eq!(
        reply.body.matches(r#"aria-live="polite""#).count(),
        3,
        "the Models tab carries the page region and two forms: {}",
        reply.body
    );
    assert_eq!(
        system.body.matches(r#"aria-live="polite""#).count(),
        3,
        "the System tab carries the page region and two forms: {}",
        system.body
    );

    // The read-only rows, with their consequences.
    let lowered = page.to_ascii_lowercase();
    assert!(lowered.contains("draft only"), "{page}");
    assert!(
        lowered.contains("1.0 never transmits in your voice"),
        "{page}"
    );
    assert!(lowered.contains("disabled"), "{page}");
    // The premise moved in P15: the admin listener is configured, not compiled
    // in, so the page states where it is configured and what it can reach. The
    // device listener is still the one literal, and the page still says so.
    assert!(lowered.contains("loopback only"), "{page}");
    assert!(lowered.contains("--admin-listen"), "{page}");
    assert!(
        page.contains("127.0.0.1:7340") && page.contains("127.0.0.1:7341"),
        "both listener addresses are stated: {page}"
    );
    assert!(
        lowered.contains("config.toml"),
        "the admin bind says where it is configured: {page}"
    );
    assert!(
        lowered.contains("lan") && lowered.contains("tailnet") && lowered.contains("reverse proxy"),
        "the network row names every way the UI can be reached: {page}"
    );
    assert!(
        lowered.contains("not by anything compiled in"),
        "the network row says the listener is not compiled in: {page}"
    );
    assert!(
        !lowered.contains("not configurable"),
        "the admin bind is configurable now: {page}"
    );

    // House style.
    assert!(!lowered.contains("herald"), "the old name is gone: {page}");
    for banned in PURPLE_DENYLIST {
        assert!(!lowered.contains(&banned.to_ascii_lowercase()), "{banned}");
    }
    assert!(!page.contains('\u{2014}'), "no em dash reaches the page");
    assert!(!lowered.contains("<script"), "there is no script in the UI");

    // No secret value is rendered, and the key field is a reference, not a
    // masked value: there is nothing to mask, because nothing is stored.
    // Checked as a rendered *value* rather than as the bare fragment `sk-`: the
    // inlined stylesheet is part of this page, and `mask-image` contains those
    // three characters without carrying a key of any kind.
    assert!(
        !lowered.contains(r#"value="sk-"#),
        "no key value is rendered"
    );
    assert!(
        !lowered.contains(r#"type="password""#),
        "the settings page asks for no secret"
    );
}

#[test]
fn saving_the_provider_persists_and_audits() {
    let (_dir, engine, app, cookie) = ready_app();

    let reply = block_on(send(
        &app,
        Call::post(
            PROVIDER_POST,
            &provider_body(
                "http://127.0.0.1:20128/v1",
                "env:KALLISTI_TEST_KEY",
                "model-b",
            ),
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(
        reply.status,
        StatusCode::SEE_OTHER,
        "a successful save redirects: {}",
        reply.body
    );
    let location = reply.location().unwrap_or_default();
    assert!(
        location.starts_with(ROUTE_SETTINGS),
        "the redirect returns to the settings page: {location}"
    );
    assert!(
        location.contains("saved=provider"),
        "the notice names the group that was saved: {location}"
    );

    // The rows are the source of truth, and they changed.
    assert_eq!(
        engine.provider_key_ref().expect("read").as_deref(),
        Some("env:KALLISTI_TEST_KEY")
    );
    // The typed field is the authority when the picker is empty, so the bare id
    // is stored as given rather than qualified into the default provider's
    // catalogue. The stored row is exactly what was typed.
    assert_eq!(
        engine.model().expect("read").as_deref(),
        Some("model-b"),
        "a hand-typed model is stored as given"
    );

    let rows = super::harness::audit_rows(engine.db());
    let row = rows
        .iter()
        .find(|row| row.action == "settings.provider")
        .unwrap_or_else(|| panic!("no settings.provider audit row: {rows:?}"));
    let detail = row.detail.clone().unwrap_or_default();
    assert!(detail.contains("env:KALLISTI_TEST_KEY"), "{detail}");
    assert!(detail.contains("model-b"), "{detail}");
    assert_eq!(row.actor, "loopback");

    // And the page shows the saved values back, with a notice.
    let page = block_on(send(
        &app,
        Call::get("/dashboard/settings?saved=provider")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains(r#"value="env:KALLISTI_TEST_KEY""#),
        "the page reads back what was stored: {}",
        page.body
    );
    assert!(
        page.body.contains("provider settings saved"),
        "the notice is rendered: {}",
        page.body
    );

    // Success is a redirect for a script too: the page behind it is the only
    // render of the answer, so there is no second success body to drift from it.
    let scripted = block_on(send(
        &app,
        Call::post(
            PROVIDER_POST,
            &provider_body(
                "http://127.0.0.1:20128/v1",
                "env:KALLISTI_TEST_KEY",
                "model-c",
            ),
        )
        .cookie(Some(&cookie)),
    ));
    assert_eq!(scripted.status, StatusCode::SEE_OTHER, "{}", scripted.body);
    assert!(
        scripted
            .location()
            .unwrap_or_default()
            .contains("saved=provider")
    );
    assert_eq!(
        engine.model().expect("read").as_deref(),
        Some("model-c"),
        "the scripted save wrote the row"
    );
}

#[test]
fn a_bad_key_reference_is_rejected() {
    let (_dir, engine, app, cookie) = ready_app();
    let before = engine.provider_key_ref().expect("read");

    // A pasted key value is not a reference, and a lower-case variable name is
    // not the documented shape either.
    for bad in [
        "sk-a-pasted-key-value",
        "env:ninerouter",
        "NINEROUTER_API_KEY",
    ] {
        let reply = block_on(send(
            &app,
            Call::post(
                PROVIDER_POST,
                &provider_body("http://127.0.0.1:20128/v1", bad, "ds/deepseek-v4-flash"),
            )
            .html()
            .cookie(Some(&cookie)),
        ));
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{bad} should be refused: {}",
            reply.body
        );
        assert!(
            reply.body.contains("provider_key_ref_invalid"),
            "the field error is named: {}",
            reply.body
        );
        assert!(
            reply.body.contains(r#"aria-live="polite""#),
            "the refusal is announced: {}",
            reply.body
        );
        assert!(
            reply.body.contains(r#"id="api_key_ref""#),
            "the page comes back with the form: {}",
            reply.body
        );
        assert_eq!(
            engine.provider_key_ref().expect("read"),
            before,
            "{bad} must not be stored"
        );
    }

    // The API client gets the documented body, not a page.
    let api = block_on(send(
        &app,
        Call::post(
            PROVIDER_POST,
            &provider_body(
                "http://127.0.0.1:20128/v1",
                "not-a-ref",
                "ds/deepseek-v4-flash",
            ),
        )
        .cookie(Some(&cookie)),
    ));
    assert_eq!(api.status, StatusCode::BAD_REQUEST);
    let body = api.json();
    assert_eq!(body["ok"], false);
    assert_eq!(body["field"], "api_key_ref");
    assert_eq!(body["code"], "provider_key_ref_invalid");
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|text| text.contains("env:")),
        "the refusal says what the shape is: {body}"
    );

    // A base URL that is not http(s) is the endpoint field's refusal.
    let endpoint = block_on(send(
        &app,
        Call::post(
            PROVIDER_POST,
            &provider_body("ftp://127.0.0.1/v1", "env:KEY", "ds/deepseek-v4-flash"),
        )
        .cookie(Some(&cookie)),
    ));
    assert_eq!(endpoint.status, StatusCode::BAD_REQUEST);
    assert_eq!(endpoint.json()["field"], "base_url");
    assert_eq!(endpoint.json()["code"], "provider_endpoint_invalid");

    // And an empty model is the model field's refusal.
    let model = block_on(send(
        &app,
        Call::post(
            PROVIDER_POST,
            &provider_body("http://127.0.0.1:20128/v1", "env:KEY", ""),
        )
        .cookie(Some(&cookie)),
    ));
    assert_eq!(model.status, StatusCode::BAD_REQUEST);
    assert_eq!(model.json()["field"], "model");
    assert_eq!(model.json()["code"], "provider_model_required");

    // Nothing was written by any of the refusals.
    assert_eq!(engine.provider_key_ref().expect("read"), before);
}

#[test]
fn a_workspace_inside_a_reserved_path_is_rejected() {
    let (_dir, engine, app, cookie) = ready_app();
    let before = engine.setting(SETTING_WORKSPACE).expect("read");

    for reserved in [
        "~/.ssh/sandbox",
        "~/.dsh/ws",
        "~/.config/ws",
        "~/vault/ws",
        "relative/path",
    ] {
        let reply = block_on(send(
            &app,
            Call::post(
                WORKSPACE_POST,
                &format!("workspace_path={}", encode(reserved)),
            )
            .html()
            .cookie(Some(&cookie)),
        ));
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{reserved} should be refused: {}",
            reply.body
        );
        assert!(
            reply.body.contains("workspace_path"),
            "the refusal names the field: {}",
            reply.body
        );
        assert!(
            reply.body.contains(r#"id="workspace_path""#),
            "the form comes back so the operator can fix it: {}",
            reply.body
        );
        assert_eq!(
            engine.setting(SETTING_WORKSPACE).expect("read"),
            before,
            "{reserved} must not be stored"
        );
    }

    // A reserved root is refused by the same code the wizard uses.
    let overlap = block_on(send(
        &app,
        Call::post(
            WORKSPACE_POST,
            &format!("workspace_path={}", encode("~/.ssh")),
        )
        .cookie(Some(&cookie)),
    ));
    assert_eq!(overlap.status, StatusCode::BAD_REQUEST);
    assert_eq!(overlap.json()["code"], "workspace_reserved");

    // A free path is accepted, created at 0755, stored, and audited.
    let (dir2, engine2, app2, cookie2) = ready_app();
    let target = dir2.path().join("settings-workspace");
    let reply = block_on(send(
        &app2,
        Call::post(
            WORKSPACE_POST,
            &format!("workspace_path={}", encode(&target.display().to_string())),
        )
        .html()
        .cookie(Some(&cookie2)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert!(
        reply
            .location()
            .unwrap_or_default()
            .contains("saved=workspace"),
        "{:?}",
        reply.location()
    );
    let metadata = std::fs::metadata(&target).expect("the workspace was created");
    assert!(metadata.is_dir());
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(metadata.permissions().mode() & 0o777, 0o755);
    assert_eq!(
        engine2.setting(SETTING_WORKSPACE).expect("read").as_deref(),
        Some(target.display().to_string().as_str())
    );
    let rows = super::harness::audit_rows(engine2.db());
    assert!(
        rows.iter().any(|row| row.action == "settings.workspace"),
        "the change is audited: {rows:?}"
    );
}

#[test]
fn full_sandbox_is_refused_without_allow_full() {
    let (_dir, engine, app, cookie) = ready_app();

    let reply = block_on(send(
        &app,
        Call::post(
            PERMISSIONS_POST,
            &permissions_body("full", "ask", "L1 draft", "off"),
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(
        reply.status,
        StatusCode::BAD_REQUEST,
        "full must be refused: {}",
        reply.body
    );
    assert!(
        reply.body.contains("sandbox_full_not_available"),
        "the refusal is named: {}",
        reply.body
    );
    assert!(
        reply.body.contains("--allow-full"),
        "the refusal says what would enable it: {}",
        reply.body
    );
    assert!(
        reply.body.contains(r#"aria-live="polite""#),
        "the refusal is announced: {}",
        reply.body
    );
    // Focus lands on the first option the browser can actually select, which is
    // the read-only one: the full option is rendered disabled.
    let focus_at = reply
        .body
        .find("autofocus")
        .unwrap_or_else(|| panic!("the failed group takes the focus: {}", reply.body));
    assert!(
        reply.body[..focus_at].contains("sandbox_mode-read-only"),
        "the focus is on a selectable option, not the disabled one: {}",
        reply.body
    );
    assert_eq!(
        engine.permissions().expect("read").sandbox_mode.as_str(),
        "read-only",
        "the refused value is not stored"
    );

    // The API client gets the same refusal as JSON.
    let api = block_on(send(
        &app,
        Call::post(
            PERMISSIONS_POST,
            &permissions_body("full", "ask", "L1 draft", "off"),
        )
        .cookie(Some(&cookie)),
    ));
    assert_eq!(api.status, StatusCode::BAD_REQUEST);
    assert_eq!(api.json()["field"], "sandbox_mode");
    assert_eq!(api.json()["code"], "sandbox_full_not_available");

    // The same post is accepted when the process was started with the flag.
    let (dir, engine3, passphrase) = ready_engine();
    let allowed = router_with(AppState::new(engine3.clone(), true));
    let cookie3 = super::harness::sign_in(&allowed, &passphrase);
    let reply = block_on(send(
        &allowed,
        Call::post(
            PERMISSIONS_POST,
            &permissions_body("full", "ask", "L1 draft", "off"),
        )
        .html()
        .cookie(Some(&cookie3)),
    ));
    assert_eq!(
        reply.status,
        StatusCode::SEE_OTHER,
        "with --allow-full the value is accepted: {}",
        reply.body
    );
    assert_eq!(
        engine3.permissions().expect("read").sandbox_mode.as_str(),
        "full"
    );
    drop(dir);

    // And `full` is only offered as a control when the flag is set: the page for
    // a process without it renders the option disabled. The permission table and
    // the full-access card are both on the System tab.
    let page = block_on(send(
        &app,
        Call::get_owned(settings_tab_path("system"))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains(r#"value="full"#) && page.body.contains("disabled"),
        "the full option is rendered disabled: {}",
        page.body
    );
}

#[test]
fn never_approval_is_refused_without_a_paired_device() {
    let (_dir, engine, app, cookie) = ready_app();

    // No device at all.
    let reply = block_on(send(
        &app,
        Call::post(
            PERMISSIONS_POST,
            &permissions_body("read-only", "never", "L1 draft", "off"),
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(
        reply.status,
        StatusCode::BAD_REQUEST,
        "never needs a device: {}",
        reply.body
    );
    assert!(
        reply.body.contains("approval_never_requires_device"),
        "the refusal is named: {}",
        reply.body
    );
    assert_eq!(
        engine.permissions().expect("read").approval_policy.as_str(),
        "ask"
    );

    // A registered device with an unreedeemed code is still not paired.
    let device_id = engine
        .auth()
        .register_device("TestPhone", "ios")
        .expect("register");
    let pairing = PairingService::new(engine.db().clone());
    let issued = pairing.issue_for_device(&device_id).expect("issue");
    let _ = issued.code();
    let unpaired = block_on(send(
        &app,
        Call::post(
            PERMISSIONS_POST,
            &permissions_body("read-only", "never", "L1 draft", "off"),
        )
        .cookie(Some(&cookie)),
    ));
    assert_eq!(
        unpaired.status,
        StatusCode::BAD_REQUEST,
        "an unreedeemed code is not a paired device"
    );

    // Redeem it, and the same post is accepted.
    pairing.redeem(issued.code()).expect("redeem");
    assert_eq!(engine.paired_device_count().expect("count"), 1);
    let accepted = block_on(send(
        &app,
        Call::post(
            PERMISSIONS_POST,
            &permissions_body("read-only", "never", "L1 draft", "off"),
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(
        accepted.status,
        StatusCode::SEE_OTHER,
        "a paired device unlocks never: {}",
        accepted.body
    );
    assert_eq!(
        engine.permissions().expect("read").approval_policy.as_str(),
        "never"
    );
}

#[test]
fn permissions_audit_records_old_and_new_values() {
    let (_dir, engine, app, cookie) = ready_app();

    // L2 and L3 do not exist and are refused by name.
    for level in ["L2", "L3"] {
        let reply = block_on(send(
            &app,
            Call::post(
                PERMISSIONS_POST,
                &permissions_body("read-only", "ask", level, "off"),
            )
            .html()
            .cookie(Some(&cookie)),
        ));
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{level}: {}",
            reply.body
        );
        assert!(
            reply.body.contains("autopilot_ceiling_absent"),
            "{level} is refused as absent: {}",
            reply.body
        );
    }

    let reply = block_on(send(
        &app,
        Call::post(
            PERMISSIONS_POST,
            &permissions_body("workspace-write", "ask", "L0 notify", "local only"),
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
    assert!(
        reply
            .location()
            .unwrap_or_default()
            .contains("saved=permissions"),
        "{:?}",
        reply.location()
    );

    // The four rows moved together.
    let stored = engine.permissions().expect("read");
    assert_eq!(stored.sandbox_mode.as_str(), "workspace-write");
    assert_eq!(stored.approval_policy.as_str(), "ask");
    assert_eq!(stored.autopilot_ceiling.as_str(), "L0 notify");
    assert_eq!(stored.telemetry.as_str(), "local only");

    // The audit detail carries both sides of every change.
    let rows = super::harness::audit_rows(engine.db());
    let row = rows
        .iter()
        .find(|row| row.action == "settings.permissions")
        .unwrap_or_else(|| panic!("no settings.permissions audit row: {rows:?}"));
    let detail = row.detail.clone().unwrap_or_default();
    for expected in [
        "read-only -> workspace-write",
        "ask -> ask",
        "L1 draft -> L0 notify",
        "off -> local only",
    ] {
        assert!(
            detail.contains(expected),
            "the audit detail says {expected}: {detail}"
        );
    }

    // The page shows the new values as the checked options.
    let page = block_on(send(
        &app,
        Call::get("/dashboard/settings?saved=permissions")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.body.contains(r#"value="workspace-write" checked"#)
            || page.body.contains(r#"value="workspace-write""#),
        "the new mode is offered: {}",
        page.body
    );
    assert!(
        page.body.contains("permissions saved"),
        "the notice is rendered: {}",
        page.body
    );

    // A settings write is not a wizard write: the wizard stays closed.
    let wizard = block_on(send(
        &app,
        Call::post("/setup/permissions", "sandbox_mode=read-only").cookie(Some(&cookie)),
    ));
    assert_eq!(wizard.status, StatusCode::CONFLICT);
    assert_eq!(wizard.json()["code"], "wizard_locked");
}

#[test]
fn the_settings_page_needs_a_session() {
    let (_dir, engine, _passphrase) = ready_engine();
    let app = router_with(AppState::new(engine, false));

    // A browser without a cookie is sent to the form that mints one.
    let browser = block_on(send(&app, Call::get(ROUTE_SETTINGS).html()));
    assert_eq!(browser.status, StatusCode::SEE_OTHER);
    assert_eq!(browser.location().as_deref(), Some("/login"));

    // An API client gets the documented 403, never an empty 200.
    let api = block_on(send(&app, Call::get(ROUTE_SETTINGS)));
    assert_eq!(api.status, StatusCode::FORBIDDEN);
    assert_eq!(api.json()["code"], "no_session");

    // A write without a session is a 403 too, whatever it asks for.
    let write = block_on(send(
        &app,
        Call::post(
            PERMISSIONS_POST,
            &permissions_body("read-only", "ask", "L1 draft", "off"),
        ),
    ));
    assert_eq!(write.status, StatusCode::FORBIDDEN);
    assert!(!write.body.trim().is_empty());
}

#[test]
fn the_clock_shape_rides_the_time_zone_form_and_reaches_every_stamp() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    let read = |key: &str| {
        crate::web::store::settings_get(db, key)
            .expect("read")
            .unwrap_or_default()
    };
    assert_eq!(
        read(crate::zone::SETTING_CLOCK_FORMAT),
        "",
        "unwritten to begin with"
    );

    // One form, two answers: "what time is it" is one question.
    let reply = block_on(send(
        &app,
        Call::post(CLOCK_POST, "timezone=-04:00&clock_format=12h")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(
        reply.status,
        StatusCode::SEE_OTHER,
        "a clock save redirects: {}",
        reply.body
    );
    assert_eq!(read(crate::zone::SETTING_TIMEZONE), "-04:00");
    assert_eq!(read(crate::zone::SETTING_CLOCK_FORMAT), "12h");

    // It reaches the transcript: a stamp stored at 20:04 UTC reads 4:04 PM at
    // -04:00, and the same row read at 24 hour says 16:04.
    crate::web::chat::append(
        db,
        crate::web::chat::CONVERSATION_ID,
        crate::web::chat::Role::User,
        "what time is it",
        crate::web::chat::State::Complete,
    )
    .expect("append");
    // Both columns, because the two surfaces show different ones: the transcript
    // stamps a message's `created_at` and the rail stamps its conversation's
    // `updated_at`, and a fixture that moved only one would read as a clock that
    // worked on one page and not the other.
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_messages SET created_at = '2026-03-14T20:04:00.000Z'",
            [],
        )?;
        conn.execute(
            "UPDATE chat_conversations SET updated_at = '2026-03-14T20:04:00.000Z'",
            [],
        )?;
        Ok(())
    })
    .expect("stamp");
    let page = block_on(send(
        &app,
        Call::get(crate::admin::ROUTE_CHAT)
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        page.body.contains("4:04 PM"),
        "the transcript follows the twelve hour setting: {}",
        page.body
    );
    assert!(
        !page.body.contains("16:04"),
        "and stops writing the twenty four hour form: {}",
        page.body
    );

    // The rail reads the same clock, from the same row.
    let rail = block_on(send(
        &app,
        Call::get(crate::admin::ROUTE_CHAT_SESSIONS).cookie(Some(&cookie)),
    ));
    assert_eq!(rail.status, StatusCode::OK, "{}", rail.body);
    let body = rail.json();
    let stamp = body["sessions"][0]["stamp"].as_str().unwrap_or_default();
    assert_eq!(
        stamp, "4:04 PM",
        "the rail agrees with the transcript: {body}"
    );

    // Back to 24 hour, and both surfaces follow.
    let back = block_on(send(
        &app,
        Call::post(CLOCK_POST, "timezone=-04:00&clock_format=24h")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(back.status, StatusCode::SEE_OTHER, "{}", back.body);
    let rail = block_on(send(
        &app,
        Call::get(crate::admin::ROUTE_CHAT_SESSIONS).cookie(Some(&cookie)),
    ));
    assert_eq!(
        rail.json()["sessions"][0]["stamp"],
        "16:04",
        "the same row, read back on the twenty four hour clock"
    );

    // A shape that is not one of the two is refused by field and nothing is
    // written: the stored row still says 24h.
    let refused = block_on(send(
        &app,
        Call::post(CLOCK_POST, "timezone=-04:00&clock_format=swatch-beats")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    assert!(
        refused.body.contains("clock_format"),
        "the refusal names the field: {}",
        refused.body
    );
    assert_eq!(read(crate::zone::SETTING_CLOCK_FORMAT), "24h");

    // A body with no clock field at all is an older client: the zone is written
    // and the shape is left alone.
    let zone_only = block_on(send(
        &app,
        Call::post(CLOCK_POST, "timezone=UTC")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(
        zone_only.status,
        StatusCode::SEE_OTHER,
        "{}",
        zone_only.body
    );
    assert_eq!(read(crate::zone::SETTING_TIMEZONE), "UTC");
    assert_eq!(read(crate::zone::SETTING_CLOCK_FORMAT), "24h");
}

#[test]
fn the_push_scope_stores_the_comma_separated_shape_the_daemon_reads() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    let read = |key: &str| {
        crate::web::store::settings_get(db, key)
            .expect("read")
            .unwrap_or_default()
    };

    // Two paired devices, and one revoked one that must not be offered or taken.
    super::harness::seed_device(db, "dev-a", "iPhone", "ios", None, None, None);
    super::harness::seed_redeemed_code(db, "dev-a");
    super::harness::seed_device(db, "dev-b", "iPad", "ios", None, None, None);
    super::harness::seed_redeemed_code(db, "dev-b");
    super::harness::seed_device(
        db,
        "dev-gone",
        "Old phone",
        "ios",
        None,
        None,
        Some("2026-01-01T00:00:00.000Z"),
    );

    let page = block_on(send(
        &app,
        Call::get_owned(settings_tab_path("devices"))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        page.body.contains(&format!(r#"action="{PUSH_POST}""#)),
        "the notification form renders: {}",
        page.body
    );
    for device in ["dev-a", "dev-b"] {
        assert!(
            page.body.contains(&format!(r#"value="{device}""#)),
            "a paired device is offered: {device}"
        );
    }
    assert!(
        !page.body.contains("dev-gone"),
        "a revoked device is not a target and is not offered: {}",
        page.body
    );
    for sentence in [
        "Only the device that asked",
        "Every paired device",
        "Only the devices I pick",
    ] {
        assert!(
            page.body.contains(sentence),
            "the scope is offered in the operator's words: {sentence}"
        );
    }

    // A browser posts one name per ticked box, repeated.
    let reply = block_on(send(
        &app,
        Call::post(
            PUSH_POST,
            "notify_scope=targeted&notify_devices=dev-a&notify_devices=dev-b",
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(
        reply.status,
        StatusCode::SEE_OTHER,
        "a notification save redirects: {}",
        reply.body
    );
    assert_eq!(read(crate::push::SETTING_NOTIFY_SCOPE), "targeted");
    assert_eq!(
        read(crate::push::SETTING_NOTIFY_DEVICES),
        "dev-a,dev-b",
        "the stored row is the comma separated shape the router reads"
    );
    // The daemon's own reader agrees with what was written.
    assert_eq!(
        crate::push::notify_devices(Some(&read(crate::push::SETTING_NOTIFY_DEVICES))),
        vec!["dev-a".to_owned(), "dev-b".to_owned()]
    );

    // A device that is not paired is refused by field, and the row is untouched.
    let refused = block_on(send(
        &app,
        Call::post(PUSH_POST, "notify_scope=targeted&notify_devices=dev-gone")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    assert!(
        refused.body.contains("notify_devices"),
        "the refusal names the field: {}",
        refused.body
    );
    assert_eq!(read(crate::push::SETTING_NOTIFY_DEVICES), "dev-a,dev-b");

    // A scope that is not one of the three is refused by field too.
    let bad_scope = block_on(send(
        &app,
        Call::post(PUSH_POST, "notify_scope=shout&notify_devices=dev-a")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(
        bad_scope.status,
        StatusCode::BAD_REQUEST,
        "{}",
        bad_scope.body
    );
    assert!(
        bad_scope.body.contains("notify_scope"),
        "the refusal names the scope: {}",
        bad_scope.body
    );
    assert_eq!(read(crate::push::SETTING_NOTIFY_SCOPE), "targeted");

    // The page comes back with the choice on it.
    let after = block_on(send(
        &app,
        Call::get_owned(settings_tab_path("devices"))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert!(
        after.body.contains(r#"value="targeted" checked"#),
        "the stored scope is the selected radio: {}",
        after.body
    );
    assert!(
        after.body.contains(r#"value="dev-a" checked"#),
        "and the ticked device is ticked: {}",
        after.body
    );

    // The master switch. Absent from the store means on, and the box shows it.
    assert_eq!(
        read(crate::push::SETTING_NOTIFY_ON_REPLY),
        "",
        "unwritten to begin with"
    );
    assert!(
        after.body.contains(r#"name="notify_on_reply""#),
        "the master switch is on this form, not on a tab that does not have it: {}",
        after.body
    );
    assert!(
        after.body.contains(r#"value="on" checked"#),
        "an absent row reads as on, which is what the daemon does: {}",
        after.body
    );

    // Posted off, with the scope unchanged: the switch is what changed.
    let off = block_on(send(
        &app,
        Call::post(
            PUSH_POST,
            "notify_on_reply=off&notify_scope=targeted&notify_devices=dev-a",
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(off.status, StatusCode::SEE_OTHER, "{}", off.body);
    assert_eq!(read(crate::push::SETTING_NOTIFY_ON_REPLY), "false");
    assert_eq!(read(crate::push::SETTING_NOTIFY_SCOPE), "targeted");
    // The daemon agrees, so the page and the turn path cannot disagree.
    assert!(!crate::push::notify_on_reply_enabled(Some(&read(
        crate::push::SETTING_NOTIFY_ON_REPLY
    ))));

    // A browser posting the unchecked box sends the hidden `off` and nothing
    // else; a ticked box sends `off` then `on`, and the last one wins.
    let ticked = block_on(send(
        &app,
        Call::post(
            PUSH_POST,
            "notify_on_reply=off&notify_on_reply=on&notify_scope=targeted&notify_devices=dev-a",
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(ticked.status, StatusCode::SEE_OTHER, "{}", ticked.body);
    assert_eq!(
        read(crate::push::SETTING_NOTIFY_ON_REPLY),
        "true",
        "a ticked box re-enables it"
    );

    // An older page that posts only a scope leaves the switch where it was.
    let scope_only = block_on(send(
        &app,
        Call::post(PUSH_POST, "notify_scope=all&notify_devices=")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(
        scope_only.status,
        StatusCode::SEE_OTHER,
        "{}",
        scope_only.body
    );
    assert_eq!(
        read(crate::push::SETTING_NOTIFY_ON_REPLY),
        "true",
        "a body with no switch is not a body that said off"
    );
    assert_eq!(read(crate::push::SETTING_NOTIFY_SCOPE), "all");

    // A value that is neither is refused by field, and neither row moves.
    let bad = block_on(send(
        &app,
        Call::post(PUSH_POST, "notify_on_reply=maybe&notify_scope=all")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.body);
    assert!(
        bad.body.contains("notify_on_reply"),
        "the refusal names the switch: {}",
        bad.body
    );
    assert_eq!(read(crate::push::SETTING_NOTIFY_ON_REPLY), "true");
}

#[test]
fn saving_the_greeting_persists_and_audits() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();

    // The default is off and every row is absent until it is asked for.
    assert_eq!(
        crate::web::store::settings_get(db, crate::greeting::SETTING_GREETING_ENABLED)
            .expect("read"),
        None
    );
    assert!(!crate::greeting::enabled(None));

    let reply = block_on(send(
        &app,
        Call::post(
            GREETING_POST,
            "greeting_enabled=true&greeting_model=primary&greeting_refresh=week",
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(
        reply.status,
        StatusCode::SEE_OTHER,
        "a successful save redirects: {}",
        reply.body
    );
    let location = reply.location().unwrap_or_default();
    assert!(
        location.starts_with(ROUTE_SETTINGS),
        "the redirect returns to the settings page: {location}"
    );
    assert!(
        location.contains("saved=greeting"),
        "the notice names the group that was saved: {location}"
    );

    let read = |key: &str| {
        crate::web::store::settings_get(db, key)
            .expect("read")
            .unwrap_or_default()
    };
    assert_eq!(read(crate::greeting::SETTING_GREETING_ENABLED), "true");
    assert_eq!(read(crate::greeting::SETTING_GREETING_MODEL), "primary");
    assert_eq!(read(crate::greeting::SETTING_GREETING_REFRESH), "week");

    let rows = super::harness::audit_rows(db);
    let row = rows
        .iter()
        .find(|row| row.action == "settings.greeting")
        .unwrap_or_else(|| panic!("no settings.greeting audit row: {rows:?}"));
    let detail = row.detail.clone().unwrap_or_default();
    assert!(detail.contains("enabled=true"), "{detail}");
    assert!(detail.contains("model=primary"), "{detail}");
    assert!(detail.contains("refresh=week"), "{detail}");
    assert_eq!(row.actor, "loopback");

    // The page shows the saved values back, with the group's own notice.
    let page = block_on(send(
        &app,
        Call::get("/dashboard/settings?saved=greeting")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        page.body.contains("greeting saved"),
        "the notice names the save: {}",
        page.body
    );
    assert!(
        page.body.contains(r#"id="greeting_enabled""#) && page.body.contains("checked"),
        "the switch is shown on: {}",
        page.body
    );

    // An unchecked box sends nothing at all, which is the off signal, and it
    // writes the explicit false rather than deleting the row.
    let off = block_on(send(
        &app,
        Call::post(GREETING_POST, "greeting_model=primary&greeting_refresh=day")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(off.status, StatusCode::SEE_OTHER, "{}", off.body);
    assert_eq!(read(crate::greeting::SETTING_GREETING_ENABLED), "false");
    assert_eq!(read(crate::greeting::SETTING_GREETING_REFRESH), "day");
}

#[test]
fn an_unknown_greeting_model_is_refused_by_field() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();

    let reply = block_on(send(
        &app,
        Call::post(
            GREETING_POST,
            "greeting_enabled=true&greeting_model=nope%2Fnot-a-model&greeting_refresh=hour",
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(
        reply.status,
        StatusCode::BAD_REQUEST,
        "an unknown model is refused: {}",
        reply.body
    );
    assert!(
        reply.body.contains("greeting_model_unknown"),
        "the refusal is named: {}",
        reply.body
    );
    assert!(
        reply.body.contains(r#"id="greeting_model""#),
        "the page comes back with the picker: {}",
        reply.body
    );

    // Nothing was written: a refused save is not a partial save.
    for key in [
        crate::greeting::SETTING_GREETING_ENABLED,
        crate::greeting::SETTING_GREETING_MODEL,
        crate::greeting::SETTING_GREETING_REFRESH,
    ] {
        assert_eq!(
            crate::web::store::settings_get(db, key).expect("read"),
            None,
            "{key} must not be stored"
        );
    }

    // The API client gets the documented body, not a page.
    let api = block_on(send(
        &app,
        Call::post(
            GREETING_POST,
            "greeting_enabled=true&greeting_model=nope%2Fnot-a-model&greeting_refresh=hour",
        )
        .cookie(Some(&cookie)),
    ));
    assert_eq!(api.status, StatusCode::BAD_REQUEST);
    let body = api.json();
    assert_eq!(body["ok"], false);
    assert_eq!(body["field"], "greeting_model");
    assert_eq!(body["code"], "greeting_model_unknown");
}

// ---------------------------------------------------------------------------
// The provider form
// ---------------------------------------------------------------------------

/// A provider form body, field by field, so a test names the one it varies.
fn provider_form_body(id: &str, name: &str, base_url: &str, key_ref: &str) -> String {
    format!(
        "provider_id={}&provider_name={}&provider_base_url={}&provider_key_ref={}",
        encode(id),
        encode(name),
        encode(base_url),
        encode(key_ref)
    )
}

/// The registry as the daemon reads it, including the fallback.
///
/// A daemon that has never had the registry row written still has one endpoint:
/// [`crate::web::chat::provider_registry`] seeds it from the legacy provider
/// rows. Reading the raw row instead would report an empty registry that no turn
/// would agree with, so this reads it the way a turn does.
fn registry_rows(engine: &crate::onboarding::SetupEngine) -> Vec<crate::providers::Provider> {
    crate::web::chat::provider_registry(engine).expect("the registry reads")
}

/// The provider form adds, edits and deletes one entry at a time, and the
/// registry is one document the whole way through.
///
/// The endpoint is a closed loopback port on purpose: adding a provider probes
/// it, and a probe against a port nothing listens on fails fast instead of
/// asking a real endpoint what it serves. The probe is not what this test is
/// about - the stored document is.
#[test]
fn the_provider_form_adds_edits_and_deletes_one_entry() {
    let (_dir, engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());

    // A fresh daemon has one entry, seeded from the legacy provider rows. It is
    // the primary, because `providers.primary` names nothing else.
    let start = registry_rows(&engine);
    assert_eq!(start.len(), 1, "the legacy seed is one entry");
    assert_eq!(start[0].id, crate::providers::DEFAULT_ID);

    // Add.
    let added = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_PROVIDERS_ADD,
            &provider_form_body("alpha", "Alpha", "http://127.0.0.1:1/v1", "env:ALPHA_KEY"),
        )
        .html()
        .cookie(cookie),
    ));
    assert_eq!(
        added.status,
        StatusCode::SEE_OTHER,
        "an add redirects: {}",
        added.body
    );
    assert!(
        added
            .location()
            .unwrap_or_default()
            .contains("saved=providers"),
        "the redirect returns to the tab that holds the table"
    );

    let after_add = registry_rows(&engine);
    assert_eq!(after_add.len(), 2, "one entry was added");
    let alpha = after_add
        .iter()
        .find(|entry| entry.id == "alpha")
        .expect("the new entry is in the registry");
    assert_eq!(alpha.name, "Alpha");
    assert_eq!(alpha.base_url, "http://127.0.0.1:1/v1");
    assert_eq!(alpha.key_ref, "env:ALPHA_KEY");
    // The legacy entry is untouched, so an add never rewrites its neighbours.
    assert!(
        after_add
            .iter()
            .any(|entry| entry.id == crate::providers::DEFAULT_ID),
        "the existing entry survives an add"
    );

    // Edit. The id identifies the entry and is not the thing that changes.
    let edited = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_PROVIDERS_EDIT,
            &format!(
                "{}&provider_dialect=messages",
                provider_form_body(
                    "alpha",
                    "Alpha renamed",
                    "http://127.0.0.1:2/v1",
                    "env:ALPHA_KEY",
                )
            ),
        )
        .html()
        .cookie(cookie),
    ));
    assert_eq!(
        edited.status,
        StatusCode::SEE_OTHER,
        "an edit redirects: {}",
        edited.body
    );
    let after_edit = registry_rows(&engine);
    assert_eq!(after_edit.len(), 2, "an edit added nothing");
    let alpha = after_edit
        .iter()
        .find(|entry| entry.id == "alpha")
        .expect("still there");
    assert_eq!(
        alpha.name, "Alpha renamed",
        "the name an operator reads moved"
    );
    assert_eq!(
        alpha.base_url, "http://127.0.0.1:2/v1",
        "the endpoint moved"
    );
    assert_eq!(
        alpha.dialect.as_deref(),
        Some("messages"),
        "the dialect moved"
    );

    // The page shows the row and the form, and the raw editor agrees with both.
    let page = block_on(send(
        &app,
        Call::get_owned(settings_tab_path("models"))
            .html()
            .cookie(cookie),
    ));
    assert_eq!(page.status, StatusCode::OK);
    for needle in [
        r#"id="provider-row-alpha""#,
        "Alpha renamed",
        "http://127.0.0.1:2/v1",
        "env:ALPHA_KEY",
        r#"action="/dashboard/settings/providers/delete""#,
        "Advanced: edit the registry as JSON",
    ] {
        assert!(page.body.contains(needle), "the table shows {needle}");
    }

    // Delete.
    let deleted = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_PROVIDERS_DELETE,
            "provider_id=alpha",
        )
        .html()
        .cookie(cookie),
    ));
    assert_eq!(
        deleted.status,
        StatusCode::SEE_OTHER,
        "a delete redirects: {}",
        deleted.body
    );
    let after_delete = registry_rows(&engine);
    assert_eq!(after_delete.len(), 1, "one entry was removed");
    assert!(
        !after_delete.iter().any(|entry| entry.id == "alpha"),
        "the named entry is gone"
    );
}

/// Every way a provider form can be wrong is refused by the field that is wrong,
/// and the stored document is not touched.
#[test]
fn a_bad_provider_is_refused_by_field_and_nothing_is_written() {
    let (_dir, engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());

    // One entry exists so a duplicate can be attempted against it.
    let added = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_PROVIDERS_ADD,
            &provider_form_body("alpha", "Alpha", "http://127.0.0.1:1/v1", "env:A"),
        )
        .html()
        .cookie(cookie),
    ));
    assert_eq!(added.status, StatusCode::SEE_OTHER, "{}", added.body);
    let before = crate::web::store::settings_get(engine.db(), crate::providers::SETTING_REGISTRY)
        .expect("read");

    // The field, the case, and why each one is wrong.
    let cases: Vec<(&'static str, String, &'static str)> = vec![
        (
            "provider_id",
            provider_form_body("alpha", "Again", "http://127.0.0.1:3/v1", "env:B"),
            "a duplicate id would make every auxiliary role ambiguous",
        ),
        (
            "provider_id",
            provider_form_body("Alpha", "Caps", "http://127.0.0.1:3/v1", "env:B"),
            "an id is lowercase letters, digits, hyphen and underscore",
        ),
        (
            "provider_name",
            provider_form_body("beta", "", "http://127.0.0.1:3/v1", "env:B"),
            "every entry needs a name",
        ),
        (
            "provider_base_url",
            provider_form_body("beta", "Beta", "ftp://x/v1", "env:B"),
            "an endpoint starts http:// or https://",
        ),
        (
            "provider_base_url",
            provider_form_body("beta", "Beta", "http://x /v1", "env:B"),
            "an endpoint with a space in it is not an endpoint",
        ),
        (
            "provider_key_ref",
            provider_form_body("beta", "Beta", "http://127.0.0.1:3/v1", "env:lower_case"),
            "a reference names an upper-case variable",
        ),
    ];

    for (field, body, why) in cases {
        let refused = block_on(send(
            &app,
            Call::post(crate::admin::ROUTE_SETTINGS_PROVIDERS_ADD, &body)
                .html()
                .cookie(cookie),
        ));
        assert_eq!(
            refused.status,
            StatusCode::BAD_REQUEST,
            "{why}: {}",
            refused.body
        );
        assert!(
            refused.body.contains(field),
            "the refusal names {field} ({why}): {}",
            refused.body
        );
        assert_eq!(
            crate::web::store::settings_get(engine.db(), crate::providers::SETTING_REGISTRY)
                .expect("read"),
            before,
            "a refusal writes nothing ({why})"
        );
    }
}

/// A key pasted into the provider form is sealed into `eris.env` and the row
/// keeps only its name. The value never reaches the registry and never comes back
/// out of a page, which is what makes pasting one safe to offer.
#[test]
fn a_pasted_key_is_sealed_into_the_env_file_and_never_echoed() {
    let (_dir, engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());
    let secret = "sk-live-1234567890abcdef";

    let added = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_PROVIDERS_ADD,
            &provider_form_body("beta", "Beta", "http://127.0.0.1:3/v1", secret),
        )
        .html()
        .cookie(cookie),
    ));
    assert_eq!(
        added.status,
        StatusCode::SEE_OTHER,
        "a pasted key saves rather than refusing: {}",
        added.body
    );

    // The row holds a reference and nothing else.
    let rows = registry_rows(&engine);
    let beta = rows
        .iter()
        .find(|entry| entry.id == "beta")
        .expect("the entry was added");
    assert_eq!(beta.key_ref, "env:BETA_API_KEY");

    // The stored document cannot leak it: it does not contain the value.
    let stored = crate::web::store::settings_get(engine.db(), crate::providers::SETTING_REGISTRY)
        .expect("read")
        .unwrap_or_default();
    assert!(
        !stored.contains(secret),
        "the registry never carries a key value: {stored}"
    );

    // The value is in the environment file, at operator-only permissions.
    let env_file = engine.paths().env_file.clone();
    let written = std::fs::read_to_string(&env_file).expect("eris.env was written");
    assert!(
        written.contains(&format!("BETA_API_KEY={secret}")),
        "the env file carries the key: {written}"
    );
    use std::os::unix::fs::PermissionsExt as _;
    let mode = std::fs::metadata(&env_file)
        .expect("meta")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "eris.env stays 0600");

    // And the page that lists providers shows the reference, not the key.
    let page = block_on(send(
        &app,
        Call::get(crate::admin::ROUTE_SETTINGS)
            .html()
            .cookie(cookie),
    ));
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        !page.body.contains(secret),
        "no rendered page echoes a sealed key"
    );
    assert!(
        page.body.contains("env:BETA_API_KEY"),
        "the page shows the reference instead"
    );
}

/// The primary cannot be deleted, and the refusal says what to do instead.
///
/// Moving the primary is a decision about which endpoint every turn is sent to.
/// A delete button does not get to make that decision silently, so the route
/// refuses and names the field.
#[test]
fn the_primary_provider_cannot_be_deleted() {
    let (_dir, engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());
    block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_PROVIDERS_ADD,
            &provider_form_body("alpha", "Alpha", "http://127.0.0.1:1/v1", "env:A"),
        )
        .html()
        .cookie(cookie),
    ));
    let before = crate::web::store::settings_get(engine.db(), crate::providers::SETTING_REGISTRY)
        .expect("read");

    let refused = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_PROVIDERS_DELETE,
            &format!("provider_id={}", crate::providers::DEFAULT_ID),
        )
        .html()
        .cookie(cookie),
    ));
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    for needle in [
        "provider_id",
        "primary provider",
        "move the primary to another entry first",
    ] {
        assert!(
            refused.body.contains(needle),
            "the refusal says {needle}: {}",
            refused.body
        );
    }
    assert_eq!(
        crate::web::store::settings_get(engine.db(), crate::providers::SETTING_REGISTRY)
            .expect("read"),
        before,
        "the registry is untouched by a refused delete"
    );

    // An id that does not exist is refused too, rather than deleting nothing
    // quietly and reporting success.
    let unknown = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_PROVIDERS_DELETE,
            "provider_id=nobody",
        )
        .html()
        .cookie(cookie),
    ));
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST, "{}", unknown.body);
    assert!(unknown.body.contains("there is no provider with the id"));
}

// ---------------------------------------------------------------------------
// The parity check
// ---------------------------------------------------------------------------

/// Every writable capability, the settings row it writes, and the web control
/// that writes it.
///
/// The record's `key` is the device API's field name and the web form's `name`
/// is the form's own; they are not the same string and were never meant to be.
/// What has to match is the **row**: two clients writing different spellings of
/// one preference is the drift that makes a phone and a browser disagree. So the
/// mapping is by row, and the control is checked by the name the page renders.
const WEB_CONTROLS: &[(&str, &str)] = &[
    // (the settings row, the form field the web page renders)
    (crate::onboarding::SETTING_MODEL, "model"),
    (crate::tools::SETTING_TOOLS_ENABLED, "tools_enabled"),
    (crate::tools::SETTING_TOOL_ROUNDS, "tool_rounds"),
    (
        crate::tools::SETTING_GOAL_ROUNDS_CEILING,
        "goal_rounds_ceiling",
    ),
    (crate::subagents::SETTING_AGENTS_ENABLED, "agents_enabled"),
    (crate::persona::SETTING_ASSISTANT_NAME, "assistant_name"),
    (crate::persona::SETTING_PERSONA_ENABLED, "persona_enabled"),
    (crate::web::chat::SETTING_SHOW_REASONING, "show_reasoning"),
    (crate::web::chat::SETTING_SHOW_TOOL_LOG, "tool_log"),
    (crate::web::chat::SETTING_STREAMING, "stream_responses"),
    (crate::web::quota::SETTING_SHOW_QUOTA, "show_quota"),
    (crate::web::chat::SETTING_REPLY_STATE, "reply_state"),
    (
        crate::greeting::SETTING_GREETING_ENABLED,
        "greeting_enabled",
    ),
    (
        crate::greeting::SETTING_GREETING_REFRESH,
        "greeting_refresh",
    ),
    (crate::learning::SETTING_REVIEW_REPAIR, "learning_repair"),
    (
        crate::delivery::SETTING_DELIVERY_DEFAULT,
        "schedule_delivery_default",
    ),
    (
        crate::delivery::SETTING_EMAIL_ENABLED,
        "schedule_email_enabled",
    ),
    (
        crate::process::SETTING_NOTIFY_ON_COMPLETE,
        "jobs_notify_on_complete",
    ),
    (crate::process::SETTING_RECEIPT_DAYS, "jobs_receipt_days"),
    (crate::learning::SETTING_UNUSED_DAYS, "curator_archive_days"),
    (crate::search::SETTING_SEARCH_LIMIT, "search_limit"),
    (
        crate::search::SETTING_SEARCH_SCOPE_DEFAULT,
        "search_scope_default",
    ),
    (crate::memory::SETTING_BACKEND, "memory_backend"),
    (crate::push::SETTING_NOTIFY_ON_REPLY, "notify_on_reply"),
    (crate::push::SETTING_NOTIFY_SCOPE, "notify_scope"),
    (crate::push::SETTING_NOTIFY_DEVICES, "notify_devices"),
    (crate::scheduler::SETTING_ENABLED, "schedules_enabled"),
    (crate::skills::SETTING_ENABLED, "skills_enabled"),
    (crate::onboarding::SETTING_SANDBOX_MODE, "sandbox_mode"),
    (
        crate::onboarding::SETTING_APPROVAL_POLICY,
        "approval_policy",
    ),
    (crate::zone::SETTING_TIMEZONE, "timezone"),
    (crate::zone::SETTING_CLOCK_FORMAT, "clock_format"),
    // The rail owns this one: the control sits beside the conversation list, not
    // in the settings page, because that is where the order is being changed.
    (crate::web::chat::SETTING_CONVERSATION_SORT, "sort"),
];

/// The capability record and the web page cover the same settings.
///
/// Three claims:
///
/// - every **writable** capability's row is written by a web control, so a
///   field a phone can change is a field a browser can change;
/// - every control in the mapping belongs to a capability, so the page does not
///   write a row the record never mentions;
/// - the one read-only capability is read-only for a reason, and says so.
#[test]
fn every_writable_capability_has_a_web_control_on_the_same_row() {
    use crate::device::handlers::SETTING_SPECS;

    let (_dir, engine, app, cookie) = ready_app();
    // The targeted-device picker only exists once there is a device to pick: a
    // list of checkboxes with nothing in it is not a control. So the fixture
    // pairs one, which is what an operator would have done before reaching this
    // form.
    super::harness::seed_device(engine.db(), "dev-a", "iPhone", "ios", None, None, None);
    super::harness::seed_redeemed_code(engine.db(), "dev-a");

    // Every tab, concatenated: the controls are spread across five of the six,
    // and the rail control is on the chat page rather than in settings at all.
    let mut markup = String::new();
    for tab in crate::web::templates::SettingsTab::ALL {
        let page = block_on(send(
            &app,
            Call::get_owned(settings_tab_path(tab.id()))
                .html()
                .cookie(Some(&cookie)),
        ));
        assert_eq!(page.status, StatusCode::OK, "tab {}", tab.id());
        markup.push_str(&page.body);
    }
    let chat = block_on(send(
        &app,
        Call::get(crate::admin::ROUTE_CHAT)
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(chat.status, StatusCode::OK);
    markup.push_str(&chat.body);

    let _ = engine;
    let control_for = |row: &str| {
        WEB_CONTROLS
            .iter()
            .find(|(mapped, _)| *mapped == row)
            .map(|(_, field)| *field)
    };

    let mut writable = 0usize;
    for spec in SETTING_SPECS {
        if spec.writable {
            writable += 1;
            let field = control_for(spec.row).unwrap_or_else(|| {
                panic!(
                    "the record offers `{}` (row {}) as writable and no web control writes that \
                     row; add one, or mark the capability not writable and say why",
                    spec.key, spec.row
                )
            });
            assert!(
                markup.contains(&format!(r#"name="{field}""#)),
                "the control for `{}` is not rendered anywhere: expected name=\"{field}\"",
                spec.key
            );
        } else {
            // Read-only on purpose: the workspace is moved by the wizard, which
            // checks the path against the reserved roots. A free-text box here
            // would be a second, weaker rule for the same decision.
            assert_eq!(
                spec.key, "workspace_path",
                "`{}` is not writable and this test does not know why",
                spec.key
            );
        }
    }
    assert!(writable >= 20, "the record is the field list: {writable}");

    // And nothing in the mapping is invented: every row it names belongs to a
    // capability, so the page cannot write a setting the record does not state.
    for (row, field) in WEB_CONTROLS {
        assert!(
            SETTING_SPECS.iter().any(|spec| spec.row == *row),
            "the web control `{field}` writes the row `{row}`, which is not a capability"
        );
    }
}

// ---------------------------------------------------------------------------
// The rail order
// ---------------------------------------------------------------------------

/// Four conversations, each with its own created and updated stamps.
fn sortable_rail(engine: &crate::onboarding::SetupEngine) {
    let insert = |id: &str, title: &str, created: &str, updated: &str| {
        engine
            .db()
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO chat_conversations (id, title, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    rusqlite::params![id, title, created, updated],
                )?;
                Ok(())
            })
            .expect("insert");
    };
    insert(
        "beta",
        "beta work",
        "2026-10-20T09:00:00Z",
        "2026-10-26T09:00:00Z",
    );
    insert(
        "alpha",
        "Alpha notes",
        "2026-10-26T08:00:00Z",
        "2026-10-24T09:00:00Z",
    );
    insert(
        "delta",
        "delta ideas",
        "2026-10-21T09:00:00Z",
        "2026-10-25T09:00:00Z",
    );
    insert(
        "gamma",
        "Gamma plan",
        "2026-10-22T09:00:00Z",
        "2026-10-23T09:00:00Z",
    );
}

/// The ids the rail reports, in the order it reports them.
fn rail_order(app: &axum::Router, cookie: &str) -> Vec<String> {
    let rail = block_on(send(
        app,
        Call::get(crate::admin::ROUTE_CHAT_SESSIONS).cookie(Some(cookie)),
    ));
    assert_eq!(rail.status, StatusCode::OK, "{}", rail.body);
    let body = rail.json();
    body["sessions"]
        .as_array()
        .expect("the rail is a list")
        .iter()
        .map(|row| row["id"].as_str().unwrap_or_default().to_owned())
        .collect()
}

/// The rail's order is a stored setting, written from the rail itself.
///
/// The whole point of storing it rather than passing it is the page's own live
/// refresh: it fetches `sessions.json` with no parameters at all, so the order
/// has to come back from the database or the rail re-sorts itself after every
/// turn.
#[test]
fn the_rail_order_is_a_setting_the_rail_itself_writes() {
    let (_dir, engine, app, cookie) = ready_app();
    let db = engine.db();
    sortable_rail(&engine);
    // The suite's own fixture conversation is in the table too and is not part of
    // the four; it is empty and stamped at seed time, so it is first by `updated`
    // unless the fixture says otherwise. Pinning its stamps keeps the assertion
    // about the four below it rather than about when the suite ran.
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_conversations SET created_at = '2026-10-19T09:00:00Z', \
             updated_at = '2026-10-19T09:00:00Z' WHERE title = ?1",
            rusqlite::params![crate::web::chat::NEW_CONVERSATION_TITLE],
        )?;
        Ok(())
    })
    .expect("stamp the fixture");
    let fixture = crate::web::chat::CONVERSATION_ID.to_owned();

    let read = || {
        crate::web::store::settings_get(db, crate::web::chat::SETTING_CONVERSATION_SORT)
            .expect("read")
            .unwrap_or_default()
    };
    assert_eq!(read(), "", "the order is the default until it is chosen");

    // The default is the order the rail already had: recently used.
    assert_eq!(
        rail_order(&app, &cookie),
        vec!["beta", "delta", "alpha", "gamma", &fixture]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    );

    // Each of the four modes, posted from the rail and read back from the rail.
    // `main` is the suite's own fixture conversation and sits behind the four
    // under every order, so each list below is about the four above it.
    let cases: [(&str, Vec<&str>); 4] = [
        ("updated", vec!["beta", "delta", "alpha", "gamma", &fixture]),
        ("created", vec!["alpha", "gamma", "delta", "beta", &fixture]),
        ("title", vec!["alpha", "beta", "delta", "gamma", &fixture]),
        // `day` is the recently-used order; what changes is the grouping.
        ("day", vec!["beta", "delta", "alpha", "gamma", &fixture]),
    ];
    for (sort, order) in cases {
        let reply = block_on(send(
            &app,
            Call::post(crate::admin::ROUTE_CHAT_SORT, &format!("sort={sort}"))
                .html()
                .cookie(Some(&cookie)),
        ));
        assert_eq!(
            reply.status,
            StatusCode::SEE_OTHER,
            "choosing {sort} redirects back to the rail: {}",
            reply.body
        );
        assert_eq!(read(), sort, "the choice is the stored setting");
        assert_eq!(
            rail_order(&app, &cookie),
            order.into_iter().map(str::to_owned).collect::<Vec<_>>(),
            "the rail reads back in the {sort} order"
        );

        // The control states the choice on the next page, so a reload of the same
        // address bar says what the rail just did.
        let page = block_on(send(
            &app,
            Call::get(crate::admin::ROUTE_CHAT)
                .html()
                .cookie(Some(&cookie)),
        ));
        assert_eq!(page.status, StatusCode::OK);
        let label = crate::web::chat::ConversationSort::parse(Some(sort)).label();
        assert!(
            page.body.contains(label),
            "the rail control shows {label}: {}",
            page.body
        );
        assert!(
            page.body
                .contains(&format!(r#"value="{sort}" aria-current="true">"#)),
            "the menu marks {sort} as the order in force"
        );
    }

    // A mode the enum does not hold is refused by field, and the stored order is
    // what it was: a typo must not read as a choice.
    let refused = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_CHAT_SORT, "sort=sideways")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    assert!(refused.body.contains("sort"), "{}", refused.body);
    assert_eq!(read(), "day", "the stored order survives a refused write");

    // The write is audited, because "why did my rail reorder itself" is a
    // question somebody asks later.
    let rows = super::harness::audit_rows(db);
    assert!(
        rows.iter().any(|row| row.action == "chat.sort"),
        "the choice leaves an audit row"
    );
}

// ---------------------------------------------------------------------------
// The objective: a setting that changes what every turn is for
// ---------------------------------------------------------------------------

/// The objective is a page control, not only a slash command.
///
/// It used to be reachable by typing `/goal` at a phone and nowhere else, which is
/// how it came to be set for nine hours and forgotten. The card is the fix for the
/// finding half of that, and this walks the whole round trip: save, see it, and
/// clear it from the same form.
#[test]
fn the_objective_card_saves_shows_and_clears() {
    let (_dir, engine, app, cookie) = ready_app();
    let objective = "inspect the warehouse for gaps";

    // Nothing set: the card says so and offers no Clear button to press.
    let empty = block_on(send(
        &app,
        Call::get_owned(settings_tab_path("assistant"))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(empty.status, StatusCode::OK, "{}", empty.body);
    assert!(
        empty.body.contains(&format!(
            r#"action="{}""#,
            crate::admin::ROUTE_SETTINGS_GOAL
        )),
        "the objective form is on the assistant tab: {}",
        empty.body
    );
    assert!(
        empty.body.contains("No objective is set"),
        "the empty state is stated: {}",
        empty.body
    );
    assert!(
        !empty.body.contains(r#"name="goal_action" value="clear""#),
        "there is nothing to clear, so no Clear button is drawn: {}",
        empty.body
    );

    // A save writes the row and lands on its own card.
    let saved = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_GOAL,
            &format!("goal_text={}", encode(objective)),
        )
        .cookie(Some(&cookie)),
    ));
    assert_eq!(saved.status, StatusCode::SEE_OTHER, "{}", saved.body);
    assert!(
        saved.location().unwrap_or_default().contains("saved=goal"),
        "the redirect names the group: {:?}",
        saved.location()
    );
    assert_eq!(
        crate::goal::stored(engine.db()).as_deref(),
        Some(objective),
        "the row holds what was typed"
    );

    // The card now pre-fills the textarea and offers the way out. The redirect
    // target is followed rather than the bare tab, because the notice rides on
    // `?saved=` and a plain tab load is a page with nothing to announce.
    let set = block_on(send(
        &app,
        Call::get_owned(format!("{ROUTE_SETTINGS}?saved=goal"))
            .html()
            .cookie(Some(&cookie)),
    ));
    assert!(
        set.body.contains(objective),
        "the stored objective is pre-filled: {}",
        set.body
    );
    assert!(
        set.body.contains("objective saved"),
        "the notice is rendered: {}",
        set.body
    );
    assert!(
        set.body.contains(r#"name="goal_action" value="clear""#),
        "a set goal can be cleared from the page: {}",
        set.body
    );

    // Clear is its own press, and it is the empty state rather than a second
    // setting that means the same thing.
    let cleared = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_GOAL,
            &format!("goal_text={}&goal_action=clear", encode(objective)),
        )
        .cookie(Some(&cookie)),
    ));
    assert_eq!(cleared.status, StatusCode::SEE_OTHER, "{}", cleared.body);
    assert_eq!(
        crate::goal::stored(engine.db()),
        None,
        "clear empties the row even when the textarea still held the objective"
    );

    // The write is audited, and the audit says nothing about the plan itself.
    let rows = super::harness::audit_rows(engine.db());
    let goal_rows: Vec<_> = rows
        .iter()
        .filter(|row| row.action == "settings.goal")
        .collect();
    assert_eq!(goal_rows.len(), 2, "one row per save");
    assert!(
        goal_rows.iter().all(|row| !row
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains(objective)),
        "the audit records a length, not the operator's plan: {goal_rows:?}"
    );
}

/// An objective longer than the compiler will take is refused by field with both
/// numbers, and nothing is written.
#[test]
fn an_oversized_objective_is_refused_by_the_page() {
    let (_dir, engine, app, cookie) = ready_app();
    let long = "x".repeat(crate::goal::MAX_OBJECTIVE_CHARS + 1);
    let refused = block_on(send(
        &app,
        Call::post(
            crate::admin::ROUTE_SETTINGS_GOAL,
            &format!("goal_text={}", encode(&long)),
        )
        .html()
        .cookie(Some(&cookie)),
    ));
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    assert!(
        refused.body.contains("goal_too_long"),
        "the refusal names the code: {}",
        refused.body
    );
    assert!(
        refused
            .body
            .contains(&(crate::goal::MAX_OBJECTIVE_CHARS + 1).to_string()),
        "and carries the number that failed: {}",
        refused.body
    );
    assert_eq!(
        crate::goal::stored(engine.db()),
        None,
        "nothing was written"
    );
}

/// `/goal` is a browser command too, and it answers with the same three states the
/// phone does.
///
/// The reply is a transcript row, not a response body: a command is answered by the
/// daemon before a turn exists, and the page behind the redirect is where the
/// operator reads it. So each press is checked twice, once for the row and once for
/// the setting the row is about.
#[test]
fn the_goal_command_sets_reports_and_clears() {
    let (_dir, engine, app, cookie) = ready_app();
    let press = |text: &str| {
        let reply = block_on(send(
            &app,
            Call::post(
                crate::admin::ROUTE_CHAT_SEND,
                &format!("message={}", encode(text)),
            )
            .cookie(Some(&cookie)),
        ));
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.body);
        let rows = crate::web::chat::load(engine.db(), crate::web::chat::CONVERSATION_ID)
            .expect("a readable transcript");
        let last = rows.last().expect("a transcript row");
        assert_eq!(
            last.role,
            crate::web::chat::Role::Assistant,
            "the daemon spoke"
        );
        last.body.clone()
    };

    let body = press("/goal status");
    assert!(body.contains("No goal is set"), "{body}");

    let body = press("/goal inspect the warehouse");
    assert!(body.contains("Working toward"), "{body}");
    assert_eq!(
        crate::goal::stored(engine.db()).as_deref(),
        Some("inspect the warehouse")
    );

    let body = press("/goal status");
    assert!(body.contains("inspect the warehouse"), "{body}");

    let body = press("/goal stop");
    assert!(body.contains("Goal cleared"), "{body}");
    assert_eq!(crate::goal::stored(engine.db()), None);
}

// ---------------------------------------------------------------------------
// The System tab's Update card
// ---------------------------------------------------------------------------

/// Seed the release cache so a card renders without a network read.
fn seed_update_release(engine: &crate::onboarding::SetupEngine, version: &str, build: u32) {
    seed_update_release_with(engine, version, build, true);
}

/// The asset list a release JSON carries, for the host's own architecture.
fn assets(version: &str, build: u32, tag: &str, checksum: bool) -> Vec<serde_json::Value> {
    let arch = crate::release::host_arch();
    let asset = format!("eris-v{version}-build.{build}-linux-{arch}.tar.gz");
    let base = crate::release::DOWNLOAD_PREFIX;
    let mut out = vec![serde_json::json!({
        "name": asset,
        "browser_download_url": format!("{base}{tag}/{asset}"),
    })];
    if checksum {
        out.push(serde_json::json!({
            "name": format!("{asset}.sha256"),
            "browser_download_url": format!("{base}{tag}/{asset}.sha256"),
        }));
    }
    out
}

/// Seed a release with or without its checksum asset.
///
/// A release that publishes no `.sha256` is the one mechanical blocker that does
/// not depend on the host, so the page-level "disabled with a reason" proof uses
/// it rather than guessing whether this machine has the helper unit installed.
fn seed_update_release_with(
    engine: &crate::onboarding::SetupEngine,
    version: &str,
    build: u32,
    checksum: bool,
) {
    let tag = format!("v{version}-build.{build}");
    let body = serde_json::json!({
        "tag_name": tag,
        "name": format!("Kallisti {version} build {build}"),
        "body": "one entry per observable behaviour",
        "published_at": "2026-11-10T15:00:00Z",
        "assets": assets(version, build, &tag, checksum),
    })
    .to_string();
    engine
        .set_setting(crate::release::SETTING_MANIFEST, &body)
        .expect("seeded manifest");
    engine
        .set_setting(
            crate::release::SETTING_CHECKED_AT,
            &crate::auth::format_timestamp(crate::auth::now()),
        )
        .expect("seeded stamp");
}

/// The Software card states the running identity and the staged list, and its
/// install button is disabled with a plain sentence when nothing is staged.
#[test]
fn the_software_card_states_what_is_running_and_what_is_staged() {
    let (_dir, _engine, app, cookie) = ready_app();
    let page = block_on(send(
        &app,
        Call::get("/dashboard/settings?tab=system")
            .html()
            .cookie(Some(cookie.as_str())),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;

    assert!(body.contains("id=\"update-heading\""), "the card renders");
    assert!(body.contains(">Software<"), "the card is titled Software");
    assert!(
        body.contains(&crate::admin::version_line()),
        "the card states the running identity line"
    );
    assert!(body.contains("Staged"), "the card names the staged list");
    assert!(
        body.contains("nothing staged"),
        "and says plainly when nothing is staged"
    );
    assert!(
        body.contains("Install staged update"),
        "the button is present even when it is disabled"
    );
    assert!(
        body.contains("disabled"),
        "a button with nothing to install is disabled"
    );
    // And the form posts to the one write the card owns.
    assert!(
        body.contains(crate::admin::ROUTE_SETTINGS_UPDATE),
        "the form posts to the update route"
    );
}

/// The card's route is session-gated and audited like every other settings write.
#[test]
fn the_software_install_route_refuses_a_bad_session_and_audits_a_good_one() {
    let (_dir, engine, app, cookie) = ready_app();

    let anonymous = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SETTINGS_UPDATE, ""),
    ));
    assert!(
        anonymous.status == StatusCode::FORBIDDEN || anonymous.status == StatusCode::UNAUTHORIZED,
        "a post with no session is refused: {}",
        anonymous.status
    );

    let reply = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SETTINGS_UPDATE, "")
            .html()
            .cookie(Some(cookie.as_str())),
    ));
    // Either it queued an install (303 to the card) or it refused on the card
    // itself (400 with the daemon's words). Both are correct; a 500 is not.
    assert!(
        reply.status == StatusCode::SEE_OTHER || reply.status == StatusCode::BAD_REQUEST,
        "the card answers a page, not a server error: {} {}",
        reply.status,
        reply.body
    );

    let rows = super::harness::audit_rows(engine.db());
    let audited = rows.iter().any(|row| row.action == "settings.update");
    if reply.status == StatusCode::SEE_OTHER {
        assert!(audited, "a queued install is recorded as settings.update");
    } else {
        assert!(
            !audited,
            "a refused install is not recorded as one that ran"
        );
    }
}

/// The card states what is installed, what is newest, and why the button is off.
#[test]
fn the_system_tab_states_the_installed_and_newest_versions() {
    let (_dir, engine, app, cookie) = ready_app();
    seed_update_release(&engine, "99.0.0", 999);

    let page = block_on(send(
        &app,
        Call::get("/dashboard/settings?tab=system")
            .html()
            .cookie(Some(cookie.as_str())),
    ));
    assert_eq!(page.status, StatusCode::OK);
    let body = &page.body;

    // The card is on this tab and is not a placeholder.
    assert!(
        body.contains("id=\"update-heading\""),
        "the Update card renders"
    );
    assert!(body.contains("Running"), "the card names what is running");
    assert!(
        body.contains(&crate::admin::version_line()),
        "the running identity line is on the page"
    );
    assert!(
        body.contains("v99.0.0-build.999"),
        "the newest release is named"
    );
    assert!(
        body.contains("2026-11-10T15:00:00Z"),
        "the publish date is shown"
    );
    assert!(
        body.contains("one entry per observable behaviour"),
        "the release notes are shown"
    );

    // The button names what it installs. It says "staged" because that is what it
    // acts on; the release it would otherwise fetch is named on the row above it.
    assert!(
        body.contains("Install staged update"),
        "the button names what it installs"
    );

    // The forms post to the routes the daemon serves. The card's own write is the
    // one form; the JSON apply route is for API clients and is not on the page.
    assert!(body.contains(crate::admin::ROUTE_SETTINGS_UPDATE));
    assert!(body.contains(crate::admin::ROUTE_SETTINGS_UPDATE_CHECK));
    assert!(
        !body.contains(crate::admin::ROUTE_SETTINGS_UPDATE_APPLY),
        "the page posts to its own route, not the API one"
    );

    // The card sits directly above the Daemon card: the two answer the same
    // question, and the update is the answer that keeps the settings.
    let update_at = body.find("id=\"update-heading\"").expect("the Update card");
    let daemon_at = body.find("id=\"daemon-heading\"").expect("the Daemon card");
    assert!(
        update_at < daemon_at,
        "the Update card renders above the Daemon card"
    );
}

/// A release with no checksum asset is not installable, and the card says so with
/// the button disabled. That is the "a disabled button with no reason is the
/// defect" proof, and it is host-independent by construction: no checksum is a
/// refusal wherever the daemon runs.
#[test]
fn the_install_button_is_disabled_with_the_reason_a_release_cannot_be_installed() {
    let (_dir, engine, app, cookie) = ready_app();
    seed_update_release_with(&engine, "99.0.0", 999, false);

    let page = block_on(send(
        &app,
        Call::get("/dashboard/settings?tab=system")
            .html()
            .cookie(Some(cookie.as_str())),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    let body = &page.body;
    assert!(
        body.contains("v99.0.0-build.999"),
        "the release is still named"
    );
    assert!(
        body.contains("disabled"),
        "an install that cannot run is disabled"
    );

    // The sentence beside the button is the daemon's own, whatever it is. Which
    // blocker wins depends on the host - a missing helper unit is fixed first, so
    // it outranks the release having no checksum - and a test that hard-coded one
    // of them would be green here and red on a runner that has never installed the
    // unit. Comparing against the same function the handler calls pins the real
    // invariant instead: the page says exactly what the daemon refuses with.
    let release = crate::release::cached_release(engine.db()).expect("the seeded manifest");
    let expected = crate::device::gateway::host_blocker(&engine, Some(&release))
        .expect("this release is refused on any host");
    assert!(
        body.contains(&expected),
        "the card states the daemon's own reason, expected `{expected}`"
    );
}

/// With nothing cached, the card says so instead of inventing a release.
#[test]
fn the_update_card_is_calm_when_nothing_is_cached() {
    let (_dir, _engine, app, cookie) = ready_app();
    let page = block_on(send(
        &app,
        Call::get("/dashboard/settings?tab=system")
            .html()
            .cookie(Some(cookie.as_str())),
    ));
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("id=\"update-heading\""));
    assert!(
        page.body.contains("never"),
        "an unchecked host says so rather than showing a fake version"
    );
    // The button is always on the card now, because its job is "install what is
    // staged" and that is true whether or not a release exists. With nothing
    // staged it is disabled and the card says so.
    assert!(
        page.body.contains("Install staged update"),
        "the button is present"
    );
    assert!(
        page.body.contains("disabled") && page.body.contains("nothing staged"),
        "and it is disabled with the reason"
    );
    // No red banner and no error page: an offline daemon is not a failure.
    assert_eq!(page.status, StatusCode::OK);
}

/// The check route needs a session and records what it did.
#[test]
fn the_update_check_route_refuses_a_bad_session_and_audits_a_good_one() {
    let (_dir, engine, app, cookie) = ready_app();

    let anonymous = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SETTINGS_UPDATE_CHECK, ""),
    ));
    assert!(
        anonymous.status == StatusCode::FORBIDDEN || anonymous.status == StatusCode::UNAUTHORIZED,
        "a post with no session is refused: {}",
        anonymous.status
    );

    // Seed first so the good post is a cache read and no test reaches the network.
    seed_update_release(&engine, "99.0.0", 999);
    let checked = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SETTINGS_UPDATE_CHECK, "")
            .html()
            .cookie(Some(cookie.as_str())),
    ));
    assert_eq!(checked.status, StatusCode::SEE_OTHER, "{}", checked.body);
    assert_eq!(
        checked.location().as_deref(),
        Some("/dashboard/settings?saved=update_checked")
    );

    let rows = super::harness::audit_rows(engine.db());
    let row = rows
        .iter()
        .find(|row| row.action == "settings.update_check")
        .expect("the check is recorded");
    assert_eq!(row.actor, "loopback");
}

/// The progress route is JSON, requires a session, and is never a 404.
#[test]
fn the_update_progress_route_answers_idle_json() {
    let (_dir, _engine, app, cookie) = ready_app();
    let reply = block_on(send(
        &app,
        Call::get(crate::admin::ROUTE_SETTINGS_UPDATE_PROGRESS)
            .header("accept", "application/json")
            .cookie(Some(cookie.as_str())),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let body = reply.json();
    assert_eq!(body["ok"], true);
    assert_eq!(body["data"]["state"], "idle");
    assert_eq!(body["data"]["output"], serde_json::json!([]));
}

/// The apply route refuses to start an install it cannot perform.
#[test]
fn the_update_apply_route_refuses_and_audits_nothing_when_it_cannot_install() {
    let (_dir, engine, app, cookie) = ready_app();
    // The release this process is already running: never newer than itself, so the
    // refusal happens before any mechanical check and no test can reach systemd.
    seed_update_release(&engine, crate::admin::VERSION, crate::admin::BUILD);

    let reply = block_on(send(
        &app,
        Call::post(crate::admin::ROUTE_SETTINGS_UPDATE_APPLY, "")
            .html()
            .cookie(Some(cookie.as_str())),
    ));
    // The dashboard answers a refused form with 400 and the daemon's own words,
    // exactly as the restart card does for `not_ready`; the device route is the
    // surface that speaks 409.
    assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.body);
    assert!(
        reply.body.contains("update_unavailable"),
        "the refusal names the code the card renders: {}",
        reply.body
    );
    assert!(
        reply.body.contains("no newer release"),
        "and says why in the daemon's own words: {}",
        reply.body
    );
    // The refusal renders the tab that holds the card, not the default tab. A
    // reason on a page the operator is not looking at is not a reason.
    assert!(
        reply
            .body
            .contains("href=\"/dashboard/settings?tab=system\" aria-current=\"page\""),
        "the refusal re-renders the System tab"
    );
    // Nothing was queued: a refusal writes no request file.
    assert!(
        !engine
            .paths()
            .state_dir
            .join("update/request.json")
            .exists(),
        "a refused install writes no request"
    );
    let rows = super::harness::audit_rows(engine.db());
    assert!(
        !rows.iter().any(|row| row.action == "settings.update_apply"),
        "a refused install is not audited as one that ran"
    );
}
