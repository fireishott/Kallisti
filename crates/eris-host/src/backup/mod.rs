//! Backup and restore of all Kallisti 1.0 state.
//!
//! State lives in exactly three allowlisted roots, plus opt-in user files:
//!
//! | Path | Contents | Secret? |
//! |---|---|---|
//! | `~/.local/share/kallisti/eris.db` | every table, Argon2id verifiers only | no |
//! | `~/.local/share/kallisti/sessions/` | `wal.jsonl` plus sealed `.jsonl.zst` | no |
//! | `~/.config/kallisti/eris.env` | key *references* (`env:NAME`), mode 0600 | no |
//! | `~/.config/kallisti/config.toml` | workspace path and log level | no |
//!
//! Three rules make that list closed rather than a starting point:
//!
//! - `eris.db-wal` and `eris.db-shm` are **never** copied. `cp eris.db` on a live
//!   WAL database produces a torn file that SQLite may still open, which is worse
//!   than a loud failure; the database member is a `VACUUM INTO` snapshot instead
//!   ([`create`]), and it is `PRAGMA quick_check`ed before it enters the archive.
//! - `first-run.txt` holds the setup passphrase. It is excluded unconditionally,
//!   on the same code path for every flag combination, because once onboarding is
//!   `ready` that passphrase *is* the admin credential: an archive containing it
//!   is a plaintext admin credential on disk.
//! - `~/kallisti-workspace/` is user data of unbounded size. It is opt-in with
//!   `--include-workspace` and its absence is recorded in the manifest.
//!
//! Restore is destructive by definition, so it verifies first and writes second:
//! magic, manifest digest, every member's sha256 into a sink (nothing on disk),
//! the newer-schema refusal, then a safety copy of the existing state, then
//! extraction into a staging directory on the target filesystem, then
//! `quick_check`, then placement with `eris.db` last. A tampered archive writes
//! nothing and exits [`EXIT_VERIFICATION`].
//!
//! Exit codes are the CLI contract: 0 ok, 1 failure, 2 usage, 3 verification
//! failure. A script can therefore tell a tampered archive from a disk error.
//!
//! There is no scheduler here and no network call. Automatic backups are a later
//! change; `cron` and systemd timers are off the allowlist for this wave.

use std::path::PathBuf;

mod create;
mod format;
mod restore;
mod verify;

pub use create::{CreateReport, CreateRequest, create};
pub use format::{
    DB_PATH, ENV_PATH, Excluded, FIRST_RUN_PATH, FORMAT_VERSION, KIND_DB, KIND_ENV,
    KIND_SESSION_SEGMENT, KIND_SESSION_WAL, KIND_TOML, KIND_WORKSPACE, MAGIC, MAX_MANIFEST_BYTES,
    Manifest, Member, MemberRoot, TOML_PATH, ZSTD_LEVEL, is_sha256_hex, member_root, sha256_hex,
};
pub use restore::{FaultPoint, RestoreReport, RestoreRequest, restore, restore_with_fault};
pub use verify::{OpenedArchive, VerifyReport, open, verify};

/// Success.
pub const EXIT_OK: u8 = 0;
/// Failure: I/O, a refusal, a database problem.
pub const EXIT_FAILURE: u8 = 1;
/// Usage: the arguments cannot describe a run.
pub const EXIT_USAGE: u8 = 2;
/// Verification failure: the archive is corrupt or was tampered with.
pub const EXIT_VERIFICATION: u8 = 3;

