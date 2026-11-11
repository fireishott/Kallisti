//! Skills: the operator's own procedures, offered to a turn by name.
//!
//! A skill is a directory holding a `SKILL.md`. The file opens with a small
//! frontmatter block (`name`, `description`) and then the procedure itself. This
//! module is the whole of it: where the directories are, which of them are
//! switched on, the catalog that goes into a turn's prompt, and the read a turn
//! does when it decides a skill actually applies.
//!
//! Four rules are enforced here rather than in a route, because a route is not the
//! only caller and the second caller is the interesting one:
//!
//! - **The catalog is names and descriptions, never the bodies.** A library of
//!   forty skills is forty procedures, and injecting them into every turn would
//!   spend the context window on work nobody asked for. The turn reads one on
//!   demand, within the caps here.
//! - **The key is a directory name, and it is validated.** A model-supplied key is
//!   untrusted input exactly like a model-supplied path: it is refused unless it is
//!   a plain slug naming a directory this module already listed, so a read cannot
//!   walk out of the library.
//! - **A skill with no description is listed by name only.** Inventing a
//!   description for somebody else's file would be the daemon writing the
//!   operator's documentation for them.
//! - **Switched off means absent.** A disabled skill is not in the catalog and
//!   cannot be read by name. Half-off is not a state.
//!
//! A skill is editable and removable from the daemon since P22: [`write`] replaces
//! the whole file atomically, and [`delete`] moves the directory into `.archive/`
//! rather than unlinking it. Both live here rather than in a route, so the device
//! API and the dashboard cannot drift apart, and both are one implementation of
//! the same rule: a write never follows a symlink, never leaves the library, and a
//! delete is a move, because nothing in this daemon deletes.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::db::Db;
use crate::web::store;

/// The global switch. Off means no catalog and no `read_skill` tool.
pub const SETTING_ENABLED: &str = "skills.enabled";

/// The directory the library is read from, when the operator wants a specific one.
pub const SETTING_DIR: &str = "skills.dir";

/// The prefix of a per-skill switch: `skills.<key>.enabled`.
pub const SETTING_PREFIX: &str = "skills.";

/// The file a skill lives in.
pub const FILE_NAME: &str = "SKILL.md";

/// The largest SKILL.md this module will read, in bytes.
pub const MAX_SKILL_BYTES: u64 = 64 * 1024;

/// How much of a skill a turn receives in one call.
pub const MAX_READ_BYTES: usize = 24 * 1024;

/// The ceiling on the library itself, as a guard against a directory somebody
/// pointed at `/` by accident.
///
/// This is no longer the size of the prompt block. It bounds how much of a
/// pathological library `list` will describe, which is a different concern from
/// how much of a healthy one a turn should carry.
pub const MAX_CATALOG: usize = 400;

/// How many skills one prompt block names.
///
/// On this host 126 skills were installed, 102 of them never read once, and every
/// one of them was paid for on every model call. Forty ordered by what is actually
/// used keeps the block to the part of the library that earns its tokens, and the
/// block states the remainder so a turn can ask for the library instead of
/// assuming a skill is absent. Roughly 120 bytes a line puts the block near five
/// kilobytes, which is a fraction of what the catalog used to cost.
pub const MAX_CATALOG_PROMPT: usize = 40;

/// The longest description the catalog keeps for one skill, in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 200;

/// Why a skill library could not be read or a skill could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    /// The library directory does not exist, or is not a directory.
    #[error(
        "the skill library `{path}` is not a directory; set the directory on the settings page"
    )]
    NoLibrary {
        /// The path that was checked.
        path: String,
    },
    /// The key is not a plain slug, or names nothing in the library.
    #[error("no skill named `{key}` is in this library")]
    Unknown {
        /// What the caller asked for.
        key: String,
    },
    /// The file is larger than [`MAX_SKILL_BYTES`].
    #[error(
        "`{key}` is {bytes} bytes, over the {limit}-byte ceiling for a skill; nothing was read"
    )]
    TooLarge {
        /// The skill's key.
        key: String,
        /// Its size on disk.
        bytes: u64,
        /// The ceiling that was applied.
        limit: u64,
    },
    /// The key is not a plain slug, so it cannot name a directory here.
    #[error(
        "`{key}` is not a usable skill key: letters, digits, dash, underscore or dot, at most 80 \
         characters, and not starting with a dot"
    )]
    KeyInvalid {
        /// What the caller asked for.
        key: String,
    },
    /// The body handed to a write carried no procedure.
    #[error("the body of `{key}` is blank, and a SKILL.md must carry its procedure")]
    Empty {
        /// The skill the write named.
        key: String,
    },
    /// The body handed to a write is over [`MAX_SKILL_BYTES`].
    #[error(
        "the body is {bytes} bytes, over the {limit}-byte ceiling for a skill; nothing was written"
    )]
    BodyTooLarge {
        /// The size that was offered.
        bytes: u64,
        /// The ceiling that was applied.
        limit: u64,
    },
    /// The path involved is a symlink, and this module will not follow one.
    #[error("`{path}` is a symlink, and this daemon does not write or archive through a link")]
    Symlink {
        /// The path that was checked.
        path: String,
    },
    /// The file exists and could not be read.
    #[error("{path}: {detail}")]
    Io {
        /// The path involved.
        path: String,
        /// The underlying message.
        detail: String,
    },
    /// The database refused.
    #[error("database: {0}")]
    Db(#[from] crate::db::DbError),
}

