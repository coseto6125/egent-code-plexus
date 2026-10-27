//! `ecp inspect` resolves its target as `impact` does (C1 "Target
//! request syntax", "Typed target result"): `--kind` and `--file` select the
//! declaration, an explicit `--kind` wins over the `Impl`-beside-type rule
//! (C1 Open point 12), and an ambiguity lists candidates with re-runnable
//! targets, no inspect blocks, and the graph caveat.

mod common;

use common::{commit_all, ecp_bin, run_git};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;
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
        Command::new(ecp_bin())
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env_remove("ECP_HOME")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env("ECP_SESSION_ID", "inspect-target")
            .env("ECP_SKIP_BG_REBUILD", "1")
            .output()
            .expect("ecp failed to spawn")
    }

    /// `ecp inspect <args> --format json`; the command must succeed.
    fn inspect(&self, args: &[&str]) -> Value {
        let mut all = vec!["inspect"];
        all.extend(args);
        all.extend(["--repo", ".", "--format", "json"]);
        let out = self.ecp(&all);
        assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        serde_json::from_str(&stdout(&out))
            .unwrap_or_else(|e| panic!("{args:?}: non-JSON output ({e}): {}", stdout(&out)))
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `(filePath, line, target)` of each candidate, sorted.
fn candidate_facts(v: &Value) -> Vec<(String, u64, Option<String>)> {
    let mut out: Vec<(String, u64, Option<String>)> = v["candidates"]
        .as_array()
        .unwrap_or_else(|| panic!("no candidates: {v}"))
        .iter()
        .map(|c| {
            (
                c["filePath"].as_str().unwrap().to_string(),
                c["line"].as_u64().unwrap(),
                c["target"].as_str().map(str::to_string),
            )
        })
        .collect();
    out.sort();
    out
}

const GADGET: &str =
    "pub struct Gadget {\n    pub n: u32,\n}\n\nimpl Gadget {\n    pub fn new() -> Self { Gadget { n: 0 } }\n}\n";

/// Stands for: C1 Open point 12. Without `--kind` the `Impl` beside the struct
/// is omitted and counted; `--kind impl` selects the `Impl` node itself.
#[test]
fn test_inspect_explicit_kind_impl_keeps_impl_node() {
    let repo = Repo::indexed(&[("src/lib.rs", GADGET)]);

    let bare = repo.inspect(&["Gadget"]);
    assert_eq!(bare["status"], "found", "{bare}");
    assert_eq!(bare["symbol"]["kind"], "Struct", "{bare}");
    assert_eq!(bare["omitted_kinds"]["Impl"], 1, "{bare}");

    let imp = repo.inspect(&["Gadget", "--kind", "impl"]);
    assert_eq!(imp["status"], "found", "{imp}");
    assert_eq!(imp["symbol"]["kind"], "Impl", "{imp}");
    assert_eq!(imp["symbol"]["startLine"], 5, "{imp}");
    assert!(imp["omitted_kinds"].get("Impl").is_none(), "{imp}");
}

const TWIN_A: &str = "export function twin() { return 1; }\n";
const TWIN_B: &str = "export function twin() { return 2; }\n";

/// Stands for: two namesakes on the same line in two files are told apart by
/// `--file`. Each candidate's `target`, run again, selects that declaration
/// alone. Failed before: the text-only target was null for both.
#[test]
fn test_inspect_same_line_namesakes_targets_rerun_to_one_declaration() {
    let repo = Repo::indexed(&[("a/x.ts", TWIN_A), ("b/x.ts", TWIN_B)]);

    let v = repo.inspect(&["twin"]);
    assert_eq!(v["status"], "ambiguous", "{v}");
    let facts = candidate_facts(&v);
    assert_eq!(
        facts
            .iter()
            .map(|(f, l, _)| (f.as_str(), *l))
            .collect::<Vec<_>>(),
        [("a/x.ts", 1), ("b/x.ts", 1)],
        "{v}"
    );
    for (file, line, target) in &facts {
        let target = target
            .as_deref()
            .unwrap_or_else(|| panic!("{file} has no target: {v}"));
        assert!(target.contains(&format!("--file {file}")), "{target}");
        let again = repo.inspect(&target.split_whitespace().collect::<Vec<_>>());
        assert_eq!(again["status"], "found", "{target}: {again}");
        assert_eq!(again["symbol"]["filePath"], file.as_str(), "{target}");
        assert_eq!(again["symbol"]["startLine"], *line, "{target}");
    }
}

/// Stands for: a filter that still admits two declarations is no reason to
/// pick one. A blank filter counts as absent.
#[test]
fn test_inspect_filter_leaving_two_candidates_stays_ambiguous() {
    let repo = Repo::indexed(&[("a/x.ts", TWIN_A), ("b/x.ts", TWIN_B)]);

    for args in [
        &["twin", "--kind", "function"][..],
        &["twin", "--file", "x.ts"],
        &["twin", "--kind", "  ", "--file", " "],
    ] {
        let v = repo.inspect(args);
        assert_eq!(v["status"], "ambiguous", "{args:?}: {v}");
        assert_eq!(v["total"], 2, "{args:?}: {v}");
        assert_eq!(v["candidates"].as_array().unwrap().len(), 2, "{args:?}");
        assert!(v.get("incoming").is_none(), "{args:?}: {v}");
    }
}

/// Stands for: a filter that excludes every declaration is a miss, and the
/// message says how many declarations the name has.
#[test]
fn test_inspect_filter_excluding_every_candidate_not_found() {
    let repo = Repo::indexed(&[("a/x.ts", TWIN_A), ("b/x.ts", TWIN_B)]);

    for args in [
        &["twin", "--file", "nowhere.ts"][..],
        &["twin", "--kind", "class"],
    ] {
        let v = repo.inspect(args);
        assert_eq!(v["status"], "error", "{args:?}: {v}");
        let message = v["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("not found") && message.contains("2 declaration(s) named 'twin'"),
            "{args:?}: {v}"
        );
    }
}

/// Stands for: `--kind` takes one known kind. An unknown kind and a list are
/// errors, and a list names `--neighbor-kind`, the flag that filters
/// neighbours by kind.
#[test]
fn test_inspect_unknown_or_list_kind_errors() {
    let repo = Repo::indexed(&[("a/x.ts", TWIN_A)]);

    let unknown = repo.ecp(&["inspect", "twin", "--kind", "banana", "--repo", "."]);
    assert!(!unknown.status.success(), "{}", stdout(&unknown));
    assert!(stderr(&unknown).contains("banana"), "{}", stderr(&unknown));

    let list = repo.ecp(&[
        "inspect",
        "twin",
        "--kind",
        "function,method",
        "--repo",
        ".",
    ]);
    assert!(!list.status.success(), "{}", stdout(&list));
    assert!(
        stderr(&list).contains("--neighbor-kind function,method"),
        "{}",
        stderr(&list)
    );
}

/// Two overloads of one owner on one line: no target text or filter tells
/// them apart.
const K_ONE_LINE: &str = "class K {\n    void load(int n) {} void load(String s) {}\n}\n";

/// Stands for: `target` is null only when no supported syntax and filter can
/// separate the declaration (C1 "Reusable target"). Both rows still report
/// their file and line.
#[test]
fn test_inspect_indistinguishable_overloads_target_null() {
    let repo = Repo::indexed(&[("K.java", K_ONE_LINE)]);

    let v = repo.inspect(&["load"]);
    assert_eq!(v["status"], "ambiguous", "{v}");
    assert_eq!(v["total"], 2, "{v}");
    assert_eq!(
        candidate_facts(&v),
        [
            ("K.java".to_string(), 2, None),
            ("K.java".to_string(), 2, None)
        ],
        "{v}"
    );
}

const K_OVERLOADS: &str =
    "class K {\n    void load(int n) {}\n    void load(String s) {}\n    void run() { load(1); }\n}\n";

/// Stands for: an ambiguity under a dirty overload set is no proof of a
/// complete answer either, so JSON and the default output both keep the
/// ambiguity status and the overlay caveat.
#[test]
fn test_inspect_ambiguous_dirty_overloads_json_and_default_keep_caveat() {
    let repo = Repo::indexed(&[("K.java", K_OVERLOADS)]);
    repo.write("K.java", &format!("{K_OVERLOADS}// comment-only edit\n"));

    let json = repo.inspect(&["load"]);
    assert_eq!(json["status"], "ambiguous", "{json}");
    let caveat = json["result"].as_str().unwrap_or_default();
    assert!(caveat.contains("could not pair"), "{json}");

    let out = repo.ecp(&["inspect", "load", "--repo", "."]);
    assert!(out.status.success(), "{}", stderr(&out));
    let text = stdout(&out);
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    assert!(lines.contains(&"status: ambiguous"), "{text}");
    assert!(text.contains("candidates"), "{text}");
    assert!(text.contains("could not pair"), "{text}");
}
