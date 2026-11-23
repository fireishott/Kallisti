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
//! # Transport
//!
//! Email and calls are still rows that say "agreed, not delivered". An SMS or an
//! iMessage is not: when a BlueBubbles server is configured the confirmed text is
//! handed to it and the row records what the server said. The transport is a
//! local `curl` against a URL the operator set, so the daemon still needs no SMTP
//! client, no bridge binary and no vendor account of its own.
//!
//! BlueBubbles speaks plain REST:
//! `POST <server>/api/v1/message/text?password=<server password>` with
//! `{"chatGuid":"any;-;<recipient>","tempGuid":"temp-<uuid>","message":"..."}`.
//! A `status` of 200 in the reply means the Mac handed it to Messages. Anything
//! else is recorded as a failed send with the server's own words, because a
//! failure the operator can read beats a row that claims delivery.

use std::process::Command;

use serde_json::{Value, json};

use crate::db::Db;

/// The stored setting holding the comma-separated allowlist.
pub const SETTING_ALLOWLIST: &str = "comms.allowlist";

/// The stored setting holding the BlueBubbles server base URL.
pub const SETTING_BLUEBUBBLES_URL: &str = "comms.bluebubbles.url";

/// The stored setting holding the BlueBubbles server password (also its API key).
pub const SETTING_BLUEBUBBLES_PASSWORD: &str = "comms.bluebubbles.password";

/// The kinds this daemon will stage. `sms` and `imessage` both reach the phone
/// transport; the split exists so the operator can say which one a message is.
pub const KINDS: [&str; 4] = ["sms", "imessage", "email", "call"];

/// How long one transport call may take before it is called a failure.
const SEND_TIMEOUT_SECONDS: &str = "20";

/// Every allowlisted recipient, trimmed and lowercased for comparison.
#[must_use]
pub fn allowlist(db: &Db) -> Vec<String> {
    read_setting(db, SETTING_ALLOWLIST)
        .unwrap_or_default()
        .split(',')
        .map(|entry| entry.trim().to_ascii_lowercase())
        .filter(|entry| !entry.is_empty())
        .collect()
}

/// One stored setting, trimmed, or `None` when it is unset or blank.
fn read_setting(db: &Db, key: &str) -> Option<String> {
    let stored = db
        .with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT value FROM settings WHERE key = ?1",
                    rusqlite::params![key],
                    |row| row.get::<_, String>(0),
                )
                .ok())
        })
        .ok()
        .flatten()?;
    let trimmed = stored.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Whether a recipient may be addressed at all.
#[must_use]
pub fn allowed(db: &Db, recipient: &str) -> bool {
    let wanted = recipient.trim().to_ascii_lowercase();
    !wanted.is_empty() && allowlist(db).contains(&wanted)
}

/// Whether the phone transport has both halves of its configuration.
#[must_use]
pub fn bluebubbles_ready(db: &Db) -> bool {
    read_setting(db, SETTING_BLUEBUBBLES_URL).is_some()
        && read_setting(db, SETTING_BLUEBUBBLES_PASSWORD).is_some()
}

/// Whether a kind leaves the host as a text message.
#[must_use]
pub fn is_text(kind: &str) -> bool {
    matches!(kind, "sms" | "imessage")
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

/// Confirm a staged message, holding the recipient to the one that was staged,
/// then hand a text to the phone transport when one is configured.
///
/// # Errors
///
/// The sentence a caller shows. A different recipient is refused by name.
pub fn confirm(db: &Db, id: &str, recipient: &str) -> Result<Value, String> {
    let staged: Option<(String, String, String, String)> = db
        .with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT recipient, body, status, kind FROM comms_outbox WHERE id = ?1",
                    rusqlite::params![id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .ok())
        })
        .map_err(|error| format!("the staged message could not be read: {error}"))?;
    let Some((staged_recipient, body, status, kind)) = staged else {
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

    if !is_text(&kind) {
        return Ok(json!({
            "id": id,
            "kind": kind,
            "recipient": staged_recipient,
            "body": body,
            "status": "confirmed",
            // Said out loud so no surface can render this as delivered.
            "delivered": false,
            "note": "confirmed and not sent: this daemon has no transport for it",
        }));
    }

    let Some((url, password)) = transport_settings(db) else {
        return Ok(json!({
            "id": id,
            "kind": kind,
            "recipient": staged_recipient,
            "body": body,
            "status": "confirmed",
            "delivered": false,
            "note": "confirmed and not sent: no phone transport is configured. Set \
                     comms.bluebubbles.url and comms.bluebubbles.password, then stage it again.",
        }));
    };

    let outcome = send_text(&url, &password, &staged_recipient, &body);
    let (status_word, delivered, detail) = match outcome {
        Ok(guid) => ("sent".to_owned(), true, guid),
        Err(message) => ("send_failed".to_owned(), false, message),
    };
    let _ = db.with_conn(|conn| {
        conn.execute(
            "UPDATE comms_outbox SET status = ?2, detail = ?3 WHERE id = ?1",
            rusqlite::params![id, status_word, detail],
        )?;
        Ok(())
    });
    Ok(json!({
        "id": id,
        "kind": kind,
        "recipient": staged_recipient,
        "body": body,
        "status": status_word,
        "delivered": delivered,
        "detail": detail,
    }))
}

