//! Containment edges of one file's parse: scope `Defines` (File, Namespace,
//! Module) and enum containment (Enum -> EnumVariant).
//!
//! The index's post-process passes (`ecp-analyzer` `post_process::scope_defines`
//! and `post_process::enum_variant_defines`) and the overlay's per-file replay
//! (`session::incremental`) both call these, so a dirty file gets the edges a
//! reindex gives. Each side maps the raw indexes and declarations it gets back
//! to its own node ids.

use crate::analyzer::types::{span_contains, RawNode, Span};
use crate::graph::NodeKind;
use rustc_hash::FxHashMap;

/// `Edge.reason` of a scope `Defines` edge.
pub const SCOPE_DEFINES_REASON: &str = "pass2:defines";

/// `Edge.reason` of an Enum -> EnumVariant `Defines` edge.
pub const ENUM_VARIANT_DEFINES_REASON: &str = "post_process:enum_variant_defines";

/// The fields of one declaration that containment reads. `span` is 0-based,
/// as `RawNode.span`.
#[derive(Debug, Clone, Copy)]
pub struct Row<'a> {
    pub kind: NodeKind,
    pub owner: Option<&'a str>,
    pub name: &'a str,
    pub span: Span,
}

impl<'a> From<&'a RawNode> for Row<'a> {
    fn from(node: &'a RawNode) -> Self {
        Self {
            kind: node.kind,
            owner: node.owner_class.as_deref(),
            name: &node.name,
            span: node.span,
        }
    }
}

/// The source of a scope `Defines` edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// The file's `File` node.
    File,
    /// The Namespace or Module at this raw index of the same file.
    Container(usize),
}

/// Whether a declaration of `kind` takes a scope `Defines` edge. Property and
/// Constructor are class members (`HasMethod` / `HasProperty`). Method is in:
/// Ruby and PHP emit a top-level `def` as a Method with no owner, and a
/// Method with a class owner finds no Namespace or Module container.
fn is_definable(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Method
            | NodeKind::Class
            | NodeKind::Interface
            | NodeKind::Trait
            | NodeKind::Const
            | NodeKind::Variable
            | NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Typedef
            | NodeKind::Macro
            | NodeKind::Module
            | NodeKind::Namespace
    )
}

/// The scope `Defines` edges of one file, as `(source, child raw index)` in
/// row order: `File -> child` for a definable row without an owner, and
/// `container -> child` for one whose owner names a Namespace or Module of
/// the file. Of two same-name containers (`namespace Acme` twice), the
/// innermost one around the child; else the first, as a statement-form
/// `namespace Acme;` contains nothing. A row whose owner names no container
/// (a class member) gets nothing: `HasMethod` / `HasProperty` cover it.
///
/// The caller maps each raw index to its node and drops a scoped child (a
/// nested `fn`, a closure node): its callable declares it, through the
/// lexical-parent table. Cost: O(rows + rows x same-name containers).
pub fn scope_defines<'a, T>(
    rows: &'a [T],
    row: impl Fn(&'a T) -> Row<'a>,
    mut emit: impl FnMut(Scope, usize),
) {
    let mut containers: FxHashMap<&'a str, Vec<usize>> = FxHashMap::default();
    for (i, r) in rows.iter().map(&row).enumerate() {
        if matches!(r.kind, NodeKind::Namespace | NodeKind::Module) {
            containers.entry(r.name).or_default().push(i);
        }
    }
    for (i, child) in rows.iter().map(&row).enumerate() {
        if !is_definable(child.kind) {
            continue;
        }
        let Some(owner) = child.owner else {
            emit(Scope::File, i);
            continue;
        };
        let container = containers.get(owner).and_then(|ids| {
            ids.iter()
                .copied()
                .filter(|&c| c != i && span_contains(row(&rows[c]).span, child.span))
                .max_by_key(|&c| {
                    let span = row(&rows[c]).span;
                    (span.0, span.1)
                })
                .or_else(|| ids.first().copied())
        });
        if let Some(c) = container {
            emit(Scope::Container(c), i);
        }
    }
}

