//! Projects: a name for a group of conversations.
//!
//! A project is the operator's own filing system, not a model feature: one row
//! is one name, and `chat_conversations.project_id` is the only thing that
//! points at it. Nothing in a turn reads a project, and no prompt carries one, so
//! a turn behaves the same whether its conversation is filed or not.
//!
//! Three rules live here rather than in a route, because the routes are not the
//! only caller and a rule that exists in one handler is a rule that a second
//! handler forgets:
//!
//! - a name is trimmed, non-empty, and at most [`MAX_NAME_CHARS`] characters;
//! - two projects never share a name, in any case: the table's `UNIQUE`
//!   constraint is the backstop, and the pre-check turns the common case into a
//!   refusal with a message instead of a raw constraint string;
//! - a conversation is in zero or one project, so assigning is an `UPDATE`, and
//!   `None` is how it goes back to no project.

use rusqlite::params;

use crate::db::{Db, DbError};

/// The longest project name, in characters rather than bytes.
///
/// Sized for a rail heading: long enough for "Quarterly contract renewal", short
/// enough that it cannot push the sessions it heads off the rail.
pub const MAX_NAME_CHARS: usize = 60;

/// One project, as the rail and the select render it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    /// Opaque id, and what an assignment stores.
    pub id: String,
    /// The name the operator reads. Unique, case-insensitively.
    pub name: String,
    /// When it was created, in the database's own format.
    pub created_at: String,
}

/// Every project, by name.
///
/// Ordered by the same collation the duplicate check uses, so the list and the
/// refusal agree about what "already exists" means.
pub fn list(db: &Db) -> Result<Vec<Project>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, name, created_at FROM projects ORDER BY name COLLATE NOCASE, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Project {
                id: row.get(0)?,
                name: row.get(1)?,
                created_at: row.get(2)?,
            })
        })?;
        rows.collect()
    })
}

/// Create a project and return its id.
///
/// The three refusals are all [`DbError`] variants rather than a separate error
/// type: the caller already has one error type to map, and a name that is
/// blank, too long, or taken is a fact about the database's rules, not a
/// transport concern.
pub fn create(db: &Db, name: &str) -> Result<String, DbError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(DbError::InvalidProjectName {
            max: MAX_NAME_CHARS,
        });
    }
    if existing_by_name(db, name)? {
        return Err(DbError::DuplicateProjectName {
            name: name.to_owned(),
        });
    }

    let id = uuid::Uuid::now_v7().to_string();
    let inserted = db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO projects (id, name) VALUES (?1, ?2)",
            params![id, name],
        )
    });
    match inserted {
        Ok(_) => Ok(id),
        // A second create that lost the race between the check above and this
        // insert still deserves the refusal a person can read, not a raw
        // "UNIQUE constraint failed" string.
        Err(DbError::Sqlite(error))
            if error.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation) =>
        {
            Err(DbError::DuplicateProjectName {
                name: name.to_owned(),
            })
        }
        Err(other) => Err(other),
    }
}

/// Whether a project with this name already exists, ignoring case.
fn existing_by_name(db: &Db, name: &str) -> Result<bool, DbError> {
    db.with_conn(|conn| {
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM projects WHERE name = ?1 COLLATE NOCASE",
                params![name],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    })
}

/// Put one conversation in a project, or take it out of one.
///
/// `None` clears the column, which is the "no project" a select sends as an
/// empty string. The returned count is the number of conversation rows the
/// update touched: `0` means the conversation id is not one the daemon knows, or
/// the project id names no project, and either way nothing was assigned, so a
/// caller must not report success.
///
/// The conversation's `updated_at` is deliberately untouched: filing a
/// conversation is not a message, and reordering the rail as a side effect of an
/// organisation action would be a lie about what was said when.
pub fn assign(db: &Db, conversation_id: &str, project: Option<&str>) -> Result<usize, DbError> {
    let project_id = match project.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => {
            let known: Option<i64> = db.with_conn(|conn| {
                conn.query_row("SELECT 1 FROM projects WHERE id = ?1", params![id], |row| {
                    row.get(0)
                })
                .optional()
            })?;
            if known.is_none() {
                return Ok(0);
            }
            Some(id.to_owned())
        }
        None => None,
    };

    db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_conversations SET project_id = ?2 WHERE id = ?1",
            params![conversation_id, project_id],
        )
    })
}

/// `Option<T>` for a query that may match no row, without a crate for it.
trait OptionalRow<T> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalRow<T> for Result<T, rusqlite::Error> {
    fn optional(self) -> Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(other) => Err(other),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web::chat;

    /// An in-memory database and one conversation to file.
    fn scratch() -> (Db, String) {
        let db = Db::open_in_memory().expect("a fresh in-memory database opens");
        let conversation = chat::create_conversation(&db).expect("a conversation row");
        (db, conversation)
    }

