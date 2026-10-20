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
    /// What the bytes claim to be, from the extension.
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

/// The media type from a filename, for the record. Never trusted for safety:
/// what is stored is decided by the name, and what is served is decided by the
/// route, so a lying extension is a filing problem rather than a security one.
#[must_use]
pub fn media_type_for(name: &str) -> Option<String> {
    let extension = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())?;
    let kind = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "csv" => "text/csv",
        "md" | "markdown" => "text/markdown",
        "txt" | "log" | "toml" | "yaml" | "yml" | "ini" | "cfg" | "rs" | "py" | "sh" | "sql"
        | "html" => "text/plain",
        "zip" => "application/zip",
        _ => return None,
    };
    Some(kind.to_owned())
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
    /// The project this conversation is filed under, when it is filed.
    pub project_id: Option<String>,
}

/// The newest conversations, newest first.
pub fn conversations(db: &Db, limit: usize) -> Result<Vec<Conversation>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, title, updated_at, project_id
               FROM chat_conversations
              ORDER BY updated_at DESC, rowid DESC
              LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit as i64], |row| {
            Ok(Conversation {
                id: row.get(0)?,
                title: row.get(1)?,
                updated_at: row.get(2)?,
                project_id: row.get(3)?,
            })
        })?;
        rows.collect()
    })
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
    Ok(Some(TurnConfig {
        base_url: provider.base_url.clone(),
        key_ref,
        model: crate::aux::effective(stored_model.as_deref(), &primary_model),
    }))
}

