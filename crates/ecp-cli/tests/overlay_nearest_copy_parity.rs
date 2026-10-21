//! Calls into duplicated source trees out of a dirty file: the session
//! overlay binds the same nearest copy as a full reindex, with the same
//! confidence and reason (`call:nearest-copy`, 0.6), over the 14 mainstream
//! languages.
//!
//! Each [`Copies`] fixture writes its files into `core/` and into
//! `android/core/` (guava's layout), so the caller in each tree must bind the
//! copy in that same tree. Three edits run per fixture: the `core/` caller,
//! the `android/core/` caller, and the `core/` caller with its copy (the copy
//! then competes as an overlay symbol). Every repo is indexed from a
//! temporary root holding all its files, so manifests (`Cargo.toml`,
//! `go.mod`, `package.json`) are on disk as the indexer sees them.
//!
//! The index binds a bare name to the nearest copy at its global tier, and a
//! qualified call (`Maps.make()`) at its receiver-typing ladder. The overlay
//! replays the global tier, also where an import tier falls back to it
//! (Python and Java imports), and not the ladder. Where the parser records a
//! qualified call by its bare member name, the call reaches the global tier
//! and the overlay matches:
//!
//! | language   | bare name (global tier)          | qualified call (ladder)        |
//! |------------|----------------------------------|--------------------------------|
//! | TypeScript | tested: package import           | N/A: package import is bare    |
//! | JavaScript | tested: package import           | N/A: package import is bare    |
//! | Python     | tested: `from` import of a fn    | tested: the retry drops `PyMaps.` |
//! | Java       | tested: static import            | known divergence               |
//! | Kotlin     | tested: top-level fn             | tested: parser keeps it bare   |
//! | C#         | tested: bare member call         | tested: parser keeps it bare   |
//! | Go         | tested: same package             | tested: parser keeps it bare   |
//! | Rust       | tested: crate import             | N/A: crate import is bare      |
//! | PHP        | tested: namespaced fn            | known divergence               |
//! | Ruby       | tested: top-level method         | known divergence               |
//! | Swift      | tested: top-level fn             | tested: parser keeps it bare   |
//! | C          | tested: bare call                | N/A: C has no qualified calls  |
//! | C++        | tested: free fn                  | known divergence               |
//! | Dart       | tested: top-level fn             | tested: parser keeps it bare   |
//!
//! The C# bare call has no `using static`, which C# needs to compile it: the
//! resolver sees the same bare name either way.

use ecp_analyzer::resolution::builder::GraphBuilder;
use ecp_core::analyzer::types::{LocalGraph, NEAREST_COPY_CONFIDENCE, NEAREST_COPY_REASON};
use ecp_core::graph::{ArchivedZeroCopyGraph, RelType, ZeroCopyGraph};
use ecp_core::session::merged::MergedGraph;
use ecp_core::session::view::{OverlayFileInput, OverlayView};
use ecp_core::session::OVERLAY_EDGE_REASON;
use std::path::Path;

const TREES: [&str; 2] = ["core", "android/core"];

/// One declaration repeated in both trees, and a caller in each tree whose
/// `t` calls it.
struct Copies {
    label: &'static str,
    /// Files written into each tree, paths relative to the tree.
    tree: &'static [(&'static str, &'static str)],
    /// Files written once at the repo root.
    shared: &'static [(&'static str, &'static str)],
    /// The caller's file, relative to a tree.
    caller: &'static str,
    /// The file of the copy the caller binds, relative to a tree.
    copy: &'static str,
    callee: &'static str,
}

/// Fixtures whose overlay still differs from the reindex, with the reason.
/// Asserted to differ, so closing one turns the test red until it goes.
const LADDER: &str = "a qualified call into duplicated trees binds at the index's \
                      receiver-typing ladder, which the overlay does not replay";
const KNOWN_DIVERGENCES: &[(&str, &str)] = &[
    ("Java/qualified static call", LADDER),
    ("PHP/static call", LADDER),
    ("Ruby/constant call", LADDER),
    ("C++/static member call", LADDER),
];

