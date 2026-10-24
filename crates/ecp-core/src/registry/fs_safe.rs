//! Filesystem helpers for cross-platform replace/delete semantics.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static RETIRED_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        return replace_file_windows(from, to);
    }
    #[cfg(not(windows))]
    fs::rename(from, to)
}

#[cfg(windows)]
pub(crate) fn with_windows_retry<T, F: FnMut() -> io::Result<T>>(mut f: F) -> io::Result<T> {
    use std::time::Duration;
    let mut last_err = None;
    for attempt in 0..100 {
        match f() {
            Ok(val) => return Ok(val),
            Err(err) => {
                let raw = err.raw_os_error();
                if raw != Some(5) && raw != Some(32) {
                    return Err(err);
                }
                last_err = Some(err);
                // Backoff: 5ms, 6ms, 7ms ... up to 104ms
                std::thread::sleep(Duration::from_millis(5 + attempt as u64));
            }
        }
    }
    Err(last_err.unwrap_or_else(io::Error::last_os_error))
}

#[cfg(not(windows))]
#[inline(always)]
pub(crate) fn with_windows_retry<T, F: FnMut() -> io::Result<T>>(mut f: F) -> io::Result<T> {
    f()
}

pub fn rename_with_retry(from: &Path, to: &Path) -> io::Result<()> {
    with_windows_retry(|| fs::rename(from, to))
}

#[cfg(windows)]
fn replace_file_windows(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    let from_wide: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to_wide: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();

    with_windows_retry(|| {
        let ok = unsafe {
            MoveFileExW(
                from_wide.as_ptr(),
                to_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if ok != 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
}

/// Flush a directory's own entries (a rename into it, a file created in it)
/// to stable storage. The commit publish calls it on `commits/` after the
/// rename, so a crash cannot keep a later gc's delete of the previous build
/// and lose the name of the new one.
///
/// Windows has no directory fsync in std: the dir is opened with
/// `FILE_FLAG_BACKUP_SEMANTICS` and flushed with `FlushFileBuffers`. Access
/// denied there means the volume does not support it and reads as success;
/// the rename then rests on NTFS's ordered metadata log. macOS `fsync` does
/// not flush the drive cache (`F_FULLFSYNC` would); neither gap is tested.
pub fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const ERROR_ACCESS_DENIED: i32 = 5;
        let unsupported = |e: &io::Error| e.raw_os_error() == Some(ERROR_ACCESS_DENIED);
        let dir = match fs::OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(path)
        {
            Ok(dir) => dir,
            Err(e) if unsupported(&e) => return Ok(()),
            Err(e) => return Err(e),
        };
        return match dir.sync_all() {
            Err(e) if unsupported(&e) => Ok(()),
            other => other,
        };
    }
    #[cfg(not(windows))]
    fs::File::open(path)?.sync_all()
}

pub fn retire_dir(path: &Path) -> io::Result<Option<PathBuf>> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => {
            return Err(io::Error::other(format!(
                "not a directory: {}",
                path.display()
            )))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    }

    let retired = retired_dir_path(path);
    rename_with_retry(path, &retired)?;
    Ok(Some(retired))
}

pub fn retire_dir_async(path: &Path) -> io::Result<Option<PathBuf>> {
    let retired = retire_dir(path)?;
    if let Some(retired_path) = retired.clone() {
        std::thread::spawn(move || {
            // WHY log, not swallow: a short-lived CLI process can exit before
            // this detached thread finishes, leaving a `.dead.*` dir behind.
            // `admin gc` sweeps such leftovers; recording the failure on stderr
            // makes the leak diagnosable instead of silent (FU-2026-05-26-001).
            if let Err(e) = fs::remove_dir_all(&retired_path) {
                eprintln!(
                    "retire_dir_async: background remove of {} failed: {e}",
                    retired_path.display()
                );
            }
        });
    }
    Ok(retired)
}

fn retired_dir_path(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let mut name: OsString = path
        .file_name()
        .map(OsString::from)
        .unwrap_or_else(|| OsString::from("dir"));
    name.push(format!(
        ".dead.{}.{}.{}",
        std::process::id(),
        RETIRED_COUNTER.fetch_add(1, Ordering::Relaxed),
        chrono::Utc::now().timestamp_millis()
    ));
    parent.join(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retire_dir_moves_to_dead_sibling() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("index");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("graph.bin"), b"x").unwrap();

        let retired = retire_dir(&dir).unwrap().unwrap();

        assert!(!dir.exists());
        assert_eq!(retired.parent(), Some(tmp.path()));
        assert!(retired
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("index.dead."));
        assert!(retired.join("graph.bin").exists());
    }

    #[test]
    fn test_sync_dir_directory_ok_missing_err() {
        let tmp = tempfile::tempdir().unwrap();
        sync_dir(tmp.path()).expect("a directory flushes, or reads as unsupported on Windows");
        assert!(sync_dir(&tmp.path().join("missing")).is_err());
    }

    #[test]
    fn retire_dir_missing_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(retire_dir(&tmp.path().join("missing")).unwrap().is_none());
    }
}