/// One skill, as the settings page and the catalog render it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// The directory name, and the name a turn reads it by.
    pub key: String,
    /// The `name` from the frontmatter, or the key when it carries none.
    pub name: String,
    /// The `description` from the frontmatter, when there is one.
    pub description: Option<String>,
    /// Where the file is, for the page that says so.
    pub path: String,
    /// Whether the operator has it switched on.
    pub enabled: bool,
    /// The file's size in bytes.
    pub bytes: u64,
    /// Who wrote it: the operator, or the learning loop. Taken from the ledger,
    /// and a skill the ledger has never seen is the operator's.
    pub origin: crate::learning::Origin,
    /// Where it is in the lifecycle: active, stale, or archived.
    pub state: crate::learning::State,
    /// Whether automatic work is forbidden from touching it.
    pub pinned: bool,
    /// Whether this daemon will accept a write to it. A skill listed here has a
    /// file this daemon can replace, so the answer is yes for every row; the
    /// field is published anyway, so a client never has to infer it from a state.
    pub editable: bool,
    /// How many times a turn has read it, from the ledger.
    ///
    /// The catalog orders by this, so it has to be on the skill rather than
    /// looked up again at render time. A skill the ledger has never seen is zero.
    pub use_count: i64,
    /// When a turn last read it, from the ledger, ISO 8601 UTC.
    pub last_used: Option<String>,
}

/// Whether the feature is on at all.
#[must_use]
pub fn enabled(stored: Option<&str>) -> bool {
    !matches!(stored.map(str::trim), Some("false"))
}

/// Whether one skill is on. A skill with no row is on.
#[must_use]
pub fn skill_enabled(stored: Option<&str>) -> bool {
    !matches!(stored.map(str::trim), Some("false"))
}

/// The settings key for one skill's switch.
#[must_use]
pub fn setting_key(key: &str) -> String {
    format!("{SETTING_PREFIX}{key}.enabled")
}

/// The default library, when the operator has not named one.
///
/// `~/.config/kallisti/skills` is where this daemon's own library lives, and it is
/// the only answer: the directory is seeded from the shipped skills when it is
/// absent, then returned whether or not it turned out to be writable. There is no
/// second candidate. A library the operator did not write for this daemon is not
/// something the settings page should print into its own shell, and a directory
/// that exists but carries no `SKILL.md` files is still the operator's own and
/// wins.
#[must_use]
pub fn default_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let owned = home.join(".config/kallisti/skills");
    seed_owned(&owned);
    Some(owned)
}

/// The skills this daemon ships with, compiled into the binary.
///
/// The library is a directory the daemon reads and never writes, so a fresh
/// install used to start with an empty one and a capability the iOS app
/// advertises with nothing behind it. Shipping the file in the repo is not
/// enough on its own: a prebuilt-binary install never sees the checkout. The
/// body is therefore `include_str!` at the same path the repo tracks, so the
/// binary and the tracked file cannot drift, and the seed only ever writes
/// into the owned directory, never over an operator's own copy.
const DEFAULT_SKILLS: &[(&str, &str)] = &[
    (
        "kallisti-ios",
        include_str!("../../../skills/kallisti-ios/SKILL.md"),
    ),
    (
        "craft-anything",
        include_str!("../../../skills/craft-anything/SKILL.md"),
    ),
];

/// Place the shipped skills into the owned library on first run.
///
/// Best effort by design: a read-only or unwritable `$HOME` leaves the library
/// as it was and the catalog simply names fewer skills, which is a smaller
/// failure than refusing to boot.
fn seed_owned(dir: &Path) {
    for (key, body) in DEFAULT_SKILLS {
        let target = dir.join(key);
        let path = target.join(FILE_NAME);
        if path.exists() {
            continue;
        }
        if std::fs::create_dir_all(&target).is_ok() {
            let _ = std::fs::write(&path, body);
        }
    }
}

/// The library actually in use: the stored directory, else the default.
#[must_use]
pub fn library_dir(stored: Option<&str>) -> Option<PathBuf> {
    match stored.map(str::trim).filter(|raw| !raw.is_empty()) {
        Some(raw) => Some(expand_tilde(raw)),
        None => default_dir(),
    }
}

/// Expand a leading `~` against `HOME`. No other shell syntax is honoured.
fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(raw)
}

/// Whether a key is a plain slug: letters, digits, `-`, `_`, `.`.
#[must_use]
pub fn valid_key(key: &str) -> bool {
    let trimmed = key.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 80
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        && !trimmed.starts_with('.')
}

/// Split a `SKILL.md` into its frontmatter fields and its body.
fn split(text: &str) -> (BTreeMap<String, String>, &str) {
    let mut fields = BTreeMap::new();
    let trimmed = text.trim_start();
    let Some(rest) = trimmed.strip_prefix("---") else {
        return (fields, text);
    };
    let Some(rest) = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))
    else {
        return (fields, text);
    };
    let Some(end) = rest.find("\n---") else {
        return (fields, text);
    };
    for line in rest[..end].lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        if key.is_empty() || key.contains(char::is_whitespace) {
            continue;
        }
        let value = value.trim().trim_matches('"').trim_matches('\'').trim();
        if !value.is_empty() {
            fields.insert(key, value.to_owned());
        }
    }
    let body = rest[end + 4..].trim_start_matches(['\r', '\n']);
    (fields, body)
}

/// One directory's worth of metadata, plus the head of the file it read.
fn describe(dir: &Path, key: &str, enabled: bool) -> Option<Skill> {
    let path = dir.join(key).join(FILE_NAME);
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let head = std::fs::File::open(&path).ok().and_then(|file| {
        use std::io::Read;
        let mut buffer = Vec::new();
        file.take(8 * 1024).read_to_end(&mut buffer).ok()?;
        Some(String::from_utf8_lossy(&buffer).into_owned())
    });
    let (fields, _) = head.as_deref().map(split).unwrap_or_default();
    let name = fields
        .get("name")
        .cloned()
        .unwrap_or_else(|| key.to_owned());
    let description = fields.get("description").cloned().map(|text| {
        let mut clipped: String = text.chars().take(MAX_DESCRIPTION_CHARS).collect();
        if text.chars().count() > MAX_DESCRIPTION_CHARS {
            clipped.push_str("...");
        }
        clipped
    });
    Some(Skill {
        key: key.to_owned(),
        name,
        description,
        path: path.display().to_string(),
        enabled,
        bytes: meta.len(),
        // The ledger overlay happens in `list`, which is the only caller with a
        // database. A skill with no row keeps exactly these values, which is the
        // documented default: the operator's, active, unpinned.
        origin: crate::learning::Origin::Operator,
        state: crate::learning::State::Active,
        pinned: false,
        editable: true,
        use_count: 0,
        last_used: None,
    })
}

