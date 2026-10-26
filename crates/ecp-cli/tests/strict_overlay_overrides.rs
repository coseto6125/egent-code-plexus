//! `Overrides` edges of the incremental overlay (F7b1-4): for a dirty file,
//! the overlay view rebuilds each method's `Overrides` edge as a fresh full
//! index of the edited tree has it, with the index's confidence and reason,
//! through the production index, fragment write and read
//! (`common/strict_parity.rs`). Both sides run the one derivation,
//! `ecp_core::analyzer::overrides::method_lookup`.
//!
//! Each table case first asserts the reindex has an `Overrides` edge the
//! edit adds, or the base index had the one it drops, so a fixture whose
//! parser records no heritage fails instead of passing empty.
//!
//! Exempt languages, which emit no `Overrides` edge:
//!
//! - go: `crates/ecp-core/src/analyzer/overrides.rs:354` gives Go the
//!   `NoConcept` rule: embedding promotes, it never overrides.
//! - c: `crates/ecp-core/src/analyzer/overrides.rs:355` returns no rules for
//!   C, which has no override.
//!
//! Only the dirty files' edges are rebuilt: a clean subclass whose supertype
//! the edit changed keeps its indexed edges (F7b2).

#[path = "common/baseline.rs"]
mod baseline;
mod common;
#[path = "common/strict_parity.rs"]
mod strict_parity;

use ecp_core::graph::{ArchivedZeroCopyGraph, RelType};
use ecp_core::session::merged::MergedGraph;
use strict_parity::{Edit, Scenario};

const OWNER: &str = "lane F (incremental overlay: builder edges of dirty files)";

/// One method of a fixture class: its name, its parameter count, and
/// whether it carries the language's explicit override marker.
#[derive(Clone, Copy)]
struct Method {
    name: &'static str,
    arity: usize,
    marked: bool,
}

const fn method(name: &'static str, arity: usize, marked: bool) -> Method {
    Method {
        name,
        arity,
        marked,
    }
}

/// One language: a clean file that declares the supertype `Base` and a
/// dirty file that declares `Child`, a subtype of it. `base_file` and
/// `child_file` render each file from its methods; `has_marker` says
/// whether the language has an explicit override marker the parser records.
struct Fixture {
    lang: &'static str,
    base: &'static str,
    child: &'static str,
    base_file: fn(&[Method]) -> String,
    child_file: fn(&[Method]) -> String,
    has_marker: bool,
}

/// `param(i)` for each of the method's parameters, joined by `", "`.
fn params(m: &Method, param: impl Fn(usize) -> String) -> String {
    (0..m.arity).map(param).collect::<Vec<_>>().join(", ")
}

/// `param(i)` for each of the method's parameters, concatenated: for a
/// parameter list that follows a receiver (`self`, `&self`).
fn trailing(m: &Method, param: impl Fn(usize) -> String) -> String {
    (0..m.arity).map(param).collect()
}

/// `decl(method)` per method, joined.
fn each(methods: &[Method], decl: impl Fn(&Method) -> String) -> String {
    methods.iter().map(decl).collect()
}

