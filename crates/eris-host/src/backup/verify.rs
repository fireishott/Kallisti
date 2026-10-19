//! Reading and verifying an archive.
//!
//! Reading is deliberately two-phase, because the two questions are different:
//!
//! - [`open`] answers "is this a Kallisti archive whose manifest I can trust?" -
//!   magic, length, JSON, `manifest_sha256`, and the path and kind validation in
//!   [`Manifest::validate`]. It never touches the payload.
//! - [`verify`] answers "is every member the bytes the manifest names?" - it
//!   decodes the payload and hashes each member into a sink, writing nothing.
//!
//! That split is why a restore can verify every member before it writes a byte,
//! and why `--list` works on an archive whose payload is corrupt.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::backup::BackupError;
use crate::backup::format::{
    LENGTH_BYTES, MAGIC, MAX_MANIFEST_BYTES, Manifest, Member, is_sha256_hex, member_root,
    sha256_hex,
};

/// An archive whose manifest has been read and trusted.
#[derive(Debug, Clone)]
pub struct OpenedArchive {
    /// The file on disk.
    pub path: PathBuf,
    /// The validated manifest.
    pub manifest: Manifest,
    /// Byte offset where the zstd payload starts.
    pub payload_offset: u64,
    /// Size of the whole archive file.
    pub archive_bytes: u64,
    /// Lowercase hex sha256 of the whole archive file.
    pub archive_sha256: String,
}

/// What a verification pass found.
#[derive(Debug, Clone)]
pub struct VerifyReport {
    /// Members checked.
    pub members: usize,
    /// Uncompressed payload bytes checked.
    pub bytes: u64,
    /// The manifest digest that was covered.
    pub manifest_sha256: String,
    /// Bytes the manifest says were dropped from a live WAL.
    pub wal_partial_bytes: u64,
}

impl VerifyReport {
    /// The one JSON object a verification prints.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "ok": true,
            "members": self.members,
            "bytes": self.bytes,
            "manifest_sha256": self.manifest_sha256,
            "wal_partial_bytes": self.wal_partial_bytes,
        })
    }
}

/// Read the magic and the manifest, and trust both or neither.
pub fn open(path: &Path) -> Result<OpenedArchive, BackupError> {
    let metadata = std::fs::metadata(path).map_err(|error| BackupError::io(path, error))?;
    let archive_bytes = metadata.len();

    let mut file = File::open(path).map_err(|error| BackupError::io(path, error))?;
    let mut magic = [0u8; MAGIC.len()];
    if file.read_exact(&mut magic).is_err() || &magic != MAGIC {
        return Err(BackupError::BadMagic {
            path: path.to_path_buf(),
        });
    }

    let mut length_bytes = [0u8; LENGTH_BYTES];
    file.read_exact(&mut length_bytes)
        .map_err(|_| BackupError::Manifest {
            message: "the archive ends before the manifest length".to_owned(),
        })?;
    let length = u32::from_le_bytes(length_bytes);
    if length == 0 || length > MAX_MANIFEST_BYTES {
        return Err(BackupError::ManifestTooLarge {
            found: length,
            max: MAX_MANIFEST_BYTES,
        });
    }
    let manifest_at = (MAGIC.len() + LENGTH_BYTES) as u64;
    if archive_bytes < manifest_at + u64::from(length) {
        return Err(BackupError::Manifest {
            message: format!(
                "the archive is {archive_bytes} bytes, shorter than its {length} byte manifest"
            ),
        });
    }

    let mut manifest_bytes = vec![0u8; length as usize];
    file.read_exact(&mut manifest_bytes)
        .map_err(|_| BackupError::Manifest {
            message: "the manifest is truncated".to_owned(),
        })?;
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).map_err(|error| BackupError::Manifest {
            message: format!("the manifest is not readable JSON: {error}"),
        })?;

    if !is_sha256_hex(&manifest.manifest_sha256) || !manifest.digest_matches() {
        return Err(BackupError::ManifestDigest {
            recorded: manifest.manifest_sha256.clone(),
            computed: manifest.digest(),
        });
    }
    manifest.validate()?;

    let (_, archive_sha256) = crate::backup::create::hash_file(path)?;

    Ok(OpenedArchive {
        path: path.to_path_buf(),
        manifest,
        payload_offset: manifest_at + u64::from(length),
        archive_bytes,
        archive_sha256,
    })
}

/// A reader that yields at most `remaining` bytes and refuses to run short.
///
/// The payload is a bare concatenation, so the only thing that says where one
/// member ends is the manifest's byte count. A payload that ends early is a
/// corrupt archive, not a short read.
struct LimitedReader<'a, R: Read> {
    inner: &'a mut R,
    remaining: u64,
}

impl<R: Read> Read for LimitedReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let wanted = buffer
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let read = self.inner.read(&mut buffer[..wanted])?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "the payload ended inside a member",
            ));
        }
        self.remaining -= read as u64;
        Ok(read)
    }
}