    #[test]
    fn create_stores_a_trimmed_name_and_lists_it() {
        let (db, _conversation) = scratch();
        let id = create(&db, "  Quarterly contract renewal  ").expect("the project is created");

        let projects = list(&db).expect("the list reads");
        assert_eq!(projects.len(), 1, "one project was created: {projects:?}");
        assert_eq!(projects[0].id, id);
        assert_eq!(
            projects[0].name, "Quarterly contract renewal",
            "the name is trimmed before it is stored"
        );
        assert!(
            !projects[0].created_at.is_empty(),
            "the database stamped the row"
        );
    }

    #[test]
    fn create_refuses_a_blank_or_oversized_name() {
        let (db, _conversation) = scratch();
        for blank in ["", "   ", "\t\n"] {
            assert!(
                matches!(create(&db, blank), Err(DbError::InvalidProjectName { .. })),
                "{blank:?} is not a project name"
            );
        }

        let longest = "a".repeat(MAX_NAME_CHARS);
        assert!(
            create(&db, &longest).is_ok(),
            "exactly the ceiling is allowed"
        );
        let too_long = "a".repeat(MAX_NAME_CHARS + 1);
        assert!(
            matches!(
                create(&db, &too_long),
                Err(DbError::InvalidProjectName { .. })
            ),
            "one character past the ceiling is refused"
        );
        assert_eq!(list(&db).expect("the list reads").len(), 1);
    }

    #[test]
    fn create_refuses_a_duplicate_name() {
        let (db, _conversation) = scratch();
        let first = create(&db, "Work").expect("the first project is created");
        assert_eq!(list(&db).expect("list").len(), 1);

        let exact = create(&db, "Work");
        assert!(
            matches!(exact, Err(DbError::DuplicateProjectName { .. })),
            "an exact duplicate is refused: {exact:?}"
        );
        // Case is not a way around it: two headings that differ only in case
        // read as one thing twice.
        let folded = create(&db, "work");
        assert!(
            matches!(folded, Err(DbError::DuplicateProjectName { .. })),
            "a case-folded duplicate is refused: {folded:?}"
        );

        let projects = list(&db).expect("the list reads");
        assert_eq!(
            projects.len(),
            1,
            "the refusals added no rows: {projects:?}"
        );
        assert_eq!(projects[0].id, first);
    }

    #[test]
    fn list_is_empty_before_anything_is_created() {
        let (db, _conversation) = scratch();
        assert!(
            list(&db).expect("the list reads").is_empty(),
            "a fresh database has no projects"
        );
    }

    #[test]
    fn assign_files_a_conversation_and_clears_it_again() {
        let (db, conversation) = scratch();
        let work = create(&db, "Work").expect("the project is created");
        let home = create(&db, "Home").expect("the second project is created");

        assert_eq!(
            assign(&db, &conversation, Some(&work)).expect("the assignment runs"),
            1,
            "one conversation row was updated"
        );
        let mut filed = chat::conversations(&db, 10).expect("the rail list reads");
        assert_eq!(filed.len(), 1);
        assert_eq!(filed[0].project_id.as_deref(), Some(work.as_str()));

        // Moving it is the same operation, not a second membership.
        assert_eq!(
            assign(&db, &conversation, Some(&home)).expect("the reassignment runs"),
            1
        );
        filed = chat::conversations(&db, 10).expect("the rail list reads");
        assert_eq!(filed[0].project_id.as_deref(), Some(home.as_str()));

        // The empty value a select sends means "no project", not "the empty id".
        assert_eq!(assign(&db, &conversation, None).expect("the clear runs"), 1);
        filed = chat::conversations(&db, 10).expect("the rail list reads");
        assert_eq!(filed[0].project_id, None, "it is ungrouped again");

        // An empty string is the same request as `None`: a form sends one, not
        // null.
        assert_eq!(
            assign(&db, &conversation, Some("  ")).expect("the clear runs"),
            1
        );
        filed = chat::conversations(&db, 10).expect("the rail list reads");
        assert_eq!(filed[0].project_id, None);
    }

    #[test]
    fn assign_reports_nothing_changed_for_an_unknown_row_or_project() {
        let (db, conversation) = scratch();
        let work = create(&db, "Work").expect("the project is created");

        assert_eq!(
            assign(&db, "no-such-conversation", Some(&work)).expect("the update runs"),
            0,
            "a conversation that does not exist is not an assignment"
        );
        assert_eq!(
            assign(&db, &conversation, Some("no-such-project")).expect("the update runs"),
            0,
            "a project id that names nothing is refused by counting, not by writing"
        );
        let filed = chat::conversations(&db, 10).expect("the rail list reads");
        assert_eq!(
            filed[0].project_id, None,
            "the refused assignments changed nothing"
        );
    }
}
