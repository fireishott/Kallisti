//! Tool calls that arrived as text instead of as calls.
//!
//! Some provider routes, when a model emits more than one call in a round, fail
//! to lift the calls into the structured `tool_calls` array and pass the model's
//! own serialization through as message content instead. The result is an answer
//! made of markup:
//!
//! ```text
//! <||DSML||calls>
//! <||DSML||invoke name="run_command">
//! <||DSML||parameter name="command" string="true">ls -la</||DSML||parameter>
//! </||DSML||invoke>
//! </||DSML||calls>
//! ```
//!
//! Showing that to the operator is a failure with extra steps, and deleting it
//! loses work the model already decided to do. So it is parsed back into real
//! calls and run through the same gate and executor as any other call.
//!
//! The parser is tolerant on purpose. It keys on the word `DSML` inside a tag and
//! on the words `invoke`, `parameter` and `calls`, so a different pipe, space or
//! slash arrangement still parses. It refuses to invent anything: a block with no
//! name is not a call.

use serde_json::{Map, Value};

/// What a text block turned into.
#[derive(Debug, Clone, PartialEq)]
pub struct Recovered {
    /// The visible text, with every DSML tag removed.
    pub text: String,
    /// The calls the model tried to make.
    pub calls: Vec<core_domain::ToolCall>,
}

/// One call under construction.
struct Building {
    name: String,
    arguments: Map<String, Value>,
    /// The parameter whose value is currently being collected.
    open_param: Option<(String, String)>,
}

/// Split model text into visible text and the calls it hid.
///
/// `None` means there was nothing to recover: no marker, so the caller keeps the
/// text exactly as it arrived, byte for byte.
#[must_use]
pub fn recover(text: &str) -> Option<Recovered> {
    if !text.contains("DSML") {
        return None;
    }

    let mut visible = String::with_capacity(text.len());
    let mut calls: Vec<core_domain::ToolCall> = Vec::new();
    let mut current: Option<Building> = None;
    let mut rest = text;

    while let Some(start) = rest.find('<') {
        let Some(end_rel) = rest[start..].find('>') else {
            break;
        };
        let end = start + end_rel;
        let tag = &rest[start..=end];
        let after = &rest[end + 1..];

        if !tag.contains("DSML") {
            // An ordinary angle bracket, which is just text.
            visible.push_str(&rest[..start + 1]);
            rest = after;
            continue;
        }

        // Everything before the tag is either the current parameter's value or
        // visible text the model wrote around the block.
        let between = &rest[..start];
        if let Some(building) = current.as_mut()
            && let Some((name, value)) = building.open_param.as_mut()
        {
            value.push_str(between);
            building
                .arguments
                .insert(name.clone(), Value::String(value.clone()));
            building.open_param = None;
        } else if current.is_none() {
            visible.push_str(between);
        }

        let lower = tag.to_ascii_lowercase();
        let closes = lower.starts_with("</");
        if lower.contains("invoke") {
            if closes {
                if let Some(building) = current.take()
                    && !building.name.is_empty()
                {
                    let index = u32::try_from(calls.len()).unwrap_or(0);
                    if let Some(call) = finish(index, building) {
                        calls.push(call);
                    }
                }
                visible.push(' ');
            } else {
                if let Some(building) = current.take()
                    && !building.name.is_empty()
                {
                    let index = u32::try_from(calls.len()).unwrap_or(0);
                    if let Some(call) = finish(index, building) {
                        calls.push(call);
                    }
                }
                current = Some(Building {
                    name: attribute(tag, "name").unwrap_or_default(),
                    arguments: Map::new(),
                    open_param: None,
                });
            }
        } else if lower.contains("parameter") {
            if closes {
                if let Some(building) = current.as_mut()
                    && let Some((name, value)) = building.open_param.take()
                {
                    building.arguments.insert(name, Value::String(value));
                }
            } else if let Some(building) = current.as_mut() {
                let name = attribute(tag, "name").unwrap_or_default();
                building.open_param = Some((name, String::new()));
            }
        }
        rest = &rest[end + 1..];
    }

    // Trailing text after the last tag. A call still open swallows it as its
    // parameter value, which the block below finishes.
    if current.is_none() {
        visible.push_str(rest);
    }

    // A block that ran to the end without its closing tag is still a call the
    // model meant to make.
    if let Some(building) = current.take()
        && !building.name.is_empty()
    {
        let index = u32::try_from(calls.len()).unwrap_or(0);
        if let Some(call) = finish(index, building) {
            calls.push(call);
        }
    }

    Some(Recovered {
        text: tidy(&visible),
        calls,
    })
}

