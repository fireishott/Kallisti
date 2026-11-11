//! Transcript search over this daemon's own history.
//!
//! `chat_messages` holds every turn this daemon has ever taken, and until this
//! module existed the only way to read it was to load one conversation and scan
//! it. That made "we solved this before" a guess. Migration 0025 puts an FTS5
//! external-content index over `body`, kept current by triggers on the table
//! itself, so no write path can forget it and the index cannot drift from the
//! rows it describes.
//!
//! The search is SQLite's, not a model call. One query costs about 20 ms on this
//! host and spends no tokens, which is the whole point: recalling what was
//! already worked out must be cheaper than working it out again.
//!
//! Two shapes, one tool:
//!
//! - **Discovery** (no `conversation`): which conversations matched, best
//!   snippet each, for the "where did we talk about this" question.
//! - **Scroll** (`conversation` set): a window of that transcript, optionally
//!   filtered by `text`, for the "show me that part" follow-up.
//!
//! Nothing here writes. The tool is offered in every sandbox mode because a
//! read-only search cannot reach past one.

use crate::db::{Db, DbError};
use crate::tools::ToolResult;

/// The tool name the model calls.
pub const TOOL: &str = "search_sessions";

/// Hits returned when the caller does not say.
pub const DEFAULT_LIMIT: usize = 8;

/// The most any one call will return, so a broad word cannot flood a turn.
pub const MAX_LIMIT: usize = 25;

/// Messages returned by one scroll window.
pub const DEFAULT_WINDOW: usize = 20;

/// How much of a body a scroll row carries. Long enough to recognise the
/// message, short enough that a window of twenty still fits in a prompt.
const SCROLL_BODY_CHARS: usize = 400;

/// One transcript match.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// `chat_messages.id`.
    pub message_id: String,
    /// The conversation it belongs to.
    pub conversation_id: String,
    /// That conversation's title, or the id when the row is missing.
    pub title: String,
    /// `user` or `assistant`.
    pub role: String,
    /// When it was written, ISO 8601 UTC.
    pub created_at: String,
    /// The excerpt, with `[` around the matched terms.
    pub snippet: String,
    /// `bm25()` for the row. Lower is better; it is negative.
    pub rank: f64,
}

/// One conversation that matched, for the discovery shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationHit {
    /// The conversation.
    pub conversation_id: String,
    /// Its title.
    pub title: String,
    /// The best-scoring excerpt in it.
    pub snippet: String,
    /// When the best-scoring message was written.
    pub created_at: String,
    /// How many messages in this conversation matched the query.
    pub matches: i64,
}

/// One message in a scroll window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The row id.
    pub id: String,
    /// `user` or `assistant`.
    pub role: String,
    /// When it was written.
    pub created_at: String,
    /// The body, clipped.
    pub body: String,
    /// The match terms, when the window was filtered by `text`.
    pub snippet: Option<String>,
}

/// The tool spec, offered beside `read_file`.
#[must_use]
pub fn spec() -> core_runtime::provider::ToolSpec {
    core_runtime::provider::ToolSpec {
        name: TOOL.to_owned(),
        description: "Search every past conversation in this daemon's transcript. \
                      Returns the conversations that match, with an excerpt and a \
                      timestamp; pass `conversation` to read a window of one. \
                      Use this before asking the operator to repeat something."
            .to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "Words to look for. All of them first, then any of them when that finds nothing. Omit when scrolling a conversation."
                },
                "conversation": {
                    "type": "string",
                    "description": "Conversation id to scroll. Returns a window of that transcript instead of a discovery list."
                },
                "before": {
                    "type": "string",
                    "description": "Scroll only: ISO 8601 timestamp; return messages written before it."
                },
                "after": {
                    "type": "string",
                    "description": "Scroll only: ISO 8601 timestamp; return messages written after it."
                },
                "limit": {
                    "type": "integer",
                    "description": format!("How many results, 1 to {MAX_LIMIT}. Defaults to {DEFAULT_LIMIT} for discovery and {DEFAULT_WINDOW} for a window.")
                }
            },
            "required": []
        }),
    }
}

