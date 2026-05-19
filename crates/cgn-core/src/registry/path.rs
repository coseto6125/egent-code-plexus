//! Path sanitization, repo/branch derivation, UID path normalization.

use std::path::{Path, PathBuf};
use thiserror::Error;
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Error)]
pub enum PathError {
    #[error("segment is empty")]
    Empty,
    #[error("segment exceeds 64 chars: {0}")]
    TooLong(String),
    #[error("segment contains illegal char or pattern: {0}")]
    Illegal(String),
}

/// Validate a single path segment (e.g. `<repo>` or `<branch>`) for use
/// inside `~/.cgn/`. Whitelist `[A-Za-z0-9_.-]+`, reject `..`, reject
/// leading `-` or `.`, max 64 chars.
pub fn sanitize_segment(s: &str) -> Result<String, PathError> {
    if s.is_empty() {
        return Err(PathError::Empty);
    }
    if s.len() > 64 {
        return Err(PathError::TooLong(s.to_string()));
    }
    if s.contains("..") || s.starts_with('-') || s.starts_with('.') {
        return Err(PathError::Illegal(s.to_string()));
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        return Err(PathError::Illegal(s.to_string()));
    }
    Ok(s.to_string())
}

/// Extract `<repo>` segment from a git remote URL. Handles SSH
/// (`git@host:user/repo.git`) and HTTPS (`https://host/user/repo.git`).
/// `None` returns Err (caller falls back to working-tree basename).
pub fn derive_repo_name(remote_url: Option<&str>) -> Result<String, PathError> {
    let url = remote_url.ok_or(PathError::Empty)?;
    // Reject if the entire URL contains suspicious path traversal patterns
    if url.contains("..") || url.contains("/../") {
        return Err(PathError::Illegal(url.to_string()));
    }
    let after_colon_or_slash = url
        .rsplit_once([':', '/'])
        .map(|(_, tail)| tail)
        .unwrap_or(url);
    let stripped = after_colon_or_slash
        .strip_suffix(".git")
        .unwrap_or(after_colon_or_slash);
    sanitize_segment(stripped)
}

/// Cross-platform stable UID path: repo-relative, forward-slash, NFC.
/// Returns Err if `absolute` isn't under `repo_root`.
pub fn uid_path(absolute: &Path, repo_root: &Path) -> Result<String, PathError> {
    let rel = absolute
        .strip_prefix(repo_root)
        .map_err(|_| PathError::Illegal(format!("{absolute:?} not under {repo_root:?}")))?;
    let s = rel.to_string_lossy().replace('\\', "/");
    Ok(s.nfc().collect())
}

/// Resolve the cgn home directory used for `registry.json` and per-branch
/// index dirs. Tries `$CGN_HOME`, then `$HOME/.cgn`; if neither directory
/// can be created and written to (read-only FS, permission denied, CI
/// sandbox), falls back to `<temp_dir>/cgn-fallback/.cgn`.
///
/// Reads and writes within a single CLI invocation use the same resolved
/// path: a project indexed in fallback mode is queryable from the same
/// environment without extra flags.
///
pub fn resolve_home_cgn() -> PathBuf {
    resolve_home_cgn_from_env(std::env::var_os("CGN_HOME"), std::env::var_os("HOME"))
}

/// Same resolution logic as [`resolve_home_cgn`], but with the HOME source
/// supplied by the caller. In-process tests (or any caller wanting to point
/// cgn at a private home without mutating the process-global `HOME` env
/// var) call this with an explicit override. Production code paths read
/// the env var via [`resolve_home_cgn`].
///
/// `#[allow(dead_code)]` because the only intended caller today is the
/// future in-process integration test refactor; ships now so the public
/// API is in place when that work lands without forcing it into the
/// same PR.
#[allow(dead_code)]
pub fn resolve_home_cgn_from<P: AsRef<Path>>(home: P) -> PathBuf {
    let candidate = home.as_ref().join(".cgn");
    if probe_writable(&candidate) {
        return candidate;
    }
    fallback_home()
}

