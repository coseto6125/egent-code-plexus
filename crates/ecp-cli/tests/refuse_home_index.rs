//! A filesystem root, or the home directory or an ancestor of it with no git
//! commit, is never indexed or walked, by a query, a build, `admin index`, its
//! `--force` and `--dump-resolver` passes, or `group sync`. A repository with a commit at or above home
//! still indexes: containers and this crate's tests use a project directory as
//! `$HOME` or put a sandbox `$HOME` inside one.
//!
//! Stands for: an agent whose unquoted `cd $D` with `$D` empty landed in
//! `$HOME` gets a refusal before any file is parsed. On 2026-11-01
//! `ecp admin index --force --repo .` in a non-git `$HOME` parsed every repo on
//! the machine and the host lost power. The asserted text "home directory"
//! stands for "the refusal names its reason"; "--repo" stands for "the advice
//! also works when the path came from `--repo`".

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Keeps the developer's git config and any inherited `GIT_DIR` out. The ecp
/// process gets no `GIT_CEILING_DIRECTORIES`: ecp treats it as an override and
/// would leave its file-reading git layout for the spawn fallback, so the tests
/// would not run the branch production uses. [`Fixture::new`] checks instead
/// that no repository sits above the fixture.
fn clear_git_env(cmd: &mut Command) -> &mut Command {
    cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env_remove("GIT_CEILING_DIRECTORIES")
        .env_remove("GIT_DIR")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
}

struct Fixture {
    root: tempfile::TempDir,
    ecp_home: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        let fx = Self {
            root: tempfile::tempdir().unwrap(),
            ecp_home: tempfile::tempdir().unwrap(),
        };
        write_py(&fx.home(), "a.py");
        let mut probe = Command::new("git");
        let above = clear_git_env(&mut probe)
            .args(["rev-parse", "--git-dir"])
            .current_dir(fx.root.path())
            .output()
            .unwrap();
        assert!(
            !above.status.success(),
            "the fixture must not sit inside a repository: {}",
            String::from_utf8_lossy(&above.stdout)
        );
        fx
    }

    fn parent(&self) -> PathBuf {
        self.root.path().join("users")
    }

    fn home(&self) -> PathBuf {
        self.parent().join("me")
    }

    fn ecp_cmd(&self, cwd: &Path, home: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ecp"));
        clear_git_env(&mut cmd)
            .args(args)
            .current_dir(cwd)
            .env("HOME", home)
            .env("ECP_HOME", self.ecp_home.path())
            .env("ECP_SKIP_BG_REBUILD", "1")
            .env_remove("CLAUDE_CODE_SESSION_ID");
        cmd
    }

    fn ecp(&self, cwd: &Path, home: &Path, args: &[&str]) -> Output {
        self.ecp_cmd(cwd, home, args).output().unwrap()
    }

    fn git(&self, dir: &Path, args: &[&str]) {
        let mut cmd = Command::new("git");
        let ok = clear_git_env(&mut cmd)
            .env("GIT_CEILING_DIRECTORIES", self.root.path())
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?} failed in {}", dir.display());
    }

    fn git_commit_all(&self, dir: &Path) {
        for args in [
            &["init", "-q"][..],
            &["add", "-A"],
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "base",
            ],
        ] {
            self.git(dir, args);
        }
    }

    fn count_named(&self, name: &str) -> usize {
        walkdir::WalkDir::new(self.ecp_home.path())
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_name() == name)
            .count()
    }

    fn graph_bins(&self) -> usize {
        self.count_named("graph.bin")
    }
}

fn write_py(dir: &Path, name: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), "def foo():\n    pass\n").unwrap();
}

fn combined(out: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    )
}

fn assert_refused(out: &Output, what: &str) {
    assert!(
        !out.status.success(),
        "{what} must be refused: {}",
        combined(out)
    );
    assert!(
        combined(out).contains("home directory"),
        "{what}: the refusal names its reason: {}",
        combined(out)
    );
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("found: true"),
        "{what}: a refused query answers nothing: {}",
        combined(out)
    );
}

