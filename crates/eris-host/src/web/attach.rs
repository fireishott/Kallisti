//! A file the assistant made, attached to its own reply.
//!
//! Before this, a turn that produced a PDF or a chart could only say so: the
//! transcript row carried the operator's files and nothing else, and the model
//! wrote "attached: report.pdf" in prose, which is a claim the daemon had no way
//! to make true. `attach_file` copies a workspace file into the conversation's
//! uploads, records the row, and binds it to the answer row the turn is writing -
//! the same four things `POST /v1/attachments` does for an operator, so both
//! surfaces render it with the code they already have.
//!
//! Two rules are the same ones the upload path keeps: the source has to resolve
//! inside the workspace (a turn cannot reach out and attach `~/.ssh/id_ed25519`),
//! and the caps are the transcript's, not this tool's, so a reply cannot carry
//! more files or more bytes than a question may.

use std::path::{Path, PathBuf};

use core_runtime::provider::ToolSpec;

use crate::db::Db;
use crate::web::chat::{self, Attachment};
use crate::web::routes::write_atomically;

/// The tool name, in one place because the spec, the executor and the tests all
/// have to agree about it.
pub const TOOL: &str = "attach_file";

/// Where a produced file goes: the conversation it belongs to, and the answer
/// row this turn is writing.
#[derive(Debug, Clone)]
pub struct Target {
    /// The state database, for the attachment row.
    pub db: Db,
    /// The conversation whose uploads tree the file lands in.
    pub conversation: String,
    /// The assistant row the file is attached to.
    pub answer: String,
}

/// The tool the model is offered when a turn has a reply row to attach to.
#[must_use]
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: TOOL.to_owned(),
        description: "Attach a file you produced to your reply, so the operator sees it as an \
                      attachment they can open. Use this instead of pasting contents or naming a \
                      path. The file must already exist inside the workspace."
            .to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file, relative to the workspace (or absolute, inside it)."
                }
            },
            "required": ["path"]
        }),
    }
}

