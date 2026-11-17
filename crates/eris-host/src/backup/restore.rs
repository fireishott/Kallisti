//! Restoring an archive, in the only order that cannot lose state.
//!
//! 1. Open the archive: magic, manifest, `manifest_sha256`, and the path and kind
//!    validation in [`crate::backup::Manifest::validate`].
//! 2. Verify every member's sha256 into a sink. Nothing is on disk yet, so a
//!    tampered archive writes nothing and exits 3.
//! 3. Refuse a manifest whose `schema_version` is newer than this binary's
//!    [`crate::db::SCHEMA_VERSION`]. Older is fine (migrations run forward); newer
//!    means the archive came from a future Eris.
//! 4. Take a safety copy: existing state moves to
//!    `pre-restore-<utc-stamp>/` before anything replaces it.
//! 5. Extract into a staging directory inside the state directory, so the final
//!    placement is a rename on the same filesystem, not a cross-device copy.
//! 6. Write the `audit_log` row into the staged database and `PRAGMA quick_check`
//!    the staged file. The row goes in before the rename, so the placed file is
//!    exactly the file the report names, and the row exists if and only if the
//!    restore completed.
//! 7. Place the files with `eris.db` **last**, so a crash cannot leave a database
//!    referring to session files that are not there.
//! 8. Re-apply the documented modes: `eris.env` 0600, `config.toml` 0644, state
//!    and config directories 0700.
//!
//! `--dry-run` stops after step 3, plus a `quick_check` of the archived database
//! in a throwaway directory, and writes nothing.

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};

use crate::backup::format::{
    DB_PATH, KIND_DB, KIND_ENV, KIND_SESSION_SEGMENT, KIND_SESSION_WAL, KIND_TOML, KIND_WORKSPACE,
    Member, MemberRoot, member_root,
};
use crate::backup::{BackupError, verify};
use crate::config::{Paths, TomlConfig};
use crate::db::BUSY_TIMEOUT;

/// A deliberate stopping point, used to prove what a crash does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    /// After the staged database passes `quick_check`, before anything is placed.
    AfterQuickCheck,
}

impl FaultPoint {
    /// The name used in the error and in the report.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AfterQuickCheck => "after-quick-check",
        }
    }
}

/// What to restore, and where.
#[derive(Debug, Clone)]
pub struct RestoreRequest {
    /// The archive to read.
    pub from: PathBuf,
    /// Replace existing state. Required whenever the target is not empty.
    pub force: bool,
    /// Verify and report, but write nothing.
    pub dry_run: bool,
    /// The roots being restored into.
    pub paths: Paths,
}

/// What a successful restore did.
#[derive(Debug, Clone)]
pub struct RestoreReport {
    /// The archive that was read.
    pub archive: PathBuf,
    /// Lowercase hex sha256 of the archive file.
    pub archive_sha256: String,
    /// Members present in the archive.
    pub members: usize,
    /// Uncompressed payload bytes verified.
    pub bytes: u64,
    /// `schema_migrations` version the archive records.
    pub schema_version: i64,
    /// The binary that wrote the archive.
    pub eris_version: String,
    /// When the archive was written, RFC 3339 UTC.
    pub created_at: String,
    /// Where the replaced state was moved, when there was any.
    pub safety_copy: Option<PathBuf>,
    /// True when this was a `--dry-run`.
    pub dry_run: bool,
    /// Members this run placed, in placement order (empty for a dry run).
    pub placed: Vec<String>,
    /// What a dry run would place.
    pub would_write: Vec<String>,
    /// Lowercase hex sha256 of the placed database.
    pub db_sha256: Option<String>,
    /// The `audit_log` row this restore wrote, when it wrote one.
    pub audit_row_id: Option<i64>,
    /// Bytes the manifest says were dropped from a live WAL.
    pub wal_partial_bytes: u64,
    /// `PRAGMA quick_check` on the database that was placed (or checked).
    pub quick_check: String,
}

impl RestoreReport {
    /// The one JSON object `--json` prints.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "ok": true,
            "archive": self.archive.display().to_string(),
            "archive_sha256": self.archive_sha256,
            "members": self.members,
            "bytes": self.bytes,
            "schema_version": self.schema_version,
            "eris_version": self.eris_version,
            "created_at": self.created_at,
            "safety_copy": self.safety_copy.as_ref().map(|path| path.display().to_string()),
            "dry_run": self.dry_run,
            "placed": self.placed,
            "would_write": self.would_write,
            "db_sha256": self.db_sha256,
            "audit_row_id": self.audit_row_id,
            "wal_partial_bytes": self.wal_partial_bytes,
            "quick_check": self.quick_check,
        })
    }
}

