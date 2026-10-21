//! Calls whose target the index picks by owner, out of a dirty file: the
//! session overlay binds the same `Calls` edges as a full reindex of the
//! edited tree. The overlay shares the index's rules
//! (`ecp_core::analyzer::same_file_pick`, `ecp_core::analyzer::type_ladder`)
//! and must replay them with the same inputs:
//!
//! | construct                               | rule                                              |
//! |-----------------------------------------|---------------------------------------------------|
//! | untyped receiver, two unrelated owners  | no owner to choose by: no edge                    |
//! | C++ namespace owner                     | a namespace member counts as a free function      |
//! | Rust `self.m()`, two impl blocks of `K` | the caller's own impl block                       |
//! | C# / Kotlin / Swift / Dart `Util.make()`| a type receiver resolves as a type, not the bare member |
//! | bare `m()` named like a class           | a member whose receiver the parser dropped: no construction |
//!
//! Each fixture edits the caller's file (a trailing blank line), so its
//! calls re-resolve in the overlay.

#[path = "common/parity.rs"]
mod parity;

use ecp_core::analyzer::types::LocalGraph;
use parity::{calls_after_edit, Parity};
use std::path::Path;

struct Fixture {
    label: &'static str,
    files: &'static [(&'static str, &'static str)],
    /// The caller's file, which the edit dirties.
    caller: &'static str,
    caller_fn: &'static str,
    callee: &'static str,
    /// `(file, 1-based line)` of the one declaration the reindex binds
    /// `callee` to; `None` when it binds none.
    bound: Option<(&'static str, u32)>,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        label: "Python/untyped receiver, two owners",
        files: &[(
            "app.py",
            "class A:\n    def save(self): pass\n\nclass B:\n    def save(self): pass\n\ndef run(x):\n    x.save()\n",
        )],
        caller: "app.py",
        caller_fn: "run",
        callee: "save",
        bound: None,
    },
    Fixture {
        label: "Java/untyped receiver, two owners",
        files: &[(
            "App.java",
            "class A { void save() {} }\nclass B { void save() {} }\nclass App {\n void run() { make().save(); }\n}\n",
        )],
        caller: "App.java",
        caller_fn: "run",
        callee: "save",
        bound: None,
    },
    Fixture {
        label: "Swift/untyped receiver, two owners",
        files: &[(
            "app.swift",
            "class A { func save() {} }\nclass B { func save() {} }\nfunc run() { x.save() }\n",
        )],
        caller: "app.swift",
        caller_fn: "run",
        callee: "save",
        bound: None,
    },
    Fixture {
        label: "Ruby/untyped receiver, two owners",
        files: &[(
            "app.rb",
            "class A\n  def save; end\nend\nclass B\n  def save; end\nend\ndef run(x)\n  x.save\nend\n",
        )],
        caller: "app.rb",
        caller_fn: "run",
        callee: "save",
        bound: None,
    },
    Fixture {
        label: "C++/untyped receiver, two owners",
        files: &[(
            "app.cpp",
            "class A { public: void save() {} };\nclass B { public: void save() {} };\nvoid run() { x.save(); }\n",
        )],
        caller: "app.cpp",
        caller_fn: "run",
        callee: "save",
        bound: None,
    },
    Fixture {
        label: "C++/namespace owner",
        files: &[(
            "app.cpp",
            "namespace ns {\nvoid save() {}\nclass A { public: void save() {} };\nvoid run() { save(); }\n}\n",
        )],
        caller: "app.cpp",
        caller_fn: "run",
        callee: "save",
        bound: Some(("app.cpp", 2)),
    },
    Fixture {
        label: "Rust/two impl blocks for one type",
        files: &[(
            "src/lib.rs",
            "pub struct K;\nimpl K {\n    fn save(&self) {}\n    fn run(&self) { self.save(); }\n}\npub trait T { fn save(&self); }\nimpl T for K {\n    fn save(&self) {}\n}\n",
        )],
        caller: "src/lib.rs",
        caller_fn: "run",
        callee: "save",
        bound: Some(("src/lib.rs", 3)),
    },
    Fixture {
        label: "C#/type receiver",
        files: &[
            ("Util.cs", "static class Util {\n public static void Make() {}\n}\n"),
            ("App.cs", "class App {\n void Make() {}\n void Run() { Util.Make(); }\n}\n"),
        ],
        caller: "App.cs",
        caller_fn: "Run",
        callee: "Make",
        bound: Some(("Util.cs", 2)),
    },
    Fixture {
        label: "Kotlin/type receiver",
        files: &[
            ("src/p/Util.kt", "package p\nclass Util {\n companion object {\n  fun make() {}\n }\n}\n"),
            ("src/p/App.kt", "package p\nclass App {\n fun make() {}\n fun run() { Util.make() }\n}\n"),
        ],
        caller: "src/p/App.kt",
        caller_fn: "run",
        callee: "make",
        bound: Some(("src/p/Util.kt", 4)),
    },
    Fixture {
        label: "Swift/type receiver",
        files: &[
            ("Util.swift", "class Util {\n static func make() {}\n}\n"),
            ("App.swift", "class App {\n func make() {}\n func run() { Util.make() }\n}\n"),
        ],
        caller: "App.swift",
        caller_fn: "run",
        callee: "make",
        bound: Some(("Util.swift", 2)),
    },
    Fixture {
        label: "Dart/type receiver",
        files: &[
            ("lib/util.dart", "class Util {\n static void make() {}\n}\n"),
            ("lib/app.dart", "import 'util.dart';\nclass App {\n void make() {}\n void run() { Util.make(); }\n}\n"),
        ],
        caller: "lib/app.dart",
        caller_fn: "run",
        callee: "make",
        bound: Some(("lib/util.dart", 2)),
    },
    Fixture {
        label: "Kotlin/bare member named like a class",
        files: &[
            ("src/p/Num.kt", "package p\nclass number_integer\n"),
            (
                "src/p/Sax.kt",
                "package p\nclass Sax {\n fun number_integer(v: Int) {}\n}\nclass Sax2 {\n fun number_integer(v: Int) {}\n}\n",
            ),
            ("src/p/App.kt", "package p\nclass App {\n fun run() { number_integer(1) }\n}\n"),
        ],
        caller: "src/p/App.kt",
        caller_fn: "run",
        callee: "number_integer",
        bound: None,
    },
    Fixture {
        label: "Dart/bare member named like a class",
        files: &[
            ("lib/num.dart", "class number_integer {}\n"),
            (
                "lib/sax.dart",
                "class Sax {\n void number_integer(int v) {}\n}\nclass Sax2 {\n void number_integer(int v) {}\n}\n",
            ),
            ("lib/app.dart", "class App {\n void run() { number_integer(1); }\n}\n"),
        ],
        caller: "lib/app.dart",
        caller_fn: "run",
        callee: "number_integer",
        bound: None,
    },
];

