//! Attachments on the device surface.
//!
//! The browser's composer has carried files since the transcript grew a
//! paperclip: `POST` a multipart body, the daemon writes the bytes under
//! `workspace/uploads/<conversation>/`, records a row, and the send route claims
//! the row for the message it belongs to. The phone had none of that - a turn
//! over `/v1` was text only, and `ErisHeraldClient` said so in a comment before
//! dropping every attachment on the floor.
//!
//! This module is the same feature over the device listener, in the shape a
//! phone can actually use it. A client that has bytes to send does three things:
//!
//! 1. `POST /v1/attachments` with the raw bytes, a filename header and the
//!    conversation it is going into. Back comes the id.
//! 2. `POST /v1/turns` with `attachments=<id>,<id>` alongside the text. The turn
//!    claims those rows for its own question row, so the transcript names them
//!    and the sweeper leaves them alone.
//! 3. `GET /v1/attachments/{id}` to draw one again later, from history.
//!
//! The heavy lifting is not repeated here. [`chat::record_attachment`] writes the
//! row, [`chat::claim_attachments`] binds it, [`chat::describe_attachments`] and
//! [`chat::describe_images`] turn it into prompt text (an image is described by
//! the pinned vision role, because the turn's own model may not see), and
//! [`chat::sniff_media_type`] decides what the bytes are rather than what the
//! filename claimed. Two rules are upheld on the way out, both copied from the
//! dashboard's own file route: the row must resolve inside the uploads tree, and
//! the read is bounded again even though the write was.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::db::Db;
use crate::web::chat::{self, Attachment};
use crate::web::routes::write_atomically;

use crate::auth::Scope;

use super::handlers::{authenticate, require_ready, require_scope};
use super::{DeviceError, DeviceService};

/// Header a client names the file with.
pub const FILENAME_HEADER: &str = "x-kallisti-filename";

/// Header a client says which conversation the file belongs to.
pub const CONVERSATION_HEADER: &str = "x-kallisti-conversation";

/// The field a turn carries its attachment ids in.
pub const ATTACHMENTS_FIELD: &str = "attachments";

fn header_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

/// `POST /v1/attachments` - take the bytes, hand back an id.
///
/// The body is the file itself rather than a multipart form. A phone has the
/// bytes in hand and a multipart encoder to hand is one more thing to get wrong;
/// `application/octet-stream` plus two headers says everything the row needs.
pub async fn upload(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }
    if let Err(error) = require_ready(&service) {
        return error.into_response();
    }

    let bytes = match axum::body::to_bytes(body, chat::MAX_ATTACHMENT_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return DeviceError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "upload_too_large",
                format!(
                    "one attachment may be at most {}",
                    chat::human_bytes(chat::MAX_ATTACHMENT_BYTES as u64)
                ),
            )
            .into_response();
        }
    };
    if bytes.is_empty() {
        return DeviceError::bad_request("upload_empty", "the upload carried no bytes")
            .into_response();
    }

    // The conversation the row belongs to. A device that names one gets it or a
    // 404; a device that names none is filing into the default conversation, the
    // same one the browser files into when its box is empty.
    let db = service.engine().db();
    let conversation = match header_value(&headers, CONVERSATION_HEADER) {
        None => match chat::resolve_conversation(db, None) {
            Ok(id) => id,
            Err(error) => {
                return DeviceError::db_unavailable(format!(
                    "cannot resolve the conversation: {error}"
                ))
                .into_response();
            }
        },
        Some(requested) => match chat::conversation(db, requested) {
            Ok(Some(row)) => row.id,
            Ok(None) => return DeviceError::conversation_not_found().into_response(),
            Err(error) => {
                return DeviceError::db_unavailable(format!(
                    "cannot read the conversation: {error}"
                ))
                .into_response();
            }
        },
    };

    let original = header_value(&headers, FILENAME_HEADER).unwrap_or("attachment");
    let safe = chat::safe_file_name(original);
    let name = if safe.trim().is_empty() {
        "attachment".to_owned()
    } else {
        safe
    };

    let directory = match service.engine().workspace_path() {
        Ok(workspace) => workspace.join("uploads").join(&conversation),
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot read the workspace: {error}"))
                .into_response();
        }
    };
    if let Err(error) = std::fs::create_dir_all(&directory) {
        return DeviceError::db_unavailable(format!(
            "cannot create {}: {error}",
            directory.display()
        ))
        .into_response();
    }

    let id = uuid::Uuid::now_v7().to_string();
    let path = directory.join(format!("{id}-{name}"));
    if let Err(error) = write_atomically(&path, &bytes) {
        return DeviceError::db_unavailable(format!("cannot write {}: {error}", path.display()))
            .into_response();
    }
    let media_type = chat::sniff_media_type(&bytes).to_owned();
    let attachment = Attachment {
        id: id.clone(),
        name: name.clone(),
        path: path.display().to_string(),
        size: bytes.len() as u64,
        media_type: Some(media_type.clone()),
    };
    if let Err(error) = chat::record_attachment(db, &conversation, &attachment) {
        // The row is the record; bytes with no row are litter nothing can find
        // again, so they go with it.
        let _ = std::fs::remove_file(&path);
        return DeviceError::db_unavailable(format!("cannot record the attachment: {error}"))
            .into_response();
    }
    let _ = service.engine().auth().audit(
        token.device_id(),
        "chat.attachment",
        Some(&format!(
            "{} bytes, {}",
            attachment.size,
            attachment.media_type.as_deref().unwrap_or("unknown type")
        )),
    );

    (
        StatusCode::CREATED,
        axum::Json(serde_json::json!({
            "ok": true,
            "attachment": {
                "id": attachment.id,
                "name": attachment.name,
                "size": attachment.size,
                "media_type": attachment.media_type,
            },
            "conversation_id": conversation,
        })),
    )
        .into_response()
}

