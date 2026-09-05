//! Concurrency invariant 4.5 — hook spawn flock serialises.
//!
//! Two concurrent `ecp` hook invocations must converge to exactly ONE
//! reindex side-effect (the second flock acquirer no-ops cleanly).
//! Mirrors the production shell template at
//! `crates/ecp-cli/src/background.rs:73-91` (markerless branch).

use ecp_cli::flock_preamble;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn slow_noop_path() -> PathBuf {
    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("target")
        });
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let path = target_dir.join(profile).join("examples").join("slow_noop");
    if !path.exists() {
        // `cargo test` doesn't auto-build examples — invoke cargo directly so
        // a clean checkout works without manual `cargo build --example` setup.
        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", "egent-code-plexus", "--example", "slow_noop"])
            .status()
            .expect("spawn cargo build --example slow_noop");
        assert!(status.success(), "cargo build --example slow_noop failed");
    }
    path
}

/// Wraps `inner` with the production flock preamble so the test pins to
/// the same quoting + redirect behaviour as `spawn_bg` (not a hand-rolled copy).
fn flock_shell(lock: &Path, inner: &str) -> String {
    format!("{}{inner}\n", flock_preamble(lock))
}

#[test]
fn hook_concurrent_spawn_flock_serializes() {
    let bin = slow_noop_path();

    let tmp = tempfile::TempDir::new().unwrap();
    let lock = tmp.path().join("reindex.lock");
    let marker = tmp.path().join("marker.txt");
    let inner = format!("'{}' '{}'", bin.display(), marker.display());
    let shell = flock_shell(&lock, &inner);

    let mut handles = Vec::new();
    for _ in 0..2 {
        let shell = shell.clone();
        handles.push(std::thread::spawn(move || {
            let mut child = Command::new("sh")
                .arg("-c")
                .arg(&shell)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn shell");
            child.wait().expect("wait shell")
        }));
    }

    let statuses: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    for (i, s) in statuses.iter().enumerate() {
        assert!(s.success(), "shell wrapper #{i} exited non-zero: {s:?}");
    }

    let content = std::fs::read_to_string(&marker).unwrap_or_default();
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "expected exactly 1 reindex side-effect, got {}: {:?}",
        lines.len(),
        lines,
    );

    assert!(lock.exists(), "lock file not created");
}

#[test]
fn hook_serial_spawn_runs_each_time() {
    let bin = slow_noop_path();

    let tmp = tempfile::TempDir::new().unwrap();
    let lock = tmp.path().join("reindex.lock");
    let marker = tmp.path().join("marker.txt");
    let inner = format!("'{}' '{}'", bin.display(), marker.display());
    let shell = flock_shell(&lock, &inner);

    for _ in 0..2 {
        let status = Command::new("sh")
            .arg("-c")
            .arg(&shell)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("status");
        assert!(status.success());
    }

    let content = std::fs::read_to_string(&marker).unwrap_or_default();
    let lines: Vec<&str> = content.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        lines.len(),
        2,
        "serial calls should each run; got {lines:?}"
    );
}

/// The no-flock branch used to run whatever a repository path spelled.
///
/// `trap "rmdir '<path>'" EXIT INT TERM` reads as quoted and is not: the body's
/// own double quotes are the outer pair, so the single quotes inside are
/// ordinary characters and the shell expands `$(...)` in the path as it parses
/// the trap. `shell_quote` was doing its job; the double quotes around its
/// output undid it.
///
/// This drives the real preamble with a PATH that has no `flock`, which is the
/// only way to reach that branch on a machine that has one.
#[cfg(unix)]
#[test]
fn no_flock_fallback_does_not_execute_a_path_that_spells_a_command() {
    let tmp = tempfile::TempDir::new().unwrap();
    let marker = tmp.path().join("substituted");

    // A PATH holding what the fallback needs and nothing more — `flock` absent
    // is the point, so linking the tools individually beats trimming a copy of
    // the system PATH.
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    for tool in ["mkdir", "rmdir", "touch", "sh"] {
        let src = ["/bin", "/usr/bin"]
            .iter()
            .map(|d| Path::new(d).join(tool))
            .find(|p| p.exists())
            .unwrap_or_else(|| panic!("{tool} must exist for this test"));
        std::os::unix::fs::symlink(&src, bin.join(tool)).unwrap();
    }
    assert!(
        !bin.join("flock").exists(),
        "the fallback branch is only reached when flock is missing"
    );

    // The lock lives in a directory whose name spells a command. Nothing here
    // is exotic: `$(...)` is a legal directory name on every Unix filesystem.
    let hostile = tmp.path().join(format!("$(touch {})", marker.display()));
    std::fs::create_dir_all(&hostile).unwrap();
    let lock = hostile.join("reindex.lock");

    // The inner command leaves its own mark. Without it the test also passes
    // when the preamble exits early — `mkdir ... || exit 0` returns 0 and
    // installs no trap — and an absent substitution marker would then prove
    // nothing about the branch this test exists to cover.
    let reached = tmp.path().join("reached");
    let inner = format!("touch '{}'", reached.display());

    let out = Command::new("sh")
        .arg("-c")
        .arg(flock_shell(&lock, &inner))
        .env("PATH", &bin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run the preamble");

    assert!(
        out.status.success(),
        "the fallback preamble exited non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        reached.exists(),
        "the preamble never reached its inner command, so nothing here \
         exercises the trap; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !marker.exists(),
        "the shell ran the command spelled in the lock path; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