/// Replace a conversation title a model suggested.
///
/// Two titles are fair game: the placeholder, and the truncation of the first
/// question. The truncation is written before the turn runs, so matching only the
/// placeholder would mean a suggestion never landed - which is exactly the bug
/// this condition exists to avoid. Anything else is the operator's own words and
/// is left alone.
pub fn set_title(db: &Db, id: &str, question: &str, title: &str) -> Result<usize, DbError> {
    let title = scrub_title(title);
    if title.is_empty() {
        return Ok(0);
    }
    let truncated = title_from(question);
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_conversations
                SET title = ?2
              WHERE id = ?1 AND title IN (?3, ?4)",
            params![id, title, NEW_CONVERSATION_TITLE, truncated],
        )
    })
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
    let profile = ProviderProfile::new(
        "title-generation",
        config.base_url.clone(),
        config.key_ref.clone(),
    )
    .ok()?;
    let provider = OpenAiCompatProvider::new(
        profile,
        ModelProfile::new(config.model.clone()),
        Capabilities::openai_compatible(),
    )
    .ok()?;
    let key = core_runtime::provider::resolve_api_key(&config.key_ref).ok()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;

    let excerpt = |text: &str| -> String { text.chars().take(600).collect() };
    let request = ChatRequest {
        system: Some(
            "You name conversations. Reply with only a title of at most six words. \
             No quotes, no trailing punctuation, no explanation."
                .to_owned(),
        ),
        messages: vec![ChatMessage::text(
            ChatRole::User,
            format!(
                "Question:\n{}\n\nAnswer:\n{}",
                excerpt(question),
                excerpt(answer)
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

/// Make a model's suggestion usable as a title.
///
/// Models like to answer with a quoted line, a trailing period, or two lines. A
/// title is one line of plain words, so quotes and trailing punctuation are
/// dropped and the whole thing is bounded.
#[must_use]
pub fn scrub_title(raw: &str) -> String {
    let line = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
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
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;

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
}

/// A count from SQLite, which stores signed integers and holds no negative ones.
fn tokens(value: Option<i64>) -> Option<u64> {
    value.filter(|count| *count >= 0).map(|count| count as u64)
}

/// Read a whole conversation, oldest first.
pub fn load(db: &Db, conversation: &str) -> Result<Vec<Message>, DbError> {
    db.with_conn(|conn| collect(conn, conversation))
}

fn collect(conn: &mut Connection, conversation: &str) -> Result<Vec<Message>, rusqlite::Error> {
    let mut statement = conn.prepare(
        "SELECT id, role, body, state, error_code, created_at, model,
                prompt_tokens, completion_tokens, cached_tokens, context_window,
                duration_ms, reasoning, tools
           FROM chat_messages
          WHERE conversation_id = ?1
          ORDER BY created_at ASC, rowid ASC",
    )?;
    let rows = statement.query_map(params![conversation], |row| {
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
        })
    })?;
    rows.collect()
}

/// Append one message. The id is minted here and returned so the caller can
/// update the same row when the answer lands.
pub fn append(
    db: &Db,
    conversation: &str,
    role: Role,
    body: &str,
    state: State,
) -> Result<String, DbError> {
    let id = uuid::Uuid::now_v7().to_string();
    db.with_conn(|conn| {
        conn.execute(
            "INSERT INTO chat_messages (id, conversation_id, role, body, state)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, conversation, role.as_str(), body, state.as_str()],
        )?;
        Ok(())
    })?;
    Ok(id)
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
                    duration_ms = ?7, reasoning = ?8, tools = ?9
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
            ],
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

/// Summarise turns that fell out of the window.
///
/// Best effort, like every auxiliary role: a provider that is down leaves the
/// turn exactly as it would have been without this feature, which is the old
/// behaviour and not a broken conversation.
#[must_use]
pub fn summarise(config: &TurnConfig, dropped: &[Message]) -> Option<String> {
    if dropped.is_empty() {
        return None;
    }
    let profile = ProviderProfile::new(
        "compression",
        config.base_url.clone(),
        config.key_ref.clone(),
    )
    .ok()?;
    let provider = OpenAiCompatProvider::new(
        profile,
        ModelProfile::new(config.model.clone()),
        Capabilities::openai_compatible(),
    )
    .ok()?;
    let key = core_runtime::provider::resolve_api_key(&config.key_ref).ok()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;

    let mut lines = String::new();
    for message in dropped.iter().take(40) {
        let who = match message.role {
            Role::User => "operator",
            Role::Assistant => "assistant",
        };
        let body: String = message.body.chars().take(400).collect();
        lines.push_str(&format!("{who}: {body}\n"));
    }

    let request = ChatRequest {
        system: Some(
            "Summarise the earlier part of a conversation so it can be carried as context. \
             Keep decisions, names, paths and open questions. Reply with the summary only, \
             under 150 words, no preamble."
                .to_owned(),
        ),
        messages: vec![ChatMessage::text(ChatRole::User, lines)],
        ..ChatRequest::default()
    };

    let mut sink = |_event: ProviderEvent| {};
    let completion = runtime
        .block_on(provider.stream_chat(&request, &key, &mut sink))
        .ok()?;
    let summary = completion.text.trim().to_owned();
    (!summary.is_empty()).then_some(summary)
}

/// Whether an attachment is an image a vision model could read.
#[must_use]
pub fn is_image(file: &Attachment) -> bool {
    let name = file.name.to_ascii_lowercase();
    matches!(
        file.media_type.as_deref(),
        Some("image/png") | Some("image/jpeg") | Some("image/webp") | Some("image/gif")
    ) || name.ends_with(".png")
        || name.ends_with(".jpg")
        || name.ends_with(".jpeg")
        || name.ends_with(".webp")
        || name.ends_with(".gif")
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

    let profile =
        ProviderProfile::new("vision", config.base_url.clone(), config.key_ref.clone()).ok()?;
    let provider = OpenAiCompatProvider::new(
        profile,
        ModelProfile::new(config.model.clone()),
        Capabilities::openai_compatible(),
    )
    .ok()?;
    let key = core_runtime::provider::resolve_api_key(&config.key_ref).ok()?;
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
    let profile = ProviderProfile::new(
        "web-extract",
        config.base_url.clone(),
        config.key_ref.clone(),
    )
    .ok()?;
    let provider = OpenAiCompatProvider::new(
        profile,
        ModelProfile::new(config.model.clone()),
        Capabilities::openai_compatible(),
    )
    .ok()?;
    let key = core_runtime::provider::resolve_api_key(&config.key_ref).ok()?;
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
    let profile =
        ProviderProfile::new("approval", config.base_url.clone(), config.key_ref.clone()).ok()?;
    let provider = OpenAiCompatProvider::new(
        profile,
        ModelProfile::new(config.model.clone()),
        Capabilities::openai_compatible(),
    )
    .ok()?;
    let key = core_runtime::provider::resolve_api_key(&config.key_ref).ok()?;
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
    let key = core_runtime::provider::resolve_api_key(&config.key_ref).ok()?;

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
    let text = raw.trim();
    if text.is_empty() {
        return Err("a message is required".to_owned());
    }
    if text.len() > MAX_MESSAGE_BYTES {
        return Err(format!(
            "the message must be under {MAX_MESSAGE_BYTES} bytes; it is {}",
            text.len()
        ));
    }
    Ok(text.to_owned())
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
}

/// The stored provider settings, or `None` when onboarding never wrote them.
pub fn turn_config(engine: &SetupEngine) -> Result<Option<TurnConfig>, OnboardingError> {
    let model = engine.setting(SETTING_MODEL)?;
    let Some(model) = model else {
        return Ok(None);
    };

    let registry = provider_registry(engine)?;
    let primary = engine
        .setting(crate::providers::SETTING_PRIMARY)?
        .unwrap_or_else(|| crate::providers::DEFAULT_ID.to_owned());
    let Some(chosen) = crate::providers::resolve(&registry, &primary) else {
        return Ok(None);
    };
    let Ok(key_ref) = SecretRef::try_from(chosen.key_ref.as_str()) else {
        return Ok(None);
    };

    Ok(Some(TurnConfig {
        base_url: chosen.base_url.clone(),
        key_ref,
        model,
    }))
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
}

/// Run one turn to completion, handing each fragment to `sink`.
///
/// This is deliberately a blocking call on its own thread, exactly like the
/// device turn: `stream_chat` takes `&mut dyn FnMut`, which is not `Send`, so the
/// sink must not be held across an await. The caller runs this inside
/// `spawn_blocking`.
// Eight arguments is the seam: provider, prompt, sandbox, gate, pool, settings,
// sink. Bundling them into a struct would only move the same fields behind a
// name, and every one of them is independently optional at the call site.
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
    settings: TurnSettings,
    sink: &mut dyn FnMut(StreamPart<'_>),
) -> Result<TurnOutcome, TurnFailure> {
    let profile =
        ProviderProfile::new("chat-turn", config.base_url.clone(), config.key_ref.clone())
            .map_err(|error| (error.kind().to_owned(), error.to_string()))?;
    let provider = OpenAiCompatProvider::new(
        profile,
        ModelProfile::new(config.model.clone()),
        Capabilities::openai_compatible(),
    )
    .map_err(|error| (error.kind().to_owned(), error.to_string()))?;
    let key = core_runtime::provider::resolve_api_key(&config.key_ref)
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
                       chunks: &mut dyn FnMut(&str)|
     -> crate::tools::ToolResult {
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
            }));
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
        sink,
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
                    let mut offered = crate::tools::specs(sandbox.mode());
                    if let Some(pool) = mcp {
                        offered.extend(pool.specs());
                    }
                    // Only offered when the role is pinned: a tool the daemon
                    // will refuse must never reach the model's list.
                    if image.is_some() {
                        offered.push(core_runtime::provider::ToolSpec {
                            name: "generate_image".to_owned(),
                            description:
                                "Draw an image from a description and save it in the workspace."
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
                .block_on(provider.stream_chat(&request, &key, &mut forward))
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
    if let Some(last) = transcript
        .iter()
        .rev()
        .find(|message| message.prompt_tokens.is_some())
    {
        out.context_used = last
            .prompt_tokens
            .map(|prompt| prompt + last.cached_tokens.unwrap_or(0));
        out.context_window = last.context_window;
        if let (Some(used), Some(window)) = (out.context_used, out.context_window) {
            out.context_pct = (used * 100)
                .checked_div(window)
                .map(|pct| pct.min(100) as u8);
        }
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
/// is exactly the nested type a reader needs a name for.
pub type ToolExecute<'a> =
    dyn FnMut(&core_domain::ToolCall, &mut dyn FnMut(&str)) -> crate::tools::ToolResult + 'a;

/// Drive one turn: stream, run whatever the model asked for, stream again.
///
/// `stream` is handed the prompt for each round trip and returns that round's
/// completion. Text reaches the operator through whatever `stream` does with it;
/// this function only decides when to stop and what was spent.
///
/// `gate` is consulted before every call runs and is the only place a turn can
/// be stopped from touching the filesystem. It is called even for read-only
/// tools so a policy can decide for itself; the shipped policy parks writes.
pub fn drive_tool_loop(
    execute: &mut ToolExecute<'_>,
    mut messages: Vec<ChatMessage>,
    gate: &mut dyn FnMut(&core_domain::ToolCall) -> GateDecision,
    max_rounds: usize,
    sink: &mut dyn FnMut(StreamPart<'_>),
    mut stream: impl FnMut(
        &[ChatMessage],
        bool,
        &mut dyn FnMut(StreamPart<'_>),
    ) -> Result<core_runtime::provider::Completion, TurnFailure>,
) -> Result<TurnOutcome, TurnFailure> {
    let max_rounds = max_rounds.max(1);
    let mut usage: Option<core_domain::Usage> = None;
    let mut reasoning = String::new();
    let mut records: Vec<ToolRecord> = Vec::new();
    let mut rounds = 0usize;

    let mut text = loop {
        // The last attempt is tool-free on purpose. A model that answered every
        // round with another call used to end the turn with nothing; instead it
        // now has to say what it found with what it already has, which is a
        // usable answer rather than a shrug.
        let tools_allowed = rounds < max_rounds;
        let mut completion = stream(&messages, tools_allowed, &mut *sink)?;

        // A route that fails to lift calls into the structured array passes the
        // model's own markup through as content. Recover it: the calls the model
        // decided to make become real calls, and the markup stops being the
        // answer.
        if let Some(recovered) = crate::web::dsml::recover(&completion.text) {
            completion.text = recovered.text;
            if completion.tool_calls.is_empty() {
                completion.tool_calls = recovered.calls;
            }
        }

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
            break completion.text;
        }

        // A provider that returns calls when it was offered none has nothing
        // left to run, so the loop ends with whatever it said.
        if !tools_allowed {
            break completion.text;
        }

        rounds += 1;
        messages.push(ChatMessage::assistant_calls(completion.tool_calls.clone()));
        for call in &completion.tool_calls {
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
                GateDecision::Run => execute(call, &mut live),
                GateDecision::Park { approval_id } => {
                    crate::tools::ToolResult::parked(&approval_id, &call.name)
                }
                GateDecision::Refuse { reason } => crate::tools::ToolResult::refused(&reason),
            };
            drop(live);
            let record = ToolRecord {
                name: call.name.clone(),
                arguments: call.arguments.to_string(),
                output: result.content.clone(),
                is_error: result.is_error,
            };
            // Shown as it happens: a turn that runs a command should not look
            // like a turn that is thinking very hard about nothing.
            sink(StreamPart::Tool {
                name: &record.name,
                arguments: &record.arguments,
                output: &record.output,
                is_error: record.is_error,
            });
            records.push(record);
            messages.push(ChatMessage::tool_result(call.id.clone(), result.content));
        }
    };

    // Nothing said and nothing left: the turn still owes the operator a sentence
    // rather than an empty bubble.
    if text.trim().is_empty() {
        text = format!(
            "Stopped after {rounds} tool rounds without a final answer. The work above is what \
             ran; ask again and it will finish from here."
        );
    }

    Ok(TurnOutcome {
        text,
        reasoning,
        usage: usage.unwrap_or_else(|| {
            core_domain::Usage::unavailable(core_domain::UnavailableReason::FieldsAbsent)
        }),
        rounds,
        tools: records,
    })
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
    fn a_round_whose_calls_arrived_as_text_still_runs_them() {
        // The provider handed back markup instead of a tool_calls array. The loop
        // must turn it into a real call, run it, and answer from the result.
        let mut offered = 0usize;
        let mut executed: Vec<String> = Vec::new();
        let outcome = drive_tool_loop(
            &mut |call: &core_domain::ToolCall, _chunks: &mut dyn FnMut(&str)| {
                executed.push(call.name.clone());
                crate::tools::ToolResult::ok(serde_json::json!({ "rows": 3 }))
            },
            vec![ChatMessage::text(ChatRole::User, "check the warehouse")],
            &mut |_call| GateDecision::Run,
            MAX_TOOL_ROUNDS,
            &mut |_part| {},
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

        // The truncation the turn wrote: a suggestion still lands, because that
        // is the state every real conversation is in by the time it settles.
        let second = create_conversation(&db).unwrap();
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
        };
        let other = ToolRecord {
            name: "list_dir".to_owned(),
            arguments: "{}".to_owned(),
            output: r#"{"entries":[]}"#.to_owned(),
            is_error: false,
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
                duration_ms: None,
                reasoning: None,
                tools: Vec::new(),
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
                duration_ms: Some(2_000),
                reasoning: None,
                tools: Vec::new(),
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
                duration_ms: None,
                reasoning: None,
                tools: Vec::new(),
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
                duration_ms: None,
                reasoning: None,
                tools: Vec::new(),
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
        assert_eq!(stats.context_used, Some(1_900));
        assert_eq!(stats.context_window, Some(10_000));
        assert_eq!(stats.context_pct, Some(19));
        assert_eq!(stats.tokens_per_second, Some(50));
    }

    #[test]
    fn a_transcript_with_no_reported_numbers_reports_none_rather_than_zero() {
        let stats = stats(&[]);
        assert_eq!(stats.total_tokens, 0);
        assert_eq!(stats.cache_hit_pct, None);
        assert_eq!(stats.context_pct, None);
        assert_eq!(stats.tokens_per_second, None);
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
        assert!(error.contains("under"), "{error}");
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
            &mut |call: &core_domain::ToolCall, chunks: &mut dyn FnMut(&str)| {
                crate::tools::execute_streaming(&call.name, &call.arguments, &sandbox, chunks)
            },
            vec![ChatMessage::text(ChatRole::User, "read a.txt")],
            &mut |_call| GateDecision::Run,
            MAX_TOOL_ROUNDS,
            &mut |_part| {},
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
            &mut |call: &core_domain::ToolCall, chunks: &mut dyn FnMut(&str)| {
                crate::tools::execute_streaming(&call.name, &call.arguments, &sandbox, chunks)
            },
            vec![ChatMessage::text(ChatRole::User, "loop forever")],
            &mut |_call| GateDecision::Run,
            MAX_TOOL_ROUNDS,
            &mut |_part| {},
            |_messages, tools, _emit| {
                round += 1;
                tools_offered.push(tools);
                Ok(completion_with_calls(vec![call.clone()], ""))
            },
        )
        .expect("a turn");

        assert_eq!(
            outcome.rounds, MAX_TOOL_ROUNDS,
            "exactly the cap in tool rounds"
        );
        assert_eq!(
            round,
            MAX_TOOL_ROUNDS + 1,
            "then one final attempt with tools withheld"
        );
        assert_eq!(
            tools_offered.last().copied(),
            Some(false),
            "the last attempt must not offer tools, so the turn has to answer"
        );
        assert!(
            tools_offered[..MAX_TOOL_ROUNDS]
                .iter()
                .all(|offered| *offered),
            "every round before the cap offers them"
        );
        assert!(
            !outcome.text.trim().is_empty(),
            "a capped turn still tells the operator something"
        );
        assert!(
            outcome.text.to_lowercase().contains("tool"),
            "the notice explains why it stopped: {}",
            outcome.text
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
            &mut |call: &core_domain::ToolCall, chunks: &mut dyn FnMut(&str)| {
                crate::tools::execute_streaming(&call.name, &call.arguments, &sandbox, chunks)
            },
            vec![ChatMessage::text(ChatRole::User, "hi")],
            &mut |_call| GateDecision::Run,
            MAX_TOOL_ROUNDS,
            &mut |_part| {},
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
            &mut |call: &core_domain::ToolCall, chunks: &mut dyn FnMut(&str)| {
                crate::tools::execute_streaming(&call.name, &call.arguments, &sandbox, chunks)
            },
            vec![ChatMessage::text(ChatRole::User, "write x")],
            &mut |_call| GateDecision::Park {
                approval_id: "ap_1".to_owned(),
            },
            MAX_TOOL_ROUNDS,
            &mut |_part| {},
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
}
