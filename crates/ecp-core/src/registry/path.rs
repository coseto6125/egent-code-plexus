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
/// inside `~/.ecp/`. Whitelist `[A-Za-z0-9_.-]+`, reject `..`, reject
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

/// Require `name` to be one ordinary path component before it is joined onto
/// a cache root.
///
/// Group names and peer session ids are caller-supplied and end up as
/// directories under `~/.ecp`. Without this, `ecp group sync ../../../X` wrote
/// `contracts.rkyv` and `meta.json` into `X` three levels above the cache root,
/// and an absolute name replaced the prefix outright — measured on 0.13.0,
/// with `groups/` left empty in both cases.
///
/// Both separators are rejected on every platform, not only on Windows: a name
/// carrying `\\` is meaningless as a single component anywhere, and accepting it
/// on Unix would let the same value mean two different directories depending on
/// which machine read it.
pub fn validate_cache_component(name: &str) -> std::io::Result<()> {
    let mut components = Path::new(name).components();
    // Unix components do not recognize Windows separators or drive prefixes.
    let windows_drive =
        name.as_bytes().get(1) == Some(&b':') && name.as_bytes()[0].is_ascii_alphabetic();
    if name.contains(['/', '\\'])
        || windows_drive
        || !matches!(components.next(), Some(std::path::Component::Normal(_)))
        || components.next().is_some()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "invalid cache name {name:?}: use a single normal path component; \
                 empty names, '.', '..', '/' or '\\' separators, and Windows drive/UNC prefixes are not allowed"
            ),
        ));
    }
    Ok(())
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

/// Resolve the ecp home directory used for `registry.json` and per-branch
/// index dirs. Tries `$ECP_HOME`, then `$HOME/.ecp`; if neither directory
/// can be created and written to (read-only FS, permission denied, CI
/// sandbox), falls back to `<temp_dir>/ecp-fallback/.ecp`.
///
/// Reads and writes within a single CLI invocation use the same resolved
/// path: a project indexed in fallback mode is queryable from the same
/// environment without extra flags.
///
pub fn resolve_home_ecp() -> PathBuf {
    resolve_home_ecp_from_env(std::env::var_os("ECP_HOME"), std::env::var_os("HOME"))
}

/// Same resolution logic as [`resolve_home_ecp`], but with the HOME source
/// supplied by the caller. In-process tests (or any caller wanting to point
/// ecp at a private home without mutating the process-global `HOME` env
/// var) call this with an explicit override. Production code paths read
/// the env var via [`resolve_home_ecp`].
///
/// `#[allow(dead_code)]` because the only intended caller today is the
/// future in-process integration test refactor; ships now so the public
/// API is in place when that work lands without forcing it into the
/// same PR.
#[allow(dead_code)]
pub fn resolve_home_ecp_from<P: AsRef<Path>>(home: P) -> PathBuf {
    let candidate = home.as_ref().join(".ecp");
    if probe_writable(&candidate) {
        return candidate;
    }
    fallback_home()
}

