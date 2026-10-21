//! Qualified calls out of a dirty file: the session overlay binds the same
//! `Calls` edges as a full reindex of the edited tree, over the 14
//! mainstream languages. Two constructs, each where the language has it and
//! the index resolves it today:
//!
//! - a member call on a typed receiver (`k.save()` with `k: K`), which the
//!   parsers record as `K.save`;
//! - a class-qualified static call (`K.st()`, `K::st()`).
//!
//! The index binds both through its qualifier tier (0.85): the member in the
//! file that defines the qualifier. Each fixture edits the caller (every line
//! below the first moves down one); a cross-file fixture also runs with the
//! callee's file edited.
//!
//! Left out, because the index binds nothing to assert parity on:
//!
//! | language   | construct            | reason                                                   |
//! |------------|----------------------|----------------------------------------------------------|
//! | JavaScript | typed receiver       | no parameter types; same-file `k.save()` stays unbound   |
//! | Go         | static call          | Go has no static methods                                 |
//! | C          | both                 | C has no methods                                         |
//! | C++        | cross-file receiver  | `save` is declared in `k.h` and defined in `k.cpp`: ambiguous |
//!
//! Pairs the index already bound by name before this tier (PHP `$k->save()`,
//! Ruby `k.save`, ...) stay in the fixtures as
//! regression guards with their own confidence.

#[path = "common/parity.rs"]
mod parity;

use ecp_core::analyzer::types::LocalGraph;
use parity::{calls_after_edit, Parity};
use std::path::Path;

struct Fixture {
    label: &'static str,
    files: &'static [(&'static str, &'static str)],
    caller: &'static str,
    /// The callee's file when it is not the caller's.
    callee: Option<&'static str>,
    /// `(callee, confidence)` the reindex binds from `run`.
    expected: &'static [(&'static str, f32)],
}

/// Fixtures whose overlay still differs from the reindex, with the reason.
/// Asserted to differ, so closing one turns the test red until it goes.
const KNOWN_DIVERGENCES: &[(&str, &str)] = &[];

