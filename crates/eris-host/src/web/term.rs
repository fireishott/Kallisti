//! A tool result, rendered the way a terminal would have printed it.
//!
//! A tool hands the model structure, not prose: `run_command` answers with
//! `command`, `exit_code`, `stdout` and `stderr` inside one JSON object. That is
//! the right shape for the model and the wrong shape for the operator, who was
//! being shown that object verbatim - a wall of escaped JSON where a terminal
//! readout belongs. The complaint that started this module was literal: a window
//! titled `kallisti - stdout` whose body read
//! `assert!(full.contains(&"run_command"));\n assert!(`, quotes and `\n` escapes
//! intact.
//!
//! So the result is parsed here and turned into lines. A command becomes a prompt
//! line and what it printed, stderr keeps a kind of its own so it can be painted,
//! and a shape nobody has taught this module falls back to pretty-printed JSON
//! rather than to escaped text. The page and the streaming script render the same
//! [`Term`], so a live turn and a reloaded transcript cannot drift apart.
//!
//! Nothing here decides a colour. [`TermKind::class`] names a class and the
//! stylesheet owns the palette, which is the same rule the rest of the UI follows.

use serde_json::{Map, Value};

/// The class the prompt line is painted with.
pub const PROMPT_CLASS: &str = "term-line prompt";

/// The most lines one window will show.
///
/// A tool truncates its own output to 64 KiB, which can still be a couple of
/// thousand lines. The extra ones are dropped with a notice rather than shipped:
/// a page that carries every byte of a `cargo build` log is a page that renders
/// slowly on a phone.
pub const MAX_LINES: usize = 400;

/// How one line of a tool's output is painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TermKind {
    /// Ordinary output.
    Out,
    /// A failure the tool reported, or a line it wrote to stderr.
    Err,
    /// A line this module added: an exit status, a truncation, a parked call.
    Notice,
}

impl TermKind {
    /// The class this line is painted with.
    #[must_use]
    pub const fn class(self) -> &'static str {
        match self {
            Self::Out => "term-line",
            Self::Err => "term-line err",
            Self::Notice => "term-line notice",
        }
    }
}

/// One line of a rendered result.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TermLine {
    /// The text, with escapes resolved and control bytes gone.
    pub text: String,
    /// How it is painted.
    pub kind: TermKind,
}

impl TermLine {
    /// One line, in one kind.
    #[must_use]
    pub fn new(text: impl Into<String>, kind: TermKind) -> Self {
        Self {
            text: text.into(),
            kind,
        }
    }
}

/// A result, as a terminal would have shown it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Term {
    /// The command line the window states first, already prefixed.
    pub prompt: String,
    /// What it printed, in order.
    pub lines: Vec<TermLine>,
}

impl Term {
    /// The whole body as plain text: what the operator would get from a copy.
    ///
    /// Useful to a test that needs to prove the escapes are gone without reading
    /// the markup.
    #[must_use]
    pub fn text(&self) -> String {
        let mut out = String::with_capacity(self.prompt.len() + 32);
        out.push_str(&self.prompt);
        for line in &self.lines {
            out.push('\n');
            out.push_str(&line.text);
        }
        out
    }
}

/// Render one tool call's result as a terminal would have shown it.
///
/// `arguments` is the JSON the model sent, `output` is what the tool returned, and
/// `is_error` decides how an unparseable result is painted.
#[must_use]
pub fn render(name: &str, arguments: &str, output: &str, is_error: bool) -> Term {
    let prompt = prompt_for(name, arguments);
    let mut lines: Vec<TermLine> = Vec::new();
    match serde_json::from_str::<Value>(output.trim()) {
        Ok(Value::Object(map)) => body(&map, &mut lines),
        // Valid JSON that is not an object is a value, not a report: print it.
        Ok(other) => push(&mut lines, &other.to_string(), TermKind::Out),
        // Not JSON at all - an MCP server's prose, or a result cut mid-way. It is
        // already text, so it is shown as text.
        Err(_) => push(
            &mut lines,
            output,
            if is_error {
                TermKind::Err
            } else {
                TermKind::Out
            },
        ),
    }
    // Trailing blanks are the tool's, not the reader's: every captured stdout
    // ends with a newline, and rendering that as an empty last line makes every
    // window look like it is still arriving.
    while lines.last().is_some_and(|line| line.text.trim().is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        lines.push(TermLine::new(
            if is_error {
                "the call failed and printed nothing"
            } else {
                "(no output)"
            },
            TermKind::Notice,
        ));
    }
    if lines.len() > MAX_LINES {
        let hidden = lines.len() - MAX_LINES;
        lines.truncate(MAX_LINES);
        lines.push(TermLine::new(
            format!("[{hidden} more lines]"),
            TermKind::Notice,
        ));
    }
    Term { prompt, lines }
}

