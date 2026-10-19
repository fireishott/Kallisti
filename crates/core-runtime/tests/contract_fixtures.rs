//! Contract fixtures must exist in the tree.
//!
//! `contract/fixtures/` is the single source of golden wire and log fixtures.
//! An empty directory does not survive a clone, so it carries a `.gitkeep`, and
//! this test fails if either the directory or the placeholder goes missing. That
//! keeps a fixture-driven test elsewhere from quietly turning into a no-op.

use std::path::{Path, PathBuf};

/// `contract/fixtures`, resolved from the crate root so the test works from any
/// working directory.
fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contract/fixtures")
}

#[test]
fn contract_fixtures_dir_exists() {
    let dir = fixtures_dir();
    assert!(
        dir.is_dir(),
        "missing contract fixtures directory: {}",
        dir.display()
    );
}

#[test]
fn contract_fixtures_dir_is_kept_in_git() {
    let keep = fixtures_dir().join(".gitkeep");
    assert!(
        keep.is_file(),
        "missing {}: an empty fixtures directory does not survive a clone",
        keep.display()
    );
}
