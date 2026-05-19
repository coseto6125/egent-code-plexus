//! Pick the most-specific dir name for a SHA in the form
//! `<source_type>_<source_id>__<sha>` — branch > tag > pr > commit fallback.

use crate::git::safe_exec;
use cgn_core::registry::sanitize_segment;
use std::io;
use std::path::Path;

pub fn pick_dirname(worktree: &Path, sha_hex: &str) -> io::Result<String> {
    let refs = list_refs_pointing_at(worktree, sha_hex)?;

    if let Some(b) = refs.iter().find_map(|r| r.strip_prefix("refs/heads/")) {
        return Ok(format!("branch_{}__{sha_hex}", sanitize_for_dir(b)));
    }
    if let Some(t) = refs.iter().find_map(|r| r.strip_prefix("refs/tags/")) {
        return Ok(format!("tag_{}__{sha_hex}", sanitize_for_dir(t)));
    }
    for r in &refs {
        if let Some(rest) = r
            .strip_prefix("refs/pull/")
            .or_else(|| r.strip_prefix("refs/merge-requests/"))
        {
            if let Some(n) = rest.split('/').next() {
                return Ok(format!("pr_{}__{sha_hex}", sanitize_for_dir(n)));
            }
        }
    }
    Ok(format!("commit__{sha_hex}"))
}

fn list_refs_pointing_at(worktree: &Path, sha_hex: &str) -> io::Result<Vec<String>> {
    let out = safe_exec::git()
        .args([
            "for-each-ref",
            "--points-at",
            sha_hex,
            "--format=%(refname)",
        ])
        .current_dir(worktree)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("git for-each-ref failed"));
    }
    let s = std::str::from_utf8(&out.stdout).map_err(io::Error::other)?;
    Ok(s.lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// `/` → `-`, other non-fs-safe → `_`, then run through `sanitize_segment`
/// to enforce length cap + leading-char rules. Returns `"x"` on total failure
/// (e.g. empty input after strip) so the dirname always parses round-trip.
fn sanitize_for_dir(s: &str) -> String {
    let replaced: String = s
        .chars()
        .map(|c| match c {
            '/' => '-',
            c if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') => c,
            _ => '_',
        })
        .collect();
    let cleaned = replaced.trim_start_matches(['.', '-']);
    let candidate = if cleaned.is_empty() { "x" } else { cleaned };
    sanitize_segment(candidate).unwrap_or_else(|_| "x".to_string())
}