const COPIES: &[Copies] = &[
    Copies {
        label: "TypeScript/package import",
        tree: &[
            ("tsp/pkgx/package.json", "{\"name\":\"pkgx\",\"main\":\"src/index.ts\"}\n"),
            ("tsp/pkgx/src/index.ts", "export function tsMake() {}\n"),
            ("tsp/app/use.ts", "import { tsMake } from 'pkgx';\nexport function t() { tsMake(); }\n"),
        ],
        shared: &[],
        caller: "tsp/app/use.ts",
        copy: "tsp/pkgx/src/index.ts",
        callee: "tsMake",
    },
    Copies {
        label: "JavaScript/package import",
        tree: &[
            ("jsp/pkgx/package.json", "{\"name\":\"pkgx\",\"main\":\"src/index.js\"}\n"),
            ("jsp/pkgx/src/index.js", "export function jsMake() {}\n"),
            ("jsp/app/use.js", "import { jsMake } from 'pkgx';\nexport function t() { jsMake(); }\n"),
        ],
        shared: &[],
        caller: "jsp/app/use.js",
        copy: "jsp/pkgx/src/index.js",
        callee: "jsMake",
    },
    // The module is ambiguous, so the import tier retries the bare name.
    Copies {
        label: "Python/function through a from import",
        tree: &[
            ("py/pkg/__init__.py", ""),
            ("py/pkg/maps.py", "def pmake():\n    pass\n"),
            ("py/tests/test_maps.py", "from pkg.maps import pmake\ndef t():\n    pmake()\n"),
        ],
        shared: &[],
        caller: "py/tests/test_maps.py",
        copy: "py/pkg/maps.py",
        callee: "pmake",
    },
    // Both copies hold `p.Maps`, so the import tier falls back to the name.
    Copies {
        label: "Java/static import",
        tree: &[
            ("src/p/Maps.java", "package p;\npublic class Maps {\n  public static void make() {}\n}\n"),
            ("test/p/MapsTest.java", "package p;\nimport static p.Maps.make;\nclass MapsTest {\n  void t() { make(); }\n}\n"),
        ],
        shared: &[],
        caller: "test/p/MapsTest.java",
        copy: "src/p/Maps.java",
        callee: "make",
    },
    Copies {
        label: "Kotlin/top-level function",
        tree: &[
            ("src/p/KMaps.kt", "package p\nfun kMake() {}\n"),
            ("test/p/KMapsTest.kt", "package p\nfun t() { kMake() }\n"),
        ],
        shared: &[],
        caller: "test/p/KMapsTest.kt",
        copy: "src/p/KMaps.kt",
        callee: "kMake",
    },
    Copies {
        label: "C#/bare member call",
        tree: &[
            ("src/p/CMaps.cs", "namespace P {\npublic static class CMaps {\n  public static void Make() {}\n}\n}\n"),
            ("test/p/CMapsTest.cs", "namespace P {\nclass CMapsTest {\n  void t() { Make(); }\n}\n}\n"),
        ],
        shared: &[],
        caller: "test/p/CMapsTest.cs",
        copy: "src/p/CMaps.cs",
        callee: "Make",
    },
    Copies {
        label: "Go/same package",
        tree: &[
            ("go/pkg/pkg.go", "package pkg\n\nfunc Make() {}\n"),
            ("go/pkg/use.go", "package pkg\n\nfunc t() { Make() }\n"),
        ],
        shared: &[],
        caller: "go/pkg/use.go",
        copy: "go/pkg/pkg.go",
        callee: "Make",
    },
    // The workspace holds `core/dup` and `core/app`; the `android/` copy of
    // the crate sits outside it.
    Copies {
        label: "Rust/crate import",
        tree: &[
            ("dup/Cargo.toml", "[package]\nname = \"dup\"\nversion = \"0.1.0\"\n"),
            ("dup/src/lib.rs", "pub fn rs_make() {}\n"),
            ("app/Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\n[dependencies]\ndup = { path = \"../dup\" }\n"),
            ("app/src/lib.rs", "use dup::rs_make;\npub fn t() { rs_make(); }\n"),
        ],
        shared: &[("Cargo.toml", "[workspace]\nmembers = [\"core/app\", \"core/dup\"]\n")],
        caller: "app/src/lib.rs",
        copy: "dup/src/lib.rs",
        callee: "rs_make",
    },
    Copies {
        label: "PHP/namespaced function",
        tree: &[
            ("src/p/pmaps.php", "<?php\nnamespace P;\nfunction pmake() {}\n"),
            ("test/p/pmaps_test.php", "<?php\nnamespace P;\nfunction t() { pmake(); }\n"),
        ],
        shared: &[],
        caller: "test/p/pmaps_test.php",
        copy: "src/p/pmaps.php",
        callee: "pmake",
    },
    Copies {
        label: "Ruby/top-level method",
        tree: &[
            ("rb/lib/rmaps.rb", "def rmake\nend\n"),
            ("rb/test/rmaps_test.rb", "# test\ndef t\n  rmake()\nend\n"),
        ],
        shared: &[],
        caller: "rb/test/rmaps_test.rb",
        copy: "rb/lib/rmaps.rb",
        callee: "rmake",
    },
    Copies {
        label: "Swift/top-level function",
        tree: &[
            ("sw/Sources/Dup/SMaps.swift", "public func smake() {}\n"),
            ("sw/Tests/SMapsTests.swift", "// test\nfunc t() { smake() }\n"),
        ],
        shared: &[],
        caller: "sw/Tests/SMapsTests.swift",
        copy: "sw/Sources/Dup/SMaps.swift",
        callee: "smake",
    },
    Copies {
        label: "C/bare call",
        tree: &[
            ("c/src/cm.c", "void c_make(void) {}\n"),
            ("c/test/t.c", "// test\nvoid t(void) { c_make(); }\n"),
        ],
        shared: &[],
        caller: "c/test/t.c",
        copy: "c/src/cm.c",
        callee: "c_make",
    },
    Copies {
        label: "C++/free function",
        tree: &[
            ("cpp/src/cm.cpp", "void cpp_make() {}\n"),
            ("cpp/test/t.cpp", "// test\nvoid t() { cpp_make(); }\n"),
        ],
        shared: &[],
        caller: "cpp/test/t.cpp",
        copy: "cpp/src/cm.cpp",
        callee: "cpp_make",
    },
    Copies {
        label: "Dart/top-level function",
        tree: &[
            ("dart/lib/dmaps.dart", "void dmake() {}\n"),
            ("dart/test/dmaps_test.dart", "// test\nvoid t() { dmake(); }\n"),
        ],
        shared: &[],
        caller: "dart/test/dmaps_test.dart",
        copy: "dart/lib/dmaps.dart",
        callee: "dmake",
    },
    // ── Qualified calls: ecp-analyzer's `duplicate_tree_calls.rs` shapes ──
    Copies {
        label: "Java/qualified static call",
        tree: &[
            ("src/p/Maps.java", "package p;\npublic class Maps { public static void make() {} }\n"),
            ("test/p/MapsTest.java", "package p;\nclass MapsTest { void t() { Maps.make(); } }\n"),
        ],
        shared: &[],
        caller: "test/p/MapsTest.java",
        copy: "src/p/Maps.java",
        callee: "make",
    },
    Copies {
        label: "Kotlin/object call",
        tree: &[
            ("src/p/KMaps.kt", "package p\nobject KMaps { fun make() {} }\n"),
            ("test/p/KMapsTest.kt", "package p\nclass KMapsTest { fun t() { KMaps.make() } }\n"),
        ],
        shared: &[],
        caller: "test/p/KMapsTest.kt",
        copy: "src/p/KMaps.kt",
        callee: "make",
    },
    Copies {
        label: "C#/static class call",
        tree: &[
            ("src/p/CMaps.cs", "namespace P { public static class CMaps { public static void Make() {} } }\n"),
            ("test/p/CMapsTest.cs", "namespace P { class CMapsTest { void t() { CMaps.Make(); } } }\n"),
        ],
        shared: &[],
        caller: "test/p/CMapsTest.cs",
        copy: "src/p/CMaps.cs",
        callee: "Make",
    },
    Copies {
        label: "PHP/static call",
        tree: &[
            ("src/p/PMaps.php", "<?php\nnamespace P;\nclass PMaps { public static function make() {} }\n"),
            ("test/p/PMapsTest.php", "<?php\nnamespace P;\nclass PMapsTest { function t() { PMaps::make(); } }\n"),
        ],
        shared: &[],
        caller: "test/p/PMapsTest.php",
        copy: "src/p/PMaps.php",
        callee: "make",
    },
    Copies {
        label: "Python/class through a from import",
        tree: &[
            ("py/pkg/__init__.py", ""),
            ("py/pkg/maps.py", "class PyMaps:\n    @staticmethod\n    def make():\n        pass\n"),
            ("py/tests/test_maps.py", "from pkg.maps import PyMaps\ndef t():\n    PyMaps.make()\n"),
        ],
        shared: &[],
        caller: "py/tests/test_maps.py",
        copy: "py/pkg/maps.py",
        callee: "make",
    },
    Copies {
        label: "Ruby/constant call",
        tree: &[
            ("rb/lib/rmaps.rb", "class RMaps\n  def self.make\n  end\nend\n"),
            ("rb/test/rmaps_test.rb", "class RMapsTest\n  def t\n    RMaps.make\n  end\nend\n"),
        ],
        shared: &[],
        caller: "rb/test/rmaps_test.rb",
        copy: "rb/lib/rmaps.rb",
        callee: "make",
    },
    Copies {
        label: "Swift/qualified call",
        tree: &[
            ("sw/Sources/Dup/SMaps.swift", "public class SMaps { public static func make() {} }\n"),
            ("sw/Tests/SMapsTests.swift", "import Dup\nclass SMapsTests { func t() { SMaps.make() } }\n"),
        ],
        shared: &[],
        caller: "sw/Tests/SMapsTests.swift",
        copy: "sw/Sources/Dup/SMaps.swift",
        callee: "make",
    },
    Copies {
        label: "Dart/qualified call",
        tree: &[
            ("dart/lib/dmaps.dart", "class DMaps { static void make() {} }\n"),
            ("dart/test/dmaps_test.dart", "import 'package:dup/dmaps.dart';\nvoid t() { DMaps.make(); }\n"),
        ],
        shared: &[],
        caller: "dart/test/dmaps_test.dart",
        copy: "dart/lib/dmaps.dart",
        callee: "make",
    },
    Copies {
        label: "Go/package call",
        tree: &[
            ("go/go.mod", "module example.com/dup\n\ngo 1.21\n"),
            ("go/pkg/pkg.go", "package pkg\n\nfunc Make() {}\n"),
            ("go/cmd/main.go", "package main\n\nimport \"example.com/dup/pkg\"\n\nfunc t() { pkg.Make() }\n"),
        ],
        shared: &[],
        caller: "go/cmd/main.go",
        copy: "go/pkg/pkg.go",
        callee: "Make",
    },
    Copies {
        label: "C++/static member call",
        tree: &[
            ("cpp/src/cmaps.cpp", "class CppMaps { public: static void make() {} };\n"),
            ("cpp/test/t.cpp", "void t() { CppMaps::make(); }\n"),
        ],
        shared: &[],
        caller: "cpp/test/t.cpp",
        copy: "cpp/src/cmaps.cpp",
        callee: "make",
    },
];

