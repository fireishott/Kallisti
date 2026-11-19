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

/// The stored setting holding a relay to hand the message to, `host:port`.
///
/// Absent, the message is only written to the outbox: composing a message is this
/// daemon's job and putting it on the wire needs a relay the operator names.
pub const SETTING_SMTP_HOST: &str = "deliver.smtp_host";

/// The default envelope sender. It is not a real mailbox and does not claim to be.
pub const DEFAULT_EMAIL_FROM: &str = "eris@kallisti.local";

/// The `settings` row holding the channel a new schedule starts on.
pub const SETTING_DELIVERY_DEFAULT: &str = "schedule.delivery_default";

/// The `settings` row that allows mail at all.
pub const SETTING_EMAIL_ENABLED: &str = "schedule.email_enabled";

/// The channel a schedule uses when the operator does not say.
pub const DEFAULT_CHANNEL: &str = "push";

/// Every channel a schedule may name.
///
/// `none` and `both` are the pre-1.8 spellings and stay accepted: a schedule row
/// written before this list grew must keep working, and `both` is exactly
/// `push`+`email`.
pub const CHANNELS: [&str; 6] = ["off", "none", "push", "inbox", "email", "both"];

/// The values the delivery setting offers.
pub const DEFAULT_VALUES: &[&str] = &["push", "inbox", "email", "off"];

/// Whether a value names a channel.
#[must_use]
pub fn valid(value: &str) -> bool {
    CHANNELS.contains(&value.trim().to_ascii_lowercase().as_str())
}

/// A stored channel, normalised, or the stored default when it is missing or
/// nonsense.
#[must_use]
pub fn normalise(db: &Db, stored: Option<&str>) -> String {
    match stored {
        Some(value) if valid(value) => value.trim().to_ascii_lowercase(),
        _ => default_channel(db),
    }
}

/// The channel a schedule starts on, from the operator's setting.
#[must_use]
pub fn default_channel(db: &Db) -> String {
    crate::web::store::settings_get(db, SETTING_DELIVERY_DEFAULT)
        .ok()
        .flatten()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| DEFAULT_VALUES.contains(&value.as_str()))
        .unwrap_or_else(|| DEFAULT_CHANNEL.to_owned())
}

/// Whether mail may leave this daemon at all.
#[must_use]
pub fn email_enabled(db: &Db) -> bool {
    !matches!(
        crate::web::store::settings_get(db, SETTING_EMAIL_ENABLED)
            .ok()
            .flatten()
            .as_deref()
            .map(str::trim),
        Some("false")
    )
}

/// The status a run's `delivered` column adds up to.
///
/// The column holds what landed, which is the useful thing to read and not a
/// word a page can sort on. This is the one mapping from it to a status, so the
/// page and any later reader agree.
#[must_use]
pub fn status_of(delivered: Option<&str>) -> &'static str {
    let value = delivered.unwrap_or_default().trim();
    if value.is_empty() {
        "pending"
    } else if value == "off" || value == "none" {
        "off"
    } else if value.contains('+') {
        "partial"
    } else {
        "delivered"
    }
}

