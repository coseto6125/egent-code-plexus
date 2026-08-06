//! Shared formatting helpers for archived graph enums. Previously each
//! command carried its own copy of `kind_to_str` / `rel_to_str`.

use ecp_core::graph::{ArchivedNodeKind, ArchivedRelType, NodeKind, RelType};

/// Variant name for a live `NodeKind` (e.g. overlay hits).
///
/// Delegates to `NodeKind::as_str`, which `ecp_core::uid::compute` also feeds
/// into the canonical byte stream — so a rendered kind label and the name
/// hashed into that node's UID cannot drift apart.
pub fn node_kind_to_str(kind: &NodeKind) -> &'static str {
    (*kind).as_str()
}

pub fn kind_to_str(kind: &ArchivedNodeKind) -> &'static str {
    node_kind_to_str(&NodeKind::from(kind))
}

/// Owned-`RelType` twin of [`rel_to_str`] — same strings, for code paths
/// holding a live `RelType` (e.g. merged overlay edges) rather than an
/// archived one. Distinct from `RelType::as_str`, which renders the PascalCase
/// variant name cypher's `type(r)` returns.
pub fn rel_type_to_str(rel: RelType) -> &'static str {
    match rel {
        RelType::Defines => "defines",
        RelType::Imports => "imports",
        RelType::Calls => "calls",
        RelType::Extends => "extends",
        RelType::Implements => "implements",
        RelType::HasMethod => "has_method",
        RelType::HasProperty => "has_property",
        RelType::Accesses => "accesses",
        RelType::HandlesRoute => "handles_route",
        RelType::StepInProcess => "step_in_process",
        RelType::References => "references",
        RelType::Fetches => "fetches",
        RelType::MirrorsField => "mirrors_field",
        RelType::Publishes => "publishes",
        RelType::Subscribes => "subscribes",
        RelType::EventTopicMirror => "event_topic_mirror",
        RelType::OpensTxScope => "opens_tx_scope",
        RelType::Overrides => "overrides",
        RelType::Decorates => "decorates",
        RelType::UsesPathLiteral => "uses_path_literal",
        RelType::ReadsField => "reads_field",
        RelType::CompensatedBy => "compensated_by",
        RelType::QueriesTable => "queries_table",
    }
}

pub fn rel_to_str(rel: &ArchivedRelType) -> &'static str {
    rel_type_to_str(RelType::from(rel))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rendered label and the name `uid::compute` hashes must be the same
    /// string. They used to be two hand-maintained tables that the compiler
    /// had no way to keep aligned — a new variant could render correctly and
    /// still hash under a different name.
    #[test]
    fn node_kind_label_matches_the_name_hashed_into_uids() {
        for kind in NODE_KINDS {
            assert_eq!(node_kind_to_str(&kind), kind.as_str(), "{kind:?}");
        }
    }

    /// Archived and live sides render identically, so a graph read back from
    /// `graph.bin` labels its nodes the same way an overlay hit does.
    #[test]
    fn archived_and_live_node_kinds_render_identically() {
        for kind in NODE_KINDS {
            let archived = unsafe { &*(&kind as *const NodeKind as *const ArchivedNodeKind) };
            assert_eq!(kind_to_str(archived), node_kind_to_str(&kind), "{kind:?}");
        }
    }

    /// Exhaustive by construction: adding a variant without extending this
    /// list fails the match below at compile time.
    const NODE_KINDS: [NodeKind; 29] = [
        NodeKind::File,
        NodeKind::Function,
        NodeKind::Class,
        NodeKind::Method,
        NodeKind::Interface,
        NodeKind::Constructor,
        NodeKind::Property,
        NodeKind::Variable,
        NodeKind::Const,
        NodeKind::Import,
        NodeKind::Route,
        NodeKind::Process,
        NodeKind::Document,
        NodeKind::Section,
        NodeKind::EntryPoint,
        NodeKind::Struct,
        NodeKind::Enum,
        NodeKind::Typedef,
        NodeKind::Namespace,
        NodeKind::Module,
        NodeKind::Macro,
        NodeKind::Annotation,
        NodeKind::Trait,
        NodeKind::Impl,
        NodeKind::SchemaField,
        NodeKind::EventTopic,
        NodeKind::TransactionScope,
        NodeKind::EnumVariant,
        NodeKind::PathLiteral,
    ];

    /// Compile-time guard: a new `NodeKind` variant breaks this match, which
    /// is the reminder to extend `NODE_KINDS` above.
    #[allow(dead_code)]
    fn every_variant_is_listed(kind: NodeKind) {
        match kind {
            NodeKind::File
            | NodeKind::Function
            | NodeKind::Class
            | NodeKind::Method
            | NodeKind::Interface
            | NodeKind::Constructor
            | NodeKind::Property
            | NodeKind::Variable
            | NodeKind::Const
            | NodeKind::Import
            | NodeKind::Route
            | NodeKind::Process
            | NodeKind::Document
            | NodeKind::Section
            | NodeKind::EntryPoint
            | NodeKind::Struct
            | NodeKind::Enum
            | NodeKind::Typedef
            | NodeKind::Namespace
            | NodeKind::Module
            | NodeKind::Macro
            | NodeKind::Annotation
            | NodeKind::Trait
            | NodeKind::Impl
            | NodeKind::SchemaField
            | NodeKind::EventTopic
            | NodeKind::TransactionScope
            | NodeKind::EnumVariant
            | NodeKind::PathLiteral => {}
        }
    }
}
