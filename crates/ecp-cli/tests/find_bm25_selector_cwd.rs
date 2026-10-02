//! A BM25 registry selector (`--repo <alias|csv|@all>`) answers from the
//! selector's own repos. The current directory's graph is not part of the
//! answer, so the query must not depend on it: a cwd that is not an indexed
//! repo must not fail the query, and the cwd must not be indexed as a side
//! effect. Everything that worked from an indexed cwd keeps printing the same
//! bytes.

mod common;

use common::{commit_all, ecp_bin, run_git};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

const MARKER: &str = "selector_marker_fn";
const ALIAS: &str = "selrepo";

struct World {
    /// Holds the repo (`<root>/selrepo`) so a test can use `root` as a cwd that
    /// contains a directory named like the alias.
    root: tempfile::TempDir,
    home: tempfile::TempDir,
    /// A directory that is neither a git repo nor indexed.
    non_repo: tempfile::TempDir,
    /// The registry key `admin index` gave the repo (`selrepo__<hash>`).
    key: String,
}

impl World {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let non_repo = tempfile::tempdir().unwrap();
        let repo = root.path().join(ALIAS);
        std::fs::create_dir(&repo).unwrap();
        std::fs::write(repo.join("lib.rs"), format!("pub fn {MARKER}() {{}}\n")).unwrap();
        run_git(&repo, &["init", "-q", "-b", "main"]);
        commit_all(&repo, "init");
        let mut w = World {
            root,
            home,
            non_repo,
            key: String::new(),
        };
        let out = w.ecp(&repo, &["admin", "index", "--repo", "."], None);
        assert!(
            out.status.success(),
            "admin index failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        w.key = std::fs::read_dir(w.home.path().join(".ecp"))
            .unwrap()
            .filter_map(Result::ok)
            .find(|e| e.path().join("commits").is_dir())
            .expect("admin index registered the repo")
            .file_name()
            .into_string()
            .unwrap();
        w
    }

    fn repo(&self) -> std::path::PathBuf {
        self.root.path().join(ALIAS)
    }

    fn ecp(&self, cwd: &Path, args: &[&str], stdin: Option<&str>) -> Output {
        spawn_ecp(cwd, self.home.path(), args, stdin)
            .wait_with_output()
            .expect("ecp did not finish")
    }

    /// Registry directories that hold at least one indexed commit.
    fn indexed_repo_dirs(&self) -> usize {
        std::fs::read_dir(self.home.path().join(".ecp"))
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .filter(|e| e.path().join("commits").is_dir())
                    .count()
            })
            .unwrap_or(0)
    }
}

