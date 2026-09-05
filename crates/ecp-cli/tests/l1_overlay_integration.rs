//! Integration test for the auto_ensure Stale → write_dirty_fragment path.
//!
//! Exercises the real end-to-end flow:
//!   build L2 index → modify a source file (without committing) →
//!   run a query → assert L1 fragments + dirty_files.json + session_meta
//!   materialise under <home>/.ecp/<repo>__<hash>/sessions/<sid>/.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::WalkDir;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
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
        Command::new("git").arg("-C").arg(&repo).args(["add", "."]),
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
        Command::new(ecp_bin())
            .args(["admin", "index", "--repo", repo.to_str().unwrap()])
            .env("HOME", &home),
        "ecp admin index",
    );

    // Confirm graph.bin materialised before we mutate the working tree.
    let ecp_root = home.join(".ecp");
    let graph_bin = WalkDir::new(&ecp_root)
        .max_depth(5)
        .into_iter()
        .filter_map(Result::ok)
        .find(|e| e.file_name() == OsStr::new("graph.bin"));
    assert!(
        graph_bin.is_some(),
        "graph.bin missing after admin index; tree:\n{:?}",
        WalkDir::new(&ecp_root)
            .max_depth(5)
            .into_iter()
            .filter_map(Result::ok)
            .map(|e| e.path().to_path_buf())
            .collect::<Vec<_>>()
    );

    // ── 3. Modify a source file WITHOUT committing ────────────────────────
    // Sleep briefly so the mtime is strictly newer than graph.bin.
    std::thread::sleep(std::time::Duration::from_millis(50));
    std::fs::write(repo.join("main.rs"), "fn original() {}\nfn added() {}\n").unwrap();

    // ── 4. Run a query — triggers auto_ensure → Stale → write_dirty_fragment
    // `find` is a lightweight agent command that goes through the graph-load path
    // in main.rs, which unconditionally calls ensure_fresh before loading graph.
    // The find itself may succeed or produce no results; either is fine
    // — we only care about the L1 side effect.
    let _ = Command::new(ecp_bin())
        .args(["find", "main", "--repo", repo.to_str().unwrap()])
        .env("HOME", &home)
        // Supply a stable session-id so the session dir is predictable.
        .env("CLAUDE_CODE_SESSION_ID", "test-l1-sid")
        .output()
        .expect("ecp find spawn failed");

    // ── 5. Assert L1 fragment exists ──────────────────────────────────────
    let fragments: Vec<_> = WalkDir::new(&ecp_root)
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
        "expected at least one graph_overlay/*.bin under {ecp_root:?};\ntree:\n{:?}",
        WalkDir::new(&ecp_root)
            .max_depth(7)
            .into_iter()
            .filter_map(Result::ok)
            .map(|e| e.path().to_path_buf())
            .collect::<Vec<_>>()
    );

    // ── 6. Assert session_meta.json exists ───────────────────────────────
    let session_metas: Vec<_> = WalkDir::new(&ecp_root)
        .max_depth(5)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_name() == OsStr::new("session_meta.json"))
        .collect();
    assert!(
        !session_metas.is_empty(),
        "expected session_meta.json under {ecp_root:?}"
    );

    // overlay_version must be ≥ 1 (bumped by write_dirty_fragment)
    let sm_content =
        std::fs::read_to_string(session_metas[0].path()).expect("read session_meta.json");
    assert!(
        sm_content.contains("\"overlay_version\"") && !sm_content.contains("\"overlay_version\":0"),
        "overlay_version should be ≥1; got: {sm_content}"
    );

    // ── 7. Assert dirty_files.json references the mutated file ───────────
    let dirty_files: Vec<_> = WalkDir::new(&ecp_root)
        .max_depth(5)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_name() == OsStr::new("dirty_files.json"))
        .collect();
    assert!(
        !dirty_files.is_empty(),
        "expected dirty_files.json under {ecp_root:?}"
    );
    let dirty_content =
        std::fs::read_to_string(dirty_files[0].path()).expect("read dirty_files.json");
    assert!(
        dirty_content.contains("main.rs"),
        "dirty_files.json should reference main.rs; got: {dirty_content}"
    );

    // `ecp peers` classifies HARD and SOFT concerns off `dirty_symbols` and
    // nothing else, so an entry written without them makes every peer overlap
    // invisible. The pre-parsed write route left the field empty, which is
    // exactly the path this query takes.
    let manifest: serde_json::Value =
        serde_json::from_str(&dirty_content).expect("dirty_files.json parses");
    let symbols: Vec<&str> = manifest["entries"]
        .as_object()
        .expect("entries object")
        .values()
        .flat_map(|e| e["dirty_symbols"].as_array().into_iter().flatten())
        .filter_map(|s| s["name"].as_str())
        .collect();
    assert!(
        symbols.contains(&"added"),
        "dirty_symbols should carry the edited file's symbols; got {symbols:?} from {dirty_content}"
    );
}

