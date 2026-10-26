//! `Extends` and `Implements` edges of the incremental overlay: for a dirty
//! file, the overlay view rebuilds each declaration's heritage edges as a
//! fresh full index of the edited tree has them, with the index's rel,
//! confidence and reason, through the production index, fragment write and
//! read (`common/strict_parity.rs`). Both sides pick the rel by
//! `ecp_core::analyzer::heritage::heritage_rel`.
//!
//! Each case first asserts the reindex has the edge the edit adds, or the
//! base index had the one it drops, so a fixture whose parser records no
//! heritage fails instead of passing empty.
//!
//! Exempt language, which records no heritage, so emits neither edge:
//!
//! - c: `crates/ecp-analyzer/src/c/queries.scm` has no `@heritage` capture,
//!   and `crates/ecp-analyzer/src/c/parser.rs:614` writes `heritage: vec![]`.
//!
//! Only the dirty files' edges are rebuilt: a clean subclass of a type the
//! edit changed keeps its indexed edge (F7b2).

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use ecp_core::graph::{ArchivedZeroCopyGraph, RelType};
use ecp_core::session::merged::MergedGraph;
use strict_parity::{Edit, Scenario};

const OWNER: &str = "lane F (incremental overlay: builder edges of dirty files)";

/// One language: a clean file that declares the types `Base` and `Other`
/// (and the interface `Iface` where the language has one), and a dirty
/// file that declares `Child`. `child_file(base, iface)` is the dirty
/// file's source with `Child` extending `base` and implementing `iface`,
/// each optional. `base_rel` is the rel of the edge to `Base`.
struct Fixture {
    lang: &'static str,
    base: &'static str,
    child: &'static str,
    base_src: &'static str,
    child_file: fn(Option<&str>, Option<&str>) -> String,
    base_rel: RelType,
    has_interface: bool,
}

/// `prefix` + the present `items` joined by `sep`, or nothing when none is
/// present.
fn listed(prefix: &str, items: &[Option<&str>], sep: &str) -> String {
    let present: Vec<&str> = items.iter().flatten().copied().collect();
    if present.is_empty() {
        String::new()
    } else {
        format!("{prefix}{}", present.join(sep))
    }
}

/// `prefix` + `text` when `text` is present, else nothing.
fn opt(prefix: &str, text: Option<&str>) -> String {
    listed(prefix, &[text], "")
}

