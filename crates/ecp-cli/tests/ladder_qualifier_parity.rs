//! R11a (B7): the qualifier tier (R13), the Rust module-tree position after
//! it (R14) and the Python `super()` rule (R12) run once, in
//! `ecp_core::analyzer::ladder::qualified`, for the index and the overlay.
//! Each case plants one fact in a small tree. It asserts what a reader of a
//! fresh full index gets (each listed caller's exact `Calls` targets, at an
//! allowed confidence), then that the overlay view of the same tree (the
//! caller files dirty, unchanged but for a trailing newline; a removed file
//! tombstoned) has no `Calls` edge that differs from that index, by source
//! uid, target uid and exact confidence (`common/strict_parity.rs`). A
//! caller the case does not list is held to parity only.
//!
//! Every fixture carries a decoy that a wrong tier would bind (the same
//! member on another type in another file, or a free namesake), so an
//! empty expectation is not a vacuous pass.
//!
//! C is exempt: it has no methods and no namespaces, so no call is
//! qualified (`overlay_qualified_call_parity.rs:21`):
//!
//! ```text
//! ladder_qualifier c C has no methods or namespaces (overlay_qualified_call_parity.rs:21)
//! ```
//!
//! The divergences the slice keeps, each behind one parameter, have unit
//! fixtures in `ecp_core::analyzer::ladder`'s tests (`StemFile`) or stay
//! the overlay's own facts (a workspace-crate `use` head, `RustTail`).

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use ecp_core::session::merged::MergedGraph;
use std::collections::BTreeSet;
use strict_parity::{run, snapshot, Edit, Observed, Scenario, Snapshot};

type Files<'a> = &'a [(&'a str, &'a str)];

const QUALIFIER_SCOPED: &[f32] = &[0.85];
/// A Rust module path: the file-name step (0.85) or the module tree (1.0).
const MODULE_PATH: &[f32] = &[0.85, 1.0];
const TYPE_OWNED: &[f32] = &[0.85];
const TYPE_HERITAGE: &[f32] = &[0.8];

/// What one caller must call: `(callee name, callee file, allowed confidences)`.
type Callees<'a> = &'a [(&'a str, &'a str, &'a [f32])];

/// One case: the tree, the caller files the overlay dirties, the files the
/// edit removes, and, per caller, what it must call in a fresh full index of
/// the edited tree.
struct Case {
    name: &'static str,
    files: Files<'static>,
    dirty: &'static [&'static str],
    removed: &'static [&'static str],
    expected: &'static [(&'static str, Callees<'static>)],
}

/// Per language, a typed receiver `k.save()` and a static `K.st()` bind in
/// `K`'s file, never in the decoy `J`'s.
const TYPESCRIPT: Case = Case {
    name: "ladder_qualifier_typescript",
    files: &[
        (
            "k.ts",
            "export class K {\n save(id: number) {}\n static st(id: number) {}\n}\n",
        ),
        (
            "j.ts",
            "export class J {\n save(id: number) {}\n static st(id: number) {}\n}\nexport function st(id: number) {}\n",
        ),
        (
            "app.ts",
            "import { K } from './k';\nclass App {\n run(k: K) { k.save(1); K.st(2); }\n}\n",
        ),
    ],
    dirty: &["app.ts"],
    removed: &[],
    expected: &[(
        "run",
        &[
            ("save", "k.ts", QUALIFIER_SCOPED),
            ("st", "k.ts", QUALIFIER_SCOPED),
        ],
    )],
};

/// `k.save()` on an untyped `k` binds through the file-name step: the
/// qualifier `k` names `k.js`.
const JAVASCRIPT: Case = Case {
    name: "ladder_qualifier_javascript",
    files: &[
        (
            "k.js",
            "export class K {\n save(id) {}\n static st(id) {}\n}\n",
        ),
        (
            "j.js",
            "export class J {\n save(id) {}\n static st(id) {}\n}\n",
        ),
        (
            "app.js",
            "import { K } from './k';\nclass App {\n run(k) { k.save(1); K.st(2); }\n}\n",
        ),
    ],
    dirty: &["app.js"],
    removed: &[],
    expected: &[(
        "run",
        &[
            ("save", "k.js", QUALIFIER_SCOPED),
            ("st", "k.js", QUALIFIER_SCOPED),
        ],
    )],
};

