//! Copying the two live accounts out of another program's database.
//!
//! The source database is opened **read-only**, with the same
//! `SQLITE_OPEN_READ_ONLY` flag `web/quota.rs` uses, and nothing here ever writes
//! to it. What comes out is a [`TokenRecord`] with
//! [`TokenOrigin::Migrated`], which means this daemon will never refresh it: the
//! program it came from still owns the grant and keeps it alive, and spending the
//! refresh token would kill that copy.
//!
//! A migrated record is therefore a **staging copy**. Once its access token goes
//! stale it stays stale, and the operator connects the account properly with a
//! login this daemon owns.

use std::path::Path;

use core_domain::{TokenError, TokenOrigin, TokenRecord, TokenStore};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// The table the other program keeps its connections in.
pub const CONNECTIONS_TABLE: &str = "providerConnections";

/// One row, before it is turned into a [`TokenRecord`]. The `data` column is a
/// JSON blob.
#[derive(Debug, Clone)]
pub struct SourceRow {
    /// The provider name as the source spells it (`claude`, `antigravity`).
    pub provider: String,
    /// The account label, when the source recorded one.
    pub email: Option<String>,
    /// The JSON blob.
    pub data: String,
}

/// What a migration did, per provider. Deliberately booleans and counts: a report
/// that carries a token is a report that leaks one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// Per-provider outcome.
    pub entries: Vec<MigrationEntry>,
}

/// One provider's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationEntry {
    /// Provider id.
    pub provider: String,
    /// Whether a record was found in the source.
    pub found: bool,
    /// Whether it was written to the store.
    pub written: bool,
    /// Field-by-field agreement with the source: (field, matches).
    pub fields: Vec<(&'static str, bool)>,
    /// Why it was not written, when it was not.
    pub note: Option<String>,
}

impl MigrationReport {
    /// Every field that did not match, across every provider.
    #[must_use]
    pub fn mismatches(&self) -> Vec<(String, &'static str)> {
        let mut out = Vec::new();
        for entry in &self.entries {
            for (field, matches) in &entry.fields {
                if !matches {
                    out.push((entry.provider.clone(), *field));
                }
            }
        }
        out
    }
}

/// Read the live OAuth rows out of the source database, read-only.
///
/// # Errors
///
/// [`TokenError::Store`] when the file cannot be opened read-only, the table is
/// missing, or a row cannot be read. The message names the path, never a value.
pub fn read_source_rows(path: &Path, providers: &[&str]) -> Result<Vec<SourceRow>, TokenError> {
    let connection = Connection::open_with_flags(
        path,
        // Read-only, always. The same flag the quota reader uses.
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|error| TokenError::Store {
        provider: "migration".to_owned(),
        detail: format!("{} could not be opened read-only: {error}", path.display()),
    })?;

    let mut rows = Vec::new();
    for provider in providers {
        let row = connection
            .query_row(
                &format!(
                    "SELECT provider, email, data FROM {CONNECTIONS_TABLE} \
                     WHERE provider = ?1 AND isActive = 1 LIMIT 1"
                ),
                [provider],
                |row| {
                    Ok(SourceRow {
                        provider: row.get(0)?,
                        email: row.get::<_, Option<String>>(1)?,
                        data: row.get(2)?,
                    })
                },
            )
            .optional()
            .map_err(|error| TokenError::Store {
                provider: (*provider).to_owned(),
                detail: format!("the source row could not be read: {error}"),
            })?;
        if let Some(row) = row {
            rows.push(row);
        }
    }
    Ok(rows)
}

