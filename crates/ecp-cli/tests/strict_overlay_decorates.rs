//! `Decorates` edges and shared `Annotation` nodes of the incremental
//! overlay (F7b1-3): for a dirty file, the overlay view rebuilds each
//! declaration's `Decorates` edges as a fresh full index of the edited tree
//! has them, with the index's confidence and reason, and keeps one
//! `Annotation` node per decorator name while any file still uses it,
//! through the production index, fragment write and read
//! (`common/strict_parity.rs`).
//!
//! Each check first asserts the reindex has the `Annotation` nodes the edit
//! adds and the base index had the ones it drops, so a fixture whose parser
//! records no decorator fails instead of passing empty.
//!
//! Exempt languages, which record no decorator, so emit no `Decorates`:
//!
//! - dart: `crates/ecp-analyzer/src/dart/queries.scm` has no `@decorator`
//!   capture; `dart/parser.rs:124` reads `capture_index_for_name("decorator")`.
//! - ruby: `crates/ecp-analyzer/src/ruby/queries.scm` has no `@decorator`
//!   capture; `ruby/parser.rs:391` reads `capture_index_for_name("decorator")`.
//!
//! Only the dirty files' edges are rebuilt: a clean file's decorator whose
//! name a dirty file now declares keeps its indexed target (F7b2).

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use ecp_core::graph::{ArchivedZeroCopyGraph, NodeKind, RelType};
use ecp_core::session::merged::MergedGraph;
use strict_parity::{Edit, Scenario};

const OWNER: &str = "lane F (incremental overlay: builder edges of dirty files)";

/// One language: two files, each a list of declarations that carry one
/// decorator or none. `file(stem, decls)` is the source of a file whose
/// declarations are `(name, decorator)`, `""` for none; `full_name(d)` is
/// the `Annotation` name of the decorator `d`.
struct Fixture {
    lang: &'static str,
    main: &'static str,
    other: &'static str,
    file: fn(&str, &[(String, &str)]) -> String,
    full_name: fn(&str) -> String,
}

/// Go symbol pragmas are an allowlist (`go/parser.rs` `GO_SYMBOL_PRAGMAS`):
/// each decorator name stands for one of them.
fn go_pragma(name: &str) -> &'static str {
    match name {
        "Shared" => "noinline",
        "Last" => "nosplit",
        "Old" => "norace",
        "New" => "noescape",
        _ => "nointerface",
    }
}

fn same(name: &str) -> String {
    name.to_string()
}

/// `decl(name, decorator)` per declaration, joined.
fn each(decls: &[(String, &str)], decl: impl Fn(&str, &str) -> String) -> String {
    decls
        .iter()
        .map(|(name, d)| decl(name.as_str(), d))
        .collect()
}

/// `text` when `d` names a decorator, else nothing.
fn when(d: &str, text: String) -> String {
    if d.is_empty() {
        String::new()
    } else {
        text
    }
}