/// `Gone` names no file: no edge, though `j.py` holds an `st`. `Dup` is
/// defined in two files and a local variable is no qualifier: parity only.
const PYTHON: Case = Case {
    name: "ladder_qualifier_python",
    files: &[
        (
            "k.py",
            "class K:\n    def save(self, id): pass\n    @staticmethod\n    def st(id): pass\n",
        ),
        (
            "j.py",
            "class J:\n    def save(self, id): pass\n    @staticmethod\n    def st(id): pass\n",
        ),
        ("d1.py", "class Dup:\n    @staticmethod\n    def st(id): pass\n"),
        ("d2.py", "class Dup:\n    @staticmethod\n    def st(id): pass\n"),
        (
            "app.py",
            "from k import K\n\nclass App:\n    def run(self, k: K):\n        k.save(1)\n        K.st(2)\n\n    def missing(self):\n        Gone.st(3)\n\n    def two(self):\n        Dup.st(4)\n\n    def local(self):\n        v = object()\n        v.save(5)\n",
        ),
    ],
    dirty: &["app.py"],
    removed: &[],
    expected: &[
        (
            "run",
            &[
                ("save", "k.py", QUALIFIER_SCOPED),
                ("st", "k.py", QUALIFIER_SCOPED),
            ],
        ),
        ("missing", &[]),
    ],
};

const JAVA: Case = Case {
    name: "ladder_qualifier_java",
    files: &[
        (
            "src/p/K.java",
            "package p;\nclass K {\n void save(int id) {}\n static void st(int id) {}\n}\n",
        ),
        (
            "src/p/J.java",
            "package p;\nclass J {\n void save(int id) {}\n static void st(int id) {}\n}\n",
        ),
        (
            "src/p/App.java",
            "package p;\nclass App {\n void run(K k) { k.save(1); K.st(2); }\n}\n",
        ),
    ],
    dirty: &["src/p/App.java"],
    removed: &[],
    expected: &[(
        "run",
        &[
            ("save", "src/p/K.java", QUALIFIER_SCOPED),
            ("st", "src/p/K.java", QUALIFIER_SCOPED),
        ],
    )],
};

const KOTLIN: Case = Case {
    name: "ladder_qualifier_kotlin",
    files: &[
        (
            "src/p/K.kt",
            "package p\nclass K {\n fun save(id: Int) {}\n companion object {\n  fun st(id: Int) {}\n }\n}\n",
        ),
        (
            "src/p/J.kt",
            "package p\nclass J {\n fun save(id: Int) {}\n companion object {\n  fun st(id: Int) {}\n }\n}\n",
        ),
        (
            "src/p/App.kt",
            "package p\nclass App {\n fun run(k: K) { k.save(1); K.st(2) }\n}\n",
        ),
    ],
    dirty: &["src/p/App.kt"],
    removed: &[],
    expected: &[(
        "run",
        &[
            ("save", "src/p/K.kt", QUALIFIER_SCOPED),
            ("st", "src/p/K.kt", QUALIFIER_SCOPED),
        ],
    )],
};

const CSHARP: Case = Case {
    name: "ladder_qualifier_csharp",
    files: &[
        (
            "K.cs",
            "namespace N {\nclass K {\n public void save(int id) {}\n public static void st(int id) {}\n}\n}\n",
        ),
        (
            "J.cs",
            "namespace N {\nclass J {\n public void save(int id) {}\n public static void st(int id) {}\n}\n}\n",
        ),
        (
            "App.cs",
            "namespace N {\nclass App {\n void run(K k) { k.save(1); K.st(2); }\n}\n}\n",
        ),
    ],
    dirty: &["App.cs"],
    removed: &[],
    expected: &[(
        "run",
        &[
            ("save", "K.cs", QUALIFIER_SCOPED),
            ("st", "K.cs", QUALIFIER_SCOPED),
        ],
    )],
};

/// Go has no static methods: the typed receiver only.
const GO: Case = Case {
    name: "ladder_qualifier_go",
    files: &[
        (
            "k.go",
            "package app\ntype K struct{}\nfunc (k *K) save(id int) {}\n",
        ),
        (
            "j.go",
            "package app\ntype J struct{}\nfunc (j *J) save(id int) {}\n",
        ),
        ("app.go", "package app\nfunc run(k *K) { k.save(1) }\n"),
    ],
    dirty: &["app.go"],
    removed: &[],
    expected: &[("run", &[("save", "k.go", QUALIFIER_SCOPED)])],
};

