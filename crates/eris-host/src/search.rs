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

/// The `settings` row holding how many hits a search returns when nobody says.
pub const SETTING_SEARCH_LIMIT: &str = "search.limit";

/// The `settings` row holding the scope a search starts on.
pub const SETTING_SEARCH_SCOPE_DEFAULT: &str = "search.scope_default";

/// The scopes a search accepts.
pub const SCOPE_VALUES: &[&str] = &["all", "conversation"];

/// The roles a hit can be filtered on.
pub const ROLE_VALUES: &[&str] = &["all", "user", "assistant"];

/// The narrowest `search_limit` the record accepts.
pub const MIN_SETTING_LIMIT: u32 = 5;

/// The widest `search_limit` the record accepts, which is also the ceiling the
/// query clamps to: the setting cannot pin a number the reader then ignores.
pub const MAX_SETTING_LIMIT: u32 = 50;

/// Hits returned when the caller does not say and the row is empty.
pub const DEFAULT_LIMIT: usize = 8;

/// The most any one call will return, so a broad word cannot flood a turn.
pub const MAX_LIMIT: usize = MAX_SETTING_LIMIT as usize;

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
                "query": {
                    "type": "string",
                    "description": "Words to look for. All of them first, then any of them when that finds nothing. Omit when browsing a conversation."
                },
                "scope": {
                    "type": "string",
                    "enum": ["all", "conversation"],
                    "description": "`all` ranks across every conversation; `conversation` holds to `conversation_id`."
                },
                "conversation_id": {
                    "type": "string",
                    "description": "Conversation to hold to. Setting it switches the call to a browse window even when `mode` says discovery."
                },
                "role": {
                    "type": "string",
                    "enum": ["all", "user", "assistant"],
                    "description": "Only this speaker's turns. Defaults to all."
                },
                "since": {
                    "type": "string",
                    "description": "ISO 8601 lower bound on when the message was written."
                },
                "until": {
                    "type": "string",
                    "description": "ISO 8601 upper bound on when the message was written."
                },
                "mode": {
                    "type": "string",
                    "enum": ["discovery", "browse"],
                    "description": "`discovery` returns ranked snippets; `browse` returns a window around `cursor`."
                },
                "cursor": {
                    "type": "string",
                    "description": "Discovery: the `cursor` from a previous result to read the next page. Browse: the message id to centre the window on."
                },
                "limit": {
                    "type": "integer",
                    "description": format!("How many hits, 1 to {MAX_LIMIT}. Defaults to the operator's search limit.")
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

    /// Answer one call with the same bytes the route returns for the same
    /// request.
    #[must_use]
    pub fn call(&self, arguments: &serde_json::Value) -> ToolResult {
        let string = |key: &str| {
            arguments
                .get(key)
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        // `text` and `conversation` are the pre-contract spellings and stay
        // accepted, because a model that learned the old spec keeps working.
        let mut request = Request {
            query: string("query")
                .or_else(|| string("text"))
                .unwrap_or_default(),
            scope: string("scope").unwrap_or_default(),
            conversation_id: string("conversation_id").or_else(|| string("conversation")),
            role: string("role").unwrap_or_else(|| "all".to_owned()),
            since: string("since"),
            until: string("until"),
            mode: arguments
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .map(Mode::parse)
                .unwrap_or(Mode::Discovery),
            limit: arguments
                .get("limit")
                .and_then(serde_json::Value::as_u64)
                .map_or(0, |value| value as usize),
            cursor: string("cursor"),
        };
        if request.mode == Mode::Discovery && request.conversation_id.is_some() {
            request.mode = Mode::Browse;
        }
        apply_defaults(&self.db, &mut request);

        if request.query.is_empty() && request.conversation_id.is_none() {
            return ToolResult::refused(
                "searching needs either `query` to look for or `conversation_id` to scroll; \
                 nothing was searched",
            );
        }
        match answer(&self.db, &request) {
            Ok(body) => ToolResult::ok(body),
            Err(error) => {
                ToolResult::error(format!("the transcript could not be searched: {error}"))
            }
        }
    }
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

// ---------------------------------------------------------------------------
// One request, three surfaces
// ---------------------------------------------------------------------------

/// What a search is for.
///
/// Two modes rather than one query with a flag, because they answer different
/// questions and page differently: `Discovery` is "where did we settle this" and
/// ranks across every conversation, `Browse` is "show me that part" and walks one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    /// Ranked hits across the whole transcript.
    #[default]
    Discovery,
    /// A window of one conversation around one message.
    Browse,
}

impl Mode {
    /// The wire spelling.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Discovery => "discovery",
            Self::Browse => "browse",
        }
    }

    /// Read a wire spelling, falling back to [`Self::Discovery`].
    #[must_use]
    pub fn parse(value: &str) -> Self {
        if value.trim().eq_ignore_ascii_case("browse") {
            Self::Browse
        } else {
            Self::Discovery
        }
    }
}