fn fixtures() -> [Fixture; 12] {
    [
        Fixture {
            lang: "typescript",
            main: "src/main.ts",
            other: "src/helper.ts",
            file: |_, decls| {
                each(decls, |n, d| {
                    format!("{}export class {n} {{}}\n\n", when(d, format!("@{d}()\n")))
                })
            },
            full_name: same,
        },
        Fixture {
            lang: "javascript",
            main: "src/main.js",
            other: "src/helper.js",
            file: |_, decls| {
                each(decls, |n, d| {
                    format!("{}class {n} {{}}\n\n", when(d, format!("@{d}\n")))
                })
            },
            full_name: same,
        },
        Fixture {
            lang: "python",
            main: "main.py",
            other: "helper.py",
            file: |_, decls| {
                each(decls, |n, d| {
                    format!(
                        "{}def {n}():\n    return 0\n\n\n",
                        when(d, format!("@{d}\n"))
                    )
                })
            },
            full_name: same,
        },
        Fixture {
            lang: "java",
            main: "src/Main.java",
            other: "src/Helper.java",
            file: |stem, decls| {
                let methods = each(decls, |n, d| {
                    format!(
                        "    {}public void {n}() {{}}\n",
                        when(d, format!("@{d}\n    "))
                    )
                });
                format!("class {stem} {{\n{methods}}}\n")
            },
            full_name: same,
        },
        Fixture {
            lang: "kotlin",
            main: "src/Main.kt",
            other: "src/Helper.kt",
            file: |_, decls| {
                each(decls, |n, d| {
                    format!("{}class {n}\n\n", when(d, format!("@{d}\n")))
                })
            },
            full_name: same,
        },
        Fixture {
            lang: "csharp",
            main: "src/Main.cs",
            other: "src/Helper.cs",
            file: |_, decls| {
                each(decls, |n, d| {
                    format!("{}public class {n} {{}}\n\n", when(d, format!("[{d}]\n")))
                })
            },
            full_name: same,
        },
        Fixture {
            lang: "go",
            main: "main.go",
            other: "helper.go",
            file: |_, decls| {
                let funcs = each(decls, |n, d| {
                    format!(
                        "{}func {n}() {{}}\n\n",
                        when(d, format!("//go:{}\n", go_pragma(d)))
                    )
                });
                format!("package main\n\n{funcs}")
            },
            full_name: |d| format!("go:{}", go_pragma(d)),
        },
        Fixture {
            lang: "rust",
            main: "src/main.rs",
            other: "src/helper.rs",
            file: |_, decls| {
                each(decls, |n, d| {
                    format!("{}fn {n}() {{}}\n\n", when(d, format!("#[{d}]\n")))
                })
            },
            full_name: same,
        },
        Fixture {
            lang: "php",
            main: "src/main.php",
            other: "src/helper.php",
            file: |_, decls| {
                let funcs = each(decls, |n, d| {
                    format!("{}function {n}() {{}}\n\n", when(d, format!("#[{d}]\n")))
                });
                format!("<?php\n{funcs}")
            },
            full_name: same,
        },
        Fixture {
            lang: "swift",
            main: "Sources/App/Main.swift",
            other: "Sources/App/Helper.swift",
            file: |_, decls| {
                each(decls, |n, d| {
                    format!("{}class {n} {{}}\n\n", when(d, format!("@{d}\n")))
                })
            },
            full_name: same,
        },
        Fixture {
            lang: "c",
            main: "src/main.c",
            other: "src/helper.c",
            file: |_, decls| {
                each(decls, |n, d| {
                    format!(
                        "{}int {n}(void) {{ return 0; }}\n",
                        when(d, format!("[[{d}]] "))
                    )
                })
            },
            full_name: same,
        },
        Fixture {
            lang: "cpp",
            main: "src/main.cpp",
            other: "src/helper.cpp",
            file: |_, decls| {
                each(decls, |n, d| {
                    format!(
                        "{}int {n}() {{ return 0; }}\n",
                        when(d, format!("[[{d}]] "))
                    )
                })
            },
            full_name: same,
        },
    ]
}

/// `node` as `kind path:line name #uid`; a node with no owning file shows
/// `-` for its path (see
/// [`test_overlay_view_new_annotation_node_has_no_file_path`]).
fn describe(merged: &MergedGraph<'_>, idx: u32) -> Option<String> {
    let node = merged.node(idx)?;
    Some(format!(
        "{} {}:{} {} #{:016x}",
        node.kind().as_str(),
        node.file_path(merged)
            .filter(|path| !path.is_empty())
            .unwrap_or("-"),
        node.start_line(),
        node.name(merged),
        node.uid()
    ))
}

/// Every `Decorates` edge of `merged` as `source -> target conf reason`,
/// sorted. `graph` resolves a base edge's reason.
fn decorates<'a>(merged: &MergedGraph<'a>, graph: &'a ArchivedZeroCopyGraph) -> Vec<String> {
    let mut out: Vec<String> = merged
        .all_edges()
        .filter(|edge| edge.rel_type() == RelType::Decorates)
        .filter_map(|edge| {
            Some(format!(
                "{} -> {} conf={:?} reason={}",
                describe(merged, edge.source)?,
                describe(merged, edge.target)?,
                edge.confidence(),
                edge.reason(graph),
            ))
        })
        .collect();
    out.sort();
    out
}

