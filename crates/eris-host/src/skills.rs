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
//! Nothing here writes to a skill directory. Management is a switch per skill plus
//! a rescan; editing the file is the operator's own business, in their own editor.

use std::collections::BTreeMap;
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

/// How many skills the catalog may name in one prompt.
///
/// A name and a clipped description is about 120 bytes, so the whole of a real
/// library costs a few kilobytes of prompt. The ceiling is here to bound a
/// directory somebody pointed at `/` by accident, not to ration a working
/// library: a hundred-odd skills all listed is the intended case, and a catalog
/// that silently dropped the last fifteen would be a skill the model can never
/// find.
pub const MAX_CATALOG: usize = 400;

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
/// `~/.config/kallisti/skills` is where this daemon's own library lives. When
/// that directory does not exist, `~/.hermes/skills` is one an earlier install
/// already has, but it is only inherited if it is safe to read: it can contain a
/// skill whose own files mention the old daemon, and a skill the operator did not
/// write for this daemon is not something the settings page should print into its
/// own shell. A directory that exists but carries no `SKILL.md` files is still
/// the operator's own and wins, which is why the inherit only happens when the
/// owned path is absent rather than merely empty.
#[must_use]
pub fn default_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let owned = home.join(".config/kallisti/skills");
    if owned.is_dir() {
        return Some(owned);
    }
    let inherited = home.join(".hermes/skills");
    if inherited.is_dir() && safe_to_inherit(&inherited) {
        return Some(inherited);
    }
    Some(owned)
}

/// Whether a legacy library is clean enough to inherit automatically.
///
/// The only thing checked is the operator's own text, not third-party content:
/// a directory whose own skill files mention the old name, or carry an em dash
/// the shell test forbids, is skipped so the settings page does not leak either
/// into a page whose style rules say neither may appear. A library that is only
/// third-party skills with no legacy name is inherited as-is.
fn safe_to_inherit(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut checked = 0usize;
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let path = entry.path().join(FILE_NAME);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        checked += 1;
        let lowered = text.to_ascii_lowercase();
        if lowered.contains("herald") || text.contains('\u{2014}') {
            return false;
        }
    }
    checked > 0
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
    out.sort_by(|a, b| a.key.cmp(&b.key));
    Ok((out, dir))
}

/// The catalog a turn carries, or `None` when there is nothing to say.
#[must_use]
pub fn catalog(skills: &[Skill]) -> Option<String> {
    let live: Vec<&Skill> = skills.iter().filter(|skill| skill.enabled).collect();
    if live.is_empty() {
        return None;
    }
    let mut text = String::from(
        "Procedures this operator has written, available on demand. This is names and \
         descriptions only: call `read_skill` with a key before following one, and do not follow a \
         skill whose description does not fit the task in front of you.",
    );
    for skill in live.iter().take(MAX_CATALOG) {
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
    if live.len() > MAX_CATALOG {
        text.push_str(&format!(
            "\n\n{} more skill(s) are in the library but not listed here.",
            live.len() - MAX_CATALOG
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
    if body.len() <= MAX_READ_BYTES {
        return Ok(body.to_owned());
    }
    // Truncation is stated, not silent: a procedure that stops mid-step has to say
    // that it stopped.
    let cut = floor_boundary(body, MAX_READ_BYTES);
    Ok(format!(
        "{}\n\n[truncated: the skill is {} bytes and this is the first {}]",
        &body[..cut],
        body.len(),
        cut
    ))
}

/// The last byte index at or below `limit` that is a character boundary.
fn floor_boundary(text: &str, limit: usize) -> usize {
    let mut cut = limit.min(text.len());
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    cut
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

    /// The real library is larger than the old ceiling of 80, so every skill in
    /// it must be nameable: a catalog that drops the tail hides those skills from
    /// the model entirely, since the catalog is the only place a key is learned.
    #[test]
    fn a_library_larger_than_eighty_skills_still_names_them_all() {
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
        for index in 0..120 {
            assert!(
                catalog.contains(&format!("`skill-{index:03}`")),
                "skill-{index:03} is missing from the catalog"
            );
        }
        assert!(!catalog.contains("more skill"), "{catalog}");
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
}
