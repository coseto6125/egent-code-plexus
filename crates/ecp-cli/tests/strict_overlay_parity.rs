//! Strict overlay parity (architecture plan, area 11): node and edge
//! multisets of the overlay view against a fresh full index of the same
//! edited tree, for dirty, deleted, renamed, emptied and shadowed files.
//! Each scenario holds its known differences in
//! `tests/baselines/strict_parity/<scenario>.txt`; see `common/baseline.rs`
//! for the exact-match and `ECP_BLESS=1` rules.
//!
//! The `test_diff_*` tests prove the comparison itself: an injected duplicate
//! edge, a changed confidence, a wrong parent or a missing node each fail.

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use ecp_core::graph::{ArchivedZeroCopyGraph, NodeKind, RelType};
use ecp_core::graph_fixture::GraphFixture;
use ecp_core::session::merged::MergedGraph;
use std::collections::BTreeSet;
use strict_parity::{check, diff, snapshot, Edit, Scenario};

const LANE_F_DELETED: &str = "lane F (area 5 deleted-file read path)";
const LANE_F_RENAMED: &str = "lane F (area 5 renamed-file read path)";
const LANE_F_SHADOW: &str = "lane F (area 5 shadow cascade)";
const LANE_F_INCREMENTAL: &str = "lane F (incremental overlay: builder edges of dirty files)";
const LANE_F_CLEAN_CALLERS: &str = "lane F (incremental overlay: clean callers re-resolved)";
const LANE_R: &str = "lane R (area 1 shared resolution ladder)";

const TS_LIB: &str = "export function helper(): number {\n  return 1;\n}\n";
const TS_MAIN: &str =
    "import { helper } from './lib';\n\nexport function run(): number {\n  return helper();\n}\n";
const TS_EXTRA: &str =
    "import { helper } from './lib';\n\nexport function extra(): number {\n  return helper();\n}\n";

/// `source` with a blank line after its first line: every later span moves
/// down by one, no token changes.
fn blank_after_first_line(source: &str) -> String {
    match source.split_once('\n') {
        Some((first, rest)) => format!("{first}\n\n{rest}"),
        None => format!("{source}\n\n"),
    }
}

// ── TS / JS scenarios ────────────────────────────────────────────────────────

#[test]
fn test_check_ts_deleted_file_matches_baseline() {
    check(&Scenario {
        name: "ts_deleted",
        owner: LANE_F_DELETED,
        files: &[
            ("src/lib.ts", TS_LIB),
            ("src/main.ts", TS_MAIN),
            ("src/extra.ts", TS_EXTRA),
        ],
        edits: &[Edit::Delete("src/extra.ts")],
    });
}

#[test]
fn test_check_ts_renamed_file_matches_baseline() {
    check(&Scenario {
        name: "ts_renamed",
        owner: LANE_F_RENAMED,
        files: &[
            ("src/lib.ts", TS_LIB),
            ("src/main.ts", TS_MAIN),
            ("src/extra.ts", TS_EXTRA),
        ],
        edits: &[Edit::Rename("src/extra.ts", "src/moved.ts")],
    });
}

/// An added `util.ts` takes `./util` from the tracked `util.js`
/// (`detect_shadow_candidates` pattern a): the cascade re-parses `util.js`,
/// and the clean importer `main.js` must end on the same target as a full
/// index.
#[test]
fn test_check_js_ts_shadowed_file_matches_baseline() {
    check(&Scenario {
        name: "js_ts_shadowed",
        owner: LANE_F_SHADOW,
        files: &[
            ("src/util.js", "export function helper() {\n  return 1;\n}\n"),
            (
                "src/main.js",
                "import { helper } from './util';\n\nexport function run() {\n  return helper();\n}\n",
            ),
        ],
        edits: &[Edit::Write(
            "src/util.ts",
            "export function helper(): number {\n  return 2;\n}\n\nexport function added(): number {\n  return 3;\n}\n",
        )],
    });
}

#[test]
fn test_check_ts_emptied_file_matches_baseline() {
    check(&Scenario {
        name: "ts_emptied",
        owner: LANE_F_INCREMENTAL,
        files: &[("src/lib.ts", TS_LIB), ("src/main.ts", TS_MAIN)],
        edits: &[Edit::Write("src/lib.ts", "")],
    });
}