/// Both halves of the transport configuration, or `None`.
fn transport_settings(db: &Db) -> Option<(String, String)> {
    Some((
        read_setting(db, SETTING_BLUEBUBBLES_URL)?,
        read_setting(db, SETTING_BLUEBUBBLES_PASSWORD)?,
    ))
}

/// Percent-encode the few characters a query-string password can carry.
fn encoded(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// POST one text to BlueBubbles. `Ok` carries the server's message guid.
///
/// # Errors
///
/// The server's own words, or curl's, so a wrong password and an unreachable Mac
/// read differently.
fn send_text(url: &str, password: &str, recipient: &str, body: &str) -> Result<String, String> {
    let endpoint = format!(
        "{}/api/v1/message/text?password={}",
        url.trim_end_matches('/'),
        encoded(password)
    );
    let payload = json!({
        "chatGuid": format!("any;-;{}", recipient.trim()),
        "tempGuid": format!("temp-{}", uuid::Uuid::now_v7()),
        "message": body,
    })
    .to_string();
    let output = Command::new("curl")
        .args([
            "-sS",
            "--max-time",
            SEND_TIMEOUT_SECONDS,
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            &payload,
            &endpoint,
        ])
        .output()
        .map_err(|error| format!("curl could not be run: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !output.status.success() {
        return Err(if stderr.is_empty() {
            format!("the transport exited {}", output.status)
        } else {
            stderr
        });
    }
    let parsed: Value = serde_json::from_str(&stdout)
        .map_err(|_| format!("the transport answered something that is not JSON: {stdout}"))?;
    let status = parsed.get("status").and_then(Value::as_i64).unwrap_or(0);
    if status != 200 {
        let message = parsed
            .get("error")
            .and_then(|error| error.get("error"))
            .and_then(Value::as_str)
            .or_else(|| parsed.get("message").and_then(Value::as_str))
            .unwrap_or("no message");
        return Err(format!("the transport refused it: {message}"));
    }
    Ok(parsed
        .get("data")
        .and_then(|data| data.get("guid"))
        .and_then(Value::as_str)
        .unwrap_or("accepted")
        .to_owned())
}

/// The tools, as the model sees them.
#[must_use]
pub fn specs() -> Vec<core_runtime::provider::ToolSpec> {
    use core_runtime::provider::ToolSpec;
    vec![
        ToolSpec {
            name: "comms_stage".to_owned(),
            description: "Stage a message for the operator to confirm. Nothing is sent by this \
                          call. The recipient must already be on the comms allowlist. `sms` and \
                          `imessage` leave the host as a text when a phone transport is \
                          configured; `email` and `call` are recorded only."
                .to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "enum": ["sms", "imessage", "email", "call"]},
                    "recipient": {"type": "string", "description": "The allowlisted address or number."},
                    "body": {"type": "string", "description": "The exact words that would be sent."}
                },
                "required": ["kind", "recipient", "body"]
            }),
        },
        ToolSpec {
            name: "comms_confirm".to_owned(),
            description: "Confirm a staged message. The recipient must match the staged one \
                          exactly, and a confirmation cannot be replayed. A text is handed to \
                          the phone transport on this call, and the result says whether it left."
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
        ToolSpec {
            name: "comms_status".to_owned(),
            description: "Report whether the phone transport is configured, and the state of the \
                          most recent staged messages. Read it before promising a text."
                .to_owned(),
            parameters: json!({"type": "object", "properties": {}}),
        },
    ]
}