/// Everything backup and restore can refuse.
///
/// Messages name paths, member names, and archive digests. They never carry a
/// member's content, a passphrase, or a key value.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// A filesystem operation failed.
    #[error("{path}: {source}")]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },
    /// A config file could not be loaded well enough to name the workspace.
    #[error("cannot read the config: {message}")]
    Config {
        /// Why, in words.
        message: String,
    },
    /// A database operation failed.
    #[error("database: {message}")]
    Db {
        /// Why, in words.
        message: String,
    },
    /// The archive does not begin with the magic.
    #[error("{path} is not a Kallisti backup: the twelve magic bytes are missing")]
    BadMagic {
        /// Archive that was read.
        path: PathBuf,
    },
    /// The archive layout is not one this binary writes.
    #[error(
        "the archive uses format version {found}, but this binary writes and reads {supported}: upgrade eris rather than downgrading the archive"
    )]
    UnsupportedFormat {
        /// Version found in the manifest.
        found: u32,
        /// Version this binary supports.
        supported: u32,
    },
    /// The manifest length field is larger than any manifest this binary writes.
    #[error("the archive claims a {found} byte manifest, above the {max} byte ceiling")]
    ManifestTooLarge {
        /// Length found in the archive.
        found: u32,
        /// Ceiling this binary enforces.
        max: u32,
    },
    /// The manifest is absent, truncated, not JSON, or self-inconsistent.
    #[error("the archive manifest is unusable: {message}")]
    Manifest {
        /// Why, in words.
        message: String,
    },
    /// The manifest's own digest does not cover its fields.
    #[error(
        "the manifest digest does not match its contents: recorded {recorded}, computed {computed}"
    )]
    ManifestDigest {
        /// Digest stored in the manifest.
        recorded: String,
        /// Digest over the manifest as read.
        computed: String,
    },
    /// A member's bytes are not the bytes the manifest names.
    #[error(
        "member {path} fails verification: the manifest says {expected}, the payload hashes to {actual}"
    )]
    MemberChecksum {
        /// Member path.
        path: String,
        /// Digest from the manifest.
        expected: String,
        /// Digest of the bytes actually present.
        actual: String,
    },
    /// The payload ended early, or the zstd frame is not decodable.
    #[error("the archive payload cannot be read: {message}")]
    Payload {
        /// Why, in words.
        message: String,
    },
    /// The archive was written by a newer Eris. Refuse; migrations do not run back.
    #[error(
        "the archive's database schema version {found} is newer than the {supported} this binary understands: restore it with the eris build that wrote it"
    )]
    NewerSchema {
        /// Version in the manifest.
        found: i64,
        /// Version this binary supports.
        supported: i64,
    },
    /// A member path could escape the target root, or names something else odd.
    #[error("member path {path:?} is refused: {reason}")]
    MemberPath {
        /// The path as it appears in the manifest.
        path: String,
        /// Why it was refused.
        reason: &'static str,
    },
    /// There is nothing to back up.
    #[error("{path} does not exist, so there is no state to back up")]
    NoState {
        /// The database path that was expected.
        path: PathBuf,
    },
    /// The destination already holds a file.
    #[error("{path} already exists; move it aside or choose another name")]
    ExistingDestination {
        /// Path that was refused.
        path: PathBuf,
    },
    /// `--out` cannot describe a run.
    #[error("{message}")]
    Usage {
        /// Why, in words.
        message: String,
    },
    /// Restore would replace existing state without `--force`.
    #[error("state already exists under {state_dir}; restore replaces it, so it needs --force")]
    NeedsForce {
        /// The state directory that is not empty.
        state_dir: PathBuf,
    },
    /// `PRAGMA quick_check` on a database did not answer `ok`.
    #[error("{path} fails PRAGMA quick_check: {result}")]
    QuickCheck {
        /// The database that failed.
        path: PathBuf,
        /// What SQLite reported.
        result: String,
    },
    /// The restore was stopped on purpose at a seam, to prove what a crash does.
    #[error("restore stopped at the injected fault point: {point}")]
    InjectedFault {
        /// Which seam.
        point: &'static str,
    },
}

impl BackupError {
    /// A portable constructor for [`BackupError::Io`].
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// A stable machine-readable code, for the `--json` error body.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "io",
            Self::Config { .. } => "config",
            Self::Db { .. } => "database",
            Self::BadMagic { .. } => "not_a_kallisti_backup",
            Self::UnsupportedFormat { .. } => "unsupported_format",
            Self::ManifestTooLarge { .. } => "manifest_too_large",
            Self::Manifest { .. } => "manifest_invalid",
            Self::ManifestDigest { .. } => "manifest_digest_mismatch",
            Self::MemberChecksum { .. } => "member_checksum_mismatch",
            Self::Payload { .. } => "payload_unreadable",
            Self::NewerSchema { .. } => "newer_schema",
            Self::MemberPath { .. } => "member_path_refused",
            Self::NoState { .. } => "no_state",
            Self::ExistingDestination { .. } => "destination_exists",
            Self::Usage { .. } => "usage",
            Self::NeedsForce { .. } => "needs_force",
            Self::QuickCheck { .. } => "quick_check_failed",
            Self::InjectedFault { .. } => "injected_fault",
        }
    }

    /// The process exit code this failure maps to.
    ///
    /// Integrity and verification failures are 3 so a script cannot mistake a
    /// tampered archive for an I/O error. Refusals and I/O failures are 1, and a
    /// run the arguments cannot describe is 2.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::BadMagic { .. }
            | Self::ManifestTooLarge { .. }
            | Self::Manifest { .. }
            | Self::ManifestDigest { .. }
            | Self::MemberChecksum { .. }
            | Self::Payload { .. }
            | Self::QuickCheck { .. } => EXIT_VERIFICATION,
            Self::Usage { .. } => EXIT_USAGE,
            _ => EXIT_FAILURE,
        }
    }
}