#[test]
fn test_check_ts_new_empty_file_matches_baseline() {
    check(&Scenario {
        name: "ts_new_empty_file",
        owner: LANE_F_INCREMENTAL,
        files: &[("src/lib.ts", TS_LIB), ("src/main.ts", TS_MAIN)],
        edits: &[Edit::Write("src/empty.ts", "")],
    });
}

#[test]
fn test_check_ts_whitespace_only_edit_matches_baseline() {
    let shifted = blank_after_first_line(TS_LIB);
    check(&Scenario {
        name: "ts_whitespace_only",
        owner: LANE_F_INCREMENTAL,
        files: &[("src/lib.ts", TS_LIB), ("src/main.ts", TS_MAIN)],
        edits: &[Edit::Write("src/lib.ts", &shifted)],
    });
}

/// `helper` is called twice from one function: each side's `Calls`
/// multiplicity must agree, which a deduplicating comparison cannot see.
#[test]
fn test_check_ts_doubled_edge_matches_baseline() {
    let main = "import { helper } from './lib';\n\nexport function run(): number {\n  return helper() + helper();\n}\n";
    let shifted = blank_after_first_line(main);
    check(&Scenario {
        name: "ts_doubled_edge",
        owner: LANE_R,
        files: &[("src/lib.ts", TS_LIB), ("src/main.ts", main)],
        edits: &[Edit::Write("src/main.ts", &shifted)],
    });
}

/// Two files declare `dup`; the dirty caller switches its import from one
/// to the other.
#[test]
fn test_check_ts_same_name_in_two_files_matches_baseline() {
    let main = |from: &str| {
        format!(
            "import {{ dup }} from './{from}';\n\nexport function run(): number {{\n  return dup();\n}}\n"
        )
    };
    let (before, after) = (main("a"), main("b"));
    check(&Scenario {
        name: "ts_same_name_two_files",
        owner: LANE_R,
        files: &[
            (
                "src/a.ts",
                "export function dup(): number {\n  return 1;\n}\n",
            ),
            (
                "src/b.ts",
                "export function dup(): number {\n  return 2;\n}\n",
            ),
            ("src/main.ts", before.as_str()),
        ],
        edits: &[Edit::Write("src/main.ts", &after)],
    });
}

/// A second `solo` in a new file makes the clean caller's unique-global
/// call ambiguous in a full index; the overlay never re-resolves a clean
/// file (`session/view.rs` module doc).
#[test]
fn test_check_js_candidate_added_matches_baseline() {
    check(&Scenario {
        name: "js_candidate_added",
        owner: LANE_F_CLEAN_CALLERS,
        files: &[
            ("src/a.js", "function solo() {\n  return 1;\n}\n"),
            ("src/b.js", "function run() {\n  return solo();\n}\n"),
        ],
        edits: &[Edit::Write(
            "src/c.js",
            "function solo() {\n  return 2;\n}\n",
        )],
    });
}

// ── 14-language dirty-file matrix ────────────────────────────────────────────

/// One language's two-file tree: `callee_file` declares `helper`,
/// `caller_file` calls it. The edit appends `appended` (a new function
/// calling `helper`) to `callee_file` and inserts a blank line into
/// `caller_file`.
struct LangFixture {
    files: &'static [(&'static str, &'static str)],
    callee_file: &'static str,
    caller_file: &'static str,
    appended: &'static str,
}

fn check_dirty(lang: &str, fx: &LangFixture) {
    let source = |path: &str| {
        fx.files
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, s)| *s)
            .expect("fixture names its own files")
    };
    let callee = format!("{}{}", source(fx.callee_file), fx.appended);
    let caller = blank_after_first_line(source(fx.caller_file));
    check(&Scenario {
        name: &format!("dirty_{lang}"),
        owner: LANE_F_INCREMENTAL,
        files: fx.files,
        edits: &[
            Edit::Write(fx.callee_file, &callee),
            Edit::Write(fx.caller_file, &caller),
        ],
    });
}

macro_rules! dirty_matrix {
    ($($lang:ident => $test:ident, $fixture:expr;)*) => {$(
        #[test]
        fn $test() {
            check_dirty(stringify!($lang), &$fixture);
        }
    )*};
}

mod strict_parity_dirty_file {
    use super::{check_dirty, LangFixture};

