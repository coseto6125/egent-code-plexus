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
/// Three execution vectors are not covered by these flags, because the
/// program's name comes from `.gitattributes` rather than from config: an
/// external diff driver, a textconv driver, and a filter driver. Diff-family
/// callers pass [`DIFF_HARDENING`] for the first two. Commands that convert
/// worktree content — `checkout`, `stash`, `archive` — pass the overrides from
/// [`filter_overrides`] for the third.
///
/// `-c diff.external=` is deliberately NOT set here — git treats the empty
/// value as a command to run and dies with `cannot run : No such file or
/// directory` on every ordinary diff, so it breaks benign repositories while
/// adding nothing `--no-ext-diff` does not already do.
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

/// `-c` settings that neutralise every filter driver visible to git in
/// `repo_dir`. Pair with [`git_with_overrides`].
///
/// `checkout`, `stash` and `archive` all convert content, so a `.gitattributes`
/// line naming `filter=<driver>` runs that driver's program. `core.hooksPath`
/// does not touch it, none of the three commands has a `--no-filters`, and the
/// driver name lives in the repository's own attributes file — so the names
/// have to be read out of the config before they can be overridden.
///
/// The enumeration deliberately asks for the *effective* config rather than a
/// scope. Scoping it to `--local` looked tighter and was leaky: a driver in
/// `.git/config.worktree` (`extensions.worktreeConfig`) and a driver reached
/// through `include.path` from `.git/config` are both absent from
/// `--local --get-regexp` and both executed on checkout when measured. A driver
/// git can apply is a driver git can resolve, so the effective list is the one
/// list that cannot hide one.
///
/// This overrides the user's own drivers too, and that is the point rather than
/// a side effect. Keeping a driver live because its name appears in the global
/// config is not a boundary: a repository that sets `filter.lfs.smudge` in its
/// own config shadows the global value for that same name, so the allowlist
/// would wave through exactly the program it exists to stop — measured, and it
/// ran. The cost is that git-lfs does not smudge inside ecp's window, so an
/// LFS-tracked file is read as its pointer stub. ecp parses source and LFS
/// holds binary assets, so this buys a complete boundary for a case that
/// barely arises.
///
/// All three keys need overriding. `process` takes precedence over `smudge` and
/// `clean`, so a repository that sets it keeps executing when only those two
/// are covered. `cat` is the passthrough: it returns the content unchanged,
/// which is what an absent filter does. It is not a valid long-running filter,
/// so the `process` override makes git log an initialisation failure and fall
/// back to the content as stored, and `required=false` is what keeps that a
/// fallback rather than a hard error. Those lines land in captured stderr,
/// which the success path discards.
pub fn filter_overrides(repo_dir: &Path) -> Vec<String> {
    let Ok(out) = git()
        .args(["config", "--name-only", "--get-regexp", "^filter\\."])
        .current_dir(repo_dir)
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut names: Vec<&str> = text
        .lines()
        .filter_map(|key| key.strip_prefix("filter."))
        // Driver names may themselves contain dots, so split off the leaf.
        .filter_map(|rest| rest.rsplit_once('.'))
        .filter(|(_, leaf)| matches!(*leaf, "smudge" | "clean" | "process"))
        .map(|(name, _)| name)
        .collect();
    names.sort_unstable();
    names.dedup();
    names
        .into_iter()
        .flat_map(|n| {
            [
                format!("filter.{n}.smudge=cat"),
                format!("filter.{n}.clean=cat"),
                format!("filter.{n}.process=cat"),
                format!("filter.{n}.required=false"),
            ]
        })
        .collect()
}

/// [`git`] plus `-c` overrides, rooted at `repo_dir`. Kept separate from
/// [`filter_overrides`] so a caller that runs several commands over
/// one repository enumerates the drivers once.
pub fn git_with_overrides(repo_dir: &Path, overrides: &[String]) -> Command {
    let mut cmd = git();
    for kv in overrides {
        cmd.arg("-c").arg(kv);
    }
    cmd.current_dir(repo_dir);
    cmd
}

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