fn fixtures() -> [Fixture; 13] {
    [
        Fixture {
            lang: "typescript",
            base: "src/base.ts",
            child: "src/child.ts",
            base_src: "export class Base {}\nexport class Other {}\nexport interface Iface {}\n",
            child_file: |base, iface| {
                format!(
                    "import {{ Base, Other, Iface }} from \"./base\";\n\nexport class Child{}{} {{}}\n",
                    opt(" extends ", base),
                    opt(" implements ", iface)
                )
            },
            base_rel: RelType::Extends,
            has_interface: true,
        },
        Fixture {
            lang: "javascript",
            base: "src/base.js",
            child: "src/child.js",
            base_src: "export class Base {}\nexport class Other {}\n",
            child_file: |base, _| {
                format!(
                    "import {{ Base, Other }} from \"./base\";\n\nexport class Child{} {{}}\n",
                    opt(" extends ", base)
                )
            },
            base_rel: RelType::Extends,
            has_interface: false,
        },
        Fixture {
            lang: "python",
            base: "base.py",
            child: "child.py",
            base_src: "class Base:\n    pass\n\n\nclass Other:\n    pass\n",
            child_file: |base, _| {
                format!(
                    "from base import Base, Other\n\n\nclass Child{}:\n    pass\n",
                    base.map(|b| format!("({b})")).unwrap_or_default()
                )
            },
            base_rel: RelType::Extends,
            has_interface: false,
        },
        Fixture {
            lang: "java",
            base: "src/app/Base.java",
            child: "src/app/Child.java",
            base_src: "package app;\n\nclass Base {}\n\nclass Other {}\n\ninterface Iface {}\n",
            child_file: |base, iface| {
                format!(
                    "package app;\n\nclass Child{}{} {{}}\n",
                    opt(" extends ", base),
                    opt(" implements ", iface)
                )
            },
            base_rel: RelType::Extends,
            has_interface: true,
        },
        Fixture {
            lang: "kotlin",
            base: "src/Base.kt",
            child: "src/Child.kt",
            base_src: "package app\n\nopen class Base\n\nopen class Other\n\ninterface Iface\n",
            child_file: |base, iface| {
                let base = base.map(|b| format!("{b}()"));
                format!(
                    "package app\n\nclass Child{}\n",
                    listed(" : ", &[base.as_deref(), iface], ", ")
                )
            },
            base_rel: RelType::Extends,
            has_interface: true,
        },
        Fixture {
            lang: "csharp",
            base: "src/Base.cs",
            child: "src/Child.cs",
            base_src: "namespace App {\n    public class Base {}\n    public class Other {}\n    public interface Iface {}\n}\n",
            child_file: |base, iface| {
                format!(
                    "namespace App {{\n    public class Child{} {{}}\n}}\n",
                    listed(" : ", &[base, iface], ", ")
                )
            },
            base_rel: RelType::Extends,
            has_interface: true,
        },
        Fixture {
            lang: "go",
            base: "app/base.go",
            child: "app/child.go",
            base_src: "package app\n\ntype Base struct{}\n\ntype Other struct{}\n",
            child_file: |base, _| match base {
                Some(b) => format!("package app\n\ntype Child struct {{\n\t{b}\n}}\n"),
                None => "package app\n\ntype Child struct{}\n".to_string(),
            },
            base_rel: RelType::Extends,
            has_interface: false,
        },
        Fixture {
            lang: "rust",
            base: "src/lib.rs",
            child: "src/child.rs",
            base_src: "pub mod child;\n\npub trait Base {}\n\npub trait Other {}\n\npub trait Iface {}\n",
            // Rust heritage comes from `impl Trait for T` and rides on the
            // block's methods; a supertrait bound records none
            // (`crates/ecp-analyzer/tests/rust_overrides_heritage.rs:26`).
            child_file: |base, iface| {
                let impls: String = [base, iface]
                    .into_iter()
                    .flatten()
                    .map(|t| format!("\nimpl {t} for Child {{\n    fn via_{t}(&self) {{}}\n}}\n"))
                    .collect();
                format!("use crate::{{Base, Iface, Other}};\n\npub struct Child;\n{impls}")
            },
            base_rel: RelType::Implements,
            has_interface: true,
        },
        Fixture {
            lang: "php",
            base: "src/Base.php",
            child: "src/Child.php",
            base_src: "<?php\nclass Base {}\nclass Other {}\ninterface Iface {}\n",
            child_file: |base, iface| {
                format!(
                    "<?php\nclass Child{}{} {{}}\n",
                    opt(" extends ", base),
                    opt(" implements ", iface)
                )
            },
            base_rel: RelType::Extends,
            has_interface: true,
        },
        Fixture {
            lang: "ruby",
            base: "lib/base.rb",
            child: "lib/child.rb",
            base_src: "class Base\nend\n\nclass Other\nend\n",
            child_file: |base, _| {
                format!(
                    "require_relative \"base\"\n\nclass Child{}\nend\n",
                    opt(" < ", base)
                )
            },
            base_rel: RelType::Extends,
            has_interface: false,
        },
        Fixture {
            lang: "swift",
            base: "Sources/Base.swift",
            child: "Sources/Child.swift",
            base_src: "class Base {}\n\nclass Other {}\n\nprotocol Iface {}\n",
            child_file: |base, iface| {
                format!("class Child{} {{}}\n", listed(": ", &[base, iface], ", "))
            },
            base_rel: RelType::Extends,
            has_interface: true,
        },
        Fixture {
            lang: "dart",
            base: "lib/base.dart",
            child: "lib/child.dart",
            base_src: "class Base {}\n\nclass Other {}\n",
            child_file: |base, _| {
                format!(
                    "import 'base.dart';\n\nclass Child{} {{}}\n",
                    opt(" extends ", base)
                )
            },
            base_rel: RelType::Extends,
            has_interface: false,
        },
        Fixture {
            lang: "cpp",
            base: "src/base.hpp",
            child: "src/child.cpp",
            base_src: "struct Base {};\n\nstruct Other {};\n",
            child_file: |base, _| {
                format!(
                    "#include \"base.hpp\"\n\nstruct Child{} {{}};\n",
                    opt(" : public ", base)
                )
            },
            base_rel: RelType::Extends,
            has_interface: false,
        },
    ]
}

/// `node` as `kind path:line name #uid`.
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

