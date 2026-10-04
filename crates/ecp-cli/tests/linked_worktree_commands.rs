use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use ecp_cli::commands::group::storage::{group_dir, read_contracts, read_meta};
use serde_json::Value;

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    main: PathBuf,
    linked: PathBuf,
    member: String,
}

fn git(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(cwd)
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        let linked = tmp.path().join("linked");
        let home = tmp.path().join("home");
        fs::create_dir_all(main.join("src")).unwrap();
        fs::create_dir(&home).unwrap();
        fs::write(main.join("src/lib.rs"), "pub fn hello_world() {}\n").unwrap();
        git(&main, &["init", "-q", "-b", "main"]);
        git(&main, &["add", "."]);
        git(&main, &["commit", "-qm", "main"]);
        git(
            &main,
            &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
        );
        let mut f = Self {
            _tmp: tmp,
            home,
            main,
            linked,
            member: String::new(),
        };
        f.run(&f.main, &["admin", "index", "--repo", "."]);
        let registry: Value =
            serde_json::from_slice(&fs::read(f.home.join(".ecp/registry.json")).unwrap()).unwrap();
        f.member = registry["repos"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        f.run(&f.main, &["admin", "group", "add", &f.member, "demo"]);
        fs::write(f.linked.join("src/new.rs"), "pub fn linked_only() {}\n").unwrap();
        git(&f.linked, &["add", "."]);
        git(&f.linked, &["commit", "-qm", "linked"]);
        f
    }

    fn run(&self, cwd: &Path, args: &[&str]) -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_ecp"))
            .current_dir(cwd)
            .args(args)
            .env("HOME", &self.home)
            .env("ECP_HOME", self.home.join(".ecp"))
            .env("ECP_SKIP_BG_REBUILD", "1")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn json(&self, cwd: &Path, args: &[&str]) -> Value {
        serde_json::from_str(&self.run(cwd, args)).unwrap()
    }

    fn assert_caveat(&self, args: &[&str]) {
        let main = self.json(&self.main, args);
        assert!(main.get("result").is_none(), "fresh main: {main}");
        let linked = self.json(&self.linked.join("src"), args);
        assert!(
            linked["result"]
                .as_str()
                .is_some_and(|s| s.contains("predates")),
            "linked HEAD requires a caveat: {linked}"
        );
    }
}

#[test]
fn test_summary_linked_worktree_reports_own_head_and_freshness() {
    let f = Fixture::new();
    let args = ["summary", "--repo", ".", "--format", "json"];
    let main = f.json(&f.main, &args);
    assert_eq!(
        main["summary"]["per_repo"][0]["freshness"]["current_head_short"],
        git(&f.main, &["rev-parse", "--short", "HEAD"])
    );
    assert_eq!(
        main["summary"]["per_repo"][0]["freshness"]["status"],
        "ready"
    );
    f.run(&f.linked, &["admin", "index", "--force", "--repo", "."]);
    fs::write(f.main.join("src/lib.rs"), "pub fn dirty_main() {}\n").unwrap();
    let linked = f.json(&f.linked.join("src"), &args);
    assert_eq!(
        linked["summary"]["per_repo"][0]["freshness"]["current_head_short"],
        git(&f.linked, &["rev-parse", "--short", "HEAD"])
    );
    assert_eq!(
        linked["summary"]["per_repo"][0]["freshness"]["status"],
        "ready"
    );
    fs::write(f.linked.join("src/new.rs"), "pub fn dirty_linked() {}\n").unwrap();
    let dirty = f.json(&f.linked, &args);
    assert_eq!(
        dirty["summary"]["per_repo"][0]["freshness"]["status"],
        "stale"
    );
}

#[test]
fn test_find_linked_worktree_reports_behind_head() {
    Fixture::new().assert_caveat(&[
        "find", "hello", "--mode", "bm25", "--repo", "@all", "--format", "json",
    ]);
}

#[test]
fn test_group_find_linked_worktree_reports_behind_head() {
    Fixture::new().assert_caveat(&["group", "find", "demo", "hello", "--json"]);
}

#[test]
fn test_group_impact_linked_worktree_reports_behind_head() {
    let f = Fixture::new();
    f.assert_caveat(&[
        "group",
        "impact",
        "demo",
        "--target",
        "hello_world",
        "--repo",
        &f.member,
        "--json",
    ]);
}

#[test]
fn test_group_status_linked_worktree_compares_own_head() {
    let f = Fixture::new();
    f.run(&f.main, &["group", "sync", "demo", "--json"]);
    let args = ["group", "status", "demo", "--json"];
    assert_eq!(f.json(&f.main, &args)["members"][0]["status"], "OK");
    let linked = f.json(&f.linked, &args);
    assert_eq!(linked["members"][0]["status"], "STALE");
    assert_eq!(linked["members"][0]["commits_behind"], 1);
}

#[test]
fn test_group_sync_linked_worktree_snapshots_own_head() {
    let f = Fixture::new();
    fs::write(
        f.linked.join("api.go"),
        "package main\nimport \"net/http\"\nfunc main() {\n mux := http.NewServeMux()\n mux.HandleFunc(\"/linked\", linkedHandler)\n}\nfunc linkedHandler(w http.ResponseWriter, r *http.Request) {}\n",
    )
    .unwrap();
    for cwd in [&f.main, &f.linked] {
        f.run(cwd, &["group", "sync", "demo", "--json"]);
        let dir = group_dir(&f.home.join(".ecp"), "demo").unwrap();
        let meta = read_meta(&dir).unwrap();
        assert_eq!(
            meta.repo_snapshots[&f.member].last_commit,
            git(cwd, &["rev-parse", "HEAD"])
        );
        let contracts = read_contracts(&dir).unwrap();
        assert_eq!(
            contracts
                .contracts
                .iter()
                .any(|c| c.inner.contract_id.contains("/linked")),
            cwd == &f.linked,
            "contracts must come from the selected worktree: {contracts:?}"
        );
    }
}

#[test]
fn test_summary_unrelated_cwd_keeps_selected_repository() {
    let f = Fixture::new();
    let other = f._tmp.path().join("other");
    fs::create_dir(&other).unwrap();
    git(&other, &["init", "-q", "-b", "main"]);
    git(&other, &["commit", "--allow-empty", "-qm", "unrelated"]);
    let summary = f.json(
        &other,
        &["summary", "--repo", &f.member, "--format", "json"],
    );
    assert_eq!(
        summary["summary"]["per_repo"][0]["freshness"]["current_head_short"],
        git(&f.main, &["rev-parse", "--short", "HEAD"])
    );
    assert_eq!(
        summary["summary"]["per_repo"][0]["freshness"]["status"],
        "ready"
    );
    let find = f.json(&other, &["group", "find", "demo", "hello", "--json"]);
    assert!(
        find.get("result").is_none(),
        "unrelated HEAD must not affect the selected member: {find}"
    );
}
