//! The encrypted token store: one file per provider, AEAD at rest.
//!
//! Where the bytes live, per the D3 decision:
//!
//! ```text
//! ~/.config/kallisti/tokens/        0700, one file per provider
//!   claude.tok                      0600, ciphertext only
//!   antigravity.tok                 0600
//! ~/.config/kallisti/keys/token.key 0600, 32 random bytes, generated once
//! ```
//!
//! The file format is deliberately boring and deliberately not JSON: a four-byte
//! magic, the twelve-byte nonce, then the AES-256-GCM ciphertext of the record's
//! JSON. The provider id is the AEAD's associated data, so a `claude.tok` copied
//! over `antigravity.tok` fails to decrypt instead of authenticating as the wrong
//! account.
//!
//! The key is generated once from the OS random source and read back as raw
//! bytes. It is never derived from a passphrase: the daemon has no passphrase at
//! runtime, and a hash of one is not a key.

use std::fs;
use std::io::ErrorKind;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use core_domain::{
    TokenError, TokenRecord, TokenStore,
};

/// The first four bytes of every token file. A file that does not start with
/// this is refused before a decrypt is attempted, so a stray file in the
/// directory produces a clear message instead of an authentication failure.
pub const MAGIC: &[u8; 4] = b"KTK1";

/// The nonce length AES-GCM is defined for with a 96-bit nonce.
pub const NONCE_LEN: usize = 12;

/// The key length: AES-256.
pub const KEY_LEN: usize = 32;

/// Directory mode: only the owner may look at the names.
pub const DIR_MODE: u32 = 0o700;

/// File mode: only the owner may read the ciphertext or the key.
pub const FILE_MODE: u32 = 0o600;

/// A token store over encrypted files.
#[derive(Debug, Clone)]
pub struct TokenFileStore {
    dir: PathBuf,
    key_path: PathBuf,
}

