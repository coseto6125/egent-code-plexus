//! Shared formatting helpers for archived graph enums. Previously each
//! command carried its own copy of `kind_to_str` / `rel_to_str`.

use ecp_core::graph::{ArchivedNodeKind, ArchivedRelType, NodeKind};

/// Owned-`NodeKind` twin of [`kind_to_str`] — same strings, for code paths
/// holding a live `NodeKind` (e.g. overlay hits) rather than an archived one.
pub fn node_kind_to_str(kind: &NodeKind) -> &'static str {
    match kind {
        NodeKind::File => "File",
        NodeKind::Function => "Function",
        NodeKind::Class => "Class",
        NodeKind::Method => "Method",
        NodeKind::Interface => "Interface",
        NodeKind::Constructor => "Constructor",
        NodeKind::Property => "Property",
        NodeKind::Variable => "Variable",
        NodeKind::Const => "Const",
        NodeKind::Import => "Import",
        NodeKind::Route => "Route",
        NodeKind::Process => "Process",
        NodeKind::Document => "Document",
        NodeKind::Section => "Section",
        NodeKind::EntryPoint => "EntryPoint",
        NodeKind::Struct => "Struct",
        NodeKind::Enum => "Enum",
        NodeKind::Typedef => "Typedef",
        NodeKind::Namespace => "Namespace",
        NodeKind::Module => "Module",
        NodeKind::Macro => "Macro",
        NodeKind::Annotation => "Annotation",
        NodeKind::Trait => "Trait",
        NodeKind::Impl => "Impl",
        NodeKind::SchemaField => "SchemaField",
        NodeKind::EventTopic => "EventTopic",
        NodeKind::TransactionScope => "TransactionScope",
        NodeKind::EnumVariant => "EnumVariant",
        NodeKind::PathLiteral => "PathLiteral",
    }
}

pub fn kind_to_str(kind: &ArchivedNodeKind) -> &'static str {
    match kind {
        ArchivedNodeKind::File => "File",
        ArchivedNodeKind::Function => "Function",
        ArchivedNodeKind::Class => "Class",
        ArchivedNodeKind::Method => "Method",
        ArchivedNodeKind::Interface => "Interface",
        ArchivedNodeKind::Constructor => "Constructor",
        ArchivedNodeKind::Property => "Property",
        ArchivedNodeKind::Variable => "Variable",
        ArchivedNodeKind::Const => "Const",
        ArchivedNodeKind::Import => "Import",
        ArchivedNodeKind::Route => "Route",
        ArchivedNodeKind::Process => "Process",
        ArchivedNodeKind::Document => "Document",
        ArchivedNodeKind::Section => "Section",
        ArchivedNodeKind::EntryPoint => "EntryPoint",
        ArchivedNodeKind::Struct => "Struct",
        ArchivedNodeKind::Enum => "Enum",
        ArchivedNodeKind::Typedef => "Typedef",
        ArchivedNodeKind::Namespace => "Namespace",
        ArchivedNodeKind::Module => "Module",
        ArchivedNodeKind::Macro => "Macro",
        ArchivedNodeKind::Annotation => "Annotation",
        ArchivedNodeKind::Trait => "Trait",
        ArchivedNodeKind::Impl => "Impl",
        ArchivedNodeKind::SchemaField => "SchemaField",
        ArchivedNodeKind::EventTopic => "EventTopic",
        ArchivedNodeKind::TransactionScope => "TransactionScope",
        ArchivedNodeKind::EnumVariant => "EnumVariant",
        ArchivedNodeKind::PathLiteral => "PathLiteral",
    }
}

pub fn rel_to_str(rel: &ArchivedRelType) -> &'static str {
    match rel {
        ArchivedRelType::Defines => "defines",
        ArchivedRelType::Imports => "imports",
        ArchivedRelType::Calls => "calls",
        ArchivedRelType::Extends => "extends",
        ArchivedRelType::Implements => "implements",
        ArchivedRelType::HasMethod => "has_method",
        ArchivedRelType::HasProperty => "has_property",
        ArchivedRelType::Accesses => "accesses",
        ArchivedRelType::HandlesRoute => "handles_route",
        ArchivedRelType::StepInProcess => "step_in_process",
        ArchivedRelType::References => "references",
        ArchivedRelType::Fetches => "fetches",
        ArchivedRelType::MirrorsField => "mirrors_field",
        ArchivedRelType::Publishes => "publishes",
        ArchivedRelType::Subscribes => "subscribes",
        ArchivedRelType::EventTopicMirror => "event_topic_mirror",
        ArchivedRelType::OpensTxScope => "opens_tx_scope",
        ArchivedRelType::Overrides => "overrides",
        ArchivedRelType::Decorates => "decorates",
        ArchivedRelType::UsesPathLiteral => "uses_path_literal",
        ArchivedRelType::ReadsField => "reads_field",
        ArchivedRelType::CompensatedBy => "compensated_by",
        ArchivedRelType::QueriesTable => "queries_table",
    }
}
