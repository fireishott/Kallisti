//! The backups surface: the archive directory, the listing, the create, and the
//! restore confirmation gate.
//!
//! The work itself is not here. [`crate::backup`] already owns the format, the
//! `VACUUM INTO` snapshot, verification, and the restore ordering, and this
//! module is the thin web-shaped skin over it: it resolves the one allowed
//! directory, decides what a page may show, and refuses a request that names a
//! path rather than a file in that directory.
//!
//! One rule is the whole safety story: a request carries a **bare filename**, and
//! every path on the way to the archive is built by joining that name to the one
//! directory this daemon owns. A name containing `/`, `\`, or `..`, or one that
//! is absolute, is refused before any filesystem call is made. Nothing in this
//! module ever builds a path from a string a form supplied.

use std::path::{Path, PathBuf};

use crate::backup::{
    BackupError, CreateReport, CreateRequest, RestoreReport, RestoreRequest, open, verify,
};
use crate::config::Paths;

/// The directory, under the state root, that holds every archive.
///
/// It is inside `~/.local/share/kallisti/`, which is on the deployment plan's
/// allowlist. Nothing here reads or writes outside it.
pub const DIRECTORY_NAME: &str = "backups";

/// The suffix of every archive this daemon writes.
pub const ARCHIVE_SUFFIX: &str = ".bk";

/// The prefix of every archive this daemon writes.
pub const NAME_PREFIX: &str = "kallisti-";

/// The state root's archive directory.
#[must_use]
pub fn directory(paths: &Paths) -> PathBuf {
    paths.state_dir.join(DIRECTORY_NAME)
}

/// Create the archive directory, mode `0700`, if it is not there.
pub fn ensure_directory(dir: &Path) -> Result<(), BackupError> {
    if dir.is_dir() {
        return Ok(());
    }
    std::fs::create_dir_all(dir).map_err(|error| BackupError::io(dir, error))?;
    set_mode(dir, 0o700)
}

/// The UTC stamp that names an archive: `2026-10-19T21-12-56Z`.
///
/// Colons are replaced because they are legal here but hostile in a shell, a URL,
/// and a copy-paste. The fixed width means a plain name sort is a chronological
/// sort, which is what the listing relies on.
#[must_use]
pub fn stamp() -> String {
    let now = crate::auth::format_timestamp(crate::auth::now());
    let seconds = now.split('.').next().unwrap_or(now.as_str());
    let seconds = seconds.trim_end_matches('Z').replace(':', "-");
    format!("{seconds}Z")
}

/// The name a fresh archive gets.
#[must_use]
pub fn new_archive_name() -> String {
    format!("{NAME_PREFIX}{}{ARCHIVE_SUFFIX}", stamp())
}

/// Refuse a name that is not a bare filename in the archive directory.
///
/// The returned `&'static str` is the reason, and it is what the page shows. The
/// checks are all on the string, before anything touches the filesystem.
pub fn validate_archive_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() {
        return Err("an archive name is required");
    }
    if name.len() > 255 {
        return Err("the archive name is longer than this filesystem allows");
    }
    if name == "." || name == ".." || name.contains("..") {
        return Err("the name must not contain a parent-directory step");
    }
    if name.contains('/') || name.contains('\\') {
        return Err("an archive is named, not located: it must be a bare filename");
    }
    if name.starts_with('.') {
        return Err("the name must not begin with a dot");
    }
    if Path::new(name).is_absolute() {
        return Err("an absolute path is not an archive name");
    }
    if name.chars().any(char::is_control) {
        return Err("the name must not contain a control character");
    }
    Ok(())
}

/// The archive a validated name points at, inside [`directory`].
pub fn archive_path(paths: &Paths, name: &str) -> Result<PathBuf, &'static str> {
    validate_archive_name(name)?;
    Ok(directory(paths).join(name))
}

/// One archive as the page lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveRow {
    /// The bare filename.
    pub name: String,
    /// Size on disk.
    pub bytes: u64,
    /// Last modification, RFC 3339 UTC.
    pub modified: String,
    /// Members the manifest states, when the manifest could be read at all.
    ///
    /// `None` means the file does not open as an archive - a truncated or
    /// foreign file in the directory - so there is no member count to report.
    /// The page shows it as a verification failure; the device route reports
    /// null rather than inventing a zero.
    pub members: Option<usize>,
    /// The verification outcome: `Ok` when every member hashes to what the
    /// manifest says, `Err` with the stable code and its sentence when it does
    /// not.
    pub verification: Result<(), String>,
}

