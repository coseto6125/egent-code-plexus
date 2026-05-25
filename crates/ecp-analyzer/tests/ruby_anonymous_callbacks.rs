//! Calls inside an anonymous block attached to a method call must be attached
//! to an `<anonymous>` Function node instead of being dropped by
//! `attach_to_enclosing` (filter (A) callback registration).
//!
//! Repro: `arr.each { |x| process(x) }` inside a method body produced 0
//! callers for `process` before this change because the block has no named
//! enclosing function in the query-match graph.

use ecp_analyzer::ruby::parser::RubyProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = RubyProvider::new().expect("provider");
    p.parse_file(Path::new("test.rb"), src.as_bytes())
        .expect("parse")
}

fn anonymous_calls(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous"))
        .flat_map(|n| n.calls.iter().map(String::as_str))
        .collect()
}

/// Brace-block `{ |x| process(x) }` — call inside must attach to the
/// `<anonymous>` node, not be dropped.
#[test]
fn brace_block_call_attaches_to_anonymous_node() {
    // Multi-line: attach_to_enclosing is row-granular, so the inner call must
    // sit on a row the block spans more narrowly than the enclosing method.
    let g = parse("def m\n  arr.each { |x|\n    process(x)\n  }\nend");
    assert!(
        anonymous_calls(&g).contains(&"process"),
        "expected `process` attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

/// `do … end` block form produces the same result as the brace form.
#[test]
fn do_end_block_call_attaches_to_anonymous_node() {
    let g = parse(
        r#"
def m
  arr.each do |x|
    process(x)
  end
end
"#,
    );
    assert!(
        anonymous_calls(&g).contains(&"process"),
        "expected `process` attached to <anonymous> (do/end), nodes: {:?}",
        g.nodes
    );
}

/// A block whose body has no call expression must NOT emit an `<anonymous>`
/// node — empty-callback bloat stays out of the graph.
#[test]
fn empty_block_emits_no_anonymous_node() {
    let g = parse("def m; arr.each { |x| x + 1 }; end");
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "block without a call must not emit a node, nodes: {:?}",
        g.nodes
    );
}

/// Passing a symbol proc (`&:process`) is a named-method reference, not an
/// anonymous block — no `<anonymous>` node must be emitted.
#[test]
fn symbol_proc_arg_is_not_treated_as_anonymous_block() {
    let g = parse("def m; arr.each(&:process); end");
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "symbol-proc argument must not emit <anonymous>, nodes: {:?}",
        g.nodes
    );
}
