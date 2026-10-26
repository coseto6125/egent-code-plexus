//! R12 (B8a): the call entry (R1: site normalization R2, the ladder, the
//! call-level Kotlin extension retry R17, the untyped-member post-filter
//! R21 and the constructor fallback R22) runs once, in
//! `ecp_core::analyzer::ladder::call`, for the index and the overlay. Each
//! case plants one tree per language. It asserts what a reader of a fresh
//! full index gets (each caller's exact `Calls` targets, at an allowed
//! confidence), then that the overlay view of the same tree (the caller file
//! dirty, unchanged but for a trailing newline) has no `Calls` edge that
//! differs from that index, by source uid, target uid and exact confidence.
//! The overlay stamps its own uncommitted-edit reason on a `Calls` edge, so
//! the comparison leaves reasons out (`common/strict_parity.rs`); the
//! confidence names the tier.
//!
//! Each tree holds a typed-receiver call, an untyped-member call that two
//! classes answer (no edge on both sides), and a constructor call, where the
//! language has each. Where a by-name tier could bind a typed call to the
//! wrong declaration, the tree plants that decoy namesake. The rules a
//! language lacks, by
//! `crates/ecp-core/src/analyzer/types.rs`:
//!
//! ```text
//! ladder_call_entry go no constructor (types.rs CallSite::constructed_type: "Go, Rust and C have no constructors")
//! ladder_call_entry rust no constructor (types.rs CallSite::constructed_type: "Go, Rust and C have no constructors")
//! ladder_call_entry c no constructor (types.rs CallSite::constructed_type: "Go, Rust and C have no constructors")
//! ladder_call_entry c no member call (types.rs CallSite::reaches_free_only: "C declares no members")
//! ```
//!
//! One divergence stays, behind `ladder::TypeName`: a constructed type path
//! whose qualifier is empty (C++ `new ::Widget()`) names `::Widget` for the
//! index, which no type declares, and `Widget` for the overlay, whose
//! last-segment retry binds it.
//! [`test_ladder_call_entry_cpp_empty_qualifier_construct_overlay_only`]
//! pins it.

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use ecp_core::session::merged::MergedGraph;
use std::collections::BTreeSet;
use strict_parity::{run, snapshot, Edit, Observed, Scenario, Snapshot};

type Files<'a> = &'a [(&'a str, &'a str)];

const GLOBAL: &[f32] = &[0.7];
const IMPORT_SCOPED: &[f32] = &[0.95];
const QUALIFIER_SCOPED: &[f32] = &[0.85];
const SAME_FILE: &[f32] = &[1.0];
/// A target whose tier the case does not fix: the parity check holds the
/// exact confidence equal on both sides.
const ANY_TIER: &[f32] = &[0.6, 0.7, 0.8, 0.85, 0.95, 1.0];

