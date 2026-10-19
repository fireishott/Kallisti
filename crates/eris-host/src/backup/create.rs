//! Writing an archive.
//!
//! The order is the safety property: stage, hash, then write.
//!
//! 1. The database member is a `VACUUM INTO` snapshot, never a file copy
//!    ([`snapshot_database`]), and `PRAGMA quick_check` must answer `ok` on the
//!    snapshot before it enters the archive. A live WAL database under a read
//!    transaction produces a single-file, fully checkpointed copy with no
//!    `-wal`/`-shm` sidecars, which is the only kind of database copy that cannot
//!    be torn.
//! 2. Session segments are copied verbatim. A live `wal.jsonl` is copied whole
//!    when this process can take the session's `fs4` advisory lock, and only to
//!    its last complete line when the daemon holds it - and then the dropped byte
//!    count is recorded in the manifest as `wal_partial_bytes` rather than lost
//!    silently.
//! 3. Every staged file is hashed before any payload byte is written, so the
//!    manifest that goes at the front of the archive describes bytes that already
//!    exist.
//!
//! The archive is written to a temporary file in the destination directory and
//! `persist_noclobber`ed into place, so an interrupted backup never leaves a
//! plausible-looking archive at the destination.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::backup::BackupError;
use crate::backup::format::{
    DB_PATH, ENV_PATH, Excluded, FIRST_RUN_PATH, FORMAT_VERSION, KIND_DB, KIND_ENV,
    KIND_SESSION_SEGMENT, KIND_SESSION_WAL, KIND_TOML, KIND_WORKSPACE, MAGIC, MAX_MANIFEST_BYTES,
    Manifest, Member, TOML_PATH, ZSTD_LEVEL, sha256_hex,
};
use crate::config::{Paths, TomlConfig};
use crate::db::BUSY_TIMEOUT;

/// What to back up, and where to put the result.
#[derive(Debug, Clone)]
pub struct CreateRequest {
    /// Absolute path of the archive to write. Must not exist.
    pub out: PathBuf,
    /// Include the sandbox workspace. Off by default: unbounded user data.
    pub include_workspace: bool,
    /// Re-read the archive after writing and verify every member.
    pub verify: bool,
    /// Allow a destination under the system temporary directory.
    pub allow_tmp: bool,
    /// The roots being backed up.
    pub paths: Paths,
}

/// What a successful backup did.
#[derive(Debug, Clone)]
pub struct CreateReport {
    /// The archive that was written.
    pub archive: PathBuf,
    /// Lowercase hex sha256 of the whole archive file.
    pub archive_sha256: String,
    /// Size of the archive file.
    pub archive_bytes: u64,
    /// Members, in payload order.
    pub members: Vec<Member>,
    /// What was left out, and why.
    pub excluded: Vec<Excluded>,
    /// Bytes dropped from the tail of a live `wal.jsonl` another writer held.
    pub wal_partial_bytes: u64,
    /// `schema_migrations` version inside the archived database.
    pub schema_version: i64,
    /// The binary that wrote it.
    pub eris_version: String,
    /// RFC 3339 UTC.
    pub created_at: String,
    /// Whether `--verify` re-read and checked every member.
    pub verified: bool,
}

impl CreateReport {
    /// The one JSON object `--json` prints.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "ok": true,
            "archive": self.archive.display().to_string(),
            "archive_bytes": self.archive_bytes,
            "archive_sha256": self.archive_sha256,
            "eris_version": self.eris_version,
            "schema_version": self.schema_version,
            "created_at": self.created_at,
            "member_count": self.members.len(),
            "members": self.members.iter().map(Member::to_json).collect::<Vec<_>>(),
            "excluded": self.excluded.iter().map(Excluded::to_json).collect::<Vec<_>>(),
            "wal_partial_bytes": self.wal_partial_bytes,
            "verified": self.verified,
        })
    }
}

/// One staged file, its manifest entry, and where it waits on disk.
struct Staged {
    member: Member,
    source: PathBuf,
}