impl TokenFileStore {
    /// The store under an explicit directory and key path. Used by tests and by
    /// the daemon, which passes the two paths from [`crate::config::Paths`].
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>, key_path: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            key_path: key_path.into(),
        }
    }

    /// The store under `~/.config/kallisti/{tokens,keys/token.key}`.
    ///
    /// # Errors
    ///
    /// [`TokenError::Store`] when `$HOME` is unset, which is the same condition
    /// that stops the daemon from loading its config at all.
    pub fn from_home() -> Result<Self, TokenError> {
        let home = std::env::var("HOME").map_err(|_| TokenError::Store {
            provider: "token-store".to_owned(),
            detail: "HOME is not set, so the token store has no path".to_owned(),
        })?;
        let base = Path::new(&home).join(".config/kallisti");
        Ok(Self::new(base.join("tokens"), base.join("keys/token.key")))
    }

    /// The directory token files live in.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where the key lives.
    #[must_use]
    pub fn key_path(&self) -> &Path {
        &self.key_path
    }

    /// The file one provider's record lives in.
    #[must_use]
    pub fn path_for(&self, provider: &str) -> PathBuf {
        self.dir.join(format!("{provider}.tok"))
    }

    /// The key bytes, generating the file on first use and never regenerating it.
    ///
    /// # Errors
    ///
    /// [`TokenError::Store`] when the directory or file cannot be created, or the
    /// file exists with the wrong length, which means something other than this
    /// daemon wrote it.
    pub fn key(&self) -> Result<[u8; KEY_LEN], TokenError> {
        if let Some(parent) = self.key_path.parent() {
            ensure_dir(parent)?;
        }
        match fs::read(&self.key_path) {
            Ok(bytes) => {
                let bytes: [u8; KEY_LEN] = bytes.try_into().map_err(|_| TokenError::Store {
                    provider: "token-key".to_owned(),
                    detail: format!(
                        "the key file is not {KEY_LEN} bytes; refusing to guess a key"
                    ),
                })?;
                Ok(bytes)
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let mut key = [0u8; KEY_LEN];
                getrandom::fill(&mut key).map_err(|error| TokenError::Store {
                    provider: "token-key".to_owned(),
                    detail: format!("the OS random source failed: {error}"),
                })?;
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(FILE_MODE)
                    .open(&self.key_path)
                    .map_err(|error| TokenError::Store {
                        provider: "token-key".to_owned(),
                        detail: format!("the key file could not be created: {error}"),
                    })?;
                use std::io::Write as _;
                file.write_all(&key).map_err(|error| TokenError::Store {
                    provider: "token-key".to_owned(),
                    detail: format!("the key file could not be written: {error}"),
                })?;
                set_mode(&self.key_path, FILE_MODE)?;
                Ok(key)
            }
            Err(error) => Err(TokenError::Store {
                provider: "token-key".to_owned(),
                detail: format!("the key file could not be read: {error}"),
            }),
        }
    }

    /// Encrypt one record to bytes: magic, nonce, ciphertext.
    fn seal(&self, record: &TokenRecord) -> Result<Vec<u8>, TokenError> {
        let key = self.key()?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| TokenError::Store {
            provider: record.provider.clone(),
            detail: "the key is not a valid AES-256 key length".to_owned(),
        })?;
        let mut nonce_bytes = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce_bytes).map_err(|error| TokenError::Store {
            provider: record.provider.clone(),
            detail: format!("the OS random source failed: {error}"),
        })?;
        let plaintext = record.to_store_json()?;
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload {
                    msg: &plaintext,
                    aad: record.provider.as_bytes(),
                },
            )
            .map_err(|_| TokenError::Store {
                provider: record.provider.clone(),
                detail: "the record could not be encrypted".to_owned(),
            })?;
        let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + ciphertext.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        Ok(out)
    }

    /// Decrypt bytes read from disk.
    fn open(&self, provider: &str, bytes: &[u8]) -> Result<TokenRecord, TokenError> {
        if bytes.len() < MAGIC.len() + NONCE_LEN || &bytes[..MAGIC.len()] != MAGIC {
            return Err(TokenError::Store {
                provider: provider.to_owned(),
                detail: "the file is not a token file this build wrote".to_owned(),
            });
        }
        let key = self.key()?;
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| TokenError::Store {
            provider: provider.to_owned(),
            detail: "the key is not a valid AES-256 key length".to_owned(),
        })?;
        let nonce = Nonce::from_slice(&bytes[MAGIC.len()..MAGIC.len() + NONCE_LEN]);
        let plaintext = cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &bytes[MAGIC.len() + NONCE_LEN..],
                    aad: provider.as_bytes(),
                },
            )
            .map_err(|_| TokenError::Store {
                // Deliberately says nothing about the bytes: a failed decrypt is
                // either the wrong key or a corrupted file, and neither is
                // something the operator can act on by seeing fragments.
                provider: provider.to_owned(),
                detail: "the file could not be decrypted with this daemon's key".to_owned(),
            })?;
        TokenRecord::from_store_json(provider, &plaintext)
    }

    /// Write a record, creating the directory and the file with owner-only modes.
    ///
    /// # Errors
    ///
    /// [`TokenError::Store`] when the directory, key, or file cannot be written.
    pub fn write(&self, record: &TokenRecord) -> Result<(), TokenError> {
        ensure_dir(&self.dir)?;
        let sealed = self.seal(record)?;
        let path = self.path_for(&record.provider);
        // Truncate-and-write rather than create-new: a re-login replaces the
        // record it replaces, and a leftover temp file is a leak waiting to
        // happen.
        fs::write(&path, &sealed).map_err(|error| TokenError::Store {
            provider: record.provider.clone(),
            detail: format!("the token file could not be written: {error}"),
        })?;
        set_mode(&path, FILE_MODE)
    }

    /// Read a record, or `None` when there is no file for that provider.
    ///
    /// # Errors
    ///
    /// [`TokenError::Store`] for any read or decrypt failure other than absence.
    pub fn read(&self, provider: &str) -> Result<Option<TokenRecord>, TokenError> {
        let path = self.path_for(provider);
        match fs::read(&path) {
            Ok(bytes) => self.open(provider, &bytes).map(Some),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(TokenError::Store {
                provider: provider.to_owned(),
                detail: format!("the token file could not be read: {error}"),
            }),
        }
    }
}

impl TokenStore for TokenFileStore {
    fn load(&self, provider: &str) -> Result<Option<TokenRecord>, TokenError> {
        self.read(provider)
    }

