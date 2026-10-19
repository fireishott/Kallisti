//! The sandbox workspace: the one directory a turn may write to.
//!
//! The default is `~/kallisti-workspace`, created at mode `0755` when it does
//! not exist. Any absolute directory is allowed **except** one that overlaps a
//! path the daemon must never be able to reach through a turn:
//! `~/.dsh`, `~/.config`, `~/.ssh`, `~/vault`. "Overlaps" is tested in both
//! directions, so neither `~/.config/ws` (inside) nor `~` (which contains all
//! four) can be chosen.
//!
//! The comparison happens on canonical paths with the longest existing prefix
//! resolved, so a symlinked home or a `..` in the middle cannot walk around the
//! check, and a directory that does not exist yet is still compared as the path
//! it would become.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};

use crate::config::expand_tilde;

/// The default workspace, as documented.
pub const DEFAULT_WORKSPACE: &str = "~/kallisti-workspace";

/// Mode a newly created workspace gets.
pub const WORKSPACE_MODE: u32 = 0o755;

/// Paths a workspace may not overlap, as documented in the onboarding spec.
pub const REFUSED_ROOTS: [&str; 4] = ["~/.dsh", "~/.config", "~/.ssh", "~/vault"];

/// Why a workspace path was refused.
#[derive(Debug, thiserror::Error)]
pub enum WorkspaceError {
    /// The field was blank.
    #[error("a workspace path is required")]
    Empty,
    /// The path was relative.
    #[error("the workspace path must be absolute or start with ~")]
    NotAbsolute {
        /// What was submitted.
        value: String,
    },
    /// The path contained a `..` component after `~` expansion.
    #[error("the workspace path must not contain ..")]
    ParentTraversal,
    /// The path overlaps a protected root.
    #[error("the workspace may not overlap {root}")]
    RefusedOverlap {
        /// The protected root that was hit.
        root: PathBuf,
    },
    /// The path exists and is not a directory.
    #[error("{path} exists and is not a directory")]
    NotADirectory {
        /// What was found.
        path: PathBuf,
    },
    /// `$HOME` is not set, so `~` cannot be expanded.
    #[error("HOME is not set, so a workspace starting with ~ cannot be resolved")]
    NoHome,
    /// The path could not be checked or created.
    #[error("cannot use {path} as the workspace: {source}")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// The directory exists and is writable by anyone.
    #[error("workspace {path} is mode {actual:04o}: a world-writable sandbox root is refused")]
    WorldWritable {
        /// Path that failed.
        path: PathBuf,
        /// Mode found.
        actual: u32,
    },
}

impl WorkspaceError {
    /// The stable machine-readable code for this refusal.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Empty => "workspace_empty",
            Self::NotAbsolute { .. } => "workspace_not_absolute",
            Self::ParentTraversal => "workspace_parent_traversal",
            Self::RefusedOverlap { .. } => "workspace_reserved",
            Self::NotADirectory { .. } => "workspace_not_a_directory",
            Self::NoHome => "workspace_no_home",
            Self::Io { .. } => "workspace_io",
            Self::WorldWritable { .. } => "workspace_world_writable",
        }
    }
}

/// A validated workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedWorkspace {
    /// The absolute path.
    pub path: PathBuf,
    /// True when this call created the directory.
    pub created: bool,
}

/// The default workspace path, expanded.
pub fn default_workspace() -> Result<PathBuf, WorkspaceError> {
    resolve(DEFAULT_WORKSPACE)
}

/// Validate one submitted path and create it at mode `0755` if it is absent.
pub fn prepare(input: &str) -> Result<PreparedWorkspace, WorkspaceError> {
    let path = resolve(input)?;
    check_overlaps(&path)?;

    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(WorkspaceError::Io {
                path: path.clone(),
                source,
            });
        }
    };

    let created = match metadata {
        Some(metadata) if metadata.is_dir() => false,
        Some(_) => {
            return Err(WorkspaceError::NotADirectory { path: path.clone() });
        }
        None => {
            fs::create_dir_all(&path).map_err(|source| WorkspaceError::Io {
                path: path.clone(),
                source,
            })?;
            fs::set_permissions(&path, fs::Permissions::from_mode(WORKSPACE_MODE)).map_err(
                |source| WorkspaceError::Io {
                    path: path.clone(),
                    source,
                },
            )?;
            true
        }
    };

    // An existing directory keeps its own mode, with one exception: a
    // world-writable sandbox root means any local account can plant a file the
    // daemon will later read, so it is refused rather than silently tightened.
    let mode = fs::metadata(&path)
        .map_err(|source| WorkspaceError::Io {
            path: path.clone(),
            source,
        })?
        .permissions()
        .mode()
        & 0o777;
    if mode & 0o002 != 0 {
        return Err(WorkspaceError::WorldWritable { path, actual: mode });
    }

    Ok(PreparedWorkspace { path, created })
}

