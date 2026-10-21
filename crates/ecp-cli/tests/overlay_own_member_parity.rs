//! A call from a member of `K` out of a dirty file (`this.save()`,
//! `self.save()`, an implicit-`this` `save()`): the session overlay binds
//! `K`'s own `save`, as a full reindex does, never a same-named interface,
//! trait, protocol or base declaration that precedes it in the file (the
//! index's `SymbolTable::prefer_owned`). A qualified call reaches it through
//! the qualifier tier, a bare call through the same-file tier.
//!
//! The fixtures are those of ecp-analyzer's `own_member_precedence.rs`: `A`
//! declares `save` first, then `K` declares `audit`, its own `save` and
//! `run`, one function per line. The edit puts a blank line under the first
//! line, so every declaration below it moves down one.
//!
//! | language | status | reason                                               |
//! |----------|--------|------------------------------------------------------|
//! | C        | N/A    | C has no methods                                     |
//! | Go       | guard  | an interface method is no same-file node; no contest |

#[path = "common/parity.rs"]
mod parity;

use ecp_core::analyzer::types::LocalGraph;
use parity::{calls_after_edit, Parity};
use std::path::Path;

struct Fixture {
    label: &'static str,
    path: &'static str,
    source: &'static str,
    /// The 1-based line of `K`'s own `save` in `source` as written.
    own_save_line: u32,
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        label: "TypeScript",
        path: "k.ts",
        source: "interface A { save(): void; }\nclass K implements A {\n  audit() {}\n  save() { this.audit(); }\n  run() { this.save(); }\n}\n",
        own_save_line: 4,
    },
    Fixture {
        label: "JavaScript",
        path: "k.js",
        source: "class A { save() {} }\nclass K extends A {\n  audit() {}\n  save() { this.audit(); }\n  run() { this.save(); }\n}\n",
        own_save_line: 4,
    },
    Fixture {
        label: "Python",
        path: "k.py",
        source: "class A:\n    def save(self):\n        pass\nclass K(A):\n    def audit(self):\n        pass\n    def save(self):\n        self.audit()\n    def run(self):\n        self.save()\n",
        own_save_line: 7,
    },
    Fixture {
        label: "Java",
        path: "K.java",
        source: "interface A { void save(); }\nclass K implements A {\n  void audit() {}\n  public void save() { audit(); }\n  void run() { this.save(); save(); }\n}\n",
        own_save_line: 4,
    },
    Fixture {
        label: "Kotlin",
        path: "K.kt",
        source: "interface A { fun save() }\nclass K : A {\n  fun audit() {}\n  override fun save() { audit() }\n  fun run() { this.save(); save() }\n}\n",
        own_save_line: 4,
    },
    Fixture {
        label: "C#",
        path: "K.cs",
        source: "interface A { void save(); }\nclass K : A {\n  void audit() {}\n  public void save() { audit(); }\n  void run() { this.save(); save(); }\n}\n",
        own_save_line: 4,
    },
    Fixture {
        label: "Go",
        path: "k.go",
        source: "package demo\ntype A interface { save() }\ntype K struct{}\nfunc (k *K) audit() {}\nfunc (k *K) save() { k.audit() }\nfunc (k *K) run() { k.save() }\n",
        own_save_line: 5,
    },
    // The `impl A for K` `save` is a later overload of the inherent one.
    Fixture {
        label: "Rust",
        path: "src/lib.rs",
        source: "struct K;\ntrait A { fn save(&self); }\nimpl K {\n fn audit(&self) {}\n fn save(&self) {}\n fn run(&self) { self.save(); }\n}\nimpl A for K { fn save(&self) { self.audit(); } }\n",
        own_save_line: 5,
    },
    Fixture {
        label: "PHP",
        path: "K.php",
        source: "<?php\ninterface A { function save(); }\nclass K implements A {\n  function audit() {}\n  function save() { $this->audit(); }\n  function run() { $this->save(); }\n}\n",
        own_save_line: 5,
    },
    Fixture {
        label: "Ruby",
        path: "k.rb",
        source: "module A\n  def save\n  end\nend\nclass K\n  include A\n  def audit\n  end\n  def save\n    audit\n  end\n  def run\n    save\n    self.save\n  end\nend\n",
        own_save_line: 9,
    },
    Fixture {
        label: "Swift",
        path: "K.swift",
        source: "protocol A { func save() }\nclass K: A {\n  func audit() {}\n  func save() { audit() }\n  func run() { self.save(); save() }\n}\n",
        own_save_line: 4,
    },
    Fixture {
        label: "C++",
        path: "k.cpp",
        source: "class A { public: virtual void save() = 0; };\nclass K : public A {\n public:\n  void audit() {}\n  void save() override { audit(); }\n  void run() { this->save(); save(); }\n};\n",
        own_save_line: 5,
    },
    Fixture {
        label: "Dart",
        path: "k.dart",
        source: "abstract class A { void save(); }\nclass K implements A {\n  void audit() {}\n  void save() { audit(); }\n  void run() { this.save(); save(); }\n}\n",
        own_save_line: 4,
    },
];

fn parse(path: &str, source: &str) -> LocalGraph {
    ecp_cli::reanalyze::pipeline()
        .parse_file_raw(Path::new(path), source.as_bytes())
        .unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// `source` with a blank line after its first line.
fn shifted(source: &str) -> String {
    match source.split_once('\n') {
        Some((first, rest)) => format!("{first}\n\n{rest}"),
        None => format!("\n{source}"),
    }
}

#[test]
fn test_build_own_member_call_matches_reindex_across_13_languages() {
    let mut failures = Vec::new();
    for fixture in FIXTURES {
        let edited = shifted(fixture.source);
        let Parity { reindex, overlay } = calls_after_edit(
            &parse,
            &[(fixture.path, fixture.source)],
            &[(fixture.path, edited.as_str())],
            &|file, name| file == fixture.path && name == "run",
        );
        // The edit moves `K`'s `save` down one line.
        let own_line = fixture.own_save_line + 1;
        let saves: Vec<_> = reindex.iter().filter(|e| e.callee == "save").collect();
        if saves.is_empty() || saves.iter().any(|e| e.callee_line != own_line) {
            failures.push(format!(
                "{}: the reindex does not bind only K's own `save` (line {own_line}), so the fixture proves nothing: {reindex:#?}",
                fixture.label
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