    fn store_token(&self, record: &TokenRecord) -> Result<(), TokenError> {
        self.write(record)
    }

    fn delete(&self, provider: &str) -> Result<(), TokenError> {
        match fs::remove_file(self.path_for(provider)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(TokenError::Store {
                provider: provider.to_owned(),
                detail: format!("the token file could not be removed: {error}"),
            }),
        }
    }

    fn list(&self) -> Result<Vec<String>, TokenError> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(TokenError::Store {
                    provider: "token-store".to_owned(),
                    detail: format!("the token directory could not be read: {error}"),
                });
            }
        };
        let mut providers = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| TokenError::Store {
                provider: "token-store".to_owned(),
                detail: format!("the token directory could not be read: {error}"),
            })?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Some(stem) = name.strip_suffix(".tok") {
                providers.push(stem.to_owned());
            }
        }
        providers.sort();
        Ok(providers)
    }
}

/// Create a directory with [`DIR_MODE`], and tighten an existing one.
///
/// Tightening rather than tolerating: a token directory another user can list is
/// a token directory that leaks which accounts exist, and an operator who chmodded
/// it by accident gets a fix rather than a surprise.
fn ensure_dir(path: &Path) -> Result<(), TokenError> {
    if path.is_dir() {
        let mode = fs::metadata(path)
            .map_err(|error| TokenError::Store {
                provider: "token-store".to_owned(),
                detail: format!("the token directory could not be inspected: {error}"),
            })?
            .permissions()
            .mode()
            & 0o777;
        if mode != DIR_MODE {
            set_mode(path, DIR_MODE)?;
        }
        return Ok(());
    }
    fs::create_dir_all(path).map_err(|error| TokenError::Store {
        provider: "token-store".to_owned(),
        detail: format!("the token directory could not be created: {error}"),
    })?;
    set_mode(path, DIR_MODE)
}

