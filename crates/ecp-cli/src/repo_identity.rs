use crate::git_cache;
use ecp_core::registry::sanitize_segment;
use std::io;
use std::path::Path;
use xxhash_rust::xxh3::xxh3_64;

/// Compute the stable per-repo directory name under `~/.ecp/`.
///
/// Identity rule (git): `<sanitize(basename(common_dir))>__<xxh3_64(canonical_common_dir)[:8]>`.
/// All worktrees of the same git repo share the same `--git-common-dir`,
/// so this naturally collapses `git worktree add` siblings onto a single
/// `<repo>/` namespace — solving v1's per-worktree duplication.
///
/// Identity rule (non-git): `<sanitize(basename(canonical_cwd))>__<xxh3_64(canonical_cwd)[:8]>`.
/// Identity is path-bound — moving the directory invalidates the cache, which
/// is acceptable for ad-hoc indexing of plain source trees / vendored sources.
pub fn repo_dir_name_for_cwd(cwd: &Path) -> io::Result<String> {
    let (identity_path, is_git) = match git_cache::common_dir(cwd) {
        Ok(p) => (std::fs::canonicalize(&p)?, true),
        Err(_) => (std::fs::canonicalize(cwd)?, false),
    };

    let basename = if is_git {
        // Git: parent of `.git` is the repo root; if common_dir is bare
        // (e.g. ends with `.git` and has no enclosing dir), fall back to
        // common_dir's own basename.
        identity_path
            .parent()
            .and_then(|p| p.file_name())
            .or_else(|| identity_path.file_name())
    } else {
        // Non-git: identity_path IS the worktree root.
        identity_path.file_name()
    }
    .and_then(|s| s.to_str())
    .unwrap_or("unknown");

    let cleaned = basename.trim_start_matches(['.', '-']);
    let safe = sanitize_segment(if cleaned.is_empty() { "repo" } else { cleaned })
        .unwrap_or_else(|_| "repo".to_string());
    let h = short_hash_hex8(identity_path.to_string_lossy().as_bytes());
    Ok(format!("{safe}__{h}"))
}

/// First 8 hex chars of `xxh3_64(bytes)` — short, filesystem-safe digest.
/// Used by `repo_dir_name_for_cwd` to disambiguate repos by canonical
/// git common-dir, and by `parse_cache` to scope cache entries by
/// `BUILDER_FINGERPRINT`. Shared so a future hash-collision tweak lands
/// in one place.
pub(crate) fn short_hash_hex8(bytes: &[u8]) -> String {
    let h = xxh3_64(bytes) as u32;
    hex::encode(h.to_le_bytes())
}

/// 16-char big-endian hex of `xxh3_64(bytes)`. Used for content / blob
/// fingerprints whose hex is logged and compared across sessions — keep
/// all 16-hex sites going through here so `grep <hash>` finds every site.
#[inline]
pub(crate) fn xxh3_hex16(bytes: &[u8]) -> String {
    format!("{:016x}", xxh3_64(bytes))
}
