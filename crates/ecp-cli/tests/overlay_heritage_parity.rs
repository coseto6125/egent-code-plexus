//! A bare call to an inherited member, out of a dirty file: the session
//! overlay binds the same `Calls` edges as a full reindex of the edited tree.
//! The index reaches the member through its heritage tier (Tier 2.75, 0.8):
//! the bases of the class that encloses the caller, each placed in its file,
//! after the import tiers and before a wildcard import.
//!
//! Every fixture keeps the base class in another file and runs four edits:
//!
//! | edit           | dirty files      | asserts                                   |
//! |----------------|------------------|-------------------------------------------|
//! | caller         | caller           | the expected edge, and parity             |
//! | caller+base    | caller and base  | the expected edge, and parity             |
//! | base renamed   | base             | overlay ⊆ reindex (the stale edge goes)   |
//! | base emptied   | base             | overlay ⊆ reindex (the stale edge goes)   |
//!
//! When only the base is dirty, the caller is clean: the overlay never
//! re-resolves a clean file, so it only drops the edge into the removed
//! member, while a reindex may rebind the caller to another tier's target
//! (the renamed class's member at 0.7, a wildcard import at 0.95).
//!
//! Left out, because the language has no class heritage for this tier:
//!
//! | language | nearest analogue                 | why it is not covered here              |
//! |----------|----------------------------------|-----------------------------------------|
//! | C        | none                             | no classes                              |
//! | Go       | struct embedding (`type K struct{ B }`) | a promoted method is called on a receiver (`k.Save()`): the receiver ladder, phase 2 |
//! | Rust     | trait default methods            | reached through `self.save()`: the receiver ladder, phase 2; a `struct` is no `Class` |

#[path = "common/parity.rs"]
mod parity;

use ecp_core::analyzer::types::LocalGraph;
use parity::{calls_after_edit, Parity};
use std::path::Path;

struct Fixture {
    label: &'static str,
    /// The file that declares the base class.
    base: (&'static str, &'static str),
    /// The caller's file; any other files follow it in `rest`.
    caller: (&'static str, &'static str),
    rest: &'static [(&'static str, &'static str)],
    /// The calling method.
    caller_fn: &'static str,
    /// `(callee, callee file, confidence)` the reindex binds.
    expected: &'static [(&'static str, &'static str, f32)],
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        label: "Java",
        base: ("src/p/Base.java", "package p;\nclass Base {\n void save() {}\n}\n"),
        caller: ("src/p/K.java", "package p;\nclass K extends Base {\n void run() { save(); }\n}\n"),
        rest: &[],
        caller_fn: "run",
        expected: &[("save", "src/p/Base.java", 0.8)],
    },
    Fixture {
        label: "Kotlin",
        base: ("src/p/Base.kt", "package p\nopen class Base {\n fun save() {}\n}\n"),
        caller: ("src/p/K.kt", "package p\nclass K : Base() {\n fun run() { save() }\n}\n"),
        rest: &[],
        caller_fn: "run",
        expected: &[("save", "src/p/Base.kt", 0.8)],
    },
    Fixture {
        label: "C#",
        base: ("Base.cs", "class Base {\n public void Save() {}\n}\n"),
        caller: ("K.cs", "class K : Base {\n void Run() { Save(); }\n}\n"),
        rest: &[],
        caller_fn: "Run",
        expected: &[("Save", "Base.cs", 0.8)],
    },
    Fixture {
        label: "Swift",
        base: ("Base.swift", "class Base {\n func save() {}\n}\n"),
        caller: ("K.swift", "class K: Base {\n func run() { save() }\n}\n"),
        rest: &[],
        caller_fn: "run",
        expected: &[("save", "Base.swift", 0.8)],
    },
    Fixture {
        label: "Dart",
        base: ("lib/base.dart", "class Base {\n void save() {}\n}\n"),
        caller: ("lib/k.dart", "import 'base.dart';\nclass K extends Base {\n void run() { save(); }\n}\n"),
        rest: &[],
        caller_fn: "run",
        expected: &[("save", "lib/base.dart", 0.8)],
    },
    Fixture {
        label: "C++",
        base: ("base.hpp", "class Base {\npublic:\n void save() {}\n};\n"),
        caller: ("k.cpp", "#include \"base.hpp\"\nclass K : public Base {\n void run() { save(); }\n};\n"),
        rest: &[],
        caller_fn: "run",
        expected: &[("save", "base.hpp", 0.8)],
    },
    Fixture {
        label: "Ruby",
        base: ("base.rb", "class Base\n  def save; end\nend\n"),
        caller: ("k.rb", "class K < Base\n  def run\n    save()\n  end\nend\n"),
        rest: &[],
        caller_fn: "run",
        expected: &[("save", "base.rb", 0.8)],
    },
    Fixture {
        label: "TypeScript",
        base: ("base.ts", "export class Base {\n save() {}\n}\n"),
        caller: ("k.ts", "import { Base } from './base';\nclass K extends Base {\n run() { save(); }\n}\n"),
        rest: &[],
        caller_fn: "run",
        expected: &[("save", "base.ts", 0.8)],
    },
    Fixture {
        label: "JavaScript",
        base: ("base.js", "export class Base {\n save() {}\n}\n"),
        caller: ("k.js", "import { Base } from './base';\nclass K extends Base {\n run() { save(); }\n}\n"),
        rest: &[],
        caller_fn: "run",
        expected: &[("save", "base.js", 0.8)],
    },
    Fixture {
        label: "Python",
        base: ("base.py", "class Base:\n    def save(self):\n        pass\n"),
        caller: ("k.py", "from base import Base\n\nclass K(Base):\n    def run(self):\n        save()\n"),
        rest: &[],
        caller_fn: "run",
        expected: &[("save", "base.py", 0.8)],
    },
    Fixture {
        label: "PHP",
        base: ("Base.php", "<?php\nclass Base {\n function save() {}\n}\n"),
        caller: ("K.php", "<?php\nclass K extends Base {\n function run() { save(); }\n}\n"),
        rest: &[],
        caller_fn: "run",
        expected: &[("save", "Base.php", 0.8)],
    },
    // The heritage tier runs before the wildcard pass: the inherited member
    // beats the star-imported one.
    Fixture {
        label: "Kotlin/wildcard import beside the base",
        base: ("src/main/kotlin/lib/Base.kt", "package lib\nopen class Base {\n fun refresh() {}\n}\n"),
        caller: ("App.kt", "package app\nimport lib.Base\nimport util.*\nclass Screen : Base() {\n fun go() { refresh() }\n}\n"),
        rest: &[("src/main/kotlin/util/Funcs.kt", "package util\nfun refresh() {}\n")],
        caller_fn: "go",
        expected: &[("refresh", "src/main/kotlin/lib/Base.kt", 0.8)],
    },
    Fixture {
        label: "Java/wildcard import beside the base",
        base: ("src/lib/Base.java", "package lib;\npublic class Base {\n public void refresh() {}\n}\n"),
        caller: ("src/app/Screen.java", "package app;\nimport lib.Base;\nimport util.*;\nclass Screen extends Base {\n void go() { refresh(); }\n}\n"),
        rest: &[("src/util/Funcs.java", "package util;\npublic class Funcs {\n public static void refresh() {}\n}\n")],
        caller_fn: "go",
        expected: &[("refresh", "src/lib/Base.java", 0.8)],
    },
    // The index reads one level of bases, so a cycle cannot loop it; the
    // overlay must read the same level.
    Fixture {
        label: "Java/cyclic heritage",
        base: ("src/p/B.java", "package p;\nclass B extends A {\n void save() {}\n}\n"),
        caller: ("src/p/A.java", "package p;\nclass A extends B {\n void run() { save(); }\n}\n"),
        rest: &[],
        caller_fn: "run",
        expected: &[("save", "src/p/B.java", 0.8)],
    },
];

