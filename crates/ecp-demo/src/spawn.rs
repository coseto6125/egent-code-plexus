//! The one subprocess runner in this crate: stdin closed, output captured,
//! and a wall-clock timeout that kills the child.

use std::process::{Output, Stdio};
use std::time::Duration;
use tokio::process::Command;

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