/// Restore an archive into `request.paths`.
pub fn restore(request: &RestoreRequest) -> Result<RestoreReport, BackupError> {
    restore_with_fault(request, None)
}

/// Restore, optionally stopping at a [`FaultPoint`] to model a crash.
pub fn restore_with_fault(
    request: &RestoreRequest,
    fault: Option<FaultPoint>,
) -> Result<RestoreReport, BackupError> {
    let paths = &request.paths;

    // 1. The manifest must be trustworthy before anything reads the payload.
    let opened = verify::open(&request.from)?;

    // 2. Every member, into a sink. This is the write-nothing verification pass.
    let verified = verify::verify(&opened)?;

    // 3. Forward incompatibility. Migrations run forward, not back.
    let supported = crate::db::SCHEMA_VERSION;
    if opened.manifest.schema_version > supported {
        return Err(BackupError::NewerSchema {
            found: opened.manifest.schema_version,
            supported,
        });
    }

    let existing = state_present(paths);
    if existing && !request.force && !request.dry_run {
        return Err(BackupError::NeedsForce {
            state_dir: paths.state_dir.clone(),
        });
    }

    if request.dry_run {
        // Check the archived database in a throwaway directory. Nothing under the
        // target roots is created, moved, or rewritten.
        let scratch = tempfile::Builder::new()
            .prefix("kallisti-restore-check-")
            .tempdir()
            .map_err(|source| BackupError::io(std::env::temp_dir(), source))?;
        let bytes = verify::read_member_bytes(&opened, DB_PATH)?;
        let candidate = scratch.path().join("eris.db");
        fs::write(&candidate, &bytes).map_err(|error| BackupError::io(&candidate, error))?;
        let result = quick_check(&candidate)?;
        if result != "ok" {
            return Err(BackupError::QuickCheck {
                path: request.from.clone(),
                result,
            });
        }
        return Ok(RestoreReport {
            archive: request.from.clone(),
            archive_sha256: opened.archive_sha256.clone(),
            members: verified.members,
            bytes: verified.bytes,
            schema_version: opened.manifest.schema_version,
            eris_version: opened.manifest.eris_version.clone(),
            created_at: opened.manifest.created_at.clone(),
            safety_copy: None,
            dry_run: true,
            placed: Vec::new(),
            would_write: opened
                .manifest
                .members
                .iter()
                .map(|member| member.path.clone())
                .collect(),
            db_sha256: None,
            audit_row_id: None,
            wal_partial_bytes: opened.manifest.wal_partial_bytes,
            quick_check: result,
        });
    }

    // 4. The safety copy, before anything replaces what is there.
    let safety_copy = if existing {
        take_safety_copy(paths)?
    } else {
        None
    };

    // 5. Extract inside the target filesystem, so placement is a rename.
    prepare_directory(&paths.state_dir, 0o700)?;
    let staging = tempfile::Builder::new()
        .prefix(".restore-staging-")
        .tempdir_in(&paths.state_dir)
        .map_err(|error| BackupError::io(&paths.state_dir, error))?;
    verify::extract(&opened, staging.path())?;

    // 6. The audit row, then quick_check on exactly the bytes that will land.
    let staged_db = staging.path().join("state/eris.db");
    if !staged_db.is_file() {
        return Err(BackupError::Manifest {
            message: format!("the archive has no {DB_PATH} member to place"),
        });
    }
    let audit_row_id = write_audit_row(
        &staged_db,
        &audit_detail(paths, &opened, safety_copy.as_deref()),
    )?;
    let result = quick_check(&staged_db)?;
    if result != "ok" {
        return Err(BackupError::QuickCheck {
            path: staged_db.clone(),
            result,
        });
    }
    let db_sha256 = crate::backup::create::hash_file(&staged_db)?.1;

    if let Some(point) = fault {
        return Err(BackupError::InjectedFault {
            point: point.as_str(),
        });
    }

    // 7. Placement. Sessions and config first; `eris.db` last.
    let placed = place_all(paths, staging.path(), &opened.manifest.members)?;

    // 8. Modes by rule, not by anything the archive carries.
    apply_permissions(paths)?;

    Ok(RestoreReport {
        archive: request.from.clone(),
        archive_sha256: opened.archive_sha256.clone(),
        members: verified.members,
        bytes: verified.bytes,
        schema_version: opened.manifest.schema_version,
        eris_version: opened.manifest.eris_version.clone(),
        created_at: opened.manifest.created_at.clone(),
        safety_copy,
        dry_run: false,
        placed,
        would_write: Vec::new(),
        db_sha256: Some(db_sha256),
        audit_row_id: Some(audit_row_id),
        wal_partial_bytes: opened.manifest.wal_partial_bytes,
        quick_check: result,
    })
}