    dirty_matrix! {
        typescript => test_check_dirty_typescript_matches_baseline, LangFixture {
            files: &[
                ("src/lib.ts", "export function helper(): number {\n  return 1;\n}\n"),
                ("src/main.ts", "import { helper } from './lib';\n\nexport function run(): number {\n  return helper();\n}\n"),
            ],
            callee_file: "src/lib.ts",
            caller_file: "src/main.ts",
            appended: "\nexport function extra(): number {\n  return helper();\n}\n",
        };
        javascript => test_check_dirty_javascript_matches_baseline, LangFixture {
            files: &[
                ("src/lib.js", "export function helper() {\n  return 1;\n}\n"),
                ("src/main.js", "import { helper } from './lib.js';\n\nexport function run() {\n  return helper();\n}\n"),
            ],
            callee_file: "src/lib.js",
            caller_file: "src/main.js",
            appended: "\nexport function extra() {\n  return helper();\n}\n",
        };
        python => test_check_dirty_python_matches_baseline, LangFixture {
            files: &[
                ("pkg/__init__.py", ""),
                ("pkg/lib.py", "def helper():\n    return 1\n"),
                ("pkg/main.py", "from pkg.lib import helper\n\n\ndef run():\n    return helper()\n"),
            ],
            callee_file: "pkg/lib.py",
            caller_file: "pkg/main.py",
            appended: "\n\ndef extra():\n    return helper()\n",
        };
        java => test_check_dirty_java_matches_baseline, LangFixture {
            files: &[
                ("src/main/java/com/acme/Lib.java", "package com.acme;\n\npublic class Lib {\n    public static int helper() {\n        return 1;\n    }\n}\n"),
                ("src/main/java/com/acme/Main.java", "package com.acme;\n\npublic class Main {\n    public int run() {\n        return Lib.helper();\n    }\n}\n"),
            ],
            callee_file: "src/main/java/com/acme/Lib.java",
            caller_file: "src/main/java/com/acme/Main.java",
            appended: "\nclass Extra {\n    int extra() {\n        return Lib.helper();\n    }\n}\n",
        };
        kotlin => test_check_dirty_kotlin_matches_baseline, LangFixture {
            files: &[
                ("src/main/kotlin/acme/Lib.kt", "package acme\n\nfun helper(): Int {\n    return 1\n}\n"),
                ("src/main/kotlin/acme/Main.kt", "package acme\n\nfun run(): Int {\n    return helper()\n}\n"),
            ],
            callee_file: "src/main/kotlin/acme/Lib.kt",
            caller_file: "src/main/kotlin/acme/Main.kt",
            appended: "\nfun extra(): Int {\n    return helper()\n}\n",
        };
        csharp => test_check_dirty_csharp_matches_baseline, LangFixture {
            files: &[
                ("src/Lib.cs", "namespace Acme\n{\n    public static class Lib\n    {\n        public static int Helper()\n        {\n            return 1;\n        }\n    }\n}\n"),
                ("src/App.cs", "namespace Acme\n{\n    public class App\n    {\n        public int Run()\n        {\n            return Lib.Helper();\n        }\n    }\n}\n"),
            ],
            callee_file: "src/Lib.cs",
            caller_file: "src/App.cs",
            appended: "\nnamespace Acme\n{\n    public static class Extra\n    {\n        public static int Twice()\n        {\n            return Lib.Helper();\n        }\n    }\n}\n",
        };
        go => test_check_dirty_go_matches_baseline, LangFixture {
            files: &[
                ("lib.go", "package main\n\nfunc helper() int {\n\treturn 1\n}\n"),
                ("main.go", "package main\n\nfunc run() int {\n\treturn helper()\n}\n"),
            ],
            callee_file: "lib.go",
            caller_file: "main.go",
            appended: "\nfunc extra() int {\n\treturn helper()\n}\n",
        };
        rust => test_check_dirty_rust_matches_baseline, LangFixture {
            files: &[
                ("Cargo.toml", "[package]\nname = \"acme\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
                ("src/lib.rs", "mod util;\n\npub fn run() -> i32 {\n    util::helper()\n}\n"),
                ("src/util.rs", "pub fn helper() -> i32 {\n    1\n}\n"),
            ],
            callee_file: "src/util.rs",
            caller_file: "src/lib.rs",
            appended: "\npub fn extra() -> i32 {\n    helper()\n}\n",
        };
        php => test_check_dirty_php_matches_baseline, LangFixture {
            files: &[
                ("src/Lib.php", "<?php\n\nfunction helper()\n{\n    return 1;\n}\n"),
                ("src/main.php", "<?php\n\nrequire_once __DIR__ . '/Lib.php';\n\nfunction run()\n{\n    return helper();\n}\n"),
            ],
            callee_file: "src/Lib.php",
            caller_file: "src/main.php",
            appended: "\nfunction extra()\n{\n    return helper();\n}\n",
        };
        ruby => test_check_dirty_ruby_matches_baseline, LangFixture {
            files: &[
                ("lib/acme.rb", "def helper()\n  1\nend\n"),
                ("lib/main.rb", "require_relative 'acme'\n\ndef run()\n  helper()\nend\n"),
            ],
            callee_file: "lib/acme.rb",
            caller_file: "lib/main.rb",
            appended: "\ndef extra()\n  helper()\nend\n",
        };
        swift => test_check_dirty_swift_matches_baseline, LangFixture {
            files: &[
                ("Sources/Acme/Lib.swift", "func helper() -> Int {\n    return 1\n}\n"),
                ("Sources/Acme/App.swift", "func run() -> Int {\n    return helper()\n}\n"),
            ],
            callee_file: "Sources/Acme/Lib.swift",
            caller_file: "Sources/Acme/App.swift",
            appended: "\nfunc extra() -> Int {\n    return helper()\n}\n",
        };
        c => test_check_dirty_c_matches_baseline, LangFixture {
            files: &[
                ("src/lib.c", "int helper(void) {\n    return 1;\n}\n"),
                ("src/main.c", "int helper(void);\n\nint run(void) {\n    return helper();\n}\n"),
            ],
            callee_file: "src/lib.c",
            caller_file: "src/main.c",
            appended: "\nint extra(void) {\n    return helper();\n}\n",
        };
        cpp => test_check_dirty_cpp_matches_baseline, LangFixture {
            files: &[
                ("src/lib.cpp", "int helper() {\n    return 1;\n}\n"),
                ("src/main.cpp", "int helper();\n\nint run() {\n    return helper();\n}\n"),
            ],
            callee_file: "src/lib.cpp",
            caller_file: "src/main.cpp",
            appended: "\nint extra() {\n    return helper();\n}\n",
        };
        dart => test_check_dirty_dart_matches_baseline, LangFixture {
            files: &[
                ("lib/lib.dart", "int helper() {\n  return 1;\n}\n"),
                ("lib/main.dart", "import 'lib.dart';\n\nint run() {\n  return helper();\n}\n"),
            ],
            callee_file: "lib/lib.dart",
            caller_file: "lib/main.dart",
            appended: "\nint extra() {\n  return helper();\n}\n",
        };
    }
}

// ── the harness detects what it must ────────────────────────────────────────

#[derive(Default)]
struct Inject {
    duplicate_call: bool,
    call_confidence: Option<f32>,
    wrong_parent: bool,
    drop_node: bool,
}

/// `K.m` (with its `HasMethod`), a second class `Other`, `run` calling `K.m`,
/// and a free `spare` function, plus one injected defect.
fn fixture(inject: Inject) -> rkyv::util::AlignedVec {
    let mut fx = GraphFixture::new();
    let k = fx.node(NodeKind::Class, "src/k.ts", "K");
    let other = fx.node(NodeKind::Class, "src/k.ts", "Other");
    let m = fx.method("src/k.ts", "K", "m");
    let run = fx.func("src/main.ts", "run");
    if !inject.drop_node {
        fx.func("src/main.ts", "spare");
    }
    fx.edge(
        if inject.wrong_parent { other } else { k },
        m,
        RelType::HasMethod,
    );
    let confidence = inject.call_confidence.unwrap_or(1.0);
    fx.edge_with(run, m, RelType::Calls, confidence, "ast-call");
    if inject.duplicate_call {
        fx.edge_with(run, m, RelType::Calls, confidence, "ast-call");
    }
    rkyv::to_bytes::<rkyv::rancor::Error>(&fx.build()).unwrap()
}

fn fixture_diff(inject: Inject) -> BTreeSet<String> {
    let clean = fixture(Inject::default());
    let injected = fixture(inject);
    let access = |bytes: &rkyv::util::AlignedVec| {
        snapshot(&MergedGraph::new(
            rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(bytes).unwrap(),
            None,
        ))
    };
    diff(&access(&clean), &access(&injected))
}

fn in_section<'a>(lines: &'a BTreeSet<String>, prefix: &str) -> Vec<&'a str> {
    lines
        .iter()
        .filter(|l| l.starts_with(prefix))
        .map(String::as_str)
        .collect()
}