impl ArchiveRow {
    /// Whether the manifest and every member verified.
    #[must_use]
    pub fn verified(&self) -> bool {
        self.verification.is_ok()
    }

    /// The one word the table shows.
    #[must_use]
    pub fn state_label(&self) -> &'static str {
        if self.verified() { "verified" } else { "fails" }
    }

    /// The verification outcome as one line, for the table cell.
    #[must_use]
    pub fn verification_line(&self) -> String {
        match &self.verification {
            Ok(()) => "every member matches its manifest".to_owned(),
            Err(reason) => reason.clone(),
        }
    }
}

/// Every archive in the directory, newest name first, each verified.
///
/// A directory that does not exist is an empty list rather than an error: the
/// first visit to the page is supposed to show the empty state, not a failure.
pub fn list(paths: &Paths) -> Result<Vec<ArchiveRow>, BackupError> {
    let dir = directory(paths);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(BackupError::io(&dir, error)),
    };

    let mut rows = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| BackupError::io(&dir, error))?;
        // `file_type` does not follow a symlink, so a link planted in this
        // directory is skipped rather than read through.
        let file_type = entry
            .file_type()
            .map_err(|error| BackupError::io(entry.path(), error))?;
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // Only names a form could name back are listed: a file the restore form
        // would refuse is not an archive this page can offer.
        if validate_archive_name(&name).is_err() {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|error| BackupError::io(entry.path(), error))?;
        let (members, verification) = inspect_members(&entry.path());
        rows.push(ArchiveRow {
            bytes: metadata.len(),
            modified: metadata
                .modified()
                .map(format_time)
                .unwrap_or_else(|_| "unknown".to_owned()),
            members,
            verification,
            name,
        });
    }
    rows.sort_by(|left, right| right.name.cmp(&left.name));
    Ok(rows)
}

/// Write one archive into the archive directory with `--verify` semantics.
///
/// The name is generated here, never supplied: `kallisti-<utc stamp>.bk`, with a
/// numeric suffix when two creates land in the same second.
pub fn create_archive(paths: &Paths) -> Result<CreateReport, BackupError> {
    let dir = directory(paths);
    ensure_directory(&dir)?;
    let out = dir.join(free_name(&dir));
    write_archive(paths, out, false)
}

/// Write one archive at a caller-named destination.
///
/// This is the device surface's create: a body may name the file the way
/// `eris backup --out` names it, and everything else is the same write. Two
/// refusals travel with it:
///
/// - the `/tmp` rule, stated in the crate's own sentence. The crate's
///   `check_destination` stands that rule down for a scratch `--state-dir` run,
///   and a device request is not a scratch run: the phone names one absolute
///   path on the installed host, so the rule is applied here as well as there.
/// - an existing file at the destination, which the crate refuses rather than
///   clobbers.
pub fn create_archive_at(
    paths: &Paths,
    out: PathBuf,
    allow_tmp: bool,
) -> Result<CreateReport, BackupError> {
    if !allow_tmp && crate::backup::is_under_temp(&out) {
        return Err(BackupError::Usage {
            message: crate::backup::temporary_destination_reason(&out),
        });
    }
    write_archive(paths, out, allow_tmp)
}

/// The one write both creates go through.
fn write_archive(
    paths: &Paths,
    out: PathBuf,
    allow_tmp: bool,
) -> Result<CreateReport, BackupError> {
    let request = CreateRequest {
        out,
        include_workspace: false,
        verify: true,
        allow_tmp,
        paths: paths.clone(),
    };
    crate::backup::create(&request)
}

/// Restore one archive that is already in the archive directory.
///
/// The caller has validated and confirmed the name. This function resolves it
/// again through [`archive_path`] so there is one gate, not two, and takes the
/// file as named: no directory is read, and no symlink is followed.
pub fn restore_archive(paths: &Paths, name: &str) -> Result<RestoreReport, BackupError> {
    let path = archive_path(paths, name).map_err(|reason| BackupError::Usage {
        message: format!("{name} is refused as an archive name: {reason}"),
    })?;
    let metadata =
        std::fs::symlink_metadata(&path).map_err(|error| BackupError::io(&path, error))?;
    if !metadata.file_type().is_file() {
        return Err(BackupError::Usage {
            message: format!("{name} is not a regular file in the archive directory"),
        });
    }
    let request = RestoreRequest {
        from: path,
        force: true,
        dry_run: false,
        paths: paths.clone(),
    };
    crate::backup::restore(&request)
}

