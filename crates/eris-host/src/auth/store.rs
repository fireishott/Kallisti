//! The auth rows in SQLite: verifiers in, nothing reversible out.
//!
//! This is the only module that touches `admin_credential`, `pairing_codes`,
//! `devices` and the P6 `auth_tokens` table. Every write here stores an Argon2id
//! PHC string or plain metadata; there is no column anywhere that holds a token,
//! a code or a passphrase.
//!
//! `audit_log` is written here too, and it carries ids only. A row that names a
//! device or a code id is useful; a row that carried the value would make the
//! audit trail the leak.

use rusqlite::params;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::token::TokenKind;
use crate::auth::{AuthError, ScopeSet, format_timestamp, hash_secret, now, verify_secret};
use crate::db::Db;

/// The minimum admin passphrase. Long enough that a loopback-only form is not the
/// weakest part of the setup.
pub const MIN_ADMIN_PASSPHRASE: usize = 12;

/// A token row about to be written. The hash is already computed.
#[derive(Debug, Clone)]
pub struct NewToken {
    /// Row id.
    pub id: String,
    /// Owning device.
    pub device_id: String,
    /// Access or refresh.
    pub kind: TokenKind,
    /// Argon2id PHC string of the token.
    pub token_hash: String,
    /// What the token may do.
    pub scope: ScopeSet,
    /// When it stops verifying.
    pub expires_at: OffsetDateTime,
}

/// A token row as read back.
#[derive(Debug, Clone)]
pub struct StoredToken {
    /// Row id.
    pub id: String,
    /// Owning device.
    pub device_id: String,
    /// Storage spelling of the kind.
    pub kind: String,
    /// Argon2id PHC string.
    pub token_hash: String,
    /// Storage spelling of the scope set.
    pub scope: String,
    /// RFC 3339 expiry.
    pub expires_at: String,
    /// When rotation retired it.
    pub rotated_at: Option<String>,
    /// When revocation retired it.
    pub revoked_at: Option<String>,
}

/// A pairing code row as read back. There is no plaintext column by design.
#[derive(Debug, Clone)]
pub struct StoredPairingCode {
    /// Row id.
    pub id: String,
    /// Argon2id PHC string of the code.
    pub code_hash: String,
    /// Device the code is bound to, when it was bound.
    pub device_id: Option<String>,
    /// RFC 3339 expiry.
    pub expires_at: String,
    /// When it was redeemed.
    pub consumed_at: Option<String>,
    /// Whether the operator ticked the box that lets this pairing mint `admin`.
    pub admin: bool,
}

/// A device row as read back.
#[derive(Debug, Clone)]
pub struct DeviceRecord {
    /// Row id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Platform label.
    pub platform: String,
    /// Set when the device was revoked.
    pub revoked_at: Option<String>,
}

/// The auth store: one database handle, no cache.
#[derive(Clone)]
pub struct AuthStore {
    db: Db,
}