/// Expand `~`, require an absolute path, and reject `..` outright.
fn resolve(input: &str) -> Result<PathBuf, WorkspaceError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(WorkspaceError::Empty);
    }
    let expanded = expand_tilde(Path::new(trimmed));
    if !expanded.is_absolute() {
        return Err(WorkspaceError::NotAbsolute {
            value: trimmed.to_owned(),
        });
    }
    let mut cleaned = PathBuf::new();
    for component in expanded.components() {
        match component {
            Component::ParentDir => return Err(WorkspaceError::ParentTraversal),
            Component::CurDir => {}
            other => cleaned.push(other.as_os_str()),
        }
    }
    if cleaned.as_os_str().is_empty() || cleaned == Path::new("/") {
        return Err(WorkspaceError::RefusedOverlap {
            root: PathBuf::from("/"),
        });
    }
    Ok(cleaned)
}

/// Refuse a workspace that is, contains, or sits inside a protected root.
fn check_overlaps(workspace: &Path) -> Result<(), WorkspaceError> {
    let workspace = canonical_or_self(workspace);
    for root in REFUSED_ROOTS {
        let root_path = expand_tilde(Path::new(root));
        if root_path.as_os_str().is_empty() {
            // `$HOME` was unset: compare against the literal, which cannot match.
            continue;
        }
        let root_path = canonical_or_self(&root_path);
        let overlaps = workspace == root_path
            || workspace.starts_with(&root_path)
            || root_path.starts_with(&workspace);
        if overlaps {
            return Err(WorkspaceError::RefusedOverlap { root: root_path });
        }
    }
    Ok(())
}

/// Canonicalize the longest existing prefix and keep the rest verbatim, so a
/// path that does not exist yet is still compared as the path it would become.
fn canonical_or_self(path: &Path) -> PathBuf {
    if let Ok(canonical) = fs::canonicalize(path) {
        return canonical;
    }
    let mut remainder: Vec<std::ffi::OsString> = Vec::new();
    let mut cursor = path.to_path_buf();
    loop {
        let Some(parent) = cursor.parent().map(Path::to_path_buf) else {
            return path.to_path_buf();
        };
        let Some(name) = cursor.file_name().map(|name| name.to_owned()) else {
            return path.to_path_buf();
        };
        remainder.push(name);
        if let Ok(canonical) = fs::canonicalize(&parent) {
            let mut out = canonical;
            for part in remainder.iter().rev() {
                out.push(part);
            }
            return out;
        }
        if parent.as_os_str().is_empty() || parent == cursor {
            return path.to_path_buf();
        }
        cursor = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_workspace_is_the_documented_path() {
        let expected =
            PathBuf::from(std::env::var("HOME").expect("HOME")).join("kallisti-workspace");
        assert_eq!(default_workspace().expect("resolve"), expected);
    }

    #[test]
    fn a_fresh_workspace_is_created_at_0755() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("workspace");
        let prepared = prepare(&path.display().to_string()).expect("prepared");
        assert!(prepared.created);
        assert_eq!(prepared.path, path);
        let mode = fs::metadata(&path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, WORKSPACE_MODE);
    }

    #[test]
    fn an_existing_workspace_is_reused_not_recreated() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("already-there");
        fs::create_dir(&path).expect("mkdir");
        fs::set_permissions(&path, fs::Permissions::from_mode(WORKSPACE_MODE)).expect("chmod");
        let prepared = prepare(&path.display().to_string()).expect("prepared");
        assert!(!prepared.created);
    }

    #[test]
    fn every_protected_root_is_refused() {
        let home = PathBuf::from(std::env::var("HOME").expect("HOME"));
        for root in REFUSED_ROOTS {
            let refused = prepare(root).expect_err("{root} must be refused");
            assert_eq!(refused.code(), "workspace_reserved", "{root}: {refused}");
            let inside = format!("{root}/sandbox");
            let refused = prepare(&inside).expect_err("a child must be refused too");
            assert_eq!(refused.code(), "workspace_reserved", "{inside}: {refused}");
        }
        // A directory that contains all of them is refused as well.
        let refused = prepare(&home.display().to_string()).expect_err("home must be refused");
        assert_eq!(refused.code(), "workspace_reserved");
        let refused = prepare("/").expect_err("root must be refused");
        assert_eq!(refused.code(), "workspace_reserved");
    }

    #[test]
    fn a_relative_path_a_parent_walk_and_a_file_are_refused() {
        assert_eq!(
            prepare("relative/path").expect_err("relative").code(),
            "workspace_not_absolute"
        );
        assert_eq!(
            prepare("/tmp/../etc").expect_err("dotdot").code(),
            "workspace_parent_traversal"
        );
        assert_eq!(prepare("   ").expect_err("blank").code(), "workspace_empty");

        let dir = tempfile::tempdir().expect("temp dir");
        let file = dir.path().join("a-file");
        fs::write(&file, b"x").expect("write");
        assert_eq!(
            prepare(&file.display().to_string())
                .expect_err("file")
                .code(),
            "workspace_not_a_directory"
        );
    }

    #[test]
    fn a_world_writable_existing_directory_is_refused() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("wide");
        fs::create_dir(&path).expect("mkdir");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o777)).expect("chmod");
        let refused = prepare(&path.display().to_string()).expect_err("world writable");
        assert_eq!(refused.code(), "workspace_world_writable");
    }

    #[test]
    fn a_dotdot_is_caught_even_when_it_would_land_outside_a_root() {
        let refused = prepare("/tmp/ws/../ws").expect_err("dotdot");
        assert_eq!(refused.code(), "workspace_parent_traversal");
    }
}