#[cfg(test)]
mod support {
    //! Fixtures shared by the `backup::` tests.
    //!
    //! Every root is a `tempfile::TempDir`, and every path comes from
    //! [`Paths::for_root`], so no test reads or writes the installed state under
    //! `~/.local/share/kallisti`.

    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use crate::backup::CreateRequest;
    use crate::backup::create::create;
    use crate::backup::restore::RestoreRequest;
    use crate::backup::{CreateReport, RestoreReport};
    use crate::config::Paths;
    use crate::db::Db;

    /// The documented `eris.env` shape: references only.
    pub const ENV_BODY: &str = "\
KALLISTI_BASE_URL=http://127.0.0.1:20128/v1
KALLISTI_MODEL=ds/deepseek-v4-flash
KALLISTI_API_KEY_REF=env:NINEROUTER_API_KEY
";

    /// `config.toml` with the one key a backup cares about plus a log level.
    pub const TOML_BODY: &str = "log_level = \"info\"\n";

    /// A sealed-segment stand-in. The bytes are opaque to backup and restore.
    pub const SEGMENT_BYTES: &[u8] = b"sealed-segment-bytes-0123456789";

    /// A WAL with two complete lines and a torn third one.
    pub const WAL_BYTES: &[u8] = b"{\"seq\":1}\n{\"seq\":2}\n{\"seq\":3";

    /// The passphrase the first-run fixture holds.
    pub const PASSPHRASE: &str = "aroma awake river candle hollow tiger";

    /// A scratch root plus its resolved paths.
    pub struct Scratch {
        /// Kept alive so the directory is removed at the end of the test.
        pub dir: TempDir,
        /// `config/` and `state/` under the scratch root.
        pub paths: Paths,
    }

    /// A fresh scratch root.
    pub fn scratch() -> Scratch {
        let dir = tempfile::tempdir().expect("scratch root");
        let paths = Paths::for_root(dir.path().join("root"));
        Scratch { dir, paths }
    }