/// The collection pass.
struct Collected {
    entries: Vec<Staged>,
    excluded: Vec<Excluded>,
    wal_partial_bytes: u64,
}

/// Back up the state under `request.paths` into `request.out`.
pub fn create(request: &CreateRequest) -> Result<CreateReport, BackupError> {
    let paths = &request.paths;
    check_destination(&request.out, request.allow_tmp, paths)?;
    let parent = request
        .out
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| BackupError::Usage {
            message: format!("{} has no parent directory", request.out.display()),
        })?;

    let staging = tempfile::Builder::new()
        .prefix("kallisti-backup-stage-")
        .tempdir()
        .map_err(|source| BackupError::io(std::env::temp_dir(), source))?;

    let collected = collect(paths, staging.path(), request.include_workspace)?;
    let schema_version = snapshot_schema_version(&staging.path().join("state/eris.db"))?;

    let mut manifest = Manifest {
        format_version: FORMAT_VERSION,
        schema_version,
        eris_version: crate::admin::VERSION.to_owned(),
        created_at: crate::auth::format_timestamp(crate::auth::now()),
        host: hostname(),
        members: collected
            .entries
            .iter()
            .map(|entry| entry.member.clone())
            .collect(),
        excluded: collected.excluded.clone(),
        wal_partial_bytes: collected.wal_partial_bytes,
        manifest_sha256: String::new(),
    };
    manifest.seal();
    let manifest_json = serde_json::to_vec(&manifest).map_err(|error| BackupError::Manifest {
        message: format!("cannot serialise the manifest: {error}"),
    })?;
    if manifest_json.len() > MAX_MANIFEST_BYTES as usize {
        return Err(BackupError::ManifestTooLarge {
            found: u32::try_from(manifest_json.len()).unwrap_or(u32::MAX),
            max: MAX_MANIFEST_BYTES,
        });
    }

    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|source| BackupError::io(parent, source))?;
    {
        let file = temporary.as_file_mut();
        file.write_all(MAGIC)
            .map_err(|source| BackupError::io(&request.out, source))?;
        let length =
            u32::try_from(manifest_json.len()).map_err(|_| BackupError::ManifestTooLarge {
                found: u32::MAX,
                max: MAX_MANIFEST_BYTES,
            })?;
        file.write_all(&length.to_le_bytes())
            .map_err(|source| BackupError::io(&request.out, source))?;
        file.write_all(&manifest_json)
            .map_err(|source| BackupError::io(&request.out, source))?;
        file.flush()
            .map_err(|source| BackupError::io(&request.out, source))?;
    }
    {
        let file = temporary.as_file_mut();
        let mut encoder = zstd::stream::write::Encoder::new(file, ZSTD_LEVEL)
            .map_err(|source| BackupError::io(&request.out, source))?;
        for entry in &collected.entries {
            let mut input = File::open(&entry.source)
                .map_err(|source| BackupError::io(&entry.source, source))?;
            let copied = std::io::copy(&mut input, &mut encoder)
                .map_err(|source| BackupError::io(&entry.source, source))?;
            if copied != entry.member.bytes {
                return Err(BackupError::Payload {
                    message: format!(
                        "{} changed while it was being staged: {} bytes expected, {copied} copied",
                        entry.member.path, entry.member.bytes
                    ),
                });
            }
        }
        encoder
            .finish()
            .map_err(|source| BackupError::io(&request.out, source))?;
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|source| BackupError::io(&request.out, source))?;

    let out = &request.out;
    match temporary.persist_noclobber(out) {
        Ok(_) => {}
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(BackupError::ExistingDestination {
                path: out.to_path_buf(),
            });
        }
        Err(error) => return Err(BackupError::io(out, error.error)),
    }

    let (archive_bytes, archive_sha256) = hash_file(out)?;

    let mut report = CreateReport {
        archive: out.clone(),
        archive_sha256,
        archive_bytes,
        members: manifest.members.clone(),
        excluded: manifest.excluded.clone(),
        wal_partial_bytes: manifest.wal_partial_bytes,
        schema_version: manifest.schema_version,
        eris_version: manifest.eris_version.clone(),
        created_at: manifest.created_at.clone(),
        verified: false,
    };

    if request.verify {
        // A backup that cannot be read back is not a backup. If the self-check
        // fails, the file is removed so nobody mistakes it for one.
        let opened = crate::backup::verify::open(out)?;
        match crate::backup::verify::verify(&opened) {
            Ok(_) => report.verified = true,
            Err(error) => {
                let _ = fs::remove_file(out);
                return Err(error);
            }
        }
    }

    Ok(report)
}