/// Whether the target roots hold state a restore would replace.
fn state_present(paths: &Paths) -> bool {
    paths.db_file.exists()
        || paths.env_file.exists()
        || paths.toml_file.exists()
        || paths.state_dir.join("sessions").exists()
        || paths.state_dir.join("eris.db-wal").exists()
}

/// The `audit_log` detail string: where the state came from, never a secret.
fn audit_detail(paths: &Paths, opened: &verify::OpenedArchive, safety: Option<&Path>) -> String {
    let safety = safety.map_or_else(|| "none".to_owned(), |path| path.display().to_string());
    format!(
        "archive_sha256={} members={} safety_copy={} target={}",
        opened.archive_sha256,
        opened.manifest.members.len(),
        safety,
        paths.state_dir.display()
    )
}

/// Move the current state aside into `pre-restore-<utc-stamp>/`.
///
/// The database and both of its sidecars move together: a database without its
/// WAL is not the state that was there. `first-run.txt` stays where it is, because
/// it holds the operator's passphrase and a restore must not take that away.
fn take_safety_copy(paths: &Paths) -> Result<Option<PathBuf>, BackupError> {
    let mut directory = paths.state_dir.join(format!(
        "pre-restore-{}",
        crate::auth::format_timestamp(crate::auth::now()).replace(':', "-")
    ));
    let mut suffix = 2;
    while directory.exists() {
        directory = paths.state_dir.join(format!(
            "pre-restore-{}-{suffix}",
            crate::auth::format_timestamp(crate::auth::now()).replace(':', "-")
        ));
        suffix += 1;
        if suffix > 100 {
            break;
        }
    }
    prepare_directory(&paths.state_dir, 0o700)?;
    prepare_directory(&directory, 0o700)?;

    let mut moved = 0usize;
    for (name, inside) in [
        ("eris.db", "state/eris.db"),
        ("eris.db-wal", "state/eris.db-wal"),
        ("eris.db-shm", "state/eris.db-shm"),
        ("sessions", "state/sessions"),
    ] {
        let source = paths.state_dir.join(name);
        if source.exists() && move_file(&source, &directory.join(inside))? {
            moved += 1;
        }
    }
    for (name, inside) in [
        ("eris.env", "config/eris.env"),
        ("config.toml", "config/config.toml"),
    ] {
        let source = paths.config_dir.join(name);
        if source.exists() && move_file(&source, &directory.join(inside))? {
            moved += 1;
        }
    }

    if moved == 0 {
        // Nothing was there after all: no empty directory left behind.
        let _ = fs::remove_dir_all(&directory);
        return Ok(None);
    }
    Ok(Some(directory))
}

/// Place every member, config and sessions first, `eris.db` last.
fn place_all(
    paths: &Paths,
    staging: &Path,
    members: &[Member],
) -> Result<Vec<String>, BackupError> {
    let mut placed = Vec::new();

    for wanted in [KIND_SESSION_SEGMENT, KIND_SESSION_WAL, KIND_ENV, KIND_TOML] {
        for member in members.iter().filter(|member| member.kind == wanted) {
            place_one(paths, staging, member, None)?;
            placed.push(member.path.clone());
        }
    }

    // The workspace root comes from the config that was just placed, so this runs
    // after the config members.
    let workspace_members: Vec<&Member> = members
        .iter()
        .filter(|member| member.kind == KIND_WORKSPACE)
        .collect();
    if !workspace_members.is_empty() {
        let workspace = TomlConfig::load(&paths.toml_file)
            .map_err(|error| BackupError::Config {
                message: error.to_string(),
            })?
            .workspace_path();
        for member in workspace_members {
            place_one(paths, staging, member, Some(&workspace))?;
            placed.push(member.path.clone());
        }
    }

    for member in members.iter().filter(|member| member.kind == KIND_DB) {
        place_one(paths, staging, member, None)?;
        placed.push(member.path.clone());
    }

    Ok(placed)
}

