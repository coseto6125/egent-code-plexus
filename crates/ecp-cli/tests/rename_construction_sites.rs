//! `ecp rename` of a type rewrites the construction sites in other files.
//!
//! A construction's `Calls` edge lands on the type's constructor, not on the
//! type, so a rename that read only the type's own in-edges left `new Foo()`
//! in another file unrenamed and listed it in `residuals`, although
//! `impact Foo` listed the same caller at depth 1. Measured on a built `ecp`
//! before the fix: C#, Java, Kotlin, TypeScript, JavaScript, Python, PHP,
//! Ruby, Swift and C++ failed; Dart, Go and Rust passed (no constructor
//! node, or the site binds to the type); C failed before and after.

mod common;

use common::{commit_all, ecp_bin, run_git};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Repo {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Repo {
    fn indexed(files: &[(&str, &str)]) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let home = tmp.path().join("home");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&home).unwrap();
        let repo = Self {
            _tmp: tmp,
            root,
            home,
        };
        for (rel, text) in files {
            let path = repo.root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        run_git(&repo.root, &["init", "-q", "-b", "main"]);
        commit_all(&repo.root, "base");
        let out = repo.ecp(&["admin", "index", "--repo", "."]);
        assert!(out.status.success(), "admin index: {}", text(&out.stderr));
        repo
    }

    fn read(&self, rel: &str) -> String {
        fs::read_to_string(self.root.join(rel)).unwrap()
    }

    fn ecp(&self, args: &[&str]) -> Output {
        Command::new(ecp_bin())
            .args(args)
            .current_dir(&self.root)
            .env("HOME", &self.home)
            .env_remove("ECP_HOME")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env("ECP_SESSION_ID", "rename-construction-sites")
            .env("ECP_SKIP_BG_REBUILD", "1")
            .output()
            .expect("ecp failed to spawn")
    }
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Stands for: after `rename Foo Bar`, neither the declaration file nor the
/// file that constructs the type spells `Foo`. The literal fixtures hold no
/// other `Foo`, so any leftover is a site the rename missed.
fn assert_rename_rewrites_construction(declaration: (&str, &str), maker: (&str, &str)) {
    let repo = Repo::indexed(&[declaration, maker]);

    let out = repo.ecp(&["rename", "Foo", "Bar", "--file", declaration.0]);

    assert!(
        out.status.success(),
        "{} {}",
        text(&out.stderr),
        text(&out.stdout)
    );
    for rel in [declaration.0, maker.0] {
        let body = repo.read(rel);
        assert!(!body.contains("Foo"), "{rel} keeps Foo:\n{body}");
        assert!(body.contains("Bar"), "{rel} lost the name:\n{body}");
    }
}

#[test]
fn test_rename_type_csharp_renames_new_site() {
    assert_rename_rewrites_construction(
        (
            "Foo.cs",
            "class Foo\n{\n    public Foo()\n    {\n    }\n}\n",
        ),
        (
            "Maker.cs",
            "class Maker\n{\n    void Make()\n    {\n        var f = new Foo();\n    }\n}\n",
        ),
    );
}

#[test]
fn test_rename_type_java_renames_new_site() {
    assert_rename_rewrites_construction(
        ("Foo.java", "class Foo {\n    Foo() {\n    }\n}\n"),
        (
            "Maker.java",
            "class Maker {\n    void make() {\n        Foo f = new Foo();\n    }\n}\n",
        ),
    );
}

/// Stands for: a constructor that chains to another (`this(1)`) leaves both
/// constructors spelled like the type, and the construction site follows.
#[test]
fn test_rename_type_java_constructor_chain_renames_all() {
    assert_rename_rewrites_construction(
        (
            "Foo.java",
            "class Foo {\n    Foo() {\n        this(1);\n    }\n    Foo(int x) {\n    }\n}\n",
        ),
        (
            "Maker.java",
            "class Maker {\n    void make() {\n        Foo f = new Foo();\n    }\n}\n",
        ),
    );
}

/// Stands for: a Kotlin secondary constructor chaining to the primary one
/// (`constructor() : this(0)`) does not stop the construction site.
#[test]
fn test_rename_type_kotlin_secondary_constructor_renames_construction_site() {
    assert_rename_rewrites_construction(
        (
            "Foo.kt",
            "class Foo(val x: Int) {\n    constructor() : this(0) {\n    }\n}\n",
        ),
        (
            "Maker.kt",
            "class Maker {\n    fun make() {\n        val f = Foo()\n    }\n}\n",
        ),
    );
}

#[test]
fn test_rename_type_kotlin_renames_construction_site() {
    assert_rename_rewrites_construction(
        ("Foo.kt", "class Foo {\n    constructor() {\n    }\n}\n"),
        (
            "Maker.kt",
            "class Maker {\n    fun make() {\n        val f = Foo()\n    }\n}\n",
        ),
    );
}

#[test]
fn test_rename_type_typescript_renames_new_site() {
    assert_rename_rewrites_construction(
        (
            "foo.ts",
            "export class Foo {\n    constructor() {\n    }\n}\n",
        ),
        (
            "maker.ts",
            "import { Foo } from './foo';\nexport function make() {\n    return new Foo();\n}\n",
        ),
    );
}

#[test]
fn test_rename_type_javascript_renames_new_site() {
    assert_rename_rewrites_construction(
        (
            "foo.js",
            "export class Foo {\n    constructor() {\n    }\n}\n",
        ),
        (
            "maker.js",
            "import { Foo } from './foo.js';\nexport function make() {\n    return new Foo();\n}\n",
        ),
    );
}

#[test]
fn test_rename_type_python_renames_construction_site() {
    assert_rename_rewrites_construction(
        (
            "foo.py",
            "class Foo:\n    def __init__(self):\n        pass\n",
        ),
        (
            "maker.py",
            "from foo import Foo\n\ndef make():\n    return Foo()\n",
        ),
    );
}

#[test]
fn test_rename_type_php_renames_new_site() {
    assert_rename_rewrites_construction(
        (
            "Foo.php",
            "<?php\nclass Foo {\n    public function __construct() {\n    }\n}\n",
        ),
        (
            "Maker.php",
            "<?php\nclass Maker {\n    public function make() {\n        return new Foo();\n    }\n}\n",
        ),
    );
}

#[test]
fn test_rename_type_ruby_renames_new_site() {
    assert_rename_rewrites_construction(
        ("foo.rb", "class Foo\n  def initialize\n  end\nend\n"),
        (
            "maker.rb",
            "require_relative 'foo'\ndef make\n  Foo.new\nend\n",
        ),
    );
}

#[test]
fn test_rename_type_swift_renames_construction_site() {
    assert_rename_rewrites_construction(
        ("Foo.swift", "class Foo {\n    init() {\n    }\n}\n"),
        ("Maker.swift", "func make() -> Foo {\n    return Foo()\n}\n"),
    );
}

#[test]
fn test_rename_type_dart_renames_construction_site() {
    assert_rename_rewrites_construction(
        ("foo.dart", "class Foo {\n  Foo() {\n  }\n}\n"),
        (
            "maker.dart",
            "import 'foo.dart';\nFoo make() {\n  return Foo();\n}\n",
        ),
    );
}

#[test]
fn test_rename_type_cpp_renames_construction_site() {
    assert_rename_rewrites_construction(
        ("foo.hpp", "class Foo {\npublic:\n    Foo() {\n    }\n};\n"),
        (
            "maker.cpp",
            "#include \"foo.hpp\"\nvoid make() {\n    Foo f = Foo();\n}\n",
        ),
    );
}

#[test]
fn test_rename_type_go_renames_composite_literal_site() {
    assert_rename_rewrites_construction(
        ("foo.go", "package main\n\ntype Foo struct {\n}\n"),
        (
            "maker.go",
            "package main\n\nfunc make() Foo {\n    return Foo{}\n}\n",
        ),
    );
}

#[test]
fn test_rename_type_rust_renames_struct_literal_site() {
    assert_rename_rewrites_construction(
        ("src/foo.rs", "pub struct Foo {\n}\n"),
        (
            "src/maker.rs",
            "use crate::foo::Foo;\npub fn make() -> Foo {\n    Foo {}\n}\n",
        ),
    );
}

#[test]
#[ignore = "FU-2026-10-26-2777b0635414: no edge from make to struct Foo"]
fn test_rename_type_c_renames_local_declaration_site() {
    assert_rename_rewrites_construction(
        ("foo.h", "struct Foo {\n    int x;\n};\n"),
        (
            "maker.c",
            "#include \"foo.h\"\nvoid make(void) {\n    struct Foo f;\n}\n",
        ),
    );
}

/// Stands for: every part of a C# `partial` type is the declaration. Failed
/// before: from the part that is not the representative, the rename wrote
/// only its own file, left the other part, its constructor and `new Foo()`,
/// and still read `risk safe`. A part groups only inside a `.csproj`.
#[test]
fn test_rename_type_csharp_partial_from_second_part_renames_all_parts() {
    let repo = Repo::indexed(&[
        (
            "App.csproj",
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n</Project>\n",
        ),
        (
            "A.cs",
            "partial class Foo\n{\n    public Foo()\n    {\n    }\n}\n",
        ),
        (
            "B.cs",
            "partial class Foo\n{\n    void Run()\n    {\n    }\n}\n",
        ),
        (
            "Maker.cs",
            "class Maker\n{\n    void Make()\n    {\n        var f = new Foo();\n    }\n}\n",
        ),
    ]);

    let out = repo.ecp(&["rename", "Foo", "Bar", "--file", "B.cs"]);

    assert!(
        out.status.success(),
        "{} {}",
        text(&out.stderr),
        text(&out.stdout)
    );
    for rel in ["A.cs", "B.cs", "Maker.cs"] {
        let body = repo.read(rel);
        assert!(!body.contains("Foo"), "{rel} keeps Foo:\n{body}");
    }
}
