//! The archive format: magic, manifest, and member paths.
//!
//! One file, no external tooling:
//!
//! ```text
//! magic      "KALLISTIBK1\n"                12 bytes
//! manifest   u32 little-endian length + JSON
//! payload    one zstd frame, the members concatenated in manifest order
//! ```
//!
//! The manifest is plaintext and readable without decompressing the payload, so
//! `--list` and verification work on an archive whose payload is corrupt, and a
//! human can `strings` the file. Each member's `sha256` is over the *uncompressed*
//! bytes as they sit in the payload, so a member is verifiable before it is
//! written anywhere.
//!
//! The manifest carries its own digest as `manifest_sha256`, computed over a
//! canonical projection ([`Manifest::digest`]) that excludes only that field. That
//! is the same discipline as the P2 log envelope: never hash an unordered map.
//! `serde_json`'s object type is a `BTreeMap`, and the projection is a struct
//! whose field order is fixed by this file, so the same manifest always hashes to
//! the same bytes.
//!
//! Member paths are a closed set of roots ([`ROOT_STATE`], [`ROOT_CONFIG`],
//! [`ROOT_WORKSPACE`]) plus a relative remainder. [`member_root`] is the only door
//! a path may come through, and it refuses absolutes, `.`, `..`, and anything
//! outside those roots. Restore never joins an unvalidated string onto the target.

use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::backup::BackupError;

/// First twelve bytes of every archive.
pub const MAGIC: &[u8; 12] = b"KALLISTIBK1\n";

/// The archive layout this binary writes and the only one it reads.
pub const FORMAT_VERSION: u32 = 1;

/// Bytes of the little-endian manifest length that follows the magic.
pub const LENGTH_BYTES: usize = 4;

/// Ceiling on a manifest, so a corrupt length field cannot ask for a huge read.
pub const MAX_MANIFEST_BYTES: u32 = 16 * 1024 * 1024;

/// zstd level for the payload. Level 3 matches the sealed session segments.
pub const ZSTD_LEVEL: i32 = 3;

/// Member kind: the consistent SQLite snapshot.
pub const KIND_DB: &str = "db";
/// Member kind: an immutable, compressed session segment.
pub const KIND_SESSION_SEGMENT: &str = "session-segment";
/// Member kind: a live `wal.jsonl`, copied whole or to its last complete line.
pub const KIND_SESSION_WAL: &str = "session-wal";
/// Member kind: `eris.env`, which holds key *references*.
pub const KIND_ENV: &str = "env";
/// Member kind: `config.toml`.
pub const KIND_TOML: &str = "toml";
/// Member kind: a file from the opt-in sandbox workspace.
pub const KIND_WORKSPACE: &str = "workspace";

/// Path root for the state directory.
pub const ROOT_STATE: &str = "state";
/// Path root for the config directory.
pub const ROOT_CONFIG: &str = "config";
/// Path root for the sandbox workspace.
pub const ROOT_WORKSPACE: &str = "workspace";

/// The member path of the database.
pub const DB_PATH: &str = "state/eris.db";
/// The member path of the secret-half config.
pub const ENV_PATH: &str = "config/eris.env";
/// The member path of the non-secret config.
pub const TOML_PATH: &str = "config/config.toml";
/// The member path of the setup passphrase file. Never a member; always excluded.
pub const FIRST_RUN_PATH: &str = "config/first-run.txt";

/// Which root a member belongs under on the target filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberRoot {
    /// `~/.local/share/kallisti`
    State,
    /// `~/.config/kallisti`
    Config,
    /// The sandbox workspace from `config.toml`
    Workspace,
}

/// One archived file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Member {
    /// Manifest path, `<root>/<relative>`, e.g. `state/eris.db`.
    pub path: String,
    /// One of the `KIND_*` values.
    pub kind: String,
    /// Uncompressed length in the payload.
    pub bytes: u64,
    /// Lowercase hex sha256 of the uncompressed bytes.
    pub sha256: String,
}