fn parse(path: &str, source: &str) -> LocalGraph {
    ecp_cli::reanalyze::pipeline()
        .parse_file_raw(Path::new(path), source.as_bytes())
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn test_overlay_owner_pick_matches_reindex_across_languages() {
    let mut failures = Vec::new();
    for fixture in FIXTURES {
        let source = fixture
            .files
            .iter()
            .find(|(path, _)| *path == fixture.caller)
            .map(|(_, source)| *source)
            .expect("the caller's file is a fixture file");
        let edited = format!("{source}\n");
        let Parity { reindex, overlay } = calls_after_edit(
            &parse,
            fixture.files,
            &[(fixture.caller, edited.as_str())],
            &|file, name| file == fixture.caller && name == fixture.caller_fn,
        );
        let hits: Vec<_> = reindex
            .iter()
            .filter(|edge| edge.callee == fixture.callee)
            .collect();
        let proves = match fixture.bound {
            None => hits.is_empty(),
            Some((file, line)) => matches!(
                hits.as_slice(),
                [edge] if edge.callee_file == file && edge.callee_line == line
            ),
        };
        if !proves {
            failures.push(format!(
                "{}: the reindex does not bind `{}` as {:?}, so the fixture proves nothing: {reindex:#?}",
                fixture.label, fixture.callee, fixture.bound
            ));
            continue;
        }
        if overlay != reindex {
            failures.push(format!(
                "{}: overlay {overlay:#?}\nreindex {reindex:#?}",
                fixture.label
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