    /// Set permission bits, unix only (the modes are the point of the test).
    pub fn set_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("chmod");
    }

    /// Permission bits of a path.
    pub fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    /// A file's bytes.
    pub fn read(path: &Path) -> Vec<u8> {
        fs::read(path).expect("read file")
    }

    /// Lowercase hex sha256 of a file's bytes.
    pub fn digest_of(path: &Path) -> String {
        crate::backup::sha256_hex(&read(path))
    }

    /// Populate a scratch root: database, both config files, the passphrase file,
    /// a sealed segment, and a live WAL.
    ///
    /// The modes are deliberately wrong (`eris.env` 0644, state dir 0755) so the
    /// permission test has something to prove.
    pub fn seed(paths: &Paths) {
        fs::create_dir_all(&paths.config_dir).expect("config dir");
        fs::write(&paths.env_file, ENV_BODY).expect("eris.env");
        set_mode(&paths.env_file, 0o644);
        fs::write(&paths.toml_file, TOML_BODY).expect("config.toml");
        set_mode(&paths.toml_file, 0o644);
        fs::write(
            paths.config_dir.join("first-run.txt"),
            format!("{PASSPHRASE}\n"),
        )
        .expect("first-run.txt");

        let db = Db::open(&paths.db_file).expect("open db");
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO audit_log (actor, action, detail) VALUES ('operator', 'setup.completed', 'onboarding')",
                [],
            )
        })
        .expect("seed audit row");
        drop(db);

        let session = paths.state_dir.join("sessions/s-1");
        fs::create_dir_all(&session).expect("session dir");
        let mut segment =
            fs::File::create(session.join("session.v1.000001-000002.jsonl.zst")).expect("segment");
        segment.write_all(SEGMENT_BYTES).expect("segment bytes");
        drop(segment);
        fs::write(session.join("wal.jsonl"), WAL_BYTES).expect("wal");
        set_mode(&paths.state_dir, 0o755);
    }

    /// A scratch root seeded with [`seed`].
    pub fn seeded() -> Scratch {
        let scratch = scratch();
        seed(&scratch.paths);
        scratch
    }

    /// The scratch root's session directory for a named session.
    pub fn session_dir(paths: &Paths, name: &str) -> PathBuf {
        paths.state_dir.join("sessions").join(name)
    }

    /// Create an archive from a scratch root.
    pub fn backup_into(paths: &Paths, out: &Path, verify: bool) -> CreateReport {
        let request = CreateRequest {
            out: out.to_path_buf(),
            include_workspace: false,
            verify,
            allow_tmp: false,
            paths: paths.clone(),
        };
        create(&request).expect("backup should succeed")
    }

    /// Restore an archive into a scratch root.
    pub fn restore_into(from: &Path, paths: &Paths, force: bool) -> Result<RestoreReport, String> {
        let request = RestoreRequest {
            from: from.to_path_buf(),
            force,
            dry_run: false,
            paths: paths.clone(),
        };
        crate::backup::restore(&request).map_err(|error| error.to_string())
    }

    /// The single `pre-restore-*` directory under a state dir.
    pub fn safety_copies(paths: &Paths) -> Vec<PathBuf> {
        let mut found = Vec::new();
        let Ok(entries) = fs::read_dir(&paths.state_dir) else {
            return found;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("pre-restore-") {
                found.push(entry.path());
            }
        }
        found.sort();
        found
    }

    /// `PRAGMA quick_check` on a database file, read-only.
    pub fn quick_check(path: &Path) -> String {
        let conn =
            rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .expect("open for quick_check");
        conn.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0))
            .expect("quick_check runs")
    }
}

// The nine acceptance tests are declared directly in this module, not in a
// `mod tests`, so `cargo test` reports them as `backup::<name>`.
#[cfg(test)]
use support::*;

#[cfg(test)]
use crate::db::Db;

#[cfg(test)]
#[test]
fn round_trip_restores_identical_state() {
    let source = seeded();
    let archive = source.dir.path().join("round-trip.kallisti-backup");
    let report = backup_into(&source.paths, &archive, true);
    assert!(report.verified, "the archive verifies after writing");

    // The archive stores the state, and the source keeps its own modes.
    assert_eq!(mode_of(&source.paths.env_file), 0o644);

    let target = scratch();
    assert!(
        !target.paths.state_dir.exists(),
        "a fresh root has no state to force"
    );
    let restored =
        restore_into(&archive, &target.paths, false).expect("restore into an empty root");

    assert_eq!(restored.members, report.members.len());
    assert!(
        restored.safety_copy.is_none(),
        "nothing existed to copy away"
    );

    // The database that landed is exactly the file the report named.
    assert_eq!(
        restored.db_sha256.as_deref(),
        Some(&*digest_of(&target.paths.db_file))
    );
    assert_eq!(quick_check(&target.paths.db_file), "ok");

    // Every seeded row survived.
    let conn = rusqlite::Connection::open(&target.paths.db_file).expect("open restored db");
    let seeded: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'setup.completed'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(seeded, 1, "the seeded audit row survived the round trip");
    let restore_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE actor = 'restore' AND action = 'backup.restore'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(restore_rows, 1, "restore leaves its own audit row");
    drop(conn);

    // Config and session files are byte-identical.
    assert_eq!(read(&target.paths.env_file), read(&source.paths.env_file));
    assert_eq!(read(&target.paths.toml_file), read(&source.paths.toml_file));
    let segment = "session.v1.000001-000002.jsonl.zst";
    assert_eq!(
        read(&session_dir(&target.paths, "s-1").join(segment)),
        read(&session_dir(&source.paths, "s-1").join(segment))
    );

    // The passphrase file never travelled.
    assert!(
        !target.paths.config_dir.join("first-run.txt").exists(),
        "first-run.txt must not be restored from an archive"
    );
}

