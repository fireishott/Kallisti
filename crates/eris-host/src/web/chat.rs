//! The browser chat transcript and the turn that fills it.
//!
//! The device surface (`device::turns`) owns jobs, leases, resume and
//! idempotency, because a phone on a bad connection needs all four. A browser on
//! the dashboard needs one thing that surface does not: a transcript it can
//! render as a conversation, with the operator's own words kept beside the
//! answer. So this module owns `chat_messages`, and the provider call is the same
//! seam the device path uses, with the same persona and the same configured
//! endpoint. Nothing here reaches the device listener, and nothing here is
//! reachable without an admin session.
//!
//! Three rules that are enforced here rather than in a template:
//!
//! - **History is bounded.** The last [`HISTORY_MESSAGES`] settled messages are
//!   sent, oldest first, and a message that is still streaming or that failed is
//!   never sent as if it were an answer.
//! - **A message that is not settled is not history.** The assistant row exists
//!   before the first token arrives, so a page refresh mid-answer shows the
//!   partial text instead of a gap.
//! - **The transcript is the operator's.** It is never logged, and the audit row
//!   for a chat turn records a count and a size, never the words.

use rusqlite::{Connection, OptionalExtension, params};

use core_domain::{Capabilities, ModelProfile, ProviderProfile};
use core_runtime::provider::{
    ChatMessage, ChatRequest, ChatRole, OpenAiCompatProvider, ProviderEvent, SecretRef,
};

use crate::db::{Db, DbError};
use crate::onboarding::{
    OnboardingError, SETTING_MODEL, SETTING_PROVIDER_BASE_URL, SETTING_PROVIDER_KEY_REF,
    SetupEngine,
};

/// The id 1.0.1 through 1.0.3 used for the single transcript.
///
/// Production code resolves a conversation now, but the migration backfills this
/// id's row and the suites pin their fixtures to it, so it stays a named constant
/// rather than a magic string in eight tests.
pub const CONVERSATION_ID: &str = "main";

/// What a fresh conversation is called until its first message names it.
pub const NEW_CONVERSATION_TITLE: &str = "New chat";

/// How many conversations the rail lists. A rail is a shortlist, not an archive.
pub const SESSION_LIMIT: usize = 30;

/// The longest title the rail stores from a first message.
pub const TITLE_LIMIT: usize = 72;

/// One file a message carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    /// Row id.
    pub id: String,
    /// The name it arrived with, already stripped of any path.
    pub name: String,
    /// Where it was written, absolute.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// What the bytes actually are, sniffed when the file landed.
    ///
    /// Recorded for the transcript's type badge and for the vision role, both of
    /// which are display and dispatch decisions. The serving route sniffs again
    /// rather than believing this column, because a row is not the bytes.
    pub media_type: Option<String>,
}

/// The largest single attachment a message may carry.
pub const MAX_ATTACHMENT_BYTES: usize = 25 * 1024 * 1024;

/// The most files one message may carry.
///
/// A screenshot and a log and a JSON file and a PDF is a normal turn; fifty is a
/// mistake. The cap exists so a drop of a whole folder is a refusal with a
/// number in it rather than a turn that writes a directory into the workspace.
pub const MAX_ATTACHMENTS: usize = 8;

/// The most bytes one message may carry across every file.
///
/// Eight files at the single-file ceiling is 200 MB, which is a workspace write
/// nobody asked for. The total is the budget that actually matters.
pub const MAX_ATTACHMENT_TOTAL_BYTES: usize = 64 * 1024 * 1024;

/// How much of a text file is pasted into the prompt.
pub const INLINE_TEXT_BYTES: usize = 8 * 1024;

/// How much pasted text one turn may carry across every attachment.
pub const INLINE_TOTAL_BYTES: usize = 32 * 1024;

/// Extensions treated as text, so their content can ride in the prompt.
const TEXT_EXTENSIONS: [&str; 18] = [
    "txt", "md", "markdown", "json", "jsonl", "csv", "tsv", "log", "toml", "yaml", "yml", "ini",
    "cfg", "rs", "py", "sh", "sql", "html",
];

/// The media type these bytes actually are, decided by magic bytes.
///
/// A browser sends a `filename` and a `Content-Type`, and both are claims: a
/// `.png` full of HTML is a `.png` that gets rendered as a page in this origin,
/// and a model asked to look at a JPEG labelled `image/png` is a model looking at
/// the wrong decoder. So the type comes from here and never from the name. What
/// the sniffer does not recognise is `application/octet-stream`, which every
/// browser downloads rather than renders.
///
/// This is the same decision the wallpaper route makes in
/// [`crate::web::wallpaper::sniff_image`], widened to the formats a message can
/// carry: the four picture formats, PDF, and text.
#[must_use]
pub fn sniff_media_type(bytes: &[u8]) -> &'static str {
    const PNG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.starts_with(&PNG) {
        return "image/png";
    }
    if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        return "image/jpeg";
    }
    if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return "image/webp";
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return "image/gif";
    }
    // An iPhone hands the browser a HEIC unless the site asks otherwise, and the
    // screenshot that arrives as `IMG_0060.HEIC` is still a picture. ISO-BMFF
    // puts the brand four bytes after `ftyp`; the HEIF family shares the box.
    if bytes.len() > 12
        && &bytes[4..8] == b"ftyp"
        && matches!(
            &bytes[8..12],
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"mif1" | b"msf1"
        )
    {
        return "image/heic";
    }
    if bytes.starts_with(b"%PDF-") {
        return "application/pdf";
    }
    if is_probably_text(bytes) {
        return "text/plain";
    }
    "application/octet-stream"
}

/// Whether a bounded head looks like text rather than a binary format.
///
/// A NUL byte is the one thing text nearly never contains, and it is exactly what
/// a tar header, a compiled object and a database page start with. A UTF-8 error
/// that is only a sequence cut off at the end of the sample is not evidence of
/// binary: the read window ends mid-character, the file did not.
fn is_probably_text(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(INLINE_TEXT_BYTES)];
    if head.is_empty() || head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(error) => error.error_len().is_none(),
    }
}

/// Whether the file's content is worth pasting into the prompt.
#[must_use]
pub fn is_text(name: &str) -> bool {
    name.rsplit_once('.')
        .map(|(_, ext)| TEXT_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

/// The name a file is stored under: no path, no traversal, bounded.
///
/// A browser sends `filename` with whatever the operator chose, and a name is
/// the one place a path can sneak into a filesystem write.
#[must_use]
pub fn safe_file_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("file").trim();
    let cleaned: String = base
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | ' ') {
                character
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').trim().to_owned();
    let cleaned = if cleaned.is_empty() {
        "file".to_owned()
    } else {
        cleaned
    };
    if cleaned.len() > 120 {
        cleaned[cleaned.len() - 120..].to_owned()
    } else {
        cleaned
    }
}

/// Record an attachment row and return the id.
pub fn record_attachment(db: &Db, conversation: &str, file: &Attachment) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO chat_attachments
                 (id, conversation_id, name, path, size, media_type)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                file.id,
                conversation,
                file.name,
                file.path,
                file.size as i64,
                file.media_type
            ],
        )?;
        Ok(())
    })
}

/// Tie the files a message carried to the message itself.
///
/// The row is written when the file lands, which is before the message exists: a
/// turn that is refused must not leave a message behind, and a file that is
/// refused must not leave a row. Once the message has an id, the files that came
/// with it are claimed, which is what lets a reloaded transcript say what went
/// with the question.
///
/// Returns how many rows were claimed, so a caller can tell "the files were
/// recorded" from "the files were recorded and are now attached to this line".
pub fn claim_attachments(
    db: &Db,
    conversation: &str,
    message_id: &str,
    ids: &[String],
) -> Result<usize, DbError> {
    if ids.is_empty() {
        return Ok(0);
    }
    db.with_conn(|conn| {
        let mut claimed = 0usize;
        for id in ids {
            claimed += conn.execute(
                "UPDATE chat_attachments
                    SET message_id = ?3
                  WHERE id = ?1 AND conversation_id = ?2 AND message_id IS NULL",
                params![id, conversation, message_id],
            )?;
        }
        Ok(claimed)
    })
}

/// How long an unclaimed attachment row may sit before it is treated as litter.
///
/// A row is written while its own request is still being read, so a row with no
/// message is normal for the length of one send. An hour is far past any send
/// and far short of a row an operator would miss.
pub const ORPHAN_ATTACHMENT_GRACE_SECS: i64 = 3600;

/// Delete attachment rows no message ever claimed, and the bytes they point at.
///
/// A send that is refused after its first file lands used to leave both: rows in
/// `chat_attachments` with `message_id IS NULL`, and files under
/// `uploads/<conversation>/` that nothing could ever show. The rows are the
/// index of the litter, so they go first and the files follow; a file that is
/// already gone is not an error, because the point is the absence, not the
/// removal.
///
/// Returns how many rows went. Called at boot, where it is a sweep rather than a
/// rule inside a request: the request path no longer writes anything it might
/// have to take back.
pub fn sweep_orphan_attachments(db: &Db, older_than_secs: i64) -> Result<usize, DbError> {
    let stale: Vec<String> = db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT path
               FROM chat_attachments
              WHERE message_id IS NULL
                AND created_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1)",
        )?;
        let rows = statement.query_map(params![format!("-{older_than_secs} seconds")], |row| {
            row.get::<_, String>(0)
        })?;
        let mut paths: Vec<String> = Vec::new();
        for row in rows {
            paths.push(row?);
        }
        Ok(paths)
    })?;
    if stale.is_empty() {
        return Ok(0);
    }
    let removed = db.with_conn(|conn| {
        conn.execute(
            "DELETE FROM chat_attachments
              WHERE message_id IS NULL
                AND created_at <= strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?1)",
            params![format!("-{older_than_secs} seconds")],
        )
    })?;
    for path in stale {
        // A path in the table is where this daemon put the bytes. A file that has
        // already gone is the outcome wanted, so a failed removal is not an error.
        let _ = std::fs::remove_file(&path);
    }
    Ok(removed)
}

/// The files each message in a conversation carried, keyed by message id.
///
/// Read once per page rather than once per line: the transcript is one query
/// already, and a second query per message is how a page with forty turns
/// becomes forty round trips.
pub fn attachments_by_message(
    db: &Db,
    conversation: &str,
) -> Result<std::collections::BTreeMap<String, Vec<Attachment>>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT message_id, id, name, path, size, media_type
               FROM chat_attachments
              WHERE conversation_id = ?1 AND message_id IS NOT NULL
              ORDER BY created_at ASC",
        )?;
        let rows = statement.query_map(params![conversation], |row| {
            let message_id: String = row.get(0)?;
            let media_type: Option<String> = row.get(5)?;
            let size: i64 = row.get(4)?;
            Ok((
                message_id,
                Attachment {
                    id: row.get(1)?,
                    name: row.get(2)?,
                    path: row.get(3)?,
                    size: size.max(0) as u64,
                    media_type,
                },
            ))
        })?;
        let mut out: std::collections::BTreeMap<String, Vec<Attachment>> =
            std::collections::BTreeMap::new();
        for row in rows {
            let (message_id, attachment) = row?;
            out.entry(message_id).or_default().push(attachment);
        }
        Ok(out)
    })
}

/// One attachment row, by the id a transcript link carries.
///
/// The row is the authority on where the bytes are, so the serving route never
/// builds a path out of anything a browser sent: an id that is not in this table
/// is a 404, not a filesystem read.
pub fn attachment(db: &Db, id: &str) -> Result<Option<Attachment>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT id, name, path, size, media_type
               FROM chat_attachments
              WHERE id = ?1",
            params![id],
            |row| {
                let size: i64 = row.get(3)?;
                Ok(Attachment {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    path: row.get(2)?,
                    size: size.max(0) as u64,
                    media_type: row.get(4)?,
                })
            },
        )
        .optional()
    })
}

/// The hour of the day in the host's own timezone.
///
/// Read from SQLite rather than pulling a timezone crate in: the database already
/// knows what `localtime` means on this host, and the greeting should follow the
/// clock of the machine the operator is actually sitting at.
pub fn local_hour(db: &Db) -> Result<u32, DbError> {
    db.with_conn(|conn| {
        let hour: i64 = conn.query_row(
            "SELECT CAST(strftime('%H', 'now', 'localtime') AS INTEGER)",
            [],
            |row| row.get(0),
        )?;
        Ok(hour.clamp(0, 23) as u32)
    })
}

/// The prompt text that tells the model what came with the message.
///
/// Text files are pasted in full up to a budget, because a path the model cannot
/// read is not an attachment. Everything else is named and located: the turn
/// runs in the workspace, so a file it may open is a file it can work with.
#[must_use]
pub fn describe_attachments(files: &[Attachment]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let mut out = String::from("\n\nThe operator attached these files to this message:\n");
    let mut budget = INLINE_TOTAL_BYTES;
    for file in files {
        out.push_str(&format!(
            "- {} ({} bytes) at {}\n",
            file.name, file.size, file.path
        ));
        if budget == 0 || !is_text(&file.name) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&file.path) else {
            out.push_str("  (the file could not be read)\n");
            continue;
        };
        let take = bytes.len().min(INLINE_TEXT_BYTES).min(budget);
        let slice = &bytes[..take];
        let text = String::from_utf8_lossy(slice);
        let truncated = take < bytes.len();
        out.push_str(&format!(
            "  contents{}:\n",
            if truncated { ", truncated" } else { "" }
        ));
        out.push_str("  ```\n");
        for line in text.lines() {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("  ```\n");
        budget -= take;
    }
    out
}

/// One conversation the rail can list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    /// Opaque id, also the transcript key.
    pub id: String,
    /// What the operator reads.
    pub title: String,
    /// The database stamp of the last message, for ordering and for the rail.
    pub updated_at: String,
    /// When the conversation was started, for the `created` sort.
    pub created_at: String,
    /// The project this conversation is filed under, when it is filed.
    pub project_id: Option<String>,
    /// The model this conversation last ran on, when one was recorded.
    ///
    /// `provider/model`, exactly as the composer's picker spells it. `None` is a
    /// conversation that predates the column or one that never sent a turn, and
    /// the page reads it as "use the configured default" - which is what every
    /// conversation did before this existed.
    pub model: Option<String>,
}

/// The `settings` row that remembers the rail's sort order.
///
/// Defined here rather than in the device handlers because the rail is the
/// surface that owns the preference: the settings page writes it, the rail reads
/// it, and the device API honours it.
pub const SETTING_CONVERSATION_SORT: &str = "display.conversation_sort";

/// How the rail lists conversations.
///
/// One enum for the web rail and the device API, so the two can never disagree
/// about what "alphabetical" means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationSort {
    /// Recently used. Today's order, and the default.
    Updated,
    /// Newest conversation first.
    Created,
    /// Alphabetical by title, case-insensitively.
    Title,
    /// Grouped by calendar day on the operator's clock, newest day first.
    Day,
}

impl ConversationSort {
    /// Every value a form or a query string may submit, in display order.
    pub const ALL: [Self; 4] = [Self::Updated, Self::Created, Self::Title, Self::Day];

    /// What an unwritten or unknown value means.
    pub const DEFAULT: Self = Self::Updated;

    /// Parse a stored or submitted value, falling back to [`Self::Updated`].
    ///
    /// An unknown value means the rail somebody already knows rather than an
    /// empty one: a bad query string must not look like a lost archive.
    #[must_use]
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some("created") => Self::Created,
            Some("title") => Self::Title,
            Some("day") => Self::Day,
            _ => Self::Updated,
        }
    }

    /// The stored spelling, and the query value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Updated => "updated",
            Self::Created => "created",
            Self::Title => "title",
            Self::Day => "day",
        }
    }

    /// The label a person reads.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Updated => "Recently used",
            Self::Created => "Newest",
            Self::Title => "Title",
            Self::Day => "By day",
        }
    }

    /// The `ORDER BY` this mode reads with.
    ///
    /// A closed set of constants rather than a string built from input: the mode
    /// is an enum, so there is no path from a query string into the SQL.
    fn order_by(self) -> &'static str {
        match self {
            Self::Updated => "updated_at DESC, rowid DESC",
            Self::Created => "created_at DESC, rowid DESC",
            Self::Title => "title COLLATE NOCASE ASC, rowid ASC",
            Self::Day => "updated_at DESC, rowid DESC",
        }
    }
}

/// The newest conversations, newest first.
///
/// The mode every existing caller reads with, kept as its own function so a
/// caller that has no opinion about sorting does not have to pass one.
pub fn conversations(db: &Db, limit: usize) -> Result<Vec<Conversation>, DbError> {
    conversations_sorted(db, limit, ConversationSort::DEFAULT)
}

/// The conversations, in the operator's chosen order.
pub fn conversations_sorted(
    db: &Db,
    limit: usize,
    sort: ConversationSort,
) -> Result<Vec<Conversation>, DbError> {
    // The order clause is a constant chosen by a match on the enum, never
    // interpolated from input.
    let sql = format!(
        "SELECT id, title, updated_at, created_at, project_id, model
           FROM chat_conversations
          ORDER BY {}
          LIMIT ?1",
        sort.order_by()
    );
    db.with_conn(|conn| {
        let mut statement = conn.prepare(&sql)?;
        let rows = statement.query_map(params![limit as i64], |row| {
            Ok(Conversation {
                id: row.get(0)?,
                title: row.get(1)?,
                updated_at: row.get(2)?,
                created_at: row.get(3)?,
                project_id: row.get(4)?,
                model: row.get(5)?,
            })
        })?;
        rows.collect()
    })
}

/// One day of a `day`-sorted list, as the rail's heading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationDay {
    /// `YYYY-MM-DD` on the operator's clock, for a stable key.
    pub key: String,
    /// What the heading reads: `Today`, `Yesterday`, or `Mar 14`.
    pub label: String,
    /// The conversations in this day, in `updated` order.
    pub conversations: Vec<Conversation>,
}

/// The calendar day a stored UTC stamp falls on, on the operator's clock.
///
/// `(year, month, day)`, so the grouping compares days rather than strings, and
/// so a conversation at 02:00Z is the previous evening for an operator at -04:00.
fn local_day(stamp: &str, offset_minutes: i64) -> Option<(i64, i64, i64)> {
    let date = stamp.split_once('T')?.0;
    let time = stamp.split_once('T')?.1;
    if time.len() < 5 {
        return None;
    }
    let year = date[0..4].parse::<i64>().ok()?;
    let month = date[5..7].parse::<i64>().ok()?;
    let day = date[8..10].parse::<i64>().ok()?;
    let hour = time[0..2].parse::<i64>().ok()?;
    let minute = time[3..5].parse::<i64>().ok()?;
    let shifted = hour * 60 + minute + offset_minutes;
    // The shift can move a stamp into the day before or after, so the day is
    // adjusted by the whole days the shift crossed rather than left alone.
    let day_delta = shifted.div_euclid(24 * 60);
    Some(shift_day(year, month, day, day_delta))
}

/// Move a calendar day by whole days, in the proleptic Gregorian calendar.
///
/// Deliberately small: the only callers are a one-day step either side of a
/// stamp, so a full date library would be a dependency for six lines of
/// arithmetic.
fn shift_day(year: i64, month: i64, day: i64, delta: i64) -> (i64, i64, i64) {
    if delta == 0 {
        return (year, month, day);
    }
    let (mut year, mut month, mut day) = (year, month, day + delta);
    let days_in_month = |year: i64, month: i64| -> i64 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
            2 => 28,
            _ => 30,
        }
    };
    while day < 1 {
        month -= 1;
        if month < 1 {
            month = 12;
            year -= 1;
        }
        day += days_in_month(year, month);
    }
    while day > days_in_month(year, month) {
        day -= days_in_month(year, month);
        month += 1;
        if month > 12 {
            month = 1;
            year += 1;
        }
    }
    (year, month, day)
}

/// The heading for a day, given today's day on the same clock.
///
/// `Today`, `Yesterday`, then the short English month and the day number. The
/// month names are a fixed list rather than a locale lookup: the product is one
/// language, and a heading is not the place to discover otherwise.
#[must_use]
pub fn day_label(day: (i64, i64, i64), today: (i64, i64, i64)) -> String {
    if day == today {
        return "Today".to_owned();
    }
    if day == shift_day(today.0, today.1, today.2, -1) {
        return "Yesterday".to_owned();
    }
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let month = MONTHS
        .get((day.1 - 1).clamp(0, 11) as usize)
        .copied()
        .unwrap_or("???");
    format!("{month} {}", day.2)
}

/// Group a `day`-sorted list into its days, newest day first.
///
/// The grouping happens here rather than in SQL because the day boundary is the
/// operator's, and SQLite does not know their zone. The input is expected in
/// `updated` order, which is the order each day's rows keep.
#[must_use]
pub fn group_by_day(
    rows: Vec<Conversation>,
    offset_minutes: i64,
    today: (i64, i64, i64),
) -> Vec<ConversationDay> {
    let mut days: Vec<ConversationDay> = Vec::new();
    for row in rows {
        let Some(day) = local_day(&row.updated_at, offset_minutes) else {
            // A row with a stamp this host cannot read still belongs somewhere,
            // so it goes in its own group under the raw stamp rather than being
            // dropped from a list the operator is reading.
            days.push(ConversationDay {
                key: row.updated_at.clone(),
                label: row.updated_at.clone(),
                conversations: vec![row],
            });
            continue;
        };
        let key = format!("{:04}-{:02}-{:02}", day.0, day.1, day.2);
        match days.last_mut() {
            Some(group) if group.key == key => group.conversations.push(row),
            _ => days.push(ConversationDay {
                key,
                label: day_label(day, today),
                conversations: vec![row],
            }),
        }
    }
    days
}

/// Today's calendar day on the operator's clock, for [`day_label`].
#[must_use]
pub fn today_on(offset_minutes: i64) -> (i64, i64, i64) {
    let now = time::OffsetDateTime::now_utc();
    shift_day(
        i64::from(now.year()),
        u32::from(now.month() as u8) as i64,
        i64::from(now.day()),
        (i64::from(now.hour()) * 60 + i64::from(now.minute()) + offset_minutes).div_euclid(24 * 60),
    )
}

/// Whether a conversation id is one the daemon knows.
pub fn conversation_exists(db: &Db, id: &str) -> Result<bool, DbError> {
    db.with_conn(|conn| {
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM chat_conversations WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    })
}

/// One conversation, when it exists.
///
/// The single-row read a route needs before it answers a transcript or clears
/// one: whether the row is there decides between an answer and a missing id, and
/// the title on it is what the client shows above the messages.
pub fn conversation(db: &Db, id: &str) -> Result<Option<Conversation>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT id, title, updated_at, created_at, project_id, model
               FROM chat_conversations WHERE id = ?1",
            params![id],
            |row| {
                Ok(Conversation {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    updated_at: row.get(2)?,
                    created_at: row.get(3)?,
                    project_id: row.get(4)?,
                    model: row.get(5)?,
                })
            },
        )
        .optional()
    })
}

/// How many messages each conversation holds, keyed by conversation id.
///
/// One grouped query rather than one per row: a list of a hundred conversations
/// must not cost a hundred reads of the same file, and a conversation with no
/// messages simply has no entry.
pub fn message_counts(db: &Db) -> Result<std::collections::BTreeMap<String, usize>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT conversation_id, COUNT(*) FROM chat_messages GROUP BY conversation_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        let mut counts = std::collections::BTreeMap::new();
        for row in rows {
            let (id, count) = row?;
            counts.insert(id, usize::try_from(count).unwrap_or(usize::MAX));
        }
        Ok(counts)
    })
}

/// Start a conversation and return its id.
pub fn create_conversation(db: &Db) -> Result<String, DbError> {
    let id = uuid::Uuid::now_v7().to_string();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO chat_conversations (id, title) VALUES (?1, ?2)",
            params![id, NEW_CONVERSATION_TITLE],
        )?;
        Ok(())
    })?;
    Ok(id)
}

/// Make sure a conversation row exists for this id.
///
/// Used by the migration's backfill path and by the suites, which pin their
/// fixtures to [`CONVERSATION_ID`] and then expect a page to find them.
pub fn seed_conversation(db: &Db, id: &str) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "INSERT OR IGNORE INTO chat_conversations (id, title) VALUES (?1, ?2)",
            params![id, NEW_CONVERSATION_TITLE],
        )?;
        Ok(())
    })
}

/// The newest conversation's id, when there is one.
pub fn latest_conversation(db: &Db) -> Result<Option<String>, DbError> {
    Ok(conversations(db, 1)?.into_iter().next().map(|row| row.id))
}

/// The conversation a page should show: the requested one when it exists, the
/// newest one otherwise, and a fresh one when the daemon has none at all.
///
/// A page never renders an empty rail because of a missing row: the answer always
/// names a conversation that exists.
pub fn resolve_conversation(db: &Db, requested: Option<&str>) -> Result<String, DbError> {
    if let Some(id) = requested.filter(|id| !id.is_empty())
        && conversation_exists(db, id)?
    {
        return Ok(id.to_owned());
    }
    match latest_conversation(db)? {
        Some(id) => Ok(id),
        None => create_conversation(db),
    }
}

/// Delete a conversation and its transcript. Returns how many messages went.
pub fn delete_conversation(db: &Db, id: &str) -> Result<usize, DbError> {
    db.with_conn(|conn| {
        let removed = conn.execute(
            "DELETE FROM chat_messages WHERE conversation_id = ?1",
            params![id],
        )?;
        conn.execute("DELETE FROM chat_conversations WHERE id = ?1", params![id])?;
        Ok(removed)
    })
}

/// Bump a conversation so the rail keeps the one in use at the top.
pub fn touch_conversation(db: &Db, id: &str) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_conversations
                SET updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
              WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    })
}

/// Record the model a conversation is now running on.
///
/// Written on every send, so the picker restores what the last turn actually
/// used rather than what the settings page says today. That is what keeps a
/// conversation on one model's prompt cache: a resumed session that quietly
/// moved to the default would re-read its whole history on a cache that does not
/// have it, and hand a model a context it never agreed to.
///
/// An empty value clears the row back to "never recorded", which reads as the
/// configured default.
pub fn set_conversation_model(db: &Db, id: &str, model: &str) -> Result<(), DbError> {
    let model = model.trim();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_conversations SET model = ?2 WHERE id = ?1",
            params![
                id,
                if model.is_empty() {
                    None
                } else {
                    Some(model.to_owned())
                }
            ],
        )?;
        Ok(())
    })
}

/// Name a conversation from its first question, when it still has the default
/// name. Never renames one the operator has already seen named.
pub fn name_from_first_message(db: &Db, id: &str, text: &str) -> Result<(), DbError> {
    let title = title_from(text);
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_conversations
                SET title = ?2
              WHERE id = ?1 AND title = ?3",
            params![id, title, NEW_CONVERSATION_TITLE],
        )?;
        Ok(())
    })
}

/// The configuration an auxiliary role resolves to, when the operator pinned it.
///
/// `None` means the role is left on the primary model, and a role on the primary
/// is a no-op by design: paying a second request to re-derive what the primary
/// already produced would be a cost with no answer behind it. Pinning a model is
/// the switch that turns a role on, and the settings page says so on the row.
///
/// # Errors
///
/// A settings read failure.
pub fn aux_pinned_config(
    engine: &SetupEngine,
    role: &str,
) -> Result<Option<TurnConfig>, OnboardingError> {
    let stored_model = engine.setting(&crate::aux::setting_key(role))?;
    if crate::aux::inherits(stored_model.as_deref()) {
        return Ok(None);
    }

    let registry = provider_registry(engine)?;
    let primary = engine
        .setting(crate::providers::SETTING_PRIMARY)?
        .unwrap_or_else(|| crate::providers::DEFAULT_ID.to_owned());
    let stored_provider = engine.setting(&crate::aux::provider_key(role))?;
    let Some(provider) = crate::aux::provider_for(stored_provider.as_deref(), &registry, &primary)
    else {
        return Ok(None);
    };
    let Ok(key_ref) = SecretRef::try_from(provider.key_ref.as_str()) else {
        return Ok(None);
    };
    let primary_model = engine.setting(SETTING_MODEL)?.unwrap_or_default();
    // An unpinned role follows the chat model, which carries its own provider in
    // front of it. Reading the bare half here rather than the whole value is
    // what keeps a role on the endpoint its own provider row names.
    let inherited = crate::web::chat::split_model_value(&primary_model, &registry, &primary).model;
    let model = crate::aux::effective(stored_model.as_deref(), &inherited);
    let dialect = engine.provider_dialect_for(&provider.id, &model)?;
    Ok(Some(TurnConfig {
        base_url: provider.base_url.clone(),
        key_ref,
        model,
        dialect,
        reasoning_split: crate::providers::reasoning_split_of(provider),
        direct: crate::providers::direct_of(provider),
        project: crate::providers::project_of(provider),
    }))
}

/// The first thing the operator said in a conversation, when there is one.
///
/// This is what the conversation was named from, so it is the only question whose
/// truncation can be a name the daemon itself wrote. Reading it here rather than
/// comparing against the current turn's own truncation is the whole fix: from the
/// second turn on the stored title is the *first* question's truncation, and a
/// condition that only knew about the latest question matched nothing, generated
/// a title every turn, and threw every one of them away.
pub fn first_question(db: &Db, id: &str) -> Result<Option<String>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT body FROM chat_messages
              WHERE conversation_id = ?1 AND role = 'user'
              ORDER BY created_at, rowid
              LIMIT 1",
            params![id],
            |row| row.get::<_, String>(0),
        )
        .optional()
    })
}

