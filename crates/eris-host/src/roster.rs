//! The standing roster: named agents the model may hand work to.
//!
//! One row is one specialist the operator defined once instead of re-explaining
//! on every dispatch: a name, a role, standing instructions, a tool allowance,
//! and optionally a memory scope. A turn calls `dispatch_agent` with `agent:
//! "<name>"` and gets that standing brief instead of a bare prompt.
//!
//! Three rules live here rather than in the dispatch path, because the dashboard
//! and the dispatch path both need them and a rule that exists in one caller is a
//! rule the next one forgets.
//!
//! - **A name is canonical.** Lowercased, `[a-z0-9-]`, 1..32 characters. The
//!   index is `COLLATE NOCASE`, so lookup matches the stored spelling, and the
//!   stored spelling is the only one.
//! - **Bounds refuse, never truncate.** `role` and `instructions` hit a named
//!   error at their limits. A silently cut brief is a brief the operator did not
//!   write.
//! - **A scope stays inside the workspace.** `memory_scope` is a directory under
//!   the configured workspace root; at dispatch time it is canonicalised and
//!   prefix-checked, and a scope that resolves outside the root refuses the
//!   dispatch rather than confining the child to somewhere new.

use std::path::{Path, PathBuf};

use rusqlite::params;

use crate::db::{Db, DbError};

/// How long a name may be, in characters.
pub const MAX_NAME_CHARS: usize = 32;

/// How long a role may be, in characters.
pub const MAX_ROLE_CHARS: usize = 120;

/// How long standing instructions may be, in characters.
pub const MAX_INSTRUCTIONS_CHARS: usize = 4_000;

/// How many rows [`describe_for_prompt`] names before it stops.
pub const PROMPT_ROWS: usize = 12;

/// What a roster row may do with tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolPolicy {
    /// The caller's tool set: everything the parent turn could call, minus
    /// dispatching itself.
    #[default]
    Inherit,
    /// Reads only. `write_file` and `run_command` are not in the child's list,
    /// and the sandbox refuses a write even if one is invented.
    ReadOnly,
}

impl ToolPolicy {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::ReadOnly => "read_only",
        }
    }

    /// Parse a stored value. Anything outside the CHECK constraint is a fact
    /// about a hand-edited database, and it is reported rather than guessed at.
    pub fn parse(value: &str) -> Result<Self, RosterError> {
        match value {
            "inherit" => Ok(Self::Inherit),
            "read_only" => Ok(Self::ReadOnly),
            other => Err(RosterError::refused(
                "roster_tool_policy_invalid",
                format!("tool_policy must be 'inherit' or 'read_only', not '{other}'"),
            )),
        }
    }

    /// The sandbox mode a child of this policy runs under.
    ///
    /// `Inherit` is [`crate::tools::SandboxMode::Full`] because dispatching is
    /// only ever offered in `Full` mode: the caller's set is the full set.
    /// `ReadOnly` is the read-only mode, so a write is refused by the sandbox as
    /// well as absent from the tool list.
    #[must_use]
    pub fn sandbox_mode(self) -> crate::tools::SandboxMode {
        match self {
            Self::Inherit => crate::tools::SandboxMode::Full,
            Self::ReadOnly => crate::tools::SandboxMode::ReadOnly,
        }
    }
}

/// One standing agent, as the table holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    /// Opaque id.
    pub id: String,
    /// The canonical name the model dispatches with.
    pub name: String,
    /// One line saying what it is for.
    pub role: String,
    /// The standing brief its child turns carry.
    pub instructions: String,
    /// Its tool allowance.
    pub tool_policy: ToolPolicy,
    /// The model role it prefers, stored now and resolved in a later build.
    pub model_role: Option<String>,
    /// A directory under the workspace its child is confined to.
    pub memory_scope: Option<String>,
    /// Whether it may be dispatched at all.
    pub enabled: bool,
    /// When the row was written.
    pub created_at: String,
    /// When the row last changed.
    pub updated_at: String,
}

/// What one create or update asks for.
#[derive(Debug, Clone, Default)]
pub struct Draft {
    /// The name, canonicalised on the way in.
    pub name: String,
    /// One line saying what it is for.
    pub role: String,
    /// The standing brief.
    pub instructions: String,
    /// The tool allowance.
    pub tool_policy: ToolPolicy,
    /// The model role, stored but not yet resolved.
    pub model_role: Option<String>,
    /// The memory scope, checked at dispatch.
    pub memory_scope: Option<String>,
}

