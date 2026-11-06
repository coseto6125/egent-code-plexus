//! A qualified function passed as a value from a dirty file (Rust
//! `parse::f` and `Self::f`, TypeScript `ns.f` and an export alias
//! `ns.renamed`, Go `pkg.F`, C++ `ns::f`, C# `Lib.Outer.Inner.M`, PHP
//! `[$this, 'cmp']`, Ruby `Lib::Util.method(:run)`, Swift `Outer.Inner.f`,
//! Java `com.example.Cb::print`): the session
//! overlay emits the same `References` edge (reason `function_reference`)
//! as a full reindex of the edited tree, reason and confidence included,
//! and masks the base edge a dirty file no longer makes.
//!
//! A fixture keeps the function in a clean file and its user in the dirty
//! one. `Self::f` and `[$this, 'cmp']` name a method of the user's own
//! class, so those two keep the function in the dirty file. Each fixture
//! runs two edits:
//!
//! | edit             | the reindex has                         |
//! |------------------|-----------------------------------------|
//! | add the value    | the user's edge to the qualified function |
//! | remove the value | none; the stale base edge must go        |

use crate::parity;

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::path_aliases::PathAliases;
use ecp_core::graph::RelType;
use parity::{edges_after_edit_with, parse, EdgeParity};
use std::path::Path;

const FUNCTION_REFERENCE: &str = "function_reference";

