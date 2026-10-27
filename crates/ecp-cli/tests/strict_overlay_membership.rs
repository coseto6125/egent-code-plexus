//! Class membership of the incremental overlay (F7b1): for a dirty class,
//! the overlay view rebuilds `HasMethod` / `HasProperty` as a fresh full
//! index of the edited tree has them, in each of the 14 mainstream
//! languages, through the production index, fragment write and read
//! (`common/strict_parity.rs`).
//!
//! Each check first asserts that the full index binds the new member and
//! that the base index bound the removed one, so a fixture whose parser
//! emits no membership fails instead of passing empty, and the overlay
//! cannot pass by keeping the base index's edges.

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use ecp_core::session::merged::MergedGraph;
use strict_parity::{snapshot, Edit, Scenario};

const OWNER: &str = "lane F (incremental overlay: builder edges of dirty files)";

/// One language's class: `class` with `{members}` where the members go,
/// `member` with `{name}` where the member's name goes, at `path`, beside
/// `extra` files the language needs.
struct Fixture {
    lang: &'static str,
    extra: &'static [(&'static str, &'static str)],
    path: &'static str,
    class: &'static str,
    member: &'static str,
}

/// C declares struct fields (`HasProperty`), Go and Rust bind methods
/// outside the type's span through their owner; every other language
/// encloses its methods in the class body.
const FIXTURES: &[Fixture] = &[
    Fixture {
        lang: "typescript",
        extra: &[],
        path: "src/k.ts",
        class: "export class K {\n{members}}\n",
        member: "  {name}(): number {\n    return 1;\n  }\n",
    },
    Fixture {
        lang: "javascript",
        extra: &[],
        path: "src/k.js",
        class: "export class K {\n{members}}\n",
        member: "  {name}() {\n    return 1;\n  }\n",
    },
    Fixture {
        lang: "python",
        extra: &[],
        path: "pkg/k.py",
        class: "class K:\n{members}",
        member: "    def {name}(self):\n        return 1\n",
    },
    Fixture {
        lang: "java",
        extra: &[],
        path: "src/main/java/com/acme/K.java",
        class: "package com.acme;\n\npublic class K {\n{members}}\n",
        member: "    int {name}() {\n        return 1;\n    }\n",
    },
    Fixture {
        lang: "kotlin",
        extra: &[],
        path: "src/main/kotlin/acme/K.kt",
        class: "package acme\n\nclass K {\n{members}}\n",
        member: "    fun {name}(): Int {\n        return 1\n    }\n",
    },
    Fixture {
        lang: "csharp",
        extra: &[],
        path: "src/K.cs",
        class: "namespace Acme\n{\n    public class K\n    {\n{members}    }\n}\n",
        member: "        public int {name}()\n        {\n            return 1;\n        }\n",
    },
    Fixture {
        lang: "go",
        extra: &[],
        path: "k.go",
        class: "package main\n\ntype K struct{}\n\n{members}",
        member: "func (k K) {name}() int {\n\treturn 1\n}\n\n",
    },
    Fixture {
        lang: "rust",
        extra: &[(
            "Cargo.toml",
            "[package]\nname = \"acme\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )],
        path: "src/lib.rs",
        class: "pub struct K;\n\nimpl K {\n{members}}\n",
        member: "    pub fn {name}(&self) -> i32 {\n        1\n    }\n",
    },
    Fixture {
        lang: "php",
        extra: &[],
        path: "src/K.php",
        class: "<?php\n\nclass K\n{\n{members}}\n",
        member: "    public function {name}()\n    {\n        return 1;\n    }\n",
    },
    Fixture {
        lang: "ruby",
        extra: &[],
        path: "lib/k.rb",
        class: "class K\n{members}end\n",
        member: "  def {name}\n    1\n  end\n",
    },
    Fixture {
        lang: "swift",
        extra: &[],
        path: "Sources/Acme/K.swift",
        class: "class K {\n{members}}\n",
        member: "    func {name}() -> Int {\n        return 1\n    }\n",
    },
    Fixture {
        lang: "c",
        extra: &[],
        path: "src/k.c",
        class: "struct K {\n{members}};\n",
        member: "    int {name};\n",
    },
    Fixture {
        lang: "cpp",
        extra: &[],
        path: "src/k.cpp",
        class: "class K {\npublic:\n{members}};\n",
        member: "    int {name}() {\n        return 1;\n    }\n",
    },
    Fixture {
        lang: "dart",
        extra: &[],
        path: "lib/k.dart",
        class: "class K {\n{members}}\n",
        member: "  int {name}() {\n    return 1;\n  }\n",
    },
];

/// The class of `fx` declaring `members` in order.
fn source(fx: &Fixture, members: &[&str]) -> String {
    let body: String = members
        .iter()
        .map(|name| fx.member.replace("{name}", name))
        .collect();
    fx.class.replace("{members}", &body)
}

fn is_membership(line: &str) -> bool {
    line.starts_with("HasMethod ") || line.starts_with("HasProperty ")
}

/// The membership edges whose source lies in `path`, as a sorted multiset.
fn membership_edges(edges: &[String], path: &str) -> Vec<String> {
    let from = format!(" {path}:");
    let mut out: Vec<String> = edges
        .iter()
        .filter(|l| is_membership(l) && l.split(" -> ").next().is_some_and(|s| s.contains(&from)))
        .cloned()
        .collect();
    out.sort();
    out
}

/// Run `files` + `edits` and assert the overlay's membership edges of
/// `path` are the reindex's, that the reindex binds `fresh`, and that no
/// difference line names a membership edge or parent. Returns the base
/// index's membership edges of `path`.
fn assert_membership_matches(
    label: &str,
    files: &[(&str, &str)],
    edits: &[Edit<'_>],
    path: &str,
    fresh: &str,
) -> Vec<String> {
    let observed = strict_parity::run(&Scenario {
        name: "",
        owner: OWNER,
        files,
        edits,
    });
    let (full, overlay) = observed.snapshots();
    let full_members = membership_edges(&full.edge, path);
    let target = format!(" {fresh} #");
    assert!(
        full_members.iter().any(|l| l.contains(&target)),
        "{label}: the reindex binds the new member: {full_members:#?}"
    );
    assert_eq!(
        membership_edges(&overlay.edge, path),
        full_members,
        "{label}: the dirty class's membership edges"
    );
    let lines = observed.lines();
    let stray: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("HasMethod") || l.contains("HasProperty"))
        .collect();
    assert!(stray.is_empty(), "{label}: {stray:#?}");
    membership_edges(
        &snapshot(&MergedGraph::new(observed.base(), None)).edge,
        path,
    )
}

/// An edit that keeps `keep`, removes `old` and adds `fresh` to a dirty
/// class: the overlay binds the new member, drops the removed one and keeps
/// one edge to the kept one, as a reindex does, in each of the 14
/// languages.
#[test]
fn test_overlay_view_dirty_class_membership_matches_reindex() {
    for fx in FIXTURES {
        let before = source(fx, &["keep", "old"]);
        let after = source(fx, &["keep", "fresh"]);
        let mut files: Vec<(&str, &str)> = fx.extra.to_vec();
        files.push((fx.path, before.as_str()));
        let base = assert_membership_matches(
            fx.lang,
            &files,
            &[Edit::Write(fx.path, &after)],
            fx.path,
            "fresh",
        );
        assert!(
            base.iter().any(|l| l.contains(" old #")),
            "{}: the base index binds the removed member: {base:#?}",
            fx.lang
        );
    }
}

/// A class declared twice in one file: the reindex binds the members of
/// both to the first declaration, and so does the overlay.
#[test]
fn test_overlay_view_class_declared_twice_membership_matches_reindex() {
    let class = |second: &str| {
        format!("class K {{\n  a(): number {{\n    return 1;\n  }}\n}}\n\nclass K {{\n{second}}}\n")
    };
    let before = class("  b(): number {\n    return 2;\n  }\n");
    let after =
        class("  b(): number {\n    return 2;\n  }\n  fresh(): number {\n    return 3;\n  }\n");
    assert_membership_matches(
        "class declared twice",
        &[("src/k.ts", before.as_str())],
        &[Edit::Write("src/k.ts", &after)],
        "src/k.ts",
        "fresh",
    );
}

/// A Java constructor named like its class is the class's member, never a
/// self-loop, on both sides.
#[test]
fn test_overlay_view_member_named_like_class_membership_matches_reindex() {
    let class = |extra: &str| {
        format!("package com.acme;\n\npublic class K {{\n    public K() {{\n    }}\n{extra}}}\n")
    };
    let path = "src/main/java/com/acme/K.java";
    let before = class("");
    let after = class("    int fresh() {\n        return 1;\n    }\n");
    let base = assert_membership_matches(
        "member named like its class",
        &[(path, before.as_str())],
        &[Edit::Write(path, &after)],
        path,
        "fresh",
    );
    assert!(
        base.iter().any(|l| l
            .split(" -> ")
            .nth(1)
            .is_some_and(|target| target.contains(" K #"))),
        "the base index binds the constructor named like its class: {base:#?}"
    );
}

/// A C# partial class split over a clean and a dirty file: membership is
/// per file, so the dirty part's members are rebuilt and the clean part's
/// stay as indexed.
#[test]
fn test_overlay_view_csharp_partial_class_membership_matches_reindex() {
    let part = |members: &str| {
        format!("namespace Acme\n{{\n    public partial class K\n    {{\n{members}    }}\n}}\n")
    };
    let method = |name: &str| {
        format!("        public int {name}()\n        {{\n            return 1;\n        }}\n")
    };
    let clean = part(&method("A"));
    let before = part(&method("B"));
    let after = part(&format!("{}{}", method("B"), method("fresh")));
    assert_membership_matches(
        "C# partial class",
        &[
            ("src/KA.cs", clean.as_str()),
            ("src/KB.cs", before.as_str()),
        ],
        &[Edit::Write("src/KB.cs", &after)],
        "src/KB.cs",
        "fresh",
    );
}

/// An edit that empties a class's file leaves no membership edge of it on
/// either side, and no difference line names one.
#[test]
fn test_overlay_view_emptied_class_file_membership_matches_reindex() {
    let source = "export class K {\n  a(): number {\n    return 1;\n  }\n}\n";
    let observed = strict_parity::run(&Scenario {
        name: "",
        owner: OWNER,
        files: &[("src/k.ts", source)],
        edits: &[Edit::Write("src/k.ts", "")],
    });
    let (full, overlay) = observed.snapshots();
    assert!(membership_edges(&full.edge, "src/k.ts").is_empty());
    assert!(membership_edges(&overlay.edge, "src/k.ts").is_empty());
    let lines = observed.lines();
    assert!(lines.iter().all(|l| !l.contains("HasMethod")), "{lines:#?}");
}
