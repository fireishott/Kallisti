//! Sealed segments: immutable, compressed, hash-linked history.
//!
//! A segment is the only compressed thing in the log, and it is written exactly
//! once: `session.v1.<first_seq>-<last_seq>.jsonl.zst`, holding the same JSON
//! lines the WAL held, zstd-compressed. The range is in the name so segments
//! sort without being read and so a reader can see the coverage without
//! decompressing anything.
//!
//! Two rules are enforced here rather than trusted downstream:
//!
//! - writing is atomic and durable (temp file, fsync, rename, fsync dir) before
//!   the caller is allowed to truncate the WAL;
//! - reading a segment that will not decompress or will not parse is
//!   [`LogError::SegmentDecode`] or [`LogError::CorruptSegment`]. A sealed
//!   segment is never trimmed, never skipped, and never partially believed.

use std::path::{Path, PathBuf};

use crate::log::{Envelope, LogError, SEGMENT_COMPRESSION_LEVEL, SEGMENT_SUFFIX, fsync_dir};

/// Prefix every sealed segment file name starts with.
pub const SEGMENT_PREFIX: &str = "session.v1";

/// One sealed segment on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealedSegment {
    /// Path of the segment file.
    pub path: PathBuf,
    /// Sequence number of the first envelope it holds.
    pub first_seq: u64,
    /// Sequence number of the last envelope it holds.
    pub last_seq: u64,
    /// How many envelopes it holds.
    pub envelopes: usize,
    /// Size of the compressed file in bytes.
    pub segment_bytes: u64,
}

/// File name for the segment covering `first_seq..=last_seq`.
#[must_use]
pub fn segment_file_name(first_seq: u64, last_seq: u64) -> String {
    format!("{SEGMENT_PREFIX}.{first_seq}-{last_seq}{SEGMENT_SUFFIX}")
}

/// Compress and write one segment, durably.
///
/// # Errors
///
/// Any filesystem or compression failure. When this returns `Err`, no segment
/// file exists and the caller must not truncate the WAL.
pub(crate) fn write(
    dir: &Path,
    first_seq: u64,
    last_seq: u64,
    envelopes: &[Envelope],
) -> Result<SealedSegment, LogError> {
    let file_name = segment_file_name(first_seq, last_seq);
    let path = dir.join(&file_name);

    let mut payload = Vec::new();
    for envelope in envelopes {
        payload.extend_from_slice(envelope.to_line()?.as_bytes());
        payload.push(b'\n');
    }

    let compressed =
        zstd::encode_all(payload.as_slice(), SEGMENT_COMPRESSION_LEVEL).map_err(|source| {
            LogError::SegmentEncode {
                path: path.clone(),
                source,
            }
        })?;

    let temp = dir.join(format!("{file_name}.tmp"));
    {
        use std::io::Write as _;

        let mut file =
            std::fs::File::create(&temp).map_err(|source| LogError::io(&temp, source))?;
        file.write_all(&compressed)
            .map_err(|source| LogError::io(&temp, source))?;
        file.sync_all()
            .map_err(|source| LogError::io(&temp, source))?;
    }
    std::fs::rename(&temp, &path).map_err(|source| LogError::io(&path, source))?;
    fsync_dir(dir)?;

    Ok(SealedSegment {
        path,
        first_seq,
        last_seq,
        envelopes: envelopes.len(),
        segment_bytes: u64::try_from(compressed.len()).unwrap_or(u64::MAX),
    })
}

/// Read and fully validate one segment.
///
/// Every line must be terminable and parseable. A decode or parse failure is an
/// error: sealed history is authoritative or it is broken, there is no third
/// state.
///
/// # Errors
///
/// [`LogError::SegmentDecode`] on a zstd failure, [`LogError::CorruptSegment`]
/// when the decompressed bytes are not clean JSONL.
pub(crate) fn read(path: &Path) -> Result<Vec<Envelope>, LogError> {
    let raw = std::fs::read(path).map_err(|source| LogError::io(path, source))?;
    let decoded = zstd::decode_all(raw.as_slice()).map_err(|source| LogError::SegmentDecode {
        path: path.to_path_buf(),
        source,
    })?;

    let mut envelopes = Vec::new();
    let mut rest = decoded.as_slice();
    while !rest.is_empty() {
        let Some(newline) = rest.iter().position(|byte| *byte == b'\n') else {
            return Err(LogError::CorruptSegment {
                path: path.to_path_buf(),
            });
        };
        let envelope =
            Envelope::from_line(&rest[..newline]).map_err(|_| LogError::CorruptSegment {
                path: path.to_path_buf(),
            })?;
        envelopes.push(envelope);
        rest = &rest[newline + 1..];
    }

    Ok(envelopes)
}

/// List every sealed segment in `dir`, ordered by first sequence number.
///
/// Recognises both the range-qualified name this build writes
/// (`session.v1.<first>-<last>.jsonl.zst`) and a bare `session.v1.jsonl.zst`, so
/// a hand-placed segment or one written by an older build still replays.
pub(crate) fn list(dir: &Path) -> Result<Vec<PathBuf>, LogError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(LogError::io(dir, source)),
    };

    let mut segments: Vec<(u64, String, PathBuf)> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| LogError::io(dir, source))?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.ends_with(".tmp")
            || !name.starts_with(SEGMENT_PREFIX)
            || !name.ends_with(SEGMENT_SUFFIX)
        {
            continue;
        }
        let first_seq = parse_first_seq(name).unwrap_or(0);
        segments.push((first_seq, name.to_owned(), entry.path()));
    }

    segments.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    Ok(segments.into_iter().map(|(_, _, path)| path).collect())
}

fn parse_first_seq(name: &str) -> Option<u64> {
    let bare = name.strip_prefix(SEGMENT_PREFIX)?.strip_prefix('.')?;
    let range = bare.strip_suffix(SEGMENT_SUFFIX)?;
    let (first, _last) = range.split_once('-')?;
    first.parse().ok()
}
