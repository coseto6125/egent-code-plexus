//! Shared atomic JSON write primitive.

use serde::Serialize;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

/// Atomic JSON write: serialize → tmp → fsync → rename. Caller is
/// responsible for any backup/.bak orchestration outside this call.
/// `parent_dir` is created if missing.
pub(super) fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}
