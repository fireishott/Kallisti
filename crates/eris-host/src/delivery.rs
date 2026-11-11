//! Delivering a scheduled result.
//!
//! The schedule engine has worked since 1.5.0 and never ran: `schedules` and
//! `schedule_runs` both held zero rows, because a run whose answer lands in a
//! conversation nobody is looking at is a run with nowhere to arrive. Delivery is
//! the missing half.
//!
//! Three channels, chosen per schedule:
//!
//! - `none` - the answer is written to its conversation and nothing else happens.
//! - `push` - the registered devices get an alert with the first line of the answer.
//! - `email` - one address, the permitted one, gets the answer.
//! - `both` - push and email.
//!
//! Email is deliberately narrow. There is one recipient, it must match the
//! configured address exactly, and anything else is refused by name. A daemon that
//! can mail an arbitrary address on a schedule is a spam relay with a cron entry,
//! and the rule against it lives in the persona file for the same reason the
//! operator wrote it there: it is a standing instruction, not a default.
//!
//! The daemon has no SMTP client and no mail credentials, so it writes a
//! standards-shaped `.eml` into `state/outbox` and stops. That is the honest
//! boundary: composing a message is this daemon's job, and putting it on the wire
//! is the job of whichever agent already holds the mailbox. Nothing is marked sent
//! that was only written.

use std::path::{Path, PathBuf};

use crate::db::Db;
use crate::scheduler::Schedule;

/// The stored setting holding the one permitted recipient.
pub const SETTING_EMAIL_TO: &str = "deliver.email_to";

/// The stored setting holding the envelope sender.
pub const SETTING_EMAIL_FROM: &str = "deliver.email_from";

/// The default envelope sender. It is not a real mailbox and does not claim to be.
pub const DEFAULT_EMAIL_FROM: &str = "eris@kallisti.local";

/// The channel a schedule uses when the operator does not say.
pub const DEFAULT_CHANNEL: &str = "push";

/// Every channel a schedule may name.
pub const CHANNELS: [&str; 4] = ["none", "push", "email", "both"];

/// Whether a value names a channel.
#[must_use]
pub fn valid(value: &str) -> bool {
    CHANNELS.contains(&value.trim().to_ascii_lowercase().as_str())
}

/// A stored channel, normalised, or the default when it is missing or nonsense.
#[must_use]
pub fn normalise(stored: Option<&str>) -> String {
    match stored {
        Some(value) if valid(value) => value.trim().to_ascii_lowercase(),
        _ => DEFAULT_CHANNEL.to_owned(),
    }
}

/// Whether a channel means push, and whether it means email.
#[must_use]
pub fn channels(value: &str) -> (bool, bool) {
    let value = normalise(Some(value));
    match value.as_str() {
        "push" => (true, false),
        "email" => (false, true),
        "both" => (true, true),
        _ => (false, false),
    }
}

/// The one address this daemon may write to, when the operator configured one.
#[must_use]
pub fn allowed_recipient(db: &Db) -> Option<String> {
    crate::web::store::settings_get(db, SETTING_EMAIL_TO)
        .ok()
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Refuse any recipient but the permitted one.
///
/// # Errors
///
/// A sentence naming the rule and where it lives, so a model that tried to widen
/// the audience is told why rather than told "no".
pub fn guard_recipient(to: &str, allowed: Option<&str>) -> Result<String, String> {
    let to = to.trim();
    if to.is_empty() {
        return Err(
            "email delivery needs a recipient, and this daemon has exactly one: set \
             `deliver.email_to` and it must match. Nothing was sent."
                .to_owned(),
        );
    }
    let Some(allowed) = allowed.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(
            "email delivery is not configured: `deliver.email_to` names no address, so there \
             is no permitted recipient to send to. Nothing was sent."
                .to_owned(),
        );
    };
    if !to.eq_ignore_ascii_case(allowed) {
        return Err(format!(
            "refused: this daemon delivers to one address only (`{allowed}`), and `{to}` is not \
             it. CC, BCC and extra recipients do not exist here; the rule is a standing \
             instruction in persona.md and a hard guard in the daemon. Nothing was sent."
        ));
    }
    Ok(to.to_owned())
}