#[test]
fn test_diff_identical_graphs_returns_no_difference() {
    assert_eq!(fixture_diff(Inject::default()), BTreeSet::new());
}

#[test]
fn test_diff_injected_duplicate_edge_returns_overlay_only_edge() {
    let lines = fixture_diff(Inject {
        duplicate_call: true,
        ..Inject::default()
    });
    let edges = in_section(&lines, "edge overlay-only x1 Calls Function src/main.ts:");
    assert_eq!(edges.len(), 1, "{lines:#?}");
    assert_eq!(lines.len(), 1, "only the extra edge differs: {lines:#?}");
}

#[test]
fn test_diff_changed_confidence_returns_both_edge_sides() {
    let lines = fixture_diff(Inject {
        call_confidence: Some(0.95),
        ..Inject::default()
    });
    assert_eq!(
        in_section(&lines, "edge full-only x1 Calls").len(),
        1,
        "{lines:#?}"
    );
    assert_eq!(
        in_section(&lines, "edge overlay-only x1 Calls").len(),
        1,
        "{lines:#?}"
    );
    assert!(
        lines.iter().any(|l| l.ends_with("conf=0.95")),
        "the exact confidence is part of the key: {lines:#?}"
    );
}

#[test]
fn test_diff_wrong_parent_returns_parent_lines_not_identity() {
    let lines = fixture_diff(Inject {
        wrong_parent: true,
        ..Inject::default()
    });
    assert!(in_section(&lines, "identity ").is_empty(), "{lines:#?}");
    assert_eq!(
        in_section(&lines, "parent full-only").len(),
        1,
        "{lines:#?}"
    );
    assert_eq!(
        in_section(&lines, "parent overlay-only").len(),
        1,
        "{lines:#?}"
    );
}

