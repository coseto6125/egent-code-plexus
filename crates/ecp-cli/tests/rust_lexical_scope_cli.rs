//! Rust nested functions through the `impact` CLI (design A, slice R6): a
//! target names a nested fn by its display owner (`outer.helper`), a root
//! target (`::helper`) names the file-level fn alone, and the blast radius of
//! each lists only its own callers, from the index and from the overlay.

mod common;

use common::{commit_all, ecp_bin, run_git};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// An indexed git repo with its own cache root.
struct Repo {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Repo {
    fn indexed(files: &[(&str, &str)]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let home = tmp.path().join("home");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&home).unwrap();
        let repo = Self {
            _tmp: tmp,
            root,
            home,
        };
        for (rel, text) in files {
            repo.write(rel, text);
        }
        run_git(&repo.root, &["init", "-q", "-b", "main"]);
        commit_all(&repo.root, "base");
        let out = repo.ecp(&["admin", "index", "--repo", "."]);
        assert!(out.status.success(), "admin index: {}", stderr(&out));
        repo
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn ecp(&self, args: &[&str]) -> Output {
        self.ecp_in(&self.root, args)
    }

    fn ecp_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(ecp_bin())
            .args(args)
            .current_dir(cwd)
            .env("HOME", &self.home)
            .env_remove("ECP_HOME")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env("ECP_SESSION_ID", "rust-lexical-scope")
            .env("ECP_SKIP_BG_REBUILD", "1")
            .output()
            .expect("ecp failed to spawn")
    }