struct LangFixture {
    lang: &'static str,
    /// The clean files, the function's own among them.
    files: &'static [(&'static str, &'static str)],
    /// The dirty file's path, its source without the value, and with it.
    user: (&'static str, &'static str, &'static str),
    /// `(source function, target file, target)` the reindex emits from
    /// the user with the value.
    row: (&'static str, &'static str, &'static str),
}

const LANGS: &[LangFixture] = &[
    LangFixture {
        lang: "Rust",
        files: &[
            ("src/lib.rs", "pub mod parse;\npub mod cache;\n"),
            ("src/parse.rs", "pub fn migrate(_: &mut u32) {}\n"),
        ],
        user: (
            "src/cache.rs",
            "pub fn all(v: &mut Vec<u32>) {\n    let _ = v;\n}\n",
            "pub fn all(v: &mut Vec<u32>) {\n    v.iter_mut().for_each(crate::parse::migrate);\n}\n",
        ),
        row: ("all", "src/parse.rs", "migrate"),
    },
    LangFixture {
        lang: "TypeScript",
        files: &[(
            "lib.ts",
            "export function f(x: number): number {\n  return x;\n}\n",
        )],
        user: (
            "app.ts",
            "import * as ns from './lib';\nexport function run(items: number[]): number[] {\n  return items;\n}\n",
            "import * as ns from './lib';\nexport function run(items: number[]): number[] {\n  return items.map(ns.f);\n}\n",
        ),
        row: ("run", "lib.ts", "f"),
    },
    LangFixture {
        lang: "Go",
        files: &[("pkg/helper.go", "package pkg\n\nfunc F() {}\n")],
        user: (
            "cmd/app.go",
            "package main\n\nimport \"example.com/app/pkg\"\n\nfunc register(f func()) {}\n\nfunc boot() {\n\tregister(nil)\n}\n",
            "package main\n\nimport \"example.com/app/pkg\"\n\nfunc register(f func()) {}\n\nfunc boot() {\n\tregister(pkg.F)\n}\n",
        ),
        row: ("boot", "pkg/helper.go", "F"),
    },
    LangFixture {
        lang: "C++",
        files: &[("ns.cpp", "namespace ns {\nint f(int x) { return x; }\n}\n")],
        user: (
            "use.cpp",
            "#include <algorithm>\n#include <vector>\nvoid apply(std::vector<int>& xs) {\n    xs.clear();\n}\n",
            "#include <algorithm>\n#include <vector>\nvoid apply(std::vector<int>& xs) {\n    std::for_each(xs.begin(), xs.end(), ns::f);\n}\n",
        ),
        row: ("apply", "ns.cpp", "f"),
    },
    LangFixture {
        lang: "Rust",
        files: &[("src/lib.rs", "pub mod entry;\n")],
        user: (
            "src/entry.rs",
            "pub struct Entry(u32);\n\nimpl Entry {\n    pub fn parse(n: u32) -> Entry {\n        Entry(n)\n    }\n    pub fn all(v: Vec<u32>) -> Vec<Entry> {\n        let _ = v;\n        Vec::new()\n    }\n}\n",
            "pub struct Entry(u32);\n\nimpl Entry {\n    pub fn parse(n: u32) -> Entry {\n        Entry(n)\n    }\n    pub fn all(v: Vec<u32>) -> Vec<Entry> {\n        v.into_iter().map(Self::parse).collect()\n    }\n}\n",
        ),
        row: ("all", "src/entry.rs", "parse"),
    },
    LangFixture {
        lang: "TypeScript",
        files: &[(
            "lib.ts",
            "function f(x: number): number {\n  return x;\n}\nexport { f as renamed };\n",
        )],
        user: (
            "app.ts",
            "import * as ns from './lib';\nexport function run(items: number[]): number[] {\n  return items;\n}\n",
            "import * as ns from './lib';\nexport function run(items: number[]): number[] {\n  return items.map(ns.renamed);\n}\n",
        ),
        row: ("run", "lib.ts", "f"),
    },
    LangFixture {
        lang: "C#",
        files: &[(
            "Lib/Outer.cs",
            "namespace Lib {\n    public class Outer {\n        public class Inner {\n            public static void M(string s) {}\n        }\n    }\n}\n",
        )],
        user: (
            "Use.cs",
            "using System.Collections.Generic;\npublic class Use {\n    public void Run(List<string> xs) {\n        xs.Clear();\n    }\n}\n",
            "using System.Collections.Generic;\npublic class Use {\n    public void Run(List<string> xs) {\n        xs.ForEach(Lib.Outer.Inner.M);\n    }\n}\n",
        ),
        row: ("Run", "Lib/Outer.cs", "M"),
    },
    LangFixture {
        lang: "PHP",
        files: &[(
            "Other.php",
            "<?php\nclass Other {\n    function cmp($a, $b) { return 1; }\n}\n",
        )],
        user: (
            "Sorter.php",
            "<?php\nclass Sorter {\n    function cmp($a, $b) { return 0; }\n    function byThis($r) { return $r; }\n}\n",
            "<?php\nclass Sorter {\n    function cmp($a, $b) { return 0; }\n    function byThis($r) { usort($r, [$this, 'cmp']); }\n}\n",
        ),
        row: ("byThis", "Sorter.php", "cmp"),
    },
    LangFixture {
        lang: "Ruby",
        files: &[(
            "util.rb",
            "module Lib\n  module Util\n    def self.run(x)\n      x\n    end\n  end\nend\n",
        )],
        user: (
            "svc.rb",
            "class Svc\n  def wire(items)\n    items\n  end\nend\n",
            "class Svc\n  def wire(items)\n    items.each(&Lib::Util.method(:run))\n  end\nend\n",
        ),
        row: ("wire", "util.rb", "run"),
    },
    LangFixture {
        lang: "Swift",
        files: &[(
            "Outer.swift",
            "enum Outer {\n    enum Inner {\n        static func f(_ x: Int) -> Int { return x }\n    }\n}\n",
        )],
        user: (
            "Use.swift",
            "struct Use {\n    func apply(_ xs: [Int]) -> [Int] {\n        return xs\n    }\n}\n",
            "struct Use {\n    func apply(_ xs: [Int]) -> [Int] {\n        return xs.map(Outer.Inner.f)\n    }\n}\n",
        ),
        row: ("apply", "Outer.swift", "f"),
    },
    LangFixture {
        lang: "Java",
        files: &[(
            "com/example/Cb.java",
            "package com.example;\n\nclass Cb {\n    static void print(String s) {}\n}\n",
        )],
        user: (
            "Use.java",
            "class Use {\n    void run(java.util.List<String> xs) {\n        xs.clear();\n    }\n}\n",
            "class Use {\n    void run(java.util.List<String> xs) {\n        xs.forEach(com.example.Cb::print);\n    }\n}\n",
        ),
        row: ("run", "com/example/Cb.java", "print"),
    },
    // A01: a sibling closure that only reads `$o` through `use (&$o)` keeps the
    // array callable's edge. The bare side holds the sibling that writes `$o`
    // (no edge), so "add the value" flips a stale-free base to the edge and
    // "remove the value" must mask the base edge.
    LangFixture {
        lang: "PHP sibling by-ref capture",
        files: &[(
            "Cb.php",
            "<?php\nclass Cb {\n    function cb_ptr(int $x) {}\n}\n",
        )],
        user: (
            "Holder.php",
            "<?php\nclass Holder {\n    function m(Cb $o, array $a) {\n        $f = function () use (&$o, $a) { return array_map([$o, 'cb_ptr'], $a); };\n        $g = function () use (&$o, $a) { $o = $a; };\n        return $f();\n    }\n}\n",
            "<?php\nclass Holder {\n    function m(Cb $o, array $a) {\n        $f = function () use (&$o, $a) { return array_map([$o, 'cb_ptr'], $a); };\n        $g = function () use (&$o) { return $o; };\n        return $f();\n    }\n}\n",
        ),
        row: ("m", "Cb.php", "cb_ptr"),
    },
    // A01 with no sibling closure on the bare side.
    LangFixture {
        lang: "PHP by-ref capture, no sibling",
        files: &[(
            "Cb.php",
            "<?php\nclass Cb {\n    function cb_ptr(int $x) {}\n}\n",
        )],
        user: (
            "Holder.php",
            "<?php\nclass Holder {\n    function m(Cb $o, array $a) {\n        return $a;\n    }\n}\n",
            "<?php\nclass Holder {\n    function m(Cb $o, array $a) {\n        $f = function () use (&$o, $a) { return array_map([$o, 'cb_ptr'], $a); };\n        $g = function () use (&$o) { return $o; };\n        return $f();\n    }\n}\n",
        ),
        row: ("m", "Cb.php", "cb_ptr"),
    },
    // A08: a property read outside the class is no method value; the method
    // handed on next to it keeps its edge.
    LangFixture {
        lang: "Python property read, typed receiver",
        files: &[(
            "cart.py",
            "class Cart:\n    def helper(self, x):\n        return x\n\n    @property\n    def total(self):\n        return 1\n",
        )],
        user: (
            "use.py",
            "from cart import Cart\n\ndef read(c: Cart, xs):\n    t = c.total\n    return xs\n",
            "from cart import Cart\n\ndef read(c: Cart, xs):\n    t = c.total\n    return list(map(c.helper, xs))\n",
        ),
        row: ("read", "cart.py", "helper"),
    },
    // A08, dirty class: dropping `@property` makes `self.total` a method value
    // (the edge appears); adding it back must mask the base edge.
    LangFixture {
        lang: "Python property decorator toggled",
        files: &[],
        user: (
            "cart.py",
            "class Cart:\n    @property\n    def total(self):\n        return 1\n\n    def show(self):\n        t = self.total\n        return t\n",
            "class Cart:\n    def total(self):\n        return 1\n\n    def show(self):\n        t = self.total\n        return t\n",
        ),
        row: ("show", "cart.py", "total"),
    },
];

/// The builder of `lang`'s fixture. Go gets a module at `root` with the
/// alias the CLI derives from its `go.mod` (`example.com/app/*` -> `./*`)
/// and the files of `tree` on disk; every other language needs neither.
fn builder_for(lang: &str, root: &Path, tree: &[(&str, &str)]) -> Box<dyn Fn() -> GraphBuilder> {
    if lang != "Go" {
        return Box::new(GraphBuilder::new);
    }
    std::fs::write(root.join("go.mod"), "module example.com/app\n\ngo 1.21\n").unwrap();
    for (path, source) in tree {
        let disk = root.join(path);
        std::fs::create_dir_all(disk.parent().unwrap()).unwrap();
        std::fs::write(disk, source).unwrap();
    }
    let mut aliases = PathAliases::new();
    aliases.add("example.com/app/*", vec!["./*".to_string()]);
    let root = root.to_path_buf();
    Box::new(move || {
        GraphBuilder::new()
            .with_repo_root(root.clone())
            .with_path_aliases(aliases.clone())
    })
}

/// Every mismatch of the two edits of `fixture`, one line each: a reindex
/// that differs from the expected rows (the fixture would prove nothing),
/// or an overlay that differs from the reindex in any field.
fn mismatches(fixture: &LangFixture) -> Vec<String> {
    let (user_path, bare, with_value) = fixture.user;
    let mut failures = Vec::new();
    for (edit, base, edited, expected) in [
        ("add the value", bare, with_value, vec![fixture.row]),
        ("remove the value", with_value, bare, Vec::new()),
    ] {
        let mut files = fixture.files.to_vec();
        files.push((user_path, base));
        let tmp = tempfile::tempdir().unwrap();
        let mut tree = fixture.files.to_vec();
        tree.push((user_path, edited));
        let new_builder = builder_for(fixture.lang, tmp.path(), &tree);
        let EdgeParity { reindex, overlay } = edges_after_edit_with(
            new_builder.as_ref(),
            &parse,
            &files,
            &[(user_path, edited)],
            RelType::References,
            &|file, _| file == user_path,
        );
        let only = |edges: Vec<parity::ReasonedEdge>| -> Vec<parity::ReasonedEdge> {
            edges
                .into_iter()
                .filter(|e| e.reason == FUNCTION_REFERENCE)
                .collect()
        };
        let (reindex, overlay) = (only(reindex), only(overlay));
        let got: Vec<(&str, &str, &str)> = reindex
            .iter()
            .map(|e| {
                (
                    e.edge.caller.as_str(),
                    e.edge.callee_file.as_str(),
                    e.edge.callee.as_str(),
                )
            })
            .collect();
        let label = format!("{}/{edit}", fixture.lang);
        if got != expected {
            failures.push(format!(
                "{label}: the reindex emits {got:?}, the fixture expects {expected:?}"
            ));
            continue;
        }
        if overlay != reindex {
            failures.push(format!(
                "{label}: overlay {overlay:#?}\nreindex {reindex:#?}"
            ));
        }
    }
    failures
}

#[test]
fn test_overlay_qualified_function_reference_matches_reindex() {
    let failures: Vec<String> = LANGS.iter().flat_map(mismatches).collect();
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
