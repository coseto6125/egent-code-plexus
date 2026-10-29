//! Node constructors and tree predicates shared by every spec.

use crate::framework_helpers::{enclosing_function_name, node_span, MODULE_LEVEL_SOURCE};
use ecp_core::analyzer::types::{RawNode, Span};
use ecp_core::graph::NodeKind;
use tree_sitter::Node;

/// `hashed` is the byte range whose xxh3 becomes `content_hash`; callers set
/// the other fields with struct-update syntax.
pub fn decl(name: String, kind: NodeKind, span: Span, hashed: &[u8]) -> RawNode {
    RawNode {
        name,
        kind,
        span,
        is_exported: false,
        heritage: Vec::new(),
        type_annotation: None,
        decorators: Vec::new(),
        calls: Vec::new(),
        field_reads: Vec::new(),
        owner_class: None,
        content_hash: ecp_core::uid::xxh3_64_bytes(hashed),
    }
}

pub fn anonymous(node: Node<'_>, source: &[u8]) -> RawNode {
    let span = span(node);
    decl(
        format!("<anonymous:{}:{}>", span.0.saturating_add(1), span.1),
        NodeKind::Function,
        span,
        &source[node.start_byte()..node.end_byte()],
    )
}

#[inline]
pub fn span(node: Node<'_>) -> Span {
    node_span(&node)
}

/// From the start of `start` to the end of `end`, saturating like [`span`].
#[inline]
pub fn span_from(start: Node<'_>, end: Node<'_>) -> Span {
    let (s_row, s_col, _, _) = span(start);
    let (_, _, e_row, e_col) = span(end);
    (s_row, s_col, e_row, e_col)
}

/// Whether `node` or a descendant has one of `kinds`. The answer does not
/// depend on the visit order, so this walk can replace any other full walk.
pub fn subtree_has_kind(node: Node<'_>, kinds: &[&str]) -> bool {
    let mut cursor = node.walk();
    loop {
        if kinds.contains(&cursor.node().kind()) {
            return true;
        }
        if cursor.goto_first_child() {
            continue;
        }
        // The cursor treats `node` as its root, so neither move leaves the subtree.
        while !cursor.goto_next_sibling() {
            if !cursor.goto_parent() {
                return false;
            }
        }
    }
}

pub fn enclosing_or_module(nodes: &[RawNode], span: Span) -> String {
    enclosing_function_name(nodes, span).unwrap_or_else(|| MODULE_LEVEL_SOURCE.to_string())
}

/// Removes consecutive nodes equal on name, span and kind. A duplicate that
/// another node separates stays: the cached graphs keep both.
pub fn dedup_consecutive(nodes: &mut Vec<RawNode>) {
    nodes.dedup_by(|a, b| a.name == b.name && a.span == b.span && a.kind == b.kind);
}

#[cfg(test)]
pub(crate) fn rust_tree(source: &[u8]) -> tree_sitter::Tree {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .expect("the Rust grammar loads");
    parser.parse(source, None).expect("the Rust grammar parses")
}

#[cfg(test)]
pub(crate) fn kind_nodes<'t>(node: Node<'t>, kind: &str, out: &mut Vec<Node<'t>>) {
    if node.kind() == kind {
        out.push(node);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        kind_nodes(child, kind, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decl_sets_empty_fields_and_hash() {
        let n = decl(
            "f".to_string(),
            NodeKind::Method,
            (1, 2, 3, 4),
            b"fn f() {}",
        );
        assert_eq!(
            (n.name.as_str(), n.kind, n.span),
            ("f", NodeKind::Method, (1, 2, 3, 4))
        );
        assert!(!n.is_exported && n.type_annotation.is_none() && n.owner_class.is_none());
        assert!(n.heritage.is_empty() && n.decorators.is_empty());
        assert!(n.calls.is_empty() && n.field_reads.is_empty());
        assert_eq!(n.content_hash, ecp_core::uid::xxh3_64_bytes(b"fn f() {}"));
    }

    #[test]
    fn test_anonymous_name_uses_row_plus_one() {
        let src = b"fn f() {\n    g(|x| h(x));\n}\n";
        let tree = rust_tree(src);
        let mut closures = Vec::new();
        kind_nodes(tree.root_node(), "closure_expression", &mut closures);
        let n = anonymous(closures[0], src);
        assert_eq!(
            (n.name.as_str(), n.kind),
            ("<anonymous:2:6>", NodeKind::Function)
        );
        assert_eq!(n.span, (1, 6, 1, 14));
        assert_eq!(n.content_hash, ecp_core::uid::xxh3_64_bytes(b"|x| h(x)"));
    }

    #[test]
    fn test_subtree_has_kind_root_itself_returns_true() {
        let tree = rust_tree(b"fn f() {}");
        let root = tree.root_node();
        assert!(subtree_has_kind(root, &["source_file"]));
        let item = root.named_child(0).expect("one item");
        assert!(subtree_has_kind(
            item,
            &["call_expression", "function_item"]
        ));
    }

    #[test]
    fn test_subtree_has_kind_absent_returns_false() {
        let tree = rust_tree(b"fn f() { let x = 1; }\nfn g() { h(); }\n");
        let f = tree.root_node().named_child(0).expect("fn f");
        // The walk must stay inside `f`: its sibling `g` holds a call.
        assert!(!subtree_has_kind(f, &["call_expression"]));
        let leaf = f.child_by_field_name("name").expect("name");
        assert!(!subtree_has_kind(leaf, &["call_expression"]));
        assert!(subtree_has_kind(tree.root_node(), &["call_expression"]));
    }

    #[test]
    fn test_dedup_nodes_separated_duplicate_stays() {
        let a = || decl("a".to_string(), NodeKind::Function, (0, 0, 0, 1), b"a");
        let b = || decl("b".to_string(), NodeKind::Function, (1, 0, 1, 1), b"b");
        let mut separated = vec![a(), b(), a()];
        dedup_consecutive(&mut separated);
        assert_eq!(separated.len(), 3);
        let mut adjacent = vec![a(), a(), b()];
        dedup_consecutive(&mut adjacent);
        let names: Vec<&str> = adjacent.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, ["a", "b"]);
    }
}
