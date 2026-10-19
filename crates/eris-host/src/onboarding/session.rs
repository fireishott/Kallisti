//! The admin browser session: one cookie, one verifier row, no cache.
//!
//! The wizard and the dashboard are the only admin surfaces in 1.0, and both sit
//! behind one cookie. The shape is the same one P6 chose for access tokens,
//! because the same reviewer complaint applies:
//!
//! - the cookie value is `<row id>.<32 random bytes as hex>`; the row id is a
//!   lookup key and the random half is the credential;
//! - SQLite holds an Argon2id verifier of the random half and never the value,
//!   so a copied `eris.db` cannot be replayed as a signed-in browser;
//! - verification re-reads `revoked_at` and `expires_at` on every request, so a
//!   revoke is immediate.
//!
//! `Secure` is deliberately absent from the attribute list: the admin listener
//! binds `127.0.0.1` over plain HTTP, and a `Secure` cookie would simply never be
//! stored by the browser, which would lock the operator out of their own wizard.
//! `HttpOnly`, `SameSite=Strict` and `Path=/` are all set.

use std::time::Duration;

use axum::http::{HeaderMap, header};
use rusqlite::{OptionalExtension, params};
use time::OffsetDateTime;

use crate::auth::{self, AuthError};
use crate::db::{Db, DbError};

/// Name of the session cookie.
pub const COOKIE_NAME: &str = "kallisti_admin";

/// How long one admin session lives before it has to be minted again.
pub const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);

/// Bytes of randomness in a session secret.
pub const SECRET_BYTES: usize = 32;

/// A freshly minted session.
#[derive(Debug, Clone)]
pub struct MintedSession {
    /// Row id, also the first half of the cookie value.
    pub id: String,
    /// The cookie value. This is the only place the secret exists.
    pub cookie_value: String,
    /// When it stops verifying.
    pub expires_at: OffsetDateTime,
}

/// One session row as read back for verification.
#[derive(Debug, Clone)]
struct SessionRow {
    token_hash: String,
    expires_at: String,
    revoked_at: Option<String>,
}

/// Why a session could not be minted or verified.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// SQLite refused.
    #[error("database error: {0}")]
    Db(#[from] DbError),
    /// The auth primitives refused.
    #[error(transparent)]
    Auth(#[from] AuthError),
    /// The cookie is not `<id>.<hex>`.
    #[error("the session cookie is not in the expected shape")]
    Malformed,
    /// No row has that id.
    #[error("no session row matches")]
    Unknown,
    /// The row is retired.
    #[error("the session was revoked")]
    Revoked,
    /// The row is past its expiry.
    #[error("the session has expired")]
    Expired,
    /// The verifier did not match.
    #[error("the session secret does not verify")]
    Rejected,
    /// A timestamp could not be parsed.
    #[error("a stored session timestamp is not RFC 3339")]
    Timestamp(#[from] time::error::Parse),
}

impl SessionError {
    /// The stable machine-readable code for this refusal.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Db(_) => "db_error",
            Self::Auth(error) => error.code(),
            Self::Malformed => "session_malformed",
            Self::Unknown => "session_unknown",
            Self::Revoked => "session_revoked",
            Self::Expired => "session_expired",
            Self::Rejected => "session_invalid",
            Self::Timestamp(_) => "corrupt_timestamp",
        }
    }
}

/// The admin session surface.
#[derive(Clone)]
pub struct AdminSessions {
    db: Db,
}