#[test]
fn test_diff_missing_node_returns_identity_line() {
    let lines = fixture_diff(Inject {
        drop_node: true,
        ..Inject::default()
    });
    let missing = in_section(&lines, "identity full-only x1 Function src/main.ts:");
    assert_eq!(missing.len(), 1, "{lines:#?}");
    assert!(missing[0].contains(" spare #"), "{lines:#?}");
}

#[test]
fn test_mismatches_new_and_stale_lines_returns_both_as_mismatches() {
    let set =
        |lines: &[&str]| -> BTreeSet<String> { lines.iter().map(|l| l.to_string()).collect() };
    let expected = set(&["edge full-only x1 a", "identity overlay-only x1 b"]);
    assert!(baseline::mismatches(&expected, &expected).is_empty());

    let new = baseline::mismatches(
        &set(&[
            "edge full-only x1 a",
            "identity overlay-only x1 b",
            "edge full-only x1 c",
        ]),
        &expected,
    );
    assert_eq!(
        new,
        vec!["section `edge`: 1 new, 0 stale", "  + edge full-only x1 c"]
    );

    let stale = baseline::mismatches(&set(&["edge full-only x1 a"]), &expected);
    assert_eq!(
        stale,
        vec![
            "section `identity`: 0 new, 1 stale",
            "  - identity overlay-only x1 b"
        ]
    );
}

#[test]
fn test_render_then_read_returns_owner_and_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("x.txt");
    let lines: BTreeSet<String> = ["edge full-only x1 z", "edge full-only x1 a"]
        .iter()
        .map(|l| l.to_string())
        .collect();
    std::fs::write(&path, baseline::render("lane F (test)", &lines)).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        text, "# owner: lane F (test)\nedge full-only x1 a\nedge full-only x1 z\n",
        "sorted, owner first"
    );
    let (owner, read) = baseline::read(&path);
    assert_eq!(owner.as_deref(), Some("lane F (test)"));
    assert_eq!(read, lines);
}