/// Append the operator's restore row to the database that was just placed.
///
/// [`crate::backup::restore`] writes its own row into the staged database before
/// placement (actor `restore`, archive digest, safety copy). This one is the web
/// surface's record of the operator's choice, and it has to go into the file on
/// disk: this process still holds the old inode open, so a row written through
/// that handle would land in the safety copy and never be seen again.
pub fn audit_placed_restore(paths: &Paths, detail: &str) -> Result<i64, BackupError> {
    audit_placed(paths, "loopback", "backup.restore", detail)
}

/// The same row, for a caller that names its own actor and action.
///
/// The browser names `loopback`; a paired phone names the device that asked, so
/// the Activity page shows which one restored rather than only which host did.
pub fn audit_placed(
    paths: &Paths,
    actor: &str,
    action: &str,
    detail: &str,
) -> Result<i64, BackupError> {
    let db = crate::db::Db::open(&paths.db_file).map_err(|error| BackupError::Db {
        message: format!("cannot open the restored database to record it: {error}"),
    })?;
    let id = db
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO audit_log (actor, action, detail) VALUES (?1, ?2, ?3)",
                rusqlite::params![actor, action, detail],
            )?;
            Ok(conn.last_insert_rowid())
        })
        .map_err(|error| BackupError::Db {
            message: format!("cannot write the restore audit row: {error}"),
        })?;
    Ok(id)
}

/// Percent-encode a filename for a query value, the way a form encoder does.
#[must_use]
pub fn query_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Percent-decode a query value, `+` as a space.
#[must_use]
pub fn query_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                let hex = &value[index + 1..index + 3];
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    out.push(byte);
                    index += 3;
                    continue;
                }
                out.push(b'%');
            }
            other => out.push(other),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The first name in the directory that a fresh archive may take.
fn free_name(dir: &Path) -> String {
    let base = new_archive_name();
    if !dir.join(&base).exists() {
        return base;
    }
    let stem = base.trim_end_matches(ARCHIVE_SUFFIX);
    for suffix in 2..=1000 {
        let candidate = format!("{stem}-{suffix}{ARCHIVE_SUFFIX}");
        if !dir.join(&candidate).exists() {
            return candidate;
        }
    }
    base
}

/// The manifest and every member, as the page's per-row answer, plus the member
/// count the manifest states when it could be read.
fn inspect_members(path: &Path) -> (Option<usize>, Result<(), String>) {
    let opened = match open(path) {
        Ok(opened) => opened,
        Err(error) => return (None, Err(format!("{}: {error}", error.code()))),
    };
    // The count comes from the manifest, which `open` has already validated: it
    // is a fact the file states, not a number this daemon computed.
    let members = opened.manifest.members.len();
    (
        Some(members),
        verify(&opened)
            .map(|_| ())
            .map_err(|error| format!("{}: {error}", error.code())),
    )
}

/// One `SystemTime` as RFC 3339 UTC.
fn format_time(at: std::time::SystemTime) -> String {
    crate::auth::format_timestamp(time::OffsetDateTime::from(at))
}

/// Set permission bits, unix only.
fn set_mode(path: &Path, mode: u32) -> Result<(), BackupError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
            .map_err(|error| BackupError::io(path, error))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The eight acceptance tests.
//
// Declared at module level, not in a `mod tests`, so `cargo test` reports them
// as `web::backups::<name>`.
// ---------------------------------------------------------------------------

#[cfg(test)]
use axum::http::StatusCode;

#[cfg(test)]
use crate::admin::{
    ROUTE_ACTIVITY, ROUTE_BACKUPS, ROUTE_BACKUPS_CREATE, ROUTE_BACKUPS_RESTORE, ROUTE_DASHBOARD,
    ROUTE_DEVICES, ROUTE_SESSIONS, ROUTE_SETTINGS,
};

#[cfg(test)]
use crate::backup::{EXIT_VERIFICATION, sha256_hex};
#[cfg(test)]
use crate::db::Db;
#[cfg(test)]
use crate::web::harness::{Call, audit_rows, block_on, encode, ready_app, send};

