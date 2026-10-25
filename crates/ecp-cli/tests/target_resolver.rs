//! One target resolver (architecture direction, area 5; C1 "Target request
//! syntax" and "Typed target result"): `impact`, `inspect` and `rename` turn
//! the target text and its filters into one declaration or a typed ambiguity.
//! A command that writes files refuses an ambiguity and a warm-attached graph,
//! and `rename` edits only the occurrences the graph binds to the chosen
//! declaration.

mod common;

use common::{commit_all, ecp_bin, run_git};
use ecp_cli::read_context::{resolve_target, target_request};
use ecp_core::contracts::identity::TargetResolution;
use ecp_core::graph::ArchivedZeroCopyGraph;
use ecp_core::graph_fixture::GraphFixture;
use ecp_core::session::MergedGraph;
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

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap()
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
            .env("ECP_SESSION_ID", "target-resolver")
            .env("ECP_SKIP_BG_REBUILD", "1")
            .output()
            .expect("ecp failed to spawn")
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

const PARSE_A: &str = "def parse():\n    return 1\n";
const PARSE_B: &str = "def parse():\n    return 2\n";

/// Stands for: two declarations match a bare name, so `rename` cannot know
/// which one the agent means. It refuses, names both, gives each a target
/// that selects it alone, and writes nothing. Failed before: `rename`
/// rewrote both files.
#[test]
fn test_rename_two_parse_functions_refuses_ambiguity() {
    let repo = Repo::indexed(&[("a.py", PARSE_A), ("b.py", PARSE_B)]);

    let out = repo.ecp(&["rename", "parse", "parse_config"]);

    assert!(!out.status.success(), "an ambiguity must exit non-zero");
    let err = stderr(&out);
    assert!(err.contains("ambiguous"), "{err}");
    let payload = stdout(&out);
    for file in ["a.py", "b.py"] {
        assert!(err.contains(file), "stderr must name {file}: {err}");
        assert!(
            payload.contains(&format!("--file {file}")),
            "stdout must give a target that selects {file}: {payload}"
        );
    }
    assert_eq!(repo.read("a.py"), PARSE_A, "a.py must stay unchanged");
    assert_eq!(repo.read("b.py"), PARSE_B, "b.py must stay unchanged");
}

const K_OVERLOADS: &str = "class K {\n    void load(int n) {}\n    void load(String s) {}\n    void run() { load(1); }\n}\n";

/// Stands for: an overload set is two declarations, not one target seeded at
/// its first member (FU-2026-10-25-faf69063bbb2). `impact` answers with the
/// typed ambiguity: both members, each with a target that, run again,
/// selects that member alone. Failed before: one blast radius for the set.
#[test]
fn test_impact_target_load_kind_method_reports_ambiguity_not_merge() {
    let repo = Repo::indexed(&[("K.java", K_OVERLOADS)]);

    let out = repo.ecp(&[
        "impact", "load", "--kind", "method", "--repo", ".", "--format", "json",
    ]);

    assert!(!out.status.success(), "an ambiguity must exit non-zero");
    let payload = json_of(&out);
    assert_eq!(payload["status"], "ambiguous", "{payload}");
    assert!(payload.get("impact").is_none(), "no merged rows: {payload}");
    let candidates = payload["candidates"].as_array().expect("candidates");
    let mut lines: Vec<u64> = candidates
        .iter()
        .map(|c| c["line"].as_u64().unwrap())
        .collect();
    lines.sort();
    assert_eq!(lines, [2, 3], "{payload}");

    for candidate in candidates {
        let target = candidate["target"]
            .as_str()
            .unwrap_or_else(|| panic!("each overload has its own target: {payload}"));
        let mut args = vec!["impact"];
        args.extend(target.split_whitespace());
        args.extend(["--repo", ".", "--format", "json"]);
        let again = repo.ecp(&args);
        assert!(again.status.success(), "{target}: {}", stderr(&again));
        let start = &json_of(&again)["impact"][0];
        assert_eq!(start["depth"], 0, "{target}: {start}");
        assert_eq!(start["line"], candidate["line"], "{target}: {start}");
    }
}