impl Member {
    /// The JSON shape `--json` and `--list` print.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "path": self.path,
            "kind": self.kind,
            "bytes": self.bytes,
            "sha256": self.sha256,
        })
    }
}

/// One thing the backup deliberately left out, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Excluded {
    /// Manifest-shaped path that was excluded.
    pub path: String,
    /// Why, in words an operator can act on.
    pub reason: String,
}

impl Excluded {
    /// The JSON shape `--json` and `--list` print.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "path": self.path,
            "reason": self.reason,
        })
    }
}

/// The whole manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Archive layout version.
    pub format_version: u32,
    /// The `schema_migrations` version inside the archived database.
    pub schema_version: i64,
    /// The binary that wrote the archive.
    pub eris_version: String,
    /// RFC 3339 UTC.
    pub created_at: String,
    /// Hostname, informational only.
    pub host: String,
    /// Members in payload order.
    pub members: Vec<Member>,
    /// What was left out on purpose.
    pub excluded: Vec<Excluded>,
    /// Bytes dropped from the tail of a live `wal.jsonl` that another writer held.
    #[serde(default)]
    pub wal_partial_bytes: u64,
    /// Lowercase hex sha256 over [`Manifest::digest`].
    pub manifest_sha256: String,
}

/// The canonical projection the manifest digest covers.
///
/// Field order is the canonical order. `manifest_sha256` cannot cover itself, and
/// it is the only field left out.
#[derive(Serialize)]
struct ManifestCore<'a> {
    format_version: u32,
    schema_version: i64,
    eris_version: &'a str,
    created_at: &'a str,
    host: &'a str,
    members: &'a [Member],
    excluded: &'a [Excluded],
    wal_partial_bytes: u64,
}

impl Manifest {
    /// Lowercase hex sha256 over the canonical field order.
    #[must_use]
    pub fn digest(&self) -> String {
        let core = ManifestCore {
            format_version: self.format_version,
            schema_version: self.schema_version,
            eris_version: &self.eris_version,
            created_at: &self.created_at,
            host: &self.host,
            members: &self.members,
            excluded: &self.excluded,
            wal_partial_bytes: self.wal_partial_bytes,
        };
        match serde_json::to_vec(&core) {
            Ok(bytes) => sha256_hex(&bytes),
            // Serialising a struct of plain owned values cannot fail; if it ever
            // does, an empty digest can never match a real one, so the archive is
            // refused instead of being accepted.
            Err(_) => String::new(),
        }
    }

    /// Fill in `manifest_sha256` from the current fields.
    pub fn seal(&mut self) {
        self.manifest_sha256 = self.digest();
    }

    /// Whether the stored digest covers exactly the fields present.
    #[must_use]
    pub fn digest_matches(&self) -> bool {
        !self.manifest_sha256.is_empty() && self.manifest_sha256 == self.digest()
    }

    /// Check everything a restore relies on before any bytes move.
    ///
    /// This is deliberately paranoid: it is the only place a manifest path becomes
    /// a filesystem path, so a malformed or hostile manifest never reaches the
    /// extraction step.
    pub fn validate(&self) -> Result<(), BackupError> {
        if self.format_version != FORMAT_VERSION {
            return Err(BackupError::UnsupportedFormat {
                found: self.format_version,
                supported: FORMAT_VERSION,
            });
        }
        if self.members.is_empty() {
            return Err(BackupError::Manifest {
                message: "the manifest lists no members".to_owned(),
            });
        }
        let mut seen: Vec<&str> = Vec::with_capacity(self.members.len());
        for member in &self.members {
            member_root(&member.path)?;
            if !is_known_kind(&member.kind) {
                return Err(BackupError::Manifest {
                    message: format!("member {} has unknown kind {:?}", member.path, member.kind),
                });
            }
            if !is_sha256_hex(&member.sha256) {
                return Err(BackupError::Manifest {
                    message: format!("member {} has a malformed sha256", member.path),
                });
            }
            if seen.contains(&member.path.as_str()) {
                return Err(BackupError::Manifest {
                    message: format!("member {} appears twice", member.path),
                });
            }
            seen.push(&member.path);
        }
        if !self.members.iter().any(|m| m.path == DB_PATH) {
            return Err(BackupError::Manifest {
                message: format!("the manifest has no {DB_PATH} member"),
            });
        }
        for excluded in &self.excluded {
            // Excluded entries are informational, but a hostile one must not be
            // able to name a path outside the roots either: the restore report
            // echoes them back.
            member_root(&excluded.path)?;
        }
        Ok(())
    }
}