/// Write one message to the outbox and return its path.
///
/// # Errors
///
/// A sentence when the directory or the file cannot be written.
pub fn write_email(
    dir: &Path,
    from: &str,
    to: &str,
    subject: &str,
    body: &str,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|error| format!("cannot make the outbox: {error}"))?;
    let name = format!(
        "{}-{}.eml",
        crate::learning::now().replace([':', '.'], "-"),
        uuid::Uuid::now_v7().simple()
    );
    let path = dir.join(name);
    // Header folding is not needed for a subject a schedule generated, but a
    // newline in the subject would forge a header, so it is collapsed to one line
    // and one space between words.
    let subject = subject.split_whitespace().collect::<Vec<_>>().join(" ");
    let message = format!(
        "To: {to}\r\nFrom: {from}\r\nSubject: {subject}\r\nDate: {date}\r\n\
         MIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\n\
         X-Kallisti-Delivery: scheduled\r\n\r\n{body}\r\n",
        date = crate::learning::now(),
    );
    std::fs::write(&path, message).map_err(|error| format!("cannot write the message: {error}"))?;
    Ok(path)
}

/// Deliver one finished run, and say what happened.
///
/// Returns `(delivered, note)`: `delivered` is what landed in the run row, and
/// `note` is the sentence an operator reads on the schedules page. A failure here
/// never fails the run: the answer exists and was written, which is the part that
/// matters, and a delivery problem is reported next to it rather than replacing it.
pub async fn deliver(
    state: &crate::admin::AppState,
    schedule: &Schedule,
    run_id: &str,
    conversation: &str,
    text: Option<&str>,
) -> (String, String) {
    let (push, email) = channels(&schedule.deliver);
    if !push && !email {
        return (
            "none".to_owned(),
            "delivery is off for this schedule".to_owned(),
        );
    }
    let body = text.unwrap_or("the run finished with no text").to_owned();
    let mut notes: Vec<String> = Vec::new();
    let mut landed: Vec<String> = Vec::new();

    if push {
        match push_alert(state, schedule, run_id, conversation, &body).await {
            Ok(note) => {
                landed.push("push".to_owned());
                notes.push(note);
            }
            Err(note) => notes.push(format!("push did not send: {note}")),
        }
    }
    if email {
        match email_message(state, schedule, &body) {
            Ok(note) => {
                landed.push("email".to_owned());
                notes.push(note);
            }
            Err(note) => notes.push(format!("email did not send: {note}")),
        }
    }
    if landed.is_empty() {
        return ("none".to_owned(), notes.join("; "));
    }
    (landed.join("+"), notes.join("; "))
}

async fn push_alert(
    state: &crate::admin::AppState,
    schedule: &Schedule,
    run_id: &str,
    conversation: &str,
    text: &str,
) -> Result<String, String> {
    let sender = match crate::apns::ApnsSender::from_env() {
        Ok(Some(sender)) => sender,
        Ok(None) => return Err("no APNs sender is configured on this host".to_owned()),
        Err(error) => return Err(error.to_string()),
    };
    let store = crate::push::PushStore::open(state.engine.paths().state_dir.join("push.db"))
        .map_err(|error| error.to_string())?;
    let targets = store.all_active().map_err(|error| error.to_string())?;
    if targets.is_empty() {
        return Err("no device is registered for push".to_owned());
    }
    let (title, body, info) =
        crate::push::reply_notification(text, Some(conversation), run_id, None);
    let title = format!("{}: {title}", schedule.name);
    let mut sent = 0usize;
    let mut last: Option<String> = None;
    for (_device, token, environment, bundle) in targets {
        let Some(token) = token.filter(|token| !token.trim().is_empty()) else {
            continue;
        };
        let outcome = sender
            .send_alert(
                &token,
                bundle.as_deref(),
                Some(environment.as_str()),
                &title,
                &body,
                Some(crate::push::MESSAGE_READY_CATEGORY),
                Some(&info),
            )
            .await;
        match outcome {
            crate::apns::Outcome::Sent => sent += 1,
            other => last = Some(format!("{other:?}")),
        }
    }
    if sent == 0 {
        return Err(last.unwrap_or_else(|| "no device carried a usable token".to_owned()));
    }
    Ok(format!("pushed to {sent} device(s)"))
}

