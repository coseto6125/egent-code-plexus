//! Hardened git subprocess wrapper. Every git invocation in ecp-cli MUST
//! go through `safe_exec::git()` to ensure hostile repo configs cannot
//! escalate to code execution. See spec §8 H4.

use ecp_core::EcpError;
use std::ffi::OsString;
use std::io;
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
///   `ecp diff --baseline` adds a worktree and checks a commit out into it.
///
/// Three execution vectors are not covered by these flags, because the
/// program's name comes from `.gitattributes` rather than from config: an
/// external diff driver, a textconv driver, and a filter driver. Diff-family
/// callers pass [`DIFF_HARDENING`] for the first two. Commands that convert
/// content — `checkout`, `reset --hard`, `stash`, `archive`, and `status` and
/// a `diff` against the worktree, which run the clean filter over every racily
/// clean or stat-dirty file — pass the overrides from [`filter_overrides`] for
/// the third.
///
/// `-c diff.external=` is deliberately NOT set here — git treats the empty
/// value as a command to run and dies with `cannot run : No such file or
/// directory` on every ordinary diff, so it breaks benign repositories while
/// adding nothing `--no-ext-diff` does not already do.
pub fn git() -> Command {
    git_at(Path::new("git"))
}

/// [`git`] for a binary the caller already resolved.
pub fn git_at(program: &Path) -> Command {
    let mut cmd = Command::new(program);
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

/// Overrides that neutralise every filter driver visible to git in
/// `repo_dir`. Pair with [`git_with_overrides`].
///
/// `checkout`, `reset --hard`, `stash`, `archive` and `status` all convert
/// content, so a `.gitattributes` line naming `filter=<driver>` runs that
/// driver's program. `core.hooksPath` does not touch it, none of these
/// commands has a `--no-filters`, and the driver name lives in the
/// repository's own attributes file — so the names have to be read out of the
/// config before they can be overridden.
///
/// Call it with the directory the converting command runs in. The config is
/// read as git sees it there, and a linked worktree can see drivers its main
/// worktree cannot: an `includeIf "gitdir:**/worktrees/**"` section applies
/// only inside the linked one.
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
/// LFS-tracked file is read as its pointer stub, and `status` lists a racily
/// clean LFS-tracked file as modified. ecp parses source and LFS
/// holds binary assets, so this buys a complete boundary for a case that
/// barely arises.
///
/// `process` takes precedence over `smudge` and `clean`, so a repository that
/// sets it keeps executing when only those two are covered. An empty value
/// starts no program: git 2.43 `apply_filter` (`convert.c`) runs a command
/// only when it is non-empty, and a set `process` also stops the other two
/// from being consulted. Nothing runs, so the content passes through as an
/// absent filter would. `required=false` keeps a driver the repository marks
/// required from turning that into a hard error.
///
/// A failure to read the config is an error, never "no drivers": the command
/// the overrides guard would otherwise run unguarded. The read runs with
/// ecp's own environment; a caller whose command runs under another builds
/// the read itself with [`FILTER_KEYS_ARGS`].
pub fn filter_overrides(repo_dir: &Path) -> io::Result<FilterOverrides> {
    let out = git()
        // `GIT_CONFIG` redirects `git config` alone (git-config(1)
        // ENVIRONMENT). The guarded command never reads that file, so a read
        // through it lists no driver while the repository's drivers stay live.
        .env_remove("GIT_CONFIG")
        .args(FILTER_KEYS_ARGS)
        .current_dir(repo_dir)
        .output()?;
    FilterOverrides::from_config_output(&out)
}

/// `git config` arguments that list the key of every filter setting git
/// resolves in its directory, NUL-terminated. A subsection name may hold any
/// byte but newline and NUL.
pub const FILTER_KEYS_ARGS: [&str; 5] =
    ["config", "-z", "--name-only", "--get-regexp", "^filter\\."];

/// The variables that hold the empty value and `false` for the
/// `--config-env` form of an override.
const FILTER_OFF_ENV: &str = "ECP_GIT_FILTER_OFF";
const FILTER_OPTIONAL_ENV: &str = "ECP_GIT_FILTER_OPTIONAL";

/// The filter driver names git can resolve in one or more directories, each
/// rendered as overrides that neutralise it. Built by [`filter_overrides`] or
/// [`FilterOverrides::from_config_output`]; applied by [`git_with_overrides`]
/// or [`FilterOverrides::apply`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FilterOverrides {
    names: Vec<OsString>,
}

