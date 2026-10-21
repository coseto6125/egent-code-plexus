//! The declared bases of the class that encloses a declaration: what the
//! index's heritage tier (Tier 2.75, `HeritageScoped`) probes for a bare call
//! the earlier tiers left unresolved. Index-time resolution and the session
//! overlay read the same rule from here.

use crate::analyzer::types::RawNode;
use crate::graph::NodeKind;

/// A parsed declaration, as the heritage lookup reads it.
pub trait HeritageNode {
    fn kind(&self) -> NodeKind;
    /// 0-based `(start_row, start_column, end_row, end_column)`, the shape of
    /// [`RawNode::span`].
    fn span(&self) -> (u32, u32, u32, u32);
    /// The declared base names, as the parser wrote them.
    fn heritage(&self) -> &[String];
}

impl HeritageNode for RawNode {
    fn kind(&self) -> NodeKind {
        self.kind
    }

    fn span(&self) -> (u32, u32, u32, u32) {
        self.span
    }

    fn heritage(&self) -> &[String] {
        &self.heritage
    }
}

/// The heritage of the smallest `Class` among `nodes` whose span contains
/// `node`'s span, or `node`'s own heritage when it is a `Class`. Empty when
/// no class encloses it (a top-level function). Ruby `module Foo` parses as a
/// `Class` too, so this covers module mixins.
///
/// "Smallest" compares `(row difference, column difference)`, and the first
/// of two equal sizes wins, as the index has always measured it.
pub fn enclosing_class_heritage<'a, N: HeritageNode>(node: &'a N, nodes: &'a [N]) -> &'a [String] {
    if node.kind() == NodeKind::Class {
        return node.heritage();
    }
    let (s_row, s_col, e_row, e_col) = node.span();
    let mut best: Option<&N> = None;
    let mut best_size: (u32, u32) = (u32::MAX, u32::MAX);
    for candidate in nodes {
        if candidate.kind() != NodeKind::Class {
            continue;
        }
        let (cs_row, cs_col, ce_row, ce_col) = candidate.span();
        if (cs_row, cs_col) > (s_row, s_col) || (ce_row, ce_col) < (e_row, e_col) {
            continue;
        }
        let size = (ce_row.saturating_sub(cs_row), ce_col.saturating_sub(cs_col));
        if size < best_size {
            best_size = size;
            best = Some(candidate);
        }
    }
    best.map_or(&[], |class| class.heritage())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Decl {
        kind: NodeKind,
        span: (u32, u32, u32, u32),
        heritage: Vec<String>,
    }

    impl HeritageNode for Decl {
        fn kind(&self) -> NodeKind {
            self.kind
        }

        fn span(&self) -> (u32, u32, u32, u32) {
            self.span
        }

        fn heritage(&self) -> &[String] {
            &self.heritage
        }
    }

    fn decl(kind: NodeKind, span: (u32, u32, u32, u32), bases: &[&str]) -> Decl {
        Decl {
            kind,
            span,
            heritage: bases.iter().map(|b| b.to_string()).collect(),
        }
    }

    #[test]
    fn test_enclosing_class_heritage_method_in_nested_class_returns_innermost_bases() {
        let nodes = [
            decl(NodeKind::Class, (0, 0, 20, 1), &["Outer"]),
            decl(NodeKind::Class, (2, 2, 10, 3), &["Inner"]),
            decl(NodeKind::Method, (3, 4, 5, 5), &[]),
            decl(NodeKind::Method, (12, 4, 14, 5), &[]),
        ];
        assert_eq!(enclosing_class_heritage(&nodes[2], &nodes), ["Inner"]);
        assert_eq!(enclosing_class_heritage(&nodes[3], &nodes), ["Outer"]);
    }

    #[test]
    fn test_enclosing_class_heritage_class_returns_its_own_bases() {
        let nodes = [
            decl(NodeKind::Class, (0, 0, 20, 1), &["Outer"]),
            decl(NodeKind::Class, (2, 2, 10, 3), &["Inner"]),
        ];
        assert_eq!(enclosing_class_heritage(&nodes[1], &nodes), ["Inner"]);
    }

    #[test]
    fn test_enclosing_class_heritage_top_level_function_returns_empty() {
        let nodes = [
            decl(NodeKind::Class, (0, 0, 2, 1), &["Base"]),
            decl(NodeKind::Function, (4, 0, 6, 1), &[]),
        ];
        assert!(enclosing_class_heritage(&nodes[1], &nodes).is_empty());
    }

    #[test]
    fn test_enclosing_class_heritage_struct_or_interface_container_returns_empty() {
        let nodes = [
            decl(NodeKind::Interface, (0, 0, 9, 1), &["Base"]),
            decl(NodeKind::Struct, (0, 0, 9, 1), &["Base"]),
            decl(NodeKind::Method, (2, 0, 3, 1), &[]),
        ];
        assert!(enclosing_class_heritage(&nodes[2], &nodes).is_empty());
    }

    #[test]
    fn test_enclosing_class_heritage_equal_sizes_returns_first_class() {
        let nodes = [
            decl(NodeKind::Class, (0, 0, 9, 1), &["First"]),
            decl(NodeKind::Class, (0, 0, 9, 1), &["Second"]),
            decl(NodeKind::Method, (2, 0, 3, 1), &[]),
        ];
        assert_eq!(enclosing_class_heritage(&nodes[2], &nodes), ["First"]);
    }
}
