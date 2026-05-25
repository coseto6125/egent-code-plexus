//! Calls inside an anonymous closure passed as a call argument must be
//! attached to an `<anonymous>` Function node instead of being dropped by
//! `attach_to_enclosing` (no named enclosing scope at module top-level).
//!
//! Repro: `list.forEach((x) { process(x); })` at top level produced
//! 0 callers for `process` before this change.

use ecp_analyzer::dart::parser::DartProvider;
use ecp_core::analyzer::provider::LanguageProvider;
use ecp_core::analyzer::types::LocalGraph;
use ecp_core::graph::NodeKind;
use std::path::Path;

fn parse(src: &str) -> LocalGraph {
    let p = DartProvider::new().expect("provider");
    p.parse_file(Path::new("test.dart"), src.as_bytes())
        .expect("parse")
}

fn anonymous_calls(g: &LocalGraph) -> Vec<&str> {
    g.nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous"))
        .flat_map(|n| n.calls.iter().map(String::as_str))
        .collect()
}

#[test]
fn block_closure_callback_attaches_call_to_anonymous_node() {
    // `forEach` with a block-body closure: `(x) { process(x); }`.
    // Multi-line: attach_to_enclosing is row-granular, so the inner call must
    // sit on a row the closure spans more narrowly than the enclosing function.
    let g = parse("void main() {\n  list.forEach((x) {\n    process(x);\n  });\n}");
    assert!(
        anonymous_calls(&g).contains(&"process"),
        "expected process attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn arrow_closure_with_call_body_is_a_grammar_limitation() {
    // tree-sitter-dart 0.2.0 parses `(x) => transform(x)` as application
    // `((x) => transform)(x)` — the closure body is just the identifier
    // `transform`, and the call binds OUTSIDE the closure. So an arrow closure
    // whose body is a single call carries no inner call_expression and cannot
    // host an <anonymous> call edge. Block-form closures (`(x) { f(x); }`) are
    // unaffected and covered above. Pinned so a future grammar bump that fixes
    // the parse flips this test and prompts re-enabling arrow support.
    let g = parse("void main() {\n  items.map((x) =>\n      transform(x));\n}");
    assert!(
        !anonymous_calls(&g).contains(&"transform"),
        "grammar limitation changed — arrow closure now carries the call, re-enable arrow capture; nodes: {:?}",
        g.nodes
    );
}

#[test]
fn top_level_closure_callback_attaches_call() {
    // Closure at module top level (no enclosing named function) — the bug
    // case where attach_to_enclosing previously had nowhere to attach.
    let g = parse("final _ = list.forEach((x) { doWork(x); });");
    assert!(
        anonymous_calls(&g).contains(&"doWork"),
        "expected doWork attached to <anonymous>, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn empty_closure_emits_no_anonymous_node() {
    // A closure with no calls must not produce an <anonymous> node.
    let g = parse("void main() { items.map((x) => x * 2); }");
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "closure without a call must not emit a node, nodes: {:?}",
        g.nodes
    );
}

#[test]
fn named_function_arg_is_not_treated_as_anonymous_callback() {
    // Passing a named function reference — not a closure, not anonymous.
    let g = parse("void main() { list.forEach(processItem); }");
    assert!(
        !g.nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name.starts_with("<anonymous")),
        "named-fn reference must not emit <anonymous>, nodes: {:?}",
        g.nodes
    );
}