impl FilterOverrides {
    /// Parses the output of `git config` run with [`FILTER_KEYS_ARGS`].
    pub fn from_config_output(out: &Output) -> io::Result<Self> {
        match out.status.code() {
            Some(0) => {}
            // `--get-regexp` exits 1, silently, when no key matches.
            Some(1) if out.stdout.is_empty() => return Ok(Self::default()),
            _ => {
                return Err(io::Error::other(format!(
                    "git config could not list the filter drivers ({}): {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).trim()
                )))
            }
        }
        let mut names = out
            .stdout
            .split(|&b| b == 0)
            .filter_map(|key| key.strip_prefix(b"filter."))
            // Driver names may themselves contain dots, so split off the leaf.
            .filter_map(|rest| {
                rest.iter()
                    .rposition(|&b| b == b'.')
                    .map(|dot| &rest[..dot])
            })
            .map(|name| os_string_from_git(name.to_vec()))
            .collect::<io::Result<Vec<_>>>()?;
        names.sort_unstable();
        names.dedup();
        Ok(Self { names })
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }

    /// Adds `other`'s drivers, so one set guards commands that read the config
    /// of several directories.
    pub fn merge(&mut self, other: Self) {
        self.names.extend(other.names);
        self.names.sort_unstable();
        self.names.dedup();
    }

    /// Adds the overrides to `cmd`, ahead of the subcommand.
    ///
    /// `-c` splits its argument at the first `=`, so for a driver named `x=y`
    /// it sets `filter.x` and leaves the driver live — measured, and it ran.
    /// `--config-env` splits at the last `=` instead, but needs git 2.31, so it
    /// is kept to the names that need it and older gits still work everywhere
    /// else.
    pub fn apply<'a>(&self, cmd: &'a mut Command) -> &'a mut Command {
        let mut via_env = false;
        for name in &self.names {
            let needs_env = name.as_encoded_bytes().contains(&b'=');
            via_env |= needs_env;
            for (leaf, value, var) in [
                ("clean", "", FILTER_OFF_ENV),
                ("smudge", "", FILTER_OFF_ENV),
                ("process", "", FILTER_OFF_ENV),
                ("required", "false", FILTER_OPTIONAL_ENV),
            ] {
                let mut key = OsString::from("filter.");
                key.push(name);
                key.push(".");
                key.push(leaf);
                key.push("=");
                if needs_env {
                    let mut arg = OsString::from("--config-env=");
                    arg.push(&key);
                    arg.push(var);
                    cmd.arg(arg);
                } else {
                    key.push(value);
                    cmd.arg("-c").arg(key);
                }
            }
        }
        if via_env {
            cmd.env(FILTER_OFF_ENV, "")
                .env(FILTER_OPTIONAL_ENV, "false");
        }
        cmd
    }
}

/// Bytes git printed for a name or path. git on Windows speaks UTF-8, so
/// anything else there cannot be passed back to it and is an error.
pub fn os_string_from_git(bytes: Vec<u8>) -> io::Result<OsString> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(OsString::from_vec(bytes))
    }
    #[cfg(not(unix))]
    {
        String::from_utf8(bytes)
            .map(OsString::from)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }
}

/// [`git`] plus the filter overrides, rooted at `repo_dir`. Kept separate from
/// [`filter_overrides`] so a caller that runs several commands over
/// one repository enumerates the drivers once.
pub fn git_with_overrides(repo_dir: &Path, overrides: &FilterOverrides) -> Command {
    let mut cmd = git();
    overrides.apply(&mut cmd);
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
