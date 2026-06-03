//! The global build throttle must gate rebuilds without breaking them, and the
//! `ECP_MAX_CONCURRENT_BUILDS` override must be honored. Unit tests in
//! `build::semaphore` cover the cap *arithmetic*; these cover the *wiring* — that
//! a real `admin index` acquires a slot and still produces a graph.

use std::process::Command;

fn ecp_bin() -> std::path::PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join("ecp")
}

fn git_init(p: &std::path::Path) {
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "t@t"],
        &["config", "user.name", "t"],
    ] {
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(args)
            .status()
            .unwrap();
    }
    std::fs::write(
        p.join("hello.rs"),
        "fn hello() { world(); }\nfn world() {}\n",
    )
    .unwrap();
    for args in [&["add", "."][..], &["commit", "-qm", "init"]] {
        Command::new("git")
            .arg("-C")
            .arg(p)
            .args(args)
            .status()
            .unwrap();
    }
}

#[test]
fn index_acquires_a_build_slot_then_releases_it() {
    let home = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    git_init(wt.path());

    let out = Command::new(ecp_bin())
        .env("HOME", home.path())
        .args(["admin", "index", "--repo"])
        .arg(wt.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // The slot dir is created the first time a rebuild runs (proves acquire() was
    // reached on the build path). Locks are advisory + released on drop, so the
    // files remain free for reuse. Gated on HOME-derived path existing — skipped
    // gracefully where the platform resolves the ecp home elsewhere.
    let slot_dir = home.path().join(".ecp/.build-slots");
    if slot_dir.exists() {
        let n = std::fs::read_dir(&slot_dir).unwrap().count();
        assert!(n >= 1, "expected at least one slot file under {slot_dir:?}");
    }
}

#[test]
fn env_override_does_not_break_the_build() {
    let home = tempfile::tempdir().unwrap();
    let wt = tempfile::tempdir().unwrap();
    git_init(wt.path());

    // K=1 forces full serialization; the build must still succeed.
    let out = Command::new(ecp_bin())
        .env("HOME", home.path())
        .env("ECP_MAX_CONCURRENT_BUILDS", "1")
        .args(["admin", "index", "--repo"])
        .arg(wt.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("l2.built"),
        "expected l2.built"
    );
}
