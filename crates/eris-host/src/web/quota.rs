//! What the gateway knows about a provider's limits, read after a turn.
//!
//! 9router sends no `x-ratelimit-*` headers on a successful stream, so the only
//! place limit state lives is its own database: per connection it records
//! `rateLimitedUntil`, a `modelLock_<model>` stamp for every model it has locked,
//! `lastError`, and `testStatus`. This module reads that file **read-only**
//! (`SQLITE_OPEN_READ_ONLY`, the pattern Argus uses) and never writes to it, so
//! the gateway is untouched by anything here.
//!
//! Everything is best effort and reported as such: a missing file, a schema that
//! moved, or a model the gateway does not know returns `None`, and the footer
//! simply omits the quota rather than inventing one.

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// The `settings` row that turns the quota line on.
///
/// Off by default: it is extra information some operators want and others find
/// noisy, and it reads a file outside this daemon's own state.
pub const SETTING_SHOW_QUOTA: &str = "assistant.show_quota";

/// The `settings` row naming the gateway database, when it is not the default.
pub const SETTING_GATEWAY_DB: &str = "assistant.gateway_db";

/// Where 9router keeps its database for this user.
pub const DEFAULT_GATEWAY_DB: &str = ".9router/db/data.sqlite";

/// Whether the quota line is shown. Only an explicit `true` turns it on.
#[must_use]
pub fn show_quota(stored: Option<&str>) -> bool {
    matches!(stored.map(str::trim), Some("true"))
}

/// The gateway database path: the stored setting, else `~/.9router/...`.
#[must_use]
pub fn gateway_db(stored: Option<&str>) -> Option<std::path::PathBuf> {
    if let Some(path) = stored.map(str::trim).filter(|path| !path.is_empty()) {
        return Some(std::path::PathBuf::from(path));
    }
    std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(DEFAULT_GATEWAY_DB))
}

/// One provider connection's limit state, as the gateway recorded it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Quota {
    /// The gateway's provider name (`deepseek`, `codex`, `commandcode`, ...).
    pub provider: String,
    /// `ok`, `locked` (this model is locked), or `limited` (the whole connection).
    pub state: String,
    /// Seconds until the lock clears, when one is in force.
    pub resets_in_secs: Option<u64>,
    /// The gateway's last recorded error, trimmed, when it is a limit.
    pub last_error: Option<String>,
    /// Requests through this provider today, from the gateway's daily rollup.
    pub requests_today: Option<u64>,
}

impl Quota {
    /// The footer text: `deepseek ok · 2.5k req today` or
    /// `codex locked · resets 29m`.
    #[must_use]
    pub fn line(&self) -> String {
        let mut parts = vec![format!("{} {}", self.provider, self.state)];
        if let Some(secs) = self.resets_in_secs {
            parts.push(format!("resets {}", human_wait(secs)));
        }
        if let Some(count) = self.requests_today {
            parts.push(format!(
                "{} req today",
                crate::web::templates::human_tokens(count)
            ));
        }
        parts.join(" \u{00B7} ")
    }
}

/// A wait for a human: `45s`, `29m`, `2h 14m`.
#[must_use]
pub fn human_wait(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3_600 {
        return format!("{}m", secs.div_ceil(60));
    }
    let hours = secs / 3_600;
    let minutes = (secs % 3_600) / 60;
    if minutes == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h {minutes}m")
    }
}

/// Split a 9router model id into the connection's provider and model.
///
/// `ds/deepseek-v4-flash` -> (`ds`, `deepseek-v4-flash`). The prefix is an alias,
/// so [`read`] matches it against both the provider name and the lock keys.
fn split_model(model: &str) -> (&str, &str) {
    model.split_once('/').unwrap_or(("", model))
}

/// The short aliases 9router uses in model ids, mapped to provider names.
///
/// Read from the routes this host actually uses; an alias not listed here still
/// resolves when its model id appears as a `modelLock_` key.
const ALIASES: &[(&str, &str)] = &[
    ("ds", "deepseek"),
    ("cmc", "commandcode"),
    ("cc", "claude"),
    ("cx", "codex"),
    ("ag", "antigravity"),
    ("oc", "opencode"),
];

/// Read the limit state for the model that just answered.
///
/// `None` when the file is missing, unreadable, or knows nothing about the model.
#[must_use]
pub fn read(path: &std::path::Path, model: &str) -> Option<Quota> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    conn.busy_timeout(std::time::Duration::from_millis(250))
        .ok()?;
    read_from(&conn, model, OffsetDateTime::now_utc())
}