#[test]
fn test_admin_index_non_git_home_refused_and_builds_nothing() {
    let fx = Fixture::new();
    let out = fx.ecp(
        &fx.home(),
        &fx.home(),
        &["admin", "index", "--force", "--repo", "."],
    );
    assert_refused(&out, "admin index in a non-git home");
    assert_eq!(fx.graph_bins(), 0, "no graph is built for home");
}

#[test]
fn test_find_in_non_git_home_refused_and_builds_nothing() {
    let fx = Fixture::new();
    let out = fx.ecp(&fx.home(), &fx.home(), &["find", "foo"]);
    assert_refused(&out, "find in a non-git home");
    assert_eq!(fx.graph_bins(), 0, "no graph is built for home");
}

#[test]
fn test_admin_index_absolute_ancestor_of_home_refused_and_names_repo_flag() {
    let fx = Fixture::new();
    let project = fx.home().join("proj");
    write_py(&project, "p.py");
    let parent = fx.parent();
    let out = fx.ecp(
        &project,
        &fx.home(),
        &["admin", "index", "--repo", parent.to_str().unwrap()],
    );
    assert_refused(&out, "admin index --repo <ancestor of home>");
    assert!(
        combined(&out).contains("--repo"),
        "the advice covers a path given by --repo: {}",
        combined(&out)
    );
    assert_eq!(fx.graph_bins(), 0, "no graph is built for it");
}

#[test]
fn test_admin_index_git_home_still_indexes() {
    let fx = Fixture::new();
    fx.git_commit_all(&fx.home());
    let out = fx.ecp(&fx.home(), &fx.home(), &["admin", "index", "--repo", "."]);
    assert!(
        out.status.success(),
        "a git repository used as $HOME keeps working: {}",
        combined(&out)
    );
    assert_eq!(fx.graph_bins(), 1, "one graph is published");
}

#[test]
fn test_admin_index_git_repo_holding_sandbox_home_still_indexes() {
    let fx = Fixture::new();
    fx.git_commit_all(&fx.parent());
    let out = fx.ecp(&fx.parent(), &fx.home(), &["admin", "index", "--repo", "."]);
    assert!(
        out.status.success(),
        "a repo that puts a sandbox $HOME inside itself keeps working: {}",
        combined(&out)
    );
    assert_eq!(fx.graph_bins(), 1, "one graph is published");
}

/// An unborn HEAD builds nothing even without the refusal (`git archive` has
/// no tree), so the contract this pins is the refusal and its reason.
#[test]
fn test_admin_index_home_with_unborn_head_refusal_names_reason() {
    let fx = Fixture::new();
    fx.git(&fx.home(), &["init", "-q"]);
    let out = fx.ecp(&fx.home(), &fx.home(), &["admin", "index", "--repo", "."]);
    assert_refused(&out, "admin index in a home whose repo has no commit");
}

#[test]
fn test_admin_index_force_below_unborn_home_refused_before_any_slot() {
    let fx = Fixture::new();
    fx.git(&fx.home(), &["init", "-q"]);
    let notes = fx.home().join("notes");
    write_py(&notes, "n.py");
    let out = fx.ecp(
        &notes,
        &fx.home(),
        &["admin", "index", "--force", "--repo", "."],
    );
    assert_refused(&out, "admin index --force below an unborn home repo");
    assert_eq!(
        fx.count_named("commits"),
        0,
        "the forced rebuild of home's top level creates no slot"
    );
}

#[test]
fn test_admin_index_non_git_home_with_foreign_git_dir_refused() {
    let fx = Fixture::new();
    let other = fx.root.path().join("other");
    write_py(&other, "o.py");
    fx.git_commit_all(&other);
    let out = fx
        .ecp_cmd(&fx.home(), &fx.home(), &["admin", "index", "--repo", "."])
        .env("GIT_DIR", other.join(".git"))
        .output()
        .unwrap();
    assert_refused(
        &out,
        "admin index in a non-git home with GIT_DIR set elsewhere",
    );
    assert_eq!(fx.graph_bins(), 0, "no graph is built for home");
}

