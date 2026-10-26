//! A dirty resolution config with no dirty source file (only `go.mod` or only
//! `Cargo.toml` edited). No source refresh runs, so no manifest was written,
//! no closure key existed, and the view was never built: the answer served
//! the indexed reading with no caveat. The refresh now leaves an empty
//! manifest for a dirty config, the closure is stored under its key, and the
//! view builds for a config-only dirty state.
//!
//! Each test runs the real `ecp` binary against an indexed git repo with its
//! own cache root; the edit stays uncommitted, as an agent leaves it.

use serde_json::Value;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

const GO_MOD_OLD: &str = "module example.com/old\n\ngo 1.21\n";
const GO_MOD_NEW: &str = "module example.com/new\n\ngo 1.21\n";
const UTIL_GO: &str = "package util\n\nfunc Help() int {\n\treturn 1\n}\n";
const OTHER_GO: &str = "package other\n\nfunc Help() int {\n\treturn 2\n}\n";
const MAIN_GO: &str =
    "package main\n\nimport \"example.com/old/util\"\n\nfunc main() {\n\tutil.Help()\n}\n";
const TOOL_GO: &str =
    "package main\n\nimport \"example.com/new/util\"\n\nfunc run() {\n\tutil.Help()\n}\n";
const TOOL2_GO: &str =
    "package main\n\nimport \"example.com/new/util\"\n\nfunc run2() {\n\tutil.Help()\n}\n";

const CARGO_OLD: &str = "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
const CARGO_NEW: &str = "[package]\nname = \"demo\"\nversion = \"0.2.0\"\nedition = \"2021\"\n";
const LIB_RS: &str = "pub mod a;\n\npub fn run() -> i32 {\n    a::helper()\n}\n";
const A_RS: &str = "pub fn helper() -> i32 {\n    1\n}\n";

/// An indexed git repo with its own cache root.
struct Repo {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
    session_id: &'static str,
}

impl Repo {
    fn new(session_id: &'static str, files: &[(&str, &str)]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        fs::create_dir_all(&home).unwrap();
        for (rel, text) in files {
            let path = repo.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let fixture = Self {
            _tmp: tmp,
            repo,
            home,
            session_id,
        };
        fixture.git(&["init", "-q", "-b", "main"]);
        fixture.git(&["config", "user.email", "test@example.invalid"]);
        fixture.git(&["config", "user.name", "test"]);
        fixture.git(&["add", "-A"]);
        fixture.git(&["commit", "-qm", "base"]);
        let index = fixture
            .ecp_command()
            .args(["admin", "index", "--repo", fixture.repo.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            index.status.success(),
            "admin index: {}",
            String::from_utf8_lossy(&index.stderr)
        );
        fixture
    }

    fn git(&self, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    }

    /// `ecp` run in the repo, with the cache root and the session pinned to
    /// the fixture, as an agent host runs it.
    fn ecp_command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ecp"));
        command
            .current_dir(&self.repo)
            .env("HOME", &self.home)
            .env_remove("ECP_HOME")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env("ECP_SESSION_ID", self.session_id)
            .env("ECP_SKIP_BG_REBUILD", "1");
        command
    }

    fn find(&self, name: &str) -> Value {
        let out = self
            .ecp_command()
            .args(["find", name, "--format", "json"])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "ecp find {name}: {}\n{stdout}",
            String::from_utf8_lossy(&out.stderr)
        );
        let start = stdout
            .find('{')
            .unwrap_or_else(|| panic!("no JSON in stdout: {stdout}"));
        serde_json::from_str(&stdout[start..]).unwrap()
    }

    fn write(&self, rel: &str, text: &str) {
        fs::write(self.repo.join(rel), text).unwrap();
    }