impl AuthStore {
    /// Wrap the state database.
    #[must_use]
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// The database underneath. Tests and the doctor use this.
    #[must_use]
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// Insert a device row and return its id.
    pub fn register_device(&self, name: &str, platform: &str) -> Result<String, AuthError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(AuthError::EmptyDeviceName);
        }
        let id = Uuid::now_v7().to_string();
        let paired_at = format_timestamp(now());
        let platform = platform.trim();
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO devices (id, name, platform, paired_at) VALUES (?1, ?2, ?3, ?4)",
                params![id, name, platform, paired_at],
            )?;
            Ok(())
        })?;
        self.audit("loopback", "auth.device_registered", Some(&id))?;
        Ok(id)
    }

    /// One device, or `None` when it was never registered.
    pub fn device(&self, device_id: &str) -> Result<Option<DeviceRecord>, AuthError> {
        let row = self.db.with_conn(|conn| {
            let mut statement =
                conn.prepare("SELECT id, name, platform, revoked_at FROM devices WHERE id = ?1")?;
            let mut rows = statement.query(params![device_id])?;
            match rows.next()? {
                Some(row) => Ok(Some(DeviceRecord {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    platform: row.get(2)?,
                    revoked_at: row.get(3)?,
                })),
                None => Ok(None),
            }
        })?;
        Ok(row)
    }

    /// Write one token verifier.
    pub fn insert_token(&self, token: &NewToken) -> Result<(), AuthError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO auth_tokens (id, device_id, kind, token_hash, scope, expires_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    token.id,
                    token.device_id,
                    token.kind.as_str(),
                    token.token_hash,
                    token.scope.to_storage(),
                    format_timestamp(token.expires_at),
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Every token row, retired ones included. Verification needs the retired
    /// rows so it can answer "revoked" instead of "unknown".
    pub fn token_rows(&self) -> Result<Vec<StoredToken>, AuthError> {
        let rows = self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, device_id, kind, token_hash, scope, expires_at, rotated_at, revoked_at \
                 FROM auth_tokens ORDER BY created_at, id",
            )?;
            let mut rows = statement.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(StoredToken {
                    id: row.get(0)?,
                    device_id: row.get(1)?,
                    kind: row.get(2)?,
                    token_hash: row.get(3)?,
                    scope: row.get(4)?,
                    expires_at: row.get(5)?,
                    rotated_at: row.get(6)?,
                    revoked_at: row.get(7)?,
                });
            }
            Ok(out)
        })?;
        Ok(rows)
    }

    /// Retire one token by revocation. `false` when it was already retired.
    pub fn mark_token_revoked(
        &self,
        token_id: &str,
        at: &OffsetDateTime,
    ) -> Result<bool, AuthError> {
        let stamp = format_timestamp(*at);
        let changed = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE auth_tokens SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
                params![token_id, stamp],
            )
        })?;
        Ok(changed > 0)
    }

    /// Retire one token by rotation. `false` when it was already retired, which
    /// is what makes a double rotate a refusal rather than a second live pair.
    pub fn mark_token_rotated(
        &self,
        token_id: &str,
        at: &OffsetDateTime,
    ) -> Result<bool, AuthError> {
        let stamp = format_timestamp(*at);
        let changed = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE auth_tokens SET rotated_at = ?2 \
                 WHERE id = ?1 AND rotated_at IS NULL AND revoked_at IS NULL",
                params![token_id, stamp],
            )
        })?;
        Ok(changed > 0)
    }

    /// Revoke every live token of one device. Returns how many rows changed.
    pub fn revoke_tokens_for_device(
        &self,
        device_id: &str,
        at: &OffsetDateTime,
    ) -> Result<usize, AuthError> {
        let stamp = format_timestamp(*at);
        let changed = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE auth_tokens SET revoked_at = ?2 WHERE device_id = ?1 AND revoked_at IS NULL",
                params![device_id, stamp],
            )
        })?;
        Ok(changed)
    }

    /// Mark the device itself revoked.
    pub fn mark_device_revoked(
        &self,
        device_id: &str,
        at: &OffsetDateTime,
    ) -> Result<bool, AuthError> {
        let stamp = format_timestamp(*at);
        let changed = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE devices SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
                params![device_id, stamp],
            )
        })?;
        Ok(changed > 0)
    }

    /// Revoke a device and all its tokens, immediately. This is the whole
    /// operation, in one place, so a later route cannot do half of it.
    pub fn revoke_device(&self, device_id: &str) -> Result<usize, AuthError> {
        if self.device(device_id)?.is_none() {
            return Err(AuthError::UnknownDevice);
        }
        let at = now();
        let tokens = self.revoke_tokens_for_device(device_id, &at)?;
        self.mark_device_revoked(device_id, &at)?;
        self.audit("loopback", "auth.revoke_device", Some(device_id))?;
        Ok(tokens)
    }

    /// Delete one device row for good, and with it every token it owned.
    ///
    /// Revoking keeps the row on purpose: it is the record that a device was cut
    /// off, and the operator can still see what it was. This is the other
    /// operation, the one for a list that filled up with probes nobody will pair
    /// again - the row is not a device any more, and keeping a hash of its tokens
    /// for ever is clutter, not history. `auth_tokens` cascades on the delete, so
    /// no verifier outlives the row that owns it.
    pub fn delete_device(&self, device_id: &str) -> Result<(), AuthError> {
        if self.device(device_id)?.is_none() {
            return Err(AuthError::UnknownDevice);
        }
        self.db.with_conn(|conn| {
            conn.execute("DELETE FROM devices WHERE id = ?1", params![device_id])
        })?;
        self.audit("loopback", "auth.delete_device", Some(device_id))?;
        Ok(())
    }

    /// Delete every device that is already revoked, and return how many went.
    ///
    /// Scoped to revoked rows on purpose: an active device is never swept up by a
    /// button that says "revoked", and a pair still in use cannot be deleted by a
    /// slip. One statement, so the count is the count.
    pub fn delete_revoked_devices(&self) -> Result<usize, AuthError> {
        let deleted = self.db.with_conn(|conn| {
            conn.execute("DELETE FROM devices WHERE revoked_at IS NOT NULL", [])
        })?;
        if deleted > 0 {
            self.audit(
                "loopback",
                "auth.delete_revoked_devices",
                Some(&format!("devices={deleted}")),
            )?;
        }
        Ok(deleted)
    }

    /// Write one pairing code verifier and return the row id.
    ///
    /// `admin` is what the operator asked for at mint time. It is stored on the
    /// code because redemption has nobody to ask: the code is the whole record
    /// of the decision.
    pub fn insert_pairing_code(
        &self,
        code_hash: &str,
        device_id: Option<&str>,
        expires_at: OffsetDateTime,
        admin: bool,
    ) -> Result<String, AuthError> {
        let id = Uuid::now_v7().to_string();
        let expires_at = format_timestamp(expires_at);
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO pairing_codes (id, code_hash, device_id, expires_at, admin) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, code_hash, device_id, expires_at, admin],
            )?;
            Ok(())
        })?;
        Ok(id)
    }

    /// Every pairing row, consumed and expired ones included.
    pub fn pairing_rows(&self) -> Result<Vec<StoredPairingCode>, AuthError> {
        let rows = self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, code_hash, device_id, expires_at, consumed_at, admin \
                 FROM pairing_codes ORDER BY created_at, id",
            )?;
            let mut rows = statement.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(StoredPairingCode {
                    id: row.get(0)?,
                    code_hash: row.get(1)?,
                    device_id: row.get(2)?,
                    expires_at: row.get(3)?,
                    consumed_at: row.get(4)?,
                    admin: row.get::<_, i64>(5)? != 0,
                });
            }
            Ok(out)
        })?;
        Ok(rows)
    }

    /// Consume a code. Conditional on `consumed_at IS NULL`, so a second redeem
    /// loses the race instead of succeeding twice.
    pub fn consume_pairing_code(
        &self,
        code_id: &str,
        at: &OffsetDateTime,
    ) -> Result<bool, AuthError> {
        let stamp = format_timestamp(*at);
        let changed = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE pairing_codes SET consumed_at = ?2 WHERE id = ?1 AND consumed_at IS NULL",
                params![code_id, stamp],
            )
        })?;
        Ok(changed > 0)
    }

    /// Store the admin passphrase as a verifier. This is the only writer of
    /// `admin_credential`, and it never has the value in hand after hashing.
    pub fn set_admin_password(&self, password: &str) -> Result<(), AuthError> {
        if password.chars().count() < MIN_ADMIN_PASSPHRASE {
            return Err(AuthError::WeakAdminPassword {
                minimum: MIN_ADMIN_PASSPHRASE,
            });
        }
        let verifier = hash_secret(password.as_bytes())?;
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO admin_credential (id, password_hash, algorithm) VALUES (1, ?1, 'argon2id') \
                 ON CONFLICT (id) DO UPDATE SET password_hash = excluded.password_hash, \
                 algorithm = excluded.algorithm, \
                 updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
                params![verifier],
            )?;
            Ok(())
        })?;
        self.audit("loopback", "auth.admin_password_set", None)
    }

    /// The stored admin verifier, or `None` before setup.
    pub fn admin_password_hash(&self) -> Result<Option<String>, AuthError> {
        let value = self.db.with_conn(|conn| {
            let mut statement =
                conn.prepare("SELECT password_hash FROM admin_credential WHERE id = 1")?;
            let mut rows = statement.query([])?;
            match rows.next()? {
                Some(row) => Ok(Some(row.get::<_, String>(0)?)),
                None => Ok(None),
            }
        })?;
        Ok(value)
    }

    /// Verify the admin passphrase against the stored verifier.
    pub fn verify_admin_password(&self, password: &str) -> Result<(), AuthError> {
        let stored = self
            .admin_password_hash()?
            .ok_or(AuthError::NoAdminCredential)?;
        if verify_secret(password.as_bytes(), &stored)? {
            Ok(())
        } else {
            self.audit("loopback", "auth.admin_password_rejected", None)?;
            Err(AuthError::AdminPasswordRejected)
        }
    }

    /// Append one audit row. `detail` is an id or another non-secret label.
    pub fn audit(&self, actor: &str, action: &str, detail: Option<&str>) -> Result<(), AuthError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO audit_log (actor, action, detail) VALUES (?1, ?2, ?3)",
                params![actor, action, detail],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Every action in the audit trail, oldest first.
    pub fn audit_actions(&self) -> Result<Vec<String>, AuthError> {
        let actions = self.db.with_conn(|conn| {
            let mut statement = conn.prepare("SELECT action FROM audit_log ORDER BY id")?;
            let mut rows = statement.query([])?;
            let mut out = Vec::new();
            while let Some(row) = rows.next()? {
                out.push(row.get::<_, String>(0)?);
            }
            Ok(out)
        })?;
        Ok(actions)
    }
}