/// The `pre-restore-<stamp>` directories under a state root.
#[cfg(test)]
fn safety_copies(paths: &Paths) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(&paths.state_dir) else {
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

/// The one archive a create just wrote, by name.
#[cfg(test)]
fn only_archive(paths: &Paths) -> String {
    let rows = list(paths).expect("the listing reads");
    assert_eq!(rows.len(), 1, "one archive, found {rows:?}");
    rows[0].name.clone()
}

/// Mode bits, unix only.
#[cfg(test)]
fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .expect("metadata")
        .permissions()
        .mode()
        & 0o777
}

#[cfg(test)]
#[test]
fn the_page_lists_created_archives() {
    let (_dir, engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());

    let created = block_on(send(
        &app,
        Call::post(ROUTE_BACKUPS_CREATE, "").html().cookie(cookie),
    ));
    assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.body);
    let location = created.location().expect("a redirect names the notice");
    let name = location
        .split("created=")
        .nth(1)
        .map(query_decode)
        .unwrap_or_else(|| panic!("the redirect carries the filename: {location}"));
    assert!(
        name.starts_with(NAME_PREFIX) && name.ends_with(ARCHIVE_SUFFIX),
        "the archive is named for its product and stamp: {name}"
    );

    let path = directory(engine.paths()).join(&name);
    let size = std::fs::metadata(&path)
        .expect("the archive is on disk")
        .len();

    let page = block_on(send(&app, Call::get(ROUTE_BACKUPS).html().cookie(cookie)));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        page.body.contains(&name),
        "the filename is listed: {}",
        page.body
    );
    assert!(
        page.body.contains(&format!("{size} bytes")),
        "the size is listed: {}",
        page.body
    );
    assert!(
        page.body.contains("verified"),
        "the verification state is listed: {}",
        page.body
    );
}

#[cfg(test)]
#[test]
fn the_empty_state_says_what_to_do_next() {
    let (_dir, _engine, app, cookie) = ready_app();
    let page = block_on(send(
        &app,
        Call::get(ROUTE_BACKUPS).html().cookie(Some(&cookie)),
    ));
    assert_eq!(page.status, StatusCode::OK, "{}", page.body);
    assert!(
        page.body.contains("No archive"),
        "the empty state is a sentence: {}",
        page.body
    );
    assert!(
        page.body
            .contains(&format!(r#"action="{ROUTE_BACKUPS_CREATE}""#)),
        "the empty state offers the create button: {}",
        page.body
    );
    assert!(
        page.body.contains("Create one now"),
        "the empty state says what to do next: {}",
        page.body
    );
}

#[cfg(test)]
#[test]
fn creating_a_backup_writes_a_verified_archive_and_audits() {
    let (_dir, engine, app, cookie) = ready_app();
    let created = block_on(send(
        &app,
        Call::post(ROUTE_BACKUPS_CREATE, "")
            .html()
            .cookie(Some(&cookie)),
    ));
    assert_eq!(created.status, StatusCode::SEE_OTHER, "{}", created.body);

    let paths = engine.paths();
    let dir = directory(paths);
    assert!(dir.is_dir(), "the archive directory exists");
    assert_eq!(mode_of(&dir), 0o700, "the archive directory is private");

    let name = only_archive(paths);
    let path = dir.join(&name);
    assert_eq!(mode_of(&path), 0o600, "the archive is private");

    // `--verify` semantics: the file on disk opens and every member hashes.
    let opened = open(&path).expect("the archive opens");
    let report = verify(&opened).expect("every member verifies");
    assert!(
        report.members > 0,
        "an archive with no member is not a backup"
    );

    // One audit row, naming the file and its digest. A digest is not a secret.
    let rows = audit_rows(engine.db());
    let last = rows.last().expect("an audit row");
    assert_eq!(last.action, "backup.create", "{rows:?}");
    assert_eq!(last.actor, "loopback");
    let detail = last.detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains(&name),
        "the detail names the file: {detail}"
    );
    let digest = sha256_hex(&std::fs::read(&path).expect("read the archive"));
    assert!(
        detail.contains(&digest),
        "the detail carries the archive digest: {detail}"
    );
    assert!(!detail.to_lowercase().contains("passphrase"), "{detail}");
}

#[cfg(test)]
#[test]
fn restore_requires_the_exact_filename_as_confirmation() {
    let (_dir, engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());
    block_on(send(
        &app,
        Call::post(ROUTE_BACKUPS_CREATE, "").html().cookie(cookie),
    ));
    let paths = engine.paths();
    let name = only_archive(paths);
    let before = sha256_hex(&std::fs::read(&paths.db_file).expect("read the database"));

    // No confirmation at all.
    let missing = block_on(send(
        &app,
        Call::post(ROUTE_BACKUPS_RESTORE, &format!("archive={}", encode(&name)))
            .html()
            .cookie(cookie),
    ));
    assert_eq!(missing.status, StatusCode::BAD_REQUEST, "{}", missing.body);
    assert!(
        missing.body.contains("confirm_mismatch"),
        "a missing confirmation is named: {}",
        missing.body
    );
    assert!(
        missing.body.contains(&name),
        "the page says which name it expected: {}",
        missing.body
    );

    // The wrong confirmation.
    let wrong = block_on(send(
        &app,
        Call::post(
            ROUTE_BACKUPS_RESTORE,
            &format!("archive={}&confirm=wrong", encode(&name)),
        )
        .html()
        .cookie(cookie),
    ));
    assert_eq!(wrong.status, StatusCode::BAD_REQUEST, "{}", wrong.body);
    assert!(
        wrong.body.contains("confirm_mismatch"),
        "a mismatched confirmation is named: {}",
        wrong.body
    );

    // Neither attempt moved anything.
    assert_eq!(
        sha256_hex(&std::fs::read(&paths.db_file).expect("read the database")),
        before,
        "a refused restore does not touch the database"
    );
    assert!(
        safety_copies(paths).is_empty(),
        "a refused restore takes no safety copy"
    );

    // The exact filename is accepted, which is what makes the gate a gate rather
    // than a wall.
    let accepted = block_on(send(
        &app,
        Call::post(
            ROUTE_BACKUPS_RESTORE,
            &format!("archive={}&confirm={}", encode(&name), encode(&name)),
        )
        .html()
        .cookie(cookie),
    ));
    assert_eq!(accepted.status, StatusCode::SEE_OTHER, "{}", accepted.body);
    assert_eq!(
        safety_copies(paths).len(),
        1,
        "the accepted restore took a safety copy"
    );

    // The web row is written where the operator will find it: in the database
    // that was placed, which is the one a restart opens.
    let placed = Db::open(&paths.db_file).expect("open the placed database");
    let rows = audit_rows(&placed);
    let mine = rows
        .iter()
        .rev()
        .find(|row| row.action == "backup.restore" && row.actor == "loopback")
        .expect("the web restore audits itself");
    let detail = mine.detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains(&name),
        "the detail names the archive: {detail}"
    );
    assert!(
        detail.contains("safety_copy="),
        "the detail carries the safety copy: {detail}"
    );
}