/// Same-named declarations that are no copies of one declaration, or copies
/// no caller is nearer to: the index binds nothing, and neither may the
/// overlay.
struct Distinct {
    label: &'static str,
    files: &'static [(&'static str, &'static str)],
    /// The dirty files: the caller first, then any other file to edit too.
    edits: &'static [&'static str],
    callee: &'static str,
}

const DISTINCT: &[Distinct] = &[
    Distinct {
        label: "C/equidistant copies",
        files: &[
            ("left/c/src/cm.c", "void c_make(void) {}\n"),
            ("right/c/src/cm.c", "void c_make(void) {}\n"),
            ("mid/c/test/t.c", "// test\nvoid t(void) { c_make(); }\n"),
        ],
        edits: &["mid/c/test/t.c"],
        callee: "c_make",
    },
    Distinct {
        label: "C/differently named files",
        files: &[
            ("core/c/src/a.c", "void c_make(void) {}\n"),
            ("android/core/c/src/b.c", "void c_make(void) {}\n"),
            ("core/c/test/t.c", "// test\nvoid t(void) { c_make(); }\n"),
        ],
        edits: &["core/c/test/t.c"],
        callee: "c_make",
    },
    Distinct {
        label: "Python/same file name in another package",
        files: &[
            ("core/a/util.py", "def helper():\n    pass\n"),
            ("android/core/b/util.py", "def helper():\n    pass\n"),
            ("core/a/app.py", "# app\ndef t():\n    helper()\n"),
        ],
        edits: &["core/a/app.py"],
        callee: "helper",
    },
    // Same directory and file name, another package: no copy.
    Distinct {
        label: "Kotlin/same file in another package",
        files: &[
            ("core/src/p/KMaps.kt", "package p\nfun kMake() {}\n"),
            ("android/core/src/p/KMaps.kt", "package q\nfun kMake() {}\n"),
            (
                "core/test/p/KMapsTest.kt",
                "package p\nfun t() { kMake() }\n",
            ),
        ],
        edits: &["core/test/p/KMapsTest.kt"],
        callee: "kMake",
    },
    // The edited copy declares the other package in its fresh parse.
    Distinct {
        label: "Kotlin/edited copy in another package",
        files: &[
            ("core/src/p/KMaps.kt", "package p\nfun kMake() {}\n"),
            ("android/core/src/p/KMaps.kt", "package q\nfun kMake() {}\n"),
            (
                "core/test/p/KMapsTest.kt",
                "package p\nfun t() { kMake() }\n",
            ),
        ],
        edits: &["core/test/p/KMapsTest.kt", "android/core/src/p/KMaps.kt"],
        callee: "kMake",
    },
];