/// Refuse a destination that cannot describe a run.
fn check_destination(out: &Path, allow_tmp: bool, paths: &Paths) -> Result<(), BackupError> {
    if !out.is_absolute() {
        return Err(BackupError::Usage {
            message: format!("--out must be an absolute path, got {}", out.display()),
        });
    }
    if out.is_dir() {
        return Err(BackupError::Usage {
            message: format!("{} is a directory, not an archive path", out.display()),
        });
    }
    let parent = out.parent().ok_or_else(|| BackupError::Usage {
        message: format!("{} has no parent directory", out.display()),
    })?;
    if !parent.is_dir() {
        return Err(BackupError::Usage {
            message: format!(
                "the directory {} does not exist; create it first",
                parent.display()
            ),
        });
    }
    if out.exists() {
        return Err(BackupError::ExistingDestination {
            path: out.to_path_buf(),
        });
    }
    if !allow_tmp && is_under_temp(out) && !is_scratch_root(paths) {
        return Err(BackupError::Usage {
            message: format!(
                "{} is under the system temporary directory, where a backup goes to be lost; choose a home directory path or pass --allow-tmp",
                out.display()
            ),
        });
    }
    Ok(())
}

/// Whether a path is inside `/tmp` or `$TMPDIR`.
fn is_under_temp(out: &Path) -> bool {
    let mut roots = vec![PathBuf::from("/tmp")];
    let temporary = std::env::temp_dir();
    if !roots.contains(&temporary) {
        roots.push(temporary);
    }
    roots.iter().any(|root| out.starts_with(root))
}

/// Whether these paths are a scratch root rather than the installed state.
///
/// The `/tmp` refusal protects the installed daemon's backups. A `--state-dir`
/// run is by definition a scratch or acceptance run against throwaway state, so
/// it is allowed to write its archive anywhere. Without `$HOME` there is no
/// installed state to defend either.
fn is_scratch_root(paths: &Paths) -> bool {
    match Paths::from_home() {
        Ok(home) => home.state_dir != paths.state_dir || home.config_dir != paths.config_dir,
        Err(_) => true,
    }
}

