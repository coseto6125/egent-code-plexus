//! The call-level fallback rows of the shared resolution design (B §3.2,
//! pinned by §4.3 item 3). Each row plants one fact in a small tree. Its own
//! test asserts what a reader of the graph gets: the exact `Calls` edges of
//! one caller in a fresh full index. The all-rows test then asserts that the
//! overlay view of every row's tree (the caller file dirty, unchanged but for
//! a trailing newline) has no `Calls` edge that differs from that index, by
//! source uid, target uid and exact confidence.
//!
//! These are pin tests. They hold today's index behaviour and must stay green
//! through every slice that moves the ladders onto `Resolution`: a red row
//! means an edge moved. Every fixture carries a decoy that a wrong fallback
//! would bind (a namesake in another file, a nested function), so an empty
//! expectation is not a vacuous pass.
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
const IMPORT_SCOPED: &[f32] = &[0.95];

/// What one caller must call: `(callee name, callee file, allowed confidences)`.
type Callees<'a> = &'a [(&'a str, &'a str, &'a [f32])];

/// One fallback row: the tree, the file the overlay dirties, and, per caller
/// of that file, what it must call in a fresh full index.
struct Row {
    name: &'static str,
    files: Files<'static>,
    caller_file: &'static str,
    expected: &'static [(&'static str, Callees<'static>)],
}

/// Row 1. `ProtoBuf` is an external import (ladder `NoEdge(External)`), yet
/// the repo declares `fun BinaryFormat.asConverterFactory`. The call-level
/// extension retry binds the top-level function, at the global tier.
const KOTLIN_EXTERNAL_EXTENSION_RETRY: Row = Row {
    name: "kotlin_external_extension_retry",
    files: &[
        (
            "src/main/kotlin/conv/Factory.kt",
            "package conv\nimport kotlinx.serialization.BinaryFormat\nfun BinaryFormat.asConverterFactory(t: String): Any = t\n",
        ),
        (
            "src/test/kotlin/conv/FactoryTest.kt",
            "package conv\nimport kotlinx.serialization.protobuf.ProtoBuf\nfun setUp() { ProtoBuf.asConverterFactory(\"x\") }\n",
        ),
    ],
    caller_file: "src/test/kotlin/conv/FactoryTest.kt",
    expected: &[(
        "setUp",
        &[(
            "asConverterFactory",
            "src/main/kotlin/conv/Factory.kt",
            GLOBAL,
        )],
    )],
};

/// Row 2. Two functions named `Widget` make the callable lookup ambiguous
/// (no edge from the ladder). The constructor fallback then finds the one
/// class `Widget` and binds its `__init__`, at the global tier.
const PYTHON_AMBIGUOUS_CONSTRUCTOR_FALLBACK: Row = Row {
    name: "python_ambiguous_constructor_fallback",
    files: &[
        (
            "pkg/widget.py",
            "class Widget:\n    def __init__(self, x):\n        self.x = x\n",
        ),
        ("pkg/a/make_one.py", "def Widget():\n    return 1\n"),
        ("pkg/b/make_two.py", "def Widget():\n    return 2\n"),
        ("app/main.py", "def run():\n    return Widget(1)\n"),
    ],
    caller_file: "app/main.py",
    expected: &[("run", &[("__init__", "pkg/widget.py", GLOBAL)])],
};

/// Row 3. `shop::Item` misses the qualified callable lookup (a class is no
/// callable); the constructor fallback binds `Item`'s constructor.
const CPP_QUALIFIED_MISS_CONSTRUCTOR_FALLBACK: Row = Row {
    name: "cpp_qualified_miss_constructor_fallback",
    files: &[
        (
            "src/shop.hpp",
            "namespace shop {\nclass Item {\npublic:\n    Item() {}\n};\n}\n",
        ),
        (
            "src/app.cpp",
            "#include \"shop.hpp\"\n\nshop::Item* make_item() {\n    return new shop::Item();\n}\n",
        ),
    ],
    caller_file: "src/app.cpp",
    expected: &[("make_item", &[("Item", "src/shop.hpp", &[0.85])])],
};

/// Row 4. `util` is a mapped Go package that lacks `Missing`
/// (`NoEdge(PackageMissing)`). The call ends there: `other.Missing`, which
/// the bare name would find, gets no edge.
const GO_PACKAGE_MISSING: Row = Row {
    name: "go_package_missing",
    files: &[
        ("go.mod", "module example.com/repo\n\ngo 1.21\n"),
        (
            "main.go",
            "package main\n\nimport \"example.com/repo/util\"\n\nfunc run() {\n\tutil.Missing()\n}\n",
        ),
        ("util/util.go", "package util\n\nfunc Present() {}\n"),
        ("other/other.go", "package other\n\nfunc Missing() {}\n"),
    ],
    caller_file: "main.go",
    expected: &[("run", &[])],
};

/// Row 5. `config` is a value the package `app` declares, not a submodule
/// (`NoEdge(DeclaredAttribute)` on the import-only path). A namesake `get_db`
/// in `fallback.py` keeps the name tiers busy; no edge. A nested `get_db` in
/// `main.py` is left out: today it binds same-file at 1.0 across the function
/// scope, the Python lexical opt-in's defect (slice R8), not this row's
/// fallback.
const PYTHON_DECLARED_ATTRIBUTE_IMPORT_ONLY: Row = Row {
    name: "python_declared_attribute_import_only",
    files: &[
        ("app/__init__.py", "class config:\n    pass\n"),
        ("app/config/settings.py", "def get_db():\n    return 1\n"),
        ("fallback.py", "def get_db():\n    return 2\n"),
        (
            "main.py",
            "from app import config\n\n\ndef caller():\n    return config.settings.get_db()\n",
        ),
    ],
    caller_file: "main.py",
    expected: &[("caller", &[])],
};

/// Row 6. `helper` is declared inside `other`, out of reach of `run`: the
/// lexical lookup is `Shadowed`. The name tiers skip, so `run` does not bind
/// the nested function; the named import still binds `lib.js` at 0.95.
const JS_LEXICAL_SHADOWED_NAMED_IMPORT: Row = Row {
    name: "js_lexical_shadowed_named_import",
    files: &[
        ("lib.js", "export function helper() {}\n"),
        (
            "main.js",
            "import { helper } from \"./lib\";\n\nfunction other() {\n  function helper() {}\n  return helper;\n}\n\nfunction run() {\n  return helper();\n}\n",
        ),
    ],
    caller_file: "main.js",
    expected: &[("run", &[("helper", "lib.js", IMPORT_SCOPED)])],
};

/// Row 7. `Foo` has no `missing`, so that chain level is broken and the outer
/// `bar` gets no edge, though `Other.bar` is the only `bar` and the untyped
/// fallback would bind it. `useControl` shows a sound chain still binds.
const CHAIN_LEVEL_BROKEN: Row = Row {
    name: "chain_level_broken",
    files: &[
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
    ],
    caller_file: "src/use.ts",
    expected: &[
        ("useBroken", &[("create", "src/foo.ts", &[0.85])]),
        (
            "useControl",
            &[
                ("create", "src/foo.ts", &[0.85]),
                ("ok", "src/foo.ts", &[0.85]),
            ],
        ),
    ],
};

/// Row 8. `from pkg.sub import mod; mod.compute()` binds the submodule ahead
/// of the ladder (`Bound`), at the import tier. The namesake `compute` in
/// `lib/other.py` stays unbound: a bound call takes no fallback.
const PYTHON_SUBMODULE_MEMBER_BOUND: Row = Row {
    name: "python_submodule_member_bound",
    files: &[
        ("pkg/__init__.py", ""),
        ("pkg/sub/__init__.py", ""),
        ("pkg/sub/mod.py", "def compute():\n    return 3\n"),
        ("lib/other.py", "def compute():\n    return 4\n"),
        (
            "app.py",
            "from pkg.sub import mod\n\n\ndef go():\n    return mod.compute()\n",
        ),
    ],
    caller_file: "app.py",
    expected: &[("go", &[("compute", "pkg/sub/mod.py", IMPORT_SCOPED)])],
};

/// Row 9. An untyped receiver (`obj`, a parameter) reaches no free function
/// of the caller's own module: the post-filter refuses `helper`, the only
/// `helper`, which the global tier binds. Another module's function is a
/// module value and passes (`run_module`).
const PYTHON_UNTYPED_POST_FILTER: Row = Row {
    name: "python_untyped_post_filter",
    files: &[
        ("lib/tools.py", "def compute():\n    return 2\n"),
        (
            "app/main.py",
            "def helper():\n    return 1\n\n\ndef run(obj):\n    return obj.helper()\n\n\ndef run_module(api):\n    return api.compute()\n",
        ),
    ],
    caller_file: "app/main.py",
    expected: &[
        ("run", &[]),
        ("run_module", &[("compute", "lib/tools.py", GLOBAL)]),
    ],
};

/// Row 10. `cfg` names no imported package, so `cfg.Load()` is a member
/// call on a value: the untyped post-filter refuses the free function `Load`
/// of the same package, which the global tier binds. `util.Present()` is a
/// mapped package member, bound ahead of the ladder at the import tier.
const GO_VALUE_RECEIVER_POST_FILTER: Row = Row {
    name: "go_value_receiver_post_filter",
    files: &[
        ("go.mod", "module example.com/repo\n\ngo 1.21\n"),
        (
            "main.go",
            "package main\n\nimport \"example.com/repo/util\"\n\nfunc run() {\n\tutil.Present()\n\tcfg.Load()\n}\n",
        ),
        ("helpers.go", "package main\n\nfunc Load() {}\n"),
        ("util/util.go", "package util\n\nfunc Present() {}\n"),
    ],
    caller_file: "main.go",
    expected: &[("run", &[("Present", "util/util.go", IMPORT_SCOPED)])],
};

/// Row 11. The nested `function Widget` in `outer` is out of reach of `make`:
/// the lexical lookup is `Shadowed`. The callable import tier misses (a class
/// is no callable), so the constructor fallback runs through the import
/// tiers alone and binds the imported class's constructor at 0.95, never the
/// nested function.
const TS_LEXICAL_SHADOWED_CONSTRUCTOR_FALLBACK: Row = Row {
    name: "ts_lexical_shadowed_constructor_fallback",
    files: &[
        ("src/widget.ts", "export class Widget {\n  constructor() {}\n}\n"),
        (
            "src/main.ts",
            "import { Widget } from './widget';\n\nfunction outer() {\n  function Widget() {}\n}\n\nfunction make() {\n  return new Widget();\n}\n",
        ),
    ],
    caller_file: "src/main.ts",
    expected: &[("make", &[("constructor", "src/widget.ts", IMPORT_SCOPED)])],
};

/// Row 12. Row 11 in JavaScript.
const JS_LEXICAL_SHADOWED_CONSTRUCTOR_FALLBACK: Row = Row {
    name: "js_lexical_shadowed_constructor_fallback",
    files: &[
        ("src/widget.js", "export class Widget {\n  constructor() {}\n}\n"),
        (
            "src/main.js",
            "import { Widget } from './widget';\n\nfunction outer() {\n  function Widget() {}\n}\n\nfunction make() {\n  return new Widget();\n}\n",
        ),
    ],
    caller_file: "src/main.js",
    expected: &[("make", &[("constructor", "src/widget.js", IMPORT_SCOPED)])],
};

const ROWS: [&Row; 12] = [
    &KOTLIN_EXTERNAL_EXTENSION_RETRY,
    &PYTHON_AMBIGUOUS_CONSTRUCTOR_FALLBACK,
    &CPP_QUALIFIED_MISS_CONSTRUCTOR_FALLBACK,
    &GO_PACKAGE_MISSING,
    &PYTHON_DECLARED_ATTRIBUTE_IMPORT_ONLY,
    &JS_LEXICAL_SHADOWED_NAMED_IMPORT,
    &CHAIN_LEVEL_BROKEN,
    &PYTHON_SUBMODULE_MEMBER_BOUND,
    &PYTHON_UNTYPED_POST_FILTER,
    &GO_VALUE_RECEIVER_POST_FILTER,
    &TS_LEXICAL_SHADOWED_CONSTRUCTOR_FALLBACK,
    &JS_LEXICAL_SHADOWED_CONSTRUCTOR_FALLBACK,
];

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

/// The caller file's source after the edit: unchanged but for a trailing newline.
fn edited_source(row: &Row) -> String {
    row.files
        .iter()
        .find(|(path, _)| *path == row.caller_file)
        .map(|(_, source)| format!("{source}\n"))
        .expect("the caller file is one of the files")
}

/// Hold the full index of `row`'s edited tree to `row.expected`: each caller
/// calls exactly those callees, at an allowed confidence.
fn pin_row(row: &Row) {
    let edited_source = edited_source(row);
    let edited: Vec<(&str, &str)> = row
        .files
        .iter()
        .map(|&(path, source)| {
            if path == row.caller_file {
                (path, edited_source.as_str())
            } else {
                (path, source)
            }
        })
        .collect();

    let edges = call_edges(&index_snapshot(&edited));
    for (caller, callees) in row.expected {
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
        assert_eq!(
            names, want,
            "{}: index Calls edges of {caller}: {actual:?}",
            row.name
        );
        for (name, file, confidence) in &actual {
            let (_, _, allowed) = callees
                .iter()
                .find(|(n, f, _)| n == name && f == file)
                .unwrap();
            assert!(
                allowed.iter().any(|c| format!("{c:?}") == *confidence),
                "{}: {caller} -> {name} at {confidence}, allowed {allowed:?}",
                row.name
            );
        }
    }
}

/// The `Calls` lines (source uid, target uid, exact confidence) on which the
/// overlay view of `row` differs from a fresh full index of the edited tree.
fn overlay_call_differences(row: &Row) -> BTreeSet<String> {
    let edited_source = edited_source(row);
    observe(&Scenario {
        name: "resolution_fallback_rows",
        owner: "R1a (B section 3.2 fallback rows)",
        files: row.files,
        edits: &[Edit::Write(row.caller_file, &edited_source)],
    })
    .into_iter()
    .filter(|line| line.starts_with("edge ") && line.contains(" Calls "))
    .collect()
}

#[test]
fn test_resolve_call_kotlin_typed_receiver_external_extension_retry_binds() {
    pin_row(&KOTLIN_EXTERNAL_EXTENSION_RETRY);
}

#[test]
fn test_resolve_call_python_global_ambiguous_constructor_fallback_binds_class() {
    pin_row(&PYTHON_AMBIGUOUS_CONSTRUCTOR_FALLBACK);
}

#[test]
fn test_resolve_call_cpp_qualified_miss_constructor_fallback_binds() {
    pin_row(&CPP_QUALIFIED_MISS_CONSTRUCTOR_FALLBACK);
}

#[test]
fn test_resolve_call_go_package_missing_emits_no_edge() {
    pin_row(&GO_PACKAGE_MISSING);
}

#[test]
fn test_resolve_call_python_declared_attribute_import_only_emits_no_edge() {
    pin_row(&PYTHON_DECLARED_ATTRIBUTE_IMPORT_ONLY);
}

#[test]
fn test_resolve_call_js_lexical_shadowed_named_import_binds() {
    pin_row(&JS_LEXICAL_SHADOWED_NAMED_IMPORT);
}

#[test]
fn test_resolve_call_chain_level_broken_outer_emits_no_edge() {
    pin_row(&CHAIN_LEVEL_BROKEN);
}

#[test]
fn test_resolve_call_python_submodule_member_bound_skips_fallbacks() {
    pin_row(&PYTHON_SUBMODULE_MEMBER_BOUND);
}

#[test]
fn test_resolve_call_python_untyped_receiver_post_filter_refuses_own_module_function() {
    pin_row(&PYTHON_UNTYPED_POST_FILTER);
}

#[test]
fn test_resolve_call_go_value_receiver_post_filter_refuses_free_function() {
    pin_row(&GO_VALUE_RECEIVER_POST_FILTER);
}

#[test]
fn test_resolve_imported_call_ts_lexical_shadowed_constructor_fallback_binds_constructor() {
    pin_row(&TS_LEXICAL_SHADOWED_CONSTRUCTOR_FALLBACK);
}

#[test]
fn test_resolve_imported_call_js_lexical_shadowed_constructor_fallback_binds_constructor() {
    pin_row(&JS_LEXICAL_SHADOWED_CONSTRUCTOR_FALLBACK);
}

/// Both resolvers decide every row from one `Resolution`, so the overlay's
/// `Calls` edges equal the index's on every row. Rows 10, 11 and 12 are the
/// ones where the overlay decided differently before R1b: its closure skipped
/// the untyped post-filter for a Go value receiver, and its lexical-shadow
/// branch skipped the constructor fallback.
#[test]
fn test_resolve_call_index_and_overlay_same_fallback_decision_all_rows() {
    let differing: Vec<(&str, BTreeSet<String>)> = ROWS
        .iter()
        .map(|row| (row.name, overlay_call_differences(row)))
        .filter(|(_, lines)| !lines.is_empty())
        .collect();
    assert!(
        differing.is_empty(),
        "the overlay's Calls edges differ from the full index: {differing:#?}"
    );
}