#[cfg(test)]
#[test]
fn live_daemon_backup_passes_quick_check() {
    use std::io::Read;

    use crate::backup::verify;

    let live = seeded();
    let archive = live.dir.path().join("live.kallisti-backup");

    // The daemon: a database handle that stays open, with a commit that is not
    // yet checkpointed, so the row exists only in the WAL.
    let db = Db::open(&live.paths.db_file).expect("open live db");
    db.set_setup_state(crate::db::SetupState::Ready)
        .expect("mark ready");
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO audit_log (actor, action, detail) VALUES ('loopback', 'live.row', 'only in the wal')",
            [],
        )
    })
    .expect("write through the wal");
    let wal = live.paths.state_dir.join("eris.db-wal");
    assert!(
        wal.exists() && std::fs::metadata(&wal).expect("wal metadata").len() > 0,
        "the committed row must still live in the WAL for this test to mean anything"
    );

    // Backup while that handle is open.
    let report = backup_into(&live.paths, &archive, true);
    assert!(report.verified);

    // No member may be a WAL or SHM sidecar: those are the torn-copy trap.
    let opened = verify::open(&archive).expect("open archive");
    assert!(
        opened
            .manifest
            .members
            .iter()
            .all(|member| !member.path.ends_with("-wal") && !member.path.ends_with("-shm")),
        "the archive must not carry a WAL or SHM sidecar"
    );

    // The snapshot taken under the live handle is a complete database.
    let bytes = verify::read_member_bytes(&opened, crate::backup::DB_PATH).expect("db member");
    let scratch = tempfile::tempdir().expect("scratch");
    let snapshot = scratch.path().join("eris.db");
    std::fs::write(&snapshot, &bytes).expect("write snapshot");
    assert_eq!(quick_check(&snapshot), "ok");

    let conn = rusqlite::Connection::open(&snapshot).expect("open snapshot");
    let rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM audit_log WHERE action = 'live.row'",
            [],
            |row| row.get(0),
        )
        .expect("count");
    assert_eq!(
        rows, 1,
        "VACUUM INTO must capture pages committed to the WAL, not just the main file"
    );
    drop(conn);
    drop(db);

    // A second read of the archived member agrees with the first.
    let mut check = Vec::new();
    let opened = verify::open(&archive).expect("reopen");
    verify::verify(&opened).expect("verifies");
    let _ = std::fs::File::open(&archive)
        .expect("open")
        .read(&mut check);
}

#[cfg(test)]
#[test]
fn tampered_member_fails_before_any_write() {
    let live = seeded();
    let archive = live.dir.path().join("tampered.kallisti-backup");
    backup_into(&live.paths, &archive, true);

    // Flip a byte inside the payload, past the manifest.
    let opened = crate::backup::open(&archive).expect("open archive");
    let offset = (opened.payload_offset as usize) + 4;
    let mut bytes = read(&archive);
    bytes[offset] ^= 0xff;
    std::fs::write(&archive, &bytes).expect("tamper");

    let before_db = digest_of(&live.paths.db_file);
    let before_env = read(&live.paths.env_file);
    let before_config: Vec<String> = std::fs::read_dir(&live.paths.config_dir)
        .expect("config dir")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();

    let request = crate::backup::RestoreRequest {
        from: archive.clone(),
        force: true,
        dry_run: false,
        paths: live.paths.clone(),
    };
    let error = crate::backup::restore(&request).expect_err("a tampered archive must be refused");

    assert_eq!(
        error.exit_code(),
        crate::backup::EXIT_VERIFICATION,
        "a tampered archive is a verification failure, not an I/O error: {error}"
    );
    assert!(
        matches!(
            error,
            crate::backup::BackupError::MemberChecksum { .. }
                | crate::backup::BackupError::Payload { .. }
                | crate::backup::BackupError::ManifestDigest { .. }
                | crate::backup::BackupError::Manifest { .. }
        ),
        "unexpected refusal: {error:?}"
    );

    // Nothing moved: same database bytes, same config bytes, same names, and no
    // safety copy (a safety copy is the first write a restore does).
    assert_eq!(digest_of(&live.paths.db_file), before_db);
    assert_eq!(read(&live.paths.env_file), before_env);
    let after_config: Vec<String> = std::fs::read_dir(&live.paths.config_dir)
        .expect("config dir")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(after_config.len(), before_config.len());
    assert!(
        safety_copies(&live.paths).is_empty(),
        "a refused restore must not take a safety copy"
    );
}