/// `text` when the method is marked, else nothing.
fn marker(m: &Method, text: &'static str) -> &'static str {
    if m.marked {
        text
    } else {
        ""
    }
}

fn fixtures() -> [Fixture; 12] {
    [
        Fixture {
            lang: "typescript",
            base: "src/base.ts",
            child: "src/child.ts",
            base_file: |ms| {
                let body = each(ms, |m| {
                    let ps = params(m, |i| format!("p{i}: number"));
                    format!("  {}({ps}): void {{}}\n", m.name)
                });
                format!("export class Base {{\n{body}}}\n")
            },
            child_file: |ms| {
                let body = each(ms, |m| {
                    let ps = params(m, |i| format!("p{i}: number"));
                    format!("  {}{}({ps}): void {{}}\n", marker(m, "override "), m.name)
                });
                format!(
                    "import {{ Base }} from \"./base\";\n\nexport class Child extends Base {{\n{body}}}\n"
                )
            },
            has_marker: true,
        },
        Fixture {
            lang: "javascript",
            base: "src/base.js",
            child: "src/child.js",
            base_file: |ms| {
                let body = each(ms, |m| {
                    format!("  {}({}) {{}}\n", m.name, params(m, |i| format!("p{i}")))
                });
                format!("export class Base {{\n{body}}}\n")
            },
            child_file: |ms| {
                let body = each(ms, |m| {
                    format!("  {}({}) {{}}\n", m.name, params(m, |i| format!("p{i}")))
                });
                format!(
                    "import {{ Base }} from \"./base\";\n\nexport class Child extends Base {{\n{body}}}\n"
                )
            },
            has_marker: false,
        },
        Fixture {
            lang: "python",
            base: "base.py",
            child: "child.py",
            base_file: |ms| {
                let body = each(ms, |m| {
                    let ps = trailing(m, |i| format!(", p{i}"));
                    format!("    def {}(self{ps}):\n        pass\n\n", m.name)
                });
                format!("class Base:\n{body}")
            },
            child_file: |ms| {
                let body = each(ms, |m| {
                    let ps = trailing(m, |i| format!(", p{i}"));
                    format!(
                        "    {}def {}(self{ps}):\n        pass\n\n",
                        marker(m, "@override\n    "),
                        m.name
                    )
                });
                format!(
                    "from typing import override\n\nfrom base import Base\n\n\nclass Child(Base):\n{body}"
                )
            },
            has_marker: true,
        },
        Fixture {
            lang: "ruby",
            base: "lib/base.rb",
            child: "lib/child.rb",
            base_file: |ms| {
                let body = each(ms, |m| {
                    format!(
                        "  def {}({})\n  end\n\n",
                        m.name,
                        params(m, |i| format!("p{i}"))
                    )
                });
                format!("class Base\n{body}end\n")
            },
            child_file: |ms| {
                let body = each(ms, |m| {
                    format!(
                        "  def {}({})\n  end\n\n",
                        m.name,
                        params(m, |i| format!("p{i}"))
                    )
                });
                format!("require_relative \"base\"\n\nclass Child < Base\n{body}end\n")
            },
            has_marker: false,
        },
        Fixture {
            lang: "php",
            base: "src/Base.php",
            child: "src/Child.php",
            base_file: |ms| {
                let body = each(ms, |m| {
                    let ps = params(m, |i| format!("$p{i}"));
                    format!("    public function {}({ps}) {{}}\n", m.name)
                });
                format!("<?php\nclass Base {{\n{body}}}\n")
            },
            child_file: |ms| {
                let body = each(ms, |m| {
                    let ps = params(m, |i| format!("$p{i}"));
                    format!(
                        "    {}public function {}({ps}) {{}}\n",
                        marker(m, "#[\\Override]\n    "),
                        m.name
                    )
                });
                format!("<?php\nclass Child extends Base {{\n{body}}}\n")
            },
            has_marker: true,
        },
        Fixture {
            lang: "swift",
            base: "Sources/Base.swift",
            child: "Sources/Child.swift",
            base_file: |ms| {
                let body = each(ms, |m| {
                    let ps = params(m, |i| format!("p{i}: Int"));
                    format!("    func {}({ps}) {{}}\n", m.name)
                });
                format!("class Base {{\n{body}}}\n")
            },
            child_file: |ms| {
                let body = each(ms, |m| {
                    let ps = params(m, |i| format!("p{i}: Int"));
                    format!("    {}func {}({ps}) {{}}\n", marker(m, "override "), m.name)
                });
                format!("class Child: Base {{\n{body}}}\n")
            },
            has_marker: true,
        },
        Fixture {
            lang: "dart",
            base: "lib/base.dart",
            child: "lib/child.dart",
            base_file: |ms| {
                let body = each(ms, |m| {
                    let ps = params(m, |i| format!("int p{i}"));
                    format!("  void {}({ps}) {{}}\n", m.name)
                });
                format!("class Base {{\n{body}}}\n")
            },
            child_file: |ms| {
                let body = each(ms, |m| {
                    let ps = params(m, |i| format!("int p{i}"));
                    format!(
                        "  {}void {}({ps}) {{}}\n",
                        marker(m, "@override\n  "),
                        m.name
                    )
                });
                format!("import 'base.dart';\n\nclass Child extends Base {{\n{body}}}\n")
            },
            has_marker: true,
        },
        Fixture {
            lang: "rust",
            base: "src/lib.rs",
            child: "src/child.rs",
            base_file: |ms| {
                let body = each(ms, |m| {
                    let ps = trailing(m, |i| format!(", p{i}: i32"));
                    format!("    fn {}(&self{ps}) {{}}\n", m.name)
                });
                format!("pub mod child;\n\npub trait Base {{\n{body}}}\n")
            },
            child_file: |ms| {
                let body = each(ms, |m| {
                    let ps = trailing(m, |i| format!(", p{i}: i32"));
                    format!("    fn {}(&self{ps}) {{}}\n", m.name)
                });
                format!(
                    "use crate::Base;\n\npub struct Child;\n\nimpl Base for Child {{\n{body}}}\n"
                )
            },
            has_marker: false,
        },
        Fixture {
            lang: "cpp",
            base: "src/base.hpp",
            child: "src/child.cpp",
            base_file: |ms| {
                let body = each(ms, |m| {
                    let ps = params(m, |i| format!("int p{i}"));
                    format!("    virtual void {}({ps}) {{}}\n", m.name)
                });
                format!("struct Base {{\n{body}}};\n")
            },
            child_file: |ms| {
                let body = each(ms, |m| {
                    let ps = params(m, |i| format!("int p{i}"));
                    format!("    void {}({ps}){} {{}}\n", m.name, marker(m, " override"))
                });
                format!("#include \"base.hpp\"\n\nstruct Child : public Base {{\n{body}}};\n")
            },
            has_marker: true,
        },
        Fixture {
            lang: "csharp",
            base: "src/Base.cs",
            child: "src/Child.cs",
            base_file: |ms| {
                let body = each(ms, |m| {
                    let ps = params(m, |i| format!("int p{i}"));
                    format!("        public virtual void {}({ps}) {{}}\n", m.name)
                });
                format!("namespace App {{\n    public class Base {{\n{body}    }}\n}}\n")
            },
            child_file: |ms| {
                let body = each(ms, |m| {
                    let ps = params(m, |i| format!("int p{i}"));
                    format!(
                        "        public {}void {}({ps}) {{}}\n",
                        marker(m, "override "),
                        m.name
                    )
                });
                format!("namespace App {{\n    public class Child : Base {{\n{body}    }}\n}}\n")
            },
            has_marker: true,
        },
        Fixture {
            lang: "kotlin",
            base: "src/Base.kt",
            child: "src/Child.kt",
            base_file: |ms| {
                let body = each(ms, |m| {
                    let ps = params(m, |i| format!("p{i}: Int"));
                    format!("    open fun {}({ps}) {{}}\n", m.name)
                });
                format!("package app\n\nopen class Base {{\n{body}}}\n")
            },
            child_file: |ms| {
                let body = each(ms, |m| {
                    let ps = params(m, |i| format!("p{i}: Int"));
                    format!("    {}fun {}({ps}) {{}}\n", marker(m, "override "), m.name)
                });
                format!("package app\n\nclass Child : Base() {{\n{body}}}\n")
            },
            has_marker: true,
        },
        Fixture {
            lang: "java",
            base: "src/app/Base.java",
            child: "src/app/Child.java",
            base_file: |ms| {
                let body = each(ms, |m| {
                    let ps = params(m, |i| format!("int p{i}"));
                    format!("    public void {}({ps}) {{}}\n", m.name)
                });
                format!("package app;\n\npublic class Base {{\n{body}}}\n")
            },
            child_file: |ms| {
                let body = each(ms, |m| {
                    let ps = params(m, |i| format!("int p{i}"));
                    format!(
                        "    {}public void {}({ps}) {{}}\n",
                        marker(m, "@Override\n    "),
                        m.name
                    )
                });
                format!("package app;\n\nclass Child extends Base {{\n{body}}}\n")
            },
            has_marker: true,
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

/// Every `Overrides` edge of `merged` as `source -> target conf reason`,
/// sorted. `graph` resolves a base edge's reason.
fn overrides<'a>(merged: &MergedGraph<'a>, graph: &'a ArchivedZeroCopyGraph) -> Vec<String> {
    let mut out: Vec<String> = merged
        .all_edges()
        .filter(|edge| edge.rel_type() == RelType::Overrides)
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

/// What a case checks the fixture against, beyond the parity itself, so a
/// fixture whose parser records no heritage fails instead of passing empty.
#[derive(Clone, Copy)]
enum Expect {
    /// The reindex has an `Overrides` edge from the named method.
    Gained(&'static str),
    /// The base index had an `Overrides` edge from the named method.
    Lost(&'static str),
}

/// Whether `edges` holds one whose source is a method named `name`.
fn has_edge_from(edges: &[String], name: &str) -> bool {
    let needle = format!(" {name} #");
    edges.iter().any(|edge| {
        edge.split(" -> ")
            .next()
            .is_some_and(|source| source.contains(&needle))
    })
}

/// Run `files` + `edits`, check `expect` on the reindex or the base index,
/// and assert the overlay's `Overrides` edges are the reindex's. Returns the
/// reindex's edges, as [`overrides`] renders them.
fn assert_overrides_match(
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
    let full_edges = overrides(&MergedGraph::new(full_graph, None), full_graph);
    match expect {
        Expect::Gained(name) => assert!(
            has_edge_from(&full_edges, name),
            "{label}: the reindex has an Overrides edge from {name}: {full_edges:#?}"
        ),
        Expect::Lost(name) => {
            let base_edges = overrides(&MergedGraph::new(base_graph, None), base_graph);
            assert!(
                has_edge_from(&base_edges, name),
                "{label}: the base index had an Overrides edge from {name}: {base_edges:#?}"
            );
        }
    }
    let view = observed.view();
    let overlay = MergedGraph::new(base_graph, view.as_ref());
    assert_eq!(
        overrides(&overlay, base_graph),
        full_edges,
        "{label}: the Overrides edges"
    );
    full_edges
}

/// Per language with an override concept, the dirty subclass of a clean
/// supertype gains an override, loses one, changes one's arity, and adds
/// the explicit marker to one. Red before F7b1-4: the overlay kept the base
/// index's `Overrides` edges of the dirty file and built none.
#[test]
fn test_overlay_view_dirty_overrides_matches_reindex() {
    let keep = method("keep", 0, false);
    let base = [method("run", 1, false), method("stop", 1, false)];
    for fx in &fixtures() {
        let base_src = (fx.base_file)(&base);
        let mut cases: Vec<(&str, Vec<Method>, Vec<Method>, Expect)> = vec![
            (
                "new override",
                vec![keep],
                vec![keep, method("run", 1, true)],
                Expect::Gained("run"),
            ),
            (
                "override removed",
                vec![keep, method("run", 1, true)],
                vec![keep],
                Expect::Lost("run"),
            ),
            (
                "arity change",
                vec![keep, method("stop", 1, true)],
                vec![keep, method("stop", 2, true)],
                Expect::Lost("stop"),
            ),
        ];
        if fx.has_marker {
            cases.push((
                "marker added",
                vec![keep, method("run", 1, false)],
                vec![keep, method("run", 1, true)],
                Expect::Gained("run"),
            ));
        }
        for (case, before, after, expect) in cases {
            let (before, after) = ((fx.child_file)(&before), (fx.child_file)(&after));
            assert_overrides_match(
                &format!("{} {case}", fx.lang),
                &[(fx.base, base_src.as_str()), (fx.child, before.as_str())],
                &[Edit::Write(fx.child, &after)],
                expect,
            );
        }
    }
}

/// One edit changes both the supertype and its subclass: the subclass's new
/// override binds the supertype's new method, both dirty.
#[test]
fn test_overlay_view_dirty_supertype_and_subclass_overrides_match_reindex() {
    let base_before = "class Base:\n    def run(self):\n        pass\n";
    let base_after =
        "class Base:\n    def run(self):\n        pass\n\n    def stop(self):\n        pass\n";
    let child_before =
        "from base import Base\n\n\nclass Child(Base):\n    def run(self):\n        pass\n";
    let child_after = "from base import Base\n\n\nclass Child(Base):\n    def run(self):\n        pass\n\n    def stop(self):\n        pass\n";
    assert_overrides_match(
        "dirty supertype and subclass",
        &[("base.py", base_before), ("child.py", child_before)],
        &[
            Edit::Write("base.py", base_after),
            Edit::Write("child.py", child_after),
        ],
        Expect::Gained("stop"),
    );
}

/// A Python diamond `D(B, C)`, `B(A)`, `C(A)`, where only `C` and `A`
/// declare `run`: C3 puts `C` before `A`, so the dirty `D.run` overrides
/// `C.run`, which a depth-first walk would miss for `A.run`.
#[test]
fn test_overlay_view_python_diamond_override_follows_c3() {
    let d_before =
        "from b import B\nfrom c import C\n\n\nclass D(B, C):\n    def keep(self):\n        pass\n";
    let d_after = "from b import B\nfrom c import C\n\n\nclass D(B, C):\n    def keep(self):\n        pass\n\n    def run(self):\n        pass\n";
    let edges = assert_overrides_match(
        "python diamond",
        &[
            ("a.py", "class A:\n    def run(self):\n        pass\n"),
            (
                "b.py",
                "from a import A\n\n\nclass B(A):\n    def walk(self):\n        pass\n",
            ),
            (
                "c.py",
                "from a import A\n\n\nclass C(A):\n    def run(self):\n        pass\n",
            ),
            ("d.py", d_before),
        ],
        &[Edit::Write("d.py", d_after)],
        Expect::Gained("run"),
    );
    assert!(
        edges
            .iter()
            .any(|edge| edge.contains(" d.py:") && edge.contains(" -> Method c.py:")),
        "D.run overrides C.run: {edges:#?}"
    );
}

/// The edit removes the supertype's file and touches the subclass: the
/// subclass's override no longer resolves a base, as in a reindex.
#[test]
fn test_overlay_view_removed_supertype_drops_override() {
    let child = |extra: &str| {
        format!("from base import Base\n\n\nclass Child(Base):\n    def run(self):\n        pass\n{extra}")
    };
    let (before, after) = (child(""), child("\n    def keep(self):\n        pass\n"));
    assert_overrides_match(
        "removed supertype",
        &[
            ("base.py", "class Base:\n    def run(self):\n        pass\n"),
            ("child.py", before.as_str()),
        ],
        &[Edit::Delete("base.py"), Edit::Write("child.py", &after)],
        Expect::Lost("run"),
    );
}

/// A static method neither overrides nor is overridden in Java; in
/// TypeScript a static overrides a static. The instance `run` keeps each
/// fixture from passing empty.
#[test]
fn test_overlay_view_static_method_overrides_match_reindex() {
    let java_base = "package app;\n\npublic class Base {\n    public void run() {}\n\n    public static void make() {}\n}\n";
    let java_child = |extra: &str| {
        format!("package app;\n\nclass Child extends Base {{\n    @Override\n    public void run() {{}}\n{extra}}}\n")
    };
    let (before, after) = (
        java_child(""),
        java_child("\n    public static void make() {}\n"),
    );
    let edges = assert_overrides_match(
        "java static",
        &[
            ("src/app/Base.java", java_base),
            ("src/app/Child.java", before.as_str()),
        ],
        &[Edit::Write("src/app/Child.java", &after)],
        Expect::Gained("run"),
    );
    assert!(
        !has_edge_from(&edges, "make"),
        "a Java static overrides nothing: {edges:#?}"
    );

    let ts_base = "export class Base {\n  run(): void {}\n\n  static make(): void {}\n}\n";
    let ts_child = |extra: &str| {
        format!("import {{ Base }} from \"./base\";\n\nexport class Child extends Base {{\n  run(): void {{}}\n{extra}}}\n")
    };
    let (before, after) = (ts_child(""), ts_child("\n  static make(): void {}\n"));
    assert_overrides_match(
        "typescript static",
        &[("src/base.ts", ts_base), ("src/child.ts", before.as_str())],
        &[Edit::Write("src/child.ts", &after)],
        Expect::Gained("run"),
    );
}

/// A TypeScript supertype method takes a rest parameter: the subclass's
/// method of another arity still pairs with it, as the `Arity` signature
/// rule allows when a rest parameter is visible.
#[test]
fn test_overlay_view_rest_parameter_override_matches_reindex() {
    let base = "export class Base {\n  run(...xs: number[]): void {}\n}\n";
    let child = |body: &str| {
        format!("import {{ Base }} from \"./base\";\n\nexport class Child extends Base {{\n  keep(): void {{}}\n{body}}}\n")
    };
    let (before, after) = (child(""), child("\n  run(a: number, b: number): void {}\n"));
    assert_overrides_match(
        "rest parameter",
        &[("src/base.ts", base), ("src/child.ts", before.as_str())],
        &[Edit::Write("src/child.ts", &after)],
        Expect::Gained("run"),
    );
}
