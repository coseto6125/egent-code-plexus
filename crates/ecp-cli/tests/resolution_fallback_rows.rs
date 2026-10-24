//! The call-level fallback rows of the shared resolution design (B §3.2,
//! pinned by §4.3 item 3). Each test plants one fact in a small tree and
//! asserts what a reader of the graph gets: the exact `Calls` edges of one
//! caller in a fresh full index, and that the overlay view of the same tree
//! (the caller file dirty, unchanged but for a trailing newline) has no
//! `Calls` edge that differs from that index.
//!
//! These are pin tests. They hold today's behaviour, before any slice moves
//! the ladders onto `Resolution`, and must stay green through every later
//! slice: a red row means an edge moved. Every fixture carries a decoy that a
//! wrong fallback would bind (a namesake in another file, a nested function),
//! so an empty expectation is not a vacuous pass.
//!
//! The confidence list of a row names the tiers the row may bind at. Where
//! the tier is known the list holds one value.

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use ecp_cli::commands::admin::index::run_analyzer_for_paths;
use ecp_core::graph::{ArchivedZeroCopyGraph, ZeroCopyGraph};
use ecp_core::session::merged::MergedGraph;
use std::collections::BTreeSet;
use std::fs;
use strict_parity::{observe, snapshot, Edit, Scenario, Snapshot};

type Files<'a> = &'a [(&'a str, &'a str)];

const GLOBAL: &[f32] = &[0.7];

/// What one caller must call: `(callee name, callee file, allowed confidences)`.
type Callees<'a> = &'a [(&'a str, &'a str, &'a [f32])];

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

/// The production full index of `files`, as the snapshot the parity harness compares.
fn index_snapshot(files: Files<'_>) -> Snapshot {
    let src = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    for (path, source) in files {
        let full = src.path().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, source).unwrap();
    }
    let (_, graph): (usize, ZeroCopyGraph) =
        run_analyzer_for_paths(src.path(), out.path(), None, None).expect("full index");
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&graph).unwrap();
    let archived = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&bytes).unwrap();
    snapshot(&MergedGraph::new(archived, None))
}

/// Hold `files` to `expected`, one entry per caller: the caller calls exactly
/// those callees. Then dirty `caller_file` and hold the overlay to the index.
fn pin_row(files: Files<'_>, caller_file: &str, expected: &[(&str, Callees<'_>)]) {
    let edited_source = files
        .iter()
        .find(|(path, _)| *path == caller_file)
        .map(|(_, source)| format!("{source}\n"))
        .expect("the caller file is one of the files");
    let edited: Vec<(&str, &str)> = files
        .iter()
        .map(|&(path, source)| {
            if path == caller_file {
                (path, edited_source.as_str())
            } else {
                (path, source)
            }
        })
        .collect();

    let edges = call_edges(&index_snapshot(&edited));
    for (caller, callees) in expected {
        let mut actual: Vec<(&str, &str, &str)> = edges
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
        assert_eq!(names, want, "index Calls edges of {caller}: {actual:?}");
        for (name, file, confidence) in &actual {
            let (_, _, allowed) = callees
                .iter()
                .find(|(n, f, _)| n == name && f == file)
                .unwrap();
            assert!(
                allowed.iter().any(|c| format!("{c:?}") == *confidence),
                "{caller} -> {name} at {confidence}, allowed {allowed:?}"
            );
        }
    }

    let differing: BTreeSet<String> = observe(&Scenario {
        name: "resolution_fallback_rows",
        owner: "R1a (B section 3.2 fallback rows)",
        files,
        edits: &[Edit::Write(caller_file, &edited_source)],
    })
    .into_iter()
    .filter(|line| line.starts_with("edge ") && line.contains(" Calls "))
    .collect();
    assert!(
        differing.is_empty(),
        "the overlay's Calls edges differ from the full index: {differing:#?}"
    );
}

/// Row 1. `ProtoBuf` is an external import (ladder `NoEdge(External)`), yet
/// the repo declares `fun BinaryFormat.asConverterFactory`. The call-level
/// extension retry binds the top-level function, at the global tier, today.
#[test]
fn test_resolve_call_kotlin_typed_receiver_external_extension_retry_binds() {
    let files = [
        (
            "src/main/kotlin/conv/Factory.kt",
            "package conv\nimport kotlinx.serialization.BinaryFormat\nfun BinaryFormat.asConverterFactory(t: String): Any = t\n",
        ),
        (
            "src/test/kotlin/conv/FactoryTest.kt",
            "package conv\nimport kotlinx.serialization.protobuf.ProtoBuf\nfun setUp() { ProtoBuf.asConverterFactory(\"x\") }\n",
        ),
    ];
    pin_row(
        &files,
        "src/test/kotlin/conv/FactoryTest.kt",
        &[(
            "setUp",
            &[(
                "asConverterFactory",
                "src/main/kotlin/conv/Factory.kt",
                GLOBAL,
            )],
        )],
    );
}

