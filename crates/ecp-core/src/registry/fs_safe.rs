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
    fn retire_dir_missing_is_noop() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(retire_dir(&tmp.path().join("missing")).unwrap().is_none());
    }
}
