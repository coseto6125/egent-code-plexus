//! The declared bases of the class that encloses a declaration: what the
//! index's heritage tier (Tier 2.75, `HeritageScoped`) probes for a bare call
//! the earlier tiers left unresolved. Index-time resolution and the session
//! overlay read the same rule from here.
//!
//! It also holds the shape of the Pass 2 heritage edges
//! ([`heritage_rel`], [`heritage_reason`]) and the C# base spelling rule
//! ([`csharp_base_arity`]): the index's `pass2_emit_node_edges` and the
//! overlay's replay of a dirty file's `Extends` / `Implements` edges.

use crate::analyzer::types::RawNode;
use crate::graph::{NodeKind, RelType};

/// The `reason` of an `Extends` edge.
pub const EXTENDS_REASON: &str = "heritage";
/// The `reason` of an `Implements` edge.
pub const IMPLEMENTS_REASON: &str = "pass2:implements";
/// Every reason [`heritage_reason`] returns.
pub const HERITAGE_REASONS: [&str; 2] = [EXTENDS_REASON, IMPLEMENTS_REASON];

/// The rel of a heritage edge to a base of kind `target`: an interface or a
/// trait is implemented, any other type is extended. The resolved target's
/// kind is the relationship, so no parser decides it (Java `implements`,
/// Kotlin `:`, Go structural, Rust `impl For` share one heritage list).
pub fn heritage_rel(target: NodeKind) -> RelType {
    match target {
        NodeKind::Interface | NodeKind::Trait => RelType::Implements,
        _ => RelType::Extends,
    }
}

/// The `reason` of a heritage edge of `rel` ([`heritage_rel`]).
pub fn heritage_reason(rel: RelType) -> &'static str {
    match rel {
        RelType::Implements => IMPLEMENTS_REASON,
        _ => EXTENDS_REASON,
    }
}

/// A C# base spelling as the bare type it names and the number of type
/// arguments of its last segment: `Conv<int, string>` is (`Conv`, 2),
/// `N.Conv<List<int>>` is (`N.Conv`, 1), `Conv` is (`Conv`, 0). A spelling
/// whose argument list belongs to an outer segment (`Outer<int>.Inner`) or
/// does not close stays whole, with arity 0.
pub fn csharp_base_arity(spelling: &str) -> (&str, u32) {
    let whole = (spelling, 0);
    let Some(open) = spelling.find('<') else {
        return whole;
    };
    let Some(args) = spelling
        .trim_end()
        .strip_suffix('>')
        .and_then(|s| s.get(open + 1..))
    else {
        return whole;
    };
    let mut depth = 0u32;
    let mut arity = 1;
    for c in args.chars() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' if depth == 0 => return whole,
            '>' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => arity += 1,
            _ => {}
        }
    }
    (spelling[..open].trim_end(), arity)
}

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

/// Whether a declaration of `kind` encloses members and declares bases:
/// a class, a struct (C++ `struct C : B`, Swift and C# conformances), an
/// interface or protocol (Java default methods, Kotlin interfaces), a trait
/// (Ruby `module`, Dart extension, Rust supertraits) or an enum (Java,
/// Kotlin and Swift conformances). A Rust `impl` block is none: its trait
/// rides on each method, and a bare call inside it never reaches a trait
/// member.
pub fn holds_heritage(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Class | NodeKind::Struct | NodeKind::Interface | NodeKind::Trait | NodeKind::Enum
    )
}

/// The heritage of the smallest type among `nodes` that [`holds_heritage`]
/// and whose span contains `node`'s span, or `node`'s own heritage when it
/// is such a type. Empty when no type encloses it (a top-level function).
///
/// "Smallest" compares `(row difference, column difference)`, and the first
/// of two equal sizes wins, as the index has always measured it.
pub fn enclosing_class_heritage<'a, N: HeritageNode>(node: &'a N, nodes: &'a [N]) -> &'a [String] {
    if holds_heritage(node.kind()) {
        return node.heritage();
    }
    let (s_row, s_col, e_row, e_col) = node.span();
    let mut best: Option<&N> = None;
    let mut best_size: (u32, u32) = (u32::MAX, u32::MAX);
    for candidate in nodes {
        if !holds_heritage(candidate.kind()) {
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

    /// An interface or a trait is implemented, any other target extended,
    /// each with its own reason.
    #[test]
    fn test_heritage_rel_by_target_kind_returns_rel_and_reason() {
        for (kind, rel) in [
            (NodeKind::Interface, RelType::Implements),
            (NodeKind::Trait, RelType::Implements),
            (NodeKind::Class, RelType::Extends),
            (NodeKind::Struct, RelType::Extends),
            (NodeKind::Enum, RelType::Extends),
        ] {
            assert_eq!(heritage_rel(kind), rel, "{kind:?}");
        }
        assert_eq!(heritage_reason(RelType::Implements), IMPLEMENTS_REASON);
        assert_eq!(heritage_reason(RelType::Extends), EXTENDS_REASON);
    }

    /// The type-argument count of the last segment; a list that belongs to
    /// an outer segment, or does not close, keeps the spelling whole.
    #[test]
    fn test_csharp_base_arity_spellings_return_bare_name_and_count() {
        for (spelling, expected) in [
            ("Conv", ("Conv", 0)),
            ("Conv<int, string>", ("Conv", 2)),
            ("N.Conv<List<int>>", ("N.Conv", 1)),
            ("Conv<Func<int, string>>", ("Conv", 1)),
            ("Outer<int>.Inner", ("Outer<int>.Inner", 0)),
            ("Conv<int", ("Conv<int", 0)),
            ("", ("", 0)),
        ] {
            assert_eq!(csharp_base_arity(spelling), expected, "{spelling:?}");
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
    fn test_enclosing_class_heritage_member_of_each_heritage_kind_returns_its_bases() {
        for kind in [
            NodeKind::Class,
            NodeKind::Struct,
            NodeKind::Interface,
            NodeKind::Trait,
            NodeKind::Enum,
        ] {
            let nodes = [
                decl(kind, (0, 0, 9, 1), &["Base"]),
                decl(NodeKind::Method, (2, 0, 3, 1), &[]),
            ];
            assert_eq!(
                enclosing_class_heritage(&nodes[1], &nodes),
                ["Base"],
                "{kind:?}"
            );
        }
    }

    #[test]
    fn test_enclosing_class_heritage_rust_impl_block_returns_empty() {
        let nodes = [
            decl(NodeKind::Impl, (0, 0, 9, 1), &["Trait"]),
            decl(NodeKind::Method, (2, 0, 3, 1), &[]),
        ];
        assert!(enclosing_class_heritage(&nodes[1], &nodes).is_empty());
    }

    #[test]
    fn test_enclosing_class_heritage_method_in_class_nested_in_struct_returns_class_bases() {
        let nodes = [
            decl(NodeKind::Struct, (0, 0, 20, 1), &["Outer"]),
            decl(NodeKind::Class, (2, 2, 10, 3), &["Inner"]),
            decl(NodeKind::Method, (3, 4, 5, 5), &[]),
        ];
        assert_eq!(enclosing_class_heritage(&nodes[2], &nodes), ["Inner"]);
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