/// Every skill in the library, by key, with the operator's switches applied.
///
/// A directory with no `SKILL.md` is not a skill and is skipped, so a stray
/// `README` or a `.git` never shows up as one.
///
/// # Errors
///
/// [`SkillError::NoLibrary`] when the directory is missing or is not a directory.
pub fn list(db: &Db, stored_dir: Option<&str>) -> Result<(Vec<Skill>, PathBuf), SkillError> {
    let dir = library_dir(stored_dir).unwrap_or_else(|| PathBuf::from("."));
    if !dir.is_dir() {
        return Err(SkillError::NoLibrary {
            path: dir.display().to_string(),
        });
    }
    let switches = store::settings_all(db).unwrap_or_default();
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Err(SkillError::NoLibrary {
            path: dir.display().to_string(),
        });
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let Some(key) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !valid_key(&key) {
            continue;
        }
        let on = skill_enabled(switches.get(&setting_key(&key)).map(String::as_str));
        if let Some(skill) = describe(&dir, &key, on) {
            out.push(skill);
        }
    }
    // The ledger is the only place origin, state and pinning are recorded, and a
    // skill it has never seen is the operator's: the loop cannot have written a
    // file it did not write. A ledger that cannot be read is not a reason to hide
    // the library, so the default stands and the page still draws.
    let ledger: BTreeMap<String, crate::learning::Usage> = crate::learning::rows(db)
        .unwrap_or_default()
        .into_iter()
        .map(|row| (row.key.clone(), row))
        .collect();
    for skill in &mut out {
        if let Some(row) = ledger.get(&skill.key) {
            skill.origin = row.created_by;
            skill.state = row.state;
            skill.pinned = row.pinned;
            skill.use_count = row.use_count;
            skill.last_used = row.last_used_at.clone();
        }
    }
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok((out, dir))
}

/// The catalog a turn carries, or `None` when there is nothing to say.
///
/// This block is inserted into the system prompt on every single model call, so
/// its size is a standing cost and its ordering is a decision. Two rules follow
/// from that:
///
/// - an archived skill is not listed. It is out of the library by definition, and
///   listing it would be paying tokens to advertise something the operator cannot
///   use;
/// - what does get listed is the part of the library a turn is most likely to
///   need: pinned first, then most-read, then most-recently-read. Before this,
///   the list was the first forty skills alphabetically, which on this host meant
///   paying for 100 entries nothing had ever opened.
///
/// [`MAX_CATALOG_PROMPT`] bounds the block; [`MAX_CATALOG`] stays the ceiling on
/// the library itself. When more is installed than is listed, the block says how
/// many are missing, so a turn knows to widen its search by asking for the
/// library rather than concluding a skill does not exist.
#[must_use]
pub fn catalog(skills: &[Skill]) -> Option<String> {
    let mut live: Vec<&Skill> = skills
        .iter()
        .filter(|skill| skill.enabled && !matches!(skill.state, crate::learning::State::Archived))
        .collect();
    if live.is_empty() {
        return None;
    }
    live.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| b.use_count.cmp(&a.use_count))
            // A read timestamp is ISO 8601 UTC, so lexicographic order is
            // chronological and `Some` sorts above `None`.
            .then_with(|| b.last_used.cmp(&a.last_used))
            .then_with(|| a.key.cmp(&b.key))
    });
    let listed = live.len().min(MAX_CATALOG_PROMPT);
    let mut text = String::from(
        "Procedures this operator has written, available on demand. This is names and \
         descriptions only: call `read_skill` with a key before following one, and do not follow a \
         skill whose description does not fit the task in front of you.",
    );
    for skill in live.iter().take(listed) {
        text.push_str("\n\n- `");
        text.push_str(&skill.key);
        text.push('`');
        if skill.name != skill.key {
            text.push_str(" (");
            text.push_str(&skill.name);
            text.push(')');
        }
        if let Some(description) = &skill.description {
            text.push_str(": ");
            text.push_str(description);
        }
    }
    if live.len() > listed {
        text.push_str(&format!(
            "\n\n{} more skill(s) are in the library but not listed here.",
            live.len() - listed
        ));
    }
    Some(text)
}

/// The catalog for this database and directory setting. A library that cannot be
/// read is `None`: a turn still answers, and the settings page reports why.
#[must_use]
pub fn catalog_for(db: &Db, on: bool, stored_dir: Option<&str>) -> Option<String> {
    if !on {
        return None;
    }
    let (skills, _) = list(db, stored_dir).ok()?;
    catalog(&skills)
}

/// Read one skill's body, for a turn that decided it applies.
///
/// # Errors
///
/// [`SkillError::Unknown`], [`SkillError::TooLarge`] or [`SkillError::Io`].
pub fn read(db: &Db, stored_dir: Option<&str>, key: &str) -> Result<String, SkillError> {
    let key = key.trim();
    if !valid_key(key) {
        return Err(SkillError::Unknown {
            key: key.to_owned(),
        });
    }
    let (skills, dir) = list(db, stored_dir)?;
    // Switched off means absent: a disabled skill is out of the catalog and
    // cannot be read by name, so half-off is never a state a turn can reach.
    if !skills.iter().any(|skill| skill.key == key && skill.enabled) {
        return Err(SkillError::Unknown {
            key: key.to_owned(),
        });
    }
    let path = dir.join(key).join(FILE_NAME);
    let bytes = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    if bytes > MAX_SKILL_BYTES {
        return Err(SkillError::TooLarge {
            key: key.to_owned(),
            bytes,
            limit: MAX_SKILL_BYTES,
        });
    }
    let text = std::fs::read_to_string(&path).map_err(|error| SkillError::Io {
        path: path.display().to_string(),
        detail: error.to_string(),
    })?;
    let (_, body) = split(&text);
    let body = body.trim();
    let body = if body.len() <= MAX_READ_BYTES {
        body.to_owned()
    } else {
        // Truncation is stated, not silent: a procedure that stops mid-step has to
        // say that it stopped.
        let cut = floor_boundary(body, MAX_READ_BYTES);
        format!(
            "{}\n\n[truncated: the skill is {} bytes and this is the first {}]",
            &body[..cut],
            body.len(),
            cut
        )
    };
    // A read is the only signal the ledger has that a skill is in use. Best
    // effort by contract: a ledger write that failed must never make a skill
    // unreadable, so the error is dropped here and the next pass reconciles.
    let _ = crate::learning::note_use(db, key);
    Ok(body)
}