/// An access to the transcript, handed to one turn.
///
/// Mirrors [`crate::skills::Access`]: the database is the only thing a search
/// needs, and holding it here keeps the turn's plumbing the same shape as the
/// skill library's.
#[derive(Debug, Clone)]
pub struct Access {
    /// The daemon's database.
    pub db: Db,
}

impl Access {
    /// Resolve an access from a database.
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Answer one call.
    #[must_use]
    pub fn call(&self, arguments: &serde_json::Value) -> ToolResult {
        let text = arguments
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .trim();
        let conversation = arguments
            .get("conversation")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let limit = arguments
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map(|value| value as usize);

        if let Some(conversation) = conversation {
            let before = arguments.get("before").and_then(serde_json::Value::as_str);
            let after = arguments.get("after").and_then(serde_json::Value::as_str);
            let window = limit.unwrap_or(DEFAULT_WINDOW).clamp(1, MAX_LIMIT);
            return match scroll(&self.db, conversation, text, before, after, window) {
                Ok(messages) => ToolResult::ok(serde_json::json!({
                    "conversation": conversation,
                    "messages": messages.iter().map(message_json).collect::<Vec<_>>(),
                    "returned": messages.len(),
                })),
                Err(error) => {
                    ToolResult::error(format!("the transcript could not be read: {error}"))
                }
            };
        }

        if text.is_empty() {
            return ToolResult::refused(
                "searching needs either `text` to look for or `conversation` to scroll; \
                 nothing was searched",
            );
        }
        let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        match discover(&self.db, text, limit) {
            Ok(hits) => ToolResult::ok(serde_json::json!({
                "query": text,
                "conversations": hits.iter().map(conversation_json).collect::<Vec<_>>(),
                "returned": hits.len(),
            })),
            Err(error) => {
                ToolResult::error(format!("the transcript could not be searched: {error}"))
            }
        }
    }
}

fn conversation_json(hit: &ConversationHit) -> serde_json::Value {
    serde_json::json!({
        "conversation": hit.conversation_id,
        "title": hit.title,
        "snippet": hit.snippet,
        "created_at": hit.created_at,
        "matches": hit.matches,
    })
}

fn message_json(message: &Message) -> serde_json::Value {
    serde_json::json!({
        "id": message.id,
        "role": message.role,
        "created_at": message.created_at,
        "body": message.body,
        "snippet": message.snippet,
    })
}

/// Turn operator or model text into an FTS5 expression that cannot be a syntax
/// error.
///
/// Every token is quoted, so a stray `*`, `-`, `NEAR(` or unbalanced quote in
/// the caller's words is data rather than grammar. Tokens shorter than four
/// characters are dropped: `a`, `is` and `the` match almost every row and would
/// let a common word crowd the result list. When a query would end up empty after
/// that, the original tokens are kept so a deliberate short search still works.
fn expression(text: &str, and: bool) -> Option<String> {
    let tokens: Vec<&str> = text
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();
    if tokens.is_empty() {
        return None;
    }
    let long: Vec<&str> = tokens
        .iter()
        .copied()
        .filter(|token| token.chars().count() >= 4)
        .collect();
    let chosen = if long.is_empty() { tokens } else { long };
    let joiner = if and { " AND " } else { " OR " };
    Some(
        chosen
            .into_iter()
            .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(joiner),
    )
}

/// Which conversations matched `text`, best first.
///
/// All words are tried first, then any word. A precise query is what the caller
/// usually means, but "warehouse sync failed" and "the warehouse sync is
/// failing" do not share a stem, so an all-words-only search misses the very
/// thing the operator remembers writing.
pub fn discover(db: &Db, text: &str, limit: usize) -> Result<Vec<ConversationHit>, DbError> {
    for and in [true, false] {
        let Some(expression) = expression(text, and) else {
            return Ok(Vec::new());
        };
        let hits = run(db, &expression, limit)?;
        if !hits.is_empty() {
            return Ok(group(hits, limit));
        }
    }
    Ok(Vec::new())
}