/// Every `Extends` and `Implements` edge of `merged` as
/// `rel source -> target conf reason`, sorted. `graph` resolves a base
/// edge's reason.
fn heritage<'a>(merged: &MergedGraph<'a>, graph: &'a ArchivedZeroCopyGraph) -> Vec<String> {
    let mut out: Vec<String> = merged
        .all_edges()
        .filter(|edge| matches!(edge.rel_type(), RelType::Extends | RelType::Implements))
        .filter_map(|edge| {
            Some(format!(
                "{} {} -> {} conf={:?} reason={}",
                edge.rel_type().as_str(),
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

/// What a case checks beyond the parity itself, so a fixture whose parser
/// records no heritage fails instead of passing empty.
#[derive(Clone, Copy)]
enum Expect {
    /// The reindex has an edge of the rel to the named type.
    Gained(RelType, &'static str),
    /// The base index had an edge of the rel to the named type.
    Lost(RelType, &'static str),
}

/// Whether `edges` holds an edge of `rel` whose target is named `name`.
fn has_edge_to(edges: &[String], rel: RelType, name: &str) -> bool {
    let needle = format!(" {name} #");
    edges.iter().any(|edge| {
        edge.starts_with(&format!("{} ", rel.as_str()))
            && edge
                .split(" -> ")
                .nth(1)
                .is_some_and(|target| target.contains(&needle))
    })
}

/// Run `files` + `edits`, check `expect`, and assert the overlay's heritage
/// edges are the reindex's. Returns the reindex's edges, as [`heritage`]
/// renders them.
fn assert_heritage_match(
    label: &str,
    files: &[(&str, &str)],
    edits: &[Edit<'_>],
    expect: Expect,
) -> Vec<String> {
    let observed = strict_parity::run(&Scenario {
        name: "",
        owner: OWNER,
        files,
        edits,
    });
    let (full_graph, base_graph) = (observed.full(), observed.base());
    let full_edges = heritage(&MergedGraph::new(full_graph, None), full_graph);
    match expect {
        Expect::Gained(rel, name) => assert!(
            has_edge_to(&full_edges, rel, name),
            "{label}: the reindex has a {} edge to {name}: {full_edges:#?}",
            rel.as_str()
        ),
        Expect::Lost(rel, name) => {
            let base_edges = heritage(&MergedGraph::new(base_graph, None), base_graph);
            assert!(
                has_edge_to(&base_edges, rel, name),
                "{label}: the base index had a {} edge to {name}: {base_edges:#?}",
                rel.as_str()
            );
        }
    }
    let view = observed.view();
    let overlay = MergedGraph::new(base_graph, view.as_ref());
    assert_eq!(
        heritage(&overlay, base_graph),
        full_edges,
        "{label}: the Extends / Implements edges"
    );
    full_edges
}

/// Per language with heritage, the dirty `Child` gains a base, loses it,
/// changes it to another clean type, and gains an implemented interface.
/// Red before the heritage replay: the overlay kept the base index's edges
/// of the dirty file and built none, so a gained base had no edge and a
/// dropped one stayed.
#[test]
fn test_overlay_view_dirty_heritage_matches_reindex() {
    for fx in &fixtures() {
        let rel = fx.base_rel;
        let mut cases: Vec<(&str, String, String, Expect)> = vec![
            (
                "base added",
                (fx.child_file)(None, None),
                (fx.child_file)(Some("Base"), None),
                Expect::Gained(rel, "Base"),
            ),
            (
                "base removed",
                (fx.child_file)(Some("Base"), None),
                (fx.child_file)(None, None),
                Expect::Lost(rel, "Base"),
            ),
            (
                "base changed",
                (fx.child_file)(Some("Base"), None),
                (fx.child_file)(Some("Other"), None),
                Expect::Gained(rel, "Other"),
            ),
        ];
        if fx.has_interface {
            cases.push((
                "implements added",
                (fx.child_file)(Some("Base"), None),
                (fx.child_file)(Some("Base"), Some("Iface")),
                Expect::Gained(RelType::Implements, "Iface"),
            ));
        }
        for (case, before, after, expect) in cases {
            assert_heritage_match(
                &format!("{} {case}", fx.lang),
                &[(fx.base, fx.base_src), (fx.child, before.as_str())],
                &[Edit::Write(fx.child, &after)],
                expect,
            );
        }
    }
}

/// A dirty file adds `impl Base for Child` for a `Child` a clean file
/// declares: Rust records the trait on each method of the block, so the
/// `Implements` edge starts at the dirty method, as in a reindex.
#[test]
fn test_overlay_view_rust_impl_for_clean_type_matches_reindex() {
    let after = "use crate::model::Child;\nuse crate::Base;\n\nimpl Base for Child {\n    fn run(&self) {}\n}\n";
    let edges = assert_heritage_match(
        "rust impl for a clean type",
        &[
            (
                "src/lib.rs",
                "pub mod child;\npub mod model;\n\npub trait Base {\n    fn run(&self);\n}\n",
            ),
            ("src/model.rs", "pub struct Child;\n"),
            ("src/child.rs", "use crate::model::Child;\n"),
        ],
        &[Edit::Write("src/child.rs", after)],
        Expect::Gained(RelType::Implements, "Base"),
    );
    assert!(
        edges
            .iter()
            .any(|edge| edge.contains(" src/child.rs:") && edge.contains(" run #")),
        "the edge starts at the impl block's method: {edges:#?}"
    );
}