/// Copy one workspace file into the conversation's uploads and bind it to the
/// answer row. Returns the row that was written.
///
/// # Errors
///
/// A reason the model can read: the path is empty, outside the workspace, not a
/// file, too large, or the reply already carries as many files as one message may.
pub fn attach(target: &Target, workspace: &Path, requested: &str) -> Result<Attachment, String> {
    let shown = requested.trim();
    if shown.is_empty() {
        return Err("the path was empty".to_owned());
    }
    if shown.len() > 1024 {
        return Err("the path is too long".to_owned());
    }
    let candidate = if Path::new(shown).is_absolute() {
        PathBuf::from(shown)
    } else {
        workspace.join(shown)
    };
    let root = workspace
        .canonicalize()
        .unwrap_or_else(|_| workspace.to_path_buf());
    let resolved = candidate
        .canonicalize()
        .map_err(|error| format!("cannot read `{shown}`: {error}"))?;
    if !resolved.starts_with(&root) {
        return Err(format!("`{shown}` is outside the workspace"));
    }
    let meta =
        std::fs::metadata(&resolved).map_err(|error| format!("cannot read `{shown}`: {error}"))?;
    if !meta.is_file() {
        return Err(format!("`{shown}` is not a file"));
    }
    if meta.len() > chat::MAX_ATTACHMENT_BYTES as u64 {
        return Err(format!(
            "`{shown}` is {}; one attachment may be at most {}",
            chat::human_bytes(meta.len()),
            chat::human_bytes(chat::MAX_ATTACHMENT_BYTES as u64)
        ));
    }
    let carried = chat::attachment_count_for(&target.db, &target.answer)
        .map_err(|error| format!("cannot read the reply's attachments: {error}"))?;
    if carried >= chat::MAX_ATTACHMENTS {
        return Err(format!(
            "this reply already carries {carried} files, which is the most one message may"
        ));
    }

    let bytes =
        std::fs::read(&resolved).map_err(|error| format!("cannot read `{shown}`: {error}"))?;
    let original = resolved
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("attachment");
    let safe = chat::safe_file_name(original);
    let name = if safe.trim().is_empty() {
        "attachment".to_owned()
    } else {
        safe
    };
    let directory = workspace.join("uploads").join(&target.conversation);
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("cannot create {}: {error}", directory.display()))?;
    let id = uuid::Uuid::now_v7().to_string();
    let path = directory.join(format!("{id}-{name}"));
    write_atomically(&path, &bytes)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    let media_type = chat::sniff_media_type(&bytes).to_owned();
    let file = Attachment {
        id: id.clone(),
        name,
        path: path.display().to_string(),
        size: bytes.len() as u64,
        media_type: Some(media_type),
    };
    if let Err(error) = chat::record_attachment(&target.db, &target.conversation, &file) {
        // Bytes with no row are litter nothing can find again, so they go with it.
        let _ = std::fs::remove_file(&path);
        return Err(format!("cannot record the attachment: {error}"));
    }
    chat::claim_attachments(&target.db, &target.conversation, &target.answer, &[id])
        .map_err(|error| format!("cannot attach it to this reply: {error}"))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Db, String, String, tempfile::TempDir) {
        let db = Db::open_in_memory().expect("in-memory database");
        let conversation = chat::create_conversation(&db).expect("a conversation");
        let answer =
            chat::append_answer(&db, &conversation, "stub/model", None).expect("an answer row");
        let dir = tempfile::tempdir().expect("a workspace");
        (db, conversation, answer, dir)
    }

    #[test]
    fn a_workspace_file_lands_on_the_reply() {
        let (db, conversation, answer, dir) = fixture();
        std::fs::write(dir.path().join("report.pdf"), b"%PDF-1.4 fake").expect("a file");
        let target = Target {
            db: db.clone(),
            conversation: conversation.clone(),
            answer: answer.clone(),
        };
        let file = attach(&target, dir.path(), "report.pdf").expect("attached");
        assert_eq!(file.name, "report.pdf");
        assert_eq!(file.size, 13);
        assert_eq!(file.media_type.as_deref(), Some("application/pdf"));
        assert_eq!(
            chat::attachment_count_for(&db, &answer).expect("counted"),
            1,
            "the row is bound to the answer, not left unclaimed"
        );
        // The bytes live under the conversation's uploads, not beside the source.
        assert!(
            file.path.contains("/uploads/"),
            "the copy is in the uploads tree: {}",
            file.path
        );
        assert!(Path::new(&file.path).exists());
    }

    #[test]
    fn a_path_outside_the_workspace_is_refused() {
        let (db, conversation, answer, dir) = fixture();
        let target = Target {
            db,
            conversation,
            answer,
        };
        let error = attach(&target, dir.path(), "/etc/hostname").expect_err("a refusal");
        assert!(error.contains("outside the workspace"), "{error}");
    }

    #[test]
    fn a_directory_is_not_a_file() {
        let (db, conversation, answer, dir) = fixture();
        std::fs::create_dir_all(dir.path().join("scratch")).expect("a directory");
        let target = Target {
            db,
            conversation,
            answer,
        };
        let error = attach(&target, dir.path(), "scratch").expect_err("a refusal");
        assert!(error.contains("not a file"), "{error}");
    }

    #[test]
    fn a_reply_carries_no_more_files_than_a_question_may() {
        let (db, conversation, answer, dir) = fixture();
        for index in 0..chat::MAX_ATTACHMENTS {
            std::fs::write(dir.path().join(format!("f{index}.txt")), b"x").expect("a file");
        }
        let target = Target {
            db,
            conversation,
            answer,
        };
        for index in 0..chat::MAX_ATTACHMENTS {
            attach(&target, dir.path(), &format!("f{index}.txt")).expect("under the cap");
        }
        let error = attach(&target, dir.path(), "f0.txt").expect_err("a refusal");
        assert!(error.contains("most one message may"), "{error}");
    }
}