/// Stage every member and hash it.
fn collect(
    paths: &Paths,
    staging: &Path,
    include_workspace: bool,
) -> Result<Collected, BackupError> {
    let staged_state = staging.join("state");
    let staged_config = staging.join("config");
    for directory in [&staged_state, &staged_config] {
        fs::create_dir_all(directory).map_err(|source| BackupError::io(directory, source))?;
    }

    let mut entries = Vec::new();
    let mut excluded = Vec::new();
    let mut wal_partial_bytes = 0u64;

    // 1. The database: a consistent snapshot, or nothing at all.
    let snapshot = staged_state.join("eris.db");
    snapshot_database(&paths.db_file, &snapshot)?;
    let (bytes, sha256) = hash_file(&snapshot)?;
    entries.push(Staged {
        member: Member {
            path: DB_PATH.to_owned(),
            kind: KIND_DB.to_owned(),
            bytes,
            sha256,
        },
        source: snapshot,
    });

    // 1b. The sidecars are never copied, and the manifest says why.
    for (name, reason) in [
        (
            "eris.db-wal",
            "SQLite's write-ahead log: the archived database is a VACUUM INTO snapshot, so copying this could only tear it",
        ),
        (
            "eris.db-shm",
            "SQLite's shared-memory index: recreated by the next open, never archived",
        ),
    ] {
        let path = paths.state_dir.join(name);
        if path.exists() {
            excluded.push(Excluded {
                path: format!("state/{name}"),
                reason: reason.to_owned(),
            });
        }
    }

    // 2. Session logs, copied under the session lock where there is one.
    let sessions = paths.state_dir.join("sessions");
    if sessions.is_dir() {
        for file in walk_files(&sessions)? {
            let relative = file.strip_prefix(&paths.state_dir).unwrap_or(&file);
            let member_path = member_path_for(relative);
            let name = file
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            if name == "wal.jsonl" {
                let directory = file.parent().unwrap_or(&sessions);
                let locked_by_other = wal_is_locked_by_another_writer(directory);
                let destination = staging.join(&member_path);
                create_parent(&destination)?;
                let (bytes, sha256, dropped) = stage_wal(&file, &destination, locked_by_other)?;
                wal_partial_bytes += dropped;
                entries.push(Staged {
                    member: Member {
                        path: member_path,
                        kind: KIND_SESSION_WAL.to_owned(),
                        bytes,
                        sha256,
                    },
                    source: destination,
                });
                continue;
            }
            if !name.ends_with(".jsonl.zst") {
                // `.lock` and anything else in the directory is recreated by the
                // next open. It is not state, so it does not travel.
                excluded.push(Excluded {
                    path: member_path,
                    reason: "recreated when the session is next opened, not state worth archiving"
                        .to_owned(),
                });
                continue;
            }
            let destination = staging.join(&member_path);
            create_parent(&destination)?;
            let (bytes, sha256) = copy_hashed(&file, &destination)?;
            entries.push(Staged {
                member: Member {
                    path: member_path,
                    kind: KIND_SESSION_SEGMENT.to_owned(),
                    bytes,
                    sha256,
                },
                source: destination,
            });
        }
    }

    // 3. Config: the secret half is a reference file, the other half is settings.
    for (file, member_path, kind) in [
        (&paths.env_file, ENV_PATH, KIND_ENV),
        (&paths.toml_file, TOML_PATH, KIND_TOML),
    ] {
        if !file.is_file() {
            continue;
        }
        let destination = staging.join(member_path);
        create_parent(&destination)?;
        let (bytes, sha256) = copy_hashed(file, &destination)?;
        entries.push(Staged {
            member: Member {
                path: member_path.to_owned(),
                kind: kind.to_owned(),
                bytes,
                sha256,
            },
            source: destination,
        });
    }

    // 4. The setup passphrase file. This exclusion is not conditional on any
    //    flag, and it is the same line for every code path that builds a backup.
    if paths.config_dir.join("first-run.txt").exists() {
        excluded.push(Excluded {
            path: FIRST_RUN_PATH.to_owned(),
            reason: "holds the setup passphrase, which is the admin credential once onboarding is ready: excluded unconditionally"
                .to_owned(),
        });
    }

    // 5. The workspace is opt-in: it is the user's files, not Eris state.
    let workspace = TomlConfig::load(&paths.toml_file)
        .map_err(|error| BackupError::Config {
            message: error.to_string(),
        })?
        .workspace_path();
    if include_workspace {
        if workspace.is_dir() {
            for file in walk_files(&workspace)? {
                let relative = file.strip_prefix(&workspace).unwrap_or(&file);
                let member_path = format!("workspace/{}", slash_path(relative));
                let destination = staging.join(&member_path);
                create_parent(&destination)?;
                let (bytes, sha256) = copy_hashed(&file, &destination)?;
                entries.push(Staged {
                    member: Member {
                        path: member_path,
                        kind: KIND_WORKSPACE.to_owned(),
                        bytes,
                        sha256,
                    },
                    source: destination,
                });
            }
        }
    } else if workspace.is_dir() {
        excluded.push(Excluded {
            path: "workspace/.not-included".to_owned(),
            reason: "the sandbox workspace is user data of unbounded size and is opt-in with --include-workspace"
                .to_owned(),
        });
    }

    Ok(Collected {
        entries,
        excluded,
        wal_partial_bytes,
    })
}

