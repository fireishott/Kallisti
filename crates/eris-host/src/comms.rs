//! Sending a message, staged and locked.
//!
//! The rule this module exists to enforce is the one in the operator's own brief:
//! nothing goes out in their name without a yes. That has three parts, and each is
//! a refusal rather than a convention:
//!
//! 1. **Allowlist.** A recipient that is not on the configured list is refused
//!    before anything is written. A model that can address anyone is a spam
//!    relay with a prompt.
//! 2. **Confirm.** A staged message does not exist as a send until it is
//!    confirmed. Staging without confirming leaves a row nobody can mistake for
//!    delivery.
//! 3. **Recipient lock.** The confirmed recipient is the staged one, compared
//!    byte for byte. A confirmation that names a different address is refused,
//!    which is the swap a confirmation step is supposed to stop.
//!
//! There is no transport here. The daemon has no SMTP client and no SMS bridge, so
//! a confirmed message is a row that says "agreed, not delivered" rather than a
//! claim that something left the host.

use serde_json::{Value, json};

use crate::db::Db;

/// The stored setting holding the comma-separated allowlist.
pub const SETTING_ALLOWLIST: &str = "comms.allowlist";

/// The kinds this daemon will stage.
pub const KINDS: [&str; 3] = ["sms", "email", "call"];

/// Every allowlisted recipient, trimmed and lowercased for comparison.
#[must_use]
pub fn allowlist(db: &Db) -> Vec<String> {
    let stored = db
        .with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT value FROM settings WHERE key = ?1",
                    rusqlite::params![SETTING_ALLOWLIST],
                    |row| row.get::<_, String>(0),
                )
                .ok())
        })
        .ok()
        .flatten()
        .unwrap_or_default();
    stored
        .split(',')
        .map(|entry| entry.trim().to_ascii_lowercase())
        .filter(|entry| !entry.is_empty())
        .collect()
}

/// Whether a recipient may be addressed at all.
#[must_use]
pub fn allowed(db: &Db, recipient: &str) -> bool {
    let wanted = recipient.trim().to_ascii_lowercase();
    !wanted.is_empty() && allowlist(db).contains(&wanted)
}

/// Stage one message for confirmation.
///
/// # Errors
///
/// The sentence a caller shows: an unknown kind, an empty body, or a recipient
/// that is not on the allowlist.
pub fn stage(db: &Db, kind: &str, recipient: &str, body: &str) -> Result<String, String> {
    if !KINDS.contains(&kind) {
        return Err(format!(
            "`{kind}` is not a kind this daemon stages. The kinds are: {}",
            KINDS.join(", ")
        ));
    }
    if body.trim().is_empty() {
        return Err("a staged message needs a body".to_owned());
    }
    if !allowed(db, recipient) {
        return Err(format!(
            "`{recipient}` is not on the comms allowlist, so nothing was staged. Add it in \
             settings first; this daemon does not message anyone who was not named there."
        ));
    }
    let id = uuid::Uuid::now_v7().to_string();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO comms_outbox (id, kind, recipient, body, status) \
             VALUES (?1, ?2, ?3, ?4, 'staged')",
            rusqlite::params![id, kind, recipient.trim(), body],
        )?;
        Ok(())
    })
    .map_err(|error| format!("the message could not be staged: {error}"))?;
    Ok(id)
}

/// Confirm a staged message, holding the recipient to the one that was staged.
///
/// # Errors
///
/// The sentence a caller shows. A different recipient is refused by name.
pub fn confirm(db: &Db, id: &str, recipient: &str) -> Result<Value, String> {
    let staged: Option<(String, String, String)> = db
        .with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT recipient, body, status FROM comms_outbox WHERE id = ?1",
                    rusqlite::params![id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .ok())
        })
        .map_err(|error| format!("the staged message could not be read: {error}"))?;
    let Some((staged_recipient, body, status)) = staged else {
        return Err("no staged message with that id exists".to_owned());
    };
    if status != "staged" {
        return Err(format!(
            "that message is already `{status}`; a confirmation cannot be replayed"
        ));
    }
    if !staged_recipient
        .trim()
        .eq_ignore_ascii_case(recipient.trim())
    {
        return Err(format!(
            "the confirmation names `{recipient}` but the message was staged for \
             `{staged_recipient}`. Nothing was confirmed: re-stage the message for the new \
             recipient instead."
        ));
    }
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE comms_outbox SET status = 'confirmed' WHERE id = ?1 AND status = 'staged'",
            rusqlite::params![id],
        )?;
        Ok(())
    })
    .map_err(|error| format!("the message could not be confirmed: {error}"))?;
    Ok(json!({
        "id": id,
        "recipient": staged_recipient,
        "body": body,
        "status": "confirmed",
        // Said out loud so no surface can render this as delivered.
        "delivered": false,
        "note": "confirmed and not sent: this daemon has no transport for it",
    }))
}

