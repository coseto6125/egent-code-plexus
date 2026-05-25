//! Shared formatting helpers for archived graph enums. Previously each
//! command carried its own copy of `kind_to_str` / `rel_to_str`.

use ecp_core::graph::{ArchivedNodeKind, ArchivedRelType};

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
    }
}