/// Move one staged member into place.
fn place_one(
    paths: &Paths,
    staging: &Path,
    member: &Member,
    workspace: Option<&Path>,
) -> Result<(), BackupError> {
    let (root, relative) = member_root(&member.path)?;
    let target_root = match root {
        MemberRoot::State => paths.state_dir.clone(),
        MemberRoot::Config => paths.config_dir.clone(),
        MemberRoot::Workspace => workspace
            .ok_or_else(|| BackupError::Manifest {
                message: format!(
                    "member {} is a workspace file but no workspace root was resolved",
                    member.path
                ),
            })?
            .to_path_buf(),
    };
    let target = target_root.join(&relative);
    if !target.starts_with(&target_root) {
        return Err(BackupError::MemberPath {
            path: member.path.clone(),
            reason: "the member path resolves outside its root",
        });
    }
    if let Some(parent) = target.parent() {
        prepare_directory(parent, 0o700)?;
    }
    let source = staging.join(&member.path);
    if !move_file(&source, &target)? {
        return Err(BackupError::Io {
            path: source,
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the staged member disappeared before it was placed",
            ),
        });
    }
    Ok(())
}

/// Rename, falling back to a copy when the two paths are on different devices.
fn move_file(source: &Path, target: &Path) -> Result<bool, BackupError> {
    if !source.exists() {
        return Ok(false);
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|error| BackupError::io(parent, error))?;
    }
    match fs::rename(source, target) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
            fs::copy(source, target).map_err(|error| BackupError::io(target, error))?;
            fs::remove_file(source).map_err(|error| BackupError::io(source, error))?;
            Ok(true)
        }
        Err(error) => Err(BackupError::io(target, error)),
    }
}

/// Create a directory and set its mode.
fn prepare_directory(path: &Path, mode: u32) -> Result<(), BackupError> {
    fs::create_dir_all(path).map_err(|error| BackupError::io(path, error))?;
    set_mode(path, mode)
}

/// Set permission bits, unix only.
fn set_mode(path: &Path, mode: u32) -> Result<(), BackupError> {
    if !path.exists() {
        return Ok(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .map_err(|error| BackupError::io(path, error))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

/// Re-apply the documented modes to everything a restore placed.
fn apply_permissions(paths: &Paths) -> Result<(), BackupError> {
    set_mode(&paths.state_dir, 0o700)?;
    set_mode(&paths.config_dir, 0o700)?;
    set_mode(&paths.db_file, 0o600)?;
    set_mode(&paths.env_file, 0o600)?;
    set_mode(&paths.toml_file, 0o644)?;

    let sessions = paths.state_dir.join("sessions");
    if sessions.is_dir() {
        set_mode(&sessions, 0o700)?;
        for entry in fs::read_dir(&sessions).map_err(|error| BackupError::io(&sessions, error))? {
            let entry = entry.map_err(|error| BackupError::io(&sessions, error))?;
            let path = entry.path();
            if path.is_dir() {
                set_mode(&path, 0o700)?;
                for file in fs::read_dir(&path).map_err(|error| BackupError::io(&path, error))? {
                    let file = file.map_err(|error| BackupError::io(&path, error))?;
                    if file.path().is_file() {
                        set_mode(&file.path(), 0o600)?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Insert the restore's `audit_log` row into the staged database.
///
/// The row is written before the rename so the placed file is exactly the file
/// the report names, and so the row exists if and only if the restore completed.
/// The connection is closed with a truncated WAL and a rollback journal, so no
/// `-wal` sidecar is left for the placement step to lose.
fn write_audit_row(database: &Path, detail: &str) -> Result<i64, BackupError> {
    let connection = Connection::open(database).map_err(|error| BackupError::Db {
        message: format!(
            "cannot open the staged database {}: {error}",
            database.display()
        ),
    })?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| BackupError::Db {
            message: format!("cannot set a busy timeout: {error}"),
        })?;
    connection
        .execute(
            "INSERT INTO audit_log (actor, action, detail) VALUES ('restore', 'backup.restore', ?1)",
            [detail],
        )
        .map_err(|error| BackupError::Db {
            message: format!("cannot write the restore audit row: {error}"),
        })?;
    let row = connection.last_insert_rowid();
    // Fold the WAL back into the file and switch to a rollback journal, so the
    // single .db file carries the row and nothing is lost with a sidecar.
    let _ = connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
        row.get::<_, i64>(0)
    });
    let _ = connection.query_row("PRAGMA journal_mode=DELETE", [], |row| {
        row.get::<_, String>(0)
    });
    drop(connection);
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut name = database
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        name.push_str(suffix);
        let sidecar = database.with_file_name(name);
        if sidecar.exists() {
            let _ = fs::remove_file(&sidecar);
        }
    }
    Ok(row)
}

/// `PRAGMA quick_check` on a database file, read-only.
fn quick_check(path: &Path) -> Result<String, BackupError> {
    let connection =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|error| {
            BackupError::Db {
                message: format!("cannot open {} to check it: {error}", path.display()),
            }
        })?;
    connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|error| BackupError::Db {
            message: format!("PRAGMA quick_check on {} failed: {error}", path.display()),
        })
}