/// The raw message hits for one expression, best first.
fn run(db: &Db, expression: &str, limit: usize) -> Result<Vec<Hit>, DbError> {
    const SQL: &str = "
        SELECT m.id,
               m.conversation_id,
               COALESCE(c.title, m.conversation_id),
               m.role,
               m.created_at,
               snippet(chat_messages_fts, 0, '[', ']', '...', 12),
               bm25(chat_messages_fts)
          FROM chat_messages_fts
          JOIN chat_messages m ON m.rowid = chat_messages_fts.rowid
          LEFT JOIN chat_conversations c ON c.id = m.conversation_id
         WHERE chat_messages_fts MATCH ?1
           AND m.state = 'complete'
         ORDER BY bm25(chat_messages_fts)
         LIMIT ?2";
    db.with_conn(|conn| {
        let mut statement = conn.prepare(SQL)?;
        let rows = statement.query_map(rusqlite::params![expression, limit as i64], |row| {
            Ok(Hit {
                message_id: row.get(0)?,
                conversation_id: row.get(1)?,
                title: row.get(2)?,
                role: row.get(3)?,
                created_at: row.get(4)?,
                snippet: row.get(5)?,
                rank: row.get(6)?,
            })
        })?;
        rows.collect()
    })
}

/// Collapse message hits into one row per conversation, keeping the best
/// snippet and counting the rest.
///
/// `limit` bounds the conversations returned, not the messages scanned: the
/// scan is already capped by the query, and a conversation with eight hits
/// deserves one line in the answer, not eight.
fn group(hits: Vec<Hit>, limit: usize) -> Vec<ConversationHit> {
    let mut grouped: Vec<ConversationHit> = Vec::new();
    for hit in hits {
        match grouped
            .iter_mut()
            .find(|existing| existing.conversation_id == hit.conversation_id)
        {
            Some(existing) => existing.matches += 1,
            None => grouped.push(ConversationHit {
                conversation_id: hit.conversation_id,
                title: hit.title,
                snippet: hit.snippet,
                created_at: hit.created_at,
                matches: 1,
            }),
        }
    }
    grouped.truncate(limit);
    grouped
}

/// A window of one conversation, optionally filtered by `text`.
pub fn scroll(
    db: &Db,
    conversation: &str,
    text: &str,
    before: Option<&str>,
    after: Option<&str>,
    limit: usize,
) -> Result<Vec<Message>, DbError> {
    // A filtered window is a different query from an unfiltered one: the first
    // has to reach the index, the second only the table. Writing them separately
    // keeps both index-backed instead of stacking `OR` clauses that defeat the
    // planner.
    if !text.is_empty() {
        let Some(expression) = expression(text, false) else {
            return Ok(Vec::new());
        };
        const SQL: &str = "
            SELECT m.id, m.role, m.created_at, m.body,
                   snippet(chat_messages_fts, 0, '[', ']', '...', 12)
              FROM chat_messages_fts
              JOIN chat_messages m ON m.rowid = chat_messages_fts.rowid
             WHERE chat_messages_fts MATCH ?1
               AND m.conversation_id = ?2
               AND (?3 IS NULL OR m.created_at < ?3)
               AND (?4 IS NULL OR m.created_at > ?4)
             ORDER BY m.created_at DESC, m.rowid DESC
             LIMIT ?5";
        return db.with_conn(|conn| {
            let mut statement = conn.prepare(SQL)?;
            let rows = statement.query_map(
                rusqlite::params![expression, conversation, before, after, limit as i64],
                |row| {
                    let body: String = row.get(3)?;
                    Ok(Message {
                        id: row.get(0)?,
                        role: row.get(1)?,
                        created_at: row.get(2)?,
                        body: clip(&body),
                        snippet: row.get(4)?,
                    })
                },
            )?;
            rows.collect()
        });
    }

    const SQL: &str = "
        SELECT id, role, created_at, body
          FROM chat_messages
         WHERE conversation_id = ?1
           AND (?2 IS NULL OR created_at < ?2)
           AND (?3 IS NULL OR created_at > ?3)
         ORDER BY created_at DESC, rowid DESC
         LIMIT ?4";
    db.with_conn(|conn| {
        let mut statement = conn.prepare(SQL)?;
        let rows = statement.query_map(
            rusqlite::params![conversation, before, after, limit as i64],
            |row| {
                let body: String = row.get(3)?;
                Ok(Message {
                    id: row.get(0)?,
                    role: row.get(1)?,
                    created_at: row.get(2)?,
                    body: clip(&body),
                    snippet: None,
                })
            },
        )?;
        rows.collect()
    })
}