#[cfg(test)]
#[test]
fn newer_schema_is_refused() {
    let live = seeded();
    let archive = live.dir.path().join("future.kallisti-backup");
    backup_into(&live.paths, &archive, true);

    // Rewrite the manifest as a future Eris would: one schema version ahead.
    let future = crate::db::SCHEMA_VERSION + 1;
    rewrite_manifest(&archive, |manifest| {
        manifest.schema_version = future;
    });

    let target = scratch();
    let request = crate::backup::RestoreRequest {
        from: archive,
        force: true,
        dry_run: false,
        paths: target.paths.clone(),
    };
    let error = crate::backup::restore(&request).expect_err("a newer schema must be refused");
    match &error {
        crate::backup::BackupError::NewerSchema { found, supported } => {
            assert_eq!(*found, future);
            assert_eq!(*supported, crate::db::SCHEMA_VERSION);
        }
        other => panic!("expected NewerSchema, got {other:?}"),
    }
    assert_eq!(error.exit_code(), crate::backup::EXIT_FAILURE);
    assert!(
        !target.paths.db_file.exists(),
        "a refused restore writes no database"
    );
}

#[cfg(test)]
#[test]
fn restore_takes_a_safety_copy() {
    let live = seeded();
    let archive = live.dir.path().join("safety.kallisti-backup");
    backup_into(&live.paths, &archive, true);

    // Change the live state after the backup, so the safety copy is provably the
    // state as it was immediately before the restore.
    std::fs::write(
        &live.paths.env_file,
        "KALLISTI_BASE_URL=http://127.0.0.1:1/v1\nKALLISTI_MODEL=changed\nKALLISTI_API_KEY_REF=env:CHANGED\n",
    )
    .expect("change env");
    let before_db = digest_of(&live.paths.db_file);

    let report = restore_into(&archive, &live.paths, true).expect("restore over existing state");
    let safety = report
        .safety_copy
        .clone()
        .expect("a restore over existing state takes a safety copy");
    assert!(safety.exists());

    let copies = safety_copies(&live.paths);
    assert_eq!(copies.len(), 1, "exactly one pre-restore directory");
    assert_eq!(copies[0], safety);

    // The safety copy holds the state that was overwritten...
    let saved_env = std::fs::read_to_string(safety.join("config/eris.env")).expect("saved env");
    assert!(saved_env.contains("KALLISTI_MODEL=changed"), "{saved_env}");
    let saved_db = safety.join("state/eris.db");
    assert!(saved_db.exists(), "the pre-restore database is kept");
    assert_eq!(digest_of(&saved_db), before_db);
    assert_eq!(quick_check(&saved_db), "ok");

    // ...and the live state is the archive's state.
    assert_eq!(read(&live.paths.env_file), ENV_BODY.as_bytes());
}

#[cfg(test)]
#[test]
fn path_traversal_member_is_rejected() {
    let live = seeded();
    let archive = live.dir.path().join("traversal.kallisti-backup");
    backup_into(&live.paths, &archive, true);

    // A member that would land outside the target root.
    rewrite_manifest(&archive, |manifest| {
        manifest.members[0].path = "state/../../etc/kallisti-escaped".to_owned();
    });

    let target = scratch();
    let request = crate::backup::RestoreRequest {
        from: archive,
        force: true,
        dry_run: false,
        paths: target.paths.clone(),
    };
    let error = crate::backup::restore(&request).expect_err("zip-slip must be refused");
    match &error {
        crate::backup::BackupError::MemberPath { path, .. } => {
            assert!(path.contains(".."), "{path}");
        }
        other => panic!("expected MemberPath, got {other:?}"),
    }
    assert_eq!(error.exit_code(), crate::backup::EXIT_FAILURE);

    // Nothing was created anywhere under the scratch root, and nothing outside it.
    assert!(
        !target
            .paths
            .state_dir
            .join("..")
            .join("..")
            .join("etc")
            .exists()
    );
    assert!(!target.dir.path().join("etc").exists());
    assert!(!target.paths.db_file.exists());

    // An absolute member is refused the same way.
    let live2 = seeded();
    let archive2 = live2.dir.path().join("absolute.kallisti-backup");
    backup_into(&live2.paths, &archive2, true);
    rewrite_manifest(&archive2, |manifest| {
        manifest.members[0].path = "/etc/kallisti-absolute".to_owned();
    });
    let target2 = scratch();
    let request = crate::backup::RestoreRequest {
        from: archive2,
        force: true,
        dry_run: false,
        paths: target2.paths.clone(),
    };
    let error = crate::backup::restore(&request).expect_err("an absolute member is refused");
    assert!(matches!(
        error,
        crate::backup::BackupError::MemberPath { .. }
    ));
    assert!(!target2.paths.db_file.exists());
}