/// The last byte index at or below `limit` that is a character boundary.
fn floor_boundary(text: &str, limit: usize) -> usize {
    let mut cut = limit.min(text.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
}

// ---------------------------------------------------------------------------
// Writing a skill, and the archive move that stands in for a delete
// ---------------------------------------------------------------------------

/// Refuse a library entry that is a link before anything else judges the key.
///
/// [`list`] skips a link, so an entry that is one is absent from the membership
/// check and would answer "no such skill" for a path that exists on disk. Naming
/// the refusal here is what makes the answer the useful one: the operator is told
/// the daemon will not write through a link, not that their directory vanished.
fn refuse_linked_entry(library: &Path, key: &str) -> Result<(), SkillError> {
    let target = library.join(key);
    match std::fs::symlink_metadata(&target) {
        Ok(meta) if meta.file_type().is_symlink() => Err(SkillError::Symlink {
            path: target.display().to_string(),
        }),
        _ => Ok(()),
    }
}

/// The skill's directory, resolved and checked: inside the library, and not a
/// link.
///
/// A key that passes [`valid_key`] cannot walk out of the library by spelling,
/// but the directory it names can still be a symlink, and a symlink is how a
/// write would land somewhere the operator never pointed at. Both the directory
/// and the file inside it are checked, and the real path is re-tested against the
/// library for the case where the link is reached through a parent.
fn directory_in_library(library: &Path, key: &str) -> Result<PathBuf, SkillError> {
    let target = library.join(key);
    let Ok(meta) = std::fs::symlink_metadata(&target) else {
        return Err(SkillError::Unknown {
            key: key.to_owned(),
        });
    };
    if meta.file_type().is_symlink() {
        return Err(SkillError::Symlink {
            path: target.display().to_string(),
        });
    }
    if !meta.is_dir() {
        return Err(SkillError::Unknown {
            key: key.to_owned(),
        });
    }
    let root = std::fs::canonicalize(library).map_err(|error| SkillError::Io {
        path: library.display().to_string(),
        detail: error.to_string(),
    })?;
    let real = std::fs::canonicalize(&target).map_err(|error| SkillError::Io {
        path: target.display().to_string(),
        detail: error.to_string(),
    })?;
    if !real.starts_with(&root) {
        return Err(SkillError::Symlink {
            path: target.display().to_string(),
        });
    }
    // A skill file that is itself a link would be replaced through the link by a
    // plain rename, which writes the link's target instead of the skill.
    let file = real.join(FILE_NAME);
    if let Ok(meta) = std::fs::symlink_metadata(&file)
        && meta.file_type().is_symlink()
    {
        return Err(SkillError::Symlink {
            path: file.display().to_string(),
        });
    }
    Ok(real)
}

/// Replace one skill's whole file, atomically.
///
/// The body is the complete `SKILL.md`, frontmatter included, and it is written
/// exactly as given: this module does not compose a file the operator did not
/// write. The temporary file sits in the skill's own directory so the rename is
/// a rename and not a copy, the old file is replaced in one step, and a crash
/// between the two leaves the previous version intact.
///
/// # Errors
///
/// [`SkillError::KeyInvalid`], [`SkillError::Empty`], [`SkillError::BodyTooLarge`],
/// [`SkillError::Unknown`] for a key that names nothing in the library,
/// [`SkillError::Symlink`] when either path is a link, and [`SkillError::Io`].
pub fn write(
    db: &Db,
    stored_dir: Option<&str>,
    key: &str,
    content: &str,
) -> Result<u64, SkillError> {
    let key = key.trim();
    if !valid_key(key) {
        return Err(SkillError::KeyInvalid {
            key: key.to_owned(),
        });
    }
    if content.trim().is_empty() {
        return Err(SkillError::Empty {
            key: key.to_owned(),
        });
    }
    let bytes = content.len() as u64;
    if bytes > MAX_SKILL_BYTES {
        return Err(SkillError::BodyTooLarge {
            bytes,
            limit: MAX_SKILL_BYTES,
        });
    }
    let (skills, dir) = list(db, stored_dir)?;
    refuse_linked_entry(&dir, key)?;
    if !skills.iter().any(|skill| skill.key == key) {
        return Err(SkillError::Unknown {
            key: key.to_owned(),
        });
    }
    let directory = directory_in_library(&dir, key)?;
    let path = directory.join(FILE_NAME);
    write_atomic(&path, content)?;
    // The ledger learns about the write after the bytes are in place, and a
    // ledger that refuses must not turn a completed write into a failure: the
    // same best-effort contract as a read. `record_written` only updates a row
    // that exists, so editing the operator's own skill creates no row and its
    // origin stays the operator's.
    let (fields, _) = split(content);
    let _ = crate::learning::record_written(db, key, fields.get("description").map(String::as_str));
    Ok(bytes)
}

/// Write one file through a temporary in the same directory, then rename.
fn write_atomic(path: &Path, text: &str) -> Result<(), SkillError> {
    let directory = path.parent().unwrap_or(Path::new("."));
    let temporary = directory.join(format!(".{FILE_NAME}.{}.tmp", std::process::id()));
    // A temporary from an earlier crash is replaced rather than appended to.
    let _ = std::fs::remove_file(&temporary);
    let written = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            // A skill is the operator's own text and is read by their editor, so
            // it is created world-readable and never executable, whatever the
            // process umask says.
            options.mode(0o644);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(SkillError::Io {
            path: temporary.display().to_string(),
            detail: error.to_string(),
        });
    }
    std::fs::rename(&temporary, path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        SkillError::Io {
            path: path.display().to_string(),
            detail: error.to_string(),
        }
    })
}

