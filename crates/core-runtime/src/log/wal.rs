//! The live write-ahead log: plain UTF-8, one JSON object per line, fsynced
//! before an ack, never compressed.
//!
//! Nothing in this module seals, compresses, or repairs. It appends bytes,
//! reads bytes, scans them into envelopes, and truncates or rewrites the file.
//! Recovery policy lives in [`crate::log::recover`]; compression lives in
//! [`crate::log::segment`]. That split is what keeps "no zstd on the live WAL"
//! checkable by reading one small file.

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::log::{Envelope, LogError, fsync_dir};

/// Result of scanning WAL bytes.
#[derive(Debug, Default)]
pub(crate) struct Scan {
    /// Every complete, parseable envelope, in file order.
    pub envelopes: Vec<Envelope>,
    /// Bytes of a torn tail that were not turned into envelopes: a final line
    /// with no terminator, or a final line that does not parse.
    pub torn: Option<u64>,
}

/// Append one envelope and fsync before returning.
///
/// The newline is written as part of the same buffer as the envelope, and
/// `sync_all` is on the path that produces the acknowledgement, so an envelope
/// that this function returned is a byte sequence already on disk.
pub(crate) fn append_line(path: &Path, envelope: &Envelope) -> Result<(), LogError> {
    let line = envelope.to_line()?;
    let mut buffer = Vec::with_capacity(line.len() + 1);
    buffer.extend_from_slice(line.as_bytes());
    buffer.push(b'\n');

    let existing = path.exists();
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|source| LogError::io(path, source))?;

    file.write_all(&buffer)
        .map_err(|source| LogError::io(path, source))?;
    file.sync_all()
        .map_err(|source| LogError::io(path, source))?;
    // Only a newly created WAL needs its directory entry made durable; paying a
    // second fsync on every append would double the ack latency for nothing.
    if !existing && let Some(parent) = path.parent() {
        fsync_dir(parent)?;
    }
    Ok(())
}

/// Read the WAL, treating a missing file as an empty WAL.
pub(crate) fn read(path: &Path) -> Result<Vec<u8>, LogError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(source) => Err(LogError::io(path, source)),
    }
}

/// Split WAL bytes into envelopes plus, at most, one torn tail.
///
/// The distinction this function draws is the one that matters downstream:
///
/// - a final line without a terminator, or a final line that does not parse,
///   is a *torn tail*: the writer died mid-append, the bytes were never
///   acknowledged, and they are trimmable;
/// - a broken line with anything after it is *corruption*: acknowledged data
///   may follow it, so it is an error, never a trim.
pub(crate) fn scan(bytes: &[u8]) -> Result<Scan, LogError> {
    let mut scan = Scan::default();
    let mut position = 0_usize;

    while position < bytes.len() {
        let rest = &bytes[position..];
        let Some(newline) = rest.iter().position(|byte| *byte == b'\n') else {
            scan.torn = Some(byte_len(rest));
            break;
        };

        let line = &rest[..newline];
        match Envelope::from_line(line) {
            Ok(envelope) => scan.envelopes.push(envelope),
            // Unparseable, but it is the last line: trimmable.
            Err(_) if position + newline + 1 == bytes.len() => {
                scan.torn = Some(byte_len(&bytes[position..]));
                break;
            }
            // Unparseable with more data behind it: fatal.
            Err(_) => {
                return Err(LogError::CorruptWal {
                    offset: byte_len(&bytes[..position]),
                });
            }
        }

        position += newline + 1;
    }

    Ok(scan)
}

/// Replace the WAL atomically with exactly `envelopes`.
///
/// Used by recovery to drop a WAL prefix that a segment already covers, or to
/// drop a torn tail. Written to a temp file, fsynced, renamed, and the directory
/// fsynced, so a crash mid-rewrite leaves either the old WAL or the new one.
pub(crate) fn rewrite(path: &Path, envelopes: &[Envelope]) -> Result<(), LogError> {
    let mut buffer = Vec::new();
    for envelope in envelopes {
        buffer.extend_from_slice(envelope.to_line()?.as_bytes());
        buffer.push(b'\n');
    }

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temp = path.with_extension("jsonl.tmp");

    {
        let mut file =
            std::fs::File::create(&temp).map_err(|source| LogError::io(&temp, source))?;
        file.write_all(&buffer)
            .map_err(|source| LogError::io(&temp, source))?;
        file.sync_all()
            .map_err(|source| LogError::io(&temp, source))?;
    }

    std::fs::rename(&temp, path).map_err(|source| LogError::io(path, source))?;
    fsync_dir(parent)
}

/// Truncate the WAL to zero bytes and make the truncation durable.
pub(crate) fn truncate(path: &Path) -> Result<(), LogError> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|source| LogError::io(path, source))?;
    file.sync_all()
        .map_err(|source| LogError::io(path, source))?;

    if let Some(parent) = path.parent() {
        fsync_dir(parent)?;
    }
    Ok(())
}

/// Well-known WAL path inside a session directory.
#[must_use]
pub fn wal_path(dir: &Path) -> PathBuf {
    dir.join(crate::log::WAL_FILE_NAME)
}

fn byte_len(bytes: &[u8]) -> u64 {
    u64::try_from(bytes.len()).unwrap_or(u64::MAX)
}