/// End-to-end: a brand-new symbol added to the working tree (never committed,
/// not in the L2 graph) must be findable via `ecp find` through the L1 overlay,
/// provided the session id is stable across the write and read invocations.
#[test]
fn find_surfaces_overlay_only_symbol() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let sid = "test-overlay-visible";

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
        Command::new("git").arg("-C").arg(&repo).args(["add", "."]),
        "git add",
    );
    run(
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["commit", "-qm", "init"]),
        "git commit",
    );

    run(
        Command::new(ecp_bin())
            .args(["admin", "index", "--repo", repo.to_str().unwrap()])
            .env("HOME", &home),
        "ecp admin index",
    );

    // Add a brand-new symbol to the working tree without committing.
    std::thread::sleep(std::time::Duration::from_millis(50));
    std::fs::write(
        repo.join("main.rs"),
        "fn original() {}\nfn overlay_only_symbol_xyz() {}\n",
    )
    .unwrap();

    // First query: triggers the incremental path → writes the overlay fragment
    // under the stable session.
    let _ = Command::new(ecp_bin())
        .args(["find", "original", "--repo", repo.to_str().unwrap()])
        .env("HOME", &home)
        .env("CLAUDE_CODE_SESSION_ID", sid)
        .output()
        .expect("ecp find (warm) spawn failed");

    // Second query: the new symbol exists only in the overlay; it must now be
    // findable through the overlay merge on the same session.
    let out = Command::new(ecp_bin())
        .args([
            "find",
            "overlay_only_symbol_xyz",
            "--repo",
            repo.to_str().unwrap(),
            "--format",
            "json",
        ])
        .env("HOME", &home)
        .env("CLAUDE_CODE_SESSION_ID", sid)
        .output()
        .expect("ecp find (overlay) spawn failed");

    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"found\":true") || stdout.contains("\"found\": true"),
        "overlay-only symbol must be findable via find; got stdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("overlay_only_symbol_xyz"),
        "find result must name the overlay symbol; got:\n{stdout}"
    );
    assert!(
        stdout.contains("main.rs"),
        "find result must carry the overlay symbol's file path; got:\n{stdout}"
    );
}

fn init_find_overlay_repo(file: &str, source: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        run(
            Command::new("git").arg("-C").arg(&repo).args(args),
            "git setup",
        );
    }
    std::fs::write(repo.join(file), source).unwrap();
    run(
        Command::new("git").arg("-C").arg(&repo).args(["add", "."]),
        "git add",
    );
    run(
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["commit", "-qm", "init"]),
        "git commit",
    );
    run(
        Command::new(ecp_bin())
            .args(["admin", "index", "--repo"])
            .arg(&repo)
            .env("HOME", &home)
            .env("ECP_HOME", &home)
            .env("CLAUDE_CODE_SESSION_ID", "test-find-suppression"),
        "ecp admin index",
    );
    (tmp, repo, home)
}