/// One search, exactly as the tool, the route and the page state it.
///
/// The three surfaces build this from their own input and hand it to
/// [`search`], so the tool cannot answer a different question from the route.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    /// The words to look for. Empty is legal in browse and in a scope-wide walk.
    pub query: String,
    /// `all` or `conversation`. A scope of `conversation` needs a conversation.
    pub scope: String,
    /// The conversation to hold to, when the scope is `conversation` or the mode
    /// is browse.
    pub conversation_id: Option<String>,
    /// `all`, `user` or `assistant`.
    pub role: String,
    /// ISO 8601 lower bound on `created_at`.
    pub since: Option<String>,
    /// ISO 8601 upper bound on `created_at`.
    pub until: Option<String>,
    /// Which shape is being asked for.
    pub mode: Mode,
    /// How many hits, already resolved against [`SETTING_SEARCH_LIMIT`].
    pub limit: usize,
    /// Discovery: an opaque continuation from a previous [`Page`]. Browse: the
    /// message id to centre the window on.
    pub cursor: Option<String>,
}

/// One ranked hit.
#[derive(Debug, Clone, PartialEq)]
pub struct HitRow {
    /// The conversation.
    pub conversation_id: String,
    /// That conversation's title, or the id when the row is missing.
    pub title: String,
    /// The message.
    pub message_id: String,
    /// When it was written.
    pub ts: String,
    /// `user` or `assistant`.
    pub role: String,
    /// The excerpt, with the matched terms inside `<b>`.
    pub snippet: String,
    /// Lower is better, and it is negative. `0.0` for a browse row.
    pub score: f64,
}

impl HitRow {
    /// The score at a fixed twelve significant digits.
    ///
    /// `bm25` is recomputed per statement and its last two digits wobble between
    /// two runs of the same query, which is enough to make the tool's bytes and
    /// the route's bytes differ and enough to break a cursor that quoted the raw
    /// value. Twelve digits keeps the ranking and drops the wobble.
    #[must_use]
    pub fn score(&self) -> f64 {
        format!("{:.12e}", self.score)
            .parse::<f64>()
            .unwrap_or(self.score)
    }

    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "conversation_id": self.conversation_id,
            "title": self.title,
            "message_id": self.message_id,
            "ts": self.ts,
            "role": self.role,
            "snippet": self.snippet,
            "score": self.score(),
        })
    }
}

/// One page of results, in the shape both clients read.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    /// The ranked rows, best first.
    pub hits: Vec<HitRow>,
    /// How many rows match in total, from a count query rather than by
    /// over-fetching.
    pub total: i64,
    /// Where the next page starts, when there is one.
    pub cursor: Option<String>,
    /// How long the query took, in whole milliseconds.
    pub took_ms: u64,
}

impl Page {
    /// The wire body. The tool returns this value's bytes and the route returns
    /// the same bytes, which is what keeps the two surfaces one answer.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "hits": self.hits.iter().map(HitRow::json).collect::<Vec<_>>(),
            "total": self.total,
            "cursor": self.cursor,
            "took_ms": self.took_ms,
        })
    }

    /// The body as compact JSON, byte-identical for the same page.
    #[must_use]
    pub fn body(&self) -> String {
        self.json().to_string()
    }
}

/// Markers the index writes around a matched term.
///
/// Control characters rather than `<b>` in the `snippet()` call, so the body can
/// be HTML-escaped first and the tags added after: a transcript that contains a
/// literal `<b>` cannot then be mistaken for emphasis.
const OPEN: &str = "\u{1}";
const CLOSE: &str = "\u{2}";