/// Split a manifest path into its root and the relative remainder.
///
/// Refuses, in this order: empty, NUL, any absolute spelling, a `.` or `..`
/// component anywhere, a first component outside the three roots, and a path that
/// names a root without naming a file inside it. That is the whole zip-slip
/// defence: a member that survives this cannot resolve outside the target root.
pub fn member_root(path: &str) -> Result<(MemberRoot, PathBuf), BackupError> {
    let reject = |reason: &'static str| {
        Err(BackupError::MemberPath {
            path: path.to_owned(),
            reason,
        })
    };

    if path.is_empty() {
        return reject("the member path is empty");
    }
    if path.contains('\0') {
        return reject("the member path contains a NUL byte");
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return reject("the member path is absolute");
    }
    // `Path::components` silently drops interior `.` components and repeated
    // separators, so the raw segments are checked first: the refusal must be a
    // property of the manifest string, not of path normalisation.
    for segment in path.split('/') {
        if segment == "." {
            return reject("the member path contains a . component");
        }
        if segment == ".." {
            return reject("the member path contains ..");
        }
    }
    let as_path = Path::new(path);
    if as_path.is_absolute() {
        return reject("the member path is absolute");
    }

    let mut components = as_path.components();
    let Some(first) = components.next() else {
        return reject("the member path is empty");
    };
    let root = match first {
        Component::Normal(name) if name == ROOT_STATE => MemberRoot::State,
        Component::Normal(name) if name == ROOT_CONFIG => MemberRoot::Config,
        Component::Normal(name) if name == ROOT_WORKSPACE => MemberRoot::Workspace,
        _ => {
            return reject("the member path does not start with state/, config/ or workspace/");
        }
    };

    let mut rest = PathBuf::new();
    let mut named_file = false;
    for component in components {
        match component {
            Component::Normal(name) => {
                rest.push(name);
                named_file = true;
            }
            Component::CurDir => return reject("the member path contains a . component"),
            Component::ParentDir => return reject("the member path contains .."),
            Component::RootDir | Component::Prefix(_) => {
                return reject("the member path is absolute");
            }
        }
    }
    if !named_file {
        return reject("the member path names a root, not a file inside it");
    }
    Ok((root, rest))
}

/// Whether a member kind is one this binary knows how to place.
#[must_use]
pub fn is_known_kind(kind: &str) -> bool {
    matches!(
        kind,
        KIND_DB | KIND_SESSION_SEGMENT | KIND_SESSION_WAL | KIND_ENV | KIND_TOML | KIND_WORKSPACE
    )
}