fn spawn_ecp(cwd: &Path, home: &Path, args: &[&str], stdin: Option<&str>) -> std::process::Child {
    let mut child = Command::new(ecp_bin())
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("ECP_SESSION_ID")
        .env("ECP_SKIP_BG_REBUILD", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("ecp failed to spawn");
    let mut pipe = child.stdin.take().unwrap();
    if let Some(text) = stdin {
        pipe.write_all(text.as_bytes()).unwrap();
    }
    drop(pipe);
    child
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn assert_ok(out: &Output, what: &str) {
    assert!(out.status.success(), "{what} failed: {}", stderr(out));
}

fn selector_find(selector: &str) -> Vec<&str> {
    vec![
        "find", MARKER, "--mode", "bm25", "--repo", selector, "--format", "json",
    ]
}

#[test]
fn test_find_selector_non_repo_cwd_lists_target_hit() {
    let w = World::new();
    let before = w.indexed_repo_dirs();

    let out = w.ecp(w.non_repo.path(), &selector_find(&w.key), None);

    assert_ok(&out, "selector find from a non-repo cwd");
    assert!(
        stdout(&out).contains(MARKER),
        "the target repo's hit must be listed: {}",
        stdout(&out)
    );
    assert_eq!(
        w.indexed_repo_dirs(),
        before,
        "a selector query must not index the cwd as a side effect"
    );
}

#[test]
fn test_find_selector_indexed_cwd_matches_non_repo_cwd_stdout() {
    let w = World::new();

    let from_repo = w.ecp(&w.repo(), &selector_find(&w.key), None);
    let from_non_repo = w.ecp(w.non_repo.path(), &selector_find(&w.key), None);

    assert_ok(&from_repo, "selector find from the indexed repo");
    assert_ok(&from_non_repo, "selector find from a non-repo cwd");
    assert_eq!(stdout(&from_repo), stdout(&from_non_repo));
}

#[test]
fn test_find_selector_all_and_csv_work_from_non_repo_cwd() {
    let w = World::new();

    let csv = format!("{},{}", w.key, w.key);
    for selector in ["@all", w.key.as_str(), csv.as_str()] {
        let out = w.ecp(w.non_repo.path(), &selector_find(selector), None);
        assert_ok(&out, &format!("selector {selector}"));
        assert!(
            stdout(&out).contains(MARKER),
            "{selector}: {}",
            stdout(&out)
        );
    }
}

#[test]
fn test_find_exact_and_fuzzy_selector_keep_rejection() {
    let w = World::new();

    let variants: [Vec<&str>; 3] = [vec![], vec!["--fuzzy"], vec!["--mode", "fuzzy"]];
    for extra in variants {
        let mut args = vec!["find", MARKER, "--repo", ALIAS];
        args.extend_from_slice(&extra);
        let out = w.ecp(&w.repo(), &args, None);
        assert!(!out.status.success(), "{extra:?} must be rejected");
        assert!(
            stderr(&out).contains(
                "--repo selrepo: registry selectors are only supported with `--mode bm25`"
            ),
            "{extra:?}: {}",
            stderr(&out)
        );
    }
}

#[test]
fn test_find_batch_selector_non_repo_cwd_matches_indexed_cwd() {
    let w = World::new();
    let args = [
        "find", "--batch", "--mode", "bm25", "--repo", &w.key, "--format", "json",
    ];
    let stdin = format!("{MARKER}\n# comment\n\n{MARKER}\n");

    let from_non_repo = w.ecp(w.non_repo.path(), &args, Some(&stdin));
    let from_repo = w.ecp(&w.repo(), &args, Some(&stdin));

    assert_ok(&from_non_repo, "batch selector find from a non-repo cwd");
    assert_ok(&from_repo, "batch selector find from the indexed repo");
    let text = stdout(&from_non_repo);
    assert_eq!(
        text.matches(&format!("=== pattern: {MARKER} ===")).count(),
        2
    );
    assert!(text.contains(MARKER));
    assert_eq!(text, stdout(&from_repo));
}

#[test]
fn test_find_unknown_alias_still_errors_from_any_cwd() {
    let w = World::new();

    for cwd in [w.repo(), w.non_repo.path().to_path_buf()] {
        for batch in [false, true] {
            let mut args = selector_find("nosuchrepo");
            if batch {
                args = vec![
                    "find",
                    "--batch",
                    "--mode",
                    "bm25",
                    "--repo",
                    "nosuchrepo",
                    "--format",
                    "json",
                ];
            }
            let out = w.ecp(&cwd, &args, Some(&format!("{MARKER}\n")));
            assert!(!out.status.success(), "{cwd:?} batch={batch}");
            assert!(
                stderr(&out).contains("--repo: not in the registry: nosuchrepo"),
                "{cwd:?} batch={batch}: {}",
                stderr(&out)
            );
        }
    }
}

#[test]
fn test_find_selector_matching_no_repo_errors_from_non_repo_cwd() {
    let w = World::new();

    // A list of only separators names no repo.
    let out = w.ecp(w.non_repo.path(), &selector_find(","), None);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("the registry holds no repositories to search"),
        "{}",
        stderr(&out)
    );

    // `@all` over an empty registry resolves to zero targets.
    let empty_home = tempfile::tempdir().unwrap();
    let out = spawn_ecp(
        w.non_repo.path(),
        empty_home.path(),
        &selector_find("@all"),
        None,
    )
    .wait_with_output()
    .unwrap();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("the registry holds no repositories to search"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn test_find_empty_and_dot_repo_keep_using_cwd_graph() {
    let w = World::new();
    let base = ["find", MARKER, "--mode", "bm25", "--format", "json"];

    let plain = w.ecp(&w.repo(), &base, None);
    assert_ok(&plain, "find without --repo");

    for value in ["", "."] {
        let mut args = base.to_vec();
        args.extend_from_slice(&["--repo", value]);
        let out = w.ecp(&w.repo(), &args, None);
        assert_ok(&out, &format!("--repo {value:?}"));
        assert_eq!(stdout(&out), stdout(&plain), "--repo {value:?}");
    }
}

/// A selector that is also a directory name in cwd is a path, not a registry
/// key (`Commands::repo()` documents the trade-off). The answer comes from that
/// directory's graph, so it matches `--repo .` run inside it.
#[test]
fn test_find_selector_named_like_cwd_directory_uses_path_semantics() {
    let w = World::new();

    let by_path = w.ecp(w.root.path(), &selector_find(ALIAS), None);
    let in_repo = w.ecp(
        &w.repo(),
        &[
            "find", MARKER, "--mode", "bm25", "--repo", ".", "--format", "json",
        ],
        None,
    );

    assert_ok(&by_path, "--repo <directory name>");
    assert_ok(&in_repo, "--repo .");
    assert_eq!(stdout(&by_path), stdout(&in_repo));
}

#[test]
fn test_find_selector_twice_concurrently_prints_identical_stdout() {
    let w = World::new();
    let args = selector_find(&w.key);

    let first = spawn_ecp(w.non_repo.path(), w.home.path(), &args, None);
    let second = spawn_ecp(w.non_repo.path(), w.home.path(), &args, None);
    let (first, second) = (
        first.wait_with_output().unwrap(),
        second.wait_with_output().unwrap(),
    );

    assert_ok(&first, "first concurrent selector find");
    assert_ok(&second, "second concurrent selector find");
    assert!(stdout(&first).contains(MARKER));
    assert_eq!(stdout(&first), stdout(&second));
}