const FIXTURES: &[Fixture] = &[
    Fixture {
        label: "TypeScript/same-file",
        files: &[("app.ts", "class K {\n save(id: number) {}\n static st(id: number) {}\n}\nclass App {\n run(k: K) { k.save(1); K.st(2); }\n}\n")],
        caller: "app.ts",
        callee: None,
        expected: &[("save", 0.85), ("st", 0.85)],
    },
    Fixture {
        label: "TypeScript/cross-file",
        files: &[
            ("k.ts", "export class K {\n save(id: number) {}\n static st(id: number) {}\n}\n"),
            ("app.ts", "import { K } from './k';\nclass App {\n run(k: K) { k.save(1); K.st(2); }\n}\n"),
        ],
        caller: "app.ts",
        callee: Some("k.ts"),
        expected: &[("save", 0.85), ("st", 0.85)],
    },
    Fixture {
        label: "JavaScript/same-file",
        files: &[("app.js", "class K {\n save(id) {}\n static st(id) {}\n}\nclass App {\n run(k) { k.save(1); K.st(2); }\n}\n")],
        caller: "app.js",
        callee: None,
        expected: &[("st", 0.85)],
    },
    // `k.save()` on an untyped `k` binds through the module-file tier: the
    // qualifier `k` names `k.js`.
    Fixture {
        label: "JavaScript/cross-file",
        files: &[
            ("k.js", "export class K {\n save(id) {}\n static st(id) {}\n}\n"),
            ("app.js", "import { K } from './k';\nclass App {\n run(k) { k.save(1); K.st(2); }\n}\n"),
        ],
        caller: "app.js",
        callee: Some("k.js"),
        expected: &[("save", 0.85), ("st", 0.85)],
    },
    Fixture {
        label: "Python/same-file",
        files: &[("app.py", "class K:\n    def save(self, id): pass\n    @staticmethod\n    def st(id): pass\n\nclass App:\n    def run(self, k: K):\n        k.save(1)\n        K.st(2)\n")],
        caller: "app.py",
        callee: None,
        expected: &[("save", 0.85), ("st", 0.85)],
    },
    Fixture {
        label: "Python/cross-file",
        files: &[
            ("k.py", "class K:\n    def save(self, id): pass\n    @staticmethod\n    def st(id): pass\n"),
            ("app.py", "from k import K\n\nclass App:\n    def run(self, k: K):\n        k.save(1)\n        K.st(2)\n"),
        ],
        caller: "app.py",
        callee: Some("k.py"),
        expected: &[("save", 0.85), ("st", 0.85)],
    },
    // The reported repro: both calls dropped while `K.java` was dirty.
    Fixture {
        label: "Java/same-file",
        files: &[("K.java", "class K {\n void audit() {}\n void save(int id) { audit(); }\n static void st(int id) {}\n}\nclass App {\n void run(K k) { k.save(1); K.st(2); }\n}\n")],
        caller: "K.java",
        callee: None,
        expected: &[("save", 0.85), ("st", 0.85)],
    },
    Fixture {
        label: "Java/cross-file",
        files: &[
            ("src/p/K.java", "package p;\nclass K {\n void save(int id) {}\n static void st(int id) {}\n}\n"),
            ("src/p/App.java", "package p;\nclass App {\n void run(K k) { k.save(1); K.st(2); }\n}\n"),
        ],
        caller: "src/p/App.java",
        callee: Some("src/p/K.java"),
        expected: &[("save", 0.85), ("st", 0.85)],
    },
    Fixture {
        label: "Kotlin/same-file",
        files: &[("app.kt", "class K {\n fun save(id: Int) {}\n companion object {\n  fun st(id: Int) {}\n }\n}\nclass App {\n fun run(k: K) { k.save(1); K.st(2) }\n}\n")],
        caller: "app.kt",
        callee: None,
        expected: &[("save", 0.85), ("st", 0.85)],
    },
    Fixture {
        label: "Kotlin/cross-file",
        files: &[
            ("src/p/K.kt", "package p\nclass K {\n fun save(id: Int) {}\n companion object {\n  fun st(id: Int) {}\n }\n}\n"),
            ("src/p/App.kt", "package p\nclass App {\n fun run(k: K) { k.save(1); K.st(2) }\n}\n"),
        ],
        caller: "src/p/App.kt",
        callee: Some("src/p/K.kt"),
        expected: &[("save", 0.85), ("st", 0.85)],
    },
    Fixture {
        label: "C#/same-file",
        files: &[("App.cs", "class K {\n public void save(int id) {}\n public static void st(int id) {}\n}\nclass App {\n void run(K k) { k.save(1); K.st(2); }\n}\n")],
        caller: "App.cs",
        callee: None,
        expected: &[("save", 0.85), ("st", 0.85)],
    },
    Fixture {
        label: "C#/cross-file",
        files: &[
            ("K.cs", "namespace N {\nclass K {\n public void save(int id) {}\n public static void st(int id) {}\n}\n}\n"),
            ("App.cs", "namespace N {\nclass App {\n void run(K k) { k.save(1); K.st(2); }\n}\n}\n"),
        ],
        caller: "App.cs",
        callee: Some("K.cs"),
        expected: &[("save", 0.85), ("st", 0.85)],
    },
    Fixture {
        label: "Go/same-file",
        files: &[("app.go", "package app\ntype K struct{}\nfunc (k *K) save(id int) {}\nfunc run(k *K) { k.save(1) }\n")],
        caller: "app.go",
        callee: None,
        expected: &[("save", 0.85)],
    },
    Fixture {
        label: "Go/cross-file",
        files: &[
            ("k.go", "package app\ntype K struct{}\nfunc (k *K) save(id int) {}\n"),
            ("app.go", "package app\nfunc run(k *K) { k.save(1) }\n"),
        ],
        caller: "app.go",
        callee: Some("k.go"),
        expected: &[("save", 0.85)],
    },
    Fixture {
        label: "Rust/same-file",
        files: &[("src/lib.rs", "struct K;\nimpl K {\n fn save(&self, id: i32) {}\n fn st(id: i32) {}\n}\nfn run(k: &K) { k.save(1); K::st(2); }\n")],
        caller: "src/lib.rs",
        callee: None,
        expected: &[("save", 0.85), ("st", 0.85)],
    },
    Fixture {
        label: "Rust/cross-file",
        files: &[
            ("src/lib.rs", "mod k;\nuse crate::k::K;\nfn run(k: &K) { k.save(1); K::st(2); }\n"),
            ("src/k.rs", "pub struct K;\nimpl K {\n pub fn save(&self, id: i32) {}\n pub fn st(id: i32) {}\n}\n"),
        ],
        caller: "src/lib.rs",
        callee: Some("src/k.rs"),
        expected: &[("save", 0.85), ("st", 0.85)],
    },
    // A method called through its type path: the `use` import names the
    // qualifier's file.
    Fixture {
        label: "Rust/use-imported type path",
        files: &[
            ("src/lib.rs", "mod k;\nuse crate::k::K;\nfn run(k: &K) { K::save(k, 1); }\n"),
            ("src/k.rs", "pub struct K;\nimpl K {\n pub fn save(&self, id: i32) {}\n}\n"),
        ],
        caller: "src/lib.rs",
        callee: Some("src/k.rs"),
        expected: &[("save", 0.85)],
    },
    // Parity only (`expected` is empty): the `as` alias and the glob each
    // reach the qualifier through a different import shape, and the overlay
    // must bind whatever the index binds, an empty set included.
    Fixture {
        label: "Rust/use as alias",
        files: &[
            ("src/lib.rs", "mod k;\nuse crate::k::K as Q;\nfn run() { Q::st(2); }\n"),
            ("src/k.rs", "pub struct K;\nimpl K {\n pub fn st(id: i32) {}\n}\n"),
        ],
        caller: "src/lib.rs",
        callee: Some("src/k.rs"),
        expected: &[],
    },
    Fixture {
        label: "Rust/use glob",
        files: &[
            ("src/lib.rs", "mod k;\nuse crate::k::*;\nfn run() { K::st(2); }\n"),
            ("src/k.rs", "pub struct K;\nimpl K {\n pub fn st(id: i32) {}\n}\n"),
        ],
        caller: "src/lib.rs",
        callee: Some("src/k.rs"),
        expected: &[],
    },
    Fixture {
        label: "PHP/same-file",
        files: &[("app.php", "<?php\nclass K {\n function save($id) {}\n static function st($id) {}\n}\nclass App {\n function run(K $k) { $k->save(1); K::st(2); }\n}\n")],
        caller: "app.php",
        callee: None,
        expected: &[("save", 1.0), ("st", 0.85)],
    },
    Fixture {
        label: "PHP/cross-file",
        files: &[
            ("K.php", "<?php\nclass K {\n function save($id) {}\n static function st($id) {}\n}\n"),
            ("App.php", "<?php\nclass App {\n function run(K $k) { $k->save(1); K::st(2); }\n}\n"),
        ],
        caller: "App.php",
        callee: Some("K.php"),
        expected: &[("save", 0.7), ("st", 0.85)],
    },
    Fixture {
        label: "Ruby/same-file",
        files: &[("app.rb", "class K\n  def save(id); end\n  def self.st(id); end\nend\nclass App\n  def run(k)\n    k.save(1)\n    K.st(2)\n  end\nend\n")],
        caller: "app.rb",
        callee: None,
        expected: &[("save", 1.0), ("st", 0.85)],
    },
    Fixture {
        label: "Ruby/cross-file",
        files: &[
            ("k.rb", "class K\n  def save(id); end\n  def self.st(id); end\nend\n"),
            ("app.rb", "class App\n  def run(k)\n    k.save(1)\n    K.st(2)\n  end\nend\n"),
        ],
        caller: "app.rb",
        callee: Some("k.rb"),
        expected: &[("save", 0.7), ("st", 0.85)],
    },
    Fixture {
        label: "Swift/same-file",
        files: &[("app.swift", "class K {\n func save(id: Int) {}\n static func st(id: Int) {}\n}\nclass App {\n func run(k: K) { k.save(id: 1); K.st(id: 2) }\n}\n")],
        caller: "app.swift",
        callee: None,
        expected: &[("save", 0.85), ("st", 0.85)],
    },
    Fixture {
        label: "Swift/cross-file",
        files: &[
            ("K.swift", "class K {\n func save(id: Int) {}\n static func st(id: Int) {}\n}\n"),
            ("App.swift", "class App {\n func run(k: K) { k.save(id: 1); K.st(id: 2) }\n}\n"),
        ],
        caller: "App.swift",
        callee: Some("K.swift"),
        expected: &[("save", 0.85), ("st", 0.85)],
    },
    Fixture {
        label: "C++/same-file",
        files: &[("app.cpp", "class K {\npublic:\n void save(int id) {}\n static void st(int id) {}\n};\nclass App {\n void run(K& k) { k.save(1); K::st(2); }\n};\n")],
        caller: "app.cpp",
        callee: None,
        expected: &[("save", 0.85), ("st", 0.85)],
    },
    Fixture {
        label: "C++/pointer receiver",
        files: &[("app.cpp", "class K {\npublic:\n void save(int id) {}\n};\nclass App {\n void run(K* k) { k->save(1); }\n};\n")],
        caller: "app.cpp",
        callee: None,
        expected: &[("save", 0.85)],
    },
    Fixture {
        label: "C++/cross-file",
        files: &[
            ("k.h", "class K {\npublic:\n void save(int id);\n static void st(int id);\n};\n"),
            ("k.cpp", "#include \"k.h\"\nvoid K::save(int id) {}\nvoid K::st(int id) {}\n"),
            ("app.cpp", "#include \"k.h\"\nclass App {\n void run(K& k) { k.save(1); K::st(2); }\n};\n"),
        ],
        caller: "app.cpp",
        callee: Some("k.h"),
        expected: &[("st", 0.85)],
    },
    Fixture {
        label: "Dart/same-file",
        files: &[("app.dart", "class K {\n void save(int id) {}\n static void st(int id) {}\n}\nclass App {\n void run(K k) { k.save(1); K.st(2); }\n}\n")],
        caller: "app.dart",
        callee: None,
        expected: &[("save", 0.85), ("st", 0.85)],
    },
    Fixture {
        label: "Dart/cross-file",
        files: &[
            ("lib/k.dart", "class K {\n void save(int id) {}\n static void st(int id) {}\n}\n"),
            ("lib/app.dart", "import 'k.dart';\nclass App {\n void run(K k) { k.save(1); K.st(2); }\n}\n"),
        ],
        caller: "lib/app.dart",
        callee: Some("lib/k.dart"),
        expected: &[("save", 0.85), ("st", 0.85)],
    },
];

