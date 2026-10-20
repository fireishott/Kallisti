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

/// One conversation the rail can list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conversation {
    /// Opaque id, also the transcript key.
    pub id: String,
    /// What the operator reads.
    pub title: String,
    /// The database stamp of the last message, for ordering and for the rail.
    pub updated_at: String,
}

/// The newest conversations, newest first.
pub fn conversations(db: &Db, limit: usize) -> Result<Vec<Conversation>, DbError> {
    db.with_conn(|conn| {
        let mut statement = conn.prepare(
            "SELECT id, title, updated_at
               FROM chat_conversations
              ORDER BY updated_at DESC, rowid DESC
              LIMIT ?1",
        )?;
        let rows = statement.query_map(params![limit as i64], |row| {
            Ok(Conversation {
                id: row.get(0)?,
                title: row.get(1)?,
                updated_at: row.get(2)?,
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
                duration_ms
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
pub fn settle(
    db: &Db,
    id: &str,
    body: &str,
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
    db.with_conn(|conn| {
        conn.execute(
            "UPDATE chat_messages
                SET body = ?2, state = 'complete', usage_json = ?3, error_code = NULL,
                    prompt_tokens = ?4, completion_tokens = ?5, cached_tokens = ?6,
                    duration_ms = ?7
              WHERE id = ?1",
            params![
                id,
                body,
                usage,
                prompt_tokens.map(|count| count as i64),
                completion_tokens.map(|count| count as i64),
                cached_tokens.map(|count| count as i64),
                duration_ms.map(|count| count as i64),
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
    ChatMessage {
        role,
        content: message.body.clone(),
    }
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
    let base_url = engine.setting(SETTING_PROVIDER_BASE_URL)?;
    let key_ref = engine.setting(SETTING_PROVIDER_KEY_REF)?;
    let model = engine.setting(SETTING_MODEL)?;
    let (Some(base_url), Some(key_ref), Some(model)) = (base_url, key_ref, model) else {
        return Ok(None);
    };
    let Ok(key_ref) = SecretRef::try_from(key_ref.as_str()) else {
        return Ok(None);
    };
    Ok(Some(TurnConfig {
        base_url,
        key_ref,
        model,
    }))
}

/// Why a turn could not be sent, in the two halves the page shows: a stable code
/// and a sentence.
pub type TurnFailure = (String, String);

/// Run one turn to completion, handing each text fragment to `sink`.
///
/// This is deliberately a blocking call on its own thread, exactly like the
/// device turn: `stream_chat` takes `&mut dyn FnMut`, which is not `Send`, so the
/// sink must not be held across an await. The caller runs this inside
/// `spawn_blocking`.
pub fn run_turn(
    config: &TurnConfig,
    system: Option<String>,
    messages: Vec<ChatMessage>,
    sink: &mut dyn FnMut(&str),
) -> Result<(String, core_domain::Usage), TurnFailure> {
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

    let request = ChatRequest {
        system: system.filter(|text| !text.trim().is_empty()),
        messages,
        ..ChatRequest::default()
    };

    let mut forward = |event: ProviderEvent| {
        if let ProviderEvent::Text(fragment) = event
            && !fragment.is_empty()
        {
            sink(&fragment);
        }
    };

    let completion = runtime
        .block_on(provider.stream_chat(&request, &key, &mut forward))
        .map_err(|error| (error.kind().to_owned(), error.to_string()))?;

    Ok((completion.text, completion.usage))
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
    fn a_streaming_row_settles_with_its_usage_and_a_failure_keeps_its_text() {
        let db = db();
        let id = append(&db, CONVERSATION_ID, Role::Assistant, "", State::Streaming).unwrap();
        settle(
            &db,
            &id,
            "the answer",
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
    fn an_empty_message_is_refused_by_name_and_the_ceiling_is_bytes() {
        assert_eq!(accept_text("  \n ").unwrap_err(), "a message is required");
        assert_eq!(accept_text("  hi  ").unwrap(), "hi");

        let too_long = "x".repeat(MAX_MESSAGE_BYTES + 1);
        let error = accept_text(&too_long).unwrap_err();
        assert!(error.contains("under"), "{error}");
    }
}