/// `chmod` one path.
fn set_mode(path: &Path, mode: u32) -> Result<(), TokenError> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| {
        TokenError::Store {
            provider: "token-store".to_owned(),
            detail: format!("the permissions could not be set: {error}"),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_domain::TokenOrigin;

    fn record(provider: &str, origin: TokenOrigin) -> TokenRecord {
        TokenRecord {
            provider: provider.to_owned(),
            access_token: format!("ACCESS-VALUE-FOR-{provider}-DO-NOT-LEAK"),
            refresh_token: Some(format!("REFRESH-VALUE-FOR-{provider}-DO-NOT-LEAK")),
            expires_at: Some(1_800_000_000),
            scopes: vec!["user:inference".to_owned()],
            project_id: Some("proj-abc".to_owned()),
            token_type: Some("Bearer".to_owned()),
            last_refresh: Some(1_700_000_000),
            account: Some("operator@example.com".to_owned()),
            origin,
        }
    }

    fn store() -> (tempfile::TempDir, TokenFileStore) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = TokenFileStore::new(
            dir.path().join("tokens"),
            dir.path().join("keys/token.key"),
        );
        (dir, store)
    }

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).expect("metadata").permissions().mode() & 0o777
    }

    #[test]
    fn a_record_round_trips_through_the_store_and_the_plaintext_is_nowhere_on_disk() {
        let (dir, store) = store();
        let record = record("claude", TokenOrigin::Migrated);
        store.store_token(&record).expect("stores");

        let loaded = store.load("claude").expect("loads").expect("present");
        assert_eq!(loaded, record);

        let path = store.path_for("claude");
        let bytes = fs::read(&path).expect("read");
        let haystack = String::from_utf8_lossy(&bytes);
        assert!(
            !haystack.contains("access-value-for-claude"),
            "the access token must not appear in the file"
        );
        assert!(
            !haystack.contains("refresh-value-for-claude"),
            "the refresh token must not appear in the file"
        );
        assert!(&bytes[..4] == MAGIC);
        assert_eq!(&bytes[4..16].len(), &NONCE_LEN);

        // Neither does the plaintext appear anywhere in the store directory.
        for entry in fs::read_dir(dir.path().join("tokens")).expect("dir") {
            let entry = entry.expect("entry");
            let content = fs::read(entry.path()).expect("read");
            assert!(
                !String::from_utf8_lossy(&content).contains("DO-NOT-LEAK"),
                "{} leaks a token",
                entry.path().display()
            );
        }
    }

    #[test]
    fn the_directory_is_0700_and_the_files_are_0600() {
        let (_dir, store) = store();
        store
            .store_token(&record("claude", TokenOrigin::Login))
            .expect("stores");
        assert_eq!(mode_of(store.dir()), DIR_MODE, "token directory");
        assert_eq!(mode_of(&store.path_for("claude")), FILE_MODE, "token file");
        assert_eq!(mode_of(&store.key_path()), FILE_MODE, "key file");
        assert_eq!(mode_of(store.key_path().parent().expect("parent")), DIR_MODE);
    }

    #[test]
    fn the_key_is_created_once_and_reused() {
        let (_dir, store) = store();
        let first = store.key().expect("first");
        assert_eq!(first.len(), KEY_LEN);
        let second = store.key().expect("second");
        assert_eq!(first, second, "the key must not be regenerated");

        // A record written under the first read is readable after a second.
        store
            .store_token(&record("claude", TokenOrigin::Login))
            .expect("stores");
        assert!(store.load("claude").expect("loads").is_some());
    }

    #[test]
    fn a_wrong_key_fails_to_decrypt_rather_than_returning_garbage() {
        let (dir, store) = store();
        store
            .store_token(&record("claude", TokenOrigin::Migrated))
            .expect("stores");

        // A different key at the same path: the same ciphertext, a different
        // secret.
        let other = TokenFileStore::new(dir.path().join("tokens"), dir.path().join("keys/other.key"));
        let error = other.load("claude").expect_err("must not decrypt");
        assert!(matches!(error, TokenError::Store { .. }), "{error:?}");
        let message = error.to_string();
        assert!(message.contains("could not be decrypted"), "{message}");
        assert!(!message.contains("ACCESS-VALUE"), "{message}");
    }

    #[test]
    fn a_record_copied_to_another_provider_does_not_authenticate() {
        let (_dir, store) = store();
        store
            .store_token(&record("claude", TokenOrigin::Login))
            .expect("stores");
        // The provider id is the AEAD's associated data, so the same bytes under
        // another name are not a valid record for that name.
        let bytes = fs::read(store.path_for("claude")).expect("read");
        fs::write(store.path_for("antigravity"), bytes).expect("write");
        let error = store.load("antigravity").expect_err("must not decrypt");
        assert!(matches!(error, TokenError::Store { .. }), "{error:?}");
    }

    #[test]
    fn a_missing_record_is_none_and_deleting_one_twice_is_not_an_error() {
        let (_dir, store) = store();
        assert!(store.load("claude").expect("loads").is_none());
        store.delete("claude").expect("deleting nothing is fine");
        store
            .store_token(&record("claude", TokenOrigin::Login))
            .expect("stores");
        assert_eq!(store.list().expect("lists"), vec!["claude".to_owned()]);
        store.delete("claude").expect("deletes");
        assert!(store.load("claude").expect("loads").is_none());
        assert!(store.list().expect("lists").is_empty());
    }

    #[test]
    fn a_file_that_is_not_a_token_file_is_refused() {
        let (_dir, store) = store();
        ensure_dir(store.dir()).expect("dir");
        fs::write(store.path_for("claude"), b"not a token file").expect("write");
        let error = store.load("claude").expect_err("must fail");
        assert!(matches!(error, TokenError::Store { .. }), "{error:?}");
        assert!(error.to_string().contains("not a token file"));
    }

    #[test]
    fn the_store_lists_what_it_holds_and_ignores_other_files() {
        let (_dir, store) = store();
        ensure_dir(store.dir()).expect("dir");
        store
            .store_token(&record("claude", TokenOrigin::Login))
            .expect("stores");
        store
            .store_token(&record("antigravity", TokenOrigin::Migrated))
            .expect("stores");
        fs::write(store.dir().join("notes.txt"), b"x").expect("write");
        assert_eq!(
            store.list().expect("lists"),
            vec!["antigravity".to_owned(), "claude".to_owned()]
        );
    }
}
