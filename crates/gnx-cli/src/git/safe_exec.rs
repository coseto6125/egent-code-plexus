//! Hardened git subprocess wrapper. Every git invocation in gnx-cli MUST
//! go through `safe_exec::git()` to ensure hostile repo configs cannot
//! escalate to code execution. See spec §8 H4.

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