/// The Enum -> EnumVariant edges of one file, as `(enum, variant)` node ids
/// in variant order. `first_declaration(name, kind, owner)` returns the id of
/// the file's first declaration of that name, kind and owner, the one later
/// overloads hang off; ids are the caller's. A variant's owner is its enum's
/// name. Enums are matched by kind, as a C `typedef enum color color;` names
/// two `color`s, and variants by owner, as two enums of one file may share a
/// variant name (`Shape::None`, `Fill::None`). Of two enums of one name, the
/// last row's declaration wins. A self-loop (an enum and its variant of one
/// id) is dropped. Cost: O(rows) lookups; nothing for a file without a variant.
pub fn enum_variant_defines<'a, T>(
    rows: &'a [T],
    row: impl Fn(&'a T) -> Row<'a>,
    first_declaration: impl Fn(&str, NodeKind, Option<&str>) -> Option<u32>,
    mut emit: impl FnMut(u32, u32),
) {
    if !rows
        .iter()
        .map(&row)
        .any(|r| r.kind == NodeKind::EnumVariant)
    {
        return;
    }
    let enum_by_name: FxHashMap<&'a str, u32> = rows
        .iter()
        .map(&row)
        .filter(|r| r.kind == NodeKind::Enum)
        .filter_map(|r| first_declaration(r.name, NodeKind::Enum, r.owner).map(|id| (r.name, id)))
        .collect();
    if enum_by_name.is_empty() {
        return;
    }
    for variant in rows
        .iter()
        .map(&row)
        .filter(|r| r.kind == NodeKind::EnumVariant)
    {
        let Some(enum_name) = variant.owner else {
            continue;
        };
        let Some(&enum_id) = enum_by_name.get(enum_name) else {
            continue;
        };
        let Some(variant_id) =
            first_declaration(variant.name, NodeKind::EnumVariant, Some(enum_name))
        else {
            continue;
        };
        if enum_id != variant_id {
            emit(enum_id, variant_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        kind: NodeKind,
        owner: Option<&'static str>,
        name: &'static str,
        span: Span,
    ) -> Row<'static> {
        Row {
            kind,
            owner,
            name,
            span,
        }
    }

    fn scopes(rows: &[Row<'static>]) -> Vec<(Scope, usize)> {
        let mut out = Vec::new();
        scope_defines(rows, |r| *r, |scope, child| out.push((scope, child)));
        out
    }

    #[test]
    fn test_scope_defines_empty_file_emits_nothing() {
        assert!(scopes(&[]).is_empty());
    }

    #[test]
    fn test_scope_defines_top_level_and_namespace_member_pick_their_scope() {
        let rows = [
            row(NodeKind::Function, None, "free", (0, 0, 1, 0)),
            row(NodeKind::Namespace, None, "Acme", (2, 0, 9, 0)),
            row(NodeKind::Class, Some("Acme"), "K", (3, 0, 8, 0)),
            row(NodeKind::Method, Some("K"), "m", (4, 0, 5, 0)),
            row(NodeKind::Property, None, "p", (6, 0, 6, 9)),
        ];
        assert_eq!(
            scopes(&rows),
            [(Scope::File, 0), (Scope::File, 1), (Scope::Container(1), 2)],
            "a class member and a non-definable kind get no Defines"
        );
    }

    #[test]
    fn test_scope_defines_same_name_namespaces_pick_innermost_else_first() {
        let rows = [
            row(NodeKind::Namespace, None, "Acme", (0, 0, 9, 0)),
            row(NodeKind::Namespace, None, "Acme", (11, 0, 19, 0)),
            row(NodeKind::Class, Some("Acme"), "Extra", (12, 0, 18, 0)),
            row(NodeKind::Class, Some("Acme"), "Outside", (30, 0, 31, 0)),
        ];
        assert_eq!(
            scopes(&rows),
            [
                (Scope::File, 0),
                (Scope::File, 1),
                (Scope::Container(1), 2),
                (Scope::Container(0), 3)
            ]
        );
    }

    #[test]
    fn test_scope_defines_whitespace_only_names_match_literally() {
        let rows = [
            row(NodeKind::Module, None, " ", (0, 0, 9, 0)),
            row(NodeKind::Function, Some(" "), " ", (1, 0, 2, 0)),
            row(NodeKind::Function, Some("  "), "f", (3, 0, 4, 0)),
        ];
        assert_eq!(scopes(&rows), [(Scope::File, 0), (Scope::Container(0), 1)]);
    }

    /// The first row of each (name, kind, owner) stands for the declaration,
    /// as the index's `SymbolTable::first_declaration` does.
    fn first_of<'a>(
        rows: &'a [Row<'static>],
    ) -> impl Fn(&str, NodeKind, Option<&str>) -> Option<u32> + 'a {
        move |name, kind, owner| {
            rows.iter()
                .position(|r| r.name == name && r.kind == kind && r.owner == owner)
                .map(|i| i as u32)
        }
    }

    fn variants(rows: &[Row<'static>]) -> Vec<(u32, u32)> {
        let mut out = Vec::new();
        enum_variant_defines(rows, |r| *r, first_of(rows), |e, v| out.push((e, v)));
        out
    }

    #[test]
    fn test_enum_variant_defines_two_enums_sharing_a_variant_name_pair_by_owner() {
        let rows = [
            row(NodeKind::Enum, None, "Shape", (0, 0, 2, 0)),
            row(NodeKind::EnumVariant, Some("Shape"), "None", (1, 0, 1, 4)),
            row(NodeKind::Enum, None, "Fill", (3, 0, 5, 0)),
            row(NodeKind::EnumVariant, Some("Fill"), "None", (4, 0, 4, 4)),
        ];
        assert_eq!(variants(&rows), [(0, 1), (2, 3)]);
    }

    #[test]
    fn test_enum_variant_defines_enum_without_variants_emits_nothing() {
        let rows = [row(NodeKind::Enum, None, "Empty", (0, 0, 0, 12))];
        assert!(variants(&rows).is_empty());
    }

    #[test]
    fn test_enum_variant_defines_variant_of_unknown_enum_emits_nothing() {
        let rows = [
            row(NodeKind::Enum, None, "Known", (0, 0, 1, 0)),
            row(NodeKind::EnumVariant, Some("Other"), "A", (2, 0, 2, 1)),
            row(NodeKind::EnumVariant, None, "B", (3, 0, 3, 1)),
        ];
        assert!(variants(&rows).is_empty());
    }
}