#[cfg(test)]
#[test]
fn archive_has_no_secret_values() {
    use crate::backup::verify;

    let live = seeded();
    let archive = live.dir.path().join("secrets.kallisti-backup");
    let report = backup_into(&live.paths, &archive, true);
    assert!(report.verified);

    let bytes = read(&archive);
    assert!(
        !contains(&bytes, PASSPHRASE.as_bytes()),
        "the setup passphrase must never reach an archive"
    );
    // The passphrase is six words; none of the distinctive ones may appear either,
    // which catches a partial write as well as a whole one.
    for word in ["aroma", "hollow", "tiger"] {
        assert!(
            !contains(&bytes, word.as_bytes()),
            "the passphrase word {word:?} must not reach an archive"
        );
    }

    // ...and the manifest says so out loud.
    assert!(
        report
            .excluded
            .iter()
            .any(|entry| entry.path == crate::backup::FIRST_RUN_PATH
                && entry.reason.to_lowercase().contains("passphrase")),
        "first-run.txt must be recorded as excluded, with a reason: {:?}",
        report.excluded
    );

    // A real key value, if this host has one in its environment, must not appear.
    if let Ok(value) = std::env::var("NINEROUTER_API_KEY")
        && value.len() >= 8
    {
        assert!(
            !contains(&bytes, value.as_bytes()),
            "the provider key value must never reach an archive"
        );
    }

    // The archived secret half carries a reference where the key goes, which is
    // the structural reason the archive needs no encryption.
    let opened = verify::open(&archive).expect("open archive");
    let env_text = String::from_utf8(
        verify::read_member_bytes(&opened, crate::backup::ENV_PATH).expect("env member"),
    )
    .expect("utf-8");
    assert_eq!(
        env_text.as_bytes(),
        ENV_BODY.as_bytes(),
        "the archived eris.env is the source file, byte for byte"
    );
    for line in env_text.lines().filter(|line| !line.trim().is_empty()) {
        let (key, value) = line.split_once('=').expect("KEY=VALUE");
        if key == "KALLISTI_API_KEY_REF" {
            assert!(
                value.starts_with("env:"),
                "the archived key reference must be a variable name, found {line:?}"
            );
        }
        assert!(!value.trim().is_empty(), "empty value in {line:?}");
    }
}

#[cfg(test)]
#[test]
fn restore_is_atomic_on_crash() {
    let live = seeded();
    let archive = live.dir.path().join("atomic.kallisti-backup");
    backup_into(&live.paths, &archive, true);

    let old_db = digest_of(&live.paths.db_file);

    // Crash between the extracted-database check and the first placement.
    let request = crate::backup::RestoreRequest {
        from: archive.clone(),
        force: true,
        dry_run: false,
        paths: live.paths.clone(),
    };
    let error = crate::backup::restore_with_fault(
        &request,
        Some(crate::backup::FaultPoint::AfterQuickCheck),
    )
    .expect_err("the injected fault stops the restore");
    assert_eq!(error.exit_code(), crate::backup::EXIT_FAILURE);

    // No half-written database is in place. The old one is in the safety copy.
    assert!(
        !live.paths.db_file.exists(),
        "the crash must not leave a partially written database in place"
    );
    let copies = safety_copies(&live.paths);
    assert_eq!(
        copies.len(),
        1,
        "the safety copy was taken before placement"
    );
    let saved_db = copies[0].join("state/eris.db");
    assert_eq!(digest_of(&saved_db), old_db);
    assert_eq!(quick_check(&saved_db), "ok");

    // No staging directory survives the crash.
    let leftovers: Vec<String> = std::fs::read_dir(&live.paths.state_dir)
        .expect("state dir")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".restore-staging-"))
        .collect();
    assert!(leftovers.is_empty(), "staging leftovers: {leftovers:?}");

    // The state is recoverable: a second, unmolested restore completes.
    let report = restore_into(&archive, &live.paths, true).expect("a retry restores");
    assert_eq!(
        report.db_sha256.as_deref(),
        Some(&*digest_of(&live.paths.db_file))
    );
    assert_eq!(quick_check(&live.paths.db_file), "ok");
    assert_eq!(read(&live.paths.env_file), ENV_BODY.as_bytes());
}