// Tests live at module level (not in a `mod tests`) so the reported name is
// exactly `auth::store::<name>`.

#[cfg(test)]
#[test]
fn no_plaintext_secret_in_db() {
    let fixture = fixture();
    let secrets = fixture.every_plaintext_secret();
    let known_ids = fixture.known_ids();

    // 1. Every column of every row, as text.
    for (table, column, value) in all_cells(&fixture.db) {
        for secret in &secrets {
            assert!(
                !value.contains(secret.as_str()),
                "{table}.{column} holds a minted secret in plaintext"
            );
        }
        assert!(
            !looks_like_hex_secret(&value),
            "{table}.{column} holds a 64-hex value, which is the shape of an access token"
        );
        // A verifier is the one allowed base64-shaped value. A UUID is an id,
        // and only the ids this fixture created are allowed to look like one.
        let allowed_id = is_uuid(&value) && known_ids.iter().any(|id| id == &value);
        if !is_argon2id_phc(&value) && !allowed_id {
            assert!(
                !has_long_base64_run(&value),
                "{table}.{column} holds a base64 run of 32+ characters that is not an argon2id verifier"
            );
        }
    }

    // 2. The bytes on disk, including the WAL. A hash cannot be searched for a
    //    plaintext, so this is the check that actually proves nothing leaked.
    let raw = database_bytes(fixture.dir.path());
    assert!(!raw.is_empty(), "the database file was readable");
    for secret in &secrets {
        assert!(
            !contains_bytes(&raw, secret.as_bytes()),
            "the database file holds a minted secret in plaintext"
        );
    }
}

