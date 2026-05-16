//! Integration test for the auto_ensure Stale → write_dirty_fragment path.
//!
//! Exercises the real end-to-end flow:
//!   build L2 index → modify a source file (without committing) →
//!   run a query → assert L1 fragments + dirty_files.json + session_meta
//!   materialise under <home>/.gnx/<repo>__<hash>/sessions/<sid>/.

use std::ffi::OsStr;
use std::process::Command;
use walkdir::WalkDir;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn run(cmd: &mut Command, label: &str) -> std::process::Output {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("{label} spawn failed: {e}"));
    if !out.status.success() {
        panic!(
            "{label} failed:\n  stderr={}\n  stdout={}",
            String::from_utf8_lossy(&out.stderr),
            String::from_utf8_lossy(&out.stdout),
        );
    }
    out
}

#[test]
fn stale_path_emits_l1_fragments_per_dirty_file() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();

    // ── 1. Init a real git repo with one committed source file ────────────
    run(
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["init", "-q", "-b", "main"]),
        "git init",
    );
    run(
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["config", "user.email", "t@t"]),
        "git config email",
    );
    run(
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["config", "user.name", "t"]),
        "git config name",
    );
    std::fs::write(repo.join("main.rs"), "fn original() {}\n").unwrap();
    run(
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["add", "."]),
        "git add",
    );
    run(
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["commit", "-qm", "init"]),
        "git commit",
    );

    // ── 2. Build L2 index ─────────────────────────────────────────────────
    run(
        Command::new(gnx_bin())
            .args(["admin", "index", "--repo", repo.to_str().unwrap()])
            .env("HOME", &home),
        "gnx admin index",
    );

    // Confirm graph.bin materialised before we mutate the working tree.
    let gnx_root = home.join(".gnx");
    let graph_bin = WalkDir::new(&gnx_root)
        .max_depth(5)
        .into_iter()
        .filter_map(Result::ok)
        .find(|e| e.file_name() == OsStr::new("graph.bin"));
    assert!(
        graph_bin.is_some(),
        "graph.bin missing after admin index; tree:\n{:?}",
        WalkDir::new(&gnx_root)
            .max_depth(5)
            .into_iter()
            .filter_map(Result::ok)
            .map(|e| e.path().to_path_buf())
            .collect::<Vec<_>>()
    );

    // ── 3. Modify a source file WITHOUT committing ────────────────────────
    // Sleep briefly so the mtime is strictly newer than graph.bin.
    std::thread::sleep(std::time::Duration::from_millis(50));
    std::fs::write(
        repo.join("main.rs"),
        "fn original() {}\nfn added() {}\n",
    )
    .unwrap();

    // ── 4. Run a query — triggers auto_ensure → Stale → write_dirty_fragment
    // `scan` is the lightest agent command that goes through the graph-load path
    // in main.rs, which unconditionally calls ensure_fresh before loading graph.
    // The scan itself may succeed or produce "unresolved" output; either is fine
    // — we only care about the L1 side effect.
    let _ = Command::new(gnx_bin())
        .args(["scan", "main.rs", "--repo", repo.to_str().unwrap()])
        .env("HOME", &home)
        // Supply a stable session-id so the session dir is predictable.
        .env("CLAUDE_CODE_SESSION_ID", "test-l1-sid")
        .output()
        .expect("gnx scan spawn failed");

    // ── 5. Assert L1 fragment exists ──────────────────────────────────────
    let fragments: Vec<_> = WalkDir::new(&gnx_root)
        .max_depth(7)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| {
            e.path()
                .parent()
                .and_then(|d| d.file_name())
                .is_some_and(|n| n == OsStr::new("graph_overlay"))
                && e.path().extension() == Some(OsStr::new("bin"))
        })
        .collect();
    assert!(
        !fragments.is_empty(),
        "expected at least one graph_overlay/*.bin under {gnx_root:?};\ntree:\n{:?}",
        WalkDir::new(&gnx_root)
            .max_depth(7)
            .into_iter()
            .filter_map(Result::ok)
            .map(|e| e.path().to_path_buf())
            .collect::<Vec<_>>()
    );

    // ── 6. Assert session_meta.json exists ───────────────────────────────
    let session_metas: Vec<_> = WalkDir::new(&gnx_root)
        .max_depth(5)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_name() == OsStr::new("session_meta.json"))
        .collect();
    assert!(
        !session_metas.is_empty(),
        "expected session_meta.json under {gnx_root:?}"
    );

    // overlay_version must be ≥ 1 (bumped by write_dirty_fragment)
    let sm_content =
        std::fs::read_to_string(session_metas[0].path()).expect("read session_meta.json");
    assert!(
        sm_content.contains("\"overlay_version\"") && !sm_content.contains("\"overlay_version\":0"),
        "overlay_version should be ≥1; got: {sm_content}"
    );

    // ── 7. Assert dirty_files.json references the mutated file ───────────
    let dirty_files: Vec<_> = WalkDir::new(&gnx_root)
        .max_depth(5)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_name() == OsStr::new("dirty_files.json"))
        .collect();
    assert!(
        !dirty_files.is_empty(),
        "expected dirty_files.json under {gnx_root:?}"
    );
    let dirty_content =
        std::fs::read_to_string(dirty_files[0].path()).expect("read dirty_files.json");
    assert!(
        dirty_content.contains("main.rs"),
        "dirty_files.json should reference main.rs; got: {dirty_content}"
    );
}