/// Stands for: an ambiguity under a dirty overload set is no proof that the
/// answer is complete either, so the ambiguity payload keeps the overlay
/// caveat in `result`.
#[test]
fn test_impact_ambiguous_dirty_overloads_payload_carries_caveat() {
    let repo = Repo::indexed(&[("K.java", K_OVERLOADS)]);
    repo.write("K.java", &format!("{K_OVERLOADS}// comment-only edit\n"));

    let out = repo.ecp(&["impact", "load", "--repo", ".", "--format", "json"]);

    let payload = json_of(&out);
    assert_eq!(payload["status"], "ambiguous", "{payload}");
    let result = payload["result"].as_str().unwrap_or_default();
    assert!(
        result.contains("could not pair") && result.contains("K.load"),
        "{payload}"
    );
}

const RUN_PY: &str = "def run():\n    return 1\n\n\ndef main():\n    return run()\n\n\ndef other():\n    run = 5\n    return run\n";
const RUN_PY_RENAMED: &str = "def launch():\n    return 1\n\n\ndef main():\n    return launch()\n\n\ndef other():\n    run = 5\n    return run\n";

/// Stands for: `rename` edits the occurrences the graph binds to the chosen
/// declaration (its own name and its callers), not every identifier that
/// spells the name. The local `run` in `other` is another binding. Failed
/// before: the second stage rewrote every `run` in the file. `--leave-unbound`
/// waives the refusal that the unbound local would otherwise cause.
#[test]
fn test_rename_unique_target_skips_unrelated_same_name_identifier() {
    let repo = Repo::indexed(&[("lib.py", RUN_PY)]);

    let out = repo.ecp(&["rename", "run", "launch", "--leave-unbound"]);

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(repo.read("lib.py"), RUN_PY_RENAMED);
    assert!(
        stdout(&out).contains("unbound"),
        "the payload must list the occurrences it left: {}",
        stdout(&out)
    );
}

/// `::name` selects the declaration with no owner and no lexical parent; a
/// bare name matches both; an owner form selects the method.
#[test]
fn test_resolve_target_colon_colon_name_selects_top_level() {
    let mut fx = GraphFixture::new();
    fx.method("src/engine.py", "Engine", "load");
    fx.func("src/util.py", "load");
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&fx.build()).unwrap();
    let graph = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    let merged = MergedGraph::new(graph, None);
    let file_of =
        |text: &str| match resolve_target(&merged, &target_request(text, None, None).unwrap()) {
            TargetResolution::Unique(c) => c.file,
            other => panic!("{text}: expected one declaration, got {other:?}"),
        };

    assert_eq!(file_of("::load"), "src/util.py");
    assert_eq!(file_of(".load"), "src/util.py");
    assert_eq!(file_of("Engine.load"), "src/engine.py");
    match resolve_target(&merged, &target_request("load", None, None).unwrap()) {
        TargetResolution::Ambiguous(a) => assert_eq!(a.total(), 2),
        other => panic!("bare load: expected an ambiguity, got {other:?}"),
    }
    match resolve_target(&merged, &target_request("::missing", None, None).unwrap()) {
        TargetResolution::NotFound { named } => assert_eq!(named, 0),
        other => panic!("::missing: expected not found, got {other:?}"),
    }
}

/// Blank filters count as absent; an unknown kind and an empty target are
/// syntax errors, never a silent miss.
#[test]
fn test_target_request_blank_filter_absent_bad_kind_error() {
    let request = target_request("load", Some("  "), Some(" ")).unwrap();
    assert_eq!(request.kind(), None);
    assert_eq!(request.file(), None);
    assert!(target_request("load", Some("function,method"), None).is_err());
    assert!(target_request("   ", None, None).is_err());
}

/// Stands for: a warm-attached graph is a sibling commit's, so a rename that
/// writes files from it can miss or misplace occurrences. It refuses and names
/// the reindex command, and writes nothing. The dry run still answers, with
/// the caveat (`caveat_payload.rs`).
#[test]
fn test_rename_warm_attached_graph_refuses() {
    let repo = Repo::indexed(&[("lib.rs", "pub fn alpha() {}\n")]);
    repo.write("extra.rs", "pub fn beta() {}\n");
    commit_all(&repo.root, "advance");

    let out = repo.ecp(&["rename", "alpha", "alpha2"]);

    assert!(!out.status.success(), "a warm-attached rename must refuse");
    let err = stderr(&out);
    assert!(
        err.contains("warm-attach") && err.contains("ecp admin index --force --repo ."),
        "{err}"
    );
    assert_eq!(repo.read("lib.rs"), "pub fn alpha() {}\n");
}

