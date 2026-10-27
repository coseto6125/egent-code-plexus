//! `ecp inspect` answers from the merged graph (base CSR + OverlayView), the
//! way `find`, `impact` and `cypher` do: a symbol in an uncommitted edit
//! reports its working-tree span and edges, a symbol the edit added is
//! found, and a symbol the edit deleted is not.
//!
//! Oracle: the same `inspect` after `git commit` and a full `admin index`
//! into a fresh HOME, where no overlay exists. Symbol spans and Calls edges
//! are language-agnostic in the merge, so one table runs the edit over the
//! 14 mainstream languages.
//!
//! Not covered here: a corrupt or rejected fragment. `ensure_fresh` re-parses
//! a fragment that fails validation before the query reads it, so the CLI
//! cannot be made to hold one; the unit tests in `commands/inspect.rs` pin
//! that a missing view answers from the base graph.

mod common;

use common::{commit_all, ecp_bin, run_git};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;

const SESSION: &str = "inspect-overlay-test-session";

/// Session-id variables `resolve_session_id` reads, removed so the host
/// agent's own session never leaks into a test.
const SESSION_VARS: [&str; 5] = [
    "ECP_SESSION_ID",
    "GEMINI_CLI_SESSION_ID",
    "CODEX_SESSION_ID",
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
];

/// One language's file, as consecutive line blocks. The committed text is
/// `header callee caller doomed footer`; the dirty text is
/// `header callee <6 blank lines> caller fresh footer`, so `caller` moves
/// down 6 lines, `fresh` is new and `doomed` is gone. `caller` and `fresh`
/// both call `callee`.
struct Lang {
    name: &'static str,
    file: &'static str,
    header: &'static str,
    callee: &'static str,
    caller: &'static str,
    doomed: &'static str,
    fresh: &'static str,
    footer: &'static str,
}

impl Lang {
    fn committed(&self) -> String {
        [
            self.header,
            self.callee,
            self.caller,
            self.doomed,
            self.footer,
        ]
        .concat()
    }

    fn dirty(&self) -> String {
        [
            self.header,
            self.callee,
            "\n\n\n\n\n\n",
            self.caller,
            self.fresh,
            self.footer,
        ]
        .concat()
    }
}

const LANGS: [Lang; 14] = [
    Lang {
        name: "typescript",
        file: "lib.ts",
        header: "",
        callee: "export function callee() {}\n",
        caller: "export function caller() {\n  callee();\n}\n",
        doomed: "export function doomed() {}\n",
        fresh: "export function fresh() {\n  callee();\n}\n",
        footer: "",
    },
    Lang {
        name: "javascript",
        file: "lib.js",
        header: "",
        callee: "function callee() {}\n",
        caller: "function caller() {\n  callee();\n}\n",
        doomed: "function doomed() {}\n",
        fresh: "function fresh() {\n  callee();\n}\n",
        footer: "",
    },
    Lang {
        name: "python",
        file: "lib.py",
        header: "",
        callee: "def callee():\n    pass\n",
        caller: "def caller():\n    callee()\n",
        doomed: "def doomed():\n    pass\n",
        fresh: "def fresh():\n    callee()\n",
        footer: "",
    },
    Lang {
        name: "java",
        file: "Box.java",
        header: "class Box {\n",
        callee: "    void callee() {}\n",
        caller: "    void caller() {\n        callee();\n    }\n",
        doomed: "    void doomed() {}\n",
        fresh: "    void fresh() {\n        callee();\n    }\n",
        footer: "}\n",
    },
    Lang {
        name: "kotlin",
        file: "lib.kt",
        header: "",
        callee: "fun callee() {}\n",
        caller: "fun caller() {\n    callee()\n}\n",
        doomed: "fun doomed() {}\n",
        fresh: "fun fresh() {\n    callee()\n}\n",
        footer: "",
    },
    Lang {
        name: "csharp",
        file: "Box.cs",
        header: "class Box {\n",
        callee: "    void callee() {}\n",
        caller: "    void caller() {\n        callee();\n    }\n",
        doomed: "    void doomed() {}\n",
        fresh: "    void fresh() {\n        callee();\n    }\n",
        footer: "}\n",
    },
    Lang {
        name: "go",
        file: "lib.go",
        header: "package p\n\n",
        callee: "func callee() {}\n",
        caller: "func caller() {\n\tcallee()\n}\n",
        doomed: "func doomed() {}\n",
        fresh: "func fresh() {\n\tcallee()\n}\n",
        footer: "",
    },
    Lang {
        name: "rust",
        file: "lib.rs",
        header: "",
        callee: "pub fn callee() {}\n",
        caller: "pub fn caller() {\n    callee();\n}\n",
        doomed: "pub fn doomed() {}\n",
        fresh: "pub fn fresh() {\n    callee();\n}\n",
        footer: "",
    },
    Lang {
        name: "php",
        file: "lib.php",
        header: "<?php\n",
        callee: "function callee() {}\n",
        caller: "function caller() {\n    callee();\n}\n",
        doomed: "function doomed() {}\n",
        fresh: "function fresh() {\n    callee();\n}\n",
        footer: "",
    },
    Lang {
        name: "ruby",
        file: "lib.rb",
        header: "",
        callee: "def callee\nend\n",
        caller: "def caller\n  callee()\nend\n",
        doomed: "def doomed\nend\n",
        fresh: "def fresh\n  callee()\nend\n",
        footer: "",
    },
    Lang {
        name: "swift",
        file: "lib.swift",
        header: "",
        callee: "func callee() {}\n",
        caller: "func caller() {\n    callee()\n}\n",
        doomed: "func doomed() {}\n",
        fresh: "func fresh() {\n    callee()\n}\n",
        footer: "",
    },
    Lang {
        name: "c",
        file: "lib.c",
        header: "",
        callee: "void callee(void) {}\n",
        caller: "void caller(void) {\n    callee();\n}\n",
        doomed: "void doomed(void) {}\n",
        fresh: "void fresh(void) {\n    callee();\n}\n",
        footer: "",
    },
    Lang {
        name: "cpp",
        file: "lib.cpp",
        header: "",
        callee: "void callee() {}\n",
        caller: "void caller() {\n    callee();\n}\n",
        doomed: "void doomed() {}\n",
        fresh: "void fresh() {\n    callee();\n}\n",
        footer: "",
    },
    Lang {
        name: "dart",
        file: "lib.dart",
        header: "",
        callee: "void callee() {}\n",
        caller: "void caller() {\n  callee();\n}\n",
        doomed: "void doomed() {}\n",
        fresh: "void fresh() {\n  callee();\n}\n",
        footer: "",
    },
];

