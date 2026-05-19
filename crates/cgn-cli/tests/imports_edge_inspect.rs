//! E2E fixture tests for `RelType::Imports` edge emission across 14
//! mainstream languages, plus a cross-language collision test pinning
//! the "resolver miss → don't emit" rule (no gitnexus-style false positives
//! like `.mjs → Path.java`).
//!
//! Each fixture writes two files: an importing file (`b.<ext>`) that
//! imports a named symbol from an imported file (`a.<ext>`). The test
//! asserts the graph contains an edge:
//!
//!     (importing_file : NodeKind::File) -[:Imports]-> (target : <symbol>)
//!
//! The cross-language test plants identically-named files in three different
//! languages under one repo and verifies the resolver only links to the
//! same-extension match (gitnexus produces ~50% cross-language false positives
//! on this shape; gnx must produce zero).

use serde_json::Value;
use std::path::Path;
use std::process::Command;

fn gnx_bin() -> &'static str {
    env!("CARGO_BIN_EXE_gnx")
}

fn write(repo: &Path, rel: &str, body: &str) {
    let full = repo.join(rel);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(full, body).unwrap();
}

fn init_and_analyze(repo: &Path) {
    let out = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(out.status.success());

    let _ = Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo)
        .output()
        .unwrap();
    let _ = Command::new("git")
        .args([
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ])
        .current_dir(repo)
        .output()
        .unwrap();

    let out = Command::new(gnx_bin())
        .args(["admin", "index", "--repo", "."])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("admin index failed to spawn");
    assert!(
        out.status.success(),
        "admin index failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn cypher_json(repo: &Path, query: &str) -> Value {
    let out = Command::new(gnx_bin())
        .args(["cypher", query, "--format", "json"])
        .current_dir(repo)
        .env("HOME", repo)
        .output()
        .expect("cypher command failed to spawn");
    assert!(
        out.status.success(),
        "cypher {query:?} failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let json_start = stdout
        .find('{')
        .unwrap_or_else(|| panic!("cypher {query:?} did not return JSON\nstdout={stdout}"));
    serde_json::from_str(&stdout[json_start..]).unwrap_or_else(|err| {
        panic!("cypher {query:?} did not return JSON: {err}\nstdout={stdout}")
    })
}

/// Count `Imports` edges whose target node has `name = target_name` in the
/// graph at `repo`. Doesn't constrain which File is the source — the assertion
/// is "at least one File imports this symbol", which is the minimum guarantee
/// per spec §4.2.
fn count_imports_to(repo: &Path, target_name: &str) -> u64 {
    let q = format!(
        "MATCH (f)-[r]->(t) WHERE r.rel_type = 'Imports' AND f.kind = 'File' AND t.name = '{target_name}' RETURN count(*) AS c"
    );
    let v = cypher_json(repo, &q);
    // Single-column count queries return flat `rows: [N]`; multi-column
    // queries return nested `rows: [[N, ...]]`. Handle both shapes.
    let row = &v["rows"][0];
    if let Some(n) = row.as_u64() {
        return n;
    }
    row[0].as_u64().unwrap_or(0)
}

// ── TypeScript ────────────────────────────────────────────────────────────

#[test]
fn imports_typescript_named() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/a.ts",
        "export function foo() { return 1; }\n",
    );
    write(
        tmp.path(),
        "src/b.ts",
        "import { foo } from './a';\nexport function caller() { return foo(); }\n",
    );
    init_and_analyze(tmp.path());
    assert!(
        count_imports_to(tmp.path(), "foo") >= 1,
        "TypeScript: expected File(b.ts) -[:Imports]-> foo, got 0"
    );
}

// ── JavaScript ────────────────────────────────────────────────────────────

#[test]
fn imports_javascript_named() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/a.mjs",
        "export function foo() { return 1; }\n",
    );
    write(
        tmp.path(),
        "src/b.mjs",
        "import { foo } from './a.mjs';\nexport function caller() { return foo(); }\n",
    );
    init_and_analyze(tmp.path());
    assert!(
        count_imports_to(tmp.path(), "foo") >= 1,
        "JavaScript: expected File(b.mjs) -[:Imports]-> foo, got 0"
    );
}

// ── Python ────────────────────────────────────────────────────────────────

#[test]
fn imports_python_from_import() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "a.py", "def foo():\n    return 1\n");
    write(
        tmp.path(),
        "b.py",
        "from a import foo\n\ndef caller():\n    return foo()\n",
    );
    init_and_analyze(tmp.path());
    assert!(
        count_imports_to(tmp.path(), "foo") >= 1,
        "Python: expected File(b.py) -[:Imports]-> foo, got 0"
    );
}

// ── Java ──────────────────────────────────────────────────────────────────

#[test]
fn imports_java_fqn() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/com/x/Alpha.java",
        "package com.x;\npublic class Alpha { public int bar() { return 1; } }\n",
    );
    write(
        tmp.path(),
        "src/com/y/Beta.java",
        "package com.y;\nimport com.x.Alpha;\npublic class Beta { void use() { new Alpha().bar(); } }\n",
    );
    init_and_analyze(tmp.path());
    assert!(
        count_imports_to(tmp.path(), "Alpha") >= 1,
        "Java: expected File(Beta.java) -[:Imports]-> Alpha, got 0"
    );
}