#[cfg(test)]
#[test]
fn db_rows_are_argon2id_hashes() {
    use argon2::Argon2;
    use argon2::password_hash::PasswordVerifier;
    use argon2::password_hash::phc::PasswordHash;

    let fixture = fixture();
    let verifiers = fixture.verifier_cells();
    assert!(
        verifiers.len() >= 6,
        "expected the admin verifier, the pairing verifier and four token verifiers: {}",
        verifiers.len()
    );

    for (where_, stored, plaintext, bytes) in verifiers {
        assert!(
            stored.starts_with("$argon2id$"),
            "{where_} must carry the argon2id PHC prefix"
        );
        let parsed =
            PasswordHash::new(&stored).unwrap_or_else(|error| panic!("{where_} parses: {error}"));
        assert_eq!(parsed.algorithm.as_str(), "argon2id", "{where_}");
        assert_ne!(
            stored, plaintext,
            "{where_} stores a verifier, not the value"
        );
        assert!(
            Argon2::default().verify_password(&bytes, &parsed).is_ok(),
            "{where_} must verify the value it was minted from"
        );
    }
}

#[cfg(test)]
#[test]
fn rotation_is_recorded() {
    let fixture = fixture();
    let store = AuthStore::new(fixture.db.clone());
    let rotated = store
        .token_rows()
        .expect("token rows")
        .into_iter()
        .filter(|row| row.rotated_at.is_some())
        .count();
    assert_eq!(rotated, 1, "exactly the rotated refresh row is retired");
    assert!(
        store
            .audit_actions()
            .expect("audit rows")
            .iter()
            .any(|action| action == "auth.rotate"),
        "rotation is audited"
    );
}