/// Receiver forms other than a typed parameter that the parsers also record
/// as `Type.member`, and a construction through a namespace import.
const RECEIVER_FORMS: &[Fixture] = &[
    // J's namesake leaves the bare `st` ambiguous: only the type K binds it.
    Fixture {
        label: "Python/from-imported class with a namesake",
        files: &[
            ("k.py", "class K:\n    @staticmethod\n    def st(id): pass\n"),
            ("j.py", "class J:\n    @staticmethod\n    def st(id): pass\n"),
            ("app.py", "from k import K\n\ndef run():\n    K.st(2)\n"),
        ],
        caller: "app.py",
        callee: Some("k.py"),
        expected: &[("st", 0.85)],
    },
    Fixture {
        label: "TypeScript/this receiver",
        files: &[("app.ts", "class A {\n save() {}\n run() { this.save(); }\n}\n")],
        caller: "app.ts",
        callee: None,
        expected: &[("save", 0.85)],
    },
    Fixture {
        label: "Java/this receiver",
        files: &[("A.java", "class A {\n void save() {}\n void run() { this.save(); }\n}\n")],
        caller: "A.java",
        callee: None,
        expected: &[("save", 0.85)],
    },
    Fixture {
        label: "Kotlin/this receiver",
        files: &[("app.kt", "class A {\n fun save() {}\n fun run() { this.save() }\n}\n")],
        caller: "app.kt",
        callee: None,
        expected: &[("save", 0.85)],
    },
    Fixture {
        label: "C#/this receiver",
        files: &[("A.cs", "class A {\n void save() {}\n void run() { this.save(); }\n}\n")],
        caller: "A.cs",
        callee: None,
        expected: &[("save", 0.85)],
    },
    Fixture {
        label: "PHP/this receiver",
        files: &[("a.php", "<?php\nclass A {\n function save() {}\n function run() { $this->save(); }\n}\n")],
        caller: "a.php",
        callee: None,
        expected: &[("save", 0.85)],
    },
    Fixture {
        label: "Swift/self receiver",
        files: &[("a.swift", "class A {\n func save() {}\n func run() { self.save() }\n}\n")],
        caller: "a.swift",
        callee: None,
        expected: &[("save", 0.85)],
    },
    Fixture {
        label: "Dart/this receiver",
        files: &[("a.dart", "class A {\n void save() {}\n void run() { this.save(); }\n}\n")],
        caller: "a.dart",
        callee: None,
        expected: &[("save", 0.85)],
    },
    Fixture {
        label: "Ruby/self receiver",
        files: &[("a.rb", "class A\n  def save; end\n  def run\n    self.save\n  end\nend\n")],
        caller: "a.rb",
        callee: None,
        expected: &[("save", 0.85)],
    },
    Fixture {
        label: "C++/this receiver",
        files: &[("a.cpp", "class A {\n void save() {}\n void run() { this->save(); }\n};\n")],
        caller: "a.cpp",
        callee: None,
        expected: &[("save", 0.85)],
    },
    Fixture {
        label: "JavaScript/this receiver",
        files: &[("app.js", "class A {\n save() {}\n run() { this.save(); }\n}\n")],
        caller: "app.js",
        callee: None,
        expected: &[("save", 0.85)],
    },
    Fixture {
        label: "Java/typed local",
        files: &[("A.java", "class K { void save() {} }\nclass A {\n void run() { K k = new K(); k.save(); }\n}\n")],
        caller: "A.java",
        callee: None,
        expected: &[("save", 0.85)],
    },
    // The qualifier tier looks the member up by file, so an inherited member
    // declared in the same file binds too.
    Fixture {
        label: "C#/inherited member in the qualifier's file",
        files: &[("App.cs", "class B { public void save() {} }\nclass K : B {}\nclass App { void run(K k) { k.save(); } }\n")],
        caller: "App.cs",
        callee: None,
        expected: &[("save", 0.85)],
    },
    Fixture {
        label: "Rust/self receiver",
        files: &[("src/lib.rs", "struct K;\nimpl K {\n fn save(&self) {}\n fn run(&self) { self.save(); }\n}\n")],
        caller: "src/lib.rs",
        callee: None,
        expected: &[("save", 0.85)],
    },
    // The whole path `shop.Widget` resolves before its last segment does.
    Fixture {
        label: "TypeScript/namespace construction",
        files: &[
            ("shop.ts", "export class Widget {}\n"),
            ("app.ts", "import * as shop from './shop';\nfunction run() { return new shop.Widget(); }\n"),
        ],
        caller: "app.ts",
        callee: Some("shop.ts"),
        expected: &[("Widget", 0.95)],
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

/// Every mismatch of `fixtures`, one line each: a reindex that misses an
/// expected callee, an overlay that differs from the reindex, or a known
/// divergence that no longer differs.
fn mismatches(fixtures: &[Fixture]) -> Vec<String> {
    let mut failures = Vec::new();
    for fixture in fixtures {
        let source = |path: &str| {
            fixture
                .files
                .iter()
                .find(|(file, _)| *file == path)
                .map(|(_, source)| shifted(source))
                .unwrap()
        };
        let caller = (fixture.caller, source(fixture.caller));
        let mut dirty_sets = vec![("caller", vec![caller.clone()])];
        if let Some(callee) = fixture.callee {
            dirty_sets.push(("caller+callee", vec![caller, (callee, source(callee))]));
        }
        for (dirty_set, edits) in &dirty_sets {
            let edits: Vec<(&str, &str)> = edits.iter().map(|(p, s)| (*p, s.as_str())).collect();
            let Parity { reindex, overlay } =
                calls_after_edit(&parse, fixture.files, &edits, &|file, name| {
                    file == fixture.caller && name == "run"
                });
            let label = format!("{}/{dirty_set}", fixture.label);
            let missing: Vec<_> = fixture
                .expected
                .iter()
                .filter(|&&(callee, confidence)| {
                    !reindex
                        .iter()
                        .any(|e| e.callee == callee && e.confidence == confidence)
                })
                .collect();
            if !missing.is_empty() {
                failures.push(format!(
                    "{label}: the reindex lacks {missing:?}, so the fixture proves nothing: {reindex:#?}"
                ));
                continue;
            }
            let known = KNOWN_DIVERGENCES
                .iter()
                .find(|(known, _)| *known == fixture.label);
            match (known, overlay == reindex) {
                (None, true) | (Some(_), false) => {}
                (None, false) => failures.push(format!(
                    "{label}: overlay {overlay:#?}\nreindex {reindex:#?}"
                )),
                (Some((_, reason)), true) => failures.push(format!(
                    "{label}: known divergence ({reason}) is closed; remove it from KNOWN_DIVERGENCES"
                )),
            }
        }
    }
    failures
}

#[test]
fn test_overlay_qualified_call_matches_reindex_across_14_languages() {
    let failures = mismatches(FIXTURES);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn test_overlay_qualified_receiver_forms_match_reindex() {
    let failures = mismatches(RECEIVER_FORMS);
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