/// Walk the payload in manifest order, handing each member's bytes to `sink`.
///
/// `sink` may read less than the whole member; the remainder is drained so the
/// next member starts at the right offset, and a short payload is an error.
fn walk_payload<F>(opened: &OpenedArchive, mut sink: F) -> Result<(), BackupError>
where
    F: FnMut(&Member, &mut dyn Read) -> Result<(), BackupError>,
{
    let mut file =
        File::open(&opened.path).map_err(|error| BackupError::io(&opened.path, error))?;
    file.seek(SeekFrom::Start(opened.payload_offset))
        .map_err(|error| BackupError::io(&opened.path, error))?;
    let mut decoder = zstd::stream::read::Decoder::new(BufReader::new(file)).map_err(|error| {
        BackupError::Payload {
            message: format!("the zstd frame cannot be opened: {error}"),
        }
    })?;

    let mut scratch = vec![0u8; 64 * 1024];
    for member in &opened.manifest.members {
        let mut limited = LimitedReader {
            inner: &mut decoder,
            remaining: member.bytes,
        };
        sink(member, &mut limited)?;
        while limited.remaining > 0 {
            let drained = limited
                .read(&mut scratch)
                .map_err(|error| BackupError::Payload {
                    message: format!("member {} is short: {error}", member.path),
                })?;
            if drained == 0 {
                return Err(BackupError::Payload {
                    message: format!("member {} ended early", member.path),
                });
            }
        }
    }
    Ok(())
}

/// Verify every member's sha256 without writing anything.
pub fn verify(opened: &OpenedArchive) -> Result<VerifyReport, BackupError> {
    let mut total = 0u64;
    walk_payload(opened, |member, reader| {
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|error| BackupError::Payload {
                    message: format!("member {} cannot be read: {error}", member.path),
                })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            total += read as u64;
        }
        let actual = hex::encode(hasher.finalize());
        if actual != member.sha256 {
            return Err(BackupError::MemberChecksum {
                path: member.path.clone(),
                expected: member.sha256.clone(),
                actual,
            });
        }
        Ok(())
    })?;

    Ok(VerifyReport {
        members: opened.manifest.members.len(),
        bytes: total,
        manifest_sha256: opened.manifest.manifest_sha256.clone(),
        wal_partial_bytes: opened.manifest.wal_partial_bytes,
    })
}

/// Extract every member under `root`, keeping the manifest's relative layout.
///
/// The manifest paths were validated when the archive was opened, and they are
/// re-validated here: this is the only function that turns one into a
/// filesystem path. Each member is re-hashed as it is written, so a payload that
/// changes between the verification pass and this one still cannot land.
pub fn extract(opened: &OpenedArchive, root: &Path) -> Result<Vec<PathBuf>, BackupError> {
    let mut written = Vec::new();
    walk_payload(opened, |member, reader| {
        // Validate the member path, then keep its `<root>/<relative>` spelling so
        // the staged tree mirrors the manifest exactly.
        member_root(&member.path)?;
        let target = root.join(&member.path);
        if !target.starts_with(root) {
            return Err(BackupError::MemberPath {
                path: member.path.clone(),
                reason: "the member path resolves outside the staging root",
            });
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| BackupError::io(parent, error))?;
        }
        let mut file = File::create(&target).map_err(|error| BackupError::io(&target, error))?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = reader
                .read(&mut buffer)
                .map_err(|error| BackupError::Payload {
                    message: format!("member {} cannot be read: {error}", member.path),
                })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .map_err(|error| BackupError::io(&target, error))?;
        }
        file.sync_all()
            .map_err(|error| BackupError::io(&target, error))?;
        let actual = hex::encode(hasher.finalize());
        if actual != member.sha256 {
            return Err(BackupError::MemberChecksum {
                path: member.path.clone(),
                expected: member.sha256.clone(),
                actual,
            });
        }
        written.push(target);
        Ok(())
    })?;
    Ok(written)
}

/// Read one member into memory. Used by `--dry-run` and by the tests.
pub fn read_member_bytes(opened: &OpenedArchive, path: &str) -> Result<Vec<u8>, BackupError> {
    let mut found: Option<Vec<u8>> = None;
    walk_payload(opened, |member, reader| {
        if member.path != path {
            return Ok(());
        }
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .map_err(|error| BackupError::Payload {
                message: format!("member {} cannot be read: {error}", member.path),
            })?;
        if sha256_hex(&bytes) != member.sha256 {
            return Err(BackupError::MemberChecksum {
                path: member.path.clone(),
                expected: member.sha256.clone(),
                actual: sha256_hex(&bytes),
            });
        }
        found = Some(bytes);
        Ok(())
    })?;
    found.ok_or_else(|| BackupError::Manifest {
        message: format!("the manifest has no member {path}"),
    })
}