fn find_overlay_json(repo: &Path, home: &Path, name: &str, mode: &str) -> serde_json::Value {
    let out = run(
        Command::new(ecp_bin())
            .args([
                "find", name, "--mode", mode, "--all", "--file", "src", "--format", "json",
                "--repo",
            ])
            .arg(repo)
            .env("HOME", home)
            .env("ECP_HOME", home)
            .env("CLAUDE_CODE_SESSION_ID", "test-find-suppression"),
        "ecp find",
    );
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "invalid find JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

#[test]
fn find_suppresses_symbols_in_an_emptied_dirty_file() {
    let (_tmp, repo, home) = init_find_overlay_repo("src/a.rs", "fn wanderer() {}\n");
    let clean = find_overlay_json(&repo, &home, "wanderer", "exact");
    assert_eq!(clean["found"], true, "{clean}");
    assert_eq!(clean["matches"][0]["file"], "src/a.rs", "{clean}");
    assert_eq!(clean["matches"][0]["line"], 1, "{clean}");

    std::thread::sleep(std::time::Duration::from_millis(50));
    std::fs::write(repo.join("src/a.rs"), "\n").unwrap();
    // Empty fragments must suppress too, including when a later process reads them.
    for mode in ["exact", "exact", "fuzzy"] {
        let result = find_overlay_json(&repo, &home, "wanderer", mode);
        assert_eq!(result["found"], false, "{result}");
        assert_eq!(result["matches"], serde_json::json!([]), "{result}");
        assert_eq!(result["total_candidates"], 0, "{result}");
        assert_eq!(result["returned"], 0, "{result}");
    }
}

#[test]
fn find_honours_dirty_file_suppression_across_languages() {
    for (file, prefix, declaration, suffix) in [
        ("src/a.ts", "", "function NAME() {}\n", ""),
        ("src/a.js", "", "function NAME() {}\n", ""),
        ("src/a.py", "", "def NAME(): pass\n", ""),
        ("src/A.java", "class A {\n", "void NAME() {}\n", "}\n"),
        ("src/a.kt", "", "fun NAME() {}\n", ""),
        ("src/A.cs", "class A {\n", "void NAME() {}\n", "}\n"),
        ("src/a.go", "package a\n", "func NAME() {}\n", ""),
        ("src/a.rs", "", "fn NAME() {}\n", ""),
        ("src/a.php", "<?php\n", "function NAME() {}\n", ""),
        ("src/a.rb", "", "def NAME; end\n", ""),
        ("src/a.swift", "", "func NAME() {}\n", ""),
        ("src/a.c", "", "void NAME() {}\n", ""),
        ("src/a.cpp", "", "void NAME() {}\n", ""),
        ("src/a.dart", "", "void NAME() {}\n", ""),
    ] {
        let deleted = declaration.replace("NAME", "overlay_deleted");
        let kept = declaration.replace("NAME", "overlay_kept");
        let added = declaration.replace("NAME", "overlay_added");
        let (_tmp, repo, home) =
            init_find_overlay_repo(file, &format!("{prefix}{deleted}{kept}{suffix}"));
        let first_line = prefix.lines().count() as u64 + 1;

        for (name, line) in [
            ("overlay_deleted", first_line),
            ("overlay_kept", first_line + 1),
        ] {
            let clean = find_overlay_json(&repo, &home, name, "exact");
            assert_eq!(clean["found"], true, "{file}: {clean}");
            assert_eq!(clean["total_candidates"], 1, "{file}: {clean}");
            assert_eq!(clean["matches"][0]["file"], file, "{file}: {clean}");
            assert_eq!(clean["matches"][0]["line"], line, "{file}: {clean}");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        std::fs::write(
            repo.join(file),
            format!("{prefix}\n\n{kept}{added}{suffix}"),
        )
        .unwrap();

        for (name, line) in [
            ("overlay_kept", first_line + 2),
            ("overlay_added", first_line + 3),
        ] {
            let result = find_overlay_json(&repo, &home, name, "exact");
            assert_eq!(result["found"], true, "{file}: {result}");
            assert_eq!(result["total_candidates"], 1, "{file}: {result}");
            assert_eq!(result["matches"][0]["file"], file, "{file}: {result}");
            assert_eq!(result["matches"][0]["line"], line, "{file}: {result}");
        }
        let result = find_overlay_json(&repo, &home, "overlay_deleted", "exact");
        assert_eq!(result["found"], false, "{file}: {result}");
        assert_eq!(result["matches"], serde_json::json!([]), "{file}: {result}");
        assert_eq!(result["total_candidates"], 0, "{file}: {result}");

        let result = find_overlay_json(&repo, &home, "overlay_", "fuzzy");
        let mut names: Vec<_> = result["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["overlay_added", "overlay_kept"], "{file}: {result}");
        assert_eq!(result["total_candidates"], 2, "{file}: {result}");
        assert_eq!(result["returned"], 2, "{file}: {result}");
    }
}

/// A symbol that already existed at HEAD but moved in the working tree must
/// report where it is now, not where it was.
///
/// The overlay carries this file's parse as it is on disk, and `find` was
/// discarding every overlay hit whose uid the base graph already had — then
/// reporting the base node's line. So one file answered from two vintages at
/// once: a symbol added since HEAD got its real line from the overlay, and a
/// symbol that merely moved got HEAD's, with `found: true` on both and nothing
/// to tell them apart. A model following the second one reads the wrong lines.
/// PR #762 fixed this by routing the base scan through `MergedGraph`; this
/// test holds the contract with no `--file` filter, which that PR's matrix
/// never queries without.
///
/// Both symbols are asserted in the same run for exactly that reason — checking
/// only the moved one would pass for a build that dropped the overlay entirely.
#[test]
fn find_reports_the_worktree_line_for_a_symbol_that_moved() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let sid = "test-moved-symbol";

    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "t@t"],
        vec!["config", "user.name", "t"],
    ] {
        run(
            Command::new("git").arg("-C").arg(&repo).args(&args),
            "git setup",
        );
    }
    std::fs::write(repo.join("tools.rs"), "fn moved_fn() {}\n").unwrap();
    run(
        Command::new("git").arg("-C").arg(&repo).args(["add", "."]),
        "git add",
    );
    run(
        Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["commit", "-qm", "init"]),
        "git commit",
    );
    run(
        Command::new(ecp_bin())
            .args(["admin", "index", "--repo", repo.to_str().unwrap()])
            .env("HOME", &home),
        "ecp admin index",
    );

    // Push `moved_fn` down by 24 uncommitted lines and add a second symbol
    // below it. Neither is committed, so the L2 graph still describes HEAD.
    std::thread::sleep(std::time::Duration::from_millis(50));
    let padding = "// pad\n".repeat(24);
    std::fs::write(
        repo.join("tools.rs"),
        format!("{padding}fn moved_fn() {{}}\nfn added_fn() {{}}\n"),
    )
    .unwrap();

    // First query writes the overlay fragment for the stable session.
    let _ = Command::new(ecp_bin())
        .args(["find", "moved_fn", "--repo", repo.to_str().unwrap()])
        .env("HOME", &home)
        .env("CLAUDE_CODE_SESSION_ID", sid)
        .output()
        .expect("warm query");

    let find = |name: &str| -> String {
        let out = Command::new(ecp_bin())
            .args([
                "find",
                name,
                "--repo",
                repo.to_str().unwrap(),
                "--format",
                "json",
            ])
            .env("HOME", &home)
            .env("CLAUDE_CODE_SESSION_ID", sid)
            .output()
            .expect("find");
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    let moved = find("moved_fn");
    assert!(
        moved.contains("\"line\":25") || moved.contains("\"line\": 25"),
        "moved_fn is on line 25 of the working tree, not line 1 of HEAD; got:\n{moved}"
    );

    let added = find("added_fn");
    assert!(
        added.contains("\"line\":26") || added.contains("\"line\": 26"),
        "added_fn must keep reporting its overlay line; got:\n{added}"
    );
}