/// The command line the window leads with.
///
/// A shell command keeps its `$` because that is what the operator types and what
/// the output below it answers. Every other tool is stated as `name subject`,
/// which is honest about what ran rather than dressing a file read up as `cat`.
#[must_use]
pub fn prompt_for(name: &str, arguments: &str) -> String {
    let parsed: Option<Value> = serde_json::from_str(arguments.trim()).ok();
    let field = |key: &str| {
        parsed
            .as_ref()
            .and_then(|value| value.get(key))
            .and_then(Value::as_str)
            .unwrap_or("")
    };
    if name == "run_command" {
        let command = field("command");
        if command.trim().is_empty() {
            return "$ (no command)".to_owned();
        }
        return format!("$ {command}");
    }
    let subject = match name {
        "read_file" | "write_file" | "list_dir" => field("path"),
        "fetch_url" => field("url"),
        "generate_image" => field("prompt"),
        _ => "",
    };
    if !subject.trim().is_empty() {
        return format!("{name} {}", shorten(subject.trim(), 160));
    }
    match parsed {
        None | Some(Value::Null) => name.to_owned(),
        Some(Value::Object(map)) if map.is_empty() => name.to_owned(),
        Some(other) => format!("{name} {}", shorten(&other.to_string(), 160)),
    }
}

/// Turn one JSON object into lines, or into pretty JSON when its shape is unknown.
fn body(map: &Map<String, Value>, lines: &mut Vec<TermLine>) {
    // A parked or approved call is the approval rewrite's own envelope, and its
    // `output` is whatever the tool answered once a person said yes.
    if let Some(status) = map.get("status").and_then(Value::as_str) {
        match status {
            "pending_approval" => {
                if let Some(message) = map.get("message").and_then(Value::as_str) {
                    push(lines, message, TermKind::Notice);
                }
                if let Some(tool) = map.get("tool").and_then(Value::as_str) {
                    push(lines, &format!("waiting on you: {tool}"), TermKind::Notice);
                }
                return;
            }
            "approved" => {
                if let Some(inner) = map.get("output").and_then(Value::as_str) {
                    // What actually ran answered with its own result, which is
                    // usually JSON again. One level of nesting is all a rewrite
                    // ever adds, so one level of recursion is all this needs.
                    match serde_json::from_str::<Value>(inner.trim()) {
                        Ok(Value::Object(inner_map)) => body(&inner_map, lines),
                        Ok(other) => push(lines, &other.to_string(), TermKind::Out),
                        Err(_) => push(lines, inner, TermKind::Out),
                    }
                }
                return;
            }
            _ => {}
        }
    }
    if let Some(error) = map.get("error").and_then(Value::as_str) {
        push(lines, error, TermKind::Err);
        return;
    }
    if map.contains_key("stdout") || map.contains_key("stderr") || map.contains_key("exit_code") {
        // An absent or empty stream contributes nothing. A blank stdout line at
        // the head of a window whose only content is stderr is noise, and the
        // "printed nothing" notice already covers a call that said nothing at
        // all.
        if let Some(stdout) = map.get("stdout").and_then(Value::as_str)
            && !stdout.is_empty()
        {
            push(lines, stdout, TermKind::Out);
        }
        if let Some(stderr) = map.get("stderr").and_then(Value::as_str)
            && !stderr.is_empty()
        {
            push(lines, stderr, TermKind::Err);
        }
        // A non-zero exit is the failure, and it belongs in the window rather
        // than only in the model's next sentence. A null exit code means the
        // kernel ended the child - `timeout` doing its job - and saying so beats
        // printing nothing at the moment the operator most wants a reason.
        match map.get("exit_code") {
            Some(Value::Number(number)) => {
                if let Some(code) = number.as_i64()
                    && code != 0
                {
                    push(lines, &format!("exit {code}"), TermKind::Notice);
                }
            }
            Some(_) => push(
                lines,
                "no exit code: the command was killed",
                TermKind::Notice,
            ),
            None => {}
        }
        return;
    }
    if let Some(content) = map.get("content").and_then(Value::as_str) {
        push(lines, content, TermKind::Out);
        return;
    }
    if let Some(entries) = map.get("entries").and_then(Value::as_array) {
        for entry in entries {
            if let Some(name) = entry.as_str() {
                push(lines, name, TermKind::Out);
            }
        }
        return;
    }
    if let Some(text) = map.get("text").and_then(Value::as_str) {
        push(lines, text, TermKind::Out);
        if map.get("truncated").and_then(Value::as_bool) == Some(true) {
            push(
                lines,
                "the page was longer than the fetch limit and stops here",
                TermKind::Notice,
            );
        }
        return;
    }
    // A write and a drawing both answer with where it landed. That is one line,
    // not a two-key JSON document.
    if let (Some(path), Some(bytes)) = (
        map.get("path").and_then(Value::as_str),
        map.get("bytes").and_then(Value::as_u64),
    ) {
        push(
            lines,
            &format!("wrote {path} ({bytes} bytes)"),
            TermKind::Out,
        );
        return;
    }
    // A shape this module has never been taught. Pretty JSON, never escaped JSON:
    // a new tool renders as something readable on the day it ships.
    if !map.is_empty() {
        let pretty = serde_json::to_string_pretty(&Value::Object(map.clone())).unwrap_or_default();
        push(lines, &pretty, TermKind::Out);
    }
}