/// `GET /v1/attachments/{id}` - the bytes an id names.
///
/// Three rules, in the order the dashboard's own file route applies them: the id
/// has to be a row, the row's path has to resolve inside the uploads tree, and
/// the read is bounded by the same ceiling the upload enforced. The type comes
/// from the bytes, not from the row, so a file renamed `.png` is served as what
/// it is.
pub async fn bytes(
    State(service): State<DeviceService>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let token = match authenticate(&service, &headers) {
        Ok(token) => token,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = require_scope(&token, Scope::Chat) {
        return error.into_response();
    }

    let db = service.engine().db();
    let row = match chat::attachment(db, &id) {
        Ok(Some(row)) => row,
        Ok(None) => {
            return DeviceError::new(
                StatusCode::NOT_FOUND,
                "attachment_not_found",
                "no attachment with that id is recorded",
            )
            .into_response();
        }
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot read the attachment: {error}"))
                .into_response();
        }
    };

    let root = match service.engine().workspace_path() {
        Ok(workspace) => workspace.join("uploads"),
        Err(error) => {
            return DeviceError::db_unavailable(format!("cannot read the workspace: {error}"))
                .into_response();
        }
    };
    let Ok(root) = std::fs::canonicalize(root) else {
        return DeviceError::new(
            StatusCode::NOT_FOUND,
            "attachment_not_found",
            "no attachment directory exists",
        )
        .into_response();
    };
    let Ok(real) = std::fs::canonicalize(&row.path) else {
        return DeviceError::new(
            StatusCode::NOT_FOUND,
            "attachment_not_found",
            "that attachment is recorded but its bytes are not on disk",
        )
        .into_response();
    };
    if !real.starts_with(&root) {
        return DeviceError::new(
            StatusCode::NOT_FOUND,
            "attachment_not_found",
            "that attachment is not inside the attachment directory",
        )
        .into_response();
    }
    let Ok(file) = std::fs::File::open(&real) else {
        return DeviceError::new(
            StatusCode::NOT_FOUND,
            "attachment_not_found",
            "that attachment is recorded but cannot be opened",
        )
        .into_response();
    };
    use std::io::Read as _;
    let mut bytes: Vec<u8> = Vec::new();
    let budget = (chat::MAX_ATTACHMENT_BYTES as u64) + 1;
    if file.take(budget).read_to_end(&mut bytes).is_err()
        || bytes.len() > chat::MAX_ATTACHMENT_BYTES
    {
        return DeviceError::new(
            StatusCode::NOT_FOUND,
            "attachment_not_found",
            "that attachment is recorded but cannot be read",
        )
        .into_response();
    }

    let media_type = chat::sniff_media_type(&bytes);
    // An image is inline, because the transcript draws it. Anything else is a
    // download, so a browser or a phone never executes an upload in this origin.
    let disposition = if media_type.starts_with("image/") {
        "inline"
    } else {
        "attachment"
    };
    let safe_name = row.name.replace(['"', '\\'], "").replace(['\r', '\n'], "");
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, media_type.to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("{disposition}; filename=\"{safe_name}\""),
            ),
            (header::CACHE_CONTROL, "private, max-age=60".to_owned()),
        ],
        bytes,
    )
        .into_response()
}