// ---- fixtures ----

#[cfg(test)]
struct Fixture {
    db: Db,
    dir: tempfile::TempDir,
    admin_password: String,
    pairing_code: String,
    device_id: String,
    minted: Vec<(String, String)>,
}

#[cfg(test)]
impl Fixture {
    fn every_plaintext_secret(&self) -> Vec<String> {
        let mut secrets = vec![self.admin_password.clone(), self.pairing_code.clone()];
        for (_, plaintext) in &self.minted {
            secrets.push(plaintext.clone());
        }
        secrets
    }

    /// Every id this database is allowed to contain. A UUID-shaped value that is
    /// not one of these is not an id, it is something smuggled in.
    fn known_ids(&self) -> Vec<String> {
        let store = AuthStore::new(self.db.clone());
        let mut ids = vec![self.device_id.clone()];
        for row in store.token_rows().expect("token rows") {
            ids.push(row.id);
        }
        for row in store.pairing_rows().expect("pairing rows") {
            ids.push(row.id);
        }
        ids
    }

    fn verifier_cells(&self) -> Vec<(String, String, String, Vec<u8>)> {
        let store = AuthStore::new(self.db.clone());
        let mut cells = Vec::new();
        cells.push((
            "admin_credential.password_hash".to_owned(),
            store
                .admin_password_hash()
                .expect("read")
                .expect("the admin verifier exists"),
            self.admin_password.clone(),
            self.admin_password.as_bytes().to_vec(),
        ));
        for row in store.pairing_rows().expect("pairing rows") {
            cells.push((
                format!("pairing_codes.code_hash {}", row.id),
                row.code_hash,
                self.pairing_code.clone(),
                self.pairing_code.as_bytes().to_vec(),
            ));
        }
        for row in store.token_rows().expect("token rows") {
            let plaintext = self
                .minted
                .iter()
                .find(|(id, _)| id == &row.id)
                .map(|(_, plaintext)| plaintext.clone())
                .expect("every token row came from a mint in this test");
            // The verifier is over the 32 raw bytes; the hex string is only the
            // spelling the client receives.
            let bytes = hex::decode(&plaintext).expect("a minted token is hex");
            cells.push((
                format!("auth_tokens.token_hash {}", row.id),
                row.token_hash,
                plaintext,
                bytes,
            ));
        }
        cells
    }
}