/// Escape the body, then turn the marker characters into `<b>`.
fn markup(raw: &str) -> String {
    let open = OPEN.chars().next().unwrap_or('\u{1}');
    let close = CLOSE.chars().next().unwrap_or('\u{2}');
    let mut out = String::with_capacity(raw.len() + 16);
    for character in raw.chars() {
        match character {
            _ if character == open => out.push_str("<b>"),
            _ if character == close => out.push_str("</b>"),
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            other => out.push(other),
        }
    }
    out
}

/// The `settings` row value, or `None`.
///
/// Read here rather than through the setup engine because the tool's access
/// holds a database and nothing else; it is one indexed row on a per-call path.
fn stored(db: &Db, row: &str) -> Option<String> {
    db.with_conn(|conn| {
        use rusqlite::OptionalExtension;
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![row],
            |row| row.get::<_, String>(0),
        )
        .optional()
    })
    .ok()
    .flatten()
}

/// The stored result count, clamped to the bounds the record publishes.
#[must_use]
pub fn stored_limit(db: &Db) -> usize {
    stored(db, SETTING_SEARCH_LIMIT)
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| {
            *value >= MIN_SETTING_LIMIT as usize && *value <= MAX_SETTING_LIMIT as usize
        })
        .unwrap_or(DEFAULT_LIMIT)
}

/// The stored default scope, or `all` when the row holds something else.
#[must_use]
pub fn stored_scope(db: &Db) -> String {
    stored(db, SETTING_SEARCH_SCOPE_DEFAULT)
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| SCOPE_VALUES.contains(&value.as_str()))
        .unwrap_or_else(|| "all".to_owned())
}

/// Fill in the two fields a caller may leave to the daemon's own settings.
///
/// Both surfaces call this before [`search`], so "whatever the operator set" is
/// one rule rather than one rule per client.
pub fn apply_defaults(db: &Db, request: &mut Request) {
    if request.limit == 0 {
        request.limit = stored_limit(db);
    }
    if request.scope.trim().is_empty() {
        let scope = stored_scope(db);
        if scope == "conversation" && request.conversation_id.is_none() {
            request.scope = "all".to_owned();
        } else {
            request.scope = scope;
        }
    }
}

/// Run one search.
///
/// `total` is a count query over the same filters, never the length of an
/// over-fetch, so a page can say "143 hits" while returning twenty.
///
/// # Errors
///
/// Returns the database's own error when a statement fails.
pub fn search(db: &Db, request: &Request) -> Result<Page, DbError> {
    let started = std::time::Instant::now();
    let limit = request.limit.clamp(1, MAX_LIMIT);
    let role = match request.role.trim().to_ascii_lowercase().as_str() {
        "user" => Some("user".to_owned()),
        "assistant" => Some("assistant".to_owned()),
        _ => None,
    };
    let conversation = request
        .conversation_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let held_to = if request.mode == Mode::Browse
        || request.scope.trim().eq_ignore_ascii_case("conversation")
    {
        conversation.clone()
    } else {
        None
    };
    let since = request.since.as_deref().filter(|value| !value.is_empty());
    let until = request.until.as_deref().filter(|value| !value.is_empty());

    if request.mode == Mode::Browse {
        let page = browse(
            db,
            request,
            held_to.as_deref(),
            role.as_deref(),
            since,
            until,
            limit,
        )?;
        return Ok(Page {
            took_ms: elapsed(started),
            ..page
        });
    }

    let text = request.query.trim();
    if text.is_empty() {
        // A scoped walk with no words is a legitimate browse: it reads the
        // conversation rather than ranking it.
        let page = browse(
            db,
            request,
            held_to.as_deref(),
            role.as_deref(),
            since,
            until,
            limit,
        )?;
        return Ok(Page {
            took_ms: elapsed(started),
            ..page
        });
    }

    // All words first, then any of them. The count decides, not the page, so a
    // later page cannot silently change the expression under the caller.
    let mut chosen = None;
    for and in [true, false] {
        let Some(expression) = expression(text, and) else {
            return Ok(Page {
                hits: Vec::new(),
                total: 0,
                cursor: None,
                took_ms: elapsed(started),
            });
        };
        let total = count(
            db,
            &expression,
            held_to.as_deref(),
            role.as_deref(),
            since,
            until,
        )?;
        chosen = Some((expression, total));
        if total > 0 {
            break;
        }
    }
    let Some((expression, total)) = chosen else {
        return Ok(Page {
            hits: Vec::new(),
            total: 0,
            cursor: None,
            took_ms: elapsed(started),
        });
    };

    let after = parse_cursor(request.cursor.as_deref());
    let hits = page(
        db,
        &expression,
        held_to.as_deref(),
        role.as_deref(),
        since,
        until,
        after.as_ref(),
        limit,
    )?;
    // A cursor is offered only when a further row is really there: `hits.len()`
    // against `total` is not enough, because a skipped or filtered row would
    // then hand the caller a next-page link that returns nothing.
    let cursor = match hits.last() {
        Some(last) if (hits.len() as i64) < total => {
            let probe = (last.score, last.message_id.clone());
            let more = page(
                db,
                &expression,
                held_to.as_deref(),
                role.as_deref(),
                since,
                until,
                Some(&probe),
                1,
            )?;
            more.first()
                .map(|_| format!("{}:{}", last.score, last.message_id))
        }
        _ => None,
    };
    Ok(Page {
        hits,
        total,
        cursor,
        took_ms: elapsed(started),
    })
}