/// Lowercase hex sha256 of a byte slice.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// Whether a string is exactly 64 lowercase hex characters.
#[must_use]
pub fn is_sha256_hex(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with(members: Vec<Member>) -> Manifest {
        let mut manifest = Manifest {
            format_version: FORMAT_VERSION,
            schema_version: 4,
            eris_version: "1.0.0".to_owned(),
            created_at: "2026-10-19T00:00:00Z".to_owned(),
            host: "host".to_owned(),
            members,
            excluded: Vec::new(),
            wal_partial_bytes: 0,
            manifest_sha256: String::new(),
        };
        manifest.seal();
        manifest
    }

    fn db_member() -> Member {
        Member {
            path: DB_PATH.to_owned(),
            kind: KIND_DB.to_owned(),
            bytes: 3,
            sha256: sha256_hex(b"abc"),
        }
    }

    #[test]
    fn the_magic_is_twelve_bytes() {
        assert_eq!(MAGIC.len(), 12);
        assert_eq!(LENGTH_BYTES + MAGIC.len(), 16);
    }

    #[test]
    fn the_digest_covers_every_field_but_itself() {
        let mut manifest = manifest_with(vec![db_member()]);
        assert!(manifest.digest_matches());

        manifest.host = "elsewhere".to_owned();
        assert!(!manifest.digest_matches(), "host is covered");

        manifest.seal();
        assert!(manifest.digest_matches());

        manifest.manifest_sha256 = "0".repeat(64);
        assert!(!manifest.digest_matches());
    }

    #[test]
    fn the_digest_is_stable_across_a_json_round_trip() {
        let manifest = manifest_with(vec![db_member()]);
        let text = serde_json::to_string(&manifest).expect("serialises");
        let parsed: Manifest = serde_json::from_str(&text).expect("parses");
        assert_eq!(parsed.digest(), manifest.digest());
        assert_eq!(parsed.manifest_sha256, manifest.manifest_sha256);
    }

    #[test]
    fn a_member_path_must_stay_inside_its_root() {
        for path in [
            "/etc/passwd",
            "//etc/passwd",
            "state/../../etc/passwd",
            "../state/eris.db",
            "state/./eris.db",
            "",
            "state",
            "elsewhere/eris.db",
            "state\\..\\eris.db",
        ] {
            let error = member_root(path).expect_err(path);
            assert!(
                matches!(error, BackupError::MemberPath { .. }),
                "{path:?} gave {error:?}"
            );
        }
    }

    #[test]
    fn the_documented_member_paths_split() {
        assert_eq!(
            member_root("state/eris.db").expect("db").0,
            MemberRoot::State
        );
        assert_eq!(
            member_root("state/sessions/s-1/wal.jsonl").expect("wal").1,
            PathBuf::from("sessions/s-1/wal.jsonl")
        );
        assert_eq!(
            member_root("config/eris.env").expect("env").0,
            MemberRoot::Config
        );
        assert_eq!(
            member_root("workspace/notes.txt").expect("ws").0,
            MemberRoot::Workspace
        );
    }

    #[test]
    fn a_manifest_without_a_database_is_refused() {
        let manifest = manifest_with(vec![Member {
            path: ENV_PATH.to_owned(),
            kind: KIND_ENV.to_owned(),
            bytes: 1,
            sha256: sha256_hex(b"x"),
        }]);
        assert!(matches!(
            manifest.validate(),
            Err(BackupError::Manifest { .. })
        ));
    }

    #[test]
    fn a_duplicate_member_is_refused() {
        let manifest = manifest_with(vec![db_member(), db_member()]);
        assert!(matches!(
            manifest.validate(),
            Err(BackupError::Manifest { .. })
        ));
    }

    #[test]
    fn a_newer_format_is_refused() {
        let mut manifest = manifest_with(vec![db_member()]);
        manifest.format_version = FORMAT_VERSION + 1;
        manifest.seal();
        assert!(matches!(
            manifest.validate(),
            Err(BackupError::UnsupportedFormat { .. })
        ));
    }

    #[test]
    fn a_malformed_digest_is_refused() {
        let mut manifest = manifest_with(vec![db_member()]);
        manifest.members[0].sha256 = "not-a-digest".to_owned();
        manifest.seal();
        assert!(matches!(
            manifest.validate(),
            Err(BackupError::Manifest { .. })
        ));
        assert!(!is_sha256_hex("ABC"));
        assert!(is_sha256_hex(&"a".repeat(64)));
    }

    #[test]
    fn the_path_traversal_verdict_is_lexical_not_filesystem_dependent() {
        // A root-relative spelling that only escapes after a symlink resolves is
        // still refused here, because `..` is refused outright.
        let error = member_root("state/a/../../../b").expect_err("must be refused");
        assert!(matches!(error, BackupError::MemberPath { .. }));
    }
}