/// Why the roster refused something, in the two halves the model reads: a stable
/// code and a sentence that names the rule it hit.
#[derive(Debug, thiserror::Error)]
pub enum RosterError {
    /// A named input rule, or a dispatch rule, refused the request.
    #[error("{reason}")]
    Refused {
        /// A short code the model and a test can match on.
        code: &'static str,
        /// The sentence, which names the rule rather than saying "denied".
        reason: String,
    },
    /// The database could not answer.
    #[error("the roster could not be read: {0}")]
    Unavailable(#[from] DbError),
}

impl RosterError {
    /// A refusal with a code and a sentence.
    #[must_use]
    pub fn refused(code: &'static str, reason: impl Into<String>) -> Self {
        Self::Refused {
            code,
            reason: reason.into(),
        }
    }

    /// The code, for a caller that only has room for one string.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Refused { code, .. } => code,
            Self::Unavailable(_) => "roster_unavailable",
        }
    }

    /// The sentence.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Self::Refused { reason, .. } => reason.clone(),
            Self::Unavailable(error) => {
                format!("the roster could not be read ({error}), so nothing was dispatched")
            }
        }
    }
}

/// What dispatching to a standing agent needs from its row, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The canonical name, for the label.
    pub name: String,
    /// The standing brief the child carries.
    pub instructions: String,
    /// Its tool allowance.
    pub tool_policy: ToolPolicy,
    /// Where the child's file tools are rooted: the scope directory when the row
    /// has one, otherwise the workspace root.
    pub root: PathBuf,
}

/// The columns every read shares, so a new field cannot be added to one query and
/// forgotten in another.
const COLUMNS: &str = "id, name, role, instructions, tool_policy, model_role, memory_scope, enabled, created_at, updated_at";

/// Row to [`Agent`].
fn row_to_agent(row: &rusqlite::Row<'_>) -> rusqlite::Result<Agent> {
    let tool_policy: String = row.get(4)?;
    let enabled: i64 = row.get(7)?;
    Ok(Agent {
        id: row.get(0)?,
        name: row.get(1)?,
        role: row.get(2)?,
        instructions: row.get(3)?,
        tool_policy: ToolPolicy::parse(&tool_policy).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::other(error.to_string())),
            )
        })?,
        model_role: row.get(5)?,
        memory_scope: row.get(6)?,
        enabled: enabled != 0,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

/// The name as it is stored: trimmed and lowercased.
#[must_use]
pub fn canonical_name(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Check a draft against the boundary rules. Refuses with a named error; never
/// truncates.
pub fn validate(draft: &Draft) -> Result<(), RosterError> {
    let name = canonical_name(&draft.name);
    if name.is_empty() {
        return Err(RosterError::refused(
            "roster_name_invalid",
            "a roster name is required: 1 to 32 characters from a-z, 0-9 and -",
        ));
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(RosterError::refused(
            "roster_name_invalid",
            format!(
                "a roster name is at most {MAX_NAME_CHARS} characters and this one is {}",
                name.chars().count()
            ),
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(RosterError::refused(
            "roster_name_invalid",
            "a roster name is 1 to 32 characters from a-z, 0-9 and - after lowercasing, \
             with no spaces",
        ));
    }
    let role = draft.role.trim();
    if role.is_empty() {
        return Err(RosterError::refused(
            "roster_role_invalid",
            format!("a role is required: 1 to {MAX_ROLE_CHARS} characters"),
        ));
    }
    if role.chars().count() > MAX_ROLE_CHARS {
        return Err(RosterError::refused(
            "roster_role_invalid",
            format!(
                "a role is at most {MAX_ROLE_CHARS} characters and this one is {}",
                role.chars().count()
            ),
        ));
    }
    let instructions = draft.instructions.trim();
    if instructions.is_empty() {
        return Err(RosterError::refused(
            "roster_instructions_invalid",
            format!("standing instructions are required: 1 to {MAX_INSTRUCTIONS_CHARS} characters"),
        ));
    }
    if instructions.chars().count() > MAX_INSTRUCTIONS_CHARS {
        return Err(RosterError::refused(
            "roster_instructions_invalid",
            format!(
                "standing instructions are at most {MAX_INSTRUCTIONS_CHARS} characters and these \
                 are {}",
                instructions.chars().count()
            ),
        ));
    }
    Ok(())
}

/// Every row, oldest name first.
pub fn list(db: &Db) -> Result<Vec<Agent>, DbError> {
    let sql = format!("SELECT {COLUMNS} FROM roster ORDER BY name COLLATE NOCASE, id");
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map([], row_to_agent)?;
        rows.collect()
    })
}

/// One row by name, case-insensitively.
pub fn get_by_name(db: &Db, name: &str) -> Result<Option<Agent>, DbError> {
    let sql = format!("SELECT {COLUMNS} FROM roster WHERE name = ?1 COLLATE NOCASE");
    let mut found: Option<Agent> = None;
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&sql)?;
        let mut rows = statement.query(params![canonical_name(name)])?;
        if let Some(row) = rows.next()? {
            found = Some(row_to_agent(row)?);
        }
        Ok(())
    })?;
    Ok(found)
}