/// The two tools, as the model sees them.
#[must_use]
pub fn specs() -> Vec<core_runtime::provider::ToolSpec> {
    use core_runtime::provider::ToolSpec;
    vec![
        ToolSpec {
            name: "comms_stage".to_owned(),
            description: "Stage a message for the operator to confirm. Nothing is sent by this                           call or by any other: the recipient must already be on the comms                           allowlist, and the daemon has no transport. Use it to show the exact                           words and the exact recipient before anything else happens."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "enum": ["sms", "email", "call"]},
                    "recipient": {"type": "string", "description": "The allowlisted address or number."},
                    "body": {"type": "string", "description": "The exact words that would be sent."}
                },
                "required": ["kind", "recipient", "body"]
            }),
        },
        ToolSpec {
            name: "comms_confirm".to_owned(),
            description: "Confirm a staged message. The recipient must match the staged one                           exactly, and a confirmation cannot be replayed."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string", "description": "The staged id."},
                    "recipient": {"type": "string", "description": "The same recipient that was staged."}
                },
                "required": ["id", "recipient"]
            }),
        },
    ]
}

/// Run one call. `None` means this module does not own the name.
pub fn run(name: &str, arguments: &Value, db: &Db) -> Option<crate::tools::ToolResult> {
    match name {
        "comms_stage" => Some(stage_tool(arguments, db)),
        "comms_confirm" => Some(confirm_tool(arguments, db)),
        _ => None,
    }
}

fn stage_tool(arguments: &Value, db: &Db) -> crate::tools::ToolResult {
    let kind = arguments.get("kind").and_then(Value::as_str).unwrap_or("");
    let recipient = arguments
        .get("recipient")
        .and_then(Value::as_str)
        .unwrap_or("");
    let body = arguments.get("body").and_then(Value::as_str).unwrap_or("");
    match stage(db, kind, recipient, body) {
        Ok(id) => crate::tools::ToolResult::ok(json!({
            "id": id,
            "kind": kind,
            "recipient": recipient,
            "status": "staged",
            "delivered": false,
        })),
        Err(message) => crate::tools::ToolResult::refused(&message),
    }
}

fn confirm_tool(arguments: &Value, db: &Db) -> crate::tools::ToolResult {
    let id = arguments.get("id").and_then(Value::as_str).unwrap_or("");
    let recipient = arguments
        .get("recipient")
        .and_then(Value::as_str)
        .unwrap_or("");
    match confirm(db, id, recipient) {
        Ok(value) => crate::tools::ToolResult::ok(value),
        Err(message) => crate::tools::ToolResult::refused(&message),
    }
}

#[cfg(test)]
mod tests {
    use super::{SETTING_ALLOWLIST, confirm, stage};
    use crate::db::Db;

    fn with_allowlist(entries: &str) -> Db {
        let db = Db::open_in_memory().expect("database");
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = ?2",
                rusqlite::params![SETTING_ALLOWLIST, entries],
            )?;
            Ok(())
        })
        .expect("allowlist");
        db
    }

    /// A recipient nobody named is refused before anything is written.
    #[test]
    fn a_stranger_is_refused_and_nothing_is_staged() {
        let db = with_allowlist("kristie@example.test");
        let refused = stage(&db, "sms", "+15025550100", "hi").expect_err("refused");
        assert!(refused.contains("allowlist"), "{refused}");
        let rows: i64 = db
            .with_conn(|conn| {
                Ok(conn.query_row("SELECT count(*) FROM comms_outbox", [], |r| r.get(0))?)
            })
            .expect("count");
        assert_eq!(rows, 0);
    }

    /// The confirmation cannot swap the recipient: that is the attack a confirm
    /// step exists to stop.
    #[test]
    fn a_confirmation_cannot_swap_the_recipient() {
        let db = with_allowlist("kristie@example.test,amber@example.test");
        let id = stage(&db, "email", "kristie@example.test", "dinner at six").expect("staged");
        let refused = confirm(&db, &id, "amber@example.test").expect_err("refused");
        assert!(refused.contains("staged for"), "{refused}");
        let ok = confirm(&db, &id, "kristie@example.test").expect("confirmed");
        assert_eq!(ok["status"], "confirmed");
        assert_eq!(ok["delivered"], false);
    }

    /// A confirmation cannot be replayed: the second one finds a message that is
    /// no longer staged.
    #[test]
    fn a_confirmation_cannot_be_replayed() {
        let db = with_allowlist("kristie@example.test");
        let id = stage(&db, "sms", "kristie@example.test", "on my way").expect("staged");
        confirm(&db, &id, "kristie@example.test").expect("first");
        let refused = confirm(&db, &id, "kristie@example.test").expect_err("second");
        assert!(refused.contains("cannot be replayed"), "{refused}");
    }

    /// An unknown kind is refused by name with the legal set.
    #[test]
    fn an_unknown_kind_is_refused() {
        let db = with_allowlist("kristie@example.test");
        let refused = stage(&db, "pigeon", "kristie@example.test", "hi").expect_err("refused");
        assert!(refused.contains("sms, email, call"), "{refused}");
    }
}
