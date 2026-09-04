//! The one subprocess runner in this crate: stdin closed, output captured,
//! and a wall-clock timeout that kills the child.

use std::path::Path;
use std::process::{Output, Stdio};
use std::time::Duration;
use tokio::process::Command;

/// An `ecp` invocation with a scrubbed environment: only what the CLI
/// needs to find its registry crosses over. `GITHUB_TOKEN` and anything
/// else the host set must not reach a process driven by visitor input.
pub fn ecp_command(bin: &Path) -> Command {
    let mut cmd = Command::new(bin);
    cmd.env_clear();
    for (key, value) in std::env::vars_os() {
        let keep = key
            .to_str()
            .is_some_and(|k| k == "PATH" || k == "HOME" || k == "TMPDIR" || k.starts_with("ECP_"));
        if keep {
            cmd.env(key, value);
        }
    }
    cmd
}

/// `Ok(None)` means the timeout fired. Dropping the timed-out future drops
/// the child, and `kill_on_drop` turns that into SIGKILL; nothing else stops
/// a runaway query or clone.
pub async fn run_with_timeout(
    mut cmd: Command,
    timeout: Duration,
) -> std::io::Result<Option<Output>> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = cmd.spawn()?;
    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(out) => out.map(Some),
        Err(_) => Ok(None),
    }
}