fn parse(path: &str, source: &str) -> LocalGraph {
    ecp_cli::reanalyze::pipeline()
        .parse_file_raw(Path::new(path), source.as_bytes())
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// `source` with a blank line after its first line: every declaration
/// below moves down one.
fn shifted(source: &str) -> String {
    match source.split_once('\n') {
        Some((first, rest)) => format!("{first}\n\n{rest}"),
        None => format!("\n{source}"),
    }
}

/// `source` with its first `class <Base>` declaration renamed, so the
/// caller's heritage names a class that no longer exists.
fn base_renamed(source: &str) -> String {
    let declared = ["class Base", "class B "]
        .into_iter()
        .find(|decl| source.contains(decl))
        .expect("the base fixture declares `Base` or `B`");
    source.replacen(declared, "class Renamed ", 1)
}

/// A dirty-set label, its dirty files, and whether it proves parity: the
/// reindex holds the expected edges and the overlay equals it.
type DirtySet<'a> = (&'a str, Vec<(&'a str, String)>, bool);

fn mismatches(fixtures: &[Fixture]) -> Vec<String> {
    let mut failures = Vec::new();
    for fixture in fixtures {
        let (base_path, base_src) = fixture.base;
        let (caller_path, caller_src) = fixture.caller;
        let mut files = vec![fixture.base, fixture.caller];
        files.extend_from_slice(fixture.rest);
        let dirty_sets: [DirtySet<'_>; 4] = [
            ("caller", vec![(caller_path, shifted(caller_src))], true),
            (
                "caller+base",
                vec![
                    (caller_path, shifted(caller_src)),
                    (base_path, shifted(base_src)),
                ],
                true,
            ),
            (
                "base renamed",
                vec![(base_path, base_renamed(base_src))],
                false,
            ),
            ("base emptied", vec![(base_path, String::new())], false),
        ];
        for (dirty_set, edits, proves) in &dirty_sets {
            let edits: Vec<(&str, &str)> = edits.iter().map(|(p, s)| (*p, s.as_str())).collect();
            let Parity { reindex, overlay } =
                calls_after_edit(&parse, &files, &edits, &|file, name| {
                    file == caller_path && name == fixture.caller_fn
                });
            let label = format!("{}/{dirty_set}", fixture.label);
            let missing: Vec<_> = fixture
                .expected
                .iter()
                .filter(|&&(callee, file, confidence)| {
                    !reindex.iter().any(|e| {
                        e.callee == callee && e.callee_file == file && e.confidence == confidence
                    })
                })
                .collect();
            if *proves && !missing.is_empty() {
                failures.push(format!(
                    "{label}: the reindex lacks {missing:?}, so the fixture proves nothing: {reindex:#?}"
                ));
                continue;
            }
            let diverges = if *proves {
                overlay != reindex
            } else {
                overlay.iter().any(|edge| !reindex.contains(edge))
            };
            if diverges {
                failures.push(format!(
                    "{label}: overlay {overlay:#?}\nreindex {reindex:#?}"
                ));
            }
        }
    }
    failures
}

#[test]
fn test_overlay_heritage_call_matches_reindex_across_class_languages() {
    let failures = mismatches(FIXTURES);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
