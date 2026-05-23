//! Shared invocation of the running `ecp` binary as a subprocess.
//!
//! Several CLI flows compose features by re-shelling into the same `ecp`
//! binary (e.g. `pr-analyze` → `ecp impact`, `diff bindings` → `ecp admin
//! index --dump-resolver`). Each call site previously open-coded the same
//! `current_exe + Command::output + exit-check + stderr-bubble` boilerplate
//! with subtly different error mapping. Centralising it here makes new
//! sub-feature wiring two lines instead of fifteen.

use ecp_core::EcpError;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Resolve the currently-running `ecp` binary's filesystem path.
///
/// Used by callers that need to embed the path in a config file (agent
/// host MCP / hook JSON, systemd unit) where the path itself is the
/// payload. For subprocess invocation, use [`run_self`] — it folds the
/// path lookup, spawn, and exit-check into one call.
pub fn self_exe() -> Result<PathBuf, EcpError> {
    std::env::current_exe().map_err(|e| EcpError::Output(format!("current_exe: {e}")))
}

/// Spawn the running `ecp` binary with `args`, return captured stdout
/// bytes. Non-zero exit and spawn failure are both surfaced as
/// `EcpError::Output` with the offending subcommand name and stderr
/// excerpt embedded — callers don't need to re-format error context.
///
/// Caller wraps the bytes into whatever shape they need (JSON parse,
/// file write, JSONL iterate). The helper deliberately stops at "bytes"
/// because each consumer's parse-error mapping is its own concern.
pub fn run_self(args: &[&str]) -> Result<Vec<u8>, EcpError> {
    run_at(&self_exe()?, args)
}

/// Run an explicit binary path with `args`. Internal seam used by
/// [`run_self`] and by unit tests that need a known-good / known-bad
/// executable instead of `current_exe()` (which under `cargo test` is
/// the test runner, not `ecp`).
pub(crate) fn run_at(exe: &Path, args: &[&str]) -> Result<Vec<u8>, EcpError> {
    let subcmd = args.first().copied().unwrap_or("?");
    let out = Command::new(exe)
        .args(args)
        .output()
        .map_err(|e| EcpError::Output(format!("ecp {subcmd} spawn: {e}")))?;
    if !out.status.success() {
        return Err(EcpError::Output(format!(
            "ecp {subcmd} failed (exit {}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_exe_returns_existing_path() {
        let p = self_exe().expect("current_exe must succeed inside cargo-spawned test");
        assert!(
            p.exists(),
            "self_exe must return a path that exists on disk: {}",
            p.display()
        );
    }

    // `/bin/true` and `/bin/false` are Linux-runner fixtures. Windows
    // doesn't have them at all; macOS-14 (Apple Silicon) GitHub runners
    // have them but the spawn returns ENOENT (likely SIP / sandboxing of
    // /bin/). Both surface as `No such file or directory (os error 2)` →
    // narrow the gate to Linux. The behaviour these tests pin (success-
    // path empty stdout, non-zero exit error message shape) is platform-
    // independent inside the helper; Linux-only coverage is sufficient
    // because the spawn-failure test below covers the cross-platform
    // NotFound branch.

    #[cfg(target_os = "linux")]
    #[test]
    fn run_at_propagates_nonzero_exit_with_subcommand_in_message() {
        // `/bin/false` exits 1 unconditionally, simulating an ecp subcommand
        // that ran but returned an error. We pass `"impact"` as the (unused)
        // first arg so the error message picks it up — that's what every
        // real caller does.
        let err =
            run_at(Path::new("/bin/false"), &["impact"]).expect_err("/bin/false exits 1 → Err");
        let msg = format!("{err}");
        assert!(
            msg.contains("ecp impact failed"),
            "error message must embed the subcommand name: {msg}"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn run_at_returns_stdout_on_success() {
        // `/bin/true` exits 0 with empty stdout. Confirms the success path
        // returns Ok(empty Vec) rather than ever bubbling /bin/true's exit.
        let out = run_at(Path::new("/bin/true"), &["any-arg"]).expect("/bin/true exits 0 → Ok");
        assert!(out.is_empty(), "stdout from /bin/true must be empty");
    }

    #[test]
    fn run_at_surfaces_spawn_failure() {
        // Non-existent binary path → spawn() fails with NotFound. The
        // helper must wrap it as EcpError::Output("ecp <subcmd> spawn: …").
        let err = run_at(Path::new("/no/such/binary/exists/here/ever"), &["any"])
            .expect_err("non-existent exe must fail to spawn");
        let msg = format!("{err}");
        assert!(msg.contains("spawn:"), "spawn-failure message: {msg}");
    }
}
