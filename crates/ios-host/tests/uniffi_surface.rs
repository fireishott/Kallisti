//! The UniFFI surface, exercised from the host target.
//!
//! These exercise the same public functions the Swift bindings will call.
//! They do not build the bindings and they do not prove the device half: see
//! `docs/adr/0003-lite-spike.md`, which keeps the device gate `unproven`.

use ios_host::{TurnResult, fake_provider, run_fake_turn};

#[test]
fn run_fake_turn_returns_the_fixture_string() {
    assert_eq!(run_fake_turn("x".to_owned()).text, "fixture-ok: x");
}

#[test]
fn run_fake_turn_echoes_the_input_verbatim() {
    let input = "hello world / ünïcode / \"quoted\"";
    let expected = format!("fixture-ok: {input}");
    assert_eq!(run_fake_turn(input.to_owned()).text, expected);
}

#[test]
fn run_fake_turn_of_empty_input_is_the_bare_fixture() {
    assert_eq!(run_fake_turn(String::new()).text, "fixture-ok: ");
}

#[test]
fn turn_result_is_a_plain_record() {
    // A UniFFI `Record` is data: cloneable, comparable, and free of handles.
    let first = TurnResult {
        text: "fixture-ok: a".to_owned(),
    };
    let second = first.clone();
    assert_eq!(first, second);
    assert_eq!(second.text, "fixture-ok: a");
}

#[test]
fn the_fixture_prefix_is_the_one_the_plan_requires() {
    assert_eq!(fake_provider::FIXTURE_PREFIX, "fixture-ok: ");
    assert_eq!(fake_provider::respond("x"), "fixture-ok: x");
}

#[test]
fn two_turns_are_independent() {
    let a = run_fake_turn("a".to_owned());
    let b = run_fake_turn("b".to_owned());
    assert_ne!(a, b);
    assert_eq!(a.text, "fixture-ok: a");
    assert_eq!(b.text, "fixture-ok: b");
}