/// Stands for: graph paths are relative to the worktree top, so `rename` run
/// from a subdirectory edits the files the graph names. Failed before: it
/// joined them onto the cwd, read nothing and edited nothing.
#[test]
fn test_rename_from_subdirectory_edits_files_under_worktree_top() {
    let repo = Repo::indexed(&[("pkg/lib.py", RUN_PY)]);

    let out = repo.ecp_in(
        &repo.root.join("pkg"),
        &["rename", "run", "launch", "--leave-unbound"],
    );

    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(repo.read("pkg/lib.py"), RUN_PY_RENAMED, "{}", stdout(&out));
}

// ── find, path and pattern select through the same resolver (F5b) ───────────

const ENGINE_PY: &str = "class Engine:\n    def load(self):\n        return 1\n";
const UTIL_LOAD_PY: &str = "def load():\n    return 2\n";

/// Stands for: qualified `Owner.member` text means the node the resolver
/// picks for it. Failed before: `find` compared the whole text with the node
/// name `load`, so `Engine.load` answered `found:false`.
#[test]
fn test_find_engine_dot_load_found_true() {
    let repo = Repo::indexed(&[("engine.py", ENGINE_PY), ("util.py", UTIL_LOAD_PY)]);

    let out = repo.ecp(&["find", "Engine.load", "--format", "json", "--repo", "."]);

    assert!(out.status.success(), "{}", stderr(&out));
    let payload = json_of(&out);
    assert_eq!(payload["found"], true, "{payload}");
    assert_eq!(payload["status"], "success", "one declaration: {payload}");
    assert_eq!(payload["total_candidates"], 1, "{payload}");
    assert_eq!(payload["matches"][0]["file"], "engine.py", "{payload}");
    assert_eq!(payload["matches"][0]["name"], "load", "{payload}");
}

/// Stands for: `find` reports two same-name declarations as the typed
/// ambiguity (`status`, `candidates` with a re-runnable `target`) and still
/// returns its top-ranked match. Failed before: the payload said `success`
/// and gave no way to select the other declaration.
#[test]
fn test_find_bare_name_two_files_reports_typed_ambiguity() {
    let repo = Repo::indexed(&[("a.py", PARSE_A), ("b.py", PARSE_B)]);

    let out = repo.ecp(&["find", "parse", "--format", "json", "--repo", "."]);

    assert!(out.status.success(), "{}", stderr(&out));
    let payload = json_of(&out);
    assert_eq!(payload["status"], "ambiguous", "{payload}");
    assert_eq!(payload["total_candidates"], 2, "{payload}");
    assert_eq!(payload["matches"].as_array().unwrap().len(), 1, "{payload}");
    let candidates = payload["candidates"].as_array().expect("candidates");
    for file in ["a.py", "b.py"] {
        let candidate = candidates
            .iter()
            .find(|c| c["filePath"] == file)
            .unwrap_or_else(|| panic!("no candidate for {file}: {payload}"));
        let target = candidate["target"].as_str().expect("re-runnable target");
        let mut args = vec!["find"];
        args.extend(target.split_whitespace());
        args.extend(["--format", "json", "--repo", "."]);
        let again = json_of(&repo.ecp(&args));
        assert_eq!(again["status"], "success", "{target}: {again}");
        assert_eq!(again["matches"][0]["file"], file, "{target}: {again}");
    }
}

/// Stands for: a `--kind` list narrows `find` before it reports an ambiguity.
/// A class and a function share a name; `--kind function` leaves one.
#[test]
fn test_find_kind_filter_leaves_one_declaration_no_ambiguity() {
    let repo = Repo::indexed(&[("a.py", "class parse:\n    pass\n"), ("b.py", PARSE_B)]);

    let out = repo.ecp(&[
        "find", "parse", "--kind", "function", "--format", "json", "--repo", ".",
    ]);

    let payload = json_of(&out);
    assert_eq!(payload["status"], "success", "{payload}");
    assert_eq!(payload["matches"][0]["file"], "b.py", "{payload}");
    assert!(payload.get("candidates").is_none(), "{payload}");
}