/// One `Calls` edge out of `t`, by identity, with its reason. The overlay's
/// uncommitted-edit marker reads as the index's plain `call`.
#[derive(Debug, PartialEq, PartialOrd)]
struct Call {
    callee_file: String,
    callee: String,
    callee_line: u32,
    confidence: f32,
    reason: String,
}

fn is_source(path: &str) -> bool {
    matches!(
        Path::new(path).extension().and_then(|e| e.to_str()),
        Some(
            "ts" | "js"
                | "py"
                | "java"
                | "kt"
                | "cs"
                | "go"
                | "rs"
                | "php"
                | "rb"
                | "swift"
                | "c"
                | "cpp"
                | "dart"
        )
    )
}

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

/// Index `files` from a temporary repo root that holds every one of them.
fn index(files: &[(String, String)]) -> ZeroCopyGraph {
    let root = tempfile::tempdir().expect("tempdir");
    let mut builder = GraphBuilder::new().with_repo_root(root.path().to_path_buf());
    for (rel, source) in files {
        let path = root.path().join(rel);
        std::fs::create_dir_all(path.parent().expect("a file has a directory")).unwrap();
        std::fs::write(&path, source).unwrap();
        if is_source(rel) {
            builder.add_graph(parse(rel, source));
        }
    }
    builder.build()
}