/// A `use`-imported type, and module paths anchored at `crate::`, `self::`
/// and `super::`. `src/other.rs` holds the decoy free `helper`.
const RUST: Case = Case {
    name: "ladder_qualifier_rust",
    files: &[
        (
            "src/k.rs",
            "pub struct K;\nimpl K {\n pub fn save(&self, id: i32) {}\n pub fn st(id: i32) {}\n}\n",
        ),
        (
            "src/j.rs",
            "pub struct J;\nimpl J {\n pub fn save(&self, id: i32) {}\n pub fn st(id: i32) {}\n}\n",
        ),
        ("src/util.rs", "pub mod deep;\npub fn helper() {}\n"),
        (
            "src/util/deep.rs",
            "pub fn via_super() { super::helper(); }\n",
        ),
        ("src/other.rs", "pub fn helper() {}\n"),
        (
            "src/lib.rs",
            "mod k;\nmod j;\nmod util;\nmod other;\nuse crate::k::K;\nfn run(k: &K) { k.save(1); K::st(2); }\nfn via_crate() { crate::util::helper(); }\nfn via_self() { self::util::helper(); }\n",
        ),
    ],
    dirty: &["src/lib.rs", "src/util/deep.rs"],
    removed: &[],
    expected: &[
        (
            "run",
            &[
                ("save", "src/k.rs", QUALIFIER_SCOPED),
                ("st", "src/k.rs", QUALIFIER_SCOPED),
            ],
        ),
        ("via_crate", &[("helper", "src/util.rs", MODULE_PATH)]),
        ("via_self", &[("helper", "src/util.rs", MODULE_PATH)]),
        ("via_super", &[("helper", "src/util.rs", MODULE_PATH)]),
    ],
};

/// A Rust path into a module file the edit removes binds nothing, though
/// `src/other.rs` holds the only other `f`.
const RUST_REMOVED_FILE: Case = Case {
    name: "ladder_qualifier_rust_removed_file",
    files: &[
        ("src/gone.rs", "pub fn f() {}\n"),
        ("src/other.rs", "pub fn f() {}\n"),
        (
            "src/lib.rs",
            "mod gone;\nmod other;\nfn run_gone() { crate::gone::f(); }\n",
        ),
    ],
    dirty: &["src/lib.rs"],
    removed: &["src/gone.rs"],
    expected: &[("run_gone", &[])],
};

/// `$k->save()` binds by name before the qualifier tier: the static call
/// only.
const PHP: Case = Case {
    name: "ladder_qualifier_php",
    files: &[
        (
            "K.php",
            "<?php\nclass K {\n function save($id) {}\n static function st($id) {}\n}\n",
        ),
        (
            "J.php",
            "<?php\nclass J {\n function save($id) {}\n static function st($id) {}\n}\n",
        ),
        (
            "App.php",
            "<?php\nclass App {\n function run() { K::st(2); }\n}\n",
        ),
    ],
    dirty: &["App.php"],
    removed: &[],
    expected: &[("run", &[("st", "K.php", QUALIFIER_SCOPED)])],
};

/// An untyped `k.save` binds by name before the qualifier tier: the class
/// call only.
const RUBY: Case = Case {
    name: "ladder_qualifier_ruby",
    files: &[
        (
            "k.rb",
            "class K\n  def save(id); end\n  def self.st(id); end\nend\n",
        ),
        (
            "j.rb",
            "class J\n  def save(id); end\n  def self.st(id); end\nend\n",
        ),
        ("app.rb", "class App\n  def run\n    K.st(2)\n  end\nend\n"),
    ],
    dirty: &["app.rb"],
    removed: &[],
    expected: &[("run", &[("st", "k.rb", QUALIFIER_SCOPED)])],
};