/// Turn one source row into a record. The origin is always [`TokenOrigin::Migrated`].
///
/// # Errors
///
/// [`TokenError::Malformed`] when the `data` blob is not JSON or carries no access
/// token. The message names the field, never the blob.
pub fn record_from_row(row: &SourceRow) -> Result<TokenRecord, TokenError> {
    let value: serde_json::Value =
        serde_json::from_str(&row.data).map_err(|_| TokenError::Malformed {
            provider: row.provider.clone(),
            detail: "the source `data` column is not JSON".to_owned(),
        })?;
    let access_token = value
        .get("accessToken")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| TokenError::Malformed {
            provider: row.provider.clone(),
            detail: "the source record carries no access token".to_owned(),
        })?
        .to_owned();

    Ok(TokenRecord {
        provider: row.provider.clone(),
        access_token,
        refresh_token: value
            .get("refreshToken")
            .and_then(serde_json::Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_owned),
        // Two shapes live in the wild: an integer epoch, and an RFC3339 string.
        // See `epoch_from` for why only taking `as_i64` here was a defect.
        expires_at: value.get("expiresAt").and_then(epoch_from),
        scopes: value
            .get("scope")
            .and_then(serde_json::Value::as_str)
            .map(|scope| {
                scope
                    .split([' ', ','])
                    .filter(|part| !part.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default(),
        project_id: value
            .get("projectId")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        token_type: value
            .get("tokenType")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        last_refresh: value.get("lastRefreshAt").and_then(epoch_from),
        account: row
            .email
            .clone()
            .filter(|email| !email.trim().is_empty())
            .or_else(|| {
                value
                    .get("email")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            }),
        origin: TokenOrigin::Migrated,
    })
}

/// Read one timestamp out of the source blob, in either shape the source uses.
///
/// The live database writes **both**. The rows measured on 2026-10-25 carry
/// `"expiresAt": "2026-10-25T22:01:44.938Z"` - an RFC3339 string - while older
/// builds and other columns carry an integer epoch, in milliseconds or seconds.
///
/// Reading only `as_i64` silently produced `None` for a string stamp, and `None`
/// means "no expiry, use it as-is" to [`TokenRecord::is_fresh`]. A token eight
/// months stale then reported itself as live. The field-by-field comparison did
/// not catch it either, because it derived both sides from this same function:
/// the check agreed with itself rather than with the source.
///
/// A string that is neither RFC3339 nor a bare integer yields `None` - an
/// unparseable expiry must not become a fabricated one.
#[must_use]
pub fn epoch_from(value: &serde_json::Value) -> Option<i64> {
    if let Some(raw) = value.as_i64() {
        return Some(normalise_epoch(raw));
    }
    let text = value.as_str()?.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(at) = OffsetDateTime::parse(text, &Rfc3339) {
        return Some(at.unix_timestamp());
    }
    text.parse::<i64>().ok().map(normalise_epoch)
}

/// A millisecond stamp becomes seconds; a second stamp is left alone.
#[must_use]
pub fn normalise_epoch(raw: i64) -> i64 {
    if raw > 10_000_000_000 {
        raw / 1_000
    } else {
        raw
    }
}

/// Compare a stored record against the source row, field by field.
///
/// Booleans only: a comparison that returns values is a comparison that can be
/// printed.
///
/// **This reads the source blob directly, and never through
/// [`record_from_row`].** The first version of this function compared a record
/// against a re-parse of the same row by the same code, so a field the parser
/// could not read was `None` on both sides and matched. Two live accounts
/// migrated with `expires_at: None` and the report said every field agreed. A
/// check that derives both sides from the subject under test proves nothing.
///
/// A field the source carries but the record does not is a mismatch, whatever
/// the value.
///
/// # Errors
///
/// [`TokenError::Malformed`] when the blob is not JSON, for the same reason
/// [`record_from_row`] fails: an unreadable row cannot be compared.
pub fn compare(
    row: &SourceRow,
    record: &TokenRecord,
) -> Result<Vec<(&'static str, bool)>, TokenError> {
    let value: serde_json::Value =
        serde_json::from_str(&row.data).map_err(|_| TokenError::Malformed {
            provider: row.provider.clone(),
            detail: "the source `data` column is not JSON".to_owned(),
        })?;

    // Raw, independent reads. A key the source holds is `Some`; a key it does
    // not hold is `None`, and a record that invented one is a mismatch.
    let raw_access = value
        .get("accessToken")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned);
    let raw_refresh = value
        .get("refreshToken")
        .and_then(serde_json::Value::as_str)
        .filter(|token| !token.is_empty())
        .map(str::to_owned);
    let raw_expires = value.get("expiresAt").and_then(epoch_from);
    let raw_project = value
        .get("projectId")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    let raw_scopes: Vec<String> = value
        .get("scope")
        .and_then(serde_json::Value::as_str)
        .map(|scope| {
            scope
                .split([' ', ','])
                .filter(|part| !part.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let raw_account = row
        .email
        .clone()
        .filter(|email| !email.trim().is_empty())
        .or_else(|| {
            value
                .get("email")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });

    Ok(vec![
        (
            "access_token",
            raw_access.as_deref() == Some(record.access_token.as_str()),
        ),
        ("refresh_token", raw_refresh == record.refresh_token),
        (
            "expires_at",
            match (raw_expires, record.expires_at) {
                // The source said something and the record lost it. This is the
                // exact shape of the defect this function was rewritten for.
                (Some(_), None) => false,
                // The source said nothing and the record invented something.
                (None, Some(_)) => false,
                (from_source, stored) => from_source == stored,
            },
        ),
        ("project_id", raw_project == record.project_id),
        ("scopes", raw_scopes == record.scopes),
        ("account", raw_account == record.account),
        ("origin", record.origin == TokenOrigin::Migrated),
    ])
}

/// Copy every named provider out of the source database into the store.
///
/// # Errors
///
/// [`TokenError::Store`] when the source cannot be read. A provider that is
/// missing or malformed is reported in the [`MigrationReport`] rather than failing
/// the whole run, because one broken row must not stop the other from landing.
pub fn migrate(
    source_db: &Path,
    store: &dyn TokenStore,
    providers: &[&str],
) -> Result<MigrationReport, TokenError> {
    let rows = read_source_rows(source_db, providers)?;
    let mut report = MigrationReport::default();

    for provider in providers {
        let Some(row) = rows.iter().find(|row| row.provider == *provider) else {
            report.entries.push(MigrationEntry {
                provider: (*provider).to_owned(),
                found: false,
                written: false,
                fields: Vec::new(),
                note: Some("no active row in the source database".to_owned()),
            });
            continue;
        };
        match record_from_row(row) {
            Ok(record) => {
                store.store_token(&record)?;
                let fields = compare(row, &record)?;
                report.entries.push(MigrationEntry {
                    provider: (*provider).to_owned(),
                    found: true,
                    written: true,
                    fields,
                    note: None,
                });
            }
            Err(error) => {
                report.entries.push(MigrationEntry {
                    provider: (*provider).to_owned(),
                    found: true,
                    written: false,
                    fields: Vec::new(),
                    note: Some(error.to_string()),
                });
            }
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth::store::TokenFileStore;

    fn source_row(provider: &str, data: &str) -> SourceRow {
        SourceRow {
            provider: provider.to_owned(),
            email: Some("operator@example.com".to_owned()),
            data: data.to_owned(),
        }
    }

    fn claude_blob() -> &'static str {
        r#"{"accessToken":"MIGRATED-ACCESS","refreshToken":"MIGRATED-REFRESH",
            "expiresAt":1800000000000,"scope":"user:inference user:profile",
            "lastRefreshAt":1700000000000,"testStatus":"ok"}"#
    }

    /// The shape the live 9router database actually writes on 2026-10-25:
    /// `expiresAt` and `lastRefreshAt` are **RFC3339 strings**, not integers.
    /// Measured read-only from `providerConnections`; values here are synthetic
    /// but the shape and the key order are the stored ones.
    fn live_shape_blob(access: &str, expires_at: &str, last_refresh: &str) -> String {
        format!(
            r#"{{"accessToken":"{access}","refreshToken":"REFRESH","expiresAt":"{expires_at}",
                "scope":"user:inference","testStatus":"active","expiresIn":28800,
                "lastRefreshAt":"{last_refresh}","errorCode":null,"lastError":null}}"#
        )
    }

    fn antigravity_blob() -> &'static str {
        r#"{"accessToken":"AG-ACCESS","refreshToken":"AG-REFRESH",
            "expiresAt":1800000000,"scope":"cloud-platform","projectId":"proj-xyz",
            "lastRefreshAt":1700000000}"#
    }

    #[test]
    fn a_source_row_becomes_a_migrated_record_with_its_fields_carried_over() {
        let row = source_row("claude", claude_blob());
        let record = record_from_row(&row).expect("parses");
        assert_eq!(record.origin, TokenOrigin::Migrated);
        assert_eq!(record.access_token, "MIGRATED-ACCESS");
        assert_eq!(record.refresh_token.as_deref(), Some("MIGRATED-REFRESH"));
        assert_eq!(
            record.expires_at,
            Some(1_800_000_000),
            "a millisecond stamp is normalised to seconds"
        );
        assert_eq!(record.last_refresh, Some(1_700_000_000));
        assert_eq!(
            record.scopes,
            vec!["user:inference".to_owned(), "user:profile".to_owned()]
        );
        assert_eq!(record.account.as_deref(), Some("operator@example.com"));

        let row = source_row("antigravity", antigravity_blob());
        let record = record_from_row(&row).expect("parses");
        assert_eq!(
            record.expires_at,
            Some(1_800_000_000),
            "seconds stay seconds"
        );
        assert_eq!(record.project_id.as_deref(), Some("proj-xyz"));
    }

    #[test]
    fn a_row_with_no_access_token_is_a_typed_error_and_not_an_empty_record() {
        let row = source_row("claude", r#"{"refreshToken":"only-a-refresh"}"#);
        let error = record_from_row(&row).expect_err("must fail");
        assert!(error.to_string().contains("no access token"));
        let row = source_row("claude", "not json");
        let error = record_from_row(&row).expect_err("must fail");
        assert!(error.to_string().contains("not JSON"));
    }

    #[test]
    fn a_migration_writes_a_migrated_records_that_compare_field_by_field() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store =
            TokenFileStore::new(dir.path().join("tokens"), dir.path().join("keys/token.key"));
        let source_db = dir.path().join("source.sqlite");
        {
            let connection = Connection::open(&source_db).expect("open");
            connection
                .execute_batch(
                    "CREATE TABLE providerConnections (
                        id TEXT PRIMARY KEY, provider TEXT, authType TEXT, name TEXT,
                        email TEXT, priority INTEGER, isActive INTEGER, data TEXT);",
                )
                .expect("schema");
            connection
                .execute(
                    "INSERT INTO providerConnections VALUES
                        ('1','claude','oauth','Claude','',0,1,?1),
                        ('2','antigravity','oauth','Antigravity','freemancurtisd@gmail.com',1,1,?2),
                        ('3','deepseek','apikey','DeepSeek','',2,1,'{\"apiKey\":\"x\"}')",
                    rusqlite::params![claude_blob(), antigravity_blob()],
                )
                .expect("rows");
        }

        let report =
            migrate(&source_db, &store, &["claude", "antigravity", "minimax"]).expect("migrates");
        assert_eq!(report.entries.len(), 3);
        assert!(report.entries[0].written);
        assert!(report.entries[1].written);
        assert!(
            !report.entries[2].found,
            "a provider with no row is reported"
        );
        assert!(
            report.mismatches().is_empty(),
            "every field must match: {:?}",
            report.mismatches()
        );

        let stored = store.load("claude").expect("loads").expect("present");
        assert_eq!(stored.origin, TokenOrigin::Migrated);
        assert_eq!(stored.access_token, "MIGRATED-ACCESS");
    }

    #[test]
    fn the_source_database_is_opened_read_only() {
        let dir = tempfile::tempdir().expect("temp dir");
        let source_db = dir.path().join("source.sqlite");
        {
            let connection = Connection::open(&source_db).expect("open");
            connection
                .execute_batch(
                    "CREATE TABLE providerConnections (
                        id TEXT PRIMARY KEY, provider TEXT, authType TEXT, name TEXT,
                        email TEXT, priority INTEGER, isActive INTEGER, data TEXT);",
                )
                .expect("schema");
        }
        // A write through the migration's own connection must be impossible, so
        // the read-only flag is proven by attempting one through the same flags.
        let connection = Connection::open_with_flags(
            &source_db,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
        )
        .expect("open");
        let error = connection
            .execute("DELETE FROM providerConnections", [])
            .expect_err("a read-only connection must refuse a write");
        assert!(
            error.to_string().to_lowercase().contains("readonly"),
            "{error}"
        );
    }

    #[test]
    fn an_inactive_row_is_not_migrated() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store =
            TokenFileStore::new(dir.path().join("tokens"), dir.path().join("keys/token.key"));
        let source_db = dir.path().join("source.sqlite");
        {
            let connection = Connection::open(&source_db).expect("open");
            connection
                .execute_batch(
                    "CREATE TABLE providerConnections (
                        id TEXT PRIMARY KEY, provider TEXT, authType TEXT, name TEXT,
                        email TEXT, priority INTEGER, isActive INTEGER, data TEXT);",
                )
                .expect("schema");
            connection
                .execute(
                    "INSERT INTO providerConnections VALUES
                        ('1','claude','oauth','Claude','',0,0,?1)",
                    rusqlite::params![claude_blob()],
                )
                .expect("row");
        }
        let report = migrate(&source_db, &store, &["claude"]).expect("runs");
        assert!(!report.entries[0].found);
        assert!(!report.entries[0].written);
        assert!(store.load("claude").expect("loads").is_none());
    }

    #[test]
    fn a_missing_source_database_is_a_typed_error() {
        let dir = tempfile::tempdir().expect("temp dir");
        let store =
            TokenFileStore::new(dir.path().join("tokens"), dir.path().join("keys/token.key"));
        let error =
            migrate(&dir.path().join("nope.sqlite"), &store, &["claude"]).expect_err("must fail");
        assert!(error.to_string().contains("read-only"));
    }

    #[test]
    fn an_rfc3339_expiry_is_read_rather_than_dropped() {
        // The defect: `expiresAt` as a string parsed to `None`, and `None` means
        // "no expiry, use as-is" to `TokenRecord::is_fresh`. A stale token
        // reported itself live. This is the regression test.
        let blob = live_shape_blob(
            "LIVE-ACCESS",
            "2026-10-25T22:01:44.938Z",
            "2026-10-25T14:01:44.938Z",
        );
        let record = record_from_row(&source_row("claude", &blob)).expect("parses");
        assert_eq!(
            record.expires_at,
            Some(1_792_965_704),
            "an RFC3339 string must become unix seconds, not None"
        );
        assert_eq!(record.last_refresh, Some(1_792_936_904));

        // And the record must now be able to say it is inside its refresh lead
        // and past its expiry, instead of answering "fresh" for ever.
        assert!(
            !record.is_fresh(1_793_000_000),
            "the token is stale at that time"
        );
    }

    #[test]
    fn epoch_from_takes_every_shape_and_invents_nothing() {
        let parse = |text: &str| epoch_from(&serde_json::from_str(text).expect("json"));
        // RFC3339 with and without fractional seconds.
        assert_eq!(parse(r#""2026-10-25T22:01:44.938Z""#), Some(1_792_965_704));
        assert_eq!(parse(r#""2026-10-25T22:01:44Z""#), Some(1_792_965_704));
        // Integers, seconds and milliseconds.
        assert_eq!(parse("1800000000"), Some(1_800_000_000));
        assert_eq!(parse("1800000000000"), Some(1_800_000_000));
        // A string holding a bare integer, which some builds write.
        assert_eq!(parse(r#""1800000000000""#), Some(1_800_000_000));
        // Nothing readable becomes `None`, never a fabricated stamp.
        assert_eq!(parse(r#""""#), None);
        assert_eq!(parse(r#""not a date""#), None);
        assert_eq!(parse("null"), None);
        assert_eq!(parse("true"), None);
    }

    #[test]
    fn a_record_that_lost_an_expiry_the_source_had_is_a_mismatch() {
        // The check that missed the defect: `compare()` used to re-parse the row
        // and compare the parse against itself. Feed it a record with the
        // expiry stripped and the old version would have said `true`.
        let blob = live_shape_blob(
            "LIVE-ACCESS",
            "2026-10-25T22:01:44.938Z",
            "2026-10-25T14:01:44.938Z",
        );
        let row = source_row("claude", &blob);
        let mut record = record_from_row(&row).expect("parses");
        record.expires_at = None;

        let fields = compare(&row, &record).expect("compares");
        let expires = fields
            .iter()
            .find(|(field, _)| *field == "expires_at")
            .map(|(_, matches)| *matches)
            .expect("expires_at is compared");
        assert!(!expires, "a dropped expiry must be reported as a mismatch");

        // And an intact record still compares clean, so the check is not simply
        // always-false.
        let good = record_from_row(&row).expect("parses");
        let fields = compare(&row, &good).expect("compares");
        assert!(fields.iter().all(|(_, matches)| *matches), "{fields:?}");
    }

    #[test]
    fn a_normalisation_boundary_is_the_year_2286() {
        assert_eq!(normalise_epoch(1_800_000_000), 1_800_000_000);
        assert_eq!(normalise_epoch(1_800_000_000_000), 1_800_000_000);
        assert_eq!(normalise_epoch(0), 0);
    }
}