/// The manifest path for a file relative to the state directory.
fn member_path_for(relative: &Path) -> String {
    format!("state/{}", slash_path(relative))
}

/// A path rendered with `/` separators, so the manifest is host-independent.
fn slash_path(path: &Path) -> String {
    let mut text = String::new();
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            if !text.is_empty() {
                text.push('/');
            }
            text.push_str(&name.to_string_lossy());
        }
    }
    text
}

/// Every regular file under `root`, sorted, so the manifest is reproducible.
fn walk_files(root: &Path) -> Result<Vec<PathBuf>, BackupError> {
    fn walk(directory: &Path, out: &mut Vec<PathBuf>) -> Result<(), BackupError> {
        let entries =
            fs::read_dir(directory).map_err(|source| BackupError::io(directory, source))?;
        let mut names: Vec<PathBuf> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| BackupError::io(directory, source))?;
            names.push(entry.path());
        }
        names.sort();
        for path in names {
            let metadata =
                fs::symlink_metadata(&path).map_err(|source| BackupError::io(&path, source))?;
            if metadata.file_type().is_symlink() {
                // A symlink is not state: never follow one out of the root.
                continue;
            }
            if metadata.is_dir() {
                walk(&path, out)?;
            } else if metadata.is_file() {
                out.push(path);
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    if root.is_dir() {
        walk(root, &mut files)?;
    }
    Ok(files)
}

/// The consistent database copy: `VACUUM INTO`, then `quick_check`.
fn snapshot_database(source: &Path, destination: &Path) -> Result<(), BackupError> {
    if !source.is_file() {
        return Err(BackupError::NoState {
            path: source.to_path_buf(),
        });
    }
    let wal_existed = sidecar(source, "-wal").exists();

    // Read-only first: a backup should not need write access to the live state.
    // A WAL database without its `-shm` cannot always be opened read-only, so a
    // failure falls back to a read-write handle, which reads the same pages.
    let read_only = snapshot_with(source, destination, OpenFlags::SQLITE_OPEN_READ_ONLY);
    if read_only.is_err() {
        let _ = fs::remove_file(destination);
        snapshot_with(source, destination, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    }

    // The sidecars belong to the live database, not to this snapshot. If opening
    // for the snapshot created them where there were none, remove them again.
    if !wal_existed {
        for suffix in ["-wal", "-shm"] {
            let path = sidecar(source, suffix);
            if path.exists() {
                let _ = fs::remove_file(&path);
            }
        }
    }

    if !destination.is_file() {
        return Err(BackupError::Db {
            message: format!("VACUUM INTO produced no file at {}", destination.display()),
        });
    }

    let result = quick_check(destination)?;
    if result != "ok" {
        return Err(BackupError::QuickCheck {
            path: destination.to_path_buf(),
            result,
        });
    }
    Ok(())
}

/// One `VACUUM INTO` attempt with explicit open flags.
fn snapshot_with(source: &Path, destination: &Path, flags: OpenFlags) -> Result<(), BackupError> {
    let connection =
        Connection::open_with_flags(source, flags).map_err(|error| BackupError::Db {
            message: format!("cannot open {} for a snapshot: {error}", source.display()),
        })?;
    connection
        .busy_timeout(BUSY_TIMEOUT)
        .map_err(|error| BackupError::Db {
            message: format!("cannot set a busy timeout: {error}"),
        })?;
    let text = destination.to_string_lossy().into_owned();
    match connection.execute("VACUUM INTO ?1", [text.as_str()]) {
        Ok(_) | Err(rusqlite::Error::ExecuteReturnedResults) => Ok(()),
        Err(error) => Err(BackupError::Db {
            message: format!("VACUUM INTO {} failed: {error}", destination.display()),
        }),
    }
}

/// `PRAGMA quick_check` on a database file, read-only.
fn quick_check(path: &Path) -> Result<String, BackupError> {
    let connection =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|error| {
            BackupError::Db {
                message: format!("cannot open {} to check it: {error}", path.display()),
            }
        })?;
    let result: String = connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .map_err(|error| BackupError::Db {
            message: format!("PRAGMA quick_check on {} failed: {error}", path.display()),
        })?;
    Ok(result)
}

/// The `schema_migrations` version inside a snapshot.
fn snapshot_schema_version(path: &Path) -> Result<i64, BackupError> {
    let connection =
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|error| {
            BackupError::Db {
                message: format!("cannot open {}: {error}", path.display()),
            }
        })?;
    connection
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .map_err(|error| BackupError::Db {
            message: format!(
                "cannot read the schema version of {}: {error}",
                path.display()
            ),
        })
}