impl AdminSessions {
    /// Wrap the state database.
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Mint one session and return the cookie value exactly once.
    pub fn mint(&self) -> Result<MintedSession, SessionError> {
        let id = uuid::Uuid::now_v7().to_string();
        let secret = auth::random_bytes::<SECRET_BYTES>()?;
        let hex_secret = hex::encode(secret);
        let verifier = auth::hash_secret(hex_secret.as_bytes())?;
        let expires_at = auth::now() + time::Duration::seconds(SESSION_TTL.as_secs() as i64);

        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO admin_sessions (id, token_hash, expires_at) VALUES (?1, ?2, ?3)",
                params![id, verifier, auth::format_timestamp(expires_at)],
            )?;
            Ok(())
        })?;

        Ok(MintedSession {
            cookie_value: format!("{id}.{hex_secret}"),
            id,
            expires_at,
        })
    }

    /// Verify a cookie value. Every failure is a distinct code, and `Ok(())`
    /// means the browser is signed in.
    pub fn verify(&self, cookie_value: &str) -> Result<(), SessionError> {
        let (id, secret) = cookie_value
            .split_once('.')
            .ok_or(SessionError::Malformed)?;
        if id.is_empty()
            || secret.len() != SECRET_BYTES * 2
            || !secret.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(SessionError::Malformed);
        }
        let row: Option<SessionRow> = self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT token_hash, expires_at, revoked_at FROM admin_sessions WHERE id = ?1",
                params![id],
                |row| {
                    Ok(SessionRow {
                        token_hash: row.get(0)?,
                        expires_at: row.get(1)?,
                        revoked_at: row.get(2)?,
                    })
                },
            )
            .optional()
        })?;

        let row = row.ok_or(SessionError::Unknown)?;
        if row.revoked_at.is_some() {
            return Err(SessionError::Revoked);
        }
        if auth::now() >= auth::parse_timestamp(&row.expires_at)? {
            return Err(SessionError::Expired);
        }
        if !auth::verify_secret(secret.as_bytes(), &row.token_hash)? {
            return Err(SessionError::Rejected);
        }
        Ok(())
    }

    /// Retire every live session. `eris setup --reset` calls this so a reset
    /// does not leave a signed-in browser behind it.
    pub fn revoke_all(&self) -> Result<usize, SessionError> {
        let changed = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE admin_sessions SET revoked_at = ?1 WHERE revoked_at IS NULL",
                params![auth::format_timestamp(auth::now())],
            )
        })?;
        Ok(changed)
    }

    /// Live (unrevoked, unexpired) session count. The dashboard uses it, and a
    /// test asserts a reset takes it to zero.
    pub fn live_count(&self) -> Result<usize, SessionError> {
        let now = auth::format_timestamp(auth::now());
        let count: i64 = self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM admin_sessions WHERE revoked_at IS NULL AND expires_at > ?1",
                params![now],
                |row| row.get(0),
            )
        })?;
        Ok(usize::try_from(count).unwrap_or(0))
    }
}

/// The `Set-Cookie` value for a minted session.
///
/// `Secure` is absent on purpose (loopback HTTP), and `HttpOnly` and
/// `SameSite=Strict` are not negotiable: this cookie is the whole admin surface.
#[must_use]
pub fn set_cookie_header(cookie_value: &str) -> String {
    format!(
        "{COOKIE_NAME}={cookie_value}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}",
        SESSION_TTL.as_secs()
    )
}

/// The `Set-Cookie` value that clears the session cookie.
#[must_use]
pub fn clear_cookie_header() -> String {
    format!("{COOKIE_NAME}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0")
}

