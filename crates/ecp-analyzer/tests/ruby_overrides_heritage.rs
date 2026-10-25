//! `Overrides` in Ruby binds the declared base (slice P7): two types
//! named `Base` in two files, and `Child` extends the one of its own file.
//! The pass reads the supertype Pass 1.9 resolved, so the edge never reaches
//! the other `Base`. Before P7 the pass paired by class name alone, so a
//! marked method reached the first file's `Base`, and an unmarked one got no
//! edge.
//!
//! Ruby has no override marker, so the pair is a name match in the declared base.

mod overrides_support;

use overrides_support::{assert_binds_declared_base, Evidence};

#[test]
fn test_overrides_ruby_two_same_named_bases_binds_declared_one() {
    assert_binds_declared_base(
        &ecp_analyzer::ruby::parser::RubyProvider::new().unwrap(),
        ("a/decoy.rb", "class Base\n  def run\n  end\nend\n"),
        (
            "b/child.rb",
            "class Base\n  def run\n  end\nend\n\nclass Child < Base\n  def run\n  end\nend\n",
        ),
        "run",
        Evidence::Heuristic,
    );
}

/// Stands for: Ruby looks a method up through `ancestors` — the included
/// modules, last first, then the superclass — so `C.m` binds `M.m`, not
/// the superclass's `B.m`.
#[test]
fn test_overrides_ruby_ancestor_lookup_preserves_order() {
    let graph = overrides_support::graph_of(
        &ecp_analyzer::ruby::parser::RubyProvider::new().unwrap(),
        &[(
            "c.rb",
            "module M\n  def m\n  end\nend\n\nclass B\n  def m\n  end\nend\n\nclass C < B\n  include M\n  def m\n  end\nend\n",
        )],
    );
    let edges = overrides_support::overrides(&graph);
    assert_eq!(edges.len(), 1, "C.m -> M.m only; got {edges:?}");
    assert_eq!((edges[0].0.line, edges[0].1.line), (12, 1), "{edges:?}");
    overrides_support::assert_evidence(&edges[0], Evidence::Heuristic);
}