fn email_message(
    state: &crate::admin::AppState,
    schedule: &Schedule,
    text: &str,
) -> Result<String, String> {
    let db = state.engine.db();
    let allowed = allowed_recipient(db);
    let to = guard_recipient(allowed.as_deref().unwrap_or_default(), allowed.as_deref())?;
    let from = crate::web::store::settings_get(db, SETTING_EMAIL_FROM)
        .ok()
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_EMAIL_FROM.to_owned());
    let dir = state.engine.paths().state_dir.join("outbox");
    let subject = format!("Kallisti: {}", schedule.name);
    let path = write_email(&dir, &from, &to, &subject, text)?;
    Ok(format!("wrote {} for {to}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_channel_set_is_closed_and_defaults_to_push() {
        for value in CHANNELS {
            assert!(valid(value), "{value}");
        }
        assert!(!valid("telegram"), "no channel bridges, here or anywhere");
        assert_eq!(normalise(None), DEFAULT_CHANNEL);
        assert_eq!(normalise(Some("EMAIL")), "email");
        assert_eq!(normalise(Some("nonsense")), DEFAULT_CHANNEL);
    }

    #[test]
    fn a_channel_plans_its_two_halves() {
        assert_eq!(channels("push"), (true, false));
        assert_eq!(channels("email"), (false, true));
        assert_eq!(channels("both"), (true, true));
        assert_eq!(channels("none"), (false, false));
    }

    #[test]
    fn one_recipient_and_no_others() {
        let allowed = Some("operator@example.test");
        assert!(guard_recipient("operator@example.test", allowed).is_ok());
        assert!(
            guard_recipient("OPERATOR@example.test", allowed).is_ok(),
            "a case difference is the same address"
        );
        let refused = guard_recipient("someone-else@example.test", allowed)
            .expect_err("a second address is refused");
        assert!(refused.contains("one address only"), "{refused}");
        assert!(
            refused.contains("persona.md"),
            "the rule's home is named: {refused}"
        );
        assert!(refused.contains("Nothing was sent"));
    }

    #[test]
    fn an_unconfigured_recipient_says_so_rather_than_sending_anywhere() {
        let refused = guard_recipient("operator@example.test", None).expect_err("refused");
        assert!(refused.contains("not configured"), "{refused}");
        let empty = guard_recipient("", Some("operator@example.test")).expect_err("refused");
        assert!(empty.contains("exactly one"), "{empty}");
    }

    #[test]
    fn the_message_is_a_well_formed_eml_with_a_stripped_subject() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = write_email(
            dir.path(),
            "eris@kallisti.local",
            "operator@example.test",
            "Kallisti: brief\r\nBcc: evil@example.test",
            "the morning brief",
        )
        .expect("written");
        let body = std::fs::read_to_string(&path).expect("read");
        assert!(body.starts_with("To: operator@example.test\r\n"), "{body}");
        assert!(
            body.contains("Subject: Kallisti: brief Bcc: evil@example.test"),
            "{body}"
        );
        assert!(
            !body.contains("\r\nBcc:"),
            "a subject cannot forge a header: {body}"
        );
        assert!(body.ends_with("the morning brief\r\n"), "{body}");
        assert!(path.extension().is_some_and(|ext| ext == "eml"));
    }
}