/// Replace a conversation title a model suggested.
///
/// Three titles are fair game: the placeholder, the truncation of the question
/// this turn asked, and the truncation of the conversation's own first question.
/// Truncations are written by the daemon before a turn runs, so a condition that
/// knew only about the placeholder would mean a suggestion never landed.
///
/// The first-question candidate is what lets a later turn rename the
/// conversation: by then the stored title is the first question's truncation, and
/// the current turn's question has nothing to do with what is on the row.
///
/// Anything else is the operator's own words - or a title a previous model
/// already wrote - and is left alone. That second case is deliberate: a
/// conversation that has a real name keeps it, so a three-word title from turn one
/// is not replaced by a different three-word title on turn nine.
pub fn set_title(db: &Db, id: &str, question: &str, title: &str) -> Result<usize, DbError> {
    let title = scrub_title(title);
    if title.is_empty() {
        return Ok(0);
    }
    let truncated = title_from(question);
    // A read before the write, because both candidates are computed values and
    // the placeholder is a constant: three of them do not fit one `IN` without
    // asking which ones apply. The window between the two is the operator typing
    // a rename, which the write's own condition still catches.
    let first = first_question(db, id)?.map(|text| title_from(&text));
    let rows = db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_conversations
                SET title = ?2
              WHERE id = ?1 AND (title = ?3 OR title = ?4 OR title = ?5)",
            params![
                id,
                title,
                NEW_CONVERSATION_TITLE,
                truncated,
                first.unwrap_or_default()
            ],
        )
    })?;
    Ok(rows)
}

/// A title from the first question: one line, trimmed, bounded.
#[must_use]
pub fn title_from(text: &str) -> String {
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= TITLE_LIMIT {
        return collapsed;
    }
    let mut cut: String = collapsed.chars().take(TITLE_LIMIT).collect();
    cut.push('…');
    cut
}

/// Ask the resolved role for a title.
///
/// Best effort by design: a title is a nicety, so a provider that is down or a
/// reply that is unusable leaves the truncated title in place rather than
/// failing a turn that already answered.
#[must_use]
pub fn suggest_title(config: &TurnConfig, question: &str, answer: &str) -> Option<String> {
    let profile = config.profile("title-generation").ok()?;
    let provider = OpenAiCompatProvider::new(
        profile,
        ModelProfile::new(config.model.clone()),
        Capabilities::openai_compatible(),
    )
    .ok()?;
    let key = crate::oauth::credential::resolve_key_now(&config.key_ref).ok()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;

    // The question decides the name, not the answer.
    //
    // A title names the subject of the conversation, and the subject is what the
    // operator asked about. Sending an answer excerpt beside it let a long reply
    // outvote the question: the model named whatever the last paragraph was
    // about, so a thread that began "why is the trigger failing" came back as
    // "Rowid Collation Settings". The answer is still sent, because two lines
    // can disambiguate a one-word question, but it is trimmed hard and the
    // instruction says which one wins.
    let excerpt = |text: &str, cap: usize| -> String { text.chars().take(cap).collect() };
    let question = strip_think(question);
    let answer = strip_think(answer);
    let request = ChatRequest {
        system: Some(
            "You name a conversation after its subject. Reply with only a title of at \
             most six words. No quotes, no trailing punctuation, no explanation, and no \
             thinking: if you reason at all, close it before the title."
                .to_owned(),
        ),
        messages: vec![ChatMessage::text(
            ChatRole::User,
            format!(
                "The subject of this conversation, taken from what the operator asked. \
                 Name it, not the answer:\n{}\n\nThe reply, for context only, not the \
                 subject:\n{}",
                excerpt(&question, 600),
                excerpt(&answer, 240)
            ),
        )],
        // No cap: a short cap is spent on a thinking model's reasoning before it
        // ever writes the title, which produced an empty answer and a rename that
        // silently did nothing.
        ..ChatRequest::default()
    };

    let mut sink = |_event: ProviderEvent| {};
    let completion = runtime
        .block_on(provider.stream_chat(&request, &key, &mut sink))
        .ok()?;
    let title = scrub_title(&completion.text);
    (!title.is_empty()).then_some(title)
}

/// Drop a chain of thought that a provider inlined into its content.
///
/// Not every endpoint separates reasoning from the answer: MiniMax inlines it
/// unless the request asks otherwise, and a gateway in front of a thinking model
/// passes the tags straight through. The tags are markup, never words the
/// operator asked for, so the block between them is removed here rather than
/// stored as a title.
///
/// An opening tag with no closing one is a truncated thought, and it runs to the
/// end of the text: half a chain of thought is still not a title. A closing tag
/// with no opening one - a stream that began mid-thought - drops everything in
/// front of it, which is the rest of that same truncated thought.
#[must_use]
pub fn strip_think(raw: &str) -> String {
    const CLOSES: [&str; 2] = ["</think>", "</thinking>"];
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(open) = rest.find("<think").or_else(|| rest.find("<thinking")) {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        // The end of the tag itself, so `<thinking>` does not leave `ing>` behind.
        let Some(tag_end) = after.find('>') else {
            // A `<think` with no `>` is a truncated tag, so nothing after it is
            // trustworthy text either.
            return out;
        };
        let body = &after[tag_end + 1..];
        match CLOSES
            .iter()
            .filter_map(|close| body.find(close).map(|at| (at, close.len())))
            .min()
        {
            Some((at, len)) => rest = &body[at + len..],
            // Unclosed: the thought never ended, so the answer never started.
            None => return out,
        }
    }
    out.push_str(rest);
    for close in CLOSES {
        if let Some(at) = out.find(close) {
            out = out[at + close.len()..].to_owned();
        }
    }
    out
}

/// Make a model's suggestion usable as a title.
///
/// Models like to answer with a quoted line, a trailing period, two lines, or -
/// on an endpoint that inlines its reasoning - the whole chain of thought before
/// the title. A title is one line of plain words, so a think block is dropped
/// first, then quotes and trailing punctuation, and the result is bounded.
///
/// A suggestion that is empty after that is refused rather than stored: a
/// conversation named `<think>` is worse than a conversation named after its
/// first question.
#[must_use]
pub fn scrub_title(raw: &str) -> String {
    let cleaned = strip_think(raw);
    let line = cleaned
        .lines()
        .map(str::trim)
        // A line that still opens with markup is a tag this build does not know,
        // not a name: it is skipped rather than stored.
        .find(|line| !line.is_empty() && !line.starts_with('<'))
        .unwrap_or_default();
    // Punctuation first, then quotes: a model that writes `"A title".` puts the
    // stop outside the quote, so trimming quotes first would leave the `"` behind.
    let without_stop = line.trim().trim_end_matches(['.', '!', '?', ':']);
    let unquoted = without_stop
        .trim_matches(|c| c == '"' || c == '\'' || c == '`')
        .trim();
    let collapsed: String = unquoted.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= TITLE_LIMIT {
        return collapsed;
    }
    let mut cut: String = collapsed.chars().take(TITLE_LIMIT).collect();
    cut.push('…');
    cut
}

/// The largest single message a browser may send, in bytes.
///
/// 16 KiB was the first number and it was too small for the work people actually
/// paste: a stack trace, a config file, a log excerpt. 128 KiB fits a long paste
/// and is still a paste rather than an upload, which is what the attachment path
/// is for. The composer states the same number, so a refusal is never a surprise.
pub const MAX_MESSAGE_BYTES: usize = 128 * 1024;

/// How many settled messages of context a turn carries.
pub const HISTORY_MESSAGES: usize = 24;

/// Who spoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The operator.
    User,
    /// The daemon.
    Assistant,
}

impl Role {
    /// The stored and wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }

    /// Parse a stored value. The database constrains this already; a value
    /// outside the set is a corruption, not a new role.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            _ => None,
        }
    }
}

/// How far along one message is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// The whole body is there.
    Complete,
    /// The body is still arriving.
    Streaming,
    /// The turn was refused; `error_code` says why.
    Failed,
}

impl State {
    /// The stored spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Streaming => "streaming",
            Self::Failed => "failed",
        }
    }

    /// Parse a stored value.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "complete" => Some(Self::Complete),
            "streaming" => Some(Self::Streaming),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// One tool call a turn ran, as the transcript keeps it.
///
/// `arguments` is kept as the model sent it, and `output` is what the tool
/// returned. Both are shown to the operator, so a command that failed is visible
/// as a failure rather than as an answer that quietly ignored it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolRecord {
    /// The tool name.
    pub name: String,
    /// The arguments, as JSON.
    pub arguments: String,
    /// What the tool returned, already truncated by the tool.
    pub output: String,
    /// Whether the tool reported a failure.
    pub is_error: bool,
    /// What a `clarify` call asked and what the answer was, when the tool was
    /// `clarify`.
    ///
    /// Kept beside the arguments rather than inside them because the arguments are
    /// what the model wrote and this is what the operator saw and said. A page
    /// redrawing the card after a reload reads the question from here, and its
    /// answer is the same value the model was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked: Option<Asked>,
}

/// One parked question and its answer, as the transcript keeps them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Asked {
    /// What the model asked.
    pub request: crate::web::clarify::Request,
    /// What the operator decided, once they did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<crate::web::clarify::Answer>,
}

/// One transcript row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Row id.
    pub id: String,
    /// Who spoke.
    pub role: Role,
    /// What was said, so far.
    pub body: String,
    /// Whether the body is settled.
    pub state: State,
    /// The failure's code, when `state` is [`State::Failed`].
    pub error_code: Option<String>,
    /// When the row was written, as the database stamps it.
    pub created_at: String,
    /// The model that answered, when this is an answer.
    pub model: Option<String>,
    /// Prompt tokens the provider reported.
    pub prompt_tokens: Option<u64>,
    /// Completion tokens the provider reported.
    pub completion_tokens: Option<u64>,
    /// Prompt tokens the provider reported as served from its cache.
    pub cached_tokens: Option<u64>,
    /// The prompt size of the turn's last round trip, which is how full the
    /// window actually was.
    ///
    /// Deliberately not `prompt_tokens`: that is a sum over every round, so a turn
    /// that ran a hundred tool calls reported the sum as its context size and the
    /// footer printed shares above 100%. `None` on rows written before this
    /// existed, where the footer says so rather than guessing.
    pub context_tokens: Option<u64>,
    /// The context window declared for this model at the time of the turn.
    pub context_window: Option<u64>,
    /// Wall time from request to last byte.
    pub duration_ms: Option<u64>,
    /// The model's own reasoning, when the provider streamed any.
    ///
    /// Never the answer, never sent back into a later prompt, and never shown
    /// unless the operator leaves the thinking bubble switched on.
    pub reasoning: Option<String>,
    /// The tool calls this turn ran, in order. Empty when it ran none.
    pub tools: Vec<ToolRecord>,
    /// The id the client that sent this row minted for it, when it sent one.
    ///
    /// A device holds its own id for the message the moment the operator hits
    /// send, which is before the daemon has a row to point at. Echoing that id
    /// back on the row it wrote is what lets the device recognise its own
    /// optimistic row in a later refresh instead of drawing a second copy of
    /// the same sentence. `None` for every row written before 0019 and for
    /// every browser turn, which mints no such id.
    pub client_message_id: Option<String>,
    /// The turn this row belongs to.
    ///
    /// Stamped on both rows of a turn once the job exists, so a client can
    /// group the question, the answer and the reasoning that belongs to them
    /// without guessing from timestamps. `None` on rows written before 0019.
    pub job_id: Option<String>,
    /// How many tool rounds the turn ran, when it recorded that.
    pub rounds: Option<u64>,
    /// Whether the tool budget, rather than the model, ended this reply.
    ///
    /// Recorded, not inferred. The turn loop always knew; only the live page was
    /// ever told, so the fact died with the page. `None` on a row written before
    /// migration 0022, which is why it is not a `bool`: a page must be able to
    /// say nothing rather than print a confident "not capped".
    pub capped: Option<bool>,
    /// Whether the engine stopped the turn early on purpose so the model could
    /// write the answer with what it already had.
    ///
    /// A separate column from `capped` because they are separate facts, and
    /// `None` on a row written before migration 0023 for the same reason: a page
    /// must be able to say nothing rather than print a confident "did not wrap
    /// up".
    pub wrapped_up: Option<bool>,
    /// The gateway's limit state for the answering model, read at settle.
    pub quota: Option<crate::web::quota::Quota>,
}

/// A count from SQLite, which stores signed integers and holds no negative ones.
fn tokens(value: Option<i64>) -> Option<u64> {
    value.filter(|count| *count >= 0).map(|count| count as u64)
}

/// Read a whole conversation, oldest first.
pub fn load(db: &Db, conversation: &str) -> Result<Vec<Message>, DbError> {
    db.with_conn(|conn| collect(conn, conversation, None))
}

/// Read at most `limit` messages of a conversation, oldest first, and say whether
/// anything was left behind.
///
/// A transcript has no ceiling: a conversation used for a year would otherwise be
/// answered in one body. The bound is the page rather than the row, so a caller
/// that needs more asks again, and `true` is what tells it there is more to ask
/// for instead of leaving it to believe it has read the whole thing.
pub fn load_bounded(
    db: &Db,
    conversation: &str,
    limit: usize,
) -> Result<(Vec<Message>, bool), DbError> {
    let total: i64 = db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM chat_messages WHERE conversation_id = ?1",
            params![conversation],
            |row| row.get(0),
        )
    })?;
    let total = usize::try_from(total).unwrap_or(usize::MAX);
    let messages = db.with_conn(|conn| collect(conn, conversation, Some(limit)))?;
    let truncated = total > messages.len();
    Ok((messages, truncated))
}

/// What one transcript read is allowed to cost on the wire, in bytes.
///
/// A message count is not a size bound. One measured conversation on this daemon
/// held 41 messages and 9.2 MB of JSON, because a turn that runs shell commands
/// stores every command's whole stdout as a tool record. A phone that asked for
/// that transcript and then moved on could not finish the read in the time the
/// surface allowed, so the chat opened blank - intermittently, and only for the
/// conversations that had done real work.
pub const TRANSCRIPT_BYTE_BUDGET: usize = 512 * 1024;

/// How much of one tool's output a transcript read carries.
///
/// The full record stays in the database; this is the preview a reader is given,
/// and it says how much it left behind, so an answer that came from a 140 KB
/// stdout is not presented as one that came from all of it.
pub const TOOL_OUTPUT_PREVIEW: usize = 8 * 1024;

/// A transcript cut to something a phone can read.
pub struct BoundedTranscript {
    /// The messages that survived, oldest first.
    pub messages: Vec<Message>,
    /// How many of the oldest messages were dropped to fit the budget.
    pub omitted: usize,
    /// How many tool outputs were cut down to [`TOOL_OUTPUT_PREVIEW`].
    pub elided_outputs: usize,
}

/// Cut a transcript to a byte budget and a per-tool preview.
///
/// Two bounds rather than one, because they answer different questions. The
/// preview stops a single command owning the whole read; the budget bounds the
/// read however many commands a turn ran. The newest messages are the ones kept:
/// a transcript that has to drop something drops the part nobody is scrolling
/// back to, and `omitted` says how much went.
#[must_use]
pub fn bound_transcript(
    messages: Vec<Message>,
    budget: usize,
    preview: usize,
) -> BoundedTranscript {
    let mut elided_outputs = 0;
    let mut kept: Vec<Message> = messages;
    for message in &mut kept {
        for tool in &mut message.tools {
            if tool.output.len() > preview {
                let cut = floor_char_boundary(&tool.output, preview);
                let left = tool.output.len() - cut;
                tool.output.truncate(cut);
                tool.output
                    .push_str(&format!("\n... {left} bytes of output not shown"));
                elided_outputs += 1;
            }
        }
    }
    // Walk from the newest backwards, so the budget is spent on the messages a
    // reader is actually looking at.
    let mut total = 0usize;
    let mut keep_from = 0usize;
    for (index, message) in kept.iter().enumerate().rev() {
        let cost = wire_cost(message);
        // Always keep the newest message, however large it is: a transcript with
        // nothing in it is not a bounded read, it is an empty one.
        if total + cost > budget && index + 1 != kept.len() {
            keep_from = index + 1;
            break;
        }
        total += cost;
    }
    if keep_from > 0 {
        kept.drain(..keep_from);
    }
    BoundedTranscript {
        messages: kept,
        omitted: keep_from,
        elided_outputs,
    }
}

/// What one message costs once it is JSON on the wire, near enough to bound it by.
fn wire_cost(message: &Message) -> usize {
    let tools: usize = message
        .tools
        .iter()
        .map(|tool| tool.name.len() + tool.arguments.len() + tool.output.len())
        .sum();
    256 + message.body.len() + message.reasoning.as_ref().map_or(0, String::len) + tools
}

/// The largest index at or below `max` that is a UTF-8 char boundary.
fn floor_char_boundary(text: &str, max: usize) -> usize {
    if max >= text.len() {
        return text.len();
    }
    let mut index = max;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn collect(
    conn: &mut Connection,
    conversation: &str,
    limit: Option<usize>,
) -> Result<Vec<Message>, rusqlite::Error> {
    const SELECT: &str = "SELECT id, role, body, state, error_code, created_at, model,
                prompt_tokens, completion_tokens, cached_tokens, context_window,
                duration_ms, reasoning, tools, rounds, quota_json, context_tokens,
                client_message_id, job_id, capped, wrapped_up
           FROM chat_messages
          WHERE conversation_id = ?1
          ORDER BY created_at ASC, rowid ASC";
    // Two statements rather than one with a sentinel limit: SQLite's "negative
    // means no limit" is a behaviour to lean on only when there is no clearer way
    // to say the same thing.
    match limit {
        Some(limit) => {
            let mut statement = conn.prepare(&format!("{SELECT} LIMIT ?2"))?;
            let rows = statement.query_map(params![conversation, limit as i64], row_message)?;
            rows.collect()
        }
        None => {
            let mut statement = conn.prepare(SELECT)?;
            let rows = statement.query_map(params![conversation], row_message)?;
            rows.collect()
        }
    }
}

/// One transcript row, as the reader wants it.
fn row_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<Message> {
    let role: String = row.get(1)?;
    let state: String = row.get(3)?;
    Ok(Message {
        id: row.get(0)?,
        role: Role::parse(&role).unwrap_or(Role::Assistant),
        body: row.get(2)?,
        state: State::parse(&state).unwrap_or(State::Failed),
        error_code: row.get(4)?,
        created_at: row.get(5)?,
        model: row.get(6)?,
        prompt_tokens: tokens(row.get(7)?),
        completion_tokens: tokens(row.get(8)?),
        cached_tokens: tokens(row.get(9)?),
        context_window: tokens(row.get(10)?),
        duration_ms: tokens(row.get(11)?),
        reasoning: row.get(12)?,
        tools: row
            .get::<_, Option<String>>(13)?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default(),
        rounds: tokens(row.get(14)?),
        quota: row
            .get::<_, Option<String>>(15)?
            .and_then(|raw| serde_json::from_str(&raw).ok()),
        context_tokens: tokens(row.get(16)?),
        client_message_id: row.get(17)?,
        job_id: row.get(18)?,
        capped: row.get::<_, Option<i64>>(19)?.map(|flag| flag != 0),
        wrapped_up: row.get::<_, Option<i64>>(20)?.map(|flag| flag != 0),
    })
}

/// Append one message. The id is minted here and returned so the caller can
/// update the same row when the answer lands.
///
/// This is the browser's shape: the browser holds no id of its own for a
/// message, so both handles are `None` and the row is identified by the id
/// returned here alone.
pub fn append(
    db: &Db,
    conversation: &str,
    role: Role,
    body: &str,
    state: State,
) -> Result<String, DbError> {
    append_identified(db, conversation, role, body, state, None)
}

/// Append one message, optionally under the id its sender minted for it.
///
/// `client_message_id` is the device's own handle for the message it just sent.
/// It is written here, on the row the daemon creates, so the two ends can agree
/// on identity without either inferring it. A device that sends no id (and every
/// browser turn) writes `None`, which is the same row it always got.
pub fn append_identified(
    db: &Db,
    conversation: &str,
    role: Role,
    body: &str,
    state: State,
    client_message_id: Option<&str>,
) -> Result<String, DbError> {
    let id = uuid::Uuid::now_v7().to_string();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO chat_messages (id, conversation_id, role, body, state, client_message_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                id,
                conversation,
                role.as_str(),
                body,
                state.as_str(),
                client_message_id
            ],
        )?;
        Ok(())
    })?;
    Ok(id)
}

/// The newest question row of a conversation, which is the row this turn's
/// operator question just became.
///
/// Read back rather than carried: `append_identified` is handed `&str` and
/// dangling borrows would outlive a `match` arm. One indexed read on a table the
/// turn is about to write anyway, and it is the only way to name the row without
/// changing a signature the browser also calls.
pub fn question_id(db: &Db, conversation: &str) -> Result<Option<String>, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT id FROM chat_messages
              WHERE conversation_id = ?1 AND role = 'user'
              ORDER BY created_at DESC, rowid DESC
              LIMIT 1",
            params![conversation],
            |row| row.get(0),
        )
        .optional()
    })
}

/// Stamp the turn's job id onto both rows of a turn.
///
/// The question and the answer row are written before the job exists, because a
/// client that reads history mid-answer has to see them. The job id arrives a
/// moment later, once the queue has accepted the turn, and this is where the two
/// rows learn which turn they are. One statement, so a row can never carry half
/// a turn.
///
/// Rows that already carry a job id are left alone: a turn is stamped once.
pub fn stamp_turn(
    db: &Db,
    question_id: &str,
    answer_id: &str,
    job_id: &str,
) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_messages
                SET job_id = ?1
              WHERE id IN (?2, ?3) AND job_id IS NULL",
            params![job_id, question_id, answer_id],
        )?;
        Ok(())
    })
}

/// Append the assistant row for a turn that is about to run.
///
/// The model id and the window are written now rather than on settle: if the
/// daemon dies mid-answer the row still says which model was asked and how big
/// its window was, which is what makes the partial row readable later.
pub fn append_answer(
    db: &Db,
    conversation: &str,
    model: &str,
    context_window: Option<u64>,
) -> Result<String, DbError> {
    let id = uuid::Uuid::now_v7().to_string();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO chat_messages
                 (id, conversation_id, role, body, state, model, context_window)
             VALUES (?1, ?2, 'assistant', '', 'streaming', ?3, ?4)",
            params![
                id,
                conversation,
                model,
                context_window.map(|count| count as i64)
            ],
        )?;
        Ok(())
    })?;
    Ok(id)
}

/// The numbers one answer cost, as the turn measured them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Accounting {
    /// Prompt tokens the provider reported.
    pub prompt_tokens: Option<u64>,
    /// Completion tokens the provider reported.
    pub completion_tokens: Option<u64>,
    /// Prompt tokens served from the provider's cache, when it said so.
    pub cached_tokens: Option<u64>,
    /// Wall time from request to last byte.
    pub duration_ms: Option<u64>,
    /// Tool rounds the turn ran.
    pub rounds: Option<u64>,
    /// The last round trip's own prompt size, which is the live context size.
    pub context_tokens: Option<u64>,
    /// Whether the tool budget ended the turn. `None` when the caller did not
    /// measure it, which is every path that is not a real turn: a fixture, a
    /// slash command, a test. Only `Some(true)` is written, so a column full of
    /// zeros cannot pretend a model finished on its own.
    pub capped: Option<bool>,
    /// Whether the engine took the tools away early and the model wrote the
    /// answer. `None` for the same reason as [`Self::capped`]: not measured is
    /// not the same fact as measured and false.
    pub wrapped_up: Option<bool>,
}

/// Record what a running turn has produced so far, without settling it.
///
/// The row exists before the first token precisely so a refresh mid-answer shows
/// the partial reply. It only did that if something wrote the partial reply, and
/// nothing did: `settle` was the only writer, and that runs when there is no
/// partial reply left to show. A page that reloaded therefore showed an empty
/// bubble under the word "Streaming" for as long as the turn ran - the shape the
/// operator cannot distinguish from a daemon that has stopped working.
///
/// So the turn writes what it has as it goes. The body, the calls that already
/// ran, and the round count are the whole of what a reloaded page needs, and all
/// three are what the row's own transcript rendering already reads.
///
/// Only a row still streaming is written. A turn that was stopped or failed by
/// something else keeps that decision: progress must never resurrect a row whose
/// state the operator already changed.
pub fn progress(
    db: &Db,
    id: &str,
    body: &str,
    tools: &[ToolRecord],
    rounds: usize,
) -> Result<(), DbError> {
    let tools_json = (!tools.is_empty())
        .then(|| serde_json::to_string(tools).ok())
        .flatten();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_messages
                SET body = ?2, tools = ?3, rounds = ?4
              WHERE id = ?1 AND state = 'streaming'",
            params![id, body, tools_json, rounds as i64],
        )?;
        Ok(())
    })
}

/// Settle a streaming row with its final text and the provider's own usage.
///
/// `reasoning` is the model's own thinking when it streamed some. It is stored
/// beside the body and never merged into it: the answer is what the operator
/// asked for, and a transcript that mixed the two could not be quoted.
pub fn settle(
    db: &Db,
    id: &str,
    body: &str,
    reasoning: Option<&str>,
    tools: &[ToolRecord],
    usage: Option<&serde_json::Value>,
    accounting: Accounting,
) -> Result<(), DbError> {
    let usage = usage.map(serde_json::Value::to_string);
    // Copied out of the borrow: rusqlite binds owned values, not references, and
    // SQLite stores signed integers while a token count is a u64.
    let Accounting {
        prompt_tokens,
        completion_tokens,
        cached_tokens,
        duration_ms,
        rounds,
        context_tokens,
        capped,
        wrapped_up,
    } = accounting;
    let reasoning = reasoning.filter(|text| !text.trim().is_empty());
    // Null rather than "[]" for a turn that called nothing: absent and empty are
    // the same fact, and a column full of empty arrays is noise to read by hand.
    let tools_json = (!tools.is_empty())
        .then(|| serde_json::to_string(tools).ok())
        .flatten();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_messages
                SET body = ?2, state = 'complete', usage_json = ?3, error_code = NULL,
                    prompt_tokens = ?4, completion_tokens = ?5, cached_tokens = ?6,
                    duration_ms = ?7, reasoning = ?8, tools = ?9, rounds = ?10,
                    context_tokens = ?11, capped = ?12, wrapped_up = ?13
              WHERE id = ?1",
            params![
                id,
                body,
                usage,
                prompt_tokens.map(|count| count as i64),
                completion_tokens.map(|count| count as i64),
                cached_tokens.map(|count| count as i64),
                duration_ms.map(|count| count as i64),
                reasoning,
                tools_json,
                rounds.map(|count| count as i64),
                context_tokens.map(|count| count as i64),
                // Written only when the budget really ended the turn. A row that
                // finished on its own keeps NULL, so "not capped" and "never
                // measured" stay different facts in the table.
                capped.filter(|flag| *flag).map(|_| 1i64),
                // The same rule for the other fact: only a turn that really did
                // wrap up writes a one, so NULL keeps meaning "not this".
                wrapped_up.filter(|flag| *flag).map(|_| 1i64),
            ],
        )?;
        Ok(())
    })
}

/// Record the gateway's limit state beside an answer.
///
/// Separate from [`settle`] because it is optional and read from a file this
/// daemon does not own: a quota that could not be read leaves the column null
/// and the answer exactly as it settled.
pub fn set_quota(db: &Db, id: &str, quota: &crate::web::quota::Quota) -> Result<(), DbError> {
    let raw = serde_json::to_string(quota).unwrap_or_default();
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_messages SET quota_json = ?2 WHERE id = ?1",
            params![id, raw],
        )?;
        Ok(())
    })
}

/// Mark a row refused, keeping whatever text arrived before the failure.
pub fn fail(db: &Db, id: &str, code: &str, body: &str) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_messages
                SET body = ?2, state = 'failed', error_code = ?3
              WHERE id = ?1",
            params![id, body, code],
        )?;
        Ok(())
    })
}

/// Fail every row that was still streaming when this process started.
///
/// A `streaming` row means "a turn is writing this right now". At boot no such
/// turn exists, so any row left in that state is orphaned by a restart. Left
/// alone it renders as a bubble that never fills in, and it suppresses the
/// conversation totals beneath it because the line only renders behind a settled
/// answer.
///
/// Returns how many rows were settled, so startup can say so.
pub fn fail_orphaned(db: &Db) -> Result<usize, DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_messages
                SET state = 'failed', error_code = 'interrupted'
              WHERE state = 'streaming'",
            [],
        )
    })
}

/// Replace a parked tool record with what the call actually returned.
///
/// The transcript is the record of what happened. If a call was parked and then
/// approved, a row that still says "pending_approval" is a row that lies about the
/// turn it belongs to. The approval id stays in the record so the decision remains
/// traceable after the fact.
///
/// Returns how many rows changed, which is zero when the turn never stored the
/// call, and that zero is reported rather than hidden.
pub fn apply_approval_result(
    db: &Db,
    approval_id: &str,
    output: &str,
    is_error: bool,
) -> Result<usize, DbError> {
    let rows: Vec<(String, String)> = db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, tools FROM chat_messages
              WHERE tools IS NOT NULL AND tools LIKE ?1",
        )?;
        let needle = format!("%{approval_id}%");
        let rows = statement.query_map([needle], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?;

    let mut changed = 0usize;
    for (message_id, raw) in rows {
        let Ok(mut records) = serde_json::from_str::<Vec<ToolRecord>>(&raw) else {
            continue;
        };
        let mut touched = false;
        for record in &mut records {
            if record.output.contains(approval_id) {
                record.output = serde_json::json!({
                    "status": "approved",
                    "approval_id": approval_id,
                    "output": output,
                })
                .to_string();
                record.is_error = is_error;
                touched = true;
            }
        }
        if !touched {
            continue;
        }
        let Ok(encoded) = serde_json::to_string(&records) else {
            continue;
        };
        changed += db.with_conn(|conn| {
            conn.execute(
                "UPDATE chat_messages SET tools = ?2 WHERE id = ?1",
                rusqlite::params![message_id, encoded],
            )
        })?;
    }
    Ok(changed)
}