const SWIFT: Case = Case {
    name: "ladder_qualifier_swift",
    files: &[
        (
            "K.swift",
            "class K {\n func save(id: Int) {}\n static func st(id: Int) {}\n}\n",
        ),
        (
            "J.swift",
            "class J {\n func save(id: Int) {}\n static func st(id: Int) {}\n}\n",
        ),
        (
            "App.swift",
            "class App {\n func run(k: K) { k.save(id: 1); K.st(id: 2) }\n}\n",
        ),
    ],
    dirty: &["App.swift"],
    removed: &[],
    expected: &[(
        "run",
        &[
            ("save", "K.swift", QUALIFIER_SCOPED),
            ("st", "K.swift", QUALIFIER_SCOPED),
        ],
    )],
};

/// The caller's own file declares `K`: the first step. A declaration in
/// `k.h` defined in `k.cpp` is ambiguous across files, so `K` stays in the
/// caller's file here; `j.cpp` holds the decoy.
const CPP: Case = Case {
    name: "ladder_qualifier_cpp",
    files: &[
        (
            "j.cpp",
            "class J {\npublic:\n void save(int id) {}\n static void st(int id) {}\n};\n",
        ),
        (
            "app.cpp",
            "class K {\npublic:\n void save(int id) {}\n static void st(int id) {}\n};\nclass App {\n void run(K& k) { k.save(1); K::st(2); }\n};\n",
        ),
    ],
    dirty: &["app.cpp"],
    removed: &[],
    expected: &[(
        "run",
        &[
            ("save", "app.cpp", QUALIFIER_SCOPED),
            ("st", "app.cpp", QUALIFIER_SCOPED),
        ],
    )],
};

const DART: Case = Case {
    name: "ladder_qualifier_dart",
    files: &[
        (
            "lib/k.dart",
            "class K {\n void save(int id) {}\n static void st(int id) {}\n}\n",
        ),
        (
            "lib/j.dart",
            "class J {\n void save(int id) {}\n static void st(int id) {}\n}\n",
        ),
        (
            "lib/app.dart",
            "import 'k.dart';\nclass App {\n void run(K k) { k.save(1); K.st(2); }\n}\n",
        ),
    ],
    dirty: &["lib/app.dart"],
    removed: &[],
    expected: &[(
        "run",
        &[
            ("save", "lib/k.dart", QUALIFIER_SCOPED),
            ("st", "lib/k.dart", QUALIFIER_SCOPED),
        ],
    )],
};

/// FU-2026-10-05-bb4eba1b9224: `crate::x::parse` is a module path, and
/// `P::parse` is no member of the module `x`, so nothing binds, though
/// `src/y.rs` holds a free `parse`. The module's own free function binds,
/// and `P::parse` binds through its type.
const RUST_MODULE_PATH_METHOD: Case = Case {
    name: "ladder_qualifier_rust_module_path_method",
    files: &[
        (
            "src/x.rs",
            "pub struct P;\nimpl P {\n pub fn parse() {}\n}\npub fn other() {}\n",
        ),
        ("src/y.rs", "pub fn parse() {}\n"),
        (
            "src/lib.rs",
            "mod x;\nmod y;\nuse crate::x::P;\nfn run_path() { crate::x::parse(); }\nfn run_free() { crate::x::other(); }\nfn run_type() { P::parse(); }\n",
        ),
    ],
    dirty: &["src/lib.rs"],
    removed: &[],
    expected: &[
        ("run_path", &[]),
        ("run_free", &[("other", "src/x.rs", MODULE_PATH)]),
        ("run_type", &[("parse", "src/x.rs", QUALIFIER_SCOPED)]),
    ],
};