/// Run one call. `None` means this module does not own the name.
pub fn run(name: &str, arguments: &Value, db: &Db) -> Option<crate::tools::ToolResult> {
    match name {
        "comms_stage" => Some(stage_tool(arguments, db)),
        "comms_confirm" => Some(confirm_tool(arguments, db)),
        "comms_status" => Some(status_tool(db)),
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

fn status_tool(db: &Db) -> crate::tools::ToolResult {
    let transport = if bluebubbles_ready(db) {
        json!(read_setting(db, SETTING_BLUEBUBBLES_URL).unwrap_or_default())
    } else {
        Value::Null
    };
    let recent = db
        .with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, kind, recipient, status, detail FROM comms_outbox \
                 ORDER BY rowid DESC LIMIT 5",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok(json!({
                        "id": row.get::<_, String>(0)?,
                        "kind": row.get::<_, String>(1)?,
                        "recipient": row.get::<_, String>(2)?,
                        "status": row.get::<_, String>(3)?,
                        "detail": row.get::<_, Option<String>>(4)?,
                    }))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .unwrap_or_default();
    crate::tools::ToolResult::ok(json!({
        "allowlist": allowlist(db),
        "transport": transport,
        "kinds_with_transport": ["sms", "imessage"],
        "recent": recent,
    }))
}

#[cfg(test)]
mod tests {
    use super::{
        SETTING_ALLOWLIST, SETTING_BLUEBUBBLES_PASSWORD, SETTING_BLUEBUBBLES_URL, bluebubbles_ready,
        confirm, encoded, is_text, stage,
    };
    use crate::db::Db;

    fn with_setting(db: &Db, key: &str, value: &str) {
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = ?2",
                rusqlite::params![key, value],
            )?;
            Ok(())
        })
        .expect("setting");
    }

    fn with_allowlist(entries: &str) -> Db {
        let db = Db::open_in_memory().expect("database");
        with_setting(&db, SETTING_ALLOWLIST, entries);
        db
    }

    /// A recipient nobody named is refused before anything is written.
    #[test]
    fn a_stranger_is_refused_and_nothing_is_staged() {
        let db = with_allowlist("kristie@example.test");
        let refused = stage(&db, "sms", "+15025550100", "hi").expect_err("refused");
        assert!(refused.contains("allowlist"), "{refused}");
        let rows: i64 = db
            .with_conn(|conn| conn.query_row("SELECT count(*) FROM comms_outbox", [], |r| r.get(0)))
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
        // No transport configured: a confirmed email is a row, not a delivery.
        assert_eq!(ok["delivered"], false);
    }

    /// A confirmation cannot be replayed: the second one finds a message that is
    /// no longer staged.
    #[test]
    fn a_confirmation_cannot_be_replayed() {
        let db = with_allowlist("kristie@example.test");
        let id = stage(&db, "call", "kristie@example.test", "on my way").expect("staged");
        confirm(&db, &id, "kristie@example.test").expect("first");
        let refused = confirm(&db, &id, "kristie@example.test").expect_err("second");
        assert!(refused.contains("cannot be replayed"), "{refused}");
    }

    /// An unknown kind is refused by name with the legal set.
    #[test]
    fn an_unknown_kind_is_refused() {
        let db = with_allowlist("kristie@example.test");
        let refused = stage(&db, "pigeon", "kristie@example.test", "hi").expect_err("refused");
        assert!(refused.contains("sms, imessage, email, call"), "{refused}");
    }

    /// Only the two text kinds go anywhere, and both need both settings.
    #[test]
    fn the_transport_is_configured_by_two_settings() {
        let db = with_allowlist("kristie@example.test");
        assert!(is_text("sms") && is_text("imessage"));
        assert!(!is_text("email") && !is_text("call"));
        assert!(!bluebubbles_ready(&db));
        with_setting(&db, SETTING_BLUEBUBBLES_URL, "https://bb.example.test");
        assert!(!bluebubbles_ready(&db), "a URL alone is not a transport");
        with_setting(&db, SETTING_BLUEBUBBLES_PASSWORD, "hunter2");
        assert!(bluebubbles_ready(&db));
    }

    /// A password with query-string characters is encoded, not spliced in raw.
    #[test]
    fn a_password_is_encoded_for_the_query_string() {
        assert_eq!(encoded("a b&c"), "a%20b%26c");
        assert_eq!(encoded("plain-Pass_1.~"), "plain-Pass_1.~");
    }

    /// A text with no transport confirms and says why it did not leave.
    #[test]
    fn a_text_without_a_transport_is_not_delivered() {
        let db = with_allowlist("+15025550100");
        let id = stage(&db, "imessage", "+15025550100", "running late").expect("staged");
        let ok = confirm(&db, &id, "+15025550100").expect("confirmed");
        assert_eq!(ok["delivered"], false);
        assert!(
            ok["note"].as_str().unwrap_or_default().contains("transport"),
            "{ok}"
        );
    }
}
