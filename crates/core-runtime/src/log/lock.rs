//! One writer per session, enforced with an advisory `flock` on `.lock`.
//!
//! The lock is advisory rather than mandatory: it guards the log from a second
//! *writer*, not the filesystem from a `cat`. What it buys is the property the
//! WAL depends on - one process owns the append cursor - and it is released by
//! the kernel when the file handle closes, so a crashed writer does not leave a
//! session permanently unopenable.
//!
//! `fs4` is used deliberately: its Unix implementation is `flock(2)`, which
//! conflicts between separate open file descriptions even inside one process.
//! A POSIX `fcntl` record lock would not, and a second `SessionLog::open` in the
//! same process would then silently succeed and corrupt the chain.

use std::fs::File;
use std::path::{Path, PathBuf};

use crate::log::{LOCK_FILE_NAME, LogError};

/// An exclusive, held lock on a session directory.
///
/// Released on drop, and by the kernel if the process dies.
#[derive(Debug)]
pub struct SessionLock {
    path: PathBuf,
    file: File,
}

impl SessionLock {
    /// Take the exclusive lock for `dir`, creating `.lock` if needed.
    ///
    /// # Errors
    ///
    /// [`LogError::AlreadyLocked`] when another writer holds it, or an I/O
    /// failure opening or locking the file.
    pub fn acquire(dir: &Path) -> Result<Self, LogError> {
        let path = dir.join(LOCK_FILE_NAME);
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|source| LogError::io(&path, source))?;

        // Fully qualified on purpose: std 1.99 added an inherent `File::try_lock`,
        // which would otherwise win method resolution and quietly replace the
        // pinned `fs4` (flock-based) path with std's. The lock we take must be the
        // one this crate documents and tests.
        match <File as fs4::FileExt>::try_lock(&file) {
            Ok(()) => Ok(Self { path, file }),
            Err(fs4::TryLockError::WouldBlock) => Err(LogError::AlreadyLocked { path }),
            Err(fs4::TryLockError::Error(source)) => Err(LogError::io(&path, source)),
        }
    }

    /// Path of the lock file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SessionLock {
    fn drop(&mut self) {
        // Closing the handle releases the lock too; unlock explicitly so the
        // release happens at a defined moment rather than at fd teardown.
        let _unlocked = <File as fs4::FileExt>::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use core_domain::SessionId;

    use crate::log::{LogError, SessionLog};

    #[test]
    fn second_writer_gets_already_locked() {
        let root = tempfile::tempdir().expect("tempdir");
        let session_id = SessionId::now_v7();

        let first = SessionLog::open(root.path(), session_id).expect("first writer opens");

        let error =
            SessionLog::open(root.path(), session_id).expect_err("a second writer must be refused");
        match error {
            LogError::AlreadyLocked { path } => {
                assert!(path.ends_with(".lock"), "unexpected lock path: {path:?}");
            }
            other => panic!("expected AlreadyLocked, got {other:?}"),
        }

        // Releasing the writer releases the lock.
        drop(first);
        let third = SessionLog::open(root.path(), session_id).expect("lock is released on drop");
        assert_eq!(third.last_seq(), 0);
    }
}