/// Delete one skill from the library, for real.
///
/// The directory and its `SKILL.md` are removed from disk, and the ledger row is
/// dropped so the curator and the learning loop cannot file the key back. This is
/// irreversible: the `.archive/` directory is the curator's ageing attic, not a
/// trash can, and [`crate::learning::restore`] has nothing to put back once this
/// has run. An operator who wants a copy keeps one before pressing the button.
///
/// The ledger row goes first. A row that outlives its directory is a key the
/// learning loop may write again, which would look like a delete that failed.
///
/// # Errors
///
/// [`SkillError::KeyInvalid`], [`SkillError::Unknown`] when the key names nothing
/// in the library, [`SkillError::Symlink`], [`SkillError::Io`] when the directory
/// cannot be removed, or [`SkillError::Db`] when the ledger row cannot be dropped.
pub fn delete(db: &Db, stored_dir: Option<&str>, key: &str) -> Result<String, SkillError> {
    let key = key.trim();
    if !valid_key(key) {
        return Err(SkillError::KeyInvalid {
            key: key.to_owned(),
        });
    }
    let (skills, dir) = list(db, stored_dir)?;
    refuse_linked_entry(&dir, key)?;
    if !skills.iter().any(|skill| skill.key == key) {
        return Err(SkillError::Unknown {
            key: key.to_owned(),
        });
    }
    let target = directory_in_library(&dir, key)?;
    if crate::learning::usage(db, key).ok().flatten().is_some() {
        crate::learning::forget(db, key)?;
    }
    std::fs::remove_dir_all(&target).map_err(|error| SkillError::Io {
        path: target.display().to_string(),
        detail: error.to_string(),
    })?;
    Ok(target.display().to_string())
}

/// The tool name, in one place because the spec and the dispatcher must agree.
pub const TOOL: &str = "read_skill";

/// The tool spec a turn is offered when a catalog exists.
#[must_use]
pub fn spec() -> core_runtime::provider::ToolSpec {
    core_runtime::provider::ToolSpec {
        name: TOOL.to_owned(),
        description: "Read one of the operator's skills by key, and follow it. Call this before \
                      acting on a skill named in the skill catalog."
            .to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "The skill key, exactly as the catalog spells it."
                }
            },
            "required": ["key"]
        }),
    }
}

/// Everything a turn needs to answer a `read_skill` call: the database the switches
/// live in, and the library directory as it was resolved for the turn.
#[derive(Debug, Clone)]
pub struct Access {
    /// The daemon's database.
    pub db: Db,
    /// The stored `skills.dir`, or `None` for the default.
    pub dir: Option<String>,
}

impl Access {
    /// Resolve an access from a database and the stored directory setting.
    #[must_use]
    pub fn new(db: Db, stored_dir: Option<String>) -> Self {
        Self {
            db,
            dir: stored_dir,
        }
    }

    /// Answer one call.
    #[must_use]
    pub fn call(&self, arguments: &serde_json::Value) -> crate::tools::ToolResult {
        let key = arguments
            .get("key")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if key.trim().is_empty() {
            return crate::tools::ToolResult::refused(
                "read_skill needs a key, exactly as the skill catalog spells it",
            );
        }
        match read(&self.db, self.dir.as_deref(), key) {
            Ok(body) => crate::tools::ToolResult::ok(serde_json::json!({
                "key": key.trim(),
                "body": body,
            })),
            Err(error) => crate::tools::ToolResult::refused(&error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("a temp dir");
        let library = dir.path().join("skills");
        std::fs::create_dir_all(&library).expect("the library");
        (dir, library)
    }

    fn write_skill(library: &Path, key: &str, front: &str, body: &str) {
        let directory = library.join(key);
        std::fs::create_dir_all(&directory).expect("the skill dir");
        std::fs::write(
            directory.join(FILE_NAME),
            format!("---\n{front}\n---\n\n{body}\n"),
        )
        .expect("the skill file");
    }

    fn db() -> Db {
        Db::open_in_memory().expect("in-memory db")
    }

    #[test]
    fn frontmatter_is_split_from_the_body() {
        let (fields, body) = split("---\nname: Deploy\ndescription: Ship it\n---\n\nRun this.\n");
        assert_eq!(fields.get("name").map(String::as_str), Some("Deploy"));
        assert_eq!(
            fields.get("description").map(String::as_str),
            Some("Ship it")
        );
        assert_eq!(body.trim(), "Run this.");
    }

    #[test]
    fn a_file_with_no_frontmatter_is_all_body() {
        let (fields, body) = split("Just steps.\n");
        assert!(fields.is_empty());
        assert_eq!(body, "Just steps.\n");
    }

    #[test]
    fn a_key_must_be_a_plain_slug() {
        assert!(valid_key("deploy-host"));
        assert!(valid_key("sports_warehouse.v2"));
        assert!(!valid_key("../etc"));
        assert!(!valid_key("a/b"));
        assert!(!valid_key(".hidden"));
        assert!(!valid_key(""));
    }

    #[test]
    fn the_library_lists_only_directories_holding_a_skill_file() {
        let (_dir, library) = scratch();
        write_skill(&library, "deploy", "name: Deploy", "steps");
        std::fs::create_dir_all(library.join("empty")).expect("an empty dir");
        std::fs::write(library.join("loose.md"), "no").expect("a loose file");

        let db = db();
        let (skills, dir) = list(&db, Some(&library.display().to_string())).expect("the library");
        assert_eq!(dir, library);
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].key, "deploy");
        assert_eq!(skills[0].name, "Deploy");
        assert!(skills[0].enabled, "a skill with no row is on");
    }

