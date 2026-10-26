//! A source-level guard against a bearer secret pasted into `src/`.
//!
//! The rule: no string literal in `src/` may contain a run of 32 or more
//! characters drawn from the base64/hex alphabet. A minted token, a pairing code
//! written out as a fixture, or a pasted API key all look like that; a sentence,
//! a SQL statement, a path and an identifier do not.
//!
//! The scanner is not allowed to be trivially satisfiable:
//!
//! - it fails when it discovers no files at all;
//! - it fails unless it read the auth modules (the ones that would leak);
//! - it fails unless it actually read a meaningful number of bytes;
//! - `the_scanner_flags_a_secret_shaped_literal` proves it still catches one.

use std::fs;
use std::path::{Path, PathBuf};

/// Shortest run that counts as secret-shaped. A 32-byte token in hex is 64
/// characters and in base64url is 43, so 32 is beneath both and above anything
/// an identifier or an English word reaches.
const MIN_SECRET_RUN: usize = 32;

/// Literals that are published constants rather than secrets.
///
/// The only entry is the worked example from RFC 7636 appendix B, which the PKCE
/// test pins `S256` to. It is a public test vector: printed in the RFC, not a
/// grant, and it cannot authenticate anything. Match is exact and whole-run: the
/// run compared is the longest secret-alphabet run in the literal, and it has to
/// appear here character for character, so a real token cannot be waved through
/// by adding a prefix of itself to this list.
const PUBLISHED_NOT_SECRET: &[&str] = &[
    // RFC 7636 B.1, the verifier half of the worked example.
    "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
    // RFC 7636 B.1, the S256 challenge derived from it.
    "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
];

/// Every `.rs` file under `root`, recursively, sorted for a stable message.
fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn is_secret_alphabet(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'_' | b'-')
}

/// Whether a run is a path or an identifier rather than a secret.
///
/// A bearer is drawn from the whole base64 or hex alphabet, so it is almost
/// always mixed case. A route (`/dashboard/settings/providers/probe`) and a model
/// pin (`commandcode/deepseek/deepseek-v4-flash`) are lowercase words joined by
/// separators, and at 35 characters they are long enough to trip a rule that only
/// counts length. Exempting them cannot hide a real token: the chance of a
/// 43-character base64url secret containing no uppercase at all is nil, and a
/// lowercase-only secret with no separator - a hex digest - is still caught,
/// which `the_scanner_flags_a_secret_shaped_literal` keeps honest.
fn is_path_or_identifier(run: &str) -> bool {
    let mut separators = 0usize;
    for character in run.chars() {
        match character {
            '/' | '-' => separators += 1,
            _ if character.is_ascii_lowercase() || character.is_ascii_digit() => {}
            _ => return false,
        }
    }
    separators > 0
}

/// The longest secret-alphabet run in one line, with its length.
fn longest_run(line: &str) -> (usize, String) {
    let mut best = (0usize, String::new());
    let mut current = String::new();
    for character in line.chars() {
        if character.is_ascii() && is_secret_alphabet(character as u8) {
            current.push(character);
        } else {
            if current.len() > best.0 {
                best = (current.len(), current.clone());
            }
            current.clear();
        }
    }
    if current.len() > best.0 {
        best = (current.len(), current);
    }
    best
}

/// The contents of the string literals on one line. A comment line is skipped
/// by the caller; this only cares about quoted text.
fn string_literals(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut literals = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != b'"' {
            index += 1;
            continue;
        }
        index += 1;
        let start = index;
        let mut escaped = false;
        while index < bytes.len() {
            if escaped {
                escaped = false;
                index += 1;
                continue;
            }
            match bytes[index] {
                b'\\' => escaped = true,
                b'"' => break,
                _ => {}
            }
            index += 1;
        }
        let end = index.min(bytes.len());
        if start <= end && end <= line.len() {
            literals.push(line[start..end].to_owned());
        }
        index += 1;
    }
    literals
}

#[test]
fn no_bearer_literals_in_src() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let files = rust_files(&src);

    assert!(
        !files.is_empty(),
        "the scanner found no Rust files under {}; a scanner that scans nothing passes everything",
        src.display()
    );
    assert!(
        files.len() >= 8,
        "expected the whole crate under {}, found {} files",
        src.display(),
        files.len()
    );
    for required in ["auth/token.rs", "auth/pairing.rs", "config.rs"] {
        assert!(
            files.iter().any(|file| file.ends_with(required)),
            "{required} was not scanned; the walk is not covering the crate"
        );
    }

    let mut violations = Vec::new();
    let mut scanned_bytes = 0usize;
    for file in &files {
        let text = fs::read_to_string(file).expect("source files are UTF-8");
        scanned_bytes += text.len();
        for (line_number, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            for literal in string_literals(line) {
                let (length, run) = longest_run(&literal);
                if PUBLISHED_NOT_SECRET.contains(&run.as_str()) {
                    continue;
                }
                if length >= MIN_SECRET_RUN && !is_path_or_identifier(&run) {
                    violations.push(format!(
                        "{}:{}: {length}-character literal beginning {:?}",
                        file.display(),
                        line_number + 1,
                        &run[..12.min(run.len())]
                    ));
                }
            }
        }
    }

    assert!(
        scanned_bytes > 20_000,
        "only {scanned_bytes} bytes were scanned; the walk is not reading the crate"
    );
    assert!(
        violations.is_empty(),
        "secret-shaped string literals in src/: {violations:#?}"
    );
}

#[test]
fn the_scanner_flags_a_secret_shaped_literal() {
    // 32 characters of base64 alphabet, shaped like a slice of a real bearer.
    let sample = format!("\"{}\"", "QUJDREVGR0hJSktMTU5PUFFSU1RVVldY");
    let literal = string_literals(&sample)
        .into_iter()
        .next()
        .expect("the sample has a literal");
    let (length, _) = longest_run(&literal);
    assert!(
        length >= MIN_SECRET_RUN,
        "a {length}-character sample must be flagged"
    );

    // A normal source line is not flagged, even though it has long words.
    let innocent = "let message = \"the pairing code belongs in the handler, not here\";";
    for literal in string_literals(innocent) {
        assert!(longest_run(&literal).0 < MIN_SECRET_RUN, "{literal}");
    }

    // A 64-hex literal is flagged too.
    let hex = format!("\"{}\"", "0123456789abcdef".repeat(4));
    let literal = string_literals(&hex)
        .into_iter()
        .next()
        .expect("the sample has a literal");
    assert_eq!(longest_run(&literal).0, 64);
}

#[test]
fn tracing_calls_never_carry_a_secret() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let forbidden = [
        "token",
        "password",
        "passphrase",
        "secret",
        "bearer",
        "verifier",
        "hash",
    ];
    let mut findings = Vec::new();
    for file in rust_files(&src) {
        let text = fs::read_to_string(&file).expect("source files are UTF-8");
        for (line_number, line) in text.lines().enumerate() {
            if !line.contains("tracing::") {
                continue;
            }
            let lowered = line.to_ascii_lowercase();
            for word in forbidden {
                if lowered.contains(word) {
                    findings.push(format!(
                        "{}:{}: a tracing call mentions {word:?}",
                        file.display(),
                        line_number + 1
                    ));
                }
            }
        }
    }
    assert!(
        findings.is_empty(),
        "a log line must never carry a secret: {findings:#?}"
    );
}