/// Stands for: `--callers-of` takes its caller files from the overlay-aware
/// graph. Failed before: the walk read the base graph, so a caller written
/// into an untracked file was never scanned.
#[test]
fn test_pattern_callers_of_sees_untracked_caller() {
    let repo = Repo::indexed(&[("lib.rs", "pub fn alpha() {}\n")]);
    repo.write("extra.rs", "pub fn use_alpha() { alpha(); }\n");

    let out = repo.ecp(&[
        "pattern",
        "-p",
        "alpha()",
        "--callers-of",
        "alpha",
        "--format",
        "json",
        "--repo",
        ".",
    ]);

    assert!(out.status.success(), "{}", stderr(&out));
    let payload = json_of(&out);
    let files: Vec<&str> = payload["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["file"].as_str().unwrap())
        .collect();
    assert!(files.contains(&"extra.rs"), "{payload}");
}

/// Stands for: `pattern --callers-of` refuses two same-name declarations with
/// the typed ambiguity, and each candidate's `target` holds the arguments that
/// select it. Failed before: it scanned the callers of both.
#[test]
fn test_pattern_callers_of_bare_name_two_files_reports_typed_ambiguity() {
    let repo = Repo::indexed(&[("a.py", PARSE_A), ("b.py", PARSE_B)]);

    let out = repo.ecp(&[
        "pattern",
        "-p",
        "pass",
        "--callers-of",
        "parse",
        "--format",
        "json",
        "--repo",
        ".",
    ]);

    assert!(!out.status.success(), "an ambiguity must exit non-zero");
    let payload = json_of(&out);
    assert_eq!(payload["status"], "ambiguous", "{payload}");
    assert!(payload.get("matches").is_none(), "{payload}");
    for file in ["a.py", "b.py"] {
        let candidate = payload["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["filePath"] == file)
            .unwrap_or_else(|| panic!("no candidate for {file}: {payload}"));
        let target = candidate["target"].as_str().expect("re-runnable target");
        let mut args = vec!["pattern", "-p", "pass"];
        args.extend(target.split_whitespace());
        args.extend(["--format", "json", "--repo", "."]);
        let again = repo.ecp(&args);
        assert!(again.status.success(), "{target}: {}", stderr(&again));
        assert_eq!(json_of(&again)["scanned_files"], 1, "{target}");
    }
}

const APP_PY: &str = "from util import load\n\n\ndef run():\n    return load()\n";

/// Stands for: `path` selects an endpoint with the resolver's grammar. `::load`
/// is the top-level `load`, not `Engine.load`. Failed before: `path` read
/// `::load` as a method name and found no symbol.
#[test]
fn test_path_colon_colon_name_selects_top_level_endpoint() {
    let repo = Repo::indexed(&[
        ("engine.py", ENGINE_PY),
        ("util.py", UTIL_LOAD_PY),
        ("app.py", APP_PY),
    ]);

    let out = repo.ecp(&["path", "run", "::load", "--format", "json", "--repo", "."]);

    assert!(out.status.success(), "{}", stderr(&out));
    let payload = json_of(&out);
    assert_eq!(payload["found"], true, "{payload}");
    let last = payload["path"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["filePath"], "util.py", "{payload}");
}

/// Stands for: `path` reports an ambiguous endpoint as the typed ambiguity and
/// names the side. Each `target` holds the `--from-file` / `--to-file`
/// arguments that select it. Failed before: both declarations seeded one walk.
#[test]
fn test_path_bare_name_two_files_reports_typed_ambiguity() {
    let repo = Repo::indexed(&[
        ("a.py", PARSE_A),
        ("b.py", PARSE_B),
        ("main.py", "def main():\n    return 0\n"),
    ]);

    let out = repo.ecp(&["path", "main", "parse", "--format", "json", "--repo", "."]);

    assert!(!out.status.success(), "an ambiguity must exit non-zero");
    let payload = json_of(&out);
    assert_eq!(payload["status"], "ambiguous", "{payload}");
    assert_eq!(payload["side"], "to", "{payload}");
    for file in ["a.py", "b.py"] {
        let candidate = payload["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["filePath"] == file)
            .unwrap_or_else(|| panic!("no candidate for {file}: {payload}"));
        let target = candidate["target"].as_str().expect("re-runnable target");
        assert!(target.contains(&format!("--to-file {file}")), "{target}");
        let mut args = vec!["path", "main"];
        args.extend(target.split_whitespace());
        args.extend(["--format", "json", "--repo", "."]);
        let again = repo.ecp(&args);
        assert!(again.status.success(), "{target}: {}", stderr(&again));
        assert_eq!(json_of(&again)["toCandidates"], 1, "{target}");
    }
}