    #[test]
    fn a_disabled_skill_is_out_of_the_catalog_and_cannot_be_read() {
        let (_dir, library) = scratch();
        write_skill(&library, "deploy", "description: Ship it", "run the thing");
        write_skill(&library, "quiet", "description: Silent", "nothing");
        let db = db();
        let raw = library.display().to_string();
        store::settings_put(&db, &setting_key("quiet"), "false").expect("the switch");

        let (skills, _) = list(&db, Some(&raw)).expect("the library");
        let catalog = catalog(&skills).expect("a catalog");
        assert!(catalog.contains("`deploy`"), "{catalog}");
        assert!(!catalog.contains("`quiet`"), "{catalog}");
        assert!(matches!(
            read(&db, Some(&raw), "quiet"),
            Err(SkillError::Unknown { .. })
        ));
    }

    #[test]
    fn the_catalog_is_names_and_descriptions_and_never_a_body() {
        let (_dir, library) = scratch();
        write_skill(
            &library,
            "deploy",
            "description: Ship it",
            "SECRET STEP ORDER",
        );
        let db = db();
        let (skills, _) = list(&db, Some(&library.display().to_string())).expect("the library");
        let catalog = catalog(&skills).expect("a catalog");
        assert!(catalog.contains("Ship it"), "{catalog}");
        assert!(!catalog.contains("SECRET STEP ORDER"), "{catalog}");
        assert!(catalog.contains("read_skill"), "{catalog}");
    }

    /// The prompt block is bounded, and it says so out loud.
    ///
    /// This test used to assert the opposite: that a 120-skill library was named
    /// in full. On this host that meant paying for 100 entries nothing had ever
    /// opened, on every model call. The block now names the forty a turn is most
    /// likely to need and states the remainder, which is what keeps "absent" and
    /// "not listed" distinguishable to the model.
    #[test]
    fn a_library_past_the_prompt_ceiling_names_the_used_names_and_counts_the_rest() {
        let (_dir, library) = scratch();
        for index in 0..120 {
            write_skill(
                &library,
                &format!("skill-{index:03}"),
                &format!("description: number {index}"),
                "steps",
            );
        }
        let db = db();
        let (skills, _) = list(&db, Some(&library.display().to_string())).expect("the library");
        assert_eq!(skills.len(), 120);
        let catalog = catalog(&skills).expect("a catalog");
        let named = catalog.matches("- `skill-").count();
        assert_eq!(named, MAX_CATALOG_PROMPT, "{catalog}");
        assert!(
            catalog.contains(&format!("{} more skill(s)", 120 - MAX_CATALOG_PROMPT)),
            "the block states what it left out: {catalog}"
        );
    }

    #[test]
    fn an_archived_skill_is_not_in_the_catalog_and_use_orders_the_rest() {
        let (_dir, library) = scratch();
        write_skill(&library, "never-used", "description: one", "steps");
        write_skill(&library, "well-used", "description: two", "steps");
        write_skill(&library, "put-away", "description: three", "steps");
        let db = db();
        crate::learning::record_created(&db, "put-away", Some("three"), None).expect("ledger");
        crate::learning::set_state(&db, "put-away", crate::learning::State::Archived)
            .expect("archived");
        crate::learning::record_created(&db, "well-used", Some("two"), None).expect("ledger");
        crate::learning::note_use(&db, "well-used").expect("a read");
        crate::learning::note_use(&db, "well-used").expect("another read");

        let (skills, _) = list(&db, Some(&library.display().to_string())).expect("the library");
        let catalog = catalog(&skills).expect("a catalog");
        assert!(
            !catalog.contains("put-away"),
            "an archived skill is out of the library: {catalog}"
        );
        let used = catalog.find("well-used").expect("the read skill is listed");
        let unused = catalog
            .find("never-used")
            .expect("the unread skill is listed");
        assert!(
            used < unused,
            "what a turn has actually read comes first: {catalog}"
        );
    }

    #[test]
    fn reading_a_skill_drops_the_frontmatter_and_keeps_the_body() {
        let (_dir, library) = scratch();
        write_skill(&library, "deploy", "name: Deploy", "1. build\n2. ship");
        let db = db();
        let body = read(&db, Some(&library.display().to_string()), "deploy").expect("the body");
        assert!(body.starts_with("1. build"), "{body}");
        assert!(!body.contains("name: Deploy"), "{body}");
    }

    #[test]
    fn a_read_outside_the_library_is_refused() {
        let (_dir, library) = scratch();
        write_skill(&library, "deploy", "description: Ship it", "steps");
        let db = db();
        let raw = library.display().to_string();
        for key in ["../secrets", "/etc/passwd", "deploy/../deploy"] {
            assert!(
                matches!(read(&db, Some(&raw), key), Err(SkillError::Unknown { .. })),
                "{key} must be refused"
            );
        }
    }

    #[test]
    fn an_oversized_skill_is_refused_by_name() {
        let (_dir, library) = scratch();
        let directory = library.join("huge");
        std::fs::create_dir_all(&directory).expect("the dir");
        std::fs::write(
            directory.join(FILE_NAME),
            "x".repeat((MAX_SKILL_BYTES + 1) as usize),
        )
        .expect("the file");
        let db = db();
        let raw = library.display().to_string();
        let (skills, _) = list(&db, Some(&raw)).expect("the library");
        assert_eq!(skills.len(), 1);
        assert!(matches!(
            read(&db, Some(&raw), "huge"),
            Err(SkillError::TooLarge { .. })
        ));
    }

    #[test]
    fn a_missing_library_is_a_named_error() {
        let db = db();
        let missing = std::env::temp_dir().join("kallisti-no-such-library-0001");
        assert!(matches!(
            list(&db, Some(&missing.display().to_string())),
            Err(SkillError::NoLibrary { .. })
        ));
        assert!(catalog_for(&db, true, Some(&missing.display().to_string())).is_none());
    }

