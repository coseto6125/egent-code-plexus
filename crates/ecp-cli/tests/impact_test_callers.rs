//! `ecp impact` and test-file callers, through a real index.
//!
//! - Default output lists test callers, tagged `test: true`: a rename breaks
//!   them too, and an agent running the documented `ecp impact --target X`
//!   saw 6 of 39 callers on a real repo when tests were dropped silently.
//! - `--exclude-tests` drops them and reports `hidden_test_callers: N`.
//! - A test fake sharing a production method's name no longer hides the
//!   production callers (resolver test-double tie-break).

mod common;

use common::{ecp_bin, run_git};
use std::path::Path;
use std::process::{Command, Output};

fn write(repo: &Path, rel: &str, body: &str) {
    let path = repo.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn setup_repo(repo: &Path, home: &Path) {
    write(repo, "app/__init__.py", "");
    write(repo, "app/util.py", "def normalize(x):\n    return x\n");
    write(
        repo,
        "app/handler.py",
        "from app.util import normalize\n\n\ndef handle():\n    return normalize(1)\n",
    );
    write(
        repo,
        "tests/test_util.py",
        "from app.util import normalize\n\n\ndef test_normalize():\n    assert normalize(1) == 1\n",
    );
    write(
        repo,
        "app/service.py",
        "class FlightService:\n    def scan_range(self, a):\n        return a\n",
    );
    write(
        repo,
        "app/search.py",
        "from app.service import FlightService\n\n\ndef search():\n    service = FlightService()\n    return service.scan_range(1)\n",
    );
    write(
        repo,
        "tests/fakes.py",
        "class FakeFlightService:\n    def scan_range(self, a):\n        return a\n",
    );
    run_git(repo, &["init", "-q", "-b", "main"]);
    run_git(
        repo,
        &[
            "remote",
            "add",
            "origin",
            "git@github.com:E-NoR/impact-test-callers.git",
        ],
    );
    run_git(repo, &["add", "-A"]);
    run_git(
        repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn impact(repo: &Path, home: &Path, args: &[&str]) -> Output {
    let mut argv = vec!["impact"];
    argv.extend_from_slice(args);
    argv.extend_from_slice(&["--repo", ".", "--format", "json"]);
    let out = Command::new(ecp_bin())
        .args(&argv)
        .current_dir(repo)
        .env("HOME", home)
        .output()
        .expect("impact failed to spawn");
    assert!(
        out.status.success(),
        "impact {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn json_of(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap()
}

/// `(name, test)` for every reached node past the target itself.
fn callers(json: &serde_json::Value) -> Vec<(String, bool)> {
    json["impact"]
        .as_array()
        .unwrap_or_else(|| panic!("impact array missing: {json}"))
        .iter()
        .filter(|e| e["depth"].as_u64() > Some(0))
        .map(|e| {
            (
                e["name"].as_str().unwrap().to_string(),
                e["test"]
                    .as_bool()
                    .unwrap_or_else(|| panic!("every entry carries `test`: {e}")),
            )
        })
        .collect()
}

fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    setup_repo(repo.path(), home.path());
    (repo, home)
}

#[test]
fn test_impact_default_lists_test_callers_tagged() {
    let (repo, home) = fixture();
    let json = json_of(&impact(repo.path(), home.path(), &["normalize"]));
    let mut got = callers(&json);
    got.sort();
    assert_eq!(
        got,
        vec![
            ("handle".to_string(), false),
            ("test_normalize".to_string(), true)
        ],
        "{json}"
    );
    assert!(json.get("hidden_test_callers").is_none(), "{json}");
}

#[test]
fn test_impact_exclude_tests_hides_and_counts_test_callers() {
    let (repo, home) = fixture();
    let out = impact(repo.path(), home.path(), &["normalize", "--exclude-tests"]);
    let json = json_of(&out);
    assert_eq!(
        callers(&json),
        vec![("handle".to_string(), false)],
        "{json}"
    );
    assert_eq!(json["hidden_test_callers"], 1, "{json}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("1 test callers hidden"),
        "stderr must say tests were hidden: {stderr}"
    );
}

#[test]
fn test_impact_legacy_include_tests_flag_before_positional_still_parses() {
    // `--include-tests` takes an optional `=value`; a bare flag must not
    // swallow the positional target that follows it.
    let (repo, home) = fixture();
    let json = json_of(&impact(
        repo.path(),
        home.path(),
        &["--include-tests", "normalize"],
    ));
    assert_eq!(callers(&json).len(), 2, "{json}");
}

#[test]
fn test_impact_production_caller_resolves_past_test_double() {
    // `service.scan_range(1)` has an untyped receiver, so the call resolves
    // by bare name. The fake in tests/ used to make that AmbiguousGlobal.
    let (repo, home) = fixture();
    let json = json_of(&impact(
        repo.path(),
        home.path(),
        &["scan_range", "--file", "app/service.py"],
    ));
    assert_eq!(
        callers(&json),
        vec![("search".to_string(), false)],
        "{json}"
    );
}

#[test]
fn test_impact_ambiguous_name_with_no_callers_does_not_suggest_dead_code() {
    // Two production definitions and no resolvable caller: the hint must
    // point at the suppressed bare calls, not at dead code.
    let repo = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    write(
        repo.path(),
        "src/alpha.py",
        "def process():\n    return 1\n",
    );
    write(repo.path(), "src/beta.py", "def process():\n    return 2\n");
    write(
        repo.path(),
        "src/caller.py",
        "def run_all():\n    process()\n",
    );
    run_git(repo.path(), &["init", "-q", "-b", "main"]);
    run_git(repo.path(), &["add", "-A"]);
    run_git(
        repo.path(),
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    let out = Command::new(ecp_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo.path())
        .env("HOME", home.path())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = impact(
        repo.path(),
        home.path(),
        &["process", "--file", "src/alpha.py"],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("share its name"),
        "hint must name the ambiguity: {stderr}"
    );
    assert!(
        !stderr.contains("dead code, or recent rename"),
        "hint must not suggest dead code: {stderr}"
    );
}

#[test]
fn test_impact_exclude_tests_keeps_a_test_file_target_and_does_not_count_it() {
    // The start node is the question, not a caller: excluding tests must not
    // drop it (the walk's first entry is always the target at depth 0), and
    // it must not be counted as a hidden test caller.
    let (repo, home) = fixture();
    let json = json_of(&impact(
        repo.path(),
        home.path(),
        &["test_normalize", "--direction", "down", "--exclude-tests"],
    ));
    let rows = json["impact"].as_array().unwrap();
    assert_eq!(rows[0]["name"], "test_normalize", "{json}");
    assert_eq!(rows[0]["depth"], 0, "{json}");
    assert!(
        rows.iter().any(|r| r["name"] == "normalize"),
        "the production callee is still reached: {json}"
    );
    assert!(json.get("hidden_test_callers").is_none(), "{json}");
}
