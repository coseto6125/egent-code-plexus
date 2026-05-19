//! Cross-platform advisory file lock for registry serialization.
//! Spec §2.1 Layer 1.

use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

/// RAII exclusive file lock. Lock is released when dropped.
pub struct FileLock {
    _file: File,
}

impl FileLock {
    /// Acquire an exclusive lock on `path`. Blocks until acquired.
    /// Creates the lockfile if missing.
    pub fn acquire_exclusive(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        file.lock_exclusive()?;
        Ok(Self { _file: file })
    }

    /// Try to acquire exclusive lock without blocking.
    pub fn try_exclusive(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)?;
        file.try_lock_exclusive()?;
        Ok(Self { _file: file })
    }
}