/// `path` with `suffix` appended to the file name (`eris.db` -> `eris.db-wal`).
fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.push_str(suffix);
    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

/// Whether another writer holds the session's advisory lock.
///
/// The lock file is opened read-only and never created here: a missing `.lock`
/// means no writer has opened the session, which is exactly "not locked". `fs4`
/// is used through a fully qualified call because std 1.99 added an inherent
/// `File::try_lock`, and the lock that must be taken is the `flock` one the log
/// writer takes.
fn wal_is_locked_by_another_writer(directory: &Path) -> bool {
    let lock = directory.join(".lock");
    let Ok(file) = File::open(&lock) else {
        return false;
    };
    match <File as fs4::FileExt>::try_lock(&file) {
        Ok(()) => {
            let _ = <File as fs4::FileExt>::unlock(&file);
            false
        }
        Err(_) => true,
    }
}

/// Copy a live `wal.jsonl`.
///
/// Whole file when this process holds the lock, so the copy cannot be split
/// across an append. Only the complete-line prefix when the daemon holds it, with
/// the dropped byte count returned so the manifest records it.
fn stage_wal(
    source: &Path,
    destination: &Path,
    locked_by_other: bool,
) -> Result<(u64, String, u64), BackupError> {
    let bytes = fs::read(source).map_err(|error| BackupError::io(source, error))?;
    let keep = if locked_by_other {
        match bytes.iter().rposition(|byte| *byte == b'\n') {
            Some(last) => last + 1,
            None => 0,
        }
    } else {
        bytes.len()
    };
    let dropped = bytes.len() - keep;
    let mut file =
        File::create(destination).map_err(|error| BackupError::io(destination, error))?;
    file.write_all(&bytes[..keep])
        .map_err(|error| BackupError::io(destination, error))?;
    file.sync_all()
        .map_err(|error| BackupError::io(destination, error))?;
    Ok((keep as u64, sha256_hex(&bytes[..keep]), dropped as u64))
}

/// Copy a file, returning its length and digest.
fn copy_hashed(source: &Path, destination: &Path) -> Result<(u64, String), BackupError> {
    let mut input = File::open(source).map_err(|error| BackupError::io(source, error))?;
    let mut output =
        File::create(destination).map_err(|error| BackupError::io(destination, error))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|error| BackupError::io(source, error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        output
            .write_all(&buffer[..read])
            .map_err(|error| BackupError::io(destination, error))?;
        total += read as u64;
    }
    output
        .sync_all()
        .map_err(|error| BackupError::io(destination, error))?;
    Ok((total, hex::encode(hasher.finalize())))
}

/// Hash a whole file.
pub(crate) fn hash_file(path: &Path) -> Result<(u64, String), BackupError> {
    let mut input = File::open(path).map_err(|error| BackupError::io(path, error))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|error| BackupError::io(path, error))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok((total, hex::encode(hasher.finalize())))
}

/// Create the parent directory of a staged member.
fn create_parent(path: &Path) -> Result<(), BackupError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| BackupError::io(parent, error))?;
    }
    Ok(())
}

/// The host name, informational only.
fn hostname() -> String {
    fs::read_to_string("/etc/hostname")
        .map(|text| text.trim().to_owned())
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}