/// The `Calls` edges of every `t` declared in `caller_file`, sorted. No
/// dedup: a base edge the overlay fails to mask shows as a second edge.
fn calls<'g>(
    merged: &MergedGraph<'g>,
    graph: &'g ArchivedZeroCopyGraph,
    caller_file: &str,
) -> Vec<Call> {
    let mut edges: Vec<Call> = merged
        .all_edges()
        .filter(|edge| edge.rel_type() == RelType::Calls)
        .filter_map(|edge| {
            let caller = merged.node(edge.source)?;
            if caller.file_path(merged)? != caller_file || caller.name(merged) != "t" {
                return None;
            }
            let callee = merged.node(edge.target)?;
            let reason = match edge.reason(graph) {
                OVERLAY_EDGE_REASON => "call",
                reason => reason,
            };
            Some(Call {
                callee_file: callee.file_path(merged)?.to_string(),
                callee: callee.name(merged).to_string(),
                callee_line: callee.start_line(),
                confidence: edge.confidence(),
                reason: reason.to_string(),
            })
        })
        .collect();
    edges.sort_by(|a, b| a.partial_cmp(b).expect("a confidence is never NaN"));
    edges
}

/// The calls out of `edits[0]`'s `t` after `edits` (paths of `files`) gain
/// a blank second line: from a full reindex of the edited tree, and from
/// the overlay of the edits over the index of `files`.
fn reindex_and_overlay(files: &[(String, String)], edits: &[&str]) -> (Vec<Call>, Vec<Call>) {
    let edited_sources: Vec<(String, String)> = edits
        .iter()
        .map(|&path| {
            let (_, source) = files
                .iter()
                .find(|(file, _)| file == path)
                .unwrap_or_else(|| panic!("{path} is a fixture file"));
            (path.to_string(), shifted(source))
        })
        .collect();
    let edited_tree: Vec<(String, String)> = files
        .iter()
        .map(|(path, source)| {
            edited_sources
                .iter()
                .find(|(edited, _)| edited == path)
                .map_or_else(|| (path.clone(), source.clone()), Clone::clone)
        })
        .collect();
    let caller = edits[0];

    let full = rkyv::to_bytes::<rkyv::rancor::Error>(&index(&edited_tree)).unwrap();
    let full = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&full).unwrap();
    let reindex = calls(&MergedGraph::new(full, None), full, caller);

    let base = rkyv::to_bytes::<rkyv::rancor::Error>(&index(files)).unwrap();
    let base = rkyv::access::<ArchivedZeroCopyGraph, rkyv::rancor::Error>(&base).unwrap();
    let inputs: Vec<OverlayFileInput> = edited_sources
        .iter()
        .map(|(path, source)| parse(path, source).into())
        .collect();
    let view = OverlayView::build(base, &inputs).expect("edits make a view");
    let overlay = calls(&MergedGraph::new(base, Some(&view)), base, caller);
    (reindex, overlay)
}

