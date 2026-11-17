//! The Lite boundary is a dependency graph, not a feature flag (locked
//! amendment 9), and Gate A of the Lite proof is a turn with no listener and no
//! network (locked amendment 2). This test enforces that at the source level:
//! no file under `crates/ios-host/src` may mention an HTTP client, an async
//! runtime, or a socket type.
//!
//! This file lives in `tests/` on purpose. It necessarily contains the very
//! tokens it forbids, so keeping it out of `src/` is what lets the CI grep over
//! `crates/ios-host/src/` stay empty. It reads the sources instead of relying
//! on a constant list alone, so a new module cannot be added and forgotten.

use std::fs;
use std::path::{Path, PathBuf};

/// Tokens that must never appear in this crate's own sources.
///
/// Matched case-insensitively, same as the CI grep.
const FORBIDDEN: &[&str] = &["reqwest", "tokio", "TcpListener", "UdpSocket", "std::net"];

/// Collect every `.rs` file under `dir`, recursively.
fn collect_rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read source dir {}: {e}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|e| panic!("cannot read entry in {}: {e}", dir.display()));
        let path = entry.path();
        let file_type = entry
            .file_type()
            .unwrap_or_else(|e| panic!("cannot stat {}: {e}", path.display()));
        if file_type.is_dir() {
            collect_rust_sources(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn sources_contain_no_network_api() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let src_dir = manifest_dir.join("src");
    assert!(
        src_dir.is_dir(),
        "expected a src directory at {}",
        src_dir.display()
    );

    let mut sources = Vec::new();
    collect_rust_sources(&src_dir, &mut sources);
    sources.sort();

    // Guard against a vacuous pass: the scan must actually be looking at the
    // crate surface, not at an empty tree.
    assert!(
        sources.iter().any(|p| p.ends_with("src/lib.rs")),
        "src/lib.rs was not found under {}",
        src_dir.display()
    );
    assert!(
        sources.len() >= 2,
        "expected at least lib.rs and the fake provider, found {} file(s): {:?}",
        sources.len(),
        sources
    );

    let mut violations = Vec::new();
    for path in &sources {
        let text = fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
        let lower = text.to_lowercase();
        for token in FORBIDDEN {
            if lower.contains(&token.to_lowercase()) {
                violations.push(format!(
                    "{} contains {:?}",
                    path.strip_prefix(manifest_dir).unwrap_or(path).display(),
                    token
                ));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "the Lite crate must stay I/O-free; found:\n  {}",
        violations.join("\n  ")
    );
}

#[test]
fn the_dependency_graph_stays_minimal() {
    // The manifest is the boundary. A network stack or a device framework
    // appearing here is the same failure as it appearing in the source.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_path = manifest_dir.join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", manifest_path.display()));
    let lower = manifest.to_lowercase();

    for token in [
        "reqwest",
        "tokio",
        "hyper",
        "ureq",
        "async-std",
        "smol",
        "eris-host",
        "core-runtime",
        "uniffi_bindgen",
        "uniffi_build",
    ] {
        assert!(
            !lower.contains(&token.to_lowercase()),
            "crates/ios-host/Cargo.toml must not depend on {token:?}"
        );
    }

    // The one allowed dependency beyond the local domain crate.
    assert!(
        lower.contains("uniffi"),
        "ios-host must depend on uniffi for the proc-macro surface"
    );
}