    #[test]
    fn the_global_switch_off_means_no_catalog() {
        let (_dir, library) = scratch();
        write_skill(&library, "deploy", "description: Ship it", "steps");
        let db = db();
        let raw = library.display().to_string();
        assert!(catalog_for(&db, false, Some(&raw)).is_none());
        assert!(catalog_for(&db, true, Some(&raw)).is_some());
    }

    #[test]
    fn an_access_answers_a_call_with_the_body() {
        let (_dir, library) = scratch();
        write_skill(&library, "deploy", "description: Ship it", "run the thing");
        let db = db();
        let access = Access::new(db, Some(library.display().to_string()));
        let result = access.call(&serde_json::json!({ "key": "deploy" }));
        assert!(!result.is_error, "{}", result.content);
        assert!(
            result.content.contains("run the thing"),
            "{}",
            result.content
        );
        let missing = access.call(&serde_json::json!({ "key": "nope" }));
        assert!(missing.is_error);
    }

    // -----------------------------------------------------------------------
    // The write path
    // -----------------------------------------------------------------------

    /// A complete SKILL.md, which is what a write takes.
    fn full_skill(name: &str, description: &str, body: &str) -> String {
        format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n")
    }

    #[test]
    fn a_write_refuses_a_key_that_is_not_a_slug() {
        let (_dir, library) = scratch();
        write_skill(&library, "deploy", "description: Ship it", "steps");
        let db = db();
        let raw = library.display().to_string();
        for key in ["../escape", "a/b", ".hidden", "", "with space"] {
            assert!(
                matches!(
                    write(&db, Some(&raw), key, &full_skill("X", "Y", "body")),
                    Err(SkillError::KeyInvalid { .. })
                ),
                "{key} must be refused before any file is touched"
            );
        }
        assert_eq!(
            read(&db, Some(&raw), "deploy").expect("the untouched skill"),
            "steps"
        );
    }

    #[test]
    fn a_write_refuses_an_empty_body() {
        let (_dir, library) = scratch();
        write_skill(&library, "deploy", "description: Ship it", "steps");
        let db = db();
        let raw = library.display().to_string();
        for body in ["", "   ", "\n\n\t\n"] {
            assert!(
                matches!(
                    write(&db, Some(&raw), "deploy", body),
                    Err(SkillError::Empty { .. })
                ),
                "{body:?} must be refused"
            );
        }
        assert_eq!(
            read(&db, Some(&raw), "deploy").expect("the untouched skill"),
            "steps"
        );
    }

    #[test]
    fn a_write_refuses_a_body_over_the_ceiling() {
        let (_dir, library) = scratch();
        write_skill(&library, "deploy", "description: Ship it", "steps");
        let db = db();
        let raw = library.display().to_string();
        let huge = "x".repeat((MAX_SKILL_BYTES + 1) as usize);
        assert!(matches!(
            write(&db, Some(&raw), "deploy", &huge),
            Err(SkillError::BodyTooLarge { .. })
        ));
        assert_eq!(
            read(&db, Some(&raw), "deploy").expect("the untouched skill"),
            "steps"
        );
    }