// ── Kotlin ────────────────────────────────────────────────────────────────

#[test]
fn imports_kotlin_fqn() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/com/x/Alpha.kt",
        "package com.x\nclass Alpha { fun bar() = 1 }\n",
    );
    write(
        tmp.path(),
        "src/com/y/Beta.kt",
        "package com.y\nimport com.x.Alpha\nclass Beta { fun use() = Alpha().bar() }\n",
    );
    init_and_analyze(tmp.path());
    assert!(
        count_imports_to(tmp.path(), "Alpha") >= 1,
        "Kotlin: expected File(Beta.kt) -[:Imports]-> Alpha, got 0"
    );
}

// ── C# ────────────────────────────────────────────────────────────────────

#[test]
fn imports_csharp_using() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/X/Alpha.cs",
        "namespace X { public class Alpha { public int Bar() => 1; } }\n",
    );
    write(
        tmp.path(),
        "src/Y/Beta.cs",
        "using X;\nnamespace Y { public class Beta { void Use() { new Alpha().Bar(); } } }\n",
    );
    init_and_analyze(tmp.path());
    // C# `using X;` names a namespace, not a symbol — resolved to the
    // file under `src/X/*.cs` via Step 3e (namespace/module-dir match).
    assert!(
        count_imports_to(tmp.path(), "Alpha.cs") >= 1,
        "C#: expected File(Beta.cs) -[:Imports]-> File(Alpha.cs), got 0"
    );
}

// ── Go ────────────────────────────────────────────────────────────────────
//
// Go `import "module/pkg"` is module-style (no named symbol in the
// specifier). emit_imports_edges Step 3d (last-segment + caller-extension
// suffix match) wires this as a File → File edge to the package's file.

#[test]
fn imports_go_package() {
    let tmp = tempfile::tempdir().unwrap();
    write(tmp.path(), "go.mod", "module sample\n\ngo 1.21\n");
    write(
        tmp.path(),
        "alpha/alpha.go",
        "package alpha\n\nfunc Foo() int { return 1 }\n",
    );
    write(
        tmp.path(),
        "beta/beta.go",
        "package beta\n\nimport \"sample/alpha\"\n\nfunc Caller() int { return alpha.Foo() }\n",
    );
    init_and_analyze(tmp.path());
    assert!(
        count_imports_to(tmp.path(), "alpha.go") >= 1,
        "Go: expected File(beta.go) -[:Imports]-> File(alpha.go), got 0"
    );
}

// ── Rust ──────────────────────────────────────────────────────────────────

#[test]
fn imports_rust_use() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "Cargo.toml",
        "[package]\nname = \"sample\"\nversion = \"0.0.1\"\nedition = \"2021\"\n",
    );
    write(tmp.path(), "src/a.rs", "pub fn foo() -> i32 { 1 }\n");
    write(
        tmp.path(),
        "src/b.rs",
        "use crate::a::foo;\npub fn caller() -> i32 { foo() }\n",
    );
    write(tmp.path(), "src/lib.rs", "pub mod a;\npub mod b;\n");
    init_and_analyze(tmp.path());
    assert!(
        count_imports_to(tmp.path(), "foo") >= 1,
        "Rust: expected File(b.rs) -[:Imports]-> foo, got 0"
    );
}

// ── PHP ───────────────────────────────────────────────────────────────────

#[test]
fn imports_php_use() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "src/App/Alpha.php",
        "<?php\nnamespace App;\nclass Alpha { public function bar() { return 1; } }\n",
    );
    write(
        tmp.path(),
        "src/Web/Beta.php",
        "<?php\nnamespace Web;\nuse App\\Alpha;\nclass Beta { public function use() { return (new Alpha)->bar(); } }\n",
    );
    init_and_analyze(tmp.path());
    assert!(
        count_imports_to(tmp.path(), "Alpha") >= 1,
        "PHP: expected File(Beta.php) -[:Imports]-> Alpha, got 0"
    );
}

// ── Ruby ──────────────────────────────────────────────────────────────────

#[test]
fn imports_ruby_require_relative() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "lib/alpha.rb",
        "class Alpha\n  def bar\n    1\n  end\nend\n",
    );
    write(
        tmp.path(),
        "lib/beta.rb",
        "require_relative 'alpha'\n\nclass Beta\n  def use\n    Alpha.new.bar\n  end\nend\n",
    );
    init_and_analyze(tmp.path());
    // Ruby `require_relative 'alpha'` is module-style — Step 3b ./prefix
    // retry resolves it to alpha.rb (File→File edge).
    assert!(
        count_imports_to(tmp.path(), "alpha.rb") >= 1,
        "Ruby: expected File(beta.rb) -[:Imports]-> File(alpha.rb), got 0"
    );
}

// ── Swift ─────────────────────────────────────────────────────────────────