/// Row 2. Two functions named `Widget` make the callable lookup ambiguous
/// (no edge from the ladder). The constructor fallback then finds the one
/// class `Widget` and binds its `__init__`, at the global tier, today.
#[test]
fn test_resolve_call_python_global_ambiguous_constructor_fallback_binds_class() {
    let files = [
        (
            "pkg/widget.py",
            "class Widget:\n    def __init__(self, x):\n        self.x = x\n",
        ),
        ("pkg/a/make_one.py", "def Widget():\n    return 1\n"),
        ("pkg/b/make_two.py", "def Widget():\n    return 2\n"),
        ("app/main.py", "def run():\n    return Widget(1)\n"),
    ];
    pin_row(
        &files,
        "app/main.py",
        &[("run", &[("__init__", "pkg/widget.py", GLOBAL)])],
    );
}

/// Row 3. `shop::Item` misses the qualified callable lookup (a class is no
/// callable); the constructor fallback binds `Item`'s constructor, today.
#[test]
fn test_resolve_call_cpp_qualified_miss_constructor_fallback_binds() {
    let files = [
        (
            "src/shop.hpp",
            "namespace shop {\nclass Item {\npublic:\n    Item() {}\n};\n}\n",
        ),
        (
            "src/app.cpp",
            "#include \"shop.hpp\"\n\nshop::Item* make_item() {\n    return new shop::Item();\n}\n",
        ),
    ];
    pin_row(
        &files,
        "src/app.cpp",
        &[("make_item", &[("Item", "src/shop.hpp", &[0.85])])],
    );
}

/// Row 4. `util` is a mapped Go package that lacks `Missing`. The call ends
/// there: `other.Missing`, which the bare name would find, gets no edge.
#[test]
fn test_resolve_call_go_package_missing_emits_no_edge() {
    let files = [
        ("go.mod", "module example.com/repo\n\ngo 1.21\n"),
        (
            "main.go",
            "package main\n\nimport \"example.com/repo/util\"\n\nfunc run() {\n\tutil.Missing()\n}\n",
        ),
        ("util/util.go", "package util\n\nfunc Present() {}\n"),
        ("other/other.go", "package other\n\nfunc Missing() {}\n"),
    ];
    pin_row(&files, "main.go", &[("run", &[])]);
}

/// Row 5. `config` is a value the package `app` declares, not a submodule.
/// A namesake `get_db` in `fallback.py` keeps the name tiers busy; the
/// import-only path emits no edge, today. A nested `get_db` in `main.py` is
/// left out: today it binds same-file at 1.0 across the function scope, the
/// Python lexical opt-in's defect (slice R8), not this row's fallback.
#[test]
fn test_resolve_call_python_declared_attribute_import_only_emits_no_edge() {
    let files = [
        ("app/__init__.py", "class config:\n    pass\n"),
        ("app/config/settings.py", "def get_db():\n    return 1\n"),
        ("fallback.py", "def get_db():\n    return 2\n"),
        (
            "main.py",
            "from app import config\n\n\ndef caller():\n    return config.settings.get_db()\n",
        ),
    ];
    pin_row(&files, "main.py", &[("caller", &[])]);
}

/// Row 6. `helper` is declared inside `other`, out of reach of `run`: the
/// lexical lookup is `Shadowed`. The name tiers skip, so `run` does not bind
/// the nested function; the named import still binds `lib.js` at 0.95.
#[test]
fn test_resolve_call_js_lexical_shadowed_named_import_binds() {
    let files = [
        ("lib.js", "export function helper() {}\n"),
        (
            "main.js",
            "import { helper } from \"./lib\";\n\nfunction other() {\n  function helper() {}\n  return helper;\n}\n\nfunction run() {\n  return helper();\n}\n",
        ),
    ];
    pin_row(
        &files,
        "main.js",
        &[("run", &[("helper", "lib.js", &[0.95])])],
    );
}

/// Row 7. `Foo` has no `missing`, so that chain level is broken and the outer
/// `bar` gets no edge, though `Other.bar` is the only `bar` and the untyped
/// fallback would bind it. `useControl` shows a sound chain still binds.
#[test]
fn test_resolve_call_chain_level_broken_outer_emits_no_edge() {
    let files = [
        (
            "src/foo.ts",
            "export class Foo {\n  static create(): Foo {\n    return new Foo();\n  }\n  ok(): void {}\n}\n",
        ),
        (
            "src/other.ts",
            "export class Other {\n  bar(): void {}\n}\n",
        ),
        (
            "src/use.ts",
            "import { Foo } from \"./foo\";\n\nexport function useBroken() {\n  Foo.create().missing().bar();\n}\n\nexport function useControl() {\n  Foo.create().ok();\n}\n",
        ),
    ];
    pin_row(
        &files,
        "src/use.ts",
        &[
            ("useBroken", &[("create", "src/foo.ts", &[0.85])]),
            (
                "useControl",
                &[
                    ("create", "src/foo.ts", &[0.85]),
                    ("ok", "src/foo.ts", &[0.85]),
                ],
            ),
        ],
    );
}