/// Whether a channel means push, inbox, or email.
#[must_use]
pub fn channels(db: &Db, value: &str) -> (bool, bool, bool) {
    match normalise(db, Some(value)).as_str() {
        "push" => (true, false, false),
        "inbox" => (false, true, false),
        "email" => (false, false, true),
        "both" => (true, false, true),
        _ => (false, false, false),
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
    let (push, inbox, email) = channels(state.engine.db(), &schedule.deliver);
    if !push && !inbox && !email {
        return (
            "off".to_owned(),
            "delivery is off for this schedule".to_owned(),
        );
    }
    let body = text.unwrap_or("the run finished with no text").to_owned();
    let mut notes: Vec<String> = Vec::new();
    let mut landed: Vec<String> = Vec::new();

    if push {
        match twice(|| push_alert(state, schedule, run_id, conversation, &body)).await {
            Ok(note) => {
                landed.push("push".to_owned());
                notes.push(note);
            }
            Err(note) => notes.push(format!("push did not send: {note}")),
        }
    }
    // A schedule that asked for mail on a daemon with mail switched off still has
    // somewhere to go, and the run row says that is what happened rather than
    // reporting a failure the operator cannot act on.
    let (inbox, email) = if email && !email_enabled(state.engine.db()) {
        notes.push("mail is off, so it went to the inbox instead".to_owned());
        (true, false)
    } else {
        (inbox, email)
    };
    if inbox {
        match inbox_item(state, schedule, run_id, conversation, &body) {
            Ok(note) => {
                landed.push("inbox".to_owned());
                notes.push(note);
            }
            Err(note) => notes.push(format!("the inbox did not take it: {note}")),
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

/// How long the single retry waits. Short: the run has already been written and
/// the next fire does not wait on this.
const RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(500);

/// Run one attempt, and once more after a second, if it failed.
async fn twice<F, Fut>(attempt: F) -> Result<String, String>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<String, String>>,
{
    match attempt().await {
        Ok(note) => Ok(note),
        Err(first) => {
            tokio::time::sleep(RETRY_BACKOFF).await;
            attempt()
                .await
                .map_err(|again| format!("{first}; a second try: {again}"))
        }
    }
}

/// Put one item in every paired device's inbox.
fn inbox_item(
    state: &crate::admin::AppState,
    schedule: &Schedule,
    run_id: &str,
    conversation: &str,
    text: &str,
) -> Result<String, String> {
    let store = crate::push::PushStore::open(state.engine.paths().state_dir.join("push.db"))
        .map_err(|error| error.to_string())?;
    let targets = store.all_active().map_err(|error| error.to_string())?;
    let devices: Vec<String> = targets
        .into_iter()
        .map(|(device, _token, _environment, _bundle)| device)
        .collect();
    if devices.is_empty() {
        return Err("no device is registered".to_owned());
    }
    let inbox = crate::inbox::InboxStore::open(state.engine.paths().state_dir.join("inbox.db"))
        .map_err(|error| error.to_string())?;
    let payload = serde_json::json!({
        "conversation": conversation,
        "run": run_id,
        "schedule": schedule.id,
    });
    let mut written = 0usize;
    for device in &devices {
        let title = format!("{}: run finished", schedule.name);
        let body: String = text.chars().take(400).collect();
        if inbox
            .create(
                device,
                "schedule",
                &title,
                &body,
                "normal",
                Some(&payload),
                None,
            )
            .is_ok()
        {
            written += 1;
        }
    }
    if written == 0 {
        return Err("the inbox refused every item".to_owned());
    }
    Ok(format!("inbox: {written} device(s)"))
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
    let Some(relay) = crate::web::store::settings_get(db, SETTING_SMTP_HOST)
        .ok()
        .flatten()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
    else {
        return Ok(format!("wrote {} for {to} (no relay set)", path.display()));
    };
    let message = std::fs::read_to_string(&path).unwrap_or_default();
    match smtp_send(&relay, &from, &to, &message) {
        Ok(()) => {
            tracing::info!(%relay, to = %to, "smtp: handed the message to the relay");
            Ok(format!("smtp {relay}: {to}"))
        }
        Err(error) => Err(format!(
            "the message is at {} but the relay `{relay}` refused it: {error}",
            path.display()
        )),
    }
}

/// Hand one message to a relay, plain SMTP over one TCP connection.
///
/// No TLS: a relay on loopback or the LAN is the case this exists for, and a
/// STARTTLS handshake without a TLS crate would be a promise this cannot keep.
/// The envelope is fixed to one sender and one recipient, which is the same rule
/// the guard enforces a level up.
fn smtp_send(relay: &str, from: &str, to: &str, message: &str) -> Result<(), String> {
    use std::io::{BufRead, BufReader, Write};
    let stream = std::net::TcpStream::connect(relay)
        .map_err(|error| format!("cannot reach `{relay}`: {error}"))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(15)))
        .ok();
    let mut reader = BufReader::new(stream.try_clone().map_err(|error| error.to_string())?);
    let mut writer = stream;

    let mut greet = String::new();
    reader
        .read_line(&mut greet)
        .map_err(|error| error.to_string())?;
    if !greet.starts_with("220") {
        return Err(format!("the relay opened with `{}`", greet.trim()));
    }
    for line in [
        "EHLO kallisti.local".to_owned(),
        format!("MAIL FROM:<{from}>"),
        format!("RCPT TO:<{to}>"),
        "DATA".to_owned(),
    ] {
        writer
            .write_all(format!("{line}\r\n").as_bytes())
            .map_err(|error| error.to_string())?;
        let mut reply = String::new();
        reader
            .read_line(&mut reply)
            .map_err(|error| error.to_string())?;
        let wanted = if line == "DATA" { "354" } else { "2" };
        if !reply.starts_with(wanted) {
            let verb = line.split(' ').next().unwrap_or_default().to_owned();
            return Err(format!("`{verb}` answered `{}`", reply.trim()));
        }
    }
    // Dot-stuffing is the protocol's own rule: a line that is one dot would end
    // the body early.
    for line in message.lines() {
        let stuffed = if line.starts_with('.') {
            format!(".{line}")
        } else {
            line.to_owned()
        };
        writer
            .write_all(format!("{stuffed}\r\n").as_bytes())
            .map_err(|error| error.to_string())?;
    }
    writer
        .write_all(b".\r\n")
        .map_err(|error| error.to_string())?;
    let mut reply = String::new();
    reader
        .read_line(&mut reply)
        .map_err(|error| error.to_string())?;
    if !reply.starts_with('2') {
        return Err(format!("the relay answered `{}`", reply.trim()));
    }
    let _ = writer.write_all(b"QUIT\r\n");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("an in-memory database")
    }

    #[test]
    fn the_channel_set_is_closed_and_defaults_to_push() {
        for value in CHANNELS {
            assert!(valid(value), "{value}");
        }
        assert!(!valid("telegram"), "no channel bridges, here or anywhere");
        assert_eq!(default_channel(&db()), DEFAULT_CHANNEL);
        let db = db();
        assert_eq!(normalise(&db, Some("EMAIL")), "email");
        assert_eq!(normalise(&db, Some("nonsense")), DEFAULT_CHANNEL);
    }

    #[test]
    fn the_delivery_default_is_a_setting_and_an_unknown_one_falls_back() {
        let db = db();
        crate::web::store::settings_put(&db, SETTING_DELIVERY_DEFAULT, "inbox").expect("store");
        assert_eq!(default_channel(&db), "inbox");
        assert_eq!(normalise(&db, None), "inbox");
        // A schedule row that names its own channel still wins.
        assert_eq!(normalise(&db, Some("off")), "off");
        crate::web::store::settings_put(&db, SETTING_DELIVERY_DEFAULT, "pigeon").expect("store");
        assert_eq!(default_channel(&db), DEFAULT_CHANNEL);
    }

    #[test]
    fn mail_switched_off_is_a_fact_the_channel_plan_can_read() {
        let db = db();
        assert!(email_enabled(&db), "mail is on until it is turned off");
        crate::web::store::settings_put(&db, SETTING_EMAIL_ENABLED, "false").expect("store");
        assert!(!email_enabled(&db));
    }

    #[test]
    fn a_run_status_reads_the_delivery_column() {
        assert_eq!(status_of(None), "pending");
        assert_eq!(status_of(Some("")), "pending");
        assert_eq!(status_of(Some("off")), "off");
        assert_eq!(status_of(Some("none")), "off");
        assert_eq!(status_of(Some("push")), "delivered");
        assert_eq!(status_of(Some("push+inbox")), "partial");
    }

    #[test]
    fn a_channel_plans_its_halves() {
        let db = db();
        assert_eq!(channels(&db, "push"), (true, false, false));
        assert_eq!(channels(&db, "inbox"), (false, true, false));
        assert_eq!(channels(&db, "email"), (false, false, true));
        assert_eq!(channels(&db, "both"), (true, false, true));
        assert_eq!(channels(&db, "none"), (false, false, false));
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

/// The journal key for one run on one channel.
///
/// One event, one key. Both paths write the same key, so a run that goes out by
/// push and then by email cannot arrive twice through the same channel, and a
/// retry of a delivery that already landed is refused rather than duplicated.
#[must_use]
pub fn dedupe_key(run_id: &str, channel: &str) -> String {
    format!("{run_id}:{channel}")
}

/// Write one delivery outcome to the journal.
///
/// `false` means the key was already there: this delivery has been attempted
/// before and the caller must not send it again.
///
/// # Errors
///
/// A database failure.
pub fn journal(
    db: &Db,
    run_id: &str,
    channel: &str,
    status: &str,
    note: &str,
) -> Result<bool, crate::db::DbError> {
    let key = dedupe_key(run_id, channel);
    db.with_conn(|conn| {
        let inserted = conn.execute(
            "INSERT INTO delivery_journal (id, dedupe_key, run_id, channel, status, note) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(dedupe_key) DO NOTHING",
            rusqlite::params![
                uuid::Uuid::now_v7().to_string(),
                key,
                run_id,
                channel,
                status,
                note,
            ],
        )?;
        Ok(inserted == 1)
    })
}

/// What the journal holds for one run, oldest first.
///
/// # Errors
///
/// A database failure.
pub fn journal_for(db: &Db, run_id: &str) -> Result<Vec<DeliveryEntry>, crate::db::DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT channel, status, note, created_at FROM delivery_journal \
              WHERE run_id = ?1 ORDER BY created_at, id",
        )?;
        let rows = statement.query_map(rusqlite::params![run_id], |row| {
            Ok(DeliveryEntry {
                channel: row.get(0)?,
                status: row.get(1)?,
                note: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })
}

/// The newest journal entries, newest first.
///
/// # Errors
///
/// A database failure.
pub fn recent(db: &Db, limit: usize) -> Result<Vec<DeliveryEntry>, crate::db::DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT channel, status, note, created_at FROM delivery_journal \
              ORDER BY created_at DESC, id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map(rusqlite::params![limit as i64], |row| {
            Ok(DeliveryEntry {
                channel: row.get(0)?,
                status: row.get(1)?,
                note: row.get(2)?,
                created_at: row.get(3)?,
            })
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })
}

/// One journalled delivery attempt.
#[derive(Debug, Clone)]
pub struct DeliveryEntry {
    /// Which channel.
    pub channel: String,
    /// What happened.
    pub status: String,
    /// Why, when the status needs a reason.
    pub note: String,
    /// When it was recorded.
    pub created_at: String,
}

#[cfg(test)]
mod journal_tests {
    use super::{dedupe_key, journal, journal_for};
    use crate::db::Db;

    /// The same event on the same channel is journalled once: a retry is refused
    /// rather than sent twice.
    #[test]
    fn a_second_delivery_of_the_same_event_is_refused() {
        let db = Db::open_in_memory().expect("database");
        assert_eq!(dedupe_key("run-1", "push"), "run-1:push");
        assert!(journal(&db, "run-1", "push", "ok", "").expect("first"));
        assert!(!journal(&db, "run-1", "push", "ok", "").expect("second"));
        let entries = journal_for(&db, "run-1").expect("read");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].channel, "push");
    }

    /// The newest first, bounded: a journal is read to find the last failure, not
    /// to page through history.
    #[test]
    fn the_journal_reads_newest_first() {
        let db = Db::open_in_memory().expect("database");
        journal(&db, "run-a", "push", "ok", "").expect("a");
        journal(&db, "run-b", "email", "failed", "no recipient").expect("b");
        let rows = super::recent(&db, 10).expect("read");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].status, "failed", "the newest is first");
        assert_eq!(super::recent(&db, 1).expect("read").len(), 1);
    }

    /// Two channels for one run are two entries: the key carries the channel.
    #[test]
    fn both_channels_of_one_run_are_journalled_separately() {
        let db = Db::open_in_memory().expect("database");
        assert!(journal(&db, "run-2", "push", "ok", "").expect("push"));
        assert!(journal(&db, "run-2", "email", "failed", "no recipient").expect("email"));
        let entries = journal_for(&db, "run-2").expect("read");
        assert_eq!(entries.len(), 2);
    }
}