/// Clip a body for a window, on a line boundary when there is one, and say that
/// something was dropped rather than pretending the message ended there.
fn clip(body: &str) -> String {
    let trimmed = body.trim();
    let mut end = 0;
    for (count, (index, character)) in trimmed.char_indices().enumerate() {
        if count >= SCROLL_BODY_CHARS {
            end = index;
            break;
        }
        if character == '\n' && count > SCROLL_BODY_CHARS / 2 {
            return format!("{}\n[... clipped]", &trimmed[..index]);
        }
    }
    if end == 0 {
        return trimmed.to_owned();
    }
    format!("{} [... clipped]", &trimmed[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seeded() -> Db {
        let db = Db::open_in_memory().expect("in-memory database");
        db.with_conn(|conn| {
            conn.execute_batch(
                "INSERT INTO chat_conversations (id, title) VALUES ('c1', 'Warehouse lane health');
                 INSERT INTO chat_conversations (id, title) VALUES ('c2', 'Kitchen shopping');
                 INSERT INTO chat_messages (id, conversation_id, role, body)
                   VALUES ('m1', 'c1', 'user', 'the warehouse refresh keeps failing at 03:00');
                 INSERT INTO chat_messages (id, conversation_id, role, body)
                   VALUES ('m2', 'c1', 'assistant', 'the warehouse refresh was fixed by widening the timeout');
                 INSERT INTO chat_messages (id, conversation_id, role, body)
                   VALUES ('m3', 'c2', 'user', 'we need oat milk and coffee');",
            )
        })
        .expect("seed");
        db
    }

    #[test]
    fn migration_builds_an_index_as_large_as_the_table() {
        let db = seeded();
        let (indexed, rows): (i64, i64) = db
            .with_conn(|conn| {
                Ok((
                    conn.query_row("SELECT count(*) FROM chat_messages_fts", [], |row| {
                        row.get(0)
                    })?,
                    conn.query_row("SELECT count(*) FROM chat_messages", [], |row| row.get(0))?,
                ))
            })
            .expect("count");
        assert_eq!(
            indexed, rows,
            "the external-content index must match the table"
        );
        assert!(db.schema_version().expect("version") >= 25);
    }

    #[test]
    fn triggers_keep_the_index_current_on_insert_update_and_delete() {
        let db = seeded();
        let count = |db: &Db| -> i64 {
            db.with_conn(|conn| {
                conn.query_row("SELECT count(*) FROM chat_messages_fts", [], |row| {
                    row.get(0)
                })
            })
            .expect("count")
        };
        let before = count(&db);

        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO chat_messages (id, conversation_id, role, body)
                 VALUES ('m4', 'c1', 'user', 'a brand new sentence about the loading dock')",
                [],
            )
        })
        .expect("insert");
        assert_eq!(count(&db), before + 1, "an insert must reach the index");

        // The new row is findable, and stops being findable after an update that
        // removes the word.
        db.with_conn(|conn| {
            conn.execute(
                "UPDATE chat_messages SET body = 'nothing relevant here' WHERE id = 'm4'",
                [],
            )
        })
        .expect("update");
        let found = discover(&db, "dock", 5).expect("search");
        assert!(
            found.is_empty(),
            "an update must move the index with the row"
        );

        db.with_conn(|conn| conn.execute("DELETE FROM chat_messages WHERE id = 'm4'", []))
            .expect("delete");
        assert_eq!(count(&db), before, "a delete must leave the index");
    }

    #[test]
    fn a_match_returns_the_message_and_a_non_match_returns_nothing() {
        let db = seeded();
        let hits = discover(&db, "warehouse refresh", 5).expect("search");
        assert_eq!(hits.len(), 1, "both messages are in one conversation");
        assert_eq!(hits[0].conversation_id, "c1");
        assert_eq!(hits[0].title, "Warehouse lane health");
        assert_eq!(hits[0].matches, 2);
        assert!(
            hits[0].snippet.contains('[') && hits[0].snippet.contains(']'),
            "the snippet marks the match: {}",
            hits[0].snippet
        );
        assert!(discover(&db, "zeppelin", 5).expect("search").is_empty());
    }

    #[test]
    fn an_all_words_miss_falls_back_to_any_word() {
        let db = seeded();
        // "failing" and "fixed" share no token, so all-words finds nothing and
        // the fallback finds the conversation that holds both ideas.
        let hits = discover(&db, "failing fixed", 5).expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].conversation_id, "c1");
        assert_eq!(hits[0].matches, 2);
    }

    #[test]
    fn hostile_punctuation_is_data_not_grammar() {
        let db = seeded();
        for query in [
            "warehouse*",
            "NEAR(",
            "\"unbalanced",
            "a OR b",
            "-refresh",
            "",
        ] {
            let result = discover(&db, query, 5);
            assert!(result.is_ok(), "query {query:?} must not be a syntax error");
        }
    }

    #[test]
    fn scrolling_a_conversation_returns_a_window_and_clips() {
        let db = seeded();
        let window = scroll(&db, "c1", "", None, None, 10).expect("scroll");
        assert_eq!(window.len(), 2);
        assert!(window.iter().all(|message| message.snippet.is_none()));

        let filtered = scroll(&db, "c1", "timeout", None, None, 10).expect("scroll");
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "m2");
        assert!(filtered[0].snippet.is_some());
    }

    #[test]
    fn a_long_body_is_clipped_and_says_so() {
        let long = "x".repeat(SCROLL_BODY_CHARS * 2);
        let clipped = clip(&long);
        assert!(clipped.ends_with("[... clipped]"));
        assert!(clipped.len() < long.len() + 20);
        assert_eq!(clip("short"), "short");
    }

    #[test]
    fn the_expression_drops_short_words_but_keeps_a_short_query() {
        assert_eq!(
            expression("the warehouse", true).as_deref(),
            Some("\"warehouse\"")
        );
        assert_eq!(
            expression("is it", true).as_deref(),
            Some("\"is\" AND \"it\"")
        );
        assert_eq!(
            expression("is it", false).as_deref(),
            Some("\"is\" OR \"it\"")
        );
        assert_eq!(expression("a b", false).as_deref(), Some("\"a\" OR \"b\""));
        assert_eq!(expression("   ", true), None);
    }

    #[test]
    fn the_tool_answers_both_shapes_and_refuses_an_empty_call() {
        let access = Access::new(seeded());

        let discovery = access.call(&serde_json::json!({ "text": "warehouse" }));
        assert!(!discovery.is_error);
        assert!(discovery.content.contains("Warehouse lane health"));

        let window = access.call(&serde_json::json!({ "conversation": "c2" }));
        assert!(!window.is_error);
        assert!(window.content.contains("oat milk"));

        let empty = access.call(&serde_json::json!({}));
        assert!(
            empty.is_error,
            "a call with no shape must not report success"
        );
        assert!(empty.content.contains("nothing was searched"));
    }

    #[test]
    fn limit_is_clamped() {
        let db = seeded();
        let hits = discover(&db, "warehouse", MAX_LIMIT + 500).expect("search");
        assert!(hits.len() <= MAX_LIMIT);
    }
}