#[cfg(test)]
#[test]
fn permissions_are_reapplied() {
    let live = seeded();
    // The seeded source is deliberately loose: eris.env is world-readable.
    assert_eq!(mode_of(&live.paths.env_file), 0o644);
    let archive = live.dir.path().join("modes.kallisti-backup");
    backup_into(&live.paths, &archive, true);

    let target = scratch();
    restore_into(&archive, &target.paths, false).expect("restore");

    // The manifest carries kinds, not modes, so these are re-applied by rule.
    assert_eq!(mode_of(&target.paths.env_file), 0o600, "eris.env");
    assert_eq!(mode_of(&target.paths.toml_file), 0o644, "config.toml");
    assert_eq!(
        mode_of(&target.paths.state_dir),
        0o700,
        "the state directory"
    );
    assert_eq!(
        mode_of(&target.paths.config_dir),
        0o700,
        "the config directory"
    );
    assert_eq!(mode_of(&target.paths.db_file), 0o600, "the database");

    // Backup must not have changed the source's modes.
    assert_eq!(mode_of(&live.paths.env_file), 0o644);
}

#[cfg(test)]
#[test]
fn a_live_wal_is_cut_at_a_complete_line_when_the_daemon_holds_the_lock() {
    let live = seeded();
    let archive = live.dir.path().join("locked-wal.kallisti-backup");

    // A second session whose WAL is being appended to right now: the test holds
    // the same `fs4` advisory lock the log writer takes.
    let locked = session_dir(&live.paths, "s-2");
    std::fs::create_dir_all(&locked).expect("session dir");
    std::fs::write(locked.join("wal.jsonl"), WAL_BYTES).expect("wal");
    let held = core_runtime::log::SessionLock::acquire(&locked).expect("hold the session lock");

    let report = backup_into(&live.paths, &archive, true);
    let torn_tail = WAL_BYTES.len()
        - WAL_BYTES
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map(|last| last + 1)
            .expect("a complete line");
    assert_eq!(
        report.wal_partial_bytes, torn_tail as u64,
        "the dropped tail is recorded in the manifest instead of being lost silently"
    );
    assert!(
        report
            .excluded
            .iter()
            .any(|entry| entry.path.ends_with("/.lock")),
        "the lock file is not state and is recorded as excluded: {:?}",
        report.excluded
    );

    // The unlocked session was copied whole; the locked one only to its last
    // complete line.
    let target = scratch();
    restore_into(&archive, &target.paths, false).expect("restore");
    assert_eq!(
        read(&session_dir(&target.paths, "s-1").join("wal.jsonl")),
        WAL_BYTES,
        "an unlocked WAL is copied byte for byte"
    );
    let restored = read(&session_dir(&target.paths, "s-2").join("wal.jsonl"));
    assert_eq!(restored.len(), WAL_BYTES.len() - torn_tail);
    assert!(
        !contains(&restored, b"{\"seq\":3"),
        "the torn tail must not travel"
    );

    drop(held);
}

#[cfg(test)]
/// Rewrite the manifest inside an archive, recompute its digest, and keep the
/// payload bytes exactly as they were.
///
/// Test-only: this is how a test builds the archive a tampered or hostile source
/// would hand a restore. It is deliberately not part of the public API.
fn rewrite_manifest(archive: &std::path::Path, mutate: impl FnOnce(&mut Manifest)) {
    let bytes = read(archive);
    let length_at = MAGIC.len();
    let length = u32::from_le_bytes(
        bytes[length_at..length_at + 4]
            .try_into()
            .expect("four length bytes"),
    ) as usize;
    let manifest_at = length_at + 4;
    let mut manifest: Manifest =
        serde_json::from_slice(&bytes[manifest_at..manifest_at + length]).expect("manifest json");
    mutate(&mut manifest);
    manifest.seal();
    let json = serde_json::to_vec(&manifest).expect("serialise");
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(&json);
    out.extend_from_slice(&bytes[manifest_at + length..]);
    std::fs::write(archive, out).expect("write rewritten archive");
}

#[cfg(test)]
/// Whether `haystack` contains `needle`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}