/// How many rows a conversation has.
pub fn count(db: &Db, conversation: &str) -> Result<i64, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM chat_messages WHERE conversation_id = ?1",
            params![conversation],
            |row| row.get(0),
        )
    })
}

/// Which answer rows are still marked streaming in one conversation.
///
/// A `streaming` row is the daemon's own record that a turn is writing that row
/// right now, which is the only honest reading of "a turn is running here": the
/// web path keeps no in-memory table of its turns, and a row survives a restart
/// precisely so this question is answered from the database rather than from
/// process memory. Newest first, so a stop reads the turn that matters first.
pub fn streaming(db: &Db, conversation: &str) -> Result<Vec<String>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id FROM chat_messages
              WHERE conversation_id = ?1 AND state = 'streaming'
              ORDER BY id DESC",
        )?;
        let rows = statement.query_map(params![conversation], |row| row.get::<_, String>(0))?;
        rows.collect()
    })
}

/// Whether one answer row is still streaming.
///
/// Read immediately before a finished turn is settled: a row that is no longer
/// streaming was stopped or failed by something else, and settling over it would
/// replace that decision with an answer the operator already cancelled.
pub fn still_streaming(db: &Db, id: &str) -> Result<bool, DbError> {
    db.with_conn(|conn| {
        conn.query_row(
            "SELECT state = 'streaming' FROM chat_messages WHERE id = ?1",
            params![id],
            |row| row.get::<_, bool>(0),
        )
    })
}

/// Delete a conversation. Returns how many rows went.
pub fn clear(db: &Db, conversation: &str) -> Result<usize, DbError> {
    db.with_conn(|conn| {
        let removed = conn.execute(
            "DELETE FROM chat_messages WHERE conversation_id = ?1",
            params![conversation],
        )?;
        Ok(removed)
    })
}

/// One message as the provider seam wants it.
#[must_use]
pub fn as_chat_message(message: &Message) -> ChatMessage {
    let role = match message.role {
        Role::User => ChatRole::User,
        Role::Assistant => ChatRole::Assistant,
    };
    ChatMessage::text(role, message.body.clone())
}

/// The context a turn carries: settled messages only, newest window, oldest
/// first, and never the message that is being answered right now.
#[must_use]
pub fn history(transcript: &[Message], answering: &str) -> Vec<ChatMessage> {
    let settled: Vec<&Message> = transcript
        .iter()
        .filter(|message| message.id != answering)
        .filter(|message| message.state == State::Complete)
        .filter(|message| !message.body.trim().is_empty())
        .collect();
    let start = settled.len().saturating_sub(HISTORY_MESSAGES);
    settled[start..]
        .iter()
        .map(|message| as_chat_message(message))
        .collect()
}

/// The same window, sized by a number of messages rather than by
/// [`HISTORY_MESSAGES`].
///
/// [`crate::web::context::plan`] decides how many settled messages fit the window
/// the model actually declared, and that number is what a turn carries. The fixed
/// window above stays as the floor a turn falls back to when nobody could resolve
/// a context length.
#[must_use]
pub fn history_keeping(transcript: &[Message], answering: &str, keep: usize) -> Vec<ChatMessage> {
    let settled: Vec<&Message> = transcript
        .iter()
        .filter(|message| message.id != answering)
        .filter(|message| message.state == State::Complete)
        .filter(|message| !message.body.trim().is_empty())
        .collect();
    let start = settled.len().saturating_sub(keep);
    settled[start..]
        .iter()
        .map(|message| as_chat_message(message))
        .collect()
}

/// The same tail, sized by a number of messages.
///
/// The pair of [`history_keeping`]: whatever a plan sized the window to is
/// exactly what falls outside it, so the summary and the prompt cannot disagree
/// about where the boundary was.
#[must_use]
pub fn dropped_keeping(transcript: &[Message], answering: &str, keep: usize) -> Vec<Message> {
    let settled: Vec<&Message> = transcript
        .iter()
        .filter(|message| message.id != answering)
        .filter(|message| message.state == State::Complete)
        .filter(|message| !message.body.trim().is_empty())
        .collect();
    let start = settled.len().saturating_sub(keep);
    settled[..start]
        .iter()
        .map(|message| (*message).clone())
        .collect()
}

/// The turns the window leaves behind, oldest first.
///
/// `history` keeps the newest [`HISTORY_MESSAGES`] and drops the rest. Dropping
/// is silent by design when nothing summarises them, so this returns the same
/// slice `history` excludes: a compression role has something to read, and a turn
/// with no role loses nothing it did not already lose.
#[must_use]
pub fn dropped(transcript: &[Message], answering: &str) -> Vec<Message> {
    let settled: Vec<&Message> = transcript
        .iter()
        .filter(|message| message.id != answering)
        .filter(|message| message.state == State::Complete)
        .filter(|message| !message.body.trim().is_empty())
        .collect();
    let start = settled.len().saturating_sub(HISTORY_MESSAGES);
    settled[..start]
        .iter()
        .map(|message| (*message).clone())
        .collect()
}

/// One auxiliary completion: a system line, a user line, and the text back.
///
/// Every role in this module is the same three steps: build a provider from the
/// resolved [`TurnConfig`], resolve the key, run one round trip and read the
/// reply. Each of them used to spell it out again. Naming it once is what lets a
/// new role be a prompt rather than a copy of fifty lines of client setup, and it
/// is the only place that decides an auxiliary call is best effort: a provider
/// that is down returns `None` and the caller carries on as if the role were
/// unpinned.
#[must_use]
pub fn aux_completion(config: &TurnConfig, system: &str, user: &str) -> Option<String> {
    let profile = config.profile("auxiliary").ok()?;
    let provider = OpenAiCompatProvider::new(
        profile,
        ModelProfile::new(config.model.clone()),
        Capabilities::openai_compatible(),
    )
    .ok()?;
    let key = crate::oauth::credential::resolve_key_now(&config.key_ref).ok()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;

    let request = ChatRequest {
        system: Some(system.to_owned()),
        messages: vec![ChatMessage::text(ChatRole::User, user)],
        ..ChatRequest::default()
    };
    let mut sink = |_event: ProviderEvent| {};
    let completion = runtime
        .block_on(provider.stream_chat(&request, &key, &mut sink))
        .ok()?;
    // A role that asked for no thinking can still be answered by a model that
    // thinks anyway, and its thought is not the summary.
    let text = strip_think(&completion.text);
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// Summarise turns that fell out of the window.
///
/// Best effort, like every auxiliary role: a provider that is down leaves the
/// turn exactly as it would have been without this feature, which is the old
/// behaviour and not a broken conversation.
///
/// Superseded by [`crate::web::context::compress`], which carries a summary
/// forward between turns instead of rebuilding it every time. Kept because it is
/// the whole no-state path in one call, and because a caller that has no
/// conversation row to write to - a subagent, a scheduled run - has nowhere to
/// put a stored summary.
#[must_use]
pub fn summarise(config: &TurnConfig, dropped: &[Message]) -> Option<String> {
    if dropped.is_empty() {
        return None;
    }
    let mut lines = String::new();
    for message in dropped.iter().take(40) {
        let who = match message.role {
            Role::User => "operator",
            Role::Assistant => "assistant",
        };
        let body: String = message.body.chars().take(400).collect();
        lines.push_str(&format!("{who}: {body}\n"));
    }
    aux_completion(
        config,
        "Summarise the earlier part of a conversation so it can be carried as context. \
         Keep decisions, names, paths and open questions. Reply with the summary only, \
         under 150 words, no preamble.",
        &lines,
    )
}

/// Whether an attachment is an image a vision model could read.
#[must_use]
pub fn is_image(file: &Attachment) -> bool {
    let name = file.name.to_ascii_lowercase();
    matches!(
        file.media_type.as_deref(),
        Some("image/png")
            | Some("image/jpeg")
            | Some("image/webp")
            | Some("image/gif")
            | Some("image/heic")
    ) || name.ends_with(".png")
        || name.ends_with(".jpg")
        || name.ends_with(".jpeg")
        || name.ends_with(".webp")
        || name.ends_with(".gif")
        || name.ends_with(".heic")
        || name.ends_with(".heif")
}

/// The largest image handed to a vision model in one turn, in bytes.
pub const MAX_VISION_BYTES: u64 = 8 * 1024 * 1024;

/// Ask a vision model what is in the attached images.
///
/// This is the one role whose output is not a side effect: the operator attached
/// a picture, so the picture has to reach a model or the attachment is a filename
/// and nothing more. Best effort like every role - a provider that cannot see
/// leaves the turn with the same filename description it had before.
#[must_use]
pub fn describe_images(config: &TurnConfig, files: &[Attachment]) -> Option<String> {
    use base64::Engine as _;

    let mut urls: Vec<String> = Vec::new();
    for file in files.iter().filter(|file| is_image(file)) {
        if file.size > MAX_VISION_BYTES {
            continue;
        }
        let Ok(bytes) = std::fs::read(&file.path) else {
            continue;
        };
        let media = file
            .media_type
            .clone()
            .unwrap_or_else(|| "image/png".to_owned());
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        urls.push(format!("data:{media};base64,{encoded}"));
    }
    if urls.is_empty() {
        return None;
    }

    let profile = config.profile("vision").ok()?;
    let provider = OpenAiCompatProvider::new(
        profile,
        ModelProfile::new(config.model.clone()),
        Capabilities::openai_compatible(),
    )
    .ok()?;
    let key = crate::oauth::credential::resolve_key_now(&config.key_ref).ok()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;

    let request = ChatRequest {
        system: Some(
            "Describe what is in the attached image for someone who cannot see it. \
             Transcribe any text exactly. Be specific and brief: what it is, what it says, \
             anything that looks wrong."
                .to_owned(),
        ),
        messages: vec![ChatMessage::text(ChatRole::User, "What is in this image?")],
        images: urls,
        ..ChatRequest::default()
    };

    let mut sink = |_event: ProviderEvent| {};
    let completion = runtime
        .block_on(provider.stream_chat(&request, &key, &mut sink))
        .ok()?;
    let text = completion.text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// Pull the readable content out of a fetched page.
///
/// `fetch_url` already strips tags; this asks the role's model what in the page
/// actually matters, which is the difference between a wall of navigation text
/// and the article someone asked about. Best effort: a failure leaves the fetched
/// text exactly as the tool returned it.
#[must_use]
pub fn extract_readable(config: &TurnConfig, page: &str) -> Option<String> {
    let text = page.trim();
    if text.is_empty() {
        return None;
    }
    let profile = config.profile("web-extract").ok()?;
    let provider = OpenAiCompatProvider::new(
        profile,
        ModelProfile::new(config.model.clone()),
        Capabilities::openai_compatible(),
    )
    .ok()?;
    let key = crate::oauth::credential::resolve_key_now(&config.key_ref).ok()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;

    let excerpt: String = text.chars().take(12_000).collect();
    let request = ChatRequest {
        system: Some(
            "You extract the meaningful content of a web page. Return only what a reader \
             came for: the article, the data, the answer. Drop navigation, cookie banners, \
             related links and boilerplate. Keep it under 300 words."
                .to_owned(),
        ),
        messages: vec![ChatMessage::text(ChatRole::User, excerpt)],
        ..ChatRequest::default()
    };

    let mut sink = |_event: ProviderEvent| {};
    let completion = runtime
        .block_on(provider.stream_chat(&request, &key, &mut sink))
        .ok()?;
    let extracted = completion.text.trim().to_owned();
    (!extracted.is_empty()).then_some(extracted)
}

/// What the approval role thinks of a parked call.
///
/// Advice, never a decision: the row still waits for a person. The point is to
/// hand that person a reading of the call rather than a raw JSON argument blob at
/// the moment they have to say yes or no.
///
/// Best effort. A role that cannot answer leaves the queue exactly as it was.
#[must_use]
pub fn advise_approval(
    config: &TurnConfig,
    tool: &str,
    arguments: &str,
    sandbox_mode: &str,
) -> Option<String> {
    let profile = config.profile("approval").ok()?;
    let provider = OpenAiCompatProvider::new(
        profile,
        ModelProfile::new(config.model.clone()),
        Capabilities::openai_compatible(),
    )
    .ok()?;
    let key = crate::oauth::credential::resolve_key_now(&config.key_ref).ok()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;

    let request = ChatRequest {
        system: Some(
            "You review a single tool call an assistant wants to run on the operator's machine. \
             Reply with one line: a verdict of SAFE, CAREFUL or RISKY, then a dash, then one \
             sentence saying why. Nothing else. Judge blast radius: what it writes, what it \
             reaches, whether it is reversible."
                .to_owned(),
        ),
        messages: vec![ChatMessage::text(
            ChatRole::User,
            format!(
                "Sandbox mode: {sandbox_mode}\nTool: {tool}\nArguments: {}",
                arguments.chars().take(2000).collect::<String>()
            ),
        )],
        ..ChatRequest::default()
    };

    let mut sink = |_event: ProviderEvent| {};
    let completion = runtime
        .block_on(provider.stream_chat(&request, &key, &mut sink))
        .ok()?;
    let advice: String = completion
        .text
        .trim()
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(240)
        .collect();
    (!advice.trim().is_empty()).then_some(advice.trim().to_owned())
}

/// The most pixels of prompt kept when naming a generated file.
pub const IMAGE_SLUG_CHARS: usize = 40;

/// The extension that matches what the bytes actually are.
///
/// The image endpoint returns whatever its model draws - in practice JPEG - so a
/// name is chosen from the magic bytes rather than assumed. A `.png` full of JPEG
/// is a file that lies about itself to every tool that reads the name first.
#[must_use]
pub fn image_extension(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return "png";
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return "jpg";
    }
    if bytes.len() > 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return "webp";
    }
    if bytes.starts_with(b"GIF8") {
        return "gif";
    }
    "bin"
}

/// A filesystem-safe stem for a generated image, from its prompt.
///
/// Deterministic and boring on purpose: a filename derived from a prompt is a
/// record of what was asked, and a slug that cannot contain `..` or a separator
/// cannot walk out of the workspace.
#[must_use]
pub fn image_slug(prompt: &str, stamp: &str, extension: &str) -> String {
    let mut stem: String = prompt
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    stem = stem
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    stem.truncate(IMAGE_SLUG_CHARS);
    stem = stem.trim_end_matches('-').to_owned();
    if stem.is_empty() {
        stem = "image".to_owned();
    }
    format!("{stamp}-{stem}.{extension}")
}

/// Ask the image role to draw something.
///
/// Returns the decoded PNG bytes. Best effort like every role: a provider that
/// cannot draw leaves the turn with a named failure instead of a broken file.
#[must_use]
pub fn generate_image(config: &TurnConfig, prompt: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;

    let endpoint = format!(
        "{}/images/generations",
        config.base_url.trim_end_matches('/')
    );
    let body = serde_json::json!({
        "model": config.model,
        "prompt": prompt,
        "n": 1,
        "response_format": "b64_json",
    });

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    let key = crate::oauth::credential::resolve_key_now(&config.key_ref).ok()?;

    let response = runtime.block_on(async {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .ok()?;
        client
            .post(&endpoint)
            .bearer_auth(key.expose())
            .json(&body)
            .send()
            .await
            .ok()?
            .json::<serde_json::Value>()
            .await
            .ok()
    })?;

    let encoded = response
        .get("data")
        .and_then(serde_json::Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("b64_json"))
        .and_then(serde_json::Value::as_str)?;
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()
}

/// Trim and validate what the browser typed.
///
/// An empty message is a refusal by name rather than a silently dropped turn, and
/// the ceiling is a byte count because that is what the provider bills.
pub fn accept_text(raw: &str) -> Result<String, String> {
    accept_message(raw, false)
}

/// Trim and validate a message that may carry files.
///
/// An empty box is a refusal only when nothing is attached: a picture or a log is
/// a message in its own right, and refusing it for having no caption made the
/// operator type filler to send a screenshot. The body may still be empty after
/// this, which is why the caller writes a placeholder line for the transcript.
pub fn accept_message(raw: &str, has_files: bool) -> Result<String, String> {
    let text = raw.trim();
    if text.is_empty() {
        if has_files {
            return Ok(String::new());
        }
        return Err("a message is required".to_owned());
    }
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(format!(
            "that message is {} and the limit is {}; send it in two parts, or attach it as a file",
            human_bytes(text.len() as u64),
            human_bytes(MAX_MESSAGE_BYTES as u64)
        ));
    }
    Ok(text.to_owned())
}

/// A byte count in the unit a person reads: `940 bytes`, `12.4 kB`, `1.8 MB`.
///
/// A refusal has to name a size the operator recognises. `474674` is not a size
/// anybody thinks in, and `128 KiB` next to it is the reason the sentence was not
/// understood the first time.
#[must_use]
pub fn human_bytes(bytes: u64) -> String {
    if bytes < 1_000 {
        return format!("{bytes} bytes");
    }
    if bytes < 1_000_000 {
        return format!("{:.1} kB", bytes as f64 / 1_000.0);
    }
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// What the transcript says for a message that was nothing but files.
///
/// The bubble renders the attachments themselves, so this line is the title and
/// the one thing a row needs in order not to read as an empty message. It names
/// the files because the same line becomes the conversation's name.
#[must_use]
pub fn attachments_line(files: &[Attachment]) -> String {
    if files.is_empty() {
        return String::new();
    }
    let names: Vec<&str> = files.iter().map(|file| file.name.as_str()).collect();
    if names.len() == 1 {
        return format!("Attached {}", names[0]);
    }
    format!("Attached {} files: {}", names.len(), names.join(", "))
}

/// Where a chat turn sends to, and with what.
#[derive(Debug, Clone)]
pub struct TurnConfig {
    /// The OpenAI-compatible base URL.
    pub base_url: String,
    /// The `env:NAME` reference the key is read from.
    pub key_ref: SecretRef,
    /// The model id.
    pub model: String,
    /// Which request shape this endpoint serves this model in.
    ///
    /// One gateway here serves two: its Claude models answer only on the
    /// Anthropic shape and everything else on the OpenAI one. The shape travels
    /// with the turn because it is a property of the model and the endpoint
    /// together, and a turn that guessed wrong would fail on the wire rather
    /// than in the settings page.
    pub dialect: core_domain::ApiDialect,
    /// Whether this endpoint must be asked to split reasoning from the answer.
    ///
    /// Travels with the turn for the same reason the dialect does: it is a
    /// property of the deployment the registry entry declares, and a turn that
    /// dropped it would put the chain of thought back into the reply.
    pub reasoning_split: bool,
    /// Whether this endpoint is the vendor's own API rather than a gateway.
    ///
    /// Travels with the turn for the third time and the same reason: the vendor
    /// refuses a request without `anthropic-version`, and a gateway in front of
    /// the same model refuses one that carries it. The registry entry declares
    /// it, so the turn carries it rather than a vendor name deciding it.
    pub direct: bool,
    /// The cloud project an Antigravity turn is billed to, when one is known.
    ///
    /// Only the Antigravity dialect reads it, and only a login can discover it:
    /// a projection nobody has connected leaves this `None` and the first turn
    /// says so by name instead of the endpoint answering an opaque 400.
    pub project: Option<String>,
}

impl TurnConfig {
    /// The provider profile this turn is sent as.
    ///
    /// Every role in this module builds its profile here, so the four facts that
    /// decide the wire shape - dialect, reasoning split, whether this is the
    /// vendor's own API, and which cloud project pays - travel together. A role
    /// that spelled them out again is exactly how `greeting.rs` shipped in build
    /// 106 with two of the four missing, and sent the wrong request in silence.
    ///
    /// # Errors
    ///
    /// [`core_domain::LlmError::Protocol`] when the configured base URL is not
    /// one a request can be built for.
    pub fn profile(
        &self,
        label: impl Into<String>,
    ) -> Result<ProviderProfile, core_domain::LlmError> {
        ProviderProfile::new(label, self.base_url.clone(), self.key_ref.clone()).map(|profile| {
            profile
                .with_dialect(self.dialect)
                .with_reasoning_split(self.reasoning_split)
                .with_direct(self.direct)
                .with_project(self.project.clone())
        })
    }
}

/// The stored provider settings, or `None` when onboarding never wrote them.
pub fn turn_config(engine: &SetupEngine) -> Result<Option<TurnConfig>, OnboardingError> {
    let Some(choice) = model_choice(engine)? else {
        return Ok(None);
    };

    let registry = provider_registry(engine)?;
    let Some(chosen) = registry
        .iter()
        .find(|provider| provider.id == choice.provider_id)
        .or_else(|| crate::providers::resolve(&registry, &choice.provider_id))
    else {
        return Ok(None);
    };
    let Ok(key_ref) = SecretRef::try_from(chosen.key_ref.as_str()) else {
        return Ok(None);
    };
    let dialect = engine.provider_dialect_for(&chosen.id, &choice.model)?;

    Ok(Some(TurnConfig {
        base_url: chosen.base_url.clone(),
        key_ref,
        model: choice.model,
        dialect,
        reasoning_split: crate::providers::reasoning_split_of(chosen),
        direct: crate::providers::direct_of(chosen),
        project: crate::providers::project_of(chosen),
    }))
}

/// The provider and the model a turn runs on, which are one decision.
///
/// Stored as one row, `provider/model`, because a model id belongs to the
/// endpoint that published it: `deepseek/deepseek-v4-flash` is served by the
/// native gateway and by nothing else, and a model chosen on one provider must
/// keep its provider when the primary moves. A stored value whose first segment
/// is not a provider id is read whole, which is what keeps every value written
/// before this form existed working: `cmc/deepseek/deepseek-v4-flash` is one
/// model id on the primary, not the model `deepseek-v4-flash` on `cmc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    /// The registry id of the provider that serves the model.
    pub provider_id: String,
    /// The model id, as that provider spells it.
    pub model: String,
}

/// Split a stored or submitted model value into a provider and a model.
///
/// The left half is a provider only when the registry actually holds that id.
/// Refusing to treat an unknown prefix as a provider is deliberate: 9router
/// spells its ids with slashes, and reading those as providers would silently
/// re-point every existing install at nothing.
#[must_use]
pub fn split_model_value(
    value: &str,
    registry: &[crate::providers::Provider],
    primary: &str,
) -> ModelChoice {
    let value = value.trim();
    if let Some((head, rest)) = value.split_once('/')
        && registry.iter().any(|provider| provider.id == head)
        && !rest.trim().is_empty()
    {
        return ModelChoice {
            provider_id: head.to_owned(),
            model: rest.to_owned(),
        };
    }
    ModelChoice {
        provider_id: crate::providers::resolve(registry, primary)
            .map(|provider| provider.id.clone())
            .unwrap_or_else(|| crate::providers::DEFAULT_ID.to_owned()),
        model: value.to_owned(),
    }
}

/// The stored model choice, or `None` when no model is configured.
pub fn model_choice(engine: &SetupEngine) -> Result<Option<ModelChoice>, OnboardingError> {
    let Some(stored) = engine.setting(SETTING_MODEL)? else {
        return Ok(None);
    };
    if stored.trim().is_empty() {
        return Ok(None);
    }
    let registry = provider_registry(engine)?;
    let primary = engine
        .setting(crate::providers::SETTING_PRIMARY)?
        .unwrap_or_else(|| crate::providers::DEFAULT_ID.to_owned());
    Ok(Some(split_model_value(&stored, &registry, &primary)))
}

/// Every `provider/model` pair this daemon could run on, as
/// `(option value, label)`.
///
/// One list over every configured provider, in registry order, taken from the
/// catalogue that provider's own probe published. A provider with no probe yet
/// contributes nothing: an option that names a model nobody has confirmed would
/// make the picker promise more than the endpoint can answer.
#[must_use]
pub fn model_options(engine: &SetupEngine) -> Vec<(String, String)> {
    let Ok(registry) = provider_registry(engine) else {
        return Vec::new();
    };
    let mut options = Vec::new();
    for provider in &registry {
        let catalog = engine.provider_catalog(&provider.id).unwrap_or_default();
        for model in catalog {
            options.push((
                format!("{}/{}", provider.id, model),
                format!("{} - {}", provider.name, model),
            ));
        }
    }
    options
}

/// The stored choice as an option value, qualified with its provider.
///
/// A stored value that names no provider is qualified with the primary, because
/// that is the provider a turn would resolve it to. The value is returned as it
/// is when the primary does not serve it either, so the page can show what is
/// stored rather than silently replacing it with the nearest match.
#[must_use]
pub fn model_value(engine: &SetupEngine) -> String {
    let Ok(Some(choice)) = model_choice(engine) else {
        return String::new();
    };
    format!("{}/{}", choice.provider_id, choice.model)
}

/// Render a stored model value as the picker's own option value.
///
/// A pin written before this picker existed is a bare model id, and the option
/// values are provider-qualified, so the two would never compare equal and the
/// select would show nothing selected on a working install. Qualifying the stored
/// value with the provider the role actually runs on is what keeps the page
/// showing what a turn would really do. A value that already names a provider,
/// and the inherit sentinel, are returned unchanged.
#[must_use]
pub fn qualify_model(
    value: &str,
    registry: &[crate::providers::Provider],
    provider_id: &str,
) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed == crate::aux::INHERIT {
        return trimmed.to_owned();
    }
    if let Some((head, rest)) = trimmed.split_once('/')
        && registry.iter().any(|provider| provider.id == head)
        && !rest.trim().is_empty()
    {
        return trimmed.to_owned();
    }
    format!("{provider_id}/{trimmed}")
}

/// Point a turn at the provider and the request shape a model choice names.
///
/// A model id typed from the composer that carries no provider keeps the turn's
/// provider: the composer means "this model, on the endpoint I am already
/// talking to", and only an explicit `provider/model` moves the endpoint.
pub fn apply_model(
    config: &mut TurnConfig,
    engine: &SetupEngine,
    value: &str,
) -> Result<(), OnboardingError> {
    let registry = provider_registry(engine)?;
    let primary = engine
        .setting(crate::providers::SETTING_PRIMARY)?
        .unwrap_or_else(|| crate::providers::DEFAULT_ID.to_owned());
    // A bare id keeps the turn's own endpoint; a qualified one moves it.
    let (provider_id, model) = if value
        .trim()
        .split_once('/')
        .is_some_and(|(head, _)| registry.iter().any(|provider| provider.id == head))
    {
        let choice = split_model_value(value, &registry, &primary);
        (choice.provider_id, choice.model)
    } else {
        let current = registry
            .iter()
            .find(|provider| provider.base_url == config.base_url)
            .or_else(|| crate::providers::resolve(&registry, &primary));
        (
            current.map(|p| p.id.clone()).unwrap_or_default(),
            value.trim().to_owned(),
        )
    };
    if let Some(chosen) = registry.iter().find(|provider| provider.id == provider_id)
        && let Ok(key_ref) = SecretRef::try_from(chosen.key_ref.as_str())
    {
        config.base_url = chosen.base_url.clone();
        config.key_ref = key_ref;
        config.reasoning_split = crate::providers::reasoning_split_of(chosen);
        // The four facts that decide the wire shape all move with the endpoint.
        // A turn that changed provider and kept the old direct flag would send
        // vendor headers to a gateway, so the whole set is taken from the entry.
        config.direct = crate::providers::direct_of(chosen);
        config.project = crate::providers::project_of(chosen);
    }
    let dialect = engine.provider_dialect_for(&provider_id, &model)?;
    config.dialect = dialect;
    config.model = model;
    Ok(())
}

/// The configured providers, seeded from the legacy rows when the registry has
/// never been written.
///
/// A registry that does not parse falls back to the legacy rows rather than
/// disabling every turn: the settings page refuses to store an invalid document,
/// so this can only be a hand-edited database, and a daemon that answers is more
/// useful than one that will not start. The settings page shows the registry it
/// actually read, so the fallback is visible.
pub fn provider_registry(
    engine: &SetupEngine,
) -> Result<Vec<crate::providers::Provider>, OnboardingError> {
    let legacy_url = engine.setting(SETTING_PROVIDER_BASE_URL)?;
    let legacy_key = engine.setting(SETTING_PROVIDER_KEY_REF)?;

    if let Some(raw) = engine.setting(crate::providers::SETTING_REGISTRY)?
        && !raw.trim().is_empty()
        && let Ok(parsed) = crate::providers::parse(&raw)
        && !parsed.is_empty()
    {
        return Ok(parsed);
    }

    Ok(match (legacy_url, legacy_key) {
        (Some(base_url), Some(key_ref)) => crate::providers::seed(&base_url, &key_ref),
        _ => Vec::new(),
    })
}

/// Why a turn could not be sent, in the two halves the page shows: a stable code
/// and a sentence.
pub type TurnFailure = (String, String);