fn owned(files: &[(&str, &str)]) -> Vec<(String, String)> {
    files
        .iter()
        .map(|&(path, source)| (path.to_string(), source.to_string()))
        .collect()
}

#[test]
fn test_build_nearest_copy_call_matches_reindex_across_14_languages() {
    let mut failures = Vec::new();
    for fixture in COPIES {
        let mut files = owned(fixture.shared);
        for tree in TREES {
            files.extend(
                fixture
                    .tree
                    .iter()
                    .map(|&(path, source)| (format!("{tree}/{path}"), source.to_string())),
            );
        }
        let in_tree = |tree: &str, path: &str| format!("{tree}/{path}");
        let dirty_sets = [
            ("core caller", vec![in_tree(TREES[0], fixture.caller)]),
            ("android caller", vec![in_tree(TREES[1], fixture.caller)]),
            (
                "core caller+copy",
                vec![
                    in_tree(TREES[0], fixture.caller),
                    in_tree(TREES[0], fixture.copy),
                ],
            ),
        ];
        let known = KNOWN_DIVERGENCES
            .iter()
            .find(|(label, _)| *label == fixture.label);
        for (dirty_set, edits) in &dirty_sets {
            let label = format!("{}/{dirty_set}", fixture.label);
            let edits: Vec<&str> = edits.iter().map(String::as_str).collect();
            let (reindex, overlay) = reindex_and_overlay(&files, &edits);
            let tree = TREES
                .into_iter()
                .find(|tree| edits[0].starts_with(&format!("{tree}/")))
                .expect("the caller lies in a tree");
            let nearest = in_tree(tree, fixture.copy);
            let bound: Vec<_> = reindex
                .iter()
                .filter(|e| e.callee == fixture.callee)
                .collect();
            let proves = matches!(
                bound.as_slice(),
                [edge] if edge.callee_file == nearest
                    && edge.confidence == NEAREST_COPY_CONFIDENCE
                    && edge.reason == NEAREST_COPY_REASON
            );
            if !proves {
                failures.push(format!(
                    "{label}: the reindex does not bind the nearest copy {nearest}, so the fixture proves nothing: {reindex:#?}"
                ));
                continue;
            }
            match (known, overlay == reindex) {
                (None, true) | (Some(_), false) => {}
                (None, false) => {
                    failures.push(format!("{label}: overlay {overlay:#?}\nreindex {reindex:#?}"))
                }
                (Some((_, reason)), true) => failures.push(format!(
                    "{label}: known divergence ({reason}) is closed; remove it from KNOWN_DIVERGENCES"
                )),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn test_build_same_name_without_nearest_copy_binds_nothing_like_reindex() {
    let mut failures = Vec::new();
    for fixture in DISTINCT {
        let (reindex, overlay) = reindex_and_overlay(&owned(fixture.files), fixture.edits);
        if reindex.iter().any(|e| e.callee == fixture.callee) {
            failures.push(format!(
                "{}: the reindex binds `{}`, so the fixture proves nothing: {reindex:#?}",
                fixture.label, fixture.callee
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
