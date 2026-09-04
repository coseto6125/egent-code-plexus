//! Hardened git subprocess wrapper. Every git invocation in ecp-cli MUST
//! go through `safe_exec::git()` to ensure hostile repo configs cannot
//! escalate to code execution. See spec §8 H4.

use ecp_core::EcpError;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Build a `Command` rooted at `git` with security-hardening flags
/// pre-attached. Caller appends operational args after.
///
/// Flags blocked:
/// - `protocol.ext.allow=never` — disables `ext::` external commands in URLs
/// - `core.fsmonitor=` — empties any user-defined fsmonitor exec
/// - `core.editor=false` — neutralizes editor invocations
/// - `credential.helper=` — empties helper to avoid running arbitrary bins
/// - `core.hooksPath=/dev/null` — points the hook directory at nothing, so a
///   scanned repository's `.git/hooks/post-checkout` (and the rest) cannot run
///   during ecp's own bookkeeping. This one needs no config at all from the
///   attacker: an executable dropped in `.git/hooks/` is enough, and
///   `ecp diff --baseline` checks out and stashes on the user's behalf.
///
/// Two execution vectors have no `-c` that turns them off, because the
/// program's name comes from `.gitattributes` rather than from config: an
/// external diff driver and a textconv driver. Diff-family callers pass
/// [`DIFF_HARDENING`] for those. `-c diff.external=` is deliberately NOT set
/// here — git treats the empty value as a command to run and dies with
/// `cannot run : No such file or directory` on every ordinary diff, so it
/// breaks benign repositories while adding nothing `--no-ext-diff` does not
/// already do.
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
        "-c",
        "core.hooksPath=/dev/null",
    ]);
    cmd
}

/// Per-invocation flags for every diff-family command (`diff`, `log -p`,
/// `show <commit>`, `blame`, `grep`). They close the two ways a scanned
/// repository turns rendering a diff into running its own code:
/// - `--no-ext-diff` — ignores `diff.external`, which the repo's own
///   `.git/config` can point at any executable.
/// - `--no-textconv` — ignores per-attribute textconv drivers, which no `-c`
///   can pre-empt because the driver name comes from `.gitattributes`.
pub const DIFF_HARDENING: [&str; 2] = ["--no-ext-diff", "--no-textconv"];

/// Reject a user-supplied revision that git would read as an option.
///
/// Revisions reach git as bare argv elements, so a value like
/// `--output=/etc/passwd` is an *option*, not a revision: `git diff` then
/// writes its patch over that path and reports no changes, because stdout
/// came back empty. A git refname can never begin with `-`
/// (`git check-ref-format` forbids it), so refusing the whole class costs no
/// legitimate input.
///
/// `--` cannot substitute for this check: before a revision it tells git the
/// remaining arguments are paths, which silently turns the revision into a
/// pathspec and changes what the diff means.
pub fn reject_option_like_rev(rev: &str) -> Result<(), EcpError> {
    if rev.starts_with('-') {
        return Err(EcpError::InvalidArgument(format!(
            "revision `{rev}` starts with `-`, which git reads as an option, not a revision"
        )));
    }
    Ok(())
}

/// True when running inside an agent sandbox that restricts (or fully blocks)
/// outbound network — where a network git op would block on connect instead of
/// failing fast. Lets callers skip the op and report "offline" immediately
/// rather than waiting out a timeout.
///
/// - `CODEX_SANDBOX_NETWORK_DISABLED` — set by Codex when network is disabled.
/// - `GEMINI_SANDBOX` — set by Gemini CLI to the sandbox backend (docker /
///   podman / sandbox-exec / true) when sandboxing is on; treated as restricted
///   since its default profile blocks egress.
pub fn sandbox_network_restricted() -> bool {
    std::env::var_os("CODEX_SANDBOX_NETWORK_DISABLED").is_some()
        || std::env::var("GEMINI_SANDBOX")
            .map(|v| !v.is_empty() && v != "0" && v != "false")
            .unwrap_or(false)
}

/// Run `cmd` to completion, killing it and returning `None` if it outlives
/// `timeout`. For network git ops (`ls-remote`) where a sandboxed/restricted
/// network leaves the child blocked in `poll()` indefinitely — a plain
/// `.output()` would hang the caller forever. Polls `try_wait` on a short tick
/// rather than pulling in a wait-with-timeout dependency.
pub fn output_with_timeout(mut cmd: Command, timeout: Duration) -> Option<Output> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A command that sleeps ~30s, portable across the test runners. `sleep`/
    /// `printf` are absent on Windows (shell built-ins, not executables), so the
    /// timeout tests must dispatch on the OS.
    fn long_running_cmd() -> Command {
        if cfg!(windows) {
            let mut cmd = Command::new("cmd");
            cmd.args(["/C", "ping -n 31 127.0.0.1 >NUL"]);
            cmd
        } else {
            let mut cmd = Command::new("sleep");
            cmd.arg("30");
            cmd
        }
    }

    /// Echo `hello` to stdout, portable across runners. Windows `cmd /C echo`
    /// appends a trailing CRLF, so callers compare against the trimmed output.
    fn echo_hello_cmd() -> Command {
        if cfg!(windows) {
            let mut cmd = Command::new("cmd");
            cmd.args(["/C", "echo hello"]);
            cmd
        } else {
            let mut cmd = Command::new("printf");
            cmd.arg("hello");
            cmd
        }
    }

    #[test]
    fn output_with_timeout_kills_a_hanging_child() {
        // The child far outlives the 200ms bound — must be killed and return None.
        let start = Instant::now();
        let result = output_with_timeout(long_running_cmd(), Duration::from_millis(200));
        assert!(result.is_none(), "expected None for a timed-out child");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "should return shortly after the deadline, not wait out the child"
        );
    }

    #[test]
    fn output_with_timeout_returns_fast_command_output() {
        let out = output_with_timeout(echo_hello_cmd(), Duration::from_secs(5))
            .expect("echo should finish");
        assert!(out.status.success());
        let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
        assert_eq!(stdout.trim(), "hello");
    }

    #[test]
    fn sandbox_detection_keys_off_codex_and_gemini_vars() {
        // Saved/restored to avoid cross-test env bleed.
        let codex = std::env::var_os("CODEX_SANDBOX_NETWORK_DISABLED");
        let gemini = std::env::var_os("GEMINI_SANDBOX");
        std::env::remove_var("CODEX_SANDBOX_NETWORK_DISABLED");
        std::env::remove_var("GEMINI_SANDBOX");
        assert!(!sandbox_network_restricted());

        std::env::set_var("GEMINI_SANDBOX", "docker");
        assert!(sandbox_network_restricted());
        std::env::set_var("GEMINI_SANDBOX", "false");
        assert!(!sandbox_network_restricted());

        std::env::remove_var("GEMINI_SANDBOX");
        std::env::set_var("CODEX_SANDBOX_NETWORK_DISABLED", "1");
        assert!(sandbox_network_restricted());

        std::env::remove_var("CODEX_SANDBOX_NETWORK_DISABLED");
        if let Some(v) = codex {
            std::env::set_var("CODEX_SANDBOX_NETWORK_DISABLED", v);
        }
        if let Some(v) = gemini {
            std::env::set_var("GEMINI_SANDBOX", v);
        }
    }
}
