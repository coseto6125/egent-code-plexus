//! A file the indexer left out has to reach the reading model.
//!
//! The walker's per-file size cap dropped a file, incremented a counter, and
//! emitted a `tracing::warn!` on stderr. The graph said nothing, `ecp summary`
//! reported `blind_spots.total: 0`, and `ecp find` answered `found: false` for a
//! symbol that is in the tree. ECP.md tells the consuming model that an
//! uncaveated `found:false` means the symbol does not exist, so that answer was
//! a fabrication, not a gap.

use std::path::Path;
use std::process::Command;

fn ecp_bin() -> &'static str {
    env!("CARGO_BIN_EXE_ecp")
}

fn git(repo: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("git available");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repo holding `ok.py` plus, when `with_oversized` is set, a `big.py` whose
/// size puts it past the 1 MiB default cap.
fn fixture(root: &Path, with_oversized: bool) {
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("ok.py"), "def visible():\n    return 1\n").unwrap();
    if with_oversized {
        let mut big = String::from("def big_fn():\n    return 1\n");
        big.push_str(&"# pad\n".repeat(400_000));
        std::fs::write(repo.join("big.py"), big).unwrap();
    }
    git(&repo, &["init", "-q", "."]);
    git(&repo, &["config", "user.email", "t@example.invalid"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "one"]);
}

fn run(root: &Path, args: &[&str]) -> String {
    let out = Command::new(ecp_bin())
        .args(args)
        .current_dir(root.join("repo"))
        .env("ECP_HOME", root.join("home").join(".ecp"))
        .output()
        .expect("run ecp");
    String::from_utf8_lossy(&out.stdout).to_string()
}

#[test]
fn an_oversized_file_becomes_a_blind_spot_rather_than_silence() {
    let tmp = tempfile::tempdir().unwrap();
    fixture(tmp.path(), true);
    run(tmp.path(), &["admin", "index", "--repo", "."]);

    let summary = run(tmp.path(), &["summary", "--repo", ".", "--format", "json"]);
    assert!(
        summary.contains("file-too-large"),
        "the omission has to be in the graph, not only in a log line: {summary}"
    );
}

/// Every query names its format. The default is structured today and these
/// assertions hold against it, but a test that leans on a default is a test
/// that breaks for a reason unrelated to what it checks.
///
/// The caveat rides the `result` field, which is what ECP.md already documents
/// as the marker for a provisional answer — so this needs no new convention for
/// the reading model to learn.
///
/// Both directions matter. Without the second half the caveat could be attached
/// to every miss, which would train the reader to ignore it.
#[test]
fn a_miss_names_the_omissions_only_when_the_index_has_some() {
    let incomplete = tempfile::tempdir().unwrap();
    fixture(incomplete.path(), true);
    run(incomplete.path(), &["admin", "index", "--repo", "."]);
    let miss = run(
        incomplete.path(),
        &["find", "big_fn", "--repo", ".", "--format", "json"],
    );
    assert!(
        miss.contains("\"found\":false"),
        "the symbol is genuinely out of the graph: {miss}"
    );
    assert!(
        miss.contains("left out 1 file"),
        "a miss against an incomplete index must say so: {miss}"
    );

    let complete = tempfile::tempdir().unwrap();
    fixture(complete.path(), false);
    run(complete.path(), &["admin", "index", "--repo", "."]);
    let clean_miss = run(
        complete.path(),
        &["find", "nonexistent", "--repo", ".", "--format", "json"],
    );
    assert!(
        clean_miss.contains("\"found\":false"),
        "expected a miss: {clean_miss}"
    );
    assert!(
        !clean_miss.contains("left out"),
        "a complete index must not caveat its misses: {clean_miss}"
    );

    let hit = run(
        complete.path(),
        &["find", "visible", "--repo", ".", "--format", "json"],
    );
    assert!(
        hit.contains("\"found\":true") && !hit.contains("left out"),
        "a hit carries no omission caveat: {hit}"
    );
}