    #[test]
    fn a_write_replaces_the_whole_file_and_reports_its_size() {
        let (_dir, library) = scratch();
        write_skill(&library, "deploy", "description: Ship it", "old steps");
        let db = db();
        let raw = library.display().to_string();
        let text = full_skill("Deploy", "Ship it carefully", "new steps entirely");

        let bytes = write(&db, Some(&raw), "deploy", &text).expect("the write");
        assert_eq!(bytes, text.len() as u64);

        let on_disk = std::fs::read_to_string(library.join("deploy").join(FILE_NAME))
            .expect("the file is there");
        assert_eq!(on_disk, text, "the body is written exactly as offered");
        let (skills, _) = list(&db, Some(&raw)).expect("the library");
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].bytes, text.len() as u64);
        assert_eq!(skills[0].description.as_deref(), Some("Ship it carefully"));

        // No temporary is left behind, in the skill directory or the library.
        let leftovers: Vec<String> = std::fs::read_dir(library.join("deploy"))
            .expect("the skill dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != FILE_NAME)
            .collect();
        assert!(leftovers.is_empty(), "a temporary is left: {leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_written_skill_is_readable_and_not_executable() {
        use std::os::unix::fs::PermissionsExt as _;
        let (_dir, library) = scratch();
        write_skill(&library, "deploy", "description: Ship it", "steps");
        let db = db();
        let raw = library.display().to_string();
        write(
            &db,
            Some(&raw),
            "deploy",
            &full_skill("Deploy", "Ship it", "new"),
        )
        .expect("the write");
        let mode = std::fs::metadata(library.join("deploy").join(FILE_NAME))
            .expect("the file")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o644, "a skill file is 0644, got {mode:o}");
    }

    #[test]
    fn a_write_to_an_unknown_key_is_refused() {
        let (_dir, library) = scratch();
        write_skill(&library, "deploy", "description: Ship it", "steps");
        let db = db();
        let raw = library.display().to_string();
        assert!(matches!(
            write(
                &db,
                Some(&raw),
                "no-such-skill",
                &full_skill("X", "Y", "body")
            ),
            Err(SkillError::Unknown { .. })
        ));
        assert!(
            !library.join("no-such-skill").exists(),
            "a refused write creates nothing"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_skill_directory_is_refused_rather_than_followed() {
        let (root, library) = scratch();
        let outside = root.path().join("outside");
        std::fs::create_dir_all(&outside).expect("the outside dir");
        std::fs::write(
            outside.join(FILE_NAME),
            full_skill("Linked", "A link", "outside the library"),
        )
        .expect("the outside file");
        std::os::unix::fs::symlink(&outside, library.join("linked")).expect("the link");
        let db = db();
        let raw = library.display().to_string();

        assert!(matches!(
            write(&db, Some(&raw), "linked", &full_skill("X", "Y", "body")),
            Err(SkillError::Symlink { .. })
        ));
        assert!(matches!(
            delete(&db, Some(&raw), "linked"),
            Err(SkillError::Symlink { .. })
        ));
        let untouched = std::fs::read_to_string(outside.join(FILE_NAME)).expect("the outside file");
        assert!(
            untouched.contains("outside the library"),
            "the link target must not be written through: {untouched}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_skill_file_is_refused_rather_than_followed() {
        let (root, library) = scratch();
        let outside = root.path().join("outside.md");
        std::fs::write(&outside, "not a skill").expect("the outside file");
        let directory = library.join("deploy");
        std::fs::create_dir_all(&directory).expect("the skill dir");
        std::os::unix::fs::symlink(&outside, directory.join(FILE_NAME)).expect("the link");
        let db = db();
        let raw = library.display().to_string();

        assert!(matches!(
            write(&db, Some(&raw), "deploy", &full_skill("X", "Y", "body")),
            Err(SkillError::Symlink { .. })
        ));
        assert_eq!(
            std::fs::read_to_string(&outside).expect("the outside file"),
            "not a skill",
            "the link target must not be written through"
        );
    }

    // -----------------------------------------------------------------------
    // The archive delete
    // -----------------------------------------------------------------------

    #[test]
    fn deleting_a_skill_removes_its_directory_from_disk() {
        let (_dir, library) = scratch();
        write_skill(&library, "deploy", "description: Ship it", "steps");
        let db = db();
        let raw = library.display().to_string();

        let removed = delete(&db, Some(&raw), "deploy").expect("the delete");
        assert!(
            removed.ends_with("/deploy"),
            "the answer names the directory that went: {removed}"
        );
        assert!(!library.join("deploy").exists(), "the directory is gone");
        assert!(
            !library.join(crate::learning::ARCHIVE_DIR).exists(),
            "a delete is not an archive: nothing is left in .archive/"
        );

        let (skills, _) = list(&db, Some(&raw)).expect("the library");
        assert!(skills.is_empty(), "the skill is gone from the list");
    }

    #[test]
    fn deleting_a_learned_skill_drops_its_ledger_row_and_its_files() {
        let (_dir, library) = scratch();
        write_skill(&library, "learned-one", "description: Learned", "steps");
        let db = db();
        let raw = library.display().to_string();
        crate::learning::record_created(&db, "learned-one", Some("Learned"), None)
            .expect("a ledger row");
        assert!(
            crate::learning::usage(&db, "learned-one")
                .expect("db")
                .is_some(),
            "the row exists before the delete"
        );

        delete(&db, Some(&raw), "learned-one").expect("the delete");
        assert!(
            crate::learning::usage(&db, "learned-one")
                .expect("db")
                .is_none(),
            "the row is dropped, so the loop cannot grow the skill back"
        );
        assert!(
            !library.join("learned-one").exists(),
            "a learned skill is deleted for real, not archived"
        );
    }

    #[test]
    fn deleting_an_unknown_key_is_refused() {
        let (_dir, library) = scratch();
        write_skill(&library, "deploy", "description: Ship it", "steps");
        let db = db();
        let raw = library.display().to_string();
        assert!(matches!(
            delete(&db, Some(&raw), "no-such-skill"),
            Err(SkillError::Unknown { .. })
        ));
        assert!(matches!(
            delete(&db, Some(&raw), "../escape"),
            Err(SkillError::KeyInvalid { .. })
        ));
        assert!(library.join("deploy").is_dir(), "nothing was moved");
    }

    #[test]
    fn the_ledger_decides_origin_state_and_pinning() {
        let (_dir, library) = scratch();
        write_skill(&library, "operator-one", "description: Mine", "steps");
        write_skill(&library, "learned-one", "description: Its", "steps");
        let db = db();
        let raw = library.display().to_string();
        crate::learning::record_created(&db, "learned-one", Some("Its"), None)
            .expect("a ledger row");
        crate::learning::set_pinned(&db, "learned-one", true).expect("pin");
        crate::learning::set_state(&db, "learned-one", crate::learning::State::Stale)
            .expect("state");

        let (skills, _) = list(&db, Some(&raw)).expect("the library");
        let operator = skills
            .iter()
            .find(|skill| skill.key == "operator-one")
            .expect("the operator skill");
        assert_eq!(operator.origin, crate::learning::Origin::Operator);
        assert_eq!(operator.state, crate::learning::State::Active);
        assert!(!operator.pinned, "no row means unpinned");
        assert!(operator.editable);

        let learned = skills
            .iter()
            .find(|skill| skill.key == "learned-one")
            .expect("the learned skill");
        assert_eq!(learned.origin, crate::learning::Origin::Agent);
        assert_eq!(learned.state, crate::learning::State::Stale);
        assert!(learned.pinned);
        assert!(learned.editable);
    }

    /// A fresh install used to start with an empty library, so the capability
    /// the app advertises had nothing behind it. The seed must create the
    /// directory, write the shipped body, and then never touch it again: a
    /// second run leaves an operator's edited copy exactly as they left it.
    #[test]
    fn the_shipped_skills_are_seeded_once_and_an_operators_copy_wins() {
        let dir = tempfile::tempdir().expect("a temp dir");
        let library = dir.path().join("skills");
        assert!(!library.exists(), "the library starts absent");

        seed_owned(&library);

        let seeded = library.join("kallisti-ios").join(FILE_NAME);
        assert!(seeded.is_file(), "the shipped skill is written");
        let body = std::fs::read_to_string(&seeded).expect("the seeded body");
        assert!(
            body.contains("description:"),
            "the body keeps its frontmatter"
        );

        std::fs::write(&seeded, "---\ndescription: mine\n---\n\nmine\n").expect("the edit");
        seed_owned(&library);

        let after = std::fs::read_to_string(&seeded).expect("the edited body");
        assert!(
            after.contains("mine"),
            "the operator copy is not overwritten"
        );
        assert!(
            !after.contains("attach_file"),
            "the shipped body did not return"
        );
    }
}