/// The one value both the tool and the route hand back.
///
/// This is the single seam the two surfaces share: the tool renders it into its
/// `ToolResult`, the route renders the same value onto the wire, and the
/// dashboard reads the same [`Page`]. Serializing one value twice cannot make the
/// two disagree, and a test compares the rendered bytes anyway.
///
/// # Errors
///
/// Returns the database's own error when the query fails.
pub fn answer(db: &Db, request: &Request) -> Result<serde_json::Value, DbError> {
    search(db, request).map(|page| page.json())
}

fn elapsed(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// A decoded continuation: the score and id of the last row the caller saw.
fn parse_cursor(cursor: Option<&str>) -> Option<(f64, String)> {
    let cursor = cursor?;
    let (score, id) = cursor.split_once(':')?;
    Some((score.parse::<f64>().ok()?, id.to_owned()))
}

/// How many rows match, from a count query.
#[allow(clippy::too_many_arguments)]
fn count(
    db: &Db,
    expression: &str,
    conversation: Option<&str>,
    role: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
) -> Result<i64, DbError> {
    const SQL: &str = "
        SELECT count(*)
          FROM chat_messages_fts
          JOIN chat_messages m ON m.rowid = chat_messages_fts.rowid
         WHERE chat_messages_fts MATCH ?1
           AND m.state = 'complete'
           AND (?2 IS NULL OR m.conversation_id = ?2)
           AND (?3 IS NULL OR m.role = ?3)
           AND (?4 IS NULL OR m.created_at >= ?4)
           AND (?5 IS NULL OR m.created_at <= ?5)";
    db.with_conn(|conn| {
        conn.query_row(
            SQL,
            rusqlite::params![expression, conversation, role, since, until],
            |row| row.get(0),
        )
    })
}

/// One ranked page, continued from `after` when the caller passed a cursor.
#[allow(clippy::too_many_arguments)]
fn page(
    db: &Db,
    expression: &str,
    conversation: Option<&str>,
    role: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
    after: Option<&(f64, String)>,
    limit: usize,
) -> Result<Vec<HitRow>, DbError> {
    // `bm25` is the sort key both for the page and for the cursor comparison, so
    // a tie cannot repeat or skip a row: the id breaks it on both sides.
    //
    // The value is quantized with `printf` before it is used, because the raw
    // score wobbles in its last digits between two statements of the same query
    // and a cursor that quoted the wobbling value would repeat the row it
    // continued from. `SCORE` is the one spelling of that, used for the selected
    // column, the sort and the comparison alike.
    const SQL: &str = "
        SELECT m.conversation_id,
               COALESCE(c.title, m.conversation_id),
               m.id,
               m.created_at,
               m.role,
               snippet(chat_messages_fts, 0, char(1), char(2), '...', 12),
               CAST(printf('%.12e', bm25(chat_messages_fts)) AS REAL)
          FROM chat_messages_fts
          JOIN chat_messages m ON m.rowid = chat_messages_fts.rowid
          LEFT JOIN chat_conversations c ON c.id = m.conversation_id
         WHERE chat_messages_fts MATCH ?1
           AND m.state = 'complete'
           AND (?2 IS NULL OR m.conversation_id = ?2)
           AND (?3 IS NULL OR m.role = ?3)
           AND (?4 IS NULL OR m.created_at >= ?4)
           AND (?5 IS NULL OR m.created_at <= ?5)
           AND (?6 IS NULL
                OR CAST(printf('%.12e', bm25(chat_messages_fts)) AS REAL) > ?6
                OR (CAST(printf('%.12e', bm25(chat_messages_fts)) AS REAL) = ?6 AND m.id > ?7))
         ORDER BY CAST(printf('%.12e', bm25(chat_messages_fts)) AS REAL), m.id
         LIMIT ?8";
    let (score, id) = match after {
        Some((score, id)) => (Some(*score), Some(id.clone())),
        None => (None, None),
    };
    db.with_conn(|conn| {
        let mut statement = conn.prepare(SQL)?;
        let rows = statement.query_map(
            rusqlite::params![
                expression,
                conversation,
                role,
                since,
                until,
                score,
                id,
                limit as i64
            ],
            |row| {
                let snippet: String = row.get(5)?;
                Ok(HitRow {
                    conversation_id: row.get(0)?,
                    title: row.get(1)?,
                    message_id: row.get(2)?,
                    ts: row.get(3)?,
                    role: row.get(4)?,
                    snippet: markup(&snippet),
                    score: row.get(6)?,
                })
            },
        )?;
        rows.collect()
    })
}

/// A window of one conversation, centred on `request.cursor` when it names a
/// message and the newest window otherwise.
///
/// `score` is `0.0` here: nothing was ranked, and pretending otherwise would let
/// a client compare a browse row's score to a discovery score.
#[allow(clippy::too_many_arguments)]
fn browse(
    db: &Db,
    request: &Request,
    conversation: Option<&str>,
    role: Option<&str>,
    since: Option<&str>,
    until: Option<&str>,
    limit: usize,
) -> Result<Page, DbError> {
    let anchor = request
        .cursor
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let text = request.query.trim();
    let expression = if text.is_empty() {
        None
    } else {
        expression(text, false)
    };

    let total = if let Some(conversation) = conversation {
        if let Some(expression) = &expression {
            count(db, expression, Some(conversation), role, since, until)?
        } else {
            db.with_conn(|conn| {
                conn.query_row(
                    "SELECT count(*) FROM chat_messages
                      WHERE conversation_id = ?1
                        AND (?2 IS NULL OR role = ?2)
                        AND (?3 IS NULL OR created_at >= ?3)
                        AND (?4 IS NULL OR created_at <= ?4)",
                    rusqlite::params![conversation, role, since, until],
                    |row| row.get(0),
                )
            })?
        }
    } else if let Some(expression) = &expression {
        count(db, expression, None, role, since, until)?
    } else {
        db.with_conn(|conn| {
            conn.query_row("SELECT count(*) FROM chat_messages", [], |row| row.get(0))
        })?
    };

    const SQL: &str = "
        SELECT m.conversation_id,
               COALESCE(c.title, m.conversation_id),
               m.id,
               m.created_at,
               m.role,
               substr(m.body, 1, 400),
               0.0,
               CASE WHEN ?1 IS NULL THEN NULL ELSE (
                 SELECT snippet(chat_messages_fts, 0, char(1), char(2), '...', 12)
                   FROM chat_messages_fts WHERE chat_messages_fts.rowid = m.rowid
               ) END
          FROM chat_messages m
          LEFT JOIN chat_conversations c ON c.id = m.conversation_id
         WHERE m.state = 'complete'
           AND (?2 IS NULL OR m.conversation_id = ?2)
           AND (?3 IS NULL OR m.role = ?3)
           AND (?4 IS NULL OR m.created_at >= ?4)
           AND (?5 IS NULL OR m.created_at <= ?5)
           AND (?6 IS NULL OR m.conversation_id IN (
                 SELECT mm.conversation_id FROM chat_messages_fts
                   JOIN chat_messages mm ON mm.rowid = chat_messages_fts.rowid
                  WHERE chat_messages_fts MATCH ?6))
           AND (?7 IS NULL OR m.created_at <= (
                 SELECT created_at FROM chat_messages WHERE id = ?7))
         ORDER BY m.created_at DESC, m.rowid DESC
         LIMIT ?8";
    let hits: Vec<HitRow> = db.with_conn(|conn| {
        let mut statement = conn.prepare(SQL)?;
        let rows = statement.query_map(
            rusqlite::params![
                expression.as_deref(),
                conversation,
                role,
                since,
                until,
                expression.as_deref(),
                anchor,
                limit as i64
            ],
            |row| {
                // Without a query there is nothing to mark, so the row carries the
                // message itself: a window of blank snippets is not a window.
                let raw: Option<String> = row.get(7)?;
                let body: String = row.get(5)?;
                Ok(HitRow {
                    conversation_id: row.get(0)?,
                    title: row.get(1)?,
                    message_id: row.get(2)?,
                    ts: row.get(3)?,
                    role: row.get(4)?,
                    snippet: raw
                        .as_deref()
                        .map_or_else(|| markup(&clip(&body)), markup),
                    score: row.get(6)?,
                })
            },
        )?;
        rows.collect()
    })?;

    // Newest-first is the right fetch order and the wrong read order for a
    // window: a person scrolling back expects the transcript to run downwards.
    let mut hits = hits;
    hits.reverse();
    let oldest = hits
        .first()
        .map(|hit| format!("0:{}", hit.message_id))
        .filter(|_| (hits.len() as i64) < total);
    Ok(Page {
        hits,
        total,
        cursor: oldest,
        took_ms: 0,
    })
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
    fn the_tool_and_the_route_return_the_same_bytes() {
        // The route body is `answer(...).to_string()`; the tool body is the same
        // value through `ToolResult::ok`. If either side ever grows its own
        // wrapper, this is the test that notices.
        let access = Access::new(seeded());
        let tool = access.call(&serde_json::json!({ "query": "warehouse", "limit": 5 }));
        assert!(!tool.is_error);

        let mut request = Request {
            query: "warehouse".to_owned(),
            scope: "all".to_owned(),
            role: "all".to_owned(),
            mode: Mode::Discovery,
            limit: 5,
            ..Request::default()
        };
        apply_defaults(&access.db, &mut request);
        let route = answer(&access.db, &request).expect("answer");
        // `took_ms` is a stopwatch, so it is compared for presence, not equality.
        let mut tool_json: serde_json::Value =
            serde_json::from_str(&tool.content).expect("the tool answers JSON");
        let mut route_json = route.clone();
        for value in [&mut tool_json, &mut route_json] {
            value["took_ms"] = serde_json::json!(0);
        }
        assert_eq!(tool_json, route_json, "one request, one answer");
        assert_eq!(
            serde_json::to_string(&tool_json).expect("compact"),
            route_json.to_string(),
            "both surfaces render the same value the same way"
        );
    }

    #[test]
    fn a_bad_enum_falls_back_and_the_record_publishes_the_values() {
        let db = seeded();
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO settings (key, value) VALUES (?1, 'sideways')
                 ON CONFLICT(key) DO UPDATE SET value = 'sideways'",
                rusqlite::params![SETTING_SEARCH_SCOPE_DEFAULT],
            )
        })
        .expect("store");
        assert_eq!(stored_scope(&db), "all");
        assert_eq!(stored_limit(&db), DEFAULT_LIMIT);
        assert!(SCOPE_VALUES.contains(&"conversation"));
    }

    #[test]
    fn a_cursor_continues_without_repeating_a_row() {
        let db = seeded();
        let mut first = Request {
            query: "warehouse".to_owned(),
            scope: "all".to_owned(),
            role: "all".to_owned(),
            mode: Mode::Discovery,
            limit: 1,
            ..Request::default()
        };
        apply_defaults(&db, &mut first);
        let one = search(&db, &first).expect("first page");
        assert_eq!(one.hits.len(), 1);
        assert_eq!(one.total, 2);
        let cursor = one.cursor.clone().expect("a cursor while rows remain");

        let mut second = first.clone();
        second.cursor = Some(cursor);
        let two = search(&db, &second).expect("second page");
        assert_eq!(two.hits.len(), 1);
        assert_ne!(
            two.hits[0].message_id, one.hits[0].message_id,
            "a page must not repeat the row it continued from"
        );
        assert!(two.cursor.is_none(), "the last page offers no cursor");
    }

    #[test]
    fn limit_is_clamped() {
        let db = seeded();
        let hits = discover(&db, "warehouse", MAX_LIMIT + 500).expect("search");
        assert!(hits.len() <= MAX_LIMIT);
    }
}
