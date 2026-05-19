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
    let ok = unsafe {
        MoveFileExW(
            from_wide.as_ptr(),
            to_wide.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
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
    fs::rename(path, &retired)?;
    Ok(Some(retired))
}

pub fn retire_dir_async(path: &Path) -> io::Result<Option<PathBuf>> {
    let retired = retire_dir(path)?;
    if let Some(retired_path) = retired.clone() {
        std::thread::spawn(move || {
            let _ = fs::remove_dir_all(retired_path);
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