/// The `settings` row that holds how full a window may get before a turn
/// compresses what it cannot carry.
///
/// Named here rather than only in [`crate::web::context`] because two surfaces
/// resolve it: the composer sizes its window from it and a device turn does too,
/// and a phone turn must protect the same window the browser does.
pub const SETTING_CONTEXT_SHARE: &str = crate::web::context::SETTING_SHARE;

/// The token budget a turn's prompt may occupy, from the model it runs on.
///
/// The window is the one the provider serving that model declared, times the
/// configured share. `None` when no model is pinned or no window was probed: a
/// budget invented from nothing would cut tool results for no reason a reader
/// could see, which is worse than letting the provider report the overflow.
#[must_use]
pub fn context_budget_for(engine: &SetupEngine, share: Option<&str>) -> Option<u64> {
    let choice = model_choice(engine).ok().flatten()?;
    let window = engine
        .provider_contexts_for(&choice.provider_id)
        .ok()
        .and_then(|contexts| contexts.get(&choice.model).copied())
        .filter(|window| *window > 0)?;
    Some(window * u64::from(crate::web::context::share(share)) / 100)
}

/// The `settings` row that remembers whether the thinking bubble is shown.
///
/// Default on: the bubble exists to be seen, and hiding it is the operator's
/// choice rather than the starting state.
pub const SETTING_SHOW_REASONING: &str = "assistant.show_reasoning";

/// Whether to show and stream the model's reasoning.
///
/// Anything other than an explicit `false` leaves it on, so a corrupt row cannot
/// silently hide a feature the operator asked for.
#[must_use]
pub fn show_reasoning(stored: Option<&str>) -> bool {
    !matches!(stored.map(str::trim), Some("false"))
}

/// The `settings` row that remembers how a settled reply is drawn.
///
/// Default `expanded`: the whole answer, which is what every build before 108
/// did. The other two states cost the operator a choice before they can lose
/// anything, and a corrupt row falls back to the full answer rather than hiding
/// one.
pub const SETTING_REPLY_STATE: &str = "assistant.reply_state";

/// How much of a settled reply the transcript draws.
///
/// This is the reply's own size, not the thinking bubble's. Minimized is the
/// headline: a thread of twenty answers becomes twenty summary lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyState {
    /// The whole answer, which is the shape every earlier build shipped.
    Expanded,
    /// The first few lines, clipped under a fade. Press it to read the rest.
    Collapsed,
    /// One line: who said it and the opening words.
    Minimized,
}

impl ReplyState {
    /// Every state, in the order the Settings select offers them.
    pub const ALL: [Self; 3] = [Self::Expanded, Self::Collapsed, Self::Minimized];

    /// The stored spelling, which is also the `data-reply-state` wire value the
    /// page script reads and writes.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Expanded => "expanded",
            Self::Collapsed => "collapsed",
            Self::Minimized => "minimized",
        }
    }

    /// The line the Settings select shows for this state.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Expanded => "Expanded - the whole answer",
            Self::Collapsed => "Collapsed - the first few lines",
            Self::Minimized => "Minimized - one summary line",
        }
    }
}

/// The stored reply size, defaulting to the full answer.
///
/// Anything unrecognised, including a missing row, is `Expanded`, so a corrupt
/// value cannot hide an answer the operator has not read.
#[must_use]
pub fn reply_state(stored: Option<&str>) -> ReplyState {
    match stored.map(str::trim) {
        Some("collapsed") => ReplyState::Collapsed,
        Some("minimized") => ReplyState::Minimized,
        _ => ReplyState::Expanded,
    }
}

/// The `settings` row that remembers whether the dashboard streams answers.
///
/// Default on: streaming is what makes a long answer readable while it is being
/// written. Off means the composer hands the turn to the plain form and the page
/// comes back rendered once, which is the one path a browser with no script uses.
pub const SETTING_STREAMING: &str = "assistant.stream_responses";

/// Whether the dashboard should stream answers.
///
/// Anything other than an explicit `false` leaves it on, so a corrupt row cannot
/// silently turn every answer into a full-page reload.
#[must_use]
pub fn streaming_enabled(stored: Option<&str>) -> bool {
    !matches!(stored.map(str::trim), Some("false"))
}

/// The `settings` row that remembers whether the run log is shown.
///
/// Default on: a turn that ran something should say so. The log is a record, and
/// hiding the record is the operator's choice rather than the starting state.
pub const SETTING_SHOW_TOOL_LOG: &str = "assistant.show_tool_log";

/// How much of the run log to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolLog {
    /// One window per call, each openable.
    Full,
    /// One collapsed line for the whole turn, openable for the detail.
    Summary,
    /// Not rendered and not streamed.
    Off,
}

impl ToolLog {
    /// The stored spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Summary => "summary",
            Self::Off => "off",
        }
    }

    /// Whether anything at all is rendered or streamed.
    #[must_use]
    pub fn any(self) -> bool {
        !matches!(self, Self::Off)
    }
}

/// The `settings` value for the run log mode.
///
/// The boolean that shipped first is still honoured: `false` is `off`, `true` is
/// `full`, and anything unrecognised falls back to `full` so a corrupt row cannot
/// silently hide what a turn did.
#[must_use]
pub fn tool_log(store: Option<&str>) -> ToolLog {
    match store.map(str::trim) {
        Some("off") | Some("false") => ToolLog::Off,
        Some("summary") => ToolLog::Summary,
        _ => ToolLog::Full,
    }
}

/// Whether to render and stream the run log at all.
#[must_use]
pub fn show_tool_log(stored: Option<&str>) -> bool {
    tool_log(stored).any()
}

/// What the operator configured about tool use for this turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnSettings {
    /// Whether a turn may call tools at all.
    pub tools: bool,
    /// How many tool rounds it may take before the last one is forced to answer.
    pub max_rounds: usize,
    /// Whether the turn may dispatch subagents.
    ///
    /// Separate from `tools` because dispatching is not a tool the sandbox runs:
    /// it writes a row and starts a child turn. Keeping it apart is what lets a
    /// child turn be built with both switches off, which is the whole guarantee
    /// that a subagent can neither use a tool nor dispatch another subagent.
    pub agents: bool,
    /// How many tokens this turn's prompt may occupy before the loop cuts tool
    /// results to fit, from the window the model declared at the configured
    /// share. `None` when nobody could resolve a window, which leaves the loop
    /// exactly as it was rather than inventing a budget.
    ///
    /// Carried on the settings rather than computed in the loop because the
    /// window is a property of the model the caller resolved, and the loop serves
    /// the browser, the phone, a subagent and a note worker from one body.
    pub context_budget: Option<u64>,
}

// ---------------------------------------------------------------------------
// Steering a turn that is already running
// ---------------------------------------------------------------------------

/// What a running turn should do about the operator, asked at a round boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnSignal {
    /// Nothing was typed: carry on exactly as the turn was going.
    Continue,
    /// The operator asked it to stop.
    ///
    /// This is a boundary, not a socket close. The provider request that is
    /// already streaming is not aborted mid-flight - `stream_chat` is one blocking
    /// call and there is no seam inside it to interrupt - so what stops is the
    /// turn: no further round trips, no answer written. See [`drive_tool_loop`].
    Stop,
    /// The operator typed this while the turn ran. The current round ends, the
    /// message is appended, and a fresh round answers it as a continuation.
    Steer(String),
}

/// How a turn hears the operator: a question with three answers, asked between
/// rounds.
///
/// A `&mut dyn FnMut` rather than a new generic parameter. `drive_tool_loop`,
/// `run_turn` and `run_turn_with_key` all take this by reference, so the seam is
/// one more argument instead of one more type parameter threaded through every
/// caller, and a turn that has nothing to hear passes a closure that says so.
/// The turn asks; the queue answers; neither knows the other exists.
pub type TurnPoll<'a> = dyn FnMut() -> TurnSignal + 'a;

/// One running turn's stop flag and the messages waiting for it.
#[derive(Debug, Default)]
struct Control {
    steers: std::collections::VecDeque<String>,
    stop: bool,
}

impl Control {
    /// What the operator asked for, in one answer.
    ///
    /// A stop outranks a steer: an operator who asked to stop does not want one
    /// more round trip run first.
    fn signal(&mut self) -> TurnSignal {
        if self.stop {
            return TurnSignal::Stop;
        }
        match self.steers.pop_front() {
            Some(text) => TurnSignal::Steer(text),
            None => TurnSignal::Continue,
        }
    }
}

/// The lock around one turn's [`Control`].
///
/// A poisoned lock is recovered rather than propagated: the only writer is this
/// module, and a panic inside it must not turn a steer into a stuck turn.
#[derive(Debug, Default)]
struct TurnControl {
    inner: std::sync::Mutex<Control>,
}

impl TurnControl {
    fn lock(&self) -> std::sync::MutexGuard<'_, Control> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn signal(&self) -> TurnSignal {
        self.lock().signal()
    }

    fn push(&self, text: String) -> usize {
        let mut control = self.lock();
        control.steers.push_back(text);
        control.steers.len()
    }

    fn queued(&self) -> usize {
        self.lock().steers.len()
    }

    /// Whatever was still waiting when the turn stopped listening.
    fn drain(&self) -> Vec<String> {
        self.lock().steers.drain(..).collect()
    }

    fn request_stop(&self) -> bool {
        let mut control = self.lock();
        let first = !control.stop;
        control.stop = true;
        first
    }
}

type Running = std::sync::Mutex<std::collections::BTreeMap<String, std::sync::Arc<TurnControl>>>;

/// Which conversation is running a turn, and what has been said to it.
///
/// One entry per conversation, taken before the turn's first row is written and
/// released when its blocking task returns. The entry is what makes a second
/// message a steer instead of a second turn: the claim and the queue are one
/// operation under one lock, so a message can never fall between "is anything
/// running here" and "queue it for that turn".
#[derive(Debug, Clone, Default)]
pub struct Steering {
    running: std::sync::Arc<Running>,
}

/// Where a message went: into a new turn, or into the one already running.
#[derive(Debug)]
pub enum Claim {
    /// The conversation was free. The lease is the claim on it, held for as long
    /// as the turn runs.
    Turn(TurnLease),
    /// A turn is already streaming here. The message is queued for it, and the
    /// number is how many are now waiting.
    Steered(usize),
}

impl Steering {
    /// An empty queue: nothing is running anywhere yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Claim the conversation for a turn, or hand the message to the turn that
    /// already holds it.
    pub fn claim(&self, conversation: &str, text: &str) -> Claim {
        let mut running = self.lock();
        if let Some(control) = running.get(conversation) {
            return Claim::Steered(control.push(text.to_owned()));
        }
        let control = std::sync::Arc::new(TurnControl::default());
        running.insert(conversation.to_owned(), std::sync::Arc::clone(&control));
        Claim::Turn(TurnLease {
            steering: self.clone(),
            conversation: conversation.to_owned(),
            control,
        })
    }

    /// Queue a message for the running turn. `None` when nothing is running,
    /// which is the honest answer to "did it land".
    pub fn queue(&self, conversation: &str, text: &str) -> Option<usize> {
        let running = self.lock();
        running
            .get(conversation)
            .map(|control| control.push(text.to_owned()))
    }

    /// Ask the running turn to stop. False when nothing is running, or when it
    /// was already asked.
    pub fn stop(&self, conversation: &str) -> bool {
        let running = self.lock();
        running
            .get(conversation)
            .is_some_and(|control| control.request_stop())
    }

    /// How many messages are waiting for the running turn.
    #[must_use]
    pub fn queued(&self, conversation: &str) -> usize {
        let running = self.lock();
        running
            .get(conversation)
            .map_or(0, |control| control.queued())
    }

    /// Whether a turn is running in this conversation at all.
    #[must_use]
    pub fn running(&self, conversation: &str) -> bool {
        self.lock().contains_key(conversation)
    }

    fn lock(
        &self,
    ) -> std::sync::MutexGuard<'_, std::collections::BTreeMap<String, std::sync::Arc<TurnControl>>>
    {
        self.running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Drop one turn's entry, and only that turn's: a lease that somehow outlived
    /// a replacement must not evict the turn that took over.
    fn release(&self, conversation: &str, control: &std::sync::Arc<TurnControl>) {
        let mut running = self.lock();
        if running
            .get(conversation)
            .is_some_and(|held| std::sync::Arc::ptr_eq(held, control))
        {
            running.remove(conversation);
        }
    }
}

/// A conversation claimed for one running turn.
///
/// Dropping it releases the claim, which is what makes the next message a turn of
/// its own again. The lease is also the turn's ear: [`TurnLease::signal`] is the
/// hook [`drive_tool_loop`] calls between rounds.
#[derive(Debug)]
pub struct TurnLease {
    steering: Steering,
    conversation: String,
    control: std::sync::Arc<TurnControl>,
}

impl TurnLease {
    /// What the operator has asked for since the last boundary.
    #[must_use]
    pub fn signal(&self) -> TurnSignal {
        self.control.signal()
    }

    /// How many messages are waiting right now.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.control.queued()
    }

    /// Messages that arrived after the turn stopped listening.
    ///
    /// A steer that lands between the last boundary check and the write of the
    /// answer has no round left to join. It is taken here so the caller can say
    /// so, rather than let the operator's words disappear.
    #[must_use]
    pub fn drain(&self) -> Vec<String> {
        self.control.drain()
    }
}

impl Drop for TurnLease {
    fn drop(&mut self) {
        self.steering.release(&self.conversation, &self.control);
    }
}

/// One thing the turn produced while it was still running.
///
/// The two are kept apart all the way to the database because they mean different
/// things: the reply is what the operator asked for, and the reasoning is how the
/// model got there. Merging them would make an answer impossible to quote.
pub enum StreamPart<'a> {
    /// Reply text.
    Text(&'a str),
    /// The model's own reasoning, when the provider streams one.
    Reasoning(&'a str),
    /// A tool call that just ran, so the operator can watch the work.
    Tool {
        /// The tool name.
        name: &'a str,
        /// The arguments, as JSON.
        arguments: &'a str,
        /// What it returned.
        output: &'a str,
        /// Whether it reported a failure.
        is_error: bool,
        /// What a `clarify` call asked and what was answered, when the tool was
        /// `clarify`.
        ///
        /// Carried on the finished call rather than only in the transcript so a
        /// surface that stores its own copy - the device hub does - keeps the same
        /// card the browser draws, instead of a run-log row with a question-shaped
        /// string in it.
        asked: Option<&'a Asked>,
    },
    /// A tool call that is about to run, before it has produced anything.
    ///
    /// The finished [`StreamPart::Tool`] frame is a receipt: it lands after the
    /// call returned, which is the right order for a run log and the wrong order
    /// for a status line. A page that wants to say "Writing a file" while the file
    /// is being written has to hear about the call first, and until this frame
    /// existed it could only say what had already happened. Sent before the gate,
    /// so a call that is parked for approval is announced as loudly as one that
    /// runs.
    ToolStart {
        /// The tool name.
        name: &'a str,
        /// The arguments, as JSON.
        arguments: &'a str,
    },
    /// Output from a call that is still running.
    ///
    /// A command that takes twenty seconds used to look like a turn that was
    /// thinking very hard about nothing, and then produced its output all at
    /// once. This is that same output as it arrives, which is what makes the run
    /// log read like a terminal rather than like a receipt printed afterwards.
    ToolOutput {
        /// The tool that is currently writing.
        name: &'a str,
        /// What it wrote, this chunk.
        chunk: &'a str,
    },
    /// A `clarify` call is waiting for an answer.
    ///
    /// Sent before the turn blocks, because a page that learns about the question
    /// after it is answered has nothing to draw: the card is only useful while
    /// somebody can still click it.
    Asked {
        /// What the model asked.
        request: &'a crate::web::clarify::Request,
    },
    /// A tool result was cut down to fit the context window.
    ///
    /// Said out loud because the model's next round is answering with less than
    /// the tool actually returned. A turn that quietly answers from a truncated
    /// output reads as a wrong answer rather than as a window that filled up.
    Elided {
        /// How many results were cut this round.
        results: usize,
    },
    /// A round is starting, before the model has said anything.
    ///
    /// The longest silence in a turn is the model call itself: nothing is written
    /// for as long as the provider takes, which on a hundred-thousand-token
    /// context is minutes. Without this the page had no way to tell a turn that
    /// was thinking from a turn that had died, and the operator's only reading was
    /// the word "Streaming" sitting still. Sent before the call, so the count on
    /// screen is the round about to run rather than the one that just finished.
    Round {
        /// Which round is starting, one-based, never above the budget.
        round: usize,
        /// The turn's tool budget.
        max_rounds: usize,
        /// Whether that round may still call tools. False on the closing round,
        /// where the model has to answer with what it already has.
        tools_allowed: bool,
    },
}

/// Run one turn to completion, handing each fragment to `sink`.
///
/// This is deliberately a blocking call on its own thread, exactly like the
/// device turn: `stream_chat` takes `&mut dyn FnMut`, which is not `Send`, so the
/// sink must not be held across an await. The caller runs this inside
/// `spawn_blocking`.
///
/// The key is resolved here, from the daemon's own environment. A caller that
/// resolves it elsewhere - the device surface injects its lookup so a test can
/// prove the path - calls [`run_turn_with_key`] with the key it already has.
///
/// `poll` is the turn's ear: a caller that has something to say to a running turn
/// passes its queue, and a caller that does not passes a closure that always says
/// [`TurnSignal::Continue`]. Nothing else about the turn changes, so there is one
/// loop whether or not anybody is steering it.
// The argument list is the seam: provider, prompt, sandbox, gate, pool, settings,
// poll, sink. Bundling them into a struct would only move the same fields behind a
// name, and every one of them is independently optional at the call site. `poll`
// stays a `&mut dyn FnMut` for the same reason: one more reference, not one more
// type parameter threaded through every caller.
#[allow(clippy::too_many_arguments)]
pub fn run_turn(
    config: &TurnConfig,
    system: Option<String>,
    messages: Vec<ChatMessage>,
    sandbox: &crate::tools::Sandbox,
    gate: &mut dyn FnMut(&core_domain::ToolCall) -> GateDecision,
    mcp: Option<&crate::mcp::Pool>,
    web_extract: Option<&TurnConfig>,
    image: Option<&TurnConfig>,
    agents: Option<&crate::subagents::Desk>,
    skills: Option<&crate::skills::Access>,
    settings: TurnSettings,
    ask: Option<&crate::web::clarify::Ask>,
    poll: &mut TurnPoll<'_>,
    sink: &mut dyn FnMut(StreamPart<'_>),
) -> Result<TurnOutcome, TurnFailure> {
    let key = crate::oauth::credential::resolve_key_now(&config.key_ref)
        .map_err(|error| (error.kind().to_owned(), error.to_string()))?;
    run_turn_with_key(
        config,
        &key,
        system,
        messages,
        sandbox,
        gate,
        mcp,
        web_extract,
        image,
        agents,
        skills,
        settings,
        ask,
        poll,
        sink,
    )
}

/// What happened around a failed primary turn, for the caller's audit and stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResilientNote {
    /// The primary answered, or there was no fallback pinned to consult.
    Primary,
    /// A fallback was pinned but its key could not be read, so the primary
    /// failure stands.
    FallbackUnusable(String),
    /// The primary failed with this code and the turn was retried on the fallback
    /// model.
    Retried {
        primary_model: String,
        primary_code: String,
        model: String,
    },
}

/// [`run_turn`] with an optional fallback.
///
/// A turn runs on the primary model. When that fails and a fallback model is
/// pinned, the same turn runs once more on the fallback. The operator's Stop is
/// never retried: a turn they cancelled stays cancelled. The note reports which
/// path was taken so the caller can audit it and, on a retry, mark the answer
/// with the model that actually produced it.
#[allow(clippy::too_many_arguments)]
pub fn run_turn_resilient(
    config: &TurnConfig,
    fallback: Option<&TurnConfig>,
    system: Option<String>,
    messages: Vec<ChatMessage>,
    sandbox: &crate::tools::Sandbox,
    gate: &mut dyn FnMut(&core_domain::ToolCall) -> GateDecision,
    mcp: Option<&crate::mcp::Pool>,
    web_extract: Option<&TurnConfig>,
    image: Option<&TurnConfig>,
    agents: Option<&crate::subagents::Desk>,
    skills: Option<&crate::skills::Access>,
    settings: TurnSettings,
    ask: Option<&crate::web::clarify::Ask>,
    poll: &mut TurnPoll<'_>,
    sink: &mut dyn FnMut(StreamPart<'_>),
) -> (Result<TurnOutcome, TurnFailure>, ResilientNote) {
    let key = match crate::oauth::credential::resolve_key_now(&config.key_ref) {
        Ok(key) => key,
        Err(error) => {
            return (
                Err((error.kind().to_owned(), error.to_string())),
                ResilientNote::Primary,
            );
        }
    };
    // The fallback key is resolved up front too: a missing one must be reported
    // as "fallback unusable", not as a silent skip, and the retry decision itself
    // stays in `run_turn_resilient_with_keys` where it is tested.
    let fallback_key =
        fallback.and_then(|config| crate::oauth::credential::resolve_key_now(&config.key_ref).ok());
    run_turn_resilient_with_keys(
        config,
        &key,
        fallback,
        fallback_key.as_ref(),
        system,
        messages,
        sandbox,
        gate,
        mcp,
        web_extract,
        image,
        agents,
        skills,
        settings,
        ask,
        poll,
        sink,
    )
}

/// [`run_turn_resilient`] with both keys already resolved; the fallback key is
/// `None` when the role is unpinned or its key could not be read.
#[allow(clippy::too_many_arguments)]
pub fn run_turn_resilient_with_keys(
    config: &TurnConfig,
    key: &core_runtime::provider::SecretKey,
    fallback: Option<&TurnConfig>,
    fallback_key: Option<&core_runtime::provider::SecretKey>,
    system: Option<String>,
    messages: Vec<ChatMessage>,
    sandbox: &crate::tools::Sandbox,
    gate: &mut dyn FnMut(&core_domain::ToolCall) -> GateDecision,
    mcp: Option<&crate::mcp::Pool>,
    web_extract: Option<&TurnConfig>,
    image: Option<&TurnConfig>,
    agents: Option<&crate::subagents::Desk>,
    skills: Option<&crate::skills::Access>,
    settings: TurnSettings,
    ask: Option<&crate::web::clarify::Ask>,
    poll: &mut TurnPoll<'_>,
    sink: &mut dyn FnMut(StreamPart<'_>),
) -> (Result<TurnOutcome, TurnFailure>, ResilientNote) {
    // The retry needs a fresh copy of what the first attempt consumes.
    let retry_system = system.clone();
    let retry_messages = messages.clone();

    match run_turn_with_key(
        config,
        key,
        system,
        messages,
        sandbox,
        gate,
        mcp,
        web_extract,
        image,
        agents,
        skills,
        settings,
        ask,
        poll,
        sink,
    ) {
        Ok(turn) => (Ok(turn), ResilientNote::Primary),
        Err(failure) => {
            // The operator's Stop is final; never resurrect an answer they cut off.
            if failure.0 == stopped().0 {
                return (Err(failure), ResilientNote::Primary);
            }
            let Some(fallback_config) = fallback else {
                return (Err(failure), ResilientNote::Primary);
            };
            let Some(fallback_key) = fallback_key else {
                return (
                    Err(failure),
                    ResilientNote::FallbackUnusable(
                        "the fallback key could not be resolved".to_owned(),
                    ),
                );
            };
            let note = ResilientNote::Retried {
                primary_model: config.model.clone(),
                primary_code: failure.0.clone(),
                model: fallback_config.model.clone(),
            };
            let retried = run_turn_with_key(
                fallback_config,
                fallback_key,
                retry_system,
                retry_messages,
                sandbox,
                gate,
                mcp,
                web_extract,
                image,
                agents,
                skills,
                settings,
                ask,
                poll,
                sink,
            );
            (retried, note)
        }
    }
}

/// Rewrite which model answered a row, so a fallback answer is not badged as the
/// primary that failed to produce it.
pub fn set_model(db: &Db, id: &str, model: &str) -> Result<(), DbError> {
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_messages SET model = ?2 WHERE id = ?1",
            params![id, model],
        )?;
        Ok(())
    })
}

/// The same turn, with the provider key already resolved.
///
/// Split from [`run_turn`] so the device surface drives this exact loop rather
/// than a second copy of it, while still resolving `env:NAME` through the lookup
/// its own tests inject. Everything about the rounds, the tool calls and the
/// reasoning is shared: there is one loop, and this is it.
#[allow(clippy::too_many_arguments)]
pub fn run_turn_with_key(
    config: &TurnConfig,
    key: &core_runtime::provider::SecretKey,
    system: Option<String>,
    messages: Vec<ChatMessage>,
    sandbox: &crate::tools::Sandbox,
    gate: &mut dyn FnMut(&core_domain::ToolCall) -> GateDecision,
    mcp: Option<&crate::mcp::Pool>,
    web_extract: Option<&TurnConfig>,
    image: Option<&TurnConfig>,
    agents: Option<&crate::subagents::Desk>,
    skills: Option<&crate::skills::Access>,
    settings: TurnSettings,
    ask: Option<&crate::web::clarify::Ask>,
    poll: &mut TurnPoll<'_>,
    sink: &mut dyn FnMut(StreamPart<'_>),
) -> Result<TurnOutcome, TurnFailure> {
    let profile = config
        .profile("chat-turn")
        .map_err(|error| (error.kind().to_owned(), error.to_string()))?;
    let provider = OpenAiCompatProvider::new(
        profile,
        ModelProfile::new(config.model.clone()),
        Capabilities::openai_compatible(),
    )
    .map_err(|error| (error.kind().to_owned(), error.to_string()))?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            (
                "runtime_unavailable".to_owned(),
                format!("cannot start the chat runtime: {error}"),
            )
        })?;

    let system = system.filter(|text| !text.trim().is_empty());

    // A namespaced name belongs to an MCP server; anything else is a local tool.
    // The split is by shape, not by a lookup, so a server that failed to start
    // still reports "that tool is MCP" rather than "unknown tool".
    let mut execute = |call: &core_domain::ToolCall,
                       chunks: &mut dyn FnMut(&str),
                       asked: &mut dyn FnMut(Asked),
                       poll: &mut TurnPoll<'_>|
     -> crate::tools::ToolResult {
        // A question. Parked here rather than in the gate, because the gate is
        // consulted for every call and this one is not a permission decision:
        // for a clarify the gate always says run, and landing here is the only
        // signal that the model asked rather than acted.
        //
        // Parked before it is waited on, and the wait reports the answer back
        // through `asked` so the transcript can redraw the card with what was
        // decided instead of asking again on every reload.
        if call.name == crate::web::clarify::TOOL {
            let Ok(request) = crate::web::clarify::Request::parse(&call.arguments) else {
                return crate::tools::ToolResult::refused(
                    "that question could not be read: each question needs a question and at \
                     least one choice, and one call asks at most five questions of at most \
                     four choices each",
                );
            };
            // No registry means no surface can answer, so asking would block the
            // turn for an hour and then tell the model nobody was there. Say so
            // immediately instead, which is a tool result the model can act on.
            let Some(ask) = ask.as_ref() else {
                return crate::tools::ToolResult::refused(
                    "this turn has no way to ask the operator a question, so nothing was \
                     asked. Ask in your answer instead and end the turn.",
                );
            };
            let gate = ask.registry.park_for(&ask.conversation, request.clone());
            let stopped = gate.wait(crate::web::clarify::DEFAULT_TIMEOUT, || match poll() {
                TurnSignal::Stop => crate::web::clarify::WaitPoll::Stop,
                _ => crate::web::clarify::WaitPoll::Continue,
            });
            let Some(answer) = stopped else {
                return crate::tools::ToolResult::refused(
                    "the operator stopped the turn while the question was open, so it was \
                     never answered and nothing was done with it",
                );
            };
            asked(Asked {
                request,
                answer: Some(answer.clone()),
            });
            return crate::tools::ToolResult {
                content: answer.render(),
                is_error: false,
            };
        }
        if crate::mcp::protocol::split_qualified(&call.name).is_some() {
            let Some(pool) = mcp else {
                return crate::tools::ToolResult::refused(
                    "no MCP server is configured, so this tool cannot run",
                );
            };
            return match pool.call(&call.name, &call.arguments) {
                Ok(outcome) => crate::tools::ToolResult::from_mcp(&call.name, &outcome),
                Err(error) => crate::tools::ToolResult::refused(&error.message),
            };
        }
        // Drawing is not a local capability: with the image role pinned the
        // request goes to its provider, and the file lands in the workspace so
        // the operator can open it.
        if call.name == "generate_image"
            && let Some(config) = image
            && let Some(prompt) = call
                .arguments
                .get("prompt")
                .and_then(serde_json::Value::as_str)
        {
            let Some(bytes) = generate_image(config, prompt) else {
                return crate::tools::ToolResult::refused(
                    "the image role could not draw that, so nothing was written",
                );
            };
            let stamp = uuid::Uuid::now_v7().to_string();
            let name = format!(
                "generated/{}",
                image_slug(prompt, &stamp, image_extension(&bytes))
            );
            let Ok(path) = crate::tools::resolve(sandbox.workspace(), &name) else {
                return crate::tools::ToolResult::refused("the image path escaped the workspace");
            };
            if let Some(parent) = path.parent()
                && std::fs::create_dir_all(parent).is_err()
            {
                return crate::tools::ToolResult::refused("cannot create the image directory");
            }
            if std::fs::write(&path, &bytes).is_err() {
                return crate::tools::ToolResult::refused("cannot write the image");
            }
            return crate::tools::ToolResult::ok(serde_json::json!({
                "path": name,
                "bytes": bytes.len(),
                // Ready to copy: the transcript draws this line as the picture,
                // so the operator sees what was drawn rather than a path.
                "display": format!("![generated image]({name})"),
            }));
        }

        // Dispatching is not a sandbox tool: it writes a row and starts a child
        // turn, so it is answered here, before the sandbox layer, exactly as
        // drawing is. The refusal is explicit rather than silent, because a call
        // the model made and nothing ran is a fact it has to be able to report.
        if call.name == crate::subagents::TOOL {
            let Some(desk) = agents.filter(|_| settings.agents) else {
                return crate::tools::ToolResult::refused(
                    "dispatching is not available in this turn: it needs the operator's \
                     switch on and the Full sandbox mode, so nothing was dispatched",
                );
            };
            let argument = |key: &str| {
                call.arguments
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            let prompt = argument("prompt");
            let label = argument("label");
            return match desk.dispatch(&label, &prompt) {
                Ok(id) => crate::tools::ToolResult::ok(serde_json::json!({
                    "status": "dispatched",
                    "id": id,
                    "label": crate::subagents::label_for(&label, &prompt),
                    "model": desk.model(),
                    "note": "The subagent is running in the background with no tools. \
                             Its result appears in the subagents control in the top bar; \
                             do not wait for it in this turn.",
                })),
                Err(error) => crate::tools::ToolResult {
                    content: serde_json::json!({
                        "status": "refused",
                        "code": error.code(),
                        "reason": error.reason(),
                    })
                    .to_string(),
                    is_error: true,
                },
            };
        }

        // A skill is the operator's own file, read out of their library. It is
        // answered here rather than in the sandbox because the switches that
        // decide which skills are on live in the database, which the sandbox does
        // not hold. Not offered means not answerable: a call for a skill on a turn
        // that was never given access is refused explicitly.
        if call.name == crate::skills::TOOL {
            let Some(access) = skills else {
                return crate::tools::ToolResult::refused(
                    "the skill library is not available to this turn, so nothing was read",
                );
            };
            return access.call(&call.arguments);
        }

        let result = crate::tools::execute_streaming(&call.name, &call.arguments, sandbox, chunks);
        // A fetched page is a wall of navigation until something reads it. With
        // the role pinned, its model reads it; without one the tool's own text
        // stands, which is exactly what the tool was doing before the role
        // existed.
        if call.name == "fetch_url"
            && !result.is_error
            && let Some(config) = web_extract
            && let Ok(payload) = serde_json::from_str::<serde_json::Value>(&result.content)
            && let Some(text) = payload.get("text").and_then(serde_json::Value::as_str)
            && let Some(extracted) = extract_readable(config, text)
        {
            return crate::tools::ToolResult::ok(serde_json::json!({
                "url": payload.get("url").cloned().unwrap_or(serde_json::Value::Null),
                "extracted": extracted,
            }));
        }
        result
    };

    let outcome = drive_tool_loop(
        &mut execute,
        messages,
        gate,
        settings.max_rounds,
        settings.context_budget,
        sink,
        poll,
        |round, tools_allowed, emit| {
            let mut forward = |event: ProviderEvent| match event {
                ProviderEvent::Text(fragment) if !fragment.is_empty() => {
                    emit(StreamPart::Text(&fragment));
                }
                ProviderEvent::Reasoning(fragment) if !fragment.is_empty() => {
                    emit(StreamPart::Reasoning(&fragment));
                }
                _ => {}
            };
            let request = ChatRequest {
                system: system.clone(),
                messages: round.to_vec(),
                // Offered on every round that may still call one: the model needs
                // them again to answer the result it just received. Withheld on the
                // last round so the turn ends in an answer instead of a refusal.
                tools: if tools_allowed && settings.tools {
                    let mut offered = crate::tools::specs_offering(sandbox.mode(), settings.agents);
                    if let Some(pool) = mcp {
                        offered.extend(pool.specs());
                    }
                    // Offered only when this turn has a library access at all, on
                    // the same rule: a tool the daemon will refuse must never reach
                    // the model's list.
                    if skills.is_some() {
                        offered.push(crate::skills::spec());
                    }
                    // Offered only when a surface can answer it: a turn with no
                    // registry has no way to hear back, and the tool refuses
                    // itself anyway, so listing it would cost a round trip to
                    // learn what the list could have said.
                    if ask.is_some() {
                        offered.push(crate::web::clarify::spec());
                    }
                    // Only offered when the role is pinned: a tool the daemon
                    // will refuse must never reach the model's list.
                    if image.is_some() {
                        offered.push(core_runtime::provider::ToolSpec {
                            name: "generate_image".to_owned(),
                            description:
                                "Draw an image from a description and save it in the workspace. \
                                 The result carries a display line; put it on a line of its own \
                                 in your answer so the operator sees the picture."
                                    .to_owned(),
                            parameters: serde_json::json!({
                                "type": "object",
                                "properties": {
                                    "prompt": {
                                        "type": "string",
                                        "description": "What to draw."
                                    }
                                },
                                "required": ["prompt"]
                            }),
                        });
                    }
                    offered
                } else {
                    Vec::new()
                },
                ..ChatRequest::default()
            };
            runtime
                .block_on(provider.stream_chat(&request, key, &mut forward))
                .map_err(|error| (error.kind().to_owned(), error.to_string()))
        },
    )?;

    Ok(outcome)
}