fn resolve_home_cgn_from_env(
    cgn_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> PathBuf {
    if let Some(path) = cgn_home {
        let candidate = PathBuf::from(path);
        if probe_writable(&candidate) {
            return candidate;
        }
    }
    if let Some(h) = home {
        let candidate = PathBuf::from(h).join(".cgn");
        if probe_writable(&candidate) {
            return candidate;
        }
    }
    fallback_home()
}

fn fallback_home() -> PathBuf {
    std::env::temp_dir().join("cgn-fallback").join(".cgn")
}

fn probe_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".cgn-write-probe");
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_writable_true_for_normal_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(probe_writable(tmp.path()));
        // probe file should be cleaned up
        assert!(!tmp.path().join(".cgn-write-probe").exists());
    }

    #[cfg(unix)]
    #[test]
    fn probe_writable_false_for_readonly_dir() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let ro = tmp.path().join("ro");
        std::fs::create_dir(&ro).unwrap();
        let mut perms = std::fs::metadata(&ro).unwrap().permissions();
        perms.set_mode(0o500); // read+exec, no write
        std::fs::set_permissions(&ro, perms).unwrap();
        assert!(!probe_writable(&ro));
        // restore perms so tempdir cleanup works
        let mut p = std::fs::metadata(&ro).unwrap().permissions();
        p.set_mode(0o700);
        std::fs::set_permissions(&ro, p).unwrap();
    }

    #[test]
    fn probe_writable_false_when_path_is_an_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        assert!(!probe_writable(&file));
    }

    /// Single test covers all `resolve_home_cgn` scenarios sequentially —
    /// HOME is process-global and racing with parallel tests would corrupt
    /// other env readers. Since only `resolve_home_cgn` reads HOME in this
    /// crate, serial mutation inside one test is safe.
    #[test]
    fn resolve_home_cgn_covers_env_override_happy_path_and_fallback() {
        let orig_home = std::env::var_os("HOME");
        let orig_cgn_home = std::env::var_os("CGN_HOME");
        std::env::remove_var("CGN_HOME");

        // (1) HOME unset → tmp fallback
        std::env::remove_var("HOME");
        let p = resolve_home_cgn();
        assert!(
            p.starts_with(std::env::temp_dir()),
            "no-HOME should fall back to temp_dir, got {p:?}"
        );
        assert!(p.ends_with(".cgn"), "fallback path tail should end in .cgn");

        // (2) HOME set + writable, no registry.json → probe runs, returns <HOME>/.cgn, no leftover probe
        let writable = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", writable.path());
        let p = resolve_home_cgn();
        assert_eq!(p, writable.path().join(".cgn"));
        assert!(p.exists(), "probe path should be created");
        assert!(
            !p.join(".cgn-write-probe").exists(),
            "probe file should be cleaned up"
        );

        // (3) CGN_HOME set + writable → use it as the exact cgn root
        let override_home = tempfile::tempdir().unwrap();
        let override_cgn = override_home.path().join("custom-cgn");
        std::env::set_var("CGN_HOME", &override_cgn);
        let p = resolve_home_cgn();
        assert_eq!(p, override_cgn);
        assert!(p.exists(), "CGN_HOME path should be created");
        std::env::remove_var("CGN_HOME");

        // (4) HOME points to read-only dir without registry.json → tmp fallback
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let ro = tempfile::tempdir().unwrap();
            let mut perms = std::fs::metadata(ro.path()).unwrap().permissions();
            perms.set_mode(0o500);
            std::fs::set_permissions(ro.path(), perms).unwrap();
            std::env::set_var("HOME", ro.path());
            let p = resolve_home_cgn();
            assert!(
                p.starts_with(std::env::temp_dir()),
                "read-only HOME should fall back, got {p:?}"
            );
            // restore so tempdir cleanup works
            let mut p2 = std::fs::metadata(ro.path()).unwrap().permissions();
            p2.set_mode(0o700);
            std::fs::set_permissions(ro.path(), p2).unwrap();
        }

        // restore HOME
        match orig_home {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        match orig_cgn_home {
            Some(h) => std::env::set_var("CGN_HOME", h),
            None => std::env::remove_var("CGN_HOME"),
        }
    }
}