/// Split the `attachments` field a turn carries into ids.
///
/// Two spellings, because two clients exist: a comma-joined string (what the
/// phone sends) and a JSON array of strings (what a hand-written curl and the
/// web body parser produce). Anything else is a refusal naming the field rather
/// than a guess.
pub fn parse_ids(raw: &str) -> Result<Vec<String>, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let mut ids: Vec<String> = Vec::new();
    if trimmed.starts_with('[') {
        let parsed: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|_| format!("{ATTACHMENTS_FIELD} is not a JSON array of ids"))?;
        let array = parsed
            .as_array()
            .ok_or_else(|| format!("{ATTACHMENTS_FIELD} is not a JSON array of ids"))?;
        for value in array {
            let id = value
                .as_str()
                .ok_or_else(|| format!("{ATTACHMENTS_FIELD} holds something that is not an id"))?;
            ids.push(id.trim().to_owned());
        }
    } else {
        for part in trimmed.split(',') {
            ids.push(part.trim().to_owned());
        }
    }
    ids.retain(|id| !id.is_empty());
    if ids.len() > chat::MAX_ATTACHMENTS {
        return Err(format!(
            "a message may carry at most {} files; this one carries more",
            chat::MAX_ATTACHMENTS
        ));
    }
    Ok(ids)
}

/// Bind the ids a turn named to the question row it just wrote.
///
/// A row is returned for each id that exists, in the order asked for, so the
/// prompt can describe the files even if the claim itself matched nothing (a row
/// already claimed by an earlier message is not this message's file, and saying
/// so is more honest than silently attaching last turn's screenshot).
pub fn claim_for_turn(
    db: &Db,
    conversation: &str,
    message_id: &str,
    ids: &[String],
) -> Result<Vec<Attachment>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut files: Vec<Attachment> = Vec::new();
    for id in ids {
        match chat::attachment(db, id) {
            Ok(Some(row)) => files.push(row),
            Ok(None) => return Err(format!("no attachment with the id {id} is recorded")),
            Err(error) => return Err(format!("cannot read attachment {id}: {error}")),
        }
    }
    chat::claim_attachments(db, conversation, message_id, ids)
        .map_err(|error| format!("cannot bind the attachments to this message: {error}"))?;
    Ok(files)
}

/// The prompt text a turn needs so a model is not blind to what came with it.
///
/// The files are named, and an image is described by the pinned vision role
/// before the turn runs - the same order the browser's turn uses, because the
/// model serving the turn may be one that cannot see at all. A turn with no
/// files is the operator's words, unchanged.
pub fn prompt_with_attachments(text: &str, files: &[Attachment]) -> String {
    if files.is_empty() {
        return text.to_owned();
    }
    let mut prompt = text.to_owned();
    let described = chat::describe_attachments(files);
    if !described.trim().is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(&described);
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The phone sends one comma-joined string; the browser-shaped body parser
    /// sends a JSON array. Both are the same list.
    #[test]
    fn ids_arrive_in_both_spellings_a_client_uses() {
        assert_eq!(parse_ids("").expect("empty is none"), Vec::<String>::new());
        assert_eq!(
            parse_ids(" a , b ,,").expect("comma joined"),
            vec!["a".to_owned(), "b".to_owned()]
        );
        assert_eq!(
            parse_ids(r#"["a","b"]"#).expect("a json array"),
            vec!["a".to_owned(), "b".to_owned()]
        );
    }

    /// The cap is a refusal with the number in it, not a truncation: silently
    /// dropping the ninth file is worse than saying nine is too many.
    #[test]
    fn more_files_than_one_message_may_carry_is_refused() {
        let ids: Vec<String> = (0..=chat::MAX_ATTACHMENTS)
            .map(|n| format!("id{n}"))
            .collect();
        let error = parse_ids(&ids.join(",")).expect_err("a refusal");
        assert!(
            error.contains(&chat::MAX_ATTACHMENTS.to_string()),
            "{error}"
        );
    }

    /// A turn with a file and no words still tells the model what came with it,
    /// and a turn with no file is the operator's words unchanged - the same two
    /// rules the browser's turn follows.
    #[test]
    fn the_prompt_names_the_files_and_never_invents_one() {
        let file = Attachment {
            id: "a".to_owned(),
            name: "shot.png".to_owned(),
            path: "/tmp/shot.png".to_owned(),
            size: 10,
            media_type: Some("image/png".to_owned()),
        };
        assert_eq!(prompt_with_attachments("hello", &[]), "hello");
        let prompt = prompt_with_attachments("", std::slice::from_ref(&file));
        assert!(prompt.contains("shot.png"), "{prompt}");
    }
}