/// What a conversation has cost, for the header chips and the status strip.
///
/// Every number here is a sum of numbers a provider actually reported. A total
/// over an unreported row is unreported, not zero, and a percentage with no
/// denominator is `None` rather than a confident-looking 0.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    /// Questions asked.
    pub turns: usize,
    /// Answers that settled.
    pub answers: usize,
    /// Prompt tokens reported across the transcript.
    pub prompt_tokens: u64,
    /// Completion tokens reported across the transcript.
    pub completion_tokens: u64,
    /// Prompt tokens served from cache, where a provider said so.
    pub cached_tokens: u64,
    /// Prompt plus completion, as reported.
    pub total_tokens: u64,
    /// Cached share of the prompt, when there is a denominator.
    pub cache_hit_pct: Option<u8>,
    /// The newest answer's prompt size, which is the live context size.
    pub context_used: Option<u64>,
    /// The window declared for the model that answered it.
    pub context_window: Option<u64>,
    /// How full that window is.
    pub context_pct: Option<u8>,
    /// Completion tokens per second of streaming, averaged over answers that
    /// reported a duration.
    pub tokens_per_second: Option<u32>,
}

/// Total a transcript into [`Stats`].
#[must_use]
pub fn stats(transcript: &[Message]) -> Stats {
    let mut out = Stats::default();
    let mut timed_ms: u64 = 0;
    let mut timed_tokens: u64 = 0;

    for message in transcript {
        match message.role {
            Role::User => out.turns += 1,
            Role::Assistant => {
                if message.state == State::Complete {
                    out.answers += 1;
                }
                out.prompt_tokens += message.prompt_tokens.unwrap_or(0);
                out.completion_tokens += message.completion_tokens.unwrap_or(0);
                out.cached_tokens += message.cached_tokens.unwrap_or(0);
                if let (Some(tokens), Some(ms)) = (message.completion_tokens, message.duration_ms)
                    && ms > 0
                {
                    timed_tokens += tokens;
                    timed_ms += ms;
                }
            }
        }
    }

    out.total_tokens = out.prompt_tokens + out.completion_tokens;
    let prompt_total = out.prompt_tokens + out.cached_tokens;
    out.cache_hit_pct = (out.cached_tokens * 100)
        .checked_div(prompt_total)
        .map(|pct| pct as u8);
    // The two numbers are resolved independently, newest row first. A failed
    // round trip carries the window it was sent to but no token count, and a row
    // written before `context_tokens` existed carries neither: stopping at the
    // first row that has either one let a single failed answer blank out the
    // good number the transcript already had. Taking the newest row that has
    // each figure separately means the footer says "unknown" only when the
    // whole transcript is unknown, not when the last attempt failed.
    out.context_used = transcript
        .iter()
        .rev()
        .find_map(|message| message.context_tokens);
    out.context_window = transcript
        .iter()
        .rev()
        .find_map(|message| message.context_window);
    if let (Some(used), Some(window)) = (out.context_used, out.context_window) {
        out.context_pct = (used * 100)
            .checked_div(window)
            .map(|pct| pct.min(100) as u8);
    }
    out.tokens_per_second = (timed_tokens * 1000)
        .checked_div(timed_ms)
        .map(|rate| rate as u32);
    out
}

/// The number of rows a conversation holds, for a page that wants a count without
/// loading the transcript twice.
#[must_use]
pub fn has_messages(transcript: &[Message]) -> bool {
    transcript
        .iter()
        .any(|message| !message.body.trim().is_empty())
}

/// The most provider round trips one turn may take.
///
/// A model that answers with another tool call every time would otherwise loop
/// for as long as the operator leaves the tab open. At the cap the turn stops and
/// says so; it does not silently return nothing.
pub const MAX_TOOL_ROUNDS: usize = 12;

/// What a turn produced once the tool loop settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnOutcome {
    /// The reply. A capped turn carries the notice instead of an empty string.
    pub text: String,
    /// The model's own reasoning, when the provider streamed any.
    pub reasoning: String,
    /// Usage summed over every round trip, never just the last one.
    pub usage: core_domain::Usage,
    /// How many tool rounds ran.
    pub rounds: usize,
    /// What the tools actually did, in order, for the transcript.
    pub tools: Vec<ToolRecord>,
    /// The prompt size of the final round trip: the live context size.
    ///
    /// Not the summed `usage`: that is what the turn cost, and a turn that called
    /// a tool ten times spent ten prompts. This is the one number that answers
    /// "how full is the window".
    pub context_tokens: Option<u64>,
    /// Whether the tool budget, rather than the model, ended this turn.
    ///
    /// Reported so a caller can say so rather than leave the operator to guess
    /// from a sentence. A capped turn still carries a usable [`Self::text`]: the
    /// forced answer or a summary of what ran.
    pub capped: bool,
    /// Whether the engine took the tools away early on purpose and the model
    /// wrote the answer itself.
    ///
    /// A different fact from [`Self::capped`], not a shade of it. `capped` says
    /// the budget decided when the turn ended; `wrapped_up` says the turn saw the
    /// budget coming and spent its last rounds answering instead of reaching for
    /// one more tool. A turn that wrapped up and still produced no prose falls
    /// back to the forced answer, and *that* turn is capped, because the answer
    /// was forced rather than written.
    pub wrapped_up: bool,
}

/// What happens to a call before it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDecision {
    /// Run it now.
    Run,
    /// Park it for a human. The model is told it is waiting.
    Park {
        /// The approval row this call is waiting on.
        approval_id: String,
    },
    /// Refuse it outright, without running it.
    ///
    /// The fallback when the call cannot even be parked - a queue that cannot be
    /// written is a reason to do nothing, never a reason to go ahead unapproved.
    Refuse {
        /// Why it was refused, for the model and the operator.
        reason: String,
    },
}

/// The tool executor a turn drives: one call, and a sink for the output it
/// streams while it runs.
///
/// Named rather than spelled inline because the closure takes a closure, which
/// is exactly the nested type a reader needs a name for - and a signature clippy
/// rightly calls complex.
pub type ToolExecute<'a> = dyn FnMut(
        &core_domain::ToolCall,
        &mut dyn FnMut(&str),
        &mut dyn FnMut(Asked),
        &mut TurnPoll<'_>,
    ) -> crate::tools::ToolResult
    + 'a;

