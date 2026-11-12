//! The `budget` tool, driven through a turn's own dispatch path.
//!
//! Two claims. The tool is offered in every sandbox mode, because reading a number
//! changes nothing. And the number it answers is the number the database holds: a
//! turn that acts on a budget has to be acting on the same figure an operator can
//! check with `sqlite3`, or the tool is a plausible lie.

use eris_host::budget;
use eris_host::db::Db;
use eris_host::tools::{SandboxMode, specs_offering};

/// The tool's name is in the offered list in every mode.
#[test]
fn the_tool_is_offered_in_every_mode() {
    for mode in [SandboxMode::Full, SandboxMode::ReadOnly] {
        let offered = specs_offering(mode, true);
        assert!(
            offered.iter().any(|spec| spec.name == budget::TOOL),
            "budget must reach the model in {mode:?}"
        );
    }
    // And it stays offered with agent dispatch switched off: it is not a dispatch.
    let without_agents = specs_offering(SandboxMode::Full, false);
    assert!(
        without_agents.iter().any(|spec| spec.name == budget::TOOL),
        "the budget tool is not gated on dispatching"
    );
}

/// The spec names what the model must do when the cap is reached.
#[test]
fn the_spec_says_what_to_do_at_the_cap() {
    let spec = budget::spec();
    assert_eq!(spec.name, "budget");
    assert!(
        spec.description.contains("cap_reached"),
        "the description names the field: {}",
        spec.description
    );
    assert!(
        spec.description.to_lowercase().contains("stop"),
        "and says what to do: {}",
        spec.description
    );
    // Both arguments exist and neither is required, so a turn can call it bare.
    let required = &spec.parameters["required"];
    assert_eq!(required.as_array().map(Vec::len), Some(0));
}

/// The answer equals the rows, and the soft cap comes from the setting.
#[test]
fn the_answer_matches_the_rows_in_the_database() {
    let db = Db::open_in_memory().expect("a database");
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO chat_messages (id, conversation_id, role, body, state, model, \
             prompt_tokens, completion_tokens, duration_ms, created_at) \
             VALUES ('m-1', 'conv-1', 'assistant', 'an answer', 'complete', 'a-model', \
             1200, 340, 800, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            [],
        )?;
        Ok(())
    })
    .expect("a row");

    let result = budget::execute(
        &db,
        Some("5000"),
        &serde_json::json!({ "conversation": "conv-1", "window": "all" }),
    );
    assert!(!result.is_error, "{}", result.content);
    let body: serde_json::Value = serde_json::from_str(&result.content).expect("json");
    assert_eq!(body["turns"], 1);
    assert_eq!(body["prompt_tokens"], 1_200);
    assert_eq!(body["completion_tokens"], 340);
    assert_eq!(body["total_tokens"], 1_540);
    assert_eq!(body["soft_cap_tokens"], 5_000);
    assert_eq!(body["remaining_tokens"], 3_460);
    assert_eq!(body["cap_reached"], false);

    // The same figure a query gets, so the tool and the database cannot disagree.
    let sum: i64 = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT sum(COALESCE(prompt_tokens, 0)) + sum(COALESCE(completion_tokens, 0)) \
                 FROM chat_messages WHERE conversation_id = 'conv-1'",
                [],
                |row| row.get(0),
            )
        })
        .expect("a sum");
    assert_eq!(body["total_tokens"].as_i64(), Some(sum));
}

/// A bare call reports every conversation in the window.
#[test]
fn a_bare_call_reports_the_whole_daemon() {
    let db = Db::open_in_memory().expect("a database");
    let result = budget::execute(&db, None, &serde_json::json!({}));
    assert!(!result.is_error, "{}", result.content);
    let body: serde_json::Value = serde_json::from_str(&result.content).expect("json");
    assert_eq!(body["turns"], 0, "an empty database reports zero turns");
    assert_eq!(body["soft_cap_tokens"], budget::DEFAULT_SOFT_CAP);
    assert_eq!(body["window"], "all time", "the default window");
    assert_eq!(body["cap_reached"], false);
}

/// The sandbox layer refuses by name rather than claiming an unknown tool.
#[test]
fn the_sandbox_layer_refuses_by_name() {
    let result = budget::sandbox_refusal();
    assert!(result.is_error || result.content.contains("conversation turn"));
    assert!(
        result.content.contains("conversation turn"),
        "the refusal names where it does run: {}",
        result.content
    );
}