    fn index(&self) {
        let out = self
            .ecp_command()
            .args(["admin", "index", "--repo", self.repo.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "admin index: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The fixture's session dir, `<home>/.ecp/<repo dir>/sessions/<id>`;
    /// `None` before a query made it.
    fn session_dir(&self) -> Option<PathBuf> {
        fs::read_dir(self.home.join(".ecp"))
            .ok()?
            .filter_map(Result::ok)
            .map(|entry| entry.path().join("sessions").join(self.session_id))
            .find(|dir| dir.is_dir())
    }
}

/// The empty manifest and the stored closure of a config-only edit.
fn l1_files(session: &std::path::Path) -> [PathBuf; 2] {
    [
        session.join("l1").join("dirty_events.json"),
        session.join("l1").join("closure.bin"),
    ]
}

fn result_of(payload: &Value) -> String {
    payload["result"].as_str().unwrap_or_default().to_string()
}

/// The `caller_count` of the match of `find` in `file`.
fn callers_in(payload: &Value, file: &str) -> u64 {
    payload["matches"]
        .as_array()
        .and_then(|matches| matches.iter().find(|m| m["file"].as_str() == Some(file)))
        .and_then(|m| m["caller_count"].as_u64())
        .unwrap_or_else(|| panic!("no match in {file}: {payload}"))
}

fn go_files(go_mod: &str) -> [(&str, &str); 6] {
    [
        ("go.mod", go_mod),
        ("util/util.go", UTIL_GO),
        ("other/other.go", OTHER_GO),
        ("main.go", MAIN_GO),
        ("cmd/tool/tool.go", TOOL_GO),
        ("cmd/tool2/tool2.go", TOOL2_GO),
    ]
}

/// Stands for: after an edit to `go.mod` alone, a clean importer of the new
/// module path binds as a fresh index binds it. The proof edge is the `Calls`
/// edge `run -> util.Help`, read as `caller_count` of `util/util.go`'s
/// `Help`: the old path's importer `main.go` is the one caller before, the
/// two importers of the new path are the callers after. A repo freshly
/// indexed with the new `go.mod` gives the expected count; reverting the file
/// gives the first answer back, with no caveat.
#[test]
fn test_find_go_mod_only_edit_clean_importer_of_new_path_binds() {
    let fx = Repo::new("cfg-only-go-mod", &go_files(GO_MOD_OLD));
    let fresh = Repo::new("cfg-only-go-mod-fresh", &go_files(GO_MOD_NEW));
    let before = callers_in(&fx.find("Help"), "util/util.go");
    let expected = callers_in(&fresh.find("Help"), "util/util.go");
    assert_eq!(
        (before, expected),
        (1, 2),
        "setup: the old path binds one caller, the new path two"
    );

    fx.write("go.mod", GO_MOD_NEW);
    let first = fx.find("Help");
    let second = fx.find("Help");
    assert_eq!(callers_in(&first, "util/util.go"), expected, "{first}");
    assert_eq!(second, first, "a view-cache hit must give the same answer");

    fx.write("go.mod", GO_MOD_OLD);
    let reverted = fx.find("Help");
    assert_eq!(callers_in(&reverted, "util/util.go"), before, "{reverted}");
    assert_eq!(result_of(&reverted), "", "no dirty state, no caveat");
}

/// Stands for: `Cargo.toml` is kept as indexed (its rows come from the Rust
/// module tree), so after a `Cargo.toml`-only edit the answer says the clean
/// files keep their indexed edges, naming the config. The marker is the
/// `(kept configs: Cargo.toml)` text of the payload's `result`; no source is
/// dirty, so only the config-only view can carry it.
#[test]
fn test_find_cargo_toml_only_edit_result_names_kept_config() {
    let fx = Repo::new(
        "cfg-only-cargo",
        &[
            ("Cargo.toml", CARGO_OLD),
            ("src/lib.rs", LIB_RS),
            ("src/a.rs", A_RS),
        ],
    );
    assert_eq!(result_of(&fx.find("helper")), "", "setup: a clean tree");

    fx.write("Cargo.toml", CARGO_NEW);
    let first = fx.find("helper");
    let second = fx.find("helper");
    let result = result_of(&first);
    assert!(
        result.contains("kept configs: Cargo.toml"),
        "no kept-config caveat: {first}"
    );
    assert!(result.contains("Calls"), "names the kinds: {result:?}");
    assert_eq!(result_of(&second), result, "a cache hit says the same");
    assert_eq!(
        callers_in(&first, "src/a.rs"),
        1,
        "indexed edge kept: {first}"
    );
}

/// Stands for: once the `Cargo.toml` edit is committed and HEAD is indexed,
/// the tree is clean, so the answer carries no kept-config caveat, and the
/// empty manifest and stored closure the edit left are gone (a clean tree
/// reads neither again). The marker is the `(kept configs: Cargo.toml)` text
/// of `result`, present while the edit is dirty. `admin index` stands for
/// the background rebuild a commit starts (`ECP_SKIP_BG_REBUILD` holds it
/// off here); without it the query warm-attaches the parent's graph.
#[test]
fn test_find_cargo_toml_only_edit_committed_result_has_no_caveat() {
    let fx = Repo::new(
        "cfg-only-cargo-commit",
        &[
            ("Cargo.toml", CARGO_OLD),
            ("src/lib.rs", LIB_RS),
            ("src/a.rs", A_RS),
        ],
    );
    fx.write("Cargo.toml", CARGO_NEW);
    let dirty = result_of(&fx.find("helper"));
    assert!(
        dirty.contains("kept configs: Cargo.toml"),
        "setup: the caveat shows while the edit is dirty: {dirty:?}"
    );
    let session = fx.session_dir().expect("setup: the query made a session");
    for file in l1_files(&session) {
        assert!(file.exists(), "setup: {} written", file.display());
    }

    fx.git(&["commit", "-qam", "bump"]);
    fx.index();
    let clean = fx.find("helper");
    let again = fx.find("helper");

    assert_eq!(result_of(&clean), "", "clean tree, no caveat: {clean}");
    assert_eq!(result_of(&again), "", "still none: {again}");
    assert_eq!(callers_in(&clean, "src/a.rs"), 1, "{clean}");
    for file in l1_files(&session) {
        assert!(!file.exists(), "{} left behind", file.display());
    }
}