/// Close one call: pull the trailing parameter in and build it.
fn finish(index: u32, mut building: Building) -> Option<core_domain::ToolCall> {
    if let Some((name, value)) = building.open_param.take() {
        building
            .arguments
            .insert(name, Value::String(value.trim().to_owned()));
    }
    let arguments = Value::Object(building.arguments);
    core_domain::ToolCall::new(
        index,
        format!("dsml-{index}"),
        building.name.trim(),
        arguments,
    )
    .ok()
}

/// The value of `name="..."` inside a tag.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let rest = &tag[start..];
    let end = rest.find('"')?;
    Some(rest[..end].trim().to_owned())
}

/// Collapse the whitespace the removed tags left behind.
fn tidy(text: &str) -> String {
    text.split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape seen in the wild, pipes and spaces included.
    const SAMPLE: &str = "Let me check the warehouse.\n\
        <| | DSML | | calls>\n\
        <| | DSML | | invoke name=\"run_command\">\n\
        <| | DSML | | parameter name=\"command\" string=\"true\">cd ~/sports-warehouse && ls -la db</| | DSML | | parameter>\n\
        </| | DSML | | invoke>\n\
        <| | DSML | | invoke name=\"read_file\">\n\
        <| | DSML | | parameter name=\"path\" string=\"true\">notes.md</| | DSML | | parameter>\n\
        </| | DSML | | invoke>\n\
        </| | DSML | | calls>\n";

    #[test]
    fn two_calls_come_back_as_calls_and_the_markup_goes() {
        let recovered = recover(SAMPLE).expect("markers were present");
        assert_eq!(recovered.calls.len(), 2);

        assert_eq!(recovered.calls[0].name, "run_command");
        assert_eq!(
            recovered.calls[0].arguments["command"],
            "cd ~/sports-warehouse && ls -la db"
        );
        assert_eq!(recovered.calls[1].name, "read_file");
        assert_eq!(recovered.calls[1].arguments["path"], "notes.md");

        assert!(!recovered.text.contains("DSML"), "{}", recovered.text);
        assert!(
            recovered.text.contains("Let me check the warehouse."),
            "the prose the model wrote is kept: {}",
            recovered.text
        );
    }

    #[test]
    fn text_without_markers_is_left_exactly_alone() {
        assert!(recover("just an answer, no markup").is_none());
        assert!(recover("a < b and c > d").is_none());
    }

    #[test]
    fn markup_with_no_name_is_not_invented_into_a_call() {
        let text = "<||DSML||calls>\n<||DSML||invoke>\n</||DSML||invoke>\n</||DSML||calls>";
        let recovered = recover(text).expect("markers were present");
        assert!(recovered.calls.is_empty(), "a nameless block is not a call");
        assert!(!recovered.text.contains("DSML"));
    }

    #[test]
    fn an_unclosed_block_still_yields_the_call_it_started() {
        let text = "<||DSML||invoke name=\"list_dir\">\n\
                    <||DSML||parameter name=\"path\" string=\"true\">.</||DSML||parameter>";
        let recovered = recover(text).expect("markers were present");
        assert_eq!(recovered.calls.len(), 1);
        assert_eq!(recovered.calls[0].arguments["path"], ".");
    }

    #[test]
    fn a_single_pipe_and_no_spaces_parses_too() {
        let text = "<|DSML|invoke name=\"fetch_url\">\
                    <|DSML|parameter name=\"url\" string=\"true\">https://example.com</|DSML|parameter>\
                    </|DSML|invoke>";
        let recovered = recover(text).expect("markers were present");
        assert_eq!(recovered.calls.len(), 1);
        assert_eq!(recovered.calls[0].name, "fetch_url");
        assert_eq!(recovered.calls[0].arguments["url"], "https://example.com");
    }
}