/// Drive one turn: stream, run whatever the model asked for, stream again.
///
/// `stream` is handed the prompt for each round trip and returns that round's
/// completion. Text reaches the operator through whatever `stream` does with it;
/// this function only decides when to stop and what was spent.
///
/// `gate` is consulted before every call runs and is the only place a turn can
/// be stopped from touching the filesystem. It is called even for read-only
/// tools so a policy can decide for itself; the shipped policy parks writes.
///
/// `poll` is consulted at three boundaries: before each provider round trip,
/// between the tool calls of a round, and once more after the last round trip
/// returns. That third one is what makes a steer land on a one-round answer,
/// which is most answers: a turn makes one long round trip, and a steer typed
/// while it streamed would otherwise arrive after there was nothing left to
/// steer.
///
/// These are boundaries, not interruptions. A provider stream already in flight
/// runs to its end - `stream_chat` is a single blocking call with no seam inside
/// it - so a stop takes effect when the current call returns: no further round
/// trip, no tool calls after the one that is running, and no answer written. No
/// socket is closed, and this function does not claim one is.
// Eight arguments, and every one of them is a distinct thing the loop cannot
// infer: what to run, what it has said so far, what may run, how many rounds, how
// big the window is, where to report, how to be stopped, and how to ask for the
// next completion. Bundling them into a struct would move the same eight fields
// one line up and make the call sites read worse.
#[allow(clippy::too_many_arguments)]
pub fn drive_tool_loop(
    execute: &mut ToolExecute<'_>,
    mut messages: Vec<ChatMessage>,
    gate: &mut dyn FnMut(&core_domain::ToolCall) -> GateDecision,
    max_rounds: usize,
    budget: Option<u64>,
    sink: &mut dyn FnMut(StreamPart<'_>),
    poll: &mut TurnPoll<'_>,
    mut stream: impl FnMut(
        &[ChatMessage],
        bool,
        &mut dyn FnMut(StreamPart<'_>),
    ) -> Result<core_runtime::provider::Completion, TurnFailure>,
) -> Result<TurnOutcome, TurnFailure> {
    let max_rounds = max_rounds.max(1);
    let mut usage: Option<core_domain::Usage> = None;
    let mut context_tokens: Option<u64> = None;
    let mut reasoning = String::new();
    let mut records: Vec<ToolRecord> = Vec::new();
    let mut rounds = 0usize;
    // Whether the tool-free round has already been introduced. Pushed once, so a
    // steer arrives as the operator's own words rather than behind two notices.
    let mut budget_told = false;
    // Whether the model has been told the budget is nearly gone. Separate from
    // `budget_told` because it is a different sentence: the wrap-up is the last
    // chance to write, and the spent notice is the fact that the tools are gone.
    let mut wrap_told = false;
    // Whether the turn ended because the budget ran out rather than because the
    // model was finished. Reported, not inferred from the text. Every break below
    // assigns it, which is why it has no default: a third way out would have to
    // say what it means.
    let mut capped: bool;
    // Whether the tools were taken away early on purpose and the model answered
    // with what it had. Assigned at every break too, for the same reason: a new
    // way out has to decide which of the two facts it is.
    let mut wrapped_up: bool;

    let mut text = loop {
        // The first boundary: nothing has been sent for this round yet, so a stop
        // here costs nothing and a steer joins the prompt before it is billed.
        match poll() {
            TurnSignal::Stop => return Err(stopped()),
            TurnSignal::Steer(instruction) => {
                messages.push(ChatMessage::text(ChatRole::User, instruction));
                continue;
            }
            TurnSignal::Continue => {}
        }

        // The last attempt is tool-free on purpose. A model that answered every
        // round with another call used to end the turn with nothing; instead it
        // now has to say what it found with what it already has, which is a
        // usable answer rather than a shrug.
        //
        // It is told that is what is happening. A model that is silently denied
        // its tools reaches for them anyway, and the markup it writes in that
        // state is not an answer: it is the tool call it could not make. That is
        // exactly how a long turn used to end on an empty bubble.
        // The lookahead. Two rounds before the budget ends, the turn stops
        // exploring: the tools are taken away and the remaining rounds are spent
        // getting a written answer out of what has already been found. A turn that
        // only learns the budget is gone *after* it is gone has nothing left but
        // the forced answer, which is the "max turns hit, I did not finish"
        // complaint this is for.
        //
        // Never fires on a budget of one or two rounds. There is no room to look
        // ahead in a turn with one round, and two rounds of wrapped-up prose is not
        // better than one round with the tools the operator gave it. Those budgets
        // behave exactly as they did before.
        let wrapping = max_rounds > 2 && rounds >= max_rounds.saturating_sub(2);
        let tools_allowed = rounds < max_rounds && !wrapping;
        // One coherent sentence, not two. When the wrap-up fires the model is told
        // to write; when the hard budget is spent and nothing was said yet it is
        // told the tools are gone. A turn that was already told to wrap up does not
        // then get the spent notice as well.
        if wrapping && !wrap_told {
            wrap_told = true;
            messages.push(ChatMessage::text(ChatRole::User, WRAP_UP));
        } else if !tools_allowed && !budget_told && !wrap_told {
            budget_told = true;
            messages.push(ChatMessage::text(ChatRole::User, BUDGET_SPENT));
        }
        // Said before the call, not after it. The model call is the longest
        // silence in a turn, and a page that hears nothing for two minutes cannot
        // tell work from death. The count is the round about to run, and whether
        // it may still use tools, so the closing round reads as what it is rather
        // than as the budget being spent twice.
        // The window is checked here, before every round trip, and not only at
        // the start of the turn. A turn that calls twelve tools accumulates twelve
        // results, and the size that matters is the one about to be sent rather
        // than the one that was sent first. Cutting a result is the only lever
        // available mid-turn: an aux call here would be a second silence the
        // operator did not ask for, on top of the one they are already waiting in.
        //
        // The cut is announced. A model answering from a truncated result and an
        // operator who does not know it is truncated is how a wrong answer looks
        // like a working one.
        if let Some(budget) = budget {
            let elided = crate::web::context::elide(&mut messages, budget);
            if elided > 0 {
                sink(StreamPart::Elided { results: elided });
            }
        }
        sink(StreamPart::Round {
            round: (rounds + 1).min(max_rounds),
            max_rounds,
            tools_allowed,
        });
        let mut completion = stream(&messages, tools_allowed, &mut *sink)?;

        // A route that fails to lift calls into the structured array passes the
        // model's own markup through as content. Recover it: the calls the model
        // decided to make become real calls, and the markup stops being the
        // answer.
        //
        // Only while tools are on offer. On the tool-free round those calls
        // cannot run, so adopting them would spend a round on nothing and leave
        // the same empty answer behind.
        if let Some(recovered) = crate::web::dsml::recover(&completion.text) {
            completion.text = recovered.text;
            if completion.tool_calls.is_empty() && tools_allowed {
                completion.tool_calls = recovered.calls;
            }
        }

        // The last round's own prompt size, not the sum: this is what the footer
        // calls the context, and a sum over rounds is not a window.
        context_tokens = round_context(&completion.usage).or(context_tokens);
        usage = Some(match usage {
            None => completion.usage,
            Some(total) => total.accrue(completion.usage),
        });
        // Reasoning accumulates across rounds: a turn that thought twice about
        // two tool calls has one train of thought, not the last leg of it.
        if !completion.reasoning.trim().is_empty() {
            if !reasoning.is_empty() {
                reasoning.push('\n');
            }
            reasoning.push_str(&completion.reasoning);
        }

        if completion.tool_calls.is_empty() {
            // The last boundary: the operator may have typed while that round was
            // streaming, and an answer is not final until nobody is still talking.
            // What the model already said is kept as its own turn, so the new
            // instruction is answered as a continuation rather than a restart, and
            // the round that produced it is not run again.
            match poll() {
                TurnSignal::Stop => return Err(stopped()),
                TurnSignal::Steer(instruction) => {
                    if !completion.text.trim().is_empty() {
                        messages.push(ChatMessage::text(ChatRole::Assistant, completion.text));
                    }
                    messages.push(ChatMessage::text(ChatRole::User, instruction));
                    continue;
                }
                TurnSignal::Continue => {}
            }
            // A boundary that produced nothing usable still spent the budget, so
            // the turn is capped even though the model, not a stray call, is what
            // left it empty. A turn that was told to wrap up and did write is not
            // capped: the answer is the model's own, arrived at early.
            let said = !completion.text.trim().is_empty();
            wrapped_up = wrap_told && said;
            capped = !tools_allowed && !wrapped_up;
            break completion.text;
        }

        // A provider that returns calls when it was offered none has nothing
        // left to run, so the loop ends with whatever it said. Whatever it said
        // may be nothing, which is why the turn does not end here.
        if !tools_allowed {
            // Reaching for a tool it was told not to use is not an answer, so this
            // turn is capped whichever notice it was given.
            capped = true;
            wrapped_up = false;
            break completion.text;
        }

        rounds += 1;
        messages.push(ChatMessage::assistant_calls(completion.tool_calls.clone()));
        for call in &completion.tool_calls {
            // A boundary between calls as well: a round that asked for five
            // commands stops before the second one rather than after the fifth.
            if matches!(poll(), TurnSignal::Stop) {
                return Err(stopped());
            }
            // One rendering of the arguments, shared by the start frame and the
            // record: two `to_string` calls of the same value would be the same
            // bytes twice, and the borrow has to outlive both frames anyway.
            let arguments = call.arguments.to_string();
            // Announced before it runs, not after: a status line that learns what
            // is happening only when it has already happened is a receipt, not a
            // status. Ahead of the gate, so a parked call is still stated, and
            // ahead of the writing closure below, because one sink cannot be lent
            // to two closures at once.
            sink(StreamPart::ToolStart {
                name: &call.name,
                arguments: &arguments,
            });
            // The card goes out before the turn blocks on it, and before `live`
            // borrows the sink: a question nobody can see is a turn that appears
            // to hang.
            let mut asked: Option<Asked> = None;
            if call.name == crate::web::clarify::TOOL
                && let Ok(request) = crate::web::clarify::Request::parse(&call.arguments)
            {
                sink(StreamPart::Asked { request: &request });
                asked = Some(Asked {
                    request,
                    answer: None,
                });
            }
            // A running call may write while it runs. The chunk goes out ahead of
            // the result, on the same sink, so a live turn and a reloaded
            // transcript tell the same story in the same order.
            let mut live = |chunk: &str| {
                sink(StreamPart::ToolOutput {
                    name: &call.name,
                    chunk,
                });
            };
            let result = match gate(call) {
                // The executor hands back what was answered, which is what makes
                // the transcript able to redraw a settled card.
                GateDecision::Run => {
                    execute(call, &mut live, &mut |payload| asked = Some(payload), poll)
                }
                GateDecision::Park { approval_id } => {
                    crate::tools::ToolResult::parked(&approval_id, &call.name)
                }
                GateDecision::Refuse { reason } => crate::tools::ToolResult::refused(&reason),
            };
            drop(live);
            let record = ToolRecord {
                name: call.name.clone(),
                arguments,
                output: result.content.clone(),
                is_error: result.is_error,
                asked,
            };
            // Shown as it happens: a turn that runs a command should not look
            // like a turn that is thinking very hard about nothing.
            sink(StreamPart::Tool {
                name: &record.name,
                arguments: &record.arguments,
                output: &record.output,
                is_error: record.is_error,
                asked: record.asked.as_ref(),
            });
            records.push(record);
            messages.push(ChatMessage::tool_result(call.id.clone(), result.content));
        }
    };

    // A budget that ran out mid-job is not an answer. The tool-free round above is
    // the turn's last chance to talk, and a model that spent it reaching for a tool
    // it no longer had wrote markup instead of prose. Ask once more, plainly, with
    // the record of what already ran in front of it, before falling back to a
    // summary of that record.
    if text.trim().is_empty() {
        if matches!(poll(), TurnSignal::Stop) {
            return Err(stopped());
        }
        let mut ask = messages.clone();
        ask.push(ChatMessage::text(ChatRole::User, ANSWER_NOW));
        if let Ok(completion) = stream(&ask, false, &mut *sink) {
            usage = Some(match usage {
                None => completion.usage,
                Some(total) => total.accrue(completion.usage),
            });
            if !completion.reasoning.trim().is_empty() {
                if !reasoning.is_empty() {
                    reasoning.push('\n');
                }
                reasoning.push_str(&completion.reasoning);
            }
            // Markup is still not an answer, even now: the calls inside it never
            // ran, so only the prose around it is reportable.
            let prose = crate::web::dsml::recover(&completion.text)
                .map_or(completion.text, |recovered| recovered.text);
            if !prose.trim().is_empty() {
                text = prose;
            }
        }
    }

    // Still nothing: say what the turn actually did. A summary of real work is an
    // answer; a shrug is not, and it is also why the next turn had no idea where
    // the last one had got to.
    if text.trim().is_empty() {
        text = capped_notice(&records, rounds, max_rounds);
        // A summary of what ran is not an answer the model wrote, so a turn that
        // wrapped up and then fell through here is capped. The tag says what
        // happened rather than what was hoped for.
        capped = true;
        wrapped_up = false;
    }

    Ok(TurnOutcome {
        text,
        reasoning,
        usage: usage.unwrap_or_else(|| {
            core_domain::Usage::unavailable(core_domain::UnavailableReason::FieldsAbsent)
        }),
        rounds,
        tools: records,
        context_tokens,
        capped,
        wrapped_up,
    })
}

/// The instruction the lookahead round opens with.
///
/// Pushed two rounds before the budget ends, and only once. It has to do three
/// things in one paragraph: say the budget is nearly gone so this reads as the
/// last chance rather than a hint, say what to do with what has already been
/// found, and say what not to do, because a model told only "the tools are gone"
/// writes the call it could not make and calls that an answer.
const WRAP_UP: &str = "Your tool budget for this turn is nearly spent and the tools are now \
     withdrawn. Stop exploring and answer now, in plain prose, with no tool calls: what you \
     found, what you changed, what is still unfinished, and the exact next step so the work can \
     be picked up from this message.";

/// The instruction the tool-free round opens with.
///
/// A model that is only denied tools still writes the call it wanted to make, and
/// on the wire that markup arrives as the answer. Telling it the budget is spent
/// is what turns "reaching for a tool I cannot have" into "say what I found".
const BUDGET_SPENT: &str = "Your tool budget for this turn is spent and no more tools are \
     available. Answer in plain prose now: what you found, what you changed, and what is still \
     unfinished. Do not request or write out any further tool calls.";

/// The instruction the last-resort answer round opens with.
///
/// Reached only when the tool-free round produced nothing usable, so this states
/// the output shape rather than relying on the model to infer it a second time.
const ANSWER_NOW: &str = "Answer in plain prose only, with no tool calls. Summarize what this \
     turn changed or found, name anything that is unfinished, and state the exact next step so \
     the work can be picked up from this message.";

/// What a turn says when its budget ran out and it produced no prose of its own.
///
/// Not "ask again": a message that only reports the stop tells the next turn
/// nothing about where this one got to, which is exactly why a resumed
/// conversation used to restart the job instead of finishing it. Naming the work,
/// the tools and the last result makes the transcript itself the resume point.
fn capped_notice(records: &[ToolRecord], rounds: usize, max_rounds: usize) -> String {
    let mut out = format!(
        "The tool budget ran out mid-job: {rounds} of {max_rounds} rounds spent, {} call(s) run.",
        records.len()
    );
    let mut names: Vec<(&str, usize)> = Vec::new();
    for record in records {
        match names.iter_mut().find(|(name, _)| *name == record.name) {
            Some((_, count)) => *count += 1,
            None => names.push((record.name.as_str(), 1)),
        }
    }
    if !names.is_empty() {
        let listed: Vec<String> = names
            .iter()
            .map(|(name, count)| match count {
                1 => (*name).to_owned(),
                many => format!("{name} x{many}"),
            })
            .collect();
        out.push_str("\nWhat ran: ");
        out.push_str(&listed.join(", "));
        out.push('.');
    }
    let failed = records.iter().filter(|record| record.is_error).count();
    if failed > 0 {
        out.push_str(&format!("\n{failed} of those came back as an error."));
    }
    if let Some(last) = records.last() {
        let excerpt = one_line_excerpt(&last.output, 200);
        if !excerpt.is_empty() {
            out.push_str(&format!("\nLast result from {}: {excerpt}", last.name));
        }
    }
    out.push_str(
        "\nNothing above was undone. Continue and I will pick up from here with a fresh budget.",
    );
    out
}

/// One line of a tool's output, cut to `limit` characters.
///
/// A tool result is often a whole file or a command's stderr, and a transcript
/// summary that quotes all of it is not a summary.
fn one_line_excerpt(text: &str, limit: usize) -> String {
    let flat: String = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let mut cut: String = flat.chars().take(limit).collect();
    cut.push('…');
    cut
}

/// The prompt size one round trip cost, cache reads included.
///
/// A provider that folds cache reads into its prompt total and one that reports
/// them beside it agree here, because [`core_domain::Usage::input_tokens`] has
/// already subtracted the folded ones.
#[must_use]
pub fn round_context(usage: &core_domain::Usage) -> Option<u64> {
    usage
        .input_tokens()
        .map(|input| input.saturating_add(usage.cache_read_tokens().unwrap_or(0)))
}

/// What a stopped turn returns.
///
/// A failure rather than an answer, because the answer is exactly what the
/// operator declined to wait for: the caller records the row as cancelled and the
/// partial text that had already streamed stays as the record of what they saw.
#[must_use]
pub fn stopped() -> TurnFailure {
    (
        "cancelled".to_owned(),
        "the operator stopped this turn before it finished".to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Db {
        Db::open_in_memory().expect("in-memory database")
    }

    #[test]
    fn a_transcript_round_trips_in_order() {
        let db = db();
        let first = append(&db, CONVERSATION_ID, Role::User, "hello", State::Complete).unwrap();
        let second = append(&db, CONVERSATION_ID, Role::Assistant, "hi", State::Complete).unwrap();
        let third = append(&db, CONVERSATION_ID, Role::User, "again", State::Complete).unwrap();

        let transcript = load(&db, CONVERSATION_ID).unwrap();
        let ids: Vec<&str> = transcript
            .iter()
            .map(|message| message.id.as_str())
            .collect();
        assert_eq!(ids, vec![first.as_str(), second.as_str(), third.as_str()]);
        assert_eq!(transcript[1].role, Role::Assistant);
        assert_eq!(transcript[1].body, "hi");
    }

    /// An unclaimed attachment row is litter only once it is old. The sweep is
    /// what clears the litter an earlier build left, and it must not touch a row
    /// that is merely between being written and being claimed, nor one that a
    /// message already owns.
    #[test]
    fn the_sweep_removes_old_unclaimed_attachments_and_their_files() {
        let db = db();
        let dir = tempfile::tempdir().expect("a scratch directory");
        let old_file = dir.path().join("old.txt");
        let fresh_file = dir.path().join("fresh.txt");
        let claimed_file = dir.path().join("claimed.txt");
        for path in [&old_file, &fresh_file, &claimed_file] {
            std::fs::write(path, b"bytes").expect("a file to sweep");
        }
        // One row from an hour and a half ago with no message: the litter a
        // refused send used to leave.
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO chat_attachments
                     (id, conversation_id, name, path, size, created_at)
                 VALUES ('old', ?1, 'old.txt', ?2, 5,
                         strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-5400 seconds'))",
                params![CONVERSATION_ID, old_file.display().to_string()],
            )?;
            Ok(())
        })
        .unwrap();
        // One written just now, in the middle of its own send.
        record_attachment(
            &db,
            CONVERSATION_ID,
            &Attachment {
                id: "fresh".to_owned(),
                name: "fresh.txt".to_owned(),
                path: fresh_file.display().to_string(),
                size: 5,
                media_type: Some("text/plain".to_owned()),
            },
        )
        .unwrap();
        // One a message already owns, old enough that only the claim saves it.
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO chat_attachments
                     (id, conversation_id, message_id, name, path, size, created_at)
                 VALUES ('claimed', ?1, 'some-message', 'claimed.txt', ?2, 5,
                         strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-5400 seconds'))",
                params![CONVERSATION_ID, claimed_file.display().to_string()],
            )?;
            Ok(())
        })
        .unwrap();

        let removed = sweep_orphan_attachments(&db, ORPHAN_ATTACHMENT_GRACE_SECS).unwrap();
        assert_eq!(removed, 1, "only the old, unclaimed row is litter");
        assert!(!old_file.exists(), "the bytes went with the row");
        assert!(fresh_file.exists(), "a row in its own send is left alone");
        assert!(claimed_file.exists(), "a claimed row is never swept");
        let left: Vec<String> = db
            .with_conn(|conn| {
                let mut statement = conn.prepare("SELECT id FROM chat_attachments ORDER BY id")?;
                let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
                let mut ids = Vec::new();
                for row in rows {
                    ids.push(row?);
                }
                Ok(ids)
            })
            .unwrap();
        assert_eq!(left, vec!["claimed".to_owned(), "fresh".to_owned()]);
    }

    #[test]
    fn progress_writes_a_partial_answer_and_leaves_a_settled_row_alone() {
        let db = db();
        let answer = append_answer(&db, CONVERSATION_ID, "m/one", None).unwrap();
        let tools = [ToolRecord {
            name: "run_command".to_owned(),
            arguments: r#"{"command":"echo hi"}"#.to_owned(),
            output: "hi".to_owned(),
            is_error: false,
            asked: None,
        }];

        progress(&db, &answer, "half", &tools, 3).unwrap();
        let rows = load(&db, CONVERSATION_ID).unwrap();
        let row = rows.iter().find(|row| row.id == answer).expect("the row");
        assert_eq!(row.body, "half");
        assert_eq!(row.rounds, Some(3));
        assert_eq!(row.tools, tools);
        assert_eq!(row.state, State::Streaming);

        // A settled row keeps the operator's final answer: a late progress flush
        // from a turn that already finished must not resurrect a partial body.
        settle(
            &db,
            &answer,
            "final",
            None,
            &tools,
            None,
            Accounting {
                rounds: Some(3),
                ..Accounting::default()
            },
        )
        .unwrap();
        progress(&db, &answer, "stale partial", &tools, 9).unwrap();
        let rows = load(&db, CONVERSATION_ID).unwrap();
        let row = rows.iter().find(|row| row.id == answer).expect("the row");
        assert_eq!(row.body, "final");
        assert_eq!(row.rounds, Some(3), "progress must not touch a settled row");
        assert_eq!(row.state, State::Complete);
    }

    #[test]
    fn the_run_log_has_three_modes_and_the_old_boolean_still_maps() {
        assert_eq!(tool_log(None), ToolLog::Full);
        assert_eq!(
            tool_log(Some("true")),
            ToolLog::Full,
            "the old on means full"
        );
        assert_eq!(tool_log(Some("summary")), ToolLog::Summary);
        assert_eq!(tool_log(Some("off")), ToolLog::Off);
        assert_eq!(
            tool_log(Some("false")),
            ToolLog::Off,
            "the old off means off"
        );
        assert_eq!(
            tool_log(Some("nonsense")),
            ToolLog::Full,
            "junk cannot hide it"
        );
        assert!(!ToolLog::Off.any());
        assert!(ToolLog::Summary.any());
    }

    #[test]
    fn the_run_log_is_on_unless_it_says_otherwise() {
        assert!(show_tool_log(None), "a fresh install shows what it ran");
        assert!(show_tool_log(Some("true")));
        assert!(show_tool_log(Some("garbage")), "a corrupt row keeps it on");
        assert!(!show_tool_log(Some("false")));
    }

    #[test]
    fn an_image_attachment_is_recognised_and_a_text_file_is_not() {
        let png = Attachment {
            id: "a".to_owned(),
            name: "shot.png".to_owned(),
            path: "/tmp/shot.png".to_owned(),
            size: 10,
            media_type: Some("image/png".to_owned()),
        };
        let named_jpg = Attachment {
            media_type: None,
            name: "photo.JPG".to_owned(),
            ..png.clone()
        };
        let notes = Attachment {
            name: "notes.txt".to_owned(),
            media_type: Some("text/plain".to_owned()),
            ..png.clone()
        };
        assert!(is_image(&png));
        assert!(is_image(&named_jpg), "a missing media type still counts");
        let heic = Attachment {
            name: "IMG_0060.HEIC".to_owned(),
            media_type: Some("image/heic".to_owned()),
            ..png.clone()
        };
        assert!(is_image(&heic), "an iPhone photo is a picture");
        assert!(!is_image(&notes));
    }

    #[test]
    fn a_generated_image_is_named_from_its_prompt_and_cannot_escape() {
        let name = image_slug("A red square, on white", "stamp", "png");
        assert_eq!(name, "stamp-a-red-square-on-white.png");

        let hostile = image_slug("../../etc/passwd", "stamp", "jpg");
        assert!(!hostile.contains('/'), "no separators survive: {hostile}");
        assert!(!hostile.contains(".."), "no traversal survives: {hostile}");

        let empty = image_slug("   ", "stamp", "png");
        assert_eq!(
            empty, "stamp-image.png",
            "a blank prompt still names a file"
        );

        let long = image_slug(&"word ".repeat(40), "stamp", "png");
        assert!(long.len() < 80, "the stem is bounded: {long}");
    }

    #[test]
    fn an_images_extension_comes_from_its_bytes_not_from_hope() {
        assert_eq!(image_extension(&[0x89, b'P', b'N', b'G', 0x0D]), "png");
        assert_eq!(image_extension(&[0xFF, 0xD8, 0xFF, 0xE0]), "jpg");
        assert_eq!(
            image_extension(b"RIFF____WEBPVP8 "),
            "webp",
            "a webp announces itself at both ends"
        );
        assert_eq!(image_extension(b"GIF89a"), "gif");
        assert_eq!(image_extension(b"not an image"), "bin");
    }

    #[test]
    fn a_served_type_comes_from_the_bytes_and_never_from_the_name() {
        assert_eq!(
            sniff_media_type(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0]),
            "image/png"
        );
        assert_eq!(sniff_media_type(&[0xff, 0xd8, 0xff, 0xe0]), "image/jpeg");
        assert_eq!(
            sniff_media_type(b"RIFF____WEBPVP8 "),
            "image/webp",
            "a webp announces itself at both ends"
        );
        for gif in [b"GIF87a".as_slice(), b"GIF89a".as_slice()] {
            assert_eq!(sniff_media_type(gif), "image/gif");
        }
        assert_eq!(
            sniff_media_type(b"\x00\x00\x00\x18ftypheic\x00\x00\x00\x00"),
            "image/heic",
            "an ISO-BMFF HEIC is a picture, not a download"
        );
        assert_eq!(sniff_media_type(b"%PDF-1.7\n"), "application/pdf");
        assert_eq!(sniff_media_type(b"line one\nline two\n"), "text/plain");
        assert_eq!(
            sniff_media_type("h\u{e9}llo, w\u{f6}rld".as_bytes()),
            "text/plain",
            "utf-8 that is not ascii is still text"
        );
    }

    #[test]
    fn a_type_nothing_recognises_is_an_octet_stream_rather_than_the_name() {
        // An HTML page with a `.png` name would be rendered in this origin by a
        // browser that believed the name, so the name is not consulted at all.
        assert_eq!(
            sniff_media_type(b"<!doctype html><script>alert(1)</script>"),
            "text/plain",
            "html is text, and text is never served as html"
        );
        assert_eq!(
            sniff_media_type(&[0x50, 0x4b, 0x03, 0x04, 0x00, 0x00]),
            "application/octet-stream",
            "a zip has a NUL in its header, so it is not text"
        );
        assert_eq!(sniff_media_type(&[]), "application/octet-stream");
        assert_eq!(
            sniff_media_type(&[0x00, 0x01, 0x02]),
            "application/octet-stream",
            "an empty or NUL-led sample is never text"
        );
    }

    #[test]
    fn a_utf8_sequence_cut_off_by_the_read_window_is_still_text() {
        // The sample ends mid-character, which is a fact about the window rather
        // than about the file, so the file is not called binary.
        let mut bytes = vec![b'a'; INLINE_TEXT_BYTES];
        bytes.truncate(INLINE_TEXT_BYTES - 1);
        bytes.extend_from_slice(&[0xe2, 0x82]);
        assert_eq!(sniff_media_type(&bytes), "text/plain");
        // A real invalid sequence is not a cut-off sequence.
        let mut broken = vec![b'a'; INLINE_TEXT_BYTES - 2];
        broken.extend_from_slice(&[0xe2, 0x28]);
        assert_eq!(sniff_media_type(&broken), "application/octet-stream");
    }

    #[test]
    fn a_round_whose_calls_arrived_as_text_still_runs_them() {
        // The provider handed back markup instead of a tool_calls array. The loop
        // must turn it into a real call, run it, and answer from the result.
        let mut offered = 0usize;
        let mut executed: Vec<String> = Vec::new();
        let outcome = drive_tool_loop(
            &mut |call: &core_domain::ToolCall, _chunks: &mut dyn FnMut(&str), _asked: &mut dyn FnMut(Asked), _poll: &mut TurnPoll<'_>| {
                executed.push(call.name.clone());
                crate::tools::ToolResult::ok(serde_json::json!({ "rows": 3 }))
            },
            vec![ChatMessage::text(ChatRole::User, "check the warehouse")],
            &mut |_call| GateDecision::Run,
            MAX_TOOL_ROUNDS,
            None,
            &mut |_part| {},
            &mut || TurnSignal::Continue,
            |_messages, tools, _emit| {
                offered += 1;
                // Markup once, then an answer: a stub that always returns markup
                // would loop to the cap and prove nothing about recovery.
                if tools && offered == 1 {
                    Ok(completion_with_calls(
                        Vec::new(),
                        "<||DSML||calls>\
                         <||DSML||invoke name=\"run_command\">\
                         <||DSML||parameter name=\"command\" string=\"true\">ls db</||DSML||parameter>\
                         </||DSML||invoke></||DSML||calls>",
                    ))
                } else {
                    Ok(completion_with_calls(Vec::new(), "three rows"))
                }
            },
        )
        .expect("a turn");

        assert_eq!(offered, 2, "the markup bought another round");
        assert_eq!(
            executed,
            vec!["run_command".to_owned()],
            "the call really ran"
        );
        assert_eq!(outcome.text, "three rows");
        assert!(
            outcome
                .tools
                .iter()
                .all(|record| record.name == "run_command"),
            "the recovered call is in the run log: {:?}",
            outcome.tools.len()
        );
        assert!(
            !outcome.text.contains("DSML"),
            "markup never reaches the operator"
        );
    }

    /// A claim on a conversation, for the steering tests below.
    fn claimed(steering: &Steering) -> TurnLease {
        match steering.claim("c", "ignored") {
            Claim::Turn(lease) => lease,
            Claim::Steered(_) => panic!("the conversation was free"),
        }
    }

    #[test]
    fn a_second_message_queues_for_the_turn_that_holds_the_conversation() {
        let steering = Steering::new();
        let lease = claimed(&steering);
        assert!(steering.running("c"), "the turn holds the conversation");

        // A second message is not a second turn: it is queued for the first one.
        match steering.claim("c", "check the logs") {
            Claim::Steered(waiting) => assert_eq!(waiting, 1),
            Claim::Turn(_) => panic!("a running turn must not be joined by a second one"),
        }
        assert_eq!(steering.queued("c"), 1);

        // Handed over exactly once, oldest first.
        assert_eq!(
            lease.signal(),
            TurnSignal::Steer("check the logs".to_owned())
        );
        assert_eq!(lease.signal(), TurnSignal::Continue);
        assert_eq!(steering.queued("c"), 0, "a steer is delivered once");

        // Releasing the lease is what frees the conversation for the next turn.
        drop(lease);
        assert!(!steering.running("c"));
        assert_eq!(
            steering.queue("c", "too late"),
            None,
            "nothing is running, so there is nothing to steer"
        );
        assert!(matches!(steering.claim("c", "a turn"), Claim::Turn(_)));
    }

    #[test]
    fn a_stop_outranks_what_is_waiting_and_is_asked_only_once() {
        let steering = Steering::new();
        let lease = claimed(&steering);
        assert_eq!(steering.queue("c", "wait"), Some(1));
        assert!(steering.stop("c"));
        assert!(
            !steering.stop("c"),
            "the second request is not a second request"
        );
        assert_eq!(
            lease.signal(),
            TurnSignal::Stop,
            "the operator who asked to stop does not want a round run first"
        );
        assert_eq!(
            lease.queued(),
            1,
            "the queued message is still on record, not silently eaten"
        );
        assert_eq!(lease.drain(), vec!["wait".to_owned()]);
        assert_eq!(steering.queued("c"), 0);
    }

    #[test]
    fn a_steer_queued_while_the_answer_streams_is_answered_in_a_fresh_round() {
        // The ordinary shape of an answer: one round trip that is done. The
        // operator types while it streams, and the loop must append that text and
        // run exactly one more round, rather than dropping the message or starting
        // the turn over.
        let steering = Steering::new();
        let lease = claimed(&steering);
        let mut seen: Vec<Vec<ChatMessage>> = Vec::new();
        let mut calls = 0usize;
        let outcome = drive_tool_loop(
            &mut |_call: &core_domain::ToolCall,
                  _chunks: &mut dyn FnMut(&str),
                  _asked: &mut dyn FnMut(Asked),
                  _poll: &mut TurnPoll<'_>| {
                crate::tools::ToolResult::ok(serde_json::json!({}))
            },
            vec![ChatMessage::text(ChatRole::User, "start")],
            &mut |_call| GateDecision::Run,
            MAX_TOOL_ROUNDS,
            None,
            &mut |_part| {},
            &mut || lease.signal(),
            |messages, _tools, _emit| {
                seen.push(messages.to_vec());
                calls += 1;
                if calls == 1 {
                    // Typed while this round was streaming.
                    steering.queue("c", "check the logs");
                    Ok(completion_with_calls(Vec::new(), "the first answer"))
                } else {
                    Ok(completion_with_calls(Vec::new(), "the second answer"))
                }
            },
        )
        .expect("a turn");

        assert_eq!(calls, 2, "a steer buys one more round trip and no more");
        assert_eq!(outcome.text, "the second answer");
        let second = &seen[1];
        assert_eq!(second.len(), 3, "question, what was said, then the steer");
        assert_eq!(second[1].role, ChatRole::Assistant);
        assert_eq!(
            second[1].content, "the first answer",
            "the model keeps what it already said"
        );
        assert_eq!(second[2].role, ChatRole::User);
        assert_eq!(second[2].content, "check the logs");
        assert_eq!(steering.queued("c"), 0, "the steer was taken once");
    }

    #[test]
    fn a_steer_queued_before_a_tool_round_joins_the_next_round_trip() {
        // The other boundary: the model asked for a tool, and a steer arrived
        // while that call ran. It must join before the next provider call.
        let steering = Steering::new();
        let lease = claimed(&steering);
        let call = core_domain::ToolCall::new(0, "call_1", "read_file", serde_json::json!({}))
            .expect("a call");
        let mut second_round: Vec<ChatMessage> = Vec::new();
        let mut calls = 0usize;
        let outcome = drive_tool_loop(
            &mut |_call: &core_domain::ToolCall,
                  _chunks: &mut dyn FnMut(&str),
                  _asked: &mut dyn FnMut(Asked),
                  _poll: &mut TurnPoll<'_>| {
                steering.queue("c", "actually read b.txt");
                crate::tools::ToolResult::ok(serde_json::json!({ "rows": 1 }))
            },
            vec![ChatMessage::text(ChatRole::User, "read a.txt")],
            &mut |_call| GateDecision::Run,
            MAX_TOOL_ROUNDS,
            None,
            &mut |_part| {},
            &mut || lease.signal(),
            |messages, _tools, _emit| {
                calls += 1;
                if calls == 1 {
                    Ok(completion_with_calls(vec![call.clone()], ""))
                } else {
                    second_round = messages.to_vec();
                    Ok(completion_with_calls(Vec::new(), "read"))
                }
            },
        )
        .expect("a turn");

        assert_eq!(calls, 2);
        assert_eq!(outcome.text, "read");
        let last = second_round.last().expect("a last message");
        assert_eq!(last.role, ChatRole::User);
        assert_eq!(last.content, "actually read b.txt");
    }

    #[test]
    fn a_stop_ends_the_turn_at_its_next_boundary_without_writing_an_answer() {
        let steering = Steering::new();
        let lease = claimed(&steering);
        let mut calls = 0usize;
        let stopped = drive_tool_loop(
            &mut |_call: &core_domain::ToolCall,
                  _chunks: &mut dyn FnMut(&str),
                  _asked: &mut dyn FnMut(Asked),
                  _poll: &mut TurnPoll<'_>| {
                crate::tools::ToolResult::ok(serde_json::json!({}))
            },
            vec![ChatMessage::text(ChatRole::User, "count to a billion")],
            &mut |_call| GateDecision::Run,
            MAX_TOOL_ROUNDS,
            None,
            &mut |_part| {},
            &mut || lease.signal(),
            |_messages, _tools, _emit| {
                calls += 1;
                // Asked for while this round was streaming. The round finishes -
                // there is no seam inside it to interrupt - and nothing after it
                // runs.
                steering.stop("c");
                Ok(completion_with_calls(Vec::new(), "half an answer"))
            },
        )
        .expect_err("a stopped turn is not an answer");

        assert_eq!(calls, 1, "the round in flight is not repeated");
        assert_eq!(stopped.0, "cancelled");
        assert!(
            stopped.1.contains("stopped"),
            "the reason says what happened: {}",
            stopped.1
        );
    }

    #[test]
    fn a_stop_between_two_calls_of_one_round_skips_the_second_call() {
        let steering = Steering::new();
        let lease = claimed(&steering);
        let first = core_domain::ToolCall::new(0, "call_1", "read_file", serde_json::json!({}))
            .expect("a call");
        let second = core_domain::ToolCall::new(1, "call_2", "list_dir", serde_json::json!({}))
            .expect("a call");
        let mut executed: Vec<String> = Vec::new();
        let stopped = drive_tool_loop(
            &mut |call: &core_domain::ToolCall,
                  _chunks: &mut dyn FnMut(&str),
                  _asked: &mut dyn FnMut(Asked),
                  _poll: &mut TurnPoll<'_>| {
                executed.push(call.name.clone());
                // The operator stops the turn while the first call is running.
                steering.stop("c");
                crate::tools::ToolResult::ok(serde_json::json!({}))
            },
            vec![ChatMessage::text(ChatRole::User, "look at two things")],
            &mut |_call| GateDecision::Run,
            MAX_TOOL_ROUNDS,
            None,
            &mut |_part| {},
            &mut || lease.signal(),
            |_messages, _tools, _emit| {
                Ok(completion_with_calls(
                    vec![first.clone(), second.clone()],
                    "",
                ))
            },
        )
        .expect_err("a stopped turn is not an answer");

        assert_eq!(
            executed,
            vec!["read_file".to_owned()],
            "the second never ran"
        );
        assert_eq!(stopped.0, "cancelled");
    }

    #[test]
    fn a_suggested_title_is_reduced_to_one_plain_line() {
        // Quotes, a trailing full stop and a second line all go.
        assert_eq!(
            scrub_title("\"Warehouse data health\"."),
            "Warehouse data health"
        );
        assert_eq!(
            scrub_title("  Kill the rail  \nsecond line"),
            "Kill the rail"
        );
        assert_eq!(scrub_title("`backticked`"), "backticked");
        assert_eq!(
            scrub_title("Why is the build broken?"),
            "Why is the build broken"
        );
        assert_eq!(scrub_title(""), "");
    }

    #[test]
    fn a_suggested_title_is_bounded_like_a_truncated_one() {
        let long = "word ".repeat(40);
        let title = scrub_title(&long);
        assert_eq!(
            title.chars().count(),
            TITLE_LIMIT + 1,
            "cut plus its ellipsis"
        );
        assert!(title.ends_with('…'));
    }

    #[test]
    fn a_chain_of_thought_never_becomes_a_title() {
        // The shape MiniMax actually produced, inlined into `content` because the
        // registry entry never asked for the split. The thought was cut off by the
        // rail's own limit, so there is no closing tag and no title either.
        let inlined = "<think>We need answer only title max six words. Need infer \
                       likely subject from the question";
        assert_eq!(
            scrub_title(inlined),
            "",
            "half a thought is not a name: the truncation of the question stands"
        );
        assert!(!scrub_title(inlined).contains('<'));
    }

    #[test]
    fn a_closed_thought_is_dropped_and_the_title_after_it_kept() {
        assert_eq!(
            scrub_title(
                "<think>The user asked about the rail. Name it.</think>Warehouse data health"
            ),
            "Warehouse data health"
        );
        // A `thinking` tag and a leading blank line behave the same way.
        assert_eq!(
            scrub_title("<thinking>reason\n</thinking>\n\nKill the rail."),
            "Kill the rail"
        );
        // Only the thought, nothing after it: still no title rather than the thought.
        assert_eq!(scrub_title("<think>only a thought</think>"), "");
    }

    #[test]
    fn the_titles_the_live_database_held_scrub_to_nothing() {
        // The exact stored shape from `chat_conversations` on 2026-10-24, all
        // seven of them: a `<think` opening, no closing tag, and the thought cut
        // at the title limit with an ellipsis. The truncation is what makes this
        // shape different from the closed case above - the rail cut the thought
        // mid-word, so there was never a `</think>` to find.
        for stored in [
            "<think>We need output title max six words only. Need infer title for con\u{2026}",
            "<think>We need respond only title at most six words no punctuation. User\u{2026}",
            "<think>We need answer must only title max six words no punctuation expla\u{2026}",
            "<think>We need respond only title max six words no punctuation. Need inf\u{2026}",
            "<think>We need answer only title max six words. Need respond to user's Q\u{2026}",
            "<think>We need answer title max six words only. Need infer likely subjec\u{2026}",
            "<think>We need answer only title max six words, no punctuation. Need inf\u{2026}",
        ] {
            assert_eq!(
                scrub_title(stored),
                "",
                "an unclosed thought is not a name, however it was cut"
            );
        }
    }

    #[test]
    fn a_suggestion_that_scrubs_to_nothing_never_writes_an_empty_title() {
        let db = db();
        let question = "Sup homie?";
        let conversation = create_conversation(&db).unwrap();
        append(&db, &conversation, Role::User, question, State::Complete).unwrap();
        name_from_first_message(&db, &conversation, question).unwrap();
        let before = conversations(&db, 1).unwrap()[0].title.clone();
        assert_eq!(before, "Sup homie?");

        // A chain of thought that never ended scrubs to empty, so the write is
        // refused before it reaches SQL: `Ok(0)`, and the truncation of the
        // question stands. No fallback is invented, because the row already has
        // one from the turn that created it.
        for suggestion in [
            "<think>We need answer only title max six words. Need respond to user's Q\u{2026}",
            "",
            "   ",
        ] {
            assert_eq!(
                set_title(&db, &conversation, question, suggestion).unwrap(),
                0,
                "a suggestion with no name in it is refused, not stored"
            );
            assert_eq!(
                conversations(&db, 1).unwrap()[0].title,
                before,
                "the question's truncation stands instead of an empty title"
            );
        }
    }

    #[test]
    fn strip_think_keeps_the_text_around_the_thought() {
        assert_eq!(
            strip_think("before <think>hidden</think> after"),
            "before  after"
        );
        assert_eq!(strip_think("no tags here"), "no tags here");
        // A stray closing tag drops what came before it, which is the rest of a
        // thought whose opening frame was lost.
        assert_eq!(
            strip_think("half a thought</think>The answer"),
            "The answer"
        );
        // An unclosed tag swallows the rest: the answer never started.
        assert_eq!(strip_think("before <think>never ends"), "before ");
    }

    #[test]
    fn the_window_reports_exactly_what_it_drops() {
        let db = db();
        let conversation = create_conversation(&db).unwrap();
        // One more turn than the window holds.
        for index in 0..(HISTORY_MESSAGES + 2) {
            let role = if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            };
            append(
                &db,
                &conversation,
                role,
                &format!("message {index}"),
                State::Complete,
            )
            .unwrap();
        }
        let transcript = load(&db, &conversation).unwrap();
        let all = transcript.len();
        let kept = history(&transcript, "nobody");
        let lost = dropped(&transcript, "nobody");

        assert_eq!(kept.len(), HISTORY_MESSAGES, "the window is exactly full");
        assert_eq!(
            lost.len(),
            all - HISTORY_MESSAGES,
            "what is dropped is what the window excluded, no more and no less"
        );
        assert_eq!(lost[0].body, "message 0", "oldest first");
    }

    fn registry() -> Vec<crate::providers::Provider> {
        crate::providers::parse(
            r#"[{"id":"default","name":"9router","base_url":"http://127.0.0.1:20128/v1",
                 "key_ref":"env:NINEROUTER_API_KEY"},
                {"id":"commandcode","name":"Command Code",
                 "base_url":"https://api.commandcode.ai/provider/v1",
                 "key_ref":"env:COMMANDCODE_API_KEY"}]"#,
        )
        .expect("the fixture registry validates")
    }

    #[test]
    fn a_model_whose_first_segment_is_not_a_provider_stays_whole() {
        let registry = registry();
        // 9router spells its ids with slashes. Reading `cmc` as a provider would
        // re-point the install at nothing, so only a registry id counts.
        let choice = split_model_value("cmc/deepseek/deepseek-v4-flash", &registry, "default");
        assert_eq!(choice.provider_id, "default");
        assert_eq!(choice.model, "cmc/deepseek/deepseek-v4-flash");

        let chosen = split_model_value(
            "commandcode/deepseek/deepseek-v4-flash",
            &registry,
            "default",
        );
        assert_eq!(chosen.provider_id, "commandcode");
        assert_eq!(chosen.model, "deepseek/deepseek-v4-flash");
    }

    #[test]
    fn a_qualified_value_round_trips_through_the_picker_rendering() {
        let registry = registry();
        for value in [
            "default/cmc/deepseek/deepseek-v4-flash",
            "commandcode/claude-opus-5-5",
        ] {
            assert_eq!(
                qualify_model(value, &registry, "default"),
                value,
                "a value that already names a provider is left alone"
            );
        }
        assert_eq!(
            qualify_model("cmc/deepseek-v4-flash", &registry, "default"),
            "default/cmc/deepseek-v4-flash",
            "a bare pin is qualified with the provider the role runs on"
        );
        assert_eq!(qualify_model("primary", &registry, "default"), "primary");
        assert_eq!(qualify_model("", &registry, "default"), "");
    }

    #[test]
    fn a_qualified_choice_moves_the_turn_to_that_provider() {
        let (_dir, engine) = crate::onboarding::tests::fixture_for_chat();
        let mut config = TurnConfig {
            base_url: "http://127.0.0.1:20128/v1".to_owned(),
            key_ref: SecretRef::Env("NINEROUTER_API_KEY".to_owned()),
            model: "cmc/deepseek/deepseek-v4-flash".to_owned(),
            dialect: core_domain::ApiDialect::ChatCompletions,
            reasoning_split: false,
            direct: false,
            project: None,
        };
        apply_model(&mut config, &engine, "commandcode/claude-opus-5-5")
            .expect("the engine is readable");
        assert_eq!(config.base_url, "https://api.commandcode.ai/provider/v1");
        assert_eq!(config.model, "claude-opus-5-5");
        assert_eq!(
            config.key_ref,
            SecretRef::Env("COMMANDCODE_API_KEY".to_owned())
        );
    }

    #[test]
    fn a_bare_choice_keeps_the_turns_own_endpoint() {
        let (_dir, engine) = crate::onboarding::tests::fixture_for_chat();
        let mut config = TurnConfig {
            base_url: "https://api.commandcode.ai/provider/v1".to_owned(),
            key_ref: SecretRef::Env("COMMANDCODE_API_KEY".to_owned()),
            model: "deepseek/deepseek-v4-flash".to_owned(),
            dialect: core_domain::ApiDialect::ChatCompletions,
            reasoning_split: false,
            direct: false,
            project: None,
        };
        apply_model(&mut config, &engine, "other/model").expect("readable");
        assert_eq!(
            config.base_url, "https://api.commandcode.ai/provider/v1",
            "the composer means this model on the endpoint it is already talking to"
        );
        assert_eq!(config.model, "other/model");
    }

    #[test]
    fn a_short_conversation_drops_nothing_and_summarises_nothing() {
        let db = db();
        let conversation = create_conversation(&db).unwrap();
        append(&db, &conversation, Role::User, "hello", State::Complete).unwrap();
        let transcript = load(&db, &conversation).unwrap();

        assert!(dropped(&transcript, "nobody").is_empty());
        // With nothing dropped the role must not spend a request at all.
        let config = TurnConfig {
            base_url: "http://127.0.0.1:1/v1".to_owned(),
            key_ref: SecretRef::Env("KALLISTI_TEST_KEY".to_owned()),
            model: "m/one".to_owned(),
            dialect: core_domain::ApiDialect::ChatCompletions,
            reasoning_split: false,
            direct: false,
            project: None,
        };
        assert!(
            summarise(&config, &[]).is_none(),
            "an empty window costs nothing and calls nothing"
        );
    }

    #[test]
    fn a_rename_replaces_the_placeholder_and_the_truncation_but_nothing_else() {
        let db = db();
        let question = "Compare the disk usage of the workspace against the warehouse";

        // Placeholder: a suggestion lands.
        let first = create_conversation(&db).unwrap();
        assert_eq!(
            set_title(&db, &first, question, "Warehouse versus workspace").unwrap(),
            1
        );

        // The truncation the first turn wrote: a suggestion still lands, because
        // that is the state every real conversation is in by the time it settles.
        let second = create_conversation(&db).unwrap();
        append(&db, &second, Role::User, question, State::Complete).unwrap();
        name_from_first_message(&db, &second, question).unwrap();
        assert_eq!(
            set_title(&db, &second, question, "Warehouse versus workspace").unwrap(),
            1,
            "the truncated title is a placeholder too"
        );

        // A title that is neither is the operator's, and is left alone.
        assert_eq!(
            set_title(&db, &second, question, "Something else").unwrap(),
            0
        );
    }

    #[test]
    fn a_later_turn_renames_a_conversation_named_from_its_first_question() {
        let db = db();
        let first_question = "Compare the disk usage of the workspace against the warehouse";
        let later = "and what did the sweep find overnight";

        let conversation = create_conversation(&db).unwrap();
        append(
            &db,
            &conversation,
            Role::User,
            first_question,
            State::Complete,
        )
        .unwrap();
        name_from_first_message(&db, &conversation, first_question).unwrap();
        append(&db, &conversation, Role::User, later, State::Complete).unwrap();

        // The stored title is the first question's truncation, and the question
        // this turn asked truncates to something else entirely. Before the fix
        // this matched neither candidate, so a title was generated every turn and
        // every one of them was thrown away.
        assert_ne!(title_from(first_question), title_from(later));
        assert_eq!(
            set_title(&db, &conversation, later, "Overnight sweep findings").unwrap(),
            1,
            "a later turn can name a conversation that is still on its truncation"
        );
        assert_eq!(
            conversations(&db, 1).unwrap()[0].title,
            "Overnight sweep findings"
        );

        // And once it has a real name, a second suggestion does not replace it:
        // a three-word title from turn two is not improved by a different
        // three-word title from turn nine.
        assert_eq!(
            set_title(&db, &conversation, later, "Something else entirely").unwrap(),
            0,
            "a conversation with a real name keeps it"
        );
    }

    #[test]
    fn a_conversation_records_the_model_it_runs_on_and_clears_it() {
        let db = db();
        let id = create_conversation(&db).unwrap();
        // Split so the mixed-case model id never appears as one 32-character
        // literal: the source-level secret scanner reads a run that long as a
        // pasted bearer, and a model pin is not one.
        const MODEL: &str = concat!("commandcode/", "MiniMaxAI/", "MiniMax-M3");

        // Nothing recorded: a conversation that predates the column, or one that
        // never sent a turn. The page reads this as "use the configured default".
        assert_eq!(
            conversation(&db, &id).unwrap().unwrap().model,
            None,
            "a fresh conversation has no model recorded"
        );

        set_conversation_model(&db, &id, MODEL).unwrap();
        assert_eq!(
            conversation(&db, &id).unwrap().unwrap().model.as_deref(),
            Some(MODEL),
            "the model travels with the conversation, not with the settings page"
        );
        assert_eq!(
            conversations(&db, 1).unwrap()[0].model.as_deref(),
            Some(MODEL),
            "the rail list carries it too, so a resume needs one read"
        );

        // An empty value is "never recorded", not an empty model id.
        set_conversation_model(&db, &id, "   ").unwrap();
        assert_eq!(conversation(&db, &id).unwrap().unwrap().model, None);
    }

    #[test]
    fn the_first_question_is_the_first_user_row_even_after_more_turns() {
        let db = db();
        let id = create_conversation(&db).unwrap();
        assert_eq!(
            first_question(&db, &id).unwrap(),
            None,
            "a conversation nobody has spoken in has no first question"
        );

        append(&db, &id, Role::User, "one", State::Complete).unwrap();
        append(&db, &id, Role::Assistant, "reply", State::Complete).unwrap();
        append(&db, &id, Role::User, "two", State::Complete).unwrap();
        assert_eq!(
            first_question(&db, &id).unwrap().as_deref(),
            Some("one"),
            "an answer between two questions does not become the first question"
        );
    }

    #[test]
    fn a_role_on_the_primary_resolves_to_nothing_so_it_costs_nothing() {
        let (_dir, engine, _app, _cookie) = super::super::harness::ready_app();

        // Unset means inherit.
        assert!(
            aux_pinned_config(&engine, "title_generation")
                .expect("config")
                .is_none(),
            "an unpinned role must not spend a request"
        );

        engine
            .set_setting(&crate::aux::setting_key("title_generation"), "m/cheap")
            .unwrap();
        let pinned = aux_pinned_config(&engine, "title_generation")
            .expect("config")
            .expect("a pinned role resolves");
        assert_eq!(pinned.model, "m/cheap");
        assert_eq!(
            pinned.base_url,
            engine
                .setting(SETTING_PROVIDER_BASE_URL)
                .unwrap()
                .unwrap_or_default(),
            "an unpinned provider follows the primary endpoint"
        );
    }

    #[test]
    fn approving_a_call_replaces_the_parked_record_with_what_it_returned() {
        let db = db();
        let conversation = create_conversation(&db).unwrap();
        let answer = append_answer(&db, &conversation, "m/one", None).unwrap();
        let parked = ToolRecord {
            name: "write_file".to_owned(),
            arguments: r#"{"path":"a.txt","content":"hi"}"#.to_owned(),
            output: serde_json::json!({
                "status": "pending_approval",
                "approval_id": "ap-1",
                "tool": "write_file",
            })
            .to_string(),
            is_error: false,
            asked: None,
        };
        let other = ToolRecord {
            name: "list_dir".to_owned(),
            arguments: "{}".to_owned(),
            output: r#"{"entries":[]}"#.to_owned(),
            is_error: false,
            asked: None,
        };
        settle(
            &db,
            &answer,
            "waiting on you",
            None,
            &[parked, other],
            None,
            Accounting::default(),
        )
        .unwrap();

        let changed = apply_approval_result(&db, "ap-1", r#"{"bytes":2}"#, false).expect("apply");
        assert_eq!(changed, 1);

        let transcript = load(&db, &conversation).unwrap();
        let records = &transcript[0].tools;
        assert_eq!(records.len(), 2, "the other call is left alone");
        assert_eq!(records[0].name, "write_file");
        assert!(
            records[0].output.contains(r#""status":"approved""#),
            "the record says it ran: {}",
            records[0].output
        );
        assert!(
            records[0].output.contains("ap-1"),
            "the decision stays traceable: {}",
            records[0].output
        );
        assert!(
            !records[0].output.contains("pending_approval"),
            "a parked row must not survive a decision"
        );
        assert!(
            records[1].output.contains("entries"),
            "an unrelated call is untouched"
        );
    }

    #[test]
    fn approving_an_unknown_call_changes_nothing_and_says_so() {
        let db = db();
        let conversation = create_conversation(&db).unwrap();
        let answer = append_answer(&db, &conversation, "m/one", None).unwrap();
        settle(
            &db,
            &answer,
            "done",
            None,
            &[ToolRecord {
                name: "list_dir".to_owned(),
                arguments: "{}".to_owned(),
                output: r#"{"entries":[]}"#.to_owned(),
                is_error: false,
                asked: None,
            }],
            None,
            Accounting::default(),
        )
        .unwrap();

        assert_eq!(
            apply_approval_result(&db, "nope", "x", false).expect("apply"),
            0,
            "no row mentions that approval, so nothing is rewritten"
        );
    }

    #[test]
    fn a_row_left_streaming_by_a_restart_is_failed_rather_than_left_pending() {
        let db = db();
        let orphan = append(&db, CONVERSATION_ID, Role::Assistant, "", State::Streaming).unwrap();
        let settled = append(
            &db,
            CONVERSATION_ID,
            Role::Assistant,
            "already done",
            State::Complete,
        )
        .unwrap();

        let swept = fail_orphaned(&db).expect("swept");
        assert_eq!(swept, 1, "one orphan, and the settled row is not touched");

        let transcript = load(&db, CONVERSATION_ID).unwrap();
        let orphan = transcript
            .iter()
            .find(|message| message.id == orphan)
            .unwrap();
        assert_eq!(orphan.state, State::Failed);
        assert_eq!(orphan.error_code.as_deref(), Some("interrupted"));
        let settled = transcript
            .iter()
            .find(|message| message.id == settled)
            .unwrap();
        assert_eq!(settled.state, State::Complete);
        assert_eq!(settled.error_code, None);
    }

    #[test]
    fn a_streaming_row_settles_with_its_usage_and_a_failure_keeps_its_text() {
        let db = db();
        let id = append(&db, CONVERSATION_ID, Role::Assistant, "", State::Streaming).unwrap();
        settle(
            &db,
            &id,
            "the answer",
            Some("thinking about it"),
            &[],
            Some(&serde_json::json!({"output": 3})),
            Accounting {
                prompt_tokens: Some(1_200),
                completion_tokens: Some(18),
                cached_tokens: Some(1_150),
                duration_ms: Some(900),
                rounds: None,
                context_tokens: Some(1_900),
                capped: None,
                wrapped_up: None,
            },
        )
        .unwrap();
        let transcript = load(&db, CONVERSATION_ID).unwrap();
        assert_eq!(transcript[0].state, State::Complete);
        assert_eq!(transcript[0].body, "the answer");
        assert_eq!(
            transcript[0].reasoning.as_deref(),
            Some("thinking about it"),
            "reasoning is stored beside the answer, never inside it"
        );
        assert_eq!(transcript[0].prompt_tokens, Some(1_200));
        assert_eq!(transcript[0].completion_tokens, Some(18));
        assert_eq!(transcript[0].cached_tokens, Some(1_150));
        assert_eq!(transcript[0].duration_ms, Some(900));

        let failed = append(
            &db,
            CONVERSATION_ID,
            Role::Assistant,
            "half",
            State::Streaming,
        )
        .unwrap();
        fail(&db, &failed, "provider_timeout", "half").unwrap();
        let transcript = load(&db, CONVERSATION_ID).unwrap();
        let refused = transcript.last().expect("the failed row");
        assert_eq!(refused.state, State::Failed);
        assert_eq!(refused.error_code.as_deref(), Some("provider_timeout"));
        assert_eq!(refused.body, "half");
    }

    #[test]
    fn history_is_settled_only_and_never_the_question_being_answered() {
        let db = db();
        append(&db, CONVERSATION_ID, Role::User, "q1", State::Complete).unwrap();
        append(&db, CONVERSATION_ID, Role::Assistant, "a1", State::Complete).unwrap();
        append(&db, CONVERSATION_ID, Role::Assistant, "", State::Streaming).unwrap();
        let failed = append(&db, CONVERSATION_ID, Role::Assistant, "oops", State::Failed).unwrap();
        let question = append(&db, CONVERSATION_ID, Role::User, "q2", State::Complete).unwrap();

        let transcript = load(&db, CONVERSATION_ID).unwrap();
        let context = history(&transcript, &question);
        let bodies: Vec<&str> = context
            .iter()
            .map(|message| message.content.as_str())
            .collect();
        assert_eq!(bodies, vec!["q1", "a1"]);
        assert!(context.iter().all(|message| message.content != "oops"));
        assert_eq!(transcript.iter().filter(|m| m.id == failed).count(), 1);
    }

    #[test]
    fn history_is_capped_at_the_window_and_keeps_the_newest() {
        let db = db();
        for index in 0..(HISTORY_MESSAGES + 6) {
            let role = if index % 2 == 0 {
                Role::User
            } else {
                Role::Assistant
            };
            append(
                &db,
                CONVERSATION_ID,
                role,
                &format!("m{index}"),
                State::Complete,
            )
            .unwrap();
        }
        let transcript = load(&db, CONVERSATION_ID).unwrap();
        let context = history(&transcript, "none");
        assert_eq!(context.len(), HISTORY_MESSAGES);
        assert_eq!(
            context.last().expect("a last message").content,
            format!("m{}", HISTORY_MESSAGES + 5)
        );
    }

    #[test]
    fn clearing_a_conversation_removes_every_row_and_nothing_else_does() {
        let db = db();
        append(&db, CONVERSATION_ID, Role::User, "keep", State::Complete).unwrap();
        append(&db, "other", Role::User, "elsewhere", State::Complete).unwrap();
        assert_eq!(count(&db, CONVERSATION_ID).unwrap(), 1);

        let removed = clear(&db, CONVERSATION_ID).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(count(&db, CONVERSATION_ID).unwrap(), 0);
        assert_eq!(count(&db, "other").unwrap(), 1);
    }

    #[test]
    fn conversations_are_listed_newest_first_and_deleted_with_their_messages() {
        let db = db();
        let older = create_conversation(&db).unwrap();
        append(&db, &older, Role::User, "older question", State::Complete).unwrap();
        let newer = create_conversation(&db).unwrap();
        append(&db, &newer, Role::User, "newer question", State::Complete).unwrap();
        // The stamp has millisecond resolution; two rows made in the same
        // millisecond tie, and the tie-break is insertion order. Waiting is what
        // makes "touched" mean what the rail shows.
        std::thread::sleep(std::time::Duration::from_millis(5));
        touch_conversation(&db, &older).unwrap();

        let listed = conversations(&db, SESSION_LIMIT).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(
            listed[0].id, older,
            "touch moves a conversation to the top of the rail"
        );

        let removed = delete_conversation(&db, &older).unwrap();
        assert_eq!(removed, 1, "the message goes with the conversation");
        assert!(!conversation_exists(&db, &older).unwrap());
        assert_eq!(count(&db, &older).unwrap(), 0);
    }

    #[test]
    fn a_page_always_resolves_a_conversation_that_exists() {
        let db = db();
        // Nothing yet: resolve creates one rather than returning nothing.
        let first = resolve_conversation(&db, None).unwrap();
        assert!(conversation_exists(&db, &first).unwrap());

        // A requested id that exists wins.
        let second = create_conversation(&db).unwrap();
        assert_eq!(resolve_conversation(&db, Some(&second)).unwrap(), second);

        // A bogus id is the newest conversation, never an error.
        assert_eq!(resolve_conversation(&db, Some("nope")).unwrap(), second);
    }

    #[test]
    fn a_conversation_is_named_by_its_first_message_once() {
        let db = db();
        let id = create_conversation(&db).unwrap();
        name_from_first_message(&db, &id, "  What is   the\n state of the lab?  ").unwrap();
        let title = conversations(&db, 1).unwrap()[0].title.clone();
        assert_eq!(title, "What is the state of the lab?");

        // A second message never renames it.
        name_from_first_message(&db, &id, "a different question").unwrap();
        assert_eq!(conversations(&db, 1).unwrap()[0].title, title);
    }

    #[test]
    fn a_long_first_message_is_cut_to_a_title() {
        let long = "x".repeat(TITLE_LIMIT + 20);
        let title = title_from(&long);
        assert_eq!(
            title.chars().count(),
            TITLE_LIMIT + 1,
            "the cut plus its ellipsis"
        );
        assert!(title.ends_with('…'));
    }

    #[test]
    fn stats_sum_what_providers_reported_and_refuse_to_invent_the_rest() {
        let transcript = vec![
            Message {
                id: "u1".into(),
                role: Role::User,
                body: "q1".into(),
                state: State::Complete,
                error_code: None,
                created_at: "2026-10-19T20:00:00Z".into(),
                model: None,
                prompt_tokens: None,
                completion_tokens: None,
                cached_tokens: None,
                context_window: None,
                context_tokens: None,
                duration_ms: None,
                reasoning: None,
                tools: Vec::new(),
                rounds: None,
                client_message_id: None,
                job_id: None,
                capped: None,
                wrapped_up: None,
                quota: None,
            },
            Message {
                id: "a1".into(),
                role: Role::Assistant,
                body: "a1".into(),
                state: State::Complete,
                error_code: None,
                created_at: "2026-10-19T20:00:01Z".into(),
                model: Some("m/one".into()),
                prompt_tokens: Some(1_000),
                completion_tokens: Some(100),
                cached_tokens: Some(900),
                context_window: Some(10_000),
                context_tokens: Some(1_900),
                duration_ms: Some(2_000),
                reasoning: None,
                tools: Vec::new(),
                rounds: None,
                client_message_id: None,
                job_id: None,
                capped: None,
                wrapped_up: None,
                quota: None,
            },
            Message {
                id: "a2".into(),
                role: Role::Assistant,
                body: "".into(),
                state: State::Failed,
                error_code: Some("provider_timeout".into()),
                created_at: "2026-10-19T20:00:02Z".into(),
                model: Some("m/one".into()),
                prompt_tokens: None,
                completion_tokens: None,
                cached_tokens: None,
                context_window: Some(10_000),
                context_tokens: None,
                duration_ms: None,
                reasoning: None,
                tools: Vec::new(),
                rounds: None,
                client_message_id: None,
                job_id: None,
                capped: None,
                wrapped_up: None,
                quota: None,
            },
            Message {
                id: "u2".into(),
                role: Role::User,
                body: "q2".into(),
                state: State::Complete,
                error_code: None,
                created_at: "2026-10-19T20:00:03Z".into(),
                model: None,
                prompt_tokens: None,
                completion_tokens: None,
                cached_tokens: None,
                context_window: None,
                context_tokens: None,
                duration_ms: None,
                reasoning: None,
                tools: Vec::new(),
                rounds: None,
                client_message_id: None,
                job_id: None,
                capped: None,
                wrapped_up: None,
                quota: None,
            },
        ];

        let stats = stats(&transcript);
        assert_eq!(stats.turns, 2);
        assert_eq!(stats.answers, 1, "a failed answer is not a settled one");
        assert_eq!(stats.prompt_tokens, 1_000);
        assert_eq!(stats.completion_tokens, 100);
        assert_eq!(stats.cached_tokens, 900);
        assert_eq!(stats.total_tokens, 1_100);
        // 900 cached of 1,900 prompt-side tokens.
        assert_eq!(stats.cache_hit_pct, Some(47));
        // The live context is the last round's own prompt, never the sum over a
        // transcript: a hundred tool rounds must not read as a window 1028% full.
        assert_eq!(stats.context_used, Some(1_900));
        assert_eq!(stats.context_window, Some(10_000));
        assert_eq!(stats.context_pct, Some(19));
        assert_eq!(stats.tokens_per_second, Some(50));
    }

    /// The bug the screenshot carried: a turn whose summed prompt was ten times
    /// its window printed `context 1028%`.
    #[test]
    fn a_long_turn_is_not_a_full_window() {
        let transcript = vec![Message {
            id: "a1".into(),
            role: Role::Assistant,
            body: "done".into(),
            state: State::Complete,
            error_code: None,
            created_at: "2026-10-19T20:00:01Z".into(),
            model: Some("m/one".into()),
            // A hundred rounds' worth of prompt, summed.
            prompt_tokens: Some(1_028_000),
            completion_tokens: Some(9_000),
            cached_tokens: None,
            context_window: Some(1_000_000),
            // And the last round trip, which is what the window held.
            context_tokens: Some(96_000),
            duration_ms: Some(2_000),
            reasoning: None,
            tools: Vec::new(),
            rounds: Some(100),
            client_message_id: None,
            job_id: None,
            capped: None,
            wrapped_up: None,
            quota: None,
        }];
        let stats = stats(&transcript);
        assert_eq!(stats.context_used, Some(96_000));
        assert_eq!(
            stats.context_pct,
            Some(9),
            "a share of a window, not of a bill"
        );
        assert_eq!(
            crate::web::templates::context_share(stats.context_used, stats.context_window),
            "context 9.6%"
        );
    }

    /// A row from before the column existed says the context is unknown rather
    /// than printing the summed prompt as a share of the window.
    #[test]
    fn an_older_row_reports_the_context_as_unknown() {
        let transcript = vec![Message {
            id: "a1".into(),
            role: Role::Assistant,
            body: "done".into(),
            state: State::Complete,
            error_code: None,
            created_at: "2026-10-19T20:00:01Z".into(),
            model: Some("m/one".into()),
            prompt_tokens: Some(1_028_000),
            completion_tokens: Some(9_000),
            cached_tokens: None,
            context_window: Some(1_000_000),
            context_tokens: None,
            duration_ms: Some(2_000),
            reasoning: None,
            tools: Vec::new(),
            rounds: Some(100),
            client_message_id: None,
            job_id: None,
            capped: None,
            wrapped_up: None,
            quota: None,
        }];
        let stats = stats(&transcript);
        assert_eq!(stats.context_used, None);
        assert_eq!(stats.context_pct, None);
    }

    #[test]
    fn a_transcript_with_no_reported_numbers_reports_none_rather_than_zero() {
        let stats = stats(&[]);
        assert_eq!(stats.total_tokens, 0);
        assert_eq!(stats.cache_hit_pct, None);
        assert_eq!(stats.context_pct, None);
        assert_eq!(stats.tokens_per_second, None);
    }

    /// The bug the operator reported: a forced answer looked finished the
    /// moment the page was reloaded.
    #[test]
    fn a_capped_answer_is_stored_as_capped_and_a_finished_one_is_not() {
        let db = db();

        let capped = append(&db, CONVERSATION_ID, Role::Assistant, "", State::Streaming).unwrap();
        settle(
            &db,
            &capped,
            "the forced answer",
            None,
            &[],
            None,
            Accounting {
                rounds: Some(100),
                capped: Some(true),
                wrapped_up: Some(false),
                ..Accounting::default()
            },
        )
        .unwrap();

        let finished = append(&db, CONVERSATION_ID, Role::Assistant, "", State::Streaming).unwrap();
        settle(
            &db,
            &finished,
            "the whole job",
            None,
            &[],
            None,
            Accounting {
                rounds: Some(6),
                capped: Some(false),
                wrapped_up: Some(false),
                ..Accounting::default()
            },
        )
        .unwrap();

        // A row nobody measured: the phone's settle path, a slash command.
        let unmeasured =
            append(&db, CONVERSATION_ID, Role::Assistant, "", State::Streaming).unwrap();
        settle(
            &db,
            &unmeasured,
            "no numbers either way",
            None,
            &[],
            None,
            Accounting::default(),
        )
        .unwrap();

        let transcript = load(&db, CONVERSATION_ID).unwrap();
        let capped = transcript
            .iter()
            .find(|row| row.id == capped)
            .expect("capped row");
        assert_eq!(
            capped.capped,
            Some(true),
            "the budget ended it, and the row says so"
        );
        let finished = transcript
            .iter()
            .find(|row| row.id == finished)
            .expect("finished row");
        assert_eq!(
            finished.capped, None,
            "a turn that finished on its own records nothing: only a forced answer sets the flag"
        );
        let unmeasured = transcript
            .iter()
            .find(|row| row.id == unmeasured)
            .expect("unmeasured row");
        assert_eq!(
            unmeasured.capped, None,
            "not measured and not capped are the same column value, and neither is a zero"
        );
    }

    #[test]
    fn a_file_name_cannot_carry_a_path() {
        assert_eq!(safe_file_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_file_name("C:\\Users\\x\\notes.txt"), "notes.txt");
        assert_eq!(safe_file_name("weird name: v2?.md"), "weird name_ v2_.md");
        assert_eq!(safe_file_name("   "), "file");
        assert_eq!(safe_file_name("..."), "file");
        let long = "a".repeat(300);
        assert!(safe_file_name(&long).len() <= 120);
    }

    #[test]
    fn text_files_ride_in_the_prompt_and_binaries_are_only_located() {
        let dir = tempfile::tempdir().expect("temp dir");
        let text = dir.path().join("notes.md");
        std::fs::write(&text, "# Title\n\nBody line").unwrap();
        let binary = dir.path().join("scan.pdf");
        std::fs::write(&binary, b"%PDF-1.7\x00\x01").unwrap();

        let files = vec![
            Attachment {
                id: "a".into(),
                name: "notes.md".into(),
                path: text.display().to_string(),
                size: 18,
                media_type: Some("text/markdown".into()),
            },
            Attachment {
                id: "b".into(),
                name: "scan.pdf".into(),
                path: binary.display().to_string(),
                size: 9,
                media_type: Some("application/pdf".into()),
            },
        ];

        let described = describe_attachments(&files);
        assert!(described.contains("notes.md"));
        assert!(described.contains("Body line"), "text content is pasted");
        assert!(described.contains("scan.pdf"));
        assert!(
            !described.contains("%PDF"),
            "binary content is never pasted into a prompt"
        );
        assert_eq!(describe_attachments(&[]), "");
    }

    #[test]
    fn is_text_knows_the_extensions_it_pastes() {
        for name in ["a.txt", "b.md", "c.json", "d.rs", "e.csv"] {
            assert!(is_text(name), "{name}");
        }
        for name in ["a.pdf", "b.png", "c.zip", "noextension"] {
            assert!(!is_text(name), "{name}");
        }
    }

    #[test]
    fn an_empty_message_is_refused_by_name_and_the_ceiling_is_bytes() {
        assert_eq!(accept_text("  \n ").unwrap_err(), "a message is required");
        assert_eq!(accept_text("  hi  ").unwrap(), "hi");

        let too_long = "x".repeat(MAX_MESSAGE_BYTES + 1);
        let error = accept_text(&too_long).unwrap_err();
        assert!(
            error.contains("131.1 kB"),
            "the size is named in a unit: {error}"
        );
        assert!(
            error.contains("131.1 kB"),
            "the limit is named too, in the same units: {error}"
        );
    }

    fn completion_with_calls(
        calls: Vec<core_domain::ToolCall>,
        text: &str,
    ) -> core_runtime::provider::Completion {
        core_runtime::provider::Completion {
            text: text.to_owned(),
            reasoning: String::new(),
            tool_calls: calls,
            usage: core_domain::Usage::reconcile(
                core_domain::CacheReadReporting::NotReported,
                Some(10),
                None,
                Some(1),
            ),
            finish_reason: Some("stop".to_owned()),
        }
    }

    fn scratch_workspace(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("kallisti-loop-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch");
        root
    }

    /// The daemon's half of attachment-only sends: an empty box with a file is a
    /// message, and the row it writes still says something.
    #[test]
    fn an_empty_message_is_only_a_refusal_when_nothing_is_attached() {
        assert_eq!(
            accept_message("   ", false).unwrap_err(),
            "a message is required"
        );
        assert_eq!(accept_message("", true).expect("a file is a message"), "");
        assert_eq!(accept_message("  hi  ", true).expect("trimmed"), "hi");
        let too_long = "x".repeat(MAX_MESSAGE_BYTES + 1);
        assert!(
            accept_message(&too_long, true).is_err(),
            "a caption is still bounded, attached or not"
        );
    }

    /// The line a file-only message leaves in the transcript, and therefore the
    /// line the conversation is named after.
    #[test]
    fn a_file_only_message_is_named_after_its_files() {
        let one = [file("shot.png")];
        assert_eq!(attachments_line(&one), "Attached shot.png");
        let two = [file("a.log"), file("b.json")];
        assert_eq!(attachments_line(&two), "Attached 2 files: a.log, b.json");
        assert_eq!(attachments_line(&[]), "");
    }

    fn file(name: &str) -> Attachment {
        Attachment {
            id: name.to_owned(),
            name: name.to_owned(),
            path: format!("/tmp/{name}"),
            size: 1,
            media_type: None,
        }
    }

    #[test]
    fn a_tool_call_is_executed_and_the_turn_resumes_with_the_result() {
        let root = scratch_workspace("resume");
        std::fs::write(root.join("a.txt"), "alpha").expect("file");
        let sandbox = crate::tools::Sandbox::new(
            root.clone(),
            crate::onboarding::permissions::SandboxMode::ReadOnly,
        );

        let call = core_domain::ToolCall::new(
            0,
            "call_1",
            "read_file",
            serde_json::json!({"path": "a.txt"}),
        )
        .expect("a call");

        let mut seen: Vec<Vec<ChatMessage>> = Vec::new();
        let mut round = 0usize;
        let outcome = drive_tool_loop(
            &mut |call: &core_domain::ToolCall,
                  chunks: &mut dyn FnMut(&str),
                  _asked: &mut dyn FnMut(Asked),
                  _poll: &mut TurnPoll<'_>| {
                crate::tools::execute_streaming(&call.name, &call.arguments, &sandbox, chunks)
            },
            vec![ChatMessage::text(ChatRole::User, "read a.txt")],
            &mut |_call| GateDecision::Run,
            MAX_TOOL_ROUNDS,
            None,
            &mut |_part| {},
            &mut || TurnSignal::Continue,
            |messages, _tools, _emit| {
                seen.push(messages.to_vec());
                round += 1;
                if round == 1 {
                    Ok(completion_with_calls(vec![call.clone()], ""))
                } else {
                    Ok(completion_with_calls(Vec::new(), "alpha"))
                }
            },
        )
        .expect("a turn");

        assert_eq!(round, 2, "the provider is called once per round");
        assert_eq!(outcome.text, "alpha");
        assert_eq!(outcome.rounds, 1, "one tool round happened");

        let second = seen.get(1).expect("a second request");
        let replayed = second
            .iter()
            .find(|message| !message.tool_calls.is_empty())
            .expect("the assistant calls are replayed");
        assert_eq!(replayed.role, ChatRole::Assistant);
        assert_eq!(replayed.tool_calls[0].id, "call_1");

        let result = second
            .iter()
            .find(|message| message.role == ChatRole::Tool)
            .expect("the tool result is sent back");
        assert_eq!(result.tool_call_id.as_deref(), Some("call_1"));
        assert!(
            result.content.contains("alpha"),
            "the tool result must be the real content: {}",
            result.content
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_model_that_only_calls_tools_is_stopped_at_the_cap_with_a_notice() {
        let root = scratch_workspace("cap");
        std::fs::write(root.join("a.txt"), "alpha").expect("file");
        let sandbox = crate::tools::Sandbox::new(
            root.clone(),
            crate::onboarding::permissions::SandboxMode::ReadOnly,
        );

        let call = core_domain::ToolCall::new(
            0,
            "call_1",
            "read_file",
            serde_json::json!({"path": "a.txt"}),
        )
        .expect("a call");

        let mut round = 0usize;
        let mut tools_offered: Vec<bool> = Vec::new();
        let outcome = drive_tool_loop(
            &mut |call: &core_domain::ToolCall,
                  chunks: &mut dyn FnMut(&str),
                  _asked: &mut dyn FnMut(Asked),
                  _poll: &mut TurnPoll<'_>| {
                crate::tools::execute_streaming(&call.name, &call.arguments, &sandbox, chunks)
            },
            vec![ChatMessage::text(ChatRole::User, "loop forever")],
            &mut |_call| GateDecision::Run,
            MAX_TOOL_ROUNDS,
            None,
            &mut |_part| {},
            &mut || TurnSignal::Continue,
            |_messages, tools, _emit| {
                round += 1;
                tools_offered.push(tools);
                Ok(completion_with_calls(vec![call.clone()], ""))
            },
        )
        .expect("a turn");

        // Two fewer tool rounds than the budget, on purpose. The lookahead takes
        // the tools away two rounds early, so a turn that would have spent every
        // round reaching for one spends its last two writing. This assertion used
        // to read `MAX_TOOL_ROUNDS`, which was the cap in tool rounds before the
        // lookahead existed; the cap is now the budget, and the tool rounds are
        // the budget minus the wrap-up window.
        assert_eq!(
            outcome.rounds,
            MAX_TOOL_ROUNDS - 2,
            "the wrap-up window is two rounds, so two rounds of tools are given up"
        );
        assert_eq!(
            round, MAX_TOOL_ROUNDS,
            "every round of the budget is still used: the two given up on tools are \
             spent asking for prose instead"
        );
        assert_eq!(
            &tools_offered[round - 2..],
            &[false, false],
            "neither closing attempt may offer tools, so the turn has to answer"
        );
        assert!(
            tools_offered[..MAX_TOOL_ROUNDS - 2]
                .iter()
                .all(|offered| *offered),
            "every round before the wrap-up offers them"
        );
        assert!(outcome.capped, "the budget ended this turn, not the model");
        assert!(
            !outcome.text.trim().is_empty(),
            "a capped turn still tells the operator something"
        );
        assert!(
            outcome.text.to_lowercase().contains("tool"),
            "the notice explains why it stopped: {}",
            outcome.text
        );
        assert!(
            outcome.text.contains("read_file"),
            "the notice names the work that ran, so the next turn knows where it \
             got to: {}",
            outcome.text
        );
        assert!(
            outcome.text.contains("Continue"),
            "the notice says how to pick the job back up: {}",
            outcome.text
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A model that reaches for a tool it is no longer offered writes that call as
    /// markup, which used to become an empty answer and a shrug. It is asked once
    /// more in plain prose, and that prose is the answer.
    #[test]
    fn a_capped_turn_answers_when_it_is_asked_plainly() {
        let root = scratch_workspace("cap-answer");
        let sandbox = crate::tools::Sandbox::new(
            root.clone(),
            crate::onboarding::permissions::SandboxMode::ReadOnly,
        );

        let call = core_domain::ToolCall::new(
            0,
            "call_1",
            "read_file",
            serde_json::json!({"path": "a.txt"}),
        )
        .expect("a call");

        let mut attempt = 0usize;
        let outcome = drive_tool_loop(
            &mut |call: &core_domain::ToolCall,
                  chunks: &mut dyn FnMut(&str),
                  _asked: &mut dyn FnMut(Asked),
                  _poll: &mut TurnPoll<'_>| {
                crate::tools::execute_streaming(&call.name, &call.arguments, &sandbox, chunks)
            },
            vec![ChatMessage::text(ChatRole::User, "loop forever")],
            &mut |_call| GateDecision::Run,
            1,
            None,
            &mut |_part| {},
            &mut || TurnSignal::Continue,
            |_messages, tools, _emit| {
                attempt += 1;
                // Round one runs a tool. Every later attempt - the tool-free one
                // and the plain-prose one - has only markup to give until the
                // final ask, which answers.
                if tools {
                    Ok(completion_with_calls(vec![call.clone()], ""))
                } else if attempt < 3 {
                    Ok(completion_with_calls(
                        Vec::new(),
                        "<|DSML|tool_calls><|DSML|invoke name=\"read_file\"></|DSML|invoke>",
                    ))
                } else {
                    Ok(completion_with_calls(
                        Vec::new(),
                        "Read a.txt: it says alpha. Nothing else is unfinished.",
                    ))
                }
            },
        )
        .expect("a turn");

        assert!(outcome.capped, "the cap ended it");
        assert_eq!(outcome.rounds, 1, "one tool round, then the boundary");
        assert_eq!(
            outcome.text, "Read a.txt: it says alpha. Nothing else is unfinished.",
            "the plain ask is what produced a usable answer"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_turn_carries_the_workspace_tools() {
        let root = scratch_workspace("specs");
        let sandbox = crate::tools::Sandbox::new(
            root.clone(),
            crate::onboarding::permissions::SandboxMode::ReadOnly,
        );

        let mut offered: Vec<String> = Vec::new();
        let _ = drive_tool_loop(
            &mut |call: &core_domain::ToolCall,
                  chunks: &mut dyn FnMut(&str),
                  _asked: &mut dyn FnMut(Asked),
                  _poll: &mut TurnPoll<'_>| {
                crate::tools::execute_streaming(&call.name, &call.arguments, &sandbox, chunks)
            },
            vec![ChatMessage::text(ChatRole::User, "hi")],
            &mut |_call| GateDecision::Run,
            MAX_TOOL_ROUNDS,
            None,
            &mut |_part| {},
            &mut || TurnSignal::Continue,
            |_messages, _tools, _emit| Ok(completion_with_calls(Vec::new(), "ok")),
        )
        .expect("a turn");

        offered.extend(
            crate::tools::specs(crate::onboarding::permissions::SandboxMode::WorkspaceWrite)
                .into_iter()
                .map(|spec| spec.name),
        );
        assert!(offered.contains(&"read_file".to_owned()));
        assert!(offered.contains(&"write_file".to_owned()));
        assert!(offered.contains(&"list_dir".to_owned()));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_write_the_gate_parks_never_touches_the_disk_and_says_so() {
        let root = scratch_workspace("parked");
        let sandbox = crate::tools::Sandbox::new(
            root.clone(),
            crate::onboarding::permissions::SandboxMode::WorkspaceWrite,
        );

        let call = core_domain::ToolCall::new(
            0,
            "call_1",
            "write_file",
            serde_json::json!({"path": "x.txt", "content": "hi"}),
        )
        .expect("a call");

        let mut seen: Vec<Vec<ChatMessage>> = Vec::new();
        let mut round = 0usize;
        let outcome = drive_tool_loop(
            &mut |call: &core_domain::ToolCall,
                  chunks: &mut dyn FnMut(&str),
                  _asked: &mut dyn FnMut(Asked),
                  _poll: &mut TurnPoll<'_>| {
                crate::tools::execute_streaming(&call.name, &call.arguments, &sandbox, chunks)
            },
            vec![ChatMessage::text(ChatRole::User, "write x")],
            &mut |_call| GateDecision::Park {
                approval_id: "ap_1".to_owned(),
            },
            MAX_TOOL_ROUNDS,
            None,
            &mut |_part| {},
            &mut || TurnSignal::Continue,
            |messages, _tools, _emit| {
                seen.push(messages.to_vec());
                round += 1;
                if round == 1 {
                    Ok(completion_with_calls(vec![call.clone()], ""))
                } else {
                    Ok(completion_with_calls(
                        Vec::new(),
                        "That write is waiting for your approval.",
                    ))
                }
            },
        )
        .expect("a turn");

        assert!(
            !root.join("x.txt").exists(),
            "a parked write must not touch the disk"
        );

        let second = seen.get(1).expect("a second request");
        let result = second
            .iter()
            .find(|message| message.role == ChatRole::Tool)
            .expect("a tool result");
        assert!(
            result.content.contains("pending_approval"),
            "the model is told it is waiting: {}",
            result.content
        );
        assert!(
            result.content.contains("ap_1"),
            "the approval id travels with it: {}",
            result.content
        );
        assert!(outcome.text.contains("approval"), "{}", outcome.text);

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A model that calls a tool every round is what the lookahead exists for.
    ///
    /// The turn is given four rounds and asked to wrap up two before the end, so
    /// the tools have to be gone on the third round and the model has to be told
    /// why once.
    #[test]
    fn the_wrap_up_fires_two_rounds_before_the_budget_and_once_only() {
        let root = scratch_workspace("wrapup");
        std::fs::write(root.join("a.txt"), "alpha").expect("file");
        let sandbox = crate::tools::Sandbox::new(
            root.clone(),
            crate::onboarding::permissions::SandboxMode::ReadOnly,
        );
        let call = core_domain::ToolCall::new(
            0,
            "call_1",
            "read_file",
            serde_json::json!({"path": "a.txt"}),
        )
        .expect("a call");

        let mut seen: Vec<Vec<ChatMessage>> = Vec::new();
        let mut tools_offered: Vec<bool> = Vec::new();
        let outcome = drive_tool_loop(
            &mut |call: &core_domain::ToolCall,
                  chunks: &mut dyn FnMut(&str),
                  _asked: &mut dyn FnMut(Asked),
                  _poll: &mut TurnPoll<'_>| {
                crate::tools::execute_streaming(&call.name, &call.arguments, &sandbox, chunks)
            },
            vec![ChatMessage::text(ChatRole::User, "keep looking")],
            &mut |_call| GateDecision::Run,
            4,
            None,
            &mut |_part| {},
            &mut || TurnSignal::Continue,
            |messages, tools, _emit| {
                seen.push(messages.to_vec());
                tools_offered.push(tools);
                Ok(completion_with_calls(vec![call.clone()], ""))
            },
        )
        .expect("a turn");

        assert_eq!(
            &tools_offered[..4],
            &[true, true, false, false],
            "a budget of four wraps at round two, so the last two rounds are tool-free"
        );
        // The snapshots accumulate, so the last one is the whole history. One
        // WRAP_UP in it means it was pushed once; a push per round would leave
        // two, because the wrap-up covers two rounds.
        let told: usize = seen
            .last()
            .expect("the provider was called")
            .iter()
            .filter(|message| message.content == WRAP_UP)
            .count();
        assert_eq!(told, 1, "the model is told once, not every round");
        assert!(
            outcome.capped,
            "the model never wrote the answer it was asked for"
        );
        assert!(
            !outcome.wrapped_up,
            "a turn that produced no prose is capped, not wrapped up"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// The same turn, but the model does what it was told.
    #[test]
    fn a_wrapped_up_turn_writes_its_own_answer_and_is_not_capped() {
        let root = scratch_workspace("wrapup-answer");
        std::fs::write(root.join("a.txt"), "alpha").expect("file");
        let sandbox = crate::tools::Sandbox::new(
            root.clone(),
            crate::onboarding::permissions::SandboxMode::ReadOnly,
        );
        let call = core_domain::ToolCall::new(
            0,
            "call_1",
            "read_file",
            serde_json::json!({"path": "a.txt"}),
        )
        .expect("a call");

        let mut round = 0usize;
        let outcome = drive_tool_loop(
            &mut |call: &core_domain::ToolCall,
                  chunks: &mut dyn FnMut(&str),
                  _asked: &mut dyn FnMut(Asked),
                  _poll: &mut TurnPoll<'_>| {
                crate::tools::execute_streaming(&call.name, &call.arguments, &sandbox, chunks)
            },
            vec![ChatMessage::text(ChatRole::User, "read it and tell me")],
            &mut |_call| GateDecision::Run,
            4,
            None,
            &mut |_part| {},
            &mut || TurnSignal::Continue,
            |_messages, tools, _emit| {
                round += 1;
                if tools {
                    Ok(completion_with_calls(vec![call.clone()], ""))
                } else {
                    Ok(completion_with_calls(Vec::new(), "a.txt says alpha"))
                }
            },
        )
        .expect("a turn");

        assert_eq!(round, 3, "two rounds of tools, then the wrapped-up answer");
        assert_eq!(outcome.text, "a.txt says alpha");
        assert!(
            outcome.wrapped_up,
            "the engine stopped it early and the model wrote the answer"
        );
        assert!(
            !outcome.capped,
            "the answer was written by the model, not forced by the budget"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A turn that finishes inside its budget is exactly what it was before.
    #[test]
    fn a_short_turn_is_neither_capped_nor_wrapped_up() {
        let root = scratch_workspace("short-turn");
        let outcome = drive_tool_loop(
            &mut |_call: &core_domain::ToolCall,
                  _chunks: &mut dyn FnMut(&str),
                  _asked: &mut dyn FnMut(Asked),
                  _poll: &mut TurnPoll<'_>| {
                crate::tools::ToolResult::ok(serde_json::json!("unused"))
            },
            vec![ChatMessage::text(ChatRole::User, "say hi")],
            &mut |_call| GateDecision::Run,
            MAX_TOOL_ROUNDS,
            None,
            &mut |_part| {},
            &mut || TurnSignal::Continue,
            |_messages, _tools, _emit| Ok(completion_with_calls(Vec::new(), "hi")),
        )
        .expect("a turn");
        assert_eq!(outcome.text, "hi");
        assert_eq!(outcome.rounds, 0);
        assert!(!outcome.capped);
        assert!(!outcome.wrapped_up);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A turn that was told to wrap up and then wrote nothing is not wrapped up.
    ///
    /// The two facts have to stay apart at the last resort too: the engine did
    /// stop it early, but the answer the operator reads is the summary of what
    /// ran, not the model's own words, so the tag is `capped`. Marking it
    /// `wrapped up` would credit the model with an answer it did not write.
    #[test]
    fn a_wrapped_up_turn_that_says_nothing_is_capped_with_the_last_resort() {
        let root = scratch_workspace("wrapup-silent");
        std::fs::write(root.join("a.txt"), "alpha").expect("file");
        let sandbox = crate::tools::Sandbox::new(
            root.clone(),
            crate::onboarding::permissions::SandboxMode::ReadOnly,
        );
        let call = core_domain::ToolCall::new(
            0,
            "call_1",
            "read_file",
            serde_json::json!({"path": "a.txt"}),
        )
        .expect("a call");

        let outcome = drive_tool_loop(
            &mut |call: &core_domain::ToolCall,
                  chunks: &mut dyn FnMut(&str),
                  _asked: &mut dyn FnMut(Asked),
                  _poll: &mut TurnPoll<'_>| {
                crate::tools::execute_streaming(&call.name, &call.arguments, &sandbox, chunks)
            },
            vec![ChatMessage::text(ChatRole::User, "keep looking")],
            &mut |_call| GateDecision::Run,
            4,
            None,
            &mut |_part| {},
            &mut || TurnSignal::Continue,
            // Never a word of prose, not even when asked plainly: the tool-free
            // round, the wrap-up round and the last-resort ask all come back as
            // the call it was told it could not make.
            |_messages, _tools, _emit| Ok(completion_with_calls(vec![call.clone()], "")),
        )
        .expect("a turn");

        assert!(
            outcome.text.to_lowercase().contains("budget ran out"),
            "the last resort names what happened: {}",
            outcome.text
        );
        assert!(
            outcome.text.contains("read_file"),
            "and names the work that ran, so the next turn knows where it got to: {}",
            outcome.text
        );
        assert!(
            outcome.capped,
            "the answer was forced, so this turn is capped"
        );
        assert!(
            !outcome.wrapped_up,
            "the model wrote none of that answer, so it did not wrap the turn up"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    /// A budget of one has no room to look ahead, so it behaves as it always did.
    #[test]
    fn a_one_round_budget_never_wraps_up() {
        let root = scratch_workspace("one-round");
        std::fs::write(root.join("a.txt"), "alpha").expect("file");
        let sandbox = crate::tools::Sandbox::new(
            root.clone(),
            crate::onboarding::permissions::SandboxMode::ReadOnly,
        );
        let call = core_domain::ToolCall::new(
            0,
            "call_1",
            "read_file",
            serde_json::json!({"path": "a.txt"}),
        )
        .expect("a call");

        let mut seen: Vec<Vec<ChatMessage>> = Vec::new();
        let outcome = drive_tool_loop(
            &mut |call: &core_domain::ToolCall,
                  chunks: &mut dyn FnMut(&str),
                  _asked: &mut dyn FnMut(Asked),
                  _poll: &mut TurnPoll<'_>| {
                crate::tools::execute_streaming(&call.name, &call.arguments, &sandbox, chunks)
            },
            vec![ChatMessage::text(ChatRole::User, "one round only")],
            &mut |_call| GateDecision::Run,
            1,
            None,
            &mut |_part| {},
            &mut || TurnSignal::Continue,
            |messages, _tools, _emit| {
                seen.push(messages.to_vec());
                Ok(completion_with_calls(vec![call.clone()], ""))
            },
        )
        .expect("a turn");
        assert!(
            seen.iter()
                .all(|messages| messages.iter().all(|message| message.content != WRAP_UP)),
            "there is nowhere to look ahead to in a one-round budget"
        );
        assert!(outcome.capped, "the budget still ends the turn");
        assert!(!outcome.wrapped_up);
        let _ = std::fs::remove_dir_all(&root);
    }
    /// Four conversations with deliberately crossed orders: the newest by
    /// `updated_at` is not the newest by `created_at`, and the alphabetical first
    /// is neither. Each sort mode has to disagree with the others or the test
    /// would pass against a function that ignored the mode.
    fn sorted_fixture() -> Db {
        let db = db();
        let insert = |id: &str, title: &str, created: &str, updated: &str| {
            db.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO chat_conversations (id, title, created_at, updated_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![id, title, created, updated],
                )?;
                Ok(())
            })
            .expect("insert");
        };
        insert(
            "beta",
            "beta work",
            "2026-10-20T09:00:00Z",
            "2026-10-26T09:00:00Z",
        );
        insert(
            "alpha",
            "Alpha notes",
            "2026-10-26T08:00:00Z",
            "2026-10-24T09:00:00Z",
        );
        insert(
            "delta",
            "delta ideas",
            "2026-10-21T09:00:00Z",
            "2026-10-25T09:00:00Z",
        );
        insert(
            "gamma",
            "Gamma plan",
            "2026-10-22T09:00:00Z",
            "2026-10-23T09:00:00Z",
        );
        db
    }

    fn ids(db: &Db, sort: ConversationSort) -> Vec<String> {
        conversations_sorted(db, SESSION_LIMIT, sort)
            .expect("the list reads")
            .into_iter()
            .map(|row| row.id)
            .collect()
    }

    /// The four modes, each asserted against an order the others do not share.
    #[test]
    fn the_four_sorts_order_the_same_conversations_four_ways() {
        let db = sorted_fixture();
        assert_eq!(
            ids(&db, ConversationSort::Updated),
            vec!["beta", "delta", "alpha", "gamma"]
        );
        assert_eq!(
            ids(&db, ConversationSort::Created),
            vec!["alpha", "gamma", "delta", "beta"]
        );
        assert_eq!(
            ids(&db, ConversationSort::Title),
            vec!["alpha", "beta", "delta", "gamma"],
            "title is case-insensitive, so Alpha sorts with the lower-case titles"
        );
        // `day` is the updated order; the grouping is what changes, not the order.
        assert_eq!(
            ids(&db, ConversationSort::Day),
            vec!["beta", "delta", "alpha", "gamma"]
        );
    }

    /// The default is today's order, and an unknown value is the default rather
    /// than an empty rail.
    #[test]
    fn an_unknown_sort_means_the_order_the_rail_already_had() {
        assert_eq!(ConversationSort::parse(None), ConversationSort::Updated);
        assert_eq!(ConversationSort::parse(Some("")), ConversationSort::Updated);
        assert_eq!(
            ConversationSort::parse(Some("sideways")),
            ConversationSort::Updated
        );
        assert_eq!(
            ConversationSort::parse(Some(" title ")),
            ConversationSort::Title
        );
        assert_eq!(ConversationSort::parse(Some("day")), ConversationSort::Day);
        assert_eq!(ConversationSort::Created.as_str(), "created");
        assert_eq!(ConversationSort::ALL.len(), 4);
    }

    /// The `conversations` entry point is the updated order, so every existing
    /// caller reads exactly what it read before.
    #[test]
    fn the_plain_list_is_the_recently_used_one() {
        let db = sorted_fixture();
        let plain: Vec<String> = conversations(&db, SESSION_LIMIT)
            .expect("the list reads")
            .into_iter()
            .map(|row| row.id)
            .collect();
        assert_eq!(plain, ids(&db, ConversationSort::Updated));
    }

    /// The day boundary is the operator's, not UTC, and the headings are the
    /// three the rail reads.
    ///
    /// The fixture sits on both sides of midnight at -04:00: 02:00Z is the
    /// previous evening there, which is exactly the case a UTC grouping gets
    /// wrong.
    #[test]
    fn the_day_grouping_uses_the_operator_clock_and_labels_each_day() {
        let db = db();
        let insert = |id: &str, updated: &str| {
            db.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO chat_conversations (id, title, created_at, updated_at)
                     VALUES (?1, ?1, ?2, ?2)",
                    params![id, updated],
                )?;
                Ok(())
            })
            .expect("insert");
        };
        insert("late-tonight", "2026-03-15T01:00:00Z");
        insert("earlier-tonight", "2026-03-14T22:00:00Z");
        insert("yesterday", "2026-03-14T03:00:00Z");
        insert("older", "2026-03-12T15:00:00Z");

        let rows = conversations_sorted(&db, SESSION_LIMIT, ConversationSort::Day)
            .expect("the list reads");
        let today = (2026, 3, 14);
        let groups = group_by_day(rows, -240, today);

        let labels: Vec<&str> = groups.iter().map(|group| group.label.as_str()).collect();
        assert_eq!(labels, vec!["Today", "Yesterday", "Mar 12"]);
        assert_eq!(
            groups[0].key, "2026-03-14",
            "01:00Z on the 15th is the evening of the 14th at -04:00"
        );
        assert_eq!(
            groups[0]
                .conversations
                .iter()
                .map(|row| row.id.as_str())
                .collect::<Vec<_>>(),
            vec!["late-tonight", "earlier-tonight"]
        );
        assert_eq!(groups[1].conversations[0].id, "yesterday");
        assert_eq!(groups[2].conversations[0].id, "older");
    }

    /// A day that is not today, not yesterday and wraps a month boundary still
    /// reads as a month and a number.
    #[test]
    fn a_day_label_handles_the_month_boundary() {
        assert_eq!(day_label((2026, 3, 1), (2026, 3, 2)), "Yesterday");
        assert_eq!(day_label((2026, 2, 28), (2026, 3, 1)), "Yesterday");
        assert_eq!(day_label((2026, 3, 14), (2026, 3, 14)), "Today");
        assert_eq!(day_label((2026, 2, 14), (2026, 3, 14)), "Feb 14");
        assert_eq!(day_label((2025, 12, 31), (2026, 1, 1)), "Yesterday");
    }
}

#[cfg(test)]
mod transcript_bound_tests {
    use super::*;

    fn message(index: usize, body: &str, outputs: &[&str]) -> Message {
        Message {
            id: format!("m{index}"),
            role: if index % 2 == 0 { Role::User } else { Role::Assistant },
            body: body.to_owned(),
            state: State::Complete,
            error_code: None,
            created_at: format!("2026-10-27T00:00:{index:02}.000Z"),
            model: None,
            prompt_tokens: None,
            completion_tokens: None,
            cached_tokens: None,
            context_tokens: None,
            context_window: None,
            duration_ms: None,
            reasoning: None,
            tools: outputs
                .iter()
                .map(|output| ToolRecord {
                    name: "run_command".to_owned(),
                    arguments: "{\"command\":\"ls\"}".to_owned(),
                    output: (*output).to_owned(),
                    is_error: false,
                    asked: None,
                })
                .collect(),
            client_message_id: None,
            job_id: None,
            rounds: None,
            capped: None,
            wrapped_up: None,
            quota: None,
        }
    }

    /// The defect this exists for: one measured conversation was 41 messages and
    /// 9.2 MB of JSON because each tool call stored a whole stdout. A reader that
    /// asks for that transcript has to get something bounded instead.
    #[test]
    fn a_transcript_of_huge_tool_outputs_fits_the_budget() {
        let huge = "x".repeat(140_000);
        let messages: Vec<Message> = (0..8)
            .map(|index| message(index, "a short line", &[huge.as_str()]))
            .collect();
        let bounded = bound_transcript(messages, TRANSCRIPT_BYTE_BUDGET, TOOL_OUTPUT_PREVIEW);

        let bytes: usize = bounded.messages.iter().map(wire_cost).sum();
        assert!(
            bytes <= TRANSCRIPT_BYTE_BUDGET,
            "bounded transcript is {bytes} bytes, over the {TRANSCRIPT_BYTE_BUDGET} budget"
        );
        assert!(!bounded.messages.is_empty(), "a bounded read keeps something");
        assert_eq!(bounded.elided_outputs, bounded.messages.len());
        // The newest message survives: the reader is looking at the bottom.
        assert_eq!(
            bounded.messages.last().expect("a last message").id,
            "m7",
            "the newest message must be the one kept"
        );
    }

    /// The preview is what a reader is given, and it says what it left behind.
    #[test]
    fn an_elided_output_says_how_much_it_left_out() {
        let huge = "y".repeat(20_000);
        let bounded = bound_transcript(vec![message(0, "q", &[huge.as_str()])], 1 << 20, 4096);
        let output = &bounded.messages[0].tools[0].output;
        assert!(output.starts_with(&"y".repeat(4096)));
        assert!(output.contains("15904 bytes of output not shown"), "got: {output}");
        assert_eq!(bounded.elided_outputs, 1);
        assert_eq!(bounded.omitted, 0);
    }

    /// A short transcript is passed through untouched, so the bound costs a
    /// healthy chat nothing.
    #[test]
    fn a_short_transcript_is_untouched() {
        let messages: Vec<Message> = (0..4).map(|i| message(i, "hello", &["ls"])).collect();
        let bounded = bound_transcript(messages, TRANSCRIPT_BYTE_BUDGET, TOOL_OUTPUT_PREVIEW);
        assert_eq!(bounded.messages.len(), 4);
        assert_eq!(bounded.omitted, 0);
        assert_eq!(bounded.elided_outputs, 0);
    }

    /// Elision must not split a UTF-8 scalar in half: the preview is a byte
    /// budget, and the output is whatever the command printed.
    #[test]
    fn elision_respects_char_boundaries() {
        let emoji = "\u{1F600}".repeat(4000); // four bytes each, 16000 total
        let bounded = bound_transcript(vec![message(0, "q", &[emoji.as_str()])], 1 << 20, 4095);
        let output = &bounded.messages[0].tools[0].output;
        assert!(output.chars().count() > 0);
        // If a scalar were split, the string could not round-trip through chars.
        assert!(output.starts_with('\u{1F600}'));
    }
}