#[cfg(test)]
#[test]
fn restore_rejects_a_path_that_escapes_the_directory() {
    let (_dir, engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());
    block_on(send(
        &app,
        Call::post(ROUTE_BACKUPS_CREATE, "").html().cookie(cookie),
    ));
    let paths = engine.paths();
    let before = sha256_hex(&std::fs::read(&paths.db_file).expect("read the database"));
    let dir = directory(paths);
    let names_before = std::fs::read_dir(&dir).expect("archive dir").count();

    for escaped in ["../../etc/passwd", "/etc/passwd", "..", "../eris.db"] {
        let reply = block_on(send(
            &app,
            Call::post(
                ROUTE_BACKUPS_RESTORE,
                &format!("archive={}&confirm={}", encode(escaped), encode(escaped)),
            )
            .html()
            .cookie(cookie),
        ));
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{escaped} must be refused: {}",
            reply.body
        );
        assert!(
            reply.body.contains("bad_archive"),
            "{escaped} is refused by name: {}",
            reply.body
        );
        assert!(
            !reply.body.contains("/etc/passwd"),
            "the refusal does not echo a path back: {}",
            reply.body
        );
    }

    // Nothing was read into place, nothing was copied, nothing was moved.
    assert_eq!(
        sha256_hex(&std::fs::read(&paths.db_file).expect("read the database")),
        before
    );
    assert!(safety_copies(paths).is_empty());
    assert_eq!(
        std::fs::read_dir(&dir).expect("archive dir").count(),
        names_before,
        "the archive directory is unchanged"
    );
    assert!(!paths.state_dir.join("etc").exists());
}