    /// `impact <target> --direction upstream` as JSON; `extra` adds flags.
    fn impact(&self, target: &str, extra: &[&str]) -> Output {
        let mut args = vec![
            "impact",
            target,
            "--direction",
            "upstream",
            "--repo",
            ".",
            "--format",
            "json",
        ];
        args.extend(extra);
        self.ecp(&args)
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The first JSON object on stdout.
fn json_of(out: &Output) -> Value {
    let text = stdout(out);
    let start = text
        .find('{')
        .unwrap_or_else(|| panic!("no JSON on stdout: {text}\nstderr: {}", stderr(out)));
    serde_json::Deserializer::from_str(&text[start..])
        .into_iter::<Value>()
        .next()
        .unwrap()
        .unwrap()
}

/// The start row's line and the sorted `(name, line)` of every caller of a
/// resolved `impact`. Panics on an ambiguity or a failure.
fn blast(out: &Output) -> (u64, Vec<(String, u64)>) {
    assert!(out.status.success(), "impact: {}", stderr(out));
    let payload = json_of(out);
    let rows = payload["impact"]
        .as_array()
        .unwrap_or_else(|| panic!("no impact rows: {payload}"));
    let start = rows
        .iter()
        .find(|r| r["depth"] == 0)
        .unwrap_or_else(|| panic!("no start row: {payload}"));
    let mut callers: Vec<(String, u64)> = rows
        .iter()
        .filter(|r| r["depth"].as_u64().unwrap_or(0) > 0)
        .map(|r| {
            (
                r["name"].as_str().unwrap_or_default().to_string(),
                r["line"].as_u64().unwrap_or_default(),
            )
        })
        .collect();
    callers.sort();
    (start["line"].as_u64().unwrap(), callers)
}

/// Lines: 1 file-level `helper` · 5 `top_caller` · 9 `outer` · 10 nested
/// `helper`, called on line 13.
const LIB: &str = "fn helper() -> i32 {\n    1\n}\n\nfn top_caller() -> i32 {\n    helper()\n}\n\nfn outer() -> i32 {\n    fn helper() -> i32 {\n        2\n    }\n    helper()\n}\n";

fn caller(name: &str, line: u64) -> (String, u64) {
    (name.to_string(), line)
}

/// Stands for: `--file` selects the file-level `helper` of `lib.rs`, and its
/// blast radius holds its own caller alone; `outer`, which calls the
/// `helper` nested in another file's `outer`, is no caller of it.
#[test]
fn test_impact_helper_file_filter_file_level_lists_only_own_callers() {
    let nested =
        "fn outer() -> i32 {\n    fn helper() -> i32 {\n        2\n    }\n    helper()\n}\n";
    let top =
        "pub fn helper() -> i32 {\n    1\n}\n\npub fn top_caller() -> i32 {\n    helper()\n}\n";
    let repo = Repo::indexed(&[("src/a.rs", top), ("src/b.rs", nested)]);

    let (line, callers) = blast(&repo.impact("helper", &["--file", "src/a.rs"]));

    assert_eq!(line, 1);
    assert_eq!(callers, vec![caller("top_caller", 5)]);
}

/// Stands for: a target names a nested fn by its display owner, and a root
/// target names the file-level fn; each blast radius lists its own caller
/// alone. The qualifier reads the display owner: with the stored owner
/// empty (I1), `outer.helper` would match nothing.
#[test]
fn test_impact_outer_dot_helper_selects_nested_root_selects_file_level() {
    let repo = Repo::indexed(&[("src/lib.rs", LIB)]);

    assert_eq!(
        blast(&repo.impact("outer.helper", &[])),
        (10, vec![caller("outer", 9)])
    );
    assert_eq!(
        blast(&repo.impact("::helper", &[])),
        (1, vec![caller("top_caller", 5)])
    );
}

/// Stands for: a bare `helper` names two declarations, so `impact` answers
/// the typed ambiguity; each candidate's target, run again, selects that
/// declaration alone.
#[test]
fn test_impact_bare_helper_ambiguous_candidate_targets_select_each() {
    let repo = Repo::indexed(&[("src/lib.rs", LIB)]);

    let out = repo.impact("helper", &[]);

    assert!(!out.status.success(), "an ambiguity must exit non-zero");
    let payload = json_of(&out);
    assert_eq!(payload["status"], "ambiguous", "{payload}");
    let candidates = payload["candidates"].as_array().expect("candidates");
    let mut lines: Vec<u64> = candidates
        .iter()
        .map(|c| c["line"].as_u64().unwrap())
        .collect();
    lines.sort();
    assert_eq!(lines, [1, 10], "{payload}");
    for candidate in candidates {
        let target = candidate["target"]
            .as_str()
            .unwrap_or_else(|| panic!("each candidate has its own target: {payload}"));
        let mut args = vec!["impact"];
        args.extend(target.split_whitespace());
        args.extend(["--direction", "upstream", "--repo", ".", "--format", "json"]);
        let (line, _) = blast(&repo.ecp(&args));
        assert_eq!(Some(line), candidate["line"].as_u64(), "{target}");
    }
}

/// Stands for: a fn in a two-level module is named by its innermost
/// module, and its caller in another file reaches it.
#[test]
fn test_impact_b_dot_f_two_level_module_lists_cross_file_caller() {
    let lib = "mod a {\n    pub mod b {\n        pub fn f() {}\n    }\n}\n";
    let main = "fn g() {\n    f();\n}\n";
    let repo = Repo::indexed(&[("src/lib.rs", lib), ("src/main.rs", main)]);

    assert_eq!(blast(&repo.impact("b.f", &[])), (3, vec![caller("g", 1)]));
}

/// Stands for: a comment-only edit puts `lib.rs` in the overlay, and the
/// overlay binds the nested and the file-level `helper` exactly as the
/// index does. `@line` selects each, so the check does not depend on the
/// overlay's display owner.
#[test]
fn test_impact_overlay_comment_edit_nested_helper_same_callers_as_index() {
    let repo = Repo::indexed(&[("src/lib.rs", LIB)]);
    let indexed = [
        blast(&repo.impact("helper@1", &[])),
        blast(&repo.impact("helper@10", &[])),
    ];
    assert_eq!(indexed[0], (1, vec![caller("top_caller", 5)]));
    assert_eq!(indexed[1], (10, vec![caller("outer", 9)]));

    repo.write("src/lib.rs", &format!("{LIB}// comment-only edit\n"));
    let overlay = [
        blast(&repo.impact("helper@1", &[])),
        blast(&repo.impact("helper@10", &[])),
    ];

    assert_eq!(overlay, indexed);
}

/// Stands for: a nested fn in a dirty file is named by its display owner,
/// as in the index. Blocked: `MergedGraph::display_owner` and the target
/// qualifier read a virtual node's stored owner only, and the overlay view
/// keeps no lexical parent per virtual node (`session/merged.rs`
/// `display_owner`; `session/view.rs` `OverlayView`).
#[test]
#[ignore = "needs a lexical parent per overlay virtual node"]
fn test_impact_overlay_outer_dot_helper_selects_nested() {
    let repo = Repo::indexed(&[("src/lib.rs", LIB)]);
    repo.write("src/lib.rs", &format!("{LIB}// comment-only edit\n"));

    assert_eq!(
        blast(&repo.impact("outer.helper", &[])),
        (10, vec![caller("outer", 9)])
    );
    assert_eq!(
        blast(&repo.impact("::helper", &[])),
        (1, vec![caller("top_caller", 5)])
    );
}

/// Stands for: a session that a previous binary wrote (nested fn stored with
/// the owner `outer`) is not served as is; today's binary reparses the
/// fragment, and the nested fn reads as scoped. Blocked: the 0.16.4 fixture
/// (`tests/fixtures/stored-by-0.16.4/`) holds no nested function.
#[test]
#[ignore = "needs fixture from 0.16.4"]
fn test_overlay_view_session_written_by_previous_binary_reparses_rust_fragment() {
    unimplemented!("needs a 0.16.4 session whose dirty Rust file nests a fn");
}
