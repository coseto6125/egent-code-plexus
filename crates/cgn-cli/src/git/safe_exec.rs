//! Hardened git subprocess wrapper. Every git invocation in cgn-cli MUST
//! go through `safe_exec::git()` to ensure hostile repo configs cannot
//! escalate to code execution. See spec §8 H4.

use std::path::Path;
use std::process::Command;

/// Build a `Command` rooted at `git` with security-hardening flags
/// pre-attached. Caller appends operational args after.
///
/// Flags blocked:
/// - `protocol.ext.allow=never` — disables `ext::` external commands in URLs
/// - `core.fsmonitor=` — empties any user-defined fsmonitor exec
/// - `core.editor=false` — neutralizes editor invocations
/// - `credential.helper=` — empties helper to avoid running arbitrary bins
pub fn git() -> Command {
    let mut cmd = Command::new("git");
    cmd.args([
        "-c",
        "protocol.ext.allow=never",
        "-c",
        "core.fsmonitor=",
        "-c",
        "core.editor=false",
        "-c",
        "credential.helper=",
    ]);
    cmd
}

/// Short HEAD SHA for `repo_root` via the hardened `git()` wrapper.
/// Returns `None` when git is missing, the directory isn't a checkout, or
/// the command fails — callers degrade to a `null` / `"?"` field rather
/// than failing the whole report.
pub fn head_short(repo_root: &Path) -> Option<String> {
    let out = git()
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(repo_root)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!s.is_empty()).then_some(s)
}