/// What one caller must call: `(callee name, callee file, allowed confidences)`.
type Callees<'a> = &'a [(&'a str, &'a str, &'a [f32])];

/// One case: the tree, the caller file the overlay dirties, and, per caller,
/// what it must call in a fresh full index of the edited tree.
struct Case {
    name: &'static str,
    files: Files<'static>,
    dirty: &'static str,
    expected: &'static [(&'static str, Callees<'static>)],
}

/// Python. `Repo` is a from-imported type: `Repo.make()` binds its member,
/// not `lib/extra.py`'s `make`. `obj.save()` is untyped: the one `save`, a
/// method, passes the post-filter. Two classes define `close`: no edge.
/// `Widget()` takes the constructor fallback to `__init__`.
const PYTHON: Case = Case {
    name: "ladder_call_entry_python",
    files: &[
        ("pkg/__init__.py", ""),
        (
            "pkg/repo.py",
            "class Repo:\n    @staticmethod\n    def make():\n        return 1\n\n    def save(self):\n        return 2\n",
        ),
        (
            "pkg/widget.py",
            "class Widget:\n    def __init__(self):\n        self.x = 1\n",
        ),
        (
            "pkg/shapes.py",
            "class Circle:\n    def close(self):\n        return 1\n\n\nclass Square:\n    def close(self):\n        return 2\n",
        ),
        ("lib/extra.py", "def make():\n    return 3\n"),
        (
            "app/main.py",
            "from pkg.repo import Repo\n\n\ndef typed():\n    return Repo.make()\n\n\ndef untyped(obj):\n    return obj.save()\n\n\ndef two(obj):\n    return obj.close()\n\n\ndef construct():\n    return Widget()\n",
        ),
    ],
    dirty: "app/main.py",
    expected: &[
        ("typed", &[("make", "pkg/repo.py", ANY_TIER)]),
        ("untyped", &[("save", "pkg/repo.py", GLOBAL)]),
        ("two", &[]),
        ("construct", &[("__init__", "pkg/widget.py", ANY_TIER)]),
    ],
};

/// TypeScript. `Repo.make()` binds the static member at the qualifier tier,
/// not `src/extra.ts`'s `make`. `x.close()` reaches two classes: no edge.
/// `new Widget()` lands on the imported class's constructor.
const TYPESCRIPT: Case = Case {
    name: "ladder_call_entry_typescript",
    files: &[
        (
            "src/repo.ts",
            "export class Repo {\n  static make(): number {\n    return 1;\n  }\n}\n",
        ),
        (
            "src/widget.ts",
            "export class Widget {\n  constructor() {}\n}\n",
        ),
        (
            "src/shapes.ts",
            "export class Circle {\n  close(): void {}\n}\nexport class Square {\n  close(): void {}\n}\n",
        ),
        ("src/extra.ts", "export function make(): number {\n  return 2;\n}\n"),
        (
            "src/main.ts",
            "import { Repo } from './repo';\nimport { Widget } from './widget';\n\nexport function typed() {\n  return Repo.make();\n}\n\nexport function two(x: any) {\n  return x.close();\n}\n\nexport function construct() {\n  return new Widget();\n}\n",
        ),
    ],
    dirty: "src/main.ts",
    expected: &[
        ("typed", &[("make", "src/repo.ts", QUALIFIER_SCOPED)]),
        ("two", &[]),
        ("construct", &[("constructor", "src/widget.ts", ANY_TIER)]),
    ],
};

/// JavaScript: the TypeScript tree without types.
const JAVASCRIPT: Case = Case {
    name: "ladder_call_entry_javascript",
    files: &[
        (
            "src/repo.js",
            "export class Repo {\n  static make() {\n    return 1;\n  }\n}\n",
        ),
        (
            "src/widget.js",
            "export class Widget {\n  constructor() {}\n}\n",
        ),
        (
            "src/shapes.js",
            "export class Circle {\n  close() {}\n}\nexport class Square {\n  close() {}\n}\n",
        ),
        ("src/extra.js", "export function make() {\n  return 2;\n}\n"),
        (
            "src/main.js",
            "import { Repo } from './repo';\nimport { Widget } from './widget';\n\nexport function typed() {\n  return Repo.make();\n}\n\nexport function two(x) {\n  return x.close();\n}\n\nexport function construct() {\n  return new Widget();\n}\n",
        ),
    ],
    dirty: "src/main.js",
    expected: &[
        ("typed", &[("make", "src/repo.js", QUALIFIER_SCOPED)]),
        ("two", &[]),
        ("construct", &[("constructor", "src/widget.js", ANY_TIER)]),
    ],
};

/// Java. `Util.run()` binds the imported class's member, not
/// `misc.Other.run`. `make().close()` is a member call on a computed
/// receiver that two classes answer: no edge. `new Widget()` lands on the
/// class's constructor.
const JAVA: Case = Case {
    name: "ladder_call_entry_java",
    files: &[
        (
            "lib/Util.java",
            "package lib;\npublic class Util {\n    public static void run() {}\n}\n",
        ),
        (
            "lib/Widget.java",
            "package lib;\npublic class Widget {\n    public Widget() {}\n}\n",
        ),
        (
            "lib/Shapes.java",
            "package lib;\nclass Circle {\n    void close() {}\n}\nclass Square {\n    void close() {}\n}\n",
        ),
        (
            "misc/Other.java",
            "package misc;\npublic class Other {\n    public static void run() {}\n}\n",
        ),
        (
            "app/Main.java",
            "package app;\nimport lib.Util;\nimport lib.Widget;\nclass Main {\n    void typed() { Util.run(); }\n    void two() { make().close(); }\n    void construct() { new Widget(); }\n}\n",
        ),
    ],
    dirty: "app/Main.java",
    expected: &[
        ("typed", &[("run", "lib/Util.java", ANY_TIER)]),
        ("two", &[]),
        ("construct", &[("Widget", "lib/Widget.java", ANY_TIER)]),
    ],
};

/// Kotlin. A class import types `b.size()`: `Box`'s member binds. `Box`
/// owns no `grow`: the call-level extension retry binds `fun Box.grow()`,
/// declared in a file the caller does not import. `x.close()` reaches two
/// classes: no edge. `Widget()` constructs the class, which declares no
/// constructor of its own.
const KOTLIN: Case = Case {
    name: "ladder_call_entry_kotlin",
    files: &[
        (
            "src/main/kotlin/lib/Box.kt",
            "package lib\nclass Box {\n fun size(): Int = 0\n}\n",
        ),
        (
            "src/main/kotlin/lib/BoxExt.kt",
            "package lib\nfun Box.grow() {}\n",
        ),
        ("src/main/kotlin/lib/Widget.kt", "package lib\nclass Widget\n"),
        (
            "src/main/kotlin/lib/Shapes.kt",
            "package lib\nclass Circle {\n fun close() {}\n}\nclass Square {\n fun close() {}\n}\n",
        ),
        (
            "src/main/kotlin/other/Helpers.kt",
            "package other\nfun size(): Int = 1\n",
        ),
        (
            "src/main/kotlin/app/Main.kt",
            "package app\nimport lib.Box\nimport lib.Widget\nfun typed(b: Box) { b.size() }\nfun extension(b: Box) { b.grow() }\nfun two(x: Any) { x.close() }\nfun construct() { Widget() }\n",
        ),
    ],
    dirty: "src/main/kotlin/app/Main.kt",
    expected: &[
        (
            "typed",
            &[("size", "src/main/kotlin/lib/Box.kt", IMPORT_SCOPED)],
        ),
        (
            "extension",
            &[("grow", "src/main/kotlin/lib/BoxExt.kt", GLOBAL)],
        ),
        ("two", &[]),
        (
            "construct",
            &[("Widget", "src/main/kotlin/lib/Widget.kt", ANY_TIER)],
        ),
    ],
};

/// C#. `Util.Make()` binds the type's member, not the caller's own `Make`.
/// `Console` is a library type: `WriteLine` binds nothing, though `Tools`
/// declares one. `x.Close()` reaches two classes: no edge. `new Widget()`
/// lands on the class's constructor.
const CSHARP: Case = Case {
    name: "ladder_call_entry_csharp",
    files: &[
        (
            "Util.cs",
            "static class Util {\n public static void Make() {}\n}\n",
        ),
        ("Widget.cs", "class Widget {\n public Widget() {}\n}\n"),
        (
            "Shapes.cs",
            "class Circle {\n public void Close() {}\n}\nclass Square {\n public void Close() {}\n}\n",
        ),
        (
            "Tools.cs",
            "class Tools {\n public void WriteLine(string s) {}\n}\n",
        ),
        (
            "App.cs",
            "class App {\n void Make() {}\n void Typed() { Util.Make(); }\n void Two(object x) { x.Close(); }\n void External() { Console.WriteLine(\"x\"); }\n void Construct() { new Widget(); }\n}\n",
        ),
    ],
    dirty: "App.cs",
    expected: &[
        ("Typed", &[("Make", "Util.cs", ANY_TIER)]),
        ("Two", &[]),
        ("External", &[]),
        ("Construct", &[("Widget", "Widget.cs", ANY_TIER)]),
    ],
};

/// Go. `util` is a mapped package: `util.Present()` binds ahead of the
/// ladder at the import tier. `d` and `x` name no package
/// (`GoPackageLookup::Value`), so their calls are untyped member calls:
/// `d.Fetch()` binds the one `Fetch` method, and `x.Close()`, which two
/// types answer, binds nothing. Go has no constructor.
const GO: Case = Case {
    name: "ladder_call_entry_go",
    files: &[
        ("go.mod", "module example.com/repo\n\ngo 1.21\n"),
        ("util/util.go", "package util\n\nfunc Present() {}\n"),
        (
            "dog.go",
            "package main\n\ntype Dog struct{}\n\nfunc (d *Dog) Fetch() {}\n",
        ),
        (
            "shapes.go",
            "package main\n\ntype Circle struct{}\n\nfunc (c *Circle) Close() {}\n\ntype Square struct{}\n\nfunc (s *Square) Close() {}\n",
        ),
        (
            "main.go",
            "package main\n\nimport \"example.com/repo/util\"\n\nfunc (d *Dog) Bark() {\n\td.Fetch()\n}\n\nfunc run() {\n\tutil.Present()\n\tx.Close()\n}\n",
        ),
    ],
    dirty: "main.go",
    expected: &[
        ("Bark", &[("Fetch", "dog.go", ANY_TIER)]),
        ("run", &[("Present", "util/util.go", IMPORT_SCOPED)]),
    ],
};

/// Rust. `Repo::make()` binds the associated function through the `use`.
/// `x.close()` on an untyped value reaches two types' methods: no edge. A
/// bare `helper()` binds the caller's file. Rust has no constructor.
const RUST: Case = Case {
    name: "ladder_call_entry_rust",
    files: &[
        (
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        ),
        (
            "src/repo.rs",
            "pub struct Repo;\n\nimpl Repo {\n    pub fn make() -> Repo {\n        Repo\n    }\n}\n",
        ),
        (
            "src/shapes.rs",
            "pub struct Circle;\npub struct Square;\n\nimpl Circle {\n    pub fn close(&self) {}\n}\n\nimpl Square {\n    pub fn close(&self) {}\n}\n",
        ),
        (
            "src/lib.rs",
            "mod repo;\nmod shapes;\n\nuse crate::repo::Repo;\n\npub fn typed() {\n    Repo::make();\n}\n\npub fn two() {\n    let x = shapes_of();\n    x.close();\n}\n\nfn helper() {}\n\npub fn bare() {\n    helper();\n}\n",
        ),
    ],
    dirty: "src/lib.rs",
    expected: &[
        ("typed", &[("make", "src/repo.rs", ANY_TIER)]),
        ("two", &[]),
        ("bare", &[("helper", "src/lib.rs", SAME_FILE)]),
    ],
};

/// PHP. `Util::run()` binds the imported class's member. `$x->close()`
/// reaches two classes: no edge. `new Widget()` lands on `__construct`.
const PHP: Case = Case {
    name: "ladder_call_entry_php",
    files: &[
        (
            "src/Util.php",
            "<?php\nnamespace App;\nclass Util {\n    public static function run() {}\n}\n",
        ),
        (
            "src/Widget.php",
            "<?php\nnamespace App;\nclass Widget {\n    public function __construct() {}\n}\n",
        ),
        (
            "src/Shapes.php",
            "<?php\nnamespace App;\nclass Circle {\n    public function close() {}\n}\nclass Square {\n    public function close() {}\n}\n",
        ),
        (
            "src/Other.php",
            "<?php\nnamespace Other;\nclass Other {\n    public static function run() {}\n}\n",
        ),
        (
            "app.php",
            "<?php\nuse App\\Util;\nuse App\\Widget;\nfunction typed() { Util::run(); }\nfunction two($x) { $x->close(); }\nfunction construct() { new Widget(); }\n",
        ),
    ],
    dirty: "app.php",
    expected: &[
        ("typed", &[("run", "src/Util.php", ANY_TIER)]),
        ("two", &[]),
        ("construct", &[("__construct", "src/Widget.php", ANY_TIER)]),
    ],
};

/// Ruby. `Util.make` binds the class method. `x.close` reaches two classes:
/// no edge. `Widget.new` constructs the class, which declares no
/// `initialize`.
const RUBY: Case = Case {
    name: "ladder_call_entry_ruby",
    files: &[
        ("lib/util.rb", "class Util\n  def self.make\n  end\nend\n"),
        ("lib/widget.rb", "class Widget\nend\n"),
        (
            "lib/shapes.rb",
            "class Circle\n  def close; end\nend\nclass Square\n  def close; end\nend\n",
        ),
        (
            "app.rb",
            "def typed\n  Util.make\nend\n\ndef two(x)\n  x.close\nend\n\ndef construct\n  Widget.new\nend\n",
        ),
    ],
    dirty: "app.rb",
    expected: &[
        ("typed", &[("make", "lib/util.rb", ANY_TIER)]),
        ("two", &[]),
        ("construct", &[("Widget", "lib/widget.rb", ANY_TIER)]),
    ],
};

/// Swift. `Util.make()` binds the type's member, not the caller's own
/// `make`. `x.close()` names no type: an untyped member call that two
/// classes answer, no edge. `Widget()` constructs the class.
const SWIFT: Case = Case {
    name: "ladder_call_entry_swift",
    files: &[
        ("Util.swift", "class Util {\n static func make() {}\n}\n"),
        ("Widget.swift", "class Widget {\n}\n"),
        (
            "Shapes.swift",
            "class Circle {\n func close() {}\n}\nclass Square {\n func close() {}\n}\n",
        ),
        (
            "App.swift",
            "class App {\n func make() {}\n func typed() { Util.make() }\n func two() { x.close() }\n func construct() { Widget() }\n}\n",
        ),
    ],
    dirty: "App.swift",
    expected: &[
        ("typed", &[("make", "Util.swift", ANY_TIER)]),
        ("two", &[]),
        ("construct", &[("Widget", "Widget.swift", ANY_TIER)]),
    ],
};

/// C. No receivers and no constructors: a bare `helper()` binds the one
/// `helper` of the project.
const C: Case = Case {
    name: "ladder_call_entry_c",
    files: &[
        ("util.c", "int helper(void) {\n    return 1;\n}\n"),
        ("other.c", "int other(void) {\n    return 2;\n}\n"),
        ("main.c", "int run(void) {\n    return helper();\n}\n"),
    ],
    dirty: "main.c",
    expected: &[("run", &[("helper", "util.c", ANY_TIER)])],
};

/// C++. `Util::make()` binds the static member. `x.close()` reaches two
/// classes: no edge. `new shop::Item()` misses the callable lookup and the
/// constructor fallback binds `Item`'s constructor (B §3.2 row 3).
const CPP: Case = Case {
    name: "ladder_call_entry_cpp",
    files: &[
        (
            "src/shop.hpp",
            "namespace shop {\nclass Item {\npublic:\n    Item() {}\n};\n}\n",
        ),
        (
            "src/util.hpp",
            "class Util {\npublic:\n    static void make() {}\n};\n",
        ),
        (
            "src/shapes.hpp",
            "class Circle {\npublic:\n    void close() {}\n};\nclass Square {\npublic:\n    void close() {}\n};\n",
        ),
        (
            "src/app.cpp",
            "#include \"shop.hpp\"\n#include \"util.hpp\"\n\nvoid typed() {\n    Util::make();\n}\n\nvoid two() {\n    x.close();\n}\n\nshop::Item* construct() {\n    return new shop::Item();\n}\n",
        ),
    ],
    dirty: "src/app.cpp",
    expected: &[
        ("typed", &[("make", "src/util.hpp", ANY_TIER)]),
        ("two", &[]),
        ("construct", &[("Item", "src/shop.hpp", QUALIFIER_SCOPED)]),
    ],
};

/// Dart. `Util.make()` binds the type's member, not the caller's own
/// `make`. `x.close()` reaches two classes: no edge. `Widget()` constructs
/// the class.
const DART: Case = Case {
    name: "ladder_call_entry_dart",
    files: &[
        ("lib/util.dart", "class Util {\n static void make() {}\n}\n"),
        ("lib/widget.dart", "class Widget {\n}\n"),
        (
            "lib/shapes.dart",
            "class Circle {\n void close() {}\n}\nclass Square {\n void close() {}\n}\n",
        ),
        (
            "lib/app.dart",
            "import 'util.dart';\nimport 'widget.dart';\nclass App {\n void make() {}\n void typed() { Util.make(); }\n void two(dynamic x) { x.close(); }\n void construct() { Widget(); }\n}\n",
        ),
    ],
    dirty: "lib/app.dart",
    expected: &[
        ("typed", &[("make", "lib/util.dart", ANY_TIER)]),
        ("two", &[]),
        ("construct", &[("Widget", "lib/widget.dart", ANY_TIER)]),
    ],
};

/// `name` of a snapshot node line `<kind> <file>:<line> <name> #<uid>`, with its file.
fn name_and_file(node: &str) -> (&str, &str) {
    let mut parts = node.split(' ');
    let (_kind, location, name) = (parts.next(), parts.next(), parts.next());
    let location = location.unwrap_or_default();
    let file = location.rsplit_once(':').map_or(location, |(file, _)| file);
    (name.unwrap_or_default(), file)
}

/// `(caller, callee, callee file, confidence)` of every `Calls` edge.
fn call_edges(snap: &Snapshot) -> Vec<(String, String, String, String)> {
    snap.edge
        .iter()
        .filter_map(|line| {
            let (source, target) = line.strip_prefix("Calls ")?.split_once(" -> ")?;
            let (target, confidence) = target.rsplit_once(" conf=")?;
            let (caller, _) = name_and_file(source);
            let (callee, file) = name_and_file(target);
            Some((
                caller.to_string(),
                callee.to_string(),
                file.to_string(),
                confidence.to_string(),
            ))
        })
        .collect()
}

/// The production index of `case`'s edited tree and the overlay of the edit.
fn observe_case(case: &Case) -> Observed {
    let (_, source) = case
        .files
        .iter()
        .find(|(path, _)| *path == case.dirty)
        .expect("the dirty file is one of the files");
    let edited = format!("{source}\n");
    run(&Scenario {
        name: case.name,
        owner: "R12 (B8a call entry)",
        files: case.files,
        edits: &[Edit::Write(case.dirty, &edited)],
    })
}

/// The overlay's `Calls` lines that differ from the full index.
fn differing_calls(observed: &Observed) -> BTreeSet<String> {
    observed
        .lines()
        .into_iter()
        .filter(|line| line.starts_with("edge ") && line.contains(" Calls "))
        .collect()
}

/// Hold the full index of `case`'s edited tree to `case.expected`, then the
/// overlay's `Calls` edges to the index's.
fn check(case: &Case) {
    let observed = observe_case(case);
    let calls = call_edges(&snapshot(&MergedGraph::new(observed.full(), None)));
    for (caller, callees) in case.expected {
        let mut actual: Vec<(&str, &str, &str)> = calls
            .iter()
            .filter(|(source, ..)| source == caller)
            .map(|(_, callee, file, confidence)| {
                (callee.as_str(), file.as_str(), confidence.as_str())
            })
            .collect();
        actual.sort();
        let names: Vec<(&str, &str)> = actual.iter().map(|&(n, f, _)| (n, f)).collect();
        let mut want: Vec<(&str, &str)> = callees.iter().map(|&(n, f, _)| (n, f)).collect();
        want.sort();
        assert_eq!(
            names, want,
            "{}: index Calls edges of {caller}: {actual:?}",
            case.name
        );
        for (name, file, confidence) in &actual {
            let (_, _, allowed) = callees
                .iter()
                .find(|(n, f, _)| n == name && f == file)
                .unwrap();
            assert!(
                allowed.iter().any(|c| format!("{c:?}") == *confidence),
                "{}: {caller} -> {name} at {confidence}, allowed {allowed:?}",
                case.name
            );
        }
    }
    let differing = differing_calls(&observed);
    assert!(
        differing.is_empty(),
        "{}: the overlay's Calls edges differ from the full index: {differing:#?}",
        case.name
    );
}

#[test]
fn test_ladder_call_entry_typescript_same_both_sides() {
    check(&TYPESCRIPT);
}

#[test]
fn test_ladder_call_entry_javascript_same_both_sides() {
    check(&JAVASCRIPT);
}

#[test]
fn test_ladder_call_entry_python_same_both_sides() {
    check(&PYTHON);
}

#[test]
fn test_ladder_call_entry_java_same_both_sides() {
    check(&JAVA);
}

#[test]
fn test_ladder_call_entry_kotlin_same_both_sides() {
    check(&KOTLIN);
}

#[test]
fn test_ladder_call_entry_csharp_same_both_sides() {
    check(&CSHARP);
}

#[test]
fn test_ladder_call_entry_go_same_both_sides() {
    check(&GO);
}

#[test]
fn test_ladder_call_entry_rust_same_both_sides() {
    check(&RUST);
}

#[test]
fn test_ladder_call_entry_php_same_both_sides() {
    check(&PHP);
}

#[test]
fn test_ladder_call_entry_ruby_same_both_sides() {
    check(&RUBY);
}

#[test]
fn test_ladder_call_entry_swift_same_both_sides() {
    check(&SWIFT);
}

#[test]
fn test_ladder_call_entry_c_same_both_sides() {
    check(&C);
}

#[test]
fn test_ladder_call_entry_cpp_same_both_sides() {
    check(&CPP);
}

#[test]
fn test_ladder_call_entry_dart_same_both_sides() {
    check(&DART);
}

/// The divergence the slice keeps (`ladder::TypeName`): `new ::Widget()`
/// names a type path with an empty qualifier. The index's gate reads the
/// whole path, which no type declares, so `make` calls nothing; the
/// overlay's gate reads `Widget`, and its last-segment retry binds the
/// class's constructor. The fix that converges them flips this test: the
/// overlay line goes away.
#[test]
fn test_ladder_call_entry_cpp_empty_qualifier_construct_overlay_only() {
    let case = Case {
        name: "ladder_call_entry_cpp_empty_qualifier",
        files: &[
            (
                "src/widget.hpp",
                "class Widget {\npublic:\n    Widget() {}\n};\n",
            ),
            (
                "src/app.cpp",
                "#include \"widget.hpp\"\n\nWidget* make() {\n    return new ::Widget();\n}\n",
            ),
        ],
        dirty: "src/app.cpp",
        expected: &[],
    };
    let observed = observe_case(&case);
    let calls: Vec<_> = call_edges(&snapshot(&MergedGraph::new(observed.full(), None)))
        .into_iter()
        .filter(|(caller, ..)| caller == "make")
        .collect();
    assert!(calls.is_empty(), "the index binds no type: {calls:?}");
    let differing: Vec<String> = differing_calls(&observed).into_iter().collect();
    assert_eq!(differing.len(), 1, "{differing:#?}");
    assert!(
        differing[0].starts_with("edge overlay-only x1 Calls ")
            && differing[0].contains(" make ")
            && differing[0].contains("src/widget.hpp"),
        "only the overlay constructs `::Widget`: {differing:#?}"
    );
}