#[cfg(test)]
fn fixture() -> Fixture {
    use crate::auth::ScopeSet;
    use crate::auth::pairing::PairingService;
    use crate::auth::token::{TokenOrigin, TokenService};

    let dir = tempfile::tempdir().expect("temp dir");
    let db = Db::open(dir.path().join("eris.db")).expect("the database opens");
    let store = AuthStore::new(db.clone());

    let admin_password = "correct-horse-battery-staple-9".to_owned();
    store
        .set_admin_password(&admin_password)
        .expect("the admin passphrase is stored as a verifier");

    let pairing_code = {
        let pairing = PairingService::new(db.clone());
        let issued = pairing.issue().expect("a pairing code");
        issued.code().to_owned()
    };

    let service = TokenService::new(db.clone());
    let device = service.register_device("phone", "ios").expect("device");
    let pair = service
        .mint_pair(&device, ScopeSet::device_default(), TokenOrigin::Device)
        .expect("an access and refresh pair");
    let rotated = service.rotate(pair.refresh.token()).expect("rotate");

    let minted = vec![
        (
            pair.access.token_id().to_owned(),
            pair.access.token().to_owned(),
        ),
        (
            pair.refresh.token_id().to_owned(),
            pair.refresh.token().to_owned(),
        ),
        (
            rotated.access.token_id().to_owned(),
            rotated.access.token().to_owned(),
        ),
        (
            rotated.refresh.token_id().to_owned(),
            rotated.refresh.token().to_owned(),
        ),
    ];

    Fixture {
        db,
        dir,
        admin_password,
        pairing_code,
        device_id: device,
        minted,
    }
}

#[cfg(test)]
fn all_cells(db: &Db) -> Vec<(String, String, String)> {
    let mut cells = Vec::new();
    for table in db.table_names().expect("table names") {
        let columns = db.column_names(&table).expect("column names");
        let rows = db
            .with_conn(|conn| {
                let mut statement = conn.prepare(&format!("SELECT * FROM \"{table}\""))?;
                let width = statement.column_count();
                let mut rows = statement.query([])?;
                let mut out = Vec::new();
                while let Some(row) = rows.next()? {
                    let mut values = Vec::with_capacity(width);
                    for index in 0..width {
                        // Integer ids and nulls are part of "every column".
                        let value = match row.get_ref(index)? {
                            rusqlite::types::ValueRef::Null => None,
                            rusqlite::types::ValueRef::Integer(number) => Some(number.to_string()),
                            rusqlite::types::ValueRef::Real(number) => Some(number.to_string()),
                            rusqlite::types::ValueRef::Text(bytes) => {
                                Some(String::from_utf8_lossy(bytes).into_owned())
                            }
                            rusqlite::types::ValueRef::Blob(bytes) => Some(hex::encode(bytes)),
                        };
                        values.push(value);
                    }
                    out.push(values);
                }
                Ok(out)
            })
            .expect("rows");
        for row in rows {
            for (index, value) in row.into_iter().enumerate() {
                if let Some(value) = value {
                    let column = columns
                        .get(index)
                        .cloned()
                        .unwrap_or_else(|| format!("#{index}"));
                    cells.push((table.clone(), column, value));
                }
            }
        }
    }
    cells
}

#[cfg(test)]
fn is_uuid(value: &str) -> bool {
    value.len() == 36
        && value.char_indices().all(|(index, character)| match index {
            8 | 13 | 18 | 23 => character == '-',
            _ => character.is_ascii_hexdigit(),
        })
}

#[cfg(test)]
fn looks_like_hex_secret(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
fn is_base64_alphabet(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'_' | b'-')
}

#[cfg(test)]
fn has_long_base64_run(value: &str) -> bool {
    let mut run = 0usize;
    for byte in value.bytes() {
        if is_base64_alphabet(byte) {
            run += 1;
            if run >= 32 {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

#[cfg(test)]
fn is_argon2id_phc(value: &str) -> bool {
    use argon2::password_hash::phc::PasswordHash;
    value.starts_with("$argon2id$") && PasswordHash::new(value).is_ok()
}

#[cfg(test)]
fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

#[cfg(test)]
fn database_bytes(dir: &std::path::Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .expect("the state directory is readable")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries {
        if let Ok(contents) = std::fs::read(&path) {
            bytes.extend_from_slice(&contents);
        }
    }
    bytes
}