#[cfg(test)]
#[test]
fn a_tampered_archive_is_refused_and_nothing_is_placed() {
    let (_dir, engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());
    block_on(send(
        &app,
        Call::post(ROUTE_BACKUPS_CREATE, "").html().cookie(cookie),
    ));
    let paths = engine.paths();
    let dir = directory(paths);
    let name = only_archive(paths);
    let before = sha256_hex(&std::fs::read(&paths.db_file).expect("read the database"));

    // A copy inside the directory with one payload byte flipped. Flipping a byte
    // past the manifest cannot change the manifest digest, so this is the
    // member-checksum path: the one that has to fail closed and write nothing.
    let tampered = dir.join("tampered-copy.bk");
    let opened = open(&dir.join(&name)).expect("open the original");
    let mut bytes = std::fs::read(dir.join(&name)).expect("read the original");
    let offset = (opened.payload_offset as usize) + 4;
    bytes[offset] ^= 0xff;
    std::fs::write(&tampered, &bytes).expect("write the tampered copy");

    let reply = block_on(send(
        &app,
        Call::post(
            ROUTE_BACKUPS_RESTORE,
            &format!(
                "archive={}&confirm={}",
                encode("tampered-copy.bk"),
                encode("tampered-copy.bk")
            ),
        )
        .html()
        .cookie(cookie),
    ));
    assert_eq!(
        reply.status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a verification failure is its own status class: {}",
        reply.body
    );
    let lowered = reply.body.to_ascii_lowercase();
    assert!(
        [
            "member_checksum_mismatch",
            "payload_unreadable",
            "manifest_digest_mismatch",
            "manifest_invalid",
        ]
        .iter()
        .any(|code| lowered.contains(code)),
        "the verification code is shown: {}",
        reply.body
    );

    // Nothing was placed, and the refused archive is still where it was so a
    // person can see what failed.
    assert_eq!(
        sha256_hex(&std::fs::read(&paths.db_file).expect("read the database")),
        before,
        "the live database is untouched"
    );
    assert!(
        safety_copies(paths).is_empty(),
        "a refused restore takes no safety copy"
    );
    let staging: Vec<String> = std::fs::read_dir(&paths.state_dir)
        .expect("state dir")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".restore-staging-"))
        .collect();
    assert!(staging.is_empty(), "no staging leftovers: {staging:?}");
    assert!(tampered.exists(), "the refused archive is left in place");

    // And the CLI's distinction is visible in the body, not just the status.
    assert!(
        reply.body.contains(&EXIT_VERIFICATION.to_string()),
        "the verification exit class is visible: {}",
        reply.body
    );

    std::fs::remove_file(&tampered).expect("clean up the tampered copy");
}

#[cfg(test)]
#[test]
fn the_nav_has_no_disabled_entries_left() {
    let (_dir, _engine, app, cookie) = ready_app();
    let cookie = Some(cookie.as_str());
    let pages = [
        ROUTE_DASHBOARD,
        ROUTE_SESSIONS,
        ROUTE_DEVICES,
        ROUTE_SETTINGS,
        ROUTE_ACTIVITY,
        ROUTE_BACKUPS,
    ];
    // The rail no longer carries the admin nav: settings is the index of every
    // section, and every page keeps the cog back to it.
    let settings = block_on(send(&app, Call::get(ROUTE_SETTINGS).html().cookie(cookie)));
    assert_eq!(settings.status, StatusCode::OK, "{}", settings.body);
    for link in pages {
        assert!(
            settings.body.contains(&format!(r#"href="{link}""#)),
            "settings links {link}: {}",
            settings.body
        );
    }
    for route in pages {
        let page = block_on(send(&app, Call::get(route).html().cookie(cookie)));
        assert_eq!(page.status, StatusCode::OK, "{route}: {}", page.body);
        assert_eq!(
            page.body.matches(r#"aria-disabled="true""#).count(),
            0,
            "{route} has no disabled nav entry: {}",
            page.body
        );
        assert!(
            page.body.contains(&format!(r#"href="{ROUTE_SETTINGS}""#)),
            "{route} keeps the cog: {}",
            page.body
        );
    }
}

/// `ROUTE_BACKUPS` is a real page, so it needs a session like every other one.
#[cfg(test)]
#[test]
fn the_page_needs_a_session_like_every_other_dashboard_page() {
    let (_dir, _engine, app, _cookie) = ready_app();
    let reply = block_on(send(&app, Call::get(ROUTE_BACKUPS)));
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.body);
    assert_eq!(reply.json()["code"], "no_session");
}