fn read_from(conn: &Connection, model: &str, now: OffsetDateTime) -> Option<Quota> {
    let (prefix, bare) = split_model(model);
    let provider_hint = ALIASES
        .iter()
        .find(|(alias, _)| *alias == prefix)
        .map_or(prefix, |(_, name)| *name);

    let mut statement = conn
        .prepare("SELECT provider, data FROM providerConnections WHERE isActive = 1")
        .ok()?;
    let rows: Vec<(String, String)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .ok()?
        .filter_map(Result::ok)
        .collect();

    let lock_key = format!("modelLock_{bare}");
    // The connection is the provider the alias names; failing that, the one that
    // holds a lock key for this exact model.
    let (provider, data) = rows
        .iter()
        .find(|(provider, _)| provider == provider_hint)
        .or_else(|| rows.iter().find(|(_, data)| data.contains(&lock_key)))?;
    let data: serde_json::Value = serde_json::from_str(data).ok()?;

    let future = |key: &str| -> Option<u64> {
        let stamp = data.get(key)?.as_str()?;
        let at = OffsetDateTime::parse(stamp, &Rfc3339).ok()?;
        let wait = (at - now).whole_seconds();
        (wait > 0).then(|| u64::try_from(wait).unwrap_or(0))
    };
    let model_lock = future(&lock_key);
    let connection_lock = future("rateLimitedUntil");
    let (state, resets_in_secs) = match (connection_lock, model_lock) {
        (Some(secs), _) => ("limited", Some(secs)),
        (None, Some(secs)) => ("locked", Some(secs)),
        (None, None) => ("ok", None),
    };
    let last_error = data
        .get("lastError")
        .and_then(serde_json::Value::as_str)
        .filter(|text| resets_in_secs.is_some() && !text.trim().is_empty())
        .map(|text| text.chars().take(160).collect());

    let today = now.date().to_string();
    let requests_today = conn
        .query_row(
            "SELECT data FROM usageDaily WHERE dateKey = ?1",
            [&today],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|daily| {
            daily
                .get("byProvider")?
                .get(provider.as_str())?
                .get("requests")?
                .as_u64()
        });

    Some(Quota {
        provider: provider.clone(),
        state: state.to_owned(),
        resets_in_secs,
        last_error,
        requests_today,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gateway() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE providerConnections (id TEXT, provider TEXT, isActive INTEGER, data TEXT);
             CREATE TABLE usageDaily (dateKey TEXT PRIMARY KEY, data TEXT);",
        )
        .unwrap();
        conn
    }

    fn now() -> OffsetDateTime {
        OffsetDateTime::parse("2026-10-21T22:00:00Z", &Rfc3339).unwrap()
    }

    #[test]
    fn a_locked_model_reports_its_reset() {
        let conn = gateway();
        conn.execute(
            "INSERT INTO providerConnections VALUES ('a','codex',1,?1)",
            [r#"{"modelLock_gpt-6-luna":"2026-10-21T22:29:00Z","lastError":"[429]: usage limit"}"#],
        )
        .unwrap();
        let quota = read_from(&conn, "cx/gpt-6-luna", now()).unwrap();
        assert_eq!(quota.state, "locked");
        assert_eq!(quota.resets_in_secs, Some(29 * 60));
        assert_eq!(quota.line(), "codex locked \u{00B7} resets 29m");
    }

    #[test]
    fn a_whole_connection_limit_outranks_a_model_lock() {
        let conn = gateway();
        conn.execute(
            "INSERT INTO providerConnections VALUES ('a','commandcode',1,?1)",
            [r#"{"rateLimitedUntil":"2026-10-22T00:14:00Z"}"#],
        )
        .unwrap();
        let quota = read_from(&conn, "cmc/deepseek/deepseek-v4-flash", now()).unwrap();
        assert_eq!(quota.state, "limited");
        assert_eq!(human_wait(quota.resets_in_secs.unwrap()), "2h 14m");
    }

    #[test]
    fn an_expired_lock_is_ok_and_today_is_counted() {
        let conn = gateway();
        conn.execute(
            "INSERT INTO providerConnections VALUES ('a','deepseek',1,?1)",
            [r#"{"modelLock_deepseek-v4-flash":"2026-10-21T20:00:00Z"}"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO usageDaily VALUES ('2026-10-21', ?1)",
            [r#"{"byProvider":{"deepseek":{"requests":2511}}}"#],
        )
        .unwrap();
        let quota = read_from(&conn, "ds/deepseek-v4-flash", now()).unwrap();
        assert_eq!(quota.state, "ok");
        assert_eq!(quota.resets_in_secs, None);
        assert_eq!(quota.last_error, None);
        assert_eq!(quota.line(), "deepseek ok \u{00B7} 2.5k req today");
    }

    #[test]
    fn an_unknown_model_is_none_not_a_guess() {
        let conn = gateway();
        assert!(read_from(&conn, "zz/nothing", now()).is_none());
    }

    #[test]
    fn the_setting_is_off_unless_it_says_true() {
        assert!(!show_quota(None));
        assert!(!show_quota(Some("yes")));
        assert!(show_quota(Some("true")));
    }
}