fn resolve_home_ecp_from_env(
    ecp_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> PathBuf {
    let candidates: Vec<PathBuf> = [
        ecp_home.map(PathBuf::from),
        home.map(|h| PathBuf::from(h).join(".ecp")),
    ]
    .into_iter()
    .flatten()
    .collect();

    // A writable root serves reads and writes both, so it wins outright.
    if let Some(writable) = candidates.iter().find(|c| probe_writable(c)) {
        return writable.clone();
    }

    // Nothing is writable, which is what an agent under a read-only sandbox
    // sees — and the root it cannot write is usually the one holding the
    // index it wants to read. Sending it to an empty temp directory threw
    // that index away: with a writable temp dir the query rebuilt the whole
    // graph somewhere else, and with a read-only one it died as `Permission
    // denied (os error 13)` while the answer sat on disk. Prefer a root that
    // already holds a registry, and let a write fail where the data lives.
    // Opened, not stat'd: a `registry.json` this process cannot read is no more
    // use than a missing one, and `read_or_empty` turns it into a hard error
    // rather than an empty registry — so picking that root strands the command
    // where the temp fallback would still have worked.
    if let Some(readable) = candidates
        .iter()
        .find(|c| std::fs::File::open(c.join("registry.json")).is_ok())
    {
        return readable.clone();
    }

    fallback_home()
}

fn fallback_home() -> PathBuf {
    std::env::temp_dir().join("ecp-fallback").join(".ecp")
}

/// The user's home directory, for resolving host config roots like
/// `~/.claude`, `~/.codex`, `~/.config`. The single authority — host
/// modules (`claude.rs`, `codex.rs`, hooks, telemetry) call this instead of
/// reading `HOME` directly, so the platform rule lives in one place.
///
/// Unix reads `$HOME` (unchanged behaviour). Windows reads `$HOME` first so
/// WSL / Git-Bash sessions keep working, then falls back to `$USERPROFILE`
/// (the native home var, e.g. `C:\Users\<name>`). Returns `None` only when
/// no home var is set on the platform.
pub fn home_dir() -> Option<PathBuf> {
    home_dir_from(
        std::env::var_os("HOME"),
        #[cfg(windows)]
        std::env::var_os("USERPROFILE"),
    )
}

/// Resolution logic for [`home_dir`] with env sources injected, so tests
/// exercise the platform rule without mutating process-global env.
#[cfg(unix)]
fn home_dir_from(home: Option<std::ffi::OsString>) -> Option<PathBuf> {
    home.map(PathBuf::from)
}

#[cfg(windows)]
fn home_dir_from(
    home: Option<std::ffi::OsString>,
    userprofile: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    home.or(userprofile).map(PathBuf::from)
}

fn probe_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".ecp-write-probe");
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
        assert!(!tmp.path().join(".ecp-write-probe").exists());
    }

    /// A sandboxed agent gets a read-only filesystem and an index it can
    /// still read. Sending it to an empty temp directory discards that index:
    /// with a writable temp dir the query silently rebuilds the whole graph
    /// elsewhere, and with a read-only one it dies as `Permission denied`
    /// while the answer sits on disk.
    #[cfg(unix)]
    #[test]
    fn an_unwritable_root_holding_a_registry_beats_the_temp_fallback() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cache = home.join(".ecp");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("registry.json"), b"{}").unwrap();

        let mut perms = std::fs::metadata(&cache).unwrap().permissions();
        perms.set_mode(0o500);
        std::fs::set_permissions(&cache, perms).unwrap();

        let resolved = resolve_home_ecp_from_env(None, Some(home.into_os_string()));

        let mut restore = std::fs::metadata(&cache).unwrap().permissions();
        restore.set_mode(0o700);
        std::fs::set_permissions(&cache, restore).unwrap();

        assert_eq!(
            resolved, cache,
            "an unwritable root holding an index must still be read from"
        );
    }

    /// A `registry.json` the process cannot open is no better than a missing
    /// one: `read_or_empty` propagates the permission error instead of
    /// returning an empty registry, so choosing that root fails the command
    /// where the temp fallback would still have answered.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_registry_does_not_win_the_root() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cache = home.join(".ecp");
        std::fs::create_dir_all(&cache).unwrap();
        let registry = cache.join("registry.json");
        std::fs::write(&registry, b"{}").unwrap();

        let mut file_perms = std::fs::metadata(&registry).unwrap().permissions();
        file_perms.set_mode(0o000);
        std::fs::set_permissions(&registry, file_perms).unwrap();
        let mut dir_perms = std::fs::metadata(&cache).unwrap().permissions();
        dir_perms.set_mode(0o500);
        std::fs::set_permissions(&cache, dir_perms).unwrap();

        let resolved = resolve_home_ecp_from_env(None, Some(home.into_os_string()));

        let mut restore_dir = std::fs::metadata(&cache).unwrap().permissions();
        restore_dir.set_mode(0o700);
        std::fs::set_permissions(&cache, restore_dir).unwrap();
        let mut restore_file = std::fs::metadata(&registry).unwrap().permissions();
        restore_file.set_mode(0o600);
        std::fs::set_permissions(&registry, restore_file).unwrap();

        assert_eq!(resolved, fallback_home());
    }

    /// The companion. An unwritable root with nothing in it answers no query,
    /// so the temp fallback is still right — otherwise the rule above would
    /// strand every fresh sandbox on a root it cannot use.
    #[cfg(unix)]
    #[test]
    fn an_unwritable_empty_root_still_falls_back_to_temp() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let cache = home.join(".ecp");
        std::fs::create_dir_all(&cache).unwrap();

        let mut perms = std::fs::metadata(&cache).unwrap().permissions();
        perms.set_mode(0o500);
        std::fs::set_permissions(&cache, perms).unwrap();

        let resolved = resolve_home_ecp_from_env(None, Some(home.into_os_string()));

        let mut restore = std::fs::metadata(&cache).unwrap().permissions();
        restore.set_mode(0o700);
        std::fs::set_permissions(&cache, restore).unwrap();

        assert_eq!(resolved, fallback_home());
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

    /// Single test covers all `resolve_home_ecp` scenarios sequentially —
    /// HOME is process-global and racing with parallel tests would corrupt
    /// other env readers. Since only `resolve_home_ecp` reads HOME in this
    /// crate, serial mutation inside one test is safe.
    #[test]
    fn resolve_home_ecp_covers_env_override_happy_path_and_fallback() {
        let orig_home = std::env::var_os("HOME");
        let orig_ecp_home = std::env::var_os("ECP_HOME");
        std::env::remove_var("ECP_HOME");

        // (1) HOME unset → tmp fallback
        std::env::remove_var("HOME");
        let p = resolve_home_ecp();
        assert!(
            p.starts_with(std::env::temp_dir()),
            "no-HOME should fall back to temp_dir, got {p:?}"
        );
        assert!(p.ends_with(".ecp"), "fallback path tail should end in .ecp");

        // (2) HOME set + writable, no registry.json → probe runs, returns <HOME>/.ecp, no leftover probe
        let writable = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", writable.path());
        let p = resolve_home_ecp();
        assert_eq!(p, writable.path().join(".ecp"));
        assert!(p.exists(), "probe path should be created");
        assert!(
            !p.join(".ecp-write-probe").exists(),
            "probe file should be cleaned up"
        );

        // (3) ECP_HOME set + writable → use it as the exact ecp root
        let override_home = tempfile::tempdir().unwrap();
        let override_ecp = override_home.path().join("custom-ecp");
        std::env::set_var("ECP_HOME", &override_ecp);
        let p = resolve_home_ecp();
        assert_eq!(p, override_ecp);
        assert!(p.exists(), "ECP_HOME path should be created");
        std::env::remove_var("ECP_HOME");

        // (4) HOME points to read-only dir without registry.json → tmp fallback
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let ro = tempfile::tempdir().unwrap();
            let mut perms = std::fs::metadata(ro.path()).unwrap().permissions();
            perms.set_mode(0o500);
            std::fs::set_permissions(ro.path(), perms).unwrap();
            std::env::set_var("HOME", ro.path());
            let p = resolve_home_ecp();
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
        match orig_ecp_home {
            Some(h) => std::env::set_var("ECP_HOME", h),
            None => std::env::remove_var("ECP_HOME"),
        }
    }

    // `home_dir_from` is env-injected and pure — no process-global mutation,
    // so these run in parallel without the serial guard `resolve_home_ecp`
    // needs. The `#[cfg]` arms below mirror the platform compiled in.

    #[cfg(unix)]
    #[test]
    fn home_dir_from_unix_uses_home_and_none_when_unset() {
        use std::ffi::OsString;
        assert_eq!(
            home_dir_from(Some(OsString::from("/home/u"))),
            Some(PathBuf::from("/home/u"))
        );
        assert_eq!(home_dir_from(None), None);
    }

    #[cfg(windows)]
    #[test]
    fn home_dir_from_windows_prefers_home_then_userprofile() {
        use std::ffi::OsString;
        // HOME wins (WSL / Git-Bash compatibility) when both are set.
        assert_eq!(
            home_dir_from(
                Some(OsString::from(r"C:\wsl\home")),
                Some(OsString::from(r"C:\Users\u"))
            ),
            Some(PathBuf::from(r"C:\wsl\home"))
        );
        // No HOME → fall back to USERPROFILE (native Windows shells).
        assert_eq!(
            home_dir_from(None, Some(OsString::from(r"C:\Users\u"))),
            Some(PathBuf::from(r"C:\Users\u"))
        );
        // Neither set → None.
        assert_eq!(home_dir_from(None, None), None);
    }
}