/// Add a row. The name is stored canonically; a name already taken refuses.
pub fn create(db: &Db, draft: &Draft) -> Result<Agent, RosterError> {
    validate(draft)?;
    let name = canonical_name(&draft.name);
    let role = draft.role.trim();
    let instructions = draft.instructions.trim();
    let model_role = trimmed(&draft.model_role);
    let memory_scope = trimmed(&draft.memory_scope);
    let id = uuid::Uuid::now_v7().to_string();

    let taken = db.with_conn(|conn| {
        let transaction = conn.transaction()?;
        let clash: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM roster WHERE name = ?1 COLLATE NOCASE",
            params![name],
            |row| row.get(0),
        )?;
        if clash > 0 {
            transaction.commit()?;
            return Ok(true);
        }
        transaction.execute(
            "INSERT INTO roster
                 (id, name, role, instructions, tool_policy, model_role, memory_scope)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                name,
                role,
                instructions,
                draft.tool_policy.as_str(),
                model_role,
                memory_scope,
            ],
        )?;
        transaction.commit()?;
        Ok(false)
    })?;
    if taken {
        return Err(RosterError::refused(
            "roster_name_taken",
            format!("a standing agent is already named '{name}'"),
        ));
    }
    get_by_name(db, &name)?
        .ok_or_else(|| RosterError::refused("roster_unavailable", "the row vanished after insert"))
}

/// Replace a row's editable fields. A rename that collides refuses.
pub fn update(db: &Db, name: &str, draft: &Draft) -> Result<Agent, RosterError> {
    validate(draft)?;
    let current = get_by_name(db, name)?.ok_or_else(|| unknown_agent(db, name))?;
    let rename = canonical_name(&draft.name);
    let role = draft.role.trim();
    let instructions = draft.instructions.trim();
    let model_role = trimmed(&draft.model_role);
    let memory_scope = trimmed(&draft.memory_scope);

    let taken = db.with_conn(|conn| {
        let transaction = conn.transaction()?;
        if rename != current.name {
            let clash: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM roster WHERE name = ?1 COLLATE NOCASE AND id != ?2",
                params![rename, current.id],
                |row| row.get(0),
            )?;
            if clash > 0 {
                transaction.commit()?;
                return Ok(true);
            }
        }
        transaction.execute(
            "UPDATE roster
                SET name = ?2,
                    role = ?3,
                    instructions = ?4,
                    tool_policy = ?5,
                    model_role = ?6,
                    memory_scope = ?7,
                    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
              WHERE id = ?1",
            params![
                current.id,
                rename,
                role,
                instructions,
                draft.tool_policy.as_str(),
                model_role,
                memory_scope,
            ],
        )?;
        transaction.commit()?;
        Ok(false)
    })?;
    if taken {
        return Err(RosterError::refused(
            "roster_name_taken",
            format!("a standing agent is already named '{rename}'"),
        ));
    }
    get_by_name(db, &rename)?
        .ok_or_else(|| RosterError::refused("roster_unavailable", "the row vanished after update"))
}

/// Turn a row on or off.
pub fn set_enabled(db: &Db, name: &str, enabled: bool) -> Result<(), RosterError> {
    let changed = db.with_conn(|conn| {
        conn.execute(
            "UPDATE roster
                SET enabled = ?2,
                    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
              WHERE name = ?1 COLLATE NOCASE",
            params![canonical_name(name), i64::from(enabled)],
        )
    })?;
    if changed == 0 {
        return Err(unknown_agent(db, name));
    }
    Ok(())
}

/// Remove a row. `false` means there was nothing by that name.
pub fn delete(db: &Db, name: &str) -> Result<bool, RosterError> {
    let changed = db.with_conn(|conn| {
        conn.execute(
            "DELETE FROM roster WHERE name = ?1 COLLATE NOCASE",
            params![canonical_name(name)],
        )
    })?;
    Ok(changed > 0)
}

