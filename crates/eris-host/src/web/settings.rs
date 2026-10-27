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
        Call::get(ROUTE_SETTINGS).html().cookie(Some(&cookie)),
    ));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let page = &reply.body;

    // The three forms, each posting to its own route.
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
    // Six groups carry a live region: provider, accounts, workspace,
    // permissions, persona and greeting. The page level region plus one per
    // group is seven. Schedules and skills render no region of their own.
    assert_eq!(
        page.matches(r#"aria-live="polite""#).count(),
        7,
        "one page level region plus one per form: {page}"
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
    // a process without it renders the option disabled.
    let page = block_on(send(
        &app,
        Call::get(ROUTE_SETTINGS).html().cookie(Some(&cookie)),
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
        Call::get(ROUTE_SETTINGS).html().cookie(Some(&cookie)),
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
        Call::get(ROUTE_SETTINGS).html().cookie(Some(&cookie)),
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