fn ecp(repo: &Path, home: &Path, args: &[&str], with_session: bool) -> std::process::Output {
    let mut cmd = Command::new(ecp_bin());
    cmd.args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env_remove("ECP_HOME")
        .env("ECP_SKIP_BG_REBUILD", "1");
    for var in SESSION_VARS {
        cmd.env_remove(var);
    }
    if with_session {
        cmd.env("ECP_SESSION_ID", SESSION);
    }
    cmd.output().expect("ecp failed to spawn")
}

fn index(repo: &Path, home: &Path) {
    let out = ecp(repo, home, &["admin", "index", "--repo", "."], true);
    assert!(
        out.status.success(),
        "admin index failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A committed, indexed repo holding `files`, with its own HOME.
struct Repo {
    repo: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Repo {
    fn indexed(files: &[(&str, &str)]) -> Self {
        let r = Repo {
            repo: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
        };
        for (rel, body) in files {
            r.write(rel, body);
        }
        run_git(r.repo.path(), &["init", "-q", "-b", "main"]);
        commit_all(r.repo.path(), "init");
        index(r.repo.path(), r.home.path());
        r
    }

    fn write(&self, rel: &str, body: &str) {
        fs::write(self.repo.path().join(rel), body).unwrap();
    }

    /// `format: None` keeps inspect's default output format.
    fn inspect_raw(&self, home: &Path, name: &str, format: Option<&str>, session: bool) -> String {
        let mut args = vec!["inspect", "--name", name, "--repo", "."];
        if let Some(format) = format {
            args.extend(["--format", format]);
        }
        let out = ecp(self.repo.path(), home, &args, session);
        assert!(
            out.status.success(),
            "inspect {name} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn inspect_in(&self, home: &Path, name: &str, session: bool) -> Value {
        let stdout = self.inspect_raw(home, name, Some("json"), session);
        serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("non-JSON inspect output ({e}): {stdout}"))
    }

    /// `inspect` on the working tree, through the session overlay.
    fn inspect(&self, name: &str) -> Value {
        self.inspect_in(self.home.path(), name, true)
    }

    /// Commit the working tree and index it into a fresh HOME: the oracle,
    /// with no overlay. Returns that HOME.
    fn commit_and_reindex(&self) -> tempfile::TempDir {
        commit_all(self.repo.path(), "edit");
        let home = tempfile::tempdir().unwrap();
        index(self.repo.path(), home.path());
        home
    }
}

/// What the oracle compares: the symbol's file and span, and the sorted
/// names of its Calls neighbours each way.
#[derive(Debug, PartialEq)]
struct Summary {
    file: String,
    start: u64,
    end: u64,
    calls_out: Vec<String>,
    calls_in: Vec<String>,
}

fn call_names(v: &Value, section: &str) -> Vec<String> {
    let mut names: Vec<String> = v[section]["calls"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|e| e["name"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn summary(v: &Value) -> Summary {
    assert_eq!(v["status"], "found", "expected one match: {v}");
    let symbol = &v["symbol"];
    Summary {
        file: symbol["filePath"].as_str().unwrap().to_string(),
        start: symbol["startLine"].as_u64().unwrap(),
        end: symbol["endLine"].as_u64().unwrap(),
        calls_out: call_names(v, "outgoing"),
        calls_in: call_names(v, "incoming"),
    }
}

fn is_not_found(v: &Value) -> bool {
    v["status"] == "error"
        && v["message"]
            .as_str()
            .is_some_and(|m| m.contains("not found"))
}

#[test]
fn test_inspect_dirty_edit_matches_reindex_for_all_14_languages() {
    for lang in &LANGS {
        let r = Repo::indexed(&[(lang.file, &lang.committed())]);
        r.write(lang.file, &lang.dirty());

        let dirty: Vec<Summary> = ["caller", "fresh", "callee"]
            .iter()
            .map(|name| summary(&r.inspect(name)))
            .collect();
        let doomed = r.inspect("doomed");
        assert!(
            is_not_found(&doomed),
            "{}: a symbol the edit deleted must not be found: {doomed}",
            lang.name
        );

        let oracle_home = r.commit_and_reindex();
        let oracle: Vec<Summary> = ["caller", "fresh", "callee"]
            .iter()
            .map(|name| summary(&r.inspect_in(oracle_home.path(), name, false)))
            .collect();
        assert_eq!(
            dirty, oracle,
            "{}: the dirty tree must answer like a reindex of it",
            lang.name
        );
        assert!(
            oracle[0].start > 1,
            "{}: the edit must have moved caller: {:?}",
            lang.name,
            oracle[0]
        );
    }
}

/// A clean tree leaves the session's overlay empty: the answer is the same
/// as with no session at all.
#[test]
fn test_inspect_clean_tree_with_session_matches_no_session() {
    let lang = &LANGS[7];
    let r = Repo::indexed(&[(lang.file, &lang.committed())]);
    let with_session = r.inspect_raw(r.home.path(), "caller", Some("json"), true);
    let without = r.inspect_raw(r.home.path(), "caller", Some("json"), false);
    assert_eq!(with_session, without);
    let caller = summary(&serde_json::from_str(&with_session).unwrap());
    assert_eq!((caller.start, caller.end), (2, 4));
}

/// A namesake in a clean file and one in a dirty file are both listed, the
/// dirty one at its working-tree line.
/// Contract: `a.rs:1` and `b.rs:7` stand for "the clean and the dirty
/// declaration are both visible, and the dirty one is at its working-tree
/// line", read from the ambiguity's `candidates`.
#[test]
fn test_inspect_clean_and_dirty_namesakes_lists_both() {
    let r = Repo::indexed(&[
        ("a.rs", "pub fn twin() {}\n"),
        ("b.rs", "pub fn twin() {}\n"),
    ]);
    r.write("b.rs", "\n\n\n\n\n\npub fn twin() {}\n");
    let v = r.inspect("twin");
    assert_eq!(v["status"], "ambiguous", "{v}");
    let mut spans: Vec<(String, u64)> = v["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["filePath"].as_str().unwrap().to_string(),
                c["line"].as_u64().unwrap(),
            )
        })
        .collect();
    spans.sort();
    assert_eq!(spans, [("a.rs".to_string(), 1), ("b.rs".to_string(), 7)]);
}

/// `Owner.method` targets a dirty-file method, moved or added, by its owner.
#[test]
fn test_inspect_owner_filter_on_dirty_method_reports_worktree_lines() {
    let lang = &LANGS[3];
    let r = Repo::indexed(&[(lang.file, &lang.committed())]);
    r.write(lang.file, &lang.dirty());

    let caller = summary(&r.inspect("Box.caller"));
    let fresh = summary(&r.inspect("Box.fresh"));
    assert!(is_not_found(&r.inspect("Other.caller")));
    assert_eq!((caller.start, caller.end), (9, 11));
    assert_eq!((fresh.start, fresh.end), (12, 14));
    assert_eq!(fresh.calls_out, ["callee"]);
}

/// The toon-based outputs carry the working-tree span too, not only json:
/// the default (toon with LLM compression) and `--format toon`.
#[test]
fn test_inspect_toon_formats_carry_worktree_lines() {
    let lang = &LANGS[7];
    let r = Repo::indexed(&[(lang.file, &lang.committed())]);
    r.write(lang.file, &lang.dirty());

    let json = summary(&r.inspect("caller"));
    assert_eq!((json.start, json.end), (8, 10));
    for format in [None, Some("toon")] {
        let out = r.inspect_raw(r.home.path(), "caller", format, true);
        let lines: Vec<&str> = out.lines().map(str::trim).collect();
        assert!(
            lines.contains(&"startLine: 8") && lines.contains(&"endLine: 10"),
            "format {format:?} lacks the working-tree span:\n{out}"
        );
    }
}