/// The `name - role` line per enabled row, capped so the prompt cannot grow
/// without a bound. Empty when the roster is empty or everything is off: an
/// empty roster means no prompt block at all, which the caller decides.
pub fn describe_for_prompt(db: &Db) -> Result<String, DbError> {
    let rows: Vec<(String, String)> = db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT name, role FROM roster WHERE enabled = 1 ORDER BY name COLLATE NOCASE, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect()
    })?;
    if rows.is_empty() {
        return Ok(String::new());
    }
    let mut lines: Vec<String> = rows
        .iter()
        .take(PROMPT_ROWS)
        .map(|(name, role)| format!("{name} - {role}"))
        .collect();
    if rows.len() > PROMPT_ROWS {
        lines.push(format!("and {} more", rows.len() - PROMPT_ROWS));
    }
    Ok(lines.join(", "))
}

/// The enabled names, for a refusal that says who is available.
fn enabled_names(db: &Db) -> Result<String, DbError> {
    let rows: Vec<String> = db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT name FROM roster WHERE enabled = 1 ORDER BY name COLLATE NOCASE, id",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect()
    })?;
    if rows.is_empty() {
        return Ok("none".to_owned());
    }
    Ok(rows.join(", "))
}

/// The refusal for a name that is not a usable standing agent.
fn unknown_agent(db: &Db, name: &str) -> RosterError {
    match enabled_names(db) {
        Ok(names) => RosterError::refused(
            "roster_unknown_agent",
            format!(
                "no enabled standing agent is named '{}'. The enabled agents are: {names}",
                canonical_name(name)
            ),
        ),
        Err(error) => RosterError::Unavailable(error),
    }
}

/// Resolve a dispatch name to what the child needs.
///
/// The three refusals are separate on purpose: an unknown name, a disabled row
/// and a scope that escapes the workspace are different facts for the model and
/// the operator, and a single "cannot dispatch" hides which rule was hit.
pub fn resolve_for_dispatch(
    db: &Db,
    name: &str,
    workspace_root: &Path,
) -> Result<Resolved, RosterError> {
    let row = match get_by_name(db, name)? {
        Some(row) => row,
        None => return Err(unknown_agent(db, name)),
    };
    if !row.enabled {
        return Err(match enabled_names(db) {
            Ok(names) => RosterError::refused(
                "roster_agent_disabled",
                format!(
                    "the standing agent '{}' is disabled. The enabled agents are: {names}",
                    row.name
                ),
            ),
            Err(error) => RosterError::Unavailable(error),
        });
    }
    let root = match row.memory_scope.as_deref().map(str::trim) {
        Some(scope) if !scope.is_empty() => scoped_root(workspace_root, scope, &row.name)?,
        _ => workspace_root.to_owned(),
    };
    Ok(Resolved {
        name: row.name,
        instructions: row.instructions,
        tool_policy: row.tool_policy,
        root,
    })
}

/// Where a memory scope points, or a named refusal.
///
/// Canonicalise both ends and prefix-check, so `..`, a symlink and an absolute
/// path all end up judged by where they actually resolve. A scope that lands
/// outside the workspace root refuses: a roster row is not a way to give a child
/// a new root.
fn scoped_root(workspace_root: &Path, scope: &str, agent: &str) -> Result<PathBuf, RosterError> {
    let root = std::fs::canonicalize(workspace_root).map_err(|error| {
        RosterError::refused(
            "roster_scope_unusable",
            format!("the workspace root could not be resolved ({error})"),
        )
    })?;
    let target = std::fs::canonicalize(root.join(scope)).map_err(|error| {
        RosterError::refused(
            "roster_scope_missing",
            format!(
                "the memory scope '{scope}' of agent '{agent}' could not be resolved under the \
                 workspace ({error})"
            ),
        )
    })?;
    if !target.starts_with(&root) {
        return Err(RosterError::refused(
            "roster_scope_outside_workspace",
            format!(
                "the memory scope '{scope}' of agent '{agent}' resolves outside the workspace, so \
                 nothing was dispatched"
            ),
        ));
    }
    Ok(target)
}