#[cfg(unix)]
#[test]
fn test_find_in_home_reached_through_symlinked_home_var_refused() {
    let fx = Fixture::new();
    let link = fx.root.path().join("link");
    std::os::unix::fs::symlink(fx.home(), &link).unwrap();
    let out = fx.ecp(&fx.home(), &link, &["find", "foo"]);
    assert_refused(&out, "find in home when $HOME is a symlink to it");
    assert_eq!(fx.graph_bins(), 0, "no graph is built for home");
}

#[test]
fn test_group_sync_non_git_member_below_home_skipped_not_walked() {
    let fx = Fixture::new();
    let project = fx.home().join("proj");
    write_py(&project, "p.py");
    let built = fx.ecp(&project, &fx.home(), &["admin", "index", "--repo", "."]);
    assert!(built.status.success(), "setup build: {}", combined(&built));
    let reg =
        ecp_core::registry::RegistryFile::read_or_empty(&fx.ecp_home.path().join("registry.json"))
            .unwrap();
    let member = reg
        .repos
        .keys()
        .find(|k| k.starts_with("proj"))
        .cloned()
        .unwrap_or_else(|| panic!("proj not in registry: {:?}", reg.repos.keys()));
    let added = fx.ecp(
        &project,
        &fx.home(),
        &["admin", "group", "add", &member, "g"],
    );
    assert!(added.status.success(), "group add: {}", combined(&added));

    let synced = fx.ecp(&project, &fx.home(), &["group", "sync", "g"]);
    assert!(synced.status.success(), "group sync: {}", combined(&synced));
    let gdir = ecp_cli::commands::group::storage::group_dir(fx.ecp_home.path(), "g").unwrap();
    let meta: serde_json::Value =
        serde_json::from_slice(&std::fs::read(gdir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(
        meta["missing_repos"],
        serde_json::json!([member]),
        "the member whose walk root is home is skipped: {meta}"
    );
}

#[test]
fn test_find_in_home_with_graph_from_earlier_release_refused() {
    let fx = Fixture::new();
    // An earlier release indexed this directory; HOME pointed elsewhere then,
    // so today's binary builds it the same way.
    let elsewhere = fx.root.path().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let built = fx.ecp(&fx.home(), &elsewhere, &["admin", "index", "--repo", "."]);
    assert!(built.status.success(), "setup build: {}", combined(&built));
    assert_eq!(fx.graph_bins(), 1, "setup publishes one graph");
    write_py(&fx.home().join("new"), "b.py");

    let out = fx.ecp(&fx.home(), &fx.home(), &["find", "foo"]);
    assert_refused(&out, "find in a home that already has a graph");
    assert_eq!(fx.graph_bins(), 1, "no second graph is built");
}

#[test]
fn test_admin_index_dump_resolver_in_home_refused_and_writes_nothing() {
    let fx = Fixture::new();
    let dump = fx.root.path().join("dump.jsonl");
    let out = fx.ecp(
        &fx.home(),
        &fx.home(),
        &[
            "admin",
            "index",
            "--repo",
            ".",
            "--dump-resolver",
            dump.to_str().unwrap(),
        ],
    );
    assert_refused(&out, "admin index --dump-resolver in home");
    assert!(!dump.exists(), "the dump pass never ran");
}

#[test]
fn test_find_in_non_git_project_below_home_still_indexes() {
    let fx = Fixture::new();
    let project = fx.home().join("proj");
    write_py(&project, "p.py");
    let out = fx.ecp(&project, &fx.home(), &["find", "foo"]);
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("found: true"),
        "an ad-hoc non-git project below home keeps working: {}",
        combined(&out)
    );
}

#[test]
fn test_find_in_git_project_below_home_still_indexes() {
    let fx = Fixture::new();
    let project = fx.home().join("repo");
    write_py(&project, "p.py");
    fx.git_commit_all(&project);
    let out = fx.ecp(&project, &fx.home(), &["find", "foo"]);
    assert!(
        out.status.success() && String::from_utf8_lossy(&out.stdout).contains("found: true"),
        "a git project below home keeps working: {}",
        combined(&out)
    );
}