#[test]
fn imports_swift_module() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "Sources/Alpha/Alpha.swift",
        "public struct Alpha {\n    public init() {}\n    public func bar() -> Int { 1 }\n}\n",
    );
    write(
        tmp.path(),
        "Sources/Beta/Beta.swift",
        "import Alpha\n\npublic struct Beta {\n    public init() {}\n    public func use() -> Int { Alpha().bar() }\n}\n",
    );
    init_and_analyze(tmp.path());
    assert!(
        count_imports_to(tmp.path(), "Alpha") >= 1,
        "Swift: expected File(Beta.swift) -[:Imports]-> Alpha, got 0"
    );
}

// ── C ─────────────────────────────────────────────────────────────────────

#[test]
fn imports_c_include() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "alpha.h",
        "#ifndef ALPHA_H\n#define ALPHA_H\nint alpha_bar(void);\n#endif\n",
    );
    write(
        tmp.path(),
        "alpha.c",
        "#include \"alpha.h\"\nint alpha_bar(void) { return 1; }\n",
    );
    write(
        tmp.path(),
        "beta.c",
        "#include \"alpha.h\"\nint beta_use(void) { return alpha_bar(); }\n",
    );
    init_and_analyze(tmp.path());
    // C `#include "alpha.h"` resolves to alpha.h via Step 3c suffix match
    // (File→File edge — C/C++ headers don't carry symbol names in the
    // specifier).
    assert!(
        count_imports_to(tmp.path(), "alpha.h") >= 1,
        "C: expected File(beta.c) -[:Imports]-> File(alpha.h), got 0"
    );
}

// ── C++ ───────────────────────────────────────────────────────────────────

#[test]
fn imports_cpp_include() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "include/alpha.hpp",
        "#pragma once\nclass Alpha { public: int bar(); };\n",
    );
    write(
        tmp.path(),
        "src/alpha.cpp",
        "#include \"alpha.hpp\"\nint Alpha::bar() { return 1; }\n",
    );
    write(
        tmp.path(),
        "src/beta.cpp",
        "#include \"alpha.hpp\"\nint beta_use() { return Alpha().bar(); }\n",
    );
    init_and_analyze(tmp.path());
    // C++ `#include "alpha.hpp"` resolves to include/alpha.hpp via Step 3c
    // suffix match — search-path dirs handled by the suffix index.
    assert!(
        count_imports_to(tmp.path(), "alpha.hpp") >= 1,
        "C++: expected File(beta.cpp) -[:Imports]-> File(alpha.hpp), got 0"
    );
}

// ── Dart ──────────────────────────────────────────────────────────────────

#[test]
fn imports_dart_import() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        tmp.path(),
        "lib/alpha.dart",
        "class Alpha {\n  int bar() => 1;\n}\n",
    );
    write(
        tmp.path(),
        "lib/beta.dart",
        "import 'alpha.dart';\n\nclass Beta {\n  int use() => Alpha().bar();\n}\n",
    );
    init_and_analyze(tmp.path());
    // Dart `import 'alpha.dart';` is module-style — resolved to alpha.dart
    // via Step 3 path resolution (File→File edge).
    assert!(
        count_imports_to(tmp.path(), "alpha.dart") >= 1,
        "Dart: expected File(beta.dart) -[:Imports]-> File(alpha.dart), got 0"
    );
}

// ── Cross-language collision test ─────────────────────────────────────────
//
// gitnexus on .sample_repo emits IMPORTS edges like
//     solidity/eslint.config.mjs → Path.java
// because its resolver does cross-language name globbing. This test pins the
// gnx rule (spec §2: "resolver miss → don't emit"): when a TS file imports
// `./foo`, gnx must only link to foo.ts in the same directory, never to
// foo.py / foo.java which happen to exist with the same basename elsewhere.

#[test]
fn imports_cross_language_no_false_positive() {
    let tmp = tempfile::tempdir().unwrap();
    // The TS file that imports './foo' — only `src/foo.ts` should match.
    write(
        tmp.path(),
        "src/foo.ts",
        "export function foo_ts() { return 1; }\n",
    );
    write(
        tmp.path(),
        "src/use.ts",
        "import { foo_ts } from './foo';\nexport function caller() { return foo_ts(); }\n",
    );
    // Distractors with same basename in different languages — must NOT be
    // linked from `src/use.ts`.
    write(tmp.path(), "other/foo.py", "def foo_py():\n    return 1\n");
    write(
        tmp.path(),
        "other/foo.java",
        "package other;\npublic class Foo { public int foo_java() { return 1; } }\n",
    );
    init_and_analyze(tmp.path());

    // Target name `foo_ts` must be linked — same-extension same-directory match
    assert!(
        count_imports_to(tmp.path(), "foo_ts") >= 1,
        "cross-language: legitimate TS→TS import of foo_ts should fire"
    );
    // Distractor symbols must NOT be linked — different language / different dir
    assert_eq!(
        count_imports_to(tmp.path(), "foo_py"),
        0,
        "cross-language: TS import './foo' must NOT cross-link to foo.py"
    );
    assert_eq!(
        count_imports_to(tmp.path(), "foo_java"),
        0,
        "cross-language: TS import './foo' must NOT cross-link to foo.java"
    );
}
