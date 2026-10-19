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
const WORKSPACE_POST: &str = "/dashboard/settings/workspace";
const PERMISSIONS_POST: &str = "/dashboard/settings/permissions";

/// A provider form body, with the reference spelled the way the rule requires.
fn provider_body(base_url: &str, key_ref: &str, model: &str) -> String {
    format!(
        "base_url={}&api_key_ref={}&model={}",
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
    assert_eq!(
        page.matches(r#"aria-live="polite""#).count(),
        4,
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
    assert!(lowered.contains("loopback only"), "{page}");
    assert!(
        page.contains("127.0.0.1:7340") && page.contains("127.0.0.1:7341"),
        "both listener literals are stated: {page}"
    );
    assert!(
        lowered.contains("not configurable"),
        "the admin bind says it is not configurable: {page}"
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
    assert!(!lowered.contains("sk-"), "no key value is rendered");
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