/// Split text into lines and append them in one kind.
fn push(lines: &mut Vec<TermLine>, text: &str, kind: TermKind) {
    for raw in text.split('\n') {
        // A carriage return means the line was redrawn in place - a progress
        // bar, a spinner - and a terminal shows only what came after the last
        // one. Keeping the earlier frames would print the bar a hundred times.
        let raw = raw.rsplit('\r').next().unwrap_or("");
        lines.push(TermLine::new(clean(raw), kind));
    }
}

/// Drop control bytes and ANSI escapes, so captured output reads as text.
///
/// A tool that runs `ls --color=always` returns sequences a browser would paint
/// as literal `[01;34m` noise. There is no terminal to obey them, so they are
/// removed rather than shown.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            match chars.next() {
                // CSI: ESC [ parameters, ending on a byte in @..=~.
                Some('[') => {
                    for next in chars.by_ref() {
                        if ('@'..='~').contains(&next) {
                            break;
                        }
                    }
                }
                // OSC: ESC ] ... terminated by BEL or ESC \.
                Some(']') => {
                    for next in chars.by_ref() {
                        if next == '\u{7}' {
                            break;
                        }
                        if next == '\u{1b}' {
                            let _ = chars.next();
                            break;
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        if character == '\t' {
            out.push('\t');
            continue;
        }
        if (character as u32) < 0x20 || character == '\u{7f}' {
            continue;
        }
        out.push(character);
    }
    out
}

/// Cut a long string to a stated budget, with an ellipsis when it was cut.
fn shorten(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut out: String = text.chars().take(limit).collect();
    out.push_str("...");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `run_command` returns, as the daemon stores it.
    fn command(stdout: &str, stderr: &str, exit: Option<i64>) -> String {
        serde_json::json!({
            "command": "cargo test",
            "exit_code": exit,
            "stdout": stdout,
            "stderr": stderr,
        })
        .to_string()
    }

    #[test]
    fn a_command_becomes_a_prompt_line_and_real_lines() {
        let term = render(
            "run_command",
            r#"{"command":"cargo test"}"#,
            &command("running 2 tests\ntest a ... ok\n", "", Some(0)),
            false,
        );
        assert_eq!(term.prompt, "$ cargo test");
        assert_eq!(term.lines.len(), 2, "the trailing newline is not a line");
        assert_eq!(term.lines[0].text, "running 2 tests");
        assert_eq!(term.lines[1].text, "test a ... ok");
        assert!(
            term.lines.iter().all(|line| line.kind == TermKind::Out),
            "a clean run is all ordinary output"
        );
    }

    #[test]
    fn the_escapes_that_started_this_are_gone() {
        // The complaint, reproduced: a result whose stdout is a code snippet.
        let snippet = "assert!(full.contains(&\"run_command\"));\nassert!(confined);";
        let term = render(
            "run_command",
            r#"{"command":"grep -n assert src/tools/mod.rs"}"#,
            &command(snippet, "", Some(0)),
            false,
        );
        let text = term.text();
        assert!(
            !text.contains("\\n"),
            "a literal backslash-n is not a line break: {text}"
        );
        assert!(
            !text.contains("\\\""),
            "a literal escaped quote is not a quote: {text}"
        );
        assert!(text.contains("assert!(full.contains(&\"run_command\"));"));
        assert_eq!(term.lines.len(), 2);
    }

    #[test]
    fn stderr_keeps_its_own_kind_and_a_nonzero_exit_is_stated() {
        let term = render(
            "run_command",
            r#"{"command":"cargo build"}"#,
            &command("", "error: expected one of", Some(101)),
            true,
        );
        assert_eq!(term.lines[0].kind, TermKind::Err);
        assert_eq!(term.lines[0].text, "error: expected one of");
        assert_eq!(term.lines[1].text, "exit 101");
        assert_eq!(term.lines[1].kind, TermKind::Notice);
    }

    #[test]
    fn a_killed_command_says_so_instead_of_printing_nothing() {
        let term = render(
            "run_command",
            r#"{"command":"sleep 900"}"#,
            &command("", "", None),
            true,
        );
        assert_eq!(term.prompt, "$ sleep 900");
        assert!(
            term.lines.iter().any(|line| line.text.contains("killed")),
            "a null exit code is the kernel, and the window says so: {:?}",
            term.lines
        );
    }

    #[test]
    fn a_command_that_printed_nothing_still_gets_a_line() {
        let term = render(
            "run_command",
            r#"{"command":"true"}"#,
            &command("", "", Some(0)),
            false,
        );
        assert_eq!(term.lines.len(), 1);
        assert_eq!(term.lines[0].text, "(no output)");
        assert_eq!(term.lines[0].kind, TermKind::Notice);
    }

    #[test]
    fn a_read_renders_its_content_as_text_not_as_a_json_field() {
        let output = serde_json::json!({
            "path": "src/main.rs",
            "bytes": 12,
            "content": "fn main() {\n    run();\n}\n",
        })
        .to_string();
        let term = render("read_file", r#"{"path":"src/main.rs"}"#, &output, false);
        assert_eq!(term.prompt, "read_file src/main.rs");
        assert_eq!(
            term.lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            vec!["fn main() {", "    run();", "}"]
        );
        assert!(
            !term.text().contains("content"),
            "the field name is not part of the readout"
        );
    }

    #[test]
    fn a_directory_listing_is_one_entry_per_line() {
        let output = serde_json::json!({"path": ".", "entries": ["a/", "b.txt"]}).to_string();
        let term = render("list_dir", r#"{"path":"."}"#, &output, false);
        assert_eq!(term.prompt, "list_dir .");
        assert_eq!(term.lines[0].text, "a/");
        assert_eq!(term.lines[1].text, "b.txt");
    }

    #[test]
    fn a_refusal_is_an_error_line_with_the_reason_as_written() {
        let output =
            serde_json::json!({"error": "path escapes the workspace: /etc/passwd"}).to_string();
        let term = render("read_file", r#"{"path":"/etc/passwd"}"#, &output, true);
        assert_eq!(term.lines[0].kind, TermKind::Err);
        assert_eq!(
            term.lines[0].text,
            "path escapes the workspace: /etc/passwd"
        );
    }

    #[test]
    fn a_parked_call_says_nothing_ran() {
        let output = serde_json::json!({
            "status": "pending_approval",
            "approval_id": "ap-1",
            "tool": "write_file",
            "message": "This call is waiting for the operator to approve it. Nothing has run yet.",
        })
        .to_string();
        let term = render("write_file", r#"{"path":"x.txt"}"#, &output, false);
        assert_eq!(term.prompt, "write_file x.txt");
        assert!(
            term.lines.iter().all(|line| line.kind == TermKind::Notice),
            "a parked call is not an answer and not an error"
        );
        assert!(term.text().contains("Nothing has run yet"));
        assert!(term.text().contains("waiting on you: write_file"));
    }

    #[test]
    fn an_approved_write_shows_what_actually_ran() {
        let output = serde_json::json!({
            "status": "approved",
            "approval_id": "ap-1",
            "output": serde_json::json!({"path": "round8.txt", "bytes": 13}).to_string(),
        })
        .to_string();
        let term = render("write_file", r#"{"path":"round8.txt"}"#, &output, false);
        assert_eq!(term.lines[0].text, "wrote round8.txt (13 bytes)");
        assert!(
            !term.text().contains("approved"),
            "the envelope is not the news"
        );
    }

    #[test]
    fn a_shape_nobody_taught_this_module_renders_as_pretty_json() {
        let output = serde_json::json!({
            "server": "warehouse",
            "rows": 3,
            "windows": [1, 2],
        })
        .to_string();
        let term = render("warehouse__query", r#"{"sql":"select 1"}"#, &output, false);
        assert_eq!(term.prompt, "warehouse__query {\"sql\":\"select 1\"}");
        let text = term.text();
        assert!(
            !text.contains("\\\""),
            "the fallback must not be escaped JSON: {text}"
        );
        assert!(
            text.contains("\"rows\": 3"),
            "pretty, one key per line: {text}"
        );
    }

    #[test]
    fn ansi_escapes_and_carriage_returns_are_cleaned() {
        let stdout = "\u{1b}[01;34mdir\u{1b}[0m\tfile\nprogress 10%\rprogress 90%\n\u{7}bell";
        let term = render(
            "run_command",
            r#"{"command":"ls --color=always"}"#,
            &command(stdout, "", Some(0)),
            false,
        );
        assert_eq!(term.lines[0].text, "dir\tfile");
        assert_eq!(
            term.lines[1].text, "progress 90%",
            "a redrawn line shows its last frame"
        );
        assert_eq!(term.lines[2].text, "bell");
        assert!(!term.text().contains('\u{1b}'));
    }

    #[test]
    fn a_result_that_is_not_json_is_shown_as_text() {
        let term = render("mcp__tool", "{}", "plain words, no structure", false);
        assert_eq!(term.lines[0].text, "plain words, no structure");
        assert_eq!(term.lines[0].kind, TermKind::Out);
    }

    #[test]
    fn a_huge_result_is_cut_with_a_notice_rather_than_shipped_whole() {
        let stdout: String = (0..MAX_LINES + 25)
            .map(|index| format!("line {index}\n"))
            .collect();
        let term = render(
            "run_command",
            r#"{"command":"cargo build"}"#,
            &command(&stdout, "", Some(0)),
            false,
        );
        assert_eq!(term.lines.len(), MAX_LINES + 1);
        assert_eq!(
            term.lines.last().map(|line| line.text.as_str()),
            Some("[25 more lines]")
        );
        assert_eq!(
            term.lines.last().map(|line| line.kind),
            Some(TermKind::Notice)
        );
    }

    #[test]
    fn the_term_serializes_into_the_shape_the_script_renders() {
        let term = render(
            "run_command",
            r#"{"command":"echo hi"}"#,
            &command("hi\n", "", Some(0)),
            false,
        );
        let wire = serde_json::to_value(&term).expect("a term serializes");
        assert_eq!(wire["prompt"], "$ echo hi");
        assert_eq!(wire["lines"][0]["text"], "hi");
        assert_eq!(
            wire["lines"][0]["kind"], "out",
            "the script reads a kind name, not a variant"
        );
    }

    #[test]
    fn every_kind_names_a_class_the_stylesheet_can_paint() {
        assert_eq!(TermKind::Out.class(), "term-line");
        assert_eq!(TermKind::Err.class(), "term-line err");
        assert_eq!(TermKind::Notice.class(), "term-line notice");
        assert!(PROMPT_CLASS.starts_with("term-line"));
    }
}