/// The full name of every visible `Annotation` node no file owns, sorted:
/// a multiset, so a doubled node shows twice.
fn annotations(merged: &MergedGraph<'_>) -> Vec<String> {
    let mut out: Vec<String> = (0..merged.node_count())
        .filter(|&idx| idx >= merged.base_len() || merged.base_visible(idx))
        .filter_map(|idx| merged.node(idx))
        .filter(|node| {
            node.kind() == NodeKind::Annotation && node.file_path(merged).is_none_or(str::is_empty)
        })
        .map(|node| node.name(merged).to_string())
        .collect();
    out.sort();
    out
}

/// Run `files` + `edits` and assert the overlay's `Decorates` edges and
/// `Annotation` nodes are the reindex's, that the reindex has an
/// `Annotation` node for each of `gained`, and that the base index had one
/// for each of `lost` and the reindex has none. Returns the reindex's
/// `Decorates` edges, as [`decorates`] renders them.
fn assert_decorates_match(
    label: &str,
    files: &[(&str, &str)],
    edits: &[Edit<'_>],
    gained: &[String],
    lost: &[String],
) -> Vec<String> {
    let observed = strict_parity::run(&Scenario {
        name: "",
        owner: OWNER,
        files,
        edits,
    });
    let (full_graph, base_graph) = (observed.full(), observed.base());
    let full_merged = MergedGraph::new(full_graph, None);
    let full = annotations(&full_merged);
    let base = annotations(&MergedGraph::new(base_graph, None));
    for name in gained {
        assert!(
            full.contains(name),
            "{label}: the reindex has the Annotation {name}: {full:#?}"
        );
    }
    for name in lost {
        assert!(
            base.contains(name),
            "{label}: the base index had the Annotation {name}: {base:#?}"
        );
        assert!(
            !full.contains(name),
            "{label}: the reindex has no Annotation {name}: {full:#?}"
        );
    }
    let view = observed.view();
    let overlay = MergedGraph::new(base_graph, view.as_ref());
    let full_edges = decorates(&full_merged, full_graph);
    assert_eq!(
        decorates(&overlay, base_graph),
        full_edges,
        "{label}: the Decorates edges"
    );
    assert_eq!(annotations(&overlay), full, "{label}: the Annotation nodes");
    full_edges
}

/// The dirty file drops `Shared`, which the clean file still uses (the node
/// stays), drops `Last`, which no other file uses (the node goes), renames
/// `Old` to `New`, a name new to the repo (a new node), and adds `Other`,
/// which the clean file uses (no second node), in each language that
/// records decorators. Red before F7b1-3: the overlay kept the base
/// index's edges and nodes.
#[test]
fn test_overlay_view_dirty_decorates_matches_reindex() {
    for fx in &fixtures() {
        let decls = |stem: &str, decorators: &[&'static str]| -> Vec<(String, &'static str)> {
            decorators
                .iter()
                .enumerate()
                .map(|(i, &d)| (format!("{stem}{i}"), d))
                .collect()
        };
        let before = (fx.file)("Main", &decls("Main", &["Shared", "Last", "Old"]));
        let after = (fx.file)("Main", &decls("Main", &["Other", "", "New"]));
        let other = (fx.file)("Helper", &decls("Helper", &["Shared", "Other"]));
        let names =
            |list: &[&str]| -> Vec<String> { list.iter().map(|&d| (fx.full_name)(d)).collect() };
        assert_decorates_match(
            fx.lang,
            &[(fx.other, other.as_str()), (fx.main, before.as_str())],
            &[Edit::Write(fx.main, &after)],
            &names(&["Shared", "Other", "New"]),
            &names(&["Last", "Old"]),
        );
    }
}

const PY_FOO_A: &str = "@Foo\ndef alpha():\n    return 1\n";
const PY_FOO_B: &str = "@Foo\ndef beta():\n    return 2\n";

/// Two files use `@Foo`; the edit drops it from one. The one `Annotation`
/// node survives, with one `Decorates` edge, from the clean file.
#[test]
fn test_overlay_view_new_decorator_shares_annotation_node() {
    let observed = strict_parity::run(&Scenario {
        name: "",
        owner: OWNER,
        files: &[("a.py", PY_FOO_A), ("b.py", PY_FOO_B)],
        edits: &[Edit::Write("a.py", "def alpha():\n    return 1\n")],
    });
    let base = observed.base();
    let view = observed.view();
    let overlay = MergedGraph::new(base, view.as_ref());
    let edges = decorates(&overlay, base);
    assert_eq!(annotations(&overlay), ["Foo"]);
    assert_eq!(edges.len(), 1, "{edges:#?}");
    assert!(
        edges[0].starts_with("Function b.py:")
            && edges[0].contains(" beta #")
            && edges[0].contains(" -> Annotation -:1 Foo #"),
        "{edges:#?}"
    );
    let full = observed.full();
    assert_eq!(edges, decorates(&MergedGraph::new(full, None), full));
}

/// Both files dirty and both add `@Fresh`, new to the repo: the overlay
/// makes one `Annotation` node, as the reindex does.
#[test]
fn test_overlay_view_two_dirty_files_share_one_new_annotation_node() {
    let fresh = |name: &str| format!("@Fresh\ndef {name}():\n    return 1\n");
    let (alpha, beta) = (fresh("alpha"), fresh("beta"));
    assert_decorates_match(
        "two dirty files",
        &[("a.py", PY_FOO_A), ("b.py", PY_FOO_B)],
        &[Edit::Write("a.py", &alpha), Edit::Write("b.py", &beta)],
        &["Fresh".to_string()],
        &["Foo".to_string()],
    );
}

/// Arguments drop, a qualified decorator looks up its last segment and
/// names its node by the whole path, a class and its method of the same
/// name each take their edge, and a decorator an import binds reaches the
/// class it names with the tier's confidence.
#[test]
fn test_overlay_view_decorator_arguments_qualified_and_same_name_match_reindex() {
    let before = "class Report:\n    def Report(self):\n        return 1\n";
    let after = "from cached import Cached\n\n\n@a.b.Tagged(id=1)\nclass Report:\n    @Cached(60)\n    def Report(self):\n        return 1\n\n    @a.b.Tagged(id=2)\n    def render(self):\n        return 2\n";
    let edges = assert_decorates_match(
        "arguments, qualified, same name",
        &[
            ("cached.py", "class Cached:\n    pass\n"),
            ("main.py", before),
        ],
        &[Edit::Write("main.py", after)],
        &["a.b.Tagged".to_string()],
        &[],
    );
    assert!(
        edges
            .iter()
            .any(|e| e.contains(" Report #") && e.contains(" -> Class cached.py:1 Cached #")),
        "the method's decorator binds the imported class: {edges:#?}"
    );
    assert_eq!(
        edges
            .iter()
            .filter(|e| e.contains(" -> Annotation -:1 a.b.Tagged #"))
            .count(),
        2,
        "the class and render each reach the one node: {edges:#?}"
    );
}

/// A file the edit empties sources no `Decorates` edge; a node only it
/// used goes, as in a reindex.
#[test]
fn test_overlay_view_emptied_file_drops_its_decorates() {
    assert_decorates_match(
        "emptied file",
        &[
            ("a.py", PY_FOO_A),
            ("b.py", "@Bar\ndef beta():\n    return 2\n"),
        ],
        &[Edit::Write("a.py", "")],
        &["Bar".to_string()],
        &["Foo".to_string()],
    );
}

/// A removed file's decorators go with it: a node only it used goes.
#[test]
fn test_overlay_view_removed_file_drops_its_annotation() {
    assert_decorates_match(
        "removed file",
        &[
            ("a.py", PY_FOO_A),
            ("b.py", "@Bar\ndef beta():\n    return 2\n"),
        ],
        &[Edit::Delete("a.py")],
        &["Bar".to_string()],
        &["Foo".to_string()],
    );
}

/// A new `Annotation` node has no owning file, as the index's has none: a
/// consumer reads no path for it, never an empty one.
#[test]
fn test_overlay_view_new_annotation_node_has_no_file_path() {
    let observed = strict_parity::run(&Scenario {
        name: "",
        owner: OWNER,
        files: &[("a.py", "def alpha():\n    return 1\n")],
        edits: &[Edit::Write("a.py", "@Fresh\ndef alpha():\n    return 1\n")],
    });
    let base = observed.base();
    let view = observed.view().expect("a dirty file");
    let merged = MergedGraph::new(base, Some(&view));
    let fresh: Vec<Option<&str>> = (merged.base_len()..merged.node_count())
        .filter_map(|idx| merged.node(idx))
        .filter(|node| node.kind() == NodeKind::Annotation)
        .map(|node| node.file_path(&merged))
        .collect();
    assert_eq!(fresh, [None]);
}