/// The session cookie value from a request, when one is present.
#[must_use]
pub fn cookie_from_headers(headers: &HeaderMap) -> Option<String> {
    let mut found: Option<String> = None;
    for value in headers.get_all(header::COOKIE) {
        let Ok(text) = value.to_str() else {
            continue;
        };
        for pair in text.split(';') {
            let Some((name, value)) = pair.trim().split_once('=') else {
                continue;
            };
            if name.trim() == COOKIE_NAME && !value.trim().is_empty() {
                found = Some(value.trim().to_owned());
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, AdminSessions) {
        let dir = tempfile::tempdir().expect("temp dir");
        let db = Db::open(dir.path().join("eris.db")).expect("db opens");
        (dir, AdminSessions::new(db))
    }

    #[test]
    fn a_minted_cookie_verifies_and_stores_no_value() {
        let (_dir, sessions) = fixture();
        let minted = sessions.mint().expect("mint");
        sessions.verify(&minted.cookie_value).expect("verifies");
        assert_eq!(sessions.live_count().expect("count"), 1);

        let stored = sessions
            .db
            .with_conn(|conn| {
                let mut statement = conn.prepare("SELECT token_hash FROM admin_sessions")?;
                let mut rows = statement.query([])?;
                let mut out = Vec::new();
                while let Some(row) = rows.next()? {
                    out.push(row.get::<_, String>(0)?);
                }
                Ok(out)
            })
            .expect("rows");
        assert_eq!(stored.len(), 1);
        let verifier = &stored[0];
        assert!(
            verifier.starts_with("$argon2id$"),
            "the row must hold an argon2id PHC string: {verifier}"
        );
        let secret = minted.cookie_value.split_once('.').expect("id.secret").1;
        assert!(
            !verifier.contains(secret),
            "the stored row must not contain the cookie secret"
        );
    }

    #[test]
    fn a_tampered_or_foreign_cookie_is_refused() {
        let (_dir, sessions) = fixture();
        let minted = sessions.mint().expect("mint");
        let (id, _secret) = minted.cookie_value.split_once('.').expect("split");

        assert_eq!(
            sessions
                .verify(&format!("{id}.{}", "0".repeat(SECRET_BYTES * 2)))
                .expect_err("wrong secret")
                .code(),
            "session_invalid"
        );
        assert_eq!(
            sessions
                .verify("not-a-cookie")
                .expect_err("malformed")
                .code(),
            "session_malformed"
        );
        let unknown = format!("{}.{}", uuid::Uuid::now_v7(), "a".repeat(SECRET_BYTES * 2));
        assert_eq!(
            sessions.verify(&unknown).expect_err("unknown").code(),
            "session_unknown"
        );
    }

    #[test]
    fn a_revoked_session_stops_verifying_immediately() {
        let (_dir, sessions) = fixture();
        let minted = sessions.mint().expect("mint");
        sessions.verify(&minted.cookie_value).expect("verifies");
        assert_eq!(sessions.revoke_all().expect("revoke"), 1);
        assert_eq!(
            sessions
                .verify(&minted.cookie_value)
                .expect_err("revoked")
                .code(),
            "session_revoked"
        );
        assert_eq!(sessions.live_count().expect("count"), 0);
    }

    #[test]
    fn an_expired_session_stops_verifying() {
        let (_dir, sessions) = fixture();
        let minted = sessions.mint().expect("mint");
        sessions
            .db
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE admin_sessions SET expires_at = '2000-01-01T00:00:00Z' WHERE id = ?1",
                    params![minted.id],
                )?;
                Ok(())
            })
            .expect("expire");
        assert_eq!(
            sessions
                .verify(&minted.cookie_value)
                .expect_err("expired")
                .code(),
            "session_expired"
        );
    }

    #[test]
    fn the_cookie_attributes_are_the_documented_ones() {
        let header = set_cookie_header("id.secret");
        assert!(header.contains("HttpOnly"));
        assert!(header.contains("SameSite=Strict"));
        assert!(header.contains("Path=/"));
        assert!(
            !header.contains("Secure"),
            "Secure would be dropped over loopback HTTP and lock the operator out"
        );
        assert!(clear_cookie_header().contains("Max-Age=0"));
    }

    #[test]
    fn the_cookie_is_read_back_out_of_a_messy_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "other=1; kallisti_admin=abc.def; another=2"
                .parse()
                .expect("a header value"),
        );
        assert_eq!(
            cookie_from_headers(&headers).as_deref(),
            Some("abc.def"),
            "the session cookie is found next to unrelated cookies"
        );
        assert!(cookie_from_headers(&HeaderMap::new()).is_none());
    }
}