/// A trimmed option: blank becomes `None`, so an empty field and an absent field
/// are one fact.
fn trimmed(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("in-memory database")
    }

    fn draft() -> Draft {
        Draft {
            name: "Scout".to_owned(),
            role: "watches the web and relays what matters".to_owned(),
            instructions: "Read the brief, fetch, and report findings with citations.".to_owned(),
            tool_policy: ToolPolicy::ReadOnly,
            model_role: None,
            memory_scope: None,
        }
    }

    #[test]
    fn migration_thirty_creates_the_roster() {
        let db = db();
        // At least thirty: a later migration must not make this test fail, because
        // what it claims is that 0030 ran and created this table, not that no
        // migration ever followed it.
        assert!(db.schema_version().expect("version") >= 30);
        assert!(crate::db::TABLES.contains(&"roster"));
        let columns = db.column_names("roster").expect("roster");
        for expected in [
            "id",
            "name",
            "role",
            "instructions",
            "tool_policy",
            "model_role",
            "memory_scope",
            "enabled",
            "created_at",
            "updated_at",
        ] {
            assert!(columns.iter().any(|c| c == expected), "missing {expected}");
        }
    }

    #[test]
    fn a_name_is_canonical_and_bounded() {
        let db = db();
        let created = create(&db, &draft()).expect("created");
        assert_eq!(created.name, "scout", "stored lowercase");
        assert!(create(&db, &draft()).is_err(), "the name is taken");

        let mut long = draft();
        long.name = "a".repeat(MAX_NAME_CHARS + 1);
        let error = create(&db, &long).expect_err("too long");
        assert_eq!(error.code(), "roster_name_invalid");

        let mut spaced = draft();
        spaced.name = "two words".to_owned();
        let error = create(&db, &spaced).expect_err("spaces");
        assert_eq!(error.code(), "roster_name_invalid");
    }

    #[test]
    fn bounds_refuse_rather_than_truncate() {
        let mut case = draft();
        case.role = "r".repeat(MAX_ROLE_CHARS + 1);
        let error = validate(&case).expect_err("role too long");
        assert_eq!(error.code(), "roster_role_invalid");

        let mut case = draft();
        case.instructions = "i".repeat(MAX_INSTRUCTIONS_CHARS + 1);
        let error = validate(&case).expect_err("instructions too long");
        assert_eq!(error.code(), "roster_instructions_invalid");

        let mut case = draft();
        case.role = String::new();
        assert_eq!(
            validate(&case).expect_err("no role").code(),
            "roster_role_invalid"
        );
    }

    #[test]
    fn the_prompt_line_is_capped_and_says_how_many_are_left() {
        let db = db();
        for index in 0..14 {
            let mut draft = draft();
            draft.name = format!("agent-{index:02}");
            create(&db, &draft).expect("created");
        }
        let described = describe_for_prompt(&db).expect("described");
        assert_eq!(described.matches(" - ").count(), PROMPT_ROWS);
        assert!(described.ends_with("and 2 more"), "got {described}");
        assert!(described.starts_with("agent-00 - "), "got {described}");

        set_enabled(&db, "agent-00", false).expect("off");
        let described = describe_for_prompt(&db).expect("described");
        assert!(
            !described.starts_with("agent-00"),
            "disabled rows are omitted"
        );
    }

    #[test]
    fn an_empty_roster_describes_nothing() {
        let db = db();
        assert_eq!(describe_for_prompt(&db).expect("described"), "");
    }

    #[test]
    fn dispatch_resolution_names_who_is_available() {
        let db = db();
        create(&db, &draft()).expect("created");
        let error = resolve_for_dispatch(&db, "nobody", Path::new("/tmp")).expect_err("unknown");
        assert_eq!(error.code(), "roster_unknown_agent");
        assert!(error.reason().contains("scout"), "got {}", error.reason());

        set_enabled(&db, "scout", false).expect("off");
        let error = resolve_for_dispatch(&db, "scout", Path::new("/tmp")).expect_err("disabled");
        assert_eq!(error.code(), "roster_agent_disabled");
    }

    #[test]
    fn a_scope_inside_the_workspace_roots_the_child_there() {
        let db = db();
        let workspace = std::env::temp_dir().join(format!("roster-scope-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(workspace.join("scout-notes")).expect("workspace");
        let mut draft = draft();
        draft.memory_scope = Some("scout-notes".to_owned());
        create(&db, &draft).expect("created");
        let resolved = resolve_for_dispatch(&db, "scout", &workspace).expect("resolved");
        assert_eq!(
            resolved.root,
            std::fs::canonicalize(workspace.join("scout-notes")).expect("canonical"),
            "the child is rooted at the scope, not the workspace root"
        );
    }
}