/// R12: `super().m()` takes the first base that owns `m` (`Two` reads
/// `Other` before `Base`); a base that inherits it binds the owner at the
/// heritage tier. A class with no base and a call outside a class bind
/// nothing, though `lib/util.py` holds a free `a` and `b`.
const PYTHON_SUPER: Case = Case {
    name: "ladder_super_member_python",
    files: &[
        ("app/__init__.py", ""),
        ("lib/__init__.py", ""),
        (
            "app/base.py",
            "class Base:\n    def a(self):\n        return 1\n\n    def b(self):\n        return 2\n",
        ),
        (
            "app/other.py",
            "class Other:\n    def b(self):\n        return 3\n",
        ),
        (
            "lib/util.py",
            "def a():\n    return 4\n\n\ndef b():\n    return 5\n",
        ),
        (
            "app/sub.py",
            "from app.base import Base\nfrom app.other import Other\n\n\nclass Sub(Base):\n    def call_a(self):\n        return super().a()\n\n\nclass Two(Other, Base):\n    def call_b(self):\n        return super().b()\n\n\nclass Mid(Base):\n    pass\n\n\nclass Deep(Mid):\n    def call_deep(self):\n        return super().a()\n\n\nclass Lone:\n    def call_lone(self):\n        return super().a()\n\n\ndef outside():\n    return super().a()\n",
        ),
    ],
    dirty: &["app/sub.py"],
    removed: &[],
    expected: &[
        ("call_a", &[("a", "app/base.py", TYPE_OWNED)]),
        ("call_b", &[("b", "app/other.py", TYPE_OWNED)]),
        ("call_deep", &[("a", "app/base.py", TYPE_HERITAGE)]),
        ("call_lone", &[]),
        ("outside", &[]),
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

/// `(caller, callee, callee file, confidence)` of every `rel` edge.
fn edges(snap: &Snapshot, rel: &str) -> Vec<(String, String, String, String)> {
    let prefix = format!("{rel} ");
    snap.edge
        .iter()
        .filter_map(|line| {
            let (source, target) = line.strip_prefix(prefix.as_str())?.split_once(" -> ")?;
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
    let edited: Vec<(&str, String)> = case
        .dirty
        .iter()
        .map(|&path| {
            let (_, source) = case
                .files
                .iter()
                .find(|(p, _)| *p == path)
                .expect("a dirty file is one of the files");
            (path, format!("{source}\n"))
        })
        .collect();
    let edits: Vec<Edit<'_>> = edited
        .iter()
        .map(|(path, source)| Edit::Write(path, source.as_str()))
        .chain(case.removed.iter().map(|&path| Edit::Delete(path)))
        .collect();
    run(&Scenario {
        name: case.name,
        owner: "R11a (B7 qualifier tier)",
        files: case.files,
        edits: &edits,
    })
}

/// Hold the full index of `case`'s edited tree to `case.expected`, then the
/// overlay's `Calls` edges to the index's.
fn check(case: &Case) {
    let observed = observe_case(case);
    let calls = edges(&snapshot(&MergedGraph::new(observed.full(), None)), "Calls");
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
    let differing: BTreeSet<String> = observed
        .lines()
        .into_iter()
        .filter(|line| line.starts_with("edge ") && line.contains(" Calls "))
        .collect();
    assert!(
        differing.is_empty(),
        "{}: the overlay's Calls edges differ from the full index: {differing:#?}",
        case.name
    );
}

#[test]
fn test_ladder_qualifier_typescript_same_both_sides() {
    check(&TYPESCRIPT);
}

#[test]
fn test_ladder_qualifier_javascript_same_both_sides() {
    check(&JAVASCRIPT);
}

#[test]
fn test_ladder_qualifier_python_same_both_sides() {
    check(&PYTHON);
}

#[test]
fn test_ladder_qualifier_java_same_both_sides() {
    check(&JAVA);
}

#[test]
fn test_ladder_qualifier_kotlin_same_both_sides() {
    check(&KOTLIN);
}

#[test]
fn test_ladder_qualifier_csharp_same_both_sides() {
    check(&CSHARP);
}

#[test]
fn test_ladder_qualifier_go_same_both_sides() {
    check(&GO);
}

#[test]
fn test_ladder_qualifier_rust_same_both_sides() {
    check(&RUST);
}

#[test]
fn test_ladder_qualifier_rust_removed_file_same_both_sides() {
    check(&RUST_REMOVED_FILE);
}

#[test]
fn test_ladder_qualifier_php_same_both_sides() {
    check(&PHP);
}

#[test]
fn test_ladder_qualifier_ruby_same_both_sides() {
    check(&RUBY);
}

#[test]
fn test_ladder_qualifier_swift_same_both_sides() {
    check(&SWIFT);
}

#[test]
fn test_ladder_qualifier_cpp_same_both_sides() {
    check(&CPP);
}

#[test]
fn test_ladder_qualifier_dart_same_both_sides() {
    check(&DART);
}

#[test]
fn test_ladder_qualifier_rust_module_path_never_binds_method() {
    check(&RUST_MODULE_PATH_METHOD);
}

#[test]
fn test_ladder_super_member_python_overlay_matches_index() {
    check(&PYTHON_SUPER);
}
